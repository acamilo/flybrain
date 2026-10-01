//! The verdict file, `fly-shadow-verdict-v1`: the contract CUT-01's automatic cutover reads.
//!
//! The shadow rewrites `verdict.json` in its output directory atomically (temporary file, rename)
//! every few seconds and at every change of status. The fields CUT-01 relies on:
//!
//! | Field | Meaning |
//! | --- | --- |
//! | `format` | `fly-shadow-verdict-v1` |
//! | `status` | `running`, `pass`, `diverged`, `stopped` or `error` |
//! | `candidate` | what was shadowed: `binarySha256` of the shadow binary; `binaries`, the SHA-256 of every release binary beside it by name (`fly-shadow`, `fly-session`, `flysim`, `fly-edge`: the session-runtime service CUT-01 switches to is built from the same tree in the same release); `release`, that directory; `compatibility` and its `compatibilitySha256`; `executionMode`; `agentThreads` |
//! | `required.brainSeconds` | the window the decision names: 10,800 s (3 h) of live brain time |
//! | `compared.brainSeconds`, `compared.transitions` | how much of the live fly was compared, transition by transition, with zero divergence |
//! | `passedAt` | when `compared.brainSeconds` first reached `required.brainSeconds` with no divergence |
//! | `firstDivergence` | `null`, or the first difference with its context (also `divergence.json`) |
//! | `declaredDifferences` | the named exclusions the comparison applied |
//! | `runId` | SHADOW-02, a remote shadow: the relay's run (`null` for a shadow on the container) |
//! | `relay` | SHADOW-02, added by the relay on the container: `lagTransitions` there includes the live trace not yet on the box |
//!
//! `pass` is sticky only while nothing diverges: a shadow that keeps running after it passed and
//! then finds a difference turns the verdict to `diverged`. `stopped` is a shadow that exited
//! before it passed (a `pass` stays `pass` when the shadow stops). `error` is the shadow's own
//! failure (it could not read its inputs), which proves nothing either way.
//!
//! **The cutover rule (for CUT-01), [`allows_cutover`].** Cut over only if, read at the moment of
//! cutting over, every one of these holds; anything else keeps the legacy fly:
//!
//! - `format` is `fly-shadow-verdict-v1`, `status` is `pass`, `firstDivergence` is `null`, and
//!   `compared.brainSeconds` reaches `required.brainSeconds` and never less than the operator's
//!   10,800 s (`REQUIRED_BRAIN_SECONDS`), whatever window the shadow was run with;
//! - *the release*: `candidate.release` is the directory `/opt/fly/current` resolves to now,
//!   every binary in `candidate.binaries` still has its recorded SHA-256 there, and the binary
//!   being switched to (`flysim-session`, SERVE-01's service) is one of them -- the shadow
//!   vouches for the release it ran in and nothing else;
//! - `candidate.compatibility` is the live service's `--print-compatibility`;
//! - *saves were compared*: at least one live save per [`BRAIN_SECONDS_PER_SAVE`] brain seconds
//!   compared byte for byte, and no more than [`MAX_UNAVAILABLE_FRACTION`] of the saves the trace
//!   named were unavailable (a shadow starved past its spool must not pass on transitions alone);
//! - *nothing was skipped for a reason inside the session runtime*: every `skipped` entry's `kind`
//!   is one of [`SKIP_KINDS`] (the live side's own events); a session-side failure is a
//!   divergence, never a skip;
//! - *the shadow is caught up and alive*: `updatedAt` is no older than `max_age_s`, and
//!   `lagTransitions` is at most [`MAX_LAG_TRANSITIONS`].

use std::path::Path;

use serde_json::{Value, json};

use crate::trace::{Agreement, Difference};

pub const FORMAT: &str = "fly-shadow-verdict-v1";

/// Skips that are the live side's own events, not the session runtime's failures: the operator
/// reward pulse (declared), a startup save rotated away before the shadow reached it, and a
/// process that ran no transition. A trace that stopped (byte cap, or no consumer before this
/// shadow started) is a coverage gap, never a skip: it ends the window, see [`Verdict::end_window`]
/// (SHADOW-02 B2).
pub const SKIP_KINDS: [&str; 3] = [
    "operator-reward-pulse",
    "startup-save-gone",
    "no-transition",
];

/// At least one live save compared byte for byte per this many brain seconds (the live loop
/// saves every 5 s; 600 leaves room for a lagging shadow's spool evictions).
pub const BRAIN_SECONDS_PER_SAVE: f64 = 600.0;

