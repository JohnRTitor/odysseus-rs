//! T0209 — the empty/stale session cleanup pass, end to end against a database.
//!
//! The Python is `cleanup_empty_sessions` (`core/session_manager.py:731-782`)
//! and its suite is `tests/test_session_manager_cleanup.py`, whose single test
//! pins the naive-datetime case: a session whose `last_accessed` is older than
//! the cutoff must be archived, with the exact stats dict returned.
//!
//! The policy itself is unit-tested in `session_cleanup.rs`; this file is about
//! the pass doing the right thing to real rows, including the two nullable
//! columns whose handling differs from each other.
//!
//! No test sleeps and none reads the wall clock: `now` is passed in, so the
//! boundaries are exact numbers of seconds (AGENTS.md §11).

use std::error::Error;

use odysseus_store::{
    Store,
    session_cleanup::{CleanupPolicy, cleanup_empty_sessions},
    sessions::{NewMessage, NewSession, SessionStore},
};

const NOW: f64 = 1_800_000_000.0;
const HOUR: f64 = 3_600.0;
const DAY: f64 = 86_400.0;

fn store() -> Result<(tempfile::TempDir, Store), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("app.db"));
    store.migrate()?;
    Ok((dir, store))
}

async fn new_session(
    sessions: &SessionStore,
    id: &str,
    created: f64,
) -> Result<(), Box<dyn Error>> {
    sessions
        .create(NewSession {
            id: id.into(),
            name: format!("Session {id}"),
            endpoint_url: "http://example.test".into(),
            model: "m".into(),
            owner: "alice".into(),
            rag: false,
            now: created,
        })
        .await?;
    Ok(())
}

async fn add_message(
    sessions: &SessionStore,
    session_id: &str,
    id: &str,
    at: f64,
) -> Result<(), Box<dyn Error>> {
    sessions
        .add_message(session_id, NewMessage::new(id, "user", "hello", at))
        .await?;
    Ok(())
}

fn archived(store: &Store, id: &str) -> Result<bool, Box<dyn Error>> {
    let conn = store.connect()?;
    let value = conn.query_row("SELECT archived FROM sessions WHERE id = ?1", [id], |row| {
        row.get::<_, Option<bool>>(0)
    })?;
    Ok(value.unwrap_or(false))
}

fn exists(store: &Store, id: &str) -> Result<bool, Box<dyn Error>> {
    let conn = store.connect()?;
    Ok(conn
        .query_row("SELECT 1 FROM sessions WHERE id = ?1", [id], |_| Ok(()))
        .is_ok())
}

// ---------------------------------------------------------------------------
// The delete side
// ---------------------------------------------------------------------------

/// An empty session past the minimum age goes.
#[tokio::test]
async fn an_empty_session_older_than_one_hour_is_deleted() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "stale-empty", NOW - 2.0 * HOUR).await?;

    let stats = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert_eq!(stats.deleted_empty, 1);
    assert_eq!(stats.archived_old, 0);
    assert_eq!(stats.total_checked, 1);
    assert!(!exists(&store, "stale-empty")?);
    Ok(())
}

/// An empty session inside the grace window stays, which is the whole point of
/// `min_age_hours`.
#[tokio::test]
async fn an_empty_session_younger_than_one_hour_survives() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "fresh", NOW - 600.0).await?;

    let stats = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert_eq!(stats.deleted_empty, 0);
    assert!(
        exists(&store, "fresh")?,
        "a session ten minutes old must survive"
    );
    Ok(())
}

