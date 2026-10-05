//! T0305: capability detection and the LM Studio probe's 60 s TTL.
//!
//! Every test drives a [`TestClock`] by hand. Nothing here sleeps or reads the wall
//! clock, so the suite is deterministic and fast (`AGENTS.md` §11).

use std::{cell::RefCell, time::Duration};

use odysseus_store::{
    model_endpoints::TransportError,
    model_endpoints_capabilities::{
        LMSTUDIO_PROBE_TTL, LmStudioProbe, LmStudioTransport, ModelCapability, ModelFamily,
        TestClock, VisionSupport, capability_of, is_local_host, is_vision_model_name,
        model_supports_vision, parse_lmstudio_models,
    },
};

/// The payload from `TestLmStudioSupportsVision.PAYLOAD` in the Python suite.
const PAYLOAD: &str = r#"{"models":[
  {"key":"qwen3.6-27b-custom-finetune","architecture":"qwen35",
   "capabilities":{"vision":true,"trained_for_tool_use":true}},
  {"key":"text-only-llm","architecture":"qwen35","capabilities":{"vision":false}},
  {"key":"no-caps-model","architecture":"qwen35"}
]}"#;

const URL: &str = "http://localhost:1234/v1/chat/completions";

/// A transport that serves one body and counts requests.
struct FakeTransport {
    body: RefCell<String>,
    calls: RefCell<usize>,
    fail: RefCell<bool>,
}

impl FakeTransport {
    fn serving(body: &str) -> Self {
        Self {
            body: RefCell::new(body.to_owned()),
            calls: RefCell::new(0),
            fail: RefCell::new(false),
        }
    }

    fn failing() -> Self {
        Self {
            body: RefCell::new(String::new()),
            calls: RefCell::new(0),
            fail: RefCell::new(true),
        }
    }

    fn calls(&self) -> usize {
        *self.calls.borrow()
    }
}

impl LmStudioTransport for FakeTransport {
    fn get(&self, _url: &str, _timeout: Duration) -> Result<String, TransportError> {
        *self.calls.borrow_mut() += 1;
        if *self.fail.borrow() {
            return Err(TransportError::Timeout);
        }
        Ok(self.body.borrow().clone())
    }
}

// ── TTL behaviour ─────────────────────────────────────────────────────────

/// A hit inside the TTL is served from cache without a second request.
#[test]
fn a_cached_result_is_reused_inside_the_ttl() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(1_000);
    let probe = LmStudioProbe::new(&transport, &clock);

    assert!(probe.probe(URL).is_some());
    assert_eq!(transport.calls(), 1);
    for _ in 0..5 {
        assert!(probe.probe(URL).is_some());
    }
    assert_eq!(transport.calls(), 1, "no re-probe inside the TTL");
}

/// Past the TTL the entry is stale and the endpoint is probed again.
#[test]
fn an_entry_expires_exactly_at_the_ttl() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(1_000);
    let probe = LmStudioProbe::new(&transport, &clock);

    let _ = probe.probe(URL);
    assert_eq!(transport.calls(), 1);

    // One second short of the TTL is still fresh. `saturating_sub` rather than
    // `-`: a plain subtraction panics on underflow, and a test asserting on a
    // boundary should not be able to panic on an arithmetic edge.
    clock.advance(LMSTUDIO_PROBE_TTL.saturating_sub(Duration::from_secs(1)));
    let _ = probe.probe(URL);
    assert_eq!(transport.calls(), 1, "59s in is inside a 60s TTL");

    // The comparison is strict (`expiry > now`), so exactly at 60s it is stale.
    clock.advance(Duration::from_secs(1));
    let _ = probe.probe(URL);
    assert_eq!(transport.calls(), 2, "the TTL boundary re-probes");
}

/// A TTL is a parameter, not a constant.
#[test]
fn the_ttl_is_injectable() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::with_ttl(
        &transport,
        &clock,
        Duration::from_secs(5),
        Duration::from_millis(50),
    );

    let _ = probe.probe(URL);
    clock.advance(Duration::from_secs(6));
    let _ = probe.probe(URL);
    assert_eq!(transport.calls(), 2, "a 5s TTL expires sooner");
}

/// **A transport failure is not cached**, so the next call retries.
///
/// This is the asymmetry that matters. Caching a timeout would tell the next caller
/// "this is not LM Studio" for a full minute because the server was briefly busy.
#[test]
fn a_transport_failure_is_not_cached_and_stays_retryable() {
    let transport = FakeTransport::failing();
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);

    assert!(probe.probe(URL).is_none());
    assert!(probe.probe(URL).is_none());
    assert!(probe.probe(URL).is_none());
    assert_eq!(transport.calls(), 3, "every attempt retried");
    assert_eq!(probe.cached_len(), 0, "nothing was cached");
}

