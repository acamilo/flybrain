//! The append-only event log.
//!
//! `docs/control-api.md`: "append-only JSONL at `<saveDir>/events.jsonl`, one `FeedEvent` per
//! line, rotated daily. The feed's `events` array is the live tail of this file."
//! `docs/design/flysim.md` section 8 names the rotated files `events-YYYYMMDD.jsonl`. Both hold
//! here: the live file is always `events.jsonl` (which is what `infra/bin/fly-recap` and
//! `fly-retention` read), and the first append after UTC midnight renames the previous day's file
//! to `events-YYYYMMDD.jsonl` before opening a fresh one.
//!
//! The writer lives on the sim thread. The control API and the snapshot builder read the same
//! events out of an in-memory ring shared with them, so neither ever reads the file back.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::snapshot::{FeedEvent, FeedEventKind, RewardKind};

/// Events retained in memory for `GET /events` and the feed tail.
pub const RING_CAPACITY: usize = 4_096;

/// The shared in-memory tail. Cheap to clone; the sim thread writes, the API reads.
#[derive(Debug, Clone, Default)]
pub struct EventRing {
    inner: Arc<Mutex<VecDeque<FeedEvent>>>,
}

impl EventRing {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, event: FeedEvent) {
        let mut guard = self.inner.lock().expect("event ring poisoned");
        if guard.len() == RING_CAPACITY {
            guard.pop_front();
        }
        guard.push_back(event);
    }

    /// `GET /events?since=<id>&limit=<n>`: events with `id > since`, oldest first.
    ///
    /// The ring holds the most recent [`RING_CAPACITY`] events; a `since` older than that returns
    /// the oldest retained event onwards rather than reading the files back, which is what the
    /// bridge and the recap tooling need (they poll continuously, and the files stay on disk for
    /// anything historical).
    pub fn page(&self, since: u64, limit: usize) -> Vec<FeedEvent> {
        let guard = self.inner.lock().expect("event ring poisoned");
        guard
            .iter()
            .filter(|event| event.id > since)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("event ring poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Highest id in the ring, for resuming the id sequence after a restart.
    pub fn last_id(&self) -> u64 {
        self.inner
            .lock()
            .expect("event ring poisoned")
            .back()
            .map_or(0, |event| event.id)
    }
}

/// Everything but the identity fields, which the log assigns.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub kind: FeedEventKind,
    pub label: String,
    pub value: Option<f64>,
    pub reward_kind: Option<RewardKind>,
    pub by: Option<String>,
}

impl NewEvent {
    pub fn new(kind: FeedEventKind, label: impl Into<String>) -> Self {
        Self { kind, label: label.into(), value: None, reward_kind: None, by: None }
    }

    pub fn value(mut self, value: f64) -> Self {
        self.value = Some(value);
        self
    }

    pub fn reward_kind(mut self, kind: RewardKind) -> Self {
        self.reward_kind = Some(kind);
        self
    }

    pub fn by(mut self, by: impl Into<String>) -> Self {
        self.by = Some(by.into());
        self
    }
}

/// The JSONL writer plus the shared ring plus the pending tail for the next snapshot.
#[derive(Debug)]
pub struct EventLog {
    dir: PathBuf,
    file: File,
    /// UTC day the open file belongs to, `YYYYMMDD`.
    day: String,
    next_id: u64,
    ring: EventRing,
    pending: Vec<FeedEvent>,
    unflushed: usize,
}

