//! Model-endpoint configuration: CRUD, encrypted credentials, provider detection
//! and model discovery.
//!
//! Ported from `routes/model_routes.py` and the provider helpers in
//! `src/llm_core.py`. Per `AGENTS.md` §14.2 the Python is the specification; where
//! this module departs from it, the reason is in the item's documentation.
//!
//! ## The table
//!
//! Eighteen columns (`crates/store/migrations/0001_baseline.sql:317`), of which
//! only `id`, `name`, `base_url`, `created_at`, `updated_at` are `NOT NULL`. Two
//! consequences shape everything below:
//!
//! * **`owner` is nullable, and `NULL` means shared.** `database.py:189` says so
//!   explicitly ("null = legacy/shared") and `routes/model_routes.py:2157` sets
//!   `owner = None` when the caller passes `shared=true`. A listing therefore has
//!   to return both the caller's own endpoints *and* the shared ones, or shared
//!   endpoints silently disappear.
//! * **`provider_auth_id` is a plain indexed string with no foreign key.** That is
//!   recorded in `DATA_MIGRATION.md` §2.2 as deliberate and is preserved as-is
//!   rather than "fixed".
//!
//! ## Ownership
//!
//! ```text
//! read  (list/get): owner = :owner OR owner IS NULL
//! write (create/update/delete): owner = :owner exactly, or admin
//! ```
//!
//! Writes never match a `NULL`-owner row, so a shared endpoint cannot be edited or
//! deleted through a per-user path. Admins bypass the owner check because
//! `routes/model_routes.py` gates management on an admin privilege.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Duration,
};

use {
    fernet::Fernet,
    rusqlite::{Connection, OptionalExtension, Row, params},
};

use crate::crypto::{encrypt, is_encrypted, maybe_decrypt};

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

/// The parts of a URL this module needs.
///
/// Hand-rolled because `odysseus-store` has no URL dependency and adding one is
/// not this task's call. Only the fields provider detection and probe-building
/// use are extracted; anything else is left alone rather than half-supported.
///
/// Owns its strings rather than borrowing. Parsing allocates a `String` per call,
/// which would be a real cost in a loop — but provider detection runs once per
/// endpoint load and once per discovery probe, not per token or per byte, so the
/// clarity is worth more than the allocation. The alternative, borrowing with a
/// self-referential struct, is how `str::leak` accidents happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlParts {
    /// Lowercased hostname, without a trailing dot. Empty when absent.
    pub host: String,
    /// Explicit port, when the URL carried one.
    pub port: Option<u16>,
    /// Path component, starting with `/`. Empty for a bare host.
    pub path: String,
    /// Scheme, lowercased.
    pub scheme: String,
}

impl UrlParts {
    /// Splits `url` into the parts provider detection cares about.
    ///
    /// Never fails: a URL this cannot understand yields empty fields, and every
    /// caller treats "no host" as "no match". Failing closed here would mean a
    /// malformed configured URL could not be listed at all, which is worse than
    /// listing it with an undetectable provider.
    #[must_use]
    pub fn parse(url: &str) -> Self {
        let url = url.trim();
        // `urlparse` only produces a hostname when the URL has a scheme or an
        // explicit `//` authority. Without either, a bare word like `localhost`
        // yields no host -- which matters, because `localhost` alone would
        // otherwise be detected as a local Ollama.
        let (scheme, rest) = match url.split_once("://") {
            Some((scheme, rest)) => (scheme, rest),
            None => match url.strip_prefix("//") {
                Some(rest) => ("", rest),
                None => ("", ""),
            },
        };
        // Authority ends at the first '/', '?' or '#'.
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let tail = &rest[authority_end..];

        // Strip userinfo: everything up to the last '@' in the authority.
        let hostport = authority
            .rsplit_once('@')
            .map_or(authority, |(_, after)| after);

        // IPv6 literals are bracketed: `[::1]:11434`.
        let (host, port) = if let Some(inner) = hostport.strip_prefix('[') {
            match inner.split_once(']') {
                Some((inside, after)) => {
                    (inside, after.strip_prefix(':').and_then(|p| p.parse().ok()))
                }
                None => (hostport, None),
            }
        } else {
            match hostport.split_once(':') {
                Some((h, p)) => (h, p.parse().ok()),
                None => (hostport, None),
            }
        };

        Self {
            host: host.trim_end_matches('.').to_ascii_lowercase(),
            port,
            path: tail.split(['?', '#']).next().unwrap_or("").to_owned(),
            scheme: scheme.to_ascii_lowercase(),
        }
    }
}

/// True when `url`'s hostname **equals** `domain` or is a **subdomain** of it.
///
/// This is the single most security-relevant function in the module. The correct
/// test is `host == d || host.ends_with("." + d)`:
///
/// * a substring test matches `https://evil.test/?x=api.anthropic.com`, and
/// * a bare `ends_with(d)` matches `anthropic.com.example`, which is a domain an
///   attacker can register.
///
/// Both hand a user's API key to whoever controls the look-alike host, so the
/// dot is load-bearing. Empty input never matches.
#[must_use]
pub fn host_matches(url: &str, domain: &str) -> bool {
    if url.is_empty() || domain.is_empty() {
        return false;
    }
    let host = UrlParts::parse(url).host;
    if host.is_empty() {
        return false;
    }
    let domain = domain.trim_end_matches('.').to_ascii_lowercase();
    host == domain || host.ends_with(&format!(".{domain}"))
}

/// True for a native Ollama API base, mirroring `_is_ollama_native_url`.
///
/// Ollama exposes both a native `/api` surface and an OpenAI-compatible `/v1`
/// one, and the two need different model-list URLs. A base that already ends in
/// `/v1` is the OpenAI-compatible surface regardless of host.
#[must_use]
pub fn is_ollama_native_url(url: &str) -> bool {
    if host_matches(url, "ollama.com") {
        return true;
    }
    let parts = UrlParts::parse(url);
    let path = parts.path.trim_end_matches('/');
    if path.starts_with("/v1") {
        return false;
    }
    let local = matches!(
        parts.host.as_str(),
        "localhost" | "127.0.0.1" | "0.0.0.0" | "::1"
    ) || parts.port == Some(11434);
    local && (path.is_empty() || path == "/api" || path.starts_with("/api/"))
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// A detected API provider.
///
/// Fieldless, so one byte (§12.1 rule 4: size enums by traffic). `Unknown` is
/// deliberately *not* a variant — the unknown case is `OpenAi`, the
/// OpenAI-compatible default that most providers implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Provider {
    /// Native Ollama, including Ollama Cloud.
    Ollama,
    /// Anthropic.
    Anthropic,
    /// `OpenCode` Zen Go tier.
    OpenCodeGo,
    /// `OpenCode` Zen.
    OpenCodeZen,
    /// `OpenRouter`.
    OpenRouter,
    /// Groq.
    Groq,
    /// NVIDIA.
    Nvidia,
    /// Moonshot / Kimi (`moonshot.ai` and `moonshot.cn`).
    Moonshot,
    /// A `ChatGPT` subscription base, which exposes no model list.
    ChatGptSubscription,
    /// GitHub Copilot.
    Copilot,
    /// Cerebras.
    Cerebras,
    /// Mistral.
    Mistral,
    /// The OpenAI-compatible default, and the fallback for unknown hosts.
    OpenAi,
}

