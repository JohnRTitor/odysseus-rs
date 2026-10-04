//! T0306 / T0307: endpoint resolution, normalisation, and chat-model selection.
//!
//! The normalisation cases are transcribed from `tests/test_endpoint_resolver*.py`
//! where one exists, because per `AGENTS.md` §11 the Python suite is the
//! specification.

use odysseus_store::model_endpoints_resolver::{
    ModelFamily, NON_CHAT_MARKERS, ResolveError, ResolvedBase, anthropic_api_root, append_path,
    chat_url, enabled_models, first_chat_model, model_family, models_url, normalize_base,
    ollama_api_root,
};

fn ids(list: &[&str]) -> Vec<Box<str>> {
    list.iter().map(|s| Box::from(*s)).collect()
}

/// `normalize_base` strips a known API path suffix, repeatedly.
#[test]
fn normalize_strips_known_api_suffixes() {
    for (input, expected) in [
        ("http://h/v1/models", "http://h/v1"),
        ("http://h/v1/chat/completions", "http://h/v1"),
        ("http://h/v1/completions", "http://h/v1"),
        ("http://h/v1/messages", "http://h"),
        ("http://h/responses", "http://h"),
        // Ollama's native surface collapses to the shared `/api` root.
        ("http://h/api/chat", "http://h/api"),
        ("http://h/api/tags", "http://h/api"),
        ("http://h/api/generate", "http://h/api"),
        // Trailing slashes never survive.
        ("http://h/v1/models/", "http://h/v1"),
        ("http://h/v1///", "http://h/v1"),
        // A base with nothing to strip is returned trimmed.
        ("  http://h/v1  ", "http://h/v1"),
        ("", ""),
    ] {
        assert_eq!(normalize_base(input), expected, "{input}");
    }
}

/// Stripping one suffix can expose another, so normalisation must iterate.
///
/// `…/v1/models` loses `/models`; `…/api/tags` loses `/tags` and is then checked
/// against the `/api` forms again.
#[test]
fn normalize_repeats_until_nothing_matches() {
    // `/models` first, then the result no longer ends in any suffix.
    assert_eq!(normalize_base("http://h/v1/models"), "http://h/v1");
    // A double-suffixed base loses both.
    assert_eq!(normalize_base("http://h/api/chat/"), "http://h/api");
    // Idempotence: normalising a normalised base changes nothing.
    for input in [
        "http://h/v1/models",
        "http://h/api/tags",
        "http://h/v1/chat/completions",
        "http://h",
    ] {
        let once = normalize_base(input);
        assert_eq!(normalize_base(&once), once, "{input}");
    }
}

/// `append_path` adds exactly one segment, never doubling a slash.
#[test]
fn append_path_never_doubles_a_slash() {
    assert_eq!(append_path("http://h", "/models"), "http://h/models");
    assert_eq!(append_path("http://h/v1", "/models"), "http://h/v1/models");
    assert_eq!(append_path("http://h/v1/", "/models"), "http://h/v1/models");
    assert_eq!(
        append_path("http://h/v1///", "models"),
        "http://h/v1/models"
    );
    assert_eq!(
        append_path("http://h:1234/v1", "/models"),
        "http://h:1234/v1/models",
        "the port must survive"
    );
}

/// Anthropic loses its `/v1` root; an OpenAI-compatible server keeps it.
#[test]
fn the_anthropic_root_is_the_only_one_that_drops_v1() {
    assert_eq!(
        anthropic_api_root("https://api.anthropic.com/v1"),
        "https://api.anthropic.com"
    );
    assert_eq!(
        anthropic_api_root("https://api.anthropic.com"),
        "https://api.anthropic.com",
        "no /v1 to drop"
    );
    // A non-Anthropic base keeps its /v1, which is what makes the chat URL work.
    assert_eq!(
        anthropic_api_root("http://h/openai/v1"),
        "http://h/openai/v1"
    );
    // A look-alike host is not Anthropic and so keeps its /v1.
    assert_eq!(
        anthropic_api_root("https://api.anthropic.com.example/v1"),
        "https://api.anthropic.com.example/v1"
    );
}

