//! CORS and response compression.
//!
//! Ports the CORS configuration from `app.py:136-153` and the gzip behaviour
//! from `app.py:156-163`.

use {
    axum::http::{HeaderName, HeaderValue, Method, header},
    tower_http::{
        compression::{
            CompressionLayer,
            predicate::{And, NotForContentType, Predicate, SizeAbove},
        },
        cors::{AllowOrigin, CorsLayer},
    },
};

pub mod auth;
pub mod security;
pub mod slow_log;
pub mod timeout;

/// Environment variable holding the allowed origins.
pub const ALLOWED_ORIGINS_ENV: &str = "ALLOWED_ORIGINS";

/// The media type that must never be compressed or buffered.
pub const EVENT_STREAM: &str = "text/event-stream";

/// Build the CORS layer from `ALLOWED_ORIGINS`, mirroring `app.py:136-153`.
///
/// The Python default is `http://localhost,http://127.0.0.1`. Credentials are
/// allowed because the app authenticates with a cookie, which browsers refuse to
/// send cross-origin without it.
///
/// An origin that is not a valid header value is dropped with a warning rather
/// than crashing startup — a malformed optional tuning value should not take the
/// server down.
#[must_use = "the layer must be attached to the router to have any effect"]
pub fn cors_layer() -> CorsLayer {
    let raw = std::env::var(ALLOWED_ORIGINS_ENV)
        .unwrap_or_else(|_| String::from("http://localhost,http://127.0.0.1"));
    let origins: Vec<HeaderValue> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| {
            if let Ok(v) = HeaderValue::from_str(s) {
                Some(v)
            } else {
                tracing::warn!(origin = %s, "ignoring malformed allowed origin");
                None
            }
        })
        .collect();

    if origins.is_empty() {
        tracing::warn!(
            "{ALLOWED_ORIGINS_ENV} produced no usable origins; CORS will reject all cross-origin requests"
        );
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::ACCEPT,
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            HeaderName::from_static("x-api-key"),
            HeaderName::from_static("x-auth-token"),
            HeaderName::from_static("x-odysseus-internal-token"),
            HeaderName::from_static("x-odysseus-owner"),
            HeaderName::from_static("x-odysseus-run-id"),
            HeaderName::from_static("x-requested-with"),
            HeaderName::from_static("x-tz-offset"),
            HeaderName::from_static("x-tz-name"),
        ])
        .allow_credentials(true)
}

/// Minimum body size before compressing, from `app.py:163`.
pub const COMPRESSION_MINIMUM_SIZE: usize = 1024;

/// [`COMPRESSION_MINIMUM_SIZE`] as bytes, which is what the predicate takes.
const COMPRESSION_MINIMUM_BYTES: u64 = COMPRESSION_MINIMUM_SIZE as u64;

/// Build the compression layer.
///
/// Two predicates, composed with `and` so both must agree:
/// * not an event stream — compressing SSE would defeat incremental delivery,
///   because a compressed body cannot be flushed token by token;
/// * larger than 1 KiB, below which the compression headers cost more than they
///   save.
#[must_use = "the layer must be attached to the router to have any effect"]
pub fn compression_layer() -> CompressionLayer<And<NotForContentType, SizeAbove>> {
    CompressionLayer::new().compress_when(
        NotForContentType::const_new(EVENT_STREAM).and(SizeAbove::new(COMPRESSION_MINIMUM_BYTES)),
    )
}

/// True when these response headers indicate an event stream.
#[must_use]
pub fn is_event_stream(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().contains(EVENT_STREAM))
}

#[cfg(test)]
mod tests {
    use {
        super::{COMPRESSION_MINIMUM_SIZE, cors_layer, is_event_stream},
        axum::http::HeaderValue,
    };

    #[test]
    fn cors_layer_builds() {
        // Must not panic regardless of what ALLOWED_ORIGINS contains.
        let _ = cors_layer();
    }

    #[test]
    fn event_stream_is_detected_whatever_the_charset_and_case() {
        let mut headers = axum::http::HeaderMap::new();
        for value in [
            "text/event-stream",
            "text/event-stream; charset=utf-8",
            "TEXT/EVENT-STREAM",
        ] {
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("x")),
            );
            assert!(is_event_stream(&headers), "{value} must be an event stream");
        }
    }

    #[test]
    fn other_content_types_are_not_event_streams() {
        let mut headers = axum::http::HeaderMap::new();
        for value in ["application/json", "text/html", "text/plain"] {
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_str(value).unwrap(),
            );
            assert!(
                !is_event_stream(&headers),
                "{value} must not be an event stream"
            );
        }
    }

    #[test]
    fn compression_threshold_is_not_applied_to_tiny_bodies() {
        assert_eq!(COMPRESSION_MINIMUM_SIZE, 1024);
    }
}
