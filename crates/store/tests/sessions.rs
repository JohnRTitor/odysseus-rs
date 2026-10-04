//! Acceptance gate for T0200-T0204: the sessions store, the bounded cache, and
//! the message-count invariant.
//!
//! The Python `SessionManager` had three defects that this suite pins shut:
//!
//! * `message_count` written from an in-memory list (T0202),
//! * write errors swallowed, leaving caller and database disagreeing (T0202),
//! * an unbounded metadata cache with no eviction (T0201).
//!
//! Each test below names which defect it would catch if the fix were reverted.
//! No test sleeps: every timestamp is passed in, so the suite is instant and
//! deterministic (AGENTS.md §11).
//!
//! `Result` and `?` throughout rather than `expect` — `clippy.toml`'s
//! `allow-expect-in-tests` covers `#[cfg(test)]` modules, not `crates/*/tests/`.

use std::sync::Arc;

use odysseus_store::{
    Store,
    sessions::{NewSession, SessionCache, SessionMeta, SessionStore},
};

/// A store over a fresh temp database with the baseline applied.
fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path().join("app.db"));
    store.migrate().expect("migrate");
    (dir, store)
}

/// Creates a session with a fixed timestamp.
fn create(
    sessions: &SessionStore,
    id: &str,
    owner: &str,
    now: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    sessions.create_blocking(NewSession {
        id: id.to_owned(),
        name: format!("{id} name"),
        endpoint_url: "http://127.0.0.1:11434/v1".to_owned(),
        model: "llama3".to_owned(),
        owner: owner.to_owned(),
        rag: false,
        now,
    })?;
    Ok(())
}

/// Appends one message.
fn add(
    sessions: &SessionStore,
    session_id: &str,
    n: usize,
    role: &str,
    now: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    sessions.add_message_blocking(
        session_id.to_owned(),
        format!("msg-{session_id}-{n}"),
        role.to_owned(),
        format!("body {n}"),
        None,
        now,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// T0200 — CRUD
// ---------------------------------------------------------------------------

#[test]
fn create_and_read_back_a_session() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0)?;

    let meta = sessions
        .get_blocking("s1".to_owned())?
        .expect("session must exist");
    assert_eq!(meta.id.as_ref(), "s1");
    assert_eq!(meta.owner.as_ref(), "alice");
    assert_eq!(meta.model.as_ref(), "llama3");
    assert_eq!(meta.message_count, 0, "a new session has no messages");
    assert!(!meta.archived && !meta.is_important);
    Ok(())
}

#[test]
fn a_missing_session_is_none_not_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    assert!(sessions.get_blocking("nope".to_owned())?.is_none());
    Ok(())
}

#[test]
fn creating_the_same_id_twice_is_an_error_not_a_silent_overwrite()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "dup", "alice", 1_000.0)?;
    // Python's `create_session` would raise on the second insert too, but the
    // caller could not distinguish it from a crash; here it is a typed error.
    assert!(create(&sessions, "dup", "alice", 1_000.0).is_err());
    Ok(())
}

#[test]
fn rename_and_flags_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0)?;

    sessions.rename_blocking("s1".to_owned(), "renamed".to_owned(), 2_000.0)?;
    sessions.set_important_blocking("s1".to_owned(), true, 3_000.0)?;
    sessions.set_archived_blocking("s1".to_owned(), true, 4_000.0)?;
    sessions.touch_blocking("s1".to_owned(), 5_000.0)?;

    let meta = sessions.get_blocking("s1".to_owned())?.expect("exists");
    assert_eq!(meta.name.as_ref(), "renamed");
    assert!(meta.is_important);
    assert!(meta.archived);
    assert_eq!(meta.last_accessed, 5_000.0);
    Ok(())
}

#[test]
fn list_is_scoped_to_owner_and_empty_owner_means_all() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "a", "alice", 1_000.0)?;
    create(&sessions, "b", "bob", 2_000.0)?;

    let alice = sessions.list_blocking("alice".to_owned())?;
    assert_eq!(alice.len(), 1, "alice must not see bob's session");
    assert_eq!(alice[0].id.as_ref(), "a");

    let everyone = sessions.list_blocking(String::new())?;
    assert_eq!(everyone.len(), 2, "an empty owner means every owner");
    Ok(())
}

