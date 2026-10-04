//! SQLite persistence via `rusqlite` + `sqlite-vec`.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: axum, leptos, reqwest, business rules
//!
//! Violating this is an architectural defect, not a style preference.
//!
//! # Concurrency
//!
//! `rusqlite` is **synchronous**. AGENTS.md §5.1 (decision D11) is
//! non-negotiable: no DB call may run on the async runtime. Callers must use
//! [`crate::blocking`] helpers or `tokio::task::spawn_blocking`.

#![deny(missing_docs)]

pub mod api_tokens;
pub mod atomic;
pub mod auth;
pub mod blocking;
pub mod crypto;
pub mod pool;
pub mod ratelimit;
pub mod schema;
pub mod totp;

use std::path::{Path, PathBuf};

use {rusqlite::Connection, thiserror::Error};

/// Errors surfaced by the store layer.
#[derive(Debug, Error)]
pub enum StoreError {
    /// The underlying SQLite call failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The blocking task was cancelled before it finished, which happens when
    /// the runtime shuts down mid-query.
    #[error("blocking task cancelled before completion")]
    Interrupted,

    /// Filesystem or permission failure while opening the database.
    #[error("io error at {path}: {source}")]
    Io {
        /// The path being operated on.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// A `sqlite-vec` statement failed.
    #[error("sqlite-vec error: {0}")]
    Vec(String),

    /// Encryption or decryption failed.
    #[error("crypto error: {0}")]
    Crypto(#[from] crate::crypto::CryptoError),

    /// `auth.json` could not be read while resolving token ownership.
    ///
    /// Distinct from [`StoreError::Io`] because the failure is in a JSON sidecar
    /// rather than the database, and because the caller must treat it as
    /// *fail closed* — never as "no owners exist", which would revoke every
    /// token (`AGENTS.md` §6).
    #[error("auth.json error: {0}")]
    Auth(String),

    /// A path could not be used because it is malformed for the operation.
    #[error("invalid path: {0}")]
    Path(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, StoreError>;

/// A handle to the application's SQLite database.
///
/// Deliberately cheap to clone: it holds the path and reopens a connection per
/// operation rather than holding a long-lived handle, so it can never be
/// accidentally held across an `await`.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
}

impl Store {
    /// Opens (or will create) the database at `path`.
    ///
    /// Does not run migrations. Call [`Store::migrate`] explicitly so ordering
    /// is visible at the call site rather than hidden in a constructor.
    #[must_use]
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Opens the database at the default location.
    ///
    /// Resolution order mirrors `DATA_DIR` handling in `src/constants.py`: the
    /// `DATA_DIR` env var if set and non-empty, otherwise `./data`. The
    /// `app.db` filename is unchanged so an existing installation works without
    /// moving any files.
    ///
    /// Cannot fail: no filesystem access happens here. The file is created on
    /// the first [`Store::connect`], which is where IO actually occurs.
    #[must_use]
    pub fn open_default() -> Self {
        Self::open(default_data_dir().join("app.db"))
    }

    /// The directory holding the database.
    ///
    /// [`crate::auth::AuthStore`] keeps `auth.json` and `sessions.json` beside
    /// the DB rather than in it, so it needs this to find the same directory. A
    /// bare filename with no parent yields an empty path, which would place the
    /// sidecars in the process CWD — so the parent is required to exist.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the database path has no parent directory.
    pub fn data_dir(&self) -> Result<PathBuf> {
        self.path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map_or_else(
                || {
                    Err(StoreError::Path(
                        "database path has no parent directory".into(),
                    ))
                },
                |parent| Ok(parent.to_path_buf()),
            )
    }

    /// The on-disk path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Opens a fresh connection with every pragma the application depends on.
    ///
    /// The Python source applies these as class-level `Engine` listeners
    /// (`core/database.py:142-147`), so they apply to *every* connection.
    /// Reproducing that per-connection is the only way to get the same
    /// guarantee, because there is no shared engine here.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if the parent directory cannot be created, and
    /// [`StoreError::Sqlite`] if the database cannot be opened or a pragma fails.
    pub fn connect(&self) -> Result<Connection> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(&self.path)?;
        crate::pool::configure(&conn)?;
        Ok(conn)
    }

    /// Runs the migration chain.
    ///
    /// Idempotent: a database already at the current version is a single
    /// indexed read. The Python source ran ~45 migration functions at every
    /// startup, which is startup cost paid on every boot for no reason.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Sqlite`] if the version table cannot be read or
    /// written, and [`StoreError::Vec`] if a required migration has not been
    /// generated yet.
    pub fn migrate(&self) -> Result<MigrationReport> {
        crate::schema::migrate(&self.connect()?)
    }

    /// Locks the database file to owner-only permissions.
    ///
    /// The Python source applies `safe_chmod(0o600)` to the database **and** its
    /// `-journal` / `-wal` / `-shm` sidecars
    /// (`core/database.py:2075-2102`). Skipping the sidecars would leak data
    /// through a readable WAL, so all four are handled.
    ///
    /// No-op on Windows, where `chmod` is largely meaningless.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Io`] if permissions cannot be applied to the database
    /// or one of its sidecars.
    pub fn restrict_permissions(&self) -> Result<()> {
        #[cfg(windows)]
        {
            return Ok(());
        }
        const MODE: u32 = 0o600;
        for path in std::iter::once(self.path.clone()).chain(self.sidecars()) {
            if path.exists() {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(MODE))
                    .map_err(|source| StoreError::Io { path, source })?;
            }
        }
        Ok(())
    }

    /// The SQLite sidecar files that can contain committed data.
    fn sidecars(&self) -> impl Iterator<Item = PathBuf> + '_ {
        const SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];
        SUFFIXES
            .into_iter()
            .map(move |suffix| PathBuf::from(format!("{}{suffix}", self.path.display())))
    }
}

