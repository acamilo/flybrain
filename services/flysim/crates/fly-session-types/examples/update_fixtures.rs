//! Regenerates the derived fixture files.
//!
//! `cargo run -p fly-session-types --example update_fixtures`. `tests/fixtures_current.rs`
//! fails if the checked-in files differ from what this writes, so the digests in the
//! fixtures can never drift from the code that produced them.

use std::collections::BTreeMap;

use fly_session_types::gameboy::{self, LegacyComposition};
use fly_session_types::scalar::{DomainType, RationalNs, Scope};
use fly_session_types::workers::AssetRef;
use fly_session_types::{canonical, checkpoint, fixtures, schema, seed};
use serde_json::{Map, Value, json};

fn main() {
    let dir = fixtures::dir();
    for (name, contents) in derived() {
        let path = dir.join(&name);
        std::fs::write(&path, contents).expect("write fixture");
        println!("wrote {}", path.display());
    }
}

/// Every derived fixture, as `(file name, exact bytes)`.
pub fn derived() -> Vec<(String, String)> {
    vec![
        ("schema-set.json".to_owned(), schema_set()),
        ("contract-digest.json".to_owned(), contract_digest()),
        ("valid.json".to_owned(), valid()),
        ("operations.json".to_owned(), operations()),
        ("seed-vectors.json".to_owned(), seed_vectors()),
        ("checkpoint-envelope.json".to_owned(), checkpoint_envelope()),
        ("gameboy-legacy.json".to_owned(), gameboy_legacy()),
    ]
}

/// An example legacy composition. The ROM digest is a placeholder -- the real one is computed
/// by the composition that runs, and no ROM identity belongs in a fixture. The macro channels and
/// the decoder digest are the real macros-mode vector of `gameboy-decoder-config.json`, which
/// `flysim`'s `legacy_profile_identity` test computes from `gameboy_decoder_config_with_macros`
/// and the TypeScript test from the oracle preset. The compatibility string is today's, byte for
/// byte, because its segments must agree with the declaration.
pub fn example_composition() -> LegacyComposition {
    let pokered = "0cd19d3b877b7dc66d12c7050bed9a7f38154d4b";
    LegacyComposition {
        composition_id: "pokered-live".to_owned(),
        profile: gameboy::profile_asset_ref(),
        executor: gameboy::ExecutorDeclaration {
            rom: AssetRef {
                id: "pokered-rom".to_owned(),
                digest: canonical::sha256_hex(b"placeholder: the cartridge digest is the operator's"),
                byte_length: 1_048_576,
                format: "gb-rom".to_owned(),
            },
            adapter: "pokered-unique8-v7".to_owned(),
            symbol_provenance: pokered.to_owned(),
            mode: "macros".to_owned(),
            macro_channels: decoder_vector("macros")["macroChannels"]
                .as_array()
                .expect("macroChannels")
                .iter()
                .map(|c| c.as_str().expect("channel").to_owned())
                .collect(),
        },
        decoder_config_digest: decoder_vector("macros")["digest"]
            .as_str()
            .expect("digest")
            .to_owned(),
        environment: gameboy::EnvironmentDeclaration {
            slots: vec!["best".to_owned()],
            audio_sample_rate: 48_000,
        },
        flysim_compatibility: format!(
            "{}/pokered-unique8-v7/{}/{}/binjgb:c60e138da5a795ebb55e56b11b7e90024e41112c/pokered:{pokered}/statefmt:199616-x86_64-unknown-linux-gnu",
            gameboy::KERNEL_VERSION,
            gameboy::FAFB_V783_FINGERPRINT,
            gameboy::PLASTICITY_VERSION,
        ),
    }
}

/// One case of the (flysim-written) decoder-config vectors.
fn decoder_vector(name: &str) -> Value {
    let file = fixtures::load("gameboy-decoder-config.json").expect("gameboy-decoder-config.json");
    fixtures::cases(&file)
        .expect("cases")
        .iter()
        .find(|c| c["name"] == Value::String(name.to_owned()))
        .unwrap_or_else(|| panic!("decoder vector {name}"))
        .clone()
}

