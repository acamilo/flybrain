//! Following the live service's `FLY_TRACE_DIR`: one file per flysim process, named
//! `trace-<start wall ms>-<pid>.jsonl`, so the names sort in start order. A file is a *segment*:
//! the process that wrote it restored one checkpoint (its startup save names it), ran, and ended
//! where the next file begins.
//!
//! The live recorder buffers about a megabyte, so lines arrive in bursts some seconds late and the
//! last line of a file may be incomplete until the next burst. A reader therefore hands out only
//! complete lines. A file has ended when it has no more complete lines *and* a newer file exists:
//! systemd starts the next process only after the previous one exited, and a process flushes its
//! trace on the way out. A process killed hard loses its unflushed tail; those transitions are
//! simply not compared.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// The trace files in `dir`, in start order.
pub fn trace_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("trace-") && n.ends_with(".jsonl"))
        })
        .collect();
    files.sort();
    Ok(files)
}

/// The first trace file in `dir` that sorts after `after` (or the first of all).
pub fn next_file(dir: &Path, after: Option<&Path>) -> std::io::Result<Option<PathBuf>> {
    Ok(trace_files(dir)?
        .into_iter()
        .find(|path| after.is_none_or(|after| path.file_name() > after.file_name())))
}

/// Whether a file newer than `path` exists in its directory.
pub fn superseded(path: &Path) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    matches!(next_file(dir, Some(path)), Ok(Some(_)))
}

/// A reader of one growing file that returns complete lines only.
pub struct Follower {
    path: PathBuf,
    reader: BufReader<File>,
    partial: Vec<u8>,
    pub lines: u64,
    /// Bytes of complete lines handed out.
    pub consumed: u64,
}

impl Follower {
    pub fn open(path: &Path) -> std::io::Result<Follower> {
        Ok(Follower {
            path: path.to_owned(),
            reader: BufReader::with_capacity(1 << 20, File::open(path)?),
            partial: Vec::new(),
            lines: 0,
            consumed: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The next complete line, if one has been written; `None` when the reader is at the end of
    /// what exists so far (an incomplete last line is kept for the next call).
    pub fn next_line(&mut self) -> std::io::Result<Option<String>> {
        let read = self.reader.read_until(b'\n', &mut self.partial)?;
        if read == 0 || self.partial.last() != Some(&b'\n') {
            return Ok(None);
        }
        self.consumed += self.partial.len() as u64;
        self.partial.pop();
        let line = String::from_utf8(std::mem::take(&mut self.partial))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.lines += 1;
        Ok(Some(line))
    }

    /// About how many complete lines are written but not yet read: the bytes beyond the reader
    /// over the mean line length so far.
    pub fn backlog_lines(&self) -> u64 {
        let Ok(len) = std::fs::metadata(&self.path).map(|m| m.len()) else {
            return 0;
        };
        if self.lines == 0 {
            return 0;
        }
        let mean = (self.consumed / self.lines).max(1);
        len.saturating_sub(self.consumed) / mean
    }

    /// Bytes of an incomplete last line held back.
    pub fn pending_bytes(&self) -> usize {
        self.partial.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn complete_lines_only_and_files_in_start_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("trace-0000000000100-7.jsonl");
        let b = dir.path().join("trace-0000000000200-9.jsonl");
        std::fs::write(dir.path().join("other.txt"), b"x").unwrap();
        let mut file = File::create(&a).unwrap();
        file.write_all(b"{\"format\":1}\n{\"half").unwrap();
        file.flush().unwrap();
        let mut follower = Follower::open(&a).unwrap();
        assert_eq!(
            follower.next_line().unwrap().as_deref(),
            Some("{\"format\":1}")
        );
        assert_eq!(follower.next_line().unwrap(), None);
        assert_eq!(follower.pending_bytes(), 6);
        assert!(!superseded(&a));
        file.write_all(b"\":2}\n").unwrap();
        file.flush().unwrap();
        assert_eq!(
            follower.next_line().unwrap().as_deref(),
            Some("{\"half\":2}")
        );
        assert_eq!(follower.next_line().unwrap(), None);
        std::fs::write(&b, b"").unwrap();
        assert!(superseded(&a));
        assert!(!superseded(&b));
        assert_eq!(trace_files(dir.path()).unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(next_file(dir.path(), None).unwrap(), Some(a.clone()));
        assert_eq!(next_file(dir.path(), Some(&a)).unwrap(), Some(b.clone()));
        assert_eq!(next_file(dir.path(), Some(&b)).unwrap(), None);
    }
}
