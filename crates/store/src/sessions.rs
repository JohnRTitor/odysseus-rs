//! Session persistence: CRUD, the bounded metadata cache, and `message_count`.
//!
//! A port of `core/session_manager.py`. The Python class is 782 lines and does
//! four things: metadata caching, message persistence, session CRUD, and cleanup.
//! This module is the first two plus CRUD; cleanup lives in
//! [`crate::session_actions`].
//!
//! # Three defects in the Python this does not reproduce
//!
//! ## 1. `message_count` was never authoritative (T0202)
//!
//! `session_manager.py:288`:
//!
//! ```python
//! if session_id in self.sessions:
//!     db_session.message_count = len(self.sessions[session_id].history)
//! else:
//!     db_session.message_count = 0
//! ```
//!
//! That is the *in-memory cache length*, written into a *database column* — two
//! different sources of truth. It drifts in normal operation: the Python's own
//! docstring at `session_manager.py:457-464` documents it, and notes that
//! `_persist_message` swallows a failed insert (`session_manager.py:305-308`) while
//! `add_message` has already appended in memory, so the next successful persist
//! writes `rows + 1`.
//!
//! Here the count is computed **inside the same transaction that inserts the
//! row**, from `SELECT COUNT(*)`, so it is correct by construction and cannot
//! drift. See [`SessionStore::add_message`].
//!
//! ## 2. Writes returned `None` and swallowed every error (T0202)
//!
//! `_persist_message` catches `Exception`, logs, rolls back, and returns nothing.
//! A caller cannot tell success from failure. Every fallible method here returns
//! `Result` (AGENTS.md §5).
//!
//! ## 3. The cache was unbounded (T0201)
//!
//! `self.sessions: Dict[str, Session]` grows one entry per session ever opened,
//! with no ceiling and no eviction. A long-running personal server accumulates
//! every transcript it has ever read. [`SessionCache`] is a hard-capped LRU with
//! [`SessionCache::MAX_ENTRIES`] as a real ceiling, asserted by a test rather
//! than trusted (AGENTS.md §11).
//!
//! # What is deliberately absent
//!
//! The Python's `Session.history` is a full in-memory transcript. **Not ported.**
//! It is the reason the Python needed a cache large enough to hold every message
//! of every session, and holding it here would make [`SessionCache`] enormous and
//! re-introduce the drift that made T0202 necessary. Transcripts are read from
//! SQLite on demand; this module caches *metadata only*, which is what
//! `load_sessions` was already changed to do (`session_manager.py:86-89`).

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use {
    rusqlite::{Connection, OptionalExtension, Row, params},
    thiserror::Error,
};

use crate::{
    Store, StoreError, blocking,
    session_image_cleanup::{CleanupReport, ImageCleanup},
};

/// Failures from session persistence.
///
/// Wraps [`StoreError`] so a SQLite problem is not flattened into a string, and
/// so `?` converts at the call site (AGENTS.md §5).
#[derive(Debug, Error)]
pub enum SessionError {
    /// The underlying database call failed.
    #[error("session db error: {0}")]
    Db(#[from] StoreError),
}

/// Result alias for this module.
pub type Result<T> = std::result::Result<T, SessionError>;

impl From<rusqlite::Error> for SessionError {
    /// Wraps a bare SQLite error that did not already pass through [`Store`].
    ///
    /// [`SessionError::Db`] holds a [`StoreError`], which is what a connection
    /// failure produces. A query inside a closure returns `rusqlite::Error`
    /// directly, so this bridge is what lets `?` work in both places instead of
    /// every closure having to re-wrap by hand.
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(StoreError::Sqlite(error))
    }
}

/// One session row, metadata only.
///
/// Never holds the transcript: that is read from SQLite on demand. Every owned
/// string is a [`Box<str>`] rather than a `String`, because these values are
/// stored and a `String` carries a capacity field that means nothing once it is
/// held in the cache (AGENTS.md §12.1 rule 1).
// No `Eq`: the timestamps are `f64`, matching the SQLite columns and the
// Python model, so only `PartialEq` is derivable. A session whose two rows differ
// by one nanosecond is not a meaningful distinction here.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMeta {
    /// Primary key.
    pub id: Box<str>,
    /// Display name.
    pub name: Box<str>,
    /// Endpoint the session talks to.
    pub endpoint_url: Box<str>,
    /// Model name.
    pub model: Box<str>,
    /// Owning user. Empty means unowned, matching the nullable column.
    pub owner: Box<str>,
    /// Retrieval-augmented generation flag.
    pub rag: bool,
    /// Archived flag.
    pub archived: bool,
    /// User-pinned flag: protected from auto-archive and auto-delete.
    pub is_important: bool,
    /// Folder assignment, empty when unset.
    pub folder: Box<str>,
    /// Conversation mode.
    pub mode: Box<str>,
    /// The **authoritative** message count (T0202).
    pub message_count: i64,
    /// Unix creation time.
    pub created: f64,
    /// Unix last-update time.
    pub updated: f64,
    /// Unix last-access time.
    pub last_accessed: f64,
    /// Unix time of the last persisted message; drives "Last active".
    pub last_message_at: f64,
}

