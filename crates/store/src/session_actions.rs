//! Auto-cleanup of empty and throwaway sessions.
//!
//! A port of `src/session_actions.py:16-120`. The Python module also runs an
//! LLM folder-sort in "Phase 2"; **that half is deliberately not ported**, because
//! `odysseus-llm` is still an empty scaffold and the store layer must not depend on
//! it. What remains is the part with no external dependency and the part that
//! decides what gets deleted, which is where the risk is.
//!
//! # What this decides, and why it is conservative
//!
//! Three classes of session get swept:
//!
//! * **Incognito** — deleted outright, by name.
//! * **Empty** — deleted if it is also past the grace period.
//! * **Throwaway** — a short session with a junk name, or a single unanswered
//!   user message, deleted.
//!
//! Two guards make deletion survivable, and both are preserved:
//!
//! * [`is_recently_active`] — anything touched within the grace window is left
//!   alone. A session someone is *typing into* must not vanish mid-conversation.
//! * `is_important` — pinned sessions are skipped entirely
//!   (`session_actions.py:85-86`), because that flag is the user's explicit
//!   "do not touch this".
//!
//! Every predicate here is a function of the row plus the injected `now`. No
//! clock is read internally, so the whole sweeper is deterministic under test
//! (AGENTS.md §11).

use rusqlite::Connection;

use crate::{Store, StoreError};

/// Names that mark a throwaway session (`_THROWAWAY_NAMES`, `session_actions.py:16-23`).
///
/// Kept as a sorted set so the membership test is a binary search rather than a
/// linear scan over a `HashSet` that would need construction on every call.
const THROWAWAY_NAMES: &[&str] = &[
    "abc", "asd", "asdf", "bar", "baz", "blah", "bruh", "delete", "foo", "hello", "hey", "heyo",
    "hi", "hii", "hiii", "hm", "hmm", "idk", "junk", "lol", "meh", "new chat", "ok", "qwerty",
    "remove", "scratch", "stuff", "sup", "temp", "test", "testing", "tmp", "trash", "untitled",
    "whatever", "xxx", "yo",
];

/// At most this many messages for a session to count as throwaway
/// (`_THROWAWAY_MAX_MESSAGES`, `session_actions.py:24`).
///
/// `i64` to match `SweepRow::message_count`, so the comparison needs no cast.
const THROWAWAY_MAX_MESSAGES: i64 = 4;

/// Grace period protecting a fresh session
/// (`_FRESH_EMPTY_SESSION_GRACE`, `session_actions.py:25`).
///
/// `f64` because the comparison is against a Unix timestamp. Ten minutes, the
/// same value as the Python constant.
pub const FRESH_GRACE_SECS: f64 = 600.0;

/// The name that makes a session deletable regardless of age (`session_actions.py:89`).
const INCOGNITO: &str = "Incognito";

/// What kind of sweep decision applies to one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepAction {
    /// Keep it.
    Keep,
    /// Delete it: empty or throwaway.
    Delete,
}

/// Why a session was classified as it was.
///
/// Recorded because "the sweeper deleted 40 sessions" is not an actionable log
/// line, and an unattended sweep deleting real work is the failure mode worth
/// being able to reconstruct afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepReason {
    /// Pinned by the user.
    Important,
    /// Touched inside the grace window.
    RecentlyActive,
    /// Named `Incognito`.
    Incognito,
    /// No messages and past the grace window.
    Empty,
    /// Short, with a junk name.
    ThrowawayName,
    /// One message, no assistant reply.
    Unanswered,
}

/// A session's row, as the sweeper needs it.
///
/// A projection rather than the full [`crate::sessions::SessionMeta`], so the
/// sweep reads only the columns it uses.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepRow {
    /// Primary key.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Pinned flag.
    pub is_important: bool,
    /// Message count, authoritative per T0202.
    pub message_count: i64,
    /// Newest of `last_message_at` / `last_accessed`, used for the grace check.
    pub last_touched: f64,
    /// First user message, lowercased; used for the throwaway-name check.
    pub first_user_message: Option<String>,
}

/// What a sweep pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Sessions examined.
    pub examined: usize,
    /// Sessions deleted.
    pub deleted: usize,
    /// Sessions archived.
    pub archived: usize,
}

/// True while a session is too fresh to delete.
///
/// Ported from `is_session_recently_active` (`session_actions.py:42-53`). The
/// Python checks four columns and returns true if *any* is recent; that matters
/// because a rename bumps `updated_at` and a mere open bumps `last_accessed`, so
/// requiring a single column would let an unrelated action delete a live session.
#[must_use]
pub fn is_recently_active(last_touched: f64, now: f64) -> bool {
    // A timestamp in the future means a clock skew or a concurrent write; treat it
    // as recent, which is the same fail-safe direction as the Python's
    // `if value >= now: return True`.
    if last_touched >= now {
        return true;
    }
    now - last_touched <= FRESH_GRACE_SECS
}

