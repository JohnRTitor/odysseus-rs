//! Provider detection, with the near-miss cases as the point of the exercise.
//!
//! `host_matches` decides which credentials get sent where. The failure mode is
//! specific and severe: a substring test matches `?x=api.anthropic.com`, and a
//! bare `ends_with("anthropic.com")` matches `anthropic.com.example` — both of
//! which hand a user's API key to whoever controls a host they do not own.

use odysseus_store::model_endpoints::{Provider, host_matches, is_ollama_native_url};

#[test]
fn an_exact_host_matches() {
    assert!(host_matches("https://anthropic.com/v1", "anthropic.com"));
    assert!(host_matches(
        "https://api.anthropic.com/v1",
        "anthropic.com"
    ));
}

#[test]
fn a_subdomain_matches() {
    assert!(host_matches("https://api.anthropic.com", "anthropic.com"));
    assert!(host_matches("https://a.b.c.anthropic.com", "anthropic.com"));
    assert!(host_matches("https://moonshot.cn/v1", "moonshot.cn"));
}

/// The attack this function exists to prevent: `ends_with` on the wrong side.
#[test]
fn a_lookalike_domain_never_matches() {
    for hostile in [
        "https://anthropic.com.example/v1",
        "https://notanthropic.com/v1",
        "https://anthropic.com.attacker.net/v1",
        "https://xanthropic.com/v1",
        "https://api.anthropic.computer/v1",
    ] {
        assert!(
            !host_matches(hostile, "anthropic.com"),
            "{hostile} must not be treated as Anthropic"
        );
    }
}

/// The other half: a provider name sitting in a path or query is not a match.
#[test]
fn a_provider_named_in_the_path_or_query_does_not_match() {
    for decoy in [
        "https://attacker.test/proxy?upstream=api.anthropic.com",
        "https://attacker.test/api.anthropic.com/v1",
        "https://attacker.test/#api.anthropic.com",
    ] {
        assert!(
            !host_matches(decoy, "anthropic.com"),
            "{decoy} must not be treated as Anthropic"
        );
    }
}

#[test]
fn host_matching_is_case_insensitive_and_tolerates_a_trailing_dot() {
    assert!(host_matches(
        "https://API.Anthropic.COM/v1",
        "anthropic.com"
    ));
    assert!(host_matches(
        "https://api.anthropic.com./v1",
        "anthropic.com"
    ));
    assert!(host_matches(
        "https://api.anthropic.com/v1",
        "ANTHROPIC.COM"
    ));
}

#[test]
fn an_absent_or_malformed_url_matches_nothing() {
    assert!(!host_matches("", "anthropic.com"));
    assert!(!host_matches("not a url", "anthropic.com"));
    assert!(!host_matches("https://", "anthropic.com"));
    assert!(!host_matches("https://x", ""));
}

/// A scheme-less base URL has **no** hostname, so it matches nothing.
///
/// `urlparse("api.openrouter.ai/api/v1").hostname` is `None`, so Python's
/// `_host_match` returns `False` too, and this port matches that. The operational
/// consequence is worth stating rather than hiding: an endpoint saved without a
/// `scheme://` prefix loses provider detection and falls through to the
/// OpenAI-compatible default. That is the safe direction — the default is
/// `/v1/models`, which most providers expose — but it is a silent downgrade, so
/// it is pinned here where a future change to the parser would show up.
#[test]
fn a_scheme_less_base_has_no_host_and_falls_back() {
    assert!(!host_matches("api.openrouter.ai/api/v1", "openrouter.ai"));
    assert_eq!(
        Provider::detect("api.openrouter.ai/api/v1"),
        Provider::OpenAi
    );

    // With a scheme, or an explicit `//` authority, it matches.
    assert!(host_matches("//api.openrouter.ai/api/v1", "openrouter.ai"));
    assert!(host_matches(
        "https://api.openrouter.ai/api/v1",
        "openrouter.ai"
    ));
}