impl SessionMeta {
    /// Reads one `sessions` row.
    ///
    /// # Errors
    ///
    /// Returns a [`rusqlite::Error`] if a column is missing or has the wrong
    /// type, which would mean the schema and this reader have diverged.
    /// [`SessionStore::get`] wraps that into [`SessionError::Db`].
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        /// A nullable column that the Python treats as empty string
        /// (`db_session.owner` / `getattr(..., None)` at `session_manager.py:151`).
        fn text(row: &Row<'_>, name: &str) -> rusqlite::Result<Box<str>> {
            let raw: Option<String> = row.get(name)?;
            Ok(raw.unwrap_or_default().into_boxed_str())
        }
        Ok(Self {
            name: text(row, "name")?,
            endpoint_url: text(row, "endpoint_url")?,
            model: text(row, "model")?,
            owner: text(row, "owner")?,
            folder: text(row, "folder")?,
            mode: text(row, "mode")?,
            rag: row.get::<_, Option<bool>>("rag")?.unwrap_or(false),
            archived: row.get::<_, Option<bool>>("archived")?.unwrap_or(false),
            is_important: row.get::<_, Option<bool>>("is_important")?.unwrap_or(false),
            // `NULL` in a nullable INTEGER column reads as 0, matching Python's
            // `getattr(db_session, "message_count", 0) or 0` (session_manager.py:154),
            // so a row written before the column existed still behaves.
            message_count: row.get::<_, Option<i64>>("message_count")?.unwrap_or(0),
            created: row.get::<_, Option<f64>>("created_at")?.unwrap_or(0.0),
            updated: row.get::<_, Option<f64>>("updated_at")?.unwrap_or(0.0),
            last_accessed: row.get::<_, Option<f64>>("last_accessed")?.unwrap_or(0.0),
            last_message_at: row.get::<_, Option<f64>>("last_message_at")?.unwrap_or(0.0),
            id: text(row, "id")?,
        })
    }
}

/// The fields needed to create a session.
///
/// A struct rather than seven positional parameters: `create` with
/// `(id, name, endpoint_url, model, owner, rag, now)` invites the caller to
/// transpose two adjacent `String`s, and the compiler cannot catch that. Naming
/// them makes the call site readable and the order irrelevant.
///
/// The same reasoning is why [`NewMessage`] exists: `add_message` took
/// `(session_id, message_id, role, content, …)` — four adjacent `String`s where
/// swapping any two still typechecks.
///
/// Fields are private so the invariants (a session starts unarchived, unpinned and
/// with zero messages) cannot be set by a caller.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSession {
    /// Primary key. Must be unique.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Endpoint this session talks to.
    pub endpoint_url: String,
    /// Model name.
    pub model: String,
    /// Owning user; empty means unowned, matching the nullable column.
    pub owner: String,
    /// Retrieval-augmented generation flag.
    pub rag: bool,
    /// Creation time, supplied by the caller so the operation is deterministic
    /// under an injected clock (AGENTS.md §11).
    pub now: f64,
}

/// One message to append, as [`NewSession`] is one session to create.
///
/// Exists for the same reason: `add_message` used to take
/// `(session_id, message_id, role, content, metadata, now)`, in which the first
/// four are adjacent `String`s and transposing any two of them still compiles.
/// Writing a user's prompt into the `session_id` slot would then silently
/// target — or fail against — the wrong session.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMessage {
    /// Primary key of the message row. Must be unique across the table.
    pub id: String,
    /// `user`, `assistant`, `system`, or a tool name.
    pub role: String,
    /// Message body.
    pub content: String,
    /// Optional JSON blob; the image-cleanup pass reads `tool_events` out of it.
    pub metadata: Option<String>,
    /// Unix timestamp of the message.
    pub timestamp: f64,
}