impl Provider {
    /// Every provider, for a UI listing.
    pub const ALL: [Provider; 13] = [
        Provider::Ollama,
        Provider::Anthropic,
        Provider::OpenCodeGo,
        Provider::OpenCodeZen,
        Provider::OpenRouter,
        Provider::Groq,
        Provider::Nvidia,
        Provider::Moonshot,
        Provider::ChatGptSubscription,
        Provider::Copilot,
        Provider::Cerebras,
        Provider::Mistral,
        Provider::OpenAi,
    ];

    /// The stable wire name, matching Python's `_detect_provider` return values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Provider::Ollama => "ollama",
            Provider::Anthropic => "anthropic",
            Provider::OpenCodeGo => "opencode-go",
            Provider::OpenCodeZen => "opencode-zen",
            Provider::OpenRouter => "openrouter",
            Provider::Groq => "groq",
            Provider::Nvidia => "nvidia",
            Provider::Moonshot => "moonshot",
            Provider::ChatGptSubscription => "chatgpt-subscription",
            Provider::Copilot => "copilot",
            Provider::Cerebras => "cerebras",
            Provider::Mistral => "mistral",
            Provider::OpenAi => "openai",
        }
    }

    /// Parses a wire name.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::UnknownProvider`] for a name not in [`Provider::ALL`].
    pub fn parse(name: &str) -> Result<Self, EndpointError> {
        Provider::ALL
            .into_iter()
            .find(|p| p.as_str() == name)
            .ok_or_else(|| EndpointError::UnknownProvider(name.to_owned()))
    }

    /// True when this provider has no model-list endpoint.
    ///
    /// `routes/model_routes.py:687` treats `chatgpt-subscription` as
    /// discovery-only: probing it for a model list is meaningless, so discovery
    /// returns "unsupported" rather than an empty list that looks like an outage.
    #[must_use]
    pub const fn is_discovery_only(self) -> bool {
        matches!(self, Provider::ChatGptSubscription)
    }

    /// Detects the provider for a configured base URL.
    ///
    /// Ports `_detect_provider` (`src/llm_core.py:966`) including its ordering:
    /// the more specific host patterns are tested before the generic ones, and
    /// the OpenAI-compatible fallback is last. Two entries in Python are
    /// omitted because they call into optional Python modules with no schema
    /// meaning — see the module docs.
    #[must_use]
    pub fn detect(url: &str) -> Self {
        if is_ollama_native_url(url) {
            return Provider::Ollama;
        }
        if host_matches(url, "anthropic.com") {
            return Provider::Anthropic;
        }
        // Python passes `"opencode.ai/zen/go"` to `_host_match`, which compares it
        // against a *hostname* and therefore never matches: those two branches are
        // dead upstream, even though the function's own docstring promises
        // "matches on hostname (exact or subdomain) rather than substring, so a URL
        // that merely contains a provider's domain in its path or query ... is not
        // misclassified".
        //
        // They are reproduced here as the behaviour they were evidently meant to
        // express -- hostname `opencode.ai` plus a `/zen/go` path prefix. A
        // substring search is the tempting transliteration and it breaks that
        // stated invariant: it also matches on the query string, the fragment and
        // any userinfo, so `https://attacker.example/?next=opencode.ai/zen/go`
        // would be classified as OpenCode Go and given its models URL. Host plus
        // path prefix cannot be spoofed that way.
        //
        // The prefixes are compared segment-wise, so `/zenith` is not a Zen hit.
        if host_matches(url, "opencode.ai") {
            let path = UrlParts::parse(url.trim()).path;
            let is_zen_path = |p: &str| p == "/zen" || p.starts_with("/zen/");
            if path == "/zen/go" || path.starts_with("/zen/go/") {
                return Provider::OpenCodeGo;
            }
            if is_zen_path(&path) {
                return Provider::OpenCodeZen;
            }
        }
        if host_matches(url, "openrouter.ai") {
            return Provider::OpenRouter;
        }
        if host_matches(url, "groq.com") {
            return Provider::Groq;
        }
        if host_matches(url, "nvidia.com") {
            return Provider::Nvidia;
        }
        if host_matches(url, "moonshot.ai") || host_matches(url, "moonshot.cn") {
            return Provider::Moonshot;
        }
        if host_matches(url, "cerebras.ai") {
            return Provider::Cerebras;
        }
        if host_matches(url, "mistral.ai") {
            return Provider::Mistral;
        }
        Provider::OpenAi
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Small value types
// ---------------------------------------------------------------------------

/// Whether an endpoint is local, a hosted API, or a proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndpointKind {
    /// Classify by URL, as `endpoint_kind`'s default does.
    #[default]
    Auto,
    /// A server on this machine or the LAN.
    Local,
    /// A hosted API.
    Api,
    /// Something that fronts another provider.
    Proxy,
}

impl EndpointKind {
    /// The stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EndpointKind::Auto => "auto",
            EndpointKind::Local => "local",
            EndpointKind::Api => "api",
            EndpointKind::Proxy => "proxy",
        }
    }

    /// Parses a wire name.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::UnknownEndpointKind`] for anything else.
    pub fn parse(name: &str) -> Result<Self, EndpointError> {
        match name {
            "auto" => Ok(EndpointKind::Auto),
            "local" => Ok(EndpointKind::Local),
            "api" => Ok(EndpointKind::Api),
            "proxy" => Ok(EndpointKind::Proxy),
            other => Err(EndpointError::UnknownEndpointKind(other.to_owned())),
        }
    }
}

/// How the cached model list is refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RefreshMode {
    /// Background refresh with TTL and backoff.
    #[default]
    Auto,
    /// Refresh only on an explicit request.
    Manual,
    /// Use the cached list; never refresh.
    Disabled,
}

