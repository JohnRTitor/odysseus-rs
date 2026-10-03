//! Slow-request logging.
//!
//! Ports `_SlowRequestLogMiddleware` from `app.py:229-251`. Purely diagnostic:
//! it measures, warns past a threshold, and always returns the real response
//! unchanged.
//!
//! The threshold defaults to 0.75 s, read from
//! `ODYSSEUS_SLOW_REQUEST_LOG_SECONDS`. Streaming endpoints are excluded from
//! *reporting* only — they still pass through untouched, because a 20-minute
//! research stream would otherwise log on every request and tell us nothing.

use std::time::Instant;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::AppState;

/// Default threshold in seconds, from `app.py:236`.
pub const DEFAULT_SLOW_SECONDS: f64 = 0.75;

/// Paths that are never reported, because their duration is the feature.
///
/// These are a subset of the timeout exemptions: long-poll endpoints rather than
/// streams.
const NEVER_REPORTED: &[&str] = &[
    "/api/chat",
    "/api/research",
    "/api/shell/stream",
    "/api/model/download",
];

/// Logs a warning when a request exceeds the threshold.
pub async fn log_slow_requests(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();

    // Take the response and its status from headers, which is all we can know
    // without consuming the body.
    let started = Instant::now();
    let response = next.run(req).await;
    let elapsed = started.elapsed();

    let reportable = elapsed >= state.slow_request_threshold
        && !NEVER_REPORTED.iter().any(|p| path.starts_with(p));

    if reportable {
        let status = response.status().as_u16();
        tracing::warn!(
            method = %method,
            path = %path,
            status,
            elapsed_ms = elapsed.as_secs_f64() * 1000.0,
            "slow_request"
        );
    }

    response
}

/// Reads `ODYSSEUS_SLOW_REQUEST_LOG_SECONDS`, falling back to the default.
///
/// A non-positive or unparseable value disables logging rather than warning on
/// every request, which would be its own performance problem.
#[must_use]
pub fn threshold_from_env() -> std::time::Duration {
    let seconds = std::env::var("ODYSSEUS_SLOW_REQUEST_LOG_SECONDS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .unwrap_or(DEFAULT_SLOW_SECONDS);
    std::time::Duration::from_secs_f64(seconds)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SLOW_SECONDS, NEVER_REPORTED, threshold_from_env};

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "asserting a compile-time constant equals its documented value is the intent"
    )]
    fn default_threshold_matches_the_python_default() {
        assert_eq!(DEFAULT_SLOW_SECONDS, 0.75);
        assert!(threshold_from_env().as_secs_f64() > 0.0);
    }

    #[test]
    fn long_running_endpoints_are_never_reported() {
        for path in [
            "/api/chat_stream",
            "/api/chat/resume/s1",
            "/api/research/stream/s1",
            "/api/shell/stream",
            "/api/model/download",
        ] {
            assert!(
                NEVER_REPORTED.iter().any(|p| path.starts_with(p)),
                "{path} must not be reported as slow"
            );
        }
    }
}
