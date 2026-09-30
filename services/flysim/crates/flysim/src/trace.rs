//! `FLY_TRACE=<path>`: a per-frame record of the legacy loop, for parity and for the port.
//!
//! One JSON object per line. The first line names the format; every other line is either one
//! transition `k -> k+1` of the legacy frame order (`crate::frame`) together with what happened at
//! the boundary it reached, or -- once, before the first transition -- the captures taken at the
//! boundary the run started on.
//!
//! The field names follow the step trace of the session framework
//! (`docs/design/session-framework/step-v1.md` section 8 and its 2026-09-23 amendment, Rust
//! `fly_session_types::trace`) wherever a legacy field is the same thing:
//!
//! - `behaviour` is what two runs of one build pair must agree on, byte for byte:
//!   `step`, `ticksAdvanced`, `brainTicks` and `remainder` (a `RationalNs`, exact, because every
//!   legacy remainder is a multiple of 2^-15 ms: legacy-gameboy-v1 section 3), the decision, the
//!   controller mask, the macro and reward events in the order they happened, the digests of the
//!   rates, of the spikes of this transition, of the frame and of work RAM, the rank, and
//!   `boundaryActions` -- a ratchet capture is a `save-slot` of slot `best` with the digest of the
//!   saved emulator state, a ratchet recovery is a `rollback` to `best` -- in the shape of
//!   `TraceBehaviour.boundaryActions`;
//! - `operational.captures` is every checkpoint taken at the boundary, in order, each with the
//!   number of boundary actions applied before it, in the shape of `TraceOperational.captures`.
//!   The legacy loop archives a milestone *before* the ratchet captures (legacy-gameboy-v1 section
//!   4), so a climb records `afterActions: 0` ahead of a `save-slot`: the declared difference, as
//!   it happens, which the ported loop's validator refuses on purpose.
//!
//! `admissions` is sugar and operator reward pulses applied at the top of the frame, before the
//! brain ticks: the admission cut of legacy-gameboy-v1 section 15.
//!
//! Wall time is never written, so a run with the periodic checkpoint intervals pushed out of the
//! way produces the same file twice. Off by default; when off, nothing here is constructed and the
//! loop pays one `Option` test per hook.
//!
//! **The shadow run's input (SHADOW-01).** The trace is also what the shadow run
//! (`fly-legacy-session::shadow`) follows beside the live service, so three switches serve it.
//! None changes a line's behaviour fields or anything the fly does:
//!
//! - `FLY_TRACE_DIR=<dir>` writes one file per process, `trace-<wall ms, 13 digits>-<pid>.jsonl`,
//!   instead of the one `FLY_TRACE` path a restart would truncate. File names sort in start order;
//!   a process's file ends where the next one begins.
//! - `FLY_TRACE_MAX_BYTES` caps one file (default 4 GiB in directory mode, none otherwise). At the
//!   cap the recorder writes `{"truncated":true}` and stops; the loop runs on untouched.
//! - *A consumer must be alive* (directory mode). The shadow touches `<dir>/consumer` every 30 s.
//!   A process starts no trace when that file is missing or older than
//!   `FLY_TRACE_CONSUMER_STALE_SECONDS` (default 600), and a running one checks it once a minute
//!   of frames and stops the same way as at the cap, with `"reason":"no-consumer"`. A dead or
//!   diverged shadow therefore turns the trace off by itself, with no root action and no restart.
//! - *The directory is bounded* (directory mode). Before a process creates its file it deletes
//!   the oldest `trace-*.jsonl` until the ones left and the new file's cap fit in
//!   `FLY_TRACE_DIR_MAX_BYTES` (default 8 GiB). The shadow deletes each file once compared.
//! - `FLY_TRACE_LEDGERS=<n>` adds `ledgersDigest` to every transition whose `step` is a multiple
//!   of `n`: the SHA-256 of [`crate::frame::ledgers_string`] after the boundary (adapter export,
//!   ratchet, slot, executor scene/bound/running/counts/nearer). `1` is every transition. The
//!   header then carries `ledgersEvery`.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use flybrain_core::lif::LifNetwork;
use flybrain_gb::RewardEvent;
use flybrain_gb::emulator::Emulator;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::macros::MacroEvent;

/// The format name on the first line.
pub const FORMAT: &str = "flysim-legacy-frame-trace-v1";

/// The environment variable that turns the trace on.
pub const ENV: &str = "FLY_TRACE";

