//! `fly-shadow ingest`: the build box's half of the remote shadow (SHADOW-02; the protocol is
//! [`super::remote`]). It is the forced command of the one key the release container's relay
//! holds, so it is the only thing that key can run: it reads frames on stdin, writes the mirror
//! under its root and nothing else, and answers on stdout.
//!
//! The mirror, which `flyshadow-remote.service` (`fly-shadow run`) reads exactly as the shadow on
//! the container reads the live directories:
//!
//! | Path | What |
//! | --- | --- |
//! | `<root>/trace/` | the live trace files, appended as they grow; the shadow's heartbeat `consumer` |
//! | `<root>/hot/`, `<root>/durable/` | the live saves (`<gen>.checkpoint`), bounded; the sugar journal in `hot/` |
//! | `<root>/out/` | the shadow's verdict, divergence and spool |
//! | `<root>/run-id` | the relay's run: a new one resets the mirror and restarts the shadow |
//! | `<root>/live.env` | the live composition's settings ([`super::remote::FORWARDED_ENV`]) |
//! | `<root>/ingest.json` | the trace files it created, and the ones the shadow has finished |
//!
//! Every name is checked before it becomes a path, and the release is checked at the handshake:
//! the relay's release directory and binary hashes must be this binary's own, so the box shadows
//! the exact release the container runs, installed at the same path (the verdict binds to it).
//!
//! It reports the shadow *alive* only while the shadow's heartbeat is fresh and its verdict is
//! this run's and running or passed: that is what the relay turns into the container's
//! heartbeat, the one flysim's trace needs.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::remote::{self, Frame};

/// How the ingest runs.
#[derive(Clone, Debug)]
pub struct IngestConfig {
    pub root: PathBuf,
    /// This binary's release directory and binaries ([`super::release`]).
    pub release: String,
    pub binaries: BTreeMap<String, String>,
    /// The mirrored saves are kept under this many bytes, oldest generation first.
    pub keep_checkpoint_bytes: u64,
    /// The shadow's heartbeat may be at most this old for the shadow to count as alive.
    pub alive_within: Duration,
    pub status_every: Duration,
}

impl IngestConfig {
    pub fn new(root: PathBuf, release: String, binaries: BTreeMap<String, String>) -> Self {
        IngestConfig {
            root,
            release,
            binaries,
            keep_checkpoint_bytes: 3 << 30,
            alive_within: Duration::from_secs(90),
            status_every: Duration::from_secs(2),
        }
    }
}

/// The mirror's directories.
pub struct Layout {
    pub trace: PathBuf,
    pub hot: PathBuf,
    pub durable: PathBuf,
    pub out: PathBuf,
    pub run_id: PathBuf,
    pub live_env: PathBuf,
    pub ledger: PathBuf,
}

impl Layout {
    pub fn of(root: &Path) -> Layout {
        Layout {
            trace: root.join("trace"),
            hot: root.join("hot"),
            durable: root.join("durable"),
            out: root.join("out"),
            run_id: root.join("run-id"),
            live_env: root.join("live.env"),
            ledger: root.join("ingest.json"),
        }
    }
}

/// What both threads share.
#[derive(Default)]
struct Ledger {
    run_id: String,
    /// Body bytes applied on this connection.
    received: u64,
    /// Trace files this ingest created and still has, and those the shadow has since removed
    /// (finished or skipped): a finished file is never written again.
    created: BTreeSet<String>,
    done: BTreeSet<String>,
}

impl Ledger {
    fn to_json(&self) -> Value {
        json!({"runId": self.run_id, "created": self.created, "done": self.done})
    }

    fn load(path: &Path) -> Ledger {
        let v: Value = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        let names = |key: &str| -> BTreeSet<String> {
            v[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str())
                .filter(|n| remote::is_trace_name(n))
                .map(str::to_owned)
                .collect()
        };
        Ledger {
            run_id: v["runId"].as_str().unwrap_or("").to_owned(),
            received: 0,
            created: names("created"),
            done: names("done"),
        }
    }

    fn save(&self, path: &Path) {
        if let Err(e) = remote::atomic_write(path, self.to_json().to_string().as_bytes()) {
            eprintln!("fly-shadow ingest: {}: {e}", path.display());
        }
    }
}

type Output = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(out: &Output, header: Value, body: &[u8]) -> std::io::Result<()> {
    let mut out = out.lock().unwrap_or_else(|p| p.into_inner());
    remote::write_frame(&mut **out, header, body)
}

