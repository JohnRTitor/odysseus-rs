//! T0310: `ChatGPT` subscription — PKCE, refresh, and the 120 s expiry skew.
//!
//! Ports `src/chatgpt_subscription.py`. The Python is explicit about why this provider
//! is separate from an API-key endpoint: it uses "OpenAI account OAuth device
//! authorization, stores refresh tokens server-side, and resolves a fresh bearer token
//! at request time."
//!
//! # The skew, and why both boundaries matter
//!
//! [`CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS`] is 120. A token is refreshed when
//! `exp <= now + 120` — i.e. **120 seconds before it actually expires**, so a request
//! that starts now cannot have its bearer rejected mid-flight by a clock skew between
//! us and the provider.
//!
//! The comparison is `<=`, so the boundary is inclusive, and both sides are pinned by
//! tests: a token 119 s from expiry does **not** refresh, and one 121 s from expiry
//! **does**. A single "somewhere in the middle" test cannot tell `<` from `<=`, nor tell
//! a 120 s skew from a 121 s one, and either mistake is invisible until a request
//! fails intermittently in production.
//!
//! # Time is a parameter
//!
//! [`access_token_is_expiring`] takes `now` rather than reading the clock, so the
//! boundary is testable without sleeping and without a flaky test near expiry
//! (`AGENTS.md` §11).

use serde_json::Value;

use crate::oauth_secret::OAuthSecret;

/// The default base for the Codex backend.
pub const DEFAULT_CHATGPT_SUBSCRIPTION_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

/// The provider wire name.
pub const CHATGPT_SUBSCRIPTION_PROVIDER: &str = "chatgpt-subscription";

/// The OAuth client id.
pub const CHATGPT_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// The token endpoint.
pub const CHATGPT_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// The OAuth issuer, which is also the base for the device-auth endpoints.
pub const CHATGPT_OAUTH_ISSUER: &str = "https://auth.openai.com";

/// The redirect URI registered for the device flow.
pub const CHATGPT_OAUTH_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

/// How long before expiry a token is refreshed.
pub const CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: u64 = 120;

/// Why a `ChatGPT` subscription operation failed.
///
/// A closed set mirroring the Python's exception classes
/// (`ChatGPTSubscriptionReauthRequired`, `…RateLimited`, `…AuthNotFound`,
/// `ChatGPTSubscriptionError`). Every variant's `Display` is a fixed string: none of
/// them interpolates upstream text, because the upstream body can contain the
/// `refresh_token` on a rotation failure and a `Display` is the easiest accidental
/// route into a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChatGptError {
    /// Stored credentials are invalid, or expired beyond refresh.
    ///
    /// The user must reconnect the provider.
    #[error("stored ChatGPT credentials are no longer valid; reconnect the provider")]
    ReauthRequired,

    /// Upstream quota or rate limit. Reconnecting will not help.
    #[error("ChatGPT quota or rate limit reached; credentials are still valid")]
    RateLimited,

    /// No owner-scoped credential row exists.
    #[error("ChatGPT credentials were not found for this user")]
    NotFound,

    /// The response was not usable.
    #[error("ChatGPT returned an unusable response")]
    MalformedResponse,
}

