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

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// The journal's file name inside the hot directory.
pub const FILE_NAME: &str = "sugar-journal.jsonl";

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

/// The append-only journal. Opened on the first input, so a run nobody feeds writes no file.
pub struct SugarJournal {
    path: PathBuf,
    file: Option<File>,
}

impl SugarJournal {
    pub fn new(hot_dir: &Path) -> Self {
        Self { path: hot_dir.join(FILE_NAME), file: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line. Each line is one `write` of the whole line, so a crash leaves at most a
    /// torn last line, which a reader skips.
    pub fn record(&mut self, entry: &Entry<'_>) {
        if self.file.is_none() {
            match OpenOptions::new().create(true).append(true).open(&self.path) {
                Ok(file) => self.file = Some(file),
                Err(error) => {
                    tracing::warn!(%error, path = %self.path.display(), "could not open the sugar journal");
                    return;
                }
            }
        }
        let mut line = entry.to_json().to_string();
        line.push('\n');
        if let Some(file) = self.file.as_mut()
            && let Err(error) = file.write_all(line.as_bytes())
        {
            tracing::warn!(%error, path = %self.path.display(), "could not append to the sugar journal");
            // Reopen on the next input rather than write through a broken handle.
            self.file = None;
        }
    }
}

/// Read a journal back: every whole line, in order. A torn or unreadable line is skipped.
pub fn read(path: &Path) -> std::io::Result<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("frame").is_some())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_are_appended_with_their_frame_and_survive_a_reopen() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let mut journal = SugarJournal::new(dir.path());
        assert!(!journal.path().exists(), "nothing is written before an input");
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
        journal.record(&Entry { frame: 43, input: Input::Reward { value: 0.5 }, event_id: 8, ..sugar });
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
}
