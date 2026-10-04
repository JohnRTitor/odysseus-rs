//! Endpoint URL resolution and normalisation — one implementation.
//!
//! Ports `src/endpoint_resolver.py`, whose own docstring states its purpose:
//! *"Consolidates the 4+ copies of `normalize_base` / `resolve_endpoint` logic into
//! one place."* That consolidation is the whole point of T0306, so this module is
//! deliberately the **only** place in the workspace that answers "what URL does
//! this endpoint's base URL become for a chat request / a model list".
//!
//! ## Why the shape is what it is
//!
//! Python threads the same two steps through every entry point, and the steps
//! compose:
//!
//! ```text
//! resolve_url            # optional: Tailscale fallback when DNS fails
//!   -> normalize_base    # strip a known API path suffix (idempotent)
//!     -> append_path     # add exactly one segment, never double-slash
//! ```
//!
//! Three separate functions rather than one `build_everything(url)` because each
//! step is independently meaningful: `normalize_base` is what makes an operator
//! pasting a full `/v1/chat/completions` URL work, and `append_path` is what keeps
//! a `/openai` prefix intact. Folding them together would make it impossible to
//! normalise without also appending, which is precisely the duplication being
//! removed.
//!
//! The rule that keeps this from growing a fourth branch: **every provider
//! difference is a `(root, suffix)` pair chosen from one table.** `build_chat_url`
//! and `build_models_url` each ask that table for their own pair and then call the
//! same `append_path`. Adding a provider means adding a row, not adding an `if` to
//! two functions. If you find yourself writing an `if` in `build_*_url`, the
//! provider is missing from the table — add the row instead.
//!
//! ## Departures from Python, and why
//!
//! * **Tailscale resolution is not performed here.** `resolve_url` shells out to
//!   `tailscale status --json` and rewrites the host to an IP. That is process
//!   spawning and DNS, neither of which belongs in a store crate, and it is a
//!   network effect that must be injectable to be testable (`AGENTS.md` §11). The
//!   seam is [`ResolvedBase::apply_host_override`]: a caller that has an override
//!   passes it in, and normalisation proceeds identically. Nothing is lost, and a
//!   unit test never needs a `tailscale` binary.
//! * **Query/fragment rejection is a typed error, not an exception.** Python raises
//!   `ValueError` from `_validated_endpoint_base`; here it is
//!   [`ResolveError::QueryOrFragment`], because `odysseus-web` has to map it to a
//!   status code and cannot match on a message string (§5).

use std::fmt;

use crate::model_endpoints::{Provider, host_matches};

/// Suffixes `normalize_base` strips, in Python's order.
///
/// Order is load-bearing only in that stripping is repeated until nothing matches,
/// so a base like `…/v1/models` loses `/models` and is then re-checked. The set is
/// the union of what OpenAI-compatible, Anthropic and Ollama clients append.
const BASE_SUFFIXES: &[&str] = &[
    "/models",
    "/chat/completions",
    "/completions",
    "/v1/messages",
    "/responses",
];

/// Suffixes stripped only when they follow an `/api` segment.
///
/// Ollama's native surface hangs off `/api`, and `…/api/tags` must normalise to the
/// same root as `…/api/chat` rather than to `…/api`.
const API_SUFFIXES: &[&str] = &["/chat", "/tags", "/generate"];

/// Hosts for which a bare base is known to serve models at `/v1/models`.
///
/// Python's list is `{localhost, 127.0.0.1, ::1, host.docker.internal}` plus
/// `api.deepseek.com` and `api.openai.com`. A **remote unknown host is not here**,
/// and that is the point: inventing `/v1` for a host we know nothing about breaks
/// providers that serve `/models` directly, so unknown remote hosts get `/models`
/// appended to whatever the caller configured.
const V1_MODELS_BY_DEFAULT_HOSTS: &[&str] = &[
    "localhost",
    "127.0.0.1",
    "::1",
    "host.docker.internal",
    "api.deepseek.com",
    "api.openai.com",
];

/// The `(root, suffix)` a provider serves a given operation from.
///
/// One row per provider per operation is the entire provider-dispatch mechanism;
/// see the module docs. `None` for the root means "use the normalised base
/// unchanged".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Route {
    root: Root,
    suffix: &'static str,
}

