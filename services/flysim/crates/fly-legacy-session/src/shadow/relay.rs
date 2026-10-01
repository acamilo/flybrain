//! `fly-shadow relay`: the release container's half of the remote shadow (SHADOW-02; the protocol
//! is [`super::remote`]). It runs as `flyshadow.service` in place of the local shadow, so
//! `fly-shadow-run check`, its guard and CUT-01 see the unit they always did, and it is a few
//! file reads and one ssh stream instead of a second brain on the stream's CPUs.
//!
//! Every tick (a quarter second) it pushes, in this order:
//!
//! 1. the new live saves in both stores (a hot one lives about ten seconds), so a trace line never
//!    arrives on the box before the save it names;
//! 2. the new bytes of every trace file of this run, oldest file first, listed *after* reading the
//!    sugar journal, and a file's size read after the listing: when a newer file exists the older
//!    process has exited and flushed, so the older file is complete when the newer one appears on
//!    the box;
//! 3. the sugar journal it read at the start of the tick, so every boot header the box sees has
//!    its trace file there already (the shadow's coverage check).
//!
//! Trace files older than the run (`FLY_SHADOW_RUN_ID` is the start's Unix ms) and saves written
//! more than a minute before it are not sent: they belong to processes this run never follows.
//!
//! **A stalled link.** A writer thread owns the ssh pipe, so the relay never blocks on it: it keeps
//! ticking, reports itself unhealthy and stops the heartbeat. New saves are still read the moment
//! they appear and held in memory ([`QUEUE_SAVES`], about 8 minutes of hot saves), because the
//! container keeps a hot save only about ten seconds; trace bytes wait on disk ([`QUEUE_TRACE`]),
//! and a newer trace file and the journal are held back until every older file is complete on its
//! way to the box (the box ends a segment when a newer file appears, and checks each boot header
//! against the files it has).
//!
//! **The heartbeat.** flysim traces only while `<trace dir>/consumer` is fresh. The relay touches
//! it every 10 s only while it is *healthy*: connected; the box reported within the last 30 s that
//! the shadow of this run is alive; and the box has acknowledged everything that was on the
//! container's disk a minute ago (`received` against the bytes sent by the end of each tick). A
//! dead box, a dead shadow, a broken or a stalled connection all stop the heartbeat, and flysim
//! then stops its trace as it would for a dead local shadow. The relay removes the file when it
//! stops.
//!
//! **The verdict.** The box's `verdict.json` is written to `/srv/fly/shadow/verdict.json` only
//! when it is this run's (`runId`), with `lagTransitions` raised by the live trace not yet on the
//! box and a `relay` member (the `fly-shadow-verdict-v1` amendment of 2026-09-30); `check`'s
//! catch-up and age bounds therefore cover the sync as well as the shadow. `relay.json` beside it
//! says whether the relay is healthy. A diverged verdict brings `divergence.json` and its
//! checkpoint files back, and then the relay exits with status 3, as a diverged local shadow does.
//! The box may send at most three divergence files, and only after a diverged verdict of this run
//! ([`DivergenceFiles`]): a faulty box cannot fill the container's disk.
//!
//! **Coverage lost.** A trace that flysim stopped (`"reason":"no-consumer"`, after ten minutes without a
//! heartbeat, or `"reason":"byte-cap"`) leaves the live fly running untraced, and the box's
//! catch-up bound counts trace *lines*, so a box that has not yet reached that line looks caught up
//! once the link is back. The relay therefore scans the trace bytes it forwards for that marker;
//! on finding it, it sets `coverageLost` in `relay.json` (sticky, also across a relay restart of the
//! same run) and reports itself unhealthy, which stops the heartbeat and makes `check` refuse. It
//! clears only when the box's verdict says its window starts after that stop (`window.lastStopTrace`
//! is the lost trace or a later one: the shadow ended its window at the stop, so its compared
//! brain time counts only from after the gap), never by the shadow catching up or restarting. A
//! relay that restarts mid-run ends the run's coverage (flysim stops tracing without a heartbeat)
//! and the loss is safe. `relay.json` also carries `traceAgeSeconds`, the age of the newest trace file, which
//! `check` bounds.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::remote::{self, Frame};
use super::spool::generation_of;
use super::{StopFlag, follow, verdict};

/// How the relay runs.
#[derive(Clone, Debug)]
pub struct RelayConfig {
    /// The live service's `FLY_TRACE_DIR` (and the heartbeat in it), its stores, and where the
    /// verdict goes (`FLY_SHADOW_DIR`).
    pub trace_dir: PathBuf,
    pub hot_dir: PathBuf,
    pub durable_dir: PathBuf,
    pub out_dir: PathBuf,
    /// The command whose stdin and stdout are the box's ingest: `ssh ... <box>`.
    pub command: Vec<String>,
    pub run_id: String,
    pub release: String,
    pub binaries: BTreeMap<String, String>,
    /// [`remote::FORWARDED_ENV`] as the live service has them.
    pub env: BTreeMap<String, String>,
    /// The sync may be at most this far behind the container's disk for the relay to be healthy.
    pub stall: Duration,
    /// The box's last report that the shadow is alive may be at most this old.
    pub alive_within: Duration,
    pub beat_every: Duration,
    pub tick: Duration,
    pub reconnect_after: Duration,
    /// How long to wait for the box's answer to `hello`.
    pub handshake: Duration,
}

