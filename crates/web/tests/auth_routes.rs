//! Integration tests for the `/api/auth` routes.
//!
//! T0064 is the acceptance gate: a real password checked against the real
//! `auth.json`, producing a session in the real `sessions.json` that then
//! authenticates a request through the real gate. Nothing here is stubbed or
//! mocked — the point is the files on disk.

#![expect(
    clippy::expect_used,
    reason = "fixture helpers in an integration test; clippy does not class these as test code"
)]
//!
//! These deliberately reuse the `AppState` shape from `auth_gate.rs`, because the
//! login route is only meaningful if the cookie it sets actually works against
//! the gate.

use {
    axum::{
        Router,
        body::Body,
        http::{Request, StatusCode, header},
        routing::get,
    },
    odysseus_store::auth::AuthStore,
    serde_json::{Value, json},
    std::{net::SocketAddr, sync::Arc},
    tower::ServiceExt as _,
};

use odysseus_web::{
    AppState,
    identity::AuthContext,
    middleware::{auth, cors_layer},
};

struct Harness {
    state: AppState,
    _dir: std::path::PathBuf,
}

impl Harness {
    /// An install with no accounts yet.
    fn empty() -> Self {
        Self::build(None)
    }

    /// An install with `alice`, no second factor.
    fn with_alice() -> Self {
        Self::build(Some(("alice", false)))
    }

    /// An install with `alice` and 2FA enrolled.
    fn with_totp() -> Self {
        Self::build(Some(("alice", true)))
    }

    fn build(user: Option<(&str, bool)>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let store = odysseus_store::Store::open(path.join("app.db"));
        store.migrate().expect("migrate");
        let auth = AuthStore::new(&path);
        auth.prepare().expect("prepare");

        if let Some((name, totp)) = user {
            auth.create_user(name, PASSWORD, true).expect("create");
            if totp {
                // Enrol without confirming: the pending secret is already stored
                // and `totp_enabled` is what the login route branches on, so this
                // is enough to exercise the prompt without a valid code.
                auth.totp_generate_secret(name).expect("generate secret");
                let mut raw = std::fs::read_to_string(auth.auth_path()).expect("read auth.json");
                let doc: Value = serde_json::from_str(&raw).expect("parse");
                let user_obj = &doc["users"][name];
                let mut updated = user_obj.clone();
                updated["totp_enabled"] = json!(true);
                let users = doc["users"].as_object().expect("users object").clone();
                let mut new_users = serde_json::Map::new();
                for (k, v) in users {
                    let chosen = if k == name { updated.clone() } else { v };
                    new_users.insert(k, chosen);
                }
                let mut out = doc.clone();
                out["users"] = Value::Object(new_users);
                raw = serde_json::to_string_pretty(&out).expect("serialise");
                std::fs::write(auth.auth_path(), raw).expect("write auth.json");
            }
        }

        let state = AppState {
            api_tokens: Arc::new(odysseus_store::api_tokens::ApiTokenStore::new(
                store.clone(),
            )),
            auth: Arc::new(auth),
            store: Arc::new(store),
            internal_tool_token: Arc::from("in-process-token"),
            localhost_bypass: false,
            request_timeout: std::time::Duration::from_secs(45),
            slow_request_threshold: std::time::Duration::from_secs_f64(0.75),
        };
        Self { state, _dir: path }
    }

    /// The production stack: the auth routes plus the gate, in the real order.
    fn router(&self) -> Router {
        gated_with_auth_routes(&self.state).with_state(self.state.clone())
    }
}

const PASSWORD: &str = "correct horse battery staple";

/// The production layer order with the `/api/auth` routes mounted: CORS outside,
/// gate inside, routes innermost.
fn gated_with_auth_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api/probe", get(probe_handler))
        .merge(odysseus_web::routes::auth::routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_authentication,
        ))
        .layer(cors_layer())
}

async fn probe_handler(axum::Extension(ctx): axum::Extension<AuthContext>) -> String {
    ctx.effective_user().unwrap_or("anonymous").to_owned()
}

fn from_peer(mut request: Request<Body>, peer: SocketAddr) -> Request<Body> {
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    request
}

fn remote() -> SocketAddr {
    SocketAddr::from(([203, 0, 113, 9], 40_000))
}

async fn send(router: Router, request: Request<Body>) -> axum::response::Response {
    router.oneshot(request).await.expect("oneshot")
}

async fn json_of(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn post(path: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// A plain GET. Named `plain_get` because `get` is axum's router helper.
fn plain_get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request")
}

/// The `session` cookie from a `Set-Cookie` header, value only.
fn session_cookie_from(response: &axum::response::Response) -> Option<String> {
    let raw = response.headers().get(header::SET_COOKIE)?.to_str().ok()?;
    raw.split(';')
        .next()?
        .split_once('=')
        .map(|(_, value)| value.to_owned())
}

// ---------------------------------------------------------------------------
// T0064 — the acceptance gate
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_against_the_real_auth_file_returns_ok_and_sets_a_cookie() {
    // The T0064 shape: a real password checked against the real `auth.json`.
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK, "login must succeed");
    assert!(
        response.headers().contains_key(header::SET_COOKIE),
        "a successful login must set the session cookie"
    );
    let body = json_of(response).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(body["username"], json!("alice"));
}

