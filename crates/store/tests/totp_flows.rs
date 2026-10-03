//! Enrolment and login flows for TOTP two-factor authentication.
//!
//! `crates/store/src/totp.rs` unit-tests the pure logic against `totp-rs`; this
//! file drives it through `auth.json`, which is where the state actually lives and
//! where the security-relevant ordering is decided.
//!
//! The specification is `core/auth.py:483-562` plus
//! `tests/test_totp_failclosed.py`. Every test here is deterministic: `now` is a
//! parameter, never the wall clock (AGENTS.md §11).

use {
    odysseus_store::{
        auth::{AuthError, AuthStore},
        totp::{self, BACKUP_CODE_COUNT, MIN_SECRET_BYTES},
    },
    std::error::Error,
};

/// A fixed instant on a 30-second step boundary, so the acceptance window is unambiguous.
const NOW: u64 = 1_758_000_000;

type TestResult = Result<(), Box<dyn Error>>;

fn store() -> Result<(tempfile::TempDir, AuthStore), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let auth = AuthStore::new(dir.path());
    Ok((dir, auth))
}

fn with_alice() -> Result<(tempfile::TempDir, AuthStore), Box<dyn Error>> {
    let (dir, auth) = store()?;
    auth.create_user("alice", "hunter2hunter2", false)?;
    Ok((dir, auth))
}

/// Plants a hand-edited `auth.json`, the way a corrupt file actually reaches the store.
///
/// Going through the raw file rather than a typed setter is deliberate: these tests
/// are about state no legitimate code path can produce.
fn plant(
    auth: &AuthStore,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<(), Box<dyn Error>> {
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(auth.auth_path())?)?;
    edit(&mut doc);
    std::fs::write(auth.auth_path(), serde_json::to_string_pretty(&doc)?)?;
    auth.migrate_and_persist()?;
    Ok(())
}

/// Enrols `alice` and returns her backup codes.
fn enrol(auth: &AuthStore) -> Result<Vec<String>, Box<dyn Error>> {
    let (secret, uri) = auth.totp_generate_secret("alice")?;
    assert!(!secret.is_empty(), "a secret must be returned to scan");
    assert!(uri.starts_with("otpauth://totp/Odysseus:alice"), "{uri}");

    let code = totp::current_code(&totp::parse_secret(&secret)?, NOW)?;
    Ok(auth.totp_confirm_enable("alice", &code, NOW)?)
}

// -- enrolment ---------------------------------------------------------------

#[test]
fn a_fresh_user_has_no_second_factor() -> TestResult {
    let (_dir, auth) = with_alice()?;
    assert!(!auth.totp_enabled("alice")?);
    assert!(auth.totp_remaining_backup_codes("alice")?.is_empty());
    Ok(())
}

#[test]
fn generating_a_secret_does_not_enable_two_factor() -> TestResult {
    // auth.py:488-497 stores it as *pending*. Enabling here would lock out anyone
    // whose authenticator app never scanned the QR code.
    let (_dir, auth) = with_alice()?;
    let (secret, uri) = auth.totp_generate_secret("alice")?;

    assert!(!secret.is_empty());
    assert!(uri.contains("issuer=Odysseus"), "{uri}");
    assert!(
        !auth.totp_enabled("alice")?,
        "a pending secret must not count as enabled"
    );

    // And the user can still sign in with no second factor at all.
    assert!(auth.totp_verify("alice", "", NOW)?);
    Ok(())
}

#[test]
fn confirming_with_the_right_code_enables_two_factor_and_mints_backup_codes() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let codes = enrol(&auth)?;

    assert!(auth.totp_enabled("alice")?);
    assert_eq!(codes.len(), BACKUP_CODE_COUNT);
    assert_eq!(auth.totp_remaining_backup_codes("alice")?, codes);
    for code in &codes {
        assert_eq!(code.len(), 8, "token_hex(4)");
    }
    Ok(())
}

#[test]
fn confirming_with_a_wrong_code_enables_nothing() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let (secret, _uri) = auth.totp_generate_secret("alice")?;
    let wrong = totp::current_code(&totp::parse_secret(&secret)?, NOW + 300)?;

    let err = auth
        .totp_confirm_enable("alice", &wrong, NOW)
        .expect_err("must refuse");
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    assert!(
        !auth.totp_enabled("alice")?,
        "a failed confirmation must not enable 2FA"
    );
    assert!(
        auth.totp_verify("alice", "", NOW)?,
        "the user must still be able to sign in"
    );
    Ok(())
}