impl RelayConfig {
    /// The defaults beside the directories, command and release.
    pub fn with_defaults(
        trace_dir: PathBuf,
        hot_dir: PathBuf,
        durable_dir: PathBuf,
        out_dir: PathBuf,
        command: Vec<String>,
        run_id: String,
    ) -> RelayConfig {
        RelayConfig {
            trace_dir,
            hot_dir,
            durable_dir,
            out_dir,
            command,
            run_id,
            release: String::new(),
            binaries: BTreeMap::new(),
            env: BTreeMap::new(),
            stall: Duration::from_secs(60),
            alive_within: Duration::from_secs(30),
            beat_every: Duration::from_secs(10),
            tick: Duration::from_millis(250),
            reconnect_after: Duration::from_secs(5),
            handshake: Duration::from_secs(120),
        }
    }
}

/// How the relay ended.
#[derive(Debug, PartialEq, Eq)]
pub enum RelayEnd {
    Stopped,
    /// The remote shadow diverged; its verdict and divergence files are in `out_dir`.
    Diverged,
}

/// Saves are read and queued for the box while less than this is waiting to be written. Hot saves
/// come at about [`SAVE_BYTES_PER_SECOND`], and flysim keeps the trace on for ten minutes of a
/// missing heartbeat, so this holds twelve minutes: a stall shorter than that loses no save the
/// shadow needs. The relay unit's `MemoryMax` (`fly-shadow-run`) leaves room for it.
pub const QUEUE_SAVES: u64 = 400 << 20;

/// What the live stores produce, measured (256 MiB is about 8 minutes).
pub const SAVE_BYTES_PER_SECOND: u64 = 560_000;

/// How long flysim keeps tracing without a heartbeat (`FLY_TRACE_CONSUMER_STALE_SECONDS`).
pub const STALE_WINDOW_SECONDS: u64 = 600;

/// Trace bytes (and the journal) are queued only while less than this is waiting.
pub const QUEUE_TRACE: u64 = 16 << 20;

/// One connection to the box.
struct Conn {
    child: Child,
    /// Frames for the writer thread, which owns the pipe; body bytes not yet written; the
    /// writer's error, once it has one.
    tx: Option<mpsc::Sender<(Value, Arc<Vec<u8>>)>>,
    queued: Arc<AtomicU64>,
    write_error: Arc<Mutex<Option<String>>>,
    frames: mpsc::Receiver<std::io::Result<Frame>>,
    /// Bytes of each trace file the box has (sent, or reported at the handshake).
    sent: BTreeMap<String, u64>,
    /// Sizes the box last acknowledged.
    acked: BTreeMap<String, u64>,
    /// Journal files sent: (length, modification ms), and the set last announced.
    journal_sent: BTreeMap<String, (u64, u64)>,
    journal_names: BTreeSet<String>,
    /// Body bytes sent on this connection, the ends of recent ticks, and the latest tick start
    /// whose bytes the box has all received.
    sent_total: u64,
    ticks: VecDeque<(Instant, u64)>,
    caught_up_at: Instant,
    /// The box's last status: when, and whether this run's shadow is alive.
    last_status: Option<(Instant, bool, String)>,
    last_frame_sent: Instant,
}

impl Conn {
    /// Queues a frame for the writer; never blocks. An error is the writer's (a broken link).
    fn send(&mut self, header: Value, body: Vec<u8>) -> std::io::Result<()> {
        self.send_shared(header, Arc::new(body))
    }