impl EventLog {
    /// Open (or create) `<dir>/events.jsonl`, seeding the ring and the id sequence from it.
    pub fn open(dir: &Path, ring: EventRing) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating event log directory {}", dir.display()))?;
        let path = dir.join("events.jsonl");
        let mut next_id = 1;
        if path.exists() {
            for event in read_events(&path)? {
                next_id = next_id.max(event.id + 1);
                ring.push(event);
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            day: utc_day(now_wall_ms()),
            next_id,
            ring,
            pending: Vec::new(),
            unflushed: 0,
        })
    }

    pub fn ring(&self) -> &EventRing {
        &self.ring
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// Continue an id sequence recorded in a checkpoint manifest.
    pub fn resume_from(&mut self, last_id: u64) {
        self.next_id = self.next_id.max(last_id + 1);
    }

    /// Append one event: to the file, to the ring, and to the pending snapshot tail.
    pub fn append(&mut self, wall_ms: u64, brain_ms: f64, event: NewEvent) -> FeedEvent {
        let event = FeedEvent {
            id: self.next_id,
            wall_ms,
            brain_ms: crate::snapshot::finite(brain_ms).max(0.0),
            kind: event.kind,
            label: event.label,
            value: event.value.map(crate::snapshot::finite),
            reward_kind: event.reward_kind,
            by: event.by,
        };
        self.next_id += 1;
        if let Err(error) = self.write_line(wall_ms, &event) {
            // A full or read-only disk must not take the stream down; the ring still has it.
            tracing::error!(%error, "could not append to the event log");
        }
        self.ring.push(event.clone());
        self.pending.push(event.clone());
        event
    }

    /// Take the events since the previous snapshot (`FeedHeader.events`).
    pub fn take_pending(&mut self) -> Vec<FeedEvent> {
        std::mem::take(&mut self.pending)
    }

    /// Flush and fsync at most once a second, as section 8 specifies.
    pub fn flush(&mut self) -> Result<()> {
        if self.unflushed == 0 {
            return Ok(());
        }
        self.file.flush()?;
        self.file.sync_data()?;
        self.unflushed = 0;
        Ok(())
    }

    fn write_line(&mut self, wall_ms: u64, event: &FeedEvent) -> Result<()> {
        self.rotate_if_needed(wall_ms)?;
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.unflushed += 1;
        Ok(())
    }

    fn rotate_if_needed(&mut self, wall_ms: u64) -> Result<()> {
        let day = utc_day(wall_ms);
        if day == self.day {
            return Ok(());
        }
        self.file.flush()?;
        self.file.sync_data()?;
        let live = self.dir.join("events.jsonl");
        let archive = self.dir.join(format!("events-{}.jsonl", self.day));
        // An existing archive means the process restarted after midnight; append rather than
        // clobber a day that is already on disk.
        if archive.exists() {
            let mut existing = OpenOptions::new().append(true).open(&archive)?;
            let mut source = File::open(&live)?;
            std::io::copy(&mut source, &mut existing)?;
            std::fs::remove_file(&live)?;
        } else {
            std::fs::rename(&live, &archive)?;
        }
        self.file = OpenOptions::new().create(true).append(true).open(&live)?;
        self.day = day;
        self.unflushed = 0;
        Ok(())
    }
}

/// Read a JSONL event file, skipping any line that is not a whole event (a torn last line after
/// an unclean shutdown).
pub fn read_events(path: &Path) -> Result<Vec<FeedEvent>> {
    let file = File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<FeedEvent>(&line) {
            Ok(event) => out.push(event),
            Err(error) => tracing::warn!(%error, "skipping an unreadable event log line"),
        }
    }
    Ok(out)
}

/// `YYYYMMDD` in UTC for a `Date.now()`-style millisecond timestamp.
///
/// Hand-rolled rather than pulled from `chrono`: the service needs exactly this one calendar
/// conversion, and the civil-from-days algorithm is short and exact.
pub fn utc_day(wall_ms: u64) -> String {
    let days = (wall_ms / 86_400_000) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}{month:02}{day:02}")
}

