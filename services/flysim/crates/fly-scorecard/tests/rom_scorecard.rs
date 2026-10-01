//! The scorecard on the real cartridge and connectome: a short run per runtime from a real
//! checkpoint. ROM-gated: `FLY_ROM`, `FLY_DOOR_CHECKPOINT` (rom-env.sh) and `data/fafb-v783`;
//! without them each test prints why and returns (source rom-env.sh, or it covers nothing).
//!
//! What it holds:
//! - a run measures something (frames, a pad, macros) and its minutes are the minutes asked for;
//! - the same seed twice is the same run, and two seeds are not (the idle frames);
//! - the session runtime and the legacy loop, from the same checkpoint and seed, give the same
//!   report: what the scorecard measures does not depend on which runtime ran it.

use std::path::PathBuf;

use fly_scorecard::runner::{RunSpec, run_legacy, run_session};
use fly_scorecard::tally::RunReport;
use flysim::snapshot::MacroMode;

fn spec(seed: u32) -> Option<RunSpec> {
    let rom = std::env::var_os("FLY_ROM").map(PathBuf::from);
    let checkpoint = std::env::var_os("FLY_DOOR_CHECKPOINT").map(PathBuf::from);
    let dataset = fly_session::legacy_parity::fafb_dir();
    let (Some(rom), Some(checkpoint), Some(dataset)) = (rom, checkpoint, dataset) else {
        eprintln!("skipping: FLY_ROM, FLY_DOOR_CHECKPOINT or data/fafb-v783 missing");
        return None;
    };
    Some(RunSpec {
        id: "door".to_owned(),
        checkpoint,
        rom,
        dataset,
        seed,
        minutes: 0.4,
        threads: 2,
        mode: MacroMode::Macros,
        tally: RunSpec::tally_config(5.0, 20.0),
    })
}

/// A report with what legitimately differs between runs of the same thing blanked.
fn essence(mut report: RunReport) -> RunReport {
    report.wall_seconds = 0.0;
    report.runtime = String::new();
    report
}

#[test]
fn the_legacy_loop_is_measured_reproducibly_and_seeds_differ() {
    let (Some(a), Some(b), Some(c)) = (spec(0), spec(3), spec(4)) else { return };
    let first = run_legacy(&a).expect("a legacy run");
    assert!(first.frames > 1000, "{} frames", first.frames);
    assert!((first.minutes - 0.4).abs() < 0.01, "{} minutes", first.minutes);
    assert_eq!(first.idle_frames, 0);
    assert!(first.macros.starts > 0, "the macro layer dealt nothing");
    assert!(first.scenes.values().sum::<u64>() == first.frames);
    assert_eq!(essence(first.clone()), essence(run_legacy(&a).expect("a second run")));
    let (x, y) = (run_legacy(&b).expect("seed 3"), run_legacy(&c).expect("seed 4"));
    assert_ne!(x.idle_frames, y.idle_frames);
    assert!(x.idle_frames > 0);
    // Different starting points of the brain's own trajectory: the runs need not agree.
    eprintln!(
        "seed 0/3/4 macro starts {}/{}/{}, pad {:.3}/{:.3}/{:.3}",
        first.macros.starts,
        x.macros.starts,
        y.macros.starts,
        first.pad.empty_fraction,
        x.pad.empty_fraction,
        y.pad.empty_fraction
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_runtime_and_the_legacy_loop_measure_the_same_run() {
    let Some(spec) = spec(2) else { return };
    let legacy = run_legacy(&spec).expect("the legacy run");
    let session = run_session(&spec).await.expect("the session run");
    assert_eq!(session.runtime, "session");
    assert_eq!(legacy.runtime, "legacy");
    assert!(session.frames > 1000);
    assert_eq!(essence(legacy), essence(session));
}