    /// [`Conn::send`] of a body the relay also keeps (a save not yet confirmed).
    fn send_shared(&mut self, header: Value, body: Arc<Vec<u8>>) -> std::io::Result<()> {
        if let Some(e) = self.write_error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return Err(std::io::Error::other(e));
        }
        let len = body.len() as u64;
        self.queued.fetch_add(len, Ordering::Relaxed);
        self.tx
            .as_ref()
            .ok_or_else(|| std::io::Error::other("closed"))?
            .send((header, body))
            .map_err(|_| std::io::Error::other("the writer stopped"))?;
        self.sent_total += len;
        self.last_frame_sent = Instant::now();
        Ok(())
    }

    /// Body bytes queued and not yet written to the link.
    fn queued(&self) -> u64 {
        self.queued.load(Ordering::Relaxed)
    }

    fn close(mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send((json!({"t": "bye"}), Arc::new(Vec::new())));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A save read from a live store and not yet confirmed by the box.
struct PendingSave {
    store: &'static str,
    generation: u64,
    bytes: Arc<Vec<u8>>,
    /// On the current connection: queued, with the connection's byte total after it (the box has
    /// it once `received` reaches that).
    queued_at: Option<u64>,
}

/// The relay's state across connections.
struct Relay {
    config: RelayConfig,
    stop: StopFlag,
    run_start_ms: Option<u64>,
    /// Trace files the box has finished with (its shadow deleted them): deleted here too.
    done: BTreeSet<String>,
    /// Generations read (or held by the box, or older than the run).
    sent_gens: BTreeSet<u64>,
    /// Saves read and not yet confirmed by the box, oldest first, and their bytes: they survive a
    /// broken connection and are sent again on the next ([`QUEUE_SAVES`] bounds them).
    pending: VecDeque<PendingSave>,
    pending_bytes: u64,
    /// Trace bytes and lines sent, for the mean line length.
    trace_bytes: u64,
    trace_lines: u64,
    last_beat: Option<Instant>,
    last_relay_json: Instant,
    diverged_at: Option<Instant>,
    have_divergence: bool,
    remote_lag: u64,
    /// Divergence files taken from the box ([`DivergenceFiles`]).
    files: DivergenceFiles,
    /// The trace (and the box shadow's window) at which a no-consumer stop was forwarded.
    coverage_lost: Option<CoverageLost>,
    /// The `startedAt` of the latest relayed verdict: the box shadow's window.
    window: Option<String>,
    /// The end of the last scanned byte of each trace file and its tail, so a marker split between
    /// two chunks is still found.
    scan_tail: BTreeMap<String, (u64, Vec<u8>)>,
}

/// Coverage of the live fly was lost: flysim stopped its trace (no consumer, or the byte cap).
#[derive(Clone, Debug, PartialEq, Eq)]
struct CoverageLost {
    trace: String,
    /// The box shadow's window when it was seen; `None` when no verdict had arrived (never clears).
    window: Option<String>,
    at: String,
}

/// Whether trace file `a` started at or after `b` (by the start in its name; else by name).
fn trace_not_before(a: &str, b: &str) -> bool {
    match (
        super::trace_start_ms(Path::new(a)),
        super::trace_start_ms(Path::new(b)),
    ) {
        (Some(x), Some(y)) => x >= y,
        _ => a >= b,
    }
}

/// The reasons for which flysim stops a trace, as written in its `{"truncated":true,"reason":..}`
/// line: no consumer (a stale heartbeat) and the byte cap. Both leave the live fly running
/// untraced, so the relay treats them alike (SHADOW-02 r3 N4).
const STOP_MARKERS: [&[u8]; 2] = [b"\"reason\":\"no-consumer\"", b"\"reason\":\"byte-cap\""];

/// Whether `bytes` hold a whole `{"truncated":true,"reason":"no-consumer"|"byte-cap"}` line.
fn has_stop_marker(bytes: &[u8]) -> bool {
    STOP_MARKERS.iter().any(|marker| {
        let mut from = 0;
        while let Some(i) = bytes[from..].windows(marker.len()).position(|w| w == *marker) {
            let at = from + i;
            let start = bytes[..at].iter().rposition(|b| *b == b'\n').map_or(0, |n| n + 1);
            let end = bytes[at..].iter().position(|b| *b == b'\n').map_or(bytes.len(), |n| at + n);
            if bytes[start..end].windows(16).any(|w| w == b"\"truncated\":true") {
                return true;
            }
            from = at + 1;
        }
        false
    })
}

/// The divergence files the relay takes from the box: only after a diverged verdict of this run,
/// at most three (`divergence.json` and one checkpoint of each side), each of bounded size.
#[derive(Default)]
pub struct DivergenceFiles {
    names: BTreeSet<String>,
}

impl DivergenceFiles {
    pub const MAX_FILES: usize = 3;
    pub const MAX_JSON: usize = 4 << 20;
    pub const MAX_CHECKPOINT: usize = 16 << 20;

    /// Whether a file of this name and size may be written now.
    pub fn admit(&mut self, name: &str, len: usize, diverged: bool) -> bool {
        if !diverged || !remote::is_divergence_file(name) {
            return false;
        }
        let cap = if name == "divergence.json" {
            Self::MAX_JSON
        } else {
            Self::MAX_CHECKPOINT
        };
        if len > cap || (!self.names.contains(name) && self.names.len() >= Self::MAX_FILES) {
            return false;
        }
        self.names.insert(name.to_owned());
        true
    }
}

fn log(message: impl std::fmt::Display) {
    eprintln!("fly-shadow relay: {message}");
}

/// Runs the relay until it is stopped or the remote shadow diverges.
pub fn run(config: RelayConfig, stop: StopFlag) -> Result<RelayEnd, String> {
    if config.command.is_empty() {
        return Err("no command to reach the box".to_owned());
    }
    for dir in [&config.out_dir, &config.trace_dir] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut relay = Relay {
        run_start_ms: config.run_id.parse().ok(),
        config,
        stop,
        done: BTreeSet::new(),
        sent_gens: BTreeSet::new(),
        pending: VecDeque::new(),
        pending_bytes: 0,
        trace_bytes: 0,
        trace_lines: 0,
        last_beat: None,
        last_relay_json: Instant::now() - Duration::from_secs(60),
        diverged_at: None,
        have_divergence: false,
        remote_lag: 0,
        files: DivergenceFiles::default(),
        coverage_lost: None,
        window: None,
        scan_tail: BTreeMap::new(),
    };
    relay.load_coverage_lost();
    let heartbeat = relay.config.trace_dir.join(flysim::trace::CONSUMER_FILE);
    let ended = relay.run_loop();
    let _ = std::fs::remove_file(&heartbeat);
    relay.write_relay_json(None, false);
    ended
}

impl Relay {
    fn run_loop(&mut self) -> Result<RelayEnd, String> {
        loop {
            if self.stop.requested() {
                return Ok(RelayEnd::Stopped);
            }
            let conn = match self.connect() {
                Ok(conn) => conn,
                Err((message, wait)) => {
                    log(format!("{message}; retrying in {} s", wait.as_secs()));
                    self.write_relay_json(None, false);
                    if self.sleep(wait) {
                        return Ok(RelayEnd::Stopped);
                    }
                    continue;
                }
            };
            log("connected; following the live trace");
            match self.session(conn) {
                Ok(Some(end)) => return Ok(end),
                Ok(None) => {}
                Err(message) => log(format!("the connection ended: {message}")),
            }
            if self.stop.requested() {
                return Ok(RelayEnd::Stopped);
            }
            self.write_relay_json(None, false);
            if self.sleep(self.config.reconnect_after) {
                return Ok(RelayEnd::Stopped);
            }
        }
    }

    /// Sleeps `d`, or less when stopped (then `true`).
    fn sleep(&self, d: Duration) -> bool {
        let until = Instant::now() + d;
        while Instant::now() < until {
            if self.stop.requested() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.stop.requested()
    }

    /// Starts the command, says hello and reads the box's state.
    fn connect(&mut self) -> Result<Conn, (String, Duration)> {
        let retry = self.config.reconnect_after;
        let mut child = Command::new(&self.config.command[0])
            .args(&self.config.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| (format!("{}: {e}", self.config.command[0]), retry))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (out_tx, outgoing) = mpsc::channel::<(Value, Arc<Vec<u8>>)>();
        let queued = Arc::new(AtomicU64::new(0));
        let write_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        {
            let (queued, write_error) = (Arc::clone(&queued), Arc::clone(&write_error));
            std::thread::Builder::new()
                .name("fly-shadow-relay-write".to_owned())
                .spawn(move || {
                    let mut out = BufWriter::with_capacity(1 << 20, stdin);
                    for (header, body) in outgoing {
                        if let Err(e) = remote::write_frame(&mut out, header, &body) {
                            *write_error.lock().unwrap_or_else(|p| p.into_inner()) =
                                Some(e.to_string());
                            return;
                        }
                        queued.fetch_sub(body.len() as u64, Ordering::Relaxed);
                    }
                })
                .expect("the writer thread starts");
        }
        let (tx, frames) = mpsc::channel();
        std::thread::Builder::new()
            .name("fly-shadow-relay-read".to_owned())
            .spawn(move || {
                let mut reader = BufReader::with_capacity(1 << 20, stdout);
                loop {
                    match remote::read_frame(&mut reader) {
                        Ok(Some(frame)) => {
                            if tx.send(Ok(frame)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {
                            let _ = tx.send(Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "the box closed the connection",
                            )));
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    }
                }
            })
            .expect("the reader thread starts");
        let now = Instant::now();
        let mut conn = Conn {
            child,
            tx: Some(out_tx),
            queued,
            write_error,
            frames,
            sent: BTreeMap::new(),
            acked: BTreeMap::new(),
            journal_sent: BTreeMap::new(),
            journal_names: BTreeSet::new(),
            sent_total: 0,
            ticks: VecDeque::new(),
            caught_up_at: now,
            last_status: None,
            last_frame_sent: now,
        };
        let hello = json!({
            "t": "hello",
            "protocol": remote::PROTOCOL,
            "runId": self.config.run_id,
            "release": self.config.release,
            "binaries": self.config.binaries,
            "env": self.config.env,
        });
        if let Err(e) = conn.send(hello, Vec::new()) {
            conn.close();
            return Err((format!("sending hello: {e}"), retry));
        }
        let state = match conn.frames.recv_timeout(self.config.handshake) {
            Ok(Ok(frame)) if frame.kind() == "state" => frame,
            Ok(Ok(frame)) if frame.kind() == "error" => {
                let message = frame.header["message"].as_str().unwrap_or("").to_owned();
                conn.close();
                // A refusal (another release, another protocol) will not fix itself soon.
                return Err((
                    format!("the box refused: {message}"),
                    retry.max(Duration::from_secs(60)),
                ));
            }
            Ok(Ok(frame)) => {
                let kind = frame.kind().to_owned();
                conn.close();
                return Err((format!("expected state, got {kind:?}"), retry));
            }
            Ok(Err(e)) => {
                conn.close();
                return Err((format!("the handshake: {e}"), retry));
            }
            Err(_) => {
                conn.close();
                return Err(("no answer to hello".to_owned(), retry));
            }
        };
        let h = &state.header;
        if h["runId"].as_str() != Some(self.config.run_id.as_str()) {
            conn.close();
            return Err((format!("the box answered for the run {}", h["runId"]), retry));
        }
        for (name, size) in h["traces"].as_object().into_iter().flatten() {
            if let Some(size) = size.as_u64() {
                conn.sent.insert(name.clone(), size);
                conn.acked.insert(name.clone(), size);
            }
        }
        self.absorb_done(&h["done"]);
        // What the box holds, and what this relay still has pending (sent again on this
        // connection unless the box has it): a save queued on a connection that broke is not lost.
        let held: BTreeSet<u64> = h["gens"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .collect();
        self.pending.retain(|p| !held.contains(&p.generation));
        for p in self.pending.iter_mut() {
            p.queued_at = None;
        }
        self.pending_bytes = self.pending.iter().map(|p| p.bytes.len() as u64).sum();
        self.sent_gens = held;
        self.sent_gens
            .extend(self.pending.iter().map(|p| p.generation));
        Ok(conn)
    }

    /// Trace files the box has finished: delete them here, never send them again.
    fn absorb_done(&mut self, done: &Value) {
        for name in done.as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !remote::is_trace_name(name) {
                continue;
            }
            if self.done.insert(name.to_owned()) {
                let path = self.config.trace_dir.join(name);
                if path.is_file() && std::fs::remove_file(&path).is_ok() {
                    log(format!("{name} is finished on the box; removed here"));
                }
            }
        }
    }

    /// One connection's life. `Ok(Some(end))` ends the relay; `Ok(None)` or `Err` reconnects.
    fn session(&mut self, mut conn: Conn) -> Result<Option<RelayEnd>, String> {
        let result = loop {
            if self.stop.requested() {
                break Ok(Some(RelayEnd::Stopped));
            }
            let tick = Instant::now();
            if let Err(e) = self.push(&mut conn) {
                break Err(format!("sending: {e}"));
            }
            conn.ticks.push_back((tick, conn.sent_total));
            // Incoming frames.
            let mut ended = None;
            loop {
                match conn.frames.try_recv() {
                    Ok(Ok(frame)) => {
                        if let Err(e) = self.receive(&mut conn, frame) {
                            ended = Some(Err(e));
                            break;
                        }
                    }
                    Ok(Err(e)) => {
                        ended = Some(Err(e.to_string()));
                        break;
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        ended = Some(Err("the reader stopped".to_owned()));
                        break;
                    }
                }
            }
            if let Some(ended) = ended {
                break ended;
            }
            let healthy = self.healthy(&conn);
            if healthy
                && self
                    .last_beat
                    .is_none_or(|t| t.elapsed() >= self.config.beat_every)
            {
                let path = self.config.trace_dir.join(flysim::trace::CONSUMER_FILE);
                if let Err(e) = std::fs::write(&path, verdict::now_iso()) {
                    log(format!("could not write {}: {e}", path.display()));
                }
                self.last_beat = Some(Instant::now());
            }
            if self.last_relay_json.elapsed() >= Duration::from_secs(5) {
                self.write_relay_json(Some(&conn), healthy);
            }
            if let Some(at) = self.diverged_at
                && (self.have_divergence || at.elapsed() >= Duration::from_secs(60))
            {
                log("the remote shadow diverged: its verdict and divergence are in place");
                break Ok(Some(RelayEnd::Diverged));
            }
            if conn.last_frame_sent.elapsed() >= Duration::from_secs(5)
                && conn.queued() == 0
                && let Err(e) = conn.send(json!({"t": "ping"}), Vec::new())
            {
                break Err(format!("sending: {e}"));
            }
            let spent = tick.elapsed();
            if spent < self.config.tick {
                std::thread::sleep(self.config.tick - spent);
            }
        };
        conn.close();
        result
    }

    /// Healthy: connected, the box's shadow for this run alive lately, the sync caught up.
    fn healthy(&self, conn: &Conn) -> bool {
        let alive = conn.last_status.as_ref().is_some_and(|(at, alive, _)| {
            *alive && at.elapsed() <= self.config.alive_within
        });
        alive
            && conn.caught_up_at.elapsed() <= self.config.stall
            && self.diverged_at.is_none()
            && self.coverage_lost.is_none()
    }

    /// A previous relay of this run may have seen the stop: keep it (sticky).
    fn load_coverage_lost(&mut self) {
        let Ok(bytes) = std::fs::read(self.config.out_dir.join("relay.json")) else {
            return;
        };
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else {
            return;
        };
        if v["runId"].as_str() != Some(self.config.run_id.as_str()) {
            return;
        }
        let lost = &v["coverageLost"];
        if let Some(trace) = lost["trace"].as_str() {
            self.coverage_lost = Some(CoverageLost {
                trace: trace.to_owned(),
                window: lost["window"].as_str().map(str::to_owned),
                at: lost["at"].as_str().unwrap_or("").to_owned(),
            });
            self.window = lost["window"].as_str().map(str::to_owned);
        }
    }

    /// Writes `coverageLost` into `relay.json` and syncs it (file and directory).
    fn persist_coverage_lost(&self) {
        let path = self.config.out_dir.join("relay.json");
        let mut value = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|v| v["runId"].as_str() == Some(self.config.run_id.as_str()))
            .unwrap_or_else(|| {
                json!({"format": "fly-shadow-relay-v1", "runId": self.config.run_id, "healthy": false})
            });
        value["healthy"] = json!(false);
        value["coverageLost"] = self.coverage_lost.as_ref().map_or(
            Value::Null,
            |c| json!({"trace": c.trace, "window": c.window, "at": c.at}),
        );
        if let Err(e) = verdict::write_json_durable(&path, &value) {
            log(format!("could not write {}: {e}", path.display()));
        }
    }

    /// Looks for flysim's stop (no consumer, byte cap) in trace bytes about to be sent.
    fn scan_trace(&mut self, name: &str, offset: u64, chunk: &[u8]) {
        let mut buf = match self.scan_tail.remove(name) {
            Some((end, tail)) if end == offset => tail,
            _ => Vec::new(),
        };
        buf.extend_from_slice(chunk);
        if self.coverage_lost.is_none() && has_stop_marker(&buf) {
            log(format!(
                "{name}: flysim stopped its trace (no consumer or byte cap); coverage of the live fly \
                 is lost for this window"
            ));
            self.coverage_lost = Some(CoverageLost {
                trace: name.to_owned(),
                window: self.window.clone(),
                at: verdict::now_iso(),
            });
            // Durable before the chunk that holds the stop is sent: a relay that dies after
            // sending and before its next tick would otherwise resume past the stop unaware.
            self.persist_coverage_lost();
            self.last_relay_json = Instant::now() - Duration::from_secs(60);
        }
        let keep = buf.len().saturating_sub(256);
        self.scan_tail
            .insert(name.to_owned(), (offset + chunk.len() as u64, buf.split_off(keep)));
    }

    /// One tick's sends: saves, then trace bytes, then the journal read first.
    fn push(&mut self, conn: &mut Conn) -> std::io::Result<()> {
        // The journal, read before the trace directory is listed (coverage).
        let mut journal: Vec<(String, Vec<u8>, (u64, u64))> = Vec::new();
        let mut journal_names = BTreeSet::new();
        for entry in std::fs::read_dir(&self.config.hot_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !remote::is_journal_name(&name) {
                continue;
            }
            journal_names.insert(name.clone());
            let Ok(meta) = entry.metadata() else { continue };
            let signature = (meta.len(), remote::mtime_ms(&meta).unwrap_or(0));
            if conn.journal_sent.get(&name) == Some(&signature) {
                continue;
            }
            if let Ok(bytes) = std::fs::read(entry.path()) {
                journal.push((name, bytes, signature));
            }
        }
        // New saves.
        let floor_ms = self.run_start_ms.map(|ms| ms.saturating_sub(60_000));
        for (store, dir) in [
            ("hot", self.config.hot_dir.clone()),
            ("durable", self.config.durable_dir.clone()),
        ] {
            let mut found: Vec<(u64, PathBuf, Option<u64>)> = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let generation = generation_of(e.file_name().to_str()?)?;
                    let modified = e.metadata().ok().and_then(|m| remote::mtime_ms(&m));
                    Some((generation, e.path(), modified))
                })
                .collect();
            found.sort();
            for (generation, path, modified) in found {
                if self.sent_gens.contains(&generation) {
                    continue;
                }
                if self.pending_bytes >= QUEUE_SAVES {
                    break;
                }
                if let (Some(floor), Some(modified)) = (floor_ms, modified)
                    && modified < floor
                {
                    self.sent_gens.insert(generation);
                    continue;
                }
                // Rotated away since the listing: gone for good.
                let Ok(bytes) = std::fs::read(&path) else {
                    continue;
                };
                self.pending_bytes += bytes.len() as u64;
                self.pending.push_back(PendingSave {
                    store,
                    generation,
                    bytes: Arc::new(bytes),
                    queued_at: None,
                });
                self.sent_gens.insert(generation);
            }
        }
        // Every save not yet queued on this connection, in generation order, before any trace.
        let mut order: Vec<usize> = (0..self.pending.len())
            .filter(|i| self.pending[*i].queued_at.is_none())
            .collect();
        order.sort_by_key(|i| self.pending[*i].generation);
        for i in order {
            let p = &self.pending[i];
            let header = json!({"t": "ckpt", "store": p.store, "gen": p.generation});
            conn.send_shared(header, Arc::clone(&p.bytes))?;
            self.pending[i].queued_at = Some(conn.sent_total);
        }
        // Trace bytes: list, then read each size. A file that cannot be brought up to date now holds
        // back every newer file and the journal.
        let files = follow::trace_files(&self.config.trace_dir).unwrap_or_default();
        let mut complete = true;
        for path in files {
            if !complete {
                break;
            }
            let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                continue;
            };
            if self.done.contains(&name) || !remote::is_trace_name(&name) {
                continue;
            }
            if let (Some(start), Some(run)) = (super::trace_start_ms(&path), self.run_start_ms)
                && start < run
            {
                continue;
            }
            let Ok(size) = std::fs::metadata(&path).map(|m| m.len()) else {
                continue;
            };
            // A new file exists on the box at once, even while flysim's buffer still holds its
            // first bytes: its boot header may reach the box in this very tick (coverage).
            if !conn.sent.contains_key(&name) {
                conn.send(json!({"t": "trace", "name": name, "offset": 0}), Vec::new())?;
                conn.sent.insert(name.clone(), 0);
            }
            let mut offset = conn.sent.get(&name).copied().unwrap_or(0);
            if size <= offset {
                continue;
            }
            let Ok(mut file) = std::fs::File::open(&path) else {
                continue;
            };
            file.seek(SeekFrom::Start(offset))?;
            while offset < size {
                if conn.queued() >= QUEUE_TRACE {
                    complete = false;
                    break;
                }
                let want = (size - offset).min(remote::TRACE_CHUNK) as usize;
                let mut chunk = vec![0u8; want];
                file.read_exact(&mut chunk)?;
                self.trace_bytes += chunk.len() as u64;
                self.trace_lines += chunk.iter().filter(|b| **b == b'\n').count() as u64;
                self.scan_trace(&name, offset, &chunk);
                conn.send(json!({"t": "trace", "name": name, "offset": offset}), chunk)?;
                offset += want as u64;
                conn.sent.insert(name.clone(), offset);
            }
        }
        // The journal as read above, once every trace file it can name is on its way.
        if complete {
            for (name, bytes, signature) in journal {
                conn.send(json!({"t": "journal", "name": name}), bytes)?;
                conn.journal_sent.insert(name, signature);
            }
            if journal_names != conn.journal_names {
                conn.send(
                    json!({"t": "journal-set", "names": journal_names}),
                    Vec::new(),
                )?;
                conn.journal_names = journal_names;
            }
        }
        // Keep the set of sent generations bounded (the newest 2048, about 3 h of hot saves). An
        // older one still on disk is skipped again by its age, or at worst sent again (harmless:
        // the ingest rewrites the same file).
        if self.sent_gens.len() > 4096
            && let Some(&oldest) = self.sent_gens.iter().nth(self.sent_gens.len() - 2048)
        {
            self.sent_gens = self.sent_gens.split_off(&oldest);
        }
        Ok(())
    }

    fn receive(&mut self, conn: &mut Conn, frame: Frame) -> Result<(), String> {
        let h = &frame.header;
        match frame.kind() {
            "status" => {
                let received = h["received"].as_u64().unwrap_or(0);
                while conn
                    .ticks
                    .front()
                    .is_some_and(|(_, total)| *total <= received)
                {
                    let (at, _) = conn.ticks.pop_front().expect("a tick");
                    conn.caught_up_at = at;
                }
                // Saves the box now has.
                let before = self.pending.len();
                self.pending
                    .retain(|p| p.queued_at.is_none_or(|t| t > received));
                if self.pending.len() != before {
                    self.pending_bytes = self.pending.iter().map(|p| p.bytes.len() as u64).sum();
                }
                conn.acked.clear();
                for (name, size) in h["traces"].as_object().into_iter().flatten() {
                    if let Some(size) = size.as_u64() {
                        conn.acked.insert(name.clone(), size);
                    }
                }
                self.absorb_done(&h["done"]);
                let alive = h["alive"].as_bool().unwrap_or(false);
                let why = h["why"].as_str().unwrap_or("").to_owned();
                if !alive
                    && conn
                        .last_status
                        .as_ref()
                        .is_none_or(|(_, was, old)| *was || *old != why)
                {
                    log(format!("the remote shadow is not alive: {why}"));
                }
                conn.last_status = Some((Instant::now(), alive, why));
            }
            "resync" => {
                let name = h["name"].as_str().unwrap_or("");
                if remote::is_trace_name(name) {
                    let size = h["size"].as_u64().unwrap_or(0);
                    log(format!("the box has {size} bytes of {name}; resending from there"));
                    conn.sent.insert(name.to_owned(), size);
                }
            }
            "verdict" => self.relay_verdict(conn, &frame.body),
            "file" => {
                let name = h["name"].as_str().unwrap_or("");
                if !self
                    .files
                    .admit(name, frame.body.len(), self.diverged_at.is_some())
                {
                    log(format!(
                        "refused a file {name:?} ({} bytes) from the box",
                        frame.body.len()
                    ));
                } else {
                    let path = self.config.out_dir.join(name);
                    remote::atomic_write(&path, &frame.body)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if name == "divergence.json" {
                        self.have_divergence = true;
                    }
                }
            }
            "error" => return Err(format!("the box: {}", h["message"])),
            other => return Err(format!("an unknown frame {other:?}")),
        }
        Ok(())
    }

    /// Live trace bytes not yet on the box, and about how many transitions that is.
    fn unsynced(&self, conn: Option<&Conn>) -> (u64, u64) {
        let bytes: u64 = follow::trace_files(&self.config.trace_dir)
            .unwrap_or_default()
            .iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                if self.done.contains(&name) {
                    return None;
                }
                if let (Some(start), Some(run)) = (super::trace_start_ms(path), self.run_start_ms)
                    && start < run
                {
                    return None;
                }
                let size = std::fs::metadata(path).ok()?.len();
                let acked = conn.and_then(|c| c.acked.get(&name).copied()).unwrap_or(0);
                Some(size.saturating_sub(acked))
            })
            .sum();
        let mean = if self.trace_lines > 0 {
            (self.trace_bytes / self.trace_lines).max(1)
        } else {
            750
        };
        (bytes, bytes.div_ceil(mean))
    }

    /// Seconds since the newest trace file of this run was written (`None`: there is none).
    fn trace_age_seconds(&self) -> Option<u64> {
        follow::trace_files(&self.config.trace_dir)
            .unwrap_or_default()
            .iter()
            .filter(|path| {
                !matches!((super::trace_start_ms(path), self.run_start_ms),
                          (Some(start), Some(run)) if start < run)
            })
            .filter_map(|path| std::fs::metadata(path).ok()?.modified().ok())
            .max()
            .map(|t| {
                std::time::SystemTime::now()
                    .duration_since(t)
                    .unwrap_or_default()
                    .as_secs()
            })
    }

    /// Writes the box's verdict here, when it is this run's, with the sync's backlog added.
    fn relay_verdict(&mut self, conn: &Conn, bytes: &[u8]) {
        let Ok(mut v) = serde_json::from_slice::<Value>(bytes) else {
            log("the box sent a verdict that does not parse");
            return;
        };
        if v["runId"].as_str() != Some(self.config.run_id.as_str()) {
            return;
        }
        let (unsynced_bytes, unsynced) = self.unsynced(Some(conn));
        let remote_lag = v["lagTransitions"].as_u64().unwrap_or(u64::MAX / 2);
        self.remote_lag = remote_lag;
        v["lagTransitions"] = json!(remote_lag.saturating_add(unsynced));
        v["relay"] = json!({
            "relayedAt": verdict::now_iso(),
            "remoteLagTransitions": remote_lag,
            "unsyncedBytes": unsynced_bytes,
            "unsyncedTransitions": unsynced,
        });
        // The loss clears only once the box's verdict counts from after it: its window's last
        // trace stop is the lost trace or a later one (SHADOW-02 B2). A verdict that has not
        // reached the stop, or a restarted shadow's that replays the stopped trace before its
        // stop line, still counts the time before the gap, so the loss stays.
        if let Some(lost) = &self.coverage_lost
            && let Some(stop) = v["window"]["lastStopTrace"].as_str()
            && trace_not_before(stop, &lost.trace)
        {
            log("the box's verdict counts from after the trace stop: the coverage loss is closed");
            self.coverage_lost = None;
            self.last_relay_json = Instant::now() - Duration::from_secs(60);
        }
        self.window = v["startedAt"].as_str().map(str::to_owned);
        if v["status"] == "diverged" && self.diverged_at.is_none() {
            self.diverged_at = Some(Instant::now());
        }
        let path = self.config.out_dir.join("verdict.json");
        if let Err(e) = verdict::write_json(&path, &v) {
            log(format!("could not write {}: {e}", path.display()));
        }
    }

    fn write_relay_json(&mut self, conn: Option<&Conn>, healthy: bool) {
        self.last_relay_json = Instant::now();
        let (unsynced_bytes, unsynced) = self.unsynced(conn);
        let coverage_lost = self.coverage_lost.as_ref().map(|c| {
            json!({"trace": c.trace, "window": c.window, "at": c.at})
        });
        let value = json!({
            "format": "fly-shadow-relay-v1",
            "updatedAt": verdict::now_iso(),
            "runId": self.config.run_id,
            "connected": conn.is_some(),
            "healthy": healthy,
            "coverageLost": coverage_lost,
            "traceAgeSeconds": self.trace_age_seconds(),
            "remoteAlive": conn.and_then(|c| c.last_status.as_ref()).map(|(_, alive, _)| *alive),
            "remoteWhy": conn.and_then(|c| c.last_status.as_ref()).map(|(_, _, why)| why.clone()),
            "caughtUpSecondsAgo": conn.map(|c| c.caught_up_at.elapsed().as_secs()),
            "sentBytes": conn.map(|c| c.sent_total),
            "queuedBytes": conn.map(|c| c.queued()),
            "pendingSaves": self.pending.len(),
            "unsyncedBytes": unsynced_bytes,
            "unsyncedTransitions": unsynced,
            "remoteLagTransitions": self.remote_lag,
            "lastBeatSecondsAgo": self.last_beat.map(|t| t.elapsed().as_secs()),
        });
        let path = self.config.out_dir.join("relay.json");
        if let Err(e) = verdict::write_json(&path, &value) {
            log(format!("could not write {}: {e}", path.display()));
        }
    }
}

