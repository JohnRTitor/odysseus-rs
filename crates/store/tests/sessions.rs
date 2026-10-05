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
//! The two `expect`s that remain are assertions about values, not about I/O.

use std::sync::Arc;

use odysseus_store::{
    Store,
    session_actions::{
        FRESH_GRACE_SECS, SweepAction, SweepReason, SweepRow, classify, is_recently_active,
        preview_sessions, sweep_sessions,
    },
    session_image_cleanup::ImageCleanup,
    sessions::{NewMessage, NewSession, SessionCache, SessionMeta, SessionStore},
};

/// A store over a fresh temp database with the baseline applied.
///
/// Returns the `TempDir` too: the tests need it to outlive the `Store`, since
/// deleting it out from under an open connection would make the assertions
/// meaningless rather than failing them.
///
/// # Errors
///
/// Returns an error if the temp directory cannot be created or the schema cannot
/// be applied. Propagating rather than `expect`ing keeps this compatible with the
/// workspace's `expect_used = "deny"`, which covers `crates/*/tests/`.
fn store() -> Result<(tempfile::TempDir, Store), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path().join("app.db"));
    store.migrate()?;
    Ok((dir, store))
}

/// Creates a session with a fixed timestamp.
async fn create(
    sessions: &SessionStore,
    id: &str,
    owner: &str,
    now: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    sessions
        .create(NewSession {
            id: id.to_owned(),
            name: format!("{id} name"),
            endpoint_url: "http://127.0.0.1:11434/v1".to_owned(),
            model: "llama3".to_owned(),
            owner: owner.to_owned(),
            rag: false,
            now,
        })
        .await?;
    Ok(())
}

/// Appends one message.
async fn add(
    sessions: &SessionStore,
    session_id: &str,
    n: usize,
    role: &str,
    now: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    sessions
        .add_message(
            session_id,
            NewMessage::new(
                format!("msg-{session_id}-{n}"),
                role,
                format!("body {n}"),
                now,
            ),
        )
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// T0200 — CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_and_read_back_a_session() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;

    let meta = sessions.get("s1").await?.expect("session must exist");
    assert_eq!(meta.id.as_ref(), "s1");
    assert_eq!(meta.owner.as_ref(), "alice");
    assert_eq!(meta.model.as_ref(), "llama3");
    assert_eq!(meta.message_count, 0, "a new session has no messages");
    assert!(!meta.archived && !meta.is_important);
    Ok(())
}

#[tokio::test]
async fn a_missing_session_is_none_not_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    assert!(sessions.get("nope").await?.is_none());
    Ok(())
}

#[tokio::test]
async fn creating_the_same_id_twice_is_an_error_not_a_silent_overwrite()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "dup", "alice", 1_000.0).await?;
    // Python's `create_session` would raise on the second insert too, but the
    // caller could not distinguish it from a crash; here it is a typed error.
    assert!(create(&sessions, "dup", "alice", 1_000.0).await.is_err());
    Ok(())
}

#[tokio::test]
async fn rename_and_flags_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;

    sessions.rename("s1", "renamed", 2_000.0).await?;
    sessions.set_important("s1", true, 3_000.0).await?;
    sessions.set_archived("s1", true, 4_000.0).await?;
    sessions.touch("s1", 5_000.0).await?;

    let meta = sessions.get("s1").await?.expect("exists");
    assert_eq!(meta.name.as_ref(), "renamed");
    assert!(meta.is_important);
    assert!(meta.archived);
    // `last_accessed` round-trips through SQLite as a REAL. The value was written
    // as the exact double 5_000.0 and comes back bit-identical, so an epsilon
    // comparison would only obscure that; the cast to bits asserts exactness
    // without tripping `float_cmp`.
    assert_eq!(
        meta.last_accessed.to_bits(),
        5_000.0_f64.to_bits(),
        "touch must set last_accessed to exactly the timestamp it was given"
    );
    Ok(())
}

#[tokio::test]
async fn list_is_scoped_to_owner_and_empty_owner_means_all()
-> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "a", "alice", 1_000.0).await?;
    create(&sessions, "b", "bob", 2_000.0).await?;

    let alice = sessions.list("alice").await?;
    assert_eq!(alice.len(), 1, "alice must not see bob's session");
    assert_eq!(alice[0].id.as_ref(), "a");

    let everyone = sessions.list("").await?;
    assert_eq!(everyone.len(), 2, "an empty owner means every owner");
    Ok(())
}

