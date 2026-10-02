#![allow(dead_code)]

//! A control rig: flysim's control state with a scripted stand-in for the loop behind it, the
//! embedded router with the control services registered, and the edge connected over its socket.
//! Every request can be sent to both sides — `direct` (flysim's own `:7401` router) and `edge`
//! (the same router over the bus) — and the answers compared byte for byte.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use fly_control_edge::{BusBackend, EdgeConfig, EdgeMetrics};
use flysim::AppState;
use flysim::bus::{EmbeddedBus, Uses};
use flysim::chat::{ChatRefusal, RejectReason};
use flysim::controlbus::{ControlHost, Scope};
use flysim::eventlog::{EventLog, EventRing, NewEvent};
use flysim::ratelimit::Refusal;
use flysim::simloop::{Command, Shared, booting_snapshot};
use flysim::snapshot::{FeedEventKind, FeedStatus, MacroMode, Snapshot};
use tokio::sync::{mpsc, watch};
use tower::ServiceExt;

/// The stand-in loop: answers every command deterministically from what the request says, so
/// the same request gets the same answer however often (and from whichever side) it is sent.
#[derive(Default)]
pub struct FakeSim {
    /// Commands received, by kind.
    pub received: Mutex<Vec<&'static str>>,
    /// Hold every command without answering (a wedged loop).
    pub hold: AtomicBool,
    /// Answer `/checkpoint` with a failure.
    pub fail_checkpoint: AtomicBool,
    /// Held commands: kept so their reply channels stay open.
    pub held: Mutex<Vec<Command>>,
    pub answered: AtomicU64,
}

impl FakeSim {
    pub fn received(&self, kind: &str) -> usize {
        self.received
            .lock()
            .unwrap()
            .iter()
            .filter(|k| **k == kind)
            .count()
    }

    pub fn release_held(&self) {
        self.held.lock().unwrap().clear();
    }

    fn answer(&self, command: Command) {
        let kind = match &command {
            Command::Stimulate { .. } => "stimulate",
            Command::Reward { .. } => "reward",
            Command::Chat { .. } => "chat",
            Command::Checkpoint { .. } => "checkpoint",
            Command::Pause { .. } => "pause",
            Command::Resume { .. } => "resume",
            Command::Shutdown { .. } => "shutdown",
        };
        self.received.lock().unwrap().push(kind);
        if self.hold.load(Ordering::SeqCst) {
            self.held.lock().unwrap().push(command);
            return;
        }
        self.answered.fetch_add(1, Ordering::SeqCst);
        match command {
            Command::Stimulate { by, reply, .. } => {
                let answer = match by.as_str() {
                    "pulse" => Err(Refusal::PulseActive {
                        retry_after_ms: 250,
                    }),
                    "spent" => Err(Refusal::RateLimited {
                        retry_after_ms: 41_500,
                    }),
                    "drop" => return,
                    "hang" => {
                        self.held.lock().unwrap().push(Command::Stimulate {
                            duration_ms: None,
                            by,
                            source: String::new(),
                            reply,
                        });
                        return;
                    }
                    _ => Ok(41),
                };
                let _ = reply.send(answer);
            }
            Command::Reward { reply, .. } => {
                let _ = reply.send(42);
            }
            Command::Chat {
                by, text, reply, ..
            } => {
                let answer = if text.contains("http") {
                    Err(ChatRefusal::Rejected(RejectReason::Url))
                } else if by == "fast" {
                    Err(ChatRefusal::RateLimited {
                        retry_after_ms: 1_999,
                    })
                } else {
                    Ok(43)
                };
                let _ = reply.send(answer);
            }
            Command::Checkpoint { reply } => {
                let answer = if self.fail_checkpoint.load(Ordering::SeqCst) {
                    Err("disk full".to_owned())
                } else {
                    Ok(12)
                };
                let _ = reply.send(answer);
            }
            Command::Pause { reply } => {
                let _ = reply.send(FeedStatus::Paused);
            }
            Command::Resume { reply } => {
                let _ = reply.send(FeedStatus::Running);
            }
            Command::Shutdown { reply } => {
                let _ = reply.send(());
            }
        }
    }
}

pub struct Rig {
    pub state: AppState,
    pub sim: Arc<FakeSim>,
    pub snapshots: watch::Sender<Arc<Snapshot>>,
    pub bus: EmbeddedBus,
    pub host: Option<ControlHost>,
    pub edge: BusBackend,
    pub metrics: Arc<EdgeMetrics>,
    pub config: EdgeConfig,
    pub dir: tempfile::TempDir,
    _log: EventLog,
}