/// What a migration run actually did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// The schema version before this run.
    pub from: u32,
    /// The schema version after this run.
    pub to: u32,
    /// Descriptions of the steps that were applied, in order.
    pub applied: Vec<&'static str>,
}

impl MigrationReport {
    /// True when the schema was already current.
    #[must_use]
    pub const fn is_noop(&self) -> bool {
        self.applied.is_empty()
    }
}

/// Resolves `DATA_DIR`, mirroring `DATA_DIR` handling in `src/constants.py`.
///
/// Split from the environment read so it is a pure function and therefore testable
/// without mutating process state — which matters because `std::env::set_var` is
/// `unsafe` in edition 2024 and `unsafe_code` is denied workspace-wide.
///
/// Misresolution here is not cosmetic: `app.db` and the `auth.json` /
/// `sessions.json` beside it are found through this same directory, so pointing
/// it somewhere else splits the database from the credential store.
fn default_data_dir() -> PathBuf {
    resolve_data_dir(std::env::var("DATA_DIR").ok())
}

/// The pure resolution rule: a set, non-blank value wins; anything else is
/// `./data`.
fn resolve_data_dir(raw: Option<String>) -> PathBuf {
    raw.map(|dir| dir.trim().to_owned())
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from("data"), PathBuf::from)
}

#[cfg(test)]
mod permission_tests {
    //! `restrict_permissions` is the only thing standing between a shared machine
    //! and a world-readable credential store, and its sidecar coverage was
    //! invisible to CI.
    //!
    //! The existing test in `tests/baseline.rs` stats `app.db` and nothing else.
    //! Nothing in the workspace sets `journal_mode=WAL`, so no `-wal`/`-shm` file
    //! exists at assert time, and `restrict_permissions` guards every path with
    //! `if path.exists()`. Deleting `.chain(self.sidecars())` therefore left the
    //! whole suite green — verified by mutation, not assumed.
    //!
    //! The sidecars are created explicitly here rather than coaxed out of SQLite:
    //! the contract is "if this file exists, tighten it", and manufacturing the
    //! file tests that contract directly. Relying on journal mode would make the
    //! test silently vacuous again the moment that setting changes.

    use {
        super::Store,
        std::{
            os::unix::fs::PermissionsExt as _,
            path::{Path, PathBuf},
        },
    };

