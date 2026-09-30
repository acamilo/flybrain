//! SHADOW-01: the session runtime run beside the live legacy fly, transition by transition, as
//! the gate for the automatic cutover (CUT-01). `docs/design/session-framework/legacy-gameboy-v1.md`
//! section 18 is the contract; this module is its implementation.
//!
//! **What "the same inputs" means.** The legacy loop is deterministic given its start state and
//! the admissions applied at the top of each frame. Everything nondeterministic it consumes
//! reaches the fly through exactly two doors:
//!
//! 1. *The start state.* Each flysim process restores a checkpoint with
//!    `legacy-transient-reset` (or warms up a fresh fly) and writes a durable startup save of the
//!    state it starts from. The trace's boundary line names that save (`g<N>`). A restart, a
//!    watchdog kill, `fly-loop-recover` and `fly-reset-to-milestone` are all a new process, so a
//!    new file and a new start.
//! 2. *Admissions.* Sugar (the rate limiter and the pulse rule run on wall time and the live
//!    pulse) and an operator reward pulse are applied at the top of a frame, and each lands in that
//!    transition's `admissions`.
//!
//! Wall time reaches nothing else the fly does: pacing, the publish rate, the checkpoint
//! intervals, chat (a caption track) and pause (no frames run) change no transition. The per-frame
//! trace records which checkpoints were taken where, so the shadow reproduces those too.
//!
//! So the shadow follows the live trace (`FLY_TRACE_DIR`): for each process file it boots a new
//! session from that process's own startup save through the ordinary FLYSIM01 import, replays each
//! transition's admissions into the admission queue, runs the transition, and compares its own
//! trace line with the live one field by field -- ticks, clock, remainder, rates, spikes,
//! decision, pad, macro events, frame, work RAM, rewards, rank, slot saves and rollbacks -- plus
//! the ledger digest where the live trace carries one (`FLY_TRACE_LEDGERS`), and the FLYSIM01 bytes
//! of every live save it can still read ([`checkpoint`]). The first difference stops the shadow
//! with a `diverged` verdict and the context around it.

pub mod checkpoint;
pub mod follow;
pub mod spool;
pub mod verdict;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::types::{id, parse_id};
use flysim::snapshot::MacroMode;

use crate::admission::LegacyAdmission;
use crate::composition::{LegacyConfig, LegacySession};
use crate::trace::{self, Agreement, Difference};
use checkpoint::ShadowCapture;
use follow::Follower;
use spool::Spool;
use verdict::{Checkpoints, Cost, Divergence, Skipped, Status, Verdict};

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    flysim::trace::sha256_hex(bytes)
}

/// 3 hours of live brain time: the operator's decision of 2026-09-23.
pub const REQUIRED_BRAIN_SECONDS: f64 = 10_800.0;

/// How the shadow runs.
#[derive(Clone, Debug)]
pub struct ShadowConfig {
    /// The live service's `FLY_TRACE_DIR`.
    pub trace_dir: PathBuf,
    /// The live stores, read only (`FLY_STATE_HOT`, `FLY_STATE`).
    pub hot_dir: PathBuf,
    pub durable_dir: PathBuf,
    /// `verdict.json`, `divergence.json`.
    pub out_dir: PathBuf,
    pub spool_dir: PathBuf,
    /// The session's sockets and artifact stores, one directory per segment.
    pub work_dir: PathBuf,
    pub rom_path: PathBuf,
    pub dataset_dir: PathBuf,
    /// The agent profile: the production one on the stream; the toy connectome in tests.
    pub profile: LegacyProfileKind,
    /// The live service's configuration that a save records or the composition needs.
    pub macro_mode: MacroMode,
    pub speed: f64,
    pub mode: ExecutionMode,
    pub agent_threads: usize,
    pub required_brain_seconds: f64,
    /// Follow every trace file present at start, oldest first (a rehearsal), rather than only the
    /// newest.
    pub all_files: bool,
    /// Keep trace files of finished segments (default: delete them once compared).
    pub keep_traces: bool,
    /// Keep every spooled checkpoint (a rehearsal replays them afterwards).
    pub keep_spool: bool,
    pub exit_on_pass: bool,
    /// Stop after this many compared transitions (a bounded rehearsal); `None` runs on.
    pub max_transitions: Option<u64>,
    pub spool_max_bytes: u64,
    pub context_lines: usize,
    pub poll: Duration,
    /// Stay behind a live fly that is behind real time.
    pub lag_guard: Option<LagGuard>,
    /// The sha256 of the shadow binary (the candidate), for the verdict.
    pub binary_sha256: String,
    /// The release binaries beside it, by name, and their directory (the verdict's `candidate`).
    pub binaries: std::collections::BTreeMap<String, String>,
    pub release: String,
}

