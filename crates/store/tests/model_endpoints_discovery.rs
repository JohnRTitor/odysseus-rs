//! Model discovery through an injected transport.
//!
//! `AGENTS.md` §11 forbids sleep-based synchronisation in tests, and a discovery
//! probe is exactly where that rule gets forgotten: the tempting shape is "retry
//! three times with a 100 ms sleep", which is slow, flaky, and untestable. The
//! transport here is a value, so a probe is a pure function of its inputs and the
//! timeout is an argument the fake can assert on.

use std::{cell::RefCell, collections::BTreeMap, time::Duration};

use odysseus_store::model_endpoints::{
    Discovery, ModelTransport, ProbeResponse, Provider, TransportError, discover_models,
    models_url, probe_headers,
};

/// A transport that records what it was asked and replies from a script.
struct FakeTransport {
    /// Keyed by URL; the first URL whose key is a substring of the request wins.
    replies: Vec<(String, Result<ProbeResponse, TransportError>)>,
    /// Every request made, for assertions.
    seen: RefCell<Vec<(String, BTreeMap<String, String>, Duration)>>,
}

impl FakeTransport {
    fn new(replies: Vec<(&str, Result<ProbeResponse, TransportError>)>) -> Self {
        Self {
            replies: replies
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            seen: RefCell::new(Vec::new()),
        }
    }
}

impl ModelTransport for FakeTransport {
    fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<ProbeResponse, TransportError> {
        self.seen
            .borrow_mut()
            .push((url.to_owned(), headers.clone(), timeout));
        self.replies
            .iter()
            .find(|(needle, _)| url.contains(needle.as_str()))
            .map_or(
                Err(TransportError::Failed("no scripted reply".to_owned())),
                |(_, r)| r.clone(),
            )
    }
}

fn openai_body() -> String {
    r#"{"object":"list","data":[{"id":"llama3:8b"},{"id":"qwen2.5"},{"id":"mistral-nemo"}]}"#
        .to_owned()
}

#[test]
fn an_openai_compatible_server_yields_its_models() {
    let transport = FakeTransport::new(vec![("/v1/models", Ok(ProbeResponse::ok(openai_body())))]);
    let found = discover_models(
        &transport,
        "http://192.168.1.50:1234/v1",
        None,
        Duration::from_secs(2),
    )
    .expect("discovery");

    let Discovery::Models(ids) = found else {
        panic!("expected models, got {found:?}")
    };
    assert_eq!(
        ids.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
        ["llama3:8b", "qwen2.5", "mistral-nemo"]
    );
}

#[test]
fn ollama_is_probed_on_its_native_tags_endpoint_not_v1_models() {
    let transport = FakeTransport::new(vec![(
        "/api/tags",
        Ok(ProbeResponse::ok(
            r#"{"models":[{"name":"llama3:8b"}]}"#.to_owned(),
        )),
    )]);
    let found = discover_models(
        &transport,
        "http://localhost:11434",
        None,
        Duration::from_secs(1),
    )
    .expect("discovery");
    assert!(matches!(found, Discovery::Models(_)), "got {found:?}");

    let seen = transport.seen.borrow();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].0.ends_with("/api/tags"),
        "Ollama lists models at /api/tags, got {}",
        seen[0].0
    );
}

/// The timeout must reach the transport rather than becoming a sleep.
#[test]
fn the_timeout_is_handed_to_the_transport_and_never_slept() {
    let transport = FakeTransport::new(vec![("/v1/models", Ok(ProbeResponse::ok(openai_body())))]);
    let started = std::time::Instant::now();
    let _ = discover_models(
        &transport,
        "http://192.168.1.50:1234/v1",
        None,
        Duration::from_millis(250),
    );
    let elapsed = started.elapsed();

    assert_eq!(
        transport.seen.borrow()[0].2,
        Duration::from_millis(250),
        "timeout must be passed through"
    );
    assert!(
        elapsed < Duration::from_millis(200),
        "a fake transport must return immediately; took {elapsed:?}, so something slept"
    );
}

#[test]
fn the_api_key_is_sent_as_a_bearer_header_and_only_when_present() {
    let with_key = FakeTransport::new(vec![("/v1/models", Ok(ProbeResponse::ok(openai_body())))]);
    let _ = discover_models(
        &with_key,
        "http://192.168.1.50:1234/v1",
        Some("sk-probe"),
        Duration::from_secs(1),
    );
    let headers = &with_key.seen.borrow()[0].1;
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("Bearer sk-probe")
    );

    let without = FakeTransport::new(vec![("/v1/models", Ok(ProbeResponse::ok(openai_body())))]);
    let _ = discover_models(
        &without,
        "http://192.168.1.50:1234/v1",
        None,
        Duration::from_secs(1),
    );
    assert!(
        without.seen.borrow()[0].1.is_empty(),
        "an empty Authorization header confuses local servers; send none"
    );
}

