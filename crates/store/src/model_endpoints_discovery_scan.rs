//! T0304: host port-scan discovery.
//!
//! Ports `ModelDiscovery` (`src/model_discovery.py`): probe a set of hosts across
//! the well-known local-model-server ports and report which ones answer with a
//! model list.
//!
//! ## Everything the outside world touches is injected
//!
//! Python's `_get_hosts` reads three environment variables and shells out to
//! `tailscale status --json`; `discover_models` then makes up to
//! `hosts × 26` HTTP requests on a 50-thread pool. None of that is reachable from a
//! unit test without a subprocess, a real network, or both — and `AGENTS.md` §11
//! requires tests that never touch the network or the wall clock.
//!
//! So the inputs are values: [`DiscoverySources`] supplies the host list and the
//! [`ScanTransport`] performs each request. The scan itself is then a pure
//! function of `(sources, transport, timeout)`, which is what makes the port list,
//! the dedup rule and the fingerprinting all testable without a socket.
//!
//! ## The scan is deliberately bounded
//!
//! `hosts × 26` requests is a lot, and the common failure is a firewall dropping
//! packets rather than refusing them, so every request carries an explicit
//! timeout and [`ScanOptions::max_targets`] caps the total. The cap is *not* in
//! Python: it is a bound this port adds so a misconfigured `LLM_HOSTS` cannot turn a
//! settings page load into thousands of requests. It defaults above the realistic
//! maximum, so it does not change behaviour for a normal host list, and it is
//! [`None`]-able for callers that genuinely want the full sweep.

use std::{collections::BTreeSet, time::Duration};

use crate::model_endpoints::{TransportError, UrlParts};

/// The ports scanned, in Python's order: 8000–8020 (vLLM, `SGLang`, Cookbook), then
/// 8080 (llama.cpp), 1234 (LM Studio), 11434 (Ollama) and 11435 (APFEL, whose
/// default port 11434 is taken by Ollama).
const WELL_KNOWN_PORTS: &[u16] = &[
    8000, 8001, 8002, 8003, 8004, 8005, 8006, 8007, 8008, 8009, 8010, 8011, 8012, 8013, 8014, 8015,
    8016, 8017, 8018, 8019, 8020, 8080, 1234, 11434, 11435,
];

/// Always appended: Docker Desktop and Linux compose map this to the host machine,
/// which is the common "I started Ollama normally on this computer" case.
const DOCKER_HOST: &str = "host.docker.internal";

/// The OpenAI-compatible chat path recorded on each discovered item.
const OPENAI_CHAT_PATH: &str = "/v1/chat/completions";

/// Server software identified by a native API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSoftware {
    /// LM Studio's native `/api/v1/models`.
    LmStudio,
    /// llama.cpp's `llama-server`, via its unprefixed `/props`.
    LlamaCpp,
}

impl ServerSoftware {
    /// The wire name, matching Python's `"lmstudio"` / `"llamacpp"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LmStudio => "lmstudio",
            Self::LlamaCpp => "llamacpp",
        }
    }
}

/// One endpoint that answered with a model list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredItem {
    /// The host that was probed.
    pub host: Box<str>,
    /// The port that answered.
    pub port: u16,
    /// The OpenAI-compatible chat URL for this endpoint.
    pub chat_url: Box<str>,
    /// The model ids it reported, in the order the server listed them.
    pub models: Box<[Box<str>]>,
    /// The same ids with a leading `/` stripped, for display.
    pub models_display: Box<[Box<str>]>,
    /// The server software, if a native API identified it.
    pub software: Option<ServerSoftware>,
}

/// The outcome of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanResult {
    /// The hosts that were scanned.
    pub hosts: Box<[Box<str>]>,
    /// The endpoints that answered, sorted by host then port.
    pub items: Box<[DiscoveredItem]>,
}

