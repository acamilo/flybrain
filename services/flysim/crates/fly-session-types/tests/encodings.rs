//! The scalar encodings agree with the bus's, and the four identities cannot be confused.

use fly_session_types::fixtures;
use fly_session_types::scalar::{
    ArtifactIdentity, BusCallId, DomainRequestId, OwnerKind, OwnerToken, is_digest, is_id, parse_u64,
};
use serde_json::Value;

/// The domain `Id`, `U64` and `Digest` are the bus encodings, not a second opinion about them.
#[test]
fn domain_scalars_are_the_bus_scalars() {
    let ids = [
        "a",
        "fly-a",
        "0",
        "a.b_c-d",
        "",
        "A",
        "-a",
        ".a",
        "a b",
        "fly/a",
        &"a".repeat(64),
        &"a".repeat(65),
    ];
    for id in ids {
        assert_eq!(
            is_id(id),
            flybus::wire::is_id(id),
            "Id encoding must agree with the bus for {id:?}"
        );
    }
    let numbers = [
        "0",
        "1",
        "18446744073709551615",
        "18446744073709551616",
        "01",
        "",
        "-1",
        "1.0",
        " 1",
    ];
    for text in numbers {
        assert_eq!(
            parse_u64(text),
            flybus::wire::parse_u64(text),
            "U64 encoding must agree with the bus for {text:?}"
        );
    }
    let digests = [
        &"a".repeat(64),
        &"0".repeat(64),
        &"A".repeat(64),
        &"g".repeat(64),
        &"a".repeat(63),
    ];
    for digest in digests {
        assert_eq!(
            is_digest(digest),
            flybus::wire::is_digest(digest),
            "Digest encoding must agree with the bus"
        );
    }
}

#[test]
fn u64_boundaries_reject_from_the_fixture() {
    let file = fixtures::load("boundaries.json").expect("boundaries.json");
    let cases = file.get("u64").and_then(Value::as_array).expect("u64");
    for case in cases {
        let text = fixtures::field(case, "text").expect("text");
        let accept = case.get("accept").and_then(Value::as_bool).expect("accept");
        assert_eq!(
            parse_u64(text).is_some(),
            accept,
            "{text:?}: {}",
            fixtures::field(case, "reason").unwrap_or("")
        );
    }
}

/// bus callId, domain requestId and delivery/hold owner tokens are four types. The fixture
/// says, for every spelling, which of them accept it: no string is accepted by two.
#[test]
fn the_four_identities_never_accept_each_others_spellings() {
    let file = fixtures::load("identities.json").expect("identities.json");
    for case in fixtures::cases(&file).expect("cases") {
        let text = fixtures::field(case, "text").expect("text");
        let call = case.get("busCallId").and_then(Value::as_bool).expect("busCallId");
        let request = case
            .get("domainRequestId")
            .and_then(Value::as_bool)
            .expect("domainRequestId");
        let owner = case.get("ownerToken").expect("ownerToken");
        assert_eq!(BusCallId::parse(text).is_ok(), call, "busCallId {text:?}");
        assert_eq!(
            DomainRequestId::parse(text).is_ok(),
            request,
            "domainRequestId {text:?}"
        );
        match owner {
            Value::Null => assert!(
                OwnerToken::parse(text).is_err(),
                "owner token {text:?} must be refused"
            ),
            Value::String(kind) => {
                let parsed = OwnerToken::parse(text).expect("an owner token");
                let expected = match kind.as_str() {
                    "delivery" => OwnerKind::Delivery,
                    "hold" => OwnerKind::Hold,
                    other => panic!("unknown owner kind {other:?}"),
                };
                assert_eq!(parsed.kind(), expected, "owner kind of {text:?}");
            }
            other => panic!("unexpected ownerToken field {other:?}"),
        }
        let accepted = [call, request, OwnerToken::parse(text).is_ok()]
            .iter()
            .filter(|a| **a)
            .count();
        assert!(
            accepted <= 1,
            "{text:?} is accepted by more than one identity type"
        );
    }
}

/// An artifact identity is the naming half of a bus ArtifactRef, and nothing else in these
/// contracts is one: an AssetRef is persistent installed content, not live bytes.
#[test]
fn artifact_identity_is_the_naming_half_of_an_artifact_ref() {
    let file = fixtures::load("identities.json").expect("identities.json");
    let section = file.get("artifact").expect("artifact");
    let reference =
        flybus::wire::ArtifactRef::from_json(section.get("ref").expect("ref")).expect("a ref");
    let identity = ArtifactIdentity::of(&reference);
    identity.validate().expect("a valid identity");
    let expected = section.get("identity").expect("identity");
    assert_eq!(identity.store_id, expected["storeId"].as_str().unwrap());
    assert_eq!(identity.artifact_id, expected["artifactId"].as_str().unwrap());
    assert_eq!(identity.generation.to_string(), expected["generation"].as_str().unwrap());

    let asset = fly_session_types::workers::AssetRef::from_json(section_asset(&file)).expect("asset");
    assert_ne!(
        asset.id, identity.artifact_id,
        "the fixture's asset and artifact are deliberately different things"
    );
}

fn section_asset(file: &Value) -> &Value {
    file.get("asset").expect("asset")
}

/// A delivery id or hold token is connection-private. The domain reader has no field that
/// takes one, which is what `canonical::reject_bus_identities` enforces; here we only pin
/// that the two prefixes the bus issues are the two kinds this type knows.
#[test]
fn owner_tokens_come_in_exactly_two_kinds() {
    assert_eq!(OwnerToken::delivery(7).as_str(), "dlv-7");
    assert_eq!(OwnerToken::hold(9).as_str(), "own-9");
    assert_eq!(OwnerToken::delivery(7).kind(), OwnerKind::Delivery);
    assert_eq!(OwnerToken::hold(9).kind(), OwnerKind::Hold);
    assert_eq!(BusCallId::from_serial(12).as_str(), "call-12");
    assert_eq!(DomainRequestId::from_serial(41).as_str(), "req-41");
    assert_eq!(DomainRequestId::from_serial(41).serial(), 41);
}

use fly_session_types::scalar::DomainType as _;
