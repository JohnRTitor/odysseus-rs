//! Encryption at rest for columns that hold secrets.
//!
//! The Python source uses Fernet through a `SQLAlchemy` `TypeDecorator`
//! (`core/database.py:150-172`), applied to six columns. This module is the
//! Rust equivalent and is deliberately *not* a `SQLAlchemy`-style implicit
//! wrapper: every call site states explicitly whether it is encrypting or
//! decrypting, so a missing call is visible in review rather than hidden in a
//! type.
//!
//! # Compatibility requirement
//!
//! `data/.app_key` holds a 44-byte Fernet key and **must stay readable**.
//! Changing the format orphans every encrypted column in an existing install:
//! model endpoint API keys, provider OAuth access/refresh tokens, MCP OAuth
//! tokens, and signature PNG/SVG data.

use std::path::Path;

use {fernet::Fernet, thiserror::Error};

/// Failures from encrypt/decrypt.
#[derive(Debug, Error)]
pub enum CryptoError {
    /// The key file could not be read.
    #[error("key file at {path} unreadable: {source}")]
    ReadKey {
        /// The path attempted.
        path: String,
        /// The underlying IO failure.
        #[source]
        source: std::io::Error,
    },

    /// The key was not a valid Fernet key.
    ///
    /// A Fernet key is exactly 32 url-safe base64 bytes. On an existing install
    /// this means `data/.app_key` is wrong, which orphans every encrypted
    /// column — so it is reported loudly rather than treated as "no secrets".
    #[error("invalid fernet key (expected 32 url-safe base64 bytes): {0}")]
    InvalidKey(String),

    /// The ciphertext could not be decrypted.
    ///
    /// For a real install this means the value was not produced by this key.
    #[error("decrypt failed (wrong key, or the stored value is corrupt): {0}")]
    Decrypt(String),
}

/// The marker distinguishing a ciphertext from a plaintext value.
///
/// Matches the Python format exactly (`core/crypto.py`): a stored value is
/// encrypted iff it begins with this. The three idempotence checks in the Python
/// migrations rely on it, so changing it would make them re-encrypt on every run.
pub const ENCRYPTED_PREFIX: &str = "enc:";

/// Loads a Fernet key from disk.
///
/// # Errors
///
/// Returns [`CryptoError::ReadKey`] if the file cannot be read and
/// [`CryptoError::InvalidKey`] if its contents are not a Fernet key.
pub fn load_key(path: impl AsRef<Path>) -> Result<Fernet, CryptoError> {
    let path = path.as_ref();
    let raw = std::fs::read_to_string(path).map_err(|source| CryptoError::ReadKey {
        path: path.display().to_string(),
        source,
    })?;
    Fernet::new(raw.trim()).ok_or_else(|| CryptoError::InvalidKey(path.display().to_string()))
}

/// Encrypts `plaintext`, returning the stored representation.
///
/// Matches the Python format: a ciphertext string prefixed with `enc:`. The
/// prefix is what makes the three idempotence checks in the Python migrations
/// (`is_encrypted()`) work, so re-running them is safe.
///
/// Empty input is returned unchanged so an optional column round-trips to `NULL`
/// rather than to the ciphertext of an empty string.
///
/// Fernet places no upper bound on plaintext length, so signature PNGs are
/// fine.
#[must_use]
pub fn encrypt(key: &Fernet, plaintext: &str) -> String {
    if plaintext.is_empty() {
        return String::new();
    }
    format!("{ENCRYPTED_PREFIX}{}", key.encrypt(plaintext.as_bytes()))
}

/// Decrypts a value produced by [`encrypt`].
///
/// Passes non-prefixed values through unchanged, because the Python migrations
/// (`core/database.py:2326-2413`) deliberately encrypt legacy plaintext rows
/// with raw SQL that bypassed the `TypeDecorator`. Refusing to read plaintext
/// would make those rows permanently unreadable.
///
/// # Errors
///
/// Returns [`CryptoError::Decrypt`] if the value is prefixed but cannot be
/// decrypted.
pub fn maybe_decrypt(key: &Fernet, stored: &str) -> Result<String, CryptoError> {
    let Some(token) = stored.strip_prefix(ENCRYPTED_PREFIX) else {
        return Ok(stored.to_owned());
    };
    let plaintext = key
        .decrypt(token)
        .map_err(|e| CryptoError::Decrypt(e.to_string()))?;
    String::from_utf8(plaintext).map_err(|e| CryptoError::Decrypt(e.to_string()))
}

/// True when `stored` is already in the encrypted representation.
///
/// Lets callers skip re-encrypting an already-encrypted value, which matters
/// because the Python migrations rely on being idempotent.
#[must_use]
pub fn is_encrypted(stored: &str) -> bool {
    stored.starts_with(ENCRYPTED_PREFIX)
}

