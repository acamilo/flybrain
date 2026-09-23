//! `legacy-gameboy-v1` and the 2026-09-23 extension methods: every digest in
//! `fixtures/gameboy-legacy.json` is what the code computes, and every rule that needs a second
//! value in hand refuses what it must.

use fly_session_types::extensions::*;
use fly_session_types::gameboy::{
    self, ChannelsDecision, LegacyComposition, LegacyProfile, Location, MemoryInspection,
    ReadoutContext, RollbackRequest,
};
use fly_session_types::scalar::{DomainType, RationalNs, SchemaRef, Scope, TypedValue};
use fly_session_types::workers::{EpisodeRequest, EpisodeRequestKind};
use fly_session_types::{canonical, fixtures};
use serde_json::{Value, json};

fn legacy() -> Value {
    fixtures::load("gameboy-legacy.json").expect("gameboy-legacy.json")
}

#[test]
fn every_registered_schema_reference_is_the_digest_of_its_declaration() {
    let file = legacy();
    for schema in gameboy::PAYLOAD_SCHEMAS {
        let recorded = SchemaRef::from_json(&file["schemaRefs"][schema.id]).expect("recorded ref");
        assert_eq!(recorded, schema.schema_ref(), "{}", schema.id);
        assert_eq!(
            recorded.digest,
            canonical::digest_of(&schema.declaration()).expect("digest"),
            "{}: the digest is over the declaration",
            schema.id
        );
    }
    assert_eq!(
        file["extensionSetDigest"].as_str(),
        Some(gameboy::extension_set_digest().as_str())
    );
    assert_eq!(
        canonical::digest_of(&file["extensionSet"]).expect("digest"),
        gameboy::extension_set_digest(),
        "the checked-in set hashes to the recorded digest"
    );
}

