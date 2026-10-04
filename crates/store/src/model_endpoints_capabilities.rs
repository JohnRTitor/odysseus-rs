//! T0305: model capability detection, and the LM Studio probe with its 60 s TTL.
//!
//! Ports `model_capability_readers/lmstudio.py` plus the probe that feeds it,
//! `_probe_lmstudio_models` / `lmstudio_supports_vision` (`src/chat_helpers.py`).
//!
//! ## The TTL is a parameter, and the clock is a value
//!
//! Python's probe caches per `(host, port)` with a 60-second expiry computed from
//! `time.time()`. Reading the clock here would make every test that touches this
//! either sleep or depend on wall-clock timing, both of which `AGENTS.md` §11
//! forbids. So [`LmStudioProbe`] takes a [`Clock`] and a TTL, and nothing in this
//! module reads the system clock: a test advances a `TestClock` by hand and asserts
//! on the cache directly.
//!
//! ## The cache is deliberately asymmetric
//!
//! This is the part that is easy to get wrong, so it is stated as a rule rather
//! than left to the implementation:
//!
//! * **A transport failure is not cached.** If the request raises or times out, the
//!   probe returns "unknown" and caches nothing, so the next call retries. The
//!   Python comment calls the negative case "transient errors uncached" and the test
//!   `test_failures_stay_retryable` asserts the tailscale analogue runs twice.
//! * **A definitive answer is cached, including a negative.** A response that is
//!   well-formed but is not LM Studio caches `None` for the full TTL. Caching only
//!   successes would mean re-probing every unknown local endpoint on every call,
//!   which is the cost the TTL exists to avoid.
//!
//! Note the boundary falls on *receiving a response*, not on its status: a 500 from
//! a real LM Studio is a response, so the `None` it produces is cached. Only a
//! failure to obtain a response stays uncached.

use std::{cell::RefCell, collections::BTreeMap, time::Duration};

use crate::model_endpoints::{TransportError, UrlParts};

/// The upstream TTL for a probe result.
pub const LMSTUDIO_PROBE_TTL: Duration = Duration::from_secs(60);

/// A source of the current time.
///
/// Injected rather than read from [`std::time::SystemTime`] so TTL behaviour is
/// testable without sleeping.
pub trait Clock {
    /// Seconds since an arbitrary but fixed epoch.
    ///
    /// Seconds rather than an instant because that is all a TTL needs, and a
    /// monotonic `u64` keeps a fake clock trivial.
    fn now_secs(&self) -> u64;
}

/// A clock that does not move.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrozenClock(u64);

impl FrozenClock {
    /// A clock pinned at `secs`.
    #[must_use]
    pub const fn at(secs: u64) -> Self {
        Self(secs)
    }
}

impl Clock for FrozenClock {
    fn now_secs(&self) -> u64 {
        self.0
    }
}

/// A clock a test advances by hand.
///
/// Uses interior mutability so a probe can hold a shared `&TestClock` — [`Clock`] is
/// read-only by design — while the test still moves time forward. Without this, the
/// borrow checker forces a caller to rebuild the probe every time it wanted to test
/// an expiry, which is the opposite of what the TTL tests need to do.
#[derive(Debug, Default)]
pub struct TestClock(std::cell::Cell<u64>);

impl TestClock {
    /// A clock at `secs`.
    #[must_use]
    pub fn at(secs: u64) -> Self {
        Self(std::cell::Cell::new(secs))
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by.as_secs());
    }
}

impl Clock for TestClock {
    fn now_secs(&self) -> u64 {
        self.0.get()
    }
}

/// What LM Studio reported about one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioModel {
    /// The model key.
    pub key: Box<str>,
    /// The display name, if the server sent one.
    pub display_name: Option<Box<str>>,
    /// Whether the server advertises vision.
    ///
    /// `None` means the server did not say, which is not the same as `false` — the
    /// caller falls back to a name-based heuristic rather than denying vision.
    pub vision: Option<bool>,
    /// Whether the server advertises tool use.
    pub tool_use: Option<bool>,
    /// Whether the server advertises reasoning.
    pub reasoning: Option<bool>,
    /// The model type as reported (`llm`, `vlm`, `embedding`, …).
    pub model_type: Option<Box<str>>,
}

