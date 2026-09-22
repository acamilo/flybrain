//! Restarting a run from an earlier rung, on disk, with flysim stopped.
//!
//! The operator's decision of 2026-09-22 was "restart the live run from an early checkpoint
//! instead of from scratch". `FLY_RESET_STATE=1` cannot do that: it archives everything and the
//! next start warms up a fresh fly. What this does instead is promote one milestone archive --
//! `milestone-<N>.checkpoint`, which `store::Store::commit` writes at the first commit at a new
//! best rank and which no rotation ever unlinks -- to being the only thing either store will
//! restore.
//!
//! `infra/bin/fly-reset-to-milestone` is the operator-facing wrapper; it refuses to run while
//! flysim is up, calls `flysim --reset-to-milestone N`, and fixes ownership afterwards. The work
//! is here rather than in that script because two of the steps are inside the `FLYSIM01`
//! envelope: the ratchet's attempts and recoveries counters live in the checkpoint's manifest,
//! and a shell script has no business rewriting one.
//!
//! What it does, in order, and nothing else:
//!
//! 1. **archives** every file in the durable and hot stores into a dated directory, by copying,
//!    so a step that fails later has destroyed nothing;
//! 2. **rewrites** the rung's archive with the ratchet's `attempts` and `recoveries` at zero, so
//!    the recovery budget is not already spent when the restarted run begins. `best` is left
//!    alone: the archive's own `best` is the rung it was taken at, which is exactly what the
//!    restarted run is at, and the rank the stream shows is recomputed by the adapter from the
//!    restored game state anyway;
//! 3. **installs** it as the newest generation in both stores, so the restore order
//!    (`store::restore_order`: hot latest, hot previous, durable latest, ...) reaches it first;
//! 4. **clears** the milestone archives above N -- rungs the run had reached and is now below --
//!    and the generation files of the run being abandoned;
//! 5. **clears the session ledgers**: the event log `events.jsonl` and its rotations. The
//!    checkpoint carries `lastEventId`, so restoring an old checkpoint over a newer log would
//!    re-issue ids the log already holds. The macro layer's own session ledgers (blocked,
//!    talked, reached, pushed-back) are memory-only by contract
//!    (`docs/design/macros.md` section 12.1: "a restored run offers every target once more"),
//!    so stopping flysim is what resets those and this has nothing to do.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::store::{self, Store, StoreManifest};

/// Generations kept by the stores this tool writes. Only used for the rotation bound, which
/// this tool does not trigger; the durable store's own value.
const KEEP_GENERATIONS: usize = 8;

/// A generation number no commit ever allocates, so `Store::candidates` drops the
/// generation-file half of an archive entry and offers only `milestone-<rank>.checkpoint`.
///
/// `Sim::boot` allocates `highest_generation() + 1`, which is 1 or more, so 0 names no file.
/// The milestone archives below the rung being restored are kept exactly this way: still on
/// disk, still restorable as a deeper fallback, and with no generation file pretending to be
/// their contents.
const NO_GENERATION: u64 = 0;

/// What the reset did, one line per step, for the operator's terminal and the run record.
pub type Report = Vec<String>;

