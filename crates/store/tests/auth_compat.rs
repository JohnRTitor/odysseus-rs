//! Compatibility with the `auth.json` and `sessions.json` that Python writes.
//!
//! These are the two files the Rust and Python builds share on disk. Unlike the
//! SQLite schema, nothing here is generated — the Python side owns the format, so
//! the fixtures below are pinned to what `core/auth.py` emits, and drift in either
//! direction must fail a test rather than surface as "I got logged out".
//!
//! No Python interpreter is available in this environment, so the fixtures are
//! transcribed from the source rather than captured at run time. Each cites the
//! line that produces it.

use {
    odysseus_store::auth::{AuthStore, Privileges},
    std::error::Error,
};

/// `core/auth.py:275-280` — the record `create_user` writes.
///
/// It stores an explicit privilege map, not an empty one: those keys are the wire
/// format read back by `auth_helpers.require_privilege`. The hash is a real
/// cost-12 `$2b$` modular-crypt digest, the format `bcrypt.gensalt()` emits.
const PYTHON_USER_RECORD: &str = r#""alice": {
      "password_hash": "$2b$12$8GTDUnQLeOEj0Ma6it0BU.LD0dqLzcwMUjke8.NhXhNJBeY8LT5gq",
      "created": 1758000000.123456,
      "is_admin": false,
      "privileges": {
        "can_use_agent": true,
        "can_use_browser": true,
        "can_use_documents": true,
        "can_use_research": true,
        "can_generate_images": true,
        "can_manage_memory": true,
        "can_use_bash": false,
        "max_messages_per_day": 0,
        "allowed_models": [],
        "allowed_models_restricted": false,
        "block_all_models": false
      }
    }"#;

/// `auth.json` holding exactly one ordinary user named `alice`.
fn one_user() -> String {
    format!(r#"{{"users": {{{PYTHON_USER_RECORD} }}}}"#)
}

type TestResult = Result<(), Box<dyn Error>>;

/// Writes `files` into a fresh temp data directory and points a store at it.
fn store_with(files: &[(&str, &str)]) -> Result<(tempfile::TempDir, AuthStore), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let data_dir = dir.path().to_path_buf();
    for (name, contents) in files {
        std::fs::write(data_dir.join(name), contents)?;
    }
    Ok((dir, AuthStore::new(&data_dir)))
}

fn read_json(path: &std::path::Path) -> Result<serde_json::Value, Box<dyn Error>> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[test]
fn reads_an_auth_json_written_by_python() -> TestResult {
    let contents = format!(
        "{{\n  \"signup_enabled\": true,\n  \"users\": {{\n    {PYTHON_USER_RECORD}\n  }}\n}}"
    );
    let (_dir, store) = store_with(&[("auth.json", &contents)])?;

    let user = store.user("alice")?;
    assert_eq!(
        user.password_hash,
        "$2b$12$8GTDUnQLeOEj0Ma6it0BU.LD0dqLzcwMUjke8.NhXhNJBeY8LT5gq"
    );
    assert!(!user.is_admin);
    // Python's `time.time()` carries microseconds; they must not be truncated.
    assert!(
        (user.created - 1_758_000_000.123_456).abs() < 1e-6,
        "created was {}",
        user.created
    );

    let privs = store.privileges("alice")?;
    assert!(privs.can_use_agent);
    assert!(!privs.can_use_bash, "shell stays off for an ordinary user");
    assert_eq!(privs.max_messages_per_day, 0);
    assert!(!privs.block_all_models);

    assert!(store.is_configured()?);
    Ok(())
}

#[test]
fn python_bcrypt_hashes_verify() -> TestResult {
    // Cost 12 is the Python default (`bcrypt.gensalt()`), and `$2b$` is the same
    // modular-crypt format both implementations emit. Verifying here is what keeps
    // an existing install's passwords working after the cutover.
    let (_dir, store) = store_with(&[("auth.json", &one_user())])?;
    let user = store.user("alice")?;

    assert!(
        AuthStore::verify_password("hunter2hunter2", &user.password_hash)?,
        "the correct password must verify against the stored hash"
    );
    assert!(
        !AuthStore::verify_password("wrong", &user.password_hash)?,
        "a wrong password must not verify"
    );
    Ok(())
}

