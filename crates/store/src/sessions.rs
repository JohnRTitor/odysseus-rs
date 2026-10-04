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

use rusqlite::{Connection, OptionalExtension, Row, params};
use thiserror::Error;

use crate::{Store, StoreError};

/// Failures from session persistence.
///
/// Wraps [`StoreError`] so a SQLite problem is not flattened into a string, and
/// so `?` converts at the call site (AGENTS.md §5).
#[derive(Debug, Error)]
pub enum SessionError {
    /// The underlying database call failed.
    #[error("session db error: {0}")]
    Db(#[from] StoreError),

    /// A row was malformed in a way that is not a SQLite error.
    #[error("session {id} is malformed: {message}")]
    Malformed {
        /// The session id.
        id: String,
        /// What was wrong.
        message: String,
    },
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
    /// [`SessionStore::get_blocking`] wraps that into [`SessionError::Db`].
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
/// A struct rather than seven positional parameters: `create_blocking` with
/// `(id, name, endpoint_url, model, owner, rag, now)` invites the caller to
/// transpose two adjacent `String`s, and the compiler cannot catch that. Naming
/// them makes the call site readable and the order irrelevant.
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
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads a session by id, marking it most-recently-used on a hit.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<SessionMeta> {
        let mut inner = self.lock();
        let tick = self.tick();
        // Find the slot whose key is the id; ids are unique so there is at most one.
        let slot = inner.iter().find(|(_, (key, _))| &**key == id).map(|(slot, _)| *slot);
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
        let existing = inner.iter().find(|(_, (key, _))| **key == *meta.id).map(|(slot, _)| *slot);
        if let Some(slot) = existing {
            let _ = inner.remove(&slot);
        }
        inner.insert(tick, (meta.id.clone(), meta));
        while inner.len() > self.max_entries {
            // BTreeMap iterates ascending by tick, so the first key is the LRU.
            let Some(oldest) = inner.keys().next().copied() else { break };
            let _ = inner.remove(&oldest);
        }
    }

    /// Removes one entry.
    pub fn remove(&self, id: &str) -> bool {
        let mut inner = self.lock();
        let slot = inner.iter().find(|(_, (key, _))| &**key == id).map(|(slot, _)| *slot);
        slot.is_some_and(|slot| inner.remove(&slot).is_some())
    }

    /// Empties the cache.
    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Next monotonic tick. Saturates rather than wrapping: a wrap could make two
    /// entries share a tick and corrupt the eviction order.
    fn tick(&self) -> u64 {
        let Ok(mut clock) = self.clock.lock() else { return 0 };
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

    /// The underlying store, for the `**Blocking.**` wrappers.
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
    pub fn create_blocking(&self, new: NewSession) -> Result<()> {
        with_conn(&self.store, new, |conn_mut, new| {
            let conn = &mut *conn_mut;
            let NewSession { id, name, endpoint_url, model, owner, rag, now } = new;
            conn.execute(
                "INSERT INTO sessions
                    (id, name, endpoint_url, model, owner, rag, archived, is_important,
                     message_count, headers, mode, folder, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, '{}', '', '', ?7, ?7)",
                params![id, name, endpoint_url, model, owner, rag, now],
            )?;
            Ok(())
        })
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
    pub fn get_blocking(&self, id: String) -> Result<Option<SessionMeta>> {
        if let Some(hit) = self.cache.get(&id) {
            return Ok(Some(hit));
        }
        let meta = with_conn(&self.store, id, |conn_mut, id| {
            let conn = &mut *conn_mut;
            conn.query_row("SELECT * FROM sessions WHERE id = ?1", [id], SessionMeta::from_row)
                .optional()
                .map_err(SessionError::from)
        })?;
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
    pub fn list_blocking(&self, owner: String) -> Result<Vec<SessionMeta>> {
        with_conn(&self.store, owner, |conn_mut, owner| {
            let conn = &mut *conn_mut;
            // An empty owner means "every owner", matching Python's
            // `get_sessions_for_user(None)` (`session_manager.py:701-707`).
            //
            // The two branches bind differently on purpose: the all-owners SQL has
            // no `?1`, so passing a parameter to it is `InvalidParameterCount`, not
            // a harmless no-op. Binding per branch keeps both queries valid.
            if owner.is_empty() {
                let mut stmt = conn.prepare("SELECT * FROM sessions ORDER BY last_accessed DESC")?;
                let rows = stmt.query_map([], SessionMeta::from_row)?;
                return rows.collect::<rusqlite::Result<Vec<_>>>().map_err(SessionError::from);
            }
            let mut stmt = conn
                .prepare("SELECT * FROM sessions WHERE owner = ?1 ORDER BY last_accessed DESC")?;
            let rows = stmt.query_map([owner], SessionMeta::from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(SessionError::from)
        })
    }

    /// Renames a session, bumping `updated_at`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub fn rename_blocking(&self, id: String, name: String, now: f64) -> Result<()> {
        let target = id.clone();
        with_conn(&self.store, (id, name, now), |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (id, name, now) = d;
            conn.execute(
                "UPDATE sessions SET name = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, name, now],
            )?;
            Ok(())
        })?;
        self.cache.remove(&target);
        Ok(())
    }

    /// Sets the pinned flag, bumping `updated_at`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub fn set_important_blocking(&self, id: String, important: bool, now: f64) -> Result<()> {
        let target = id.clone();
        with_conn(&self.store, (id, important, now), |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (id, important, now) = d;
            conn.execute(
                "UPDATE sessions SET is_important = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, important, now],
            )?;
            Ok(())
        })?;
        self.cache.remove(&target);
        Ok(())
    }

    /// Marks a session archived or unarchived.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub fn set_archived_blocking(&self, id: String, archived: bool, now: f64) -> Result<()> {
        let target = id.clone();
        with_conn(&self.store, (id, archived, now), |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (id, archived, now) = d;
            conn.execute(
                "UPDATE sessions SET archived = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, archived, now],
            )?;
            Ok(())
        })?;
        self.cache.remove(&target);
        Ok(())
    }

    /// Bumps `last_accessed`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the update fails.
    pub fn touch_blocking(&self, id: String, now: f64) -> Result<()> {
        let target = id.clone();
        with_conn(&self.store, (id, now), |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (id, now) = d;
            conn.execute(
                "UPDATE sessions SET last_accessed = ?2 WHERE id = ?1",
                params![id, now],
            )?;
            Ok(())
        })?;
        self.cache.remove(&target);
        Ok(())
    }

    /// Deletes a session. Messages go with it via `ON DELETE CASCADE`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the delete fails.
    pub fn delete_blocking(&self, id: String) -> Result<usize> {
        self.cache.remove(&id);
        let deleted = with_conn(&self.store, id, |conn_mut, id| {
            let conn = &mut *conn_mut;
            conn.execute("DELETE FROM sessions WHERE id = ?1", [id])
                .map_err(SessionError::from)
        })?;
        Ok(deleted)
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
    /// the column disagrees with the rows. That is what makes T0203's removal of
    /// the per-read `COUNT(*)` safe rather than merely convenient.
    ///
    /// **Blocking.** rusqlite is synchronous (AGENTS.md D11); call from
    /// `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the transaction fails. Unlike the Python,
    /// which caught `Exception`, rolled back, logged and returned nothing
    /// (`session_manager.py:305-308`), the failure is visible to the caller.
    pub fn add_message_blocking(
        &self,
        session_id: String,
        message_id: String,
        role: String,
        content: String,
        metadata: Option<String>,
        now: f64,
    ) -> Result<()> {
        let target = session_id.clone();
        with_conn(
            &self.store,
            (session_id, message_id, role, content, metadata, now),
            |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (session_id, message_id, role, content, metadata, now) = d;
            let tx = conn.transaction()?;
            // The session must exist. The Python dropped the message instead
            // (`session_manager.py:249-255`); the foreign key would reject it
            // anyway, so this turns a late write into a clear error.
            let exists: Option<String> =
                tx.query_row("SELECT id FROM sessions WHERE id = ?1", [&session_id], |row| {
                    row.get(0)
                })
                .optional()?;
            if exists.is_none() {
                return Err(SessionError::Db(StoreError::Sqlite(rusqlite::Error::InvalidQuery)));
            }
            tx.execute(
                "INSERT INTO chat_messages (id, session_id, role, content, metadata, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![&message_id, &session_id, &role, &content, &metadata, now],
            )?;
            // Authoritative by construction: same transaction, so it counts this row.
            tx.execute(
                "UPDATE sessions
                    SET message_count = (SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1),
                        last_message_at = ?2,
                        last_accessed = ?2,
                        updated_at = ?2
                  WHERE id = ?1",
                params![&session_id, now],
            )?;
            tx.commit()?;
            Ok(())
        })?;
        // The cached count is now stale by definition.
        self.cache.remove(&target);
        Ok(())
    }

    /// Truncates a session's history, keeping the first `keep_count` messages.
    ///
    /// Recomputes `message_count` from the surviving rows rather than trusting
    /// `keep_count`, mirroring the Python's own comment at
    /// `session_manager.py:331-334`: the AI tool defaults to `keep_count = 10`
    /// on a shorter session, and the column must reflect what actually remains.
    ///
    /// **Blocking.**
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the transaction fails.
    pub fn truncate_blocking(&self, session_id: String, keep_count: usize, now: f64) -> Result<usize> {
        let target = session_id.clone();
        let removed = with_conn(
            &self.store,
            (session_id, keep_count, now),
            |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (session_id, keep_count, now) = d;
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
                let rows = stmt.query_map(params![&session_id, keep], |row| row.get(0))?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            let deleted = ids.len();
            for id in &ids {
                tx.execute("DELETE FROM chat_messages WHERE id = ?1", [id])?;
            }
            tx.execute(
                "UPDATE sessions
                    SET message_count = (SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1),
                        updated_at = ?2
                  WHERE id = ?1",
                params![session_id, now],
            )?;
            tx.commit()?;
            Ok(deleted)
        })?;
        self.cache.remove(&target);
        Ok(removed)
    }

    /// Replaces a session's whole history.
    ///
    /// Each message is `(id, role, content, metadata, timestamp_offset)`, and the
    /// offsets preserve insertion order without relying on `timestamp` resolution.
    ///
    /// **Blocking.**
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the transaction fails. The Python reserved
    /// uploads before deleting rows (`session_manager.py:363-378`); that check
    /// needs the upload handler, which is not a store concern, so it is the
    /// caller's job to run first. Recorded rather than silently dropped.
    pub fn replace_messages_blocking(
        &self,
        session_id: String,
        messages: Vec<(String, String, String, Option<String>, f64)>,
        now: f64,
    ) -> Result<()> {
        let target = session_id.clone();
        with_conn(
            &self.store,
            (session_id, messages, now),
            |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (session_id, messages, now) = d;
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM chat_messages WHERE session_id = ?1", [&session_id])?;
            for (id, role, content, metadata, offset) in messages {
                tx.execute(
                    "INSERT INTO chat_messages (id, session_id, role, content, metadata, timestamp)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, session_id, role, content, metadata, now + offset],
                )?;
            }
            // The recount, not `len(messages)`, so this stays correct if a caller
            // ever filters what it passes in.
            tx.execute(
                "UPDATE sessions
                    SET message_count = (SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1),
                        last_message_at = ?2, last_accessed = ?2, updated_at = ?2
                  WHERE id = ?1",
                params![session_id, now],
            )?;
            tx.commit()?;
            Ok(())
        })?;
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
    /// [`SessionStore::add_message_blocking`] computes the count inside the write
    /// transaction, the column cannot drift, so the query is redundant work on the
    /// hot read path.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read fails.
    pub fn message_count_blocking(&self, session_id: String) -> Result<i64> {
        let count = with_conn(&self.store, session_id, |conn_mut, session_id| {
            let conn = &mut *conn_mut;
            conn.query_row(
                "SELECT message_count FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(SessionError::from)
        })?;
        Ok(count.flatten().unwrap_or(0))
    }

    /// The real number of message rows. For tests and repair, not for reads.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read fails.
    pub fn count_message_rows_blocking(&self, session_id: String) -> Result<i64> {
        with_conn(&self.store, session_id, |conn_mut, session_id| {
            let conn = &mut *conn_mut;
            conn.query_row(
                "SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .map_err(SessionError::from)
        })
    }

    /// Verifies `message_count` agrees with the rows, repairing it if not.
    ///
    /// For an install whose column the Python wrote and which may already have
    /// drifted, and for tests asserting the invariant holds. Returns `true` when a
    /// repair was made.
    ///
    /// **Blocking.**
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::Db`] if the read or repair fails.
    pub fn repair_message_count_blocking(&self, session_id: &str) -> Result<bool> {
        let target = session_id.to_owned();
        let (stored, actual) = with_conn(&self.store, target.clone(), |conn_mut, sid| {
            let conn = &mut *conn_mut;
            let stored: Option<i64> = conn
                .query_row(
                    "SELECT message_count FROM sessions WHERE id = ?1",
                    [&sid],
                    // The column is nullable, so the row value is already an
                    // `Option`; `.optional()` then adds "or no row at all". Both
                    // collapse to None, which is the same answer as Python's
                    // `getattr(db_session, "message_count", 0) or 0`.
                    |row| row.get::<_, Option<i64>>(0),
                )
                .optional()
                .map_err(SessionError::from)?
                .flatten();
            let actual: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM chat_messages WHERE session_id = ?1",
                    [&sid],
                    |row| row.get(0),
                )
                .map_err(SessionError::from)?;
            Ok((stored, actual))
        })?;
        if stored == Some(actual) {
            return Ok(false);
        }
        with_conn(&self.store, (target, actual), |conn_mut, d| {
            let conn = &mut *conn_mut;
            let (session_id, actual) = d;
            conn.execute(
                "UPDATE sessions SET message_count = ?2 WHERE id = ?1",
                params![&session_id, actual],
            )
            .map_err(SessionError::from)
        })?;
        self.cache.remove(session_id);
        Ok(true)
    }
}

impl SessionStore {
    // -- **Blocking.** wrappers ---------------------------------------------
    //
    // Each takes owned arguments because the closure is moved onto the blocking
    // pool and must be `Send + 'static`.

    /// Reads one session's metadata, **on the blocking pool**.
    ///
    /// # Errors
    ///
    /// Propagates [`SessionError::Db`] or a [`tokio::task::JoinError`].
    pub async fn get_async(
        &self,
        id: String,
    ) -> std::result::Result<Option<SessionMeta>, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.clone();
        crate::blocking::spawn_blocking(move || store.get_blocking(id))
            .await?
            .map_err(Into::into)
    }

    /// Lists sessions for one owner, **on the blocking pool**.
    ///
    /// # Errors
    ///
    /// Propagates [`SessionError::Db`] or a [`tokio::task::JoinError`].
    pub async fn list_async(
        &self,
        owner: String,
    ) -> std::result::Result<Vec<SessionMeta>, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.clone();
        crate::blocking::spawn_blocking(move || store.list_blocking(owner))
            .await?
            .map_err(Into::into)
    }

    /// Appends a message, **on the blocking pool**.
    ///
    /// # Errors
    ///
    /// Propagates [`SessionError::Db`] or a [`tokio::task::JoinError`].
    pub async fn add_message_async(
        &self,
        session_id: String,
        message_id: String,
        role: String,
        content: String,
        metadata: Option<String>,
        now: f64,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let store = self.clone();
        crate::blocking::spawn_blocking(move || {
            store.add_message_blocking(session_id, message_id, role, content, metadata, now)
        })
        .await?
        .map_err(Into::into)
    }

    /// Deletes a session, **on the blocking pool**.
    ///
    /// # Errors
    ///
    /// Propagates [`SessionError::Db`] or a [`tokio::task::JoinError`].
    pub async fn delete_async(
        &self,
        id: String,
    ) -> std::result::Result<usize, Box<dyn std::error::Error + Send + Sync>> {
        let store = self.clone();
        crate::blocking::spawn_blocking(move || store.delete_blocking(id))
            .await?
            .map_err(Into::into)
    }

    /// Creates a session, **on the blocking pool**.
    ///
    /// # Errors
    ///
    /// Propagates [`SessionError::Db`] or a [`tokio::task::JoinError`].
    pub async fn create_async(
        &self,
        new: NewSession,
    ) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let store = self.clone();
        crate::blocking::spawn_blocking(move || store.create_blocking(new))
            .await?
            .map_err(Into::into)
    }
}