/// A server that comes back after a failure is picked up immediately.
#[test]
fn a_recovered_endpoint_is_detected_without_waiting_for_a_ttl() {
    let transport = FakeTransport::failing();
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);
    assert!(probe.probe(URL).is_none());

    // The server comes up; no clock movement at all.
    *transport.fail.borrow_mut() = false;
    *transport.body.borrow_mut() = PAYLOAD.to_owned();
    assert!(probe.probe(URL).is_some());
}

/// A definitive "not LM Studio" answer **is** cached, so unknown local endpoints are
/// not re-probed on every call.
#[test]
fn a_definitive_negative_is_cached_for_the_ttl() {
    let transport = FakeTransport::serving(r#"{"data":[{"id":"gpt-4o"}]}"#);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);

    assert!(probe.probe(URL).is_none());
    assert!(probe.probe(URL).is_none());
    assert_eq!(transport.calls(), 1, "the negative answer is cached");
    assert_eq!(probe.cached_len(), 1);
}

/// The cache key includes the port, so two LM Studio instances on one host differ.
#[test]
fn the_cache_is_keyed_by_host_and_port() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);

    let _ = probe.probe("http://localhost:1234/v1");
    let _ = probe.probe("http://localhost:5000/v1");
    assert_eq!(probe.cached_len(), 2, "two ports are two entries");
    assert_eq!(transport.calls(), 2);
}

/// A remote host is never probed at all.
#[test]
fn a_remote_endpoint_is_never_probed() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);

    assert_eq!(
        probe.supports_vision("https://api.openai.com/v1/chat/completions", "gpt-4o"),
        None
    );
    assert_eq!(
        transport.calls(),
        0,
        "no request may be sent to a public host"
    );
    assert_eq!(probe.cached_len(), 0);
}

// ── parsing and capability records ────────────────────────────────────────

/// The LM Studio shape requires `key` and `architecture` on the first entry.
#[test]
fn only_the_lmstudio_native_shape_parses() {
    assert!(parse_lmstudio_models(PAYLOAD).is_some());
    // OpenAI shape.
    assert!(parse_lmstudio_models(r#"{"data":[{"id":"gpt-4o"}]}"#).is_none());
    // Ollama tags shape.
    assert!(parse_lmstudio_models(r#"{"models":[{"name":"llama3","size":1}]}"#).is_none());
    // Empty models list: the shape matched, the list did not.
    assert!(parse_lmstudio_models(r#"{"models":[]}"#).is_none());
    assert!(parse_lmstudio_models("not json").is_none());
}

/// An absent `vision` flag is `None`, not `false`.
///
/// The distinction is the whole point: `None` means "fall back to the name
/// heuristic", `false` means "this model cannot see".
#[test]
fn an_absent_capability_is_unknown_rather_than_false() {
    let models = parse_lmstudio_models(PAYLOAD).expect("parses");
    let no_caps = models
        .iter()
        .find(|m| m.key.as_ref() == "no-caps-model")
        .expect("present");
    assert_eq!(no_caps.vision, None);
    assert_eq!(no_caps.tool_use, None);
    assert_eq!(no_caps.reasoning, None);
}

/// `capabilities.vision` is read straight from the payload, overriding the name.
#[test]
fn the_reported_vision_flag_is_read_from_capabilities() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);

    // A vision finetune whose name contains no vision keyword: the name heuristic
    // would say false, the endpoint says true.
    assert_eq!(
        probe.supports_vision(URL, "qwen3.6-27b-custom-finetune"),
        Some(true)
    );
    assert!(!is_vision_model_name("qwen3.6-27b-custom-finetune"));
    assert_eq!(probe.supports_vision(URL, "text-only-llm"), Some(false));
    assert_eq!(probe.supports_vision(URL, "no-caps-model"), None);
    assert_eq!(probe.supports_vision(URL, "not-listed"), None);
    assert_eq!(probe.supports_vision(URL, ""), None);
}

/// A model is matched by key or display name, case-insensitively.
#[test]
fn a_model_matches_by_key_or_display_name() {
    let transport = FakeTransport::serving(
        r#"{"models":[{"key":"k1","display_name":"Fancy Name","architecture":"a",
                       "capabilities":{"vision":true}}]}"#,
    );
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);
    assert_eq!(probe.supports_vision(URL, "k1"), Some(true));
    assert_eq!(probe.supports_vision(URL, "Fancy Name"), Some(true));
    assert_eq!(probe.supports_vision(URL, "fancy name"), Some(true));
}