/// Deleting a session cascades to its messages, so the pass cannot leave
/// orphaned transcript rows behind.
#[tokio::test]
async fn deleting_an_empty_session_removes_nothing_else() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "gone", NOW - 5.0 * HOUR).await?;
    new_session(&sessions, "kept", NOW - 5.0 * HOUR).await?;
    add_message(&sessions, "kept", "m1", NOW - 4.0 * HOUR).await?;

    cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert!(!exists(&store, "gone")?);
    assert!(
        exists(&store, "kept")?,
        "a session with messages is not empty"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The archive side
// ---------------------------------------------------------------------------

/// The Python's own test, ported: an old, non-empty, unarchived, unpinned
/// session is archived and the stats match exactly.
#[tokio::test]
async fn an_old_non_empty_session_is_archived() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "old-chat", NOW - 40.0 * DAY).await?;
    add_message(&sessions, "old-chat", "m1", NOW - 35.0 * DAY).await?;
    // `touch` is what a real session does when opened.
    sessions.touch("old-chat", NOW - 35.0 * DAY).await?;

    let stats = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert!(
        archived(&store, "old-chat")?,
        "last_accessed 35d ago is past the 30d cutoff"
    );
    assert_eq!(stats.archived_old, 1);
    assert_eq!(stats.deleted_empty, 0);
    assert_eq!(stats.total_checked, 1);
    Ok(())
}

/// The archive boundary the task calls out: 29d23h stays, 30d1h goes.
#[tokio::test]
async fn the_archive_boundary_is_thirty_days() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());

    new_session(&sessions, "inside", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "inside", "m1", NOW - 60.0 * DAY).await?;
    sessions
        .touch("inside", NOW - (29.0 * DAY + 23.0 * HOUR))
        .await?;

    new_session(&sessions, "outside", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "outside", "m2", NOW - 60.0 * DAY).await?;
    sessions.touch("outside", NOW - (30.0 * DAY + HOUR)).await?;

    let stats = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert!(
        !archived(&store, "inside")?,
        "29d23h is inside the window and must be left alone"
    );
    assert!(
        archived(&store, "outside")?,
        "30d1h is outside the window and must be archived"
    );
    assert_eq!(stats.archived_old, 1);
    Ok(())
}

/// A session nobody has touched since creation has a NULL `last_accessed`.
///
/// This is the one that matters: coalescing that column to `0` makes it read as
/// ancient, so a brand-new session would be archived by the first pass.
#[tokio::test]
async fn a_session_with_no_last_accessed_is_not_archived() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "untouched", NOW - 365.0 * DAY).await?;
    add_message(&sessions, "untouched", "m1", NOW - 365.0 * DAY).await?;

    // `create` populates `last_accessed`, so the NULL state has to be built
    // explicitly. It is real: the column is nullable, and a row written by an
    // older build or by another writer can carry it as NULL. That is precisely
    // the row a `COALESCE(last_accessed, 0)` would misread as ancient.
    let conn = store.connect()?;
    conn.execute(
        "UPDATE sessions SET last_accessed = NULL WHERE id = 'untouched'",
        [],
    )?;

    let last: Option<f64> = conn.query_row(
        "SELECT last_accessed FROM sessions WHERE id = 'untouched'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        last.is_none(),
        "precondition: the column must actually be NULL"
    );

    let stats = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert!(
        !archived(&store, "untouched")?,
        "a NULL last_accessed is not evidence of staleness"
    );
    assert_eq!(stats.archived_old, 0);
    Ok(())
}

/// A pinned session is never auto-archived.
#[tokio::test]
async fn a_pinned_session_is_never_archived() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "pinned", NOW - 90.0 * DAY).await?;
    add_message(&sessions, "pinned", "m1", NOW - 90.0 * DAY).await?;
    sessions.touch("pinned", NOW - 90.0 * DAY).await?;
    sessions.set_important("pinned", true, NOW).await?;

    cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert!(!archived(&store, "pinned")?);
    Ok(())
}

/// A recently active session is untouched even if it is old by creation.
#[tokio::test]
async fn a_recently_active_session_is_untouched() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "busy", NOW - 300.0 * DAY).await?;
    add_message(&sessions, "busy", "m1", NOW - 60.0).await?;
    sessions.touch("busy", NOW - 60.0).await?;

    cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert!(!archived(&store, "busy")?);
    Ok(())
}

