//! T0304: host port-scan discovery.
//!
//! The scan is a pure function of its injected sources and transport, so every case
//! here runs without a socket, a subprocess, or a sleep.

use std::{cell::RefCell, collections::BTreeMap, time::Duration};

use odysseus_store::{
    model_endpoints::TransportError,
    model_endpoints_discovery_scan::{
        DiscoverySources, ScanOptions, ScanResponse, ScanTransport, ServerSoftware, scan,
    },
};

/// A transport that replies from a script and records what it was asked.
struct FakeTransport {
    replies: BTreeMap<String, Result<ScanResponse, TransportError>>,
    seen: RefCell<Vec<String>>,
    timeouts: RefCell<Vec<Duration>>,
}

impl FakeTransport {
    fn new(replies: Vec<(&str, Result<ScanResponse, TransportError>)>) -> Self {
        Self {
            replies: replies
                .into_iter()
                .map(|(url, r)| (url.to_owned(), r))
                .collect(),
            seen: RefCell::new(Vec::new()),
            timeouts: RefCell::new(Vec::new()),
        }
    }

    fn models(url: &str, body: &str) -> Self {
        Self::new(vec![(url, Ok(ScanResponse::ok(body)))])
    }
}

impl ScanTransport for FakeTransport {
    fn get(&self, url: &str, timeout: Duration) -> Result<ScanResponse, TransportError> {
        self.seen.borrow_mut().push(url.to_owned());
        self.timeouts.borrow_mut().push(timeout);
        self.replies
            .get(url)
            .cloned()
            .unwrap_or(Err(TransportError::Failed("connection refused".to_owned())))
    }
}

fn as_strs(items: &[Box<str>]) -> Vec<&str> {
    items.iter().map(AsRef::as_ref).collect()
}

fn openai_body() -> &'static str {
    r#"{"data":[{"id":"qwen3.6-27b"}]}"#
}

/// Ports are scanned in a fixed order, so the well-known list is pinned rather than
/// left implicit — a port silently dropped from the list is invisible otherwise.
#[test]
fn the_well_known_port_list_is_exact() {
    let transport = FakeTransport::new(Vec::new());
    let _ = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    let seen = transport.seen.borrow();
    let ports: Vec<u16> = seen
        .iter()
        .filter_map(|u| u.strip_prefix("http://h:"))
        .filter_map(|rest| rest.split('/').next())
        .filter_map(|p| p.parse().ok())
        .collect();
    assert_eq!(
        ports.len(),
        25,
        "8000..=8020 is 21, plus 8080/1234/11434/11435"
    );
    assert_eq!(ports.first(), Some(&8000));
    assert_eq!(ports[20], 8020);
    assert!(ports.contains(&8080));
    assert!(ports.contains(&1234), "LM Studio");
    assert!(ports.contains(&11434), "Ollama");
    assert!(
        ports.contains(&11435),
        "APFEL, whose default port is Ollama's"
    );
}

/// A hit records the OpenAI-compatible chat URL, the ids, and the display ids.
#[test]
fn a_hit_reports_its_chat_url_and_models() {
    let transport = FakeTransport::models("http://h:8000/v1/models", openai_body());
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(result.items.len(), 1);
    let item = &result.items[0];
    assert_eq!(item.port, 8000);
    assert_eq!(item.chat_url.as_ref(), "http://h:8000/v1/chat/completions");
    assert_eq!(as_strs(&item.models), ["qwen3.6-27b"]);
}

/// Display ids lose a leading `/`, which some servers prefix.
#[test]
fn display_ids_strip_a_leading_slash() {
    let transport = FakeTransport::models(
        "http://h:8000/v1/models",
        r#"{"data":[{"id":"/org/model"},{"id":"plain"}]}"#,
    );
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(as_strs(&result.items[0].models), ["/org/model", "plain"]);
    assert_eq!(
        as_strs(&result.items[0].models_display),
        ["org/model", "plain"]
    );
}

