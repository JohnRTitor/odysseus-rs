//! Structural inventory of `0001_baseline.sql`, as verified against the Python
//! SQLAlchemy models in T0044 (`DATA_MIGRATION.md` §2.2 / §2.3).
//!
//! Why this file exists: `crates/store/src/schema.rs` already pins the object
//! *counts* (28 tables, 81 explicit indexes, 3 triggers) and the FTS5 shadow-table
//! rules. It does not pin the *shape* of the objects that carry behaviour — the
//! foreign keys that make `ON DELETE` work, the absence of SQL `DEFAULT` clauses,
//! the `owner`-column set, or the fact that `user_tool_data.id` is a bare rowid
//! alias. Those are exactly the properties a careless regeneration of the baseline
//! from a legacy Python `app.db` would change silently, and every one of them was
//! a finding in T0044.
//!
//! Scope, deliberately narrow: these are assertions about schema *shape*, recorded
//! as a regression guard. They are not a re-implementation of the model diff — the
//! diff itself is a document, because the models live in another repository and
//! cannot be read from a test. Each test names the §2.2 row it protects.
//!
//! Nothing here may be "fixed" to match a model. Decision D6 makes the live
//! database the source of truth (`DATA_MIGRATION.md` §3.1), so a failing
//! assertion means the baseline drifted and a human must record the change in
//! §2.2 first.

use std::error::Error;

use rusqlite::Connection;

/// Opens an in-memory database with the baseline applied.
fn baseline_db() -> Result<Connection, Box<dyn Error>> {
    let conn = Connection::open_in_memory()?;
    odysseus_store::pool::configure(&conn)?;
    odysseus_store::schema::migrate(&conn)?;
    Ok(conn)
}

/// The 27 ORM tables. Excluded: the five FTS5 shadow tables (`chat_messages_fts_%`
/// does not match `chat_messages_fts` itself, which has no trailing `_segment`),
/// SQLite's own `sqlite_%` bookkeeping, and `schema_version`, which
/// `schema::migrate` creates as version bookkeeping and which is not part of the
/// baseline.
fn orm_tables(conn: &Connection) -> Result<Vec<String>, Box<dyn Error>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master
          WHERE type = 'table'
            AND name NOT LIKE 'chat_messages_fts_%'
            AND name NOT LIKE 'sqlite_%'
            AND name <> 'schema_version'
          ORDER BY name",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// `(table, column, notnull, dflt_value)` for one column of one table.
type ColumnRow = (String, String, bool, Option<String>);

/// `(table, from_column, parent_table, on_delete)` for one foreign key.
type FkRow = (String, String, String, String);