/// A pass over a mixed population counts each category once.
#[tokio::test]
async fn the_stats_count_each_category() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());

    // Two empty and old enough to delete.
    new_session(&sessions, "e1", NOW - 3.0 * HOUR).await?;
    new_session(&sessions, "e2", NOW - 4.0 * HOUR).await?;
    // One empty but too young.
    new_session(&sessions, "young", NOW - 300.0).await?;
    // Two old enough to archive.
    new_session(&sessions, "a1", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "a1", "m1", NOW - 60.0 * DAY).await?;
    sessions.touch("a1", NOW - 60.0 * DAY).await?;
    new_session(&sessions, "a2", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "a2", "m2", NOW - 60.0 * DAY).await?;
    sessions.touch("a2", NOW - 60.0 * DAY).await?;
    // One active.
    new_session(&sessions, "keep", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "keep", "m3", NOW - 60.0).await?;
    sessions.touch("keep", NOW - 60.0).await?;

    let stats = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;

    assert_eq!(stats.total_checked, 6);
    assert_eq!(stats.deleted_empty, 2);
    assert_eq!(stats.archived_old, 2);
    assert!(exists(&store, "young")?);
    assert!(exists(&store, "keep")?);
    Ok(())
}

/// Running the pass twice must be a no-op the second time: the first pass
/// already handled everything it could.
#[tokio::test]
async fn a_second_pass_finds_nothing_left_to_do() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "e", NOW - 3.0 * HOUR).await?;
    new_session(&sessions, "a", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "a", "m1", NOW - 60.0 * DAY).await?;
    sessions.touch("a", NOW - 60.0 * DAY).await?;

    let first = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;
    assert_eq!((first.deleted_empty, first.archived_old), (1, 1));

    let second = cleanup_empty_sessions(store.clone(), CleanupPolicy::default(), NOW).await?;
    assert_eq!(
        (second.deleted_empty, second.archived_old),
        (0, 0),
        "the second pass must not re-archive an already-archived session"
    );
    Ok(())
}

/// A stricter policy raises the minimum age and shortens the archive window.
///
/// Note the direction of the delete rule: `min_age_hours` is the age an empty
/// session must *reach* before it may be deleted, so raising it to 24 h makes
/// the sweeper **less** eager to delete, not more.
#[tokio::test]
async fn a_stricter_policy_archives_sooner_but_deletes_later() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "two_hours_old", NOW - 2.0 * HOUR).await?;
    new_session(&sessions, "recent", NOW - 60.0 * DAY).await?;
    add_message(&sessions, "recent", "m1", NOW - 60.0 * DAY).await?;
    sessions.touch("recent", NOW - 8.0 * DAY).await?;

    let strict = CleanupPolicy {
        auto_archive_days: 7,
        min_age_hours: 24,
    };
    let stats = cleanup_empty_sessions(store.clone(), strict, NOW).await?;

    assert_eq!(
        stats.deleted_empty, 0,
        "2h is short of a 24h minimum, so the empty session must survive"
    );
    assert!(exists(&store, "two_hours_old")?);
    assert_eq!(stats.archived_old, 1, "8d idle is past a 7d window");
    Ok(())
}

/// And the same session *is* deleted once it clears the stricter minimum.
#[tokio::test]
async fn a_stricter_policy_deletes_once_the_minimum_is_reached() -> Result<(), Box<dyn Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    new_session(&sessions, "two_days_old", NOW - 48.0 * HOUR).await?;

    let strict = CleanupPolicy {
        auto_archive_days: 7,
        min_age_hours: 24,
    };
    let stats = cleanup_empty_sessions(store.clone(), strict, NOW).await?;

    assert_eq!(stats.deleted_empty, 1, "48h clears a 24h minimum");
    Ok(())
}
