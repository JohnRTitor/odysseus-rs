//! TOTP two-factor authentication.
//!
//! Ports the second factor of `core/auth.py:483-562`. The *state* lives in
//! `auth.json` (`totp_enabled`, `totp_secret`, `totp_secret_pending`,
//! `totp_backup_codes`), which [`crate::auth::AuthStore`] owns; this module holds
//! the pure logic and the `totp-rs` wrapper.
//!
//! # Invariants that must survive the port
//!
//! * **2FA fails closed.** A user with `totp_enabled` but no stored secret is
//!   *refused*, never waved through. `auth.py:532-537` records that this once
//!   returned `True`, which bypassed the second factor entirely; the fix is the
//!   reason the ordering below matters.
//! * **"not enabled" is checked first, and returns `True`.** That is not a
//!   contradiction of the above: a user who has not enabled 2FA has no second
//!   factor to fail. `tests/test_totp_failclosed.py:17-21` pins it.
//! * **Backup codes are checked before the TOTP code** and are single-use: a
//!   match is removed from the stored list and written back
//!   (`auth.py:539-546`).
//! * **`totp_disable` requires the password** (`auth.py:552-553`) and clears the
//!   secrets and the flag in one write, so a reader can never see
//!   `totp_enabled == false` alongside a live secret.
//!
//! # Parameters
//!
//! | Parameter | Value | Source |
//! |---|---|---|
//! | algorithm | SHA1 | `pyotp/otp.py:33-34` |
//! | digits | 6 | `pyotp/otp.py:15` |
//! | period | 30 s | `pyotp/totp.py:23` |
//! | skew | ±1 step (3 windows) | `valid_window=1`, `auth.py:512,548` |
//! | secret | 20 bytes / 160 bits / 32 base32 chars | `pyotp.random_base32()`, `auth.py:493` |
//! | backup codes | 8 × 8 lowercase hex (32 bits each) | `secrets.token_hex(4)`, `auth.py:520-521` |
//!
//! `totp-rs` 6's builder defaults are `skew: 1`, `SHA1`, `6` digits, `30` s
//! (verified in `builder.rs`), which happen to match pyotp exactly — so the
//! defaults are stated explicitly here anyway, because relying on a dependency's
//! defaults for a security parameter is how a version bump silently widens the
//! acceptance window.
//!
//! # Known divergences from Python
//!
//! * **Backup-code comparison is constant-time.** `auth.py:540` uses `code in
//!   backup` on a list, which leaks the matching index through timing. The
//!   stored format is unchanged; only the comparison changed.
//! * **No `qr` feature.** `routes/auth_routes.py:258-263` renders the QR PNG
//!   server-side, but image generation is presentation and belongs in the web
//!   layer (AGENTS.md §8). `totp-rs` exposes `to_qr_png` for whoever takes it.

use {
    subtle::ConstantTimeEq,
    thiserror::Error,
    totp_rs::{Algorithm, Builder, Secret},
};

/// Issuer shown in an authenticator app, from `issuer_name="Odysseus"`
/// (`auth.py:502`).
pub const ISSUER: &str = "Odysseus";

/// Number of backup codes minted on enable, from `range(8)` (`auth.py:520`).
pub const BACKUP_CODE_COUNT: usize = 8;

/// Random bytes per backup code, from `secrets.token_hex(4)` (`auth.py:520`).
///
/// 4 bytes is 32 bits. That is Python's choice and it is preserved: the codes are
/// stored in `auth.json`, so changing the width would lock out anyone who has
/// already enrolled.
pub const BACKUP_CODE_BYTES: usize = 4;

/// Minimum secret length in bytes, from `pyotp.random_base32`'s 160-bit floor.
///
/// Enforced on the way *in* as well as out, because an empty `totp_secret` in a
/// hand-edited `auth.json` decodes successfully to a zero-length key.
pub const MIN_SECRET_BYTES: usize = 20;

/// Time steps either side of now that a code is accepted for.
///
/// pyotp's `valid_window=1` accepts `t-1`, `t`, `t+1` (`pyotp/totp.py:80-84`).
pub const SKEW: u16 = 1;

/// Failures from the TOTP layer.
#[derive(Debug, Error)]
pub enum TotpError {
    /// The stored secret is not valid base32.
    ///
    /// Fails closed at the call site: a user with 2FA enabled and an unparseable
    /// secret is refused, never admitted.
    #[error("totp secret is not valid base32: {0}")]
    MalformedSecret(String),

    /// The `otpauth://` URI could not be built.
    #[error("could not build the provisioning uri: {0}")]
    Provisioning(String),

