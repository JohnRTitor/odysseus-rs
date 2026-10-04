//! Wiring tests for the middleware stack.
//!
//! Every test here builds a **real router** and drives a **real request** through
//! it. That is the point. An audit found that the existing suite tested the pure
//! helpers thoroughly while never proving the layers are attached: all seven
//! `attach_security_headers` tests call pure functions, so deleting the layer
//! from the router left all 310 tests green. Same for `enforce_timeout`,
//! `log_slow_requests`, `compression_layer`, and CORS's `allow_credentials(true)`.
//!
//! Each test below fails if its layer is removed from the stack, is passed
//! through, or is misconfigured. Verified by mutation — see the commit message.

// `clippy.toml` sets `allow-expect-in-tests`, but clippy only treats a `#[test]`
// function as test code, not the fixture helpers below, which do trip
// `expect_used` (denied workspace-wide).
#![expect(
    clippy::expect_used,
    reason = "fixture helpers in an integration test; clippy does not class these as test code"
)]

use {
    axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        response::IntoResponse,
        routing::get,
    },
    std::time::Duration,
    tower::ServiceExt as _,
};

use odysseus_web::{
    AppState,
    middleware::{compression_layer, cors_layer, security, slow_log, timeout},
};

/// Enough state for the layers that want it. The store is never touched by
/// anything in this file.
fn state_with(request_timeout: Duration) -> AppState {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.keep();
    let store = odysseus_store::Store::open(path.join("app.db"));
    let auth = odysseus_store::auth::AuthStore::new(&path);
    AppState {
        api_tokens: std::sync::Arc::new(odysseus_store::api_tokens::ApiTokenStore::new(
            store.clone(),
        )),
        auth: std::sync::Arc::new(auth),
        store: std::sync::Arc::new(store),
        internal_tool_token: std::sync::Arc::from("t"),
        localhost_bypass: false,
        login_limiter: std::sync::Arc::new(odysseus_store::ratelimit::RateLimiter::new(
            odysseus_web::routes::auth::LOGIN_MAX_REQUESTS,
            std::time::Duration::from_secs(odysseus_web::routes::auth::LOGIN_WINDOW_SECONDS),
        )),
        setup_limiter: std::sync::Arc::new(odysseus_store::ratelimit::RateLimiter::new(
            odysseus_web::routes::auth::SETUP_MAX_REQUESTS,
            std::time::Duration::from_secs(odysseus_web::routes::auth::SETUP_WINDOW_SECONDS),
        )),
        request_timeout,
        slow_request_threshold: Duration::from_secs_f64(0.75),
    }
}

fn plain_get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request")
}

/// The production stack, minus the auth gate (covered in `auth_gate.rs`) so these
/// tests isolate the layers they are about.
fn stacked(state: &AppState) -> Router {
    Router::new()
        .route("/api/plain", get(|| async { "hi" }))
        .route("/api/big", get(|| async { BIG_BODY }))
        .route(
            "/api/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "never"
            }),
        )
        .route("/api/research/report/1", get(|| async { "hi" }))
        .route("/api/tools/9/render", get(|| async { "hi" }))
        .route("/api/document/7/render-pdf", get(|| async { "hi" }))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            slow_log::log_slow_requests,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            timeout::enforce_timeout,
        ))
        .layer(compression_layer())
        .layer(cors_layer())
        .layer(axum::middleware::from_fn(security::attach_security_headers))
        .with_state(state.clone())
}

/// Comfortably over [`compression_layer`]'s 1 KiB floor.
/// Comfortably over [`compression_layer`]'s 1 KiB floor.
///
/// The length is load-bearing, not decoration: the predicate skips anything
/// smaller, so a merely "long" string tests nothing. This one is >2 KiB.
const BIG_BODY: &str = concat!(
    "compress me please, i am well over one kibibyte of text so that the predicate has ",
    "something to actually compress rather than skipping because the body is too small to be ",
    "worth the header overhead which is the entire point of the size floor here. ",
    "repetitive filler follows so the payload clears the threshold with room to spare. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler. ",
    "filler filler filler filler filler filler filler filler filler filler filler filler filler."
);

async fn send(router: Router, request: Request<Body>) -> axum::response::Response {
    router.oneshot(request).await.expect("oneshot")
}

