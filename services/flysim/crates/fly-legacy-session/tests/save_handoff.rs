//! TASK-01 round-2 review notes R2-1 and R2-2 on the session's save hand-off. The toy raw
//! composition from power-on (`FLY_ROM`; a build without it skips):
//!
//! - R2-1: a save begun at boundary k reaches the writer even when the step after it fails, and
//!   `flush_captures` hands it over while the loop idles (a pause), as the legacy loop's writer
//!   thread has it from the boundary on.
//! - R2-2: `completed_saves` reports every queued save's outcome, a failed one beside the rest.

use std::path::PathBuf;

use fly_legacy_session::composition::{
    LegacyConfig, LegacySession, StoreConfig,
};
use fly_session::ExecutionMode;
use fly_session::legacy_agent::LegacyProfileKind;
use fly_session::legacy_checkpoint::SaveKind;
use fly_session::legacy_parity;
use fly_session::types::id;
use flysim::snapshot::MacroMode;

async fn session(root: &std::path::Path) -> Option<(LegacySession, StoreConfig)> {
    let Some(rom_path) = std::env::var_os("FLY_ROM").map(PathBuf::from) else {
        eprintln!("skipping: FLY_ROM is not set (source bin/rom-env.sh)");
        return None;
    };
    let config = LegacyConfig {
        mode: ExecutionMode::InProcess,
        rom_path,
        dataset_dir: legacy_parity::toy::dir(),
        profile: LegacyProfileKind::Toy,
        macro_mode: MacroMode::Raw,
        agent_id: id("fly"),
        agent_threads: 1,
        record: true,
    };
    let mut session = LegacySession::start(&root.join("run"), config)
        .await
        .expect("the session starts");
    session.bootstrap().await.expect("Ready(0)");
    let store = StoreConfig {
        hot_dir: root.join("hot"),
        durable_dir: root.join("durable"),
        keep_generations: 4,
        // Saves only when asked.
        hot_seconds: 1e9,
        checkpoint_seconds: 1e9,
        speed: 1.0,
    };
    session.attach_store(&store).expect("the store attaches");
    Some((session, store))
}

/// One step, then the interval save it began (the first boundary is due for a durable one)
/// written and collected, so a test starts with nothing queued.
async fn warm_up(session: &mut LegacySession) {
    session.advance().await.expect("a step");
    session.flush_captures().await.expect("the flush");
    for outcome in session.completed_saves(true).await {
        outcome.expect("the first save");
    }
}

fn files_in(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_begun_save_reaches_the_writer_when_the_next_step_fails() {
    let root = tempfile::tempdir().expect("tmp");
    let Some((mut session, _)) = session(root.path()).await else { return };
    warm_up(&mut session).await;
    for _ in 0..2 {
        session.advance().await.expect("a step");
    }
    session.begin_save(SaveKind::Hot).await.expect("a capture begins");
    // The next transition fails at its commit.
    session.coordinator.injections.at_step = 3;
    session.coordinator.injections.undeclared_stimulus = true;
    let error = session.advance().await.err().expect("the step fails");
    assert!(error.starts_with("step ("), "{error}");
    let outcomes = session.completed_saves(true).await;
    assert_eq!(outcomes.len(), 1, "the save begun before the failed step was written");
    let report = outcomes.into_iter().next().unwrap().expect("and it committed");
    assert!(!report.durable);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pause_flushes_the_begun_save() {
    let root = tempfile::tempdir().expect("tmp");
    let Some((mut session, store)) = session(root.path()).await else { return };
    warm_up(&mut session).await;
    session.begin_save(SaveKind::Hot).await.expect("a capture begins");
    let before = files_in(&store.hot_dir);
    let queued = session.flush_captures().await.expect("the flush");
    assert_eq!(queued.len(), 1);
    let outcomes = session.completed_saves(true).await;
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].is_ok());
    assert!(files_in(&store.hot_dir) > before, "the hot generation is on disk");
    assert!(
        session.flush_captures().await.expect("a second flush").is_empty(),
        "nothing is held twice"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_save_outcome_is_reported_after_a_failure() {
    let root = tempfile::tempdir().expect("tmp");
    let Some((mut session, store)) = session(root.path()).await else { return };
    warm_up(&mut session).await;
    // The store's hot directory goes away under the writer: every save after it fails. The
    // first failure must not hide the outcomes behind it.
    std::fs::remove_dir_all(&store.hot_dir).expect("remove hot");
    std::fs::write(&store.hot_dir, b"not a directory").expect("block hot");
    let mut expected = 0;
    for _ in 0..3 {
        session.begin_save(SaveKind::Hot).await.expect("begin");
        session.flush_captures().await.expect("flush");
        expected += 1;
    }
    let outcomes = session.completed_saves(true).await;
    assert_eq!(outcomes.len(), expected, "one outcome per queued save: {outcomes:?}");
    assert!(outcomes.iter().all(Result::is_err), "the writes failed: {outcomes:?}");
}