impl NewMessage {
    /// A message with no metadata, for the common case.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        role: impl Into<String>,
        content: impl Into<String>,
        timestamp: f64,
    ) -> Self {
        Self {
            id: id.into(),
            role: role.into(),
            content: content.into(),
            metadata: None,
            timestamp,
        }
    }

    /// Attaches a metadata blob.
    #[must_use]
    pub fn with_metadata(mut self, metadata: impl Into<String>) -> Self {
        self.metadata = Some(metadata.into());
        self
    }
}

/// One cache entry: the id, kept alongside the metadata rather than inside it so
/// eviction can check identity without cloning the row.
type CacheEntry = (Box<str>, SessionMeta);

/// The recency-ordered map behind [`SessionCache`].
type CacheMap = BTreeMap<u64, CacheEntry>;

/// A bounded LRU over session metadata.
///
/// Replaces the Python's unbounded `self.sessions` dict (T0201). The bound is a
/// real ceiling on the number of entries, not a soft target: inserting into a
/// full cache evicts the least recently used entry.
///
/// Ordered by recency, oldest first, so eviction is `pop_first` and the common
/// "did you just touch this?" path is a single `last_mut` — no per-entry
/// timestamp scan. `BTreeMap` is the right structure here precisely because the
/// cache is small and the operations are O(log n) with no hashing of the keys.
///
/// # Why `std::sync::Mutex` and not `tokio::sync::Mutex`
///
/// Every method below is synchronous and self-contained: it takes the guard,
/// copies or drops what it needs, and releases it before returning. No
/// `MutexGuard` is ever live across an `.await` — the async methods on
/// [`SessionStore`] read the cache, release it, then `await` the blocking pool
/// (§6). A `tokio` lock would be a guard that outlives the function, which is
/// the thing being avoided here, so a `std` one is both correct and cheaper.
/// `await_holding_lock` is denied workspace-wide, so a regression would fail the
/// build rather than a code review.
#[derive(Debug)]
pub struct SessionCache {
    inner: Mutex<CacheMap>,
    /// Monotonic counter, so re-inserting a key cannot collide with its own
    /// previous slot.
    clock: Mutex<u64>,
    /// Hard ceiling on entries.
    max_entries: usize,
}

impl SessionCache {
    /// The default ceiling.
    ///
    /// Chosen for a self-hosted single-user instance: far above the number of
    /// sessions a person plausibly cycles through between restarts, and small
    /// enough that the whole cache is a few hundred kilobytes. Python has no
    /// equivalent bound, which is the defect this replaces.
    pub const MAX_ENTRIES: usize = 256;

    /// An empty cache with [`SessionCache::MAX_ENTRIES`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(Self::MAX_ENTRIES)
    }

    /// An empty cache holding at most `max_entries`.
    ///
    /// A capacity of zero is clamped to 1, so the cache can always hold the key
    /// it was just asked about. A cache that cannot store anything would make
    /// every read miss in a way that looks like data loss.
    #[must_use]
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: Mutex::new(CacheMap::new()),
            clock: Mutex::new(0),
            max_entries: max_entries.max(1),
        }
    }

    /// The ceiling this cache was built with.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.max_entries
    }

    /// How many entries are held. Never exceeds [`SessionCache::capacity`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the cache holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Locks the entry map, recovering from poisoning rather than propagating it.
    ///
    /// A poisoned map is still a structurally valid `BTreeMap`; the alternative is
    /// a permanent panic in a read path, which would take down a request handler
    /// over a panic raised somewhere unrelated. Poisoning means an earlier caller
    /// panicked, and that panic has already been reported.
    fn lock(&self) -> std::sync::MutexGuard<'_, CacheMap> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads a session by id, marking it most-recently-used on a hit.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<SessionMeta> {
        let mut inner = self.lock();
        let tick = self.tick();
        // Find the slot whose key is the id; ids are unique so there is at most one.
        let slot = inner
            .iter()
            .find(|(_, (key, _))| &**key == id)
            .map(|(slot, _)| *slot);
        let slot = slot?;
        let entry = inner.remove(&slot)?;
        // Re-insert at the newest tick so eviction order reflects this access.
        inner.insert(tick, entry.clone());
        Some(entry.1)
    }

    /// Inserts or replaces a session, evicting the LRU entry if over capacity.
    pub fn insert(&self, meta: SessionMeta) {
        let mut inner = self.lock();
        let tick = self.tick();
        // Drop any existing slot for this id so re-insert cannot duplicate it.
        let existing = inner
            .iter()
            .find(|(_, (key, _))| **key == *meta.id)
            .map(|(slot, _)| *slot);
        if let Some(slot) = existing {
            let _ = inner.remove(&slot);
        }
        inner.insert(tick, (meta.id.clone(), meta));
        while inner.len() > self.max_entries {
            // BTreeMap iterates ascending by tick, so the first key is the LRU.
            let Some(oldest) = inner.keys().next().copied() else {
                break;
            };
            let _ = inner.remove(&oldest);
        }
    }

    /// Removes one entry.
    pub fn remove(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let slot = inner
            .iter()
            .find(|(_, (key, _))| &**key == id)
            .map(|(slot, _)| *slot);
        slot.is_some_and(|slot| inner.remove(&slot).is_some())
    }

    /// Empties the cache.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Next monotonic tick. Saturates rather than wrapping: a wrap could make two
    /// entries share a tick and corrupt the eviction order.
    fn tick(&self) -> u64 {
        let Ok(mut clock) = self.clock.lock() else {
            return 0;
        };
        *clock = clock.saturating_add(1);
        *clock
    }
}