/// One file per process in this directory (SHADOW-01).
pub const DIR_ENV: &str = "FLY_TRACE_DIR";

/// The per-file byte cap.
pub const MAX_BYTES_ENV: &str = "FLY_TRACE_MAX_BYTES";

/// The ledger digest's period in transitions.
pub const LEDGERS_ENV: &str = "FLY_TRACE_LEDGERS";

/// The default per-file cap in directory mode: about 23 hours of stream.
pub const DEFAULT_DIR_MAX_BYTES: u64 = 4 << 30;

/// The consumer's heartbeat file in [`DIR_ENV`] mode.
pub const CONSUMER_FILE: &str = "consumer";

/// How old the heartbeat may be (seconds).
pub const CONSUMER_STALE_ENV: &str = "FLY_TRACE_CONSUMER_STALE_SECONDS";
pub const DEFAULT_CONSUMER_STALE_SECONDS: u64 = 600;

/// The cap over every trace file in the directory.
pub const DIR_MAX_BYTES_ENV: &str = "FLY_TRACE_DIR_MAX_BYTES";
pub const DEFAULT_DIR_TOTAL_BYTES: u64 = 8 << 30;

/// Transitions between two heartbeat checks: about a minute of frames.
const CONSUMER_CHECK_EVERY: u64 = 3_600;

/// Whether `dir`'s consumer heartbeat is at most `stale` old.
pub fn consumer_alive(dir: &Path, stale: std::time::Duration) -> bool {
    std::fs::metadata(dir.join(CONSUMER_FILE))
        .and_then(|m| m.modified())
        .ok()
        // A heartbeat dated ahead of this clock (a clock stepped back) is fresh, not missing.
        .map(|t| {
            std::time::SystemTime::now()
                .duration_since(t)
                .unwrap_or_default()
        })
        .is_some_and(|age| age <= stale)
}

/// Deletes the oldest trace files in `dir` until those left total at most `keep` bytes.
pub fn prune_dir(dir: &Path, keep: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::path::PathBuf, u64)> = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("trace-") && n.ends_with(".jsonl"))
        })
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.len())))
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|(_, len)| len).sum();
    for (path, len) in files {
        if total <= keep {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            tracing::warn!(path = %path.display(), "removed an old frame trace to bound its directory");
            total -= len;
        }
    }
}

/// The per-process file name in [`DIR_ENV`] mode: start wall time (13 digits, so names sort in
/// start order) and pid.
pub fn dir_file_name(wall_ms: u64, pid: u32) -> String {
    format!("trace-{wall_ms:013}-{pid}.jsonl")
}

/// The ratchet's one slot, as the legacy composition declares it (legacy-gameboy-v1 section 9).
pub const SLOT: &str = "best";

/// Work RAM, `$C000..=$DFFF`, as the CPU sees it.
const WRAM: std::ops::RangeInclusive<u16> = 0xc000..=0xdfff;

/// Lowercase hex SHA-256.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// A legacy remainder in milliseconds as exact nanoseconds, `{numerator, denominator}` in lowest
/// terms. The remainder is always `m / 32768` ms for an integer `m` (legacy-gameboy-v1 section
/// 3), so this never rounds; a value that is not is written with its bits instead.
pub fn remainder_ns(remainder_ms: f64) -> Value {
    let scaled = remainder_ms * 32_768.0;
    if scaled.fract() != 0.0 || !(0.0..32_768.0 * 1_000.0).contains(&scaled) {
        return json!({ "inexactBits": format!("{:016x}", remainder_ms.to_bits()) });
    }
    // ns = m * 1e6 / 32768 = m * 15625 / 512.
    let mut numerator = scaled as u64 * 15_625;
    let mut denominator = 512u64;
    let (mut a, mut b) = (numerator, denominator);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    if a > 1 {
        numerator /= a;
        denominator /= a;
    }
    if numerator == 0 {
        denominator = 1;
    }
    json!({ "numerator": numerator.to_string(), "denominator": denominator.to_string() })
}

fn macro_json(phase: &str, event: &MacroEvent) -> Value {
    json!({
        "phase": phase,
        "slot": event.slot,
        "name": event.name,
        "outcome": event.outcome.map(|outcome| outcome.as_str()),
    })
}