#[test]
fn a_changed_constraint_is_a_new_schema_reference() {
    let mut changed = gameboy::READOUT_CONTEXT;
    let fields: &'static [_] = Box::leak(
        gameboy::READOUT_CONTEXT
            .fields
            .iter()
            .map(|f| {
                let mut f = *f;
                if f.name == "bound" {
                    f.constraint = "<= 32";
                }
                f
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    changed.fields = fields;
    assert_ne!(
        changed.schema_ref().digest,
        gameboy::READOUT_CONTEXT.schema_ref().digest
    );
}

#[test]
fn the_profile_document_is_the_one_legacy_profile_and_its_asset_ref_is_its_digest() {
    let file = legacy();
    let document = &file["profile"]["document"];
    let parsed = LegacyProfile::from_json(document).expect("the legacy profile");
    assert_eq!(parsed, gameboy::legacy_profile());
    let canonical_text = canonical::canonicalize(document).expect("canonical");
    assert_eq!(
        file["profile"]["canonical"].as_str(),
        Some(canonical_text.as_str())
    );
    let asset = gameboy::profile_asset_ref();
    assert_eq!(
        asset.digest,
        canonical::sha256_hex(canonical_text.as_bytes())
    );
    assert_eq!(asset.byte_length, canonical_text.len() as u64);
    assert_eq!(asset.id, gameboy::PROFILE_ID);
    assert_eq!(file["profile"]["assetRef"], asset.to_json());
    // The pinned identities are the defaults CLAUDE.md fixes, and the fingerprint is today's
    // schema-1 value (flysim's legacy_profile_identity test recomputes it from the dataset).
    assert_eq!(document["kernelVersion"], "lif-1ms-f64-v2");
    assert_eq!(document["plasticityVersion"], "fly-kc-mbon-rstdp-v2");
    assert_eq!(gameboy::FAFB_V783_FINGERPRINT.split(':').count(), 7);
    assert!(
        gameboy::FAFB_V783_FINGERPRINT
            .split(':')
            .all(fly_session_types::scalar::is_digest)
    );
}

/// The legacy loop's f64 accumulator and step-v1's rational one are the same numbers.
#[test]
fn the_frame_clock_is_exact_and_matches_the_legacy_f64_accumulator() {
    let legacy_ms_per_frame: f64 = 1000.0 / (4_194_304.0 / 70_224.0);
    assert_eq!(
        legacy_ms_per_frame,
        548_625.0 / 32_768.0,
        "the constant is dyadic, so exact"
    );
    let step = gameboy::step_duration();
    assert_eq!(
        RationalNs::reduced(70_224 * 1_000_000_000, 4_194_304).expect("frame"),
        step
    );
    let tick = gameboy::tick_duration();
    let file = legacy();
    let frames = file["clock"]["frames"].as_array().expect("frames");
    let mut exact = RationalNs::ZERO;
    let mut float = 0.0f64;
    let mut total = 0u64;
    for frame in 0..100_000u64 {
        exact = exact.checked_add(&step).expect("no overflow");
        let (ticks, remainder) = exact.divide_floor(&tick).expect("tick");
        exact = remainder;
        float += legacy_ms_per_frame;
        let steps = float.floor();
        float -= steps;
        assert_eq!(ticks, steps as u64, "frame {frame}: tick counts agree");
        // The f64 remainder in ms, as an exact nanosecond rational.
        let float_ns =
            RationalNs::reduced((float * 32_768.0) as u128 * 1_000_000, 32_768).expect("dyadic");
        assert_eq!(
            float * 32_768.0,
            (float * 32_768.0).floor(),
            "frame {frame}: dyadic"
        );
        assert_eq!(remainder, float_ns, "frame {frame}: remainders agree");
        total += ticks;
        if let Some(recorded) = frames.get(frame as usize) {
            assert_eq!(recorded["ticks"], Value::String(ticks.to_string()));
            assert_eq!(recorded["remainder"], remainder.to_json());
        }
    }
    assert!(
        total > 1_674_000,
        "100000 frames is about 1674 brain seconds"
    );
}

#[test]
fn the_example_composition_digest_is_recorded_and_moves_with_the_decoder() {
    let file = legacy();
    let example = LegacyComposition::from_json(&file["composition"]["example"]).expect("example");
    assert_eq!(
        file["composition"]["digest"].as_str(),
        Some(example.digest().expect("digest").as_str())
    );
    // The decoder configuration and the macro channels are in the composition digest, not in
    // the legacy compatibility string: changing either moves the digest and leaves the string.
    let mut decoder = example.clone();
    decoder.decoder_config_digest = canonical::sha256_hex(b"another decoder");
    assert_ne!(decoder.digest().expect("d"), example.digest().expect("d"));
    assert_eq!(decoder.flysim_compatibility, example.flysim_compatibility);
    let mut channels = example.clone();
    channels.executor.macro_channels.pop();
    channels.validate().expect("still a valid composition");
    assert_ne!(channels.digest().expect("d"), example.digest().expect("d"));
    assert_eq!(channels.flysim_compatibility, example.flysim_compatibility);
}

#[test]
fn typed_values_are_read_only_under_their_registered_schema() {
    let context = ReadoutContext {
        boot: false,
        bound: vec!["macro_talk".to_owned()],
        location: Some(Location {
            area: 2,
            x: 17,
            y: 9,
        }),
    };
    let typed = context.to_typed();
    assert_eq!(typed.schema, gameboy::READOUT_CONTEXT.schema_ref());
    assert_eq!(
        ReadoutContext::from_typed(&typed).expect("round trip"),
        context
    );
    let wrong = TypedValue::new(gameboy::CHANNELS.schema_ref(), typed.value.clone()).expect("tv");
    assert!(
        ReadoutContext::from_typed(&wrong).is_err(),
        "another schema is refused"
    );
    let synthetic = TypedValue::new(
        SchemaRef::new(
            "gameboy-readout-context-v1",
            1,
            &canonical::sha256_hex(b"x"),
        )
        .expect("r"),
        typed.value,
    )
    .expect("tv");
    assert!(
        ReadoutContext::from_typed(&synthetic).is_err(),
        "the same id with another digest is another schema"
    );
}

#[test]
fn bound_is_an_ordered_subset_and_the_macro_winner_is_always_bound() {
    let channels: Vec<String> = ["macro_go_objective", "macro_talk", "macro_next"]
        .iter()
        .map(|c| (*c).to_owned())
        .collect();
    let context = ReadoutContext {
        boot: false,
        bound: vec!["macro_go_objective".to_owned(), "macro_next".to_owned()],
        location: None,
    };
    context.validate_against(&channels).expect("ordered subset");
    let reversed = ReadoutContext {
        bound: vec!["macro_next".to_owned(), "macro_go_objective".to_owned()],
        ..context.clone()
    };
    assert!(
        reversed.validate_against(&channels).is_err(),
        "order is the decoder's"
    );
    let foreign = ReadoutContext {
        bound: vec!["macro_heal".to_owned()],
        ..context.clone()
    };
    assert!(foreign.validate_against(&channels).is_err());

    let mut decision = ChannelsDecision {
        buttons: [true, false, false, false, true, false, false, false],
        macro_channel: Some("macro_next".to_owned()),
    };
    assert_eq!(decision.mask(), 0x11, "up and a, GAMEBOY_BUTTON_BITS");
    decision.validate_against(&context).expect("bound");
    decision.macro_channel = Some("macro_talk".to_owned());
    assert!(
        decision.validate_against(&context).is_err(),
        "an unbound channel cannot be the macro group's winner"
    );
}

#[test]
fn the_inspection_rom_is_the_environment_content() {
    let file = legacy();
    let example = LegacyComposition::from_json(&file["composition"]["example"]).expect("example");
    let rom = example.executor.rom.clone();
    let inspection = MemoryInspection::from_json(&json!({
        "memory": {"storeId": "store-1", "artifactId": "wram-1", "generation": "1",
                   "byteLength": "65536", "contentType": "application/octet-stream", "digest": null},
        "romDigest": rom.digest,
    }))
    .expect("inspection");
    inspection
        .validate_against(&rom.digest, &rom)
        .expect("agree");
    assert!(
        inspection
            .validate_against(&canonical::sha256_hex(b"other cartridge"), &rom)
            .is_err()
    );
}

#[test]
fn a_rollback_request_carries_the_registered_outcome() {
    let request = EpisodeRequest {
        kind: EpisodeRequestKind::Rollback,
        reason: "stall".to_owned(),
        outcome: RollbackRequest {
            slot_id: "best".to_owned(),
            trigger: "stall".to_owned(),
        }
        .to_typed(),
    };
    let read = EpisodeRequest::from_json(&request.to_json()).expect("round trip");
    assert_eq!(read.kind, EpisodeRequestKind::Rollback);
    let outcome = RollbackRequest::from_typed(&read.outcome).expect("registered outcome");
    assert_eq!(outcome.slot_id, "best");
}

#[test]
fn the_extension_methods_check_their_scope() {
    let new_epoch = Scope::new("live", "epoch-8", 4101).expect("scope");
    let same_epoch = Scope::new("live", "epoch-7", 4101).expect("scope");
    let restore = RestoreSlotParams {
        slot_id: "best".to_owned(),
        prior_epoch: "epoch-7".to_owned(),
        policy: ROLLBACK_POLICY.to_owned(),
    };
    restore
        .validate_against_scope(&new_epoch)
        .expect("a new epoch");
    assert!(
        restore.validate_against_scope(&same_epoch).is_err(),
        "a rollback always moves to a new epoch"
    );

    let saved = SaveSlotResult {
        slot_id: "best".to_owned(),
        boundary: 4101,
        state_digest: canonical::sha256_hex(b"state"),
        byte_length: 199_616,
    };
    saved
        .validate_against_scope(&same_epoch)
        .expect("the scoped boundary");
    assert!(
        saved
            .validate_against_scope(&Scope::new("live", "epoch-7", 4100).expect("s"))
            .is_err()
    );

    let file = fixtures::load("valid.json").expect("valid.json");
    let case = |name: &str| -> Value {
        fixtures::cases(&file)
            .expect("cases")
            .iter()
            .find(|c| c["name"] == Value::String(name.to_owned()))
            .unwrap_or_else(|| panic!("case {name}"))["value"]
            .clone()
    };
    let rollback = AgentRollbackParams::from_json(&case("agent rollback params")).expect("params");
    rollback
        .validate_against_scope(&new_epoch)
        .expect("input is the scoped boundary");
    assert!(
        rollback
            .validate_against_scope(&Scope::new("live", "epoch-8", 4102).expect("s"))
            .is_err(),
        "the installed input is the restored boundary, not the next one"
    );
    let result = AgentRollbackResult::from_json(&case("agent rollback result")).expect("result");
    result.validate_against_scope(&new_epoch).expect("no tick");
    assert!(
        result
            .validate_against_scope(&Scope::new("live", "epoch-8", 4100).expect("s"))
            .is_err()
    );
    let restored = RestoreSlotResult::from_json(&case("restore slot result")).expect("result");
    restored
        .validate_against_scope(&new_epoch)
        .expect("same boundary");
    let inspection = MemoryInspection::from_typed(&restored.observation.inspection)
        .expect("the observation's inspection is the registered memory image");
    assert_eq!(inspection.memory.byte_length, gameboy::MEMORY_IMAGE_BYTES);
    assert_eq!(SLOTS_CAPABILITY, "gameboy-slots-v1");
    assert_eq!(ROLLBACK_CAPABILITY, ROLLBACK_POLICY);
}