#[test]
fn the_ollama_root_peels_native_endpoints_and_adds_api_to_a_bare_host() {
    assert_eq!(ollama_api_root("http://h:11434"), "http://h:11434/api");
    assert_eq!(ollama_api_root("http://h:11434/api"), "http://h:11434/api");
    assert_eq!(
        ollama_api_root("http://h:11434/api/tags"),
        "http://h:11434/api"
    );
    assert_eq!(
        ollama_api_root("http://h:11434/api/chat"),
        "http://h:11434/api"
    );
    assert_eq!(
        ollama_api_root("http://h:11434/api/generate"),
        "http://h:11434/api"
    );
    // An explicit non-native path is left alone.
    assert_eq!(ollama_api_root("http://h/custom"), "http://h/custom");
    // The hosted root is forced, whatever path was configured.
    assert_eq!(
        ollama_api_root("https://ollama.com/some/path"),
        "https://ollama.com/api"
    );
}

/// Each provider gets the URL shape its client actually calls.
#[test]
fn chat_urls_match_each_provider_surface() {
    for (base, expected) in [
        (
            "https://api.openai.com/v1",
            "https://api.openai.com/v1/chat/completions",
        ),
        (
            "https://api.anthropic.com/v1",
            "https://api.anthropic.com/v1/messages",
        ),
        (
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1/messages",
        ),
        ("http://localhost:11434", "http://localhost:11434/api/chat"),
        (
            "http://localhost:11434/api",
            "http://localhost:11434/api/chat",
        ),
        (
            "https://chatgpt.com/backend-api/codex",
            "https://chatgpt.com/backend-api/codex/responses",
        ),
        // Unknown hosts are OpenAI-compatible.
        (
            "http://192.168.1.5:8000/v1",
            "http://192.168.1.5:8000/v1/chat/completions",
        ),
        // A pasted full URL normalises back to its base first.
        (
            "http://localhost:1234/v1/chat/completions",
            "http://localhost:1234/v1/chat/completions",
        ),
    ] {
        assert_eq!(chat_url(base).expect("valid base"), expected, "{base}");
    }
}

/// `/v1` is inserted only for an allow-listed pathless host.
///
/// Note the loopback cases are absent here on purpose. Python's
/// `_is_ollama_native_url` returns true for `localhost`/`127.0.0.1`/`::1` on
/// **any** port when the path is empty or `/api`, so `http://localhost:1234` — even
/// though 1234 is LM Studio's default — is detected as native Ollama and served
/// from `/api/tags`, never `/v1/models`. That is surprising enough to be worth
/// stating rather than discovering later; `_v1_insertion_needs_a_remote_host`
/// covers the allow-list itself.
#[test]
fn models_urls_insert_v1_only_for_an_allow_listed_pathless_host() {
    for (base, expected) in [
        ("https://api.openai.com", "https://api.openai.com/v1/models"),
        (
            "https://api.deepseek.com",
            "https://api.deepseek.com/v1/models",
        ),
        // Explicit path: never rewritten.
        (
            "http://10.0.0.9:1234/openai/v1",
            "http://10.0.0.9:1234/openai/v1/models",
        ),
        (
            "http://10.0.0.9:1234/openai",
            "http://10.0.0.9:1234/openai/models",
        ),
        // Unrecognised remote host: /v1 is not invented.
        (
            "http://192.168.1.50:1234",
            "http://192.168.1.50:1234/models",
        ),
    ] {
        assert_eq!(
            models_url(base).expect("valid base").as_deref(),
            Some(expected),
            "{base}"
        );
    }
}

/// A pathless loopback base is Ollama, so it never reaches the `/v1` rule.
#[test]
fn a_pathless_loopback_base_is_native_ollama_not_openai() {
    // 1234 is LM Studio's default port and `/v1/models` would be the OpenAI answer,
    // but Python classifies the bare loopback base as native Ollama first.
    assert_eq!(
        models_url("http://localhost:1234")
            .expect("valid base")
            .as_deref(),
        Some("http://localhost:1234/api/tags")
    );
    // `host.docker.internal` is not a loopback literal, so it stays generic.
    assert_eq!(
        models_url("http://host.docker.internal")
            .expect("valid base")
            .as_deref(),
        Some("http://host.docker.internal/v1/models")
    );
}

/// Anthropic and Ollama serve their model lists from their native paths.
#[test]
fn provider_native_model_lists_are_not_openai_shaped() {
    assert_eq!(
        models_url("https://api.anthropic.com/v1")
            .expect("valid base")
            .as_deref(),
        Some("https://api.anthropic.com/v1/models")
    );
    assert_eq!(
        models_url("http://localhost:11434")
            .expect("valid base")
            .as_deref(),
        Some("http://localhost:11434/api/tags")
    );
}

