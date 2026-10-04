//! T0101/T0103 — the SSR shell must actually render, and the pre-paint script
//! must reach the browser carrying the same nonce the CSP header authorises.
//!
//! These drive the **real production router**, security headers included, for the
//! same reason `middleware_wiring.rs` does: a shell can compile, render an empty
//! body, and stay 200 — and every unit-level assertion on it would still pass.
//! The one failure mode that cannot be caught any other way here is nonce drift,
//! which produces a permanently unstyled page and zero server-side errors.
//!
//! `leptos_routes` is not exercised: the shell is reached through
//! `app::router`, so this covers the path a browser actually takes.

// `clippy.toml` sets `allow-expect-in-tests`, but clippy only treats a `#[test]`
// function as test code, not the fixture helpers below, which do trip
// `expect_used` (denied workspace-wide).
#![expect(
    clippy::expect_used,
    reason = "fixture helpers in an integration test; clippy does not class these as test code"
)]

use {
    axum::{
        body::Body,
        http::{Request, StatusCode},
    },
    std::time::Duration,
    tower::ServiceExt as _,
};

use odysseus_web::AppState;

/// Leptos's out-of-order HTML streaming spawns onto a global executor, and on
/// non-wasm targets Leptos does **not** initialise one — `leptos::task::spawn`
/// reaches `any_spawner::Executor::spawn`, which panics if nothing set it up.
/// The same call belongs in the server's `main`; without it every `/` request
/// aborts mid-render, and no unit test on the components would notice.
fn init_executor_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = any_spawner::Executor::init_tokio();
    });
}

/// A configured install, so `/` is past the auth gate rather than a login redirect.
fn state() -> AppState {
    init_executor_once();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.keep();
    let store = odysseus_store::Store::open(path.join("app.db"));
    store.migrate().expect("migrate");
    let auth = odysseus_store::auth::AuthStore::new(&path);
    auth.prepare().expect("prepare auth store");
    auth.create_user("alice", "correct horse battery", true)
        .expect("create admin");
    AppState {
        api_tokens: std::sync::Arc::new(odysseus_store::api_tokens::ApiTokenStore::new(
            store.clone(),
        )),
        auth: std::sync::Arc::new(auth),
        store: std::sync::Arc::new(store),
        internal_tool_token: std::sync::Arc::from("t"),
        localhost_bypass: true,
        login_limiter: std::sync::Arc::new(odysseus_store::ratelimit::RateLimiter::new(
            odysseus_web::routes::auth::LOGIN_MAX_REQUESTS,
            Duration::from_secs(odysseus_web::routes::auth::LOGIN_WINDOW_SECONDS),
        )),
        setup_limiter: std::sync::Arc::new(odysseus_store::ratelimit::RateLimiter::new(
            odysseus_web::routes::auth::SETUP_MAX_REQUESTS,
            Duration::from_secs(odysseus_web::routes::auth::SETUP_WINDOW_SECONDS),
        )),
        request_timeout: Duration::from_secs(45),
        slow_request_threshold: Duration::from_secs_f64(0.75),
    }
}

/// A request as it arrives from a real socket.
///
/// `ConnectInfo` is not optional decoration: without it the auth gate has no peer
/// address, cannot apply the loopback bypass, and redirects to `/login`. A test
/// that omits it measures the login page, not the shell.
fn from_loopback(uri: &str) -> Request<Body> {
    let mut request = Request::builder()
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [127, 0, 0, 1],
            40_000,
        ))));
    request
}

/// Status, body and the raw CSP header for `GET /`.
async fn get_root() -> (StatusCode, String, Option<String>) {
    let response = odysseus_web::app::router(&state())
        .oneshot(from_loopback("/"))
        .await
        .expect("router responds");
    let status = response.status();
    let csp = response
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .map(ToOwned::to_owned);
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body reads");
    (status, String::from_utf8_lossy(&body).into_owned(), csp)
}