/// How a provider's API root is derived from the normalised base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// The normalised base, unchanged.
    Base,
    /// Anthropic's root: a base ending in `/v1` loses it, so `/v1/messages` does
    /// not become `/v1/v1/messages`.
    AnthropicApi,
    /// Ollama's native root: a bare host gains `/api`.
    OllamaApi,
}

impl Root {
    fn apply(self, base: &str) -> String {
        match self {
            Self::Base => base.to_owned(),
            Self::AnthropicApi => anthropic_api_root(base),
            Self::OllamaApi => ollama_api_root(base),
        }
    }
}

/// The chat route for every provider.
///
/// Total by design: every provider speaks *something*, so there is no `None` arm to
/// reach and no branch in [`chat_url`] to handle a gap. Adding a provider means
/// adding a row here, and the compiler then names every place that must decide.
fn chat_route(provider: Provider) -> Route {
    match provider {
        Provider::Anthropic => Route {
            root: Root::AnthropicApi,
            suffix: "/v1/messages",
        },
        Provider::Ollama => Route {
            root: Root::OllamaApi,
            suffix: "/chat",
        },
        Provider::ChatGptSubscription => Route {
            root: Root::Base,
            suffix: "/responses",
        },
        // Everything else is OpenAI-compatible, including the discovery-only
        // Copilot variant, which speaks the same `/chat/completions` shape.
        Provider::OpenAi
        | Provider::OpenRouter
        | Provider::Groq
        | Provider::Nvidia
        | Provider::Moonshot
        | Provider::Cerebras
        | Provider::Mistral
        | Provider::OpenCodeGo
        | Provider::OpenCodeZen
        | Provider::Copilot => Route {
            root: Root::Base,
            suffix: "/chat/completions",
        },
    }
}

/// The model-list route for each provider, or `None` when the provider has no
/// model-list endpoint.
fn models_route(provider: Provider) -> Option<Route> {
    Some(match provider {
        Provider::Anthropic => Route {
            root: Root::AnthropicApi,
            suffix: "/v1/models",
        },
        Provider::Ollama => Route {
            root: Root::OllamaApi,
            suffix: "/tags",
        },
        // No model list to fetch; discovery reports "unsupported" rather than an
        // empty list that reads like an outage.
        Provider::ChatGptSubscription => return None,
        Provider::OpenAi
        | Provider::OpenRouter
        | Provider::Groq
        | Provider::Nvidia
        | Provider::Moonshot
        | Provider::Cerebras
        | Provider::Mistral
        | Provider::OpenCodeGo
        | Provider::OpenCodeZen
        | Provider::Copilot => Route {
            root: Root::Base,
            suffix: "/models",
        },
    })
}

/// Why a base URL could not be normalised.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// The base carried a query string or a fragment.
    ///
    /// Python raises `ValueError("Endpoint base URL must not include query or
    /// fragment")`. It matters because a caller that configured
    /// `http://host/v1?key=secret` would otherwise have that query carried into
    /// every appended path, and `append_path` would place a path *after* the
    /// query — producing a URL whose effective path is not the one requested.
    #[error("endpoint base URL must not include a query string or fragment")]
    QueryOrFragment,
}

/// A validated, normalised base URL, ready for [`append_path`].
///
/// Constructing one is the only way to get a base out of this module, so a caller
/// cannot skip validation and then append to a URL that still carries a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBase {
    url: String,
}

impl ResolvedBase {
    /// Validates and normalises `base`.
    ///
    /// Equivalent to Python's `_prepare_endpoint_base` minus the Tailscale step:
    /// strip a known suffix, reject a query or fragment, strip them if somehow
    /// present, and normalise again so the Tailscale-rewritten form converges on
    /// the same result.
    ///
    /// # Errors
    ///
    /// Returns [`ResolveError::QueryOrFragment`] if `base` carries either.
    pub fn prepare(base: &str) -> Result<Self, ResolveError> {
        let normalized = normalize_base(base);
        let validated = Self::validate(&normalized)?;
        // Python normalises a second time here because `resolve_url` may have
        // rewritten the authority, and the rewrite is not guaranteed to preserve a
        // normalised path. Normalising twice is idempotent, so without an override
        // this is a no-op; [`Self::apply_host_override`] is the case it exists for.
        Self::validate(&normalize_base(&validated.url))
    }