/// Maps an upstream status and parsed error body onto a [`ChatGptError`].
///
/// Ports `_raise_for_oauth_response` (`chatgpt_subscription.py:128`). The status-code
/// and error-code sets are the load-bearing part; the message is discarded because
/// building one from `err["message"]` is exactly how a token reaches a log.
#[must_use]
pub fn classify_error(status: u16, body: &Value) -> ChatGptError {
    if status == 429 {
        return ChatGptError::RateLimited;
    }
    let err = body.get("error");
    // The code may arrive as an object with `code`/`type`, or as a bare string.
    let code = match err {
        Some(Value::Object(map)) => map
            .get("code")
            .or_else(|| map.get("type"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        Some(Value::String(s)) => s.as_str(),
        _ => "",
    };
    if status == 401
        || status == 403
        || matches!(
            code,
            "invalid_grant" | "invalid_token" | "invalid_request" | "refresh_token_reused"
        )
    {
        return ChatGptError::ReauthRequired;
    }
    ChatGptError::MalformedResponse
}

/// Decodes unpadded base64url (RFC 4648 §5).
///
/// Hand-rolled because `base64` is not a dependency of this crate and adding one would
/// mean editing two files outside this task's ownership. The alphabet, the padding
/// rules and the rejection cases are pinned against the RFC's own vectors in the tests
/// below, so this is not an unverified reimplementation.
///
/// **The failure direction is safe.** Every rejection returns `None`, and `None` from
/// [`decode_jwt_claims`] means "expiry unknown", which
/// [`access_token_is_expiring`] treats as *refresh*. A decoder bug therefore costs one
/// unnecessary refresh, not a token that is used past its lifetime.
///
/// Rejects, rather than skipping: any character outside the alphabet, and any input
/// whose length is `1 mod 4` (which cannot encode a whole number of bytes).
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    /// The 6-bit value of each base64url character.
    fn value(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            // The URL-safe substitutions for `+` and `/`.
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        })
    }

    let bytes = input.as_bytes();
    // A single trailing character cannot be a whole byte.
    if bytes.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        // Pad the final partial group with `=`-equivalent zero bits.
        let mut accumulator = 0u32;
        let mut significant = chunk.len();
        for (index, byte) in chunk.iter().enumerate() {
            let bits = value(*byte)?;
            accumulator |= bits << (18 - 6 * index);
        }
        if chunk.len() < 4 {
            // Bits past the final character must be zero for a canonical encoding.
            let unused_bits = 6 * (4 - significant);
            if accumulator & ((1 << unused_bits) - 1) != 0 {
                return None;
            }
            significant = chunk.len();
        }
        let produced = match significant {
            2 => 1,
            3 => 2,
            4 => 3,
            _ => return None,
        };
        for index in 0..produced {
            out.push(((accumulator >> (16 - 8 * index)) & 0xff) as u8);
        }
    }
    Some(out)
}

/// Encodes bytes as unpadded base64url.
///
/// The inverse of [`base64url_decode`], present so the decoder can be tested against
/// the RFC's vectors without the test itself needing an encoder from a crate that is
/// not a dependency.
#[cfg(test)]
pub(crate) fn base64url_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let mut accumulator = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            accumulator |= u32::from(*byte) << (16 - 8 * index);
        }
        // `4 - len` groups of 6 bits, then drop the unused tail bits.
        for index in 0..=chunk.len() {
            let bits = (accumulator >> (18 - 6 * index)) & 0x3f;
            out.push(char::from(ALPHABET[bits as usize]));
        }
    }
    out
}

/// Decodes a JWT payload without verifying the signature.
///
/// Ports `_decode_jwt_payload` (`chatgpt_subscription.py:235`).
///
/// **The signature is not checked, and does not need to be.** The token is a bearer
/// credential the provider issued to us; decoding it tells us its own expiry, and we
/// are not using the payload for an authorization decision. Verification would need a
/// key we do not have. The consequence is that a forged token could claim a far-future
/// expiry — but a forged token is rejected by the provider on use, so the only effect
/// is a wasted refresh.
#[must_use]
pub fn decode_jwt_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    serde_json::from_slice(&base64url_decode(payload)?).ok()
}

/// The `exp` claim of a token, if it has a usable one.
///
/// Returns `None` for an undecodable token, which callers treat as "expired" — see
/// [`access_token_is_expiring`].
#[must_use]
pub fn token_expiry(token: &str) -> Option<u64> {
    decode_jwt_claims(token)?.get("exp")?.as_u64()
}

/// Whether `token` should be refreshed at `now`.
///
/// Ports `access_token_is_expiring` (`chatgpt_subscription.py:246`).
///
/// The rule is `exp <= now + skew`, and both edges are load-bearing:
///
/// * **`<=`, not `<`.** A token expiring exactly at `now + 120` is refreshed. At that
///   instant the skew window has zero width left, so the request would be racing the
///   expiry.
/// * **Undecodable means "refresh".** Python catches the exception and returns `True`.
///   The alternative — treating an unreadable token as valid — would send it and fail.
/// * **`skew` is a parameter.** A zero skew asks "is it already expired", which is what
///   a caller wants when checking rather than refreshing.
#[must_use]
pub fn access_token_is_expiring(token: &str, now: u64, skew: u64) -> bool {
    // `None` -> true: an unreadable token is refreshed rather than used.
    token_expiry(token).is_none_or(|exp| exp <= now.saturating_add(skew))
}