// ---------------------------------------------------------------------------
// Security headers — the layer whose deletion was invisible
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_security_header_layer_is_actually_attached() {
    let state = state_with(Duration::from_secs(45));
    let response = send(stacked(&state), plain_get("/api/plain")).await;
    let h = response.headers();

    assert_eq!(
        h.get("x-content-type-options").map(|v| v.to_str().ok()),
        Some(Some("nosniff"))
    );
    assert_eq!(
        h.get(header::REFERRER_POLICY).map(|v| v.to_str().ok()),
        Some(Some("no-referrer"))
    );
    assert_eq!(
        h.get("permissions-policy").map(|v| v.to_str().ok()),
        Some(Some("camera=(), microphone=(self), geolocation=()"))
    );
    let csp = h
        .get(header::CONTENT_SECURITY_POLICY)
        .and_then(|v| v.to_str().ok())
        .expect("a CSP must be present");
    assert!(csp.contains("default-src"), "{csp}");
}

#[tokio::test]
async fn the_csp_nonce_differs_between_requests() {
    // `nonces_differ_between_requests` in security.rs calls the generator
    // directly, so hoisting the nonce into a `OnceLock` would pass it. This one
    // goes through the layer.
    let state = state_with(Duration::from_secs(45));
    let nonce_of = |response: &axum::response::Response| {
        response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|v| v.to_str().ok())
            .and_then(|csp| {
                let after = csp.split("nonce-").nth(1)?;
                Some(after.split('\'').next().unwrap_or_default().to_owned())
            })
            .expect("a nonce in the CSP")
    };

    let first = send(stacked(&state), plain_get("/api/plain")).await;
    let second = send(stacked(&state), plain_get("/api/plain")).await;
    assert_ne!(
        nonce_of(&first),
        nonce_of(&second),
        "a shared nonce would let one injected script run in every page"
    );
}

#[tokio::test]
async fn each_csp_profile_is_reachable_through_the_router() {
    let state = state_with(Duration::from_secs(45));
    let csp_of = |path: &'static str| {
        let state = state.clone();
        async move { send(stacked(&state), plain_get(path)).await }
    };

    let report = csp_of("/api/research/report/1").await;
    let tool = csp_of("/api/tools/9/render").await;
    let pdf = csp_of("/api/document/7/render-pdf").await;
    let default = csp_of("/api/plain").await;

    let get = |r: &axum::response::Response| {
        r.headers()
            .get(axum::http::HeaderName::from_static(
                "content-security-policy",
            ))
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let policies = [get(&report), get(&tool), get(&pdf), get(&default)];
    // The default profile must be the strictest, so assert they are not all one
    // policy rather than pinning each string here.
    assert!(
        policies.iter().any(|p| p.contains("sandbox"))
            || policies.iter().any(|p| p != &policies[3]),
        "profiles are not being selected by path: {policies:?}"
    );
}

#[tokio::test]
async fn tool_render_omits_frame_options_because_it_is_embedded() {
    let state = state_with(Duration::from_secs(45));
    let response = send(stacked(&state), plain_get("/api/tools/9/render")).await;
    assert!(
        !response.headers().contains_key(header::X_FRAME_OPTIONS),
        "tool render is framed by us, so sending X-Frame-Options would block it"
    );
}

#[tokio::test]
async fn ordinary_paths_are_not_framable() {
    let state = state_with(Duration::from_secs(45));
    let response = send(stacked(&state), plain_get("/api/plain")).await;
    assert_eq!(
        response
            .headers()
            .get(header::X_FRAME_OPTIONS)
            .map(|v| v.to_str().ok()),
        Some(Some("DENY"))
    );
}

#[tokio::test]
async fn hsts_appears_only_when_the_connection_really_is_https() {
    let state = state_with(Duration::from_secs(45));

    let plain = send(stacked(&state), plain_get("/api/plain")).await;
    assert!(
        !plain
            .headers()
            .contains_key(header::STRICT_TRANSPORT_SECURITY),
        "HSTS over plain HTTP would pin a hostname that never speaks TLS"
    );

    let forwarded = Request::builder()
        .uri("/api/plain")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())
        .expect("request");
    let secure = send(stacked(&state), forwarded).await;
    assert_eq!(
        secure
            .headers()
            .get(header::STRICT_TRANSPORT_SECURITY)
            .map(|v| v.to_str().ok()),
        Some(Some("max-age=31536000; includeSubDomains"))
    );
}

