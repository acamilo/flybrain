//! ENV-01: the Game Boy compatibility environment worker against the emulator driven directly the
//! way flysim drives it, in every execution mode, plus its refusals.
//!
//! Without a cartridge the toy cartridge runs (our own program, assembled by the harness), and
//! its records are the committed goldens. With `FLY_ROM` (rom-env) the real cartridge runs from a
//! `FLYSIM01` checkpoint; with `FLY_ENV01_TRACE_DIR` the service's own `FLY_TRACE`s are replayed.
//! Each ROM job prints `skipped: ...` and passes when its inputs are not installed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fly_session::launcher::ExecutionMode;
use fly_session::legacy_env::{self, BackendConfig, WorldState};
use fly_session::legacy_env_parity::{
    self as parity, EnvRig, EnvScript, EnvStart, FlyTrace, button_walk, compare, golden_json,
    run_direct, run_on_worker, scripted, toy_cart, toy_script,
};
use fly_session::types::*;
use fly_session_types::extensions::{
    METHOD_RESTORE_SLOT, METHOD_SAVE_SLOT, RestoreSlotParams, SLOTS_CAPABILITY, SaveSlotParams,
};
use serde_json::{Value, json};

macro_rules! all_modes {
    ($($name:ident),* $(,)?) => {
        mod in_process {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name(fly_session::launcher::ExecutionMode::InProcess).await
                }
            )*
        }
        mod thread {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name(fly_session::launcher::ExecutionMode::Thread).await
                }
            )*
        }
        mod process {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name(fly_session::launcher::ExecutionMode::Process).await
                }
            )*
        }
    };
}

struct Toy {
    _dir: tempfile::TempDir,
    path: PathBuf,
    rom: Arc<Vec<u8>>,
    backend: BackendConfig,
}

/// The toy cartridge in a temporary file (outside the checkout), with its backend configuration.
fn toy(variant: u8) -> Toy {
    let dir = tempfile::tempdir().expect("tempdir");
    let rom = toy_cart::rom(variant);
    let path = dir.path().join("toy.cart");
    std::fs::write(&path, &rom).expect("write the toy cartridge");
    let backend = BackendConfig::legacy(&digest_of_bytes(&rom));
    Toy {
        _dir: dir,
        path,
        rom: Arc::new(rom),
        backend,
    }
}

async fn rig(
    mode: ExecutionMode,
    path: &Path,
    backend: &BackendConfig,
) -> (tempfile::TempDir, EnvRig) {
    let dir = tempfile::tempdir().expect("tempdir");
    let rig = EnvRig::start(dir.path(), mode, path, backend.clone())
        .await
        .expect("the rig starts");
    (dir, rig)
}

// -------------------------------------------------------------------------------------------
// The toy cartridge: the harness's own inputs reach frames, memory and audio

#[test]
fn the_toy_cartridge_turns_every_input_into_frames_memory_and_audio() {
    let toy = toy(0);
    let script = |mask: u8| EnvScript {
        name: "one".into(),
        start: EnvStart::Fresh,
        ops: (0..30).map(|_| parity::EnvOp::Advance(mask)).collect(),
    };
    let quiet = run_direct(toy.rom.clone(), &toy.backend, &script(0)).unwrap();
    let pressed = run_direct(
        toy.rom.clone(),
        &toy.backend,
        &script(flybrain_gb::emulator::buttons::A),
    )
    .unwrap();
    let last = |r: &[parity::EnvRecord]| r.last().unwrap().clone();
    assert_ne!(
        last(&quiet).frame,
        last(&pressed).frame,
        "the pad reaches the screen"
    );
    assert_ne!(
        last(&quiet).memory,
        last(&pressed).memory,
        "the pad reaches memory"
    );
    assert_ne!(
        last(&quiet).audio.unwrap().digest,
        last(&pressed).audio.unwrap().digest,
        "and the APU"
    );
    // Frames move with no input at all (the scroll follows the frame count).
    assert_ne!(quiet[5].frame, quiet[6].frame);
    // O[0] is after the one-frame scaffold, and carries no audio.
    assert_eq!(quiet[0].engine_frame, 1);
    assert_eq!(quiet[0].boundary, 0);
    assert!(quiet[0].audio.is_none());
    // ~804 stereo frames per Game Boy frame at 48 kHz, contiguous.
    let a = quiet[1].audio.clone().unwrap();
    let b = quiet[2].audio.clone().unwrap();
    assert!((790..=820).contains(&a.frames), "{}", a.frames);
    assert_eq!(b.first_sample, a.first_sample + a.frames);
    // A variant is another cartridge.
    assert_ne!(toy_cart::rom(0), toy_cart::rom(1));
}