/// The trace files in `dir` and their sizes.
fn trace_sizes(dir: &Path) -> BTreeMap<String, u64> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            remote::is_trace_name(&name).then_some((name, e.metadata().ok()?.len()))
        })
        .collect()
}

/// The generations of `<gen>.checkpoint` files in `dir`, with their sizes.
fn gens_in(dir: &Path) -> BTreeMap<u64, u64> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let generation = super::spool::generation_of(e.file_name().to_str()?)?;
            Some((generation, e.metadata().ok()?.len()))
        })
        .collect()
}

/// Removes every file in `dir` (not directories) whose name `pick` accepts.
fn remove_files(dir: &Path, pick: impl Fn(&str) -> bool) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_ok_and(|t| t.is_file()) && pick(&name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A new run: the previous run's mirror, verdict and spool go.
fn reset(layout: &Layout) {
    remove_files(&layout.trace, remote::is_trace_name);
    for dir in [&layout.hot, &layout.durable] {
        remove_files(dir, |n| {
            super::spool::generation_of(n).is_some() || remote::is_journal_name(n)
        });
    }
    remove_files(&layout.out, |n| {
        n == "verdict.json" || remote::is_divergence_file(n)
    });
    let _ = std::fs::remove_dir_all(layout.out.join("spool"));
}

/// The shadow's liveness as the relay needs it: its heartbeat fresh, and its verdict this run's
/// and running or passed. Returns `(alive, why, verdict bytes if this run's, its status)`.
fn shadow_state(
    layout: &Layout,
    run_id: &str,
    alive_within: Duration,
) -> (bool, String, Option<Vec<u8>>, String) {
    let heartbeat_age = std::fs::metadata(layout.trace.join(flysim::trace::CONSUMER_FILE))
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| {
            std::time::SystemTime::now()
                .duration_since(t)
                .unwrap_or_default()
        });
    let bytes = std::fs::read(layout.out.join("verdict.json")).ok();
    let verdict: Value = bytes
        .as_deref()
        .and_then(|b| serde_json::from_slice(b).ok())
        .unwrap_or(Value::Null);
    let status = verdict["status"].as_str().unwrap_or("").to_owned();
    if verdict["runId"].as_str() != Some(run_id) {
        return (
            false,
            "the shadow has not started this run yet".to_owned(),
            None,
            status,
        );
    }
    let alive = match heartbeat_age {
        None => (false, "the shadow has no heartbeat".to_owned()),
        Some(age) if age > alive_within => (
            false,
            format!("the shadow's heartbeat is {} s old", age.as_secs()),
        ),
        Some(_) if status == "running" || status == "pass" => (true, String::new()),
        Some(_) => (false, format!("the shadow's verdict is {status}")),
    };
    (alive.0, alive.1, bytes, status)
}

