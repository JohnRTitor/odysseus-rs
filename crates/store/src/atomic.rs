//! Atomic file writes.
//!
//! Ports `core/atomic_io.py`. A partially-written `auth.json` would lock every
//! user out of the instance and a half-written `settings.json` would silently
//! revert configuration, so a torn write is not an acceptable failure mode here.
//!
//! The sequence is: write to a uniquely-named temp file in the **same
//! directory**, `fsync` it, then `rename` over the target. `rename` within a
//! filesystem is atomic, so a reader sees either the old file or the new one.
//!
//! The temp name is suffixed with random bytes rather than the process id
//! precisely so two processes racing on the same path cannot collide — the
//! Python source makes the same point at `core/atomic_io.py:24-29`.
//!
//! Files land at mode 0600. `sessions.json` holds directly usable 7-day session
//! tokens and `auth.json` holds bcrypt hashes plus TOTP secrets, so a world-readable
//! mode would hand a local account on a shared host every logged-in session.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use serde::Serialize;

/// Writes `value` as pretty JSON to `path`, atomically.
///
/// Creates the parent directory if needed. The `fsync` is what makes this
/// durable: without it the rename can land before the data, and a power loss
/// leaves an empty file where valid JSON used to be.
///
/// # Errors
///
/// Returns an error if the directory cannot be created, the temp file cannot be
/// written or synced, or the rename fails. On any failure the original file is
/// left untouched.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let serialised = serde_json::to_vec_pretty(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write_bytes(path, &serialised)
}

/// Writes raw bytes to `path`, atomically.
///
/// # Errors
///
/// Returns an error as described by [`atomic_write_json`].
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;

    let temp = temp_sibling(path)?;

    // Write, flush, and sync. `sync_all` covers both the file contents and the
    // metadata; `flush` alone only pushes into the OS buffer.
    let write_result = (|| -> std::io::Result<()> {
        let mut file = create_private(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();

    if let Err(e) = write_result {
        // Do not leave debris behind on failure.
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }

    Ok(())
}

/// A unique temp path beside `path`, so the rename stays within one filesystem.
///
/// Named `{file}.tmp.{32 hex}`, the shape `core/atomic_io.py:32` produces
/// (`f"{path}.tmp.{uuid.uuid4().hex}"`). Matching it means a leftover temp file is
/// recognisable to the same cleanup tooling and to an operator's `ls auth.json.tmp.*`
/// whichever implementation created it.
///
/// The suffix is random rather than the pid for the reason the Python source
/// gives: a pid is constant for the life of a process, so two writers on one path
/// still race. A `uuid4` hex is 16 random bytes.
fn temp_sibling(path: &Path) -> std::io::Result<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path.file_name().map_or_else(
        || std::ffi::OsString::from("atomic"),
        std::ffi::OsStr::to_os_string,
    );

    let mut suffix = [0u8; 16];
    // Propagated, not swallowed: on failure the buffer would stay all-zero, so
    // every writer would pick the same temp path, interleave their writes into one
    // file, and rename a mixed-length invalid document into place.
    getrandom::fill(&mut suffix)
        .map_err(|e| std::io::Error::other(format!("no entropy for a unique temp name: {e}")))?;

    let mut unique = String::with_capacity(32);
    for byte in suffix {
        use std::fmt::Write as _;
        let _ = write!(unique, "{byte:02x}");
    }

    Ok(dir.join(format!("{}.tmp.{unique}", name.to_string_lossy())))
}

/// Creates `path` readable and writable only by its owner.
///
/// `OpenOptions::mode` only applies when the file is actually created, and the umask
/// can only remove bits — so 0600 is requested explicitly rather than relying on
/// the default 0666 & ~umask, which is world-readable on a typical host.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(test)]
mod tests {
    use {
        super::{atomic_write_bytes, atomic_write_json},
        std::path::Path,
    };

    #[test]
    fn writes_json_and_creates_missing_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested/deeper/file.json");
        atomic_write_json(&path, &serde_json::json!({ "a": 1 })).expect("write");
        let raw = std::fs::read_to_string(&path).expect("read");
        let marker = "\"a\"";
        assert!(raw.contains(marker), "unexpected content: {raw}");
    }

    #[test]
    fn overwriting_replaces_the_content_completely() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.json");
        atomic_write_json(&path, &serde_json::json!({ "long": "x".repeat(500) })).expect("first");
        atomic_write_json(&path, &serde_json::json!({ "short": 1 })).expect("second");
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(!raw.contains("xxxx"), "old content must be gone: {raw}");
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("file.json");
        atomic_write_json(&path, &serde_json::json!({ "a": 1 })).expect("write");

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "left temp files: {leftovers:?}");
    }

    #[test]
    fn concurrent_writers_do_not_collide_or_corrupt() {
        // The temp name carries random bytes rather than a pid precisely so this
        // case is safe: two writers must produce a valid file, not interleave.
        let dir = tempfile::tempdir().expect("tempdir");
        let path: &Path = &dir.path().join("shared.json");

        std::thread::scope(|scope| {
            for i in 0..8u64 {
                let path = path.to_path_buf();
                scope.spawn(move || {
                    for _ in 0..20 {
                        atomic_write_json(&path, &serde_json::json!({ "writer": i }))
                            .expect("write");
                    }
                });
            }
        });

        let raw = std::fs::read_to_string(path).expect("read");
        serde_json::from_str::<serde_json::Value>(&raw).expect("final file must be valid JSON");
    }

    #[test]
    fn files_are_not_world_readable() {
        // `sessions.json` holds directly usable 7-day session tokens and
        // `auth.json` holds bcrypt hashes plus TOTP secrets. The default
        // 0666 & ~umask is world-readable on a typical host.
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secrets.json");
        atomic_write_json(&path, &serde_json::json!({ "a": 1 })).expect("write");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    #[test]
    fn an_existing_loose_mode_is_tightened_on_rewrite() {
        // `rename` preserves the temp file's mode, and `OpenOptions::mode` only
        // applies on creation — so a file that already exists loosely must be
        // chmod'ed, not just replaced.
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, "{}").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("loosen");

        atomic_write_json(&path, &serde_json::json!({ "a": 1 })).expect("write");
        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
    }

    #[test]
    fn raw_bytes_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("blob.bin");
        atomic_write_bytes(&path, b"\x00\x01\x02binary").expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"\x00\x01\x02binary");
    }
}
