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

pub mod blocking;
pub mod crypto;
pub mod pool;
pub mod schema;

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
fn default_data_dir() -> PathBuf {
    std::env::var("DATA_DIR")
        .ok()
        .map(|dir| dir.trim().to_owned())
        .filter(|dir| !dir.is_empty())
        .map_or_else(|| PathBuf::from("data"), PathBuf::from)
}
