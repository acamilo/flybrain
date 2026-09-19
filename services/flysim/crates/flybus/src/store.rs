//! The file-backed artifact store (bus-v1 section 8).
//!
//! Layout under the configured root, one directory per store incarnation:
//!
//! ```text
//! <root>/<storeId>/.flybus-store   marker: this directory belongs to a flybus store
//! <root>/<storeId>/.lock           flock()ed by the live router for its whole life
//! <root>/<storeId>/staging/a-<n>   producer-writable staging file, preallocated
//! <root>/<storeId>/sealed/a-<n>    immutable copy, mode 0444, never rewritten
//! ```
//!
//! Sealing copies staging into a fresh inode rather than renaming it, so a writable handle a
//! producer kept (or duplicated) after sealing reaches only the unlinked staging inode, never
//! the sealed bytes. A store directory whose lock nobody holds is an orphan of a stopped
//! router and is deleted when the next router starts on the same root.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::error::{BusError, ErrorCode};
use crate::wire::{Location, hex};

const MARKER: &str = ".flybus-store";
const LOCK: &str = ".lock";

pub(crate) fn staging_rel(serial: u64) -> String {
    format!("staging/a-{serial}")
}

pub(crate) fn sealed_rel(serial: u64) -> String {
    format!("sealed/a-{serial}")
}

pub(crate) enum SealFailure {
    Mismatch(String),
    Io(io::Error),
}

impl From<io::Error> for SealFailure {
    fn from(e: io::Error) -> SealFailure {
        SealFailure::Io(e)
    }
}

pub(crate) struct Store {
    dir: PathBuf,
    // Held open for the flock; closing it releases the lock.
    _lock: File,
}

fn try_lock(file: &File) -> bool {
    // SAFETY: flock on a valid, owned descriptor.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}

impl Store {
    /// Creates `<root>/<store_id>`, first deleting orphaned store directories under `root`.
    pub(crate) fn create(root: &Path, store_id: &str) -> io::Result<Store> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        clean_orphans(root)?;
        let dir = root.join(store_id);
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&dir)?;
        builder.create(dir.join("staging"))?;
        builder.create(dir.join("sealed"))?;
        File::create(dir.join(MARKER))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(LOCK))?;
        if !try_lock(&lock) {
            return Err(io::Error::other("could not lock a fresh store directory"));
        }
        Ok(Store { dir, _lock: lock })
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    /// Creates the staging file and reserves its blocks, so a full disk fails here and not in
    /// the producer's write.
    pub(crate) fn create_staging(&self, serial: u64, len: u64) -> io::Result<()> {
        let path = self.path(&staging_rel(serial));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let reserve = || -> io::Result<()> {
            if len == 0 {
                return Ok(());
            }
            let off_len = libc::off_t::try_from(len)
                .map_err(|_| io::Error::other("length overflows off_t"))?;
            // SAFETY: posix_fallocate on a valid descriptor opened for writing.
            let rc = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, off_len) };
            match rc {
                0 => Ok(()),
                libc::EOPNOTSUPP | libc::EINVAL => file.set_len(len),
                e => Err(io::Error::from_raw_os_error(e)),
            }
        };
        let result = reserve();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        result
    }

    /// Copies exactly `len` staging bytes into a fresh sealed file, checking the length and,
    /// when given, the SHA-256. On failure the partial sealed file is removed; the staging file
    /// is left for the caller either way.
    pub(crate) fn seal(
        &self,
        serial: u64,
        len: u64,
        digest: Option<&str>,
    ) -> Result<(), SealFailure> {
        let mut src = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path(&staging_rel(serial)))?;
        let dst_path = self.path(&sealed_rel(serial));
        let mut dst = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&dst_path)?;
        let result = copy_exact(&mut src, &mut dst, len, digest).and_then(|()| {
            dst.set_permissions(fs::Permissions::from_mode(0o444))?;
            Ok(())
        });
        if result.is_err() {
            let _ = fs::remove_file(&dst_path);
        }
        result
    }

    /// Unlinks a store file; a missing file is not an error.
    pub(crate) fn remove(&self, rel: &str) {
        let _ = fs::remove_file(self.path(rel));
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn read_retry(r: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn copy_exact(
    src: &mut File,
    dst: &mut File,
    len: u64,
    digest: Option<&str>,
) -> Result<(), SealFailure> {
    let mut hasher = digest.map(|_| Sha256::new());
    let mut buf = vec![0u8; 256 * 1024];
    let mut remaining = len;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = read_retry(src, &mut buf[..want])?;
        if n == 0 {
            return Err(SealFailure::Mismatch(format!(
                "staging holds {} of {len} declared bytes",
                len - remaining
            )));
        }
        if let Some(h) = hasher.as_mut() {
            h.update(&buf[..n]);
        }
        dst.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    if read_retry(src, &mut buf[..1])? != 0 {
        return Err(SealFailure::Mismatch(format!(
            "staging holds more than the declared {len} bytes"
        )));
    }
    if let (Some(h), Some(want)) = (hasher, digest) {
        let got = hex(&h.finalize());
        if got != want {
            return Err(SealFailure::Mismatch(format!(
                "digest mismatch: content hashes to {got}"
            )));
        }
    }
    Ok(())
}

/// Deletes store directories under `root` that carry the marker and whose lock is free.
/// Directories without the marker are never touched.
fn clean_orphans(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() || !path.join(MARKER).is_file() {
            continue;
        }
        let stale = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path.join(LOCK))
        {
            Ok(lock) => try_lock(&lock),
            Err(e) if e.kind() == io::ErrorKind::NotFound => true,
            Err(e) => return Err(e),
        };
        if stale {
            fs::remove_dir_all(&path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Client side

/// Resolves a location grant beneath `<root>/<store_id>`. Absolute paths, `..`, anything but
/// plain `[a-z0-9._-]` components, and symlinks that lead outside the store are refused.
pub(crate) fn resolve(root: &Path, store_id: &str, loc: &Location) -> Result<PathBuf, BusError> {
    if loc.store_id != store_id {
        return Err(BusError::new(
            ErrorCode::ArtifactGone,
            "location names another store incarnation",
        ));
    }
    let refuse =
        |why: &str| BusError::new(ErrorCode::StoreFailure, format!("location refused: {why}"));
    let rel = Path::new(&loc.relative_path);
    if loc.relative_path.is_empty() || rel.is_absolute() {
        return Err(refuse("not a relative path"));
    }
    for c in rel.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str().unwrap_or("");
                if s.is_empty()
                    || !s.bytes().all(|b| {
                        b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || matches!(b, b'.' | b'_' | b'-')
                    })
                {
                    return Err(refuse("unexpected path component"));
                }
            }
            _ => return Err(refuse("parent, root or current-directory component")),
        }
    }
    let base = root.join(&loc.store_id);
    let base = base
        .canonicalize()
        .map_err(|e| refuse(&format!("store directory: {e}")))?;
    let full = base.join(rel).canonicalize().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => BusError::new(ErrorCode::ArtifactGone, "artifact file is gone"),
        _ => refuse(&e.to_string()),
    })?;
    if !full.starts_with(&base) {
        return Err(refuse("escapes the store"));
    }
    Ok(full)
}

