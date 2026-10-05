//! Housekeeping for empty and stale sessions.
//!
//! Ported from `cleanup_empty_sessions` (`core/session_manager.py:731-782`).
//!
//! # Two rules, and the NULL hazard between them
//!
//! The Python deletes an *empty* session once it is older than `min_age_hours`,
//! and archives a *non-empty* one once it has not been accessed for
//! `auto_archive_days`. Both compare a nullable column against a cutoff, and
//! the two sides deliberately disagree:
//!
//! * `created_at IS NULL` skips the age check entirely, so the row is deleted
//!   whatever its age (`session_manager.py:754-760` — the `if created_at is not
//!   None` guard).
//! * `last_accessed IS NULL` makes the session **never** archive, because the
//!   Python's `and db_session.last_accessed` short-circuits on `None`
//!   (`session_manager.py:768`).
//!
//! That asymmetry is easy to lose in SQL, and losing it is a data-loss bug in
//! the first case and a spurious-archive bug in the second. Coalescing either
//! column to `0` is precisely wrong: `0` is before every cutoff, so a session
//! that has never been touched would look ancient and be archived. The columns
//! are therefore read as [`Option<f64>`] and the null cases are decided in Rust,
//! where they can be tested without a database.
//!
//! No wall clock: `now` is passed in, so the boundaries are exact and the tests
//! need no sleep (AGENTS.md §11).

use {rusqlite::Connection, serde::Serialize};

use crate::{Store, StoreError, blocking::with_conn};

/// Seconds in a day, as `f64` to match the timestamp columns.
const SECS_PER_DAY: f64 = 86_400.0;
/// Seconds in an hour, as `f64`.
const SECS_PER_HOUR: f64 = 3_600.0;

/// How aggressive the cleanup pass is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupPolicy {
    /// Archive a non-empty, unarchived session after this many days without
    /// access.
    pub auto_archive_days: u32,
    /// Do not delete an empty session until it is at least this many hours old.
    ///
    /// Exists to stop the sweeper deleting a session the user has just opened.
    pub min_age_hours: u32,
}

impl Default for CleanupPolicy {
    /// The Python's own defaults (`session_manager.py:731`).
    fn default() -> Self {
        Self {
            auto_archive_days: 30,
            min_age_hours: 1,
        }
    }
}

impl CleanupPolicy {
    /// The age below which a session is archived: `now - auto_archive_days`.
    #[must_use]
    pub fn archive_cutoff(&self, now: f64) -> f64 {
        now - f64::from(self.auto_archive_days) * SECS_PER_DAY
    }

    /// The age below which an empty session is left alone: `now - min_age_hours`.
    #[must_use]
    pub fn min_age(&self, now: f64) -> f64 {
        now - f64::from(self.min_age_hours) * SECS_PER_HOUR
    }
}

/// What a cleanup pass did, mirroring the Python's `stats` dict.
///
/// Serialised because the Python returns this straight to a caller and the web
/// layer renders the three counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct CleanupStats {
    /// Sessions examined.
    pub total_checked: usize,
    /// Empty sessions deleted.
    pub deleted_empty: usize,
    /// Stale sessions archived.
    pub archived_old: usize,
}

/// A session row, as the cleanup pass needs it.
///
/// A projection rather than the full [`crate::sessions::SessionMeta`], so the
/// pass reads only the six columns it uses.
#[derive(Debug, Clone, PartialEq)]
pub struct CleanupRow {
    /// Primary key.
    pub id: String,
    /// Archived already; an archived session is never re-examined.
    pub archived: bool,
    /// User-pinned: never auto-archived.
    pub is_important: bool,
    /// Message count, authoritative per T0202.
    pub message_count: i64,
    /// Creation time, or `None` when the column is NULL.
    ///
    /// `None` means *no age check*, so the row is deleted regardless of age.
    pub created_at: Option<f64>,
    /// Last access, or `None` when the column is NULL.
    ///
    /// `None` means *never archive*: a session nobody has touched is not
    /// evidence of a stale session.
    pub last_accessed: Option<f64>,
}

/// What to do with one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupAction {
    /// Delete the row.
    Delete,
    /// Set `archived = 1`.
    Archive,
    /// Leave it alone.
    Keep,
}

