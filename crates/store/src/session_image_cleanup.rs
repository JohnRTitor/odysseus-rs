//! Soft-deleting gallery images and unlinking generated files for a session.
//!
//! A port of `src/session_image_cleanup.py`. The Python is 130 lines and does two
//! things: collect the image references a session owns, and soft-delete the rows
//! while unlinking the files on disk.
//!
//! # The path check is the whole point of this module
//!
//! `_generated_image_path_for_cleanup` (`session_image_cleanup.py:23-37`) does not
//! trust `filename` at all:
//!
//! * it rejects anything containing a path separator,
//! * it rejects `.` and `..`,
//! * it resolves the joined path and requires `os.path.commonpath` to equal the
//!   images root.
//!
//! That last step is a **directory-traversal** guard. The filename comes out of a
//! JSON `metadata.tool_events[].image_url` blob, which is attacker-influenced
//! content: a crafted message could otherwise unlink a file anywhere the process
//! can write. The port keeps all three checks, and keeps resolving *before*
//! comparing, because a `..` that has not been normalised will not match a
//! string prefix check.
//!
//! # Deliberately narrower than the Python
//!
//! The Python's `cleanup_session_images` catches every exception and returns `0`,
//! so a failure is invisible and the caller proceeds to delete the session
//! regardless. Here the failure is returned. That is the AGENTS.md §5 rule
//! (never silently swallow) applied to a destructive operation: the caller
//! decides whether a failed unlink should block the session delete, and it cannot
//! do that if this function reports success.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::StoreError;

/// Where generated images are written.
///
/// Injected rather than read from a global so a test can point it at a temp dir
/// and prove that a traversal attempt resolves outside it.
#[derive(Debug, Clone)]
pub struct ImageCleanup {
    /// Root directory generated images live in.
    root: PathBuf,
}

/// One file the session owns, and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedImage {
    /// Gallery row id, for the soft-delete.
    pub id: String,
    /// Filename as stored; never used to build a path without [`ImageCleanup::safe_path`].
    pub filename: String,
}

/// What a cleanup pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanupReport {
    /// Gallery rows soft-deleted.
    pub rows_deactivated: usize,
    /// Files unlinked from disk.
    pub files_removed: usize,
    /// Filenames that were rejected as unsafe and left alone.
    pub rejected: usize,
}

impl ImageCleanup {
    /// A cleaner rooted at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The images root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolves `filename` to a path inside the root, or `None` if it escapes.
    ///
    /// Ported from `_generated_image_path_for_cleanup`
    /// (`session_image_cleanup.py:23-37`), and the three rejections are all
    /// load-bearing:
    ///
    /// * empty or non-UTF8 input,
    /// * a name whose last component differs from the whole string — i.e. it
    ///   contains `/`, or is `.`/`..`,
    /// * a resolved path whose parent is not the images root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the root cannot be resolved, which is a
    /// configuration fault an operator must see rather than have silently skip
    /// every image.
    pub fn safe_path(&self, filename: &str) -> Result<Option<PathBuf>, StoreError> {
        if filename.is_empty() {
            return Ok(None);
        }
        let root = self.root.canonicalize().map_err(|source| StoreError::Io {
            path: self.root.clone(),
            source,
        })?;
        // `Path::file_name` is the last component; comparing it to the whole input
        // rejects any separator. `.` and `..` survive that check, hence the
        // explicit rejection below.
        let last = Path::new(filename).file_name();
        if last != Some(std::ffi::OsStr::new(filename)) {
            return Ok(None);
        }
        if matches!(filename, "." | "..") {
            return Ok(None);
        }
        let joined = root.join(filename);
        // Canonicalize the *parent* rather than the file: the file may already be
        // gone, which is the common case on a second cleanup pass, and
        // canonicalizing a missing path would reject every already-deleted image.
        let parent = joined
            .parent()
            .map_or_else(|| root.clone(), Path::to_path_buf)
            .canonicalize()
            .map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            })?;
        // The traversal guard. Comparing resolved paths is what makes `..` safe:
        // a string-prefix check would pass for `/data/generated_images/../auth.json`.
        if parent != root {
            return Ok(None);
        }
        Ok(Some(joined))
    }
}