#[tokio::test]
async fn the_root_document_is_server_rendered_html() {
    let (status, body, _) = get_root().await;
    assert_eq!(status, StatusCode::OK, "body was: {body}");
    let lowered = body.to_lowercase();
    assert!(
        lowered.contains("<!doctype html>"),
        "the root must be a full HTML document, not a fragment; body was: {body}"
    );
}

#[tokio::test]
async fn the_title_tag_is_in_the_rendered_head() {
    let (status, body, _) = get_root().await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("<title>Odysseus</title>"),
        "leptos_meta must emit the title into the SSR document; body was: {body}"
    );
}

/// T0103's whole point is that the theme script runs *before* first paint. Missing
/// means a flash of the wrong theme; present-but-divergent from `theme.rs` means
/// permanently wrong colours.
///
/// The expected variable set is derived from `theme.rs` rather than written out
/// here. A hardcoded list would be a third copy of the same ten names, and the
/// test would then pass against a script that had drifted from both — which is
/// the exact failure a third copy invites.
#[tokio::test]
async fn the_served_prepaint_script_sets_every_syntax_variable() {
    let (status, body, _) = get_root().await;
    assert_eq!(status, StatusCode::OK);

    let expected = odysseus_ui::syntax_vars(&odysseus_ui::THEMES[0].1)
        .expect("the first palette entry must derive")
        .iter()
        .map(|(name, _)| name.to_owned())
        .collect::<std::collections::BTreeSet<_>>();

    assert!(
        expected.len() >= 10,
        "the palette should derive at least ten syntax variables, got {}",
        expected.len()
    );
    for var in &expected {
        assert!(
            body.contains(var.as_str()),
            "the served pre-paint script must set {var}; body was: {body}"
        );
    }
}

/// The nonce in the script tag and the nonce in `script-src` are produced by
/// different code paths. If they ever drift, the browser refuses the inline
/// script and the page is unstyled forever, with nothing in the server logs.
#[tokio::test]
async fn the_inline_script_nonce_matches_the_csp_header() {
    let (status, body, csp) = get_root().await;
    assert_eq!(status, StatusCode::OK);
    let csp = csp.expect("CSP header must be present on the shell");

    let header_nonce = csp
        .split("'nonce-")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .filter(|n| !n.is_empty())
        .unwrap_or("");

    assert!(!header_nonce.is_empty(), "no nonce in script-src: {csp}");
    assert!(
        body.contains(&format!("nonce=\"{header_nonce}\"")),
        "the inline script must carry the nonce script-src authorises; \
         header nonce was {header_nonce}, body was: {body}"
    );
}

/// N2: an unimplemented feature must say so. A chat window rendering empty looks
/// broken; one rendering a disabled control with a reason looks unfinished, which
/// is the truth.
#[tokio::test]
async fn the_shell_advertises_that_chat_is_not_built_yet() {
    let (status, body, _) = get_root().await;
    assert_eq!(status, StatusCode::OK);
    let lowered = body.to_lowercase();
    assert!(
        lowered.contains("not") && lowered.contains("chat"),
        "the shell must state chat is unimplemented rather than render an empty window; \
         body was: {body}"
    );
}

/// An unknown path must reach the Leptos `NotFound` route rather than axum's
/// default empty 404 — the shell owns the 404 body, since it owns the document.
#[tokio::test]
async fn an_unknown_path_renders_the_shells_own_not_found_route() {
    let response = odysseus_web::app::router(&state())
        .oneshot(from_loopback("/no/such/page"))
        .await
        .expect("router responds");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body reads");
    let body = String::from_utf8_lossy(&body).into_owned();

    assert_eq!(status, StatusCode::NOT_FOUND, "body was: {body}");
    assert!(
        body.contains("<!doctype html>") || body.to_lowercase().contains("<!doctype html>"),
        "the 404 must come from the Leptos shell, not axum; body was: {body}"
    );
}