    /// The OS entropy source failed.
    ///
    /// Failing closed matters more here than anywhere else in the store: a
    /// guessable TOTP seed is a second factor anyone can compute.
    #[error("no entropy for a totp secret: {0}")]
    Entropy(String),
}

/// Mints a new TOTP secret: 20 bytes, 160 bits.
///
/// `pyotp.random_base32()` rejects anything under 160 bits, and
/// `totp_rs::Secret::generate()` produces exactly 20 bytes, so the two agree.
///
/// # Errors
///
/// Returns [`TotpError::Entropy`] if the OS entropy source fails.
pub fn generate_secret() -> Result<Secret, TotpError> {
    // `Secret::generate` is infallible in totp-rs 6 and draws from the OS, but the
    // value is still checked so a future version cannot hand back a short secret.
    let secret = Secret::generate();
    if secret.as_bytes().len() < MIN_SECRET_BYTES {
        return Err(TotpError::Entropy(
            "generated secret is shorter than 160 bits".into(),
        ));
    }
    Ok(secret)
}

/// Parses a base32 secret as stored in `auth.json`.
///
/// # Errors
///
/// Returns [`TotpError::MalformedSecret`] if it does not decode, **or if it
/// decodes to fewer than [`MIN_SECRET_BYTES`]**.
///
/// That length floor is a security requirement, not pedantry. `Secret::try_from_base32("")`
/// succeeds and yields a zero-length key, so a `auth.json` carrying
/// `"totp_secret": ""` alongside `"totp_enabled": true` would otherwise be
/// *admitted* and have its codes checked against a publicly known key. pyotp
/// refuses to generate anything under 160 bits for the same reason.
pub fn parse_secret(stored: &str) -> Result<Secret, TotpError> {
    let secret =
        Secret::try_from_base32(stored).map_err(|e| TotpError::MalformedSecret(e.to_string()))?;
    if secret.as_bytes().len() < MIN_SECRET_BYTES {
        return Err(TotpError::MalformedSecret(format!(
            "secret is {} bytes, below the {MIN_SECRET_BYTES}-byte minimum",
            secret.as_bytes().len()
        )));
    }
    Ok(secret)
}

/// The base32 form to persist into `auth.json`.
///
/// `pyotp` re-pads and case-folds when decoding (`pyotp/otp.py:53-56`), so the
/// stored casing is not load-bearing; this writes the canonical uppercase form.
#[must_use]
pub fn secret_to_base32(secret: &Secret) -> String {
    secret.to_base32()
}

/// Builds the `otpauth://` URI for an authenticator app to scan.
///
/// Byte-identical to pyotp's output for a simple username:
///
/// ```text
/// otpauth://totp/Odysseus:alice?secret=<SECRET>&issuer=Odysseus
/// ```
///
/// `algorithm` and `period` are omitted because they equal pyotp's defaults
/// (`pyotp/utils.py:45-47`), and a space would be `%20` rather than `+`
/// (`pyotp/utils.py:76`), which `totp-rs`'s percent-encoding also produces.
///
/// # Errors
///
/// Returns [`TotpError::MalformedSecret`] if the secret does not decode, and
/// [`TotpError::Provisioning`] if the URI cannot be assembled.
pub fn provisioning_uri(username: &str, secret: &Secret) -> Result<String, TotpError> {
    build_totp(username, secret)?
        .to_url()
        .map_err(|e| TotpError::Provisioning(e.to_string()))
}

/// Verifies a 6-digit TOTP code.
///
/// `now` is Unix seconds and is a parameter rather than read from a clock, so
/// tests are deterministic (AGENTS.md §11).
#[must_use]
pub fn verify_code(secret: &Secret, code: &str, now: u64) -> bool {
    build_totp("", secret).is_ok_and(|totp| totp.check(code, now).is_some())
}

/// The code that would be valid at `now`. Test and diagnostic aid only.
///
/// # Errors
///
/// Propagates [`TotpError::MalformedSecret`].
pub fn current_code(secret: &Secret, now: u64) -> Result<String, TotpError> {
    Ok(build_totp("", secret)?.generate(now).to_string())
}

/// Builds a `Totp` with this project's parameters stated explicitly.
fn build_totp(account_name: &str, secret: &Secret) -> Result<totp_rs::Totp, TotpError> {
    let mut builder = Builder::new()
        .with_algorithm(Algorithm::SHA1)
        .with_digits(6)
        .with_step_duration(30)
        .with_skew(SKEW)
        .with_secret(secret.as_bytes());

    if !account_name.is_empty() {
        builder = builder
            .with_account_name(account_name)
            .with_issuer(Some(ISSUER));
    }

    builder
        .build()
        .map_err(|e| TotpError::MalformedSecret(e.to_string()))
}