    /// Validates without normalising.
    fn validate(base: &str) -> Result<Self, ResolveError> {
        let trimmed = base.trim().trim_end_matches('/');
        if trimmed.contains('?') || trimmed.contains('#') {
            return Err(ResolveError::QueryOrFragment);
        }
        Ok(Self {
            url: trimmed.to_owned(),
        })
    }

    /// Rewrites the authority, keeping scheme, port and path.
    ///
    /// This is the seam for `resolve_url`'s Tailscale fallback. `host` is expected
    /// to be a bare address; a port present on `self` is preserved, matching
    /// Python's `netloc = f"{ip}:{parsed.port}"`.
    #[must_use]
    pub fn apply_host_override(&self, host: &str) -> Self {
        let parts = crate::model_endpoints::UrlParts::parse(&self.url);
        let authority = match parts.port {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        let scheme = if parts.scheme.is_empty() {
            "http".to_owned()
        } else {
            parts.scheme.clone()
        };
        Self {
            url: format!("{scheme}://{authority}{}", parts.path),
        }
    }

    /// The normalised URL text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.url
    }
}

/// Strips known API path suffixes from a base URL.
///
/// Ports `normalize_base` (`src/endpoint_resolver.py:225`). Applied repeatedly,
/// because stripping `/models` from `…/v1/models` can expose a suffix that was
/// previously hidden — `…/api/tags` becomes `…/api`, then stops.
///
/// Idempotent: `normalize_base(normalize_base(x)) == normalize_base(x)` for every
/// input, which is what makes calling it on both sides of a host rewrite safe.
#[must_use]
pub fn normalize_base(url: &str) -> String {
    let mut current = url.trim().trim_end_matches('/').to_owned();
    loop {
        let before = current.len();
        for suffix in BASE_SUFFIXES {
            if current.ends_with(suffix) {
                let keep = current.len() - suffix.len();
                truncate_path(&mut current, keep);
            }
        }
        for suffix in API_SUFFIXES {
            let api_suffix = format!("/api{suffix}");
            if current.ends_with(&api_suffix) {
                // Python cuts `len(suffix)`, not `len("/api" + suffix)`, so the
                // `/api` segment survives: `.../api/tags` normalises to `.../api`,
                // which is the root both `/api/chat` and `/api/generate` are built
                // from. Cutting the whole `/api/tags` would lose it.
                let keep = current.len() - suffix.len();
                truncate_path(&mut current, keep);
            }
        }
        if current.len() == before {
            return current;
        }
    }
}

/// Cuts `url` to `keep` bytes and then strips any trailing slashes, in place.
///
/// Python's `url[:-n].rstrip("/")` allocates a fresh string per suffix. Since the
/// truncation point is always a suffix of the current value, this can shrink the
/// existing buffer instead — no allocation, and it keeps the loop idempotent without
/// re-allocating on every pass.
fn truncate_path(url: &mut String, keep: usize) {
    url.truncate(keep);
    let trimmed = url.trim_end_matches('/').len();
    url.truncate(trimmed);
}

/// Appends exactly one path segment to a base URL.
///
/// Ports `_append_endpoint_path` (`src/endpoint_resolver.py:250`). Existing
/// trailing slashes are collapsed first so `http://h/v1/` + `/models` is
/// `http://h/v1/models` and never `http://h/v1//models`, and query/fragment are
/// dropped because a base that reached here has already been validated to have
/// neither.
#[must_use]
pub fn append_path(base: &str, suffix: &str) -> String {
    let parts = crate::model_endpoints::UrlParts::parse(base);
    let current = parts.path.trim_end_matches('/');
    let extra = format!("/{}", suffix.trim_start_matches('/'));
    let path = if current.is_empty() {
        extra
    } else {
        format!("{current}{extra}")
    };
    join_url(&parts.scheme, &authority_of(base), &path)
}

/// Anthropic's API root for `base`.
///
/// Ports `_anthropic_api_root` (`src/endpoint_resolver.py:263`). A base already
/// ending in `/v1` loses it so that appending `/v1/messages` cannot produce
/// `/v1/v1/messages` — but **only** for Anthropic, because an OpenAI-compatible
/// server configured at `…/openai/v1` genuinely wants that `/v1` kept.
#[must_use]
pub fn anthropic_api_root(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    if host_matches(base, "anthropic.com") && base.ends_with("/v1") {
        return base
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_owned();
    }
    base.to_owned()
}