/// Fetches LM Studio's native model list.
pub trait LmStudioTransport {
    /// Fetches `url`, giving up after `timeout`.
    ///
    /// # Errors
    ///
    /// [`TransportError::Timeout`] or [`TransportError::Failed`]. The probe
    /// distinguishes these from a response: an error means "do not cache".
    fn get(&self, url: &str, timeout: Duration) -> Result<String, TransportError>;
}

/// What a model is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFamily {
    /// Chat or text generation.
    Chat,
    /// Embeddings.
    Embedding,
    /// The server did not say.
    Unknown,
}

/// A model's capability summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapability {
    /// The model key.
    pub model_id: Box<str>,
    /// What the model is for.
    pub family: ModelFamily,
    /// Whether the model accepts image input.
    pub vision: Option<bool>,
    /// Whether the model supports tool calls.
    pub tool_use: Option<bool>,
    /// Whether the model supports reasoning.
    pub reasoning: Option<bool>,
    /// The context length in tokens, when reported.
    pub context_tokens: Option<u32>,
    /// The maximum context length, when it differs from `context_tokens`.
    pub max_context_tokens: Option<u32>,
}

/// A `(host, port)` cache key.
///
/// Port-aware, matching Python's `(host, parsed.port)` tuple: the same host on
/// 1234 and 5000 are two different LM Studio instances and must not share an entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    host: Box<str>,
    port: Option<u16>,
}

impl CacheKey {
    fn of(url: &str) -> Self {
        let parts = UrlParts::parse(url);
        Self {
            host: parts.host.into_boxed_str(),
            port: parts.port,
        }
    }
}

/// One cached probe result and when it expires.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheEntry {
    models: Option<Box<[LmStudioModel]>>,
    /// Absolute expiry in [`Clock::now_secs`] units.
    expires_at: u64,
}

/// A cached LM Studio capability probe.
///
/// Construct one per process and share it: the cache is the point. [`Self::probe`]
/// takes `&self` and mutates through a [`RefCell`], so the value is not [`Sync`];
/// wrap it in a `Mutex` if a multi-threaded caller needs one.
pub struct LmStudioProbe<'a> {
    transport: &'a dyn LmStudioTransport,
    clock: &'a dyn Clock,
    ttl: Duration,
    timeout: Duration,
    cache: RefCell<BTreeMap<CacheKey, CacheEntry>>,
}

impl std::fmt::Debug for LmStudioProbe<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately opaque: the cache holds provider-reported model metadata,
        // which is not secret but is not worth a wall of text in a log.
        f.debug_struct("LmStudioProbe")
            .field("cached_entries", &self.cache.borrow().len())
            .finish()
    }
}

impl<'a> LmStudioProbe<'a> {
    /// A probe with the upstream 60 s TTL.
    #[must_use]
    pub fn new(transport: &'a dyn LmStudioTransport, clock: &'a dyn Clock) -> Self {
        Self::with_ttl(transport, clock, LMSTUDIO_PROBE_TTL, Duration::from_secs(1))
    }

    /// A probe with an explicit TTL and request timeout.
    #[must_use]
    pub fn with_ttl(
        transport: &'a dyn LmStudioTransport,
        clock: &'a dyn Clock,
        ttl: Duration,
        timeout: Duration,
    ) -> Self {
        Self {
            transport,
            clock,
            ttl,
            timeout,
            cache: RefCell::new(BTreeMap::new()),
        }
    }

    /// Number of live cache entries, for tests and diagnostics.
    #[must_use]
    pub fn cached_len(&self) -> usize {
        self.cache.borrow().len()
    }