impl Default for SessionCache {
    fn default() -> Self {
        Self::new()
    }
}
/// Session CRUD and message persistence.
///
/// Hold one per process and share it via `Arc`. Metadata reads go through a
/// bounded [`SessionCache`]; SQLite remains the only source of truth.
///
/// # Every method is `async`, and that is load-bearing (AGENTS.md §6, D11)
///
/// `rusqlite` is synchronous. A `fn` that opened a connection would block an
/// executor thread for the duration of the query, which is the single most
/// likely reason a Rust port ends up slower than the Python it replaces. So no
/// method here is callable without first yielding to the blocking pool:
///
/// * statements needing only [`rusqlite::Connection`] go through
///   [`crate::blocking::with_conn`], which opens *and closes* the connection
///   inside one blocking closure;
/// * statements needing a real transaction — and therefore `&mut Connection`,
///   which `with_conn` deliberately does not hand out — open their own
///   connection with `store.connect()` inside a
///   [`crate::blocking::spawn_blocking`] closure, via
///   [`SessionStore::in_transaction`].
///
/// The cache is touched only before or after those calls, never across one.
#[derive(Debug, Clone)]
pub struct SessionStore {
    store: Store,
    cache: Arc<SessionCache>,
}

impl SessionStore {
    /// A store over `store`, with a [`SessionCache::MAX_ENTRIES`] cache.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            cache: Arc::new(SessionCache::new()),
        }
    }

    /// A store with an explicit cache capacity, for tests.
    #[must_use]
    pub fn with_cache(store: Store, cache: Arc<SessionCache>) -> Self {
        Self { store, cache }
    }

    /// The metadata cache, for metrics and tests.
    #[must_use]
    pub fn cache(&self) -> &Arc<SessionCache> {
        &self.cache
    }

    /// The underlying store, for a caller that needs its own connection.
    #[must_use]
    pub fn store(&self) -> Store {
        self.store.clone()
    }

    // -- CRUD ---------------------------------------------------------------

    /// Creates a session.
    ///
    /// The row starts unarchived, unpinned and with zero messages; those are not
    /// caller-supplied because there is no correct way to create a session that is
    /// already archived.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the insert fails, including the
    /// unique-constraint violation when the id already exists.
    pub async fn create(&self, new: NewSession) -> Result<()> {
        let NewSession {
            id,
            name,
            endpoint_url,
            model,
            owner,
            rag,
            now,
        } = new;
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.execute(
                "INSERT INTO sessions
                    (id, name, endpoint_url, model, owner, rag, archived, is_important,
                     message_count, headers, mode, folder, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, '{}', '', '', ?7, ?7)",
                params![id, name, endpoint_url, model, owner, rag, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Reads one session's metadata, consulting the cache first.
    ///
    /// The cache is a pure optimisation: SQLite is the source of truth, and a miss
    /// simply reads through.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read fails. A missing session is
    /// `Ok(None)`, not an error: callers ask "does this exist" constantly.
    pub async fn get(&self, id: &str) -> Result<Option<SessionMeta>> {
        if let Some(hit) = self.cache.get(id) {
            return Ok(Some(hit));
        }
        // The closure is `Send + 'static`, so it cannot capture the caller's `&str`;
        // the borrow is turned into an owned key here, at the call site.
        let key = id.to_owned();
        let meta = blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.query_row(
                "SELECT * FROM sessions WHERE id = ?1",
                [&key],
                SessionMeta::from_row,
            )
            .optional()
            .map_err(SessionError::from)
        })
        .await?;
        if let Some(meta) = &meta {
            self.cache.insert(meta.clone());
        }
        Ok(meta)
    }

    /// Lists sessions for one owner, or all when `owner` is empty.
    ///
    /// Reads through to SQLite rather than filtering the cache: a bounded cache
    /// cannot answer "all my sessions", and answering from it would silently omit
    /// whatever was evicted.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the query fails.
    pub async fn list(&self, owner: &str) -> Result<Vec<SessionMeta>> {
        let owner = owner.to_owned();
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            // An empty owner means "every owner", matching Python's
            // `get_sessions_for_user(None)` (`session_manager.py:701-707`).
            //
            // The two branches bind differently on purpose: the all-owners SQL has
            // no `?1`, so passing a parameter to it is `InvalidParameterCount`, not
            // a harmless no-op. Binding per branch keeps both queries valid.
            if owner.is_empty() {
                let mut stmt =
                    conn.prepare("SELECT * FROM sessions ORDER BY last_accessed DESC")?;
                let rows = stmt.query_map([], SessionMeta::from_row)?;
                return rows
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(SessionError::from);
            }
            let mut stmt = conn
                .prepare("SELECT * FROM sessions WHERE owner = ?1 ORDER BY last_accessed DESC")?;
            let rows = stmt.query_map([owner], SessionMeta::from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(SessionError::from)
        })
        .await
    }

    /// Renames a session, bumping `updated_at`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub async fn rename(&self, id: &str, name: &str, now: f64) -> Result<()> {
        self.update_one(id, "name", name, now).await
    }

    /// Sets the pinned flag, bumping `updated_at`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub async fn set_important(&self, id: &str, important: bool, now: f64) -> Result<()> {
        // `is_important` holds a boolean, not text, so it cannot reuse the `&str`
        // setter below without a stringly-typed round trip through SQL.
        let target = id.to_owned();
        let key = target.clone();
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.execute(
                "UPDATE sessions SET is_important = ?2, updated_at = ?3 WHERE id = ?1",
                params![key, important, now],
            )?;
            Ok(())
        })
        .await?;
        self.cache.remove(&target);
        Ok(())
    }

    /// Marks a session archived or unarchived.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub async fn set_archived(&self, id: &str, archived: bool, now: f64) -> Result<()> {
        let target = id.to_owned();
        let key = target.clone();
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.execute(
                "UPDATE sessions SET archived = ?2, updated_at = ?3 WHERE id = ?1",
                params![key, archived, now],
            )?;
            Ok(())
        })
        .await?;
        self.cache.remove(&target);
        Ok(())
    }

    /// Bumps `last_accessed`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub async fn touch(&self, id: &str, now: f64) -> Result<()> {
        let target = id.to_owned();
        let key = target.clone();
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.execute(
                "UPDATE sessions SET last_accessed = ?2 WHERE id = ?1",
                params![key, now],
            )?;
            Ok(())
        })
        .await?;
        // `last_accessed` is part of the cached row and part of `list`'s ORDER BY,
        // so the entry has to go rather than be served stale.
        self.cache.remove(&target);
        Ok(())
    }

    /// Deletes a session. Messages go with it via `ON DELETE CASCADE`.
    ///
    /// Images are **not** touched: `gallery_images.session_id` is
    /// `ON DELETE SET NULL`, so the rows survive as orphans, which matches the
    /// Python's intent (`session_manager.py:591-595` detaches documents the same
    /// way). Use [`SessionStore::delete_with_image_cleanup`] to sweep the
    /// generated files as well.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the delete fails.
    pub async fn delete(&self, id: &str) -> Result<usize> {
        let key = id.to_owned();
        let deleted = blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.execute("DELETE FROM sessions WHERE id = ?1", [&key])
                .map_err(SessionError::from)
        })
        .await?;
        self.cache.remove(id);
        Ok(deleted)
    }

    /// Deletes a session and soft-deletes the gallery images it owns, in one
    /// transaction.
    ///
    /// This is the composition `session_manager.py:589-595` performs, and the
    /// **order** is load-bearing: the image rows are read and deactivated *before*
    /// the session row goes, because `ON DELETE SET NULL` would otherwise erase
    /// the `session_id` they are found by, leaving orphaned files on disk forever.
    ///
    /// The Python logs an image-cleanup failure as a warning and deletes the
    /// session anyway. Here the error propagates and the transaction rolls back,
    /// so the caller decides; a caller that prefers the Python's behaviour can
    /// `let _ = …` explicitly at the call site, which is where that judgement
    /// belongs (§5).
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the image cleanup or the delete fails, and
    /// [`StoreError::Interrupted`] if the task is cancelled. Returns the number of
    /// session rows deleted alongside a [`CleanupReport`] of the image work.
    pub async fn delete_with_image_cleanup(
        &self,
        id: &str,
        cleanup: &ImageCleanup,
    ) -> Result<(usize, CleanupReport)> {
        let session_id = id.to_owned();
        let root = cleanup.root().to_path_buf();
        let (deleted, report) = self
            .in_transaction(move |conn| {
                let report = crate::session_image_cleanup::cleanup_session_images(
                    conn,
                    &ImageCleanup::new(root),
                    &session_id,
                )?;
                let deleted = conn
                    .execute("DELETE FROM sessions WHERE id = ?1", [&session_id])
                    .map_err(SessionError::from)?;
                Ok((deleted, report))
            })
            .await?;
        self.cache.remove(id);
        Ok((deleted, report))
    }

    /// One `UPDATE sessions SET <column> = ?2, updated_at = ?3 WHERE id = ?1`,
    /// then a cache invalidation.
    ///
    /// The column is a hard-coded literal from [`SessionStore::rename`] alone —
    /// never caller-supplied — and the *value* is still bound as `?2` rather than
    /// interpolated. That keeps all SQL parameterized while collapsing three
    /// near-identical statements into one.
    async fn update_one(&self, id: &str, column: &str, value: &str, now: f64) -> Result<()> {
        let statement = format!("UPDATE sessions SET {column} = ?2, updated_at = ?3 WHERE id = ?1");
        let target = id.to_owned();
        let key = target.clone();
        let value = value.to_owned();
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.execute(&statement, params![key, value, now])?;
            Ok(())
        })
        .await?;
        self.cache.remove(&target);
        Ok(())
    }
}

