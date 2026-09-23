//! End-to-end: the real binary, a real cartridge, the real connectome.
//!
//! Gated on `FLY_ROM` and skips cleanly without it, the same convention the rest of the
//! workspace uses:
//!
//! ```sh
//! FLY_ROM="$HOME/fly-plays-pokemon/Pokemon Red (U) [S][BF].gb" \
//!   cargo test --release -p flysim --test integration -- --nocapture
//! ```
//!
//! What it proves, in one run, because these only mean anything together:
//!
//! 1. the service boots on ephemeral ports and answers `/healthz`;
//! 2. a WebSocket client gets 10 snapshots with all three attachments at their contract sizes,
//!    and every header validates against `packages/feed/src/schema.json`;
//! 3. `POST /stimulate` is accepted and shows up as a `sugar` event in the feed and in
//!    `GET /events`;
//! 4. `POST /checkpoint` commits before it answers;
//! 5. after a `SIGKILL` — so the forced checkpoint is the only thing on disk — a restart
//!    restores it and the feed resumes from the same frame counter rather than from a cold boot;
//! 6. `SIGTERM` writes a final checkpoint on the way out.
//!
//! `--nocapture` prints the measured feed rate and realtime factor.

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use flysim::snapshot::{AttachmentKind, FRAME_BYTES, FeedHeader};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// The prototype's cartridge, if `FLY_ROM` points at one.
fn rom_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("FLY_ROM")?);
    path.is_file().then_some(path)
}

fn dataset_path() -> Option<PathBuf> {
    let path = common::repo_root().join("data/fafb-v783");
    path.join("meta.json").is_file().then_some(path)
}

/// A free loopback port. Bound, read and released: a racing process could take it, which is why
/// [`Service::start`] retries.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("binding an ephemeral port")
        .local_addr()
        .expect("local address")
        .port()
}

struct Service {
    child: Child,
    feed_port: u16,
    control_port: u16,
}

impl Service {
    /// Start the real binary against `state_dir`, with the checkpoint intervals pushed out of
    /// the way so that the only checkpoints are the startup one and the ones this test forces.
    fn start(rom: &Path, dataset: &Path, state_dir: &Path) -> Self {
        Self::start_with_env(rom, dataset, state_dir, &[])
    }

    /// [`Service::start`] plus extra environment, for a test about a configuration rather than
    /// about the default one.
    fn start_with_env(
        rom: &Path,
        dataset: &Path,
        state_dir: &Path,
        extra: &[(&str, &str)],
    ) -> Self {
        let mut last = None;
        for _ in 0..5 {
            let feed_port = free_port();
            let control_port = free_port();
            let child = Command::new(env!("CARGO_BIN_EXE_flysim"))
                .env("FLY_ROM", rom)
                .env("FLY_DATASET", dataset)
                .env("FLY_STATE", state_dir.join("state"))
                .env("FLY_STATE_HOT", state_dir.join("hot"))
                .env("FLY_FEED_BIND", format!("127.0.0.1:{feed_port}"))
                .env("FLY_CONTROL_BIND", format!("127.0.0.1:{control_port}"))
                .env("FLYSIM_LOOP_HOT_SECONDS", "3600")
                .env("FLYSIM_LOOP_CHECKPOINT_SECONDS", "3600")
                // A deny list that does not exist yet, so the chat step can create it and
                // SIGHUP the service into reading it.
                .env("FLY_CHAT_DENY_LIST", state_dir.join("chat-deny.txt"))
                .env("FLYSIM_LOG", "flysim=info")
                .envs(extra.iter().copied())
                .env_remove("NOTIFY_SOCKET")
                .stdin(Stdio::null())
                // binjgb prints the cartridge header to stdout on every boot and flysim's own
                // logs go to stderr, so this is exactly where each stream ends up.
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("spawning flysim");
            let service = Self { child, feed_port, control_port };
            match service.wait_for_health(Duration::from_secs(240)) {
                Ok(()) => return service,
                Err(error) => last = Some(error),
            }
        }
        panic!("flysim never became healthy: {}", last.unwrap_or_default());
    }

    fn wait_for_health(&self, within: Duration) -> Result<(), String> {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if let Ok((200, _)) = http_blocking(self.control_port, "GET", "/healthz", None) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(format!("no 200 from /healthz within {within:?}"))
    }