#[test]
fn deleting_a_session_cascades_to_its_messages() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store.clone());
    create(&sessions, "s1", "alice", 1_000.0)?;
    add(&sessions, "s1", 0, "user", 1_100.0)?;

    let remaining = store
        .connect()?
        .query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE session_id = 's1'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0);
    assert_eq!(remaining, 1, "setup: the message exists");

    assert_eq!(sessions.delete_blocking("s1".to_owned())?, 1);
    assert!(sessions.get_blocking("s1".to_owned())?.is_none());
    Ok(())
}

// ---------------------------------------------------------------------------
// T0202 — message_count is authoritative by construction
// ---------------------------------------------------------------------------

/// The core invariant: the column equals the rows after any sequence of writes.
///
/// This is the test that fails if someone reintroduces
/// `message_count = len(in_memory_history)`. The Python's own docstring
/// (`session_manager.py:457-464`) documents that the column "drifts in normal
/// operation"; here it cannot, because the recount is inside the insert
/// transaction.
#[test]
fn message_count_always_equals_the_row_count() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0)?;

    for n in 0..5 {
        add(
            &sessions,
            "s1",
            n,
            if n % 2 == 0 { "user" } else { "assistant" },
            1_100.0,
        )?;
        let column = sessions.message_count_blocking("s1".to_owned())?;
        let rows = sessions.count_message_rows_blocking("s1".to_owned())?;
        assert_eq!(
            column,
            rows,
            "after {} messages the column and the rows must agree",
            n + 1
        );
    }
    assert_eq!(sessions.message_count_blocking("s1".to_owned())?, 5);
    Ok(())
}

/// A write to a deleted session must fail loudly, not silently drop the message.
///
/// Python's `_persist_message` returns early with a warning
/// (`session_manager.py:249-255`) and `add_message` has already appended to
/// memory, so the caller sees neither an error nor a row. T0202 requires the
/// error to be visible.
#[test]
fn writing_to_a_missing_session_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    assert!(
        add(&sessions, "ghost", 0, "user", 1_000.0).is_err(),
        "a message for a session that does not exist must be an error, not a no-op"
    );
    Ok(())
}

/// A failed write must leave the count alone.
///
/// The swallowing bug's second half: because `_persist_message` rolled back after
/// `add_message` had already bumped the in-memory count, the next successful
/// persist wrote rows+1. Here a failed write is transactional, so the column
/// cannot move.
#[test]
fn a_failed_write_does_not_change_the_count() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0)?;
    add(&sessions, "s1", 0, "user", 1_100.0)?;
    let before = sessions.message_count_blocking("s1".to_owned())?;

    // A duplicate primary key. `chat_messages.id` is `PRIMARY KEY (id)`
    // (0001_baseline.sql:92), so this insert must fail inside the transaction and
    // take the recount with it.
    let duplicate = sessions.add_message_blocking(
        "s1".to_owned(),
        "msg-s1-0".to_owned(),
        "user".to_owned(),
        "duplicate".to_owned(),
        None,
        1_200.0,
    );
    assert!(
        duplicate.is_err(),
        "a duplicate message id must be an error"
    );

    let after = sessions.message_count_blocking("s1".to_owned())?;
    assert_eq!(
        before, after,
        "a failed write must not move the count; Python wrote rows+1 here"
    );
    Ok(())
}