    /// LM Studio's native model list for `url`, or `None` when it is not LM Studio
    /// or the probe failed.
    ///
    /// A remote host short-circuits without any request. LM Studio is a local or
    /// LAN server, so probing a public host would send a request to an arbitrary
    /// third party on the strength of a configured URL — and
    /// `test_remote_endpoint_never_probed` asserts exactly that no request is made.
    #[must_use]
    pub fn probe(&self, url: &str) -> Option<Box<[LmStudioModel]>> {
        let parts = UrlParts::parse(url);
        if !is_local_host(&parts.host) {
            return None;
        }
        let key = CacheKey::of(url);
        let now = self.clock.now_secs();

        if let Some(entry) = self.cache.borrow().get(&key)
            && entry.expires_at > now
        {
            return entry.models.clone();
        }

        let authority = match parts.port {
            Some(port) => format!("{}:{port}", parts.host),
            None => parts.host.clone(),
        };
        let scheme = if parts.scheme.is_empty() {
            "http"
        } else {
            parts.scheme.as_str()
        };
        let probe_url = format!("{scheme}://{authority}/api/v1/models");

        // An error means no response was obtained, so nothing is cached: the next
        // caller retries rather than being told "not LM Studio" for a minute.
        let Ok(body) = self.transport.get(&probe_url, self.timeout) else {
            return None;
        };
        let models = parse_lmstudio_models(&body);

        self.cache.borrow_mut().insert(
            key,
            CacheEntry {
                models: models.clone(),
                expires_at: now + self.ttl.as_secs(),
            },
        );
        models
    }

    /// Whether `model` accepts images, per the endpoint.
    ///
    /// `None` means "the endpoint did not say" — not LM Studio, model not listed, or
    /// listed without a `vision` flag — and the caller falls back to a name-based
    /// heuristic. Collapsing that into `false` would silently drop images.
    #[must_use]
    pub fn supports_vision(&self, url: &str, model: &str) -> Option<bool> {
        if model.trim().is_empty() {
            return None;
        }
        let models = self.probe(url)?;
        let want = model.trim().to_lowercase();
        let found = models.iter().find(|m| {
            // Python builds `{key.lower(), display_name.lower()}` and tests
            // membership, so both sides are folded. Comparing the needle alone
            // against a raw display name silently fails on any capitalised name.
            m.key.to_lowercase() == want
                || m.display_name
                    .as_ref()
                    .is_some_and(|d| d.to_lowercase() == want)
        });
        found?.vision
    }

    /// Capability records for every model LM Studio reports.
    #[must_use]
    pub fn capabilities(&self, url: &str) -> Box<[ModelCapability]> {
        self.probe(url).map_or_else(Box::default, |models| {
            models.iter().map(capability_of).collect()
        })
    }
}

/// Derives a capability record from one reported model.
///
/// Ports `record_from_native_model` (`model_capability_readers/lmstudio.py:107`).
///
/// Two rules are worth naming:
///
/// * **An unrecognised `type` with any capability is treated as chat.** A server that
///   reports `capabilities.reasoning` but omits `type` has told us it can generate
///   text; calling it `Unknown` would discard real information.
/// * **An unrecognised `type` with no capability falls through to the OpenAI-shaped
///   reader, and then to an unknown record.** Absence of evidence stays `Unknown`
///   rather than being promoted to `Chat`.
#[must_use]
pub fn capability_of(model: &LmStudioModel) -> ModelCapability {
    let reported = model.model_type.as_deref().map(str::to_lowercase);
    let family = match reported.as_deref().map(|t| t.replace('-', "_")).as_deref() {
        Some("embedding" | "embeddings" | "text_embedding" | "text_embeddings") => {
            ModelFamily::Embedding
        }
        Some("llm" | "chat" | "vlm" | "vision" | "text_generation") => ModelFamily::Chat,
        _ => {
            if model.vision.is_some() || model.tool_use.is_some() || model.reasoning.is_some() {
                ModelFamily::Chat
            } else {
                ModelFamily::Unknown
            }
        }
    };
    ModelCapability {
        model_id: model.key.clone(),
        family,
        // An embedding model cannot take an image regardless of what it advertises.
        vision: if family == ModelFamily::Embedding {
            Some(false)
        } else {
            model.vision
        },
        tool_use: model.tool_use,
        reasoning: model.reasoning,
        context_tokens: None,
        max_context_tokens: None,
    }
}