#[test]
fn a_malformed_bcrypt_hash_fails_closed_rather_than_erroring() -> TestResult {
    // A truncated or hand-edited hash must deny, not pass and not 500.
    let contents = r#"{"users": {"alice": {"password_hash": "not-a-hash", "is_admin": false}}}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    let user = store.user("alice")?;
    let result = AuthStore::verify_password("anything", &user.password_hash);
    assert!(
        result.is_err(),
        "a malformed hash must be an error, never Ok(true)"
    );
    Ok(())
}

#[test]
fn round_trips_a_python_written_file_without_losing_unknown_keys() -> TestResult {
    // A key this build does not know about must survive a write, or a
    // downgrade/upgrade cycle would silently drop a setting. Asserted at *both*
    // levels: `AuthFile` had a top-level `extra` map but `User` did not, so any one
    // mutation silently erased every undeclared per-user field for every user.
    let contents = r#"{"signup_enabled": true, "future_setting": {"nested": [1, 2]}, "users": {"alice": {"password_hash": "$2b$12$8GTDUnQLeOEj0Ma6it0BU.LD0dqLzcwMUjke8.NhXhNJBeY8LT5gq", "is_admin": false, "privileges": {}, "future_user_field": {"kept": true}}}}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    store.set_admin("alice", true)?;

    let on_disk = read_json(store.auth_path())?;
    assert_eq!(
        on_disk["future_setting"]["nested"],
        serde_json::json!([1, 2])
    );
    assert_eq!(on_disk["signup_enabled"], true);
    assert_eq!(
        on_disk["users"]["alice"]["future_user_field"]["kept"], true,
        "an unknown per-user key must survive: {on_disk}"
    );
    Ok(())
}

#[test]
fn an_empty_allowed_models_list_from_an_older_python_build_stays_unrestricted() -> TestResult {
    // `get_privileges` merges `{**DEFAULT_PRIVILEGES, **stored}` precisely so a file
    // predating `allowed_models_restricted` still works. Python treats an empty list
    // as unrestricted (`bool([])` is False); a stricter reading would lock such a
    // user out of every model.
    let contents = r#"{"users": {"alice": {
        "password_hash": "$2b$12$abc",
        "is_admin": false,
        "privileges": {"allowed_models": []}
    }}}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    let privs = store.privileges("alice")?;
    assert!(
        !privs.allowed_models_restricted,
        "an empty list must not block every model"
    );
    Ok(())
}

