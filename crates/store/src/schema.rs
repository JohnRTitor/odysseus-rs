//! Schema versioning and the migration chain.
//!
//! Decision D6: the baseline migration is generated from the **live**
//! `data/app.db` `sqlite_master`, not from the Python `SQLAlchemy` models. The
//! live database is ground truth; the models have known drift (see
//! `DATA_MIGRATION.md` §2.2 — `gallery_people` is referenced by the owner sweep
//! but no such table exists, and `ix_sessions_last_message_at` exists in the
//! database but not in the model).
//!
//! # Verifying the baseline
//!
//! The baseline was checked against the live database on 2026-10-03 by applying
//! it to an empty database and diffing the result: 28 tables created (27 ORM
//! plus the `chat_messages_fts` virtual table), 81 explicit indexes, 3 FTS5
//! triggers, and `PRAGMA foreign_key_check` clean. FTS5 shadow tables
//! (`chat_messages_fts_config/_content/_data/_docsize/_idx`) are created by
//! the virtual table and are therefore absent from the SQL by design.
//!
//! To regenerate after a schema change upstream:
//!
//! ```text
//! cargo run --manifest-path tools/dump-baseline/Cargo.toml -- <app.db> <out-dir>
//! ```
//!
//! FTS5 shadow tables and implicit `sqlite_autoindex_*` entries are excluded by
//! the generator, since neither can be created by name.

use rusqlite::Connection;

use crate::{MigrationReport, Result};

/// The baseline DDL, generated from a live `app.db` (see module docs).
const BASELINE: &str = include_str!("../migrations/0001_baseline.sql");

/// The schema version this build expects.
pub const TARGET_VERSION: u32 = 1;

/// Version bookkeeping lives in a table that is itself created if absent, so
/// the very first run on a fresh database works.
const CREATE_VERSION_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS schema_version (
        version     INTEGER NOT NULL PRIMARY KEY,
        applied_at  TEXT    NOT NULL DEFAULT (datetime('now')),
        note        TEXT
    );";

/// Reads the current schema version, returning 0 for a fresh database.
///
/// # Errors
///
/// Returns [`crate::StoreError::Sqlite`] if the version table cannot be read.
fn current_version(conn: &Connection) -> Result<u32> {
    conn.execute_batch(CREATE_VERSION_TABLE)?;
    let v: Option<u32> = conn.query_row("SELECT MAX(version) FROM schema_version", [], |row| {
        row.get(0)
    })?;
    Ok(v.unwrap_or(0))
}

/// Runs the migration chain to [`TARGET_VERSION`].
///
/// Idempotent. On a current database this is one indexed read plus the
/// `IF NOT EXISTS` check — which is the entire point of moving away from the
/// Python source, where ~45 migration functions ran at every startup.
///
/// # Errors
///
/// Returns [`crate::StoreError::Sqlite`] if the version table or the baseline
/// cannot be applied.
pub fn migrate(conn: &Connection) -> Result<MigrationReport> {
    let from = current_version(conn)?;
    if from >= TARGET_VERSION {
        return Ok(MigrationReport {
            from,
            to: from,
            applied: Vec::new(),
        });
    }

    let mut applied = Vec::new();

    if from == 0 {
        conn.execute_batch(BASELINE)?;
        applied.push("0001_baseline");
    }

    // Future steps are appended here, each bumping `to` by one. Do not edit
    // `0001_baseline.sql` once a release has shipped it: databases that already
    // applied it will never re-run it, so a silent edit would produce two
    // different "version 1" schemas.

    conn.execute(
        "INSERT OR REPLACE INTO schema_version(version, note) VALUES (?1, ?2)",
        rusqlite::params![TARGET_VERSION, "chain applied"],
    )?;
    Ok(MigrationReport {
        from,
        to: TARGET_VERSION,
        applied,
    })
}

#[cfg(test)]
mod tests {
    use {
        super::{BASELINE, migrate},
        crate::pool,
    };

    /// Objects created by the baseline itself.
    ///
    /// Excludes `schema_version` (migration bookkeeping, not the baseline) and
    /// the five FTS5 shadow tables. The shadows are listed explicitly rather
    /// than matched by a `chat_messages_fts_%` wildcard, because that prefix
    /// would also swallow the three `chat_messages_fts_ai/_ad/_au` triggers —
    /// which are exactly the objects the base64-scrubbing test depends on.
    fn counts(conn: &rusqlite::Connection) -> (usize, usize, usize) {
        const EXCLUDED: &str = "'schema_version'
            , 'chat_messages_fts_config'
            , 'chat_messages_fts_content'
            , 'chat_messages_fts_data'
            , 'chat_messages_fts_docsize'
            , 'chat_messages_fts_idx'";

        let count = |kind: &str| -> usize {
            let raw: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM sqlite_master
                         WHERE type = '{kind}' AND sql IS NOT NULL
                           AND name NOT IN ({EXCLUDED})"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or_default();
            usize::try_from(raw).unwrap_or(0)
        };
        (count("table"), count("index"), count("trigger"))
    }

