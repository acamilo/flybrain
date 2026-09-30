//! The spool: copies of the live stores' checkpoint generations, taken as they appear.
//!
//! The live service keeps two generations per store (`keep_generations`) and writes a hot one
//! every five seconds, so a hot file lives about ten seconds, while the trace line that names it
//! arrives some seconds after the capture (the recorder buffers) and the shadow may be further
//! behind still. The spool thread therefore polls both store directories and copies every new
//! `<gen>.checkpoint` into the spool at once. Store commits write `<gen>.checkpoint.tmp` and rename
//! it, so a file under its final name is complete; one rotated away mid-copy is skipped.
//!
//! The spool is bounded: generations below the shadow's floor (already compared, or of a finished
//! segment) are deleted, and above `max_bytes` the oldest pending ones go first; a capture whose
//! file is gone is reported as unavailable, never as a divergence. The spool only reads the live
//! stores; it never writes, renames or deletes anything in them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// The generation of a store file name, `<gen>.checkpoint`.
pub fn generation_of(name: &str) -> Option<u64> {
    name.strip_suffix(".checkpoint")?.parse().ok()
}

fn spool_name(generation: u64) -> String {
    format!("g{generation}.checkpoint")
}

/// Shared between the copying thread and the shadow.
#[derive(Clone)]
pub struct Spool {
    dir: PathBuf,
    sources: Vec<PathBuf>,
    max_bytes: u64,
    floor: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    evicted: Arc<AtomicU64>,
    /// Keep every copy (a rehearsal that replays the run again afterwards): no floor, no bound.
    keep: bool,
}

impl Spool {
    pub fn new(dir: &Path, sources: Vec<PathBuf>, max_bytes: u64) -> std::io::Result<Spool> {
        std::fs::create_dir_all(dir)?;
        Ok(Spool {
            dir: dir.to_owned(),
            sources,
            max_bytes,
            floor: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            evicted: Arc::new(AtomicU64::new(0)),
            keep: false,
        })
    }

    /// Keep every copy: the floor and the byte bound delete nothing.
    pub fn keep_all(mut self) -> Spool {
        self.keep = true;
        self
    }

    /// Generations below `floor` are no longer needed.
    pub fn set_floor(&self, floor: u64) {
        self.floor.fetch_max(floor, Ordering::Relaxed);
    }

    /// Pending generations dropped for space.
    pub fn evicted(&self) -> u64 {
        self.evicted.load(Ordering::Relaxed)
    }

    /// Where generation `generation` can be read: the spool's copy, else a live store's file.
    pub fn path(&self, generation: u64) -> Option<PathBuf> {
        let spooled = self.dir.join(spool_name(generation));
        if spooled.is_file() {
            return Some(spooled);
        }
        self.sources
            .iter()
            .map(|dir| dir.join(format!("{generation}.checkpoint")))
            .find(|path| path.is_file())
    }