/// Howard Hinnant's `civil_from_days`, for days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// `Date.now()`.
pub fn now_wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400_000;

    #[test]
    fn the_civil_calendar_conversion_matches_known_dates() {
        assert_eq!(utc_day(0), "19700101");
        assert_eq!(utc_day(DAY - 1), "19700101");
        assert_eq!(utc_day(DAY), "19700102");
        // 2026-09-15T00:00:00Z is 20,711 days after the epoch.
        assert_eq!(utc_day(20_711 * DAY), "20260915");
        assert_eq!(utc_day(20_711 * DAY + DAY - 1), "20260915");
        assert_eq!(utc_day(20_711 * DAY + DAY), "20260916");
        // A leap day: 2024-02-29 is 19,782 days after the epoch.
        assert_eq!(utc_day(19_782 * DAY), "20240229");
        assert_eq!(utc_day(19_783 * DAY), "20240301");
    }

    #[test]
    fn appended_events_land_in_the_file_the_ring_and_the_pending_tail() {
        let dir = tempfile::tempdir().unwrap();
        let ring = EventRing::new();
        let mut log = EventLog::open(dir.path(), ring.clone()).unwrap();

        let first = log.append(1_000, 10.0, NewEvent::new(FeedEventKind::System, "started"));
        let second = log.append(
            1_100,
            20.0,
            NewEvent::new(FeedEventKind::Sugar, "alex fed the fly sugar").by("alex").value(400.0),
        );
        assert_eq!((first.id, second.id), (1, 2));
        log.flush().unwrap();

        let pending = log.take_pending();
        assert_eq!(pending.len(), 2);
        assert!(log.take_pending().is_empty(), "the tail is drained, not resent");

        let on_disk = read_events(&dir.path().join("events.jsonl")).unwrap();
        assert_eq!(on_disk, vec![first.clone(), second.clone()]);
        assert_eq!(ring.page(0, 100), on_disk);
        assert_eq!(ring.page(1, 100), vec![second]);
        assert_eq!(ring.page(0, 1), vec![first]);
        assert!(ring.page(99, 100).is_empty());
    }

    #[test]
    fn reopening_continues_the_id_sequence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut log = EventLog::open(dir.path(), EventRing::new()).unwrap();
            log.append(1, 0.0, NewEvent::new(FeedEventKind::System, "one"));
            log.append(2, 0.0, NewEvent::new(FeedEventKind::System, "two"));
            log.flush().unwrap();
        }
        let ring = EventRing::new();
        let mut log = EventLog::open(dir.path(), ring.clone()).unwrap();
        assert_eq!(log.next_id(), 3);
        assert_eq!(ring.len(), 2, "the ring is seeded from the file");
        log.resume_from(41);
        assert_eq!(log.next_id(), 42, "a checkpoint can push the sequence forward");
        log.resume_from(3);
        assert_eq!(log.next_id(), 42, "but never backwards");
    }

    #[test]
    fn the_first_append_of_a_new_day_archives_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = EventLog::open(dir.path(), EventRing::new()).unwrap();
        log.day = "20260914".to_string();
        log.append(20_710 * DAY, 0.0, NewEvent::new(FeedEventKind::System, "yesterday"));
        log.append(20_711 * DAY, 1.0, NewEvent::new(FeedEventKind::System, "today"));
        log.flush().unwrap();

        let archived = read_events(&dir.path().join("events-20260914.jsonl")).unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].label, "yesterday");
        let live = read_events(&dir.path().join("events.jsonl")).unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].label, "today");
        assert_eq!(log.day, "20260915");
    }

    #[test]
    fn a_torn_final_line_is_skipped_rather_than_failing_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            "{\"id\":1,\"wallMs\":1,\"brainMs\":0,\"kind\":\"system\",\"label\":\"ok\"}\n{\"id\":2,\"wa",
        )
        .unwrap();
        let events = read_events(&path).unwrap();
        assert_eq!(events.len(), 1);
        let log = EventLog::open(dir.path(), EventRing::new()).unwrap();
        assert_eq!(log.next_id(), 2);
    }

    #[test]
    fn the_ring_is_bounded() {
        let ring = EventRing::new();
        for id in 1..=(RING_CAPACITY as u64 + 10) {
            ring.push(FeedEvent {
                id,
                wall_ms: id,
                brain_ms: 0.0,
                kind: FeedEventKind::System,
                label: String::new(),
                value: None,
                reward_kind: None,
                by: None,
            });
        }
        assert_eq!(ring.len(), RING_CAPACITY);
        assert_eq!(ring.last_id(), RING_CAPACITY as u64 + 10);
        assert_eq!(ring.page(0, 1)[0].id, 11, "the oldest retained, not id 1");
    }
}
