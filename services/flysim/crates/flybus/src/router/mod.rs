//! The embeddable router: connection tasks around the [`state`] machine.
//!
//! Each connection has a reader task, which decodes one command at a time and applies it under
//! the state lock, and a writer task, which pulls its next frame from the state and writes it
//! with the lock released. Staging creation and seal copies run on the blocking pool; seals
//! run beside the reader, so a large copy never holds up that client's releases. The router
//! never waits on a subscriber: a slow reader only stalls its own writer task.

mod state;

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use state::RouterStats;
use state::{ConnKey, ConnSignals, NextFrame, Outcome, State};

use crate::limits::Limits;
use crate::policy::Policy;
use crate::store::{SealFailure, Store};
use crate::transport::{Stream, Transport};
use crate::wire::{Envelope, FrameError, contract_digest, hex, read_frame, write_frame};

/// How the launcher configures a router.
#[derive(Clone, Debug)]
pub struct RouterConfig {
    /// Directory under which the store creates its per-incarnation directory. Put it on tmpfs
    /// for transient media. Clients must be given the same root.
    pub store_root: PathBuf,
    pub limits: Limits,
    pub policy: Policy,
    /// Maximum time an accepted transport may remain pending before completing `bus.hello`.
    pub hello_timeout: Duration,
}

impl RouterConfig {
    /// Default limits and a closed policy: add clients with [`Policy::client`].
    pub fn new(store_root: impl Into<PathBuf>) -> RouterConfig {
        RouterConfig {
            store_root: store_root.into(),
            limits: Limits::default(),
            policy: Policy::closed(),
            hello_timeout: Duration::from_secs(5),
        }
    }
}

struct Inner {
    state: Mutex<State>,
    store: Store,
    store_root: PathBuf,
    router_id: String,
    store_id: String,
    stop: watch::Sender<bool>,
    hello_timeout: Duration,
}

impl Inner {
    /// Runs `f` under the state lock, then unlinks whatever it released, outside the lock.
    fn with_state<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let r = f(&mut st);
        let unlinks = st.take_unlinks();
        drop(st);
        for rel in unlinks {
            self.store.remove(&rel);
        }
        r
    }
}

/// A router and its artifact store. Cheap to clone; every clone is the same router.
#[derive(Clone)]
pub struct Router {
    inner: Arc<Inner>,
}

fn fresh_tag() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut h = Sha256::new();
    h.update(std::process::id().to_le_bytes());
    h.update(nanos.to_le_bytes());
    h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    hex(&h.finalize()[..8])
}

impl Router {
    /// Creates the store directory (deleting orphans of stopped routers under the same root)
    /// and a router with a fresh `routerId`/`storeId`.
    pub fn new(config: RouterConfig) -> io::Result<Router> {
        config.limits.validate().map_err(io::Error::other)?;
        let tag = fresh_tag();
        let router_id = format!("router-{tag}");
        let store_id = format!("store-{tag}");
        let store = Store::create(&config.store_root, &store_id)?;
        let state = State::new(
            router_id.clone(),
            store_id.clone(),
            contract_digest(),
            config.limits,
            config.policy,
        );
        Ok(Router {
            inner: Arc::new(Inner {
                state: Mutex::new(state),
                store,
                store_root: config.store_root,
                router_id,
                store_id,
                stop: watch::Sender::new(false),
                hello_timeout: config.hello_timeout,
            }),
        })
    }

    pub fn router_id(&self) -> &str {
        &self.inner.router_id
    }

    pub fn store_id(&self) -> &str {
        &self.inner.store_id
    }

    pub fn store_root(&self) -> &Path {
        &self.inner.store_root
    }

    /// `<store_root>/<store_id>`.
    pub fn store_dir(&self) -> &Path {
        self.inner.store.dir()
    }

    pub fn stats(&self) -> RouterStats {
        self.inner.with_state(|s| s.stats())
    }

    /// Serves an explicitly trusted, unbound connection. This is refused unless the router
    /// uses [`Policy::open`]; Hello identity is self-asserted in this mode.
    pub fn serve<S: Stream>(&self, stream: S) {
        tokio::spawn(run_connection(self.inner.clone(), stream, None));
    }

    /// Serves one launcher-bound participant. Hello must name `expected_client_id`.
    pub fn serve_as<S: Stream>(&self, stream: S, expected_client_id: &str) {
        tokio::spawn(run_connection(
            self.inner.clone(),
            stream,
            Some(expected_client_id.to_owned()),
        ));
    }

    /// A connected in-memory transport for a client in this process.
    pub fn connect_in_memory(&self) -> Transport {
        let (client, router) = Transport::pair();
        self.serve(router);
        client
    }

    /// A connected in-memory transport bound to `expected_client_id` out of band.
    pub fn connect_in_memory_as(&self, expected_client_id: &str) -> Transport {
        let (client, router) = Transport::pair();
        self.serve_as(router, expected_client_id);
        client
    }