// -------------------------------------------------------------------------------------------
// Goldens and parity in every mode

async fn the_toy_golden_holds(mode: ExecutionMode) {
    let toy = toy(0);
    let script = toy_script();
    let direct =
        run_direct(toy.rom.clone(), &toy.backend, &script).expect("the direct reference runs");
    let (_dir, mut rig) = rig(mode, &toy.path, &toy.backend).await;
    let worker = run_on_worker(&mut rig, &script)
        .await
        .expect("the worker runs the script");
    rig.stop().await;
    compare(&direct, &worker).expect("the worker is the emulator driven directly");

    let golden = golden_json(&script, &worker);
    let path = parity::toy_golden_path();
    if std::env::var_os("FLY_UPDATE_FIXTURES").is_some() && mode == ExecutionMode::InProcess {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = serde_json::to_string_pretty(&golden).unwrap() + "\n";
        std::fs::write(&path, text).unwrap();
    }
    let committed: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the golden is committed"))
            .unwrap();
    let (want, got) = (
        committed["records"].as_array().unwrap(),
        golden["records"].as_array().unwrap(),
    );
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        assert_eq!(
            w,
            g,
            "{}: record {i} differs from the golden (FLY_UPDATE_FIXTURES=1 rewrites it)",
            mode.label()
        );
    }
    assert_eq!(
        committed,
        golden,
        "{} differs from the golden",
        mode.label()
    );

    // The script exercised every path, and the records show them.
    let ops: Vec<&str> = worker.iter().map(|r| r.op).collect();
    for op in [
        "initialize",
        "advance",
        "save-slot",
        "rollback",
        "capture",
        "restore",
    ] {
        assert!(ops.contains(&op), "no {op}");
    }
    // A rollback lands on the slot's frame and memory, at the same boundary, with the next chunk a
    // discontinuity; a restore lands on the captured world.
    for (i, r) in worker.iter().enumerate() {
        match r.op {
            "rollback" => {
                assert_eq!(r.boundary, worker[i - 1].boundary);
                assert!(r.audio.is_none());
                let next = worker[i + 1..].iter().find(|n| n.op == "advance").unwrap();
                assert!(next.audio.as_ref().unwrap().discontinuity);
            }
            "restore" => {
                let capture = &worker[i - 1];
                assert_eq!(capture.op, "capture");
                assert_eq!(
                    (r.frame.as_str(), r.memory.as_str()),
                    (capture.frame.as_str(), capture.memory.as_str())
                );
            }
            "advance" => assert_eq!(r.engine_frame, r.boundary + 1),
            _ => {}
        }
    }
}

// -------------------------------------------------------------------------------------------
// Refusals

fn code(result: Result<fly_session::rpc::DomainReply, DomainError>) -> ErrorCode {
    match result {
        Ok(reply) => reply.result().expect_err("the worker refused").code,
        Err(e) => e.code,
    }
}

