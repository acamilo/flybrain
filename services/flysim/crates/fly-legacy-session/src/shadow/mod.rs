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

/// The back-off that keeps the shadow from costing the live fly real time.
///
/// flysim's `fly_lag_seconds` is the *accumulated* pacing shortfall of the process: it never
/// recovers, and it grows only when the loop fell more than a second behind its schedule
/// (`pacing::MAX_CATCHUP`). So the signal is its growth, not its value: when it has grown by more
/// than `growth_seconds` within the last `window`, the shadow pauses between frames for
/// `back_off`, and again for as long as the growth continues. A metrics listener that cannot be
/// read never pauses the shadow (the guard is a backstop; the unit's `SCHED_IDLE` is the plan).
#[derive(Clone, Debug)]
pub struct LagGuard {
    /// `host:port` of the live metrics listener.
    pub metrics_addr: String,
    pub growth_seconds: f64,
    pub window: Duration,
    pub back_off: Duration,
}

/// Whether the live lag grew by more than `growth` seconds within `window` of the newest sample.
/// Samples are `(when, fly_lag_seconds)`, oldest first; a restart (the value falling) is growth
/// from zero.
pub fn lag_grew(samples: &VecDeque<(Instant, f64)>, window: Duration, growth: f64) -> bool {
    let Some(&(newest_at, newest)) = samples.back() else {
        return false;
    };
    let mut floor = newest;
    for &(at, value) in samples.iter().rev() {
        if newest_at.duration_since(at) > window {
            break;
        }
        floor = floor.min(value);
    }
    newest - floor > growth
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
    /// Nothing more could be compared in it.
    Skipped(String),
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

/// Polls the live `fly_lag_seconds` every two seconds and applies [`LagGuard`].
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
            let mut samples: VecDeque<(Instant, f64)> = VecDeque::new();
            let mut until: Option<Instant> = None;
            while !stop.requested() {
                let now = Instant::now();
                match read_lag_seconds(&guard.metrics_addr) {
                    Some(lag) => {
                        out.lag_ms.store((lag * 1000.0) as u64, Ordering::Relaxed);
                        // A new process starts from zero: forget the old one's samples.
                        if samples.back().is_some_and(|&(_, last)| lag < last) {
                            samples.clear();
                        }
                        samples.push_back((now, lag));
                        while samples
                            .front()
                            .is_some_and(|&(at, _)| now.duration_since(at) > guard.window * 2)
                        {
                            samples.pop_front();
                        }
                        if lag_grew(&samples, guard.window, guard.growth_seconds) {
                            until = Some(now + guard.back_off);
                        }
                    }
                    None => out.lag_ms.store(u64::MAX, Ordering::Relaxed),
                }
                out.back_off
                    .store(until.is_some_and(|u| now < u), Ordering::Relaxed);
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
    };
    shadow.write();
    let ended = shadow.follow().await;
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
}