/// Parses LM Studio's native `/api/v1/models` payload.
///
/// Returns `None` unless the payload is the LM Studio shape: a `models` array whose
/// **first** entry carries both `key` and `architecture`. An OpenAI-shaped response
/// (`{"data": …}`) or Ollama's `/api/tags` is not LM Studio.
///
/// A payload that parses but does not match is `None` rather than an empty list, so
/// the caller can tell "not LM Studio" from "LM Studio with no models loaded".
#[must_use]
pub fn parse_lmstudio_models(body: &str) -> Option<Box<[LmStudioModel]>> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let items = value.get("models")?.as_array()?;
    let first = items.first()?;
    first.get("key")?;
    first.get("architecture")?;
    Some(
        items
            .iter()
            .filter(|item| item.is_object())
            .map(|item| {
                let caps = item.get("capabilities");
                LmStudioModel {
                    key: item
                        .get("key")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    display_name: item
                        .get("display_name")
                        .and_then(serde_json::Value::as_str)
                        .map(Into::into),
                    vision: flag(caps, "vision"),
                    tool_use: flag(caps, "trained_for_tool_use")
                        .or_else(|| flag(caps, "tools"))
                        .or_else(|| flag(caps, "tool_use")),
                    reasoning: flag(caps, "reasoning"),
                    model_type: item
                        .get("type")
                        .or_else(|| item.get("model_type"))
                        .or_else(|| item.get("task"))
                        .and_then(serde_json::Value::as_str)
                        .map(Into::into),
                }
            })
            .collect(),
    )
}

/// Reads a capability flag as a tri-state: absent is `None`, not `false`.
fn flag(caps: Option<&serde_json::Value>, key: &str) -> Option<bool> {
    // Python tests `is True` for vision/tool use, so a non-boolean truthy value is
    // not a claim. `reasoning` is tested for truthiness instead.
    match key {
        "reasoning" => caps
            .and_then(|c| c.get(key))
            .and_then(serde_json::Value::as_bool),
        _ => match caps.and_then(|c| c.get(key)) {
            Some(serde_json::Value::Bool(true)) => Some(true),
            Some(serde_json::Value::Bool(false)) => Some(false),
            _ => None,
        },
    }
}

/// Whether a host is loopback, LAN, or a Tailscale address.
///
/// Ports `_is_local_host` (`src/chat_helpers.py:92`). A bare hostname with no dot
/// counts as local, which is how a docker-compose service name resolves.
#[must_use]
pub fn is_local_host(host: &str) -> bool {
    let host = host.trim().to_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host == "host.docker.internal" {
        return true;
    }
    // Not a file extension: this is an mDNS hostname suffix, and `host` has already
    // been lowercased above, so the comparison is case-insensitive in effect.
    #[expect(
        clippy::case_sensitive_file_extension_comparisons,
        reason = "`.local` is an mDNS hostname suffix here, not a filename extension"
    )]
    if host.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return is_private_or_local(ip);
    }
    // A dotless name is a container/service name, not a public domain.
    !host.contains('.')
}