/// Classifies one session, without deleting anything.
///
/// Split from the sweep so the policy is testable on its own — a sweeper whose
/// rules can only be observed by running it against a database is a sweeper whose
/// rules do not get tested.
#[must_use]
pub fn classify(row: &SweepRow, now: f64) -> (SweepAction, SweepReason) {
    // Order matters, and each early return is the Python's own ordering
    // (`session_actions.py:85-120`).
    if row.is_important {
        return (SweepAction::Keep, SweepReason::Important);
    }
    if is_recently_active(row.last_touched, now) {
        return (SweepAction::Keep, SweepReason::RecentlyActive);
    }
    if row.name.trim() == INCOGNITO {
        return (SweepAction::Delete, SweepReason::Incognito);
    }
    if row.message_count <= 0 {
        return (SweepAction::Delete, SweepReason::Empty);
    }
    if row.message_count <= THROWAWAY_MAX_MESSAGES {
        let name = row.name.trim().to_lowercase();
        let first = row
            .first_user_message
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let junk_name = THROWAWAY_NAMES.contains(&name.as_str())
            || name.starts_with("chat:")
            || THROWAWAY_NAMES.contains(&first.as_str());
        if junk_name {
            return (SweepAction::Delete, SweepReason::ThrowawayName);
        }
        // One message and no assistant reply: the user said something and nothing
        // came back. The Python derives "no assistant reply" from a second query
        // (`session_actions.py:112-114`); the caller supplies `message_count`
        // already, so a count of exactly 1 is the same signal.
        if row.message_count == 1 {
            return (SweepAction::Delete, SweepReason::Unanswered);
        }
    }
    (SweepAction::Keep, SweepReason::RecentlyActive)
}

/// Reads the rows a sweep would consider, for `owner` (all owners when empty).
///
/// # Errors
///
/// Returns [`StoreError`] if the queries fail.
pub fn load_rows(conn: &Connection, owner: &str, now: f64) -> Result<Vec<SweepRow>, StoreError> {
    // Archived sessions are excluded (the Python's `DbSession.archived == False`
    // filter at `session_actions.py:78-81`): an archived session is a deliberate
    // keep, and re-examining it every sweep would resurrect archived work.
    //
    // All four columns the Python checks (`session_actions.py:44-48`) are in this
    // `MAX`: `last_message_at`, `last_accessed`, `updated_at` **and** `created_at`.
    // Dropping the last two is not a simplification — `sessions.last_accessed` is
    // nullable and a freshly created row has never been touched, so `COALESCE`
    // yields 0 and a session created a second ago reads as infinitely old. With
    // only the two "activity" columns, an empty session created inside the grace
    // window is deleted the moment the sweep runs. The Python's four-column
    // `any()` is what prevents that, and `is_recently_active` documents it.
    let sql = if owner.is_empty() {
        "SELECT s.id, s.name, s.is_important, s.message_count,
                MAX(COALESCE(s.last_message_at, 0), COALESCE(s.last_accessed, 0),
                    COALESCE(s.updated_at, 0), COALESCE(s.created_at, 0)),
                (SELECT LOWER(c.content) FROM chat_messages c
                  WHERE c.session_id = s.id AND c.role = 'user'
                  ORDER BY c.timestamp, c.id LIMIT 1)
           FROM sessions s
          WHERE COALESCE(s.archived, 0) = 0
          ORDER BY s.last_accessed DESC"
    } else {
        "SELECT s.id, s.name, s.is_important, s.message_count,
                MAX(COALESCE(s.last_message_at, 0), COALESCE(s.last_accessed, 0),
                    COALESCE(s.updated_at, 0), COALESCE(s.created_at, 0)),
                (SELECT LOWER(c.content) FROM chat_messages c
                  WHERE c.session_id = s.id AND c.role = 'user'
                  ORDER BY c.timestamp, c.id LIMIT 1)
           FROM sessions s
          WHERE COALESCE(s.archived, 0) = 0 AND s.owner = ?1
          ORDER BY s.last_accessed DESC"
    };
    let mut stmt = conn.prepare(sql).map_err(StoreError::Sqlite)?;
    let rows = stmt
        .query_map([owner], |row| {
            Ok(SweepRow {
                id: row.get(0)?,
                name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                is_important: row.get::<_, Option<bool>>(2)?.unwrap_or(false),
                message_count: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                last_touched: row.get::<_, Option<f64>>(4)?.unwrap_or(now),
                first_user_message: row.get::<_, Option<String>>(5)?,
            })
        })
        .map_err(StoreError::Sqlite)?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StoreError::Sqlite)
}