impl ScanResult {
    /// The cheap `/models` URLs for warmup, at most `limit` of them.
    ///
    /// Ports `warmup_ping_urls` (`src/model_discovery.py:247`). Each item already
    /// carries a `/v1/chat/completions` URL; the suffix is swapped for the cheap
    /// probe. Python returns `[]` on any failure so a warmup pass can never crash
    /// its caller, which is why this is infallible and total.
    #[must_use]
    pub fn warmup_ping_urls(&self, limit: usize) -> Vec<Box<str>> {
        self.items
            .iter()
            .take(limit)
            .filter_map(|item| {
                item.chat_url
                    .strip_suffix("/chat/completions")
                    .map(|prefix| Box::from(format!("{prefix}/models")) as Box<str>)
            })
            .collect()
    }
}

/// One probe response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response body.
    pub body: Box<str>,
}

impl ScanResponse {
    /// A 200 with `body`.
    #[must_use]
    pub fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into().into_boxed_str(),
        }
    }

    /// A non-success status with an empty body.
    #[must_use]
    pub fn status(status: u16) -> Self {
        Self {
            status,
            body: String::new().into_boxed_str(),
        }
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }
}

/// Performs one scan request.
///
/// The trait takes only the pieces a probe needs. It cannot read the environment,
/// so a fake needs no global state and tests cannot leak into each other.
pub trait ScanTransport {
    /// Fetches `url`, giving up after `timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Timeout`] if `timeout` elapsed and
    /// [`TransportError::Failed`] for anything else. A caller must treat both as
    /// "this target did not answer" rather than propagating: a scan sweeps dozens
    /// of targets and a single refused connection is the normal case, not an error.
    fn get(&self, url: &str, timeout: Duration) -> Result<ScanResponse, TransportError>;
}

/// The host list and extra ports a scan should use.
///
/// The three inputs Python reads from the environment become fields, so a test can
/// construct any host list without `std::env::set_var` (which is `unsafe` in edition
/// 2024 and process-global besides).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscoverySources {
    /// `LLM_HOSTS`: a manual override. When non-empty it wins over Tailscale.
    pub manual_hosts: Box<[Box<str>]>,
    /// Peers found by Tailscale discovery, if any.
    pub tailscale_hosts: Box<[Box<str>]>,
    /// Values of `OLLAMA_BASE_URL`, `OLLAMA_URL` and `LM_STUDIO_URL`, in that
    /// order. Each contributes its hostname to the host list and, when the port is
    /// not already well known, that port to the scan.
    pub provider_urls: Box<[Box<str>]>,
}

/// The host list a scan will cover, plus any ports the env vars contributed.
///
/// Ports `_get_hosts` (`src/model_discovery.py:101`), with the same precedence:
/// `LLM_HOSTS` beats Tailscale, the default host is always present, and
/// `host.docker.internal` is always appended. Ordering is preserved and duplicates
/// are dropped on first sighting, which is why the default host can be prepended
/// without a later append re-adding it.
#[must_use]
pub fn hosts_to_scan(default_host: &str, sources: &DiscoverySources) -> (Vec<Box<str>>, Vec<u16>) {
    let mut hosts: Vec<Box<str>> = Vec::new();
    let mut extra_ports: Vec<u16> = Vec::new();
    let push = |hosts: &mut Vec<Box<str>>, host: &str| {
        let trimmed = host.trim();
        if !trimmed.is_empty() && !hosts.iter().any(|h| h.as_ref() == trimmed) {
            hosts.push(Box::from(trimmed));
        }
    };

    if !sources.manual_hosts.is_empty() {
        for host in &sources.manual_hosts {
            push(&mut hosts, host);
        }
        // The default host is always scanned, even when LLM_HOSTS omits it.
        if !hosts.iter().any(|h| h.as_ref() == default_host.trim()) {
            hosts.insert(0, Box::from(default_host.trim()));
        }
    } else if !sources.tailscale_hosts.is_empty() {
        for host in &sources.tailscale_hosts {
            push(&mut hosts, host);
        }
        if !hosts.iter().any(|h| h.as_ref() == default_host.trim()) {
            hosts.insert(0, Box::from(default_host.trim()));
        }
    } else {
        push(&mut hosts, default_host);
    }

    push(&mut hosts, DOCKER_HOST);

    for raw in &sources.provider_urls {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Python prepends "http://" when the value has no scheme, so a bare
        // host:port parses the same way here.
        let candidate = if trimmed.contains("://") {
            trimmed
        } else {
            &format!("http://{trimmed}")
        };
        let parts = UrlParts::parse(candidate);
        push(&mut hosts, &parts.host);
        if let Some(port) = parts.port
            && !WELL_KNOWN_PORTS.contains(&port)
            && !extra_ports.contains(&port)
        {
            extra_ports.push(port);
        }
    }

    (hosts, extra_ports)
}