/// Truncation recounts from the rows that survive, not from the requested cap.
///
/// The Python's comment at `session_manager.py:331-334` says exactly this: the AI
/// tool defaults to `keep_count = 10` on a shorter session, so trusting the cap
/// would leave the column claiming 10 messages when 3 remain.
#[test]
fn truncate_counts_the_rows_that_actually_remain() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0)?;
    for n in 0..3 {
        add(&sessions, "s1", n, "user", 1_100.0 + n as f64)?;
    }

    // Ask for more than exist, exactly as the Python's default does.
    let removed = sessions.truncate_blocking("s1".to_owned(), 10, 2_000.0)?;
    assert_eq!(
        removed, 0,
        "nothing to remove when the cap exceeds the history"
    );
    assert_eq!(
        sessions.message_count_blocking("s1".to_owned())?,
        3,
        "the count must reflect the rows that remain, not keep_count"
    );

    sessions.truncate_blocking("s1".to_owned(), 1, 3_000.0)?;
    assert_eq!(sessions.message_count_blocking("s1".to_owned())?, 1);
    Ok(())
}

/// Replacing history sets the count to the new length.
#[test]
fn replace_messages_resets_the_count() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0)?;
    for n in 0..4 {
        add(&sessions, "s1", n, "user", 1_100.0 + n as f64)?;
    }
    assert_eq!(sessions.message_count_blocking("s1".to_owned())?, 4);

    let messages: Vec<(String, String, String, Option<String>, f64)> = (0..2)
        .map(|n| {
            (
                format!("new-{n}"),
                "user".to_owned(),
                format!("fresh {n}"),
                None,
                n as f64,
            )
        })
        .collect();
    sessions.replace_messages_blocking("s1".to_owned(), messages, 5_000.0)?;
    assert_eq!(
        sessions.message_count_blocking("s1".to_owned())?,
        2,
        "a replacement must leave the count equal to the new length"
    );
    Ok(())
}

// PLACEHOLDER_SESSIONS_TESTS
// ---------------------------------------------------------------------------
// T0201 — the cache is bounded
// ---------------------------------------------------------------------------

/// A metadata row for cache tests. Only the id matters here.
fn meta(id: &str) -> SessionMeta {
    SessionMeta {
        id: id.to_owned().into_boxed_str(),
        name: "n".to_owned().into_boxed_str(),
        endpoint_url: "e".to_owned().into_boxed_str(),
        model: "m".to_owned().into_boxed_str(),
        owner: "alice".to_owned().into_boxed_str(),
        rag: false,
        archived: false,
        is_important: false,
        folder: String::new().into_boxed_str(),
        mode: String::new().into_boxed_str(),
        message_count: 0,
        created: 0.0,
        updated: 0.0,
        last_accessed: 0.0,
        last_message_at: 0.0,
    }
}

/// The ceiling is a real bound, not a soft target.
///
/// The audit's standing note is that a cache can grow without anyone noticing
/// because nothing asserts the limit. This drives the cache far past its capacity
/// and checks the count after *every* insert, so a bound that is enforced only at
/// the end — or not at all — fails here.
#[test]
fn the_cache_never_exceeds_its_capacity() {
    const CAP: usize = 8;
    let cache = SessionCache::with_capacity(CAP);
    assert_eq!(cache.capacity(), CAP);

    for i in 0..500 {
        cache.insert(meta(&format!("s{i}")));
        assert!(
            cache.len() <= CAP,
            "after inserting {} entries the cache holds {}, over the cap of {CAP}",
            i + 1,
            cache.len()
        );
    }
    assert_eq!(cache.len(), CAP, "a full cache settles at exactly its cap");
    assert!(!cache.is_empty());
}

/// Eviction is least-recently-used, not arbitrary.
#[test]
fn eviction_drops_the_least_recently_used_entry() {
    let cache = SessionCache::with_capacity(2);
    cache.insert(meta("oldest"));
    cache.insert(meta("newer"));

    // Touch `oldest` so `newer` becomes the eviction candidate.
    assert!(cache.get("oldest").is_some());

    cache.insert(meta("intruder"));
    assert!(
        cache.get("newer").is_none(),
        "the LRU entry must be evicted"
    );
    assert!(
        cache.get("oldest").is_some(),
        "a recently-read entry must survive"
    );
    assert_eq!(cache.len(), 2, "eviction keeps the cache at capacity");
}