/// A provider with no model list reports that, rather than an empty list.
#[test]
fn a_discovery_only_provider_has_no_models_url() {
    assert_eq!(
        models_url("https://chatgpt.com/backend-api/codex").expect("valid base"),
        None
    );
}

/// A base carrying a query or fragment is rejected, not silently mangled.
///
/// `append_path` puts the path after the authority, so a base like
/// `http://h/v1?key=s` would otherwise yield `http://h/v1/models?key=s`, whose
/// effective path is `/v1` — not the `/v1/models` that was requested.
#[test]
fn a_base_with_a_query_or_fragment_is_rejected() {
    for bad in [
        "http://h/v1?key=secret",
        "http://h/v1#frag",
        "http://h/v1/models?a=1",
    ] {
        assert_eq!(chat_url(bad), Err(ResolveError::QueryOrFragment), "{bad}");
        assert_eq!(models_url(bad), Err(ResolveError::QueryOrFragment), "{bad}");
    }
    // A query in the *path* position is not stripped first, so it is still rejected.
    assert_eq!(
        ResolvedBase::prepare("http://h?x=1"),
        Err(ResolveError::QueryOrFragment)
    );
}

/// The host-override seam is where Tailscale resolution plugs in.
#[test]
fn a_host_override_keeps_scheme_port_and_path() {
    let base = ResolvedBase::prepare("https://box.tailnet.ts.net:1234/v1/").expect("valid");
    let overridden = base.apply_host_override("100.64.1.7");
    assert_eq!(overridden.as_str(), "https://100.64.1.7:1234/v1");
    // And a normalised base stays normalised across the rewrite.
    assert_eq!(
        ResolvedBase::prepare("http://box/v1/models")
            .expect("valid")
            .apply_host_override("100.64.1.7")
            .as_str(),
        "http://100.64.1.7/v1"
    );
}

// ── T0307: _first_chat_model ───────────────────────────────────────────────

/// One case per non-chat marker.
///
/// A marker that silently stops matching turns a chat request into an embedding
/// request against a model that cannot chat, so each is pinned individually rather
/// than through a loop over the table.
#[test]
fn each_non_chat_marker_is_skipped() {
    for (marker, model) in NON_CHAT_MARKERS.iter().zip([
        "text-embedding-3-large", // text-embedding
        "my-embedding-model",     // embedding
        "tts-1-handsome",         // tts-
        "whisper-large-v3",       // whisper
        "dall-e-3",               // dall-e
        "text-moderation-latest", // moderation
        "rerank-v3",              // rerank
        "my-reranker-2",          // reranker
        "clip-vit-large",         // clip
        "stable-diffusion-xl",    // stable-diffusion
    ]) {
        assert_eq!(
            model_family(model),
            ModelFamily::NonChat,
            "{marker} must mark {model} non-chat"
        );
    }
    // Every marker in the table is exercised by the pairs above.
    assert_eq!(NON_CHAT_MARKERS.len(), 10);
}

/// Matching is substring and case-insensitive, exactly as Python's `p in m.lower()`.
#[test]
fn marker_matching_is_case_insensitive_substring_not_prefix() {
    // Mid-string, which a prefix reading would miss.
    assert_eq!(model_family("acme/embedding-3"), ModelFamily::NonChat);
    assert_eq!(model_family("acme/Whisper-Large"), ModelFamily::NonChat);
    // Bare `embedding` only ever matches as a substring.
    assert_eq!(model_family("embedding"), ModelFamily::NonChat);
    assert_eq!(model_family("a-embedding-b"), ModelFamily::NonChat);
    // Case folding.
    assert_eq!(model_family("TEXT-EMBEDDING-3"), ModelFamily::NonChat);
    assert_eq!(model_family("DALL-E-3"), ModelFamily::NonChat);
}

/// An ordinary chat model is not misclassified by any marker.
#[test]
fn ordinary_chat_models_are_chat() {
    for model in [
        "gpt-4o",
        "claude-sonnet-4-5",
        "llama3.1:8b",
        "qwen2.5-coder:7b",
        "openai/gpt-oss-120b",
        "mistral-small-3.2",
        "phi-4",
    ] {
        assert_eq!(model_family(model), ModelFamily::Chat, "{model}");
    }
}