#[tokio::test]
async fn the_cookie_from_login_authenticates_a_gated_route() {
    let h = Harness::with_alice();
    let login = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);

    let token = session_cookie_from(&login).expect("a session cookie");
    let probe = send(
        h.router(),
        from_peer(
            Request::builder()
                .uri("/api/probe")
                .header(header::COOKIE, format!("session={token}"))
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(probe.status(), StatusCode::OK);
    let body = axum::body::to_bytes(probe.into_body(), 4096)
        .await
        .expect("body");
    assert_eq!(body.as_ref(), b"alice", "the session must name its user");
}

#[tokio::test]
async fn the_session_survives_in_sessions_json_on_disk() {
    // Not a mock assertion: the session must be a real row in the real file, or
    // a restart would log everyone out.
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    let token = session_cookie_from(&response).expect("a session cookie");

    let raw = std::fs::read_to_string(h.state.auth.sessions_path()).expect("sessions.json");
    assert!(
        raw.contains(&token),
        "the issued token must be persisted, not just returned"
    );
}

#[tokio::test]
async fn the_cookie_carries_httponly_and_samesite() {
    // Without HttpOnly, script can read the session out of document.cookie.
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie")
        .to_str()
        .expect("utf-8")
        .to_owned();
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Lax"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
}

#[tokio::test]
async fn remember_me_sets_a_max_age() {
    let h = Harness::with_alice();
    let plain = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    assert!(
        !plain
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("Max-Age"),
        "without remember, the cookie must be session-scoped"
    );

    let remembered = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD, "remember": true }),
            ),
            remote(),
        ),
    )
    .await;
    assert!(
        remembered
            .headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("Max-Age"),
        "remember=true must persist the cookie"
    );
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_wrong_password_is_401_and_sets_no_cookie() {
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": "not the password" }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(
        !response.headers().contains_key(header::SET_COOKIE),
        "a refused login must not hand out a session"
    );
}

#[tokio::test]
async fn an_unknown_user_is_refused_exactly_like_a_wrong_password() {
    // Same status and same body, so the response cannot be used to enumerate
    // accounts.
    let h = Harness::with_alice();
    let unknown = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "nobody", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    let wrong = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": "not the password" }),
            ),
            remote(),
        ),
    )
    .await;

    assert_eq!(unknown.status(), wrong.status());
    assert_eq!(json_of(unknown).await, json_of(wrong).await);
}

#[tokio::test]
async fn login_is_case_and_whitespace_insensitive_on_the_username() {
    // `auth_routes.py:169` lowercases and strips.
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "  ALICE  ", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json_of(response).await["username"], json!("alice"));
}

#[tokio::test]
async fn an_unknown_field_in_the_body_is_rejected() {
    // `deny_unknown_fields`, per AGENTS.md §4: a typo'd field must not be
    // silently ignored.
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD, "is_admin": true }),
            ),
            remote(),
        ),
    )
    .await;
    assert!(
        response.status().is_client_error(),
        "an unknown field must not be accepted, got {}",
        response.status()
    );
}

// ---------------------------------------------------------------------------
// Second factor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_correct_password_with_2fa_enabled_returns_200_asking_for_a_code() {
    // NOT a 401. The frontend intercepts 401 globally and redirects to /login,
    // so a 401 here would bounce the user out mid-login and log a rejection for
    // a password that was correct.
    let h = Harness::with_totp();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the password was accepted; this is not an auth failure"
    );
    assert!(
        !response.headers().contains_key(header::SET_COOKIE),
        "no session before the second factor is satisfied"
    );
    assert!(
        !response.headers().contains_key(header::SET_COOKIE),
        "no session before the second factor is satisfied"
    );
    let body = json_of(response).await;
    assert_eq!(body["ok"], json!(false));
    assert_eq!(body["requires_totp"], json!(true));
    assert_eq!(body["username"], json!("alice"));
    assert!(
        body.get("token").is_none(),
        "the reply must not carry a token in the body"
    );
}

#[tokio::test]
async fn a_wrong_second_factor_code_is_401() {
    let h = Harness::with_totp();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD, "totp_code": "000000" }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
}

#[tokio::test]
async fn a_second_factor_is_not_consulted_when_none_is_enrolled() {
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD, "totp_code": "000000" }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "a stray code must not break a login for a user with no 2FA"
    );
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

