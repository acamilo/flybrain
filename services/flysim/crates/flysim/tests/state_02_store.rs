//! STATE-02: the session runtime's `FLYSIM01` store and the operator's tools, both ways.
//!
//! `fly-reset-to-milestone` and `fly-loop-reset` must keep working on a store the session runtime
//! wrote (`legacy-gameboy-v1` section 16), and the session runtime must restore a store the
//! legacy loop wrote and the tools reset. These run the real scripts from `infra/bin` and the
//! real `flysim --reset-to-milestone` against stores in a temporary directory, with `systemctl`
//! stubbed out. No ROM and no dataset: the checkpoints are synthetic but complete envelopes.

use std::path::{Path, PathBuf};
use std::process::Command;

use fly_session::fly_session_types::gameboy;
use fly_session::legacy_checkpoint::{
    self as lc, AgentImport, CheckpointerConfig, LegacyCapture, LegacyCheckpointer, RankChange,
    RestoreGate, SaveKind, Selection, TaskHalf,
};
use fly_session::legacy_env::{SlotState, WorldState, sample_at, world_time_at};
use fly_session::types::id;
use flybrain_core::agent::AgentState;
use flybrain_gb::RatchetState;
use flybrain_gb::emulator::FRAMEBUFFER_LEN;
use flysim::journal::{self, BootHeader, Entry, Input, SugarJournal};
use flysim::store::{self, RuntimeState, Store};

const COMPAT: &str = "lif-1ms-f64-v2/pokered-unique8-v7/fingerprint/fly-kc-mbon-rstdp-v2";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
}

fn agent(ms: f64) -> AgentState {
    use flybrain_core::decoder::DecoderState;
    use flybrain_core::lif::LifState;
    use flybrain_core::ordered::NumberMap;
    use flybrain_core::plasticity::PlasticityState;
    AgentState {
        version: 1,
        remainder: 0.5,
        warmed_up: true,
        network: LifState {
            membrane: vec![0.5, -0.25],
            refractory: vec![0, 3],
            last_spike_ms: vec![-1_000_000.0, 12.0],
            visual_drive: vec![0.1],
            rng: -12_345,
            reward_remaining: 0.0,
            ms,
            population_rate: 1.5,
            rates: NumberMap::from_pairs([("forward", 2.0)]),
            plasticity: PlasticityState {
                version: "fly-kc-mbon-rstdp-v2".to_string(),
                topology: 42,
                enabled: true,
                updates: 17.0,
                signal: 0.25,
                gains: vec![1.0, 0.9],
                traces: vec![0.0, 0.1],
                touched: vec![0.0, 8_000.0],
            },
        },
        decoder: DecoderState {
            version: 4,
            calibrated: true,
            baseline: NumberMap::from_pairs([("forward", 1.0)]),
            held_until: NumberMap::new(),
            next_allowed: NumberMap::new(),
            next_decision: 100.0,
            current: None,
            fatigue: NumberMap::new(),
            macro_next_decision: 0.0,
            macro_current: None,
            macro_fatigue: NumberMap::new(),
        },
    }
}

fn world(boundary: u64) -> WorldState {
    let world_time = world_time_at(boundary).unwrap();
    WorldState {
        rom_digest: "ab".repeat(32),
        episode_id: id("ep1"),
        boundary,
        audio_next_sample: sample_at(&world_time, 48_000),
        world_time,
        engine_frame: boundary + 1,
        buttons: 0,
        emulator: vec![(boundary % 251) as u8; 256],
        framebuffer: vec![9; FRAMEBUFFER_LEN],
        slots: vec![(
            id("best"),
            SlotState { game: vec![1; 32], frame: vec![4; FRAMEBUFFER_LEN], boundary },
        )],
    }
}

/// A complete capture of boundary `k`, as the participants would hand it over.
fn capture(k: u64, ratchet: RatchetState) -> LegacyCapture {
    let scope = lc::legacy_source_scope(&id("s1"), k);
    let import = AgentImport {
        agent_id: id("fly"),
        profile: gameboy::profile_asset_ref(),
        seed: 22_222,
        macro_channels: Vec::new(),
    };
    let context = gameboy::ReadoutContext { boot: false, bound: vec![], location: None }.to_typed();
    LegacyCapture {
        boundary: k,
        agent_payload: lc::agent_payload(&agent(9_000.0 + k as f64), &world(k).framebuffer, &import, &id("c"), &scope, &context)
            .unwrap(),
        world_payload: lc::world_payload(&world(k), &id("world"), &id("c"), &scope, &"cd".repeat(32)),
        task: TaskHalf { reward: serde_json::Value::Null, ratchet },
    }
}

