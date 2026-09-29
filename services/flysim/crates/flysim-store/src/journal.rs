//! The sugar journal: every admitted audience input, stamped with the frame it was applied before.
//!
//! `sugar-journal.jsonl` in `[paths] hot_dir`, one JSON object per line, append-only. It is not
//! part of a checkpoint and nothing reads it back into the fly: it is the record a shadow run
//! (the session framework's CUT-01, `docs/design/session-framework/legacy-gameboy-v1.md` section
//! 15) replays audience input from. The legacy loop applies an admitted sugar at once, in the
//! command drain at the top of a frame, so an input is fully placed by the transition it precedes:
//!
//! ```json
//! {"frame":"6465126","brainMs":108246189,"kind":"sugar","durationMs":400,"by":"viewer","source":"twitch","eventId":81234,"wallMs":1790000000000}
//! ```
//!
//! - `frame` is the frame counter when the input was applied, which is the `step` of the next
//!   transition in the frame trace (`crate::trace`): replay applies it before that transition's
//!   ticks. A restore carries the frame counter, so stamps continue across restarts.
//! - `kind` is `sugar` (a `reward-pulse` stimulation of `durationMs`, after the admission rules
//!   and the clamp) or `reward` (an operator's `POST /reward`, one reinforcement of `value`).
//! - `brainMs` cross-checks the stamp; `eventId` joins the event log; `wallMs` is for people.
//!
//! Refused requests are not journalled: admission is wall-clock policy, and a replay applies what
//! was admitted. A write that fails is a warning, never a refusal: the input has already reached
//! the fly.
//!
//! **Boot header, rotation and reset (STATE-02, before SHADOW-01).** Three things a replay needs
//! that the first version left open:
//!
//! - *Boot header.* Every process writes one header line when it starts, before any input:
//!
//!   ```json
//!   {"journal":"flysim-sugar-journal-v1","boot":{"runtime":"flysim","startFrame":"6465126","brainMs":108246189,"origin":"hot latest generation 230739","generation":230739,"compatibility":"…","wallMs":1790000000000}}
//!   ```
//!
//!   A restore splits a run into processes, and a restored process starts from a checkpoint that
//!   can be older than the last input the previous process journalled (a hot copy is up to five
//!   seconds old, a milestone reset much more). The header is where a reader cuts the journal
//!   into per-process segments ([`read_segments`]) and learns which checkpoint each one replays
//!   from. It has no `frame` member, so [`read`], which keeps input lines only, skips it.
//! - *Rotation.* The journal lives on tmpfs. When the next line would take it past
//!   [`MAX_BYTES`], the file becomes `sugar-journal-1.jsonl` (older ones shift up, the oldest
//!   beyond [`KEEP_ROTATIONS`] is removed) and a fresh file starts with a `continues` line that
//!   repeats the boot header, so every file can be read on its own.
//! - *Reset.* [`clear`] removes the journal and its rotations. `flysim --reset-to-milestone`
//!   calls it (after copying the stores aside), because a reset rewinds the frame counter to the
//!   rung's: inputs stamped with the abandoned run's frames would otherwise sit in front of the
//!   new run's and read as if they were in its future.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The journal's file name inside the hot directory.
pub const FILE_NAME: &str = "sugar-journal.jsonl";

/// The format name in every header line.
pub const FORMAT: &str = "flysim-sugar-journal-v1";

/// The size a journal file may reach before it is rotated. Sugar is rate-limited per minute, so
/// this is days of input; the bound exists because the hot directory is tmpfs.
pub const MAX_BYTES: u64 = 4 << 20;

/// Rotated files kept beside the live one: `sugar-journal-1.jsonl` (newest) and up.
pub const KEEP_ROTATIONS: usize = 3;

/// What was applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    Sugar { duration_ms: f64 },
    Reward { value: f64 },
}

/// One journal line.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<'a> {
    pub frame: u64,
    pub brain_ms: f64,
    pub input: Input,
    pub by: &'a str,
    pub source: &'a str,
    pub event_id: u64,
    pub wall_ms: u64,
}

impl Entry<'_> {
    pub fn to_json(&self) -> Value {
        let mut line = json!({ "frame": self.frame.to_string(), "brainMs": self.brain_ms });
        let map = line.as_object_mut().expect("an object");
        match self.input {
            Input::Sugar { duration_ms } => {
                map.insert("kind".into(), "sugar".into());
                map.insert("durationMs".into(), json!(duration_ms));
            }
            Input::Reward { value } => {
                map.insert("kind".into(), "reward".into());
                map.insert("value".into(), json!(value));
            }
        }
        map.insert("by".into(), self.by.into());
        map.insert("source".into(), self.source.into());
        map.insert("eventId".into(), self.event_id.into());
        map.insert("wallMs".into(), self.wall_ms.into());
        line
    }
}

