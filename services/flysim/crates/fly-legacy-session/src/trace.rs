//! The session's own `FLY_TRACE`: the legacy loop's trace format (`flysim-legacy-frame-trace-v1`,
//! FND-01) written from a session-framework run, and the comparison of two of them.
//!
//! FND-01's trace is what the running service does, frame by frame, as digests. A new-runtime
//! session driven from the same checkpoint with the same admissions must write the same lines. So
//! the session trace is assembled from exactly what crosses the session's boundaries --
//! `Agent.Prepare` (ticks, clock, remainder, decision), `Agent.Commit` (rates, spike bitset), the
//! executor's joypad batch, `Environment.Advance`'s view, the task's image-derived fields, and the
//! boundary actions -- and compared as JSON, with one declared exclusion: the decision is compared
//! as `gameboy-channels-v1` sees it ([`channels`]): the buttons down, and the first channel of the
//! context's bound set the decode holds. The decoder's list order is not in the decision schema,
//! and a macro channel the macro group still holds after its scene unbound it (the legacy `active`
//! can carry one) reaches nothing: the macro layer starts only a bound channel, and the joypad mask
//! is the buttons'. The mask, the macro events and every other field are compared as they are.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use fly_session::coordinator::StepDetails;
use fly_session::fly_session_types::gameboy::GAMEBOY_BUTTONS;
use fly_session::legacy_parity::trace_rates_digest;
use fly_session::types::*;

use crate::task::TaskRecord;

pub const FORMAT: &str = "flysim-legacy-frame-trace-v1";

/// flysim's `remainder_ns`: the exact rational nanoseconds of the frame remainder.
fn remainder_json(remainder: &RationalNs) -> Value {
    json!({"numerator": remainder.numerator.to_string(), "denominator": remainder.denominator.to_string()})
}

fn popcount(bytes: &[u8]) -> u64 {
    bytes.iter().map(|b| u64::from(b.count_ones())).sum()
}

/// One transition line of the session's trace, built from the coordinator's details of the
/// transition and the task's record of it.
pub fn line(details: &StepDetails, record: &TaskRecord) -> Result<Value, String> {
    let [(agent_id, prepared)] = details.prepared.as_slice() else {
        return Err("the legacy composition has exactly one agent".to_owned());
    };
    let commit = details
        .commits
        .iter()
        .find(|(id, _)| id == agent_id)
        .map(|(_, result)| result)
        .ok_or("no commit for the agent")?;
    let spikes = details
        .commit_attachments
        .iter()
        .find(|(id, name, _)| {
            id == agent_id && name == fly_session::legacy_agent::SPIKES_ATTACHMENT
        })
        .map(|(_, _, bytes)| bytes)
        .ok_or("the commit carried no spike bitset")?;
    let step: u64 = details
        .engine_frame
        .as_deref()
        .ok_or("O[k] has no engineFrame")?
        .parse()
        .map_err(|_| "engineFrame is not a U64")?;
    let admissions: Vec<Value> = details
        .admissions
        .iter()
        .map(|admission| match admission {
            fly_session::coordinator::Admission::Stimulus { duration_ms, .. } => {
                json!({"kind": "sugar", "durationMs": duration_ms})
            }
        })
        .collect();
    let framebuffer = details
        .view_digests
        .iter()
        .find(|(view, _)| view == "lcd")
        .map(|(_, digest)| digest.clone())
        .ok_or("O[k+1] has no lcd view")?;
    let boundary_actions: Vec<Value> = details
        .boundary_actions
        .iter()
        .map(|action| {
            json!({
                "kind": action.kind.as_str(),
                "slotId": action.slot_id,
                "stateDigest": action.state_digest,
            })
        })
        .collect();
    Ok(json!({
        "behaviour": {
            "step": step.to_string(),
            "admissions": admissions,
            "ticksAdvanced": prepared.ticks_advanced.to_string(),
            "brainTicks": prepared.brain_ticks.to_string(),
            "remainder": remainder_json(&prepared.remainder),
            "ratesDigest": trace_rates_digest(&commit.telemetry)?,
            "spikesDigest": sha256_hex(spikes),
            "spikeCount": popcount(spikes),
            "decision": record.decision,
            "mask": record.mask,
            "macroEvents": record.macro_events,
            "framebufferDigest": framebuffer,
            "wramDigest": record.wram_digest,
            "rewards": record.rewards,
            "rank": record.rank,
            "acknowledgedBoundary": (step + 1).to_string(),
            "boundaryActions": boundary_actions,
        }
    }))
}

/// The decision as `gameboy-channels-v1` sees it, given the bound channels it was masked to: the
/// buttons down, and the first bound channel the active set holds (AGENT-01's rule).
pub fn channels(decision: &Value, bound: &[String]) -> (BTreeSet<String>, Option<String>) {
    let active: Vec<&str> = decision
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let buttons = active
        .iter()
        .filter(|name| GAMEBOY_BUTTONS.contains(name))
        .map(|name| (*name).to_owned())
        .collect();
    let macro_channel = bound
        .iter()
        .find(|channel| active.contains(&channel.as_str()))
        .cloned();
    (buttons, macro_channel)
}