/// Mints [`BACKUP_CODE_COUNT`] backup codes, each 8 lowercase hex characters.
///
/// # Errors
///
/// Returns [`TotpError::Entropy`] if the OS entropy source fails. All codes are
/// drawn or none are: a partial set would leave the user with a half-enrolled
/// second factor.
pub fn generate_backup_codes() -> Result<Vec<String>, TotpError> {
    let mut codes = Vec::with_capacity(BACKUP_CODE_COUNT);
    for _ in 0..BACKUP_CODE_COUNT {
        let mut bytes = [0u8; BACKUP_CODE_BYTES];
        getrandom::fill(&mut bytes)
            .map_err(|e| TotpError::Entropy(format!("no entropy for a backup code: {e}")))?;
        codes.push(hex_lower(&bytes));
    }
    Ok(codes)
}

/// Consumes a backup code if it matches, returning the codes that remain.
///
/// Single-use, matching `auth.py:539-546`: a match is removed and the caller
/// persists the result.
///
/// Comparison is constant-time, unlike Python's `code in backup` on a list, which
/// leaks the matching index through timing. The stored format is untouched; only
/// the comparison changed.
///
/// Stored entries of the wrong width are skipped before comparing. A user copies
/// their codes off the enrolment screen, so a correctly-shaped code always has the
/// right width, and a narrower one in a hand-edited file would otherwise let a
/// blank submission match an empty entry — the fail-open direction.
///
/// Returns `None` when nothing matched, leaving the caller's list untouched.
#[must_use]
pub fn consume_backup_code(stored: &[String], presented: &str) -> Option<Vec<String>> {
    let mut matched: Option<usize> = None;

    for (index, code) in stored.iter().enumerate() {
        if code.len() != BACKUP_CODE_BYTES * 2 {
            continue;
        }
        // `ct_eq` yields 0 on a length mismatch, which is the right answer.
        let equal = code.as_bytes().ct_eq(presented.as_bytes());
        // Every element is visited and no early exit is taken, so the iteration
        // count cannot depend on which one matched. The branch below is on the
        // comparison *outcome*, which the caller already knows -- they chose the
        // code being presented. That is the honest scope of the "constant-time"
        // claim here; `remove` below is likewise a memmove whose length depends on
        // the matched index.
        matched = if bool::from(equal) {
            Some(index)
        } else {
            matched
        };
    }

    let index = matched?;
    let mut remaining = stored.to_vec();
    remaining.remove(index);
    Some(remaining)
}

/// Whether a user with 2FA enabled may be let in.
///
/// The single most security-relevant function in this module, and the reason it
/// exists separately: `auth.py:526-548` checks `totp_enabled` *first* and returns
/// `True` when it is false, then fails closed when the flag is set but the secret
/// is missing. Both halves are load-bearing and the order is not interchangeable.
#[must_use]
pub fn admits_login(totp_enabled: bool, secret_present: bool) -> bool {
    if !totp_enabled {
        // No second factor was ever enrolled, so there is nothing to fail.
        return true;
    }
    // Enabled with no secret means a corrupt or half-written auth.json. Refusing
    // is the whole point: the alternative bypasses 2FA entirely.
    secret_present
}