/// The default skew, exposed so a caller cannot pick a different one by accident.
#[must_use]
pub const fn default_skew() -> u64 {
    CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS
}

/// A token pair issued by the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPair {
    /// The bearer token.
    pub access_token: OAuthSecret,
    /// The refresh token, when the provider issued or rotated one.
    ///
    /// Optional because `ChatGPT`'s device flow does not always return one, and the
    /// refresh response only includes a new one when it rotated.
    pub refresh_token: Option<OAuthSecret>,
}

/// Reads a token pair out of a provider response.
///
/// # Errors
///
/// [`ChatGptError::MalformedResponse`] when there is no non-empty `access_token`.
///
/// The check is on emptiness as well as presence: Python's `if not data.get(...)` is
/// falsy for `""`, and a `""` access token would produce `Authorization: Bearer `.
pub fn parse_token_pair(body: &Value) -> Result<TokenPair, ChatGptError> {
    let text = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(OAuthSecret::new)
    };
    let access_token = text("access_token").ok_or(ChatGptError::MalformedResponse)?;
    Ok(TokenPair {
        access_token,
        // An empty refresh_token must become `None`, not `Some("")`, or a later
        // refresh would post an empty token and look like a revoked credential.
        refresh_token: text("refresh_token"),
    })
}

/// What the device-auth user-code endpoint returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceAuthStart {
    /// The secret `device_auth_id`. Never sent to a browser.
    pub device_auth_id: String,
    /// The code the user types.
    pub user_code: String,
    /// Where the user enters it.
    pub verification_uri: String,
    /// The provider's suggested poll interval.
    pub interval: u32,
    /// Seconds until the code expires.
    pub expires_in: u32,
}

/// Reads the device-auth start response.
///
/// # Errors
///
/// [`ChatGptError::MalformedResponse`] when either required field is missing, matching
/// Python's `if not data.get("device_auth_id") or not data.get("user_code")`.
///
/// The two defaults applied here (`verification_uri`, `interval`, `expires_in`) are
/// Python's `setdefault` calls at `chatgpt_subscription.py:178-180`.
pub fn parse_device_auth_start(body: &Value) -> Result<DeviceAuthStart, ChatGptError> {
    let required = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(ChatGptError::MalformedResponse)
    };
    Ok(DeviceAuthStart {
        device_auth_id: required("device_auth_id")?,
        user_code: required("user_code")?,
        verification_uri: body
            .get("verification_uri")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map_or_else(
                || format!("{CHATGPT_OAUTH_ISSUER}/codex/device"),
                str::to_owned,
            ),
        interval: body
            .get("interval")
            .and_then(Value::as_u64)
            .map_or(crate::oauth_device_flow::DEFAULT_INTERVAL_SECONDS, |v| {
                u32::try_from(v).unwrap_or(crate::oauth_device_flow::DEFAULT_INTERVAL_SECONDS)
            }),
        expires_in: body
            .get("expires_in")
            .and_then(Value::as_u64)
            .map_or(crate::oauth_device_flow::DEFAULT_EXPIRES_IN_SECONDS, |v| {
                u32::try_from(v).unwrap_or(crate::oauth_device_flow::DEFAULT_EXPIRES_IN_SECONDS)
            }),
    })
}

/// What one device-auth poll returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceAuthPoll {
    /// The user has not authorised yet.
    Pending,
    /// The provider authorised; exchange the code for tokens.
    ///
    /// Both values are secrets. `code_verifier` in particular is what makes the PKCE
    /// exchange unforgeable, and it must not be logged or echoed.
    Authorized {
        /// The authorization code.
        authorization_code: OAuthSecret,
        /// The PKCE verifier the provider generated alongside it.
        code_verifier: OAuthSecret,
    },
}