/// Ollama's native API root for `base`.
///
/// Ports `_ollama_api_root` (`src/llm_core.py:596`): the four native endpoints are
/// peeled back to their shared root, a bare host gains `/api`, and `ollama.com` is
/// forced to its hosted root.
#[must_use]
pub fn ollama_api_root(base: &str) -> String {
    let base = base.trim().trim_end_matches('/');
    let parts = crate::model_endpoints::UrlParts::parse(base);
    let path = parts.path.trim_end_matches('/');
    for suffix in ["/chat", "/tags", "/generate"] {
        let native = format!("/api{suffix}");
        if path.ends_with(&native) {
            let cut = base.len() - suffix.len();
            return base[..cut].trim_end_matches('/').to_owned();
        }
    }
    if path.ends_with("/api") {
        return base.to_owned();
    }
    if path.is_empty() {
        return format!("{base}/api");
    }
    if host_matches(base, "ollama.com") {
        return format!("{}://{}/api", parts.scheme, authority_of(base));
    }
    base.to_owned()
}

/// The chat URL for `base`.
///
/// Ports `build_chat_url` (`src/endpoint_resolver.py:271`).
///
/// # Errors
///
/// Propagates [`ResolvedBase::prepare`], so a base carrying a query or fragment is
/// rejected rather than silently producing a URL whose path is not the one asked
/// for.
pub fn chat_url(base: &str) -> Result<String, ResolveError> {
    let prepared = ResolvedBase::prepare(base)?;
    let provider = Provider::detect(prepared.as_str());
    let route = chat_route(provider);
    let root = route.root.apply(prepared.as_str());
    Ok(append_path(&root, route.suffix))
}

/// The model-list URL for `base`, or `None` when the provider has none.
///
/// Ports `build_models_url` (`src/endpoint_resolver.py:286`).
///
/// The subtlety is the `/v1` insertion: it happens **only** for a base with no path
/// whose host is known to serve `/v1/models` (issue #25 — a bare `http://localhost:1234`
/// has to reach `/v1/models`). An explicit path is never rewritten, so a custom
/// `/openai` prefix keeps its meaning, and an unknown *remote* host does not get a
/// `/v1` invented for it.
///
/// # Errors
///
/// Propagates [`ResolvedBase::prepare`].
pub fn models_url(base: &str) -> Result<Option<String>, ResolveError> {
    let prepared = ResolvedBase::prepare(base)?;
    let provider = Provider::detect(prepared.as_str());
    let Some(route) = models_route(provider) else {
        return Ok(None);
    };
    let root = route.root.apply(prepared.as_str());
    let parts = crate::model_endpoints::UrlParts::parse(&root);
    let path = parts.path.trim_end_matches('/');
    let uses_v1_by_default = V1_MODELS_BY_DEFAULT_HOSTS.contains(&parts.host.as_str());
    let base = if path.is_empty() && uses_v1_by_default {
        append_path(&root, "/v1")
    } else {
        root
    };
    Ok(Some(append_path(&base, route.suffix)))
}

/// The authority (`host[:port]`) of `base`, userinfo excluded.
fn authority_of(base: &str) -> String {
    let parts = crate::model_endpoints::UrlParts::parse(base);
    match parts.port {
        Some(port) => format!("{}:{port}", parts.host),
        None => parts.host.clone(),
    }
}

/// Reassembles a URL from its parts, omitting an empty scheme.
fn join_url(scheme: &str, authority: &str, path: &str) -> String {
    if scheme.is_empty() {
        format!("//{authority}{path}")
    } else {
        format!("{scheme}://{authority}{path}")
    }
}

/// The model families a model id is known to belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    /// A chat or text-generation model.
    Chat,
    /// Not a chat model.
    NonChat,
    /// The provider said nothing either way.
    Unknown,
}

/// Substrings marking a model id as **not** a chat model.
///
/// Ports `_NON_CHAT_MODEL` (`src/endpoint_resolver.py:25`) exactly, including the
/// two entries the task description omits (`embedding` and `reranker`). Removing
/// them would make `first_chat_model` auto-pick `text-embedding-3-large`, which is
/// the bug the function exists to prevent.
///
/// **Matching is substring containment, case-insensitive — not prefix.** The task
/// summary calls these "prefixes"; Python uses `p in m.lower()`. The difference is
/// load-bearing in both directions and is pinned by tests: `embedding` (a bare
/// entry, no hyphen) only matches as a substring, while a prefix reading would miss
/// `x/embedding-3`. Conversely substring matching is the *looser* of the two, so
/// it can also over-match — see [`first_chat_model`]'s docs on the fallback.
pub const NON_CHAT_MARKERS: &[&str] = &[
    "text-embedding",
    "embedding",
    "tts-",
    "whisper",
    "dall-e",
    "moderation",
    "rerank",
    "reranker",
    "clip",
    "stable-diffusion",
];

