//! `FLY_TRACE`'s boundary half is the session framework's, field for field.
//!
//! The recorder writes `behaviour.boundaryActions` and `operational.captures` in the shapes of
//! `TraceBehaviour.boundaryActions` and `TraceOperational.captures` (step-v1 section 8, amended
//! 2026-09-23). This test records the two boundaries the legacy loop produces and reads them with
//! `fly-session-types` itself, by grafting them onto the shared baseline trace:
//!
//! - a rollback followed by the post-recovery checkpoint is a valid transition trace;
//! - a rung climb -- the milestone archive, *then* the ratchet's slot save -- is refused with the
//!   capture-before-save error. That is the legacy order legacy-gameboy-v1 section 4 declares, and
//!   the trace has to show it as it happens rather than tidy it away.

use fly_session_types::fixtures;
use fly_session_types::scalar::DomainType;
use fly_session_types::trace::TransitionTrace;
use flysim::trace::FrameTrace;
use serde_json::Value;

fn lines(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("the trace")
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect()
}

/// The shared baseline transition with this boundary's actions and captures in place of its own.
fn grafted(record: &Value) -> Result<TransitionTrace, String> {
    let file = fixtures::load("traces.json").expect("traces.json");
    let mut baseline = file.get("baseline").expect("baseline").clone();
    baseline["behaviour"]["boundaryActions"] = record["behaviour"]["boundaryActions"].clone();
    baseline["operational"]["captures"] = record["operational"]["captures"].clone();
    TransitionTrace::from_json(&baseline).map_err(|error| error.to_string())
}

#[test]
fn the_boundary_half_of_the_trace_is_the_session_frameworks() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("trace.jsonl");
    let mut trace = FrameTrace::create(&path).expect("the trace file");

    // The start: the boot checkpoint, before any transition.
    trace.capture(1, 100);
    // Transition 100 -> 101 ends in a stall rollback, then the post-recovery checkpoint.
    trace.sugar(400.0);
    trace.begin(100, 1_000.0);
    trace.rolled_back(&[]);
    trace.capture(2, 101);
    // Transition 101 -> 102 climbs a rung: the archive first, then the ratchet captures.
    trace.begin(101, 1_017.0);
    trace.capture(3, 102);
    trace.slot_saved(b"emulator state");
    trace.finish();
    drop(trace);

    let lines = lines(&path);
    assert_eq!(lines.len(), 4, "format, start, two transitions: {lines:?}");
    assert_eq!(lines[0]["format"], flysim::trace::FORMAT);
    assert_eq!(lines[1]["boundary"], "100");
    assert_eq!(lines[1]["operational"]["captures"][0]["checkpointId"], "g1");

    let rollback = &lines[2];
    assert_eq!(rollback["behaviour"]["step"], "100");
    assert_eq!(rollback["behaviour"]["admissions"][0]["kind"], "sugar");
    assert_eq!(
        rollback["behaviour"]["boundaryActions"][0]["kind"],
        "rollback"
    );
    assert_eq!(rollback["operational"]["captures"][0]["afterActions"], 1);
    let parsed = grafted(rollback).expect("a rollback and then its checkpoint is a valid trace");
    assert_eq!(parsed.behaviour.boundary_actions.len(), 1);
    assert_eq!(parsed.operational.captures.len(), 1);

    let climb = &lines[3];
    assert_eq!(
        climb["behaviour"]["boundaryActions"][0]["kind"],
        "save-slot"
    );
    assert_eq!(
        climb["behaviour"]["boundaryActions"][0]["slotId"],
        flysim::trace::SLOT
    );
    assert_eq!(
        climb["behaviour"]["boundaryActions"][0]["stateDigest"],
        flysim::trace::sha256_hex(b"emulator state")
    );
    assert_eq!(climb["operational"]["captures"][0]["afterActions"], 0);
    let refused = grafted(climb).expect_err("the legacy archive precedes the slot save");
    assert!(
        refused.contains("before this boundary's slot saves"),
        "refused for the declared reason: {refused}"
    );
}