/// A refused connection or a non-2xx is a skip, not an error.
///
/// A sweep hits dozens of closed ports; treating any of them as a failure would make
/// every scan fail.
#[test]
fn unreachable_and_failing_targets_are_skipped_quietly() {
    let transport = FakeTransport::new(vec![
        ("http://h:8000/v1/models", Err(TransportError::Timeout)),
        ("http://h:8001/v1/models", Ok(ScanResponse::status(500))),
        (
            "http://h:8002/v1/models",
            Ok(ScanResponse::ok(r#"{"data":[]}"#)),
        ),
    ]);
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert!(result.items.is_empty(), "a 200 with no models is not a hit");
}

/// LM Studio is identified by its native API even on a non-default port.
#[test]
fn lmstudio_is_fingerprinted_on_any_port() {
    let native = r#"{"models":[{"type":"llm","key":"qwen3.6-27b","architecture":"qwen35"}]}"#;
    for port in [1234u16, 8080, 8000] {
        let transport = FakeTransport::new(vec![
            (
                &format!("http://h:{port}/v1/models"),
                Ok(ScanResponse::ok(openai_body())),
            ),
            (
                &format!("http://h:{port}/api/v1/models"),
                Ok(ScanResponse::ok(native)),
            ),
        ]);
        let result = scan(
            &transport,
            "h",
            &DiscoverySources::default(),
            &ScanOptions::default(),
        );
        assert_eq!(
            result.items[0].software,
            Some(ServerSoftware::LmStudio),
            "port {port}"
        );
    }
}

/// llama.cpp is identified by its unprefixed `/props`.
#[test]
fn llamacpp_is_fingerprinted_by_props() {
    let transport = FakeTransport::new(vec![
        (
            "http://h:8080/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        (
            "http://h:8080/api/v1/models",
            Ok(ScanResponse::ok(r#"{"data":[{"id":"x"}]}"#)),
        ),
        (
            "http://h:8080/props",
            Ok(ScanResponse::ok(r#"{"total_slots":4}"#)),
        ),
    ]);
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions::default(),
    );
    assert_eq!(result.items[0].software, Some(ServerSoftware::LlamaCpp));
}

/// An OpenAI-compatible server that is neither native shape stays unattributed.
#[test]
fn a_plain_openai_server_has_no_software_attribution() {
    let transport = FakeTransport::new(vec![
        (
            "http://h:8000/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        (
            "http://h:8000/api/v1/models",
            Ok(ScanResponse::ok(r#"{"data":[{"id":"gpt-4o"}]}"#)),
        ),
    ]);
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions::default(),
    );
    assert_eq!(result.items[0].software, None);
}

/// Ollama's `/api/tags` shape is not LM Studio, so port 11434 stays unattributed.
#[test]
fn the_ollama_tags_shape_is_not_lmstudio() {
    let transport = FakeTransport::new(vec![
        (
            "http://h:11434/v1/models",
            Ok(ScanResponse::ok(r#"{"data":[{"id":"llama3"}]}"#)),
        ),
        (
            "http://h:11434/api/v1/models",
            Ok(ScanResponse::ok(
                r#"{"models":[{"name":"llama3","modified_at":"x","size":1}]}"#,
            )),
        ),
    ]);
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions::default(),
    );
    assert_eq!(result.items[0].software, None);
}

/// Fingerprinting can be switched off, which is what keeps a successful scan from
/// tripling its request count.
#[test]
fn fingerprinting_is_skipped_when_disabled() {
    let native = r#"{"models":[{"key":"k","architecture":"a"}]}"#;
    let transport = FakeTransport::new(vec![
        (
            "http://h:8000/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        ("http://h:8000/api/v1/models", Ok(ScanResponse::ok(native))),
    ]);
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(result.items[0].software, None);
    assert!(
        !transport
            .seen
            .borrow()
            .iter()
            .any(|u| u.ends_with("/api/v1/models")),
        "the native probe must not be issued at all"
    );
}

/// The same machine behind two addresses collapses to one entry.
///
/// Python dedupes on `(port, sorted model ids)` rather than on host, specifically
/// so a Tailscale IP and a LAN IP for the same box do not both appear.
#[test]
fn one_machine_behind_two_addresses_is_reported_once() {
    let sources = DiscoverySources {
        tailscale_hosts: vec![Box::from("100.64.0.1"), Box::from("192.168.1.5")].into_boxed_slice(),
        ..DiscoverySources::default()
    };
    let transport = FakeTransport::new(vec![
        (
            "http://192.168.1.5:8000/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        (
            "http://100.64.0.1:8000/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
    ]);
    let result = scan(
        &transport,
        "localhost",
        &sources,
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(
        result.items.len(),
        1,
        "the same port serving the same ids on two IPs is one machine"
    );
    assert_eq!(result.items[0].host.as_ref(), "100.64.0.1");
}

/// A different port serving the same ids is a *different* endpoint and is kept.
#[test]
fn a_different_port_with_the_same_models_is_kept() {
    let transport = FakeTransport::new(vec![
        (
            "http://h:8000/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        (
            "http://h:8001/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
    ]);
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(result.items.len(), 2);
}

/// Ordering is by host then port, independent of which probe answered.
#[test]
fn results_are_sorted_by_host_then_port() {
    let sources = DiscoverySources {
        manual_hosts: vec![Box::from("a"), Box::from("b")].into_boxed_slice(),
        ..DiscoverySources::default()
    };
    let transport = FakeTransport::new(vec![
        (
            "http://b:8080/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        (
            "http://a:8000/v1/models",
            Ok(ScanResponse::ok(openai_body())),
        ),
        (
            "http://a:1234/v1/models",
            Ok(ScanResponse::ok(r#"{"data":[{"id":"other"}]}"#)),
        ),
    ]);
    let result = scan(
        &transport,
        "a",
        &sources,
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    let got: Vec<(&str, u16)> = result
        .items
        .iter()
        .map(|i| (i.host.as_ref(), i.port))
        .collect();
    assert_eq!(got, [("a", 1234), ("a", 8000), ("b", 8080)]);
}

/// The timeout is an argument, handed to every request unchanged.
#[test]
fn the_configured_timeout_reaches_every_request() {
    let transport = FakeTransport::new(Vec::new());
    let timeout = Duration::from_millis(250);
    let _ = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            timeout,
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    let timeouts = transport.timeouts.borrow();
    assert!(!timeouts.is_empty());
    assert!(timeouts.iter().all(|t| *t == timeout), "{timeouts:?}");
}

/// A misconfigured host list is bounded rather than turned into thousands of
/// requests. This cap is added by the port; Python has none.
#[test]
fn the_target_budget_bounds_the_sweep() {
    let transport = FakeTransport::new(Vec::new());
    let _ = scan(
        &transport,
        "h",
        &DiscoverySources {
            manual_hosts: vec![Box::from("a"), Box::from("b")].into_boxed_slice(),
            ..DiscoverySources::default()
        },
        &ScanOptions {
            max_targets: Some(10),
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(transport.seen.borrow().len(), 10);
}

/// `None` means the full sweep, for a caller that wants it.
///
/// The default branch always adds `host.docker.internal`, so the sweep is two hosts
/// by 25 ports.
#[test]
fn no_budget_means_every_target() {
    let transport = FakeTransport::new(Vec::new());
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            max_targets: None,
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(result.hosts.len(), 2);
    assert_eq!(transport.seen.borrow().len(), 50);
}

/// Warmup uses the cheap `/models` probe, not the chat URL.
#[test]
fn warmup_urls_swap_the_chat_path_for_the_cheap_probe() {
    let transport = FakeTransport::models("http://h:8000/v1/models", openai_body());
    let result = scan(
        &transport,
        "h",
        &DiscoverySources::default(),
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    let urls: Vec<String> = result
        .warmup_ping_urls(5)
        .into_iter()
        .map(|u| u.into_string())
        .collect();
    assert_eq!(urls, ["http://h:8000/v1/models"]);
    assert!(result.warmup_ping_urls(0).is_empty(), "limit is honoured");
}

/// A custom port from a provider env var is scanned even though it is not
/// well-known — the case `test_discover_models_scans_custom_lm_studio_port` covers.
#[test]
fn a_custom_port_from_an_env_var_is_scanned() {
    let sources = DiscoverySources {
        provider_urls: vec![Box::from("http://my-lm-box:5000")].into_boxed_slice(),
        ..DiscoverySources::default()
    };
    let transport = FakeTransport::new(vec![(
        "http://my-lm-box:5000/v1/models",
        Ok(ScanResponse::ok(openai_body())),
    )]);
    let result = scan(
        &transport,
        "localhost",
        &sources,
        &ScanOptions {
            fingerprint: false,
            ..ScanOptions::default()
        },
    );
    assert_eq!(result.items.len(), 1);
    assert_eq!(result.items[0].port, 5000);
    assert_eq!(result.items[0].host.as_ref(), "my-lm-box");
}
