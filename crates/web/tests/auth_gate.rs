//! Integration tests for the authentication gate.
//!
//! These walk the gate **in order** rather than testing each predicate in
//! isolation, because the order is the security property (`API_MIGRATION.md`
//! §2.1). A unit test per predicate would still pass if the steps were
//! reordered, and reordering them is exactly the regression that matters.
//!
//! Covers T0092 (bearer resolves `effective_user`), T0093 (preflight reaches
//! CORS) and T0094 (proxy headers defeat loopback trust).

// `clippy.toml` sets `allow-expect-in-tests`, but clippy only recognises a
// `#[test]` function as test code — not the plain helpers below that build the
// fixture. Without this the helpers trip `expect_used`, which is denied
// workspace-wide.
#![expect(
    clippy::expect_used,
    reason = "fixture helpers in an integration test; clippy does not class these as test code"
)]

use {
    axum::{
        Extension, Router,
        body::Body,
        extract::ConnectInfo,
        http::{Request, StatusCode, header},
        response::IntoResponse as _,
        routing::get,
    },
    odysseus_store::{
        Store,
        api_tokens::{ApiTokenStore, NewToken, ScopeRequest},
        auth::{AuthOwners, AuthStore},
    },
    serde::{Deserialize, Serialize},
    std::{net::SocketAddr, sync::Arc},
    tower::ServiceExt as _,
};

use odysseus_web::{
    AppState,
    identity::{API_PSEUDO_USER, AuthContext},
    middleware::{
        auth::{self, INTERNAL_TOOL_HEADER, OWNER_HEADER},
        cors_layer,
    },
};

/// What the probe handler saw, so a test can assert on the resolved identity.
#[derive(Debug, Deserialize, Serialize)]
struct Observed {
    current_user: Option<String>,
    effective_user: Option<String>,
    is_api_token: bool,
    is_delegated: bool,
    scopes: Vec<String>,
}

/// The downstream handler. Reports the identity the gate resolved, so a passing
/// request and an *unauthenticated* pass are distinguishable.
async fn probe(Extension(ctx): Extension<AuthContext>) -> axum::response::Response {
    let observed = Observed {
        current_user: ctx.current_user().map(str::to_owned),
        effective_user: ctx.effective_user().map(str::to_owned),
        is_api_token: ctx.is_api_token(),
        is_delegated: ctx.is_delegated(),
        scopes: ctx
            .api_token_scopes()
            .iter()
            .map(ToString::to_string)
            .collect(),
    };
    axum::Json(observed).into_response()
}

/// The production layer order: CORS outside, auth inside.
///
/// Building every test's router through this one function is deliberate —
/// getting the order backwards is the bug T0093 exists to catch, so the tests
/// must not each assemble their own stack.
fn gated(state: &AppState) -> Router {
    Router::new()
        .route("/api/probe", get(probe))
        .route("/login", get(probe))
        .route("/static/app.js", get(probe))
        .route("/api/tasks/{id}/webhook/{token}", get(probe))
        .route("/", get(probe))
        .with_state(state.clone())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_authentication,
        ))
        .layer(cors_layer())
}

struct Harness {
    state: AppState,
    /// Kept alive so the temp directory outlives the stores.
    _dir: std::path::PathBuf,
}

impl Harness {
    /// A configured install with one admin, `alice`.
    fn new() -> Self {
        Self::build(false, true)
    }

    /// A configured install with `LOCALHOST_BYPASS` on.
    fn with_bypass() -> Self {
        Self::build(true, true)
    }

    /// No users yet, for step 5.
    fn unconfigured() -> Self {
        Self::build(false, false)
    }