    /// Listens on a new Unix-domain socket (mode 0600) until the returned handle is dropped or
    /// the router shuts down.
    pub async fn listen_unix(&self, path: impl AsRef<Path>) -> io::Result<UnixListenerHandle> {
        self.listen_unix_inner(path.as_ref(), None).await
    }

    /// Listens on a launcher-created endpoint dedicated to `expected_client_id`.
    pub async fn listen_unix_as(
        &self,
        path: impl AsRef<Path>,
        expected_client_id: &str,
    ) -> io::Result<UnixListenerHandle> {
        self.listen_unix_inner(path.as_ref(), Some(expected_client_id.to_owned()))
            .await
    }

    async fn listen_unix_inner(
        &self,
        path: &Path,
        expected_client_id: Option<String>,
    ) -> io::Result<UnixListenerHandle> {
        let path = path.to_path_buf();
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let inner = self.inner.clone();
        let mut stop = self.inner.stop.subscribe();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            tokio::spawn(run_connection(inner.clone(), stream, expected_client_id.clone()));
                        }
                        // Out of descriptors and the like: back off rather than spin.
                        Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                    },
                    _ = stopped(&mut stop) => break,
                }
            }
        });
        Ok(UnixListenerHandle { path, task })
    }

    /// Closes every connection, releases retained values and refuses new connections. A
    /// connection at a frame boundary gets final notices; one inside a partial frame is closed
    /// without appending another frame. The store directory is removed once the last connection
    /// task and seal have finished and every `Router` clone is dropped.
    pub fn shutdown(&self) {
        self.inner.stop.send_replace(true);
        self.inner.with_state(|s| s.shutdown());
    }
}

/// Stops the listener and removes its socket file when dropped.
pub struct UnixListenerHandle {
    path: PathBuf,
    task: JoinHandle<()>,
}