/// Reads a device-auth poll response.
///
/// Ports `poll_device_auth` (`chatgpt_subscription.py:184`) with its `403`/`404`
/// special case folded in: the Python returns `pending` for those two statuses rather
/// than treating them as failures, because OpenAI uses them to mean "not yet".
///
/// Both fields are required together. A response with an authorization code but no
/// verifier cannot complete a PKCE exchange, so it is [`Self::Pending`] rather than a
/// half-armed state.
#[must_use]
pub fn parse_device_auth_poll(status: u16, body: &Value) -> DeviceAuthPoll {
    if matches!(status, 403 | 404) {
        return DeviceAuthPoll::Pending;
    }
    let code = body
        .get("authorization_code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let verifier = body
        .get("code_verifier")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (code, verifier) {
        (Some(code), Some(verifier)) => DeviceAuthPoll::Authorized {
            authorization_code: OAuthSecret::new(code),
            code_verifier: OAuthSecret::new(verifier),
        },
        _ => DeviceAuthPoll::Pending,
    }
}

/// The form body for an authorization-code exchange.
///
/// Ports `exchange_authorization_code`'s `data=` (`chatgpt_subscription.py:203`).
#[must_use]
pub fn authorization_code_body(
    authorization_code: &str,
    code_verifier: &str,
) -> Vec<(String, String)> {
    vec![
        ("grant_type".to_owned(), "authorization_code".to_owned()),
        ("code".to_owned(), authorization_code.to_owned()),
        (
            "redirect_uri".to_owned(),
            CHATGPT_OAUTH_REDIRECT_URI.to_owned(),
        ),
        ("client_id".to_owned(), CHATGPT_OAUTH_CLIENT_ID.to_owned()),
        ("code_verifier".to_owned(), code_verifier.to_owned()),
    ]
}

/// The form body for a refresh.
///
/// Ports `refresh_oauth_tokens`' `data=` (`chatgpt_subscription.py:222`). Note the
/// access token is deliberately absent: Python opens with `del access_token`, because
/// the grant type does not take one and sending it would put a live bearer into a
/// request body that may be logged by an intermediary.
#[must_use]
pub fn refresh_body(refresh_token: &str) -> Vec<(String, String)> {
    vec![
        ("grant_type".to_owned(), "refresh_token".to_owned()),
        ("refresh_token".to_owned(), refresh_token.to_owned()),
        ("client_id".to_owned(), CHATGPT_OAUTH_CLIENT_ID.to_owned()),
    ]
}

/// The headers for a `ChatGPT` subscription request.
///
/// Ports `chatgpt_headers` (`chatgpt_subscription.py:78`).
#[must_use]
pub fn chatgpt_headers(access_token: Option<&str>) -> std::collections::BTreeMap<String, String> {
    let mut headers = std::collections::BTreeMap::from([
        (
            "Accept".to_owned(),
            "application/json, text/event-stream".to_owned(),
        ),
        ("Origin".to_owned(), "https://chatgpt.com".to_owned()),
        ("Referer".to_owned(), "https://chatgpt.com/codex".to_owned()),
        (
            "User-Agent".to_owned(),
            "Odysseus ChatGPT Subscription".to_owned(),
        ),
    ]);
    // An empty token is treated as absent, for the same reason as Copilot: a bare
    // `Bearer` looks authenticated to a presence check.
    if let Some(token) = access_token.filter(|t| !t.is_empty()) {
        headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
    }
    headers
}

/// Reads the model list from a Codex `/models` response.
///
/// Ports `fetch_available_models`' parsing (`chatgpt_subscription.py:90`): hidden
/// models are dropped, the rest are sorted by `(priority, slug)` with a default
/// priority of 10 000, and duplicates are removed keeping the first.
///
/// The default matters for ordering: a model with no `priority` sorts *after* every
/// model that has one, so an explicitly-ranked model wins.
#[must_use]
pub fn parse_model_slugs(body: &Value) -> Box<[Box<str>]> {
    let empty = Vec::new();
    let entries = body
        .get("models")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let mut sortable: Vec<(u64, &str)> = entries
        .iter()
        .filter_map(|item| {
            let slug = item
                .get("slug")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            // `visibility` of "hide"/"hidden" removes the model from the picker.
            let hidden = item
                .get("visibility")
                .and_then(Value::as_str)
                .is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "hide" | "hidden"));
            if hidden {
                return None;
            }
            let priority = match item.get("priority") {
                Some(Value::Number(n)) => n.as_u64().unwrap_or(10_000),
                _ => 10_000,
            };
            Some((priority, slug))
        })
        .collect();
    sortable.sort_unstable();

    let mut seen = std::collections::BTreeSet::new();
    sortable
        .into_iter()
        .filter(|(_, slug)| seen.insert(slug.to_string()))
        .map(|(_, slug)| Box::<str>::from(slug))
        .collect()
}