/// The ssh command that reaches the box's ingest, with nothing from the user's ssh configuration:
/// this key, these known hosts, batch mode, keep-alives that end a dead connection in 30 s.
pub fn ssh_command(
    target: &str,
    key: &Path,
    known_hosts: &Path,
    port: Option<u16>,
) -> Vec<String> {
    let mut argv: Vec<String> = [
        "ssh",
        "-F",
        "/dev/null",
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "IdentitiesOnly=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ServerAliveInterval=10",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "ClearAllForwardings=yes",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    argv.push("-o".to_owned());
    argv.push(format!("UserKnownHostsFile={}", known_hosts.display()));
    argv.push("-i".to_owned());
    argv.push(key.display().to_string());
    if let Some(port) = port {
        argv.push("-p".to_owned());
        argv.push(port.to_string());
    }
    argv.push(target.to_owned());
    // Ignored by the box: its authorized_keys forces `fly-shadow ingest`.
    argv.push("fly-shadow-ingest".to_owned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_no_consumer_stop_is_found_and_nothing_else_is() {
        assert!(has_stop_marker(
            b"{\"a\":1}\n{\"reason\":\"no-consumer\",\"truncated\":true}\n"
        ));
        assert!(has_stop_marker(
            b"{\"truncated\":true,\"reason\":\"no-consumer\"}"
        ));
        // The byte cap is a stop like any other (N4).
        assert!(has_stop_marker(
            b"{\"reason\":\"byte-cap\",\"truncated\":true}\n"
        ));
        assert!(!has_stop_marker(b"{\"reason\":\"other\",\"truncated\":true}\n"));
        assert!(!has_stop_marker(b"{\"reason\":\"byte-cap\"}\n"));
        // Not a marker line.
        assert!(!has_stop_marker(b"{\"note\":{\"reason\":\"no-consumer\"}}\n"));
        assert!(!has_stop_marker(b""));
    }

    #[test]
    fn divergence_files_come_only_after_a_diverged_verdict_and_are_bounded() {
        let mut files = DivergenceFiles::default();
        assert!(!files.admit("divergence.json", 10, false), "not diverged yet");
        assert!(!files.admit("../evil", 10, true));
        assert!(!files.admit("divergence.json", DivergenceFiles::MAX_JSON + 1, true));
        assert!(files.admit("divergence.json", 10, true));
        assert!(files.admit("divergence.json", 10, true), "the same file again");
        assert!(files.admit("divergence-live-g1.checkpoint", 1 << 20, true));
        assert!(!files.admit(
            "divergence-shadow-g1.checkpoint",
            DivergenceFiles::MAX_CHECKPOINT + 1,
            true
        ));
        assert!(files.admit("divergence-shadow-g1.checkpoint", 1 << 20, true));
        assert!(
            !files.admit("divergence-shadow-g2.checkpoint", 1, true),
            "a fourth file"
        );
    }

    #[test]
    fn the_saves_queue_holds_the_whole_stale_window() {
        // Review N5: a stall of up to the trace's ten-minute stale window must not lose the saves
        // the shadow needs (with a margin of a fifth).
        const { assert!(QUEUE_SAVES >= STALE_WINDOW_SECONDS * SAVE_BYTES_PER_SECOND * 6 / 5) };
    }
}
