//! The release a verdict vouches for: the resolved directory of the running binary and the
//! SHA-256 of the release binaries in it, by name. `fly-shadow run` writes them into the verdict's
//! `candidate`; `fly-shadow check` recomputes them for what `/opt/fly/current` resolves to; the
//! remote shadow's relay and ingest (SHADOW-02) compare them at the handshake, so a box can only
//! shadow the exact release the container runs, installed at the same path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The release binaries a verdict vouches for: SERVE-01's service (`flysim-session`, what CUT-01
/// switches to), its worker program, the shadow itself, the legacy service and the edge.
pub const RELEASE_BINARIES: [&str; 5] = [
    "flysim-session",
    "fly-session",
    "fly-shadow",
    "flysim",
    "fly-edge",
];

/// SHA-256 of a file, lowercase hex.
pub fn file_sha256(path: &Path) -> std::io::Result<String> {
    Ok(super::sha256_hex(&std::fs::read(path)?))
}

/// SHA-256 of the release binaries in `dir`, by name (the ones present).
pub fn binaries_in(dir: &Path) -> std::io::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for name in RELEASE_BINARIES {
        let path = dir.join(name);
        if path.is_file() {
            out.insert(name.to_owned(), file_sha256(&path)?);
        }
    }
    Ok(out)
}

/// The resolved directory of the running binary: the release it runs from.
pub fn this_release() -> PathBuf {
    std::env::current_exe()
        .map(|exe| this_release_of(&exe))
        .unwrap_or_default()
}

/// The resolved directory of the binary at `exe`.
pub fn this_release_of(exe: &Path) -> PathBuf {
    exe.canonicalize()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned))
        .unwrap_or_default()
}
