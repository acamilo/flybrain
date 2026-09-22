//! The step-v1 section 8 trace comparator: behaviour only.

use fly_session_types::fixtures;
use fly_session_types::scalar::DomainType;
use fly_session_types::trace::TransitionTrace;
use serde_json::Value;

fn trace(value: &Value) -> TransitionTrace {
    TransitionTrace::from_json(value).expect("a valid trace")
}

#[test]
fn every_variant_compares_the_way_the_fixture_says() {
    let file = fixtures::load("traces.json").expect("traces.json");
    let baseline = trace(file.get("baseline").expect("baseline"));
    for case in file
        .get("variants")
        .and_then(Value::as_array)
        .expect("variants")
    {
        let name = fixtures::field(case, "name").expect("name");
        let variant = trace(case.get("trace").expect("trace"));
        let expected = case
            .get("behaviourEquals")
            .and_then(Value::as_bool)
            .expect("behaviourEquals");
        let equal = baseline.behaviour_equals(&variant);
        let diff = baseline.behaviour_diff(&variant);
        assert_eq!(
            equal, expected,
            "{name}: behaviour equality. differences: {diff:?}"
        );
        assert_eq!(
            diff.is_empty(),
            expected,
            "{name}: the diff must be empty exactly when the behaviour matches"
        );
        if let Ok(needle) = fixtures::field(case, "diffContains") {
            assert!(
                diff.iter().any(|line| line.contains(needle)),
                "{name}: the diff should name {needle:?}, got {diff:?}"
            );
        }
        if expected {
            assert_eq!(
                baseline.behaviour.digest().expect("digest"),
                variant.behaviour.digest().expect("digest"),
                "{name}: equal behaviour has one digest"
            );
        }
    }
}

#[test]
fn a_whole_run_compares_transition_by_transition() {
    let file = fixtures::load("traces.json").expect("traces.json");
    let baseline = trace(file.get("baseline").expect("baseline"));
    let variants = file
        .get("variants")
        .and_then(Value::as_array)
        .expect("variants");
    let reversed = trace(variants[0].get("trace").expect("trace"));
    let changed = trace(
        variants
            .iter()
            .find(|case| fixtures::field(case, "name").unwrap_or("") == "one extra neural tick")
            .expect("the extra tick variant")
            .get("trace")
            .expect("trace"),
    );
    assert!(TransitionTrace::runs_equal(
        &[baseline.clone(), baseline.clone()],
        &[reversed, baseline.clone()]
    ));
    assert!(!TransitionTrace::runs_equal(
        std::slice::from_ref(&baseline),
        &[changed]
    ));
    let longer = [baseline.clone(), baseline.clone()];
    assert!(
        !TransitionTrace::runs_equal(std::slice::from_ref(&baseline), &longer),
        "a run with more transitions is not the same run"
    );
}

/// The operational half is recorded, and never part of the comparison.
#[test]
fn operational_metadata_is_recorded_and_excluded() {
    let file = fixtures::load("traces.json").expect("traces.json");
    let baseline = trace(file.get("baseline").expect("baseline"));
    assert_eq!(baseline.operational.bus_call_ids.len(), 3);
    assert_eq!(baseline.operational.prepare_request_ids.len(), 2);
    assert_eq!(baseline.operational.delivery_ids.len(), 2);
    assert!(baseline.operational.wall_time_ns > 0);
    let retried = trace(
        file.get("variants")
            .and_then(Value::as_array)
            .expect("variants")
            .iter()
            .find(|case| {
                fixtures::field(case, "name").unwrap_or("")
                    == "a safe retry with fresh bus callIds, delivery ids and wall time"
            })
            .expect("the retry variant")
            .get("trace")
            .expect("trace"),
    );
    assert_ne!(
        baseline.operational, retried.operational,
        "the retry really did change the operational half"
    );
    assert!(baseline.behaviour_equals(&retried));
}