/// Reads a trace file's transition lines (`behaviour` only), skipping the header and the start
/// boundary line.
pub fn behaviours(text: &str) -> Result<Vec<Value>, String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header: Value = serde_json::from_str(lines.next().ok_or("an empty trace")?)
        .map_err(|e| format!("header: {e}"))?;
    if header["format"] != FORMAT {
        return Err(format!("not {FORMAT}: {}", header["format"]));
    }
    let mut out = Vec::new();
    for line in lines {
        let value: Value = serde_json::from_str(line).map_err(|e| format!("line: {e}"))?;
        if let Some(behaviour) = value.get("behaviour") {
            out.push(behaviour.clone());
        }
    }
    Ok(out)
}

/// What a comparison found, for the report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Agreement {
    pub transitions: u64,
    pub sugar: u64,
    pub reward_pulses: u64,
    pub rewards: u64,
    pub reward_kinds: Vec<String>,
    pub macro_events: u64,
    pub pressed: u64,
    pub saves: u64,
    pub rollbacks: u64,
    pub spikes: u64,
}

/// The first field two transition lines disagree on, with both values.
#[derive(Clone, Debug, PartialEq)]
pub struct Difference {
    pub field: String,
    pub legacy: Value,
    pub session: Value,
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:\n  legacy  {}\n  session {}",
            self.field, self.legacy, self.session
        )
    }
}

/// One transition, field by field over the union of both lines' keys, the decision as
/// [`channels`] of the session's bound set for that transition.
pub fn compare_line(
    legacy: &Value,
    session: &Value,
    bound: &[String],
) -> Result<(), Box<Difference>> {
    let (Some(a_map), Some(b_map)) = (legacy.as_object(), session.as_object()) else {
        return Err(Box::new(Difference {
            field: "(line)".to_owned(),
            legacy: legacy.clone(),
            session: session.clone(),
        }));
    };
    let keys: BTreeSet<&String> = a_map.keys().chain(b_map.keys()).collect();
    for key in keys {
        let (x, y) = (&legacy[key.as_str()], &session[key.as_str()]);
        let same = if key == "decision" {
            channels(x, bound) == channels(y, bound)
        } else {
            x == y
        };
        if !same {
            return Err(Box::new(Difference {
                field: key.clone(),
                legacy: x.clone(),
                session: y.clone(),
            }));
        }
    }
    Ok(())
}

impl Agreement {
    /// Counts one agreed transition line.
    pub fn count(&mut self, line: &Value) {
        self.transitions += 1;
        for admission in line["admissions"].as_array().into_iter().flatten() {
            match admission["kind"].as_str() {
                Some("sugar") => self.sugar += 1,
                Some("reward") => self.reward_pulses += 1,
                _ => {}
            }
        }
        for reward in line["rewards"].as_array().into_iter().flatten() {
            self.rewards += 1;
            if let Some(kind) = reward["kind"].as_str()
                && !self.reward_kinds.iter().any(|k| k == kind)
            {
                self.reward_kinds.push(kind.to_owned());
            }
        }
        self.macro_events += line["macroEvents"].as_array().map_or(0, |v| v.len() as u64);
        if line["mask"].as_u64().unwrap_or(0) != 0 {
            self.pressed += 1;
        }
        for action in line["boundaryActions"].as_array().into_iter().flatten() {
            match action["kind"].as_str() {
                Some("save-slot") => self.saves += 1,
                Some("rollback") => self.rollbacks += 1,
                _ => {}
            }
        }
        self.spikes += line["spikeCount"].as_u64().unwrap_or(0);
    }
}

/// Every transition identical, field by field; the decision as [`channels`] of the session's bound
/// set for that transition. The first difference is the error, with both values.
pub fn compare(
    legacy: &[Value],
    session: &[Value],
    bounds: &[Vec<String>],
) -> Result<Agreement, String> {
    if bounds.len() != session.len() {
        return Err("one bound set per session transition".to_owned());
    }
    if legacy.len() != session.len() {
        return Err(format!(
            "the legacy trace has {} transitions, the session's {}",
            legacy.len(),
            session.len()
        ));
    }
    let mut agreement = Agreement::default();
    for (n, (a, b)) in legacy.iter().zip(session).enumerate() {
        compare_line(a, b, &bounds[n])
            .map_err(|d| format!("transition {n} (step {}) {d}", a["step"]))?;
        agreement.count(a);
    }
    Ok(agreement)
}

/// `GAMEBOY_BUTTONS`, re-exported for the decision reconstruction's tests.
pub const BUTTONS: [&str; 8] = GAMEBOY_BUTTONS;