impl RefreshMode {
    /// The stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RefreshMode::Auto => "auto",
            RefreshMode::Manual => "manual",
            RefreshMode::Disabled => "disabled",
        }
    }

    /// Parses a wire name.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::UnknownRefreshMode`] for anything else.
    pub fn parse(name: &str) -> Result<Self, EndpointError> {
        match name {
            "auto" => Ok(RefreshMode::Auto),
            "manual" => Ok(RefreshMode::Manual),
            "disabled" => Ok(RefreshMode::Disabled),
            other => Err(EndpointError::UnknownRefreshMode(other.to_owned())),
        }
    }
}

/// A `JSON` array of model ids, stored as TEXT.
///
/// `hidden_models`, `cached_models` and `pinned_models` are all JSON-in-TEXT
/// (`0001_baseline.sql:322-324`). This is `Box<str>` rather than `Vec<String>`
/// for two reasons: §12.1 rule 1 — the value is immutable once written, so a
/// `Vec`'s capacity field and slack tail are pure overhead — and because the
/// column really is text, so keeping the raw form means a row written by Python
/// round-trips byte-for-byte instead of being re-serialised with different
/// whitespace.
///
/// Empty is represented by SQL `NULL`, not `[]`, matching Python's
/// `json.dumps(x) if x else None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelIdList(Option<Box<str>>);

impl ModelIdList {
    /// The empty list, stored as `NULL`.
    #[must_use]
    pub fn empty() -> Self {
        Self(None)
    }

    /// Builds a list from raw JSON text, storing `NULL` for empty or blank input.
    #[must_use]
    pub fn from_raw(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some(text) if !text.is_empty() => Self(Some(text.to_owned().into_boxed_str())),
            _ => Self(None),
        }
    }

    /// Normalises and stores ids, de-duplicating while preserving first-seen order.
    ///
    /// Ports `_normalize_model_ids` (`routes/model_routes.py:1225`), minus the
    /// comma/newline-separated *input* form: that exists to accept a form field,
    /// and this module is not the HTTP layer. Blank ids are dropped and order is
    /// kept, because `cached_models` order is the provider's listing order and the
    /// picker shows the first chat model as the default (`model_routes.py:2196`).
    #[must_use]
    pub fn from_ids<'a, I>(ids: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut kept: Vec<&str> = Vec::new();
        for raw in ids {
            let id = raw.trim();
            if id.is_empty() {
                continue;
            }
            if seen.insert(id) {
                kept.push(id);
            }
        }
        if kept.is_empty() {
            return Self::empty();
        }
        let json = serde_json::to_string(&kept).unwrap_or_else(|_| "[]".to_owned());
        Self(Some(json.into_boxed_str()))
    }

    /// The raw JSON text, or `None` when the column is `NULL`.
    #[must_use]
    pub fn as_raw(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// True when the column is `NULL` or holds an empty array.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids().is_empty()
    }

    /// The ids, parsed.
    ///
    /// A malformed column yields an empty list rather than an error: a row whose
    /// cache is corrupt should still list, with no models, and be repairable by a
    /// re-probe. Returning an error here would make one bad cache unlistable.
    #[must_use]
    pub fn ids(&self) -> Box<[Box<str>]> {
        let Some(raw) = self.as_raw() else {
            return Box::default();
        };
        let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw)
        else {
            return Box::default();
        };
        items
            .into_iter()
            .filter_map(|v| v.as_str().map(Box::<str>::from))
            .collect()
    }

    /// True when `id` is present.
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.ids().iter().any(|have| have.as_ref() == id)
    }
}

/// An API key, stored Fernet-encrypted and never displayed.
///
/// `Debug` is hand-written to print only whether a key is present, so a stray
/// `{:?}` in a log or a test failure cannot leak a credential
/// (§7: never log secret-bearing types). There is no `Display` at all.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(Option<Box<str>>);

impl ApiKey {
    /// The absent key.
    #[must_use]
    pub fn absent() -> Self {
        Self(None)
    }

    /// Encrypts a plaintext key for storage.
    ///
    /// **Idempotent.** A value that already carries the `enc:` prefix is stored
    /// unchanged, so re-saving an endpoint does not encrypt a ciphertext and
    /// render it undecryptable. That is what makes the Python migrations'
    /// repeated `is_encrypted()` checks safe, and it is why the check is here
    /// rather than in the caller.
    #[must_use]
    pub fn seal(key: &Fernet, plaintext: Option<&str>) -> Self {
        let Some(plaintext) = plaintext.map(str::trim).filter(|p| !p.is_empty()) else {
            return Self::absent();
        };
        if is_encrypted(plaintext) {
            return Self(Some(plaintext.to_owned().into_boxed_str()));
        }
        Self(Some(encrypt(key, plaintext).into_boxed_str()))
    }

    /// Re-wraps a stored value, which may already be encrypted.
    ///
    /// Used on read-modify-write paths where the caller received the *stored*
    /// representation rather than plaintext. Idempotent for the same reason as
    /// [`ApiKey::seal`].
    #[must_use]
    pub fn reseal(self, key: &Fernet) -> Self {
        match self.0.as_deref() {
            Some(v) if is_encrypted(v) => self,
            Some(v) => Self(Some(encrypt(key, v).into_boxed_str())),
            None => Self::absent(),
        }
    }

    /// The value as stored, for writing to the column.
    #[must_use]
    pub fn as_stored(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// True when a key is present, encrypted or not.
    #[must_use]
    pub fn is_present(&self) -> bool {
        self.0.is_some()
    }

    /// Decrypts to plaintext, transparently passing an unencrypted value through.
    ///
    /// The pass-through is what lets a plaintext row written before encryption
    /// existed still be used, and it mirrors `crypto::maybe_decrypt`.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Crypto`] when the value is prefixed but cannot be
    /// decrypted — a wrong key or a tampered row.
    pub fn expose(&self, key: &Fernet) -> Result<Option<String>, EndpointError> {
        let Some(stored) = self.as_stored() else {
            return Ok(None);
        };
        maybe_decrypt(key, stored)
            .map(Some)
            .map_err(|e| EndpointError::Crypto(e.to_string()))
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "ApiKey({} bytes, encrypted)", v.len()),
            None => f.write_str("ApiKey(absent)"),
        }
    }
}

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