/// `first_chat_model` picks the first chat model, preserving list order.
#[test]
fn the_first_chat_model_wins_regardless_of_position() {
    let models = ids(&[
        "text-embedding-3-large",
        "gpt-4o",
        "whisper-1",
        "gpt-4o-mini",
    ]);
    assert_eq!(first_chat_model(&models), Some("gpt-4o"));
}

/// The upstream fallback: an all-non-chat list yields `models[0]`, **not** `None`.
///
/// `test_first_chat_model_falls_back_to_first_when_all_non_chat` pins this. It is
/// a best-effort pick, not a filter — the failure surfaces as a provider 400
/// rather than as a silent refusal to dispatch.
#[test]
fn an_all_non_chat_list_falls_back_to_the_first_entry() {
    assert_eq!(
        first_chat_model(&ids(&["text-embedding-3-large"])),
        Some("text-embedding-3-large")
    );
    assert_eq!(
        first_chat_model(&ids(&["whisper-large-v3"])),
        Some("whisper-large-v3")
    );
    assert_eq!(
        first_chat_model(&ids(&["whisper-1", "dall-e-3"])),
        Some("whisper-1"),
        "the fallback is the first entry, not the shortest id"
    );
}

/// An empty list has no first entry to fall back to.
#[test]
fn an_empty_list_resolves_to_nothing() {
    let empty: Vec<Box<str>> = Vec::new();
    assert_eq!(first_chat_model(&empty), None);
}

/// Enabled models merge cached then pinned, de-duplicate, and drop hidden ids.
#[test]
fn enabled_models_merges_dedupes_and_hides() {
    let cached = ids(&["a", "b", "hidden-1"]);
    let pinned = ids(&["b", "c"]);
    let hidden = ids(&["hidden-1"]);
    // Cached first, then pinned; "b" appears in both and is kept once; the hidden
    // id is dropped from the merged list regardless of which list it came from.
    assert_eq!(enabled_models(&cached, &pinned, &hidden), ["a", "b", "c"]);

    // A hidden id is removed even when it is the only source.
    let only_hidden = ids(&["x"]);
    let pinned_hidden = ids(&["x", "y"]);
    assert_eq!(
        enabled_models(&only_hidden, &pinned_hidden, &ids(&["x"])),
        ["y"]
    );
}

/// The empty string is dropped; whitespace is **not**.
///
/// Python guards with `if not isinstance(m, str) or not m`, and `"  "` is truthy,
/// so a whitespace-only id survives upstream and is preserved here. Ported as-is
/// because the Python is the specification, but it is worth naming: a stored list
/// containing `" "` yields an enabled model that is not a usable id, and
/// `first_chat_model` will happily auto-pick it. Trimming here would be a
/// behaviour change, so it is pinned rather than silently fixed.
#[test]
fn only_the_empty_string_is_dropped_not_whitespace() {
    let cached = ids(&["", " ", "real"]);
    let enabled = enabled_models(&cached, &[], &[]);
    assert_eq!(enabled, [" ", "real"]);
}

/// An auto-pick must never land on a model the user disabled.
///
/// A Groq endpoint can list 16 models with one enabled; picking the raw first entry
/// resolves to a model that 400s.
#[test]
fn a_hidden_leading_model_is_never_auto_picked() {
    let cached = ids(&["text-embedding-3-large", "llama-3.3-70b"]);
    let pinned: Vec<Box<str>> = Vec::new();
    let hidden = ids(&["text-embedding-3-large"]);
    let enabled = enabled_models(&cached, &pinned, &hidden);
    assert_eq!(enabled, ["llama-3.3-70b"]);
    assert_eq!(first_chat_model(&enabled), Some("llama-3.3-70b"));
}

/// When every enabled model is hidden, there is nothing to pick — and crucially
/// no fallback, because the fallback reads `models[0]` of a list that is now empty.
#[test]
fn hiding_every_model_leaves_nothing_to_pick() {
    let cached = ids(&["a"]);
    let hidden = ids(&["a"]);
    let enabled = enabled_models(&cached, &[], &hidden);
    assert!(enabled.is_empty());
    assert_eq!(first_chat_model(&enabled), None);
}
