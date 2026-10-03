//! API tokens for external integrations (n8n, Make, and anything else holding a
//! bearer credential).
//!
//! Ports `routes/api_token_routes.py`, `core/database.py:634-646` (the
//! `ApiToken` model) and the bearer path of `app.py:295-345, 415-470`.
//!
//! | Concern | Python | Here |
//! |---|---|---|
//! | Storage | `api_tokens` table | same table, from `0001_baseline.sql` — no new migration |
//! | Raw token | `"ody_" + secrets.token_urlsafe(32)` | same, 47 chars total |
//! | Hash | `bcrypt.hashpw(raw_token, gensalt())` | cost 12, over the **whole** `ody_…` string |
//! | Prefix | `raw_token[:8]` — 24 bits | same, so `Vec<Candidate>` is load-bearing |
//! | Lookup | `dict[prefix] -> [(id, hash, owner, scopes)]` | `RwLock<HashMap<String, Box<[Candidate]>>>` |
//!
//! # Invariants that must survive the port
//!
//! * **The bcrypt input is the full `ody_…` string**, prefix included
//!   (`api_token_routes.py:130-131` hashes `raw_token`; `app.py:434` verifies
//!   `raw_token`). Hashing only the 43-char suffix would silently invalidate
//!   every token the Python install minted — no error, just 401s.
//! * **A token whose owner no longer exists is never served.** `app.py:323-333`
//!   resolves the owner through `normalize_known_username` and `continue`s when
//!   it comes back falsy. `auth.json` is read once per refresh, never per row.
//! * **`is_active` must be exactly `1`.** `app.py:321` filters the column with a
//!   SQLAlchemy boolean comparison, which SQLite receives as `is_active = 1`. The
//!   column is nullable with no `NOT NULL`, so a `NULL` row fails that predicate in
//!   Python and must fail here too. Decoding `NULL` as "active" would serve revoked
//!   tokens.
//! * **`touch_last_used` must not invalidate the cache.** `app.py:443-457`
//!   writes only `last_used_at` and never calls `_invalidate_cache`. If it did,
//!   every authenticated bearer request would trigger a full table scan plus an
//!   `auth.json` read.
//! * **All SQL is parameterised**, and the owner predicate stays a bare equality
//!   so `ix_api_tokens_owner` is used.
//!
//! # Known divergences from Python, and why
//!
//! * **Creation refuses an empty owner.** With `AUTH_ENABLED=false`, Python's
//!   `get_current_user` returns `None`, so `POST /api/tokens` stores `owner=NULL`
//!   — and `app.py:330` then drops that row on every refresh, so the token the
//!   admin was just handed can never authenticate. We refuse at creation
//!   instead, which fails closed and is diagnosable.
//! * **A generation counter replaces the dirty flag.** See
//!   [`PrefixCache::invalidate`]. Python's `_token_cache_dirty = False`
//!   (`app.py:337`) can discard a token created during a rebuild.
//! * **A corrupt stored hash is skipped, not fatal.** `bcrypt.checkpw` raises on
//!   a malformed hash, which in Python escapes the candidate loop and 401s — so
//!   one bad row makes every *other* token sharing that prefix unreachable. We
//!   log and continue; both outcomes deny the presented token, so this is
//!   availability-only.
//! * **No `qr` feature.** `routes/auth_routes.py:258-263` renders the QR PNG
//!   server-side, but image generation is presentation and belongs in the web
//!   layer (AGENTS.md §8).

use std::{
    collections::{BTreeSet, HashMap},
    fmt::Write as _,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use {
    crate::{Store, StoreError, blocking::spawn_blocking},
    rusqlite::{OptionalExtension, params},
    thiserror::Error,
};

/// Prefix every raw token starts with, from `api_token_routes.py:130`.
pub const TOKEN_PREFIX: &str = "ody_";

/// Number of random bytes behind a token, from `secrets.token_urlsafe(32)`.
const TOKEN_ENTROPY_BYTES: usize = 32;

/// `secrets.token_urlsafe(32)` yields exactly this many characters: 32 bytes is
/// `32 mod 3 == 2`, so base64 emits 43 unpadded characters. Asserted in a test, and
/// the mint path relies on it rather than truncating.
const TOKEN_BODY_CHARS: usize = 43;

/// Characters of the raw token stored in `token_prefix` and shown in the UI.
const PREFIX_CHARS: usize = 8;

/// Hex characters of a version-4 UUID kept as the row id.
const ID_CHARS: usize = 8;

/// `MAX_NAME_LEN` from `api_token_routes.py:12`.
pub const MAX_NAME_LEN: usize = 100;

/// Bounds `app.py:421` applies before touching the database at all.
const MIN_TOKEN_LEN: usize = 12;
const MAX_TOKEN_LEN: usize = 100;

/// The scope granted when none is requested, from `DEFAULT_SCOPES`.
const DEFAULT_SCOPE: &str = "chat";

/// How the `scopes` column is joined, matching `",".join(scope_list)`.
const SCOPE_SEPARATOR: char = ',';

/// Failures from the API token store.
#[derive(Debug, Error)]
pub enum TokenError {
    /// The database call failed.
    #[error(transparent)]
    Store(#[from] StoreError),

    /// A raw SQLite failure, wrapped so `?` works on a `rusqlite::Result`.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// A scope name is not one of the 14 the server recognises.
    #[error("unknown token scope: {0}")]
    UnknownScope(String),

    /// A profile name is not one of the four.
    #[error("unknown token profile: {0}")]
    UnknownProfile(String),

    /// The token name was empty after trimming.
    #[error("token name is required")]
    NameRequired,

    /// No such token row.
    #[error("no such api token: {0}")]
    NoSuchToken(String),

    /// bcrypt refused the value.
    ///
    /// Almost always a corrupt stored hash. Never carries the hash itself.
    #[error("api token hashing failed: {0}")]
    Hash(String),

    /// The owner could not be resolved to a live account.
    ///
    /// Raised at creation time rather than letting a token be minted that can
    /// never authenticate. See the module docs.
    #[error("api token owner {0:?} does not resolve to a live account")]
    UnknownOwner(String),

    /// The blocking task was cancelled or panicked.
    #[error("api token task did not complete")]
    Interrupted,
}

// ---------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------

/// One of the 14 scopes `ALLOWED_SCOPES` defines.
///
/// A newtype rather than an enum on purpose: the read path must tolerate a scope
/// this build has never heard of (a newer Python build's token), and an enum
/// cannot represent one. Validity is enforced at the *write* boundary by
/// [`Scope::parse`]; [`ApiTokenStore::has_scope`] is plain membership, so a
/// legacy token keeps authenticating and an unknown scope simply grants nothing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Scope(Box<str>);

impl Scope {
    /// All 14 scopes, in `sorted(ALLOWED_SCOPES)` order so the web layer does
    /// not have to sort them again.
    pub const ALL: [&'static str; 14] = [
        "calendar:read",
        "calendar:write",
        "chat",
        "cookbook:launch",
        "cookbook:read",
        "documents:read",
        "documents:write",
        "email:draft",
        "email:read",
        "email:send",
        "memory:read",
        "memory:write",
        "todos:read",
        "todos:write",
    ];

    /// A write scope and the read scope it implies.
    ///
    /// `ensure_before` in `api_token_routes.py:60-71`; the pairs are listed in
    /// the order Python applies them, which matters because each call recomputes
    /// its insertion index and so shifts the ones after it.
    const IMPLYING_PAIRS: [(&'static str, &'static str); 6] = [
        ("todos:write", "todos:read"),
        ("documents:write", "documents:read"),
        ("calendar:write", "calendar:read"),
        ("memory:write", "memory:read"),
        ("email:draft", "email:read"),
        ("cookbook:launch", "cookbook:read"),
    ];

    /// Validates a scope name.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::UnknownScope`] if it is not one of [`Scope::ALL`].
    pub fn parse(name: &str) -> Result<Self, TokenError> {
        if Self::ALL.contains(&name) {
            Ok(Self(name.into()))
        } else {
            Err(TokenError::UnknownScope(name.to_owned()))
        }
    }

    /// The scope name as stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One of the four canned scope bundles from `TOKEN_PROFILES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenProfile {
    /// Chat only.
    Chat,
    /// Codex todos: read plus write.
    CodexTodos,
    /// Codex documents: read plus write.
    CodexDocuments,
    /// Codex email drafts: read email, draft email, read and write documents.
    CodexEmailDrafts,
}

impl TokenProfile {
    /// Every profile, for a UI listing.
    pub const ALL: [TokenProfile; 4] = [
        TokenProfile::Chat,
        TokenProfile::CodexTodos,
        TokenProfile::CodexDocuments,
        TokenProfile::CodexEmailDrafts,
    ];

    /// The profile's name as accepted on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TokenProfile::Chat => "chat",
            TokenProfile::CodexTodos => "codex_todos",
            TokenProfile::CodexDocuments => "codex_documents",
            TokenProfile::CodexEmailDrafts => "codex_email_drafts",
        }
    }

    /// Parses a profile name.
    ///
    /// Case-sensitive, matching `api_token_routes.py:41` (`(profile or
    /// "").strip()`, no folding). `"Chat"` is a 400 in Python and is one here.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::UnknownProfile`] for anything unrecognised.
    pub fn parse(name: &str) -> Result<Self, TokenError> {
        match name.trim() {
            "chat" => Ok(TokenProfile::Chat),
            "codex_todos" => Ok(TokenProfile::CodexTodos),
            "codex_documents" => Ok(TokenProfile::CodexDocuments),
            "codex_email_drafts" => Ok(TokenProfile::CodexEmailDrafts),
            other => Err(TokenError::UnknownProfile(other.to_owned())),
        }
    }

    /// The scopes this profile expands to, in declaration order.
    #[must_use]
    pub fn scopes(self) -> Vec<Scope> {
        let names: &[&str] = match self {
            TokenProfile::Chat => &["chat"],
            TokenProfile::CodexTodos => &["todos:read", "todos:write"],
            TokenProfile::CodexDocuments => &["documents:read", "documents:write"],
            TokenProfile::CodexEmailDrafts => &[
                "email:read",
                "email:draft",
                "documents:read",
                "documents:write",
            ],
        };
        names.iter().filter_map(|n| Scope::parse(n).ok()).collect()
    }
}

/// Raw scope input, matching the two shapes `_normalize_scopes` accepts.
///
/// Owns its data so it can cross into `spawn_blocking` without a lifetime.
/// Normalisation is pure CPU work, so borrowing would buy nothing but friction.
#[derive(Debug, Clone, Default)]
pub enum ScopeRequest {
    /// Nothing supplied: yields `["chat"]`.
    #[default]
    Unset,
    /// A comma-separated string. Spaces are separators too, not just padding —
    /// `api_token_routes.py:49` does `scopes.replace(" ", ",").split(",")`, so
    /// `"chat memory"` is two scopes. Splitting only on `,` would yield one
    /// unknown scope and reject.
    Text(String),
    /// An explicit list, which may be empty. An empty list reaches the trailing
    /// `normalized or [DEFAULT_SCOPES]` at line 73, so it also yields `["chat"]`.
    List(Vec<String>),
}

impl ScopeRequest {
    /// Builds a [`ScopeRequest::Text`] from anything string-like.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }
}