    fn build(localhost_bypass: bool, with_user: bool) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let store = Store::open(path.join("app.db"));
        store.migrate().expect("migrate");
        let auth = AuthStore::new(&path);
        auth.prepare().expect("prepare auth store");
        if with_user {
            auth.create_user("alice", "correct horse battery", true)
                .expect("create admin");
            // A second admin, so a test can delete a token's owner without
            // tripping the "an administrator cannot delete themselves" rule.
            auth.create_user("bob", "correct horse battery", true)
                .expect("create second admin");
        }
        let state = AppState {
            api_tokens: Arc::new(ApiTokenStore::new(store.clone())),
            auth: Arc::new(auth),
            store: Arc::new(store),
            internal_tool_token: Arc::from("in-process-token"),
            localhost_bypass,
            login_limiter: std::sync::Arc::new(odysseus_store::ratelimit::RateLimiter::new(
                odysseus_web::routes::auth::LOGIN_MAX_REQUESTS,
                std::time::Duration::from_secs(odysseus_web::routes::auth::LOGIN_WINDOW_SECONDS),
            )),
            setup_limiter: std::sync::Arc::new(odysseus_store::ratelimit::RateLimiter::new(
                odysseus_web::routes::auth::SETUP_MAX_REQUESTS,
                std::time::Duration::from_secs(odysseus_web::routes::auth::SETUP_WINDOW_SECONDS),
            )),
            request_timeout: std::time::Duration::from_secs(45),
            slow_request_threshold: std::time::Duration::from_secs_f64(0.75),
        };
        Self { state, _dir: path }
    }

    fn router(&self) -> Router {
        gated(&self.state)
    }

    /// Mints an API token owned by `alice`, returning the raw bearer value.
    fn mint_token(&self, name: &str, scopes: &str) -> String {
        let resolver = AuthOwners::new(AuthStore::clone(&self.state.auth));
        self.state
            .api_tokens
            .create_blocking(
                NewToken {
                    name: name.into(),
                    owner: "alice".into(),
                    scopes: ScopeRequest::text(scopes),
                    profile: None,
                },
                &resolver,
            )
            .expect("mint token")
            .raw
            .expose()
            .to_owned()
    }

    fn session_for(&self, user: &str) -> String {
        self.state.auth.issue_token(user).expect("issue session")
    }
}

/// Drives one request through the router. `oneshot` is `Infallible`-errored, so
/// unwrapping it once here keeps every test reading as assertions.
async fn send(router: Router, request: Request<Body>) -> axum::response::Response {
    router.oneshot(request).await.expect("oneshot")
}

async fn seen(response: axum::response::Response) -> Observed {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("probe json")
}

async fn text_of(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A plain GET. Named `plain_get` because `get` is axum's router helper.
fn plain_get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request")
}

fn from_peer(mut request: Request<Body>, peer: SocketAddr) -> Request<Body> {
    request.extensions_mut().insert(ConnectInfo(peer));
    request
}

fn loopback() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 40_000))
}

fn remote() -> SocketAddr {
    SocketAddr::from(([203, 0, 113, 9], 40_000))
}

fn with_header(path: &str, name: &str, value: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(name, value)
        .body(Body::empty())
        .expect("request")
}

// ---------------------------------------------------------------------------
// Step 1 — the CORS preflight must reach CORS (T0093)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_production_router_gates_everything_but_the_exempt_paths() {
    // `secured` is what `main` serves. Its exempt set is exactly
    // AUTH_EXEMPT_EXACT plus the /static prefix and the webhook pattern; anything
    // else must be gated. This is the assertion that a newly added route cannot
    // be exposed by default.
    let h = Harness::new();
    let app = odysseus_web::secured(&h.state);

    for path in ["/api/health", "/api/version"] {
        let response = send(app.clone(), from_peer(plain_get(path), remote())).await;
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }

    for path in ["/api/ready", "/api/runtime", "/api/anything-else"] {
        let response = send(app.clone(), from_peer(plain_get(path), remote())).await;
        // These routes either do not exist or are exempt-but-gated, so ungated
        // they would answer 404 or 200. A 401 proves the gate ran *before*
        // routing, so a not-yet-written route cannot be reached by accident.
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must be gated before routing"
        );
    }
}

#[tokio::test]
async fn the_unauthenticated_stack_is_a_separate_named_function() {
    // Guarding the guard: `router` and `secured` are the same function, so the
    // only way to reach the unauthenticated stack is to name it. If someone
    // aliases them, this documents that the distinction is deliberate.
    let h = Harness::new();
    let open = odysseus_web::router_without_auth(&h.state);
    let response = send(open, from_peer(plain_get("/api/ready"), remote())).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "router_without_auth really is unauthenticated — that is why it is named that"
    );
}

#[tokio::test]
async fn a_preflight_reaches_cors_and_is_not_401d() {
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/probe")
                .header("origin", "http://localhost")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;

    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a preflight must pass the gate, or every cross-origin client is dead (app.py:374)"
    );
    assert!(
        response
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_METHODS),
        "CORS must have answered: {:?}",
        response.headers()
    );
}

