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
    format!("enc:{}", key.encrypt(plaintext.as_bytes()))
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
    let Some(token) = stored.strip_prefix("enc:") else {
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
    stored.starts_with("enc:")
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