/// The header a process writes when it starts: where its inputs begin.
#[derive(Debug, Clone, PartialEq)]
pub struct BootHeader {
    /// `flysim` for the legacy loop, `fly-session` for the session runtime.
    pub runtime: String,
    /// The frame counter the process starts at: its first input can be stamped no earlier.
    pub start_frame: u64,
    pub brain_ms: f64,
    /// `fresh start`, or the restore candidate's origin (`hot latest generation 7`, ...).
    pub origin: String,
    /// The generation restored from, if the candidate names one.
    pub generation: Option<u64>,
    pub compatibility: String,
    pub wall_ms: u64,
}

impl BootHeader {
    pub fn to_json(&self) -> Value {
        json!({
            "runtime": self.runtime,
            "startFrame": self.start_frame.to_string(),
            "brainMs": self.brain_ms,
            "origin": self.origin,
            "generation": self.generation,
            "compatibility": self.compatibility,
            "wallMs": self.wall_ms,
        })
    }
}

/// The append-only journal, rotated at [`MAX_BYTES`].
pub struct SugarJournal {
    dir: PathBuf,
    path: PathBuf,
    file: Option<File>,
    /// Bytes in the live file, as far as this process knows.
    bytes: u64,
    max_bytes: u64,
    keep: usize,
    /// The boot header this process wrote, repeated at the top of every rotated-in file.
    boot: Option<Value>,
}

impl SugarJournal {
    pub fn new(hot_dir: &Path) -> Self {
        Self::with_limits(hot_dir, MAX_BYTES, KEEP_ROTATIONS)
    }