/// Promote `rank`'s milestone archive to be the restore source of both stores.
///
/// `archive` is the dated directory the current state is copied into; it must not exist.
/// Refuses if the milestone archive is missing, which is the "that rung was never reached"
/// case and the one mistake worth refusing rather than guessing at.
pub fn reset_to_milestone(
    durable_dir: &Path,
    hot_dir: &Path,
    rank: u32,
    archive: &Path,
) -> Result<Report> {
    let durable = Store::new(durable_dir, KEEP_GENERATIONS);
    let hot = Store::new(hot_dir, KEEP_GENERATIONS);
    let source = durable.archive_path(rank);
    if !source.is_file() {
        bail!(
            "no milestone archive for rung {rank}: {} does not exist. `ls {}` shows the rungs \
             this run actually reached.",
            source.display(),
            durable_dir.display()
        );
    }
    if archive.exists() {
        bail!("the archive directory {} already exists", archive.display());
    }

    let mut report: Report = Vec::new();

    // 1. Copy everything aside first.
    let copied_durable = copy_tree(durable_dir, &archive.join("durable"))?;
    let copied_hot = copy_tree(hot_dir, &archive.join("hot"))?;
    report.push(format!(
        "archived {copied_durable} durable and {copied_hot} hot files to {}",
        archive.display()
    ));

    // 2. Zero the two recovery counters inside the envelope.
    let mut checkpoint = store::load(&source)
        .with_context(|| format!("decoding {}", source.display()))?;
    let spent = (checkpoint.runtime.ratchet.attempts, checkpoint.runtime.ratchet.recoveries);
    checkpoint.runtime.ratchet.attempts = 0;
    checkpoint.runtime.ratchet.recoveries = 0;

    let generation = durable.highest_generation().max(hot.highest_generation()) + 1;
    checkpoint.runtime.generation = generation;
    let bytes = store::encode(&checkpoint.agent, &checkpoint.runtime)?;
    report.push(format!(
        "rung {rank} (best {}, ladder rank recomputed from the game state): ratchet attempts \
         {} -> 0, recoveries {} -> 0",
        checkpoint.runtime.ratchet.best, spent.0, spent.1
    ));

    // 3/4. Clear both stores, keeping the milestone archives at or below this rung, and write
    // the promoted state as the newest generation of each.
    let kept = clear_store(durable_dir, Some(rank))?;
    clear_store(hot_dir, None)?;
    report.push(format!(
        "cleared the hot store and every milestone archive above rung {rank}; kept {} at or \
         below it: {kept:?}",
        kept.len()
    ));

    // The hot store lives on a tmpfs that a stopped container may not have mounted yet, so it
    // is created rather than assumed; the durable one already exists or the milestone archive
    // above could not have been read.
    durable.create()?;
    hot.create()?;
    store::write_atomic(&durable.generation_path(generation), &bytes)?;
    store::write_atomic(&durable.archive_path(rank), &bytes)?;
    store::write_atomic(&hot.generation_path(generation), &bytes)?;

    let mut archives: BTreeMap<u32, u64> =
        kept.iter().map(|rung| (*rung, NO_GENERATION)).collect();
    archives.insert(rank, generation);
    let durable_manifest = StoreManifest {
        generation,
        latest: Some(generation),
        previous: None,
        archives,
    };
    write_manifest(&durable, &durable_manifest)?;
    write_manifest(
        &hot,
        &StoreManifest {
            generation,
            latest: Some(generation),
            previous: None,
            archives: BTreeMap::new(),
        },
    )?;
    report.push(format!(
        "generation {generation} is now hot latest and durable latest in {} and {}",
        hot_dir.display(),
        durable_dir.display()
    ));

    // 5. The session ledgers.
    let logs = remove_matching(durable_dir, |name| {
        name == "events.jsonl" || (name.starts_with("events-") && name.ends_with(".jsonl"))
    })?;
    report.push(format!(
        "reset the session ledgers: {logs} event-log files removed (the macro layer's are \
         memory-only and are reset by stopping flysim)"
    ));

    Ok(report)
}

fn write_manifest(store: &Store, manifest: &StoreManifest) -> Result<()> {
    store.create()?;
    store::write_atomic(&store.manifest_path(), &serde_json::to_vec_pretty(manifest)?)
}

/// Copy every regular file of `from` into `to`, creating `to`. A missing source is zero files,
/// not an error: the hot store lives on a tmpfs that a stopped container does not have.
fn copy_tree(from: &Path, to: &Path) -> Result<usize> {
    if !from.is_dir() {
        return Ok(0);
    }
    std::fs::create_dir_all(to)
        .with_context(|| format!("creating {}", to.display()))?;
    let mut copied = 0;
    for entry in std::fs::read_dir(from)?.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        std::fs::copy(entry.path(), to.join(entry.file_name()))
            .with_context(|| format!("copying {}", entry.path().display()))?;
        copied += 1;
    }
    Ok(copied)
}