/// Whether an address is loopback, private, link-local, or in the Tailscale CGNAT
/// range `100.64.0.0/10`.
fn is_private_or_local(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || in_tailscale_v4(v4)
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// Membership of `100.64.0.0/10`, the CGNAT block Tailscale hands out.
fn in_tailscale_v4(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

/// A vision-capability answer with the name heuristic applied as a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionSupport {
    /// The endpoint said yes or no.
    Reported(bool),
    /// The endpoint did not say; the name heuristic was used.
    Inferred(bool),
}

impl VisionSupport {
    /// The answer as a plain bool.
    #[must_use]
    pub const fn answer(self) -> bool {
        match self {
            Self::Reported(v) | Self::Inferred(v) => v,
        }
    }
}

/// Whether `model` accepts images, preferring the endpoint's answer.
///
/// Ports `model_supports_vision` (`src/chat_helpers.py:150`): the endpoint wins when
/// it speaks, and a name-based heuristic decides otherwise. The two are kept
/// distinguishable via [`VisionSupport`] because "the endpoint confirmed" and "we
/// guessed from the name" are different confidence levels and a caller rendering a
/// UI may want to show only the first.
#[must_use]
pub fn model_supports_vision(probe: &LmStudioProbe<'_>, url: &str, model: &str) -> VisionSupport {
    if !url.is_empty()
        && let Some(reported) = probe.supports_vision(url, model)
    {
        return VisionSupport::Reported(reported);
    }
    VisionSupport::Inferred(is_vision_model_name(model))
}

/// Name-based vision heuristic.
///
/// Ports `is_vision_model` with `_VISION_MODEL_KEYWORDS` and `_VISION_VL_RE`
/// (`src/chat_helpers.py:43`).
///
/// **Errs toward `true`**, because the failure modes are not symmetric: a false
/// positive sends a text-only request to a vision model, which works, while a false
/// negative drops the user's image entirely. That is issue #124's reasoning and it
/// is why the keyword list includes families whose names contain neither "vision"
/// nor "vl" — `gemma3`, `llama4`, `phi4` — since a real image would otherwise be
/// silently discarded.
#[must_use]
pub fn is_vision_model_name(model: &str) -> bool {
    let m = model.to_lowercase();
    if VISION_KEYWORDS.iter().any(|kw| m.contains(kw)) {
        return true;
    }
    has_vl_token(&m)
}

/// Keywords implying a model accepts images.
const VISION_KEYWORDS: &[&str] = &[
    // hosted
    "gpt-4o",
    "gpt-4.1",
    "gpt-4.5",
    "gpt-4-turbo",
    "gpt-4-vision",
    "claude-sonnet",
    "claude-opus",
    "claude-haiku",
    "gemini",
    // open / local
    "vision",
    "multimodal",
    "llava",
    "bakllava",
    "moondream",
    "pixtral",
    "minicpm",
    "internvl",
    "cogvlm",
    "qwen-vl",
    "qwen2-vl",
    "qwen3-vl",
    "qwen3vl",
    // multimodal families whose names carry neither "vision" nor "vl"
    "gemma-3",
    "gemma3",
    "gemma-4",
    "gemma4",
    "llama-4",
    "llama4",
    "mistral-small-3.1",
    "mistral-small3.1",
    "mistral-small-3.2",
    "mistral-small3.2",
    "phi-4",
    "phi4",
    "glm-4.5v",
    "glm-4.6v",
    "glm-5v",
];

/// Whether `m` contains a standalone `vl` token or `vlm`.
///
/// Ports `_VISION_VL_RE = re.compile(r'(?<![a-z])vl(?![a-z])|vlm')`. The two
/// lookarounds are the point: they match `qwen2.5-VL-7b` and `qwen-vl` while *not*
/// matching `vlad` or `unvl`, which a bare `contains("vl")` would. Since `m` is
/// already lowercased by the caller, only ASCII lowercase needs excluding.
fn has_vl_token(m: &str) -> bool {
    let bytes = m.as_bytes();
    // Alternative 2: `vlm` anywhere, no boundaries required by the pattern.
    if m.contains("vlm") {
        return true;
    }
    // Alternative 1: `vl` not preceded and not followed by [a-z].
    (0..bytes.len().saturating_sub(1)).any(|i| {
        m[i..].starts_with("vl")
            && (i == 0 || !bytes[i - 1].is_ascii_lowercase())
            && (i + 2 >= bytes.len() || !bytes[i + 2].is_ascii_lowercase())
    })
}