#[test]
fn confirming_without_a_pending_secret_is_refused() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let err = auth
        .totp_confirm_enable("alice", "123456", NOW)
        .expect_err("must refuse");
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    Ok(())
}

#[test]
fn confirming_clears_the_pending_secret_so_only_one_is_live() -> TestResult {
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;

    plant(&auth, |doc| {
        assert!(
            doc["users"]["alice"]["totp_secret_pending"].is_null(),
            "the pending copy must be dropped on enable: {doc}"
        );
    })?;
    let user = auth.user("alice")?;
    assert!(user.totp_secret.is_some(), "the live secret is stored");
    Ok(())
}

#[test]
fn the_stored_secret_is_long_enough_to_be_a_real_second_factor() -> TestResult {
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;
    let stored = auth.user("alice")?.totp_secret.expect("secret");
    let parsed = totp::parse_secret(&stored)?;
    assert!(
        parsed.as_bytes().len() >= MIN_SECRET_BYTES,
        "a short secret would be guessable"
    );
    Ok(())
}

// -- the fail-closed gate ----------------------------------------------------

#[test]
fn two_factor_enabled_with_an_empty_secret_refuses_entry() -> TestResult {
    // The bug `auth.py:532-537` records: this once returned True, bypassing the
    // second factor entirely. `totp_secret: ""` decodes to a zero-length key, so
    // the length floor in `parse_secret` is what makes this a refusal.
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;

    plant(&auth, |doc| {
        doc["users"]["alice"]["totp_secret"] = serde_json::Value::String(String::new());
    })?;

    assert!(auth.totp_enabled("alice")?, "still flagged as enabled");
    assert!(
        !auth.totp_verify("alice", "123456", NOW)?,
        "an enabled-but-secretless user must be refused, not admitted"
    );
    Ok(())
}

#[test]
fn two_factor_enabled_with_an_unparseable_secret_refuses_entry() -> TestResult {
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;

    plant(&auth, |doc| {
        doc["users"]["alice"]["totp_secret"] = serde_json::Value::String("!!!not base32".into());
    })?;

    assert!(
        !auth.totp_verify("alice", "123456", NOW)?,
        "a corrupt secret must refuse, never error into an allow"
    );
    Ok(())
}

#[test]
fn two_factor_disabled_admits_the_login() -> TestResult {
    // auth.py:529-531 and tests/test_totp_failclosed.py:17-21. Not a contradiction
    // of the fail-closed rule: a user who never enrolled has nothing to fail.
    let (_dir, auth) = with_alice()?;
    assert!(auth.totp_verify("alice", "", NOW)?);
    assert!(auth.totp_verify("alice", "anything at all", NOW)?);
    Ok(())
}

#[test]
fn an_unknown_user_is_refused_by_the_second_factor() -> TestResult {
    let (_dir, auth) = with_alice()?;
    assert!(
        !auth.totp_verify("nobody", "123456", NOW)?,
        "an unknown account cannot satisfy a second factor"
    );
    Ok(())
}

// -- login verification ------------------------------------------------------

#[test]
fn the_current_code_verifies_and_a_wrong_one_does_not() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let (secret, _uri) = auth.totp_generate_secret("alice")?;
    let code = totp::current_code(&totp::parse_secret(&secret)?, NOW)?;
    auth.totp_confirm_enable("alice", &code, NOW)?;

    assert!(
        auth.totp_verify("alice", &code, NOW)?,
        "the right code must pass"
    );
    assert!(
        !auth.totp_verify("alice", "000000", NOW)? || code == "000000",
        "a wrong code must fail"
    );
    Ok(())
}

#[test]
fn a_code_from_one_step_outside_the_window_is_refused() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let (secret, _uri) = auth.totp_generate_secret("alice")?;
    let parsed = totp::parse_secret(&secret)?;
    let code = totp::current_code(&parsed, NOW)?;
    auth.totp_confirm_enable("alice", &code, NOW)?;

    // Two steps away, in both directions.
    for offset in [60i64, -60] {
        let stale = totp::current_code(
            &parsed,
            NOW.cast_signed().wrapping_add(offset).cast_unsigned(),
        )?;
        assert!(
            !auth.totp_verify("alice", &stale, NOW)?,
            "{offset}s must fail"
        );
    }
    // And one step either side is inside the window.
    for offset in [30i64, -30] {
        let ok = totp::current_code(
            &parsed,
            NOW.cast_signed().wrapping_add(offset).cast_unsigned(),
        )?;
        assert!(auth.totp_verify("alice", &ok, NOW)?, "{offset}s must pass");
    }
    Ok(())
}