async fn refusals(mode: ExecutionMode) {
    let other = toy(1);
    let toy = toy(0);
    let (_dir, rig) = rig(mode, &toy.path, &toy.backend).await;

    let mut driver = rig.driver();
    // Another backend configuration -- another scaffold, another slot set -- is another profile.
    for tweak in [
        |b: &mut BackendConfig| b.setup_frames = 0,
        |b: &mut BackendConfig| b.slots = vec![id("best"), id("spare")],
        |b: &mut BackendConfig| b.audio_sample_rate = 44_100,
    ] {
        let mut params = driver.initialize_params();
        let mut backend = toy.backend.clone();
        tweak(&mut backend);
        params.backend_config = backend.asset_ref();
        let refused = driver
            .call_raw(
                "Environment.Initialize",
                Some(driver.scope()),
                params.to_json(),
                &[],
            )
            .await;
        assert_eq!(code(refused), ErrorCode::IncompatibleState);
    }
    // Two ports, or a port the composition does not declare.
    let mut params = driver.initialize_params();
    params.port_bindings = vec![(id("p2"), id("fly"))];
    assert_eq!(
        code(
            driver
                .call_raw(
                    "Environment.Initialize",
                    Some(driver.scope()),
                    params.to_json(),
                    &[]
                )
                .await
        ),
        ErrorCode::IdentityMismatch
    );
    // Nothing moved: the right configuration still initializes.
    let first = driver
        .initialize()
        .await
        .expect("the declared configuration initializes");
    assert_eq!(first.engine_frame, 1);

    // An incomplete joypad batch, a batch for another port, an axis.
    let scope = driver.scope();
    let mut bad = driver.advance_params(0);
    bad.controls[0].buttons.pop();
    assert_eq!(
        code(
            driver
                .call_raw(
                    "Environment.Advance",
                    Some(scope.clone()),
                    bad.to_json(),
                    &[]
                )
                .await
        ),
        ErrorCode::InvalidArgument
    );
    let mut bad = driver.advance_params(0);
    bad.controls[0].port_id = id("p2");
    assert_eq!(
        code(
            driver
                .call_raw(
                    "Environment.Advance",
                    Some(scope.clone()),
                    bad.to_json(),
                    &[]
                )
                .await
        ),
        ErrorCode::InvalidArgument
    );
    driver.advance(0).await.expect("a complete batch advances");

    // A slot the composition does not declare; a slot never saved.
    let save = SaveSlotParams {
        slot_id: id("spare"),
    };
    assert_eq!(
        code(
            driver
                .call_raw(METHOD_SAVE_SLOT, Some(driver.scope()), save.to_json(), &[])
                .await
        ),
        ErrorCode::InvalidArgument
    );
    let mut next = driver.scope();
    next.epoch = id("e9");
    let restore = driver.restore_slot_params(&id("best"));
    assert_eq!(
        code(
            driver
                .call_raw(
                    METHOD_RESTORE_SLOT,
                    Some(next.clone()),
                    restore.to_json(),
                    &[]
                )
                .await
        ),
        ErrorCode::InvalidPhase
    );
    // A rollback that does not change epoch.
    assert_eq!(
        code(
            driver
                .call_raw(
                    METHOD_RESTORE_SLOT,
                    Some(driver.scope()),
                    restore.to_json(),
                    &[]
                )
                .await
        ),
        ErrorCode::InvalidArgument
    );

    // A duplicate RestoreSlot replays the cached reply; it does not roll back twice.
    driver.save_slot(&id("best")).await.unwrap();
    driver
        .advance(flybrain_gb::emulator::buttons::A)
        .await
        .unwrap();
    let request = RestoreSlotParams {
        slot_id: id("best"),
        prior_epoch: id("e1"),
        policy: "legacy-ratchet-rollback-v1".into(),
    };
    let scope_e2 = scope_at("legacy", "e2", driver.boundary());
    let once = driver
        .call_raw(
            METHOD_RESTORE_SLOT,
            Some(scope_e2.clone()),
            request.to_json(),
            &[],
        )
        .await
        .unwrap();
    let again = driver
        .retry_raw(
            once.request_id.clone(),
            METHOD_RESTORE_SLOT,
            Some(scope_e2.clone()),
            request.to_json(),
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        once.result().unwrap(),
        again.result().unwrap(),
        "the duplicate replays"
    );
    // The old epoch is gone.
    let stale = driver.advance_params(0);
    assert_eq!(
        code(
            driver
                .call_raw(
                    "Environment.Advance",
                    Some(driver.scope()),
                    stale.to_json(),
                    &[]
                )
                .await
        ),
        ErrorCode::StaleEpoch
    );

    // The rolled-back world is the driver's once it moves to the new epoch.
    driver.bump_epoch();
    driver
        .capture()
        .await
        .expect("the world captures at (e2, k)");
    rig.stop().await;

    // The cartridge on disk is not the one the configuration names: wrong ROM.
    let (_dir2, rig) = rig_with(mode, &other.path, &toy.backend).await;
    let mut driver = rig.driver();
    let refused = driver
        .call_raw(
            "Environment.Initialize",
            Some(driver.scope()),
            driver.initialize_params().to_json(),
            &[],
        )
        .await;
    assert_eq!(code(refused), ErrorCode::IncompatibleState);
    // And a legacy checkpoint of another cartridge does not stage.
    let mut state = world_of(&toy).await;
    state.rom_digest = other.backend.rom_digest.clone();
    let source = scope_at("legacy", "legacy", state.boundary);
    let payload = driver.payload_of(&state, source.clone()).await.unwrap();
    let staged = driver
        .call_raw(
            "State.StageRestore",
            Some(scope_at("legacy", "e2", state.boundary)),
            json!({
                "checkpointId": payload.result.checkpoint_id,
                "sourceScope": source.to_json(),
                "compatibilityDigest": payload.result.compatibility_digest,
                "payload": payload.artifact.reference().to_json(),
            }),
            &[("payload", &payload.artifact)],
        )
        .await;
    assert_eq!(code(staged), ErrorCode::IncompatibleState);
    let _ = rig.stop().await;

    // A launch configuration that is not the legacy scaffold refuses to initialize at all.
    let mut two = toy.backend.clone();
    two.setup_frames = 2;
    let (_dir3, rig) = rig_with(mode, &toy.path, &two).await;
    let mut driver = rig.driver();
    let refused = driver
        .call_raw(
            "Environment.Initialize",
            Some(driver.scope()),
            driver.initialize_params().to_json(),
            &[],
        )
        .await;
    assert_eq!(code(refused), ErrorCode::IncompatibleState);
    rig.stop().await;
}