/// Knobs for one scan.
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Per-request timeout.
    pub timeout: Duration,
    /// Cap on total requests, or `None` for the full sweep.
    ///
    /// Added by this port; Python has no bound. See the module docs.
    pub max_targets: Option<usize>,
    /// Whether to spend two extra requests per hit on native-API fingerprinting.
    ///
    /// Off by default because it triples the request count of a successful scan;
    /// the caller that renders a provider badge turns it on.
    pub fingerprint: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            max_targets: Some(512),
            fingerprint: true,
        }
    }
}

/// Identifies the server software behind `host:port` via its native API.
///
/// Ports `_fingerprint_provider` (`src/model_discovery.py:153`). Two shapes, tried
/// in order, each requiring the **first** element to carry specific keys — an
/// empty list is not a match, and a list whose first entry lacks `architecture` is
/// an OpenAI-compatible server rather than LM Studio.
fn fingerprint(
    transport: &dyn ScanTransport,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Option<ServerSoftware> {
    let native = match transport.get(&format!("http://{host}:{port}/api/v1/models"), timeout) {
        Ok(response) if response.is_success() => response.body,
        _ => Box::default(),
    };
    if looks_like_lmstudio(&native) {
        return Some(ServerSoftware::LmStudio);
    }
    let props = match transport.get(&format!("http://{host}:{port}/props"), timeout) {
        Ok(response) if response.is_success() => response.body,
        _ => Box::default(),
    };
    looks_like_llamacpp(&props).then_some(ServerSoftware::LlamaCpp)
}

/// Whether a body is LM Studio's native model list.
///
/// The check is on the **first** element only, matching Python: an empty list is not
/// a match, and a list whose first entry lacks `architecture` is an
/// OpenAI-compatible server rather than LM Studio.
#[must_use]
pub fn looks_like_lmstudio(body: &str) -> bool {
    let Some(models) = json_array(body, "models") else {
        return false;
    };
    let Some(first) = models.first().filter(|v| v.is_object()) else {
        return false;
    };
    first.get("key").is_some() && first.get("architecture").is_some()
}

/// Whether a body is llama.cpp's `/props`.
///
/// Any one of the three markers is enough, because a llama-server build that
/// reports `total_slots` but not `chat_template` is still llama.cpp.
#[must_use]
pub fn looks_like_llamacpp(body: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    [
        "default_generation_settings",
        "total_slots",
        "chat_template",
    ]
    .iter()
    .any(|key| value.get(key).is_some())
}

/// The array at `key`, if `body` is a JSON object carrying one.
fn json_array(body: &str, key: &str) -> Option<Vec<serde_json::Value>> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get(key)
        .and_then(serde_json::Value::as_array)
        .cloned()
}

/// Extracts model ids from an OpenAI-compatible `/v1/models` body.
///
/// Ports the body half of `_check_port` (`src/model_discovery.py:186`). Accepts
/// both shapes in use: `{"data": [{"id": …}]}` and a bare top-level list, because
/// "some OpenAI-compatible servers return a bare list".
#[must_use]
pub fn model_ids_from(body: &str) -> Box<[Box<str>]> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Box::default();
    };
    let empty: Vec<serde_json::Value> = Vec::new();
    let items = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())
        .unwrap_or(&empty);
    items
        .iter()
        .filter_map(|item| item.get("id"))
        .filter_map(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .map(Box::from)
        .collect()
}