#[tokio::test]
async fn deleting_a_session_cascades_to_its_messages() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;
    add(&sessions, "s1", 0, "user", 1_100.0).await?;

    let remaining = sessions.count_message_rows("s1").await?;
    assert_eq!(remaining, 1, "setup: the message exists");

    assert_eq!(sessions.delete("s1").await?, 1);
    assert!(sessions.get("s1").await?.is_none());
    assert_eq!(
        sessions.count_message_rows("s1").await?,
        0,
        "ON DELETE CASCADE must remove the messages"
    );
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
#[tokio::test]
async fn message_count_always_equals_the_row_count() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;

    for n in 0..5 {
        add(
            &sessions,
            "s1",
            n,
            if n % 2 == 0 { "user" } else { "assistant" },
            1_100.0,
        )
        .await?;
        let column = sessions.message_count("s1").await?;
        let rows = sessions.count_message_rows("s1").await?;
        assert_eq!(
            column,
            rows,
            "after {} messages the column and the rows must agree",
            n + 1
        );
    }
    assert_eq!(sessions.message_count("s1").await?, 5);
    Ok(())
}

/// A write to a deleted session must fail loudly, not silently drop the message.
///
/// Python's `_persist_message` returns early with a warning
/// (`session_manager.py:249-255`) and `add_message` has already appended to
/// memory, so the caller sees neither an error nor a row. T0202 requires the
/// error to be visible.
#[tokio::test]
async fn writing_to_a_missing_session_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    assert!(
        add(&sessions, "ghost", 0, "user", 1_000.0).await.is_err(),
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
#[tokio::test]
async fn a_failed_write_does_not_change_the_count() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;
    add(&sessions, "s1", 0, "user", 1_100.0).await?;
    let before = sessions.message_count("s1").await?;

    // A duplicate primary key. `chat_messages.id` is `PRIMARY KEY (id)`
    // (0001_baseline.sql:92), so this insert must fail inside the transaction and
    // take the recount with it.
    let duplicate = sessions
        .add_message(
            "s1",
            NewMessage::new("msg-s1-0", "user", "duplicate", 1_200.0),
        )
        .await;
    assert!(
        duplicate.is_err(),
        "a duplicate message id must be an error"
    );

    let after = sessions.message_count("s1").await?;
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
#[tokio::test]
async fn truncate_counts_the_rows_that_actually_remain() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;
    for n in 0..3 {
        add(
            &sessions,
            "s1",
            n,
            "user",
            1_100.0 + f64::from(u16::try_from(n).unwrap_or(u16::MAX)),
        )
        .await?;
    }

    // Ask for more than exist, exactly as the Python's default does.
    let removed = sessions.truncate("s1", 10, 2_000.0).await?;
    assert_eq!(
        removed, 0,
        "nothing to remove when the cap exceeds the history"
    );
    assert_eq!(
        sessions.message_count("s1").await?,
        3,
        "the count must reflect the rows that remain, not keep_count"
    );

    sessions.truncate("s1", 1, 3_000.0).await?;
    assert_eq!(sessions.message_count("s1").await?, 1);
    Ok(())
}

/// Replacing history sets the count to the new length.
#[tokio::test]
async fn replace_messages_resets_the_count() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store);
    create(&sessions, "s1", "alice", 1_000.0).await?;
    for n in 0..4 {
        add(
            &sessions,
            "s1",
            n,
            "user",
            1_100.0 + f64::from(u16::try_from(n).unwrap_or(u16::MAX)),
        )
        .await?;
    }
    assert_eq!(sessions.message_count("s1").await?, 4);

    let messages: Vec<NewMessage> = (0..2)
        .map(|n| {
            NewMessage::new(
                format!("new-{n}"),
                "user",
                format!("fresh {n}"),
                f64::from(n),
            )
        })
        .collect();
    sessions.replace_messages("s1", messages, 5_000.0).await?;
    assert_eq!(
        sessions.message_count("s1").await?,
        2,
        "a replacement must leave the count equal to the new length"
    );
    assert_eq!(sessions.count_message_rows("s1").await?, 2);
    Ok(())
}

// ---------------------------------------------------------------------------
// T0204 — the sweeper
// ---------------------------------------------------------------------------