impl UnixListenerHandle {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UnixListenerHandle {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Resolves once the flag is true (or its sender is gone).
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

async fn run_connection<S: Stream>(
    inner: Arc<Inner>,
    stream: S,
    expected_client_id: Option<String>,
) {
    let signals = Arc::new(ConnSignals::new());
    let Some(c) = inner.with_state(|s| s.add_conn(signals.clone(), expected_client_id)) else {
        return;
    };
    let (rd, wr) = tokio::io::split(stream);
    let writer = tokio::spawn(write_loop(inner.clone(), c, signals.clone(), wr));
    read_loop(&inner, c, &signals, rd).await;
    // A no-op if the state already closed it.
    inner.with_state(|s| s.close_conn(c, []));
    let _ = writer.await;
}

async fn read_loop<R: AsyncRead + Unpin>(
    inner: &Arc<Inner>,
    c: ConnKey,
    signals: &ConnSignals,
    mut rd: R,
) {
    let mut shutdown = signals.shutdown.subscribe();
    let hello_deadline = tokio::time::sleep(inner.hello_timeout);
    tokio::pin!(hello_deadline);
    let mut negotiated = false;
    let mut handled: u32 = 0;
    loop {
        let frame = tokio::select! {
            f = read_frame(&mut rd) => f,
            _ = stopped(&mut shutdown) => return,
            _ = &mut hello_deadline, if !negotiated => {
                inner.with_state(|s| s.reject_frame(c, "bus.hello deadline expired".into()));
                return;
            }
        };
        let bytes = match frame {
            Ok(Some(b)) => b,
            Ok(None) | Err(FrameError::Io(_)) | Err(FrameError::Truncated) => return,
            Err(e) => {
                inner.with_state(|s| s.reject_frame(c, e.to_string()));
                return;
            }
        };
        let env = match Envelope::decode(&bytes) {
            Ok(env) => env,
            Err(e) => {
                inner.with_state(|s| s.reject_frame(c, e.0));
                return;
            }
        };
        let was_hello = !negotiated && env.op == "bus.hello";
        match inner.with_state(|s| s.handle(c, env)) {
            Outcome::Done => {}
            Outcome::Close => return,
            Outcome::Allocate(job) => {
                let (i, serial, len) = (inner.clone(), job.serial, job.len);
                let created =
                    tokio::task::spawn_blocking(move || i.store.create_staging(serial, len))
                        .await
                        .unwrap_or_else(|e| Err(io::Error::other(e)));
                inner.with_state(|s| s.finish_allocate(job, created));
            }
            Outcome::Seal(job) => {
                let inner = inner.clone();
                tokio::spawn(async move {
                    let (i, serial, len, digest) =
                        (inner.clone(), job.serial, job.len, job.digest.clone());
                    let result = tokio::task::spawn_blocking(move || {
                        let r = i.store.seal(serial, len, digest.as_deref());
                        // Staging is finished with either way; unlink it before anyone can
                        // see the outcome.
                        i.store.remove(&crate::store::staging_rel(serial));
                        r
                    })
                    .await
                    .unwrap_or_else(|e| Err(SealFailure::Io(io::Error::other(e))));
                    inner.with_state(|s| s.finish_seal(job, result));
                });
            }
        }
        if was_hello {
            negotiated = true;
        }
        handled = handled.wrapping_add(1);
        if handled.is_multiple_of(32) {
            // Fairness: a client flooding commands yields to the others.
            tokio::task::yield_now().await;
        }
    }
}

/// How long a closing connection gets to write final notices and shut down the transport.
const CLOSING_WRITE: Duration = Duration::from_secs(2);

enum SelectedWrite {
    Complete,
    Closing { partial: bool },
    Failed,
}

/// Writes one selected frame while serializing each synchronous transport poll with teardown.
/// The standard mutex is released after every poll and is never held across an await.
async fn write_selected<W: AsyncWrite + Unpin>(
    wr: &mut W,
    bytes: &[u8],
    signals: &ConnSignals,
) -> SelectedWrite {
    let mut frame = Vec::with_capacity(bytes.len() + 4);
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    {
        let mut gate = signals.write_gate.lock().unwrap_or_else(|e| e.into_inner());
        if !gate.begin_frame(frame.len()) {
            return SelectedWrite::Closing {
                partial: gate.cut_partial(),
            };
        }
    }

    let mut written = 0;
    while written < frame.len() {
        let polled = std::future::poll_fn(|cx| {
            let mut gate = signals.write_gate.lock().unwrap_or_else(|e| e.into_inner());
            if gate.closing() {
                return std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "connection closing",
                )));
            }
            match std::pin::Pin::new(&mut *wr).poll_write(cx, &frame[written..]) {
                std::task::Poll::Ready(Ok(0)) => std::task::Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write router frame",
                ))),
                std::task::Poll::Ready(Ok(n)) => {
                    gate.wrote(n);
                    std::task::Poll::Ready(Ok(n))
                }
                other => other,
            }
        })
        .await;
        match polled {
            Ok(n) => written += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                let gate = signals.write_gate.lock().unwrap_or_else(|e| e.into_inner());
                return SelectedWrite::Closing {
                    partial: gate.cut_partial(),
                };
            }
            Err(_) => return SelectedWrite::Failed,
        }
    }

    let flushed = std::future::poll_fn(|cx| {
        let gate = signals.write_gate.lock().unwrap_or_else(|e| e.into_inner());
        if gate.closing() {
            return std::task::Poll::Ready(Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "connection closing",
            )));
        }
        std::pin::Pin::new(&mut *wr).poll_flush(cx)
    })
    .await;
    if let Err(e) = flushed {
        if e.kind() == io::ErrorKind::Interrupted {
            let gate = signals.write_gate.lock().unwrap_or_else(|e| e.into_inner());
            return SelectedWrite::Closing {
                partial: gate.cut_partial(),
            };
        }
        return SelectedWrite::Failed;
    }
    signals
        .write_gate
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .finish_frame();
    SelectedWrite::Complete
}

async fn write_loop<W: AsyncWrite + Unpin>(
    inner: Arc<Inner>,
    c: ConnKey,
    signals: Arc<ConnSignals>,
    mut wr: W,
) {
    let mut shutdown = signals.shutdown.subscribe();
    // False once a frame was cut short: nothing more may be written on this stream.
    let mut aligned = true;
    loop {
        match inner.with_state(|s| s.next_frame(c)) {
            NextFrame::Frame(bytes) => {
                let write = write_selected(&mut wr, &bytes, &signals);
                tokio::pin!(write);
                let selected = tokio::select! {
                    result = &mut write => result,
                    _ = stopped(&mut shutdown) => {
                        let gate = signals
                            .write_gate
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        SelectedWrite::Closing { partial: gate.cut_partial() }
                    }
                };
                match selected {
                    SelectedWrite::Complete => {}
                    SelectedWrite::Closing { partial } => {
                        aligned = !partial;
                        break;
                    }
                    SelectedWrite::Failed => {
                        aligned = false;
                        inner.with_state(|s| s.close_conn(c, []));
                        break;
                    }
                }
            }
            NextFrame::Idle => {
                tokio::select! {
                    _ = signals.wake.notified() => {}
                    _ = stopped(&mut shutdown) => {}
                }
            }
            NextFrame::Gone => break,
        }
    }
    let finals = std::mem::take(
        &mut *signals
            .final_frames
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    if aligned {
        let _ = tokio::time::timeout(CLOSING_WRITE, async {
            for f in &finals {
                if write_frame(&mut wr, f).await.is_err() {
                    break;
                }
            }
        })
        .await;
    }
    let _ = tokio::time::timeout(CLOSING_WRITE, wr.shutdown()).await;
}