/// Why a session was kept, deleted or archived.
///
/// A named reason per branch: a sweeper that can only report "nothing
/// happened" is a sweeper whose rules are untestable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupReason {
    /// Empty and old enough to delete.
    EmptyOld,
    /// Empty but younger than `min_age_hours`.
    TooYoung,
    /// Non-empty, untouched for longer than `auto_archive_days`.
    Stale,
    /// `last_accessed` is NULL, so there is no evidence it is stale.
    NeverAccessed,
    /// Already archived.
    AlreadyArchived,
    /// User-pinned.
    Important,
    /// Has messages and is recent enough.
    Active,
}

/// Classifies one session, without touching the database.
///
/// Split from the pass so the policy is testable on its own — a sweeper whose
/// rules can only be observed by running it against a database is a sweeper
/// whose rules do not get tested.
///
/// The branch order is the Python's own (`session_manager.py:752-772`): the
/// empty case is an `if`/`elif`, so an empty session is never also archived.
#[must_use]
pub fn classify(
    row: &CleanupRow,
    policy: &CleanupPolicy,
    now: f64,
) -> (CleanupAction, CleanupReason) {
    if row.message_count == 0 {
        // `if created_at is not None: … if created > min_age: continue`
        // A NULL `created_at` skips straight to the delete.
        let too_young = row
            .created_at
            .is_some_and(|created| created > policy.min_age(now));
        return if too_young {
            (CleanupAction::Keep, CleanupReason::TooYoung)
        } else {
            (CleanupAction::Delete, CleanupReason::EmptyOld)
        };
    }

    // Past this point the row is non-empty, matching the Python's `elif`.
    if row.is_important {
        return (CleanupAction::Keep, CleanupReason::Important);
    }
    if row.archived {
        return (CleanupAction::Keep, CleanupReason::AlreadyArchived);
    }
    // `and db_session.last_accessed` — a NULL short-circuits to "keep".
    let Some(last_accessed) = row.last_accessed else {
        return (CleanupAction::Keep, CleanupReason::NeverAccessed);
    };
    if last_accessed < policy.archive_cutoff(now) {
        (CleanupAction::Archive, CleanupReason::Stale)
    } else {
        (CleanupAction::Keep, CleanupReason::Active)
    }
}

/// Reads every session the cleanup pass will consider.
///
/// The nullable columns are read as [`Option<f64>`] rather than coalesced; see
/// the module documentation for why that distinction is load bearing.
///
/// # Errors
///
/// Returns [`StoreError`] if the query fails.
pub fn load_rows(conn: &Connection) -> Result<Vec<CleanupRow>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT id, archived, is_important, message_count, created_at, last_accessed
           FROM sessions",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(CleanupRow {
            id: row.get::<_, String>(0)?,
            archived: row.get::<_, Option<bool>>(1)?.unwrap_or(false),
            is_important: row.get::<_, Option<bool>>(2)?.unwrap_or(false),
            message_count: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
            created_at: row.get::<_, Option<f64>>(4)?,
            last_accessed: row.get::<_, Option<f64>>(5)?,
        })
    })?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Applies one cleanup pass, deleting empty sessions and archiving stale ones.
///
/// `now` is the caller's clock; nothing here reads the wall clock.
///
/// # Errors
///
/// Returns [`StoreError`] on a query, transaction or commit failure.
pub async fn cleanup_empty_sessions(
    store: Store,
    policy: CleanupPolicy,
    now: f64,
) -> Result<CleanupStats, StoreError> {
    with_conn(store, move |conn| cleanup_blocking(conn, &policy, now)).await
}

/// The synchronous body, split out so the transaction is obvious and testable.
///
/// # Errors
///
/// Returns [`StoreError`] on a query, transaction or commit failure.
pub fn cleanup_blocking(
    conn: &Connection,
    policy: &CleanupPolicy,
    now: f64,
) -> Result<CleanupStats, StoreError> {
    let rows = load_rows(conn)?;
    let mut stats = CleanupStats {
        total_checked: rows.len(),
        ..CleanupStats::default()
    };
    let mut to_delete: Vec<&str> = Vec::new();
    let mut to_archive: Vec<&str> = Vec::new();

    for row in &rows {
        match classify(row, policy, now).0 {
            CleanupAction::Delete => to_delete.push(&row.id),
            CleanupAction::Archive => to_archive.push(&row.id),
            CleanupAction::Keep => {}
        }
    }

    // One transaction for the whole pass: a sweep that deletes half its rows
    // and then fails leaves the store in a state the next sweep will not
    // recognise.
    conn.execute_batch("BEGIN")?;
    let applied = apply_decisions(conn, &to_delete, &to_archive, now);
    match applied {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(err) => {
            // Roll back, then report the original failure rather than the
            // rollback's.
            let _ = conn.execute_batch("ROLLBACK");
            return Err(err);
        }
    }

    stats.deleted_empty = to_delete.len();
    stats.archived_old = to_archive.len();
    Ok(stats)
}