/// At most this fraction of the saves the trace named may have been unavailable.
pub const MAX_UNAVAILABLE_FRACTION: f64 = 0.1;

/// The shadow at most about a minute behind the live trace.
pub const MAX_LAG_TRANSITIONS: u64 = 3_600;

/// The declared differences the comparison applies, by name (`legacy-gameboy-v1`).
pub const DECLARED: [(&str, &str); 5] = [
    (
        "archive-order",
        "a legacy milestone archive holds the pre-capture ratchet and slot (sections 4, 16): the \
         live file's ratchet, ratchetGame and ratchetFrame stand in for the shadow's in that capture",
    ),
    (
        "decision-list-order",
        "the decision is compared as gameboy-channels-v1: buttons down and the first bound channel \
         held (section 6)",
    ),
    (
        "host-fields",
        "generation, wallMs and lastEventId are the live file's: the session runtime has no feed \
         event log before EDGE-01 and allocates no generations as a shadow",
    ),
    (
        "admission-replay",
        "sugar is replayed from the live trace's admissions, not re-decided: admission reads a \
         one-commit-stale pulse by the operator's decision (section 15)",
    ),
    (
        "operator-reward-pulse",
        "a live POST /reward pulse cannot be reproduced (section 15 amendment): the rest of that \
         segment is reported uncompared, not diverged",
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Running,
    Pass,
    Diverged,
    Stopped,
    Error,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Pass => "pass",
            Status::Diverged => "diverged",
            Status::Stopped => "stopped",
            Status::Error => "error",
        }
    }
}

/// What the checkpoint comparisons found.
#[derive(Clone, Debug, Default)]
pub struct Checkpoints {
    /// Byte-identical after the host fields.
    pub identical: u64,
    /// Byte-identical after the host fields and a declared difference.
    pub declared: u64,
    /// Named in the trace but rotated away before the spool copied it.
    pub unavailable: u64,
}

/// A segment the shadow did not (fully) compare, and why.
#[derive(Clone, Debug)]
pub struct Skipped {
    pub trace: String,
    pub transitions_compared: u64,
    /// One of [`SKIP_KINDS`], or `trace-malformed`.
    pub kind: String,
    pub reason: String,
}

/// A coverage gap: the live trace stopped, so everything compared before it no longer counts.
#[derive(Clone, Debug)]
pub struct WindowEnd {
    pub trace: String,
    pub reason: String,
    pub discarded_brain_seconds: f64,
}

/// The first divergence and its context.
#[derive(Clone, Debug)]
pub struct Divergence {
    /// `transition`, `ledgers`, `checkpoint`, `boot` or `session-error`.
    pub kind: String,
    pub trace: String,
    pub step: Option<u64>,
    pub difference: Option<Difference>,
    pub detail: String,
    /// The last transition lines before it, live and shadow, oldest first.
    pub context: Vec<(Value, Value)>,
}

impl Divergence {
    pub fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "trace": self.trace,
            "step": self.step.map(|s| s.to_string()),
            "field": self.difference.as_ref().map(|d| d.field.clone()),
            "live": self.difference.as_ref().map(|d| d.legacy.clone()),
            "shadow": self.difference.as_ref().map(|d| d.session.clone()),
            "detail": self.detail,
            "context": self.context.iter().map(|(live, shadow)| json!({"live": live, "shadow": shadow})).collect::<Vec<_>>(),
        })
    }
}

/// The per-frame cost of the shadow, wall time.
#[derive(Clone, Debug, Default)]
pub struct Cost {
    samples: Vec<f64>,
    pub total_ms: f64,
    pub frames: u64,
    pub throttled_ms: f64,
}

impl Cost {
    /// Keeps the last 36,000 frames (10 minutes) for the percentiles.
    pub fn record(&mut self, ms: f64) {
        self.total_ms += ms;
        self.frames += 1;
        if self.samples.len() == 36_000 {
            self.samples.remove(0);
        }
        self.samples.push(ms);
    }

    pub fn to_json(&self) -> Value {
        let mut sorted = self.samples.clone();
        sorted.sort_by(f64::total_cmp);
        let pct = |p: f64| {
            if sorted.is_empty() {
                return Value::Null;
            }
            let i = ((sorted.len() - 1) as f64 * p).round() as usize;
            json!((sorted[i] * 1000.0).round() / 1000.0)
        };
        json!({
            "framesTimed": self.frames,
            "msPerFrameMean": if self.frames == 0 { Value::Null } else { json!(((self.total_ms / self.frames as f64) * 1000.0).round() / 1000.0) },
            "msPerFrameP50": pct(0.5),
            "msPerFrameP99": pct(0.99),
            "throttledSeconds": (self.throttled_ms / 1000.0).round(),
        })
    }
}