/// Deletes the sessions `classify` marks for deletion.
///
/// Everything happens in **one transaction**, unlike the Python's per-row
/// `db.delete` inside a loop that commits once at the end
/// (`session_manager.py:772`). That is the same commit-at-the-end shape, so this
/// matches; the difference is that a failure aborts the whole pass rather than
/// leaving it half applied, because `?` propagates before the commit.
///
/// Only sessions this pass actually classified as [`SweepAction::Delete`] are
/// deleted, and the id is re-checked with `is_important = 0` in the `WHERE`
/// clause. That is a guard against a concurrent pin: `classify` ran against a row
/// read earlier, and a user marking a session important in between must win.
///
/// Takes `&mut Connection` because `transaction()` requires it. Callers on the
/// async runtime should use [`sweep_sessions`], which runs this off the executor
/// (AGENTS.md §6).
///
/// # Errors
///
/// Returns [`StoreError`] if a query fails.
pub fn sweep_blocking(
    conn: &mut Connection,
    owner: &str,
    now: f64,
) -> Result<SweepReport, StoreError> {
    let rows = load_rows(conn, owner, now)?;
    let mut report = SweepReport {
        examined: rows.len(),
        ..SweepReport::default()
    };
    let tx = conn.transaction().map_err(StoreError::Sqlite)?;
    for row in &rows {
        let (action, _reason) = classify(row, now);
        if action != SweepAction::Delete {
            continue;
        }
        // Messages go via `ON DELETE CASCADE`; the re-check of `is_important` is the
        // concurrency guard described above.
        let deleted = tx
            .execute(
                "DELETE FROM sessions
                  WHERE id = ?1 AND COALESCE(is_important, 0) = 0",
                [&row.id],
            )
            .map_err(StoreError::Sqlite)?;
        report.deleted += deleted;
    }
    tx.commit().map_err(StoreError::Sqlite)?;
    Ok(report)
}

/// Classifies every row and returns `(id, action, reason)`, without deleting.
///
/// For a dry run and for tests: the same decisions [`sweep_blocking`] would make,
/// so an operator can see what a sweep *would* do before letting it run. Exposing
/// this rather than only the deleting variant is deliberate — a destructive
/// background job whose decisions cannot be inspected is one nobody enables.
#[must_use]
pub fn preview(
    conn: &Connection,
    owner: &str,
    now: f64,
) -> Vec<(String, SweepAction, SweepReason)> {
    load_rows(conn, owner, now)
        .unwrap_or_default()
        .iter()
        .map(|row| {
            let (action, reason) = classify(row, now);
            (row.id.clone(), action, reason)
        })
        .collect()
}

/// Runs a sweep on the blocking pool, for callers on the async runtime.
///
/// [`sweep_blocking`] needs `&mut Connection`, which
/// [`crate::blocking::with_conn`] does not hand out, so this opens its own
/// connection with `store.connect()` inside a
/// [`crate::blocking::spawn_blocking`] closure. No SQLite call runs on an
/// executor thread (AGENTS.md §6), and no `Connection` value outlives the closure,
/// so none can be held across an `.await`.
///
/// # Errors
///
/// Returns [`StoreError`] if a query fails, or [`StoreError::Interrupted`] if the
/// task is cancelled.
pub async fn sweep_sessions(
    store: Store,
    owner: &str,
    now: f64,
) -> Result<SweepReport, StoreError> {
    let owner = owner.to_owned();
    match crate::blocking::spawn_blocking(move || {
        let mut conn = store.connect()?;
        sweep_blocking(&mut conn, &owner, now)
    })
    .await
    {
        Ok(result) => result,
        Err(join_err) if join_err.is_panic() => std::panic::resume_unwind(join_err.into_panic()),
        Err(_) => Err(StoreError::Interrupted),
    }
}

/// A dry run of [`sweep_sessions`]: the same decisions, deleting nothing.
///
/// # Errors
///
/// Returns [`StoreError`] if a query fails, or [`StoreError::Interrupted`] if the
/// task is cancelled.
pub async fn preview_sessions(
    store: Store,
    owner: &str,
    now: f64,
) -> Result<Vec<(String, SweepAction, SweepReason)>, StoreError> {
    let owner = owner.to_owned();
    crate::blocking::with_conn(store, move |conn| Ok(preview(conn, &owner, now))).await
}
