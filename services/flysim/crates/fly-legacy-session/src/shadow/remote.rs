//! SHADOW-02: the shadow run on a build box instead of the release container. This module is the
//! wire protocol between the two halves; [`super::relay`] is the container's half and
//! [`super::ingest`] the box's.
//!
//! ```text
//!  release container                                   build box
//!  flysim --FLY_TRACE_DIR--> trace/  <-- consumer --+   flyshadow-remote.service: fly-shadow run
//!  stores (hot, durable), sugar journal             |     follows <root>/trace, reads <root>/hot,
//!  flyshadow.service = fly-shadow relay ---- ssh ---+--> fly-shadow ingest --root <root>
//!    pushes trace bytes, new saves, the journal     |     (the forced command of a key that
//!    writes back verdict.json, divergence.*,        |      can do nothing else) writes the mirror
//!    touches trace/consumer while the box is alive  |     and sends back acks, the shadow's
//!    and the sync is caught up                      |     liveness, verdict and divergence
//! ```
//!
//! The container opens the one connection (`ssh -T <box> ...`, the box's `authorized_keys`
//! forces `fly-shadow ingest`), so the build box holds no credential for the release container.
//! Both directions carry *frames*: one JSON header line with a `t` (kind) and a `len`, followed by
//! `len` bytes of body.
//!
//! | `t` | direction | header | body |
//! | --- | --- | --- | --- |
//! | `hello` | relay to ingest | `protocol`, `runId`, `release`, `binaries`, `env` | |
//! | `state` | ingest to relay | `runId`, `traces` (name to size), `done`, `gens` | |
//! | `trace` | relay to ingest | `name`, `offset` | bytes to append at `offset` |
//! | `ckpt` | relay to ingest | `store` (`hot`, `durable`), `gen` | the whole `<gen>.checkpoint` |
//! | `journal` | relay to ingest | `name` | the whole journal file |
//! | `journal-set` | relay to ingest | `names` (every journal file there is) | |
//! | `ping`, `bye` | relay to ingest | | |
//! | `status` | ingest to relay | `received`, `traces`, `done`, `alive`, `why`, `verdictStatus` | |
//! | `resync` | ingest to relay | `name`, `size` | |
//! | `verdict` | ingest to relay | | the shadow's `verdict.json` |
//! | `file` | ingest to relay | `name` (`divergence.json`, `divergence-*.checkpoint`) | the file |
//! | `error` | ingest to relay | `message` | |
//!
//! `received` is the count of body bytes the ingest has applied on this connection; the relay
//! compares it with what it had sent, which is how it knows the box has everything that was on
//! the container's disk a given time ago ([`super::relay`]).

use std::io::{BufRead, Read, Write};
use std::path::Path;

use serde_json::{Value, json};

/// The protocol name in `hello`.
pub const PROTOCOL: &str = "fly-shadow-relay-v1";

/// The longest header line accepted.
pub const MAX_HEADER: usize = 64 << 10;

/// The largest body accepted (a checkpoint is about 2.7 MB; trace chunks are 4 MiB at most).
pub const MAX_BODY: u64 = 64 << 20;

/// The biggest trace chunk the relay sends in one frame.
pub const TRACE_CHUNK: u64 = 4 << 20;

/// The live configuration the relay forwards to the box, so the remote shadow runs the live
/// service's composition (game, macro mode, accepted adapters, pins): exactly these names, and
/// nothing that is a path or a secret. The box's own env file supplies the paths.
pub const FORWARDED_ENV: [&str; 15] = [
    // BUS-01: the topology the box's shadow runs (`fly-shadow-run start --bus`).
    "FLY_SHADOW_MODE",
    "FLY_GAME",
    "FLYSIM_LOOP_GAME",
    "FLY_MACRO_MODE",
    "FLYSIM_MACROS_MODE",
    "FLY_ACCEPT_ADAPTERS",
    "FLY_MACRO_BLOCKED_MINUTES",
    "FLY_ROM_SHA256",
    "FLY_ROM_PLATFORMER_SHA256",
    "FLYSIM_GAME_PLATFORMER_ROM_SHA256",
    "FLY_REWARD_ADAPTER",
    "FLY_DECODER_PRESET",
    "FLYSIM_LOOP_SPEED",
    "FLYSIM_LOOP_WARMUP_MS",
    "FLYSIM_FEED_AUDIO_HZ",
];

/// A forwarded value is short and plain: no quotes, spaces or newlines reach the box's env file.
pub fn env_value_ok(value: &str) -> bool {
    value.len() <= 1024
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._,:+-/@".contains(&b))
}

/// One frame.
#[derive(Debug)]
pub struct Frame {
    pub header: Value,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn kind(&self) -> &str {
        self.header["t"].as_str().unwrap_or("")
    }
}