    fn get(&self, path: &str) -> (u16, Value) {
        let (status, body) =
            http_blocking(self.control_port, "GET", path, None).expect("control API request");
        (status, serde_json::from_str(&body).unwrap_or(Value::String(body)))
    }

    fn post(&self, path: &str, body: Option<Value>) -> (u16, Value) {
        let text = body.map(|value| value.to_string());
        let (status, body) = http_blocking(self.control_port, "POST", path, text.as_deref())
            .expect("control API request");
        (status, serde_json::from_str(&body).unwrap_or(Value::String(body)))
    }

    fn feed_url(&self) -> String {
        format!("ws://127.0.0.1:{}/feed", self.feed_port)
    }

    /// `SIGHUP`: re-read the chat deny list, and nothing else.
    fn hangup(&self) {
        let status = Command::new("kill")
            .arg("-HUP")
            .arg(self.child.id().to_string())
            .status()
            .expect("kill -HUP");
        assert!(status.success());
    }

    /// `SIGKILL`: no clean shutdown, no final checkpoint, nothing flushed.
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// `SIGTERM`: the shutdown path, which takes a final durable checkpoint.
    fn terminate(&mut self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .expect("kill -TERM");
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                _ => {
                    self.kill();
                    return;
                }
            }
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A minimal blocking HTTP/1.1 client, so the test needs no HTTP client dependency.
fn http_blocking(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    use std::io::{BufRead, BufReader, Read};

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let payload = body.unwrap_or("");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
        payload.len()
    )?;
    stream.flush()?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line.trim().is_empty() {
            break;
        }
    }
    let mut rest = String::new();
    reader.read_to_string(&mut rest)?;
    Ok((status, rest))
}

/// Split one feed message into its header and its attachments, by the framing in
/// `docs/feed-protocol.md`.
fn split(bytes: &[u8]) -> (FeedHeader, Vec<(AttachmentKind, Vec<u8>)>) {
    let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let header: FeedHeader = serde_json::from_slice(&bytes[4..4 + length])
        .unwrap_or_else(|error| panic!("header is not a FeedHeader: {error}"));
    let mut offset = 4 + length;
    let mut attachments = Vec::new();
    for kind in &header.attachments {
        let size = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        attachments.push((*kind, bytes[offset..offset + size].to_vec()));
        offset += size;
    }
    assert_eq!(offset, bytes.len(), "trailing bytes in the message");
    (header, attachments)
}