/// The sweeper's decisions are a pure function of the row, so they can be
/// asserted without a database.
///
/// Each case below is one branch of `classify`. The order is the interesting
/// part: a pinned session is kept *even if* it is also empty and incognito, so
/// `is_important` is checked first. A pin is the user's explicit "do not touch
/// this" and nothing may override it.
#[test]
fn classify_keeps_pinned_and_fresh_sessions() {
    let now = 100_000.0;
    let row =
        |name: &str, important: bool, count: i64, touched: f64, first: Option<&str>| SweepRow {
            id: "s".to_owned(),
            name: name.to_owned(),
            is_important: important,
            message_count: count,
            last_touched: touched,
            first_user_message: first.map(str::to_owned),
        };

    // Pinned wins over every other rule, including Incognito and empty.
    assert_eq!(
        classify(&row("Incognito", true, 0, 0.0, None), now),
        (SweepAction::Keep, SweepReason::Important)
    );
    // Inside the grace window: kept, whatever else is true.
    assert_eq!(
        classify(&row("test", false, 0, now - 1.0, None), now),
        (SweepAction::Keep, SweepReason::RecentlyActive)
    );
    // Past the grace window and empty: deleted.
    assert_eq!(
        classify(&row("Real work", false, 0, now - 5_000.0, None), now),
        (SweepAction::Delete, SweepReason::Empty)
    );
    // Past grace, short, junk name: deleted.
    assert_eq!(
        classify(&row("test", false, 3, now - 5_000.0, None), now),
        (SweepAction::Delete, SweepReason::ThrowawayName)
    );
    // A single unanswered user message is throwaway whatever the session is
    // named — but only once the junk-name check has passed, so the first message
    // here is deliberately *not* one of `THROWAWAY_NAMES`. With a junk first
    // message it is `ThrowawayName`, and that ordering is the Python's.
    assert_eq!(
        classify(
            &row(
                "Weather",
                false,
                1,
                now - 5_000.0,
                Some("what is the weather")
            ),
            now
        ),
        (SweepAction::Delete, SweepReason::Unanswered)
    );
    // Same shape, but the junk first message is caught first.
    assert_eq!(
        classify(&row("Weather", false, 1, now - 5_000.0, Some("hello")), now),
        (SweepAction::Delete, SweepReason::ThrowawayName)
    );
    // Long enough with a real name: kept. This is the case that must *not* change.
    assert_eq!(
        classify(&row("Weather", false, 9, now - 5_000.0, Some("hello")), now),
        (SweepAction::Keep, SweepReason::RecentlyActive)
    );
}

/// The grace window is `FRESH_GRACE_SECS` wide and inclusive at the edge.
#[test]
fn the_grace_window_boundary_is_inclusive() {
    let now = 100_000.0;
    assert!(is_recently_active(now - FRESH_GRACE_SECS, now));
    assert!(!is_recently_active(now - FRESH_GRACE_SECS - 1.0, now));
    // A timestamp in the future is treated as recent — fail-safe, per the Python's
    // `if value >= now: return True`.
    assert!(is_recently_active(now + 500.0, now));
}

/// A sweep deletes what it should, leaves the rest, and reports honestly.
///
/// Runs through the async [`sweep_sessions`] entry point rather than
/// `sweep_blocking`, so this also pins that the transactional sweep really does
/// run off the async runtime (AGENTS.md §6) rather than blocking an executor.
#[tokio::test]
async fn a_sweep_deletes_only_what_it_should() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    let now = 100_000.0;

    create(&sessions, "keep-fresh", "alice", now).await?;
    create(&sessions, "keep-pinned", "alice", now - 5_000.0).await?;
    create(&sessions, "drop-empty", "alice", now - 5_000.0).await?;
    create(&sessions, "drop-junk", "alice", now - 5_000.0).await?;
    sessions
        .set_important("keep-pinned", true, now - 5_000.0)
        .await?;
    // `drop-junk` needs a junk *name*; `create` derives the name from the id.
    sessions.rename("drop-junk", "test", now - 5_000.0).await?;

    let report = sweep_sessions(store.clone(), "alice", now).await?;
    assert_eq!(report.examined, 4, "every unarchived session is examined");
    assert_eq!(report.deleted, 2, "the empty one and the junk-named one");

    assert!(sessions.get("keep-fresh").await?.is_some(), "fresh is kept");
    assert!(
        sessions.get("keep-pinned").await?.is_some(),
        "pinned is kept"
    );
    assert!(
        sessions.get("drop-empty").await?.is_none(),
        "empty is deleted"
    );
    assert!(
        sessions.get("drop-junk").await?.is_none(),
        "junk is deleted"
    );
    Ok(())
}