/// Normalises a scope request, mirroring `_normalize_scopes`.
///
/// A non-empty `profile` **replaces** the scope list outright; it is not merged
/// (`api_token_routes.py:42-51`).
///
/// # Errors
///
/// Returns [`TokenError::UnknownScope`] or [`TokenError::UnknownProfile`] for
/// anything unrecognised.
pub fn normalise_scopes(
    scopes: &ScopeRequest,
    profile: Option<&str>,
) -> Result<Vec<Scope>, TokenError> {
    let requested = match profile.map(str::trim).filter(|p| !p.is_empty()) {
        Some(name) => TokenProfile::parse(name)?.scopes(),
        None => requested_from(scopes)?,
    };

    let mut normalised: Vec<Scope> = Vec::with_capacity(requested.len());
    for scope in requested {
        // Already validated by `TokenProfile::scopes`, or by `Scope::parse` below
        // for the caller-supplied path.
        let scope = Scope::parse(scope.as_str())?;
        if !normalised.contains(&scope) {
            normalised.push(scope);
        }
    }

    // `ensure_before(write, read)`: if the write is held and the read is not,
    // splice the read in immediately before the write. The index is recomputed
    // per pair, so an earlier insertion shifts the later ones — which is why the
    // pair order is fixed and must not be sorted.
    for (write, read) in Scope::IMPLYING_PAIRS {
        let (Ok(write), Ok(read)) = (Scope::parse(write), Scope::parse(read)) else {
            continue;
        };
        let Some(write_at) = normalised.iter().position(|s| *s == write) else {
            continue;
        };
        if normalised.contains(&read) {
            continue;
        }
        normalised.insert(write_at, read);
    }

    if normalised.is_empty() {
        return Ok(vec![
            Scope::parse(DEFAULT_SCOPE).unwrap_or_else(|_| Scope(DEFAULT_SCOPE.into())),
        ]);
    }
    Ok(normalised)
}

/// Expands the non-profile branch of `_normalize_scopes`.
fn requested_from(request: &ScopeRequest) -> Result<Vec<Scope>, TokenError> {
    match request {
        ScopeRequest::Unset => Ok(vec![
            Scope::parse(DEFAULT_SCOPE).unwrap_or_else(|_| Scope(DEFAULT_SCOPE.into())),
        ]),
        ScopeRequest::Text(text) if text.is_empty() => Ok(vec![
            Scope::parse(DEFAULT_SCOPE).unwrap_or_else(|_| Scope(DEFAULT_SCOPE.into())),
        ]),
        ScopeRequest::Text(text) => {
            // Spaces become commas first; a scope name can never contain one, so
            // this cannot split a legitimate scope in half.
            let normalised = text.replace(' ', ",");
            normalised
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(Scope::parse)
                .collect()
        }
        ScopeRequest::List(list) => list
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(Scope::parse)
            .collect(),
    }
}

/// Joins scopes for the `scopes` column, matching `",".join(scope_list)`.
#[must_use]
pub fn join_scopes(scopes: &[Scope]) -> String {
    let mut out = String::new();
    for (i, scope) in scopes.iter().enumerate() {
        if i > 0 {
            out.push(SCOPE_SEPARATOR);
        }
        let _ = out.write_str(scope.as_str());
    }
    out
}

/// Splits a stored `scopes` value.
///
/// Tolerant by design, matching `app.py:331` and `api_token_routes.py:90`, which
/// both use `(scopes or DEFAULT_SCOPES)` with **no** validation against
/// `ALLOWED_SCOPES`. A row with `scopes=""` reads as `["chat"]`, and a scope this
/// build has never heard of survives and simply grants nothing.
#[must_use]
pub fn split_scopes(stored: Option<&str>) -> Vec<Scope> {
    let text = stored
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(DEFAULT_SCOPE);
    let parsed: Vec<Scope> = text
        .split(SCOPE_SEPARATOR)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Scope(s.into()))
        .collect();
    if parsed.is_empty() {
        vec![Scope(DEFAULT_SCOPE.into())]
    } else {
        parsed
    }
}

// ---------------------------------------------------------------------------
// The raw token
// ---------------------------------------------------------------------------

/// A freshly minted bearer token, returned exactly once and never stored.
///
/// A distinct type so it cannot be accidentally written to the database, logged,
/// or put in an error message: `Display` and `Debug` are both withheld, and the
/// only way out is [`RawApiToken::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct RawApiToken(Box<str>);

impl RawApiToken {
    /// The token's characters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the token is empty. Always false for a minted token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The stored `token_prefix` — the first 8 characters.
    ///
    /// This is `ody_` plus 4 base64 characters: **24 bits**. By the birthday
    /// bound roughly 4096 live tokens give a ≥50% chance of one prefix collision,
    /// which is exactly why a prefix maps to a *list* of candidates.
    ///
    /// Slices rather than using `.chars()`, which is only safe because
    /// [`RawApiToken::from_client`] rejects any token containing a non-ASCII byte
    /// and every token minted here is ASCII. Both invariants are asserted in tests;
    /// a client token that reached this method unvalidated would panic on a
    /// multi-byte boundary, and `panic = "abort"` in the release profile would take
    /// the whole process down on an unauthenticated request.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.0[..PREFIX_CHARS]
    }

    /// The full token, for handing to the bcrypt hasher and to the caller.
    ///
    /// Named `expose` rather than `as_str` so that a grep for token access shows
    /// every site deliberately.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Wraps a token that arrived from a client.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::Hash`] unless the string is a plausible token:
    /// outside `app.py:421`'s `12 <= len <= 100` bound, not starting with `ody_`,
    /// or containing a non-ASCII byte.
    ///
    /// The last check is not cosmetic. `prefix()` slices at byte 8, and a token
    /// like `ody_ああab` is 12 *bytes* — long enough to pass a length-only bound —
    /// with byte 8 in the middle of a 3-byte character. Slicing there panics, and
    /// `Cargo.toml` sets `panic = "abort"` for release, so an unauthenticated
    /// `Authorization` header would abort the server. Python is immune: it slices
    /// by character, yielding a prefix that simply matches nothing.
    ///
    /// Counting ASCII bytes also settles the bytes-vs-characters question, since for
    /// an all-ASCII string the two agree.
    pub fn from_client(raw: &str) -> Result<Self, TokenError> {
        if !(MIN_TOKEN_LEN..=MAX_TOKEN_LEN).contains(&raw.len()) {
            return Err(TokenError::Hash("implausible token length".into()));
        }
        if !raw.is_ascii() {
            return Err(TokenError::Hash("token contains non-ascii bytes".into()));
        }
        if !raw.starts_with(TOKEN_PREFIX) {
            return Err(TokenError::Hash("token has the wrong prefix".into()));
        }
        Ok(Self(raw.into()))
    }
}

impl std::fmt::Debug for RawApiToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The prefix is safe to show — it is in the DB and the UI already lists
        // it — but the rest is the credential.
        write!(f, "RawApiToken({}…)", self.prefix())
    }
}

/// Mints a raw token: `"ody_"` plus 43 base64url characters.
///
/// `secrets.token_urlsafe(32)` is base64url of 32 random bytes with `=` padding
/// stripped. 32 mod 3 == 2, so the encoding is always exactly 43 characters.
///
/// # Errors
///
/// Returns [`TokenError::Hash`] if the OS entropy source fails. Failing closed
/// matters more here than anywhere else in the store: a guessable token is a
/// guessable credential.
pub fn mint_raw_token() -> Result<RawApiToken, TokenError> {
    let mut bytes = [0u8; TOKEN_ENTROPY_BYTES];
    getrandom::fill(&mut bytes)
        .map_err(|e| TokenError::Hash(format!("no entropy for a token: {e}")))?;
    let body = base64url_nopad(&bytes);
    debug_assert_eq!(
        body.len(),
        TOKEN_BODY_CHARS,
        "base64 of 32 bytes is 43 chars"
    );
    Ok(RawApiToken(format!("{TOKEN_PREFIX}{body}").into()))
}

/// base64url without padding, over the standard alphabet `A-Za-z0-9-_`.
///
/// Hand-rolled because the store crate has no `base64` dependency and
/// `fernet` does not re-export one (AGENTS.md §12.3: check the stdlib and what is
/// already there before adding a crate).
fn base64url_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;

        // 3 input bytes -> 4 sextets; 2 -> 3; 1 -> 2. Integer division alone gets
        // the 2-byte case wrong (2*4/3 == 2), which would silently mint a 42-char
        // token, so round up: ceil(len*8/6).
        let full = (chunk.len() * 8).div_ceil(6);
        for i in 0..full {
            let shift = 18 - i * 6;
            out.push(ALPHABET[((triple >> shift) & 0x3f) as usize] as char);
        }
    }
    out
}

/// A row id: the first 8 hex characters of a version-4 UUID.
///
/// Python keeps the first 8 characters of a version-4 UUID, i.e. the first 4 bytes
/// fully random. The version nibble (byte 6) and variant bits (byte 8) are set so
/// the value really is a version-4 UUID prefix; neither touches the characters used.
fn mint_id() -> Result<String, TokenError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|e| TokenError::Hash(format!("no entropy for a token id: {e}")))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let mut hex = String::with_capacity(ID_CHARS);
    for byte in &bytes[..ID_CHARS / 2] {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

// ---------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------

/// Wall-clock source, injected so tests are deterministic (AGENTS.md §11).
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// Seconds since the Unix epoch.
    fn now_secs(&self) -> i64;
}

/// Reads the system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs().cast_signed())
    }
}

/// Formats a timestamp the way SQLAlchemy binds a naive-UTC `DateTime` on
/// SQLite: `"YYYY-MM-DD HH:MM:SS.ffffff"` from `utcnow_naive()`
/// (`core/database.py:23-25`).
///
/// Hand-rolled from the civil calendar because the store crate has no date-time
/// dependency. Howard Hinnant's `civil_from_days`.
#[must_use]
pub fn format_timestamp(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // days since 1970-01-01 -> civil date
    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let year_of_era = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = doe - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.000000")
}

/// Parses a SQLAlchemy naive-UTC timestamp back to Unix seconds.
///
/// Returns `None` for anything unparseable **or out of range**; callers treat that
/// as "no timestamp" rather than failing, because a `NULL` column is legitimate.
///
/// Range validation is not pedantry. `"2025-13-45 99:99:99"` parses as six separate
/// integers, and without bounds checks `days_from_civil` would silently roll the
/// overflow into the next day while this still returned `Some`. An absurd year is
/// worse: `era * 146_097` overflows `i64`, which is a debug panic and a silent wrap
/// in release. Both need a hand-edited database to reach, but this is a
/// `Result`-shaped hole in code that otherwise looks total.
#[must_use]
pub fn parse_timestamp(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts
        .next()
        .unwrap_or("0")
        .split('.')
        .next()?
        .parse()
        .ok()?;

    // The year range keeps `era * 146_097` well inside i64; the rest rejects the
    // "13th month, 99th hour" shape that would otherwise be normalised away into a
    // plausible-looking wrong instant.
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Howard Hinnant's `days_from_civil`, the inverse of the branch in
/// [`format_timestamp`].
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_prime = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// One token row, as listed in the UI.
///
/// Carries no hash: the listing must never be able to leak one even by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiTokenRecord {
    /// Row id.
    pub id: String,
    /// Owning account.
    pub owner: String,
    /// Human label.
    pub name: String,
    /// First 8 characters, for display.
    pub token_prefix: String,
    /// Effective scopes.
    pub scopes: Box<[Scope]>,
    /// Whether the token may authenticate.
    pub is_active: bool,
    /// When it last authenticated, if ever.
    pub last_used_at: Option<i64>,
    /// Creation time, Unix seconds.
    pub created_at: i64,
}