#[test]
fn a_backup_code_admits_the_login_and_is_then_spent() -> TestResult {
    // auth.py:539-546: checked before the TOTP code, and removed on use.
    let (_dir, auth) = with_alice()?;
    let codes = enrol(&auth)?;
    let chosen = codes[2].clone();

    assert!(
        auth.totp_verify("alice", &chosen, NOW)?,
        "first use must pass"
    );
    assert_eq!(
        auth.totp_remaining_backup_codes("alice")?.len(),
        BACKUP_CODE_COUNT - 1,
        "the used code must be gone"
    );
    assert!(
        !auth.totp_verify("alice", &chosen, NOW)?,
        "a backup code is single-use"
    );
    Ok(())
}

#[test]
fn a_backup_code_is_accepted_even_with_no_usable_totp_secret() -> TestResult {
    // The backup path is checked *before* the secret is parsed, matching Python's
    // ordering. An enrolled user locked out by a corrupt secret can still use a code.
    let (_dir, auth) = with_alice()?;
    let codes = enrol(&auth)?;

    plant(&auth, |doc| {
        doc["users"]["alice"]["totp_secret"] = serde_json::Value::String("!!!".into());
    })?;

    let chosen = codes[0].clone();
    assert!(auth.totp_verify("alice", &chosen, NOW)?);
    assert!(!auth.totp_verify("alice", "000000", NOW)?);
    Ok(())
}

#[test]
fn spending_the_last_backup_code_leaves_an_empty_list_not_a_stale_one() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let codes = enrol(&auth)?;
    for code in &codes {
        assert!(
            auth.totp_verify("alice", code, NOW)?,
            "{code} must pass once"
        );
    }
    assert!(
        auth.totp_remaining_backup_codes("alice")?.is_empty(),
        "every code must be spent"
    );
    // And the key must be absent from the file, not an empty array that reads as
    // "codes remain" to a UI.
    // Python stores the emptied list rather than dropping the key (`auth.py:543`),
    // and `has no codes left` is the same state either way. Asserted so a change
    // to omitting it is a deliberate choice rather than an accident.
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(auth.auth_path())?)?;
    assert_eq!(
        doc["users"]["alice"]["totp_backup_codes"]
            .as_array()
            .map(Vec::len),
        Some(0),
        "the exhausted list must be written as empty, not left populated: {doc}"
    );
    Ok(())
}

#[test]
fn an_empty_or_absent_backup_code_never_admits_a_login() -> TestResult {
    // A blank code must not match an empty stored entry.
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;
    for blank in ["", " "] {
        assert!(
            !auth.totp_verify("alice", blank, NOW)?,
            "{blank:?} must not admit"
        );
    }
    Ok(())
}

// -- disabling ---------------------------------------------------------------

#[test]
fn disabling_requires_the_password() -> TestResult {
    // auth.py:552-553.
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;

    let err = auth
        .totp_disable("alice", "wrong-password")
        .expect_err("must refuse");
    assert!(matches!(err, AuthError::Denied(_)), "{err:?}");
    assert!(
        auth.totp_enabled("alice")?,
        "a refused disable must leave 2FA on"
    );
    Ok(())
}

#[test]
fn disabling_with_the_password_clears_the_secrets_and_the_flag_together() -> TestResult {
    // auth.py:556-559. One write, so no reader sees the flag off beside a live secret.
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;

    auth.totp_disable("alice", "hunter2hunter2")?;

    assert!(!auth.totp_enabled("alice")?);
    let user = auth.user("alice")?;
    assert!(user.totp_secret.is_none(), "the secret must be cleared");
    assert!(user.totp_secret_pending.is_none());
    assert!(user.totp_backup_codes.is_none(), "codes must be cleared");
    assert!(auth.totp_remaining_backup_codes("alice")?.is_empty());

    let raw = std::fs::read_to_string(auth.auth_path())?;
    for leaked in ["totp_secret", "totp_secret_pending", "totp_backup_codes"] {
        assert!(!raw.contains(leaked), "{leaked} must be absent: {raw}");
    }

    // And login works again with no second factor.
    assert!(auth.totp_verify("alice", "", NOW)?);
    Ok(())
}

#[test]
fn disabling_a_user_without_two_factor_is_harmless() -> TestResult {
    let (_dir, auth) = with_alice()?;
    auth.totp_disable("alice", "hunter2hunter2")?;
    assert!(!auth.totp_enabled("alice")?);
    Ok(())
}

#[test]
fn disabling_an_unknown_user_reports_not_found() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let err = auth
        .totp_disable("nobody", "hunter2hunter2")
        .expect_err("must refuse");
    assert!(matches!(err, AuthError::NoSuchUser(_)), "{err:?}");
    Ok(())
}