/// The dry run must agree with the real sweep, or it is worse than useless.
///
/// An operator who previews a sweep and then runs it has to get the same answer,
/// so `preview_sessions` is checked against `sweep_sessions` on the same fixture.
#[tokio::test]
async fn the_dry_run_predicts_the_sweep_exactly() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    let now = 100_000.0;
    create(&sessions, "a", "alice", now - 5_000.0).await?;
    create(&sessions, "b", "alice", now - 5_000.0).await?;
    create(&sessions, "c", "alice", now - 5_000.0).await?;
    sessions.set_important("c", true, now - 5_000.0).await?;

    let predicted: Vec<String> = preview_sessions(store.clone(), "alice", now)
        .await?
        .into_iter()
        .filter(|(_, action, _)| *action == SweepAction::Delete)
        .map(|(id, _, _)| id)
        .collect();

    let report = sweep_sessions(store.clone(), "alice", now).await?;
    assert_eq!(
        predicted.len(),
        report.deleted,
        "the preview promised {} deletions but the sweep performed {}",
        predicted.len(),
        report.deleted
    );
    for id in predicted {
        assert!(sessions.get(&id).await?.is_none(), "{id} was deleted");
    }
    Ok(())
}

/// A sweep is scoped to one owner and never touches another's sessions.
#[tokio::test]
async fn a_sweep_is_scoped_to_one_owner() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    let now = 100_000.0;
    create(&sessions, "mine", "alice", now - 5_000.0).await?;
    create(&sessions, "theirs", "bob", now - 5_000.0).await?;

    let report = sweep_sessions(store.clone(), "alice", now).await?;
    assert_eq!(report.examined, 1, "bob's session is not alice's to sweep");
    assert!(sessions.get("theirs").await?.is_some());
    Ok(())
}

/// A brand-new session must survive the sweep. This is a regression test.
///
/// The sweep's `last_touched` originally took the max of only `last_message_at`
/// and `last_accessed`, but a session created by `SessionStore::create` has
/// `last_accessed = NULL` — nothing has ever opened it — so `COALESCE` yielded 0
/// and the row read as infinitely old. An empty session created one second
/// earlier was therefore deleted on the next sweep. The Python avoids this by
/// checking all four of `last_message_at`, `last_accessed`, `updated_at` and
/// `created_at` (`session_actions.py:44-48`), and so does this now.
#[tokio::test]
async fn a_brand_new_empty_session_survives_the_sweep() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    let now = 100_000.0;
    create(&sessions, "just-created", "alice", now).await?;

    let report = sweep_sessions(store.clone(), "alice", now).await?;
    assert_eq!(report.examined, 1);
    assert_eq!(
        report.deleted, 0,
        "a session inside the grace window must not be swept"
    );
    assert!(sessions.get("just-created").await?.is_some());
    Ok(())
}

/// A filename that escapes the images root is refused, not unlinked.
///
/// This is the directory-traversal guard. The name arrives inside a message's
/// `metadata` JSON, which is attacker-influenceable content, so a crafted
/// message could otherwise make session-delete unlink a file anywhere the
/// process can write. The test creates a real file outside the root and asserts
/// it survives.
#[tokio::test]
async fn a_traversing_filename_is_never_unlinked() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join("generated_images");
    std::fs::create_dir_all(&root)?;
    let victim = dir.path().join("victim.txt");
    std::fs::write(&victim, b"do not delete me")?;

    let cleanup = ImageCleanup::new(&root);
    // Every one of these is the same attack in a different shape.
    for name in [
        "../victim.txt",
        "../../victim.txt",
        "./victim.txt",
        "..",
        "sub/child.png",
        "",
    ] {
        assert!(
            cleanup.safe_path(name)?.is_none(),
            "{name:?} must be rejected as a traversal attempt"
        );
    }
    assert!(
        victim.exists(),
        "the file outside the root must be untouched"
    );
    Ok(())
}

