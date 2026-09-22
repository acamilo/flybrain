//! `seed-derivation-v1` against its test vectors.

use fly_session_types::{fixtures, seed};
use serde_json::Value;

#[test]
fn every_vector_derives_its_recorded_seed() {
    let file = fixtures::load("seed-vectors.json").expect("seed-vectors.json");
    assert_eq!(
        file.get("algorithm").and_then(Value::as_str),
        Some(seed::ALGORITHM)
    );
    let vectors = file.get("vectors").and_then(Value::as_array).expect("vectors");
    for case in vectors {
        let master: u64 = fixtures::field(case, "masterSeed")
            .expect("masterSeed")
            .parse()
            .expect("a U64");
        let agent = fixtures::field(case, "agentId").expect("agentId");
        assert_eq!(
            String::from_utf8(seed::material(master, agent).expect("material")).expect("utf-8"),
            fixtures::field(case, "material").expect("material"),
            "the hashed material is part of the specification"
        );
        assert_eq!(
            seed::material_digest(master, agent).expect("digest"),
            fixtures::field(case, "materialDigest").expect("materialDigest")
        );
        assert_eq!(
            i64::from(seed::agent_seed(master, agent).expect("seed")),
            case.get("seed").and_then(Value::as_i64).expect("seed"),
            "seed for {agent} under master {master}"
        );
    }
    assert!(vectors.len() >= 20, "keep the vector table broad");
}

#[test]
fn one_composition_gets_independent_seeds() {
    let file = fixtures::load("seed-vectors.json").expect("seed-vectors.json");
    let composition = file.get("composition").expect("composition");
    let master: u64 = fixtures::field(composition, "masterSeed")
        .expect("masterSeed")
        .parse()
        .expect("a U64");
    let ids: Vec<String> = composition
        .get("agentIds")
        .and_then(Value::as_array)
        .expect("agentIds")
        .iter()
        .map(|v| v.as_str().expect("an id").to_owned())
        .collect();
    let seeds = seed::composition_seeds(master, &ids).expect("seeds");
    let recorded: Vec<i64> = composition
        .get("seeds")
        .and_then(Value::as_array)
        .expect("seeds")
        .iter()
        .map(|v| v.as_i64().expect("a seed"))
        .collect();
    assert_eq!(
        seeds.iter().map(|s| i64::from(*s)).collect::<Vec<_>>(),
        recorded
    );
    let mut unique = seeds.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), seeds.len(), "per-agent seeds are independent");
    assert!(
        seeds.iter().all(|s| *s != 0),
        "a zero seed would stall an xorshift generator"
    );
}

#[test]
fn a_different_master_seed_or_agent_id_derives_a_different_seed() {
    assert_ne!(
        seed::agent_seed(0, "fly-a").expect("seed"),
        seed::agent_seed(1, "fly-a").expect("seed")
    );
    assert_ne!(
        seed::agent_seed(0, "fly-a").expect("seed"),
        seed::agent_seed(0, "fly-b").expect("seed")
    );
    assert_eq!(
        seed::agent_seed(7, "fly-a").expect("seed"),
        seed::agent_seed(7, "fly-a").expect("seed"),
        "the derivation is a function of its recorded inputs"
    );
}

#[test]
fn invalid_inputs_are_refused_rather_than_normalized() {
    let file = fixtures::load("seed-vectors.json").expect("seed-vectors.json");
    for case in file.get("invalid").and_then(Value::as_array).expect("invalid") {
        let master: u64 = fixtures::field(case, "masterSeed")
            .expect("masterSeed")
            .parse()
            .expect("a U64");
        if let Ok(agent) = fixtures::field(case, "agentId") {
            assert!(
                seed::agent_seed(master, agent).is_err(),
                "{agent:?} must be refused: {}",
                fixtures::field(case, "reason").unwrap_or("")
            );
        } else {
            let ids: Vec<String> = case
                .get("agentIds")
                .and_then(Value::as_array)
                .expect("agentIds")
                .iter()
                .map(|v| v.as_str().expect("an id").to_owned())
                .collect();
            assert!(
                seed::composition_seeds(master, &ids).is_err(),
                "a repeated agent id must be refused"
            );
        }
    }
}