#[cfg(test)]
mod tests {
    use {
        super::{CryptoError, encrypt, is_encrypted, maybe_decrypt},
        fernet::Fernet,
    };

    fn test_key() -> Fernet {
        // 32 url-safe base64 bytes, which is what a real `.app_key` holds.
        Fernet::new("ABEiM0RVZneImaq7zN3u_wECAwQFBgcICQoLDA0ODxA=")
            .expect("fixture key is well-formed")
    }

    #[test]
    fn round_trips() {
        let key = test_key();
        let stored = encrypt(&key, "sk-secret-value");
        assert!(is_encrypted(&stored), "must carry the enc: prefix");
        assert_eq!(
            maybe_decrypt(&key, &stored).expect("decrypt"),
            "sk-secret-value"
        );
    }

    #[test]
    fn empty_stays_empty_so_optional_columns_round_trip_to_null() {
        let key = test_key();
        assert_eq!(encrypt(&key, ""), "");
    }

    #[test]
    fn legacy_plaintext_passes_through() {
        // The Python migrations encrypted legacy rows with raw SQL that
        // bypassed the TypeDecorator, so plaintext rows exist in real
        // databases and must remain readable.
        let key = test_key();
        assert_eq!(
            maybe_decrypt(&key, "plaintext-from-legacy").expect("pass through"),
            "plaintext-from-legacy"
        );
    }

    #[test]
    fn wrong_key_fails_loudly_rather_than_returning_garbage() {
        let stored = encrypt(&test_key(), "secret");
        let other = Fernet::new("WFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWDI=")
            .expect("second fixture key is well-formed");
        let result = maybe_decrypt(&other, &stored);
        assert!(
            matches!(result, Err(CryptoError::Decrypt(_))),
            "a wrong key must error, not yield empty or wrong plaintext"
        );
    }

    #[test]
    fn ciphertext_is_not_the_plaintext() {
        let key = test_key();
        let stored = encrypt(&key, "hunter2");
        assert!(
            !stored.contains("hunter2"),
            "plaintext must not be recoverable by eye"
        );
    }
}

#[cfg(test)]
mod key_tests {
    //! `load_key` had no test anywhere in the workspace: its only caller is the
    //! real-database check in `tests/baseline.rs`, which returns early when
    //! `DATA_DIR` is unset — so in CI it was dead code. It is also the function
    //! whose doc calls the 44-byte Fernet format a hard compatibility requirement,
    //! so "the key loads" is worth proving directly.

    use {
        super::{CryptoError, ENCRYPTED_PREFIX, encrypt, load_key, maybe_decrypt},
        fernet::Fernet,
    };

    fn write(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(".app_key");
        std::fs::write(&path, contents).expect("write key");
        (dir, path)
    }

    #[test]
    fn a_generated_key_loads_and_round_trips() {
        let key = Fernet::generate_key();
        let (_dir, path) = write(&key);
        let loaded = load_key(&path).expect("load");
        let sealed = encrypt(&loaded, "hello");
        assert!(sealed.starts_with(ENCRYPTED_PREFIX));
        assert_eq!(maybe_decrypt(&loaded, &sealed).expect("decrypt"), "hello");
    }

    #[test]
    fn surrounding_whitespace_is_tolerated_because_the_python_writes_a_trailing_newline() {
        let key = Fernet::generate_key();
        let (_dir, path) = write(&format!("{key}\n"));
        assert!(
            load_key(&path).is_ok(),
            "a trailing newline must not break it"
        );
    }

    #[test]
    fn a_non_fernet_key_is_rejected_rather_than_accepted_as_something_weaker() {
        let (_dir, path) = write("definitely-not-a-fernet-key");
        // Mapped to `()` first: `expect_err` and `panic!("{other:?}")` would both
        // need `Fernet: Debug`, which it deliberately is not — a key must never be
        // printable. `clippy::panic` is denied workspace-wide, so this is asserted
        // rather than panicked.
        let outcome: Result<(), CryptoError> = load_key(&path).map(|_| ());
        assert!(
            matches!(outcome, Err(CryptoError::InvalidKey(_))),
            "a malformed key must be refused with InvalidKey"
        );
    }

    #[test]
    fn a_missing_key_file_reports_the_path_it_tried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("nope");
        let outcome: Result<(), CryptoError> = load_key(&missing).map(|_| ());
        assert!(
            matches!(
                outcome,
                Err(CryptoError::ReadKey { ref path, .. }) if path.ends_with("nope")
            ),
            "a missing key file must report ReadKey naming the path it tried"
        );
    }

    #[test]
    fn a_key_file_of_the_wrong_length_is_rejected() {
        // A 32-byte secret is a valid key for many primitives but not for Fernet.
        // Accepting it would mean writing ciphertext nothing else can read.
        let (_dir, path) = write(&"A".repeat(32));
        assert!(load_key(&path).is_err());
    }
}