/// Everything the verdict file says.
#[derive(Clone, Debug)]
pub struct Verdict {
    pub status: Status,
    pub reason: String,
    pub binary_sha256: String,
    /// SHA-256 of the release binaries beside the shadow, by file name.
    pub binaries: std::collections::BTreeMap<String, String>,
    /// The directory they are in.
    pub release: String,
    pub compatibility: String,
    pub execution_mode: String,
    pub agent_threads: usize,
    pub required_brain_seconds: f64,
    pub agreement: Agreement,
    pub brain_ms: f64,
    pub ledger_checks: u64,
    pub checkpoints: Checkpoints,
    pub segments_compared: u64,
    pub skipped: Vec<Skipped>,
    /// Trace stops (coverage gaps) that ended a window: what they discarded.
    pub window_ends: Vec<WindowEnd>,
    /// The trace whose stop last ended the window (`None`: none yet). `compared` counts only after.
    pub last_stop_trace: Option<String>,
    pub divergence: Option<Divergence>,
    pub started_at: String,
    pub passed_at: Option<String>,
    pub current_trace: Option<String>,
    /// Complete live lines not yet compared in the current segment, when known.
    pub lag_transitions: u64,
    pub live_lag_seconds: Option<f64>,
    pub spool_evicted: u64,
    pub cost: Cost,
    /// SHADOW-02: the relay's run id, for a remote shadow (`null` on the release container).
    pub run_id: Option<String>,
}

pub fn now_iso() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    iso(ms)
}

/// UTC `YYYY-MM-DDTHH:MM:SS.mmmZ` from Unix milliseconds (civil-from-days).
pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

impl Verdict {
    /// A coverage gap (a trace stopped, whatever the reason): the live fly may run untraced from
    /// here, so the brain time and saves compared so far stop counting. A pass needs the whole
    /// window after the last gap (SHADOW-02 B2).
    pub fn end_window(&mut self, trace: &str, reason: String) {
        self.window_ends.push(WindowEnd {
            trace: trace.to_owned(),
            reason,
            discarded_brain_seconds: self.brain_ms / 1000.0,
        });
        self.last_stop_trace = Some(trace.to_owned());
        self.brain_ms = 0.0;
        self.checkpoints = Checkpoints::default();
    }

    pub fn brain_seconds(&self) -> f64 {
        self.brain_ms / 1000.0
    }

    pub fn to_json(&self) -> Value {
        let a = &self.agreement;
        json!({
            "format": FORMAT,
            "status": self.status.as_str(),
            "reason": self.reason,
            "candidate": {
                "binarySha256": self.binary_sha256,
                "binaries": self.binaries,
                "release": self.release,
                "compatibility": self.compatibility,
                "compatibilitySha256": crate::shadow::sha256_hex(self.compatibility.as_bytes()),
                "executionMode": self.execution_mode,
                "agentThreads": self.agent_threads,
            },
            "required": {"brainSeconds": self.required_brain_seconds},
            "compared": {
                "brainSeconds": (self.brain_seconds() * 1000.0).round() / 1000.0,
                "transitions": a.transitions,
                "segments": self.segments_compared,
                "sugar": a.sugar,
                "rewards": a.rewards,
                "rewardKinds": a.reward_kinds,
                "macroEvents": a.macro_events,
                "pressed": a.pressed,
                "slotSaves": a.saves,
                "rollbacks": a.rollbacks,
                "spikes": a.spikes,
                "ledgerChecks": self.ledger_checks,
                "checkpoints": {
                    "identical": self.checkpoints.identical,
                    "declared": self.checkpoints.declared,
                    "unavailable": self.checkpoints.unavailable,
                },
            },
            "skipped": self.skipped.iter().map(|s| json!({
                "trace": s.trace, "transitionsCompared": s.transitions_compared, "kind": s.kind, "reason": s.reason,
            })).collect::<Vec<_>>(),
            "window": {
                "lastStopTrace": self.last_stop_trace,
                "ends": self.window_ends.iter().map(|w| json!({
                    "trace": w.trace, "reason": w.reason, "discardedBrainSeconds": (w.discarded_brain_seconds * 1000.0).round() / 1000.0,
                })).collect::<Vec<_>>(),
            },
            "declaredDifferences": DECLARED.iter().map(|(name, what)| json!({"name": name, "what": what})).collect::<Vec<_>>(),
            "firstDivergence": self.divergence.as_ref().map(|d| {
                let mut v = d.to_json();
                if let Some(o) = v.as_object_mut() { o.remove("context"); }
                v
            }),
            "startedAt": self.started_at,
            "updatedAt": now_iso(),
            "passedAt": self.passed_at,
            "currentTrace": self.current_trace,
            "lagTransitions": self.lag_transitions,
            "liveLagSeconds": self.live_lag_seconds,
            "spoolEvicted": self.spool_evicted,
            "cost": self.cost.to_json(),
            "runId": self.run_id,
        })
    }
}