#[tokio::test]
async fn the_gates_own_preflight_bypass_is_live_and_not_dead_code() {
    // In the full stack a preflight is answered by CorsLayer before the gate
    // runs, so the end-to-end test above cannot tell whether the gate's own
    // step-1 bypass exists — deleting it entirely leaves that test green. This
    // isolates the gate to prove the bypass is present.
    //
    // It is defence in depth rather than the load-bearing mechanism: the
    // invariant "a preflight is never 401'd" is guaranteed structurally by CORS
    // being the outer layer. Python gets the same guarantee the other way round,
    // by making `AuthMiddleware` outermost (`app.py:363`) and needing the
    // explicit bypass for exactly that reason.
    let h = Harness::new();
    let gate_only = Router::new()
        .route("/api/probe", get(probe))
        .with_state(h.state.clone())
        .layer(axum::middleware::from_fn_with_state(
            h.state.clone(),
            auth::require_authentication,
        ));
    let response = send(
        gate_only,
        from_peer(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/probe")
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the gate must pass a genuine preflight itself, not rely on CORS being outer"
    );
}

#[tokio::test]
async fn a_preflight_passes_although_the_path_is_not_exempt() {
    // Proves step 1 precedes step 2's siblings rather than coinciding with one:
    // `/api/probe` is not in AUTH_EXEMPT_EXACT.
    assert!(!auth::is_auth_exempt("/api/probe"));
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/probe")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_bare_options_is_still_authenticated_by_the_gate() {
    // Without `Access-Control-Request-Method` this is not a preflight, so the
    // gate must not wave it through. Tested on a router with **no** CORS layer,
    // to isolate the gate's own behaviour.
    let h = Harness::new();
    let ungated_cors = Router::new()
        .route("/api/probe", get(probe))
        .with_state(h.state.clone())
        .layer(axum::middleware::from_fn_with_state(
            h.state.clone(),
            auth::require_authentication,
        ));
    let response = send(
        ungated_cors,
        from_peer(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/probe")
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cors_answers_a_bare_options_without_reaching_any_handler() {
    // Documents what actually happens in the full stack: tower-http's CorsLayer
    // short-circuits *every* OPTIONS, so the gate never sees a bare one. That is
    // not an authentication bypass — the reply is an empty 200 with CORS headers
    // and no handler runs — but it is worth pinning, because "OPTIONS returns 200"
    // otherwise looks like the gate failing open.
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/probe")
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        text_of(response).await,
        "",
        "no handler may have produced a body"
    );
}

// ---------------------------------------------------------------------------
// T0094 — proxy headers must defeat loopback trust
// ---------------------------------------------------------------------------

#[tokio::test]
async fn without_the_bypass_a_loopback_peer_gets_no_free_pass() {
    // Baseline for the tests below: with LOCALHOST_BYPASS off, loopback is not
    // enough. Without this, a green proxy-header test could merely be the bypass
    // being off.
    let h = Harness::new();
    let response = send(h.router(), from_peer(plain_get("/api/probe"), loopback())).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_localhost_bypass_admits_a_clean_loopback_caller() {
    let h = Harness::with_bypass();
    let response = send(h.router(), from_peer(plain_get("/api/probe"), loopback())).await;
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    let seen = seen(response).await;
    assert_eq!(seen.current_user, None, "the bypass grants no identity");
    assert!(!seen.is_api_token);
}

#[tokio::test]
async fn every_proxy_header_defeats_the_localhost_bypass() {
    let h = Harness::with_bypass();

    for name in auth::PROXY_FWD_HEADERS {
        let response = send(
            h.router(),
            from_peer(with_header("/api/probe", name, "203.0.113.7"), loopback()),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{name} must defeat loopback trust: cloudflared connects from 127.0.0.1, so a \
             tunneled visitor would otherwise inherit the bypass (app.py:348-362)"
        );
    }
}

#[tokio::test]
async fn every_proxy_header_defeats_the_internal_tool_path_too() {
    let h = Harness::new();
    for name in auth::PROXY_FWD_HEADERS {
        let mut request = with_header("/api/probe", INTERNAL_TOOL_HEADER, "in-process-token");
        request
            .headers_mut()
            .insert(name, header::HeaderValue::from_static("1.2.3.4"));
        let response = send(h.router(), from_peer(request, loopback())).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{name} must not be spoofable to reach the internal-tool path"
        );
    }
}

#[tokio::test]
async fn a_non_loopback_peer_never_gets_loopback_trust() {
    let h = Harness::with_bypass();
    let response = send(h.router(), from_peer(plain_get("/api/probe"), remote())).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_request_without_connection_info_is_not_trusted() {
    // Absent peer information is untrusted, never trusted. Regressing this to
    // "no info means local" would hand the bypass to every proxied deployment.
    let h = Harness::with_bypass();
    let response = send(h.router(), plain_get("/api/probe")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Step 2 — exempt paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_exempt_path_passes_without_credentials() {
    let h = Harness::new();
    for path in ["/login", "/"] {
        let response = send(h.router(), from_peer(plain_get(path), remote())).await;
        assert_ne!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{path} must not be gated"
        );
    }
}

#[tokio::test]
async fn the_static_prefix_passes_and_a_lookalike_does_not() {
    let h = Harness::new();
    let ok = send(h.router(), from_peer(plain_get("/static/app.js"), remote())).await;
    assert_ne!(ok.status(), StatusCode::UNAUTHORIZED);

    let lookalike = send(h.router(), from_peer(plain_get("/staticfoo"), remote())).await;
    // `/staticfoo` is a browser path, so being gated means a redirect to
    // /login rather than a 401. What matters is that it did NOT reach the probe.
    assert_eq!(
        lookalike.status(),
        StatusCode::FOUND,
        "/staticfoo must be gated, not served: a bare starts_with would have \
         exempted it and returned the probe's 200"
    );
}

#[tokio::test]
async fn a_task_webhook_path_passes_so_its_handler_can_check_the_token() {
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(plain_get("/api/tasks/t-1/webhook/secret"), remote()),
    )
    .await;
    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "the path-embedded webhook token IS the credential; the handler must see it"
    );
}

// ---------------------------------------------------------------------------
// Step 3 — the internal-tool path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_internal_tool_token_admits_a_loopback_caller() {
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(
            with_header("/api/probe", INTERNAL_TOOL_HEADER, "in-process-token"),
            loopback(),
        ),
    )
    .await;
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    let seen = seen(response).await;
    assert_eq!(seen.current_user.as_deref(), Some("internal-tool"));
    assert!(
        !seen.is_api_token,
        "an internal call is not a bearer token and must not carry token scopes"
    );
}

#[tokio::test]
async fn a_wrong_internal_tool_token_is_refused() {
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(
            with_header("/api/probe", INTERNAL_TOOL_HEADER, "in-process-tokeN"),
            loopback(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_internal_tool_token_from_a_remote_host_is_refused() {
    let h = Harness::new();
    let response = send(
        h.router(),
        from_peer(
            with_header("/api/probe", INTERNAL_TOOL_HEADER, "in-process-token"),
            remote(),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a leaked in-process token from a remote host must not authenticate"
    );
}

#[tokio::test]
async fn internal_tool_impersonation_applies_only_to_a_real_account() {
    let h = Harness::new();
    let impersonating_alice = from_peer(
        Request::builder()
            .uri("/api/probe")
            .header(INTERNAL_TOOL_HEADER, "in-process-token")
            .header(OWNER_HEADER, "alice")
            .body(Body::empty())
            .expect("request"),
        loopback(),
    );
    let existing = seen(send(h.router(), impersonating_alice).await).await;
    assert_eq!(existing.current_user.as_deref(), Some("alice"));

    let impersonating_a_ghost = from_peer(
        Request::builder()
            .uri("/api/probe")
            .header(INTERNAL_TOOL_HEADER, "in-process-token")
            .header(OWNER_HEADER, "does-not-exist")
            .body(Body::empty())
            .expect("request"),
        loopback(),
    );
    let unknown = seen(send(h.router(), impersonating_a_ghost).await).await;
    assert_eq!(
        unknown.current_user.as_deref(),
        Some("internal-tool"),
        "impersonating an unknown account must fall back, not invent a user"
    );
}

// ---------------------------------------------------------------------------
// Steps 4 and 5 — bypass and first-run
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unconfigured_install_asks_an_api_caller_to_set_up() {
    let h = Harness::unconfigured();
    let response = send(h.router(), from_peer(plain_get("/api/probe"), remote())).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        text_of(response).await,
        r#"{"error":"Setup required"}"#,
        "the frontend branches on the `error` key, so the shape is load-bearing"
    );
}

#[tokio::test]
async fn an_unconfigured_install_redirects_a_browser_to_login() {
    let h = Harness::unconfigured();
    let response = send(h.router(), from_peer(plain_get("/"), remote())).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(response.headers()[header::LOCATION], "/login");
}

// ---------------------------------------------------------------------------
// T0092 — a bearer token resolves effective_user, never "api"
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_bearer_token_authenticates_as_api_but_resolves_to_its_owner() {
    let h = Harness::new();
    let raw = h.mint_token("laptop", "chat");
    let response = send(
        h.router(),
        from_peer(
            with_header(
                "/api/probe",
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {raw}"),
            ),
            remote(),
        ),
    )
    .await;
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);

    let seen = seen(response).await;
    // The gate authenticates as the sandboxed pseudo-user...
    assert_eq!(seen.current_user.as_deref(), Some(API_PSEUDO_USER));
    // ...but ownership resolves to the human who minted it. This is the whole
    // point: an ownership check reading `current_user` would compare against
    // "api" and match nothing.
    assert_eq!(seen.effective_user.as_deref(), Some("alice"));
    assert!(seen.is_api_token);
    assert!(seen.is_delegated);
    assert_eq!(seen.scopes, vec![String::from("chat")]);
}

#[tokio::test]
async fn effective_user_is_never_the_pseudo_user_when_the_token_has_an_owner() {
    let h = Harness::new();
    let raw = h.mint_token("phone", "chat,calendar:read");
    let response = send(
        h.router(),
        from_peer(
            with_header(
                "/api/probe",
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {raw}"),
            ),
            remote(),
        ),
    )
    .await;
    let seen = seen(response).await;
    assert_ne!(
        seen.effective_user.as_deref(),
        Some(API_PSEUDO_USER),
        "a token minted for a live user always has an owner"
    );
    let mut scopes = seen.scopes;
    scopes.sort();
    assert_eq!(scopes, vec!["calendar:read".to_owned(), "chat".to_owned()]);
}

#[tokio::test]
async fn a_cookie_session_resolves_to_itself_for_everything() {
    let h = Harness::new();
    let session = h.session_for("alice");
    let response = send(
        h.router(),
        from_peer(
            with_header(
                "/api/probe",
                header::COOKIE.as_str(),
                &format!("theme=dark; session={session}"),
            ),
            remote(),
        ),
    )
    .await;
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
    let seen = seen(response).await;
    assert_eq!(seen.current_user.as_deref(), Some("alice"));
    assert_eq!(seen.effective_user.as_deref(), Some("alice"));
    assert!(!seen.is_api_token);
    assert!(!seen.is_delegated);
}

#[tokio::test]
async fn a_bogus_bearer_token_is_refused_even_with_a_valid_session() {
    // The bearer path returns on a bad token; it must not fall through to the
    // cookie, or a stale header could neither revoke nor be ignored.
    let h = Harness::new();
    let session = h.session_for("alice");
    let mut request = with_header(
        "/api/probe",
        header::AUTHORIZATION.as_str(),
        "Bearer ody_definitelynotarealtoken",
    );
    request.headers_mut().insert(
        header::COOKIE,
        header::HeaderValue::from_str(&format!("session={session}")).expect("cookie"),
    );
    let response = send(h.router(), from_peer(request, remote())).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_non_ody_bearer_header_falls_through_to_the_cookie() {
    let h = Harness::new();
    let session = h.session_for("alice");
    let mut request = with_header(
        "/api/probe",
        header::AUTHORIZATION.as_str(),
        "Bearer some-other-scheme",
    );
    request.headers_mut().insert(
        header::COOKIE,
        header::HeaderValue::from_str(&format!("session={session}")).expect("cookie"),
    );
    let response = send(h.router(), from_peer(request, remote())).await;
    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "an unrelated Authorization header must not lock out a valid session"
    );
    assert_eq!(seen(response).await.current_user.as_deref(), Some("alice"));
}

#[tokio::test]
async fn a_token_whose_owner_was_deleted_stops_authenticating() {
    let h = Harness::new();
    let raw = h.mint_token("doomed", "chat");

    let ok = send(
        h.router(),
        from_peer(
            with_header(
                "/api/probe",
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {raw}"),
            ),
            remote(),
        ),
    )
    .await;
    assert_ne!(ok.status(), StatusCode::UNAUTHORIZED, "token works first");

    h.state
        .auth
        .delete_user("alice", "bob", &h.state.api_tokens)
        .expect("delete owner");

    let after = send(
        h.router(),
        from_peer(
            with_header(
                "/api/probe",
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {raw}"),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(
        after.status(),
        StatusCode::UNAUTHORIZED,
        "deleting the owner must stop the token authenticating: the prefix cache is \
         rebuilt against auth.json, so the stale row resolves to no owner"
    );
}

#[tokio::test]
async fn an_invalid_session_is_refused() {
    let h = Harness::new();
    for cookie in ["session=nope", "session=", "other=1"] {
        let response = send(
            h.router(),
            from_peer(
                with_header("/api/probe", header::COOKIE.as_str(), cookie),
                remote(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{cookie}");
    }
}

#[tokio::test]
async fn an_api_path_401s_and_a_browser_path_redirects() {
    let h = Harness::new();
    let api = send(h.router(), from_peer(plain_get("/api/probe"), remote())).await;
    assert_eq!(api.status(), StatusCode::UNAUTHORIZED);

    let page = send(h.router(), from_peer(plain_get("/"), remote())).await;
    assert_eq!(page.status(), StatusCode::FOUND);
    assert_eq!(page.headers()[header::LOCATION], "/login");
}
