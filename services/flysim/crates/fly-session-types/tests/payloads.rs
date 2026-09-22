//! The `valid.json`, `invalid.json`, `raw.json` and `generated.json` fixtures.

mod common;

use fly_session_types::scalar::{DomainType, MAX_TYPED_VALUE_BYTES, SchemaRef, TypedValue};
use fly_session_types::{canonical, fixtures};
use serde_json::{Value, json};

use common::round_trip;

#[test]
fn every_valid_case_round_trips_and_canonicalizes_to_its_recorded_bytes() {
    let file = fixtures::load("valid.json").expect("valid.json");
    let cases = fixtures::cases(&file).expect("cases");
    for case in cases {
        let name = fixtures::field(case, "name").expect("name");
        let type_name = fixtures::field(case, "type").expect("type");
        let value = case.get("value").expect("value");
        let written = round_trip(type_name, value)
            .unwrap_or_else(|e| panic!("{name} ({type_name}) must be accepted: {e}"));
        let canonical_in = canonical::canonicalize(value).expect("canonicalizable");
        let canonical_out = canonical::canonicalize(&written).expect("canonicalizable");
        assert_eq!(
            canonical_in, canonical_out,
            "{name}: reading and writing must preserve every field"
        );
        assert_eq!(
            canonical_in,
            fixtures::field(case, "canonical").expect("canonical"),
            "{name}: canonical JSON must match the fixture"
        );
        assert_eq!(
            canonical::sha256_hex(canonical_in.as_bytes()),
            fixtures::field(case, "digest").expect("digest"),
            "{name}: digest must match the fixture"
        );
    }
    assert!(cases.len() >= 70, "the valid fixture should stay broad");
}

#[test]
fn every_type_the_readers_know_appears_in_the_valid_fixture() {
    let file = fixtures::load("valid.json").expect("valid.json");
    let cases = fixtures::cases(&file).expect("cases");
    let covered: Vec<&str> = cases
        .iter()
        .map(|case| fixtures::field(case, "type").expect("type"))
        .collect();
    let missing: Vec<&&str> = common::READABLE_TYPES
        .iter()
        .filter(|t| !covered.contains(*t))
        .collect();
    assert!(
        missing.is_empty(),
        "every readable type needs at least one accepted fixture; missing {missing:?}"
    );
}

#[test]
fn every_invalid_case_is_refused() {
    let file = fixtures::load("invalid.json").expect("invalid.json");
    let cases = fixtures::cases(&file).expect("cases");
    for case in cases {
        let name = fixtures::field(case, "name").expect("name");
        let type_name = fixtures::field(case, "type").expect("type");
        let reason = fixtures::field(case, "reason").expect("reason");
        let value = case.get("value").expect("value");
        let outcome = round_trip(type_name, value);
        assert!(
            outcome.is_err(),
            "{name} ({type_name}) must be refused: {reason}"
        );
    }
    assert!(cases.len() >= 80, "the invalid fixture should stay broad");
}

#[test]
fn every_raw_byte_case_is_refused_before_or_during_validation() {
    let file = fixtures::load("raw.json").expect("raw.json");
    for case in fixtures::cases(&file).expect("cases") {
        let name = fixtures::field(case, "name").expect("name");
        let type_name = fixtures::field(case, "type").expect("type");
        let reason = fixtures::field(case, "reason").expect("reason");
        let bytes = fixtures::base64(case, "base64").unwrap_or_default();
        let outcome = canonical::parse_strict(&bytes).and_then(|value| {
            round_trip(type_name, &value).map_err(|e| fly_session_types::scalar::wire_err(e.0))
        });
        assert!(outcome.is_err(), "{name} must be refused: {reason}");
    }
}

/// The recipes in `generated.json`: payloads too large to store as fixtures.
#[test]
fn generated_boundary_cases_land_on_the_right_side_of_every_limit() {
    let file = fixtures::load("generated.json").expect("generated.json");
    let pad_schema = SchemaRef::from_json(file.get("padSchema").expect("padSchema")).expect("schema");
    for case in fixtures::cases(&file).expect("cases") {
        let name = fixtures::field(case, "name").expect("name");
        let kind = fixtures::field(case, "kind").expect("kind");
        let expect_accept = fixtures::field(case, "expect").expect("expect") == "accept";
        let outcome: Result<(), String> = match kind {
            "padded-typed-value" => {
                let pad = case.get("padCharacters").and_then(Value::as_u64).expect("pad") as usize;
                TypedValue::new(pad_schema.clone(), json!({"pad": "a".repeat(pad)}))
                    .map(|_| ())
                    .map_err(|e| e.0)
            }
            "padded-request" => {
                let pad = case.get("padCharacters").and_then(Value::as_u64).expect("pad") as usize;
                let total = case
                    .get("envelopeTotal")
                    .and_then(Value::as_u64)
                    .expect("envelopeTotal") as usize;
                let request = json!({
                    "requestId": "req-1",
                    "scope": Value::Null,
                    "params": {"pad": "a".repeat(pad)},
                });
                let body = round_trip("SessionRpcRequest", &request).expect("a request");
                let length = canonical::canonicalize(&body).expect("canonicalizable").len();
                canonical::require_envelope_fit(&body, total - length)
                    .map(|_| ())
                    .map_err(|e| e.0)
            }
            "error-message" | "error-message-astral" => {
                let points = case.get("codePoints").and_then(Value::as_u64).expect("codePoints")
                    as usize;
                let character = if kind == "error-message" { 'x' } else { '\u{10400}' };
                let message: String = std::iter::repeat_n(character, points).collect();
                let failure = json!({
                    "type": "error",
                    "requestId": "req-41",
                    "workerId": "fly-a",
                    "incarnationId": "inc-1",
                    "scope": Value::Null,
                    "error": {"code": "INTERNAL", "message": message, "mutation": "unknown"},
                });
                round_trip("SessionRpcFailure", &failure)
                    .map(|_| ())
                    .map_err(|e| e.0)
            }
            other => panic!("unknown generated case kind {other:?}"),
        };
        assert_eq!(
            outcome.is_ok(),
            expect_accept,
            "{name}: expected {}, got {outcome:?}",
            if expect_accept { "accept" } else { "reject" }
        );
    }
}

#[test]
fn a_typed_value_at_the_cap_is_accepted_and_one_byte_more_is_not() {
    let schema = SchemaRef::new("pad.v1", 1, &canonical::sha256_hex(b"pad.v1")).expect("schema");
    let overhead = canonical::canonicalize(
        &TypedValue::new(schema.clone(), json!({"pad": ""}))
            .expect("empty")
            .to_json(),
    )
    .expect("canonicalizable")
    .len();
    let at_cap = TypedValue::new(
        schema.clone(),
        json!({"pad": "a".repeat(MAX_TYPED_VALUE_BYTES - overhead)}),
    )
    .expect("exactly at the cap");
    assert_eq!(at_cap.canonical_len().expect("length"), MAX_TYPED_VALUE_BYTES);
    assert!(
        TypedValue::new(
            schema,
            json!({"pad": "a".repeat(MAX_TYPED_VALUE_BYTES - overhead + 1)})
        )
        .is_err(),
        "one byte over the cap must fail"
    );
}