/// Writes `value` to `path` atomically.
pub fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let mut text = serde_json::to_string_pretty(value).expect("JSON serializes");
    text.push('\n');
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// [`write_json`], synced to disk (file and directory) before it returns.
pub fn write_json_durable(path: &Path, value: &Value) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    let mut text = serde_json::to_string_pretty(value).expect("JSON serializes");
    text.push('\n');
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// Whether the saves compared are enough for the brain time compared ([`BRAIN_SECONDS_PER_SAVE`],
/// [`MAX_UNAVAILABLE_FRACTION`]).
pub fn saves_suffice(brain_seconds: f64, compared: u64, unavailable: u64) -> Result<(), String> {
    let needed = (brain_seconds / BRAIN_SECONDS_PER_SAVE).floor().max(1.0) as u64;
    if compared < needed {
        return Err(format!(
            "{compared} live saves compared over {brain_seconds:.0} brain s; {needed} needed"
        ));
    }
    if unavailable as f64 > MAX_UNAVAILABLE_FRACTION * (compared + unavailable) as f64 {
        return Err(format!(
            "{unavailable} of {} live saves were unavailable to compare",
            compared + unavailable
        ));
    }
    Ok(())
}

/// The release the cutover would switch into: its resolved directory and the SHA-256 of the
/// binaries in it, by file name.
pub struct Release<'a> {
    pub dir: &'a str,
    pub binaries: &'a std::collections::BTreeMap<String, String>,
}

/// How far ahead of the clock a verdict's `updatedAt` may be before it is refused.
const MAX_CLOCK_SKEW_S: i64 = 60;

/// Reads a verdict file and says whether it allows the cutover, by the rule in the module notes.
/// `binary` is the file name of the session-runtime binary CUT-01 switches to, inside `release`
/// (what `/opt/fly/current` resolves to now), and `compatibility` the live service's;
/// `max_age_s` bounds `updatedAt`.
pub fn allows_cutover(
    verdict: &Value,
    binary: &str,
    release: &Release<'_>,
    compatibility: &str,
    now_ms: i64,
    max_age_s: i64,
) -> Result<(), String> {
    if verdict["format"] != FORMAT {
        return Err(format!("not {FORMAT}"));
    }
    if verdict["status"] != "pass" {
        return Err(format!("status is {}", verdict["status"]));
    }
    if !verdict["firstDivergence"].is_null() {
        return Err("a divergence is recorded".to_owned());
    }
    let candidate = &verdict["candidate"];
    if candidate["release"] != release.dir {
        return Err(format!(
            "the verdict is for the release {}, not {}",
            candidate["release"], release.dir
        ));
    }
    let shadowed = candidate["binaries"]
        .as_object()
        .ok_or("the verdict names no binaries")?;
    if !shadowed.contains_key(binary) {
        return Err(format!("{binary} was not in the shadowed release"));
    }
    for (name, sha) in shadowed {
        if release.binaries.get(name).map(String::as_str) != sha.as_str() {
            return Err(format!(
                "{name} in {} is not the binary the shadow ran beside",
                release.dir
            ));
        }
    }
    if candidate["compatibility"] != compatibility {
        return Err("the verdict is for another compatibility string".to_owned());
    }
    // Only a build that ends the window at a coverage gap counts brain time after it.
    if verdict["window"].is_null() {
        return Err(
            "the verdict does not say where its window starts (an older shadow)".to_owned(),
        );
    }
    let compared = verdict["compared"]["brainSeconds"].as_f64().unwrap_or(0.0);
    // The operator's window is a floor the verdict cannot lower: a shadow run with a shorter
    // `--required-brain-seconds` (a rehearsal, a test) never arms the cutover.
    let required = verdict["required"]["brainSeconds"]
        .as_f64()
        .unwrap_or(f64::INFINITY)
        .max(crate::shadow::REQUIRED_BRAIN_SECONDS);
    if compared < required {
        return Err(format!("{compared} of {required} brain seconds compared"));
    }
    let saves = &verdict["compared"]["checkpoints"];
    saves_suffice(
        compared,
        saves["identical"].as_u64().unwrap_or(0) + saves["declared"].as_u64().unwrap_or(0),
        saves["unavailable"].as_u64().unwrap_or(u64::MAX / 2),
    )?;
    for skip in verdict["skipped"].as_array().into_iter().flatten() {
        let kind = skip["kind"].as_str().unwrap_or("");
        if !SKIP_KINDS.contains(&kind) {
            return Err(format!(
                "a segment was skipped for {kind:?}: {}",
                skip["reason"]
            ));
        }
    }
    let lag = verdict["lagTransitions"].as_u64().unwrap_or(u64::MAX);
    if lag > MAX_LAG_TRANSITIONS {
        return Err(format!(
            "the shadow is {lag} transitions behind the live trace"
        ));
    }
    let updated = verdict["updatedAt"]
        .as_str()
        .and_then(parse_iso)
        .ok_or("no updatedAt")?;
    // A verdict from the future (beyond a small skew) is refused: after a backward clock step an
    // old pass would otherwise stay "fresh" for the size of the step plus `max_age_s`.
    if updated - now_ms > MAX_CLOCK_SKEW_S * 1000 {
        return Err(format!(
            "the verdict is dated {} s in the future",
            (updated - now_ms) / 1000
        ));
    }
    if now_ms - updated > max_age_s * 1000 {
        return Err(format!(
            "the verdict is {} s old",
            (now_ms - updated) / 1000
        ));
    }
    Ok(())
}