const HELLO: &str = r#"{"protocol":1,"client":"test","wants":["frame","audio","spikes"]}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_service_streams_takes_sugar_checkpoints_and_resumes_after_being_killed() {
    let Some(rom) = rom_path() else {
        eprintln!("skipping: set FLY_ROM to a Game Boy cartridge to run this test");
        return;
    };
    let Some(dataset) = dataset_path() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let validator = jsonschema::validator_for(&common::header_schema()).expect("schema compiles");
    let dir = tempfile::tempdir().unwrap();

    // -- 1. boot ------------------------------------------------------------------------
    let boot_started = Instant::now();
    let mut service = Service::start(&rom, &dataset, dir.path());
    let boot = boot_started.elapsed();
    eprintln!("[timing] boot to first healthy /healthz: {:.1} s", boot.as_secs_f64());

    let (status, versions) = service.get("/status");
    assert_eq!(status, 200);
    assert_eq!(versions["version"]["kernel"], json!("lif-1ms-f64-v2"));
    assert_eq!(versions["version"]["adapter"], json!("pokered-unique8-v7"));
    assert!(
        versions["version"]["dataset"].as_str().unwrap_or_default().len() > 32,
        "the dataset fingerprint is in /status: {}",
        versions["version"]
    );
    assert_eq!(
        versions["checkpoint"]["generation"], json!(1),
        "the startup commit is generation 1: {}", versions["checkpoint"]
    );
    // `/status` is the feed header reshaped, so the ladder length reaches it without
    // a parallel struct: 38 rungs for Pokémon Red (`docs/design/ladder.md`), and a
    // rank inside them.
    assert_eq!(
        versions["milestone"]["total"], json!(38),
        "the ladder length is in /status: {}", versions["milestone"]
    );
    assert!(
        versions["milestone"]["rank"].as_u64().unwrap_or(u64::MAX) < 38,
        "rank is 0..total-1: {}", versions["milestone"]
    );

    // -- 2. ten snapshots with all three attachments -------------------------------------
    let (mut socket, _) = tokio_tungstenite::connect_async(service.feed_url())
        .await
        .expect("connecting to the feed");
    socket.send(Message::text(HELLO)).await.unwrap();

    let mut headers = Vec::new();
    let mut first_at = None;
    let connected_at = Instant::now();
    while headers.len() < 30 {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot within 10 s")
            .expect("the feed is still open")
            .expect("a readable message");
        let Message::Binary(bytes) = message else {
            continue;
        };
        let (header, attachments) = split(&bytes);
        let errors: Vec<String> = validator
            .iter_errors(&serde_json::to_value(&header).unwrap())
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "a live header failed the schema: {errors:?}");

        assert_eq!(header.protocol, 1);
        assert_eq!(header.status, flysim::snapshot::FeedStatus::Running);
        assert_eq!(
            attachments.iter().map(|(kind, _)| *kind).collect::<Vec<_>>(),
            AttachmentKind::ALL.to_vec(),
            "all three attachments, in header order"
        );
        for (kind, bytes) in &attachments {
            match kind {
                AttachmentKind::Frame => assert_eq!(bytes.len(), FRAME_BYTES),
                // 48 kHz stereo f32: about 1,600 frames per 30 Hz snapshot.
                AttachmentKind::Audio => {
                    assert_eq!(bytes.len() % 8, 0, "whole stereo f32 frames");
                    assert!(!bytes.is_empty(), "the emulator produced no audio");
                }
                AttachmentKind::Spikes => {
                    assert_eq!(bytes.len(), 139_255usize.div_ceil(8));
                    let set: u64 = bytes.iter().map(|byte| u64::from(byte.count_ones())).sum();
                    assert_eq!(set, header.spike_count, "spikeCount is the bitset's popcount");
                }
            }
        }
        if first_at.is_none() {
            first_at = Some(Instant::now());
        }
        headers.push(header);
    }
    let first_at = first_at.unwrap();
    // Measured from the headers' own `wallMs`, which is when the sim published them, not when
    // this client got round to reading them: a 122 KB snapshot sits in the socket buffer, so
    // arrival times compress and make the feed look faster than it is.
    let published_span_ms =
        (headers.last().unwrap().wall_ms - headers[0].wall_ms) as f64;
    let feed_hz = (headers.len() - 1) as f64 * 1000.0 / published_span_ms;
    let arrival_hz = (headers.len() - 1) as f64 / first_at.elapsed().as_secs_f64();
    eprintln!(
        "[timing] {} snapshots published over {:.3} s: {feed_hz:.2} Hz (contract: 30 Hz while\n\
         \x20         running; the ceiling is one snapshot per two 16.74 ms emulator frames,\n\
         \x20         29.86 Hz). Client-side arrival rate {arrival_hz:.2} Hz.",
        headers.len(),
        published_span_ms / 1000.0
    );
    eprintln!(
        "[timing] first snapshot {:.3} s after connecting",
        first_at.duration_since(connected_at).as_secs_f64()
    );
    assert!(
        (25.0..31.0).contains(&feed_hz),
        "the feed published at {feed_hz:.2} Hz, which is not the contract's 30 Hz"
    );
    for pair in headers.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1, "a snapshot was dropped between publishes");
    }
    let (_, metrics) = http_blocking(service.control_port, "GET", "/metrics", None).unwrap();
    for name in ["fly_lag_seconds", "fly_realtime_factor", "fly_feed_clients"] {
        let line = metrics
            .lines()
            .find(|line| line.starts_with(&format!("{name} ")))
            .unwrap_or_default();
        eprintln!("[metrics] {line}");
    }

    for pair in headers.windows(2) {
        assert!(pair[1].seq > pair[0].seq, "seq is monotonic");
        assert!(pair[1].frame >= pair[0].frame, "the frame counter never goes backwards");
        assert!(pair[1].brain_ms >= pair[0].brain_ms, "the brain clock never goes backwards");
    }
    let last = headers.last().unwrap();
    assert!(last.frame > headers[0].frame, "the emulator is advancing");
    assert!(
        (last.brain_ms - headers[0].brain_ms) > 0.0,
        "the brain clock is advancing"
    );
    let (_, status) = service.get("/status");
    let realtime_factor = status["realtimeFactor"].as_f64().unwrap_or_default();
    eprintln!("[timing] realtimeFactor on this box: {realtime_factor:.3}x");
    assert!(realtime_factor > 0.0, "the loop reports a realtime factor");

    // -- 3. sugar ------------------------------------------------------------------------
    let (status, body) = service.post(
        "/stimulate",
        Some(json!({ "durationMs": 400, "by": "integration-test", "source": "operator" })),
    );
    assert_eq!(status, 202, "{body}");
    let event_id = body["eventId"].as_u64().expect("an event id");

    let mut sugar = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while sugar.is_none() && Instant::now() < deadline {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot")
            .expect("open")
            .expect("readable");
        let Message::Binary(bytes) = message else {
            continue;
        };
        let (header, _) = split(&bytes);
        sugar = header
            .events
            .iter()
            .find(|event| event.kind == flysim::snapshot::FeedEventKind::Sugar)
            .cloned();
        if sugar.is_some() {
            assert!(header.sugar.active, "the pulse is being applied");
            assert!(header.sugar.remaining_ms > 0.0);
            assert_eq!(header.sugar.last_by.as_deref(), Some("integration-test"));
            assert_eq!(header.sugar.today_count, 1);
        }
    }
    let sugar = sugar.expect("a sugar event reached the feed");
    assert_eq!(sugar.id, event_id);
    assert_eq!(sugar.by.as_deref(), Some("integration-test"));
    assert_eq!(sugar.value, Some(400.0));
    assert_eq!(sugar.label, "integration-test fed the fly sugar");

    // The same event is in the log the bridge and the recap tooling read.
    let (_, events) = service.get(&format!("/events?since={}", event_id - 1));
    let logged = events["events"].as_array().expect("an events array");
    assert_eq!(logged[0]["id"], json!(event_id));
    assert_eq!(logged[0]["by"], json!("integration-test"));
    let on_disk = std::fs::read_to_string(dir.path().join("state/events.jsonl")).unwrap();
    assert!(on_disk.contains("integration-test fed the fly sugar"), "{on_disk}");
    // And in the hot directory's journal, stamped with the frame it was applied before, which
    // is what a shadow run replays (`flysim::journal`).
    let journal_path = dir.path().join("hot").join(flysim::journal::FILE_NAME);
    let journal = flysim::journal::read(&journal_path).expect("the sugar journal");
    assert_eq!(journal.len(), 1, "{journal:?}");
    assert_eq!(journal[0]["kind"], json!("sugar"));
    assert_eq!(journal[0]["eventId"], json!(event_id));
    assert_eq!(journal[0]["durationMs"], json!(400.0));
    let stamped: u64 =
        journal[0]["frame"].as_str().and_then(|frame| frame.parse().ok()).expect("a frame");
    assert!(stamped >= 1, "stamped with a frame the emulator has run: {stamped}");

    // A second pulse while the first is still being applied is refused, not stacked.
    let (status, body) = service.post(
        "/stimulate",
        Some(json!({ "by": "integration-test", "source": "chat" })),
    );
    if status == 429 {
        assert!(body["retryAfterMs"].as_f64().unwrap_or_default() > 0.0, "{body}");
    } else {
        assert_eq!(status, 202, "either the pulse had ended or it was refused: {body}");
    }

    // Reward is disabled by default, and that is a 403 rather than a silent no-op.
    let (status, body) = service.post(
        "/reward",
        Some(json!({ "value": 1.0, "by": "integration-test", "source": "operator" })),
    );
    assert_eq!(status, 403, "{body}");

    // -- 3b. on-screen chat --------------------------------------------------------------
    // The whole point of the chat path is that it reaches the header and nothing else: no
    // button, no reward, no simulation state. What this step proves is that an accepted line
    // shows up in `header.chat`, that the refusals are refusals, and that the deny list can be
    // reloaded on SIGHUP while the stream is live.
    let (_, before_chat) = service.get("/status");
    let frame_before_chat = before_chat["frame"].as_u64().unwrap_or_default();
    let sugar_before_chat = before_chat["sugar"]["todayCount"].clone();
    let (status_code, body) =
        service.post("/chat", Some(json!({ "by": "integration_test", "text": "  go   LEFT!  " })));
    assert_eq!(status_code, 202, "{body}");
    let chat_event_id = body["eventId"].as_u64().expect("an event id");

    let mut seen_line = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while seen_line.is_none() && Instant::now() < deadline {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot")
            .expect("open")
            .expect("readable");
        let Message::Binary(bytes) = message else {
            continue;
        };
        let (header, _) = split(&bytes);
        let errors: Vec<String> = validator
            .iter_errors(&serde_json::to_value(&header).unwrap())
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "a header carrying chat failed the schema: {errors:?}");
        seen_line = header
            .chat
            .as_ref()
            .and_then(|lines| lines.iter().find(|line| line.id == chat_event_id).cloned());
    }
    let line = seen_line.expect("the accepted line reached the feed header");
    assert_eq!(line.by, "integration_test");
    assert_eq!(line.text, "go LEFT!", "the sanitizer collapsed the whitespace");
    assert_eq!(line.bot, None, "a viewer line is not the bot's");

    // The event log records that a line was accepted, by whom, and nothing else: the text of a
    // chat message is never written to events.jsonl.
    let (_, events) = service.get(&format!("/events?since={}", chat_event_id - 1));
    let logged = &events["events"].as_array().expect("an events array")[0];
    assert_eq!(logged["id"], json!(chat_event_id));
    assert_eq!(logged["kind"], json!("viewer"));
    assert_eq!(logged["label"], json!("chat"));
    assert_eq!(logged["by"], json!("integration_test"));
    let on_disk = std::fs::read_to_string(dir.path().join("state/events.jsonl")).unwrap();
    assert!(on_disk.contains("\"label\":\"chat\""), "{on_disk}");
    assert!(!on_disk.contains("go LEFT!"), "chat text must never reach the event log");

    // A URL is a 422 naming the rule, and the bot's own reply is accepted under a different name.
    let (status_code, body) =
        service.post("/chat", Some(json!({ "by": "integration_test", "text": "see www.evil.tv" })));
    assert_eq!(status_code, 422, "{body}");
    assert_eq!(body["error"], json!("chat line refused: url"));
    let (status_code, body) = service.post(
        "/chat",
        Some(json!({ "by": "flybridgebot", "text": "Sugar from alex!", "bot": true })),
    );
    assert_eq!(status_code, 202, "{body}");

    // A second line from the same name inside two seconds is a 429 in /stimulate's currency.
    let (status_code, body) =
        service.post("/chat", Some(json!({ "by": "flybridgebot", "text": "again" })));
    assert_eq!(status_code, 429, "{body}");
    assert!(body["retryAfterMs"].as_f64().unwrap_or_default() > 0.0, "{body}");

    // The deny list is written now and SIGHUP makes the live service read it.
    std::fs::write(dir.path().join("chat-deny.txt"), "# operator-maintained\nspoiler\n").unwrap();
    service.hangup();
    let mut denied = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    while denied.is_none() && Instant::now() < deadline {
        let (status_code, body) = service.post(
            "/chat",
            Some(json!({ "by": "spoiler_free", "text": "SPOILER: it gets the badge" })),
        );
        if status_code == 422 && body["error"] == json!("chat line refused: deny_list") {
            denied = Some(body);
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    assert!(denied.is_some(), "SIGHUP did not make the deny list take effect");

    // None of that touched the simulation: the only thing that moved is the emulator, because
    // the loop kept running while we were posting.
    let (_, after_chat) = service.get("/status");
    assert!(after_chat["frame"].as_u64().unwrap_or_default() >= frame_before_chat);
    assert_eq!(
        after_chat["sugar"]["todayCount"], sugar_before_chat,
        "chat is not a sugar pulse"
    );

    // -- 4. force a checkpoint -----------------------------------------------------------
    // Sampled *before* the request, because the loop does not stop while a checkpoint is written:
    // only the state clone is on the sim thread, and the envelope encoding and the fsyncs are on
    // the writer thread, so `/status` afterwards reports a frame that is one or two past the frame
    // the checkpoint holds. Any frame the service had already reached before the request is a
    // sound floor for the restore, and a cold boot (frame 1) still fails it.
    let (_, before_checkpoint) = service.get("/status");
    let saved_frame = before_checkpoint["frame"].as_u64().unwrap();
    let saved_brain_ms = before_checkpoint["brainMs"].as_f64().unwrap();
    let saved_run_seconds = before_checkpoint["runSeconds"].as_f64().unwrap();
    let (status, body) = service.post("/checkpoint", None);
    assert_eq!(status, 200, "{body}");
    let generation = body["generation"].as_u64().expect("a generation");
    assert!(generation >= 2, "the startup commit was generation 1");
    let (_, status) = service.get("/status");
    assert_eq!(status["checkpoint"]["generation"], json!(generation));
    assert!(
        dir.path().join(format!("state/{generation}.checkpoint")).is_file(),
        "the forced checkpoint is on disk before the request answered"
    );

    // -- 5. SIGKILL and restart ----------------------------------------------------------
    // No clean shutdown, so the forced checkpoint is the newest thing on disk. The hot copy
    // interval was pushed to an hour, so nothing in tmpfs can mask it either.
    drop(socket);
    service.kill();

    let restart_started = Instant::now();
    let mut service = Service::start(&rom, &dataset, dir.path());
    eprintln!(
        "[timing] kill to healthy again: {:.1} s (no warm-up on a restore)",
        restart_started.elapsed().as_secs_f64()
    );

    let (mut socket, _) = tokio_tungstenite::connect_async(service.feed_url())
        .await
        .expect("reconnecting to the feed");
    socket.send(Message::text(HELLO)).await.unwrap();
    let mut resumed = None;
    while resumed.is_none() {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot after the restart")
            .expect("open")
            .expect("readable");
        if let Message::Binary(bytes) = message {
            resumed = Some(split(&bytes).0);
        }
    }
    let resumed = resumed.unwrap();
    eprintln!(
        "[timing] frame {saved_frame} at the forced checkpoint, frame {} on resume",
        resumed.frame
    );
    assert!(
        resumed.frame >= saved_frame,
        "the feed resumed at frame {} but the checkpoint held {saved_frame}: a cold boot, not a \
         restore",
        resumed.frame
    );
    assert!(
        resumed.frame < saved_frame + 2_000,
        "frame {} is implausibly far past the checkpoint's {saved_frame}",
        resumed.frame
    );
    assert!(resumed.brain_ms >= saved_brain_ms, "the brain clock continued");
    assert!(resumed.run_seconds >= saved_run_seconds, "runSeconds continued across the restart");
    assert_eq!(resumed.milestone.rank, status["milestone"]["rank"], "the rank came back");
    // And the restore is on the record, naming the generation the forced checkpoint wrote, so
    // this cannot pass by accident on a run that happened to look similar.
    let log = std::fs::read_to_string(dir.path().join("state/events.jsonl")).unwrap();
    assert!(
        log.contains(&format!("Restored from durable latest generation {generation}")),
        "the event log does not record the restore:\n{log}"
    );
    assert_eq!(
        log.matches("Fresh start").count(),
        1,
        "only the first instance may warm up; the second must restore:\n{log}"
    );

    // The CHAT panel came back with the service. The ring is session state in a sidecar beside
    // the hot checkpoints, never a chunk in the envelope: the durable store holds no copy of it,
    // and the checkpoint this restore just read carries no chat text.
    let resumed_chat = resumed
        .chat
        .as_ref()
        .expect("chat is enabled, so the header carries a ring");
    let restored_line = resumed_chat
        .iter()
        .find(|line| line.id == chat_event_id)
        .unwrap_or_else(|| panic!("the chat ring did not survive the restart: {resumed_chat:?}"));
    assert_eq!(restored_line.by, "integration_test");
    assert_eq!(restored_line.text, "go LEFT!");
    assert!(
        resumed_chat.iter().any(|line| line.bot == Some(true)),
        "the bot's line came back too: {resumed_chat:?}"
    );
    assert!(
        dir.path().join("hot/chat-ring.json").is_file(),
        "the sidecar lives beside the hot checkpoints"
    );
    assert!(
        !dir.path().join("state/chat-ring.json").exists(),
        "the durable store carries no chat"
    );
    let envelope =
        std::fs::read(dir.path().join(format!("state/{generation}.checkpoint"))).unwrap();
    assert!(
        !envelope.windows(8).any(|window| window == b"go LEFT!"),
        "chat text must never enter the checkpoint envelope"
    );

    // -- 6. SIGTERM writes a final checkpoint --------------------------------------------
    let (_, before) = service.get("/status");
    let before = before["checkpoint"]["generation"].as_u64().unwrap();
    drop(socket);
    service.terminate();
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("state/manifest.json")).unwrap(),
    )
    .unwrap();
    assert!(
        manifest["latest"].as_u64().unwrap() > before,
        "a clean shutdown commits: manifest {manifest}, generation before {before}"
    );
    assert!(std::fs::read_to_string(dir.path().join("state/events.jsonl"))
        .unwrap()
        .contains("Shutting down"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bridge_client_that_wants_no_attachments_gets_header_only_snapshots() {
    let Some(rom) = rom_path() else {
        eprintln!("skipping: set FLY_ROM to a Game Boy cartridge to run this test");
        return;
    };
    let Some(dataset) = dataset_path() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let service = Service::start(&rom, &dataset, dir.path());

    let (mut socket, _) = tokio_tungstenite::connect_async(service.feed_url())
        .await
        .expect("connecting to the feed");
    socket
        .send(Message::text(r#"{"protocol":1,"client":"bridge","wants":[]}"#))
        .await
        .unwrap();

    let mut sizes = Vec::new();
    while sizes.len() < 5 {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot")
            .expect("open")
            .expect("readable");
        if let Message::Binary(bytes) = message {
            let (header, attachments) = split(&bytes);
            assert!(attachments.is_empty(), "the bridge asked for nothing");
            assert_eq!(header.spike_count, 0, "and so has no bitset to count");
            sizes.push(bytes.len());
        }
    }
    let largest = sizes.iter().max().copied().unwrap_or_default();
    eprintln!("[timing] header-only snapshot size: {largest} bytes");
    assert!(largest < 4_096, "the header stays under 4 KB: {largest}");

    // A hello with the wrong protocol version is closed, not served.
    let (mut socket, _) = tokio_tungstenite::connect_async(service.feed_url()).await.unwrap();
    socket
        .send(Message::text(r#"{"protocol":2,"client":"test","wants":[]}"#))
        .await
        .unwrap();
    let closed = match tokio::time::timeout(Duration::from_secs(5), socket.next()).await {
        Ok(Some(Ok(Message::Close(frame)))) => frame,
        Ok(Some(Ok(message))) => {
            panic!("a rejected client must not be sent anything: {message:?}")
        }
        // A reset connection is the same refusal seen from one layer down.
        Ok(Some(Err(_)) | None) | Err(_) => None,
    };
    if let Some(frame) = closed {
        assert_eq!(u16::from(frame.code), 1002, "{frame:?}");
    }
}

/// Raw mode is the default and nothing about the macro palette touches it.
///
/// The point of this test is what it does *not* find. `docs/design/macros.md` section 1: "Raw mode
/// remains and is the default until measured". The macro module is compiled into this binary, the
/// executor and the scene detector are linked, and none of it may reach the button register or the
/// feed unless the configuration asks for it.
///
/// The byte-for-byte half of that claim is proved elsewhere and differently: the golden and oracle
/// suites (`crates/flybrain-core/tests/golden_*.rs`) pin the kernel and the readout, and
/// `--print-compatibility` is unchanged, so no checkpoint moves. This test is the loop-level half:
/// the service really boots in raw mode with the palette linked in, and reports nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_mode_is_the_default_and_the_macro_palette_reaches_nothing() {
    let Some(rom) = rom_path() else {
        eprintln!("skipping: set FLY_ROM to a Game Boy cartridge to run this test");
        return;
    };
    let Some(dataset) = dataset_path() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    // No FLY_MACRO_MODE at all: the default is what is under test.
    let service = Service::start(&rom, &dataset, dir.path());

    let (mut socket, _) = tokio_tungstenite::connect_async(service.feed_url())
        .await
        .expect("connecting to the feed");
    socket.send(Message::text(HELLO)).await.unwrap();

    let mut frames = Vec::new();
    while frames.len() < 10 {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot")
            .expect("open")
            .expect("readable");
        if let Message::Binary(bytes) = message {
            let (header, _) = split(&bytes);
            let game = serde_json::to_value(&header.game).unwrap();
            assert_eq!(game["macroMode"], "raw", "raw is the default");
            assert_eq!(game["scene"], "unknown", "raw mode deals no palette, so it claims no scene");
            assert_eq!(game["palette"], json!([]), "and publishes no cells");
            assert_eq!(game["macro"], Value::Null);
            assert_eq!(game["macroOutcome"], Value::Null);
            assert!(
                header.events.iter().all(|event| {
                    serde_json::to_value(event.kind).unwrap() != "macro"
                }),
                "no macro can run in raw mode: {:?}",
                header.events
            );
            frames.push(header.frame);
        }
    }
    assert!(
        frames.last() > frames.first(),
        "the loop advances in raw mode with the palette compiled in: {frames:?}"
    );

    // `/status` mirrors the same five fields, because it reshapes the header itself.
    let (status, body) = service.get("/status");
    assert_eq!(status, 200);
    assert_eq!(body["game"]["macroMode"], "raw");
    assert_eq!(body["game"]["scene"], "unknown");
    assert_eq!(body["game"]["palette"], json!([]));
    assert_eq!(body["game"]["macro"], Value::Null);
    assert_eq!(body["game"]["macroOutcome"], Value::Null);

    // And nothing macro-shaped reached the event log either.
    let (status, events) = service.get("/events?since=0&limit=200");
    assert_eq!(status, 200);
    let kinds: Vec<&str> = events["events"]
        .as_array()
        .map(|events| events.iter().filter_map(|event| event["kind"].as_str()).collect())
        .unwrap_or_default();
    assert!(!kinds.contains(&"macro"), "{kinds:?}");
}

/// Palette mode boots, publishes the scene and the mode, and leaves the intro to the readout.
///
/// `docs/design/macros.md` section 2: the title screen has no palette and the readout's boot
/// variant applies there, so a service that starts in macros mode looks exactly like raw mode
/// until the cartridge is playable — which is the state this test can reach in the seconds it has.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn macros_mode_publishes_the_scene_and_deals_no_palette_on_the_title() {
    let Some(rom) = rom_path() else {
        eprintln!("skipping: set FLY_ROM to a Game Boy cartridge to run this test");
        return;
    };
    let Some(dataset) = dataset_path() else {
        eprintln!("skipping: data/fafb-v783 is not present in this checkout");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let service =
        Service::start_with_env(&rom, &dataset, dir.path(), &[("FLY_MACRO_MODE", "macros")]);

    let (mut socket, _) = tokio_tungstenite::connect_async(service.feed_url())
        .await
        .expect("connecting to the feed");
    socket.send(Message::text(HELLO)).await.unwrap();

    const SCENES: [&str; 9] = [
        "title",
        "overworld",
        "dialog",
        "menu",
        "battle",
        "battle-switch",
        "shop",
        "pc",
        "unknown",
    ];
    let mut frames = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    while frames.len() < 10 {
        let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await
            .expect("a snapshot")
            .expect("open")
            .expect("readable");
        if let Message::Binary(bytes) = message {
            let (header, _) = split(&bytes);
            let game = serde_json::to_value(&header.game).unwrap();
            assert_eq!(game["macroMode"], "macros");
            let scene = game["scene"].as_str().expect("a scene").to_string();
            assert!(SCENES.contains(&scene.as_str()), "{scene:?} is not a protocol scene");
            let palette = game["palette"].as_array().expect("an array").clone();
            assert!(palette.len() <= 6, "at most six cells: {palette:?}");
            for cell in &palette {
                let slot = cell["slot"].as_u64().expect("a slot");
                assert!(slot < 6, "{cell:?}");
                assert!(!cell["name"].as_str().unwrap_or_default().is_empty(), "{cell:?}");
                assert!(!cell["gloss"].as_str().unwrap_or_default().is_empty(), "{cell:?}");
                let channel = cell["channel"].as_str().unwrap_or_default();
                assert!(channel.starts_with("MB·"), "{cell:?}");
            }
            if scene == "title" {
                assert!(palette.is_empty(), "the title screen binds nothing: {palette:?}");
                assert_eq!(game["macro"], Value::Null, "and runs no macro");
            }
            seen.insert(scene);
            frames.push(header.frame);
        }
    }
    assert!(frames.last() > frames.first(), "the loop advances in macros mode: {frames:?}");
    eprintln!("[timing] scenes seen in macros mode: {seen:?}");

    let (status, body) = service.get("/status");
    assert_eq!(status, 200);
    assert_eq!(body["game"]["macroMode"], "macros");
    assert!(body["game"]["scene"].is_string());
    assert!(body["game"]["palette"].is_array());
}

/// Macros mode over a game with no palette is refused at startup, not downgraded.
#[test]
fn macros_mode_over_the_platformer_refuses_to_start() {
    let output = Command::new(env!("CARGO_BIN_EXE_flysim"))
        .env("FLY_GAME", "platformer")
        .env("FLY_MACRO_MODE", "macros")
        .env("FLYSIM_LOG", "flysim=error")
        .arg("--check-config")
        .output()
        .expect("running flysim --check-config");
    assert!(!output.status.success(), "it must not accept the configuration");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no macro palette"), "{stderr}");
}