fn config(root: &Path) -> CheckpointerConfig {
    CheckpointerConfig {
        hot_dir: root.join("hot"),
        durable_dir: root.join("state"),
        keep_generations: 8,
        hot_seconds: 5.0,
        checkpoint_seconds: 300.0,
        compatibility: COMPAT.to_owned(),
        speed: 1.0,
        slot_id: id("best"),
    }
}

fn save(checkpointer: &mut LegacyCheckpointer, kind: SaveKind, k: u64, ratchet: RatchetState) {
    let ticket = checkpointer.save(kind, capture(k, ratchet), k, 1_790_000_000_000).unwrap();
    ticket.reply.blocking_recv().unwrap().unwrap();
}

fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A PATH whose `systemctl` says every unit is inactive, so the scripts believe flysim is stopped.
fn tools(root: &Path) -> (String, PathBuf) {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "systemctl", "exit 3");
    let flysim = stub(root, "flysim-build", &format!("[ \"$1\" = --print-compatibility ] && echo \"{COMPAT}\""));
    (format!("{}:/usr/bin:/bin", bin.display()), flysim)
}

fn loop_reset_list(root: &Path, state: &Path) -> Vec<u32> {
    let (path, flysim) = tools(root);
    let env_file = root.join("fly.env");
    std::fs::write(&env_file, "").unwrap();
    let out = Command::new(repo().join("infra/bin/fly-loop-reset"))
        .arg("--list")
        .env_clear()
        .env("PATH", path)
        .env("FLY_LOOP_RESET_TEST_STATE_DIR", state)
        .env("FLY_LOOP_RESET_TEST_ENV_FILE", &env_file)
        .env("FLY_LOOP_RESET_TEST_FLYSIM", &flysim)
        .env("FLY_LOOP_RESET_TEST_LIVE_FLYSIM", &flysim)
        .output()
        .expect("fly-loop-reset runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).split_whitespace().map(|r| r.parse().unwrap()).collect()
}

fn reset_to_milestone(root: &Path, durable: &Path, hot: &Path, rank: u32) {
    let (path, _) = tools(root);
    let out = Command::new(repo().join("infra/bin/fly-reset-to-milestone"))
        .arg(rank.to_string())
        .env_clear()
        .env("PATH", path)
        .env("FLY_STATE_DIR", durable)
        .env("FLY_STATE_HOT_DIR", hot)
        .env("FLY_BIN", env!("CARGO_BIN_EXE_flysim"))
        .env("FLY_USER", "no-such-user-state02")
        .output()
        .expect("fly-reset-to-milestone runs");
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn gate() -> RestoreGate {
    RestoreGate {
        rom_sha256: "ab".repeat(32),
        compatibility: COMPAT.to_owned(),
        migrates_from: Vec::new(),
        accepted: Vec::new(),
    }
}

fn session_restore(hot: &Path, durable: &Path) -> Box<lc::Restored<lc::Halves>> {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let selection = runtime.block_on(lc::restore_from_store(
        &Store::new(hot, 8),
        &Store::new(durable, 8),
        &gate(),
        |checkpoint| async move { lc::halves(&checkpoint, &id("ep1"), &id("best"), 48_000) },
    ));
    match selection {
        Selection::Restored(restored) => restored,
        other => panic!("the session runtime should restore: {other:?}"),
    }
}

fn spent(best: u64) -> RatchetState {
    RatchetState { best, attempts: 2, recoveries: 5, ..RatchetState::default() }
}

#[test]
fn a_store_the_session_runtime_writes_is_reset_by_the_legacy_tools() {
    let root = tempfile::tempdir().unwrap();
    let mut checkpointer = LegacyCheckpointer::open(config(root.path())).unwrap();
    let (hot, durable) = (root.path().join("hot"), root.path().join("state"));

    // The session runtime's own journal, with its boot header.
    let mut sugar = SugarJournal::new(&hot);
    sugar.boot(&BootHeader {
        runtime: "fly-session".to_owned(),
        start_frame: 11,
        brain_ms: 9_010.0,
        origin: "fresh start".to_owned(),
        generation: None,
        compatibility: COMPAT.to_owned(),
        wall_ms: 1,
    });
    sugar.record(&Entry {
        frame: 40,
        brain_ms: 9_040.0,
        input: Input::Sugar { duration_ms: 300.0 },
        by: "viewer",
        source: "test",
        event_id: 1,
        wall_ms: 1,
    });

    // A run: a startup save, two climbs, hot copies and durable intervals in between.
    save(&mut checkpointer, SaveKind::Durable, 10, spent(2));
    assert_eq!(checkpointer.observe_rank(3, 9_020.0), Some(RankChange::Climbed(3)));
    save(&mut checkpointer, SaveKind::Milestone(3), 20, spent(3));
    save(&mut checkpointer, SaveKind::Hot, 25, spent(3));
    assert_eq!(checkpointer.observe_rank(5, 9_050.0), Some(RankChange::Climbed(5)));
    save(&mut checkpointer, SaveKind::Milestone(5), 50, spent(5));
    save(&mut checkpointer, SaveKind::Durable, 60, spent(5));
    save(&mut checkpointer, SaveKind::Hot, 65, spent(5));
    checkpointer.close();

    // fly-loop-reset reads the compatibility out of the session runtime's archives.
    assert_eq!(loop_reset_list(root.path(), &durable), vec![3, 5]);
    // `flysim --print-state-compatibility` does too.
    assert_eq!(store::state_compatibility(&durable).as_deref(), Some(COMPAT));

    // fly-reset-to-milestone 3, the real script and the real binary.
    reset_to_milestone(root.path(), &durable, &hot, 3);
    assert!(!durable.join("milestone-5.checkpoint").exists());
    assert!(!hot.join(journal::FILE_NAME).exists(), "the reset clears the sugar journal");

    // The legacy order and the session runtime agree on what comes back: rung 3, budget back.
    let legacy_first = store::restore_order(&Store::new(&hot, 8), &Store::new(&durable, 8));
    let legacy = store::load(&legacy_first[0].path).unwrap();
    let restored = session_restore(&hot, &durable);
    assert_eq!(restored.candidate, legacy_first[0]);
    assert_eq!(restored.installed.task.ratchet.best, 3);
    assert_eq!(
        (restored.installed.task.ratchet.attempts, restored.installed.task.ratchet.recoveries),
        (0, 0)
    );
    assert_eq!(restored.installed.world.boundary, 20);
    assert_eq!(restored.installed.agent, legacy.agent);

    // The session runtime carries on over the reset store: generations above everything, and
    // its own first climb is archived again (a new process's best).
    let mut checkpointer = LegacyCheckpointer::open(config(root.path())).unwrap();
    assert!(checkpointer.next_generation() > legacy.runtime.generation);
    checkpointer.restored(3, restored.installed.host.rank_since_ms);
    assert_eq!(checkpointer.observe_rank(4, 9_100.0), Some(RankChange::Climbed(4)));
    save(&mut checkpointer, SaveKind::Milestone(4), 90, spent(4));
    checkpointer.close();
    assert_eq!(loop_reset_list(root.path(), &durable), vec![3, 4]);
}

#[test]
fn a_store_the_legacy_loop_wrote_and_the_tools_reset_is_restored_by_the_session_runtime() {
    let root = tempfile::tempdir().unwrap();
    let (hot, durable) = (root.path().join("hot"), root.path().join("state"));
    let legacy_hot = Store::new(&hot, 8);
    let legacy_durable = Store::new(&durable, 8);
    // Exactly what `Sim`'s writer thread commits.
    let bytes = |generation: u64, k: u64, ratchet: RatchetState| {
        store::encode(
            &agent(9_000.0 + k as f64),
            &RuntimeState {
                generation,
                wall_ms: 1,
                rom_sha256: "ab".repeat(32),
                emulator_frame: k + 1,
                compatibility: COMPAT.to_owned(),
                speed: 1.0,
                buttons: 0,
                rank_since_ms: 0.0,
                last_event_id: k,
                reward: serde_json::Value::Null,
                ratchet,
                emulator: vec![(k % 251) as u8; 256],
                framebuffer: vec![9; FRAMEBUFFER_LEN],
                ratchet_game: vec![1; 32],
                ratchet_frame: vec![4; FRAMEBUFFER_LEN],
            },
        )
        .unwrap()
    };
    legacy_durable.commit(1, &bytes(1, 10, spent(2)), None).unwrap();
    legacy_durable.commit(2, &bytes(2, 20, spent(2)), Some(2)).unwrap();
    legacy_durable.commit(3, &bytes(3, 30, spent(4)), Some(4)).unwrap();
    legacy_hot.commit(4, &bytes(4, 35, spent(4)), None).unwrap();

    assert_eq!(loop_reset_list(root.path(), &durable), vec![2, 4]);
    reset_to_milestone(root.path(), &durable, &hot, 2);

    let restored = session_restore(&hot, &durable);
    assert_eq!(restored.installed.world.boundary, 20);
    assert_eq!(restored.installed.task.ratchet.best, 2);
    assert_eq!(restored.installed.task.ratchet.attempts, 0);
    // And what the session runtime would save from it is the legacy bytes again.
    let halves = &restored.installed;
    let again = lc::export_states(&halves.agent, &halves.world, &id("best"), &halves.task, &halves.host)
        .unwrap();
    assert_eq!(again, std::fs::read(&restored.candidate.path).unwrap());
}