/// Family comes from `type`, with the two fallback rules.
#[test]
fn family_derivation_follows_the_reported_type() {
    let chat = parse_lmstudio_models(r#"{"models":[{"key":"k","architecture":"a","type":"llm"}]}"#);
    let record: ModelCapability = capability_of(&chat.expect("parses")[0]);
    assert_eq!(record.family, ModelFamily::Chat);

    let embedding =
        parse_lmstudio_models(r#"{"models":[{"key":"k","architecture":"a","type":"embedding"}]}"#);
    let record = capability_of(&embedding.expect("parses")[0]);
    assert_eq!(record.family, ModelFamily::Embedding);
    assert_eq!(
        record.vision,
        Some(false),
        "an embedding model cannot take an image"
    );

    // Unknown type + a capability => chat.
    let unknown_with_caps = parse_lmstudio_models(
        r#"{"models":[{"key":"k","architecture":"a","capabilities":{"reasoning":true}}]}"#,
    );
    let record = capability_of(&unknown_with_caps.expect("parses")[0]);
    assert_eq!(
        record.family,
        ModelFamily::Chat,
        "a reported capability is evidence of generation"
    );

    // Unknown type + nothing => genuinely unknown.
    let unknown = parse_lmstudio_models(r#"{"models":[{"key":"k","architecture":"a"}]}"#);
    assert_eq!(
        capability_of(&unknown.expect("parses")[0]).family,
        ModelFamily::Unknown,
        "absence of evidence stays Unknown"
    );
}

/// `trained_for_tool_use`, `tools` and `tool_use` are all accepted spellings.
///
/// Reporting a capability under one spelling and reading another is how a model
/// silently loses its tools.
#[test]
fn every_tool_use_spelling_is_accepted() {
    for key in ["trained_for_tool_use", "tools", "tool_use"] {
        let body = format!(
            r#"{{"models":[{{"key":"k","architecture":"a","capabilities":{{"{key}":true}}}}]}}"#
        );
        let models = parse_lmstudio_models(&body).expect("parses");
        assert_eq!(
            models[0].tool_use,
            Some(true),
            "spelling {key} must be read"
        );
    }
}

// ── the vision decision ───────────────────────────────────────────────────

/// The endpoint's answer wins over the name heuristic.
#[test]
fn the_endpoint_answer_overrides_the_name_heuristic() {
    let transport = FakeTransport::serving(PAYLOAD);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);
    let answer = model_supports_vision(&probe, URL, "qwen3.6-27b-custom-finetune");
    assert_eq!(answer, VisionSupport::Reported(true));
    assert!(answer.answer());
}

/// When the endpoint does not say, the name heuristic decides.
#[test]
fn the_name_heuristic_is_the_fallback() {
    let transport = FakeTransport::serving(r#"{"data":[]}"#);
    let clock = TestClock::at(0);
    let probe = LmStudioProbe::new(&transport, &clock);

    assert_eq!(
        model_supports_vision(&probe, URL, "qwen2-vl-7b"),
        VisionSupport::Inferred(true)
    );
    assert_eq!(
        model_supports_vision(&probe, URL, "plain-llm"),
        VisionSupport::Inferred(false)
    );
    // No endpoint at all is also the fallback path.
    assert_eq!(
        model_supports_vision(&probe, "", "llava-1.6"),
        VisionSupport::Inferred(true)
    );
    assert_eq!(
        model_supports_vision(&probe, "", "mistral-7b"),
        VisionSupport::Inferred(false)
    );
}

/// The `vl` token needs boundaries, or `vlad` and `unvl` would match.
#[test]
fn the_vl_token_needs_letter_boundaries() {
    for vision in [
        "qwen2.5-vl-7b",
        "qwen-vl",
        "minicpm-vl",
        "something-vlm-thing",
        "vlm-model",
    ] {
        assert!(is_vision_model_name(vision), "{vision}");
    }
    for not_vision in ["vladimir", "unvl", "salvo", "vled-7b"] {
        assert!(
            !is_vision_model_name(not_vision),
            "{not_vision} must not match the vl token"
        );
    }
}

/// Families whose names carry neither "vision" nor "vl" are included, because the
/// failure is asymmetric: a false negative silently drops the user's image.
#[test]
fn multimodal_families_without_a_vision_keyword_are_included() {
    for vision in [
        "gemma3:4b",
        "gemma4:12b",
        "llama4:8b",
        "mistral-small3.2:24b",
        "phi4:latest",
        "glm-4.6v",
    ] {
        assert!(is_vision_model_name(vision), "{vision} accepts images");
    }
    assert!(!is_vision_model_name("mistral-7b"));
    assert!(!is_vision_model_name("llama3.1:8b"));
}

/// Locality decides whether a probe is allowed at all.
#[test]
fn locality_covers_loopback_private_linklocal_and_tailscale() {
    for local in [
        "localhost",
        "127.0.0.1",
        "127.1.2.3",
        "::1",
        "192.168.1.5",
        "10.0.0.7",
        "172.16.4.4",
        "169.254.1.1",
        "100.64.0.1",
        "100.127.255.254",
        "my-box.local",
        "host.docker.internal",
        "some-service",
    ] {
        assert!(is_local_host(local), "{local} is local");
    }
    for remote in [
        "api.openai.com",
        "8.8.8.8",
        "example.com",
        "100.63.255.255",
        "100.128.0.1",
        "",
    ] {
        assert!(!is_local_host(remote), "{remote} is not local");
    }
}

/// The upstream TTL is 60 seconds.
#[test]
fn the_default_ttl_is_sixty_seconds() {
    assert_eq!(LMSTUDIO_PROBE_TTL, Duration::from_secs(60));
}