/// Remove every checkpoint, tmp file and manifest from `dir`, keeping `milestone-<r>.checkpoint`
/// for `r <= keep_up_to`. Returns the rungs kept, ascending.
fn clear_store(dir: &Path, keep_up_to: Option<u32>) -> Result<Vec<u32>> {
    let mut kept = Vec::new();
    if !dir.is_dir() {
        return Ok(kept);
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let milestone = name
            .strip_prefix("milestone-")
            .and_then(|rest| rest.strip_suffix(".checkpoint"))
            .and_then(|rung| rung.parse::<u32>().ok());
        let remove = match milestone {
            // The promoted rung's own archive is rewritten straight after this, so it is
            // removed here like the rest and reinstated with the counters cleared.
            Some(rung) => match keep_up_to {
                Some(limit) if rung < limit => {
                    kept.push(rung);
                    false
                }
                _ => true,
            },
            None => {
                name == "manifest.json"
                    || name.ends_with(".checkpoint")
                    || name.ends_with(".checkpoint.tmp")
            }
        };
        if remove {
            std::fs::remove_file(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        }
    }
    kept.sort_unstable();
    Ok(kept)
}

fn remove_matching(dir: &Path, wanted: impl Fn(&str) -> bool) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if wanted(&entry.file_name().to_string_lossy()) {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// `YYYYMMDDTHHMMSSZ` in UTC, for the dated archive directory's name.
///
/// Built on the event log's own calendar conversion, which is the service's only one; the time of
/// day is arithmetic on the same millisecond count.
pub fn utc_stamp(wall_ms: u64) -> String {
    let second_of_day = (wall_ms % 86_400_000) / 1_000;
    format!(
        "{}T{:02}{:02}{:02}Z",
        crate::eventlog::utc_day(wall_ms),
        second_of_day / 3_600,
        (second_of_day / 60) % 60,
        second_of_day % 60,
    )
}

/// The default dated archive directory: a sibling of the durable store, which is its own
/// mountpoint and so cannot be renamed -- the same shape `infra/05-deploy.sh` uses for
/// `FLY_RESET_STATE=1`.
pub fn default_archive_dir(durable_dir: &Path, stamp: &str) -> PathBuf {
    let mut name = durable_dir.as_os_str().to_os_string();
    name.push(format!(".reset-{stamp}"));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `FLYSIM01` envelope, small but structurally complete, so these tests decode and
    /// re-encode what the running service writes rather than a stand-in.
    fn envelope(generation: u64, ratchet: flybrain_gb::RatchetState) -> Vec<u8> {
        use flybrain_core::decoder::DecoderState;
        use flybrain_core::lif::LifState;
        use flybrain_core::ordered::NumberMap;
        use flybrain_core::plasticity::PlasticityState;

        let agent = flybrain_core::agent::AgentState {
            version: 1,
            remainder: 0.75,
            warmed_up: true,
            network: LifState {
                membrane: vec![0.5, -0.25],
                refractory: vec![0, 3],
                last_spike_ms: vec![-1_000_000.0, 12.0],
                visual_drive: vec![0.1],
                rng: -12_345,
                reward_remaining: 40.0,
                ms: 9_000.0,
                population_rate: 1.5,
                rates: NumberMap::from_pairs([("forward", 2.0)]),
                plasticity: PlasticityState {
                    version: "fly-kc-mbon-rstdp-v2".to_string(),
                    topology: 42,
                    enabled: true,
                    updates: 3.0,
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
        };
        let runtime = store::RuntimeState {
            generation,
            wall_ms: 1_700_000_000_000,
            rom_sha256: "ab".repeat(32),
            emulator_frame: 12_345,
            compatibility: "kernel/pokered-unique8-v6/fingerprint".to_string(),
            speed: 1.0,
            buttons: 0,
            rank_since_ms: 4_242.0,
            last_event_id: 77,
            reward: serde_json::json!({ "version": 4, "total": 1.25 }),
            ratchet,
            emulator: vec![7; 64],
            framebuffer: vec![9; 32],
            ratchet_game: vec![1, 2, 3],
            ratchet_frame: vec![4, 5, 6],
        };
        store::encode(&agent, &runtime).unwrap()
    }

    /// A store dir holding a milestone archive for each rung in `rungs`, their generations, a
    /// manifest and an event log, all written through the store's own commit path.
    fn state_dir(root: &Path, rungs: &[u32], ratchet: flybrain_gb::RatchetState) -> Store {
        let store = Store::new(root, KEEP_GENERATIONS);
        store.create().unwrap();
        for (index, rung) in rungs.iter().enumerate() {
            let generation = index as u64 + 1;
            store.commit(generation, &envelope(generation, ratchet), Some(*rung)).unwrap();
        }
        std::fs::write(root.join("events.jsonl"), b"{}\n").unwrap();
        std::fs::write(root.join("events-20260921.jsonl"), b"{}\n").unwrap();
        store
    }

    #[test]
    fn a_missing_rung_is_refused_and_nothing_is_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let durable = tmp.path().join("state");
        let hot = tmp.path().join("hot");
        state_dir(&durable, &[3, 5], flybrain_gb::RatchetState::default());
        let before = std::fs::read_dir(&durable).unwrap().flatten().count();

        let error = reset_to_milestone(&durable, &hot, 9, &tmp.path().join("archive"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no milestone archive for rung 9"), "{error}");
        assert_eq!(std::fs::read_dir(&durable).unwrap().flatten().count(), before);
        assert!(!tmp.path().join("archive").exists(), "nothing was archived");
    }

    #[test]
    fn the_rung_becomes_both_stores_latest_with_the_recovery_budget_back() {
        let tmp = tempfile::tempdir().unwrap();
        let durable = tmp.path().join("state");
        let hot = tmp.path().join("hot");
        let spent = flybrain_gb::RatchetState {
            best: 5,
            attempts: 3,
            recoveries: 11,
            ..flybrain_gb::RatchetState::default()
        };
        state_dir(&durable, &[3, 5, 9, 11], spent);
        state_dir(&hot, &[11], spent);
        let archive = tmp.path().join("archive-20260922");

        let report = reset_to_milestone(&durable, &hot, 5, &archive).unwrap();
        assert!(report.iter().any(|line| line.contains("attempts 3 -> 0")), "{report:?}");

        // Everything that was there is in the archive.
        assert!(archive.join("durable/milestone-11.checkpoint").is_file());
        assert!(archive.join("durable/events.jsonl").is_file());
        assert!(archive.join("hot/manifest.json").is_file());

        // The rungs above 5 are gone; the ones below it stay as deeper fallbacks.
        assert!(!durable.join("milestone-9.checkpoint").exists());
        assert!(!durable.join("milestone-11.checkpoint").exists());
        assert!(durable.join("milestone-3.checkpoint").is_file());
        assert!(durable.join("milestone-5.checkpoint").is_file());
        assert!(!durable.join("events.jsonl").exists());
        assert!(!durable.join("events-20260921.jsonl").exists());
        assert!(!hot.join("milestone-11.checkpoint").exists());

        // Both stores restore the rung, and the counters are back.
        for store in [Store::new(&hot, KEEP_GENERATIONS), Store::new(&durable, KEEP_GENERATIONS)] {
            let candidates = store.candidates("x");
            let first = store::load(&candidates[0].path).unwrap();
            assert_eq!(first.runtime.ratchet.best, 5);
            assert_eq!((first.runtime.ratchet.attempts, first.runtime.ratchet.recoveries), (0, 0));
        }
        // ... including through the promoted milestone archive itself.
        let archived = store::load(&durable.join("milestone-5.checkpoint")).unwrap();
        assert_eq!((archived.runtime.ratchet.attempts, archived.runtime.ratchet.recoveries), (0, 0));

        // The rungs below it are offered, and only as their own archive files.
        let manifest = Store::new(&durable, KEEP_GENERATIONS).manifest().unwrap();
        assert_eq!(manifest.archives.get(&3), Some(&NO_GENERATION));
        assert_eq!(manifest.latest, manifest.archives.get(&5).copied());
        assert!(
            Store::new(&durable, KEEP_GENERATIONS)
                .candidates("durable")
                .iter()
                .any(|candidate| candidate.path.ends_with("milestone-3.checkpoint"))
        );
    }

    #[test]
    fn a_second_reset_refuses_to_write_over_an_existing_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let durable = tmp.path().join("state");
        let hot = tmp.path().join("hot");
        state_dir(&durable, &[4], flybrain_gb::RatchetState::default());
        let archive = tmp.path().join("archive");
        reset_to_milestone(&durable, &hot, 4, &archive).unwrap();
        let error = reset_to_milestone(&durable, &hot, 4, &archive).unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn the_stamp_is_the_event_logs_calendar_plus_a_time_of_day() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        // 2026-09-22T16:15:00Z
        assert_eq!(utc_stamp(1_790_093_700_000), "20260922T161500Z");
    }

    #[test]
    fn the_default_archive_is_a_dated_sibling_of_the_store() {
        assert_eq!(
            default_archive_dir(Path::new("/srv/fly/state"), "20260922T161500Z"),
            PathBuf::from("/srv/fly/state.reset-20260922T161500Z")
        );
    }
}
