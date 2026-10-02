//! Whether this process may use the GPU for the LIF tick, and the record of why it may not.
//!
//! A GPU failure (a batch, a sync or an attach) is final for the process: [`record_fallback`]
//! marks the GPU unusable and every backend chosen afterwards is the CPU, so a dead or flaky
//! device is never re-attached in a loop. A restart is a new process and tries the device again;
//! if that attach fails it is a fallback too, not a crash. Checkpoints are backend-neutral, so
//! the CPU continues from the same state.
//!
//! The state is per process, and a worker that runs as a child process starts with none. The
//! launcher therefore points its children at a marker file ([`MARKER_ENV`]): each fallback
//! appends a line to it, and the file (not the child's memory) is then the record, shared by the
//! launcher's later workers. The launcher empties the file when it starts, which is what ends
//! the fallback with the process that owns it.
//!
//! [`render_prometheus`] and [`write_textfile`] expose it as `fly_brain_backend{backend=..}` and
//! `fly_brain_backend_fallbacks_total` for the watchdog.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::LifBackend;

/// The marker file's path, set by the launcher for its worker processes.
pub const MARKER_ENV: &str = "FLY_LIF_GPU_MARKER";

/// Where [`write_textfile`] puts the metrics, if set: a node_exporter textfile-collector file.
pub const PROM_ENV: &str = "FLY_BRAIN_BACKEND_PROM";

/// The log line's stable prefix: what `fly-watchdog` and `fly-check` grep for.
pub const LOG_PREFIX: &str = "LIF_GPU_FALLBACK";

static FALLBACKS: AtomicU64 = AtomicU64::new(0);
static LAST_REASON: Mutex<Option<String>> = Mutex::new(None);

fn marker() -> Option<PathBuf> {
    std::env::var_os(MARKER_ENV)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn marker_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Empties the marker: the launcher calls this when it starts, so a fallback lasts as long as the
/// process that owns the workers and not across a restart.
pub fn reset_marker(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Marks the GPU unusable for this process lifetime, counts the fallback and logs it as one
/// `LIF_GPU_FALLBACK` line on stderr (the journal).
pub fn record_fallback(reason: &str) {
    let reason = reason.replace(['\n', '\r'], " ");
    FALLBACKS.fetch_add(1, Ordering::SeqCst);
    *LAST_REASON.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.clone());
    if let Some(path) = marker() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "{reason}");
        }
    }
    eprintln!("{LOG_PREFIX}: the GPU is unusable for this process, the LIF tick runs on the CPU from here on: {reason}");
}

/// True once any fallback was recorded, here or (with a marker) by a sibling worker.
pub fn gpu_unusable() -> bool {
    fallbacks() > 0
}

/// Fallbacks recorded: the marker's lines when there is a marker, else this process's count.
pub fn fallbacks() -> u64 {
    match marker() {
        Some(path) => marker_lines(&path).len() as u64,
        None => FALLBACKS.load(Ordering::SeqCst),
    }
}

/// The fallbacks recorded in the marker file at `path` (what a launcher's worker processes wrote).
pub fn marker_fallbacks(path: &Path) -> u64 {
    marker_lines(path).len() as u64
}

/// The latest fallback's reason.
pub fn last_reason() -> Option<String> {
    match marker() {
        Some(path) => marker_lines(&path).pop(),
        None => LAST_REASON.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    }
}

/// Clears this process's record (tests only; a marker is cleared with [`reset_marker`]).
#[doc(hidden)]
pub fn reset_for_tests() {
    FALLBACKS.store(0, Ordering::SeqCst);
    *LAST_REASON.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Prometheus text: which backend runs (`1` on exactly one of the two labels), which one was
/// asked for, and the fallbacks so far.
pub fn render_prometheus(active: LifBackend, wanted: LifBackend) -> String {
    let on = |backend: &str, is: bool| {
        format!("fly_brain_backend{{backend=\"{backend}\"}} {}\n", u8::from(is))
    };
    let cuda = matches!(active, LifBackend::Cuda { .. });
    let mut out = String::new();
    out.push_str("# HELP fly_brain_backend 1 for the hardware the LIF tick runs on, 0 for the other. cpu while FLY_LIF_CUDA=1 asks for cuda means the GPU fell back.\n# TYPE fly_brain_backend gauge\n");
    out.push_str(&on("cpu", !cuda));
    out.push_str(&on("cuda", cuda));
    out.push_str(&format!(
        "# HELP fly_brain_backend_wanted 1 when FLY_LIF_CUDA=1 asks for the GPU.\n# TYPE fly_brain_backend_wanted gauge\nfly_brain_backend_wanted {}\n",
        u8::from(matches!(wanted, LifBackend::Cuda { .. }))
    ));
    out.push_str(&format!(
        "# HELP fly_brain_backend_fallbacks_total GPU failures that moved the LIF tick to the CPU (a failed batch or sync, or an attach that failed) since the process started.\n# TYPE fly_brain_backend_fallbacks_total counter\nfly_brain_backend_fallbacks_total {}\n",
        fallbacks()
    ));
    out
}

/// Writes [`render_prometheus`] to [`PROM_ENV`] (temp file and rename), if set. A failure is
/// reported on stderr and otherwise ignored: a metric must not stop the brain.
pub fn write_textfile(active: LifBackend, wanted: LifBackend) {
    let Some(path) = std::env::var_os(PROM_ENV).filter(|p| !p.is_empty()) else {
        return;
    };
    let path = PathBuf::from(path);
    let tmp = path.with_extension(format!("prom.{}", std::process::id()));
    let text = render_prometheus(active, wanted);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let written = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        eprintln!("lif backend metric: cannot write {}: {error}", path.display());
    }
}
