//! Canonical JSON (RFC 8785) and the digest rules of ipc-v1 section 5.

use fly_session_types::scalar::{DomainType, Scope};
use fly_session_types::{canonical, fixtures};
use serde_json::{Value, json};

#[test]
fn object_keys_are_sorted_by_utf16_code_unit() {
    let value = json!({"b": 1, "a": 2, "A": 3, "\u{00e9}": 4, "\u{10400}": 5, "\u{ff21}": 6});
    assert_eq!(
        canonical::canonicalize(&value).expect("canonicalizable"),
        "{\"A\":3,\"a\":2,\"b\":1,\"\u{00e9}\":4,\"\u{10400}\":5,\"\u{ff21}\":6}",
        "keys sort by UTF-16 code unit, so an astral key (leading surrogate D801) sorts \
         before U+FF21, which is where a JavaScript string sort puts it too and where a sort \
         by Unicode code point would not"
    );
}

#[test]
fn numbers_print_the_way_ecmascript_prints_them() {
    let file = fixtures::load("boundaries.json").expect("boundaries.json");
    for case in file.get("doubles").and_then(Value::as_array).expect("doubles") {
        let value = case.get("value").expect("value");
        let accept = case.get("accept").and_then(Value::as_bool).expect("accept");
        let outcome = canonical::canonicalize(value);
        assert_eq!(
            outcome.is_ok(),
            accept,
            "{value}: {}",
            fixtures::field(case, "reason").unwrap_or("")
        );
        if let (Ok(text), Some(expected)) = (outcome, case.get("canonical").and_then(Value::as_str))
        {
            assert_eq!(text, expected, "{value} must print as {expected}");
        }
    }
}

#[test]
fn strings_are_escaped_the_way_json_stringify_escapes_them() {
    let value = json!({"s": "quote \" backslash \\ tab \t newline \n bell \u{7} del \u{7f} e\u{301}"});
    assert_eq!(
        canonical::canonicalize(&value).expect("canonicalizable"),
        "{\"s\":\"quote \\\" backslash \\\\ tab \\t newline \\n bell \\u0007 del \u{7f} e\u{301}\"}",
        "only the escapes JSON.stringify emits, with lowercase hex"
    );
}

#[test]
fn canonical_form_does_not_depend_on_the_input_formatting() {
    let compact = br#"{"b":[1,2,{"y":true,"x":null}],"a":"z"}"#;
    let pretty = br#"{
          "a"  :  "z",
          "b": [ 1, 2, { "x": null, "y": true } ]
        }"#;
    let left = canonical::parse_strict(compact).expect("parses");
    let right = canonical::parse_strict(pretty).expect("parses");
    assert_eq!(
        canonical::canonicalize(&left).expect("canonicalizable"),
        canonical::canonicalize(&right).expect("canonicalizable")
    );
    assert_eq!(
        canonical::digest_of(&left).expect("digest"),
        canonical::digest_of(&right).expect("digest"),
        "whitespace and key order are not content"
    );
}

