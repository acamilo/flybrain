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

/// Store file operations up to this size run in place on the async thread rather than on the
/// blocking pool: creating or sealing a staging file, and reading a sealed one whole. The store is
/// meant to be tmpfs, where such a copy takes microseconds and the pool hop costs more (BUS-01).
pub const INLINE_IO_BYTES: u64 = 256 * 1024;

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
        let result = reserve(&file, len);
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        result
    }

    /// Makes a recycled writer's staging file what a fresh allocation is: `len` bytes, all zero,
    /// reserved (BUS-02 review N3). The file still holds the artifact it was sealed from; a
    /// producer that wrote fewer bytes into its next life and sealed the whole allocation would
    /// otherwise publish the previous artifact's tail. Truncating frees those blocks, and the
    /// reservation that follows is the one `create_staging` makes.
    pub(crate) fn reset_staging(&self, serial: u64, len: u64) -> io::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.path(&staging_rel(serial)))?;
        file.set_len(0)?;
        reserve(&file, len)
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

    /// Copies the first `len` staging bytes into a fresh sealed file (BUS-02: a writer attached
    /// to a sending command seals the bytes its attachment declares, at most its allocation).
    /// The staging file is left for the caller.
    pub(crate) fn seal_prefix(&self, serial: u64, len: u64) -> Result<(), SealFailure> {
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
        let result = copy_prefix(&mut src, &mut dst, len).and_then(|()| {
            dst.set_permissions(fs::Permissions::from_mode(0o444))?;
            Ok(())
        });
        if result.is_err() {
            let _ = fs::remove_file(&dst_path);
        }
        result
    }

    /// Renames a store file within the store (a recycled writer's staging file).
    pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        fs::rename(self.path(from), self.path(to))
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

/// Reserves `len` bytes of `file` (zero-filled), so a full disk fails here and not in the
/// producer's write.
fn reserve(file: &File, len: u64) -> io::Result<()> {
    if len == 0 {
        return Ok(());
    }
    let off_len =
        libc::off_t::try_from(len).map_err(|_| io::Error::other("length overflows off_t"))?;
    // SAFETY: posix_fallocate on a valid descriptor opened for writing.
    let rc = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, off_len) };
    match rc {
        0 => Ok(()),
        libc::EOPNOTSUPP | libc::EINVAL => file.set_len(len),
        e => Err(io::Error::from_raw_os_error(e)),
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
    // Sized to the copy (BUS-02): a fixed 256 KiB buffer is a fresh mapping, and its page
    // faults, on every seal of a frame-sized artifact.
    let mut buf = vec![0u8; (len.saturating_add(1)).clamp(1, 256 * 1024) as usize];
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

fn copy_prefix(src: &mut File, dst: &mut File, len: u64) -> Result<(), SealFailure> {
    // In the kernel where it can (`copy_file_range`: one copy, no bounce through a user buffer;
    // on the store's tmpfs that halves a frame's seal), else through a buffer.
    let mut done: u64 = 0;
    while done < len {
        let want = (len - done).min(1 << 30) as usize;
        // SAFETY: two valid descriptors; null offsets use and advance each file's position.
        let n = unsafe {
            libc::copy_file_range(
                src.as_raw_fd(),
                std::ptr::null_mut(),
                dst.as_raw_fd(),
                std::ptr::null_mut(),
                want,
                0,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if done == 0
                && matches!(
                    e.raw_os_error(),
                    Some(libc::ENOSYS | libc::EXDEV | libc::EINVAL | libc::EOPNOTSUPP)
                )
            {
                return copy_prefix_buffered(src, dst, len);
            }
            return Err(SealFailure::Io(e));
        }
        if n == 0 {
            return Err(SealFailure::Mismatch(format!(
                "staging holds {done} of {len} declared bytes"
            )));
        }
        done += n as u64;
    }
    Ok(())
}

fn copy_prefix_buffered(src: &mut File, dst: &mut File, len: u64) -> Result<(), SealFailure> {
    // Sized to the copy: a frame or a memory image is one read and one write, and the buffer is
    // small enough to come from the heap rather than a fresh mapping each time.
    let mut buf = vec![0u8; len.clamp(1, 256 * 1024) as usize];
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
        dst.write_all(&buf[..n])?;
        remaining -= n as u64;
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
pub(crate) fn resolve(
    root: &Path,
    store_id: &str,
    loc: &Location,
    dirs: &DirCache,
) -> Result<PathBuf, BusError> {
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
    // The directory part (`sealed`, `staging`) is resolved once per store directory and
    // connection, and cached ([`DirCache`], BUS-02): canonicalizing the whole path is a readlink per component, twice, on every open
    // of every artifact. The file itself is then checked with one lstat: a symlink there is
    // refused like one that escapes (the open that follows uses O_NOFOLLOW as well).
    let (dir, file) = match loc.relative_path.rsplit_once('/') {
        Some((dir, file)) => (Some(dir), file),
        None => (None, loc.relative_path.as_str()),
    };
    let base = dirs
        .canonical(&root.join(&loc.store_id), None)
        .map_err(|e| refuse(&format!("store directory: {e}")))?;
    let parent = match dir {
        None => base.clone(),
        Some(dir) => dirs.canonical(&root.join(&loc.store_id), Some(dir)).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => BusError::new(ErrorCode::ArtifactGone, "artifact file is gone"),
            _ => refuse(&e.to_string()),
        })?,
    };
    if !parent.starts_with(&base) {
        return Err(refuse("escapes the store"));
    }
    let full = parent.join(file);
    match fs::symlink_metadata(&full) {
        Ok(m) if m.file_type().is_symlink() => Err(refuse("escapes the store")),
        Ok(_) => Ok(full),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(BusError::new(ErrorCode::ArtifactGone, "artifact file is gone"))
        }
        Err(e) => Err(refuse(&e.to_string())),
    }
}

/// The canonical store directories one client connection has resolved (BUS-02): a store's
/// directories are made by its router when the store is created and never move. The cache is
/// the connection's, not the process's (BUS-02 review N6), so a directory is resolved again by
/// every new connection -- after a router restart, for one -- and nothing outlives the store
/// incarnation it was resolved for. Within one connection a directory replaced after its first
/// use is not re-checked; the file itself still is (an lstat, then an O_NOFOLLOW open), and
/// the store is the user's own directory (mode 0700), so replacing it needs the user already.
#[derive(Default)]
pub(crate) struct DirCache(std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>);

impl DirCache {
    /// `store.join(dir).canonicalize()`, cached.
    fn canonical(&self, store: &Path, dir: Option<&str>) -> io::Result<PathBuf> {
        let key = match dir {
            Some(dir) => store.join(dir),
            None => store.to_path_buf(),
        };
        if let Some(hit) = self.0.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Ok(hit.clone());
        }
        let resolved = key.canonicalize()?;
        let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() > 256 {
            map.clear();
        }
        map.insert(key, resolved.clone());
        Ok(resolved)
    }
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
        assert!(resolve(root.path(), "store-x", &loc("sealed/a-1"), &DirCache::default()).is_ok());
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
                resolve(root.path(), "store-x", &loc(bad), &DirCache::default()).is_err(),
                "{bad:?}"
            );
        }
        let other = Location {
            store_id: "store-y".into(),
            relative_path: "sealed/a-1".into(),
        };
        assert_eq!(
            resolve(root.path(), "store-x", &other, &DirCache::default()).unwrap_err().code,
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