    /// Reads generation `generation`, from the spool or a live store (a file rotated away between
    /// the lookup and the read is `None`).
    pub fn read(&self, generation: u64) -> Option<Vec<u8>> {
        let spooled = self.dir.join(spool_name(generation));
        if let Ok(bytes) = std::fs::read(&spooled) {
            return Some(bytes);
        }
        self.sources
            .iter()
            .find_map(|dir| std::fs::read(dir.join(format!("{generation}.checkpoint"))).ok())
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Starts the copying thread. The first scan copies only files modified within `window`
    /// (the stores' permanent milestone archives are old and not needed); later scans copy every
    /// new generation.
    pub fn spawn(&self, period: Duration, window: Duration) -> std::thread::JoinHandle<()> {
        let spool = self.clone();
        std::thread::Builder::new()
            .name("fly-shadow-spool".to_owned())
            .spawn(move || {
                let mut state = ScanState::default();
                let mut first = true;
                while !spool.stop.load(Ordering::Relaxed) {
                    spool.scan(&mut state, first.then_some(window));
                    first = false;
                    std::thread::sleep(period);
                }
            })
            .expect("the spool thread starts")
    }

    /// One pass: copy what is new, delete what is below the floor, keep within the byte bound.
    pub fn scan(&self, state: &mut ScanState, only_newer_than: Option<Duration>) {
        let floor = self.floor.load(Ordering::Relaxed);
        let now = SystemTime::now();
        for source in &self.sources {
            let Ok(entries) = std::fs::read_dir(source) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(generation) = name.to_str().and_then(generation_of) else {
                    continue;
                };
                if (generation < floor && !self.keep) || state.seen.contains(&generation) {
                    continue;
                }
                if let Some(window) = only_newer_than {
                    let recent = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| now.duration_since(t).ok())
                        .is_some_and(|age| age <= window);
                    if !recent {
                        state.seen.insert(generation);
                        continue;
                    }
                }
                let target = self.dir.join(spool_name(generation));
                let tmp = self.dir.join(format!("{}.tmp", spool_name(generation)));
                match std::fs::copy(entry.path(), &tmp) {
                    Ok(bytes) => {
                        if std::fs::rename(&tmp, &target).is_ok() {
                            state.held.insert(generation, bytes);
                        }
                        state.seen.insert(generation);
                    }
                    // Rotated away mid-copy: gone for good; the next scan will not see it either.
                    Err(_) => {
                        let _ = std::fs::remove_file(&tmp);
                    }
                }
            }
        }
        if self.keep {
            return;
        }
        let below: Vec<u64> = state.held.range(..floor).map(|(g, _)| *g).collect();
        for generation in below {
            let _ = std::fs::remove_file(self.dir.join(spool_name(generation)));
            state.held.remove(&generation);
        }
        while state.held.values().sum::<u64>() > self.max_bytes {
            let Some((&generation, _)) = state.held.iter().next() else {
                break;
            };
            let _ = std::fs::remove_file(self.dir.join(spool_name(generation)));
            state.held.remove(&generation);
            self.evicted.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Removes every spooled file (the shadow is done).
    pub fn clear(&self) {
        if self.keep {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name
                    .to_str()
                    .is_some_and(|n| n.starts_with('g') && n.contains(".checkpoint"))
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

/// What one spool thread has copied and seen.
#[derive(Default)]
pub struct ScanState {
    seen: BTreeSet<u64>,
    held: BTreeMap<u64, u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_new_generations_releases_below_the_floor_and_stays_bounded() {
        let live = tempfile::tempdir().unwrap();
        let hot = live.path().join("hot");
        let durable = live.path().join("durable");
        std::fs::create_dir_all(&hot).unwrap();
        std::fs::create_dir_all(&durable).unwrap();
        let spool_dir = live.path().join("spool");
        let spool = Spool::new(&spool_dir, vec![hot.clone(), durable.clone()], 25).unwrap();
        std::fs::write(hot.join("5.checkpoint"), [1u8; 10]).unwrap();
        std::fs::write(durable.join("6.checkpoint"), [2u8; 10]).unwrap();
        std::fs::write(durable.join("manifest.json"), b"{}").unwrap();
        std::fs::write(hot.join("7.checkpoint.tmp"), [3u8; 10]).unwrap();
        let mut state = ScanState::default();
        spool.scan(&mut state, Some(Duration::from_secs(3600)));
        assert!(spool_dir.join("g5.checkpoint").is_file());
        assert!(spool_dir.join("g6.checkpoint").is_file());
        assert!(!spool_dir.join("g7.checkpoint").exists());
        // The live store rotates 5 away: the spool still has it.
        std::fs::remove_file(hot.join("5.checkpoint")).unwrap();
        assert_eq!(spool.read(5), Some(vec![1u8; 10]));
        // A third file overflows the 25-byte bound: the oldest pending goes.
        std::fs::write(hot.join("8.checkpoint"), [4u8; 10]).unwrap();
        spool.scan(&mut state, None);
        assert_eq!(spool.evicted(), 1);
        assert_eq!(spool.read(5), None);
        assert!(spool.path(8).is_some());
        // Below the floor is released.
        spool.set_floor(8);
        spool.scan(&mut state, None);
        assert!(!spool_dir.join("g6.checkpoint").exists());
        // Still readable from the live store while it is there.
        assert_eq!(spool.read(6), Some(vec![2u8; 10]));
        // The live stores were never touched.
        assert!(durable.join("manifest.json").is_file());
        assert!(hot.join("8.checkpoint").is_file());
        spool.clear();
        assert!(!spool_dir.join("g8.checkpoint").exists());
    }
}