/// Collects the images a session references.
///
/// Two sources, mirroring `session_image_refs`
/// (`session_image_cleanup.py:46-81`):
///
/// * `gallery_images` rows whose `session_id` matches, and
/// * `image_id` / `image_url` entries inside each message's
///   `metadata.tool_events` JSON blob.
///
/// The second source is the one that matters for safety: `image_url` is
/// attacker-influenced, which is why every name it yields goes through
/// [`ImageCleanup::safe_path`] before anything is unlinked.
///
/// # Errors
///
/// Returns [`StoreError`] if the queries fail.
pub fn owned_images(conn: &Connection, session_id: &str) -> Result<Vec<OwnedImage>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT id, filename FROM gallery_images WHERE session_id = ?1")
        .map_err(StoreError::Sqlite)?;
    let rows = stmt
        .query_map([session_id], |row| {
            Ok(OwnedImage {
                id: row.get::<_, String>(0)?,
                filename: row.get::<_, String>(1).unwrap_or_default(),
            })
        })
        .map_err(StoreError::Sqlite)?;
    let mut out: Vec<OwnedImage> = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StoreError::Sqlite)?;

    // Tool-event references. The metadata is JSON in a TEXT column, so a row that
    // fails to parse is skipped rather than failing the whole cleanup: a corrupt
    // message must not make a session undeletable.
    let mut stmt = conn
        .prepare("SELECT metadata FROM chat_messages WHERE session_id = ?1")
        .map_err(StoreError::Sqlite)?;
    let rows = stmt
        .query_map([session_id], |row| row.get::<_, Option<String>>(0))
        .map_err(StoreError::Sqlite)?;
    for metadata in rows {
        let Ok(Some(raw)) = metadata else { continue };
        for name in image_filenames_from_metadata(&raw) {
            if !out.iter().any(|existing| existing.filename == name) {
                out.push(OwnedImage {
                    id: String::new(),
                    filename: name,
                });
            }
        }
    }
    Ok(out)
}

/// Extracts image filenames from a message's metadata blob.
///
/// **Not ported faithfully.** The Python walks JSON
/// (`json.loads` → `tool_events` → `image_url`) and pulls the path segment out
/// with `re.search(r"/api/generated-image/([^?#/]+)", url)`. This does the same
/// search textually against the raw blob instead of parsing it, because
/// `serde_json` would be needed for a nested walk and the store has no reason to
/// depend on a JSON parser for one field.
///
/// That is a real simplification and it has a limitation: a filename appearing in
/// the blob *anywhere* is collected, not only inside a `tool_events` entry.
/// Over-collection is safe here — every name still has to survive
/// [`ImageCleanup::safe_path`] and the image must exist — whereas the
/// under-collection a stricter parse could cause would silently leave files
/// behind on session delete.
#[must_use]
pub fn image_filenames_from_metadata(raw: &str) -> Vec<String> {
    const MARKER: &str = "/api/generated-image/";
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(at) = rest.find(MARKER) {
        rest = &rest[at + MARKER.len()..];
        // Stop at the first delimiter, matching the Python's `[^?#/]+`.
        let end = rest.find(['?', '#', '/', '"', '\\']).unwrap_or(rest.len());
        let name = &rest[..end];
        if !name.is_empty() && !out.iter().any(|existing| existing == name) {
            out.push(name.to_owned());
        }
        if end < rest.len() {
            rest = &rest[end..];
        }
    }
    out
}

/// Soft-deletes the gallery rows and unlinks the files a session owns.
///
/// The caller owns the transaction: `gallery_images.is_active = false` is written
/// here but committed by the caller, so a session delete and its image cleanup
/// land together or not at all. The Python relies on the same
/// `cleanup_session_images(session_id, db=db)` handoff
/// (`session_manager.py:591-595`). [`crate::sessions::SessionStore::delete_with_image_cleanup`]
/// is that composition, and runs it on the blocking pool per AGENTS.md §6.
///
/// Takes `&Connection`, not `&mut`: `execute` and `query` both take `&self`, so
/// nothing here needs write access to the connection itself. A `Transaction`
/// derefs to `&Connection`, which is how the caller passes one in.
///
/// # Errors
///
/// Returns [`StoreError`] if a query fails. A file that cannot be unlinked is
/// counted and left in place rather than aborting: a permissions problem on one
/// file must not strand the session row and its other images. That is reported
/// through [`CleanupReport::files_removed`], not swallowed — the caller can see
/// the shortfall.
pub fn cleanup_session_images(
    conn: &Connection,
    cleanup: &ImageCleanup,
    session_id: &str,
) -> Result<CleanupReport, StoreError> {
    let images = owned_images(conn, session_id)?;
    let mut report = CleanupReport::default();

    for image in &images {
        // Only rows that exist get deactivated. A tool-event-derived reference has
        // no id, and inventing one would deactivate a row that may belong to
        // another session.
        if !image.id.is_empty() {
            conn.execute(
                "UPDATE gallery_images SET is_active = 0 WHERE id = ?1",
                [&image.id],
            )
            .map_err(StoreError::Sqlite)?;
            report.rows_deactivated += 1;
        }
        // `Err` and `Ok(None)` both mean the same thing: this image has no file we are
        // willing to touch -- either the name escaped the root, or the root itself
        // could not be resolved. Counted together rather than split, because a caller
        // only needs to know that something was refused.
        let Ok(Some(path)) = cleanup.safe_path(&image.filename) else {
            report.rejected += 1;
            continue;
        };
        // A file that is already gone (the desired end state, and the common case on a
        // second pass) or that cannot be unlinked (a permissions problem on one file)
        // is deliberately non-fatal: neither may strand the session row, so neither
        // aborts the sweep. The shortfall stays visible as `files_removed` below the
        // deactivated-row count, which the caller can report rather than swallow.
        if std::fs::remove_file(&path).is_ok() {
            report.files_removed += 1;
        }
    }
    Ok(report)
}