pub(crate) fn open_read(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

pub(crate) fn open_write(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(p: &str) -> Location {
        Location {
            store_id: "store-x".into(),
            relative_path: p.into(),
        }
    }

    #[test]
    fn resolve_refuses_escapes() {
        let root = tempfile::tempdir().unwrap();
        let store = root.path().join("store-x");
        fs::create_dir_all(store.join("sealed")).unwrap();
        fs::write(store.join("sealed/a-1"), b"ok").unwrap();
        fs::write(root.path().join("secret"), b"no").unwrap();
        std::os::unix::fs::symlink(root.path().join("secret"), store.join("sealed/a-2")).unwrap();
        assert!(resolve(root.path(), "store-x", &loc("sealed/a-1")).is_ok());
        for bad in [
            "",
            "/etc/passwd",
            "../secret",
            "sealed/../../secret",
            "./sealed/a-1",
            "sealed/A-1",
            "sealed/a-2",
        ] {
            assert!(
                resolve(root.path(), "store-x", &loc(bad)).is_err(),
                "{bad:?}"
            );
        }
        let other = Location {
            store_id: "store-y".into(),
            relative_path: "sealed/a-1".into(),
        };
        assert_eq!(
            resolve(root.path(), "store-x", &other).unwrap_err().code,
            ErrorCode::ArtifactGone
        );
    }

    #[test]
    fn orphans_are_removed_and_live_stores_kept() {
        let root = tempfile::tempdir().unwrap();
        let live = Store::create(root.path(), "store-live").unwrap();
        let orphan = root.path().join("store-dead");
        fs::create_dir_all(orphan.join("sealed")).unwrap();
        fs::write(orphan.join(MARKER), b"").unwrap();
        fs::write(orphan.join(LOCK), b"").unwrap();
        let unrelated = root.path().join("keep-me");
        fs::create_dir_all(&unrelated).unwrap();
        let second = Store::create(root.path(), "store-next").unwrap();
        assert!(!orphan.exists());
        assert!(live.dir().exists() && second.dir().exists() && unrelated.exists());
        drop(live);
        assert!(!root.path().join("store-live").exists());
    }
}