/// Serves one relay connection until it ends. `Err` for a refused handshake or a broken stream.
pub fn serve<R: BufRead>(
    mut input: R,
    output: Box<dyn Write + Send>,
    config: IngestConfig,
) -> Result<(), String> {
    let layout = Layout::of(&config.root);
    for dir in [&layout.trace, &layout.hot, &layout.durable, &layout.out] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let out: Output = Arc::new(Mutex::new(output));
    let refuse = |message: String| -> Result<(), String> {
        let _ = send(&out, json!({"t": "error", "message": message}), b"");
        Err(message)
    };
    let hello = match remote::read_frame(&mut input) {
        Ok(Some(frame)) if frame.kind() == "hello" => frame,
        Ok(Some(frame)) => return refuse(format!("expected hello, got {:?}", frame.kind())),
        Ok(None) => return Ok(()),
        Err(e) => return Err(format!("reading hello: {e}")),
    };
    let h = &hello.header;
    if h["protocol"] != remote::PROTOCOL {
        return refuse(format!(
            "protocol {} is not {}",
            h["protocol"],
            remote::PROTOCOL
        ));
    }
    if h["release"].as_str() != Some(config.release.as_str()) {
        return refuse(format!(
            "the container runs the release {}, this box {}: install the same release tarball at \
             the same path",
            h["release"], config.release
        ));
    }
    let theirs: BTreeMap<String, String> =
        serde_json::from_value(h["binaries"].clone()).unwrap_or_default();
    if theirs.is_empty() || theirs != config.binaries {
        return refuse(format!(
            "the release binaries differ (container {theirs:?}, box {:?})",
            config.binaries
        ));
    }
    let run_id = h["runId"].as_str().unwrap_or("").to_owned();
    if run_id.is_empty()
        || run_id.len() > 64
        || !run_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return refuse(format!("a bad run id {}", h["runId"]));
    }

    let mut ledger = Ledger::load(&layout.ledger);
    let current = super::read_run_id(&layout.run_id).unwrap_or_default();
    if current != run_id || ledger.run_id != run_id {
        eprintln!("fly-shadow ingest: a new run {run_id} (was {current:?}): the mirror is reset");
        reset(&layout);
        ledger = Ledger {
            run_id: run_id.clone(),
            ..Ledger::default()
        };
        ledger.save(&layout.ledger);
        let mut env = String::new();
        for (key, value) in h["env"].as_object().into_iter().flatten() {
            if let Some(value) = value.as_str()
                && remote::FORWARDED_ENV.contains(&key.as_str())
                && remote::env_value_ok(value)
            {
                env.push_str(&format!("{key}={value}\n"));
            }
        }
        remote::atomic_write(&layout.live_env, env.as_bytes())
            .map_err(|e| format!("{}: {e}", layout.live_env.display()))?;
        // Last: the run id is what restarts the shadow (it watches the file; a path unit starts
        // one that is down).
        remote::atomic_write(&layout.run_id, format!("{run_id}\n").as_bytes())
            .map_err(|e| format!("{}: {e}", layout.run_id.display()))?;
    }
    let gens: Vec<u64> = gens_in(&layout.hot)
        .into_keys()
        .chain(gens_in(&layout.durable).into_keys())
        .collect();
    send(
        &out,
        json!({"t": "state", "runId": run_id, "traces": trace_sizes(&layout.trace),
               "done": ledger.done, "gens": gens}),
        b"",
    )
    .map_err(|e| format!("writing state: {e}"))?;

    let ledger = Arc::new(Mutex::new(ledger));
    let stop = Arc::new(AtomicBool::new(false));
    let status_thread = {
        let (out, ledger, stop, config) = (
            Arc::clone(&out),
            Arc::clone(&ledger),
            Arc::clone(&stop),
            config.clone(),
        );
        std::thread::Builder::new()
            .name("fly-shadow-ingest-status".to_owned())
            .spawn(move || status_loop(&out, &ledger, &stop, &config))
            .expect("the status thread starts")
    };
    let result = apply_loop(&mut input, &out, &ledger, &layout, &config);
    stop.store(true, Ordering::Relaxed);
    let _ = status_thread.join();
    result
}