async fn rig_with(
    mode: ExecutionMode,
    path: &Path,
    backend: &BackendConfig,
) -> (tempfile::TempDir, EnvRig) {
    rig(mode, path, backend).await
}

/// A world of the toy cartridge at a few frames in, as data.
async fn world_of(toy: &Toy) -> WorldState {
    let script = EnvScript {
        name: "w".into(),
        start: EnvStart::Fresh,
        ops: vec![parity::EnvOp::Advance(0); 3],
    };
    let (mut direct, _) =
        parity::DirectEmulator::fresh(toy.rom.clone(), toy.backend.clone()).unwrap();
    for op in &script.ops {
        if let parity::EnvOp::Advance(m) = op {
            direct.advance(*m).unwrap();
        }
    }
    direct.state().unwrap()
}

/// The image read leaves the emulator exactly as the frame left it: the exported state after the
/// read is byte for byte the state before it, and the next frame is the frame an unread emulator
/// draws. An unguarded 64K read changes the export (binjgb syncs OAM/VRAM/timer/STAT/LY on read),
/// which this also shows, so the guard is not vacuous.
#[test]
fn the_memory_image_leaves_the_emulator_as_it_found_it() {
    use flybrain_gb::emulator::Emulator;
    let rom = toy_cart::rom(0);
    let boot = || {
        let mut e = Emulator::new(&rom, 48_000, 4_096).unwrap();
        for m in [0u8, 1, 1, 16, 0] {
            e.set_buttons(m);
            e.run_frame().unwrap();
            let _ = e.take_audio_u8();
        }
        e
    };
    let mut read = boot();
    let unread = boot();
    let before = read.export_state().unwrap();
    let image = legacy_env::memory_image(&mut read).unwrap();
    assert_eq!(image.len(), 65_536);
    // MEM-01's bulk read is neutral by construction: no guard around it.
    assert_eq!(read.export_state().unwrap(), before, "the bulk read is neutral");
    for (address, byte) in image.iter().enumerate() {
        let address = address as u16;
        if flybrain_gb::captured(address) {
            assert_eq!(*byte, unread.read_uncached(address), "byte {address:04x} is fly_gb_read_mem's");
        } else {
            assert_eq!(*byte, flybrain_gb::NOT_CAPTURED, "register byte {address:04x} is not captured");
        }
    }
    // Why the register windows are not captured: reading them through `emulator_read_mem` runs
    // binjgb's lazy catch-up, which moves the exported state.
    let mut fresh = boot();
    let _: Vec<u8> = (0..=u16::MAX).map(|a| fresh.read_uncached(a)).collect();
    assert_ne!(fresh.export_state().unwrap(), before, "a register read is not neutral");
    let mut unread = boot();
    for e in [&mut read, &mut unread] {
        e.set_buttons(32);
        e.run_frame().unwrap();
    }
    assert_eq!(read.framebuffer(), unread.framebuffer());
    assert_eq!(read.export_state().unwrap(), unread.export_state().unwrap());
}