/// One `model_endpoints` row.
///
/// String columns are `Box<str>`: these rows are read whole and held for the life
/// of a request, so a `String`'s capacity field is overhead that says nothing once
/// the value is stored (§12.1 rule 1). `api_key` is a [`ApiKey`] so it cannot be
/// printed by accident.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEndpoint {
    /// Row id. Python uses the first 8 characters of a `UUID4`.
    pub id: Box<str>,
    /// Human label.
    pub name: Box<str>,
    /// Configured base URL.
    pub base_url: Box<str>,
    /// Fernet-encrypted credential.
    pub api_key: ApiKey,
    /// Whether the endpoint may be used.
    pub is_enabled: bool,
    /// Models hidden from the picker.
    pub hidden_models: ModelIdList,
    /// Last successfully discovered model list.
    pub cached_models: ModelIdList,
    /// Models pinned by the operator, which win over the provider's order.
    pub pinned_models: ModelIdList,
    /// `"llm"` or `"image"`.
    pub model_type: Option<Box<str>>,
    /// Local / api / proxy / auto.
    pub endpoint_kind: Option<Box<str>>,
    /// Refresh policy name.
    pub model_refresh_mode: Option<Box<str>>,
    /// TTL seconds for `auto` refresh.
    pub model_refresh_interval: Option<i64>,
    /// Per-probe timeout seconds.
    pub model_refresh_timeout: Option<i64>,
    /// Tri-state: `None` means "unknown, fall back to the model-name heuristic".
    pub supports_tools: Option<bool>,
    /// Owning account, or `None` for a shared endpoint.
    pub owner: Option<Box<str>>,
    /// Plain indexed string; deliberately **not** a foreign key (§2.2).
    pub provider_auth_id: Option<Box<str>>,
    /// Creation time, as stored.
    pub created_at: Box<str>,
    /// Last update time, as stored.
    pub updated_at: Box<str>,
}

impl ModelEndpoint {
    /// Reads a row.
    ///
    /// # Errors
    ///
    /// Returns [`rusqlite::Error`] if the row does not match the schema.
    pub fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: owned(row.get("id")?),
            name: owned(row.get("name")?),
            base_url: owned(row.get("base_url")?),
            api_key: ApiKey(
                row.get::<_, Option<String>>("api_key")?
                    .map(String::into_boxed_str),
            ),
            is_enabled: row.get::<_, Option<bool>>("is_enabled")?.unwrap_or(false),
            hidden_models: ModelIdList::from_raw(
                row.get::<_, Option<String>>("hidden_models")?.as_deref(),
            ),
            cached_models: ModelIdList::from_raw(
                row.get::<_, Option<String>>("cached_models")?.as_deref(),
            ),
            pinned_models: ModelIdList::from_raw(
                row.get::<_, Option<String>>("pinned_models")?.as_deref(),
            ),
            model_type: optional(row.get("model_type")?),
            endpoint_kind: optional(row.get("endpoint_kind")?),
            model_refresh_mode: optional(row.get("model_refresh_mode")?),
            model_refresh_interval: row.get("model_refresh_interval")?,
            model_refresh_timeout: row.get("model_refresh_timeout")?,
            supports_tools: row.get("supports_tools")?,
            owner: optional(row.get("owner")?),
            provider_auth_id: optional(row.get("provider_auth_id")?),
            created_at: owned(row.get("created_at")?),
            updated_at: owned(row.get("updated_at")?),
        })
    }

    /// The detected provider for this endpoint's base URL.
    #[must_use]
    pub fn provider(&self) -> Provider {
        Provider::detect(&self.base_url)
    }

    /// The endpoint kind, defaulting to `auto` when the column is `NULL`.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::UnknownEndpointKind`] when the stored value is not
    /// one of the four known names — which would mean the row was written by a
    /// newer version than this build.
    pub fn kind(&self) -> Result<EndpointKind, EndpointError> {
        match self.endpoint_kind.as_deref() {
            None => Ok(EndpointKind::Auto),
            Some(name) => EndpointKind::parse(name),
        }
    }

    /// The refresh policy, defaulting to `auto` when the column is `NULL`.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::UnknownRefreshMode`] for an unrecognised value.
    pub fn refresh_mode(&self) -> Result<RefreshMode, EndpointError> {
        match self.model_refresh_mode.as_deref() {
            None => Ok(RefreshMode::Auto),
            Some(name) => RefreshMode::parse(name),
        }
    }
}

/// Fields a create request may set.
///
/// Separate from [`ModelEndpoint`] because the caller does not supply `id`,
/// `created_at` or `updated_at`, and because `api_key` arrives as plaintext.
#[derive(Debug, Clone, Default)]
pub struct NewEndpoint {
    /// Human label. Required and non-blank.
    pub name: String,
    /// Base URL. Required and non-blank.
    pub base_url: String,
    /// Plaintext credential; sealed on the way in.
    pub api_key: Option<String>,
    /// Defaults to enabled when `None`.
    pub is_enabled: Option<bool>,
    /// Models hidden from the picker.
    pub hidden_models: Option<ModelIdList>,
    /// Pre-seeded model cache. Normally `None`: discovery fills it.
    pub cached_models: Option<ModelIdList>,
    /// Operator-pinned models.
    pub pinned_models: Option<ModelIdList>,
    /// `"llm"` or `"image"`. Defaults to `llm`, as Python does.
    pub model_type: Option<String>,
    /// Endpoint kind. Defaults to `auto`.
    pub endpoint_kind: Option<EndpointKind>,
    /// Refresh policy. Defaults to `auto`.
    pub model_refresh_mode: Option<RefreshMode>,
    /// TTL seconds.
    pub model_refresh_interval: Option<i64>,
    /// Probe timeout seconds.
    pub model_refresh_timeout: Option<i64>,
    /// Tri-state tools support. `None` means unknown.
    pub supports_tools: Option<bool>,
    /// Create a **shared** endpoint, visible to every user.
    ///
    /// The owning column is derived from the authenticated [`Caller`], never from
    /// request data — an earlier revision of this type carried an `owner` field,
    /// which would have let a caller forge ownership of somebody else's endpoint.
    /// This is the one way to produce a `NULL`-owner row, and it is
    /// admin-only: `routes/model_routes.py:2157` gates the `shared` flag behind
    /// the endpoint-management privilege.
    pub share_with_all: bool,
    /// Provider-credential row id. Plain string, no foreign key.
    pub provider_auth_id: Option<String>,
}

/// A mutable subset of an endpoint's fields.
///
/// Every field is optional so an update touches only what it names. `api_key` is
/// `Option<Option<String>>`: `None` means "leave it alone", and `Some(None)` means
/// "clear it" — a distinction a plain `Option<String>` cannot express, and getting it
/// wrong silently deletes a user's credential on every unrelated edit.
#[derive(Debug, Clone, Default)]
pub struct EndpointPatch {
    /// New label.
    pub name: Option<String>,
    /// New base URL.
    pub base_url: Option<String>,
    /// New credential, or `Some(None)` to clear it.
    pub api_key: Option<Option<String>>,
    /// Enable or disable.
    pub is_enabled: Option<bool>,
    /// Replace the hidden list.
    pub hidden_models: Option<ModelIdList>,
    /// Replace the cached list.
    pub cached_models: Option<ModelIdList>,
    /// Replace the pinned list.
    pub pinned_models: Option<ModelIdList>,
    /// New model type.
    pub model_type: Option<Option<String>>,
    /// New endpoint kind.
    pub endpoint_kind: Option<EndpointKind>,
    /// New refresh policy.
    pub model_refresh_mode: Option<RefreshMode>,
    /// New TTL.
    pub model_refresh_interval: Option<Option<i64>>,
    /// New probe timeout.
    pub model_refresh_timeout: Option<Option<i64>>,
    /// New tri-state tools support.
    pub supports_tools: Option<Option<bool>>,
    /// New provider-credential row id.
    pub provider_auth_id: Option<Option<String>>,
}