#[test]
fn reads_a_sessions_json_written_by_python() -> TestResult {
    // `core/auth.py:157` writes the bare `{token: {username, expiry}}` map, with
    // no wrapper object.
    let token = "a".repeat(64);
    let contents = format!(r#"{{"{token}": {{"username": "alice", "expiry": 4102444800.5}}}}"#);
    let (_dir, store) = store_with(&[("auth.json", &one_user()), ("sessions.json", &contents)])?;

    assert!(store.validate_token(&token)?);
    assert_eq!(store.username_for_token(&token)?.as_deref(), Some("alice"));
    Ok(())
}

#[test]
fn an_entry_without_an_expiry_is_treated_as_expired() -> TestResult {
    // `_load_sessions` uses `v.get("expiry", 0) > now`, so an entry missing its
    // expiry is dropped rather than accepted — or, worse, failing the whole file's
    // parse and turning every authenticated request into a 500.
    let token = "b".repeat(64);
    let contents = format!(r#"{{"{token}": {{"username": "alice"}}}}"#);
    let (_dir, store) = store_with(&[("auth.json", &one_user()), ("sessions.json", &contents)])?;

    assert!(!store.validate_token(&token)?, "no expiry means expired");
    Ok(())
}

#[test]
fn an_expired_python_session_is_pruned_from_the_file() -> TestResult {
    let token = "c".repeat(64);
    let contents = format!(r#"{{"{token}": {{"username": "alice", "expiry": 1.0}}}}"#);
    let (_dir, store) = store_with(&[("auth.json", &one_user()), ("sessions.json", &contents)])?;

    assert!(!store.validate_token(&token)?);
    let after = read_json(store.sessions_path())?;
    assert!(
        after.as_object().is_some_and(serde_json::Map::is_empty),
        "expired entry must be pruned, got {after}"
    );
    Ok(())
}

#[test]
fn a_session_naming_a_deleted_user_is_rejected_and_pruned() -> TestResult {
    // The orphan check: a token whose account no longer exists must not
    // authenticate, and must be cleaned up rather than lingering.
    let token = "d".repeat(64);
    let contents = format!(r#"{{"{token}": {{"username": "ghost", "expiry": 4102444800.0}}}}"#);
    let (_dir, store) = store_with(&[("auth.json", &one_user()), ("sessions.json", &contents)])?;

    assert!(
        !store.validate_token(&token)?,
        "an orphan token must not pass"
    );
    let after = read_json(store.sessions_path())?;
    assert!(
        after.as_object().is_some_and(serde_json::Map::is_empty),
        "orphan entry must be pruned, got {after}"
    );
    Ok(())
}

#[test]
fn an_admin_written_by_python_gets_the_full_privilege_set() -> TestResult {
    // `ADMIN_PRIVILEGES` flips every boolean on but explicitly resets the two model
    // sentinels, so an admin is never blocked from using models.
    let contents = r#"{
  "users": {
    "root": {
      "password_hash": "$2b$12$abcdefghijklmnopqrstuv",
      "created": 1758000000.0,
      "is_admin": true,
      "privileges": {
        "can_use_agent": true,
        "can_use_browser": true,
        "can_use_documents": true,
        "can_use_research": true,
        "can_generate_images": true,
        "can_manage_memory": true,
        "can_use_bash": true,
        "max_messages_per_day": 0,
        "allowed_models": [],
        "allowed_models_restricted": false,
        "block_all_models": false
      }
    }
  }
}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    assert_eq!(
        store.privileges("root")?,
        Privileges::admin(),
        "is_admin must short-circuit to ADMIN_PRIVILEGES"
    );
    Ok(())
}

#[test]
fn a_python_setup_legacy_file_migrates_to_an_admin() -> TestResult {
    // The shape `setup.py` wrote before multi-user auth: one top-level username
    // and hash, no `users` key.
    let contents = r#"{
  "username": "operator",
  "password_hash": "$2b$12$abcdefghijklmnopqrstuv"
}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    let user = store.user("operator")?;
    assert!(
        user.is_admin,
        "the sole legacy account is the administrator"
    );

    // Python writes each migration back from inside the migration (`self._save()`); here
    // that is the explicit step, because a read must not write.
    store.migrate_and_persist()?;
    let after = read_json(store.auth_path())?;
    assert!(
        after.get("username").is_none(),
        "must be rewritten: {after}"
    );
    assert_eq!(after["users"]["operator"]["is_admin"], true);
    Ok(())
}

#[test]
fn a_mixed_case_python_key_still_resolves_to_the_same_account() -> TestResult {
    // `_load` lowercases keys so a hand-edited `"Alice"` still signs in.
    let contents = r#"{"users": {"Alice": {"password_hash": "$2b$12$abc", "is_admin": false}}}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    assert!(store.user_exists("alice")?);
    assert!(store.user_exists("ALICE")?);
    Ok(())
}

#[test]
fn a_reserved_username_in_a_python_file_is_dropped_not_honoured() -> TestResult {
    // `api` collides with bearer-token attribution and `internal-tool` is treated
    // as the in-process tool loopback by `core.middleware.require_admin`. An
    // account holding either would inherit that authority.
    let contents = r#"{"users": {
        "api": {"password_hash": "$2b$12$x", "is_admin": true},
        "internal-tool": {"password_hash": "$2b$12$x", "is_admin": true},
        "alice": {"password_hash": "$2b$12$x", "is_admin": false}
    }}"#;
    let (_dir, store) = store_with(&[("auth.json", contents)])?;

    assert!(!store.user_exists("api")?, "api must not be honoured");
    assert!(
        !store.user_exists("internal-tool")?,
        "internal-tool must not be honoured"
    );
    assert!(store.user_exists("alice")?);

    store.migrate_and_persist()?;
    let after = read_json(store.auth_path())?;
    assert_eq!(
        after["users"].as_object().map(serde_json::Map::len),
        Some(1),
        "the file must be rewritten without the reserved names: {after}"
    );
    Ok(())
}