    /// Panics rather than asserting, because a missing file here means the test
    /// setup is wrong and there is nothing meaningful left to compare.
    fn mode_of(path: &Path) -> u32 {
        let metadata = std::fs::metadata(path).expect("stat the file under test");
        metadata.permissions().mode() & 0o777
    }

    /// A store with a migrated database and all three sidecars present, each
    /// deliberately world-readable so tightening them is observable.
    fn store_with_permissive_sidecars() -> (tempfile::TempDir, Store, Vec<PathBuf>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = dir.path().join("app.db");
        let store = Store::open(&db);
        store.migrate().expect("migrate");

        let wide = std::fs::Permissions::from_mode(0o644);
        let mut sidecars = Vec::new();
        for suffix in ["-journal", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", db.display()));
            std::fs::write(&path, b"x").expect("write sidecar");
            std::fs::set_permissions(&path, wide.clone()).expect("loosen sidecar");
            assert_eq!(mode_of(&path), 0o644, "precondition");
            sidecars.push(path);
        }
        std::fs::set_permissions(&db, wide).expect("loosen db");
        (dir, store, sidecars)
    }

    #[test]
    fn the_database_and_every_sidecar_are_tightened_to_0600() {
        let (_dir, store, sidecars) = store_with_permissive_sidecars();
        store.restrict_permissions().expect("restrict");

        assert_eq!(
            mode_of(store.path()),
            0o600,
            "the database itself must be owner-only"
        );
        for sidecar in &sidecars {
            assert_eq!(
                mode_of(sidecar),
                0o600,
                "{} must be tightened too: skipping the sidecars leaks database \
                 contents through a readable write-ahead log",
                sidecar.display()
            );
        }
    }

    #[test]
    fn a_sidecar_created_after_the_first_call_is_tightened_on_the_next() {
        // SQLite creates `-wal`/`-shm` lazily, so the first call at startup may
        // find nothing to tighten. The guarantee has to hold on every call, not
        // just the one that happens to run first.
        let (_dir, store, _sidecars) = store_with_permissive_sidecars();
        store.restrict_permissions().expect("first restrict");

        let late = PathBuf::from(format!("{}-wal", store.path().display()));
        std::fs::write(&late, b"x").expect("write late sidecar");
        std::fs::set_permissions(&late, std::fs::Permissions::from_mode(0o644)).expect("loosen");

        store.restrict_permissions().expect("second restrict");
        assert_eq!(
            mode_of(&late),
            0o600,
            "a sidecar that appears later must still be tightened"
        );
    }

    #[test]
    fn restricting_is_idempotent_and_survives_absent_sidecars() {
        // The startup path calls this on a database that may have no sidecars at
        // all, and may call it repeatedly. Neither may be an error.
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("app.db"));
        store.migrate().expect("migrate");
        store.restrict_permissions().expect("first");
        store.restrict_permissions().expect("second");
        assert_eq!(mode_of(store.path()), 0o600);
    }

    #[test]
    fn a_blank_data_dir_falls_back_to_data_and_a_real_one_is_honoured() {
        // Misresolution splits `app.db` from `auth.json`/`sessions.json`, which
        // are found through this same directory. Nothing tested the rule at all.
        //
        // Pure rather than environment-mutating: `std::env::set_var` is `unsafe`
        // in edition 2024 and `unsafe_code` is denied workspace-wide, so the
        // environment route is not available. Testing `resolve_data_dir` directly
        // is better anyway — no global state, so it cannot race another test.
        use super::resolve_data_dir;

        // The docker-compose case: the variable is declared but the host never set
        // it, so the container injects an empty value. That must not become the
        // data directory.
        for blank in [None, Some(String::new()), Some(String::from("   "))] {
            assert_eq!(
                resolve_data_dir(blank.clone()),
                PathBuf::from("data"),
                "{blank:?} must fall back to ./data"
            );
        }

        // Surrounding whitespace is trimmed rather than becoming part of the path,
        // which would create a directory literally named " /srv/data".
        assert_eq!(
            resolve_data_dir(Some(String::from("  /srv/odysseus  "))),
            PathBuf::from("/srv/odysseus")
        );
        assert_eq!(
            resolve_data_dir(Some(String::from("relative/path"))),
            PathBuf::from("relative/path")
        );
    }
}