/// The back-off that keeps the shadow from costing the live fly real time, judged against the
/// live fly's own pace before the shadow existed (SHADOW-01 review round 2, G1).
///
/// flysim's `fly_lag_seconds` is the accumulated pacing shortfall of the process: it grows, in
/// chunks of more than a second, whenever the loop runs behind real time, and a fly that runs at a
/// realtime factor of 0.7 grows it by about 0.3 s every second whatever the shadow does. So the
/// signal is the lag *growth rate* against the baseline rate `fly-shadow-run start` measured
/// before the shadow started (`baseline.json`: `lagRate`, `margin`; 0 and `margin` when there is
/// none). See [`LagJudge`] for the rule, which lifts the back-off when pausing the shadow does not
/// help: then the shadow is not the cause, and the host guard (`fly-shadow-run guard`) is what
/// acts on a lasting degradation. A metrics listener that cannot be read never pauses the shadow.
#[derive(Clone, Debug)]
pub struct LagGuard {
    /// `host:port` of the live metrics listener.
    pub metrics_addr: String,
    /// `fly-shadow-run`'s baseline, re-read every poll when it exists.
    pub baseline_file: Option<PathBuf>,
    /// The allowed rise of the lag growth rate (s/s) when no baseline gives one.
    pub margin: f64,
    /// The span the growth rate is measured over.
    pub window: Duration,
    pub back_off: Duration,
    /// How long back-offs stay off after one that did not help.
    pub suppress: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum JudgeMode {
    Normal,
    /// Paused until `until`; the lag and time when the pause began.
    BackingOff {
        until: Instant,
        from: (Instant, f64),
    },
    Suppressed {
        until: Instant,
    },
}

/// The in-shadow back-off rule, fed `(now, fly_lag_seconds)` every poll:
///
/// - *Normal*: when the lag has grown faster than `base_rate + margin` over the last `window`,
///   pause the shadow for `back_off`.
/// - *At the end of a pause*: if the live lag kept growing faster than that while the shadow was
///   paused, the shadow is not the cause: no back-off for `suppress`. Otherwise back to normal,
///   with a fresh window.
/// - A new flysim process (the lag falls) starts the window again.
#[derive(Clone, Debug)]
pub struct LagJudge {
    pub base_rate: f64,
    pub margin: f64,
    window: Duration,
    back_off: Duration,
    suppress: Duration,
    samples: VecDeque<(Instant, f64)>,
    mode: JudgeMode,
}

impl LagJudge {
    pub fn new(
        base_rate: f64,
        margin: f64,
        window: Duration,
        back_off: Duration,
        suppress: Duration,
    ) -> LagJudge {
        LagJudge {
            base_rate,
            margin,
            window,
            back_off,
            suppress,
            samples: VecDeque::new(),
            mode: JudgeMode::Normal,
        }
    }

    fn limit(&self) -> f64 {
        self.base_rate + self.margin
    }

    /// The growth rate over the last `window`, once the samples span it.
    fn rate(&self) -> Option<f64> {
        let &(newest_at, newest) = self.samples.back()?;
        let &(oldest_at, oldest) = self
            .samples
            .iter()
            .rev()
            .find(|(at, _)| newest_at.duration_since(*at) >= self.window)?;
        let dt = newest_at.duration_since(oldest_at).as_secs_f64();
        (dt > 0.0).then(|| (newest - oldest) / dt)
    }