#[test]
fn credentials_in_the_authority_do_not_look_like_the_host() {
    // `https://anthropic.com@evil.test/` is a request to evil.test. A parser that
    // splits on the first '/' rather than the last '@' would call this Anthropic.
    assert!(!host_matches(
        "https://anthropic.com@evil.test/v1",
        "anthropic.com"
    ));
    assert!(!host_matches(
        "https://api.anthropic.com:x@evil.test/",
        "anthropic.com"
    ));
}

#[test]
fn provider_detection_matches_the_python_table() {
    let cases = [
        ("https://api.anthropic.com/v1", Provider::Anthropic),
        ("https://openrouter.ai/api/v1", Provider::OpenRouter),
        ("https://api.groq.com/openai/v1", Provider::Groq),
        ("https://integrate.api.nvidia.com/v1", Provider::Nvidia),
        ("https://api.moonshot.ai/v1", Provider::Moonshot),
        ("https://api.moonshot.cn/v1", Provider::Moonshot),
        ("https://api.cerebras.ai/v1", Provider::Cerebras),
        ("https://api.mistral.ai/v1", Provider::Mistral),
        ("https://opencode.ai/zen/go/v1", Provider::OpenCodeGo),
        ("https://opencode.ai/zen/v1", Provider::OpenCodeZen),
    ];
    for (url, expected) in cases {
        assert_eq!(Provider::detect(url), expected, "{url}");
    }
}

/// Unknown and hostile hosts both fall through to the OpenAI-compatible default,
/// which is what most providers implement — and, importantly, means a look-alike
/// host is *not* classified as the real provider.
#[test]
fn unknown_and_hostile_hosts_fall_back_to_the_openai_default() {
    for url in [
        "http://localhost:1234/v1",
        "https://api.anthropic.com.example/v1",
        "https://attacker.test/proxy?upstream=api.openai.com",
        "",
    ] {
        assert_eq!(Provider::detect(url), Provider::OpenAi, "{url}");
    }
}

/// The OpenCode branches need hostname **and** path, not a substring search.
///
/// `_detect_provider`'s docstring promises it "matches on hostname (exact or
/// subdomain) rather than substring, so a URL that merely contains a provider's
/// domain in its path or query ... is not misclassified". Every URL below carries
/// `opencode.ai/zen/go` somewhere in it and must still fall back to the default:
/// a `url.contains` transliteration would classify every one of them as OpenCode Go.
#[test]
fn opencode_needs_the_host_and_the_path_not_a_substring() {
    for decoy in [
        // In the query string.
        "https://attacker.test/api?next=opencode.ai/zen/go",
        // In the fragment.
        "https://attacker.test/api#opencode.ai/zen/go",
        // In userinfo, before an unrelated host.
        "https://opencode.ai@attacker.test/zen/go",
        // Right host, but the fragment stands in for the path.
        "https://opencode.ai/api#opencode.ai/zen/go",
        // Right host, unrelated path.
        "https://opencode.ai/v1",
        // A look-alike host with the right path.
        "https://opencode.ai.example/zen/go",
        // A prefix collision on the path segment itself.
        "https://opencode.ai/zenith/v1",
    ] {
        assert_eq!(Provider::detect(decoy), Provider::OpenAi, "{decoy}");
    }
}

/// The genuine OpenCode URLs, including a deeper path under the same prefix.
#[test]
fn opencode_matches_the_host_and_a_segment_wise_path_prefix() {
    for (url, expected) in [
        ("https://opencode.ai/zen/go", Provider::OpenCodeGo),
        ("https://opencode.ai/zen/go/v1", Provider::OpenCodeGo),
        (
            "https://opencode.ai/zen/go/deeper/path",
            Provider::OpenCodeGo,
        ),
        ("https://opencode.ai/zen", Provider::OpenCodeZen),
        ("https://opencode.ai/zen/v1", Provider::OpenCodeZen),
        // A subdomain of the provider host is still the provider host.
        ("https://api.opencode.ai/zen/go/v1", Provider::OpenCodeGo),
    ] {
        assert_eq!(Provider::detect(url), expected, "{url}");
    }
}