/// `chatgpt-subscription` has no model list, so probing one is meaningless.
///
/// The gap is deliberate and pinned here: Python decides that provider by calling
/// `src/chatgpt_subscription.is_chatgpt_subscription_base(url)`, which this port
/// does not have, so `Provider::detect` never returns the variant and a
/// chatgpt-subscription base currently falls through to the OpenAI default. That
/// is a **wrong-but-safe** classification — it yields a failed probe rather than a
/// misdirected credential — and it is recorded rather than papered over. Wiring
/// the real detection needs those two helpers ported.
#[test]
fn a_provider_with_no_model_list_is_reported_as_unsupported_not_as_empty() {
    assert_eq!(models_url("http://x", Provider::ChatGptSubscription), None);
    assert!(Provider::ChatGptSubscription.is_discovery_only());

    // Until the detection helper is ported, such a base is classified OpenAI and
    // therefore probed. Pinning today's behaviour makes the gap visible in the
    // diff that eventually closes it.
    assert_eq!(
        Provider::detect("https://chatgpt.com/backend-api/codex"),
        Provider::OpenAi
    );
}

#[test]
fn a_timeout_is_a_reportable_result_not_an_error() {
    let transport = FakeTransport::new(vec![("/v1/models", Err(TransportError::Timeout))]);
    let found = discover_models(
        &transport,
        "http://192.168.1.50:1234/v1",
        None,
        Duration::from_secs(1),
    )
    .expect("a dead server is a result, not a programming error");
    assert_eq!(found, Discovery::Failed("timeout".to_owned()));
}

#[test]
fn a_non_2xx_status_is_a_failure_with_the_code() {
    let transport = FakeTransport::new(vec![(
        "/v1/models",
        Ok(ProbeResponse {
            status: 401,
            body: String::new(),
        }),
    )]);
    let found = discover_models(
        &transport,
        "http://192.168.1.50:1234/v1",
        Some("sk-bad"),
        Duration::from_secs(1),
    )
    .expect("discovery");
    assert_eq!(found, Discovery::Failed("status 401".to_owned()));
}

#[test]
fn a_body_that_is_not_a_model_list_is_reported_rather_than_silently_empty() {
    // An HTML error page from a reverse proxy must not look like "no models",
    // which the UI would render as an empty picker.
    let transport = FakeTransport::new(vec![(
        "/v1/models",
        Ok(ProbeResponse::ok("<html>502</html>")),
    )]);
    let found = discover_models(
        &transport,
        "http://192.168.1.50:1234/v1",
        None,
        Duration::from_secs(1),
    )
    .expect("discovery");
    match found {
        Discovery::Unparseable(body) => assert!(body.contains("502")),
        other => panic!("expected Unparseable, got {other:?}"),
    }
}

#[test]
fn an_unparseable_body_is_truncated_before_it_reaches_a_log() {
    let huge = format!("<html>{}", "x".repeat(10_000));
    let transport = FakeTransport::new(vec![("/v1/models", Ok(ProbeResponse::ok(huge)))]);
    let found = discover_models(
        &transport,
        "http://192.168.1.50:1234/v1",
        None,
        Duration::from_secs(1),
    )
    .expect("discovery");
    match found {
        Discovery::Unparseable(body) => assert!(
            body.chars().count() <= 201,
            "got {} chars",
            body.chars().count()
        ),
        other => panic!("expected Unparseable, got {other:?}"),
    }
}

#[test]
fn a_blank_base_url_is_rejected_before_any_transport_work() {
    let transport = FakeTransport::new(vec![]);
    let err = discover_models(&transport, "   ", None, Duration::from_secs(1)).expect_err("blank");
    assert!(
        matches!(
            err,
            odysseus_store::model_endpoints::EndpointError::Validation(_)
        ),
        "got {err:?}"
    );
    assert!(
        transport.seen.borrow().is_empty(),
        "nothing should have been attempted"
    );
}

#[test]
fn the_probe_url_respects_an_explicit_path_prefix() {
    // issue #25: a bare host needs /v1 inserted, an explicit path must not be.
    assert_eq!(
        models_url("http://192.168.1.50:1234", Provider::OpenAi).as_deref(),
        Some("http://192.168.1.50:1234/v1/models")
    );
    assert_eq!(
        models_url("http://192.168.1.50:1234/openai/v1", Provider::OpenAi).as_deref(),
        Some("http://192.168.1.50:1234/openai/v1/models")
    );
    assert_eq!(
        models_url("http://192.168.1.50:1234/", Provider::OpenAi).as_deref(),
        Some("http://192.168.1.50:1234/v1/models"),
        "a trailing slash is still a bare host"
    );
    assert_eq!(
        models_url("https://api.anthropic.com", Provider::Anthropic).as_deref(),
        Some("https://api.anthropic.com/v1/models")
    );
}

#[test]
fn probe_headers_are_stable_and_absent_when_there_is_no_key() {
    assert_eq!(probe_headers(Some("k")).len(), 1);
    assert!(probe_headers(None).is_empty());
}