    pub fn with_limits(hot_dir: &Path, max_bytes: u64, keep: usize) -> Self {
        Self {
            dir: hot_dir.to_path_buf(),
            path: hot_dir.join(FILE_NAME),
            file: None,
            bytes: 0,
            max_bytes: max_bytes.max(1),
            keep,
            boot: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write this process's boot header. Called once, when the process knows where it starts
    /// (after its restore or its warm-up) and before it admits anything.
    pub fn boot(&mut self, header: &BootHeader) {
        let boot = header.to_json();
        self.append(&json!({ "journal": FORMAT, "boot": boot.clone() }));
        // Set after the line is written: a rotation that line itself triggers must not open
        // the new file with a `continues` naming a boot that is not in the journal yet.
        self.boot = Some(boot);
    }

    /// Append one input line.
    pub fn record(&mut self, entry: &Entry<'_>) {
        self.append(&entry.to_json());
    }

    /// One line, one `write` of the whole line, so a crash leaves at most a torn last line,
    /// which a reader skips and the next open terminates.
    fn append(&mut self, value: &Value) {
        let mut line = value.to_string();
        line.push('\n');
        if self.file.is_none() && !self.open() {
            return;
        }
        if self.bytes > 0 && self.bytes + line.len() as u64 > self.max_bytes {
            self.rotate();
            if !self.open() {
                return;
            }
        }
        if let Some(file) = self.file.as_mut() {
            match file.write_all(line.as_bytes()) {
                Ok(()) => self.bytes += line.len() as u64,
                Err(error) => {
                    tracing::warn!(%error, path = %self.path.display(), "could not append to the sugar journal");
                    // Reopen on the next input rather than write through a broken handle.
                    self.file = None;
                }
            }
        }
    }

    /// Open the live file for appending. One whose last line is torn gets its newline, so the
    /// next line is whole; one already at the limit is rotated by the caller's next append.
    fn open(&mut self) -> bool {
        let file = OpenOptions::new().create(true).read(true).append(true).open(&self.path);
        let mut file = match file {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(%error, path = %self.path.display(), "could not open the sugar journal");
                return false;
            }
        };
        let mut length = file.metadata().map(|m| m.len()).unwrap_or(0);
        if length > 0 && !ends_with_newline(&mut file, length) && file.write_all(b"\n").is_ok() {
            length += 1;
        }
        self.bytes = length;
        let fresh = length == 0;
        self.file = Some(file);
        if fresh && let Some(boot) = self.boot.clone() {
            // A file this process starts after a rotation says whose inputs follow.
            self.append(&json!({ "journal": FORMAT, "continues": boot }));
        }
        true
    }

    fn rotate(&mut self) {
        self.file = None;
        self.shift_rotations();
    }

    /// `sugar-journal.jsonl` -> `-1`, `-1` -> `-2`, ..., dropping what falls past `keep`.
    fn shift_rotations(&mut self) {
        if self.keep == 0 {
            let _ = std::fs::remove_file(&self.path);
            return;
        }
        let _ = std::fs::remove_file(rotation_path(&self.dir, self.keep));
        for index in (1..self.keep).rev() {
            let from = rotation_path(&self.dir, index);
            if from.exists() {
                let _ = std::fs::rename(&from, rotation_path(&self.dir, index + 1));
            }
        }
        if let Err(error) = std::fs::rename(&self.path, rotation_path(&self.dir, 1)) {
            tracing::warn!(%error, path = %self.path.display(), "could not rotate the sugar journal");
        }
    }
}

fn ends_with_newline(file: &mut File, length: u64) -> bool {
    let mut last = [0u8; 1];
    file.seek(SeekFrom::Start(length - 1))
        .and_then(|_| file.read_exact(&mut last))
        .map(|()| last[0] == b'\n')
        .unwrap_or(true)
}

/// `sugar-journal-<index>.jsonl`: index 1 is the newest rotation.
pub fn rotation_path(hot_dir: &Path, index: usize) -> PathBuf {
    hot_dir.join(format!("sugar-journal-{index}.jsonl"))
}

/// Is `name` the journal or one of its rotations?
pub fn is_journal_file(name: &str) -> bool {
    name == FILE_NAME
        || name
            .strip_prefix("sugar-journal-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
            .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
}

/// Remove the journal and every rotation from `hot_dir`: a reset starts a new record. Returns
/// how many files went. A missing directory is zero files.
pub fn clear(hot_dir: &Path) -> std::io::Result<usize> {
    if !hot_dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in std::fs::read_dir(hot_dir)?.flatten() {
        if is_journal_file(&entry.file_name().to_string_lossy()) {
            std::fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Read a journal back: every whole input line, in order. A torn or unreadable line is skipped,
/// and so is every header.
pub fn read(path: &Path) -> std::io::Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("frame").is_some())
        .collect())
}

/// One process's inputs: its boot header (absent for lines written before headers existed) and
/// the inputs it admitted, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub boot: Option<Value>,
    pub inputs: Vec<Value>,
}

/// The whole journal of `hot_dir`, oldest rotation first, cut into per-process segments at each
/// boot header. A `continues` line keeps the segment it names open across a rotation.
pub fn read_segments(hot_dir: &Path) -> std::io::Result<Vec<Segment>> {
    let mut files: Vec<(usize, PathBuf)> = Vec::new();
    if hot_dir.is_dir() {
        for entry in std::fs::read_dir(hot_dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(index) = name
                .strip_prefix("sugar-journal-")
                .and_then(|rest| rest.strip_suffix(".jsonl"))
                .and_then(|index| index.parse::<usize>().ok())
            {
                files.push((index, entry.path()));
            }
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    let live = hot_dir.join(FILE_NAME);
    if live.exists() {
        files.push((0, live));
    }
    let mut segments: Vec<Segment> = Vec::new();
    for (_, path) in files {
        let text = std::fs::read_to_string(&path)?;
        for value in text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()) {
            if let Some(boot) = value.get("boot") {
                segments.push(Segment { boot: Some(boot.clone()), inputs: Vec::new() });
            } else if let Some(boot) = value.get("continues") {
                let open = segments.last().is_some_and(|s| s.boot.as_ref() == Some(boot));
                if !open {
                    segments.push(Segment { boot: Some(boot.clone()), inputs: Vec::new() });
                }
            } else if value.get("frame").is_some() {
                if segments.is_empty() {
                    segments.push(Segment { boot: None, inputs: Vec::new() });
                }
                segments.last_mut().expect("a segment").inputs.push(value);
            }
        }
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_are_appended_with_their_frame_and_survive_a_reopen() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut journal = SugarJournal::new(dir.path());
        assert!(
            !journal.path().exists(),
            "nothing is written before an input"
        );
        let sugar = Entry {
            frame: 42,
            brain_ms: 703.0,
            input: Input::Sugar { duration_ms: 400.0 },
            by: "viewer",
            source: "test",
            event_id: 7,
            wall_ms: 1,
        };
        journal.record(&sugar);
        drop(journal);
        let mut journal = SugarJournal::new(dir.path());
        journal.record(&Entry {
            frame: 43,
            input: Input::Reward { value: 0.5 },
            event_id: 8,
            ..sugar
        });
        // A torn tail from a crash is skipped, not fatal.
        std::fs::OpenOptions::new()
            .append(true)
            .open(journal.path())
            .and_then(|mut file| file.write_all(b"{\"frame\":\"44\",\"kin"))
            .expect("appending a torn line");
        let lines = read(journal.path()).expect("the journal reads back");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["frame"], "42");
        assert_eq!(lines[0]["kind"], "sugar");
        assert_eq!(lines[0]["durationMs"], 400.0);
        assert_eq!(lines[1]["frame"], "43");
        assert_eq!(lines[1]["kind"], "reward");
        assert_eq!(lines[1]["value"], 0.5);
        assert_eq!(lines[1]["eventId"], 8);
    }

    fn header(frame: u64, origin: &str) -> BootHeader {
        BootHeader {
            runtime: "flysim".to_string(),
            start_frame: frame,
            brain_ms: frame as f64 * 16.74,
            origin: origin.to_string(),
            generation: Some(frame),
            compatibility: "kernel/adapter/fingerprint".to_string(),
            wall_ms: 1,
        }
    }

    fn sugar(frame: u64, event_id: u64) -> Entry<'static> {
        Entry {
            frame,
            brain_ms: frame as f64,
            input: Input::Sugar { duration_ms: 300.0 },
            by: "viewer",
            source: "test",
            event_id,
            wall_ms: 1,
        }
    }