impl EndpointPatch {
    /// True when the patch would change nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.base_url.is_none()
            && self.api_key.is_none()
            && self.is_enabled.is_none()
            && self.hidden_models.is_none()
            && self.cached_models.is_none()
            && self.pinned_models.is_none()
            && self.model_type.is_none()
            && self.endpoint_kind.is_none()
            && self.model_refresh_mode.is_none()
            && self.model_refresh_interval.is_none()
            && self.model_refresh_timeout.is_none()
            && self.supports_tools.is_none()
            && self.provider_auth_id.is_none()
    }
}

/// Who is making a request.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// The authenticated username, or `None` for anonymous.
    pub user: Option<String>,
    /// Whether the caller holds the endpoint-management privilege.
    pub is_admin: bool,
}

impl Caller {
    /// A signed-in non-admin.
    #[must_use]
    pub fn user(name: impl Into<String>) -> Self {
        Self {
            user: Some(name.into()),
            is_admin: false,
        }
    }

    /// A signed-in admin.
    #[must_use]
    pub fn admin(name: impl Into<String>) -> Self {
        Self {
            user: Some(name.into()),
            is_admin: true,
        }
    }

    /// The username, or an error when anonymous.
    fn require_user(&self) -> Result<&str, EndpointError> {
        self.user.as_deref().ok_or(EndpointError::Unauthenticated)
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Owner-scoped CRUD over `model_endpoints`.
///
/// Follows the `AuthStore` + `AuthOwners` shape: the store holds no resolver and
/// no HTTP state, and every method takes an explicit [`Caller`]. The
/// `odysseus-store` boundary (§8) forbids importing `axum`, so the privilege check
/// cannot live in a middleware — it has to be a value the caller constructs.
pub struct ModelEndpointStore<'c> {
    conn: &'c Connection,
    key: &'c Fernet,
}

/// Hand-written so `Debug` cannot print key material.
///
/// `Fernet` does not implement `Debug` itself, which is the correct default for a
/// key. Rather than derive nothing and leave the type opaque, this prints the type
/// name and nothing else.
impl fmt::Debug for ModelEndpointStore<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelEndpointStore").finish_non_exhaustive()
    }
}

impl<'c> ModelEndpointStore<'c> {
    /// Wraps a connection and the key `api_key` is sealed with.
    #[must_use]
    pub fn new(conn: &'c Connection, key: &'c Fernet) -> Self {
        Self { conn, key }
    }