/// Deleting a session deactivates its gallery rows and unlinks its files.
///
/// Also pins the **order**: the image rows are found by `session_id`, and
/// `gallery_images.session_id` is `ON DELETE SET NULL`. Deleting the session row
/// first would null that column and the cleanup would find nothing, stranding
/// every file on disk forever.
#[tokio::test]
async fn deleting_a_session_sweeps_its_images() -> Result<(), Box<dyn std::error::Error>> {
    let (dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    let root = dir.path().join("generated_images");
    std::fs::create_dir_all(&root)?;
    let image = root.join("pic.png");
    std::fs::write(&image, b"png")?;

    create(&sessions, "s1", "alice", 1_000.0).await?;
    add(&sessions, "s1", 0, "tool", 1_100.0).await?;

    // A gallery row *and* a metadata-derived reference naming the same file, so
    // both collection sources are exercised and the dedup is visible.
    let conn = store.connect()?;
    conn.execute(
        "INSERT INTO gallery_images (id, filename, prompt, session_id, is_active, created_at, updated_at)
         VALUES ('g1', 'pic.png', 'a cat', 's1', 1, 0, 0)",
        [],
    )?;
    conn.execute(
        "UPDATE chat_messages SET metadata = ?2 WHERE id = ?1",
        rusqlite::params![
            "msg-s1-0",
            r#"{"tool_events":[{"image_url":"/api/generated-image/pic.png"}]}"#
        ],
    )?;
    drop(conn);

    let cleanup = ImageCleanup::new(&root);
    let (deleted, report) = sessions.delete_with_image_cleanup("s1", &cleanup).await?;

    assert_eq!(deleted, 1, "the session row is gone");
    assert_eq!(report.rows_deactivated, 1, "the gallery row is deactivated");
    assert_eq!(
        report.files_removed, 1,
        "the file is unlinked once, despite being named by two sources"
    );
    assert!(
        !image.exists(),
        "the generated file must not survive the delete"
    );
    Ok(())
}

/// A second pass over an already-deleted session is a no-op, not an error.
///
/// Session deletion can be retried after a partial failure, so the cleanup has to
/// tolerate finding nothing. Python returns `0` here by catching everything; here
/// it is a real, typed result reporting zero work.
#[tokio::test]
async fn cleaning_up_twice_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
    let (dir, store) = store()?;
    let sessions = SessionStore::new(store.clone());
    create(&sessions, "s1", "alice", 1_000.0).await?;

    let cleanup = ImageCleanup::new(dir.path().join("generated_images"));
    let (deleted, report) = sessions.delete_with_image_cleanup("s1", &cleanup).await?;
    assert_eq!(deleted, 1);
    assert_eq!(report.rows_deactivated, 0, "the session had no images");

    let (deleted, report) = sessions.delete_with_image_cleanup("s1", &cleanup).await?;
    assert_eq!(deleted, 0, "nothing left to delete");
    assert_eq!(report.rows_deactivated, 0);
    Ok(())
}

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

/// A zero capacity is clamped to one, because a cache that cannot store its own
/// key is not a cache.
#[test]
fn capacity_is_clamped_to_at_least_one() {
    // The default cap must be a real bound. Asserted at compile time as well as
    // below: a `0` here would make the whole eviction test vacuous.
    const { assert!(SessionCache::MAX_ENTRIES > 0) };
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
///
/// Every DB-touching test is `#[tokio::test]`: the store methods are `async`
/// because `rusqlite` is synchronous and must run on the blocking pool
/// (AGENTS.md §6, D11). The cache-only tests above stay `#[test]` — they are pure
/// in-memory and do not need a runtime.
#[tokio::test]
async fn the_store_populates_and_invalidates_the_cache() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, store) = store()?;
    let sessions = SessionStore::with_cache(store, Arc::new(SessionCache::with_capacity(4)));
    create(&sessions, "s1", "alice", 1_000.0).await?;

    let _ = sessions.get("s1").await?;
    assert_eq!(sessions.cache().len(), 1, "a read populates the cache");

    // A write changes the row, so the cached copy must go rather than be served.
    sessions.rename("s1", "renamed", 2_000.0).await?;
    assert_eq!(
        sessions.cache().len(),
        0,
        "a write must invalidate the cached copy"
    );

    let meta = sessions.get("s1").await?.expect("exists");
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