/// Parses [`iso`]'s format back to Unix milliseconds.
pub fn parse_iso(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 24 || b[23] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| -> Option<i64> { text.get(r)?.parse().ok() };
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss, ms) = (n(11..13)?, n(14..16)?, n(17..19)?, n(20..23)?);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hh * 3600 + mm * 60 + ss) * 1000) + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_round_trips() {
        for ms in [0i64, 951_782_400_123, 1_790_000_000_999, 4_102_444_800_000] {
            assert_eq!(parse_iso(&iso(ms)), Some(ms), "{}", iso(ms));
        }
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso(951_782_400_123), "2000-02-29T00:00:00.123Z");
    }

    fn verdict(status: Status, brain_ms: f64) -> Value {
        Verdict {
            status,
            reason: String::new(),
            binary_sha256: "b".repeat(64),
            binaries: [("flysim-session".to_owned(), "b".repeat(64))].into(),
            release: "/opt/fly/releases/test".to_owned(),
            compatibility: "c".to_owned(),
            execution_mode: "in-process".to_owned(),
            agent_threads: 2,
            required_brain_seconds: 10_800.0,
            agreement: Agreement::default(),
            brain_ms,
            ledger_checks: 0,
            checkpoints: Checkpoints {
                identical: 2_000,
                declared: 1,
                unavailable: 5,
            },
            segments_compared: 1,
            skipped: Vec::new(),
            window_ends: Vec::new(),
            last_stop_trace: None,
            divergence: None,
            started_at: now_iso(),
            passed_at: None,
            current_trace: None,
            lag_transitions: 0,
            live_lag_seconds: None,
            spool_evicted: 0,
            cost: Cost::default(),
            run_id: None,
        }
        .to_json()
    }

    #[test]
    fn a_stop_ends_the_window() {
        // B2: brain time compared before a coverage gap never counts toward a pass.
        let mut v = Verdict {
            status: Status::Running,
            reason: String::new(),
            binary_sha256: String::new(),
            binaries: Default::default(),
            release: String::new(),
            compatibility: String::new(),
            execution_mode: String::new(),
            agent_threads: 1,
            required_brain_seconds: 10_800.0,
            agreement: Agreement::default(),
            brain_ms: 1_819_000.0,
            ledger_checks: 0,
            checkpoints: Checkpoints {
                identical: 400,
                declared: 0,
                unavailable: 0,
            },
            segments_compared: 1,
            skipped: Vec::new(),
            window_ends: Vec::new(),
            last_stop_trace: None,
            divergence: None,
            started_at: now_iso(),
            passed_at: None,
            current_trace: None,
            lag_transitions: 0,
            live_lag_seconds: None,
            spool_evicted: 0,
            cost: Cost::default(),
            run_id: None,
        };
        v.end_window("trace-1.jsonl", "no-consumer".to_owned());
        assert_eq!(v.brain_seconds(), 0.0);
        v.brain_ms += 170_000.0;
        let j = v.to_json();
        assert_eq!(j["compared"]["brainSeconds"], 170.0);
        assert_eq!(j["compared"]["checkpoints"]["identical"], 0);
        assert_eq!(j["window"]["lastStopTrace"], "trace-1.jsonl");
        assert_eq!(j["window"]["ends"][0]["discardedBrainSeconds"], 1819.0);
        // A gap is not an allowed skip.
        assert!(!SKIP_KINDS.contains(&"trace-cap"));
    }

    #[test]
    fn the_cutover_rule() {
        let now = parse_iso(&now_iso()).unwrap();
        let binaries: std::collections::BTreeMap<String, String> =
            [("flysim-session".to_owned(), "b".repeat(64))].into();
        let release = Release {
            dir: "/opt/fly/releases/test",
            binaries: &binaries,
        };
        let ok = |v: &Value| allows_cutover(v, "flysim-session", &release, "c", now, 300);
        let pass = verdict(Status::Pass, 10_800_000.0);
        assert_eq!(ok(&pass), Ok(()));
        // A verdict that does not say where its window starts is an older shadow's (B2).
        let mut undated = pass.clone();
        undated.as_object_mut().unwrap().remove("window");
        assert!(ok(&undated).is_err());
        // Another binary, another release, a changed file in it.
        assert!(allows_cutover(&pass, "fly-other", &release, "c", now, 300).is_err());
        let moved = Release {
            dir: "/opt/fly/releases/other",
            binaries: &binaries,
        };
        assert!(allows_cutover(&pass, "flysim-session", &moved, "c", now, 300).is_err());
        let changed: std::collections::BTreeMap<String, String> =
            [("flysim-session".to_owned(), "a".repeat(64))].into();
        let rebuilt = Release {
            dir: "/opt/fly/releases/test",
            binaries: &changed,
        };
        assert!(allows_cutover(&pass, "flysim-session", &rebuilt, "c", now, 300).is_err());
        assert!(allows_cutover(&pass, "flysim-session", &release, "other", now, 300).is_err());
        assert!(
            allows_cutover(&pass, "flysim-session", &release, "c", now + 301_000, 300).is_err()
        );
        // A clock stepped back: a verdict dated well ahead of now is refused, a small skew is not.
        assert!(
            allows_cutover(&pass, "flysim-session", &release, "c", now - 120_000, 300).is_err()
        );
        assert!(allows_cutover(&pass, "flysim-session", &release, "c", now - 30_000, 300).is_ok());
        assert!(ok(&verdict(Status::Pass, 10_799_000.0)).is_err());
        // A verdict that asked for less than the operator's 3 h never passes the check.
        let mut short_window = verdict(Status::Pass, 67_000.0);
        short_window["required"]["brainSeconds"] = json!(60.0);
        assert!(ok(&short_window).is_err());
        for status in [
            Status::Running,
            Status::Diverged,
            Status::Stopped,
            Status::Error,
        ] {
            assert!(ok(&verdict(status, 10_800_000.0)).is_err());
        }
        let mut diverged = pass.clone();
        diverged["firstDivergence"] = json!({"kind": "transition"});
        assert!(ok(&diverged).is_err());
        // Saves: none compared, or too many unavailable.
        let mut starved = pass.clone();
        starved["compared"]["checkpoints"] =
            json!({"identical": 0, "declared": 0, "unavailable": 0});
        assert!(ok(&starved).is_err());
        starved["compared"]["checkpoints"] =
            json!({"identical": 100, "declared": 0, "unavailable": 50});
        assert!(ok(&starved).is_err());
        // Skips: the live side's own are fine, anything else is not.
        let mut skipped = pass.clone();
        skipped["skipped"] = json!([{"kind": "startup-save-gone", "reason": "x"}]);
        assert_eq!(ok(&skipped), Ok(()));
        skipped["skipped"] = json!([{"kind": "trace-malformed", "reason": "x"}]);
        assert!(ok(&skipped).is_err());
        // Behind the live trace.
        let mut behind = pass.clone();
        behind["lagTransitions"] = json!(MAX_LAG_TRANSITIONS + 1);
        assert!(ok(&behind).is_err());
    }
}