    /// Whether the shadow should be paused now.
    pub fn observe(&mut self, now: Instant, lag: f64) -> bool {
        if self.samples.back().is_some_and(|&(_, last)| lag < last) {
            self.samples.clear();
            if let JudgeMode::BackingOff { until, .. } = self.mode {
                self.mode = JudgeMode::BackingOff {
                    until,
                    from: (now, lag),
                };
            }
        }
        self.samples.push_back((now, lag));
        while self
            .samples
            .front()
            .is_some_and(|&(at, _)| now.duration_since(at) > self.window * 3)
        {
            self.samples.pop_front();
        }
        match self.mode {
            JudgeMode::Normal => {
                if self.rate().is_some_and(|rate| rate > self.limit()) {
                    self.mode = JudgeMode::BackingOff {
                        until: now + self.back_off,
                        from: (now, lag),
                    };
                    return true;
                }
                false
            }
            JudgeMode::BackingOff { until, from } => {
                if now < until {
                    return true;
                }
                let dt = now.duration_since(from.0).as_secs_f64();
                let during = if dt > 0.0 { (lag - from.1) / dt } else { 0.0 };
                self.mode = if during > self.limit() {
                    JudgeMode::Suppressed {
                        until: now + self.suppress,
                    }
                } else {
                    JudgeMode::Normal
                };
                self.samples.clear();
                self.samples.push_back((now, lag));
                false
            }
            JudgeMode::Suppressed { until } => {
                if now >= until {
                    self.mode = JudgeMode::Normal;
                    self.samples.clear();
                    self.samples.push_back((now, lag));
                }
                false
            }
        }
    }
}

/// `fly-shadow-run`'s baseline: `(lagRate, margin)`.
pub fn read_baseline(path: &Path) -> Option<(f64, f64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    Some((v["lagRate"].as_f64()?, v["margin"].as_f64()?))
}

/// What the poller shares: the last lag read (ms, `u64::MAX` unknown) and whether to back off.
pub struct LagState {
    pub lag_ms: AtomicU64,
    pub back_off: AtomicBool,
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Ended {
    Pass,
    Diverged,
    Stopped,
    Limit,
}

/// Why a segment ended.
enum SegmentEnd {
    /// The process's file ended (the next one began).
    Completed,
    /// Nothing more could be compared in it, for a reason outside the session runtime: the
    /// kind (one of [`verdict::SKIP_KINDS`] or `trace-malformed`) and what happened.
    Skipped(&'static str, String),
    Diverged(Box<Divergence>),
    Stop(Ended),
}

/// A stop request (SIGTERM or SIGINT), checked between frames.
#[derive(Clone, Default)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub fn request(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn requested(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// Polls the live `fly_lag_seconds` every two seconds and applies [`LagJudge`].
fn spawn_lag_poller(guard: &LagGuard, stop: StopFlag) -> Arc<LagState> {
    let state = Arc::new(LagState {
        lag_ms: AtomicU64::new(u64::MAX),
        back_off: AtomicBool::new(false),
    });
    let out = Arc::clone(&state);
    let guard = guard.clone();
    std::thread::Builder::new()
        .name("fly-shadow-lag".to_owned())
        .spawn(move || {
            let mut judge = LagJudge::new(
                0.0,
                guard.margin,
                guard.window,
                guard.back_off,
                guard.suppress,
            );
            while !stop.requested() {
                if let Some((rate, margin)) = guard.baseline_file.as_deref().and_then(read_baseline)
                {
                    judge.base_rate = rate;
                    judge.margin = margin;
                }
                let back_off = match read_lag_seconds(&guard.metrics_addr) {
                    Some(lag) => {
                        out.lag_ms.store((lag * 1000.0) as u64, Ordering::Relaxed);
                        judge.observe(Instant::now(), lag)
                    }
                    None => {
                        out.lag_ms.store(u64::MAX, Ordering::Relaxed);
                        false
                    }
                };
                out.back_off.store(back_off, Ordering::Relaxed);
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("the lag poller starts");
    state
}

/// `GET /metrics` over plain HTTP/1.0, the `fly_lag_seconds` sample.
pub fn read_lag_seconds(addr: &str) -> Option<f64> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .ok()?;
    write!(stream, "GET /metrics HTTP/1.0\r\nHost: {addr}\r\n\r\n").ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    parse_lag_seconds(&body)
}

pub fn parse_lag_seconds(text: &str) -> Option<f64> {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            (parts.next()? == "fly_lag_seconds").then(|| parts.next()?.parse().ok())?
        })
}

/// Unix milliseconds now.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The start wall ms a trace file's name carries (`trace-<ms>-<pid>.jsonl`).
pub fn trace_start_ms(path: &Path) -> Option<u64> {
    path.file_name()?
        .to_str()?
        .strip_prefix("trace-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

/// The live processes that booted after `since` and have no trace file, in the sugar journal's
/// boot headers (every flysim process writes one, traced or not) against the trace files seen.
/// A process's trace file is created during its boot, after the previous process's boot header
/// and before its own, so each boot owns the trace files started in that interval. Returns the
/// untraced boots' headers; `checked` collects the boots matched, so each is judged once.
pub fn untraced_boots(
    boots: &[Value],
    traces_seen: &std::collections::BTreeSet<u64>,
    since: u64,
    checked: &mut std::collections::BTreeSet<u64>,
) -> Vec<Value> {
    let mut walls: Vec<(u64, &Value)> = boots
        .iter()
        .filter(|b| b["runtime"] == "flysim")
        .filter_map(|b| Some((b["wallMs"].as_u64()?, b)))
        .collect();
    walls.sort_by_key(|(w, _)| *w);
    walls.dedup_by_key(|(w, _)| *w);
    let mut untraced = Vec::new();
    let mut previous = 0u64;
    for (wall, boot) in walls {
        if wall > since && !checked.contains(&wall) {
            if traces_seen.range(previous + 1..=wall).next().is_some() {
                checked.insert(wall);
            } else {
                untraced.push(boot.clone());
            }
        }
        previous = wall;
    }
    untraced
}

/// Writes the heartbeat file (its modification time is what counts).
fn touch(path: &Path) {
    if let Err(e) = std::fs::write(path, verdict::now_iso()) {
        eprintln!("fly-shadow: could not write {}: {e}", path.display());
    }
}

/// The live trace's capture ids: `g<N>`.
fn generation_of_capture(capture: &Value) -> Option<u64> {
    capture["checkpointId"]
        .as_str()?
        .strip_prefix('g')?
        .parse()
        .ok()
}

/// Runs the shadow until it passes (with `exit_on_pass`), diverges, is stopped, or reaches
/// `max_transitions`.
pub async fn run(config: ShadowConfig, stop: StopFlag) -> Result<(Ended, Verdict), String> {
    for dir in [&config.out_dir, &config.work_dir, &config.trace_dir] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let session_config = LegacyConfig {
        mode: config.mode,
        rom_path: config.rom_path.clone(),
        dataset_dir: config.dataset_dir.clone(),
        profile: config.profile,
        macro_mode: config.macro_mode,
        agent_id: id("fly"),
        agent_threads: config.agent_threads,
        record: true,
    };
    let compatibility = crate::composition::compatibility_of(&session_config);
    let mut spool = Spool::new(
        &config.spool_dir,
        vec![config.hot_dir.clone(), config.durable_dir.clone()],
        config.spool_max_bytes,
    )
    .map_err(|e| format!("the spool: {e}"))?;
    if config.keep_spool {
        spool = spool.keep_all();
    }
    let spool_thread = spool.spawn(Duration::from_millis(250), Duration::from_secs(3 * 3600));
    // The consumer heartbeat the live recorder needs to keep tracing (flysim `trace`): written
    // now, before the live service is (re)started, and every 30 s; removed when the shadow stops,
    // so a stopped, diverged or dead shadow turns the live trace off by itself.
    let heartbeat = config.trace_dir.join(flysim::trace::CONSUMER_FILE);
    touch(&heartbeat);
    let beating = Arc::new(AtomicBool::new(true));
    let heartbeat_thread = {
        let (path, beating) = (heartbeat.clone(), Arc::clone(&beating));
        std::thread::Builder::new()
            .name("fly-shadow-heartbeat".to_owned())
            .spawn(move || {
                let mut last = Instant::now();
                while beating.load(Ordering::Relaxed) {
                    if last.elapsed() >= Duration::from_secs(30) {
                        touch(&path);
                        last = Instant::now();
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
            .expect("the heartbeat thread starts")
    };
    let lag = config
        .lag_guard
        .as_ref()
        .map(|guard| spawn_lag_poller(guard, stop.clone()));
    let mut shadow = Shadow {
        verdict: Verdict {
            status: Status::Running,
            reason: "following the live trace".to_owned(),
            binary_sha256: config.binary_sha256.clone(),
            binaries: config.binaries.clone(),
            release: config.release.clone(),
            compatibility: compatibility.clone(),
            execution_mode: config.mode.label().to_owned(),
            agent_threads: config.agent_threads,
            required_brain_seconds: config.required_brain_seconds,
            agreement: Agreement::default(),
            brain_ms: 0.0,
            ledger_checks: 0,
            checkpoints: Checkpoints::default(),
            segments_compared: 0,
            skipped: Vec::new(),
            divergence: None,
            started_at: verdict::now_iso(),
            passed_at: None,
            current_trace: None,
            lag_transitions: 0,
            live_lag_seconds: None,
            spool_evicted: 0,
            cost: Cost::default(),
        },
        config,
        session_config,
        compatibility,
        spool,
        lag,
        stop,
        last_write: Instant::now() - Duration::from_secs(60),
        following: None,
        started_wall_ms: now_ms(),
        traces_seen: Default::default(),
        boots_checked: Default::default(),
        last_coverage: Instant::now() - Duration::from_secs(60),
    };
    shadow.write();
    let ended = shadow.follow().await;
    beating.store(false, Ordering::Relaxed);
    let _ = heartbeat_thread.join();
    let _ = std::fs::remove_file(&heartbeat);
    shadow.spool.stop();
    let _ = spool_thread.join();
    let ended = ended?;
    match ended {
        Ended::Diverged => {}
        Ended::Pass => {}
        Ended::Stopped | Ended::Limit => {
            if shadow.verdict.status == Status::Running {
                shadow.verdict.status = Status::Stopped;
                shadow.verdict.reason = match ended {
                    Ended::Limit => "stopped at the transition limit".to_owned(),
                    _ => "stopped before the window was complete".to_owned(),
                };
            }
        }
    }
    shadow.write();
    if shadow.verdict.status != Status::Diverged {
        shadow.spool.clear();
    }
    Ok((ended, shadow.verdict))
}

struct Shadow {
    config: ShadowConfig,
    session_config: LegacyConfig,
    compatibility: String,
    spool: Spool,
    lag: Option<Arc<LagState>>,
    stop: StopFlag,
    verdict: Verdict,
    last_write: Instant,
    /// The file being compared, the bytes and lines of it compared so far.
    following: Option<(PathBuf, u64, u64)>,
    /// Coverage: when this shadow started (Unix ms), every trace file it has seen (start ms),
    /// the live boots already matched to one, and when it last looked.
    started_wall_ms: u64,
    traces_seen: std::collections::BTreeSet<u64>,
    boots_checked: std::collections::BTreeSet<u64>,
    last_coverage: Instant,
}

impl Shadow {
    fn write(&mut self) {
        self.verdict.spool_evicted = self.spool.evicted();
        // Refreshed on every write, backed off or not, so the verdict's catch-up bound is current.
        if let Some((path, consumed, lines)) = &self.following {
            self.verdict.lag_transitions = follow::backlog(path, *consumed, *lines);
        }
        self.verdict.live_lag_seconds = self.lag.as_ref().and_then(|lag| {
            let ms = lag.lag_ms.load(Ordering::Relaxed);
            (ms != u64::MAX).then(|| ms as f64 / 1000.0)
        });
        let path = self.config.out_dir.join("verdict.json");
        if let Err(e) = verdict::write_json(&path, &self.verdict.to_json()) {
            eprintln!("fly-shadow: could not write {}: {e}", path.display());
        }
        self.last_write = Instant::now();
    }

    /// Every live process since the shadow started must have left a trace: one that ran untraced
    /// (its trace could not be created, or its consumer check failed) can never be compared, so
    /// it fails the verdict as a `coverage` divergence rather than letting the window pass on the
    /// processes that were traced. Looked at every 5 s.
    fn coverage(&mut self) -> Option<Divergence> {
        if self.last_coverage.elapsed() < Duration::from_secs(5) {
            return None;
        }
        self.last_coverage = Instant::now();
        if let Ok(files) = follow::trace_files(&self.config.trace_dir) {
            self.traces_seen
                .extend(files.iter().filter_map(|f| trace_start_ms(f)));
        }
        let segments = flysim::journal::read_segments(&self.config.hot_dir).ok()?;
        let boots: Vec<Value> = segments.into_iter().filter_map(|s| s.boot).collect();
        let untraced = untraced_boots(
            &boots,
            &self.traces_seen,
            self.started_wall_ms,
            &mut self.boots_checked,
        );
        let boot = untraced.first()?;
        Some(Divergence {
            kind: "coverage".to_owned(),
            trace: String::new(),
            step: boot["startFrame"].as_str().and_then(|s| s.parse().ok()),
            difference: None,
            detail: format!(
                "a live flysim process booted at {} ({}, frame {}) left no trace while the shadow \
                 was running: its transitions can never be compared",
                verdict::iso(boot["wallMs"].as_i64().unwrap_or(0)),
                boot["origin"].as_str().unwrap_or("?"),
                boot["startFrame"].as_str().unwrap_or("?"),
            ),
            context: Vec::new(),
        })
    }

    fn write_if_due(&mut self) {
        if self.last_write.elapsed() >= Duration::from_secs(5) {
            self.write();
        }
    }

    fn diverged(&mut self, divergence: Divergence) {
        eprintln!(
            "fly-shadow: DIVERGED ({}) in {} at step {:?}: {}",
            divergence.kind,
            divergence.trace,
            divergence.step,
            divergence
                .difference
                .as_ref()
                .map_or(divergence.detail.clone(), |d| d.to_string())
        );
        let path = self.config.out_dir.join("divergence.json");
        if let Err(e) = verdict::write_json(&path, &divergence.to_json()) {
            eprintln!("fly-shadow: could not write {}: {e}", path.display());
        }
        self.verdict.status = Status::Diverged;
        self.verdict.passed_at = None;
        self.verdict.reason = format!("diverged: {}", divergence.kind);
        self.verdict.divergence = Some(divergence);
        self.write();
    }

    /// The segment loop: each process file in start order.
    async fn follow(&mut self) -> Result<Ended, String> {
        let dir = self.config.trace_dir.clone();
        let mut current: Option<PathBuf> = if self.config.all_files {
            None
        } else {
            // Only the newest file present at start: older processes are history whose startup
            // saves are gone, and are pruned.
            let files = follow::trace_files(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            if !self.config.keep_traces && files.len() > 1 {
                for old in &files[..files.len() - 1] {
                    let _ = std::fs::remove_file(old);
                }
            }
            files.into_iter().rev().nth(1)
        };
        loop {
            let next = loop {
                if self.stop.requested() {
                    return Ok(Ended::Stopped);
                }
                match follow::next_file(&dir, current.as_deref()) {
                    Ok(Some(path)) => break path,
                    Ok(None) => {}
                    Err(e) => return Err(format!("{}: {e}", dir.display())),
                }
                if let Some(divergence) = self.coverage() {
                    self.diverged(divergence);
                    return Ok(Ended::Diverged);
                }
                self.write_if_due();
                tokio::time::sleep(self.config.poll).await;
            };
            let name = next
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            eprintln!("fly-shadow: following {name}");
            self.traces_seen.extend(trace_start_ms(&next));
            self.verdict.current_trace = Some(name.clone());
            let compared_before = self.verdict.agreement.transitions;
            let end = self.segment(&next).await;
            let compared = self.verdict.agreement.transitions - compared_before;
            match end {
                SegmentEnd::Completed => {
                    eprintln!("fly-shadow: {name} ended; {compared} transitions identical");
                    if !self.config.keep_traces {
                        let _ = std::fs::remove_file(&next);
                    }
                }
                SegmentEnd::Skipped(kind, reason) => {
                    eprintln!(
                        "fly-shadow: {name} skipped ({kind}) after {compared} transitions: {reason}"
                    );
                    self.verdict.skipped.push(Skipped {
                        trace: name,
                        transitions_compared: compared,
                        kind: kind.to_owned(),
                        reason,
                    });
                    if !self.config.keep_traces {
                        let _ = std::fs::remove_file(&next);
                    }
                }
                SegmentEnd::Diverged(divergence) => {
                    self.diverged(*divergence);
                    return Ok(Ended::Diverged);
                }
                SegmentEnd::Stop(ended) => return Ok(ended),
            }
            self.write();
            current = Some(next);
        }
    }

    /// Waits for the next complete line of a segment; `None` when the segment has ended.
    async fn next_line(&mut self, follower: &mut Follower) -> Result<Option<String>, SegmentEnd> {
        loop {
            match follower.next_line() {
                Ok(Some(line)) => return Ok(Some(line)),
                Ok(None) => {}
                Err(e) => {
                    return Err(SegmentEnd::Skipped(
                        "trace-malformed",
                        format!("reading the trace: {e}"),
                    ));
                }
            }
            if self.stop.requested() {
                return Err(SegmentEnd::Stop(Ended::Stopped));
            }
            if follow::superseded(follower.path()) {
                // The next process exists, so this one has exited and flushed: one more read for
                // a final flush racing the directory listing, then the segment is over.
                tokio::time::sleep(Duration::from_millis(500)).await;
                return match follower.next_line() {
                    Ok(line) => Ok(line),
                    Err(e) => Err(SegmentEnd::Skipped(
                        "trace-malformed",
                        format!("reading the trace: {e}"),
                    )),
                };
            }
            if let Some(divergence) = self.coverage() {
                return Err(SegmentEnd::Diverged(Box::new(divergence)));
            }
            self.write_if_due();
            tokio::time::sleep(self.config.poll).await;
        }
    }

    async fn segment(&mut self, path: &Path) -> SegmentEnd {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut follower = match Follower::open(path) {
            Ok(f) => f,
            Err(e) => return SegmentEnd::Skipped("trace-malformed", format!("open: {e}")),
        };
        // The header.
        let header: Value = match self.next_line(&mut follower).await {
            Ok(Some(line)) => match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => return SegmentEnd::Skipped("trace-malformed", format!("header: {e}")),
            },
            Ok(None) => return SegmentEnd::Skipped("no-transition", "an empty trace".to_owned()),
            Err(end) => return end,
        };
        if header["format"] != trace::FORMAT {
            return SegmentEnd::Skipped(
                "trace-malformed",
                format!("not {}: {}", trace::FORMAT, header["format"]),
            );
        }
        // The start boundary and its startup save.
        let start: Value = match self.next_line(&mut follower).await {
            Ok(Some(line)) => match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    return SegmentEnd::Skipped("trace-malformed", format!("start line: {e}"));
                }
            },
            Ok(None) => {
                return SegmentEnd::Skipped(
                    "no-transition",
                    "the process ran no transition".to_owned(),
                );
            }
            Err(end) => return end,
        };
        let Some(startup) = start["operational"]["captures"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(generation_of_capture)
            .filter(|_| start.get("boundary").is_some())
        else {
            return SegmentEnd::Skipped(
                "trace-malformed",
                "no startup save at the start boundary".to_owned(),
            );
        };
        let bytes = {
            let mut waited = Duration::ZERO;
            loop {
                if let Some(bytes) = self.spool.read(startup) {
                    break bytes;
                }
                if waited >= Duration::from_secs(30) {
                    return SegmentEnd::Skipped(
                        "startup-save-gone",
                        format!(
                            "the startup save g{startup} is no longer in the stores or the spool"
                        ),
                    );
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                waited += Duration::from_millis(500);
            }
        };
        let checkpoint = match flysim::store::decode(&bytes) {
            Ok(c) => c,
            Err(e) => {
                return SegmentEnd::Diverged(Box::new(self.boot_failure(
                    &name,
                    format!("the startup save g{startup} does not decode: {e:#}"),
                )));
            }
        };
        // The legacy gate, as the live process applied it to its own save.
        let adapter = flybrain_gb::pokemon_red::PokemonRedReward::new();
        let gate = fly_session::legacy_checkpoint::RestoreGate::from_env(
            &checkpoint.runtime.rom_sha256,
            &self.compatibility,
            flybrain_gb::GameAdapter::migrates_from(&adapter),
        );
        if let Err(reason) = gate.check(&checkpoint) {
            return SegmentEnd::Diverged(Box::new(self.boot_failure(
                &name,
                format!(
                    "the live process's startup save g{startup} is refused by this candidate's \
                     restore gate ({reason}): is the shadow the build the live service runs?"
                ),
            )));
        }
        let segment_dir = self.config.work_dir.join(format!("segment-{startup}"));
        let _ = std::fs::remove_dir_all(&segment_dir);
        let started = Instant::now();
        let mut session =
            match LegacySession::start(&segment_dir, self.session_config.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    return SegmentEnd::Diverged(Box::new(
                        self.boot_failure(&name, format!("the session did not start: {e}")),
                    ));
                }
            };
        // A fresh start (the stores were empty): the live process ran the one-frame scaffold and
        // the warm-up from power-on, and the shadow does the same (`Environment.Initialize`,
        // `Agent.Initialize`) rather than import the startup save. An import would not be the
        // same emulator: binjgb's audio resampler phase is not in its exported state, so an
        // imported emulator's channel accumulators drift from a powered-on one's (found by
        // `tests/shadow.rs`). Every other start is a restore on both sides, and the same.
        let fresh = checkpoint.runtime.emulator_frame == 1;
        let booted: Result<f64, String> = match session.bootstrap().await {
            Ok(()) if fresh => Ok(0.0),
            Ok(()) => session
                .import_checkpoint(&checkpoint, &id("e2"))
                .await
                .map(|imported| imported.rank_since_ms),
            Err(e) => Err(e),
        };
        let rank_since_ms = match booted {
            Ok(ms) => ms,
            Err(e) => {
                session.stop().await;
                let _ = std::fs::remove_dir_all(&segment_dir);
                return SegmentEnd::Diverged(Box::new(self.boot_failure(
                    &name,
                    format!("the session runtime could not restore the live save g{startup}: {e}"),
                )));
            }
        };
        if fresh {
            // The fresh fly's startup save must be the shadow's own boundary 0, byte for byte.
            let outcome = match session
                .coordinator
                .capture_payloads(&id("shadow-fresh"))
                .await
            {
                Ok((boundary, world_payload, agents)) => match agents.into_iter().next() {
                    Some((_, agent_payload)) => checkpoint::compare(
                        &bytes,
                        &ShadowCapture {
                            boundary,
                            world_payload,
                            agent_payload,
                            task: session.task.task_half().0,
                            rank_since_ms,
                        },
                        &self.compatibility,
                        self.config.speed,
                        false,
                    )
                    .map_err(|m| m.detail),
                    None => Err("a capture with no agent".to_owned()),
                },
                Err(e) => Err(format!("capture: {}", e.error.message)),
            };
            match outcome {
                Ok(_) => self.verdict.checkpoints.identical += 1,
                Err(detail) => {
                    session.stop().await;
                    return SegmentEnd::Diverged(Box::new(self.boot_failure(
                        &name,
                        format!("a fresh start: the startup save g{startup} is not the shadow's boundary 0: {detail}"),
                    )));
                }
            }
        }
        eprintln!(
            "fly-shadow: {name}: {} g{startup} (boundary {}) in {:?}",
            if fresh { "fresh start, as" } else { "restored" },
            checkpoint.runtime.emulator_frame.saturating_sub(1),
            started.elapsed()
        );
        self.spool.set_floor(startup + 1);
        self.verdict.segments_compared += 1;
        // Rollbacks wait for the boundary's captures, as the host orders them (section 16).
        session.coordinator.defer_rollbacks(true);
        // A segment can run for days: keep the coordinator's in-memory history bounded.
        session.coordinator.bound_history(4096);
        let end = self
            .run_segment(&name, &mut follower, &mut session, rank_since_ms)
            .await;
        session.stop().await;
        let _ = std::fs::remove_dir_all(&segment_dir);
        end
    }

    fn boot_failure(&self, name: &str, detail: String) -> Divergence {
        Divergence {
            kind: "boot".to_owned(),
            trace: name.to_owned(),
            step: None,
            difference: None,
            detail,
            context: Vec::new(),
        }
    }

    async fn run_segment(
        &mut self,
        name: &str,
        follower: &mut Follower,
        session: &mut LegacySession,
        rank_since_ms: f64,
    ) -> SegmentEnd {
        let mut admission = LegacyAdmission::new(session.coordinator.admissions(), id("fly"));
        let mut rank = session.task.rank();
        let mut rank_since_ms = rank_since_ms;
        let mut context: VecDeque<(Value, Value)> = VecDeque::new();
        let mut captures_taken: u64 = 0;
        loop {
            let line = match self.next_line(follower).await {
                Ok(Some(line)) => line,
                Ok(None) => return SegmentEnd::Completed,
                Err(end) => return end,
            };
            let live: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    return SegmentEnd::Skipped("trace-malformed", format!("a trace line: {e}"));
                }
            };
            if live.get("truncated").is_some() {
                return SegmentEnd::Skipped(
                    "trace-cap",
                    format!("the live trace stopped ({})", live["reason"]),
                );
            }
            let Some(behaviour) = live.get("behaviour").cloned() else {
                continue;
            };
            let step: Option<u64> = behaviour["step"].as_str().and_then(|s| s.parse().ok());
            // The admissions of this transition, replayed as the live loop applied them.
            for admission_line in behaviour["admissions"].as_array().into_iter().flatten() {
                match admission_line["kind"].as_str() {
                    Some("sugar") => {
                        let Some(duration) = admission_line["durationMs"].as_f64() else {
                            return SegmentEnd::Skipped(
                                "trace-malformed",
                                "a sugar with no duration".to_owned(),
                            );
                        };
                        admission.replay_sugar(duration);
                    }
                    Some("reward") => {
                        return SegmentEnd::Skipped(
                            "operator-reward-pulse",
                            format!(
                                "an operator reward pulse at step {step:?} (declared \
                             operator-reward-pulse: not available on the session runtime)"
                            ),
                        );
                    }
                    other => {
                        return SegmentEnd::Skipped(
                            "trace-malformed",
                            format!("an unknown admission {other:?}"),
                        );
                    }
                }
            }
            // Back off while the live fly is losing real time ([`LagGuard`]).
            if let Some(lag) = self.lag.clone() {
                let throttle = Instant::now();
                while lag.back_off.load(Ordering::Relaxed) {
                    if self.stop.requested() {
                        return SegmentEnd::Stop(Ended::Stopped);
                    }
                    self.write_if_due();
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                self.verdict.cost.throttled_ms += throttle.elapsed().as_secs_f64() * 1000.0;
            }
            let t0 = Instant::now();
            if let Err(e) = session.coordinator.step().await {
                return SegmentEnd::Diverged(Box::new(Divergence {
                    kind: "session-error".to_owned(),
                    trace: name.to_owned(),
                    step,
                    difference: None,
                    detail: format!("step ({} at {}): {}", e.detail, e.phase, e.error.message),
                    context: context.into_iter().collect(),
                }));
            }
            // `Sim::track_rank`, before the boundary's captures.
            let now_rank = session.task.rank();
            if now_rank != rank {
                rank = now_rank;
                rank_since_ms = session.task.brain_ms();
            }
            // The live captures at this boundary, each where the live loop took it: before the
            // rollback (a milestone archive), or after it.
            let actions: Vec<String> = behaviour["boundaryActions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|a| a["kind"].as_str().unwrap_or("").to_owned())
                .collect();
            let rollback_at = actions.iter().position(|k| k == "rollback");
            let live_captures: Vec<(u64, usize)> = live["operational"]["captures"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|c| {
                    Some((
                        generation_of_capture(c)?,
                        c["afterActions"].as_u64()? as usize,
                    ))
                })
                .collect();
            let mut taken: Vec<(u64, usize, ShadowCapture)> = Vec::new();
            for pass in [false, true] {
                if pass && let Err(e) = session.coordinator.apply_pending_rollback().await {
                    return SegmentEnd::Diverged(Box::new(Divergence {
                        kind: "session-error".to_owned(),
                        trace: name.to_owned(),
                        step,
                        difference: None,
                        detail: format!(
                            "rollback ({} at {}): {}",
                            e.detail, e.phase, e.error.message
                        ),
                        context: context.into_iter().collect(),
                    }));
                }
                for (generation, after) in &live_captures {
                    let after_rollback = rollback_at.is_some_and(|r| *after > r);
                    if after_rollback != pass {
                        continue;
                    }
                    captures_taken += 1;
                    let checkpoint_id =
                        parse_id(&format!("shadow-{captures_taken}")).expect("a valid id");
                    match session.coordinator.capture_payloads(&checkpoint_id).await {
                        Ok((boundary, world_payload, agents)) => {
                            let Some((_, agent_payload)) = agents.into_iter().next() else {
                                return SegmentEnd::Diverged(Box::new(Divergence {
                                    kind: "session-error".to_owned(),
                                    trace: name.to_owned(),
                                    step,
                                    difference: None,
                                    detail: "a capture with no agent".to_owned(),
                                    context: context.into_iter().collect(),
                                }));
                            };
                            taken.push((
                                *generation,
                                *after,
                                ShadowCapture {
                                    boundary,
                                    world_payload,
                                    agent_payload,
                                    task: session.task.task_half().0,
                                    rank_since_ms,
                                },
                            ));
                        }
                        Err(e) => {
                            return SegmentEnd::Diverged(Box::new(Divergence {
                                kind: "session-error".to_owned(),
                                trace: name.to_owned(),
                                step,
                                difference: None,
                                detail: format!(
                                    "capture ({} at {}): {}",
                                    e.detail, e.phase, e.error.message
                                ),
                                context: context.into_iter().collect(),
                            }));
                        }
                    }
                }
            }
            let details = session.coordinator.take_details();
            let records = session.task.take_records();
            let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
            self.verdict.cost.record(elapsed);
            let session_error = |detail: String, context: VecDeque<(Value, Value)>| {
                SegmentEnd::Diverged(Box::new(Divergence {
                    kind: "session-error".to_owned(),
                    trace: name.to_owned(),
                    step,
                    difference: None,
                    detail,
                    context: context.into_iter().collect(),
                }))
            };
            let (Some(details), [record]) = (details, records.as_slice()) else {
                return session_error(
                    format!(
                        "the session recorded no transition ({} task records)",
                        records.len()
                    ),
                    context,
                );
            };
            let mut shadow_line = match trace::line(&details, record) {
                Ok(line) => line["behaviour"].clone(),
                Err(e) => return session_error(format!("the shadow's trace line: {e}"), context),
            };
            if behaviour.get("ledgersDigest").is_some() {
                shadow_line["ledgersDigest"] = Value::String(sha256_hex(record.ledgers.as_bytes()));
                self.verdict.ledger_checks += 1;
            }
            context.push_back((behaviour.clone(), shadow_line.clone()));
            while context.len() > self.config.context_lines {
                context.pop_front();
            }
            if let Err(difference) = trace::compare_line(&behaviour, &shadow_line, &record.bound) {
                let kind = if difference.field == "ledgersDigest" {
                    "ledgers"
                } else {
                    "transition"
                };
                let detail = if kind == "ledgers" {
                    format!(
                        "the shadow's ledgers after the boundary: {}",
                        record.ledgers
                    )
                } else {
                    String::new()
                };
                return SegmentEnd::Diverged(Box::new(Divergence {
                    kind: kind.to_owned(),
                    trace: name.to_owned(),
                    step,
                    difference: Some(*difference),
                    detail,
                    context: context.into_iter().collect(),
                }));
            }
            // The live saves at this boundary, byte for byte.
            let save_slots_before = |after: usize| {
                actions[..after.min(actions.len())]
                    .iter()
                    .filter(|k| *k == "save-slot")
                    .count()
            };
            let total_save_slots = actions.iter().filter(|k| *k == "save-slot").count();
            for (generation, after, capture) in &taken {
                let Some(live_bytes) = self.spool.read(*generation) else {
                    self.verdict.checkpoints.unavailable += 1;
                    continue;
                };
                let archive_order = save_slots_before(*after) < total_save_slots;
                match checkpoint::compare(
                    &live_bytes,
                    capture,
                    &self.compatibility,
                    self.config.speed,
                    archive_order,
                ) {
                    Ok(checkpoint::Outcome::Identical) => self.verdict.checkpoints.identical += 1,
                    Ok(checkpoint::Outcome::Declared) => self.verdict.checkpoints.declared += 1,
                    Err(mismatch) => {
                        // Both files, for the review.
                        let out = &self.config.out_dir;
                        let _ = std::fs::write(
                            out.join(format!("divergence-live-g{generation}.checkpoint")),
                            &live_bytes,
                        );
                        let _ = std::fs::write(
                            out.join(format!("divergence-shadow-g{generation}.checkpoint")),
                            &mismatch.shadow_bytes,
                        );
                        return SegmentEnd::Diverged(Box::new(Divergence {
                            kind: "checkpoint".to_owned(),
                            trace: name.to_owned(),
                            step,
                            difference: Some(Difference {
                                field: format!("checkpoint g{generation}"),
                                legacy: Value::String(sha256_hex(&live_bytes)),
                                session: Value::String(format!("boundary {}", capture.boundary)),
                            }),
                            detail: mismatch.detail,
                            context: context.into_iter().collect(),
                        }));
                    }
                }
                self.spool.set_floor(*generation + 1);
            }
            self.verdict.agreement.count(&behaviour);
            self.following = Some((
                follower.path().to_owned(),
                follower.consumed,
                follower.lines,
            ));
            self.verdict.brain_ms += behaviour["ticksAdvanced"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            if self.verdict.status == Status::Running
                && self.verdict.brain_seconds() >= self.config.required_brain_seconds
                && verdict::saves_suffice(
                    self.verdict.brain_seconds(),
                    self.verdict.checkpoints.identical + self.verdict.checkpoints.declared,
                    self.verdict.checkpoints.unavailable,
                )
                .is_ok()
            {
                self.verdict.status = Status::Pass;
                self.verdict.passed_at = Some(verdict::now_iso());
                self.verdict.reason = format!(
                    "{} s of live brain time compared with zero divergence",
                    self.config.required_brain_seconds
                );
                eprintln!("fly-shadow: PASS ({})", self.verdict.reason);
                self.write();
                if self.config.exit_on_pass {
                    return SegmentEnd::Stop(Ended::Pass);
                }
            }
            if self
                .config
                .max_transitions
                .is_some_and(|max| self.verdict.agreement.transitions >= max)
            {
                return SegmentEnd::Stop(Ended::Limit);
            }
            if let Some(divergence) = self.coverage() {
                return SegmentEnd::Diverged(Box::new(divergence));
            }
            self.write_if_due();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lag_sample_is_read_from_the_exposition() {
        let text = "# HELP fly_lag_seconds lag\n# TYPE fly_lag_seconds gauge\nfly_lag_seconds 0.25\nfly_other 3\n";
        assert_eq!(parse_lag_seconds(text), Some(0.25));
        assert_eq!(parse_lag_seconds("fly_lag_seconds_total 3\n"), None);
    }

    /// Feeds a judge 2-s polls for `seconds`, the lag growing at `rate(paused)` s/s; returns the
    /// fraction of the time the shadow was paused.
    fn paused_fraction(judge: &mut LagJudge, seconds: u64, rate: impl Fn(bool) -> f64) -> f64 {
        let t0 = Instant::now();
        let (mut lag, mut paused, mut paused_polls) = (5.0, false, 0u64);
        for i in 0..seconds / 2 {
            lag += 2.0 * rate(paused);
            paused = judge.observe(t0 + Duration::from_secs(i * 2), lag);
            paused_polls += u64::from(paused);
        }
        paused_polls as f64 / (seconds / 2) as f64
    }

    fn judge(base_rate: f64) -> LagJudge {
        LagJudge::new(
            base_rate,
            0.05,
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(600),
        )
    }

    #[test]
    fn a_fly_below_real_time_is_judged_against_its_own_pace() {
        // The release CT at RTF 0.66: 0.34 s of lag every second, before and during the shadow.
        let mut j = judge(0.34);
        assert_eq!(paused_fraction(&mut j, 3 * 3600, |_| 0.34), 0.0);
        // Without a baseline the same fly is paused once, the pause does not help, and the
        // back-off lifts: at most one minute in eleven.
        let mut j = judge(0.0);
        let f = paused_fraction(&mut j, 3 * 3600, |_| 0.34);
        assert!(f > 0.0 && f < 0.1, "{f}");
    }

    #[test]
    fn a_shadow_that_costs_the_fly_time_stays_paused_while_it_would() {
        // The fly keeps real time while the shadow is paused and loses 0.3 s/s while it runs:
        // pausing helps, so the judge keeps pausing it, most of the time.
        let mut j = judge(0.0);
        let f = paused_fraction(&mut j, 3 * 3600, |paused| if paused { 0.0 } else { 0.3 });
        assert!(f > 0.4, "{f}");
    }

    #[test]
    fn a_restart_starts_the_window_again() {
        let t0 = Instant::now();
        let mut j = judge(0.0);
        for i in 0..40u64 {
            assert!(!j.observe(t0 + Duration::from_secs(i * 2), 50.0));
        }
        // The lag falls to zero (a new process): no rate across the restart.
        assert!(!j.observe(t0 + Duration::from_secs(82), 0.0));
        assert!(!j.observe(t0 + Duration::from_secs(84), 0.0));
    }

    #[test]
    fn every_boot_after_the_start_owns_a_trace() {
        let boot = |wall: u64| serde_json::json!({"runtime": "flysim", "wallMs": wall, "startFrame": "1", "origin": "x"});
        let boots = vec![boot(1_000), boot(2_000), boot(3_000), boot(4_000)];
        let mut checked = Default::default();
        // Traces created during boots 2 and 4 (the one at 1,000 predates the shadow).
        let traces: std::collections::BTreeSet<u64> = [1_950, 3_990].into();
        let untraced = untraced_boots(&boots, &traces, 1_500, &mut checked);
        assert_eq!(untraced.len(), 1);
        assert_eq!(untraced[0]["wallMs"], 3_000);
        assert_eq!(checked, [2_000u64, 4_000].into());
        // A session-runtime boot is not this shadow's to follow; a boot before the start neither.
        let other = vec![serde_json::json!({"runtime": "fly-session", "wallMs": 5_000})];
        assert!(untraced_boots(&other, &traces, 1_500, &mut checked).is_empty());
        assert_eq!(
            trace_start_ms(Path::new("/x/trace-0001790741297310-444741.jsonl")),
            Some(1_790_741_297_310)
        );
    }

    #[test]
    fn capture_ids() {
        assert_eq!(
            generation_of_capture(&serde_json::json!({"checkpointId": "g42", "afterActions": 0})),
            Some(42)
        );
        assert_eq!(
            generation_of_capture(&serde_json::json!({"checkpointId": "x"})),
            None
        );
    }
}