    /// Every endpoint visible to `caller`: their own, plus all shared ones.
    ///
    /// The `owner IS NULL` arm is what keeps shared endpoints visible at all.
    /// Ordering is by name so a listing does not shuffle between calls.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Sqlite`] on a query failure.
    pub fn list(&self, caller: &Caller) -> Result<Vec<ModelEndpoint>, EndpointError> {
        let sql = "SELECT * FROM model_endpoints
                   WHERE owner = ?1 OR owner IS NULL
                   ORDER BY name, id";
        let user = caller.user.as_deref();
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![user], ModelEndpoint::from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One endpoint by id, if visible to `caller`.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Sqlite`] on a query failure.
    pub fn get(&self, caller: &Caller, id: &str) -> Result<Option<ModelEndpoint>, EndpointError> {
        let sql = "SELECT * FROM model_endpoints
                   WHERE id = ?1 AND (owner = ?2 OR owner IS NULL)";
        let mut stmt = self.conn.prepare(sql)?;
        Ok(stmt
            .query_row(params![id, caller.user.as_deref()], ModelEndpoint::from_row)
            .optional()?)
    }

    /// One endpoint by id with no visibility filter.
    ///
    /// For internal callers that have already established authorisation — the
    /// settings resolver, for instance. Prefer [`ModelEndpointStore::get`] for
    /// anything driven by a request.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Sqlite`] on a query failure.
    pub fn get_unscoped(&self, id: &str) -> Result<Option<ModelEndpoint>, EndpointError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM model_endpoints WHERE id = ?1")?;
        Ok(stmt
            .query_row(params![id], ModelEndpoint::from_row)
            .optional()?)
    }

    /// Creates an endpoint owned by the authenticated caller.
    ///
    /// `id` is generated as the first 8 characters of a `UUID4`, matching
    /// `routes/model_routes.py:2149`. The `owner` column comes from `caller`,
    /// never from `new`: a caller must not be able to name somebody else as the
    /// owner. `new.share_with_all` is the only way to produce a `NULL`-owner
    /// row, and only an admin may ask for one.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Validation`] when a non-admin asks for a shared
    /// endpoint. Ignoring the request would leave the caller believing they had
    /// shared something they had not.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Validation`] when `name` or `base_url` is blank,
    /// [`EndpointError::Unauthenticated`] for an anonymous caller, and
    /// [`EndpointError::Sqlite`] otherwise.
    pub fn create(
        &self,
        caller: &Caller,
        new: &NewEndpoint,
    ) -> Result<ModelEndpoint, EndpointError> {
        let name = new.name.trim();
        let base_url = new.base_url.trim();
        if name.is_empty() {
            return Err(EndpointError::Validation("name must not be blank".into()));
        }
        if base_url.is_empty() {
            return Err(EndpointError::Validation(
                "base_url must not be blank".into(),
            ));
        }
        let caller_user = caller.require_user()?;
        if new.share_with_all && !caller.is_admin {
            return Err(EndpointError::Validation(
                "only an admin may create a shared endpoint".into(),
            ));
        }
        // A `NULL` owner means shared (`database.py:189`). Python derives it from the
        // authenticated user with an explicit `shared=true` override; same here.
        let owner: Option<String> = if new.share_with_all {
            None
        } else {
            Some(caller_user.to_owned())
        };

        let id = short_id()?;
        let now = naive_utc_now();
        let sealed = ApiKey::seal(self.key, new.api_key.as_deref());

        self.conn.execute(
            "INSERT INTO model_endpoints
               (id, name, base_url, api_key, is_enabled, hidden_models, cached_models,
                pinned_models, model_type, endpoint_kind, model_refresh_mode,
                model_refresh_interval, model_refresh_timeout, supports_tools,
                owner, provider_auth_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)",
            params![
                id,
                name,
                base_url,
                sealed.as_stored(),
                new.is_enabled.unwrap_or(true),
                new.hidden_models.as_ref().and_then(ModelIdList::as_raw),
                new.cached_models.as_ref().and_then(ModelIdList::as_raw),
                new.pinned_models.as_ref().and_then(ModelIdList::as_raw),
                new.model_type.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("llm"),
                new.endpoint_kind.unwrap_or_default().as_str(),
                new.model_refresh_mode.unwrap_or_default().as_str(),
                new.model_refresh_interval,
                new.model_refresh_timeout,
                new.supports_tools,
                owner,
                new.provider_auth_id.as_deref(),
                now,
            ],
        )?;

        self.get_unscoped(&id)?
            .ok_or_else(|| EndpointError::NotFound(id.clone()))
    }

    /// Applies a patch.
    ///
    /// Ownership is re-checked on write, so a patch naming a `NULL`-owner row
    /// finds nothing rather than editing a shared endpoint. Admins may patch any
    /// row. `updated_at` always moves, even for an empty patch, so "touched" is
    /// observable.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::NotFound`] when the id is absent or not the
    /// caller's, and [`EndpointError::Sqlite`] on a query failure.
    pub fn update(
        &self,
        caller: &Caller,
        id: &str,
        patch: &EndpointPatch,
    ) -> Result<ModelEndpoint, EndpointError> {
        let existing = self.writable_row(caller, id)?;
        let now = naive_utc_now();

        // `api_key` is re-sealed from whatever is currently stored, so a patch
        // that does not mention it leaves it byte-identical and a patch that
        // mentions it encrypts exactly once.
        let sealed = match patch.api_key.as_ref() {
            Some(Some(plaintext)) => ApiKey::seal(self.key, Some(plaintext.as_str())),
            Some(None) => ApiKey::absent(),
            None => existing.api_key.clone().reseal(self.key),
        };
        let name = patch
            .name
            .as_deref()
            .map_or(existing.name.as_ref(), str::trim);
        if name.is_empty() {
            return Err(EndpointError::Validation("name must not be blank".into()));
        }
        let base_url = patch
            .base_url
            .as_deref()
            .map_or(existing.base_url.as_ref(), str::trim);
        if base_url.is_empty() {
            return Err(EndpointError::Validation(
                "base_url must not be blank".into(),
            ));
        }

        self.conn.execute(
            "UPDATE model_endpoints SET
               name = ?2, base_url = ?3, api_key = ?4, is_enabled = ?5,
               hidden_models = ?6, cached_models = ?7, pinned_models = ?8,
               model_type = ?9, endpoint_kind = ?10, model_refresh_mode = ?11,
               model_refresh_interval = ?12, model_refresh_timeout = ?13,
               supports_tools = ?14, provider_auth_id = ?15, updated_at = ?16
             WHERE id = ?1",
            params![
                id,
                name,
                base_url,
                sealed.as_stored(),
                patch.is_enabled.unwrap_or(existing.is_enabled),
                pick(patch.hidden_models.as_ref(), &existing.hidden_models),
                pick(patch.cached_models.as_ref(), &existing.cached_models),
                pick(patch.pinned_models.as_ref(), &existing.pinned_models),
                // `Some(None)` clears the column; `None` leaves it alone. Written
                // as an explicit match rather than `.flatten().or(existing)`, which
                // would silently turn a clear into a no-op: `flatten()` maps
                // `Some(None)` to `None`, and `.or(existing)` then substitutes the
                // old value back.
                replaceable(
                    patch.model_type.as_ref().map(Option::as_deref),
                    existing.model_type.as_deref(),
                ),
                patch
                    .endpoint_kind
                    .map_or(existing.endpoint_kind.as_deref(), |k| Some(k.as_str())),
                patch
                    .model_refresh_mode
                    .map_or(existing.model_refresh_mode.as_deref(), |m| Some(m.as_str())),
                patch
                    .model_refresh_interval
                    .unwrap_or(existing.model_refresh_interval),
                patch
                    .model_refresh_timeout
                    .unwrap_or(existing.model_refresh_timeout),
                patch.supports_tools.unwrap_or(existing.supports_tools),
                replaceable(
                    patch.provider_auth_id.as_ref().map(Option::as_deref),
                    existing.provider_auth_id.as_deref(),
                ),
                now,
            ],
        )?;

        self.get_unscoped(id)?
            .ok_or_else(|| EndpointError::NotFound(id.to_owned()))
    }

    /// Deletes an endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::NotFound`] when the id is absent or not the
    /// caller's, and [`EndpointError::Sqlite`] on a query failure.
    pub fn delete(&self, caller: &Caller, id: &str) -> Result<bool, EndpointError> {
        self.writable_row(caller, id)?;
        let affected = self
            .conn
            .execute("DELETE FROM model_endpoints WHERE id = ?1", params![id])?;
        Ok(affected > 0)
    }

    /// Replaces the cached model list, without touching `updated_at`.
    ///
    /// A discovery refresh is not a configuration change, and Python refreshes on
    /// a timer; bumping `updated_at` would make every background poll look like an
    /// edit in the UI.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::NotFound`] when the id is absent, and
    /// [`EndpointError::Sqlite`] on a query failure.
    pub fn store_cached_models(
        &self,
        id: &str,
        models: &ModelIdList,
    ) -> Result<bool, EndpointError> {
        let affected = self.conn.execute(
            "UPDATE model_endpoints SET cached_models = ?2 WHERE id = ?1",
            params![id, models.as_raw()],
        )?;
        Ok(affected > 0)
    }

    /// Fetches the row a write is allowed to touch.
    ///
    /// Admin bypasses the owner test; everyone else must own the row exactly, so a
    /// shared endpoint is invisible to a write.
    fn writable_row(&self, caller: &Caller, id: &str) -> Result<ModelEndpoint, EndpointError> {
        let found = if caller.is_admin {
            self.get_unscoped(id)?
        } else {
            let mut stmt = self
                .conn
                .prepare("SELECT * FROM model_endpoints WHERE id = ?1 AND owner = ?2")?;
            stmt.query_row(params![id, caller.user.as_deref()], ModelEndpoint::from_row)
                .optional()?
        };
        found.ok_or_else(|| EndpointError::NotFound(id.to_owned()))
    }
}

/// A `NOT NULL` TEXT column becomes a `Box<str>`.
///
/// rusqlite has no `FromSql` for `&str`, so the row hands over a `String` that is
/// immediately shrunk. `String::into_boxed_str` reallocates only when the
/// capacity exceeds the length, which for a freshly-read value it does not.
fn owned(value: String) -> Box<str> {
    value.into_boxed_str()
}

