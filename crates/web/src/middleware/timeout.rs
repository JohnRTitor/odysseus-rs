//! Request deadline.
//!
//! Ports `_RequestTimeoutMiddleware` from `app.py:194-205`. Without it a single
//! hung subprocess or missing-timeout HTTP call holds the event loop for
//! everyone; with it the caller gets a 504.
//!
//! The exempt prefixes are **load-bearing**. Each one is a long-lived operation
//! that legitimately stays open, and applying a 45-second ceiling to any of them
//! would break a working feature.

use std::time::Duration;

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::AppState;

/// Default deadline, from `REQUEST_HARD_TIMEOUT`.
pub const DEFAULT_TIMEOUT_SECONDS: f64 = 45.0;

/// Paths exempt from the deadline, from `_TIMEOUT_EXEMPT_PREFIXES`
/// (`app.py:180-192`).
///
/// Order is irrelevant because these are prefix matches, and none is a prefix of
/// another.
pub const EXEMPT_PREFIXES: &[&str] = &[
    "/api/chat",           // SSE, plus research spinoff
    "/api/shell/stream",   // long-running shell output
    "/api/research",       // multi-minute jobs
    "/api/model/download", // tmux setup may run package installs
    "/api/model/probe",    // SSE; iterates models with up to 8s timeout each
    "/api/model-endpoints",
    "/api/cookbook/setup", // remote package installs
    "/api/upload",         // large files
    "/api/image",          // diffusion proxies, own 120s upstream timeout
    "/api/memory/audit",   // retains its own 120s LLM inactivity timeout
];

/// True when the path is exempt from the deadline.
///
/// Matching is **segment-aware**, unlike the Python source. `app.py:199` tests
/// with `path.startswith(prefix)`, and the `/api/chat` entry therefore also
/// matched `ChatGPT` subscription device-flow routes, so every one of them
/// silently escaped the 45-second deadline. That bug is fixed here rather than
/// ported; see `INCOMPLETE.md`.
///
/// A prefix matches the path itself or anything beginning with `prefix` followed
/// by a slash, and nothing else. This also stops the image prefix from matching a
/// hypothetical `/api/images`.
#[must_use]
pub fn is_exempt(path: &str) -> bool {
    EXEMPT_PREFIXES
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

/// Enforces the deadline, returning 504 when exceeded.
pub async fn enforce_timeout(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if is_exempt(req.uri().path()) {
        return next.run(req).await;
    }

    match tokio::time::timeout(state.request_timeout, next.run(req)).await {
        Ok(response) => response,
        Err(_elapsed) => {
            tracing::warn!(
                timeout_secs = state.request_timeout.as_secs_f64(),
                "request exceeded the hard timeout"
            );
            (
                StatusCode::GATEWAY_TIMEOUT,
                "request exceeded the server deadline",
            )
                .into_response()
        }
    }
}

/// Reads `REQUEST_HARD_TIMEOUT`, falling back to the documented default.
///
/// An unparseable value is logged and ignored rather than rejected: refusing to
/// start because of a malformed optional tuning knob would be worse than using
/// the default.
#[must_use]
pub fn timeout_from_env() -> Duration {
    let seconds = std::env::var("REQUEST_HARD_TIMEOUT")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or_else(|| {
            tracing::warn!(
                "REQUEST_HARD_TIMEOUT is unset or invalid; using {DEFAULT_TIMEOUT_SECONDS}s"
            );
            DEFAULT_TIMEOUT_SECONDS
        });
    Duration::from_secs_f64(seconds)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_TIMEOUT_SECONDS, EXEMPT_PREFIXES, is_exempt, timeout_from_env};

    #[test]
    fn exempt_prefixes_match_the_python_list_exactly() {
        // app.py:180-192. A missing entry silently breaks a long-running
        // feature, and an extra one silently re-enables the hang this prevents.
        assert_eq!(EXEMPT_PREFIXES.len(), 10);
        for expected in [
            "/api/chat",
            "/api/shell/stream",
            "/api/research",
            "/api/model/download",
            "/api/model/probe",
            "/api/model-endpoints",
            "/api/cookbook/setup",
            "/api/upload",
            "/api/image",
            "/api/memory/audit",
        ] {
            assert!(EXEMPT_PREFIXES.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn exempt_paths_are_not_charged_a_deadline() {
        for path in [
            // Non-streaming chat, which can still run tools and take a while.
            "/api/chat",
            // Stream paths use the segment form; the Python source's
            // `/api/chat_stream` spelling is gone (API_MIGRATION.md §4.1).
            "/api/chat/stream",
            "/api/chat/resume/s1",
            "/api/chat/stop/s1",
            "/api/chat/stream_status/s1",
            "/api/shell/stream",
            "/api/research/stream/s1",
            "/api/model/download",
            "/api/model-endpoints/ep1/probe",
            "/api/cookbook/setup",
            "/api/upload",
            "/api/image/inpaint",
            "/api/memory/audit",
        ] {
            assert!(is_exempt(path), "{path} must be exempt");
        }
    }

    #[test]
    fn ordinary_paths_are_not_exempt() {
        for path in [
            "/api/sessions",
            "/api/notes",
            "/api/chatgpt-subscription/device/start",
            "/api/chatgpt-subscription/device/poll",
            "/api/copilot/device/start",
            "/api/images",
            "/api/model",
            "/api/uploads",
            "/login",
            "/",
        ] {
            assert!(!is_exempt(path), "{path} must not be exempt");
        }
    }

    /// Regression: `app.py:199` tests with `startswith`, so the `/api/chat` entry
    /// also matched `/api/chatgpt-subscription/*` and those device-flow routes
    /// escaped the deadline. Porting that would have silently carried the bug
    /// forward.
    #[test]
    fn a_prefix_does_not_match_a_sibling_with_a_shared_name() {
        assert!(
            !is_exempt("/api/chatgpt-subscription/device/start"),
            "the chat prefix must not swallow the chatgpt-subscription routes"
        );
        assert!(
            is_exempt("/api/chat/stream"),
            "the real chat stream route stays exempt"
        );
        assert!(is_exempt("/api/chat/resume/s1"));
        assert!(
            !is_exempt("/api/images"),
            "the image prefix must not match a longer sibling name"
        );
        assert!(
            !is_exempt("/api/model"),
            "the parent of an exempt route is not itself exempt"
        );
        assert!(is_exempt("/api/model/probe"));
    }

    #[test]
    fn no_exempt_prefix_shadows_another() {
        // "/api/model/download" vs "/api/model/probe" vs "/api/model-endpoints"
        // are siblings; none may be a prefix of another or the shorter one wins.
        for a in EXEMPT_PREFIXES {
            for b in EXEMPT_PREFIXES {
                if a != b {
                    assert!(
                        !b.starts_with(a),
                        "{b} starts with {a}; the shorter prefix would shadow it"
                    );
                }
            }
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "asserting a compile-time constant equals its documented value is the intent"
    )]
    fn default_timeout_matches_the_python_default() {
        assert_eq!(DEFAULT_TIMEOUT_SECONDS, 45.0);
        let unset = timeout_from_env();
        assert!(
            unset.as_secs_f64() > 0.0,
            "must always yield a positive timeout"
        );
    }
}