/// A freshly created token: the row plus the one-time raw secret.
#[derive(Debug)]
pub struct CreatedApiToken {
    /// The stored row.
    pub record: ApiTokenRecord,
    /// The raw token. Shown to the operator once and never retrievable again.
    pub raw: RawApiToken,
}

/// A cache candidate: one row that shares a prefix.
///
/// `Box<[Scope]>` rather than `Vec<Scope>` per AGENTS.md §12.1 rule 1: these are
/// built once and then read for the life of the cache entry, so a `Vec`'s
/// capacity field and slack tail are pure overhead.
#[derive(Clone)]
pub struct Candidate {
    /// Row id, for `last_used_at`.
    pub id: String,
    /// bcrypt hash of the full raw token.
    token_hash: String,
    /// Resolved owner.
    pub owner: String,
    /// Effective scopes.
    pub scopes: Box<[Scope]>,
}

impl Candidate {
    /// Verifies a presented token against this candidate's hash.
    ///
    /// bcrypt is the constant-time step, so there is no separate constant-time
    /// comparison to perform on the hash itself.
    ///
    /// # Errors
    ///
    /// Returns [`TokenError::Hash`] when the stored hash is malformed. Callers on
    /// the verification path treat that as "this candidate does not match" and
    /// continue to the next; see the module docs.
    pub fn verify(&self, presented: &RawApiToken) -> Result<bool, TokenError> {
        bcrypt::verify(presented.expose(), &self.token_hash)
            .map_err(|e| TokenError::Hash(e.to_string()))
    }
}

impl std::fmt::Debug for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Candidate")
            .field("id", &self.id)
            .field("token_hash", &"<redacted>")
            .field("owner", &self.owner)
            .field("scopes", &self.scopes)
            .finish()
    }
}

/// The verified result of presenting a bearer token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedToken {
    /// Row id.
    pub id: String,
    /// The owning account. **Never** the `"api"` pseudo-user — see the module
    /// docs on ownership.
    pub owner: String,
    /// Effective scopes.
    pub scopes: Box<[Scope]>,
}

// ---------------------------------------------------------------------------
// The prefix cache
// ---------------------------------------------------------------------------

/// `RwLock<HashMap<String, Box<[Candidate]>>>` plus the invalidation machinery.
///
/// # Why a generation counter and not a dirty flag
///
/// `app.py:307` uses `_token_cache_dirty: bool`, cleared unconditionally at
/// `app.py:337` after a rebuild. A token created *during* that rebuild is
/// committed to the database and then wiped by the stale swap — and the flag is
/// now `false`, so that token never authenticates until something else
/// invalidates. A boolean cannot detect a mid-build mutation.
///
/// [`PrefixCache::current_generation`] is bumped by every mutation instead. A
/// refresh records the generation it built for; a later reader that sees
/// [`PrefixCache::is_stale`] rebuilds again. This is a deliberate divergence from
/// `app.py`.
#[derive(Debug)]
pub struct PrefixCache {
    map: RwLock<HashMap<String, Arc<[Candidate]>>>,
    /// Generation the map in `map` was built for.
    built_at: AtomicU64,
    /// Generation of the newest mutation.
    current: AtomicU64,
}