/// Every `status_every`: acks, finished files, the shadow's liveness, and its verdict and
/// divergence files when they change.
fn status_loop(out: &Output, ledger: &Mutex<Ledger>, stop: &AtomicBool, config: &IngestConfig) {
    let layout = Layout::of(&config.root);
    let mut sent_verdict: Option<Vec<u8>> = None;
    let mut sent_files: BTreeSet<String> = BTreeSet::new();
    let mut last = Instant::now() - config.status_every;
    while !stop.load(Ordering::Relaxed) {
        if last.elapsed() < config.status_every {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        last = Instant::now();
        let (traces, received, done, run_id) = {
            let mut l = ledger.lock().unwrap_or_else(|p| p.into_inner());
            // Listed under the lock: a file `apply_trace` creates (under the same lock) between a
            // listing and this point would otherwise look removed, and be marked finished.
            let traces = trace_sizes(&layout.trace);
            let gone: Vec<String> = l
                .created
                .iter()
                .filter(|n| !traces.contains_key(*n))
                .cloned()
                .collect();
            if !gone.is_empty() {
                for name in gone {
                    l.created.remove(&name);
                    l.done.insert(name);
                }
                l.save(&layout.ledger);
            }
            (traces, l.received, l.done.clone(), l.run_id.clone())
        };
        let (alive, why, verdict, status) = shadow_state(&layout, &run_id, config.alive_within);
        let mut result = send(
            out,
            json!({"t": "status", "received": received, "traces": traces, "done": done,
                   "alive": alive, "why": why, "verdictStatus": status}),
            b"",
        );
        if result.is_ok()
            && let Some(bytes) = verdict
            && sent_verdict.as_ref() != Some(&bytes)
        {
            result = send(out, json!({"t": "verdict"}), &bytes);
            sent_verdict = Some(bytes);
        }
        if result.is_ok() && status == "diverged" {
            for entry in std::fs::read_dir(&layout.out).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if remote::is_divergence_file(&name)
                    && !sent_files.contains(&name)
                    && let Ok(bytes) = std::fs::read(entry.path())
                {
                    result = send(out, json!({"t": "file", "name": name}), &bytes);
                    sent_files.insert(name);
                }
            }
        }
        if result.is_err() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

fn apply_loop<R: BufRead>(
    input: &mut R,
    out: &Output,
    ledger: &Mutex<Ledger>,
    layout: &Layout,
    config: &IngestConfig,
) -> Result<(), String> {
    let mut since_prune = 0u64;
    loop {
        let frame: Frame = match remote::read_frame(input) {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(()),
            Err(e) => return Err(format!("reading a frame: {e}")),
        };
        let bytes = frame.body.len() as u64;
        match frame.kind() {
            "trace" => apply_trace(&frame, out, ledger, layout)?,
            "ckpt" => {
                let store = match frame.header["store"].as_str() {
                    Some("hot") => &layout.hot,
                    Some("durable") => &layout.durable,
                    other => return Err(format!("a checkpoint for the store {other:?}")),
                };
                let generation = frame.header["gen"]
                    .as_u64()
                    .ok_or("a checkpoint with no generation")?;
                remote::atomic_write(
                    &store.join(format!("{generation}.checkpoint")),
                    &frame.body,
                )
                .map_err(|e| format!("writing g{generation}: {e}"))?;
                since_prune += bytes;
                if since_prune >= 64 << 20 {
                    since_prune = 0;
                    prune_saves(layout, config.keep_checkpoint_bytes);
                }
            }
            "journal" => {
                let name = frame.header["name"].as_str().unwrap_or("");
                if !remote::is_journal_name(name) {
                    return Err(format!("a journal named {name:?}"));
                }
                remote::atomic_write(&layout.hot.join(name), &frame.body)
                    .map_err(|e| format!("writing {name}: {e}"))?;
            }
            "journal-set" => {
                let keep: BTreeSet<&str> = frame.header["names"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str())
                    .collect();
                remove_files(&layout.hot, |n| {
                    remote::is_journal_name(n) && !keep.contains(n)
                });
            }
            "ping" => {}
            "bye" => return Ok(()),
            other => return Err(format!("an unknown frame {other:?}")),
        }
        ledger.lock().unwrap_or_else(|p| p.into_inner()).received += bytes;
    }
}

/// Appends a trace chunk where it belongs, or asks the relay to resend from what is here.
fn apply_trace(
    frame: &Frame,
    out: &Output,
    ledger: &Mutex<Ledger>,
    layout: &Layout,
) -> Result<(), String> {
    let name = frame.header["name"].as_str().unwrap_or("");
    if !remote::is_trace_name(name) {
        return Err(format!("a trace named {name:?}"));
    }
    let offset = frame.header["offset"]
        .as_u64()
        .ok_or("a trace chunk with no offset")?;
    let path = layout.trace.join(name);
    let size = std::fs::metadata(&path).ok().map(|m| m.len());
    {
        let mut l = ledger.lock().unwrap_or_else(|p| p.into_inner());
        // Finished by the shadow (or created here and removed since): never written again.
        if l.done.contains(name) || (size.is_none() && l.created.contains(name)) {
            return Ok(());
        }
        if size.is_none() && offset == 0 {
            std::fs::File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            l.created.insert(name.to_owned());
            l.save(&layout.ledger);
        }
    }
    let size = size.unwrap_or(0);
    if size != offset {
        return send(
            out,
            json!({"t": "resync", "name": name, "size": size}),
            b"",
        )
        .map_err(|e| format!("writing resync: {e}"));
    }
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(&frame.body)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Keeps the mirrored saves under `keep` bytes, dropping the oldest generations first.
pub fn prune_saves(layout: &Layout, keep: u64) {
    let mut all: Vec<(u64, PathBuf, u64)> = Vec::new();
    for dir in [&layout.hot, &layout.durable] {
        for (generation, len) in gens_in(dir) {
            all.push((generation, dir.join(format!("{generation}.checkpoint")), len));
        }
    }
    all.sort();
    let mut total: u64 = all.iter().map(|(_, _, len)| len).sum();
    for (_, path, len) in all {
        if total <= keep {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A writer the test can read back.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn frames(out: &Shared) -> Vec<Frame> {
        let bytes = out.0.lock().unwrap().clone();
        let mut input = std::io::Cursor::new(bytes);
        std::iter::from_fn(|| remote::read_frame(&mut input).unwrap()).collect()
    }

    fn config(root: &Path) -> IngestConfig {
        let mut c = IngestConfig::new(
            root.to_owned(),
            "/opt/fly/releases/v1".to_owned(),
            [("fly-shadow".to_owned(), "a".repeat(64))].into(),
        );
        c.status_every = Duration::from_secs(3600);
        c
    }

    fn wire(frames: &[(Value, &[u8])]) -> std::io::Cursor<Vec<u8>> {
        let mut bytes = Vec::new();
        for (header, body) in frames {
            remote::write_frame(&mut bytes, header.clone(), body).unwrap();
        }
        std::io::Cursor::new(bytes)
    }

    fn hello(release: &str) -> Value {
        json!({"t": "hello", "protocol": remote::PROTOCOL, "runId": "1790000000000",
               "release": release, "binaries": {"fly-shadow": "a".repeat(64)},
               "env": {"FLY_MACRO_MODE": "macros", "FLY_ROM": "/elsewhere", "FLY_GAME": "a b"}})
    }

    #[test]
    fn another_release_is_refused_before_anything_is_written() {
        let root = tempfile::tempdir().unwrap();
        let out = Shared::default();
        let input = wire(&[(hello("/opt/fly/releases/v2"), b"")]);
        let err = serve(input, Box::new(out.clone()), config(root.path())).unwrap_err();
        assert!(err.contains("same path"), "{err}");
        let got = frames(&out);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind(), "error");
        assert!(!root.path().join("run-id").exists());
    }

    #[test]
    fn a_run_is_mirrored_by_name_and_offset_only() {
        let root = tempfile::tempdir().unwrap();
        let layout = Layout::of(root.path());
        std::fs::create_dir_all(&layout.hot).unwrap();
        // The previous run's leftovers go when a new run says hello.
        std::fs::write(layout.hot.join("7.checkpoint"), b"old").unwrap();
        let name = "trace-1790000000001-42.jsonl";
        let out = Shared::default();
        let input = wire(&[
            (hello("/opt/fly/releases/v1"), b""),
            (json!({"t": "trace", "name": name, "offset": 0}), b""),
            (json!({"t": "trace", "name": name, "offset": 0}), b"{\"a\":1}\n"),
            // A chunk at the wrong offset is not appended: the relay is asked to resend.
            (json!({"t": "trace", "name": name, "offset": 99}), b"x"),
            (json!({"t": "ckpt", "store": "hot", "gen": 12}), b"FLYSIM01"),
            (json!({"t": "journal", "name": "sugar-journal.jsonl"}), b"{}\n"),
            (json!({"t": "bye"}), b""),
        ]);
        serve(input, Box::new(out.clone()), config(root.path())).unwrap();
        assert_eq!(std::fs::read(layout.trace.join(name)).unwrap(), b"{\"a\":1}\n");
        assert_eq!(std::fs::read(layout.hot.join("12.checkpoint")).unwrap(), b"FLYSIM01");
        assert!(!layout.hot.join("7.checkpoint").exists());
        assert_eq!(
            std::fs::read_to_string(&layout.run_id).unwrap(),
            "1790000000000\n"
        );
        // Only the forwarded, plain settings reach the box's env file.
        assert_eq!(
            std::fs::read_to_string(&layout.live_env).unwrap(),
            "FLY_MACRO_MODE=macros\n"
        );
        let got = frames(&out);
        assert_eq!(got[0].kind(), "state");
        assert!(got.iter().any(|f| f.kind() == "resync" && f.header["size"] == 8));
        // A path in a name is refused outright.
        let input = wire(&[
            (hello("/opt/fly/releases/v1"), b""),
            (json!({"t": "trace", "name": "../x.jsonl", "offset": 0}), b"x"),
        ]);
        assert!(serve(input, Box::new(Shared::default()), config(root.path())).is_err());
        assert!(!root.path().join("x.jsonl").exists());
        // The same run reconnecting keeps the mirror.
        let input = wire(&[(hello("/opt/fly/releases/v1"), b"")]);
        let out = Shared::default();
        serve(input, Box::new(out.clone()), config(root.path())).unwrap();
        assert_eq!(frames(&out)[0].header["traces"][name], 8);
    }
}
