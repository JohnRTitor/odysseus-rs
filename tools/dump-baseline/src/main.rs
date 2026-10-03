//! T0043 — dump a baseline migration from a real `app.db`.
//!
//! Decision D6: the baseline must come from the LIVE database's `sqlite_master`,
//! never from the Python SQLAlchemy models. The models have known drift
//! (`DATA_MIGRATION.md` §2.2); the live DB is ground truth.
//!
//! Read-only against the source database. Usage:
//!   cargo run -- <path-to-app.db> <out-dir>

use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(db_path) = args.next().map(PathBuf::from) else {
        eprintln!("usage: dump-baseline <app.db> <out-dir>");
        std::process::exit(2);
    };
    let out_dir = args.next().map_or_else(|| PathBuf::from("."), PathBuf::from);

    // Open read-only so this can never mutate the source database.
    let conn = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap_or_else(|e| panic!("open {} read-only: {e}", db_path.display()));

    let mut out = String::new();
    let _ = writeln!(
        out,
        "-- 0001_baseline.sql\n\
         --\n\
         -- GENERATED from the live `data/app.db` sqlite_master (task T0043).\n\
         -- Decision D6: the live database is ground truth, not the Python\n\
         -- SQLAlchemy models, which have known drift.\n\
         --\n\
         -- Source: {}\n\
         -- Tables: {}\n\
         --\n\
         -- Regenerate with: cargo run -p dump-baseline -- <app.db> <out-dir>\n",
        db_path.display(),
        count(&conn, "table"),
    );

    // Plain tables first, then virtual tables. Triggers reference base tables,
    // and FTS5 shadow tables must be created by the virtual table itself, so
    // relying on alphabetical order would be luck rather than structure.
    let shadows = fts_shadow_tables(&conn);
    let all_tables = objects(&conn, "table", &shadows);
    let (plain, virtual_tables): (Vec<_>, Vec<_>) = all_tables
        .into_iter()
        .partition(|(_, sql)| !sql.contains("VIRTUAL TABLE"));
    for (name, sql) in plain.iter().chain(virtual_tables.iter()) {
        let _ = writeln!(out, "-- ---- TABLE {name} ----\n{}\n", terminate(sql));
    }

    for (kind, objs) in [
        ("VIEW", objects(&conn, "view", &shadows)),
        ("INDEX", objects(&conn, "index", &shadows)),
        ("TRIGGER", objects(&conn, "trigger", &shadows)),
    ] {
        for (name, sql) in objs {
            let _ = writeln!(out, "-- ---- {kind} {name} ----\n{}\n", terminate(&sql));
        }
    }

    fs::create_dir_all(&out_dir).unwrap_or_else(|e| panic!("mkdir {}: {e}", out_dir.display()));
    let out_path = out_dir.join("0001_baseline.sql");
    fs::write(&out_path, &out).unwrap_or_else(|e| panic!("write {}: {e}", out_path.display()));
    eprintln!(
        "wrote {} ({} bytes): {} tables, {} indexes, {} triggers",
        out_path.display(),
        out.len(),
        count(&conn, "table"),
        count(&conn, "index"),
        count(&conn, "trigger"),
    );
}

/// `sqlite_master.sql` carries no statement terminator, so a batch of consecutive
/// CREATEs would run together and fail with a syntax error. Append one if absent.
fn terminate(sql: &str) -> String {
    let trimmed = sql.trim_end();
    if trimmed.ends_with(';') {
        trimmed.to_owned()
    } else {
        format!("{trimmed};")
    }
}

fn count(conn: &Connection, kind: &str) -> i64 {
    conn.query_row(
        &format!("SELECT count(*) FROM sqlite_master WHERE type = '{kind}'"),
        [],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

/// Objects of `kind`, excluding objects that must not appear in a baseline:
/// implicit `sqlite_autoindex_*` rows (NULL `sql`) and FTS5 shadow tables.
fn objects(conn: &Connection, kind: &str, shadows: &[String]) -> Vec<(String, String)> {
    let sql = format!(
        "SELECT name, COALESCE(sql, '') FROM sqlite_master \
         WHERE type = '{kind}' AND sql IS NOT NULL ORDER BY name"
    );
    let mut stmt = conn.prepare(&sql).unwrap_or_else(|e| panic!("prepare {kind}: {e}"));
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap_or_else(|e| panic!("query {kind}: {e}"));
    rows.map(std::result::Result::unwrap)
        .filter(|(_, sql)| !sql.trim().is_empty())
        .filter(|(name, _)| !name.starts_with("sqlite_autoindex_"))
        .filter(|(name, _)| !shadows.iter().any(|s| name == s))
        .collect()
}

/// Names of FTS5 shadow tables belonging to this database.
///
/// FTS5 creates `<vtable>_config`, `_content`, `_data`, `_docsize` and `_idx`
/// when the virtual table is created, and records SQL for them in
/// `sqlite_master`. Emitting them would make the baseline fail to apply.
/// Derived from the virtual table names rather than hard-coded, so it stays
/// correct if the search index is ever renamed.
fn fts_shadow_tables(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND sql LIKE '%VIRTUAL TABLE%USING fts5%'",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let vtables: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap_or_else(|e| panic!("query fts5 virtual tables: {e}"))
        .map(std::result::Result::unwrap)
        .collect();
    drop(stmt);

    const SUFFIXES: [&str; 5] = ["_config", "_content", "_data", "_docsize", "_idx"];
    let mut shadows = Vec::new();
    for vt in vtables {
        for suffix in SUFFIXES {
            let candidate = format!("{vt}{suffix}");
            let exists: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [&candidate],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if exists > 0 {
                shadows.push(candidate);
            }
        }
    }
    shadows
}
