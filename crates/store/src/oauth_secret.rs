//! Secret handling for provider OAuth credentials.
//!
//! T0308 (device flow) and T0310 (`ChatGPT` subscription refresh) both hold bearer
//! credentials — a `device_code`, a `device_auth_id`, a PKCE `code_verifier`, an
//! `access_token`, a `refresh_token` — and both persist them. This module exists so
//! that handling is decided **once**: one wrapper type, one redaction story, one
//! place where the `Debug`/`Display`/`Serialize` story is enforced.
//!
//! The duplication this prevents is the T0306 trap in a security-critical corner. Two
//! modules each rolling their own "redacted debug" wrapper drift, and the drift is
//! invisible until a token reaches a log line.
//!
//! # The structural rule
//!
//! [`OAuthSecret`] deliberately does **not** implement [`serde::Serialize`], and
//! neither does any type that contains one. That is the whole design: a secret is
//! not serializable, so it cannot be returned from a handler, embedded in a
//! response body, or written into a snapshot by accident. Making the invariant a
//! *filter* instead — a `#[serde(skip_serializing)]` on the right field — would
//! leave the type serializable and rely on every future call site being careful.
//!
//! Where a secret must cross a serialization boundary, it crosses as the *stored*
//! form through [`OAuthSecret::expose`], at a call site that has decided to. The
//! browser-facing types in [`crate::oauth_device_flow`] do not use that path at all:
//! they have no field that can hold a secret.

use std::fmt;

/// A credential that must not appear in a log, a panic message, or a response body.
///
/// Wraps a `String` and exposes it only through [`Self::expose`]. `Debug` prints
/// `OAuthSecret(<redacted>)`, and there is no `Display` at all, so the common
/// accident — `format!("{secret}")`, or a struct that derives `Debug` and gets
/// `{:?}`-logged one frame up — cannot disclose it.
///
/// Not `Serialize`. That is the load-bearing part; see the module docs.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthSecret(Box<str>);

impl OAuthSecret {
    /// Wraps a secret.
    #[must_use]
    pub fn new(secret: impl Into<Box<str>>) -> Self {
        Self(secret.into())
    }

    /// Wraps an optional secret.
    #[must_use]
    pub fn from_option(secret: Option<impl Into<Box<str>>>) -> Option<Self> {
        secret.map(Self::new)
    }

    /// Reveals the secret.
    ///
    /// Named `expose` rather than `get` or `as_str` so that a call site reads as a
    /// deliberate act at the point it happens, in a diff, years later.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper, yielding the secret.
    #[must_use]
    pub fn into_inner(self) -> Box<str> {
        self.0
    }

    /// Whether the secret is empty.
    ///
    /// An empty token is a real failure mode — a provider returning `""` rather
    /// than omitting the field — and treating it as present would send
    /// `Authorization: Bearer ` and look authenticated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for OAuthSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OAuthSecret(<redacted>)")
    }
}

impl fmt::Display for OAuthSecret {
    /// Always redacted.
    ///
    /// A `Display` that returned the secret would defeat the wrapper: any
    /// `format!("{secret}")`, any `{}`-formatted struct field, any error built with
    /// `thiserror` interpolation would leak. There is no legitimate reason to
    /// interpolate a credential into a message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A secret plus the provider fields that travel with it.
///
/// Bundled because a device flow's payload is not one value: Copilot needs the
/// `device_code` *and* the GitHub host, and `ChatGPT` needs the `device_auth_id`
/// *and* the `user_code` *and* a PKCE `code_verifier`. Modelling that as a struct
/// with named secret fields keeps each field's redaction visible in one `Debug`
/// line, instead of spreading bare `Option<OAuthSecret>` across a payload map where
/// a new field could be added un-redacted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderCredential {
    /// The bearer credential itself.
    pub secret: Option<OAuthSecret>,
    /// A PKCE `code_verifier`, where the provider uses one.
    ///
    /// A verifier is a secret for the same reason a `device_code` is: whoever holds
    /// it can complete the exchange. It is a separate field rather than folded into
    /// `secret` because the two have different lifetimes — the verifier is spent at
    /// the authorization-code exchange, while `secret` may be a long-lived token.
    pub code_verifier: Option<OAuthSecret>,
}

impl ProviderCredential {
    /// A credential with only a secret.
    #[must_use]
    pub fn secret(secret: OAuthSecret) -> Self {
        Self {
            secret: Some(secret),
            code_verifier: None,
        }
    }

    /// A credential with a secret and a PKCE verifier.
    #[must_use]
    pub fn with_verifier(secret: OAuthSecret, verifier: OAuthSecret) -> Self {
        Self {
            secret: Some(secret),
            code_verifier: Some(verifier),
        }
    }

    /// Whether there is nothing to send.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secret.as_ref().is_none_or(OAuthSecret::is_empty)
    }
}

/// Compares two secrets without leaking their contents through timing.
///
/// `AGENTS.md` §4 requires `subtle` for secret comparison, so this delegates rather
/// than hand-rolling a comparison. The length check up front is not a leak: length is
/// visible in every HTTP framing of the value, and `subtle`'s own
/// `ConstantTimeEq` for slices has the same property.
#[must_use]
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;

    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::{OAuthSecret, ProviderCredential, constant_time_eq};

    #[test]
    fn debug_and_display_never_reveal() {
        let s = OAuthSecret::new("gho_supersecrettoken");
        assert_eq!(format!("{s:?}"), "OAuthSecret(<redacted>)");
        assert_eq!(format!("{s}"), "<redacted>");
        assert!(!format!("{s:?}{s}").contains("supersecret"));
    }

    #[test]
    fn a_derived_debug_does_not_reach_the_secret() {
        #[derive(Debug)]
        #[expect(dead_code, reason = "field is read by the derived Debug")]
        struct Holder {
            token: OAuthSecret,
        }
        let rendered = format!(
            "{:?}",
            Holder {
                token: OAuthSecret::new("tok_value_here"),
            }
        );
        assert!(!rendered.contains("tok_value_here"), "{rendered}");
    }

    #[test]
    fn expose_is_the_only_way_out() {
        let s = OAuthSecret::new("abc");
        assert_eq!(s.expose(), "abc");
        assert_eq!(s.clone().into_inner().as_ref(), "abc");
    }

    #[test]
    fn an_empty_secret_is_recognised() {
        assert!(OAuthSecret::new("").is_empty());
        assert!(!OAuthSecret::new("x").is_empty());
    }

    #[test]
    fn a_credential_is_empty_without_a_secret() {
        assert!(ProviderCredential::default().is_empty());
        assert!(ProviderCredential::secret(OAuthSecret::new("")).is_empty());
        assert!(!ProviderCredential::secret(OAuthSecret::new("t")).is_empty());
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "a"));
        assert!(constant_time_eq("", ""));
    }
}