/// One transition and the boundary it reached, while it is being recorded.
#[derive(Default)]
struct Record {
    step: u64,
    admissions: Vec<Value>,
    ticks: u64,
    brain_ticks: u64,
    remainder: Value,
    rates: String,
    spikes: String,
    spike_count: u64,
    decision: Vec<String>,
    mask: u32,
    macros: Vec<Value>,
    framebuffer: String,
    wram: String,
    rewards: Vec<Value>,
    rank: u32,
    boundary_actions: Vec<Value>,
    captures: Vec<Value>,
    ledgers: Option<String>,
}

/// The recorder. The loop calls it at fixed points of the frame order; it holds the transition
/// open until the next one starts, so the captures and admissions a host takes between two frames
/// land on the boundary they belong to.
pub struct FrameTrace {
    out: BufWriter<File>,
    /// Captures at the boundary the run started on, before any transition.
    initial: Vec<Value>,
    initial_step: Option<u64>,
    open: Option<Record>,
    /// Admissions since the last transition started: they belong to the next one.
    admissions: Vec<Value>,
    /// Brain clock before this transition's ticks, the lower edge of its spike window.
    ms_before: f64,
    /// Bytes written so far, and the cap (`FLY_TRACE_MAX_BYTES`).
    written: u64,
    max_bytes: Option<u64>,
    /// Set once the cap was reached: nothing more is written.
    stopped: bool,
    /// `FLY_TRACE_LEDGERS`: a ledger digest every this many transitions.
    ledgers_every: Option<u64>,
    /// Directory mode: the directory whose consumer heartbeat keeps the trace on, and its limit.
    consumer: Option<(std::path::PathBuf, std::time::Duration)>,
    transitions: u64,
}