#[tokio::test]
async fn setup_creates_the_first_admin_and_then_refuses() {
    let h = Harness::empty();
    let created = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/setup",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(created.status(), StatusCode::OK);
    assert_eq!(json_of(created).await["ok"], json!(true));

    // The admin can now log in, which is what makes this the acceptance path.
    let login = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    assert_eq!(login.status(), StatusCode::OK);

    let again = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/setup",
                &json!({ "username": "mallory", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    assert!(
        again.status().is_client_error(),
        "a second admin must not be creatable, got {}",
        again.status()
    );
    assert!(
        !h.state.auth.user_exists("mallory").expect("exists"),
        "the refused setup must not have created the account"
    );
}

#[tokio::test]
async fn setup_rejects_a_short_password_an_empty_name_and_a_reserved_name() {
    let h = Harness::empty();
    for (body, why) in [
        (
            json!({ "username": "alice", "password": "short" }),
            "short password",
        ),
        (
            json!({ "username": "   ", "password": PASSWORD }),
            "blank username",
        ),
        (
            json!({ "username": "internal-tool", "password": PASSWORD }),
            "reserved name",
        ),
    ] {
        let response = send(
            h.router(),
            from_peer(post("/api/auth/setup", &body), remote()),
        )
        .await;
        assert!(
            response.status().is_client_error(),
            "{why} must be refused, got {}",
            response.status()
        );
    }
}

// ---------------------------------------------------------------------------
// Logout and status
// ---------------------------------------------------------------------------

#[tokio::test]
async fn logout_revokes_the_session_so_the_cookie_stops_working() {
    let h = Harness::with_alice();
    let login = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    let token = session_cookie_from(&login).expect("a session cookie");

    let out = send(
        h.router(),
        from_peer(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .header(header::COOKIE, format!("session={token}"))
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(out.status(), StatusCode::OK);
    assert!(
        out.headers()
            .get(header::SET_COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("Max-Age=0"),
        "logout must clear the cookie in the browser too"
    );

    let after = send(
        h.router(),
        from_peer(
            Request::builder()
                .uri("/api/probe")
                .header(header::COOKIE, format!("session={token}"))
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(
        after.status(),
        StatusCode::UNAUTHORIZED,
        "a revoked session must not keep working"
    );
}

#[tokio::test]
async fn logout_without_a_cookie_is_still_a_success() {
    let h = Harness::with_alice();
    let response = send(
        h.router(),
        from_peer(
            Request::builder()
                .method("POST")
                .uri("/api/auth/logout")
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn status_reports_anonymous_before_login_and_the_user_after() {
    let h = Harness::with_alice();

    let anon = send(
        h.router(),
        from_peer(plain_get("/api/auth/status"), remote()),
    )
    .await;
    assert_eq!(anon.status(), StatusCode::OK, "status must be reachable");
    let body = json_of(anon).await;
    assert_eq!(body["configured"], json!(true));
    assert_eq!(body["authenticated"], json!(false));
    assert_eq!(body["username"], Value::Null);
    assert_eq!(body["is_admin"], json!(false));

    let login = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    let token = session_cookie_from(&login).expect("a session cookie");

    let authed = send(
        h.router(),
        from_peer(
            Request::builder()
                .uri("/api/auth/status")
                .header(header::COOKIE, format!("session={token}"))
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    let body = json_of(authed).await;
    assert_eq!(body["authenticated"], json!(true));
    assert_eq!(body["username"], json!("alice"));
    assert_eq!(body["is_admin"], json!(true));
    assert!(
        body["privileges"].is_object(),
        "the frontend needs the privilege map to dim UI it cannot use: {body}"
    );
}

#[tokio::test]
async fn status_reports_unconfigured_on_a_fresh_install() {
    let h = Harness::empty();
    let response = send(
        h.router(),
        from_peer(plain_get("/api/auth/status"), remote()),
    )
    .await;
    let body = json_of(response).await;
    assert_eq!(body["configured"], json!(false));
    assert_eq!(body["authenticated"], json!(false));
}

#[tokio::test]
async fn policy_is_gated_just_as_it_is_in_python() {
    // `/api/auth/policy` is NOT in Python's `AUTH_EXEMPT_EXACT` (`app.py:264-276`),
    // so it is unreachable before login in the original too. That looks like a
    // Python bug — the handler takes no auth dependency and the route exists
    // precisely so the login form can read the password minimum — but reproducing
    // it is the faithful port, and quietly "fixing" it here would diverge without
    // anyone deciding to. Recorded in INCOMPLETE.md instead.
    let h = Harness::with_alice();

    let anonymous = send(
        h.router(),
        from_peer(plain_get("/api/auth/policy"), remote()),
    )
    .await;
    assert_eq!(
        anonymous.status(),
        StatusCode::UNAUTHORIZED,
        "policy must be gated, matching the Python exempt set"
    );

    // With a session it answers, and reports what the login form needs.
    let login = send(
        h.router(),
        from_peer(
            post(
                "/api/auth/login",
                &json!({ "username": "alice", "password": PASSWORD }),
            ),
            remote(),
        ),
    )
    .await;
    let token = session_cookie_from(&login).expect("a session cookie");
    let authed = send(
        h.router(),
        from_peer(
            Request::builder()
                .uri("/api/auth/policy")
                .header(header::COOKIE, format!("session={token}"))
                .body(Body::empty())
                .expect("request"),
            remote(),
        ),
    )
    .await;
    assert_eq!(authed.status(), StatusCode::OK);
    let body = json_of(authed).await;
    assert_eq!(body["password_min_length"], json!(8));
    assert_eq!(body["session_days"], json!(7));
    assert!(
        body["reserved_usernames"].is_array(),
        "the form must grey out reserved names: {body}"
    );
}