    #[test]
    fn baseline_applies_to_an_empty_database() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();
        let report = migrate(&conn).unwrap();

        assert_eq!(report.from, 0);
        assert_eq!(report.to, 1);
        assert_eq!(report.applied, vec!["0001_baseline"]);
        assert!(!report.is_noop());
    }

    /// Guards against accidental edits to the baseline. The expected counts were
    /// measured against the live `app.db` this file was generated from.
    #[test]
    fn baseline_produces_the_expected_object_inventory() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();
        migrate(&conn).unwrap();

        let (tables, indexes, triggers) = counts(&conn);
        assert_eq!(
            tables, 28,
            "27 ORM tables plus the chat_messages_fts virtual table"
        );
        assert_eq!(triggers, 3, "chat_messages_fts_ai / _ad / _au");
        // 81 explicit CREATE INDEX statements. The remaining ~28 index entries
        // in the live database are implicit `sqlite_autoindex_*` rows created by
        // UNIQUE constraints, which have no SQL of their own.
        assert_eq!(indexes, 81, "explicit indexes only");
    }

    #[test]
    fn fts_shadow_tables_are_created_by_the_virtual_table_not_by_us() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();
        migrate(&conn).unwrap();

        let shadows = ["config", "content", "data", "docsize", "idx"];
        for suffix in shadows {
            let name = format!("chat_messages_fts_{suffix}");
            let present: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [&name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(present, 1, "FTS5 should create {name} itself");
        }

        // And they must NOT be in the SQL we ship, or applying it would fail.
        for suffix in shadows {
            let needle = format!("chat_messages_fts_{suffix}");
            assert!(
                !BASELINE.contains(&format!("CREATE TABLE {needle}")),
                "{needle} must not be created explicitly; FTS5 makes it"
            );
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();

        let first = migrate(&conn).unwrap();
        assert_eq!(first.from, 0);
        assert!(!first.is_noop());

        // Second run must be a no-op, not a re-apply that would fail on
        // "table already exists".
        let second = migrate(&conn).unwrap();
        assert_eq!(second.from, 1);
        assert_eq!(second.to, 1);
        assert!(second.is_noop(), "second migrate() must do nothing");

        let third = migrate(&conn).unwrap();
        assert!(third.is_noop());
    }

    #[test]
    fn fts_triggers_scrub_inline_media_from_the_index() {
        // The Python schema scrubs base64 payloads out of the search index so a
        // 2 MB data URI cannot bloat the FTS table. If these triggers are
        // dropped, transcript search silently stops working on rich messages.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();
        migrate(&conn).unwrap();

        conn.execute_batch(
            "INSERT INTO sessions(id, name, endpoint_url, model, created_at, updated_at)
             VALUES ('s1', 'n', 'http://x/v1', 'm', '2026-01-01', '2026-01-01');
             INSERT INTO chat_messages(id, session_id, role, content, timestamp)
             VALUES ('m1', 's1', 'user', 'see this image data:image/png;base64,AAAA', '2026-01-01');",
        )
        .unwrap();

        let indexed: String = conn
            .query_row("SELECT content FROM chat_messages_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            indexed, "[inline media omitted from search index]",
            "inline media must be replaced in the FTS index"
        );
        assert!(
            !indexed.contains("base64"),
            "raw base64 must never reach the index"
        );
    }

    #[test]
    fn foreign_keys_are_enforced_and_the_schema_is_internally_consistent() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();
        migrate(&conn).unwrap();

        let violations: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0, "baseline must not create FK violations");

        // Deleting a session must cascade to its messages, which is what the
        // CASCADE pragma exists for. Without PRAGMA foreign_keys this silently
        // does nothing and orphans rows.
        conn.execute_batch(
            "INSERT INTO sessions(id, name, endpoint_url, model, created_at, updated_at)
             VALUES ('s1', 'n', 'http://x/v1', 'm', '2026-01-01', '2026-01-01');
             INSERT INTO chat_messages(id, session_id, role, content, timestamp)
             VALUES ('m1', 's1', 'user', 'hi', '2026-01-01');
             DELETE FROM sessions WHERE id = 's1';",
        )
        .unwrap();

        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM chat_messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0, "chat_messages must cascade on session delete");
    }

    #[test]
    fn ownerless_column_tables_are_not_swept() {
        // `webhooks`, `task_runs` and `mcp_servers` have no `owner` column even
        // though the Python owner sweep lists them. Asserting the absence here
        // means a future "helpful" migration that adds `owner` to them shows up
        // as a deliberate, reviewed change rather than a surprise.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        pool::configure(&conn).unwrap();
        migrate(&conn).unwrap();

        for table in ["webhooks", "task_runs", "mcp_servers"] {
            let has_owner: i64 = conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info(?1) WHERE name = 'owner'",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has_owner, 0, "{table} has no owner column by design");
        }

        // And the ones that DO have it, do.
        for table in ["sessions", "documents", "notes", "memories"] {
            let has_owner: i64 = conn
                .query_row(
                    "SELECT count(*) FROM pragma_table_info(?1) WHERE name = 'owner'",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has_owner, 1, "{table} must have an owner column");
        }
    }
}