/// A snapshot with a few fields that are not the boot defaults, so `/status` has content.
fn snapshot() -> Snapshot {
    let mut snapshot = booting_snapshot(4_242, 1_757_000_000_000, MacroMode::Raw);
    let header = &mut snapshot.header;
    header.status = FeedStatus::Running;
    header.realtime_factor = 1.0312;
    header.uptime_seconds = 612.5;
    header.brain_ms = 3_600_250.0;
    header.frame = 215_089;
    header.population_rate = 12.375;
    header.learning.signal = -0.125;
    snapshot
}

/// The rig, with `configure` applied to flysim's configuration.
pub async fn rig(configure: impl FnOnce(&mut flysim::config::Config)) -> Rig {
    rig_with(
        configure,
        Uses {
            feed: false,
            control: true,
        },
    )
    .await
}

pub async fn rig_with(configure: impl FnOnce(&mut flysim::config::Config), uses: Uses) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut config = flysim::config::Config::default();
    config.feed.bus_dir = dir.path().join("bus");
    config.control.via = flysim::config::ControlVia::Bus;
    configure(&mut config);
    let shared = Arc::new(Shared::new(config.clone(), EventRing::new()));
    let (commands, mut command_rx) = mpsc::channel::<Command>(flysim::simloop::COMMAND_QUEUE);
    let (snapshots, receiver) = watch::channel(Arc::new(snapshot()));
    let state = AppState {
        shared: Arc::clone(&shared),
        commands,
        snapshots: receiver,
    };
    // A short event log, so `/events` has pages to turn.
    let mut log = EventLog::open(&dir.path().join("events"), state.shared.events.clone()).unwrap();
    for index in 0..7u64 {
        log.append(
            1_000 + index,
            index as f64 * 0.5,
            NewEvent::new(FeedEventKind::System, format!("event {index}")).value(index as f64),
        );
    }
    state.shared.beat();

    let sim = Arc::new(FakeSim::default());
    {
        let sim = Arc::clone(&sim);
        tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                sim.answer(command);
            }
        });
    }

    let scope = Scope::live();
    let bus = flysim::bus::start(&config.feed.bus_dir, uses, &scope)
        .await
        .unwrap();
    let host = flysim::controlbus::serve(&bus.router, state.clone(), &scope)
        .await
        .unwrap();
    let edge_config = EdgeConfig {
        retry: Duration::from_millis(50),
        ..EdgeConfig::from_flysim(&config, None)
    };
    let metrics = Arc::new(EdgeMetrics::default());
    let edge = fly_control_edge::connect(&edge_config, &metrics)
        .await
        .unwrap();
    Rig {
        state,
        sim,
        snapshots,
        bus,
        host: Some(host),
        edge,
        metrics,
        config: edge_config,
        dir,
        _log: log,
    }
}

impl Rig {
    pub fn direct(&self) -> axum::Router {
        flysim::api::router(self.state.clone())
    }

    /// Close the rig's own edge connection, so an edge process can connect as the edge (the
    /// router admits one live connection per participant).
    pub async fn close_edge(&self) {
        self.edge.client().clone().close().await;
    }

    pub fn via_edge(&self) -> axum::Router {
        flysim::api::router_with(self.edge.clone())
    }
}

/// One HTTP exchange as the bytes a client sees: status, every header, body.
#[derive(Debug, PartialEq, Eq)]
pub struct Exchange {
    pub status: StatusCode,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: Vec<u8>,
}

impl Exchange {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

pub async fn exchange(
    router: axum::Router,
    method: &Method,
    uri: &str,
    content_type: Option<&str>,
    body: &[u8],
) -> Exchange {
    let mut request = Request::builder().method(method.clone()).uri(uri);
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    let request = request.body(Body::from(body.to_vec())).unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = sorted(response.headers());
    let body = axum::body::to_bytes(response.into_body(), 64 << 20)
        .await
        .unwrap()
        .to_vec();
    Exchange {
        status,
        headers,
        body,
    }
}

fn sorted(headers: &HeaderMap) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = headers
        .iter()
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();
    out.sort();
    out
}

/// A free loopback port (bound, read, released).
pub fn free_port() -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

/// A raw HTTP/1.1 exchange over TCP: the whole response, with the `date` header line removed
/// (the one header that differs from second to second). `None` when nothing listens.
pub async fn raw_http(addr: std::net::SocketAddr, request: &str) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.ok()?;
    let text = String::from_utf8_lossy(&out).into_owned();
    let kept: Vec<&str> = text
        .split("\r\n")
        .filter(|line| !line.to_ascii_lowercase().starts_with("date:"))
        .collect();
    Some(kept.join("\r\n").into_bytes())
}