/// The legacy Game Boy extension set, the profile document and its AssetRef, the clock
/// vector and an example composition with its digest.
fn gameboy_legacy() -> String {
    let profile = gameboy::legacy_profile().to_json();
    let schema_refs: Map<String, Value> = gameboy::PAYLOAD_SCHEMAS
        .iter()
        .map(|p| (p.id.to_owned(), p.schema_ref().to_json()))
        .collect();
    // The first frames from a zero remainder: the rational accumulator of step-v1 section 5,
    // which the legacy f64 accumulator equals exactly (legacy-gameboy-v1 section 3).
    let step = gameboy::step_duration();
    let tick = gameboy::tick_duration();
    let mut accumulator = RationalNs::ZERO;
    let mut frames = Vec::new();
    for _ in 0..12 {
        accumulator = accumulator.checked_add(&step).expect("no overflow");
        let (ticks, remainder) = accumulator.divide_floor(&tick).expect("positive tick");
        accumulator = remainder;
        frames.push(json!({"ticks": ticks.to_string(), "remainder": remainder.to_json()}));
    }
    let composition = example_composition();
    write(&json!({
        "description": "The legacy Game Boy composition (legacy-gameboy-v1): registered payload schemas with their SchemaRef digests, the one legacy profile document and its AssetRef, the frame clock, and an example composition declaration with its digest.",
        "extensionSetDigest": gameboy::extension_set_digest(),
        "extensionSet": gameboy::extension_set(),
        "schemaRefs": schema_refs,
        "profile": {
            "document": profile,
            "canonical": canonical::canonicalize(&profile).expect("canonicalizable"),
            "assetRef": gameboy::profile_asset_ref().to_json(),
        },
        "clock": {
            "stepDuration": step.to_json(),
            "tickDuration": tick.to_json(),
            "legacyMsPerFrame": "1000 / (4194304 / 70224) == 548625/32768 exactly",
            "frames": frames,
        },
        "composition": {
            "example": composition.to_json(),
            "digest": composition.digest().expect("digest"),
            "recipeLines": [
                "fly-session/composition-v1",
                "session=<sessionId>",
                "epoch=<epoch>",
                "contract=<contractDigest>",
                "agent=<agentId> port=<portId> profile=<profileDigest>  (one line per agent)",
                "declaration=<this digest>  (added by the 2026-09-23 amendment)",
            ],
        },
    }))
}

fn write(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("serializable");
    text.push('\n');
    text
}

fn schema_set() -> String {
    // The rendered set is itself canonical JSON, so the file the TypeScript package hashes is
    // byte for byte what the digest was taken over.
    let mut text = schema::schema_set_json().expect("canonicalizable");
    text.push('\n');
    text
}

fn contract_digest() -> String {
    let set = schema::schema_set_json().expect("canonicalizable");
    write(&json!({
        "description": "contractDigest is the SHA-256 of the canonical schema set in schema-set.json.",
        "contractDigest": schema::contract_digest(),
        "schemaSetVersion": schema::SCHEMA_SET_VERSION,
        "schemaSetBytes": set.len(),
        "types": schema::SCHEMAS.len(),
        "enums": schema::ENUMS.len(),
        "limits": schema::LIMITS.len(),
    }))
}

fn valid() -> String {
    let mut file = fixtures::load("valid.json").expect("valid.json");
    let cases = file
        .get_mut("cases")
        .and_then(Value::as_array_mut)
        .expect("cases");
    for case in cases.iter_mut() {
        let value = case.get("value").expect("value").clone();
        let canonical = canonical::canonicalize(&value).expect("canonicalizable");
        let digest = canonical::sha256_hex(canonical.as_bytes());
        let map = case.as_object_mut().expect("case object");
        map.insert("canonical".to_owned(), Value::String(canonical));
        map.insert("digest".to_owned(), Value::String(digest));
    }
    write(&file)
}