#[test]
fn the_worker_advertises_the_slots_extension() {
    assert!(legacy_env::capabilities().contains(&id(SLOTS_CAPABILITY)));
    assert!(legacy_env::capabilities().contains(&id("checkpoint-v1")));
}

// -------------------------------------------------------------------------------------------
// The real cartridge (rom-env)

fn rom_inputs() -> Option<(PathBuf, Arc<Vec<u8>>, BackendConfig)> {
    let path = PathBuf::from(std::env::var_os("FLY_ROM")?);
    let rom = std::fs::read(&path).ok()?;
    let backend = BackendConfig::legacy(&digest_of_bytes(&rom));
    Some((path, Arc::new(rom), backend))
}

/// The real cartridge from the row-58 checkpoint (`FLY_DOOR_CHECKPOINT`): a 480-frame button walk
/// with slot saves, rollbacks (the first onto the checkpoint's own ratchet slot's successor) and
/// a restart, direct against the worker, frame by frame.
async fn rom_checkpoint_parity(mode: ExecutionMode) {
    let (Some((path, rom, backend)), Some(checkpoint)) =
        (rom_inputs(), std::env::var_os("FLY_DOOR_CHECKPOINT"))
    else {
        eprintln!("skipped: FLY_ROM or FLY_DOOR_CHECKPOINT is not set");
        return;
    };
    let bytes = Arc::new(std::fs::read(checkpoint).expect("the checkpoint reads"));
    let mut script = scripted(
        "rom-row58-480",
        EnvStart::Flysim01(bytes),
        &button_walk(480, 58),
    );
    // The checkpoint's ratchet slot is restored with the world: roll back onto it first.
    script
        .ops
        .insert(3, parity::EnvOp::Rollback(id(legacy_env::DEFAULT_SLOT)));
    let direct = run_direct(rom.clone(), &backend, &script).expect("direct");
    let (_dir, mut rig) = rig(mode, &path, &backend).await;
    let worker = run_on_worker(&mut rig, &script).await.expect("worker");
    rig.stop().await;
    compare(&direct, &worker).expect("the worker is the emulator driven directly");
    eprintln!(
        "{}: {} records identical, last {}",
        mode.label(),
        worker.len(),
        worker.last().unwrap().row()
    );
}

/// FND-01's traces of the running service (`FLY_ENV01_TRACE_DIR`: `<name>.trace.jsonl` with its
/// start `<name>.checkpoint`, or the row-58 checkpoint for `r58`): every transition's frame and
/// WRAM digest, and every slot save's state digest, reproduced by the worker.
async fn service_trace_replay(mode: ExecutionMode) {
    let (Some((path, rom, backend)), Some(dir)) =
        (rom_inputs(), std::env::var_os("FLY_ENV01_TRACE_DIR"))
    else {
        eprintln!("skipped: FLY_ROM or FLY_ENV01_TRACE_DIR is not set");
        return;
    };
    let dir = PathBuf::from(dir);
    let limit = std::env::var("FLY_ENV01_TRACE_LIMIT")
        .ok()
        .and_then(|n| n.parse().ok());
    for name in ["rollback", "climb", "r58"] {
        let trace_path = dir.join(format!("{name}.trace.jsonl"));
        let checkpoint = if name == "r58" {
            std::env::var_os("FLY_DOOR_CHECKPOINT").map(PathBuf::from)
        } else {
            Some(dir.join(format!("{name}.checkpoint")))
        };
        let Some(checkpoint) = checkpoint.filter(|c| c.is_file() && trace_path.is_file()) else {
            eprintln!("skipped: {name} trace or checkpoint missing");
            continue;
        };
        let trace = FlyTrace::read(&trace_path, limit).expect("the trace reads");
        let script = trace.script(name, Arc::new(std::fs::read(&checkpoint).unwrap()));
        let (_dir, mut rig) = rig(mode, &path, &backend).await;
        let worker = run_on_worker(&mut rig, &script).await.expect("worker");
        rig.stop().await;
        let compared = trace
            .check(&worker)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let actions: usize = trace.frames.iter().map(|f| f.actions.len()).sum();
        eprintln!(
            "{}: {name}: {} transitions, {actions} boundary actions, {compared} values equal the service's",
            mode.label(),
            trace.frames.len()
        );
        let _ = &rom;
    }
}

all_modes!(
    the_toy_golden_holds,
    refusals,
    rom_checkpoint_parity,
    service_trace_replay
);