impl Default for PrefixCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefixCache {
    /// An empty cache, already stale.
    #[must_use]
    pub fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
            // 0 vs 1 so the first read always builds.
            built_at: AtomicU64::new(0),
            current: AtomicU64::new(1),
        }
    }

    /// Marks the cache stale. Call after any mutation to `api_tokens`.
    ///
    /// `SeqCst` rather than `Relaxed`: this pairs with the `RwLock` write in
    /// [`PrefixCache::swap`], and the ordering that matters is "bump before the
    /// mutating DB commit is observable".
    pub fn invalidate(&self) {
        self.current.fetch_add(1, Ordering::SeqCst);
    }

    /// Whether the map needs rebuilding.
    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.built_at.load(Ordering::SeqCst) != self.current.load(Ordering::SeqCst)
    }

    /// Installs a freshly built map, recording the generation it covers.
    pub fn swap(&self, built: HashMap<String, Arc<[Candidate]>>, for_generation: u64) {
        // Refuse to install a map that a mutation has already made obsolete.
        //
        // Without this check there is a bounded fail-open window. Two rebuilds can
        // overlap: R1 reads generation G and queries, a mutation commits and bumps
        // to G+1, R2 queries the newer state and installs it, and then R1 installs
        // its *older* snapshot over the top -- after which R1's own
        // `candidates()` lookup can still authenticate a token the mutation
        // revoked. Skipping the stale install leaves the cache stale, and the next
        // request rebuilds again, which is the safe direction.
        if for_generation != self.current.load(Ordering::SeqCst) {
            tracing::debug!(
                built_for = for_generation,
                current = self.current.load(Ordering::SeqCst),
                "discarding an obsolete api token cache rebuild"
            );
            return;
        }

        // Build fully, then take the write lock once: a reader holding the read
        // lock can never observe a half-built map. This is what
        // `tests/test_token_cache_atomic_swap.py` checks.
        *self
            .map
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = built;
        self.built_at.store(for_generation, Ordering::SeqCst);
    }

    /// Candidates for a prefix, as a shared slice. `None` when absent.
    ///
    /// Returns the `Arc` rather than a `Vec<Candidate>` so the per-request cost is a
    /// refcount bump instead of N string allocations. Python does
    /// `list(_token_cache.get(prefix, ()))` -- a shallow copy -- but it holds
    /// *references* to the rows rather than copies of the hashes, so cloning the
    /// strings here would be strictly worse than the code being ported, and this is
    /// the authentication hot path (AGENTS.md §12, §12.1 rule 6).
    ///
    /// Takes a read lock only; the guard is dropped before returning, so a caller
    /// cannot hold it across an `.await`.
    #[must_use]
    pub fn candidates(&self, prefix: &str) -> Option<Arc<[Candidate]>> {
        let map = self
            .map
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.get(prefix).cloned()
    }

    /// How many prefixes are cached.
    #[must_use]
    pub fn prefix_count(&self) -> usize {
        self.map
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

// ---------------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------------

/// Resolves an owner string to a live account, or `None` if there is none.
///
/// Mirrors `core/auth.py:66-69`'s `normalize_known_username`. Injected rather
/// than called directly so this module does not reach into `auth`'s internals,
/// and so a refresh can resolve the whole user set **once** — `app.py:323` does
/// it per row, which is one `auth.json` read and parse per token plus a torn
/// snapshot if a user is created mid-loop.
///
/// Implementations may do IO; callers run them on the blocking pool.
pub trait OwnerResolver: Send + Sync + std::fmt::Debug {
    /// Every lowercased username that currently exists.
    ///
    /// Returning `Err` must leave the cache **empty**, not fall back to an empty
    /// user set: Python's uncaught exception propagates to `app.py:465-468` and
    /// 401s, and a set that happens to match a `NULL` owner would do the
    /// opposite.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backing store cannot be read. An
    /// implementation must NOT fall back to an empty set on failure: see the trait
    /// docs.
    fn known_owners(&self) -> Result<BTreeSet<String>, StoreError>;
}
/// Creation parameters.
#[derive(Debug)]
/// Creation parameters. Owned so the whole of `create` -- which must run on the
/// blocking pool because of bcrypt and the INSERT -- needs no lifetime.
pub struct NewToken {
    /// Human label. Stripped, then truncated to [`MAX_NAME_LEN`] by code point.
    pub name: String,
    /// The owning account. Must resolve to a live user.
    pub owner: String,
    /// Scopes, or [`ScopeRequest::Unset`] for `["chat"]`.
    pub scopes: ScopeRequest,
    /// A profile, which replaces `scopes` when non-empty.
    pub profile: Option<String>,
}

/// API token persistence and bearer verification.
#[derive(Debug, Clone)]
pub struct ApiTokenStore {
    store: Store,
    cache: Arc<PrefixCache>,
    clock: Arc<dyn Clock>,
}

impl ApiTokenStore {
    /// Builds a store over `store`, reading the system clock.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self {
            store,
            cache: Arc::new(PrefixCache::new()),
            clock: Arc::new(SystemClock),
        }
    }

    /// Builds a store with an injected clock, for deterministic tests.
    #[must_use]
    pub fn with_clock(store: Store, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            cache: Arc::new(PrefixCache::new()),
            clock,
        }
    }

    /// The shared prefix cache, for a middleware that wants to check staleness.
    #[must_use]
    pub fn cache(&self) -> &Arc<PrefixCache> {
        &self.cache
    }

    // -- synchronous core (blocking pool only) ------------------------------

    /// Creates a token. **Blocking.** See [`ApiTokenStore::create`].
    ///
    /// # Errors
    ///
    /// See [`ApiTokenStore::create`].
    pub fn create_blocking(
        &self,
        new: NewToken,
        resolver: &dyn OwnerResolver,
    ) -> Result<CreatedApiToken, TokenError> {
        let NewToken {
            name: raw_name,
            owner: raw_owner,
            scopes: request,
            profile,
        } = new;
        let name = normalise_name(&raw_name)?;
        let scopes = normalise_scopes(&request, profile.as_deref())?;
        // Fail closed rather than mint a token that can never authenticate: with
        // auth disabled Python stores `owner=NULL` and then drops the row on every
        // refresh, handing the operator a dead credential.
        let owner = raw_owner.trim().to_lowercase();
        if !resolver.known_owners()?.contains(&owner) {
            return Err(TokenError::UnknownOwner(owner.clone()));
        }

        let raw = mint_raw_token()?;
        let id = mint_id()?;

        // bcrypt runs here rather than in SQL, and it is cost 12: ~250 ms of CPU.
        // This whole function is only ever reached through `spawn_blocking`.
        let hash = bcrypt::hash(raw.expose(), bcrypt::DEFAULT_COST)
            .map_err(|e| TokenError::Hash(e.to_string()))?;

        let now = self.clock.now_secs();
        let stamp = format_timestamp(now);

        let conn = self.store.connect()?;
        conn.execute(
            "INSERT INTO api_tokens
                 (id, owner, name, token_hash, token_prefix, scopes, is_active,
                  last_used_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, NULL, ?7, ?7)",
            params![
                id,
                owner,
                name,
                hash,
                raw.prefix(),
                join_scopes(&scopes),
                stamp,
            ],
        )?;
        drop(conn);

        self.cache.invalidate();

        Ok(CreatedApiToken {
            record: ApiTokenRecord {
                id,
                owner,
                name,
                token_prefix: raw.prefix().to_owned(),
                scopes: scopes.into_boxed_slice(),
                is_active: true,
                last_used_at: None,
                created_at: now,
            },
            raw,
        })
    }

    /// Creates a token, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError`] from validation, owner resolution, entropy, bcrypt
    /// and the database.
    pub async fn create(
        &self,
        new: NewToken,
        resolver: Arc<dyn OwnerResolver>,
    ) -> Result<CreatedApiToken, TokenError> {
        let this = self.clone();
        spawn_blocking(move || this.create_blocking(new, resolver.as_ref()))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Lists a user's tokens, newest first. **Blocking.**
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::Store`].
    pub fn list_blocking(&self, owner: &str) -> Result<Vec<ApiTokenRecord>, TokenError> {
        let owner = owner.trim().to_lowercase();
        let conn = self.store.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, owner, name, token_prefix, scopes, is_active, last_used_at, created_at
               FROM api_tokens
              WHERE owner = ?1
              ORDER BY created_at DESC, id",
        )?;
        let rows = stmt.query_map(params![owner], row_to_record)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Lists a user's tokens, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::Store`].
    pub async fn list(&self, owner: &str) -> Result<Vec<ApiTokenRecord>, TokenError> {
        let this = self.clone();
        let owner = owner.to_owned();
        spawn_blocking(move || this.list_blocking(&owner))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Whether `token_id` grants `scope`. **Blocking.**
    ///
    /// Requires `is_active = 1`. A revoked token's scopes are stored unchanged, so a
    /// check that ignored the flag would keep reporting them as granted — and since
    /// this is the natural call a scope guard makes, wiring it up naively would mean
    /// `has_scope` returning `true` for a revoked token.
    ///
    /// It does **not** check ownership. `src/auth_helpers.py:60-75` additionally
    /// requires a non-empty owner and returns 403 without it, which cannot be
    /// expressed here because this takes a bare id. A caller acting on the answer
    /// must therefore compare the token's owner itself, and the natural pairing is
    /// [`ApiTokenStore::verify`] first, which resolves the owner in the same result.
    ///
    /// Plain membership against the stored list, with **no** revalidation against
    /// [`Scope::ALL`]. That matches Python, and both halves matter:
    ///
    /// * a legacy token holding a scope this build has never heard of keeps
    ///   authenticating, rather than failing closed and logging the user out;
    /// * an unknown *stored* scope **does** match if asked for by name, exactly as
    ///   Python's membership test does. That is safe because a caller can only ask
    ///   for a scope its own code knows — no request path can name a scope this
    ///   build does not recognise, so an unknown entry can never be exercised. The
    ///   danger would be *revoking* validation here, which would let a corrupted
    ///   `scopes` value satisfy a check it should fail.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`] and [`TokenError::Store`].
    pub fn has_scope_blocking(&self, token_id: &str, scope: &Scope) -> Result<bool, TokenError> {
        let conn = self.store.connect()?;
        // `Option<i64>`, because the column is nullable with no `NOT NULL` and a
        // NULL must deny. Reading it as `i64` would make the whole check error on a
        // revoked-but-null row instead of refusing.
        let row: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT scopes, is_active FROM api_tokens WHERE id = ?1",
                params![token_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((stored, active)) = row else {
            return Err(TokenError::NoSuchToken(token_id.to_owned()));
        };
        if active != Some(1) {
            return Ok(false);
        }
        Ok(split_scopes(Some(&stored))
            .iter()
            .any(|s| s.as_str() == scope.as_str()))
    }

    /// Whether `token_id` grants `scope`, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`] and [`TokenError::Store`].
    pub async fn has_scope(&self, token_id: &str, scope: &Scope) -> Result<bool, TokenError> {
        let this = self.clone();
        let (token_id, scope) = (token_id.to_owned(), scope.clone());
        spawn_blocking(move || this.has_scope_blocking(&token_id, &scope))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Sets a token's active flag. **Blocking.**
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`] and [`TokenError::Store`].
    pub fn set_active_blocking(&self, token_id: &str, is_active: bool) -> Result<(), TokenError> {
        let changed = {
            let conn = self.store.connect()?;
            conn.execute(
                "UPDATE api_tokens SET is_active = ?2, updated_at = ?3 WHERE id = ?1",
                params![token_id, is_active, format_timestamp(self.clock.now_secs())],
            )?
        };
        if changed == 0 {
            return Err(TokenError::NoSuchToken(token_id.to_owned()));
        }
        self.cache.invalidate();
        Ok(())
    }

    /// Sets a token's active flag, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`] and [`TokenError::Store`].
    pub async fn set_active(&self, token_id: &str, is_active: bool) -> Result<(), TokenError> {
        let this = self.clone();
        let token_id = token_id.to_owned();
        spawn_blocking(move || this.set_active_blocking(&token_id, is_active))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Replaces a token's scopes. **Blocking.**
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`], [`TokenError::UnknownScope`], and
    /// [`TokenError::Store`].
    pub fn set_scopes_blocking(
        &self,
        token_id: &str,
        scopes: &ScopeRequest,
    ) -> Result<Vec<Scope>, TokenError> {
        let normalised = normalise_scopes(scopes, None)?;
        let changed = {
            let conn = self.store.connect()?;
            conn.execute(
                "UPDATE api_tokens SET scopes = ?2, updated_at = ?3 WHERE id = ?1",
                params![
                    token_id,
                    join_scopes(&normalised),
                    format_timestamp(self.clock.now_secs())
                ],
            )?
        };
        if changed == 0 {
            return Err(TokenError::NoSuchToken(token_id.to_owned()));
        }
        self.cache.invalidate();
        Ok(normalised)
    }

    /// Replaces a token's scopes, on the blocking pool.
    ///
    /// # Errors
    ///
    /// As [`ApiTokenStore::set_scopes_blocking`].
    pub async fn set_scopes(
        &self,
        token_id: &str,
        scopes: ScopeRequest,
    ) -> Result<Vec<Scope>, TokenError> {
        let this = self.clone();
        let token_id = token_id.to_owned();
        spawn_blocking(move || this.set_scopes_blocking(&token_id, &scopes))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Deletes one token. **Blocking.**
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`] and [`TokenError::Store`].
    pub fn delete_blocking(&self, token_id: &str) -> Result<(), TokenError> {
        let changed = {
            let conn = self.store.connect()?;
            conn.execute("DELETE FROM api_tokens WHERE id = ?1", params![token_id])?
        };
        if changed == 0 {
            return Err(TokenError::NoSuchToken(token_id.to_owned()));
        }
        self.cache.invalidate();
        Ok(())
    }

    /// Deletes one token, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::NoSuchToken`] and [`TokenError::Store`].
    pub async fn delete(&self, token_id: &str) -> Result<(), TokenError> {
        let this = self.clone();
        let token_id = token_id.to_owned();
        spawn_blocking(move || this.delete_blocking(&token_id))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Deletes every token owned by `owner`, returning how many went.
    ///
    /// # Contract with `auth::AuthStore::delete_user` — read before wiring
    ///
    /// `core/auth.py:306-318` deletes the `ApiToken` rows **inside**
    /// `_config_lock`, *before* removing the user, and on failure logs a warning
    /// and returns `False` having touched nothing — so the admin can retry. A
    /// caller must therefore:
    ///
    /// 1. call this **before** removing the auth row and before any `save_auth`;
    /// 2. treat an `Err` here as fatal to the whole delete, leaving the user and
    ///    their sessions untouched;
    /// 3. not hold `auth`'s `config_lock` in a way that deadlocks — the guard is
    ///    a plain non-reentrant `Mutex`, so prefer the
    ///    `revoke_user_sessions_locked(&guard, …)` idiom already used there.
    ///
    /// The owner predicate stays a bare equality so `ix_api_tokens_owner` is used.
    ///
    /// **Blocking.**
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::Store`].
    pub fn delete_for_owner_blocking(&self, owner: &str) -> Result<usize, TokenError> {
        let owner = owner.trim().to_lowercase();
        let removed = {
            let conn = self.store.connect()?;
            conn.execute("DELETE FROM api_tokens WHERE owner = ?1", params![owner])?
        };
        if removed > 0 {
            self.cache.invalidate();
        }
        Ok(removed)
    }

    /// Deletes every token owned by `owner`, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::Store`].
    pub async fn delete_for_owner(&self, owner: &str) -> Result<usize, TokenError> {
        let this = self.clone();
        let owner = owner.to_owned();
        spawn_blocking(move || this.delete_for_owner_blocking(&owner))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Records a successful authentication. **Blocking.**
    ///
    /// Deliberately does **not** invalidate the cache: `app.py:443-457` writes
    /// only `last_used_at`, and invalidating here would turn every authenticated
    /// bearer request into a full table scan plus an `auth.json` read.
    ///
    /// Also deliberately does not spawn a detached task. Python uses
    /// `asyncio.create_task` (`app.py:457`), which AGENTS.md §6 forbids; the
    /// caller owns the scheduling.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::Store`]. A failure here is not worth failing the
    /// request over, so callers should log and continue.
    pub fn touch_last_used_blocking(&self, token_id: &str) -> Result<(), TokenError> {
        let conn = self.store.connect()?;
        conn.execute(
            "UPDATE api_tokens SET last_used_at = ?2 WHERE id = ?1",
            params![token_id, format_timestamp(self.clock.now_secs())],
        )?;
        Ok(())
    }

    /// Records a successful authentication, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError::Store`].
    pub async fn touch_last_used(&self, token_id: &str) -> Result<(), TokenError> {
        let this = self.clone();
        let token_id = token_id.to_owned();
        spawn_blocking(move || this.touch_last_used_blocking(&token_id))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    // -- bearer verification ------------------------------------------------

    /// Verifies a presented bearer token. **Blocking.**
    ///
    /// Rebuilds the cache if stale, then walks the candidates under the token's
    /// 8-character prefix. Python `break`s on the first match (`app.py:436-438`),
    /// so with two rows sharing a prefix the later one is only reachable when the
    /// earlier does not match; that asymmetry is preserved.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError`] from the rebuild. A *candidate's* bad hash is
    /// logged and skipped rather than propagated, so one corrupt row cannot make
    /// every other token under that prefix unreachable.
    pub fn verify_blocking(
        &self,
        presented: &str,
        resolver: &dyn OwnerResolver,
    ) -> Result<Option<VerifiedToken>, TokenError> {
        let token = RawApiToken::from_client(presented)?;
        let prefix = token.prefix();

        if self.cache.is_stale() {
            self.rebuild_cache_blocking(resolver)?;
        }

        let Some(candidates) = self.cache.candidates(prefix) else {
            return Ok(None);
        };

        for candidate in candidates.iter() {
            match candidate.verify(&token) {
                Ok(true) => {
                    return Ok(Some(VerifiedToken {
                        id: candidate.id.clone(),
                        owner: candidate.owner.clone(),
                        scopes: candidate.scopes.clone(),
                    }));
                }
                Ok(false) => {}
                Err(e) => {
                    // Availability-only: the presented token is denied either way.
                    tracing::warn!(token_id = %candidate.id, error = %e, "skipping api token with an undecodable hash");
                }
            }
        }
        Ok(None)
    }

    /// Verifies a presented bearer token, on the blocking pool.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError`] from the rebuild and from length validation.
    pub async fn verify(
        &self,
        presented: String,
        resolver: Arc<dyn OwnerResolver>,
    ) -> Result<Option<VerifiedToken>, TokenError> {
        let this = self.clone();
        spawn_blocking(move || this.verify_blocking(&presented, resolver.as_ref()))
            .await
            .map_err(|_| TokenError::Interrupted)?
    }

    /// Rebuilds the prefix map from the database. **Blocking.**
    ///
    /// `app.py:314-337`'s `_refresh_token_cache`, with the generation fix. Two
    /// departures, both deliberate:
    ///
    /// * the owner set is resolved **once**, before the loop, rather than per row;
    /// * a failing resolver leaves the cache empty instead of being cached as a
    ///   successful empty map.
    ///
    /// # Errors
    ///
    /// Propagates [`TokenError`] from the query or the resolver.
    pub fn rebuild_cache_blocking(
        &self,
        resolver: &dyn OwnerResolver,
    ) -> Result<usize, TokenError> {
        let target_generation = self.cache.current.load(Ordering::SeqCst);
        let owners = resolver.known_owners()?;

        let conn = self.store.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, owner, token_hash, token_prefix, scopes
               FROM api_tokens
              WHERE is_active = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;

        // Only allocate the map once every row has been read, then hand it to the
        // cache in one swap: a reader never sees a partially built map.
        // Built as `Vec`, converted to `Box<[Candidate]>` at the swap below so the
        // cached type carries no capacity field (AGENTS.md §12.1 rule 1).
        let mut built: HashMap<String, Vec<Candidate>> = HashMap::new();
        let mut total = 0usize;

        for row in rows {
            let (id, owner, hash, prefix, scopes) = row?;
            total += 1;

            // `normalize_known_username` returns None for a falsy or unknown
            // owner, and `app.py:330` skips the row.
            let Some(owner_key) = owner
                .as_deref()
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_lowercase)
                .filter(|o| owners.contains(o))
            else {
                tracing::warn!(token_id = %id, "ignoring active api token for an unknown owner");
                continue;
            };

            built.entry(prefix).or_default().push(Candidate {
                id,
                token_hash: hash,
                owner: owner_key,
                scopes: split_scopes(scopes.as_deref()).into_boxed_slice(),
            });
        }
        drop(stmt);
        drop(conn);

        for candidates in built.values_mut() {
            candidates.sort_by(|a, b| a.id.cmp(&b.id));
        }

        let boxed: HashMap<String, Arc<[Candidate]>> = built
            .into_iter()
            .map(|(prefix, candidates)| (prefix, Arc::from(candidates.into_boxed_slice())))
            .collect();
        self.cache.swap(boxed, target_generation);
        Ok(total)
    }
}

/// Trims a token name and truncates it to [`MAX_NAME_LEN`].
///
/// Truncation is by **code point**, not byte: `&s[..100]` panics on a multi-byte
/// boundary, and `panic` is denied workspace-wide. `strip()` runs before the
/// emptiness check (`api_token_routes.py:123-125`), so a 200-space name is an
/// error rather than a truncated name.
///
/// # Errors
///
/// Returns [`TokenError::NameRequired`] when nothing is left after trimming.
fn normalise_name(raw: &str) -> Result<String, TokenError> {
    let name: String = raw.trim().chars().take(MAX_NAME_LEN).collect();
    if name.is_empty() {
        return Err(TokenError::NameRequired);
    }
    Ok(name)
}

/// Maps a result row to [`ApiTokenRecord`].
///
/// `is_active` arrives as an integer because the column is nullable with no
/// `NOT NULL`: SQLAlchemy's `is_active == True` renders as `is_active = 1`, so
/// `NULL` and `0` are both inactive. Decoding `NULL` as "active" would serve
/// revoked tokens.
fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ApiTokenRecord> {
    let active: i64 = row.get(5)?;
    let last_used: Option<String> = row.get(6)?;
    let created: Option<String> = row.get(7)?;

    Ok(ApiTokenRecord {
        id: row.get(0)?,
        owner: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        name: row.get(2)?,
        token_prefix: row.get(3)?,
        scopes: split_scopes(row.get::<_, Option<String>>(4)?.as_deref()).into_boxed_slice(),
        is_active: active == 1,
        last_used_at: last_used.as_deref().and_then(parse_timestamp),
        created_at: created.as_deref().and_then(parse_timestamp).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use {super::*, std::sync::Mutex as StdMutex};

    /// A fixed clock, so timestamps in tests are deterministic.
    #[derive(Debug)]
    struct FixedClock(i64);

    impl Clock for FixedClock {
        fn now_secs(&self) -> i64 {
            self.0
        }
    }

    /// A resolver over a fixed set of owners.
    #[derive(Debug)]
    struct Owners(BTreeSet<String>);

    impl OwnerResolver for Owners {
        fn known_owners(&self) -> Result<BTreeSet<String>, StoreError> {
            Ok(self.0.clone())
        }
    }

    fn owners(names: &[&str]) -> Owners {
        Owners(names.iter().map(|n| (*n).to_owned()).collect())
    }

    fn tokens() -> Vec<&'static str> {
        vec!["alice", "bob", "root"]
    }

    fn store_with_clock() -> (tempfile::TempDir, ApiTokenStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("app.db"));
        store.migrate().expect("migrate");
        let api = ApiTokenStore::with_clock(store, Arc::new(FixedClock(1_758_000_000)));
        (dir, api)
    }

    fn new_token(name: &str, owner: &str) -> NewToken {
        NewToken {
            name: name.to_owned(),
            owner: owner.to_owned(),
            scopes: ScopeRequest::Unset,
            profile: None,
        }
    }

    // -- token format -------------------------------------------------------

    #[test]
    fn a_minted_token_is_ody_plus_exactly_43_base64url_chars() {
        let raw = mint_raw_token().expect("mint");
        assert_eq!(raw.len(), 47, "ody_ + 43");
        assert_eq!(
            raw.prefix().len(),
            PREFIX_CHARS,
            "prefix is the first 8 chars"
        );
        assert!(raw.prefix().starts_with(TOKEN_PREFIX), "{}", raw.prefix());
        assert!(raw.expose().starts_with("ody_"));
        // base64url alphabet: no + or /, which standard base64 would use.
        assert!(
            raw.expose()[4..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "not base64url: {}",
            raw.expose()
        );
    }

    #[test]
    fn base64url_of_32_bytes_is_43_chars_with_no_padding() {
        for seed in 0u8..=255 {
            let encoded = base64url_nopad(&[seed; 32]);
            assert_eq!(encoded.len(), 43, "seed {seed}");
            assert!(!encoded.contains('='), "seed {seed}");
        }
        assert_eq!(base64url_nopad(&[0u8; 32]).len(), 43);
        // Known-answer check on the 2-byte tail: 0xfb 0xfb, zero-padded to 18 bits,
        // is 111110 111111 101100 -> 62, 63, 44 -> "-_s" in the url-safe alphabet.
        assert!(base64url_nopad(&[0xfbu8; 32]).ends_with("-_s"));
        // The tail lengths, which integer division alone gets wrong for 2 bytes.
        assert_eq!(
            base64url_nopad(&[0u8; 1]).len(),
            2,
            "a 1-byte tail is 2 chars"
        );
        assert_eq!(
            base64url_nopad(&[0u8; 2]).len(),
            3,
            "a 2-byte tail is 3 chars"
        );
    }

    #[test]
    fn tokens_are_unique_across_mints() {
        let minted: BTreeSet<String> = (0..32)
            .map(|_| mint_raw_token().expect("mint").expose().to_owned())
            .collect();
        assert_eq!(minted.len(), 32, "token minting must not repeat");
    }

    #[test]
    fn the_raw_token_never_prints_its_secret() {
        let raw = mint_raw_token().expect("mint");
        let shown = format!("{raw:?}");
        assert!(shown.contains(raw.prefix()), "{shown}");
        assert!(
            !shown.contains(&raw.expose()[8..]),
            "the body must not appear in Debug: {shown}"
        );
    }

    #[test]
    fn an_id_is_eight_lowercase_hex_chars() {
        let id = mint_id().expect("id");
        assert_eq!(id.len(), 8, "{id}");
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "{id}"
        );
    }

    #[test]
    fn a_client_token_must_be_plausible_before_any_db_work() {
        // app.py:421 bounds the length as a cheap pre-filter.
        assert!(
            RawApiToken::from_client(&"a".repeat(11)).is_err(),
            "too short"
        );
        assert!(
            RawApiToken::from_client(&"a".repeat(101)).is_err(),
            "too long"
        );

        // A well-formed token passes.
        let good = mint_raw_token().expect("mint");
        assert!(RawApiToken::from_client(good.expose()).is_ok());
    }

    #[test]
    fn a_non_ascii_client_token_is_refused_rather_than_panicking() {
        // Regression. `prefix()` slices at byte 8, and `from_client` used to accept
        // any string within the length bounds. `"ody_ああab"` is 12 *bytes* -- long
        // enough to pass -- with byte 8 inside a 3-byte character, so calling
        // `prefix()` on it panicked. `panic = "abort"` in the release profile means
        // an unauthenticated `Authorization` header would abort the process.
        let sneaky = "ody_ああab";
        assert_eq!(sneaky.len(), 12, "the attack needs to pass a length check");
        assert!(
            RawApiToken::from_client(sneaky).is_err(),
            "a multi-byte token must be refused, not sliced"
        );

        // Every single-byte-mutation of a valid token must either be refused or safe
        // to call `prefix()` on. This is the general form of the same hazard.
        let good = mint_raw_token().expect("mint");
        for byte in good.expose().bytes().chain([0x80, 0xff, 0xc3]) {
            let mut raw = good.expose().as_bytes().to_vec();
            let index = 5.min(raw.len() - 1);
            raw[index] = byte;
            // Non-UTF-8 input cannot even be constructed as a str, so only check the
            // cases that do form a str.
            if let Ok(text) = std::str::from_utf8(&raw)
                && let Ok(token) = RawApiToken::from_client(text)
            {
                let _ = token.prefix();
            }
        }

        // And the wrong scheme prefix is refused too.
        assert!(
            RawApiToken::from_client(&format!("xyz_{}", "a".repeat(43))).is_err(),
            "a token without the ody_ prefix must be refused"
        );
    }

    #[test]
    fn a_client_token_without_the_scheme_prefix_is_refused() {
        // Python does not check this -- an unknown prefix simply matches no
        // candidates and 401s. Refusing here saves the DB round trip and makes the
        // accept-set explicit.
        for bad in [
            "aaaaaaaaaaaa",
            "odyXaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "ODY_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(
                RawApiToken::from_client(bad).is_err(),
                "{bad:?} must be refused"
            );
        }
    }

    // -- bcrypt input -------------------------------------------------------

    #[test]
    fn the_bcrypt_input_is_the_whole_ody_string_prefix_included() {
        // api_token_routes.py:130-131 hashes `raw_token`; app.py:434 verifies
        // `raw_token`. Hashing only the 43-char body would invalidate every token
        // the Python install minted.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");

        let conn = api.store.connect().expect("conn");
        let stored: String = conn
            .query_row(
                "SELECT token_hash FROM api_tokens WHERE id = ?1",
                params![created.record.id],
                |r| r.get(0),
            )
            .expect("hash");
        drop(conn);

        assert!(
            bcrypt::verify(created.raw.expose(), &stored).expect("verify"),
            "the full token must verify against the stored hash"
        );
        let body_only = &created.raw.expose()[4..];
        assert!(
            !bcrypt::verify(body_only, &stored).expect("verify"),
            "the bare 43-char body must NOT verify"
        );
    }

    #[test]
    fn a_wrong_token_is_rejected() {
        let (_dir, api) = store_with_clock();
        api.create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        let other = mint_raw_token().expect("mint");
        assert!(
            api.verify_blocking(other.expose(), &owners(&tokens()))
                .expect("verify")
                .is_none(),
            "a wrong token must not authenticate"
        );
    }

    // -- is_active ----------------------------------------------------------

    #[test]
    fn a_null_is_active_does_not_authenticate() {
        // app.py:321 filters `is_active == True`, i.e. `is_active = 1`. The column
        // is nullable with no NOT NULL, so NULL fails the predicate in Python and
        // must fail here. Decoding NULL as active would serve revoked tokens.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");

        {
            let conn = api.store.connect().expect("conn");
            conn.execute(
                "UPDATE api_tokens SET is_active = NULL WHERE id = ?1",
                params![created.record.id],
            )
            .expect("null it");
        }
        api.cache().invalidate();

        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("verify")
                .is_none(),
            "a NULL is_active must not authenticate"
        );
    }

    #[test]
    fn a_zero_is_active_does_not_authenticate() {
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        {
            let conn = api.store.connect().expect("conn");
            conn.execute(
                "UPDATE api_tokens SET is_active = 0 WHERE id = ?1",
                params![created.record.id],
            )
            .expect("zero it");
        }
        api.cache().invalidate();
        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("verify")
                .is_none()
        );
    }

    #[test]
    fn deactivating_a_token_stops_it_authenticating() {
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("v")
                .is_some()
        );

        api.set_active_blocking(&created.record.id, false)
            .expect("deactivate");
        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("v")
                .is_none()
        );
        assert!(!api.list_blocking("alice").expect("list")[0].is_active);
    }

    // -- owner resolution ---------------------------------------------------

    #[test]
    fn creation_refuses_an_owner_that_does_not_exist() {
        // Python stores owner=NULL when auth is disabled and then drops the row on
        // every refresh, so the operator gets a token that can never authenticate.
        let (_dir, api) = store_with_clock();
        let err = api
            .create_blocking(new_token("n8n", "ghost"), &owners(&tokens()))
            .expect_err("must refuse");
        assert!(matches!(err, TokenError::UnknownOwner(_)), "{err:?}");
        assert!(api.list_blocking("ghost").expect("list").is_empty());
    }

    #[test]
    fn a_token_whose_owner_disappears_stops_authenticating() {
        // app.py:323-333 skips rows whose owner no longer resolves.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("v")
                .is_some()
        );

        // Alice is deleted; the token row survives, as it would in Python.
        api.rebuild_cache_blocking(&owners(&["bob", "root"]))
            .expect("rebuild");
        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("v")
                .is_none(),
            "a token for a vanished owner must not authenticate"
        );
    }

    #[test]
    fn a_failing_resolver_leaves_the_cache_empty_rather_than_caching_a_success() {
        #[derive(Debug)]
        struct Broken;

        impl OwnerResolver for Broken {
            fn known_owners(&self) -> Result<BTreeSet<String>, StoreError> {
                Err(StoreError::Interrupted)
            }
        }

        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");

        assert!(api.rebuild_cache_blocking(&Broken).is_err());
        assert_eq!(api.cache().prefix_count(), 0, "cache must be left empty");
        assert!(
            api.verify_blocking(created.raw.expose(), &Broken).is_err(),
            "a failed refresh must propagate, not yield a cached empty success"
        );
    }

    #[test]
    fn the_owner_resolver_is_called_once_per_rebuild_not_once_per_row() {
        // app.py:323 resolves inside the per-row loop: one auth.json read and
        // parse per token, plus a torn snapshot if a user is created mid-loop.
        #[derive(Debug)]
        struct Counting {
            calls: StdMutex<usize>,
            owners: BTreeSet<String>,
        }

        impl OwnerResolver for Counting {
            fn known_owners(&self) -> Result<BTreeSet<String>, StoreError> {
                *self
                    .calls
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
                Ok(self.owners.clone())
            }
        }

        let (_dir, api) = store_with_clock();
        for name in ["a", "b", "c", "d", "e"] {
            api.create_blocking(new_token(name, "alice"), &owners(&tokens()))
                .expect("create");
        }

        let counter = Counting {
            calls: StdMutex::new(0),
            owners: tokens().into_iter().map(str::to_owned).collect(),
        };
        let total = api.rebuild_cache_blocking(&counter).expect("rebuild");
        assert_eq!(total, 5);
        assert_eq!(
            *counter
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            1,
            "the owner set must be resolved once, not per row"
        );
    }

    // -- scopes -------------------------------------------------------------

    #[test]
    fn all_fourteen_scopes_are_accepted_and_nothing_else_is() {
        for name in Scope::ALL {
            assert!(Scope::parse(name).is_ok(), "{name} should be valid");
        }
        assert_eq!(Scope::ALL.len(), 14);
        for bad in ["", "chat ", "Chat", "admin", "todos", "email"] {
            assert!(Scope::parse(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn all_four_profiles_expand_to_exactly_the_python_lists() {
        let cases: [(TokenProfile, &[&str]); 4] = [
            (TokenProfile::Chat, &["chat"]),
            (TokenProfile::CodexTodos, &["todos:read", "todos:write"]),
            (
                TokenProfile::CodexDocuments,
                &["documents:read", "documents:write"],
            ),
            (
                TokenProfile::CodexEmailDrafts,
                &[
                    "email:read",
                    "email:draft",
                    "documents:read",
                    "documents:write",
                ],
            ),
        ];
        for (profile, expected) in cases {
            let got: Vec<String> = normalise_scopes(&ScopeRequest::Unset, Some(profile.as_str()))
                .expect("profile")
                .iter()
                .map(ToString::to_string)
                .collect();
            assert_eq!(got, expected, "profile {}", profile.as_str());
        }
        assert_eq!(TokenProfile::ALL.len(), 4);
    }

    #[test]
    fn a_profile_name_is_case_sensitive_and_strip_only() {
        // api_token_routes.py:41 is `(profile or "").strip()`, no folding.
        // `api_token_routes.py:41` is `(profile or "").strip()` and then a membership
        // test, so " Chat " trims to "Chat", which is NOT a profile: a 400. Stripping
        // does not make the lookup case-insensitive.
        assert!(matches!(
            normalise_scopes(&ScopeRequest::Unset, Some(" Chat ")),
            Err(TokenError::UnknownProfile(_))
        ));
        assert!(matches!(
            normalise_scopes(&ScopeRequest::Unset, Some("Chat")),
            Err(TokenError::UnknownProfile(_))
        ));
        // Padding around a *valid* name is stripped and accepted.
        assert!(normalise_scopes(&ScopeRequest::Unset, Some(" chat ")).is_ok());
        assert!(normalise_scopes(&ScopeRequest::Unset, Some("nope")).is_err());
    }

    #[test]
    fn a_profile_replaces_the_scope_list_rather_than_merging() {
        // api_token_routes.py:42-51: a non-empty profile wins outright.
        let got = normalise_scopes(
            &ScopeRequest::List(vec!["memory:read".into()]),
            Some("codex_todos"),
        )
        .expect("profile wins");
        let names: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(names, vec!["todos:read", "todos:write"]);
    }

    #[test]
    fn every_write_scope_splices_its_read_scope_in_immediately_before_it() {
        for (write, read) in Scope::IMPLYING_PAIRS {
            let got = normalise_scopes(&ScopeRequest::text(write), None).expect("normalise");
            let names: Vec<String> = got.iter().map(ToString::to_string).collect();
            assert_eq!(
                names,
                vec![read.to_owned(), write.to_owned()],
                "pair {write}"
            );
        }
    }

    #[test]
    fn a_present_read_scope_is_not_moved_or_duplicated() {
        // ensure_before returns early when the read scope is already held, so the
        // caller's ordering survives untouched.
        let got = normalise_scopes(&ScopeRequest::text("todos:read,calendar:write"), None)
            .expect("normalise");
        let names: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            vec!["todos:read", "calendar:read", "calendar:write"],
            "an existing read scope must not move"
        );
    }

    #[test]
    fn later_pairs_index_around_earlier_insertions() {
        // ensure_before recomputes its index each call, so an earlier splice
        // shifts the later ones. All six at once must come out in Python's order.
        let text =
            "todos:write,documents:write,calendar:write,memory:write,email:draft,cookbook:launch";
        let got = normalise_scopes(&ScopeRequest::text(text), None).expect("normalise");
        let names: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            vec![
                "todos:read",
                "todos:write",
                "documents:read",
                "documents:write",
                "calendar:read",
                "calendar:write",
                "memory:read",
                "memory:write",
                "email:read",
                "email:draft",
                "cookbook:read",
                "cookbook:launch",
            ]
        );
    }

    #[test]
    fn spaces_are_scope_separators_not_just_padding() {
        // api_token_routes.py:49 is `scopes.replace(" ", ",").split(",")`.
        let got =
            normalise_scopes(&ScopeRequest::text("chat memory:read"), None).expect("normalise");
        let names: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(names, vec!["chat", "memory:read"]);
    }

    #[test]
    fn empty_scope_input_defaults_to_chat_by_both_routes() {
        // "" hits the else at line 50; [] hits `normalized or [DEFAULT_SCOPES]`
        for request in [
            ScopeRequest::Unset,
            ScopeRequest::text(""),
            ScopeRequest::text("   "),
            ScopeRequest::List(vec![]),
            ScopeRequest::List(vec![String::new(), "  ".to_owned()]),
        ] {
            let got = normalise_scopes(&request, None).expect("normalise");
            let names: Vec<String> = got.iter().map(ToString::to_string).collect();
            assert_eq!(names, vec!["chat"], "input {request:?}");
        }
    }

    #[test]
    fn duplicates_are_dropped_keeping_first_seen_order() {
        let got = normalise_scopes(
            &ScopeRequest::List(vec![
                "memory:read".into(),
                "chat".into(),
                "memory:read".into(),
                "todos:write".into(),
                "chat".into(),
            ]),
            None,
        )
        .expect("normalise");
        let names: Vec<String> = got.iter().map(ToString::to_string).collect();
        assert_eq!(
            names,
            vec!["memory:read", "chat", "todos:read", "todos:write"]
        );
    }

    #[test]
    fn an_unknown_scope_is_rejected_rather_than_silently_dropped() {
        let err = normalise_scopes(&ScopeRequest::text("chat,admin"), None).expect_err("reject");
        assert!(matches!(err, TokenError::UnknownScope(_)), "{err:?}");
    }

    #[test]
    fn stored_scopes_round_trip_through_the_column_format() {
        let got = normalise_scopes(&ScopeRequest::text("todos:write,chat"), None).expect("n");
        let joined = join_scopes(&got);
        assert_eq!(joined, "todos:read,todos:write,chat");
        let back: Vec<String> = split_scopes(Some(&joined))
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(back, vec!["todos:read", "todos:write", "chat"]);
    }

    #[test]
    fn an_empty_stored_scope_reads_as_chat_and_keeps_unknown_ones() {
        // app.py:331 and api_token_routes.py:90 use `(scopes or DEFAULT_SCOPES)`
        // with no validation, so an unknown scope must survive to grant nothing.
        assert_eq!(split_scopes(None).len(), 1);
        assert_eq!(split_scopes(Some("")).len(), 1);
        assert_eq!(split_scopes(Some("  "))[0].as_str(), "chat");
        let kept = split_scopes(Some("chat,quantum:teleport"));
        assert_eq!(kept.len(), 2, "an unknown scope must survive");
        assert_eq!(kept[1].as_str(), "quantum:teleport");
    }

    #[test]
    fn has_scope_is_plain_membership_not_revalidation() {
        // Revalidating against Scope::ALL would silently break a legacy token, and
        // a scope from a newer build must grant nothing rather than fail the lookup.
        // The write path correctly *refuses* to store an unknown scope, so the row is
        // planted directly -- that is exactly the state a newer Python build leaves.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("legacy", "alice"), &owners(&tokens()))
            .expect("create");
        {
            let conn = api.store.connect().expect("conn");
            conn.execute(
                "UPDATE api_tokens SET scopes = ?2 WHERE id = ?1",
                params![created.record.id, "chat,quantum:teleport"],
            )
            .expect("plant");
        }

        let chat = Scope::parse("chat").expect("scope");
        assert!(
            api.has_scope_blocking(&created.record.id, &chat)
                .expect("has_scope"),
            "a legacy token must keep working"
        );

        let unknown = Scope("quantum:teleport".into());
        assert!(
            api.has_scope_blocking(&created.record.id, &unknown)
                .expect("has_scope"),
            "an unknown stored scope matches by name, as Python's membership test does"
        );

        // A known scope that was never granted still denies.
        let email = Scope::parse("email:send").expect("scope");
        assert!(
            !api.has_scope_blocking(&created.record.id, &email)
                .expect("has_scope"),
            "an absent scope must deny"
        );

        assert!(matches!(
            Scope::parse("can_use_bash").expect_err("not a scope"),
            TokenError::UnknownScope(_)
        ));
    }

    #[test]
    fn the_write_path_refuses_to_store_an_unknown_scope() {
        let (_dir, api) = store_with_clock();
        let err = api
            .create_blocking(
                NewToken {
                    name: "bad".to_owned(),
                    owner: "alice".to_owned(),
                    scopes: ScopeRequest::text("chat,quantum:teleport"),
                    profile: None,
                },
                &owners(&tokens()),
            )
            .expect_err("must refuse");
        assert!(matches!(err, TokenError::UnknownScope(_)), "{err:?}");
        assert!(api.list_blocking("alice").expect("list").is_empty());
    }

    // -- the cache ----------------------------------------------------------

    #[test]
    fn two_tokens_sharing_a_prefix_both_stay_reachable() {
        // `raw_token[:8]` is only 24 bits, so collisions are expected, and a prefix
        // must map to a *list*. Two random tokens cannot be made to collide on demand,
        // so the rows are planted with hand-chosen plaintexts whose first 8 characters
        // genuinely agree: `ody_` plus four base64url `A`s.
        let (_dir, api) = store_with_clock();
        let shared_prefix = "ody_AAAA";

        let plaintexts = [
            format!("{shared_prefix}{}", "A".repeat(39)),
            format!("{shared_prefix}{}", "A".repeat(38) + "B"),
        ];
        for (i, plaintext) in plaintexts.iter().enumerate() {
            let hash = bcrypt::hash(plaintext, bcrypt::DEFAULT_COST).expect("hash");
            let conn = api.store.connect().expect("conn");
            conn.execute(
                "INSERT INTO api_tokens
                     (id, owner, name, token_hash, token_prefix, scopes, is_active,
                      last_used_at, created_at, updated_at)
                 VALUES (?1, 'alice', ?2, ?3, ?4, 'chat', 1, NULL, ?5, ?5)",
                params![
                    format!("collide{i}"),
                    format!("collider {i}"),
                    hash,
                    shared_prefix,
                    format_timestamp(1_758_000_000),
                ],
            )
            .expect("insert");
        }
        api.cache().invalidate();

        // `candidates` never rebuilds on its own -- only `verify` does -- so the
        // refresh has to be explicit here.
        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");
        let candidates = api
            .cache()
            .candidates(shared_prefix)
            .expect("prefix present");
        assert_eq!(candidates.len(), 2, "both must be candidates");

        // Both still authenticate, so neither shadows the other.
        for plaintext in &plaintexts {
            let found = api
                .verify_blocking(plaintext, &owners(&tokens()))
                .expect("verify")
                .expect("must verify");
            assert_eq!(found.owner, "alice");
        }
    }

    #[test]
    fn a_corrupt_hash_does_not_shadow_a_valid_sibling() {
        // Python's checkpw raises on a malformed hash, which escapes the candidate
        // loop and 401s -- making every other token under that prefix unreachable.
        let (_dir, api) = store_with_clock();
        let good = api
            .create_blocking(new_token("good", "alice"), &owners(&tokens()))
            .expect("create");
        let bad = api
            .create_blocking(new_token("bad", "alice"), &owners(&tokens()))
            .expect("create");

        let shared = good.raw.prefix().to_owned();
        {
            let conn = api.store.connect().expect("conn");
            // Same prefix, and a hash that is not bcrypt at all.
            conn.execute(
                "UPDATE api_tokens SET token_prefix = ?2, token_hash = 'garbage' WHERE id = ?1",
                params![bad.record.id, shared],
            )
            .expect("corrupt");
        }
        api.cache().invalidate();

        let found = api
            .verify_blocking(good.raw.expose(), &owners(&tokens()))
            .expect("verify")
            .expect("the valid sibling must still authenticate");
        assert_eq!(found.id, good.record.id);
    }

    #[test]
    fn a_mid_refresh_mutation_is_not_lost() {
        // The bug in app.py:337: clearing a dirty flag after a rebuild discards a
        // token created during that rebuild, and the flag then reads false.
        let cache = PrefixCache::new();
        assert!(cache.is_stale(), "a fresh cache must be stale");

        // Simulate: read the generation a refresh is building for, then mutate.
        let target = cache.current.load(Ordering::SeqCst);
        cache.invalidate();
        cache.swap(HashMap::new(), target);

        assert!(
            cache.is_stale(),
            "a mutation during the rebuild must leave the cache stale"
        );

        // Rebuilding again converges.
        let target = cache.current.load(Ordering::SeqCst);
        cache.swap(HashMap::new(), target);
        assert!(!cache.is_stale());
    }

    #[test]
    fn the_swap_never_exposes_a_partially_built_map() {
        // tests/test_token_cache_atomic_swap.py: a reader holding the read lock
        // sees either the whole old map or the whole new one.
        const PREFIXES: usize = 8;
        const PER_PREFIX: usize = 2;

        let cache = Arc::new(PrefixCache::new());
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let reader = {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut observations = 0usize;
                while !stop.load(Ordering::SeqCst) {
                    // Wholly empty or wholly populated -- never in between.
                    let seen = visible(&cache);
                    assert!(
                        seen.is_empty() || seen.len() == PREFIXES,
                        "a reader saw {} of {PREFIXES} prefixes: {seen:?}",
                        seen.len()
                    );
                    for (prefix, candidates) in seen {
                        assert_eq!(candidates, PER_PREFIX, "partial slice for {prefix}");
                    }
                    observations += 1;
                }
                observations
            })
        };

        // The Python assertion being ported: `assert len(_token_cache) == 0` fails
        // for any clear-then-refill implementation (`test_token_cache_atomic_swap.py:138`).
        let mut built: HashMap<String, Arc<[Candidate]>> = HashMap::new();
        for round in 0..200u32 {
            built.clear();
            for i in 0..PREFIXES {
                let candidates: Vec<Candidate> = (0..PER_PREFIX)
                    .map(|slot| Candidate {
                        id: format!("{round}-{i}-{slot}"),
                        token_hash: "h".into(),
                        owner: "alice".into(),
                        scopes: Scope::ALL[..1].iter().map(|s| Scope((*s).into())).collect(),
                    })
                    .collect();
                built.insert(format!("p{i}"), Arc::from(candidates.into_boxed_slice()));
            }
            cache.swap(built.clone(), cache.current.load(Ordering::SeqCst));
        }

        stop.store(true, Ordering::SeqCst);
        let observations = reader.join().expect("reader");
        assert!(observations > 0, "the reader never ran");
    }

    /// Snapshot every prefix currently visible, for the atomic-swap test.
    fn visible(cache: &PrefixCache) -> Vec<(String, usize)> {
        let map = cache
            .map
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.iter().map(|(k, v)| (k.clone(), v.len())).collect()
    }

    #[test]
    fn touch_last_used_leaves_the_cache_clean() {
        // app.py:443-457 writes only last_used_at. Invalidating here would make
        // every authenticated bearer request rebuild the whole cache.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");
        assert!(!api.cache().is_stale());

        api.touch_last_used_blocking(&created.record.id)
            .expect("touch");
        assert!(
            !api.cache().is_stale(),
            "touch_last_used must not invalidate the cache"
        );
        assert_eq!(
            api.list_blocking("alice").expect("list")[0].last_used_at,
            Some(1_758_000_000),
            "but it must persist the timestamp"
        );
    }

    #[test]
    fn every_mutation_invalidates_the_cache() {
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");

        api.set_active_blocking(&created.record.id, true)
            .expect("set_active");
        assert!(api.cache().is_stale(), "set_active must invalidate");

        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");
        api.set_scopes_blocking(&created.record.id, &ScopeRequest::text("memory:read"))
            .expect("set_scopes");
        assert!(api.cache().is_stale(), "set_scopes must invalidate");

        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");
        api.delete_blocking(&created.record.id).expect("delete");
        assert!(api.cache().is_stale(), "delete must invalidate");
    }

    // -- CRUD ---------------------------------------------------------------

    #[test]
    fn a_name_is_stripped_then_truncated_by_code_point() {
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("  spaced name  ", "alice"), &owners(&tokens()))
            .expect("create");
        assert_eq!(created.record.name, "spaced name");

        // A multi-byte name must not panic on a byte boundary (api_token_routes.py:123
        // slices str, which is by code point).
        let long = "é".repeat(MAX_NAME_LEN + 40);
        let created = api
            .create_blocking(new_token(&long, "alice"), &owners(&tokens()))
            .expect("create");
        assert_eq!(created.record.name.chars().count(), MAX_NAME_LEN);
    }

    #[test]
    fn an_empty_or_whitespace_name_is_rejected() {
        let (_dir, api) = store_with_clock();
        for bad in ["", "   ", "\t\n"] {
            let err = api
                .create_blocking(new_token(bad, "alice"), &owners(&tokens()))
                .expect_err("must refuse");
            assert!(
                matches!(err, TokenError::NameRequired),
                "{bad:?} gave {err:?}"
            );
        }
    }

    #[test]
    fn a_created_token_is_listed_with_its_record_intact() {
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");

        let listed = api.list_blocking("alice").expect("list");
        assert_eq!(listed.len(), 1);
        let row = &listed[0];
        assert_eq!(row.id, created.record.id);
        assert_eq!(row.owner, "alice");
        assert_eq!(row.name, "n8n");
        assert_eq!(row.token_prefix, created.raw.prefix());
        assert_eq!(row.scopes.as_ref(), &[Scope("chat".into())]);
        assert!(row.is_active);
        assert_eq!(row.last_used_at, None);
        assert_eq!(row.created_at, 1_758_000_000);
    }

    #[test]
    fn listing_is_scoped_to_the_owner_and_excludes_nothing_else() {
        let (_dir, api) = store_with_clock();
        api.create_blocking(new_token("a", "alice"), &owners(&tokens()))
            .expect("create");
        api.create_blocking(new_token("b", "bob"), &owners(&tokens()))
            .expect("create");
        assert_eq!(api.list_blocking("alice").expect("list").len(), 1);
        assert_eq!(api.list_blocking("bob").expect("list").len(), 1);
        assert!(api.list_blocking("root").expect("list").is_empty());
        // Case-insensitive, like every other username lookup.
        assert_eq!(api.list_blocking("ALICE").expect("list").len(), 1);
    }

    #[test]
    fn mutating_a_missing_token_reports_not_found() {
        let (_dir, api) = store_with_clock();
        assert!(matches!(
            api.delete_blocking("nope").expect_err("missing"),
            TokenError::NoSuchToken(_)
        ));
        assert!(matches!(
            api.set_active_blocking("nope", true).expect_err("missing"),
            TokenError::NoSuchToken(_)
        ));
        assert!(matches!(
            api.has_scope_blocking("nope", &Scope("chat".into()))
                .expect_err("missing"),
            TokenError::NoSuchToken(_)
        ));
    }

    #[test]
    fn deleting_for_owner_removes_every_row_and_nothing_else() {
        let (_dir, api) = store_with_clock();
        for name in ["a", "b", "c"] {
            api.create_blocking(new_token(name, "alice"), &owners(&tokens()))
                .expect("create");
        }
        api.create_blocking(new_token("d", "bob"), &owners(&tokens()))
            .expect("create");

        assert_eq!(api.delete_for_owner_blocking("alice").expect("delete"), 3);
        assert!(api.list_blocking("alice").expect("list").is_empty());
        assert_eq!(
            api.list_blocking("bob").expect("list").len(),
            1,
            "another owner's token must survive"
        );
        assert_eq!(api.delete_for_owner_blocking("alice").expect("again"), 0);
    }

    #[test]
    fn the_verified_owner_is_the_real_account_never_the_api_pseudo_user() {
        // app.py:451 sets current_user = "api" for the *session*, but the token's
        // own owner is carried separately and is what ownership decisions must use
        // (auth_helpers.py:15).
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        let found = api
            .verify_blocking(created.raw.expose(), &owners(&tokens()))
            .expect("verify")
            .expect("verified");
        assert_eq!(found.owner, "alice");
        assert_ne!(found.owner, "api");
        assert_ne!(found.id, "api");
    }

    #[test]
    fn concurrent_creation_loses_no_rows() {
        let (_dir, api) = store_with_clock();
        std::thread::scope(|scope| {
            for i in 0..8 {
                let api = api.clone();
                scope.spawn(move || {
                    api.create_blocking(new_token(&format!("t{i}"), "alice"), &owners(&tokens()))
                        .expect("create");
                });
            }
        });
        assert_eq!(api.list_blocking("alice").expect("list").len(), 8);
    }

    #[test]
    fn concurrent_verification_is_consistent_while_the_cache_is_rebuilt() {
        let (_dir, api) = store_with_clock();
        let mut raws = Vec::new();
        for i in 0..6 {
            let created = api
                .create_blocking(new_token(&format!("t{i}"), "alice"), &owners(&tokens()))
                .expect("create");
            raws.push(created.raw.expose().to_owned());
        }

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rebuilder = {
            let api = api.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    api.rebuild_cache_blocking(&owners(&tokens()))
                        .expect("rebuild");
                }
            })
        };

        std::thread::scope(|scope| {
            for raw in &raws {
                let api = api.clone();
                let raw = raw.clone();
                scope.spawn(move || {
                    for _ in 0..10 {
                        let found = api
                            .verify_blocking(&raw, &owners(&tokens()))
                            .expect("verify");
                        assert!(found.is_some(), "{raw} must verify throughout");
                    }
                });
            }
        });

        stop.store(true, Ordering::SeqCst);
        rebuilder.join().expect("rebuilder");
    }

    // -- layout -------------------------------------------------------------

    #[test]
    fn the_cached_types_stay_small() {
        // AGENTS.md §11: a new stored/cached type gets a layout ceiling.
        // AGENTS.md §12.1 rule 1: immutable data is Box<[T]>, never Vec.
        assert!(
            size_of::<Candidate>() <= 96,
            "Candidate grew to {}",
            size_of::<Candidate>()
        );
        // Four owned Strings (24 each) + Box<[Scope]> (16) + Option<i64> (16) +
        // i64 (8) + bool, padded to 144. A regression guard, not a target.
        assert_eq!(
            size_of::<ApiTokenRecord>(),
            144,
            "ApiTokenRecord changed shape"
        );
        // A Scope is a boxed str: 16 bytes, no inline capacity slack.
        assert_eq!(size_of::<Scope>(), size_of::<Box<str>>());
    }

    // -- timestamps ---------------------------------------------------------

    #[test]
    fn timestamps_use_the_sqlalchemy_naive_utc_text_format() {
        // core/database.py:23-25 -> "YYYY-MM-DD HH:MM:SS.ffffff"
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00.000000");
        assert_eq!(
            format_timestamp(1_758_000_000),
            "2025-09-16 05:20:00.000000"
        );
        assert_eq!(format_timestamp(951_782_400), "2000-02-29 00:00:00.000000");
    }

    #[test]
    fn timestamps_round_trip() {
        for secs in [0i64, 1, 86_399, 86_400, 1_758_000_000, 4_102_444_800] {
            let text = format_timestamp(secs);
            assert_eq!(parse_timestamp(&text), Some(secs), "{text}");
        }
        assert_eq!(parse_timestamp("nonsense"), None);
        assert_eq!(parse_timestamp(""), None);
    }

    #[test]
    fn a_stored_last_used_at_round_trips_through_the_column() {
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        api.touch_last_used_blocking(&created.record.id)
            .expect("touch");
        assert_eq!(
            api.list_blocking("alice").expect("list")[0].last_used_at,
            Some(1_758_000_000)
        );
    }

    #[test]
    fn a_revoked_token_grants_no_scopes() {
        // `has_scope` is the natural call a scope guard makes, so if it ignored
        // `is_active` a revoked token would keep reporting its scopes as granted.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        let chat = Scope::parse("chat").expect("scope");

        assert!(
            api.has_scope_blocking(&created.record.id, &chat)
                .expect("has_scope"),
            "an active token grants its scope"
        );

        api.set_active_blocking(&created.record.id, false)
            .expect("revoke");
        assert!(
            !api.has_scope_blocking(&created.record.id, &chat)
                .expect("has_scope"),
            "a revoked token must grant nothing"
        );

        // And for the other non-1 values, including the NULL that fails Python's
        // `is_active == True` predicate.
        for raw in ["0", "NULL", "2"] {
            let sql = if raw == "NULL" {
                "UPDATE api_tokens SET is_active = NULL WHERE id = ?1".to_owned()
            } else {
                format!("UPDATE api_tokens SET is_active = {raw} WHERE id = ?1")
            };
            let conn = api.store.connect().expect("conn");
            conn.execute(&sql, params![created.record.id])
                .expect("write");
            assert!(
                !api.has_scope_blocking(&created.record.id, &chat)
                    .expect("has_scope"),
                "is_active = {raw} must grant nothing"
            );
        }
    }

    #[test]
    fn a_cache_rebuild_obsoleted_by_a_mutation_is_discarded() {
        // Two rebuilds overlap: R1 targets generation G and queries, a mutation bumps
        // to G+1, R2 installs the newer map, and R1 then installs its older snapshot.
        // Installing it would let R1's *own* following lookup authenticate a token the
        // mutation revoked -- a bounded fail-open window, and the reason `swap` checks
        // the generation rather than trusting the caller.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");
        let good_prefixes = api.cache().prefix_count();
        assert_eq!(good_prefixes, 1, "one live prefix");

        // The generation the "slow" rebuild captured, then a mutation lands.
        let stale_generation = api.cache().current.load(Ordering::SeqCst);
        api.set_active_blocking(&created.record.id, false)
            .expect("revoke");

        // The stale rebuild finishes late and tries to install a map that still
        // carries the now-revoked token -- and, distinctly, more prefixes than the
        // live map has. The shape difference matters: if the two maps happened to
        // hold the same prefix count this assertion could not see the swap at all.
        let hash = bcrypt::hash(created.raw.expose(), bcrypt::DEFAULT_COST).expect("hash");
        let obsolete: HashMap<String, Arc<[Candidate]>> = (0..3)
            .map(|i| {
                (
                    format!("ody_stale{i}"),
                    Arc::from(
                        vec![Candidate {
                            id: created.record.id.clone(),
                            token_hash: hash.clone(),
                            owner: "alice".to_owned(),
                            scopes: Scope::ALL[..1].iter().map(|s| Scope((*s).into())).collect(),
                        }]
                        .into_boxed_slice(),
                    ),
                )
            })
            .collect();
        assert_ne!(
            obsolete.len(),
            good_prefixes,
            "the obsolete map must differ in shape, or this test cannot see the swap"
        );
        api.cache().swap(obsolete, stale_generation);

        // The load-bearing assertion: the obsolete map was NOT installed. Asserting
        // only `is_stale()` would pass either way, because installing a map built for
        // an older generation still leaves the cache stale -- which is why an earlier
        // version of this test could not see the bug it was written for.
        assert_eq!(
            api.cache().prefix_count(),
            good_prefixes,
            "an obsolete rebuild must not replace the installed map"
        );
        assert!(
            api.cache().is_stale(),
            "and the cache must remain stale so the next read rebuilds"
        );

        // End to end: a revoked token must not authenticate.
        assert!(
            api.verify_blocking(created.raw.expose(), &owners(&tokens()))
                .expect("verify")
                .is_none(),
            "a revoked token must not survive a stale rebuild"
        );
    }
    #[test]
    fn candidate_lookup_shares_rather_than_clones() {
        // The auth path must not allocate per request (AGENTS.md §12). `candidates`
        // hands back a shared slice, so two lookups of one prefix are one allocation.
        let (_dir, api) = store_with_clock();
        let created = api
            .create_blocking(new_token("n8n", "alice"), &owners(&tokens()))
            .expect("create");
        api.rebuild_cache_blocking(&owners(&tokens()))
            .expect("rebuild");

        let first = api
            .cache()
            .candidates(created.raw.prefix())
            .expect("prefix");
        assert_eq!(Arc::strong_count(&first), 2, "the map plus one caller");
        let second = api
            .cache()
            .candidates(created.raw.prefix())
            .expect("prefix");
        assert_eq!(Arc::strong_count(&second), 3, "two callers, one allocation");
        assert!(Arc::ptr_eq(&first, &second), "must be the same allocation");
    }

    #[test]
    fn out_of_range_timestamps_are_rejected_rather_than_normalised() {
        // `"2025-13-45 99:99:99"` parses as six integers; without bounds checks the
        // civil-from-days maths rolls the overflow into a plausible wrong instant,
        // and an absurd year overflows `era * 146_097` outright.
        for bad in [
            "2025-13-45 00:00:00.000000",
            "2025-00-01 00:00:00.000000",
            "2025-01-00 00:00:00.000000",
            "2025-01-01 99:00:00.000000",
            "2025-01-01 00:99:00.000000",
            "99999999999999999-01-01 00:00:00.000000",
            "-500000-01-01 00:00:00.000000",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad:?} must be rejected");
        }

        // The leap day and the epoch still round-trip.
        assert_eq!(
            parse_timestamp("2000-02-29 00:00:00.000000"),
            Some(951_782_400)
        );
        assert_eq!(parse_timestamp("1970-01-01 00:00:00.000000"), Some(0));
    }
}