// -- re-enrolment -----------------------------------------------------------

#[test]
fn re_enrolling_replaces_the_secret_and_the_codes() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let first = enrol(&auth)?;
    let (second_secret, _uri) = auth.totp_generate_secret("alice")?;

    // Staging a pending secret changes nothing yet: the live secret and the old
    // codes keep working until `totp_confirm_enable` succeeds. A half-finished
    // re-enrolment must not lock the user out of the one they already had.
    let old_secret = auth.user("alice")?.totp_secret.expect("secret");
    let old_code = totp::current_code(&totp::parse_secret(&old_secret)?, NOW)?;
    assert!(
        auth.totp_verify("alice", &old_code, NOW)?,
        "the previous secret must survive a staged re-enrolment"
    );
    assert!(auth.totp_verify("alice", &first[0], NOW)?);

    let new_code = totp::current_code(&totp::parse_secret(&second_secret)?, NOW)?;
    let second = auth.totp_confirm_enable("alice", &new_code, NOW)?;

    // Now the swap is complete: the old secret and the old codes are both dead.
    assert!(
        !auth.totp_verify("alice", &old_code, NOW)?,
        "a replaced secret must not verify"
    );
    assert!(auth.totp_verify("alice", &new_code, NOW)?);
    assert_eq!(auth.totp_remaining_backup_codes("alice")?, second);
    assert_ne!(first, second, "a re-enrolment must mint fresh codes");
    assert!(
        !auth.totp_verify("alice", &first[0], NOW)?,
        "a completed re-enrolment must invalidate the previous backup codes"
    );
    Ok(())
}

#[test]
fn a_provisioning_uri_can_be_re_rendered_for_an_enrolled_user() -> TestResult {
    let (_dir, auth) = with_alice()?;
    enrol(&auth)?;
    let uri = auth.totp_provisioning_uri("alice")?;
    assert!(
        uri.starts_with("otpauth://totp/Odysseus:alice?secret="),
        "{uri}"
    );
    Ok(())
}

#[test]
fn a_provisioning_uri_for_a_user_with_no_secret_is_an_error_not_a_bare_uri() -> TestResult {
    let (_dir, auth) = with_alice()?;
    let err = auth
        .totp_provisioning_uri("alice")
        .expect_err("nothing to render");
    assert!(matches!(err, AuthError::Totp(_)), "{err:?}");
    Ok(())
}

// -- durability --------------------------------------------------------------

#[test]
fn the_enrolment_survives_a_reopen_of_the_store() -> TestResult {
    // The state is in `auth.json`, so it has to round-trip through the file.
    let dir = tempfile::tempdir()?;
    {
        let auth = AuthStore::new(dir.path());
        auth.create_user("alice", "hunter2hunter2", false)?;
        enrol(&auth)?;
    }

    let reopened = AuthStore::new(dir.path());
    assert!(reopened.totp_enabled("alice")?);
    let secret = reopened.user("alice")?.totp_secret.expect("secret");
    let code = totp::current_code(&totp::parse_secret(&secret)?, NOW)?;
    assert!(
        reopened.totp_verify("alice", &code, NOW)?,
        "a reopened store must still verify codes"
    );
    assert_eq!(
        reopened.totp_remaining_backup_codes("alice")?.len(),
        BACKUP_CODE_COUNT
    );
    Ok(())
}

#[test]
fn concurrent_enrolment_leaves_exactly_one_live_secret() -> TestResult {
    // Eight threads each generating then confirming. Whatever interleaving occurs,
    // the file must end with 2FA enabled, no pending secret, and one code list.
    let (_dir, auth) = with_alice()?;
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let auth = auth.clone();
            scope.spawn(move || {
                let Ok((secret, _)) = auth.totp_generate_secret("alice") else {
                    return;
                };
                let Ok(parsed) = totp::parse_secret(&secret) else {
                    return;
                };
                let code = totp::current_code(&parsed, NOW).expect("code");
                let _ = auth.totp_confirm_enable("alice", &code, NOW);
            });
        }
    });

    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(auth.auth_path())?)?;
    let alice = &doc["users"]["alice"];
    assert_eq!(alice["totp_enabled"], true, "2FA must end up enabled");
    assert!(
        alice["totp_secret_pending"].is_null(),
        "no pending secret may survive: {alice}"
    );
    assert_eq!(
        alice["totp_backup_codes"].as_array().map(Vec::len),
        Some(BACKUP_CODE_COUNT),
        "exactly one code list of the right size: {alice}"
    );
    Ok(())
}