/// Runs the deletes and archives inside the caller's transaction.
///
/// # Errors
///
/// Returns [`StoreError`] if a statement fails.
fn apply_decisions(
    conn: &Connection,
    to_delete: &[&str],
    to_archive: &[&str],
    now: f64,
) -> Result<(), StoreError> {
    for id in to_delete {
        // `chat_messages` cascades from `sessions`; the image files do not,
        // so this only removes the rows.
        conn.execute("DELETE FROM sessions WHERE id = ?1", [id])?;
    }
    for id in to_archive {
        conn.execute(
            "UPDATE sessions SET archived = 1, updated_at = ?2 WHERE id = ?1",
            rusqlite::params![id, now],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CleanupAction, CleanupPolicy, CleanupReason, CleanupRow, SECS_PER_DAY, SECS_PER_HOUR,
        classify,
    };

    /// A fixed clock so every boundary below is an exact number of seconds,
    /// not "roughly 30 days".
    const NOW: f64 = 1_800_000_000.0;

    fn row(message_count: i64, created_at: Option<f64>, last_accessed: Option<f64>) -> CleanupRow {
        CleanupRow {
            id: "s1".to_string(),
            archived: false,
            is_important: false,
            message_count,
            created_at,
            last_accessed,
        }
    }

    /// An active, non-empty session: the common case, and the one that must
    /// never be touched.
    #[test]
    fn a_recent_non_empty_session_is_kept() {
        let r = row(3, Some(NOW - 10.0 * SECS_PER_DAY), Some(NOW - 60.0));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::Active)
        );
    }

    // -- the archive boundary, which the task calls out explicitly ------------

    /// 29 days 23 hours is *inside* the window: not archived.
    #[test]
    fn twenty_nine_days_twenty_three_hours_is_not_archived() {
        let stale_ish = NOW - (29.0 * SECS_PER_DAY + 23.0 * SECS_PER_HOUR);
        let r = row(3, Some(NOW - 60.0 * SECS_PER_DAY), Some(stale_ish));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::Active),
            "29d23h is newer than the 30d cutoff and must be kept"
        );
    }

    /// 30 days 1 hour is *outside* the window: archived.
    #[test]
    fn thirty_days_one_hour_is_archived() {
        let stale = NOW - (30.0 * SECS_PER_DAY + SECS_PER_HOUR);
        let r = row(3, Some(NOW - 60.0 * SECS_PER_DAY), Some(stale));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Archive, CleanupReason::Stale),
            "30d1h is older than the 30d cutoff and must be archived"
        );
    }

    /// The comparison is strict `<`, so a session sitting exactly on the cutoff
    /// is kept. Pinned because changing it to `<=` would archive a session on
    /// the very pass that put it there.
    #[test]
    fn exactly_on_the_cutoff_is_kept() {
        let r = row(
            3,
            Some(NOW - 60.0 * SECS_PER_DAY),
            Some(NOW - 30.0 * SECS_PER_DAY),
        );
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::Active)
        );
    }

    // -- the delete boundary --------------------------------------------------

    #[test]
    fn an_empty_session_older_than_one_hour_is_deleted() {
        let r = row(0, Some(NOW - (SECS_PER_HOUR + 60.0)), Some(NOW - 7200.0));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Delete, CleanupReason::EmptyOld)
        );
    }

    #[test]
    fn an_empty_session_younger_than_one_hour_is_kept() {
        let r = row(0, Some(NOW - 1800.0), Some(NOW - 1800.0));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::TooYoung),
            "the min-age guard is what stops the sweeper eating a fresh session"
        );
    }

    #[test]
    fn an_empty_session_exactly_one_hour_old_is_deleted() {
        // `created > min_age` is strict, so exactly one hour old is not "too
        // young" and the row goes.
        let r = row(0, Some(NOW - SECS_PER_HOUR), Some(NOW - SECS_PER_HOUR));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Delete, CleanupReason::EmptyOld)
        );
    }

    // -- the NULL asymmetry ---------------------------------------------------

    /// A NULL `created_at` skips the age check, so the row is deleted however
    /// new it looks.
    #[test]
    fn a_null_created_at_skips_the_age_check() {
        let r = row(0, None, Some(NOW));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Delete, CleanupReason::EmptyOld),
            "the Python's `if created_at is not None` guard means no age check"
        );
    }

    /// A NULL `last_accessed` must **not** archive. This is the trap: a
    /// `COALESCE(last_accessed, 0)` would make an untouched session read as
    /// `0`, which is before every cutoff, and archive it.
    #[test]
    fn a_null_last_accessed_is_never_archived() {
        let r = row(3, Some(NOW - 400.0 * SECS_PER_DAY), None);
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::NeverAccessed),
            "coalescing last_accessed to 0 here would archive a live session"
        );
    }

    #[test]
    fn a_null_last_accessed_is_still_deleted_when_empty() {
        // The empty branch runs first, so the null guard does not rescue it.
        let r = row(0, Some(NOW - 7200.0), None);
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Delete, CleanupReason::EmptyOld)
        );
    }

    // -- the other keeps ------------------------------------------------------

    #[test]
    fn a_pinned_session_is_never_archived() {
        let mut r = row(
            3,
            Some(NOW - 400.0 * SECS_PER_DAY),
            Some(NOW - 400.0 * SECS_PER_DAY),
        );
        r.is_important = true;
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::Important)
        );
    }

    #[test]
    fn an_already_archived_session_is_left_alone() {
        let mut r = row(
            3,
            Some(NOW - 400.0 * SECS_PER_DAY),
            Some(NOW - 400.0 * SECS_PER_DAY),
        );
        r.archived = true;
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Keep, CleanupReason::AlreadyArchived)
        );
    }

    #[test]
    fn a_pinned_empty_session_is_still_deleted() {
        // The Python checks emptiness first and never consults `is_important`
        // on that branch, so a pinned empty session does go. Pinned because the
        // intuitive behaviour is the opposite, and a "fix" here would silently
        // diverge from the Python.
        let mut r = row(0, Some(NOW - 7200.0), Some(NOW - 7200.0));
        r.is_important = true;
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Delete, CleanupReason::EmptyOld)
        );
    }

    /// A negative count is *not* treated as empty.
    ///
    /// The Python tests `message_count == 0` exactly, and widening that to
    /// `<= 0` would be an unrequested behaviour change on data that T0202
    /// makes authoritative and non-negative. Pinned so the decision is
    /// deliberate rather than accidental: such a row takes the archive branch.
    #[test]
    fn a_negative_message_count_is_not_treated_as_empty() {
        let r = row(-1, Some(NOW - 7200.0), Some(NOW - 400.0 * SECS_PER_DAY));
        assert_eq!(
            classify(&r, &CleanupPolicy::default(), NOW),
            (CleanupAction::Archive, CleanupReason::Stale)
        );
    }

    #[test]
    fn the_default_policy_is_thirty_days_and_one_hour() {
        let p = CleanupPolicy::default();
        assert_eq!(p.auto_archive_days, 30);
        assert_eq!(p.min_age_hours, 1);
        assert!((p.archive_cutoff(NOW) - (NOW - 30.0 * SECS_PER_DAY)).abs() < f64::EPSILON);
        assert!((p.min_age(NOW) - (NOW - SECS_PER_HOUR)).abs() < f64::EPSILON);
    }

    #[test]
    fn a_stricter_policy_archives_earlier() {
        let strict = CleanupPolicy {
            auto_archive_days: 7,
            min_age_hours: 24,
        };
        let r = row(
            3,
            Some(NOW - 30.0 * SECS_PER_DAY),
            Some(NOW - 10.0 * SECS_PER_DAY),
        );
        assert_eq!(
            classify(&r, &strict, NOW),
            (CleanupAction::Archive, CleanupReason::Stale)
        );
    }
}