/// Every column of every table, as a [`ColumnRow`].
fn columns(conn: &Connection) -> Result<Vec<ColumnRow>, Box<dyn Error>> {
    let mut out = Vec::new();
    for table in orm_tables(conn)? {
        let mut stmt = conn.prepare(
            "SELECT name, \"notnull\", dflt_value FROM pragma_table_info(?1) ORDER BY cid",
        )?;
        let rows = stmt.query_map([&table], |row| {
            Ok((
                table.clone(),
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)? != 0,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            out.push(row?);
        }
    }
    Ok(out)
}

/// §2.2 — "FKs are lost by the Python table rebuild".
///
/// The model's `ondelete`/`onupdate` for all 13 foreign keys, as
/// `(table, from_column, parent_table, on_delete)`. `calendar_events.calendar_id`
/// deliberately has no `ondelete` clause in *both* the model and the baseline
/// (`database.py:1863`); it is listed with `NO ACTION` because that is what
/// SQLite reports, not because a clause exists.
const EXPECTED_FKS: &[(&str, &str, &str, &str)] = &[
    ("calendar_events", "calendar_id", "calendars", "NO ACTION"),
    ("chat_messages", "session_id", "sessions", "CASCADE"),
    ("crew_members", "session_id", "sessions", "SET NULL"),
    ("document_versions", "document_id", "documents", "CASCADE"),
    ("documents", "session_id", "sessions", "SET NULL"),
    ("gallery_images", "album_id", "gallery_albums", "SET NULL"),
    ("gallery_images", "session_id", "sessions", "SET NULL"),
    ("memories", "session_id", "sessions", "SET NULL"),
    ("scheduled_tasks", "session_id", "sessions", "SET NULL"),
    (
        "scheduled_tasks",
        "then_task_id",
        "scheduled_tasks",
        "SET NULL",
    ),
    ("task_runs", "task_id", "scheduled_tasks", "CASCADE"),
    ("user_tool_data", "tool_id", "user_tools", "CASCADE"),
    ("user_tools", "session_id", "sessions", "SET NULL"),
];

#[test]
fn foreign_key_inventory_matches_the_models() -> Result<(), Box<dyn Error>> {
    let conn = baseline_db()?;

    let mut found: Vec<FkRow> = Vec::new();
    for table in orm_tables(&conn)? {
        let mut stmt =
            conn.prepare("SELECT \"from\", \"table\", on_delete FROM pragma_foreign_key_list(?1)")?;
        let rows = stmt.query_map([&table], |row| {
            Ok((
                table.clone(),
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            found.push(row?);
        }
    }
    found.sort();

    let mut expected: Vec<FkRow> = EXPECTED_FKS
        .iter()
        .map(|(t, f, p, d)| {
            (
                (*t).to_string(),
                (*f).to_string(),
                (*p).to_string(),
                (*d).to_string(),
            )
        })
        .collect();
    expected.sort();

    assert_eq!(
        found, expected,
        "the baseline's foreign keys must stay identical to the models' (DATA_MIGRATION.md §2.2)"
    );

    Ok(())
}

#[test]
fn scheduled_tasks_keeps_both_of_its_foreign_keys() -> Result<(), Box<dyn Error>> {
    // A dedicated assertion, not just a line in the inventory above: the Python
    // table rebuild at `database.py:1613-1639` recreates `scheduled_tasks` with
    // no `FOREIGN KEY` clause at all, so a database regenerated from an install
    // that hit that rebuild would silently lose both. `PRAGMA foreign_key_check`
    // cannot see a *missing* constraint, so nothing else in the suite would
    // notice.
    let conn = baseline_db()?;
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_foreign_key_list('scheduled_tasks')",
        [],
        |row| row.get(0),
    )?;

    assert_eq!(
        count, 2,
        "scheduled_tasks must keep FKs on session_id and then_task_id (DATA_MIGRATION.md §2.2)"
    );

    Ok(())
}

#[test]
fn no_column_carries_a_sql_default() -> Result<(), Box<dyn Error>> {
    // §2.2 — 19 columns get a SQL `DEFAULT` on the Python migration-helper path
    // but not in the baseline, because SQLAlchemy's `default=` is a Python-side
    // default that never emits DDL. The baseline therefore has none, and the Rust
    // write path must supply all 19 itself. A regeneration from a legacy database
    // would bake them in and silently change insert semantics.
    let conn = baseline_db()?;
    let with_defaults: Vec<String> = columns(&conn)?
        .into_iter()
        .filter(|(_, _, _, dflt)| dflt.is_some())
        .map(|(table, col, _, _)| format!("{table}.{col}"))
        .collect();

    assert!(
        with_defaults.is_empty(),
        "the baseline must carry no SQL DEFAULT clauses, found {with_defaults:?}"
    );

    Ok(())
}

/// §2.2 — `user_tool_data.id` is a bare rowid alias.
///
/// The model says `autoincrement=True` (`database.py:692`) but SQLAlchemy's SQLite
/// dialect drops the flag, so no SQLAlchemy-created schema carries `AUTOINCREMENT`
/// and neither does the live database. Adding it would create a `sqlite_sequence`
/// table that does not exist today and change row-reuse semantics, so this asserts
/// the absence.
#[test]
fn user_tool_data_id_is_a_rowid_alias_not_autoincrement() -> Result<(), Box<dyn Error>> {
    let conn = baseline_db()?;

    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='user_tool_data'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        !sql.to_ascii_uppercase().contains("AUTOINCREMENT"),
        "user_tool_data.id must stay a plain INTEGER PRIMARY KEY (DATA_MIGRATION.md §2.2)"
    );

    // SQLite materialises AUTOINCREMENT as a `sqlite_sequence` table, so its
    // absence is the second half of the same guarantee.
    let sequence_tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'sqlite_sequence'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        sequence_tables, 0,
        "no AUTOINCREMENT column may exist anywhere in the baseline"
    );

    Ok(())
}

/// §2.2 — the owner-column set, asserted as a set equality so a change is deliberate.
///
/// 18 of the 28 tables the baseline creates carry `owner`; these 9 do not. Asserting
/// the *whole* no-owner set — rather than one table at a time — means adding `owner` to
/// any other table fails here and has to be a reviewed decision.
///
/// It also pins which half of the mismatch with the Python owner sweep
/// (`database.py:1444-1448`) is which. Five of these nine appear in that list and are
/// inert no-ops; the other four (`chat_messages`, `chat_messages_fts`,
/// `document_versions`, `email_account_owner_locks`) are correctly absent from it.
/// Five owner-bearing tables are missing from the sweep instead — see the
/// `owner`-sweep row in §2.2.
#[test]
fn exactly_nine_tables_have_no_owner_column() -> Result<(), Box<dyn Error>> {
    let conn = baseline_db()?;

    let mut without_owner: Vec<String> = Vec::new();
    for table in orm_tables(&conn)? {
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM pragma_table_info(?1) WHERE name = 'owner'",
            [&table],
            |row| row.get(0),
        )?;
        if count == 0 {
            without_owner.push(table);
        }
    }
    without_owner.sort();

    assert_eq!(
        without_owner,
        vec![
            "calendar_events".to_string(),
            "chat_messages".to_string(),
            "chat_messages_fts".to_string(),
            "document_versions".to_string(),
            "email_account_owner_locks".to_string(),
            "mcp_servers".to_string(),
            "task_runs".to_string(),
            "user_tool_data".to_string(),
            "webhooks".to_string(),
        ],
        "the set of owner-less tables is a decision, not an accident (DATA_MIGRATION.md §2.2)"
    );

    Ok(())
}

/// §2.3 — the FTS5 table is the baseline's one virtual table and has no model.
///
/// It is created by `_migrate_chat_messages_fts` (`database.py:2208-2241`) because
/// SQLAlchemy cannot express an FTS5 virtual table, which is why it appears here
/// with no ORM class behind it.
#[test]
fn chat_messages_fts_is_the_only_virtual_table() -> Result<(), Box<dyn Error>> {
    let conn = baseline_db()?;

    let sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='chat_messages_fts'",
        [],
        |row| row.get(0),
    )?;
    assert!(
        sql.contains("VIRTUAL TABLE") && sql.contains("fts5"),
        "chat_messages_fts must be an FTS5 virtual table"
    );

    let fts_columns: Vec<String> = columns(&conn)?
        .into_iter()
        .filter(|(table, _, _, _)| table == "chat_messages_fts")
        .map(|(_, col, _, _)| col)
        .collect();
    assert_eq!(
        fts_columns,
        vec![
            "content".to_string(),
            "message_id".to_string(),
            "session_id".to_string(),
            "role".to_string(),
        ],
        "the three UNINDEXED columns are what let triggers delete by message_id"
    );

    // The three triggers attach to `chat_messages`, not to the virtual table.
    let triggers: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master
              WHERE type = 'trigger' AND sql LIKE '%ON chat_messages%'
              ORDER BY name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(
        triggers,
        vec![
            "chat_messages_fts_ad".to_string(),
            "chat_messages_fts_ai".to_string(),
            "chat_messages_fts_au".to_string(),
        ],
        "insert, delete and update must all maintain the FTS index"
    );

    Ok(())
}