fn invalid(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

/// Writes one frame and flushes.
pub fn write_frame<W: Write + ?Sized>(
    out: &mut W,
    mut header: Value,
    body: &[u8],
) -> std::io::Result<()> {
    header["len"] = json!(body.len());
    let mut line = serde_json::to_vec(&header).map_err(|e| invalid(e.to_string()))?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.write_all(body)?;
    out.flush()
}

/// Reads one frame; `None` at a clean end of the stream.
pub fn read_frame<R: BufRead>(input: &mut R) -> std::io::Result<Option<Frame>> {
    let mut line = Vec::new();
    let n = input
        .by_ref()
        .take(MAX_HEADER as u64 + 1)
        .read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(invalid("a frame header that is cut or too long"));
    }
    let header: Value = serde_json::from_slice(&line).map_err(|e| invalid(e.to_string()))?;
    if !header.is_object() {
        return Err(invalid("a frame header that is not an object"));
    }
    let len = header["len"].as_u64().unwrap_or(0);
    if len > MAX_BODY {
        return Err(invalid(format!("a {len}-byte frame body")));
    }
    let mut body = vec![0u8; len as usize];
    input.read_exact(&mut body)?;
    Ok(Some(Frame { header, body }))
}

/// `trace-<13 digits>-<pid>.jsonl`, flysim's per-process trace file name.
pub fn is_trace_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix("trace-")
        .and_then(|r| r.strip_suffix(".jsonl"))
    else {
        return false;
    };
    let Some((ms, pid)) = rest.split_once('-') else {
        return false;
    };
    ms.len() == 13
        && ms.bytes().all(|b| b.is_ascii_digit())
        && !pid.is_empty()
        && pid.len() <= 10
        && pid.bytes().all(|b| b.is_ascii_digit())
}

/// `sugar-journal.jsonl` or a rotation `sugar-journal-<n>.jsonl`.
pub fn is_journal_name(name: &str) -> bool {
    name == flysim::journal::FILE_NAME
        || name
            .strip_prefix("sugar-journal-")
            .and_then(|r| r.strip_suffix(".jsonl"))
            .is_some_and(|n| !n.is_empty() && n.len() <= 4 && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What a diverged shadow leaves for the review: `divergence.json` and the two checkpoint files.
pub fn is_divergence_file(name: &str) -> bool {
    name == "divergence.json"
        || ["divergence-live-g", "divergence-shadow-g"]
            .iter()
            .filter_map(|p| name.strip_prefix(p))
            .filter_map(|r| r.strip_suffix(".checkpoint"))
            .any(|g| !g.is_empty() && g.len() <= 20 && g.bytes().all(|b| b.is_ascii_digit()))
}

/// Writes `bytes` to `path` through a temporary file and a rename.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Unix milliseconds of a file's modification time.
pub fn mtime_ms(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_bad_ones_are_refused() {
        let mut wire = Vec::new();
        write_frame(&mut wire, json!({"t": "trace", "name": "x", "offset": 3}), b"abc\n").unwrap();
        write_frame(&mut wire, json!({"t": "ping"}), b"").unwrap();
        let mut input = std::io::Cursor::new(wire);
        let a = read_frame(&mut input).unwrap().unwrap();
        assert_eq!((a.kind(), a.body.as_slice()), ("trace", b"abc\n".as_slice()));
        assert_eq!(a.header["offset"], 3);
        assert_eq!(read_frame(&mut input).unwrap().unwrap().kind(), "ping");
        assert!(read_frame(&mut input).unwrap().is_none());
        // A body shorter than its len, an oversized body, a non-object header.
        let mut cut = std::io::Cursor::new(b"{\"t\":\"x\",\"len\":10}\nabc".to_vec());
        assert!(read_frame(&mut cut).is_err());
        let mut huge = std::io::Cursor::new(format!("{{\"len\":{}}}\n", MAX_BODY + 1).into_bytes());
        assert!(read_frame(&mut huge).is_err());
        let mut list = std::io::Cursor::new(b"[1]\n".to_vec());
        assert!(read_frame(&mut list).is_err());
    }

    #[test]
    fn names_are_checked_before_they_become_paths() {
        assert!(is_trace_name("trace-1790741297310-444741.jsonl"));
        for bad in [
            "trace-179074129731-4.jsonl",
            "trace-1790741297310-.jsonl",
            "trace-1790741297310-4/../x.jsonl",
            "../trace-1790741297310-4.jsonl",
            "consumer",
        ] {
            assert!(!is_trace_name(bad), "{bad}");
        }
        assert!(is_journal_name("sugar-journal.jsonl"));
        assert!(is_journal_name("sugar-journal-2.jsonl"));
        assert!(!is_journal_name("sugar-journal-.jsonl"));
        assert!(!is_journal_name("sugar-journal-../x.jsonl"));
        assert!(is_divergence_file("divergence.json"));
        assert!(is_divergence_file("divergence-live-g42.checkpoint"));
        assert!(!is_divergence_file("divergence-live-g.checkpoint"));
        assert!(!is_divergence_file("verdict.json"));
        assert!(env_value_ok("pokered-unique8-v7,pokered-unique8-v6"));
        assert!(!env_value_ok("a b"));
        assert!(!env_value_ok("x\ny"));
        assert!(!env_value_ok("$(x)"));
    }
}