impl FrameTrace {
    /// `FLY_TRACE`, if it is set and non-empty. A path that cannot be created is an error rather
    /// than a silent run without the trace that was asked for.
    ///
    /// `FLY_TRACE_DIR` (one file per process) wins over `FLY_TRACE`; `FLY_TRACE_MAX_BYTES` and
    /// `FLY_TRACE_LEDGERS` apply to either.
    pub fn from_env() -> std::io::Result<Option<Self>> {
        let var = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
        let number = |name: &str| -> std::io::Result<Option<u64>> {
            match var(name) {
                None => Ok(None),
                Some(value) => value
                    .to_str()
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .filter(|v| *v > 0)
                    .map(Some)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!("{name} must be a positive integer"),
                        )
                    }),
            }
        };
        let ledgers_every = number(LEDGERS_ENV)?;
        let max_bytes = number(MAX_BYTES_ENV)?;
        if let Some(dir) = var(DIR_ENV) {
            let dir = Path::new(&dir);
            let stale = std::time::Duration::from_secs(
                number(CONSUMER_STALE_ENV)?.unwrap_or(DEFAULT_CONSUMER_STALE_SECONDS),
            );
            if !consumer_alive(dir, stale) {
                tracing::warn!(
                    dir = %dir.display(),
                    "FLY_TRACE_DIR is set but no consumer is alive there: no frame trace this run"
                );
                return Ok(None);
            }
            let cap = max_bytes.unwrap_or(DEFAULT_DIR_MAX_BYTES);
            let total = number(DIR_MAX_BYTES_ENV)?.unwrap_or(DEFAULT_DIR_TOTAL_BYTES);
            prune_dir(dir, total.saturating_sub(cap));
            let wall_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            let path = dir.join(dir_file_name(wall_ms, std::process::id()));
            // A trace the live service cannot create (a full disk, a missing directory, its
            // permissions) never stops the live fly from starting: it runs untraced, and the
            // shadow, which checks every boot header in the sugar journal against the trace
            // files, reports that process as a coverage gap and fails its verdict.
            let mut trace = match Self::create_with(&path, Some(cap), ledgers_every) {
                Ok(trace) => trace,
                Err(error) => {
                    tracing::error!(%error, path = %path.display(), "could not create the frame trace: this run is untraced");
                    return Ok(None);
                }
            };
            trace.consumer = Some((dir.to_owned(), stale));
            return Ok(Some(trace));
        }
        match var(ENV) {
            Some(path) => Self::create_with(Path::new(&path), max_bytes, ledgers_every).map(Some),
            None => Ok(None),
        }
    }

    pub fn create(path: &Path) -> std::io::Result<Self> {
        Self::create_with(path, None, None)
    }

    /// A trace with a byte cap and a ledger digest period (see the module notes).
    pub fn create_with(
        path: &Path,
        max_bytes: Option<u64>,
        ledgers_every: Option<u64>,
    ) -> std::io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        let header = match ledgers_every {
            Some(every) => json!({ "format": FORMAT, "ledgersEvery": every }),
            None => json!({ "format": FORMAT }),
        };
        let header = header.to_string();
        writeln!(out, "{header}")?;
        Ok(Self {
            out,
            initial: Vec::new(),
            initial_step: None,
            open: None,
            admissions: Vec::new(),
            ms_before: 0.0,
            written: header.len() as u64 + 1,
            max_bytes,
            stopped: false,
            ledgers_every,
            consumer: None,
            transitions: 0,
        })
    }

    /// Stops the trace: one marker line, then nothing more. The loop is unaffected.
    fn stop(&mut self, reason: &str) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if let Err(error) = writeln!(
            self.out,
            "{}",
            json!({ "truncated": true, "reason": reason })
        ) {
            tracing::warn!(%error, "could not write the frame trace");
        }
        let _ = self.out.flush();
        tracing::warn!(reason, "the frame trace stopped");
    }

    fn write(&mut self, value: &Value) {
        if self.stopped {
            return;
        }
        let line = value.to_string();
        let len = line.len() as u64 + 1;
        if let Some(cap) = self.max_bytes
            && self.written + len > cap
        {
            self.stop("byte-cap");
            return;
        }
        self.written += len;
        if let Err(error) = writeln!(self.out, "{line}") {
            tracing::warn!(%error, "could not write the frame trace");
        }
    }

    /// Whether the open transition takes a ledger digest (`FLY_TRACE_LEDGERS`).
    pub fn wants_ledgers(&self) -> bool {
        match (self.ledgers_every, self.open.as_ref()) {
            (Some(every), Some(record)) => !self.stopped && record.step % every == 0,
            _ => false,
        }
    }

    /// The ledgers after the boundary, as [`crate::frame::ledgers_string`] writes them.
    pub fn ledgers(&mut self, ledgers: &str) {
        if let Some(record) = self.open.as_mut() {
            record.ledgers = Some(sha256_hex(ledgers.as_bytes()));
        }
    }

    /// Sugar admitted at the top of the frame, before the brain ticks.
    pub fn sugar(&mut self, duration_ms: f64) {
        self.admissions
            .push(json!({ "kind": "sugar", "durationMs": duration_ms }));
    }

    /// An operator reward pulse, applied at the top of the frame.
    pub fn reward_pulse(&mut self, value: f64) {
        self.admissions
            .push(json!({ "kind": "reward", "value": value }));
    }

    /// A checkpoint capture at the current boundary: the open transition's, or the start's.
    pub fn capture(&mut self, generation: u64, step: u64) {
        let (captures, after) = match self.open.as_mut() {
            Some(record) => {
                let after = record.boundary_actions.len();
                (&mut record.captures, after)
            }
            None => {
                self.initial_step.get_or_insert(step);
                (&mut self.initial, 0)
            }
        };
        captures.push(json!({ "checkpointId": format!("g{generation}"), "afterActions": after }));
    }

    /// Transition `step -> step + 1` begins: the previous one is complete.
    pub fn begin(&mut self, step: u64, ms_before: f64) {
        self.flush_open();
        self.transitions += 1;
        let consumer_gone = self.transitions.is_multiple_of(CONSUMER_CHECK_EVERY)
            && self
                .consumer
                .as_ref()
                .is_some_and(|(dir, stale)| !consumer_alive(dir, *stale));
        if consumer_gone {
            self.stop("no-consumer");
        }
        if let Some(start) = self.initial_step.take() {
            let initial = std::mem::take(&mut self.initial);
            self.write(&json!({
                "boundary": start.to_string(),
                "operational": { "captures": initial },
            }));
        }
        self.ms_before = ms_before;
        self.open = Some(Record {
            step,
            admissions: std::mem::take(&mut self.admissions),
            ..Record::default()
        });
    }

    /// Phase A's ticks, and the network they left behind.
    pub fn ticked(&mut self, ticks: u64, remainder_ms: f64, network: &LifNetwork) {
        let Some(record) = self.open.as_mut() else {
            return;
        };
        record.ticks = ticks;
        record.brain_ticks = network.ms as u64;
        record.remainder = remainder_ns(remainder_ms);
        let mut hasher = Sha256::new();
        for (role, rate) in network.rates.iter() {
            hasher.update((role.len() as u32).to_le_bytes());
            hasher.update(role.as_bytes());
            hasher.update(rate.to_bits().to_le_bytes());
        }
        record.rates = hex(&hasher.finalize());
        let (bits, count) =
            crate::snapshot::spike_bitset(&network.last_spike_ms, self.ms_before, network.ms);
        record.spikes = sha256_hex(&bits);
        record.spike_count = count;
    }

    /// The readout's decision, as decoded.
    pub fn decided(&mut self, active: &[String]) {
        if let Some(record) = self.open.as_mut() {
            record.decision = active.to_vec();
        }
    }

    /// Phase B: the mask the emulator is given, and the macro events deciding it produced.
    pub fn executed(&mut self, mask: u32, events: &[MacroEvent]) {
        if let Some(record) = self.open.as_mut() {
            record.mask = mask;
            record
                .macros
                .extend(events.iter().map(|event| macro_json("execute", event)));
        }
    }

    /// The frame the emulator produced, and work RAM after it. Read uncached, so the trace never
    /// fills the per-frame read cache the adapter and the macros share.
    pub fn advanced(&mut self, framebuffer: &[u8], emulator: &Emulator) {
        if let Some(record) = self.open.as_mut() {
            record.framebuffer = sha256_hex(framebuffer);
            let wram: Vec<u8> = WRAM
                .map(|address| emulator.read_uncached(address))
                .collect();
            record.wram = sha256_hex(&wram);
        }
    }

    /// Phase C: the reward events in adapter order, the scene's own macro events, and the rank.
    pub fn evaluated(&mut self, rewards: &[RewardEvent], abandoned: &[MacroEvent], rank: u32) {
        if let Some(record) = self.open.as_mut() {
            record.rewards.extend(rewards.iter().map(|event| {
                json!({
                    "kind": event.kind,
                    "value": event.value,
                    "stimulationMs": event.stimulation_ms,
                })
            }));
            record
                .macros
                .extend(abandoned.iter().map(|event| macro_json("evaluate", event)));
            record.rank = rank;
        }
    }

    /// The ratchet captured: a slot save at the boundary.
    pub fn slot_saved(&mut self, state: &[u8]) {
        if let Some(record) = self.open.as_mut() {
            record.boundary_actions.push(json!({
                "kind": "save-slot",
                "slotId": SLOT,
                "stateDigest": sha256_hex(state),
            }));
        }
    }

    /// The ratchet rolled back, and the macro events the rollback produced.
    pub fn rolled_back(&mut self, events: &[MacroEvent]) {
        if let Some(record) = self.open.as_mut() {
            record.boundary_actions.push(json!({
                "kind": "rollback",
                "slotId": SLOT,
                "stateDigest": null,
            }));
            record
                .macros
                .extend(events.iter().map(|event| macro_json("rollback", event)));
        }
    }

    fn flush_open(&mut self) {
        let Some(record) = self.open.take() else {
            return;
        };
        let mut line = json!({
            "behaviour": {
                "step": record.step.to_string(),
                "admissions": record.admissions,
                "ticksAdvanced": record.ticks.to_string(),
                "brainTicks": record.brain_ticks.to_string(),
                "remainder": record.remainder,
                "ratesDigest": record.rates,
                "spikesDigest": record.spikes,
                "spikeCount": record.spike_count,
                "decision": record.decision,
                "mask": record.mask,
                "macroEvents": record.macros,
                "framebufferDigest": record.framebuffer,
                "wramDigest": record.wram,
                "rewards": record.rewards,
                "rank": record.rank,
                "acknowledgedBoundary": (record.step + 1).to_string(),
                "boundaryActions": record.boundary_actions,
            },
            "operational": { "captures": record.captures },
        });
        if let Some(digest) = record.ledgers {
            line["behaviour"]["ledgersDigest"] = Value::String(digest);
        }
        self.write(&line);
    }

    /// Write the open transition and flush the file: on shutdown, and on drop.
    pub fn finish(&mut self) {
        self.flush_open();
        if let Err(error) = self.out.flush() {
            tracing::warn!(%error, "could not flush the frame trace");
        }
    }
}