fn operations() -> String {
    let mut file = fixtures::load("operations.json").expect("operations.json");
    let scope_of = |case: &Value| -> Option<Scope> {
        match case.get("scope") {
            Some(Value::Null) | None => None,
            Some(v) => Some(Scope::from_json(v).expect("scope")),
        }
    };
    for key in file
        .get_mut("keys")
        .and_then(Value::as_array_mut)
        .expect("keys")
    {
        let scope = scope_of(key).expect("an operation key has a scope");
        let method = key.get("method").and_then(Value::as_str).expect("method");
        let worker = key.get("workerId").and_then(Value::as_str).expect("workerId");
        let digest = canonical::OperationKey::new(scope, method, worker)
            .expect("valid key")
            .digest()
            .expect("digest");
        key.as_object_mut()
            .expect("object")
            .insert("digest".to_owned(), Value::String(digest));
    }
    for body in file
        .get_mut("bodies")
        .and_then(Value::as_array_mut)
        .expect("bodies")
    {
        let scope = scope_of(body);
        let method = body.get("method").and_then(Value::as_str).expect("method");
        let params = body.get("params").expect("params").clone();
        let digest =
            canonical::body_digest(method, scope.as_ref(), &params).expect("canonical body");
        body.as_object_mut()
            .expect("object")
            .insert("digest".to_owned(), Value::String(digest));
    }
    write(&file)
}

fn seed_vectors() -> String {
    let master_seeds: [u64; 5] = [0, 1, 42, 9_223_372_036_854_775_808, u64::MAX];
    let agents = ["fly-a", "fly-b", "fly-c", "fly-d"];
    let mut vectors = Vec::new();
    for master in master_seeds {
        for agent in agents {
            let material = seed::material(master, agent).expect("material");
            vectors.push(json!({
                "masterSeed": master.to_string(),
                "agentId": agent,
                "material": String::from_utf8(material).expect("utf-8"),
                "materialDigest": seed::material_digest(master, agent).expect("digest"),
                "seed": seed::agent_seed(master, agent).expect("seed"),
            }));
        }
    }
    let composition: Vec<Value> = seed::composition_seeds(
        42,
        &agents.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>(),
    )
    .expect("composition")
    .into_iter()
    .map(Value::from)
    .collect();
    write(&json!({
        "description": "seed-derivation-v1 test vectors. Both languages must reproduce every seed.",
        "algorithm": seed::ALGORITHM,
        "prefix": seed::PREFIX,
        "materialTemplate": "<prefix>\\n<masterSeed>\\n<agentId>\\n",
        "rule": "SHA-256 of the material, read as eight big-endian u32 lanes; the first nonzero lane is the seed as a two's-complement i32.",
        "vectors": vectors,
        "composition": {
            "masterSeed": "42",
            "agentIds": agents,
            "seeds": composition,
            "reason": "independent per-agent seeds from one recorded master seed and stable agent ids",
        },
        "invalid": [
            {"masterSeed": "0", "agentId": "Fly-A", "reason": "an agent id is an Id: lowercase"},
            {"masterSeed": "0", "agentId": "", "reason": "an agent id is 1..=64 characters"},
            {"masterSeed": "0", "agentIds": ["fly-a", "fly-a"],
             "reason": "a composition with a repeated agent id is refused rather than silently sharing a seed"},
        ],
    }))
}