/// The OpenCode decoys above must fall back because the parser genuinely resolves
/// the host and path -- not because the fragment or the userinfo happened to leave
/// a string that matched nothing.
///
/// If `UrlParts` stopped stripping userinfo or the fragment, the decoys would keep
/// "passing" for the wrong reason while real endpoints broke. These are the positive
/// cases that a regression would actually be caught by.
#[test]
fn userinfo_and_fragment_are_parsed_rather_than_left_in_the_host() {
    // Userinfo is dropped, so the real host is still recognised.
    assert_eq!(
        Provider::detect("https://user:pass@api.anthropic.com/v1"),
        Provider::Anthropic
    );
    // A fragment after a real path does not disturb detection.
    assert_eq!(
        Provider::detect("https://openrouter.ai/api/v1#frag"),
        Provider::OpenRouter
    );
    // ...and the path stops at the query string rather than absorbing it.
    assert_eq!(
        Provider::detect("https://opencode.ai/zen/go?beta=1"),
        Provider::OpenCodeGo
    );
}

#[test]
fn ollama_is_detected_on_both_surfaces() {
    assert_eq!(Provider::detect("http://localhost:11434"), Provider::Ollama);
    assert_eq!(
        Provider::detect("http://127.0.0.1:11434/api/tags"),
        Provider::Ollama
    );
    assert_eq!(
        Provider::detect("https://ollama.com/api/tags"),
        Provider::Ollama
    );
    // The OpenAI-compatible surface is not the native one, even on 11434.
    assert!(!is_ollama_native_url("http://localhost:11434/v1"));
    assert_eq!(
        Provider::detect("http://localhost:11434/v1"),
        Provider::OpenAi
    );
    // A non-default port on loopback is only Ollama if the path says so.
    assert!(is_ollama_native_url("http://localhost:9999/api"));
    assert!(!is_ollama_native_url("http://localhost:9999/v1"));
}

/// Port 11434 alone is enough, on **any** host.
///
/// This surprised me and is preserved deliberately. `_is_ollama_native_url`
/// (`src/llm_core.py:574`) computes
/// `host in {localhost, 127.0.0.1, 0.0.0.0, ::1} or parsed.port == 11434`, so a
/// *remote* host on port 11434 with a bare path is classified Ollama even though
/// Ollama is by definition local. The consequence is a wrong model-list URL —
/// `/api/tags` instead of `/v1/models` — for an unusual remote service on that
/// port.
///
/// Kept as-is because `AGENTS.md` §14.2 makes Python the specification and
/// divergence here would be an unrecorded behaviour change. Flagged for a
/// decision rather than silently corrected; if it is ever changed, change both
/// sides together.
#[test]
fn port_11434_alone_marks_a_remote_host_as_ollama() {
    assert!(
        is_ollama_native_url("https://remote.example:11434/"),
        "Python treats the port as sufficient; this pins that parity"
    );
    assert_eq!(
        Provider::detect("https://remote.example:11434/"),
        Provider::Ollama
    );
    // An explicit /v1 path still wins, so a real OpenAI-compatible server on 11434
    // is unaffected.
    assert!(!is_ollama_native_url("https://remote.example:11434/v1"));
}

#[test]
fn provider_names_round_trip_and_reject_nonsense() {
    for provider in Provider::ALL {
        assert_eq!(Provider::parse(provider.as_str()).ok(), Some(provider));
    }
    assert!(Provider::parse("not-a-provider").is_err());
}

#[test]
fn a_provider_without_a_model_list_says_so() {
    assert!(Provider::ChatGptSubscription.is_discovery_only());
    assert!(!Provider::OpenAi.is_discovery_only());
}