/// Lowercase hex, matching `secrets.token_hex`.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed instant, so code generation is deterministic.
    const NOW: u64 = 1_758_000_000;

    fn secret() -> Secret {
        parse_secret("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP").expect("secret")
    }

    // -- parameters ---------------------------------------------------------

    #[test]
    fn a_generated_secret_is_twenty_bytes_of_base32() {
        let generated = generate_secret().expect("generate");
        assert_eq!(
            generated.as_bytes().len(),
            20,
            "160 bits, matching pyotp.random_base32()"
        );
        assert_eq!(
            secret_to_base32(&generated).len(),
            32,
            "20 bytes is 32 base32 characters unpadded"
        );
    }

    #[test]
    fn generated_secrets_differ() {
        let a = secret_to_base32(&generate_secret().expect("generate"));
        let b = secret_to_base32(&generate_secret().expect("generate"));
        assert_ne!(a, b);
    }

    #[test]
    fn a_secret_round_trips_through_its_stored_base32_form() {
        let original = generate_secret().expect("generate");
        let stored = secret_to_base32(&original);
        let parsed = parse_secret(&stored).expect("reparse");
        assert_eq!(parsed.as_bytes(), original.as_bytes());
    }

    #[test]
    fn a_malformed_stored_secret_is_refused_rather_than_defaulted() {
        for bad in ["", "not base32!", "1111", "JBSWY3DPEHPK3PXPJBSWY3DP"] {
            assert!(
                parse_secret(bad).is_err(),
                "{bad:?} must not parse into a usable secret"
            );
        }
    }

    #[test]
    fn the_secret_never_prints_itself() {
        // A leaked TOTP seed regenerates valid codes forever, and no session
        // revocation undoes it (AGENTS.md §4, §10).
        let generated = generate_secret().expect("generate");
        let shown = format!("{generated:?}");
        assert_eq!(shown, "REDACTED");
        assert!(!shown.contains(&secret_to_base32(&generated)));
    }

    // -- the fail-closed gate ----------------------------------------------

    #[test]
    fn two_factor_disabled_admits_and_enabled_without_a_secret_refuses() {
        // auth.py:529-537. The two halves are not interchangeable.
        assert!(
            admits_login(false, false),
            "no second factor enrolled means nothing to fail"
        );
        assert!(admits_login(false, true),);
        assert!(
            !admits_login(true, false),
            "enabled with no secret must fail closed, not bypass 2FA"
        );
        assert!(admits_login(true, true));
    }

    // -- code verification --------------------------------------------------

    #[test]
    fn the_current_code_verifies_and_a_wrong_one_does_not() {
        let s = secret();
        let code = current_code(&s, NOW).expect("code");
        assert_eq!(code.len(), 6, "6 digits");
        assert!(verify_code(&s, &code, NOW));
        assert!(!verify_code(&s, "000000", NOW) || code == "000000");
    }

    #[test]
    fn the_acceptance_window_is_exactly_plus_minus_one_step() {
        // pyotp valid_window=1 accepts steps t-1, t, t+1 (`pyotp/totp.py:80-84`).
        //
        // The window is counted in 30-second *steps*, not seconds, so the boundaries
        // are at +/-30 and +/-60 from a step edge. NOW is deliberately on an edge
        // (1_758_000_000 % 30 == 0) so the boundaries are unambiguous.
        assert_eq!(NOW % 30, 0, "NOW must sit on a step boundary for this test");

        let s = secret();
        let at = |t: u64| current_code(&s, t).expect("code");
        let shifted = |offset: i64| NOW.cast_signed().wrapping_add(offset).cast_unsigned();

        for offset in [-30i64, 0, 30] {
            let presented = at(shifted(offset));
            assert!(
                verify_code(&s, &presented, NOW),
                "step {offset}s must be accepted"
            );
        }

        for offset in [-60i64, 60, -90, 90] {
            let presented = at(shifted(offset));
            assert!(
                !verify_code(&s, &presented, NOW),
                "step {offset}s is two or more steps away and must be refused"
            );
        }
    }

    #[test]
    fn the_window_is_measured_in_steps_not_seconds() {
        // +31 s crosses into the next step, which skew 1 admits; -31 s does not,
        // because it is already two steps back. Asserting the asymmetry is what
        // distinguishes "skew is a step count" from "skew is a second count" -- the
        // two differ by up to 30 s at the boundary, which is a real widening if
        // someone reads the parameter as seconds.
        let s = secret();
        let at = |t: u64| current_code(&s, t).expect("code");
        let shifted = |offset: i64| NOW.cast_signed().wrapping_add(offset).cast_unsigned();

        assert!(
            verify_code(&s, &at(shifted(31)), NOW),
            "+31s is step +1 and must be accepted"
        );
        assert!(
            !verify_code(&s, &at(shifted(-31)), NOW),
            "-31s is step -2 and must be refused"
        );
    }

    #[test]
    fn the_window_does_not_widen_with_the_step_boundary() {
        // A skew of 2 would accept 5 steps; pin the parameter that prevents it.
        let s = secret();
        let two_steps_away = current_code(&s, NOW + 60).expect("code");
        assert!(!verify_code(&s, &two_steps_away, NOW));
    }

    #[test]
    fn a_code_from_a_different_secret_is_refused() {
        let a = secret();
        let b = parse_secret("KRSXG5CTMVRXEZLUKRSXG5CTMVRXEZLU").expect("secret");
        let code = current_code(&a, NOW).expect("code");
        assert!(!verify_code(&b, &code, NOW));
    }

    #[test]
    fn malformed_codes_are_refused_without_panicking() {
        let s = secret();
        for bad in ["", "abcdef", "1234567", "      ", "¹²³¹¹"] {
            assert!(!verify_code(&s, bad, NOW), "{bad:?} must not verify");
        }
    }

    // -- provisioning -------------------------------------------------------

    #[test]
    fn the_provisioning_uri_matches_pyotps_shape_exactly() {
        // auth.py:501-502 -> pyotp.utils.base_uri with quote(issuer):quote(name),
        // defaults omitted, and spaces as %20 rather than +.
        let uri = provisioning_uri("alice", &secret()).expect("uri");
        assert!(
            uri.starts_with("otpauth://totp/Odysseus:alice?secret="),
            "{uri}"
        );
        assert!(uri.ends_with("&issuer=Odysseus"), "{uri}");
        // algorithm=SHA1, digits=6 and period=30 are the defaults and are omitted.
        assert!(!uri.contains("algorithm="), "{uri}");
        assert!(!uri.contains("digits="), "{uri}");
        assert!(!uri.contains("period="), "{uri}");
        assert!(!uri.contains('+'), "a space must be %20, not +: {uri}");
    }

    #[test]
    fn the_provisioning_uri_carries_the_actual_secret() {
        let s = secret();
        let uri = provisioning_uri("alice", &s).expect("uri");
        assert!(uri.contains(&secret_to_base32(&s)), "{uri}");
    }

    #[test]
    fn a_username_needing_encoding_does_not_break_the_uri() {
        for name in ["a@b.com", "user name", "ünïcode"] {
            let uri = provisioning_uri(name, &secret()).expect("uri");
            assert!(uri.contains("issuer=Odysseus"), "{name}: {uri}");
            assert!(!uri.contains(' '), "{name}: {uri}");
        }
    }

    // -- backup codes -------------------------------------------------------

    #[test]
    fn eight_backup_codes_of_eight_lowercase_hex_chars_are_minted() {
        let codes = generate_backup_codes().expect("codes");
        assert_eq!(codes.len(), BACKUP_CODE_COUNT);
        let unique: std::collections::HashSet<&String> = codes.iter().collect();
        assert_eq!(unique.len(), BACKUP_CODE_COUNT, "codes must not repeat");
        for code in &codes {
            assert_eq!(code.len(), 8, "token_hex(4) is 8 chars");
            assert!(
                code.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{code} must be lowercase hex"
            );
        }
    }

    #[test]
    fn a_backup_code_is_consumed_exactly_once() {
        let codes = generate_backup_codes().expect("codes");
        let target = codes[3].clone();

        let after = consume_backup_code(&codes, &target).expect("must match");
        assert_eq!(after.len(), codes.len() - 1);
        assert!(!after.contains(&target), "a used code must be gone");

        // Second use fails.
        assert!(
            consume_backup_code(&after, &target).is_none(),
            "a backup code is single-use"
        );
    }

    #[test]
    fn consuming_an_absent_code_leaves_the_list_untouched() {
        let codes = generate_backup_codes().expect("codes");
        assert!(consume_backup_code(&codes, "deadbeef").is_none());
        assert!(consume_backup_code(&codes, "").is_none());
        assert!(
            consume_backup_code(&codes, "DEADBEEF").is_none(),
            "case matters"
        );
        assert!(
            consume_backup_code(&codes, "deadbeefextra").is_none(),
            "width matters"
        );
        assert!(
            consume_backup_code(&[], "deadbeef").is_none(),
            "an empty list matches nothing"
        );
    }

    #[test]
    fn consuming_from_a_list_with_several_matches_removes_exactly_one() {
        // A duplicated stored code must still yield a usable list, not an error.
        let codes = vec![
            "aaaaaaaa".to_owned(),
            "aaaaaaaa".to_owned(),
            "bbbbbbbb".to_owned(),
        ];
        let after = consume_backup_code(&codes, "aaaaaaaa").expect("match");
        assert_eq!(after.len(), 2);
        assert_eq!(after.iter().filter(|c| *c == "aaaaaaaa").count(), 1);
    }

    #[test]
    fn backup_codes_survive_a_json_round_trip_as_plain_strings() {
        // The stored form is a JSON array of strings in auth.json.
        let codes = generate_backup_codes().expect("codes");
        let encoded = serde_json::to_string(&codes).expect("encode");
        let decoded: Vec<String> = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, codes);
        let target = codes[0].clone();
        assert_eq!(
            consume_backup_code(&decoded, &target).expect("match").len(),
            BACKUP_CODE_COUNT - 1
        );
    }

    // -- layout -------------------------------------------------------------

    #[test]
    fn the_error_type_is_small_and_the_secret_is_not_in_it() {
        // AGENTS.md §11: a layout ceiling for a stored type.
        assert!(size_of::<TotpError>() <= 64, "{}", size_of::<TotpError>());
        let err = parse_secret("!!!").expect_err("must fail");
        assert!(
            !format!("{err:?}").contains("JBSWY3DP"),
            "an error must not carry secret material"
        );
    }
}