/// A nullable TEXT column becomes an `Option<Box<str>>`.
fn optional(value: Option<String>) -> Option<Box<str>> {
    value.map(String::into_boxed_str)
}

/// The stored value when patched, the existing one otherwise.
fn pick<'a>(patched: Option<&'a ModelIdList>, existing: &'a ModelIdList) -> Option<&'a str> {
    patched.unwrap_or(existing).as_raw()
}

/// A nullable TEXT column under patch semantics: `Some(value)` sets it,
/// `Some(None)` clears it, and `None` leaves it as it was.
///
/// The three-way case is why the column is `Option<Option<Box<str>>>` rather than
/// `Option<Box<str>>`: with only two states, "clear this column" and "leave this
/// column alone" are indistinguishable, and a PATCH that omits a field must not
/// wipe it.
#[expect(
    clippy::option_option,
    reason = "the lint's suggestion (a bare Option<&str>, or a bespoke tri-state enum) \
              cannot express 'clear' vs 'leave alone', which is the whole point. \
              EndpointPatch already exposes these fields as Option<Option<_>>."
)]
fn replaceable<'a>(patched: Option<Option<&'a str>>, existing: Option<&'a str>) -> Option<&'a str> {
    match patched {
        Some(value) => value,
        None => existing,
    }
}

/// A short row id: the first 8 characters of a `UUID4`, as Python does.
///
/// 8 hex characters is 32 bits of space. That is Python's choice and this port
/// keeps it for compatibility with an existing `settings.json`
/// `default_endpoint_id`; it is noted as tight rather than adopted as a
/// recommendation. A collision is not a correctness problem here because `id` is
/// the primary key and an insert with an existing id fails loudly rather than
/// overwriting.
///
/// # Errors
///
/// Returns [`EndpointError::NoEntropy`] when the OS random source is unavailable.
/// This fails closed rather than substituting a timestamp: a predictable id is
/// guessable by anyone who can read a row, and an id collision would surface as a
/// confusing primary-key violation on an unrelated insert. Refusing to mint the id
/// names the actual problem.
fn short_id() -> Result<String, EndpointError> {
    use std::fmt::Write as _;

    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).map_err(|_| EndpointError::NoEntropy)?;
    Ok(bytes.iter().fold(String::with_capacity(8), |mut acc, b| {
        // Writing into a pre-sized String cannot fail.
        let _ = write!(acc, "{b:02x}");
        acc
    }))
}

/// Naive-UTC timestamp in the format the Python `DateTime` columns hold.
///
/// `utcnow_naive()` (`core/database.py:23`) strips tzinfo and `SQLAlchemy` writes
/// `YYYY-MM-DD HH:MM:SS.ffffff`. This produces the same shape so rows written
/// here and rows written by Python sort and compare identically.
///
/// Provisional: TASKS **T0047** owns naive-UTC conversion properly. This exists
/// because `created_at`/`updated_at` are `NOT NULL` and something has to supply
/// them, and duplicating the column type as text is the least invasive option.
/// Replace with T0047's helpers rather than growing this.
fn naive_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_micros())
        .unwrap_or_default();
    let days = i64::try_from(secs / 86_400).unwrap_or_default();
    let tod = secs % 86_400;
    let (y, mo, d) = civil_from_days(days);
    format!(
        "{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02}.{micros:06}",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's civil-from-days. Avoids a date dependency.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// The HTTP surface discovery needs.
///
/// Injected rather than linked, for three reasons: `odysseus-store` must not grow
/// an HTTP client (§8 — it is allowed `rusqlite` and nothing else), a test must
/// not sleep (§11), and the timeout becomes the transport's business rather than
/// a `tokio::time::sleep` nobody can assert on.
pub trait ModelTransport {
    /// Performs one GET and returns the status and body.
    ///
    /// `timeout` is a bound the implementation must honour. Returning
    /// [`TransportError::Timeout`] is how it says it gave up.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError`] for any failure, including a timeout.
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<ProbeResponse, TransportError>;
}

/// What a transport returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body.
    pub body: String,
}

impl ProbeResponse {
    /// A 200 with a body.
    #[must_use]
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
        }
    }
}

/// Why a probe failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The transport gave up at the timeout.
    Timeout,
    /// Anything else: DNS, refused connection, TLS, malformed response.
    Failed(String),
}

/// What a discovery attempt found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discovery {
    /// The provider exposes no model list; probing one is meaningless.
    Unsupported(Provider),
    /// Reachable, with this many models.
    Models(Box<[Box<str>]>),
    /// Reachable, but the body was not a model list this build understands.
    Unparseable(String),
    /// The transport failed.
    Failed(String),
}

/// The URL a provider's model list lives at.
///
/// Ports `build_models_url` (`src/endpoint_resolver.py:286`). The important
/// behaviour is that a base with **no path** gets `/v1` inserted (issue #25) while
/// an explicit non-empty path is left alone, so a `/openai` prefix keeps working.
#[must_use]
pub fn models_url(base_url: &str, provider: Provider) -> Option<String> {
    if provider.is_discovery_only() {
        return None;
    }
    let base = base_url.trim().trim_end_matches('/');
    // Ollama's native surface lists models at /api/tags, not /v1/models.
    if provider == Provider::Ollama {
        return Some(format!("{base}/api/tags"));
    }
    let parts = UrlParts::parse(base);
    if parts.path.is_empty() || parts.path == "/" {
        Some(format!("{base}/v1/models"))
    } else {
        Some(format!("{base}/models"))
    }
}

/// Auth headers for a probe.
///
/// Mirrors `_safe_build_headers`: a bearer token when a key is present, nothing
/// when it is not. Ollama needs no header, and sending an empty `Authorization`
/// to a local server is worse than sending none.
#[must_use]
pub fn probe_headers(api_key: Option<&str>) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
        headers.insert("Authorization".to_owned(), format!("Bearer {key}"));
    }
    headers
}