impl Drop for FrameTrace {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn the_directory_is_bounded_and_needs_a_live_consumer() {
        let dir = std::env::temp_dir().join(format!("fly-trace-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let minute = std::time::Duration::from_secs(60);
        assert!(!consumer_alive(&dir, minute), "no heartbeat file");
        std::fs::write(dir.join(CONSUMER_FILE), b"x").unwrap();
        assert!(consumer_alive(&dir, minute));
        for (name, len) in [
            ("trace-1-1.jsonl", 100),
            ("trace-2-1.jsonl", 100),
            ("trace-3-1.jsonl", 100),
        ] {
            std::fs::write(dir.join(name), vec![b'x'; len]).unwrap();
        }
        std::fs::write(dir.join("other.txt"), vec![b'x'; 1000]).unwrap();
        prune_dir(&dir, 150);
        assert!(!dir.join("trace-1-1.jsonl").exists());
        assert!(!dir.join("trace-2-1.jsonl").exists());
        assert!(dir.join("trace-3-1.jsonl").exists());
        assert!(
            dir.join("other.txt").exists(),
            "only trace files are pruned"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_running_trace_stops_when_its_consumer_goes_away() {
        let dir = std::env::temp_dir().join(format!("fly-trace-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CONSUMER_FILE), b"x").unwrap();
        let path = dir.join("t.jsonl");
        let mut trace = FrameTrace::create_with(&path, None, None).unwrap();
        trace.consumer = Some((dir.clone(), std::time::Duration::from_secs(600)));
        for step in 0..CONSUMER_CHECK_EVERY {
            trace.begin(step, 0.0);
        }
        assert!(!trace.stopped, "a live consumer keeps it on");
        std::fs::remove_file(dir.join(CONSUMER_FILE)).unwrap();
        for step in CONSUMER_CHECK_EVERY..3 * CONSUMER_CHECK_EVERY {
            trace.begin(step, 0.0);
        }
        trace.finish();
        drop(trace);
        let all = lines(&path);
        assert_eq!(all.last().unwrap()["reason"], "no-consumer");
        assert!(all.len() < (2 * CONSUMER_CHECK_EVERY) as usize + 3);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn directory_files_sort_in_start_order() {
        assert!(dir_file_name(999, 70_000) < dir_file_name(1000, 5));
        assert_eq!(dir_file_name(1, 2), "trace-0000000000001-2.jsonl");
    }

    #[test]
    fn the_byte_cap_ends_the_file_with_a_marker_and_nothing_after() {
        let dir = std::env::temp_dir().join(format!("fly-trace-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut trace = FrameTrace::create_with(&path, Some(1500), None).unwrap();
        for step in 0..40 {
            trace.begin(step, 0.0);
        }
        trace.finish();
        drop(trace);
        let all = lines(&path);
        assert_eq!(all.first().unwrap()["format"], FORMAT);
        assert_eq!(all.last().unwrap()["truncated"], true);
        assert_eq!(all.last().unwrap()["reason"], "byte-cap");
        assert!(all.len() > 2 && all.len() < 40);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size <= 1500 + 20, "{size}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ledger_digests_every_nth_step() {
        let dir = std::env::temp_dir().join(format!("fly-trace-ledgers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let mut trace = FrameTrace::create_with(&path, None, Some(2)).unwrap();
        for step in 10..14 {
            trace.begin(step, 0.0);
            if trace.wants_ledgers() {
                trace.ledgers("the ledgers");
            }
        }
        trace.finish();
        drop(trace);
        let all = lines(&path);
        assert_eq!(all[0]["ledgersEvery"], 2);
        let digests: Vec<bool> = all[1..]
            .iter()
            .map(|l| l["behaviour"].get("ledgersDigest").is_some())
            .collect();
        assert_eq!(digests, vec![true, false, true, false]);
        assert_eq!(
            all[1]["behaviour"]["ledgersDigest"],
            sha256_hex(b"the ledgers").as_str()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_legacy_remainder_is_exact_nanoseconds() {
        // The first twelve legacy frames' remainders, from the clock the fixture records.
        let per_frame = 1000.0 / (4_194_304.0 / 70_224.0);
        let mut remainder = 0.0f64;
        for _ in 0..100_000 {
            remainder += per_frame;
            remainder -= remainder.floor();
            let value = remainder_ns(remainder);
            assert!(
                value.get("numerator").is_some(),
                "{remainder} was not exact: {value}"
            );
        }
        assert_eq!(
            remainder_ns(0.0),
            json!({ "numerator": "0", "denominator": "1" })
        );
        // 0.5 ms is 500000 ns.
        assert_eq!(
            remainder_ns(0.5),
            json!({ "numerator": "500000", "denominator": "1" })
        );
        // One 2^-15 ms step is 15625/512 ns.
        assert_eq!(
            remainder_ns(1.0 / 32_768.0),
            json!({ "numerator": "15625", "denominator": "512" })
        );
    }
}