impl Shadow {
    fn write(&mut self) {
        self.verdict.spool_evicted = self.spool.evicted();
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
            // saves are gone.
            follow::trace_files(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?
                .into_iter()
                .rev()
                .nth(1)
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
                self.write_if_due();
                tokio::time::sleep(self.config.poll).await;
            };
            let name = next
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            eprintln!("fly-shadow: following {name}");
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
                SegmentEnd::Skipped(reason) => {
                    eprintln!("fly-shadow: {name} skipped after {compared} transitions: {reason}");
                    self.verdict.skipped.push(Skipped {
                        trace: name,
                        transitions_compared: compared,
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
                Err(e) => return Err(SegmentEnd::Skipped(format!("reading the trace: {e}"))),
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
                    Err(e) => Err(SegmentEnd::Skipped(format!("reading the trace: {e}"))),
                };
            }
            self.verdict.lag_transitions = 0;
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
            Err(e) => return SegmentEnd::Skipped(format!("open: {e}")),
        };
        // The header.
        let header: Value = match self.next_line(&mut follower).await {
            Ok(Some(line)) => match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => return SegmentEnd::Skipped(format!("header: {e}")),
            },
            Ok(None) => return SegmentEnd::Skipped("an empty trace".to_owned()),
            Err(end) => return end,
        };
        if header["format"] != trace::FORMAT {
            return SegmentEnd::Skipped(format!("not {}: {}", trace::FORMAT, header["format"]));
        }
        // The start boundary and its startup save.
        let start: Value = match self.next_line(&mut follower).await {
            Ok(Some(line)) => match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => return SegmentEnd::Skipped(format!("start line: {e}")),
            },
            Ok(None) => return SegmentEnd::Skipped("the process ran no transition".to_owned()),
            Err(end) => return end,
        };
        let Some(startup) = start["operational"]["captures"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(generation_of_capture)
            .filter(|_| start.get("boundary").is_some())
        else {
            return SegmentEnd::Skipped("no startup save at the start boundary".to_owned());
        };
        let bytes = {
            let mut waited = Duration::ZERO;
            loop {
                if let Some(bytes) = self.spool.read(startup) {
                    break bytes;
                }
                if waited >= Duration::from_secs(30) {
                    return SegmentEnd::Skipped(format!(
                        "the startup save g{startup} is no longer in the stores or the spool"
                    ));
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
            return SegmentEnd::Skipped(format!(
                "the live process's startup save is not this candidate's to restore ({reason}): \
                 is the shadow the build the live service runs?"
            ));
        }
        let segment_dir = self.config.work_dir.join(format!("segment-{startup}"));
        let _ = std::fs::remove_dir_all(&segment_dir);
        let started = Instant::now();
        let mut session =
            match LegacySession::start(&segment_dir, self.session_config.clone()).await {
                Ok(s) => s,
                Err(e) => return SegmentEnd::Skipped(format!("the session did not start: {e}")),
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
                Err(e) => return SegmentEnd::Skipped(format!("a trace line: {e}")),
            };
            if live.get("truncated").is_some() {
                return SegmentEnd::Skipped("the live trace reached its byte cap".to_owned());
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
                            return SegmentEnd::Skipped("a sugar with no duration".to_owned());
                        };
                        admission.replay_sugar(duration);
                    }
                    Some("reward") => {
                        return SegmentEnd::Skipped(format!(
                            "an operator reward pulse at step {step:?} (declared \
                             operator-reward-pulse: not available on the session runtime)"
                        ));
                    }
                    other => {
                        return SegmentEnd::Skipped(format!("an unknown admission {other:?}"));
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
                                return SegmentEnd::Skipped("a capture with no agent".to_owned());
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
            let (Some(details), [record]) = (details, records.as_slice()) else {
                return SegmentEnd::Skipped("the session recorded no transition".to_owned());
            };
            let mut shadow_line = match trace::line(&details, record) {
                Ok(line) => line["behaviour"].clone(),
                Err(e) => return SegmentEnd::Skipped(format!("the shadow's trace line: {e}")),
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
            if self.verdict.agreement.transitions.is_multiple_of(600) {
                self.verdict.lag_transitions = follower.backlog_lines();
            }
            self.verdict.brain_ms += behaviour["ticksAdvanced"]
                .as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0);
            if self.verdict.status == Status::Running
                && self.verdict.brain_seconds() >= self.config.required_brain_seconds
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

    #[test]
    fn the_guard_backs_off_on_growth_not_on_an_old_lag() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let window = Duration::from_secs(30);
        // An old lag of 40 s that no longer grows: no back-off.
        let steady: VecDeque<_> = (0..20).map(|i| (at(i * 2), 40.0)).collect();
        assert!(!lag_grew(&steady, window, 0.5));
        // It grew by 1.2 s within the window: back off.
        let mut grew = steady.clone();
        grew.push_back((at(40), 41.2));
        assert!(lag_grew(&grew, window, 0.5));
        // The same growth, but longer ago than the window: no longer.
        let mut old = grew.clone();
        for i in 21..45 {
            old.push_back((at(i * 2), 41.2));
        }
        assert!(!lag_grew(&old, window, 0.5));
        assert!(!lag_grew(&VecDeque::new(), window, 0.5));
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
