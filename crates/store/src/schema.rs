//! Schema versioning and the migration chain.
//!
//! Decision D6: a single baseline migration, generated from the **live**
//! `data/app.db` `sqlite_master`, not from the Python `SQLAlchemy` models. The
//! live database is ground truth; the models have known drift (see
//! `DATA_MIGRATION.md` §2.2 — e.g. `gallery_people` is referenced by the owner
//! sweep but no such table exists, and `ix_sessions_last_message_at` exists in
//! the database but not in the model).
//!
//! Until the baseline is generated from a real database, `migrate` reports the
//! work it owes rather than pretending to have applied it.

use rusqlite::Connection;

use crate::{MigrationReport, Result};

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
/// Python source, where ~45 migration functions ran on every startup.
///
/// # Errors
///
/// Returns an error if the version table cannot be read or written.
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

    // Step 1 is the baseline: the full DDL emitted from the live app.db.
    //
    // It is intentionally NOT written here. Generating it requires a real
    // `data/app.db` to read `sqlite_master` from, and hand-writing it from the
    // `SQLAlchemy` models is exactly the drift D6 exists to avoid. Until it
    // lands, we stop rather than create a half-correct schema that the Rust app
    // would then treat as authoritative.
    //
    // Task: T0043.
    if from == 0 {
        return Err(crate::StoreError::Vec(
            "baseline migration (T0043) not yet generated from a live data/app.db; \
             refusing to create a schema that would be treated as authoritative"
                .to_owned(),
        ));
    }

    // Future steps go here as `applied.push("...")` per migration.

    let _ = &mut applied;

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
