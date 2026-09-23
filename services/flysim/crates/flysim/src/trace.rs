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
}

impl FrameTrace {
    /// `FLY_TRACE`, if it is set and non-empty. A path that cannot be created is an error rather
    /// than a silent run without the trace that was asked for.
    pub fn from_env() -> std::io::Result<Option<Self>> {
        match std::env::var_os(ENV) {
            Some(path) if !path.is_empty() => Self::create(Path::new(&path)).map(Some),
            _ => Ok(None),
        }
    }

    pub fn create(path: &Path) -> std::io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(path)?);
        writeln!(out, "{}", json!({ "format": FORMAT }))?;
        Ok(Self {
            out,
            initial: Vec::new(),
            initial_step: None,
            open: None,
            admissions: Vec::new(),
            ms_before: 0.0,
        })
    }

    fn write(&mut self, value: &Value) {
        if let Err(error) = writeln!(self.out, "{value}") {
            tracing::warn!(%error, "could not write the frame trace");
        }
    }

    /// Sugar admitted at the top of the frame, before the brain ticks.
    pub fn sugar(&mut self, duration_ms: f64) {
        self.admissions.push(json!({ "kind": "sugar", "durationMs": duration_ms }));
    }

    /// An operator reward pulse, applied at the top of the frame.
    pub fn reward_pulse(&mut self, value: f64) {
        self.admissions.push(json!({ "kind": "reward", "value": value }));
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
        let Some(record) = self.open.as_mut() else { return };
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
            record.macros.extend(events.iter().map(|event| macro_json("execute", event)));
        }
    }

    /// The frame the emulator produced, and work RAM after it. Read uncached, so the trace never
    /// fills the per-frame read cache the adapter and the macros share.
    pub fn advanced(&mut self, framebuffer: &[u8], emulator: &Emulator) {
        if let Some(record) = self.open.as_mut() {
            record.framebuffer = sha256_hex(framebuffer);
            let wram: Vec<u8> = WRAM.map(|address| emulator.read_uncached(address)).collect();
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
            record.macros.extend(abandoned.iter().map(|event| macro_json("evaluate", event)));
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
            record.macros.extend(events.iter().map(|event| macro_json("rollback", event)));
        }
    }

    fn flush_open(&mut self) {
        let Some(record) = self.open.take() else { return };
        let line = json!({
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

    #[test]
    fn every_legacy_remainder_is_exact_nanoseconds() {
        // The first twelve legacy frames' remainders, from the clock the fixture records.
        let per_frame = 1000.0 / (4_194_304.0 / 70_224.0);
        let mut remainder = 0.0f64;
        for _ in 0..100_000 {
            remainder += per_frame;
            remainder -= remainder.floor();
            let value = remainder_ns(remainder);
            assert!(value.get("numerator").is_some(), "{remainder} was not exact: {value}");
        }
        assert_eq!(remainder_ns(0.0), json!({ "numerator": "0", "denominator": "1" }));
        // 0.5 ms is 500000 ns.
        assert_eq!(remainder_ns(0.5), json!({ "numerator": "500000", "denominator": "1" }));
        // One 2^-15 ms step is 15625/512 ns.
        assert_eq!(
            remainder_ns(1.0 / 32_768.0),
            json!({ "numerator": "15625", "denominator": "512" })
        );
    }
}