/// The family `model_id` belongs to, by name.
#[must_use]
pub fn model_family(model_id: &str) -> ModelFamily {
    let lowered = model_id.to_lowercase();
    if NON_CHAT_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
    {
        ModelFamily::NonChat
    } else {
        ModelFamily::Chat
    }
}

/// The first chat model in `models`, or `None` when `models` is empty.
///
/// Ports `_first_chat_model` (`src/endpoint_resolver.py:68`).
///
/// **The fallback is not a filter.** When every id is non-chat, Python returns
/// `models[0]`, not `None` — see `test_first_chat_model_falls_back_to_first_when_all_non_chat`.
/// So this returns a *best-effort* chat pick: a caller with a single
/// `text-embedding-3-large` and nothing else gets that id back rather than no
/// answer, and the failure surfaces as a provider 400 rather than as a silent
/// refusal to dispatch. That is deliberate upstream and preserved here; a stricter
/// "return `None` rather than a non-chat model" variant would be a behaviour
/// change, not a port.
///
/// Substring matching also means a chat model whose name happens to contain a
/// marker (`clip` in `eclipse-tune`, say) is treated as non-chat. Combined with
/// the fallback that is harmless — the id is still reachable, just not auto-picked.
///
/// Generic over the id representation because it is called both on a stored
/// [`crate::model_endpoints::ModelIdList`] (`&[Box<str>]`) and on the output of
/// [`enabled_models`] (`&[&str]`), and those two are meant to compose directly:
/// `first_chat_model(&enabled_models(..))`. Pinning one representation would force
/// every caller to allocate a `Vec<Box<str>>` to convert.
#[must_use]
pub fn first_chat_model<'a, S: AsRef<str> + 'a>(models: &'a [S]) -> Option<&'a str> {
    models
        .iter()
        .find(|m| model_family(m.as_ref()) == ModelFamily::Chat)
        .map_or_else(|| models.first().map(AsRef::as_ref), |m| Some(m.as_ref()))
}

/// Cached ids minus the endpoint's hidden list, order preserved.
///
/// Ports `_endpoint_enabled_models` (`src/endpoint_resolver.py:127`) over the
/// already-parsed lists rather than over raw JSON, because parsing is
/// [`crate::model_endpoints::ModelIdList`]'s job and doing it twice would be the
/// duplication T0306 exists to remove.
///
/// Pinned models are merged after cached ones and de-duplicated case-sensitively,
/// and a hidden id is dropped from both.
#[must_use]
pub fn enabled_models<'a>(
    cached: &'a [Box<str>],
    pinned: &'a [Box<str>],
    hidden: &'a [Box<str>],
) -> Vec<&'a str> {
    let mut seen = std::collections::BTreeSet::new();
    cached
        .iter()
        .chain(pinned.iter())
        .map(AsRef::as_ref)
        .filter(|id| !id.is_empty())
        .filter(|id| seen.insert(*id))
        .filter(|id| !hidden.iter().any(|h| h.as_ref() == *id))
        .collect()
}

/// A resolved dispatch target: the URL to call, the model, and whether to track cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRoute {
    /// The fully-built chat URL.
    pub url: String,
    /// The model id to send.
    pub model: Box<str>,
    /// Whether token cost should be recorded for this route.
    pub cost_tracked: bool,
}

/// Formats a resolved route for a `Debug` sink with the model id intact.
///
/// `ResolvedRoute`'s own `Debug` is derived and includes the URL, which for a
/// provider with the key in the query would be a leak. This is the redacted view
/// for logs.
pub struct RedactedRoute<'a>(pub &'a ResolvedRoute);

impl fmt::Debug for RedactedRoute<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedRoute")
            .field("url", &"<redacted>")
            .field("model", &self.0.model)
            .field("cost_tracked", &self.0.cost_tracked)
            .finish()
    }
}