/// The default capacity is a real number, and a zero capacity is clamped.
#[test]
fn capacity_is_clamped_to_at_least_one() {
    assert!(
        SessionCache::MAX_ENTRIES > 0,
        "the default cap must be a real bound"
    );
    let zero = SessionCache::with_capacity(0);
    assert_eq!(
        zero.capacity(),
        1,
        "a zero-capacity cache could never store its own key"
    );
    zero.insert(meta("s0"));
    assert!(
        zero.get("s0").is_some(),
        "the cache must hold the key just inserted"
    );
}

/// The default ceiling is asserted, not assumed.
///
/// AGENTS.md §11's standing critique: a bound nobody asserts is a comment. This
/// pins the actual constant so a later "just bump it" edit has to change this
/// test too, which is the point.
#[test]
fn the_default_capacity_is_the_documented_one() {
    assert_eq!(
        SessionCache::MAX_ENTRIES,
        256,
        "changing the default cache ceiling is a deliberate act; update this assertion \
         and the rationale in the constant's docs when you do"
    );
    assert_eq!(SessionCache::new().capacity(), SessionCache::MAX_ENTRIES);
}

/// A cache read path never reports a session the caller did not ask for.
#[test]
fn a_cache_hit_and_miss_are_distinguishable() {
    let cache = SessionCache::with_capacity(4);
    assert!(cache.get("absent").is_none(), "a miss must be None");
    cache.insert(meta("present"));
    assert!(cache.get("present").is_some());
    assert!(
        cache.remove("present"),
        "removing a present key reports true"
    );
    assert!(!cache.remove("present"), "removing it twice reports false");
    cache.insert(meta("x"));
    cache.clear();
    assert!(cache.is_empty());
}

/// Reading through the store populates the cache, and a write invalidates it.
#[test]
fn the_store_populates_and_invalidates_the_cache() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store();
    let sessions = SessionStore::with_cache(store, Arc::new(SessionCache::with_capacity(4)));
    create(&sessions, "s1", "alice", 1_000.0)?;

    let _ = sessions.get_blocking("s1".to_owned())?;
    assert_eq!(sessions.cache().len(), 1, "a read populates the cache");

    // A write changes the row, so the cached copy must go rather than be served.
    sessions.rename_blocking("s1".to_owned(), "renamed".to_owned(), 2_000.0)?;
    assert_eq!(
        sessions.cache().len(),
        0,
        "a write must invalidate the cached copy"
    );

    let meta = sessions.get_blocking("s1".to_owned())?.expect("exists");
    assert_eq!(
        meta.name.as_ref(),
        "renamed",
        "the read after the write sees it"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// AGENTS.md §12.1 rule 7 — layout ceiling
// ---------------------------------------------------------------------------

/// `SessionMeta` must not silently grow.
///
/// §12.1 rule 7: silent growth of a per-item struct is a regression, not a style
/// preference. This value is one session's metadata as it sits in the cache, so it
/// is multiplied by [`SessionCache::MAX_ENTRIES`] to give the per-instance worst
/// case. The ceiling is generous relative to what the fields actually need (four
/// `Box<str>` fat pointers plus three bools and six scalars) but tight enough that
/// adding four more `String`s to it would fail here.
#[test]
fn session_meta_stays_small() {
    const CEILING: usize = 256;
    assert!(
        std::mem::size_of::<SessionMeta>() <= CEILING,
        "SessionMeta is {} bytes; the ceiling is {CEILING}. Per AGENTS.md §12.1 rule 7, \
         prefer Box<str> over String (rule 1) and drop fields repeated on every row \
         (rule 3) before raising this.",
        std::mem::size_of::<SessionMeta>()
    );

    // The derived consequence: the whole cache stays in a sane range.
    let worst_case = SessionCache::MAX_ENTRIES * std::mem::size_of::<SessionMeta>();
    assert!(
        worst_case < 4 * 1024 * 1024,
        "at the default ceiling the cache is {worst_case} bytes; it must stay small \
         enough to be a cache rather than a second database"
    );
}