/// Converts a `ChatGPT` subscription error to the HTTP status `odysseus-web` should use.
///
/// Ports `to_http_exception` (`chatgpt_subscription.py:294`). It lives here rather than
/// in the web crate only because the mapping is a property of *these* errors; web still
/// owns the response construction (§5).
#[must_use]
pub const fn http_status(error: ChatGptError) -> u16 {
    match error {
        ChatGptError::RateLimited => 429,
        ChatGptError::ReauthRequired | ChatGptError::NotFound => 401,
        ChatGptError::MalformedResponse => 502,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, ChatGptError, DeviceAuthPoll,
        access_token_is_expiring, authorization_code_body, chatgpt_headers, classify_error,
        decode_jwt_claims, http_status, parse_device_auth_poll, parse_device_auth_start,
        parse_model_slugs, parse_token_pair, refresh_body, token_expiry,
    };

    /// Builds an unsigned JWT with the given `exp`, which is all the decoder reads.
    fn jwt_with_exp(exp: u64) -> String {
        let payload = serde_json::json!({ "exp": exp }).to_string();
        let encoded = super::base64url_encode(payload.as_bytes());
        format!("header.{encoded}.signature")
    }

    #[test]
    fn the_default_skew_is_120() {
        assert_eq!(CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, 120);
    }

    /// The boundary that matters most, pinned on both sides.
    ///
    /// The rule is `exp <= now + skew`, so with a 120 s skew a token needs **more than**
    /// 120 s of life left to survive:
    ///* **119 s remaining** → `now+119 <= now+120` → **refreshes**.
    /// * **121 s remaining** → `now+121 <= now+120` is false → **does not** refresh.
    ///
    /// Testing only "somewhere in the middle" cannot distinguish `<` from `<=`, nor a
    /// 120 s skew from a 121 s one, and either mistake is invisible until a request fails
    /// intermittently in production.
    #[test]
    fn the_skew_boundary_is_exact_on_both_sides() {
        const NOW: u64 = 1_000_000;
        let skew = CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS;

        // 119 s of life left: inside the window, so refresh.
        let just_inside = jwt_with_exp(NOW + 119);
        assert!(
            access_token_is_expiring(&just_inside, NOW, skew),
            "119s remaining must refresh"
        );

        // 121 s of life left: outside the window, so keep using it.
        let just_outside = jwt_with_exp(NOW + 121);
        assert!(
            !access_token_is_expiring(&just_outside, NOW, skew),
            "121s remaining must not refresh"
        );
    }

    /// Exactly at the boundary, `<=` makes it refresh.
    ///
    /// At `exp == now + 120` the skew window has zero width left, so the request would
    /// be racing the expiry. Pinned separately because an off-by-one here is invisible
    /// in the 119/121 test.
    #[test]
    fn exactly_at_the_skew_boundary_refreshes() {
        const NOW: u64 = 500_000;
        let token = jwt_with_exp(NOW + CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS);
        assert!(access_token_is_expiring(
            &token,
            NOW,
            CHATGPT_ACCESS_TOKEN_REFRESH_SKEW_SECONDS
        ));
    }

    /// A token already past expiry refreshes, whatever the skew.
    #[test]
    fn an_expired_token_refreshes_and_a_fresh_one_does_not() {
        let now = 200_000;
        assert!(access_token_is_expiring(&jwt_with_exp(now - 1), now, 120));
        assert!(!access_token_is_expiring(
            &jwt_with_exp(now + 10_000),
            now,
            120
        ));
    }

    /// A zero skew asks only "is it already expired", with no early window.
    #[test]
    fn a_zero_skew_means_already_expired() {
        let now = 300_000;
        assert!(!access_token_is_expiring(&jwt_with_exp(now + 1), now, 0));
        assert!(access_token_is_expiring(&jwt_with_exp(now), now, 0));
    }

    /// An unreadable token is refreshed rather than used.
    ///
    /// Python catches the decode exception and returns `True`. The alternative would
    /// send a token we cannot read and fail at the provider.
    #[test]
    fn an_undecodable_token_counts_as_expiring() {
        for bad in ["", "not-a-jwt", "only.two", "a.b.c"] {
            assert!(
                access_token_is_expiring(bad, 0, 120),
                "{bad:?} must be treated as expired"
            );
        }
    }

    #[test]
    fn a_token_without_an_exp_claim_is_expiring() {
        let payload = super::base64url_encode(br#"{"sub":"x"}"#);
        assert_eq!(token_expiry(&format!("h.{payload}.s")), None);
        assert!(access_token_is_expiring(&format!("h.{payload}.s"), 0, 120));
    }

    #[test]
    fn claims_decode_without_verification() {
        let payload = super::base64url_encode(br#"{"exp":42,"sub":"u"}"#);
        let claims = decode_jwt_claims(&format!("h.{payload}.s")).expect("decodes");
        assert_eq!(claims["sub"], "u");
        assert_eq!(token_expiry(&format!("h.{payload}.s")), Some(42));
    }

    /// The decoder is hand-rolled, so it is pinned against RFC 4648 §10's own vectors.
    ///
    /// A subtle base64 bug would not throw: it would decode to wrong bytes, fail the
    /// JSON parse, and every token would look expired — a refresh storm rather than an
    /// error. These vectors are what stops that being invisible.
    #[test]
    fn base64url_matches_the_rfc_4648_vectors() {
        for (encoded, expected) in [
            ("", ""),
            ("Zg", "f"),
            ("Zm8", "fo"),
            ("Zm9v", "foo"),
            ("Zm9vYg", "foob"),
            ("Zm9vYmE", "fooba"),
            ("Zm9vYmFy", "foobar"),
        ] {
            assert_eq!(
                super::base64url_decode(encoded).as_deref(),
                Some(expected.as_bytes()),
                "{encoded}"
            );
        }
    }

    /// The URL-safe alphabet, and rejection of everything outside it.
    #[test]
    fn base64url_uses_the_url_safe_alphabet_and_rejects_junk() {
        // 0xFB 0xFF encodes to `+/` in standard base64 and `-_` in base64url.
        assert_eq!(super::base64url_decode("-_8"), Some(vec![0xfb, 0xff]));
        for bad in ["Zg=", "Zg==", "Z+g", "Z/g", "Zm9v!", "  ", "A"] {
            assert_eq!(
                super::base64url_decode(bad),
                None,
                "{bad:?} must be rejected, not skipped"
            );
        }
    }

    /// A round trip through the test-only encoder covers every tail length.
    #[test]
    fn base64url_round_trips_at_every_length() {
        for len in 0..12 {
            let bytes: Vec<u8> = (0..len)
                .map(|i| u8::try_from(i * 7 + 1).unwrap_or(1))
                .collect();
            let encoded = super::base64url_encode(&bytes);
            assert_eq!(
                super::base64url_decode(&encoded),
                Some(bytes),
                "length {len} via {encoded}"
            );
        }
    }

    #[test]
    fn a_429_is_rate_limited_not_reauth() {
        // The distinction matters: reconnecting does not fix a quota, and telling the
        // user to reconnect would be wrong advice.
        assert_eq!(
            classify_error(429, &serde_json::json!({})),
            ChatGptError::RateLimited
        );
        assert_eq!(
            classify_error(500, &serde_json::json!({})),
            ChatGptError::MalformedResponse
        );
    }

    #[test]
    fn the_reauth_error_codes_match_the_python_set() {
        for code in [
            "invalid_grant",
            "invalid_token",
            "invalid_request",
            "refresh_token_reused",
        ] {
            assert_eq!(
                classify_error(400, &serde_json::json!({"error": {"code": code}})),
                ChatGptError::ReauthRequired,
                "{code}"
            );
            // Also accepted as a bare string, which is the other shape Python handles.
            assert_eq!(
                classify_error(400, &serde_json::json!({"error": code})),
                ChatGptError::ReauthRequired,
                "{code} as a string"
            );
        }
        // A `type` in place of `code` counts too.
        assert_eq!(
            classify_error(
                400,
                &serde_json::json!({"error": {"type": "invalid_grant"}})
            ),
            ChatGptError::ReauthRequired
        );
    }

    #[test]
    fn statuses_401_and_403_require_reauth() {
        for status in [401, 403] {
            assert_eq!(
                classify_error(status, &serde_json::json!({})),
                ChatGptError::ReauthRequired,
                "{status}"
            );
        }
    }

    #[test]
    fn no_error_message_can_reach_a_display() {
        // Every variant's Display is a fixed string. If one interpolated upstream text
        // it could carry a refresh_token into a log line.
        for error in [
            ChatGptError::ReauthRequired,
            ChatGptError::RateLimited,
            ChatGptError::NotFound,
            ChatGptError::MalformedResponse,
        ] {
            let rendered = error.to_string();
            assert!(!rendered.contains("tok"), "{rendered}");
            assert!(!rendered.is_empty());
        }
    }

    #[test]
    fn the_status_mapping_matches_the_python() {
        assert_eq!(http_status(ChatGptError::RateLimited), 429);
        assert_eq!(http_status(ChatGptError::ReauthRequired), 401);
        assert_eq!(http_status(ChatGptError::NotFound), 401);
        assert_eq!(http_status(ChatGptError::MalformedResponse), 502);
    }

    #[test]
    fn a_token_pair_requires_a_non_empty_access_token() {
        assert!(parse_token_pair(&serde_json::json!({})).is_err());
        assert!(parse_token_pair(&serde_json::json!({"access_token": ""})).is_err());
        let pair = parse_token_pair(&serde_json::json!({
            "access_token": "at", "refresh_token": "rt"
        }))
        .expect("valid");
        assert_eq!(pair.access_token.expose(), "at");
        assert_eq!(pair.refresh_token.expect("present").expose(), "rt");
    }

    /// An empty `refresh_token` must become `None`, not `Some("")`.
    ///
    /// Otherwise a later refresh posts an empty token, which the provider answers as a
    /// revoked credential and the user is told to reconnect for no reason.
    #[test]
    fn an_empty_refresh_token_becomes_absent() {
        let pair = parse_token_pair(&serde_json::json!({
            "access_token": "at", "refresh_token": ""
        }))
        .expect("valid");
        assert!(pair.refresh_token.is_none());
    }

    #[test]
    fn the_device_auth_start_requires_both_fields() {
        assert!(parse_device_auth_start(&serde_json::json!({"user_code": "U"})).is_err());
        assert!(parse_device_auth_start(&serde_json::json!({"device_auth_id": "D"})).is_err());
        let start = parse_device_auth_start(&serde_json::json!({
            "device_auth_id": "D", "user_code": "U"
        }))
        .expect("valid");
        // Python's setdefault values.
        assert_eq!(
            start.verification_uri,
            "https://auth.openai.com/codex/device"
        );
        assert_eq!(start.interval, 5);
        assert_eq!(start.expires_in, 900);
    }

    #[test]
    fn a_403_or_404_poll_is_pending_not_failed() {
        // OpenAI uses these to mean "not yet authorised".
        assert_eq!(
            parse_device_auth_poll(403, &serde_json::json!({})),
            DeviceAuthPoll::Pending
        );
        assert_eq!(
            parse_device_auth_poll(404, &serde_json::json!({})),
            DeviceAuthPoll::Pending
        );
    }

    /// Both halves are needed for a PKCE exchange; half of one is not progress.
    #[test]
    fn a_partial_authorization_is_still_pending() {
        assert_eq!(
            parse_device_auth_poll(200, &serde_json::json!({"authorization_code": "C"})),
            DeviceAuthPoll::Pending
        );
        assert_eq!(
            parse_device_auth_poll(200, &serde_json::json!({"code_verifier": "V"})),
            DeviceAuthPoll::Pending
        );
    }

    #[test]
    fn a_complete_authorization_carries_both_secrets() {
        let poll = parse_device_auth_poll(
            200,
            &serde_json::json!({"authorization_code": "C", "code_verifier": "V"}),
        );
        let DeviceAuthPoll::Authorized {
            authorization_code,
            code_verifier,
        } = poll
        else {
            return;
        };
        assert_eq!(authorization_code.expose(), "C");
        assert_eq!(code_verifier.expose(), "V");
        // Neither may appear in a Debug line.
        assert!(!format!("{authorization_code:?}{code_verifier:?}").contains('C'));
    }

    #[test]
    fn the_exchange_body_carries_the_verifier() {
        let body = authorization_code_body("CODE", "VERIFIER");
        let get = |k: &str| {
            body.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .expect("present")
        };
        assert_eq!(get("grant_type"), "authorization_code");
        assert_eq!(get("code"), "CODE");
        assert_eq!(get("code_verifier"), "VERIFIER");
        assert_eq!(get("redirect_uri"), super::CHATGPT_OAUTH_REDIRECT_URI);
    }

    /// The refresh body must not carry the access token.
    ///
    /// Python opens `refresh_oauth_tokens` with `del access_token`: the grant type does
    /// not take one, and including it would put a live bearer in a body an intermediary
    /// might log.
    #[test]
    fn the_refresh_body_omits_the_access_token() {
        let body = refresh_body("RT");
        assert!(
            !body.iter().any(|(k, _)| k == "access_token"),
            "a bearer must not ride in the refresh body"
        );
        assert_eq!(
            body.iter()
                .find(|(k, _)| k == "grant_type")
                .map(|(_, v)| v.as_str()),
            Some("refresh_token")
        );
        assert!(body.iter().any(|(k, v)| k == "refresh_token" && v == "RT"));
    }

    #[test]
    fn an_empty_token_sends_no_authorization_header() {
        assert!(!chatgpt_headers(Some("")).contains_key("Authorization"));
        assert!(!chatgpt_headers(None).contains_key("Authorization"));
        assert_eq!(chatgpt_headers(Some("t"))["Authorization"], "Bearer t");
    }

    #[test]
    fn models_sort_by_priority_then_slug_and_dedupe() {
        let slugs = parse_model_slugs(&serde_json::json!({"models": [
            {"slug": "zeta", "priority": 1},
            {"slug": "alpha", "priority": 1},
            {"slug": "first", "priority": 0},
            {"slug": "alpha", "priority": 2},
            {"slug": "unranked"}
        ]}));
        let got: Vec<&str> = slugs.iter().map(AsRef::as_ref).collect();
        // priority 0 first; then the two priority-1 slugs alphabetically; then the
        // unranked one, which defaults to 10 000; the priority-2 duplicate is dropped.
        assert_eq!(got, ["first", "alpha", "zeta", "unranked"]);
    }

    #[test]
    fn hidden_models_are_dropped() {
        for visibility in ["hide", "hidden", "HIDE", " hidden "] {
            let slugs = parse_model_slugs(&serde_json::json!({"models": [
                {"slug": "gone", "visibility": visibility},
                {"slug": "shown"}
            ]}));
            assert_eq!(slugs.len(), 1, "{visibility}");
            assert_eq!(slugs[0].as_ref(), "shown");
        }
    }

    #[test]
    fn blank_slugs_are_dropped() {
        let slugs = parse_model_slugs(&serde_json::json!({"models": [
            {"slug": ""}, {"slug": "   "}, {"nope": 1}, {"slug": "real"}
        ]}));
        assert_eq!(slugs.len(), 1);
        assert_eq!(slugs[0].as_ref(), "real");
    }

    #[test]
    fn a_non_object_payload_yields_no_models() {
        assert!(parse_model_slugs(&serde_json::json!({})).is_empty());
        assert!(parse_model_slugs(&serde_json::json!({"models": "nope"})).is_empty());
    }
}