#[test]
fn duplicate_keys_and_invalid_utf8_never_parse() {
    assert!(canonical::parse_strict(br#"{"a":1,"a":2}"#).is_err());
    assert!(canonical::parse_strict(b"{\"a\":\"\xff\"}").is_err());
    assert!(canonical::parse_strict(br#"{"a":1} {"b":2}"#).is_err());
    assert!(canonical::parse_strict(br#"{"a":NaN}"#).is_err());
}

#[test]
fn an_envelope_over_64_kib_is_refused() {
    let big = json!({"pad": "a".repeat(canonical::MAX_ENVELOPE_BYTES)});
    assert!(canonical::require_envelope_fit(&big, 0).is_err());
    let small = json!({"pad": "a"});
    let length = canonical::canonicalize(&small).expect("canonicalizable").len();
    assert_eq!(
        canonical::require_envelope_fit(&small, canonical::MAX_ENVELOPE_BYTES - length)
            .expect("fits exactly"),
        canonical::MAX_ENVELOPE_BYTES
    );
    assert!(
        canonical::require_envelope_fit(&small, canonical::MAX_ENVELOPE_BYTES - length + 1)
            .is_err(),
        "one byte past the ceiling is refused"
    );
}

#[test]
fn operation_keys_match_the_fixture_and_separate_the_operations_they_should() {
    let file = fixtures::load("operations.json").expect("operations.json");
    let mut digests: Vec<(String, String)> = Vec::new();
    for case in file.get("keys").and_then(Value::as_array).expect("keys") {
        let name = fixtures::field(case, "name").expect("name");
        let scope = Scope::from_json(case.get("scope").expect("scope")).expect("scope");
        let method = fixtures::field(case, "method").expect("method");
        let worker = fixtures::field(case, "workerId").expect("workerId");
        let key = canonical::OperationKey::new(scope, method, worker).expect("a key");
        let digest = key.digest().expect("digest");
        assert_eq!(
            digest,
            fixtures::field(case, "digest").expect("digest"),
            "{name}: operation key digest must match the fixture"
        );
        digests.push((name.to_owned(), digest));
    }
    for (index, (name, digest)) in digests.iter().enumerate() {
        for (other_name, other) in &digests[index + 1..] {
            assert_ne!(
                digest, other,
                "{name} and {other_name} are different operations"
            );
        }
    }
}

#[test]
fn canonical_bodies_match_the_fixture() {
    let file = fixtures::load("operations.json").expect("operations.json");
    for case in file.get("bodies").and_then(Value::as_array).expect("bodies") {
        let name = fixtures::field(case, "name").expect("name");
        let method = fixtures::field(case, "method").expect("method");
        let scope = match case.get("scope") {
            Some(Value::Null) | None => None,
            Some(v) => Some(Scope::from_json(v).expect("scope")),
        };
        let params = case.get("params").expect("params");
        assert_eq!(
            canonical::body_digest(method, scope.as_ref(), params).expect("digest"),
            fixtures::field(case, "digest").expect("digest"),
            "{name}: canonical body digest must match the fixture"
        );
    }
}

/// The pairs that decide whether a duplicate is a safe replay or a CONFLICT.
#[test]
fn operation_pairs_agree_with_the_fixture_about_sameness() {
    let file = fixtures::load("operations.json").expect("operations.json");
    for case in file.get("pairs").and_then(Value::as_array).expect("pairs") {
        let name = fixtures::field(case, "name").expect("name");
        let reason = fixtures::field(case, "reason").expect("reason");
        let worker = fixtures::field(case, "workerId").expect("workerId");
        let right_worker = fixtures::field(case, "rightWorkerId").unwrap_or(worker);
        let side = |key: &str, worker: &str| {
            let value = case.get(key).expect("side");
            let method = fixtures::field(value, "method").expect("method");
            let scope = Scope::from_json(value.get("scope").expect("scope")).expect("scope");
            let params = value.get("params").expect("params");
            let key_digest = canonical::OperationKey::new(scope.clone(), method, worker)
                .expect("a key")
                .digest()
                .expect("digest");
            let body = canonical::body_digest(method, Some(&scope), params).expect("digest");
            (key_digest, body)
        };
        let (left_key, left_body) = side("left", worker);
        let (right_key, right_body) = side("right", right_worker);
        assert_eq!(
            left_key == right_key,
            case.get("sameKey").and_then(Value::as_bool).expect("sameKey"),
            "{name}: operation key sameness. {reason}"
        );
        assert_eq!(
            left_body == right_body,
            case.get("sameBody").and_then(Value::as_bool).expect("sameBody"),
            "{name}: canonical body sameness. {reason}"
        );
    }
}

#[test]
fn a_domain_body_can_never_carry_a_bus_identity() {
    let file = fixtures::load("operations.json").expect("operations.json");
    for case in file.get("rejected").and_then(Value::as_array).expect("rejected") {
        let name = fixtures::field(case, "name").expect("name");
        let method = fixtures::field(case, "method").expect("method");
        let scope = Scope::from_json(case.get("scope").expect("scope")).expect("scope");
        let params = case.get("params").expect("params");
        assert!(
            canonical::body_digest(method, Some(&scope), params).is_err(),
            "{name} must be refused: {}",
            fixtures::field(case, "reason").unwrap_or("")
        );
    }
}