    #[test]
    fn each_process_writes_a_boot_header_that_cuts_the_journal_into_segments() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = SugarJournal::new(dir.path());
        journal.boot(&header(100, "fresh start"));
        journal.record(&sugar(120, 1));
        journal.record(&sugar(130, 2));
        drop(journal);
        // A crash tore the last line; the next process terminates it before its header.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.path().join(FILE_NAME))
            .and_then(|mut file| file.write_all(b"{\"frame\":\"13"))
            .unwrap();
        // The restart restored an older hot copy: its inputs start below the torn one's frame.
        let mut journal = SugarJournal::new(dir.path());
        journal.boot(&header(125, "hot latest generation 125"));
        journal.record(&sugar(126, 3));
        let segments = read_segments(dir.path()).unwrap();
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].boot.as_ref().unwrap()["origin"], "fresh start");
        assert_eq!(segments[0].inputs.len(), 2);
        assert_eq!(segments[1].boot.as_ref().unwrap()["startFrame"], "125");
        assert_eq!(segments[1].boot.as_ref().unwrap()["generation"], 125);
        assert_eq!(segments[1].inputs.len(), 1);
        assert_eq!(segments[1].inputs[0]["frame"], "126");
        // The input-only reader skips headers and the torn line, as before.
        let inputs = read(journal.path()).unwrap();
        assert_eq!(inputs.len(), 3);
    }

    #[test]
    fn the_journal_rotates_at_its_limit_and_keeps_a_bounded_number_of_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = SugarJournal::with_limits(dir.path(), 600, 2);
        journal.boot(&header(1, "fresh start"));
        for event in 0..40 {
            journal.record(&sugar(10 + event, event));
        }
        for path in [dir.path().join(FILE_NAME), rotation_path(dir.path(), 1), rotation_path(dir.path(), 2)] {
            let length = std::fs::metadata(&path).unwrap().len();
            assert!(length <= 600, "{} is {length} bytes", path.display());
        }
        assert!(!rotation_path(dir.path(), 3).exists(), "only two rotations are kept");
        // Every file says whose inputs it holds, and the segments join across the rotations.
        let live = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        let first: Value = serde_json::from_str(live.lines().next().unwrap()).unwrap();
        assert_eq!(first["journal"], FORMAT);
        assert_eq!(first["continues"]["origin"], "fresh start", "{live}");
        let segments = read_segments(dir.path()).unwrap();
        assert_eq!(segments.len(), 1, "one process, one segment: {segments:?}");
        let frames: Vec<u64> = segments[0]
            .inputs
            .iter()
            .map(|v| v["frame"].as_str().unwrap().parse().unwrap())
            .collect();
        assert!(frames.windows(2).all(|w| w[1] == w[0] + 1), "in order, no gap inside the kept files");
        assert_eq!(*frames.last().unwrap(), 49);
    }

    #[test]
    fn clear_removes_the_journal_and_its_rotations_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut journal = SugarJournal::with_limits(dir.path(), 300, 3);
        journal.boot(&header(1, "fresh start"));
        for event in 0..10 {
            journal.record(&sugar(10 + event, event));
        }
        std::fs::write(dir.path().join("manifest.json"), "{}").unwrap();
        std::fs::write(dir.path().join("sugar-journal-x.jsonl"), "").unwrap();
        let removed = clear(dir.path()).unwrap();
        assert!(removed >= 2, "{removed}");
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        let mut left = left;
        left.sort();
        assert_eq!(left, vec!["manifest.json", "sugar-journal-x.jsonl"]);
        assert_eq!(clear(&dir.path().join("missing")).unwrap(), 0);
        assert!(read_segments(dir.path()).unwrap().is_empty());
    }
}