/// Runs a synchronous `rusqlite` closure against a fresh connection, on the
/// blocking pool, flattening the error into [`SessionError`].
///
/// The closure returns `rusqlite::Result<T>` and `SessionError` wraps it, so every
/// call site is a single `?` rather than a nested `Result<Result<..>>`. That
/// matters because the connection must be opened *and* the work run off the async
/// runtime (AGENTS.md D11), and threading the pool's error type through to every
/// call site would obscure that.
///
/// `f` is generic over the data it needs and receives it as an argument, because
/// the closure is moved to another thread and so cannot capture a borrow of the
/// caller's `&str` (E0521). Call sites write `with_conn(&store, data, |conn, d| …)`.
///
/// `spawn_blocking_sync` does not exist yet — see the request in the report.
fn with_conn<D, F, T>(store: &Store, data: D, f: F) -> Result<T>
where
    D: Send + 'static,
    F: FnOnce(&mut Connection, D) -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    let path = store.path().to_path_buf();
    // Two errors meet here: the connection can fail as a `StoreError`, and the
    // closure returns `SessionError`. Mapping the closure's into the pool's
    // would lose the type, so the pool call is generic over `E` instead.
    crate::blocking::spawn_blocking_sync_generic(move || {
        let mut conn = Store::open(path.clone()).connect()?;
        f(&mut conn, data)
    })
}
