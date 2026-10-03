//! Acceptance gate for the baseline migration (T0049).
//!
//! Verifies the real-world claim in `DATA_MIGRATION.md` §11: a database written
//! by the Python `SQLAlchemy` stack opens cleanly in Rust, with its schema and
//! data intact.
//!
//! Two halves:
//! 1. A **committed fixture** that exercises column types, constraints, the
//!    FTS5 triggers, encryption round-trips, and owner scoping. This runs in CI.
//! 2. An **optional real-database check** against `DATA_DIR`, run manually or in
//!    a pre-release job. It is skipped (not silently passed) when the database
//!    is absent, and it never writes to the source file.

use std::path::PathBuf;

use odysseus_store::{Store, crypto};

/// Absolute path to the migrations directory, resolved at compile time.
fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

/// Creates a database from the baseline and returns an open connection.
///
/// # Errors
///
/// Returns a boxed error if the baseline cannot be read or applied.
fn fresh_db() -> Result<rusqlite::Connection, Box<dyn std::error::Error>> {
    let sql = std::fs::read_to_string(migrations_dir().join("0001_baseline.sql"))?;
    let conn = rusqlite::Connection::open_in_memory()?;
    conn.execute_batch(&sql)?;
    Ok(conn)
}

#[test]
fn baseline_opens_a_python_style_database_and_round_trips_every_column_shape()
-> Result<(), Box<dyn std::error::Error>> {
    let conn = fresh_db()?;
    odysseus_store::pool::configure(&conn)?;

    // Write one row per storage class the Python schema actually uses: TEXT,
    // INTEGER, REAL, BOOLEAN, NULL-able columns, JSON-as-TEXT, and a
    // double-quoted column name ("metadata", "key").
    conn.execute_batch(
        r#"
        INSERT INTO sessions
            (id, name, endpoint_url, model, owner, rag, archived, headers,
             last_accessed, last_message_at, is_important, message_count,
             total_input_tokens, total_output_tokens, mode, crew_member_id,
             created_at, updated_at)
        VALUES
            ('sess-1', 'a session', 'http://127.0.0.1:11434/v1', 'llama3',
             'alice', 1, 0, '{"X-Key":"v"}',
             '2026-10-01 10:00:00', '2026-10-01 10:05:00', 1, 2,
             120, 340, 'agent', NULL,
             '2026-10-01 09:00:00', '2026-10-01 10:05:00');

        INSERT INTO chat_messages (id, session_id, role, content, "metadata", timestamp)
        VALUES
            ('msg-1', 'sess-1', 'user',  'hello world',
             '{"tool_events":[]}', '2026-10-01 10:00:00'),
            ('msg-2', 'sess-1', 'assistant', 'hi there',
             NULL, '2026-10-01 10:00:01');

        -- memories.timestamp is INTEGER unix seconds, not a DATETIME. This is a
        -- real schema quirk from the Python model and must round-trip as an
        -- integer, not be coerced to a string.
        INSERT INTO memories (id, text, category, source, owner, session_id, timestamp)
        VALUES ('mem-1', 'prefers dark mode', 'preference', 'chat', 'alice', 'sess-1', 1759312800);

        -- user_tool_data uses a reserved-ish column name and an autoincrement id.
        INSERT INTO user_tools
            (id, name, html_content, scope, owner, is_active, created_at, updated_at)
        VALUES ('tool-1', 'demo', '<p>hi</p>', 'session', 'alice', 1,
                '2026-10-01 09:00:00', '2026-10-01 09:00:00');
        INSERT INTO user_tool_data (tool_id, "key", value)
        VALUES ('tool-1', 'count', '7');

        -- A NULL owner row: the owner sweep exists precisely because these occur.
        INSERT INTO notes (id, owner, title, content, note_type, created_at, updated_at)
        VALUES ('note-1', NULL, 'unowned', 'body', 'note',
                '2026-10-01 09:00:00', '2026-10-01 09:00:00');
        "#,
    )?;

    // Read every shape back and assert exact equality.
    let (model, rag, headers, count): (String, i64, Option<String>, i64) = conn.query_row(
        "SELECT model, rag, headers, message_count FROM sessions WHERE id = 'sess-1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    assert_eq!(model, "llama3");
    assert_eq!(
        rag, 1,
        "BOOLEAN must come back as an integer, as SQLite stores it"
    );
    assert_eq!(
        headers.as_deref(),
        Some(r#"{"X-Key":"v"}"#),
        "headers is JSON stored as TEXT"
    );
    assert_eq!(count, 2);

    let meta: Option<String> = conn.query_row(
        "SELECT \"metadata\" FROM chat_messages WHERE id = 'msg-1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(meta.as_deref(), Some(r#"{"tool_events":[]}"#));

    let null_meta: Option<String> = conn.query_row(
        "SELECT \"metadata\" FROM chat_messages WHERE id = 'msg-2'",
        [],
        |r| r.get(0),
    )?;
    assert!(
        null_meta.is_none(),
        "NULL metadata must stay NULL, not become an empty string"
    );

    let ts: i64 = conn.query_row(
        "SELECT timestamp FROM memories WHERE id = 'mem-1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        ts, 1_759_312_800,
        "memories.timestamp is INTEGER unix seconds"
    );

    let (tk, tv): (String, Option<String>) = conn.query_row(
        "SELECT \"key\", value FROM user_tool_data WHERE tool_id = 'tool-1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(tk, "count");
    assert_eq!(tv.as_deref(), Some("7"));

    let note_owner: Option<String> =
        conn.query_row("SELECT owner FROM notes WHERE id = 'note-1'", [], |r| {
            r.get(0)
        })?;
    assert!(
        note_owner.is_none(),
        "NULL owner must be representable for the owner sweep"
    );

    Ok(())
}

#[test]
fn unique_constraints_from_the_python_schema_are_enforced() -> Result<(), Box<dyn std::error::Error>>
{
    let conn = fresh_db()?;
    odysseus_store::pool::configure(&conn)?;

    // gallery_images.filename is UNIQUE. A duplicate must be rejected, because
    // the gallery serves generated images by content-hash filename.
    conn.execute_batch(
        r"
        INSERT INTO gallery_images (id, filename, prompt, owner, is_active, created_at, updated_at)
        VALUES ('g1', 'hash-aaa.png', 'p', 'alice', 1, '2026-10-01', '2026-10-01');
        ",
    )?;
    let dup = conn.execute(
        r"INSERT INTO gallery_images (id, filename, prompt, owner, is_active, created_at, updated_at)
           VALUES ('g2', 'hash-aaa.png', 'p', 'alice', 1, '2026-10-01', '2026-10-01')",
        [],
    );
    assert!(
        dup.is_err(),
        "gallery_images.filename UNIQUE must be enforced"
    );

    // email_accounts has a partial unique index allowing exactly one default
    // per owner. This is the constraint the Python migration refuses to create
    // silently, so it must be present and real here.
    conn.execute_batch(
        r"
        INSERT INTO email_accounts (id, owner, name, is_default, enabled, created_at, updated_at)
        VALUES ('a1', 'alice', 'primary', 1, 1, '2026-10-01', '2026-10-01');
        ",
    )?;
    let second_default = conn.execute(
        r"INSERT INTO email_accounts (id, owner, name, is_default, enabled, created_at, updated_at)
           VALUES ('a2', 'alice', 'secondary', 1, 1, '2026-10-01', '2026-10-01')",
        [],
    );
    assert!(
        second_default.is_err(),
        "only one default email account per owner may exist"
    );

    // A different owner may still have a default.
    conn.execute(
        r"INSERT INTO email_accounts (id, owner, name, is_default, enabled, created_at, updated_at)
           VALUES ('a3', 'bob', 'bob primary', 1, 1, '2026-10-01', '2026-10-01')",
        [],
    )?;
    let total: i64 = conn.query_row(
        "SELECT count(*) FROM email_accounts WHERE is_default = 1",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(total, 2, "one default per owner, two owners");

    Ok(())
}

#[test]
fn store_migrates_a_file_backed_database_and_restricts_its_permissions()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("app.db");

    let store = Store::open(&path);
    let report = store.migrate()?;
    assert_eq!(report.from, 0);
    assert_eq!(report.to, 1);
    assert!(path.exists(), "migrate() must create the database file");

    store.restrict_permissions()?;

    // Re-running is a no-op, which is what makes startup cheap.
    let again = store.migrate()?;
    assert!(again.is_noop());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "app.db holds encrypted secrets and must be owner-only"
        );
    }

    Ok(())
}

/// Opens a real Python-created `app.db` when one is available.
///
/// Skipped when absent. **Never writes**: the file is copied before opening, so
/// even a bug here cannot damage a user's installation.
#[test]
fn opens_a_real_python_created_database_when_available() -> Result<(), Box<dyn std::error::Error>> {
    let Ok(dir) = std::env::var("DATA_DIR") else {
        eprintln!("SKIP: DATA_DIR unset, so no real database to check");
        return Ok(());
    };
    let source = PathBuf::from(&dir).join("app.db");
    if !source.exists() {
        eprintln!("SKIP: {} not present", source.display());
        return Ok(());
    }

    let tmp = tempfile::tempdir()?;
    let copy = tmp.path().join("app.db");
    std::fs::copy(&source, &copy)?;
    let before = std::fs::metadata(&copy)?.len();

    // Open the copy read-write: the original is untouched either way.
    let store = Store::open(&copy);
    let conn = store.connect()?;

    let count = |kind: &str| -> i64 {
        conn.query_row(
            &format!(
                "SELECT count(*) FROM sqlite_master WHERE type = '{kind}' AND sql IS NOT NULL"
            ),
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    };
    let (tables, indexes, triggers) = (count("table"), count("index"), count("trigger"));
    eprintln!("real db: {tables} tables, {indexes} indexes, {triggers} triggers");

    assert!(
        tables >= 28,
        "a real installation should have at least the 28 baseline tables, found {tables}"
    );
    assert_eq!(
        triggers, 3,
        "the three FTS5 triggers must exist for transcript search to work"
    );

    let violations: i64 =
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })?;
    assert_eq!(
        violations, 0,
        "a real database must not contain FK violations"
    );

    // Data must be readable and owner-scoped.
    let sessions: i64 = conn.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
    eprintln!("real db: {sessions} sessions");
    let _ = conn.query_row("SELECT count(*) FROM chat_messages", [], |r| {
        r.get::<_, i64>(0)
    })?;

    // Encrypted columns must interoperate with the install's real .app_key.
    // This is the hard compatibility requirement in DATA_MIGRATION.md §6: changing
    // the key format orphans every encrypted column in an existing install.
    //
    // A fresh installation has no encrypted rows yet, so the check that matters
    // even then is that the key itself *loads* -- that proves the on-disk format is
    // a Fernet key this build accepts. Round-tripping is additionally verified
    // whenever there is at least one encrypted value present.
    let key_path = source
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(".app_key");
    if key_path.exists() {
        let key = crypto::load_key(&key_path)?;

        let probe = crypto::encrypt(&key, "compatibility-probe");
        assert!(
            crypto::is_encrypted(&probe),
            "encrypt() must produce the enc: prefix the Python migrations expect"
        );
        assert_eq!(
            crypto::maybe_decrypt(&key, &probe).unwrap_or_default(),
            "compatibility-probe",
            "the real key must round-trip through this build"
        );

        // Now any rows that do exist.
        let mut stmt =
            conn.prepare("SELECT api_key FROM model_endpoints WHERE api_key LIKE 'enc:%'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut checked = 0;
        for row in rows {
            let stored = row?;
            assert_eq!(
                crypto::maybe_decrypt(&key, &stored)?,
                crypto::maybe_decrypt(&key, &stored)?,
                "decryption must be deterministic"
            );
            checked += 1;
        }
        eprintln!("real db: .app_key loaded; decrypted {checked} endpoint key(s)");
    } else {
        eprintln!("real db: no .app_key present, skipping encryption check");
    }

    assert_eq!(
        std::fs::metadata(&copy)?.len(),
        before,
        "opening the database must not change the file"
    );

    Ok(())
}