fn checkpoint_envelope() -> String {
    let scope = Scope::new("demo", "epoch-1", 42).expect("scope");
    let manifest = json!({
        "envelopeVersion": checkpoint::VERSION,
        "checkpointId": "ckpt-1",
        "sourceScope": scope.to_json(),
        "episodeId": "episode-1",
        "worldTime": {"numerator": "700000000", "denominator": "1"},
        "schedulerId": "lockstep-v1",
        "compositionDigest": canonical::sha256_hex(b"composition"),
        "portMap": [{"portId": "port-1", "agentId": "fly-a"}],
        "compatibility": {
            "backendDigest": canonical::sha256_hex(b"backend"),
            "contentDigest": canonical::sha256_hex(b"content"),
            "patchDigest": canonical::sha256_hex(b"patch"),
            "controllerDigest": canonical::sha256_hex(b"controller"),
            "parserDigest": canonical::sha256_hex(b"parser"),
            "stateFormatId": "flysess-1",
        },
        "agents": [{
            "agentId": "fly-a",
            "profileDigest": canonical::sha256_hex(b"profile"),
            "datasetDigest": canonical::sha256_hex(b"fafb-v783"),
            "modelVersion": "lif-1ms-f64-v2",
            "plasticityVersion": "fly-kc-mbon-rstdp-v2",
            "seed": seed::agent_seed(42, "fly-a").expect("seed"),
            "brainTicks": "2534",
            "remainder": {"numerator": "1000000", "denominator": "3"},
            "payload": "agent-fly-a",
        }],
        "coordinator": {
            "taskLedger": "task-ledger",
            "priorInspection": "prior-inspection",
            "executorState": [{"agentId": "fly-a", "payload": "executor-fly-a"}],
            "admissionState": null,
            "eventWatermarks": {"lastSourceStep": "42", "issued": "7"},
        },
        "environment": {"workerId": "arena", "payload": "world"},
        "helperState": [],
        "payloads": payload_table(),
    });
    let bytes = checkpoint::encode(&manifest, &payloads()).expect("encode");
    let envelope = checkpoint::decode(&bytes).expect("decode");
    let entries: Vec<Value> = envelope
        .layout
        .entries
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "offset": entry.offset.to_string(),
                "byteLength": entry.byte_length.to_string(),
                "digest": checkpoint::hex(&entry.digest),
            })
        })
        .collect();
    let first_payload = envelope.layout.entries[0].offset;
    write(&json!({
        "description": "One FLYSESS1 envelope, its layout and the corruptions a reader must refuse.",
        "magic": "FLYSESS1",
        "footerMagic": "FLYSESSF",
        "version": checkpoint::VERSION,
        "manifest": manifest,
        "payloads": payloads()
            .iter()
            .map(|(name, bytes)| json!({"name": name, "base64": fixtures::encode_base64(bytes)}))
            .collect::<Vec<_>>(),
        "envelope": {
            "base64": fixtures::encode_base64(&bytes),
            "byteLength": bytes.len(),
            "layout": {
                "headerBytes": checkpoint::HEADER_BYTES,
                "manifestOffset": envelope.layout.manifest_offset.to_string(),
                "manifestBytes": envelope.layout.manifest_bytes,
                "tableOffset": envelope.layout.table_offset.to_string(),
                "tableEntryBytes": checkpoint::TABLE_ENTRY_BYTES,
                "entries": entries,
                "footerOffset": envelope.layout.footer_offset.to_string(),
                "footerBytes": checkpoint::FOOTER_BYTES,
                "totalBytes": envelope.layout.total_bytes.to_string(),
            },
        },
        "corruption": [
            {"name": "a flipped magic byte", "offset": 0, "reason": "wrong magic"},
            {"name": "an unsupported version", "offset": 8, "reason": "unsupported version"},
            {"name": "a flipped manifest byte", "offset": checkpoint::HEADER_BYTES,
             "reason": "the footer digest covers the manifest"},
            {"name": "a flipped payload byte", "offset": first_payload,
             "reason": "every payload carries its own digest"},
            {"name": "a flipped footer digest byte", "offset": bytes.len() - 40,
             "reason": "the footer digest must match the contents"},
            {"name": "a flipped footer magic byte", "offset": bytes.len() - 8,
             "reason": "a truncated file cannot look complete"},
        ],
    }))
}

fn payloads() -> Vec<(String, Vec<u8>)> {
    vec![
        ("agent-fly-a".to_owned(), b"agent state bytes".to_vec()),
        ("executor-fly-a".to_owned(), b"executor state".to_vec()),
        ("task-ledger".to_owned(), b"{\"rank\":10}".to_vec()),
        ("prior-inspection".to_owned(), b"{\"map\":40}".to_vec()),
        ("world".to_owned(), vec![0u8; 64]),
    ]
}

fn payload_table() -> Value {
    let mut out = Vec::new();
    for (name, bytes) in payloads() {
        let mut entry = Map::new();
        entry.insert("name".to_owned(), Value::String(name));
        entry.insert(
            "byteLength".to_owned(),
            Value::String(bytes.len().to_string()),
        );
        entry.insert(
            "digest".to_owned(),
            Value::String(canonical::sha256_hex(&bytes)),
        );
        out.push(Value::Object(entry));
    }
    // A BTreeMap would sort the payload names; the table order is the write order, which is
    // what the envelope records.
    let _: BTreeMap<(), ()> = BTreeMap::new();
    Value::Array(out)
}