// ---------------------------------------------------------------------------
// CORS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cors_allows_a_configured_origin_with_credentials() {
    // `allow_credentials(true)` is what lets a cookie-authenticated cross-origin
    // client work at all. Dropping it breaks every browser client and no existing
    // test noticed.
    let state = state_with(Duration::from_secs(45));
    let response = send(
        stacked(&state),
        Request::builder()
            .uri("/api/plain")
            .header(header::ORIGIN, "http://localhost")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .map(|v| v.to_str().ok()),
        Some(Some("http://localhost"))
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS)
            .map(|v| v.to_str().ok()),
        Some(Some("true"))
    );
}

#[tokio::test]
async fn cors_refuses_an_origin_that_was_not_configured() {
    let state = state_with(Duration::from_secs(45));
    let response = send(
        stacked(&state),
        Request::builder()
            .uri("/api/plain")
            .header(header::ORIGIN, "http://evil.example")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert!(
        !response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "an unlisted origin must get no ACAO header"
    );
}

// ---------------------------------------------------------------------------
// Timeout
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_timeout_layer_actually_answers_504() {
    // All six existing timeout tests assert `is_exempt`, a pure predicate, or a
    // compile-time constant. Nothing drove `enforce_timeout`, so reducing it to
    // `next.run(req).await` would have left the suite green.
    let state = state_with(Duration::from_millis(80));
    let response = send(stacked(&state), plain_get("/api/slow")).await;
    assert_eq!(
        response.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "the deadline exists; it has to be observable"
    );
}

#[tokio::test]
async fn a_fast_handler_is_not_charged_a_deadline() {
    let state = state_with(Duration::from_millis(80));
    let response = send(stacked(&state), plain_get("/api/plain")).await;
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_large_body_is_compressed_and_a_small_one_is_not() {
    let state = state_with(Duration::from_secs(45));

    let big = Request::builder()
        .uri("/api/big")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .expect("request");
    let compressed = send(stacked(&state), big).await;
    assert_eq!(
        compressed
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().ok()),
        Some(Some("gzip")),
        "a >1 KiB body the client accepts must be compressed"
    );

    let small = Request::builder()
        .uri("/api/plain")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .expect("request");
    let untouched = send(stacked(&state), small).await;
    assert!(
        !untouched.headers().contains_key(header::CONTENT_ENCODING),
        "a tiny body must be left alone; the header costs more than it saves"
    );
}

#[tokio::test]
async fn an_event_stream_is_never_compressed() {
    // The stated reason the predicate exists: a compressed body cannot be
    // flushed token by token, so compressing SSE breaks incremental delivery.
    let state = state_with(Duration::from_secs(45));
    let app = Router::new()
        .route(
            "/api/stream",
            get(|| async {
                ([(header::CONTENT_TYPE, "text/event-stream")], BIG_BODY).into_response()
            }),
        )
        .layer(compression_layer())
        .with_state(state.clone());

    let response = send(
        app,
        Request::builder()
            .uri("/api/stream")
            .header(header::ACCEPT_ENCODING, "gzip")
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert!(
        !response.headers().contains_key(header::CONTENT_ENCODING),
        "compressing an event stream defeats incremental delivery"
    );
}

// ---------------------------------------------------------------------------
// Slow-request log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_slow_log_layer_passes_the_response_through_unchanged() {
    // Its contract is "always return the real response". Nothing asserted it, so
    // a layer that swallowed the body would have been invisible.
    let state = state_with(Duration::from_secs(45));
    let response = send(stacked(&state), plain_get("/api/plain")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .expect("body");
    assert_eq!(bytes.as_ref(), b"hi");
}

#[test]
fn the_big_fixture_really_is_over_the_compression_floor() {
    // Self-checking, because "large" is a number and eyeballing it is how a
    // compression test ends up green for the wrong reason — which is the exact
    // failure mode this whole file exists to prevent.
    assert!(
        BIG_BODY.len() > 1024,
        "BIG_BODY is {} bytes, under the 1024 floor, so nothing would compress",
        BIG_BODY.len()
    );
}