/// Probes an endpoint for its model list.
///
/// Blocking and synchronous, so it belongs behind `crate::blocking` like every
/// other I/O in this crate (§11 / D11). No `sleep` appears anywhere: the timeout
/// is passed to the transport, which is the only thing that may wait.
///
/// # Errors
///
/// Returns [`EndpointError`] only for an unusable configuration — a blank base
/// URL or an endpoint whose provider has no model list. A *probe* failure is a
/// [`Discovery::Failed`] value, because "the server is down" is a reportable
/// result, not a programming error.
pub fn discover_models(
    transport: &dyn ModelTransport,
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
) -> Result<Discovery, EndpointError> {
    let base = base_url.trim();
    if base.is_empty() {
        return Err(EndpointError::Validation(
            "base_url must not be blank".into(),
        ));
    }
    let provider = Provider::detect(base);
    let Some(url) = models_url(base, provider) else {
        return Ok(Discovery::Unsupported(provider));
    };

    let response = match transport.get(&url, &probe_headers(api_key), timeout) {
        Ok(r) => r,
        Err(TransportError::Timeout) => return Ok(Discovery::Failed("timeout".to_owned())),
        Err(TransportError::Failed(message)) => return Ok(Discovery::Failed(message)),
    };

    if !(200..300).contains(&response.status) {
        return Ok(Discovery::Failed(format!("status {}", response.status)));
    }
    Ok(match parse_model_ids(&response.body) {
        Some(ids) => Discovery::Models(ids),
        None => Discovery::Unparseable(truncate(&response.body, 200)),
    })
}

/// Pulls model ids out of a provider response.
///
/// Accepts the `OpenAI` shape (`{"data":[{"id":…}]}`) and Ollama's native shape
/// (`{"models":[{"name":…}]}`), which is what Python's probes have to handle.
#[must_use]
pub fn parse_model_ids(body: &str) -> Option<Box<[Box<str>]>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    for key in ["data", "models"] {
        let Some(items) = value.get(key).and_then(serde_json::Value::as_array) else {
            continue;
        };
        let ids: Box<[Box<str>]> = items
            .iter()
            .filter_map(|item| {
                item.get("id")
                    .or_else(|| item.get("name"))
                    .or_else(|| item.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .map(Box::<str>::from)
            })
            .collect();
        return Some(ids);
    }
    None
}

/// Keeps an error message from carrying a whole HTML page into a log.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything that can go wrong in this module.
///
/// Deliberately its own type rather than a variant on [`crate::StoreError`]:
/// `StoreError` has no free-form arm, and bolting endpoint failures onto
/// `StoreError::Path` or `Auth` would misreport what went wrong. Mapping this to
/// an HTTP status is `odysseus-web`'s job (§5).
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    /// The SQLite query failed.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// `name` or `base_url` was blank.
    #[error("invalid endpoint: {0}")]
    Validation(String),

    /// The caller has no username.
    #[error("authentication required")]
    Unauthenticated,

    /// No such row, or not the caller's to touch.
    ///
    /// One variant for both cases on purpose: telling a caller "exists but is not
    /// yours" is a row-existence oracle.
    #[error("endpoint not found: {0}")]
    NotFound(String),

    /// Unrecognised provider name.
    #[error("unknown provider: {0}")]
    UnknownProvider(String),

    /// Unrecognised endpoint kind.
    #[error("unknown endpoint kind: {0}")]
    UnknownEndpointKind(String),

    /// Unrecognised refresh mode.
    #[error("unknown refresh mode: {0}")]
    UnknownRefreshMode(String),

    /// The stored credential could not be decrypted.
    #[error("credential error: {0}")]
    Crypto(String),

    /// The OS random source was unavailable, so no row id could be minted.
    ///
    /// Deliberately not a silent fallback: see [`short_id`].
    #[error("no entropy available to generate an endpoint id")]
    NoEntropy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parts_splits_the_shapes_provider_detection_needs() {
        let p = UrlParts::parse("https://API.Anthropic.com:443/v1/chat?x=1#f");
        assert_eq!(p.scheme, "https");
        assert_eq!(p.host, "api.anthropic.com");
        assert_eq!(p.port, Some(443));
        assert_eq!(p.path, "/v1/chat");

        let p = UrlParts::parse("http://[::1]:11434/api/tags");
        assert_eq!(p.host, "::1");
        assert_eq!(p.port, Some(11434));
        assert_eq!(p.path, "/api/tags");

        let p = UrlParts::parse("http://user:pw@host.example/base/");
        assert_eq!(
            p.host, "host.example",
            "userinfo must not leak into the host"
        );

        let p = UrlParts::parse("not a url at all");
        assert_eq!(p.host, "");
        assert_eq!(p.port, None);
    }

    #[test]
    fn a_trailing_dot_on_the_host_still_matches() {
        assert!(host_matches(
            "https://api.anthropic.com./v1",
            "anthropic.com"
        ));
    }

    #[test]
    fn models_url_inserts_v1_only_for_a_bare_host() {
        assert_eq!(
            models_url("http://localhost:1234", Provider::OpenAi).as_deref(),
            Some("http://localhost:1234/v1/models")
        );
        assert_eq!(
            models_url("http://localhost:1234/openai", Provider::OpenAi).as_deref(),
            Some("http://localhost:1234/openai/models"),
            "an explicit path must keep its semantics"
        );
        assert_eq!(
            models_url("http://localhost:11434", Provider::Ollama).as_deref(),
            Some("http://localhost:11434/api/tags")
        );
        assert_eq!(models_url("http://x", Provider::ChatGptSubscription), None);
    }

    #[test]
    fn model_id_list_round_trips_through_its_stored_text() {
        let list = ModelIdList::from_ids(["b", "a", "b", "  ", "c"]);
        assert_eq!(
            list.ids().iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
        assert!(list.contains("a"));
        assert!(!list.contains("zzz"));
        assert_eq!(ModelIdList::empty().as_raw(), None, "empty is NULL, not []");
        assert_eq!(
            ModelIdList::from_ids(["only"]).as_raw(),
            Some(r#"["only"]"#)
        );
    }

    #[test]
    fn a_malformed_stored_list_yields_empty_rather_than_failing() {
        let corrupt = ModelIdList::from_raw(Some("{not json"));
        assert!(corrupt.is_empty());
        assert!(!corrupt.contains("anything"));
    }

    #[test]
    fn parse_model_ids_accepts_both_provider_shapes() {
        // Compared through `map` rather than unwrapped, so the assertion states
        // the shape directly and needs no `expect`.
        assert_eq!(
            parse_model_ids(r#"{"data":[{"id":"llama3"},{"id":"qwen"}]}"#).map(|ids| ids.len()),
            Some(2)
        );
        assert_eq!(
            parse_model_ids(r#"{"models":[{"name":"llama3:8b"}]}"#).map(|ids| ids[0].to_string()),
            Some("llama3:8b".to_owned())
        );
        assert!(parse_model_ids("not json").is_none());
        assert!(parse_model_ids(r#"{"other":[]}"#).is_none());
    }

    #[test]
    fn probe_headers_omit_an_empty_key() {
        assert!(probe_headers(None).is_empty());
        assert!(probe_headers(Some("   ")).is_empty());
        assert_eq!(
            probe_headers(Some("k"))
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer k")
        );
    }
}