/// Scans `default_host` and every host in `sources` for local model servers.
///
/// Ports `discover_models` (`src/model_discovery.py:210`), including the dedup rule
/// and the final ordering.
///
/// Two behaviours are worth stating because they look like bugs and are not:
///
/// * **Dedupe is by `(port, sorted models)`, not by host.** The same machine
///   reachable through two IPs — a Tailscale address and a LAN address, say —
///   yields the same `(port, models)` pair and is reported once. Two genuinely
///   different servers on the same port serving identical model ids would also
///   collapse, which is the accepted trade-off upstream and is better than
///   reporting one machine twice.
/// * **Output is sorted by `(host, port)`**, not by which probe finished first, so
///   two runs against the same network produce the same list. Python gets this from
///   an explicit sort after a thread pool; here the scan is sequential, so the sort
///   is what preserves the guarantee rather than the completion order.
///
/// # Errors
///
/// Infallible in practice: a target that does not answer is skipped, which is the
/// expected outcome for the overwhelming majority of a port sweep.
#[must_use]
pub fn scan(
    transport: &dyn ScanTransport,
    default_host: &str,
    sources: &DiscoverySources,
    options: &ScanOptions,
) -> ScanResult {
    let (hosts, mut extra_ports) = hosts_to_scan(default_host, sources);

    let mut ports: Vec<u16> = WELL_KNOWN_PORTS.to_vec();
    // Sorted, so the resulting scan order does not depend on env var order.
    extra_ports.sort_unstable();
    for port in extra_ports {
        if !ports.contains(&port) {
            ports.push(port);
        }
    }

    let mut items: Vec<DiscoveredItem> = Vec::new();
    let mut seen: BTreeSet<(u16, Vec<Box<str>>)> = BTreeSet::new();
    let mut budget = options.max_targets;

    'outer: for host in &hosts {
        for &port in &ports {
            if let Some(remaining) = budget.as_mut() {
                if *remaining == 0 {
                    break 'outer;
                }
                *remaining -= 1;
            }
            let url = format!("http://{host}:{port}{OPENAI_CHAT_PATH}");
            let models_url = format!("http://{host}:{port}/v1/models");
            let Ok(response) = transport.get(&models_url, options.timeout) else {
                continue;
            };
            if !response.is_success() {
                continue;
            }
            let models = model_ids_from(&response.body);
            if models.is_empty() {
                continue;
            }
            let key = (port, {
                let mut sorted = models.to_vec();
                sorted.sort();
                sorted
            });
            if !seen.insert(key) {
                continue;
            }
            items.push(DiscoveredItem {
                host: host.clone(),
                port,
                chat_url: Box::from(url),
                models_display: models
                    .iter()
                    .map(|id| Box::from(id.trim_start_matches('/')) as Box<str>)
                    .collect(),
                models,
                software: if options.fingerprint {
                    fingerprint(transport, host, port, options.timeout)
                } else {
                    None
                },
            });
        }
    }

    items.sort_by(|a, b| a.host.cmp(&b.host).then_with(|| a.port.cmp(&b.port)));

    ScanResult {
        hosts: hosts.into_boxed_slice(),
        items: items.into_boxed_slice(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DiscoverySources, ServerSoftware, hosts_to_scan, looks_like_llamacpp, looks_like_lmstudio,
        model_ids_from,
    };

    #[test]
    fn manual_hosts_win_and_still_include_the_default() {
        let sources = DiscoverySources {
            manual_hosts: vec![Box::from("10.0.0.5")].into_boxed_slice(),
            tailscale_hosts: vec![Box::from("100.64.0.1")].into_boxed_slice(),
            ..DiscoverySources::default()
        };
        let (hosts, ports) = hosts_to_scan("localhost", &sources);
        assert_eq!(
            hosts[0].as_ref(),
            "localhost",
            "the default host is always scanned"
        );
        assert!(
            hosts.iter().any(|h| h.as_ref() == "10.0.0.5"),
            "the manual host is present"
        );
        assert!(
            !hosts.iter().any(|h| h.as_ref() == "100.64.0.1"),
            "manual hosts suppress Tailscale discovery"
        );
        assert!(ports.is_empty());
    }

    #[test]
    fn tailscale_hosts_are_used_when_no_manual_override() {
        let sources = DiscoverySources {
            tailscale_hosts: vec![Box::from("100.64.0.1"), Box::from("100.64.0.2")]
                .into_boxed_slice(),
            ..DiscoverySources::default()
        };
        let (hosts, _) = hosts_to_scan("localhost", &sources);
        assert_eq!(hosts[0].as_ref(), "localhost");
        assert!(hosts.iter().any(|h| h.as_ref() == "100.64.0.1"));
        assert!(hosts.iter().any(|h| h.as_ref() == "100.64.0.2"));
    }

    #[test]
    fn the_default_branch_is_the_default_host_and_docker() {
        let (hosts, ports) = hosts_to_scan("localhost", &DiscoverySources::default());
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].as_ref(), "localhost");
        assert_eq!(hosts[1].as_ref(), "host.docker.internal");
        assert!(ports.is_empty());
    }

    #[test]
    fn a_provider_url_contributes_its_host_and_port() {
        let sources = DiscoverySources {
            provider_urls: vec![Box::from("http://my-lm-box:5000")].into_boxed_slice(),
            ..DiscoverySources::default()
        };
        let (hosts, ports) = hosts_to_scan("localhost", &sources);
        assert!(hosts.iter().any(|h| h.as_ref() == "my-lm-box"));
        assert_eq!(ports, [5000], "a non-default port joins the scan");
    }

    #[test]
    fn a_provider_url_on_a_well_known_port_does_not_duplicate_it() {
        let sources = DiscoverySources {
            provider_urls: vec![Box::from("http://localhost:1234")].into_boxed_slice(),
            ..DiscoverySources::default()
        };
        let (hosts, ports) = hosts_to_scan("localhost", &sources);
        assert_eq!(
            hosts.iter().filter(|h| h.as_ref() == "localhost").count(),
            1,
            "no duplicate host"
        );
        assert!(ports.is_empty(), "1234 is already well known");
    }

    #[test]
    fn a_scheme_less_provider_url_still_parses() {
        let sources = DiscoverySources {
            provider_urls: vec![Box::from("my-box:5000")].into_boxed_slice(),
            ..DiscoverySources::default()
        };
        let (hosts, ports) = hosts_to_scan("localhost", &sources);
        assert!(hosts.iter().any(|h| h.as_ref() == "my-box"));
        assert_eq!(ports, [5000]);
    }

    #[test]
    fn lmstudio_requires_key_and_architecture_on_the_first_entry() {
        assert!(looks_like_lmstudio(
            r#"{"models":[{"type":"llm","key":"qwen","architecture":"qwen35"}]}"#
        ));
        // Ollama's /api/tags shape lacks `architecture`.
        assert!(!looks_like_lmstudio(
            r#"{"models":[{"name":"llama3","modified_at":"x","size":1}]}"#
        ));
        // OpenAI shape.
        assert!(!looks_like_lmstudio(r#"{"data":[{"id":"gpt-4o"}]}"#));
        // Empty list is not a match.
        assert!(!looks_like_lmstudio(r#"{"models":[]}"#));
    }

    #[test]
    fn llamacpp_accepts_any_one_marker() {
        assert!(looks_like_llamacpp(r#"{"default_generation_settings":{}}"#));
        assert!(looks_like_llamacpp(r#"{"total_slots":4}"#));
        assert!(looks_like_llamacpp(r#"{"chat_template":"..."}"#));
        assert!(!looks_like_llamacpp(r#"{"other":1}"#));
        assert!(!looks_like_llamacpp("not json"));
    }

    #[test]
    fn model_ids_accept_both_openai_shapes() {
        assert_eq!(
            model_ids_from(r#"{"data":[{"id":"a"},{"id":"b"}]}"#).len(),
            2
        );
        assert_eq!(
            model_ids_from(r#"[{"id":"a"},{"id":"b"}]"#).len(),
            2,
            "some servers return a bare list"
        );
        assert!(model_ids_from(r#"{"data":[]}"#).is_empty());
        assert!(model_ids_from("not json").is_empty());
    }

    #[test]
    fn server_software_wire_names_match_python() {
        assert_eq!(ServerSoftware::LmStudio.as_str(), "lmstudio");
        assert_eq!(ServerSoftware::LlamaCpp.as_str(), "llamacpp");
    }
}
