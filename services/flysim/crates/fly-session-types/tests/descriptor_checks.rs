//! Rules that need a descriptor in hand: complete batches, the observation delay rule, byte
//! shapes and descriptor agreement.

use fly_session_types::fixtures;
use fly_session_types::publishing::{CommittedSnapshot, SessionDescriptor};
use fly_session_types::scalar::{DomainType, Result};
use fly_session_types::workers::{
    EnvironmentDescriptor, PortControl, SensoryInput, StepResult, WorldObservation,
};

#[test]
fn every_descriptor_check_lands_the_way_the_fixture_says() {
    let file = fixtures::load("descriptor-checks.json").expect("descriptor-checks.json");
    let descriptor =
        EnvironmentDescriptor::from_json(file.get("descriptor").expect("descriptor")).expect("descriptor");
    let delayed = EnvironmentDescriptor::from_json(file.get("delayedDescriptor").expect("delayed"))
        .expect("delayed descriptor");
    let session = SessionDescriptor::from_json(file.get("sessionDescriptor").expect("session"))
        .expect("session descriptor");
    let previous = WorldObservation::from_json(file.get("stepResultPrevious").expect("previous"))
        .expect("previous observation");

    for case in fixtures::cases(&file).expect("cases") {
        let name = fixtures::field(case, "name").expect("name");
        let kind = fixtures::field(case, "kind").expect("kind");
        let reason = fixtures::field(case, "reason").unwrap_or("");
        let expect_accept = fixtures::field(case, "expect").expect("expect") == "accept";
        let value = case.get("value").expect("value");
        let outcome: Result<()> = match kind {
            "portControl" => PortControl::from_json(value).and_then(|control| {
                match descriptor.port(&control.port_id) {
                    Some(port) => control.validate_against(&port.controls),
                    None => fly_session_types::scalar::err("no such port"),
                }
            }),
            "advanceControls" => value
                .as_array()
                .expect("an array of controls")
                .iter()
                .map(PortControl::from_json)
                .collect::<Result<Vec<_>>>()
                .and_then(|controls| descriptor.validate_batch(&controls)),
            "sensoryInput" => SensoryInput::from_json(value)
                .and_then(|input| input.validate_against(&descriptor.views)),
            "sensoryInputDelayed" => {
                SensoryInput::from_json(value).and_then(|input| input.validate_against(&delayed.views))
            }
            "worldObservation" => WorldObservation::from_json(value)
                .and_then(|observation| observation.validate_against(&descriptor)),
            "stepResult" => StepResult::from_json(value)
                .and_then(|result| result.validate_against(&descriptor, &previous)),
            "snapshot" => CommittedSnapshot::from_json(value)
                .and_then(|snapshot| snapshot.validate_against(&session)),
            other => panic!("unknown descriptor check kind {other:?}"),
        };
        assert_eq!(
            outcome.is_ok(),
            expect_accept,
            "{name}: expected {}. {reason}. outcome: {outcome:?}",
            if expect_accept { "accept" } else { "reject" }
        );
    }
}

/// The delay rule itself, stated once: `max(0, boundary - observationDelaySteps)`.
#[test]
fn the_required_producing_boundary_saturates_at_zero() {
    let file = fixtures::load("descriptor-checks.json").expect("descriptor-checks.json");
    let delayed = EnvironmentDescriptor::from_json(file.get("delayedDescriptor").expect("delayed"))
        .expect("delayed descriptor");
    let view = &delayed.views[0];
    assert_eq!(view.observation_delay_steps, 2);
    assert_eq!(view.required_produced_step(0), 0);
    assert_eq!(view.required_produced_step(1), 0);
    assert_eq!(view.required_produced_step(2), 0);
    assert_eq!(view.required_produced_step(3), 1);
    assert_eq!(view.frame_bytes(), u64::from(160u32 * 4 * 144));
}
