//! The canonical schema set, `contractDigest` and the freshness of every derived fixture.

use fly_session_types::{canonical, fixtures, schema};
use serde_json::Value;

#[path = "../examples/update_fixtures.rs"]
#[allow(dead_code, reason = "the example's main is not used by the test that reuses its writers")]
mod updater;

/// Every derived fixture is exactly what the updater writes today. If this fails, run
/// `cargo run -p fly-session-types --example update_fixtures` and review the diff.
#[test]
fn derived_fixtures_are_current() {
    for (name, expected) in updater::derived() {
        let path = fixtures::dir().join(&name);
        let found = std::fs::read_to_string(&path).expect("a checked-in fixture");
        assert_eq!(
            found, expected,
            "{name} is stale; regenerate it with the update_fixtures example"
        );
    }
}

#[test]
fn the_contract_digest_is_the_digest_of_the_checked_in_schema_set() {
    let text = fixtures::load_bytes("schema-set.json").expect("schema-set.json");
    let recorded = fixtures::load("contract-digest.json").expect("contract-digest.json");
    let expected = recorded
        .get("contractDigest")
        .and_then(Value::as_str)
        .expect("contractDigest");
    assert_eq!(schema::contract_digest(), expected);
    // The file is the canonical schema set plus one trailing newline.
    assert_eq!(
        canonical::sha256_hex(text.strip_suffix(b"\n").expect("trailing newline")),
        expected,
        "the digest is over the canonical schema set, byte for byte"
    );
}

/// The digest comes from the schema declaration, not from the source text: reparsing the
/// checked-in file with different whitespace and key order gives the same digest.
#[test]
fn the_contract_digest_survives_reformatting() {
    let bytes = fixtures::load_bytes("schema-set.json").expect("schema-set.json");
    let parsed = canonical::parse_strict(bytes.strip_suffix(b"\n").expect("newline")).expect("parses");
    let pretty = serde_json::to_vec_pretty(&parsed).expect("serializable");
    let reparsed = canonical::parse_strict(&pretty).expect("parses");
    assert_eq!(
        canonical::digest_of(&reparsed).expect("digest"),
        schema::contract_digest(),
        "pretty printing the schema set does not change its digest"
    );
    let shuffled = canonical::parse_strict(
        br#"{"version":1,"contract":"fly-session-types-other"}"#,
    )
    .expect("parses");
    assert_ne!(
        canonical::digest_of(&shuffled).expect("digest"),
        schema::contract_digest()
    );
}

/// ... and it changes when a schema changes: a renamed field, a widened bound, one more enum
/// member or one fewer type all move the digest.
#[test]
fn the_contract_digest_changes_when_a_schema_changes() {
    let baseline = schema::contract_digest();
    let mutate = |mutation: fn(&mut Value)| {
        let mut set = schema::schema_set();
        mutation(&mut set);
        canonical::digest_of(&set).expect("digest")
    };
    let renamed_field = mutate(|set| {
        set["types"][0]["fields"][0]["name"] = Value::String("sessionIdentifier".to_owned());
    });
    let widened_bound = mutate(|set| {
        for limit in set["limits"].as_array_mut().expect("limits") {
            if limit["name"] == Value::String("maxAgents".to_owned()) {
                limit["value"] = Value::from(8u64);
            }
        }
    });
    let extra_enum_member = mutate(|set| {
        set["enums"][0]["members"]
            .as_array_mut()
            .expect("members")
            .push(Value::String("s16le-interleaved".to_owned()));
    });
    let dropped_type = mutate(|set| {
        set["types"].as_array_mut().expect("types").pop();
    });
    let relaxed_constraint = mutate(|set| {
        set["types"][0]["fields"][0]["constraint"] = Value::String("anything".to_owned());
    });
    for (what, digest) in [
        ("a renamed field", renamed_field),
        ("a widened bound", widened_bound),
        ("an extra enum member", extra_enum_member),
        ("a dropped type", dropped_type),
        ("a relaxed constraint", relaxed_constraint),
    ] {
        assert_ne!(digest, baseline, "{what} must change contractDigest");
    }
}

#[test]
fn the_schema_set_names_every_type_the_crate_reads() {
    let set = schema::schema_set();
    let names: Vec<&str> = set["types"]
        .as_array()
        .expect("types")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    for expected in [
        "Scope",
        "RationalNs",
        "TypedValue",
        "SessionRpcRequest",
        "SessionRpcFailure",
        "PrepareParams",
        "StepResult",
        "ViewRef",
        "AudioRef",
        "CaptureResult",
        "SessionDescriptor",
        "CommittedSnapshot",
        "TraceBehaviour",
        "TraceOperational",
    ] {
        assert!(names.contains(&expected), "the schema set must name {expected}");
    }
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "the rendered set is sorted by type name");
    let mut unique = sorted.clone();
    unique.dedup();
    assert_eq!(unique.len(), names.len(), "no type is declared twice");
}

/// Every bound the schema set publishes is the constant the code enforces, and every bound
/// this crate chose rather than read from a document says so.
#[test]
fn published_limits_match_the_constants_and_name_their_source() {
    let set = schema::schema_set();
    let limits = set["limits"].as_array().expect("limits");
    let find = |name: &str| -> u64 {
        limits
            .iter()
            .find(|l| l["name"] == Value::String(name.to_owned()))
            .and_then(|l| l["value"].as_u64())
            .unwrap_or_else(|| panic!("the schema set must publish {name}"))
    };
    assert_eq!(find("maxAgents"), fly_session_types::workers::MAX_AGENTS as u64);
    assert_eq!(find("maxPorts"), fly_session_types::workers::MAX_PORTS as u64);
    assert_eq!(
        find("maxRateRoles"),
        fly_session_types::workers::MAX_RATE_ROLES as u64
    );
    assert_eq!(find("maxViews"), fly_session_types::media::MAX_VIEWS as u64);
    assert_eq!(
        find("maxTypedValueBytes"),
        fly_session_types::scalar::MAX_TYPED_VALUE_BYTES as u64
    );
    assert_eq!(
        find("maxEnvelopeBytes"),
        canonical::MAX_ENVELOPE_BYTES as u64
    );
    let crate_chosen: Vec<&str> = limits
        .iter()
        .filter(|l| l["source"] == Value::String("crate".to_owned()))
        .map(|l| l["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        crate_chosen,
        [
            "maxAssets",
            "maxAudioStreams",
            "maxCapabilities",
            "maxSnapshotEvents",
            "maxSupportedMajors",
            "maxSupportedStimuli",
        ],
        "a bound with no stated source must be declared as this crate's choice"
    );
}
