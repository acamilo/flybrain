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
//!
//! `pass` is sticky only while nothing diverges: a shadow that keeps running after it passed and
//! then finds a difference turns the verdict to `diverged`. `stopped` is a shadow that exited
//! before it passed (a `pass` stays `pass` when the shadow stops). `error` is the shadow's own
//! failure (it could not read its inputs), which proves nothing either way.
//!
//! **The cutover rule (for CUT-01).** Cut over only if, read at the moment of cutting over:
//! `format` is `fly-shadow-verdict-v1`; `status` is `pass`; `firstDivergence` is `null`;
//! the session-runtime binary being switched to is one of `candidate.binaries`, by name and
//! SHA-256 (the shadow proved the release it shipped in, not another build);
//! `candidate.compatibility` is the live service's `--print-compatibility`; and `updatedAt` is no
//! older than a few minutes (the shadow is still following). Anything else keeps the legacy fly.

use std::path::Path;

use serde_json::{Value, json};

use crate::trace::{Agreement, Difference};

pub const FORMAT: &str = "fly-shadow-verdict-v1";

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
    pub reason: String,
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
    pub divergence: Option<Divergence>,
    pub started_at: String,
    pub passed_at: Option<String>,
    pub current_trace: Option<String>,
    /// Complete live lines not yet compared in the current segment, when known.
    pub lag_transitions: u64,
    pub live_lag_seconds: Option<f64>,
    pub spool_evicted: u64,
    pub cost: Cost,
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
                "trace": s.trace, "transitionsCompared": s.transitions_compared, "reason": s.reason,
            })).collect::<Vec<_>>(),
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

/// Reads a verdict file and says whether it allows the cutover, by the rule in the module notes.
/// `binary` is the file name and SHA-256 of the session-runtime binary CUT-01 switches to, and
/// `compatibility` the live service's; `max_age_s` bounds `updatedAt`.
pub fn allows_cutover(
    verdict: &Value,
    binary: (&str, &str),
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
    let (name, sha256) = binary;
    if verdict["candidate"]["binaries"][name] != sha256 {
        return Err(format!(
            "the verdict is for another build: {name} {sha256} is not among the shadowed release's binaries"
        ));
    }
    if verdict["candidate"]["compatibility"] != compatibility {
        return Err("the verdict is for another compatibility string".to_owned());
    }
    let compared = verdict["compared"]["brainSeconds"].as_f64().unwrap_or(0.0);
    let required = verdict["required"]["brainSeconds"]
        .as_f64()
        .unwrap_or(f64::INFINITY);
    if compared < required {
        return Err(format!("{compared} of {required} brain seconds compared"));
    }
    let updated = verdict["updatedAt"]
        .as_str()
        .and_then(parse_iso)
        .ok_or("no updatedAt")?;
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
            binaries: [("fly-session".to_owned(), "b".repeat(64))].into(),
            release: "/opt/fly/releases/test".to_owned(),
            compatibility: "c".to_owned(),
            execution_mode: "in-process".to_owned(),
            agent_threads: 2,
            required_brain_seconds: 10_800.0,
            agreement: Agreement::default(),
            brain_ms,
            ledger_checks: 0,
            checkpoints: Checkpoints::default(),
            segments_compared: 1,
            skipped: Vec::new(),
            divergence: None,
            started_at: now_iso(),
            passed_at: None,
            current_trace: None,
            lag_transitions: 0,
            live_lag_seconds: None,
            spool_evicted: 0,
            cost: Cost::default(),
        }
        .to_json()
    }

    #[test]
    fn the_cutover_rule() {
        let now = parse_iso(&now_iso()).unwrap();
        let sha = "b".repeat(64);
        let bin = ("fly-session", sha.as_str());
        let pass = verdict(Status::Pass, 10_800_000.0);
        assert_eq!(allows_cutover(&pass, bin, "c", now, 300), Ok(()));
        let other = "a".repeat(64);
        assert!(allows_cutover(&pass, ("fly-session", &other), "c", now, 300).is_err());
        assert!(allows_cutover(&pass, ("fly-other", &sha), "c", now, 300).is_err());
        assert!(allows_cutover(&pass, bin, "other", now, 300).is_err());
        assert!(allows_cutover(&pass, bin, "c", now + 301_000, 300).is_err());
        let short = verdict(Status::Pass, 10_799_000.0);
        assert!(allows_cutover(&short, bin, "c", now, 300).is_err());
        for status in [
            Status::Running,
            Status::Diverged,
            Status::Stopped,
            Status::Error,
        ] {
            assert!(allows_cutover(&verdict(status, 10_800_000.0), bin, "c", now, 300).is_err());
        }
        let mut diverged = pass.clone();
        diverged["firstDivergence"] = json!({"kind": "transition"});
        assert!(allows_cutover(&diverged, bin, "c", now, 300).is_err());
    }
}