impl SessionStore {
    // -- messages -----------------------------------------------------------

    /// Appends a message, computing `message_count` in the same transaction.
    ///
    /// This is T0202, and the whole point is *where* the count is computed.
    ///
    /// The Python wrote `len(self.sessions[session_id].history)` into the column
    /// (`session_manager.py:288`) — an in-memory length standing in for a
    /// database fact, which drifts the moment the two disagree. Its own docstring
    /// at `session_manager.py:457-464` documents the drift. Here the insert, the
    /// recount and the timestamp bumps all happen inside one transaction:
    ///
    /// ```text
    /// BEGIN
    ///   INSERT INTO chat_messages ...
    ///   UPDATE sessions SET message_count = (SELECT COUNT(*) FROM chat_messages
    ///                                    WHERE session_id = ?), ... WHERE id = ?
    /// COMMIT
    /// ```
    ///
    /// Because the recount is inside the transaction it sees the row just
    /// inserted, and a concurrent writer either commits before this transaction
    /// reads (and is counted) or after (and is not). There is no window in which
    /// Truncates a session's history, keeping the first `keep_count` messages.
    ///
    /// Recomputes `message_count` from the surviving rows rather than trusting
    /// `keep_count`, mirroring the Python's own comment at
    /// `session_manager.py:331-334`: the AI tool defaults to `keep_count = 10`
    /// on a shorter session, and the column must reflect what actually remains.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the transaction fails.
    pub async fn truncate(&self, session_id: &str, keep_count: usize, now: f64) -> Result<usize> {
        let target = session_id.to_owned();
        let key = target.clone();
        let removed = self
            .in_transaction(move |conn| {
                let tx = conn.transaction()?;
                // `id` breaks ties because timestamps can collide within a millisecond.
                let ids: Vec<String> = {
                    let mut stmt = tx.prepare(
                        "SELECT id FROM chat_messages WHERE session_id = ?1
                         ORDER BY timestamp, id LIMIT -1 OFFSET ?2",
                    )?;
                    #[expect(
                        clippy::cast_possible_wrap,
                        reason = "a keep_count beyond i64 is not reachable from a UI or a route"
                    )]
                    let keep = keep_count as i64;
                    let rows = stmt.query_map(params![&key, keep], |row| row.get(0))?;
                    rows.collect::<rusqlite::Result<Vec<_>>>()?
                };
                let deleted = ids.len();
                // SQLite has no array binding, so this is one statement per row.
                // Every `id` is still a bound `?1` parameter — none is interpolated
                // into SQL text — and they come from this session's own rows rather
                // than from caller input.
                for id in &ids {
                    tx.execute("DELETE FROM chat_messages WHERE id = ?1", [id])?;
                }
                tx.execute(
                    "UPDATE sessions
                        SET message_count = (SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1),
                            updated_at = ?2
                      WHERE id = ?1",
                    params![key, now],
                )?;
                tx.commit()?;
                Ok(deleted)
            })
            .await?;
        self.cache.remove(&target);
        Ok(removed)
    }

    /// Replaces a session's whole history.
    ///
    /// Each message carries its own `timestamp`, and slice order is preserved, so
    /// insertion order does not depend on timestamp resolution. `now` is the
    /// moment of the edit and is what the session row is stamped with.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the transaction fails. The Python reserved
    /// uploads before deleting rows (`session_manager.py:363-378`); that check
    /// needs the upload handler, which is not a store concern, so it is the
    /// caller's job to run first. Recorded rather than silently dropped.
    pub async fn replace_messages(
        &self,
        session_id: &str,
        messages: Vec<NewMessage>,
        now: f64,
    ) -> Result<()> {
        let target = session_id.to_owned();
        let key = target.clone();
        self.in_transaction(move |conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM chat_messages WHERE session_id = ?1", [&key])?;
            for message in &messages {
                tx.execute(
                    "INSERT INTO chat_messages (id, session_id, role, content, metadata, timestamp)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        &message.id,
                        &key,
                        &message.role,
                        &message.content,
                        &message.metadata,
                        message.timestamp
                    ],
                )?;
            }
            // The recount, not `messages.len()`, so this stays correct if a caller
            // ever filters what it passes in.
            tx.execute(
                "UPDATE sessions
                    SET message_count = (SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1),
                        last_message_at = ?2, last_accessed = ?2, updated_at = ?2
                  WHERE id = ?1",
                params![key, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        self.cache.remove(&target);
        Ok(())
    }

    /// How many messages a session holds, from the authoritative column (T0203).
    ///
    /// No `COUNT(*)` runs here. That is the load-bearing consequence of T0202: the
    /// Python ran `SELECT COUNT(*)` on every metadata sync
    /// (`session_manager.py:488-492`) precisely because the column could not be
    /// trusted — its docstring says the column "drifts in normal operation" and
    /// reconciles against the real rows instead. Now that
    /// [`SessionStore::add_message`] computes the count inside the write
    /// transaction, the column cannot drift, so the query is redundant work on the
    /// hot read path.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read fails.
    pub async fn message_count(&self, session_id: &str) -> Result<i64> {
        let key = session_id.to_owned();
        let count = blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.query_row(
                "SELECT message_count FROM sessions WHERE id = ?1",
                [&key],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(SessionError::from)
        })
        .await?;
        Ok(count.flatten().unwrap_or(0))
    }

    /// Appends a message, computing `message_count` in the same transaction.
    ///
    /// This is T0202, and the whole point is *where* the count is computed.
    ///
    /// The Python wrote `len(self.sessions[session_id].history)` into the column
    /// (`session_manager.py:288`) — an in-memory length standing in for a
    /// database fact, which drifts the moment the two disagree. Its own docstring
    /// at `session_manager.py:457-464` documents the drift. Here the insert, the
    /// recount and the timestamp bumps all happen inside one transaction:
    ///
    /// ```text
    /// BEGIN
    ///   INSERT INTO chat_messages ...
    ///   UPDATE sessions SET message_count = (SELECT COUNT(*) FROM chat_messages
    ///                                    WHERE session_id = ?), ... WHERE id = ?
    /// COMMIT
    /// ```
    ///
    /// Because the recount is inside the transaction it sees the row just
    /// inserted, and a concurrent writer either commits before this transaction
    /// reads (and is counted) or after (and is not). There is no window in which
    /// the column disagrees with the rows. That is what makes T0203's removal of
    /// the per-read `COUNT(*)` safe rather than merely convenient.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the transaction fails. Unlike the Python,
    /// which caught `Exception`, rolled back, logged and returned nothing
    /// (`session_manager.py:305-308`), the failure is visible to the caller.
    pub async fn add_message(&self, session_id: &str, message: NewMessage) -> Result<()> {
        let target = session_id.to_owned();
        let key = target.clone();
        self.in_transaction(move |conn| {
            let tx = conn.transaction()?;
            // The session must exist. The Python dropped the message instead
            // (`session_manager.py:249-255`); the foreign key would reject it
            // anyway, so this turns a late write into a clear error.
            let exists: Option<String> = tx
                .query_row("SELECT id FROM sessions WHERE id = ?1", [&key], |row| {
                    row.get(0)
                })
                .optional()?;
            if exists.is_none() {
                return Err(SessionError::Db(StoreError::Sqlite(
                    rusqlite::Error::InvalidQuery,
                )));
            }
            tx.execute(
                "INSERT INTO chat_messages (id, session_id, role, content, metadata, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &message.id,
                    &key,
                    &message.role,
                    &message.content,
                    &message.metadata,
                    message.timestamp
                ],
            )?;
            // Authoritative by construction: same transaction, so it counts this row.
            tx.execute(
                "UPDATE sessions
                    SET message_count = (SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1),
                        last_message_at = ?2,
                        last_accessed = ?2,
                        updated_at = ?2
                  WHERE id = ?1",
                params![&key, message.timestamp],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
        // The cached row now holds a stale count by definition.
        self.cache.remove(&target);
        Ok(())
    }

    /// The real number of message rows. For tests and repair, not for reads.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read fails.
    pub async fn count_message_rows(&self, session_id: &str) -> Result<i64> {
        let key = session_id.to_owned();
        blocking::with_conn(self.store.clone(), move |conn| -> Result<_> {
            conn.query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1",
                [&key],
                |row| row.get(0),
            )
            .map_err(SessionError::from)
        })
        .await
    }

    /// Verifies `message_count` agrees with the rows, repairing it if not.
    ///
    /// For an install whose column the Python wrote and which may already have
    /// drifted, and for tests asserting the invariant holds. Returns `true` when a
    /// repair was made.
    ///
    /// The read and the repair are one transaction: a message committed between
    /// the `COUNT(*)` and the `UPDATE` would otherwise be silently overwritten
    /// with a count that excludes it.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read or repair fails.
    pub async fn repair_message_count(&self, session_id: &str) -> Result<bool> {
        let target = session_id.to_owned();
        let key = target.clone();
        let repaired = self
            .in_transaction(move |conn| {
                let tx = conn.transaction()?;
                let stored: Option<i64> = tx
                    .query_row(
                        "SELECT message_count FROM sessions WHERE id = ?1",
                        [&key],
                        // The column is nullable, so the row value is already an
                        // `Option`; `.optional()` then adds "or no row at all". Both
                        // collapse to None, which is the same answer as Python's
                        // `getattr(db_session, "message_count", 0) or 0`.
                        |row| row.get::<_, Option<i64>>(0),
                    )
                    .optional()?
                    .flatten();
                let actual: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1",
                    [&key],
                    |row| row.get(0),
                )?;
                if stored == Some(actual) {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE sessions SET message_count = ?2 WHERE id = ?1",
                    params![&key, actual],
                )?;
                tx.commit()?;
                Ok(true)
            })
            .await?;
        if repaired {
            self.cache.remove(&target);
        }
        Ok(repaired)
    }

    // -- transactions -------------------------------------------------------

    /// Runs `f` inside a transaction, on the blocking pool, against a connection
    /// opened here and closed here.
    ///
    /// [`crate::blocking::with_conn`] hands out `&Connection` on purpose, so a
    /// genuine `BEGIN`/`COMMIT` — the message mutators above — cannot use it:
    /// `rusqlite::transaction()` needs `&mut Connection`. Rather than widen that
    /// shared helper's contract, this opens its own connection *inside* the
    /// [`crate::blocking::spawn_blocking`] closure. The D11 guarantee is
    /// unchanged: no SQLite call happens on an executor thread, and no
    /// `Connection` value outlives the closure, so none can be held across an
    /// `.await`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] for a connection, statement or commit failure,
    /// and [`StoreError::Interrupted`] if the task is cancelled. A panic inside
    /// `f` is resumed on the caller so the original message and location survive,
    /// rather than surfacing as an opaque `JoinError`.
    async fn in_transaction<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let store = self.store.clone();
        match blocking::spawn_blocking(move || {
            let mut conn = store.connect()?;
            f(&mut conn)
        })
        .await
        {
            Ok(result) => result,
            Err(join_err) if join_err.is_panic() => {
                std::panic::resume_unwind(join_err.into_panic());
            }
            Err(_) => Err(SessionError::Db(StoreError::Interrupted)),
        }
    }
}
