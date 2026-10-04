//! The `/api/auth` routes.
//!
//! Ports `routes/auth_routes.py`. Four handlers so far — `setup`, `login`,
//! `logout`, `status` — which is the T0064 acceptance gate: a real password
//! against the real `auth.json`, producing a session in the real
//! `sessions.json` that then authenticates a gated request.
//!
//! Two things here are easy to get wrong and are commented where they appear:
//!
//! * **A successful password with a second factor pending is a 200**, not a 401.
//!   `auth_routes.py:174-177` returns `{"ok": false, "requires_totp": true}`.
//!   The frontend intercepts 401 globally and redirects to `/login`, so using
//!   `AppError::Unauthenticated` here would bounce the user out mid-login and log
//!   a rejection for a correct password.
//! * **A nonexistent user and a wrong password must cost the same.** Python does
//!   *not* do this — `verify_password` returns early on an unknown user
//!   (`core/auth.py:568-572`) — so the oracle is ours to close, using
//!   [`odysseus_store::auth::AuthStore::verify_password_or_dummy`].

use {
    axum::{
        Json, Router,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::{get, post},
    },
    serde::{Deserialize, Serialize},
};

use crate::{AppState, error::AppError};

/// Mount every `/api/auth` route.
///
/// The paths must match [`crate::middleware::auth::AUTH_EXEMPT_EXACT`] exactly, or
/// the gate will 401 a route the browser must reach before it has a session.
///
/// Returns `Router<AppState>` without calling `with_state`, so the caller can
/// merge it into a router that applies state exactly once.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/auth/setup", post(setup))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/status", get(status))
        .route("/api/auth/policy", get(policy))
}

/// `POST /api/auth/setup` — create the first admin.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    /// The administrator's chosen username.
    username: String,
    /// The administrator's chosen password.
    password: String,
}

#[derive(Debug, Serialize)]
/// The generic success envelope the setup form expects.
pub struct OkMessage {
    /// Always true; a failure is an error status instead.
    ok: bool,
    /// Human-readable confirmation for the setup form.
    message: &'static str,
}

/// The TOTP prompt is a **200** with `ok: false`. See the module docs.
#[derive(Debug, Serialize)]
/// The successful-login reply.
pub struct LoginOk {
    /// Always true: this variant is only returned on success.
    ok: bool,
    /// The authenticated user, lowercased.
    username: String,
}

#[derive(Debug, Serialize)]
/// The "password accepted, second factor outstanding" reply.
///
/// A **200** on purpose. The frontend intercepts 401 globally and redirects to
/// `/login`, so a 401 here would bounce the user out of a login they are midway
/// through and record a rejection against a password that was correct.
pub struct LoginNeedsTotp {
    /// Always false: the password was accepted, the second factor is not yet.
    ok: bool,
    /// Tells the frontend to show the code field.
    requires_totp: bool,
    /// Who the code should be requested for.
    username: String,
}

/// `GET /api/auth/status` — is the instance set up, and who am I.
#[derive(Debug, Serialize)]
/// What `GET /api/auth/status` reports to the frontend.
///
/// The four booleans are not independent metadata that could be folded into a
/// `bitflags`: they are the four keys `AuthManager.status` returns
/// (`core/auth.py:669-680`) and the frontend branches on each by name, so the
/// field names *are* the wire format.
#[expect(
    clippy::struct_excessive_bools,
    reason = "these are the four keys core/auth.py:669-680 returns and the frontend branches on each by name; folding them into flags would change the wire format"
)]
pub struct StatusBody {
    /// Whether any account exists.
    pub configured: bool,
    /// Whether the request carried a valid session.
    pub authenticated: bool,
    /// The logged-in username, if any.
    pub username: Option<String>,
    /// Whether that user is an administrator.
    pub is_admin: bool,
    /// Whether self-registration is allowed.
    pub signup_enabled: bool,
    /// The caller's effective privileges, present only when authenticated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privileges: Option<odysseus_store::auth::Privileges>,
}

/// `GET /api/auth/policy` — public constants the frontend needs before login.
#[derive(Debug, Serialize)]
pub struct PolicyBody {
    /// Minimum accepted password length, so the form can validate before submitting.
    pub password_min_length: usize,
    /// Names nobody may register.
    pub reserved_usernames: Vec<&'static str>,
    /// Whether the registration form should be shown at all.
    pub signup_enabled: bool,
    /// Session lifetime in days, for the frontend's "stay signed in" copy.
    pub session_days: u64,
}

/// `POST /api/auth/setup` — create the initial admin account.
///
/// Only works while no account exists, and the check-and-create is atomic in the
/// store (`setup_first_run`): doing it here as two calls would let two concurrent
/// requests both create an admin.
///
/// # Errors
///
/// 400 for a short password or empty username, 403 for a reserved name, and 409
/// once an account exists.
pub async fn setup(
    State(state): State<AppState>,
    Json(body): Json<SetupRequest>,
) -> Result<Json<OkMessage>, AppError> {
    if body.password.chars().count() < odysseus_store::auth::PASSWORD_MIN_LENGTH {
        return Err(AppError::BadRequest(format!(
            "Password must be at least {} characters",
            odysseus_store::auth::PASSWORD_MIN_LENGTH
        )));
    }
    let username = body.username.trim();
    if username.is_empty() {
        return Err(AppError::BadRequest(String::from("Username is required")));
    }
    if is_reserved(username) {
        return Err(AppError::Forbidden(String::from("Username is reserved")));
    }

    // `AlreadyConfigured` is a client mistake, not a server fault, and it is the
    // expected answer on every setup attempt after the first. Left as a generic
    // `AppError::Auth` it would surface as a 500 and read as "the server is
    // broken" on a completely ordinary request.
    match state.setup_first_admin(username, &body.password).await {
        Ok(()) => Ok(Json(OkMessage {
            ok: true,
            message: "Admin account created",
        })),
        Err(AppError::Auth(odysseus_store::auth::AuthError::AlreadyConfigured)) => {
            Err(AppError::Status {
                status: StatusCode::CONFLICT,
                message: String::from("Already configured"),
            })
        }
        Err(other) => Err(other),
    }
}

/// `POST /api/auth/login` — verify a password and open a session.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    /// The account being logged into. Trimmed and lowercased before lookup.
    pub username: String,
    /// The plaintext password. Compared with bcrypt; never logged.
    pub password: String,
    /// A TOTP or backup code, once one has been requested.
    #[serde(default)]
    pub totp_code: Option<String>,
    /// Issue a persistent cookie rather than a session-scoped one.
    #[serde(default)]
    pub remember: bool,
}

/// `POST /api/auth/login`.
///
/// # Errors
///
/// 401 for a bad password or a bad second-factor code. Deliberately *not* 401
/// when the password is right but a code is still needed — that is a 200 body.
pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Response, AppError> {
    let username = normalise_username(&body.username);

    // Exactly one bcrypt comparison either way: an unknown user costs the same as
    // a known one with a wrong password.
    if !state.verify_login(&username, &body.password).await? {
        return Err(AppError::Unauthenticated);
    }

    if state.totp_enabled(&username).await? {
        let Some(code) = body.totp_code.as_deref().filter(|c| !c.is_empty()) else {
            // Password accepted, second factor outstanding. 200 on purpose.
            return Ok(Json(LoginNeedsTotp {
                ok: false,
                requires_totp: true,
                username,
            })
            .into_response());
        };
        if !state.verify_second_factor(&username, code).await? {
            return Err(AppError::Unauthenticated);
        }
    }

    let token = state.issue_session(&username).await?;
    let cookie = session_cookie(&token, body.remember, secure_cookie(&headers));
    Ok((
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
        Json(LoginOk { ok: true, username }),
    )
        .into_response())
}

/// `POST /api/auth/logout` — revoke the session and clear the cookie.
///
/// Idempotent: no cookie is a success, not an error, because the caller's goal
/// ("I am logged out") already holds.
///
/// # Errors
///
/// 500 if `sessions.json` cannot be written.
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(token) = crate::middleware::auth::session_cookie(&headers) {
        state.revoke_session(&token).await?;
    }
    let cleared = format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        crate::middleware::auth::SESSION_COOKIE
    );
    Ok((
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cleared)],
        Json(OkOnly { ok: true }),
    )
        .into_response())
}

#[derive(Debug, Serialize)]
/// The bare `{"ok": true}` envelope.
pub struct OkOnly {
    /// Always true; logout is idempotent.
    ok: bool,
}

/// `GET /api/auth/status`.
///
/// Exempt from the gate, so it resolves the cookie itself rather than reading an
/// `AuthContext` the gate never inserted.
///
/// # Errors
///
/// Returns [`AppError::Auth`] if `auth.json` or `sessions.json` cannot be read.
/// That surfaces as a 500 rather than a 200 with empty fields, because reporting
/// "not configured" for an unreadable file would reopen first-run setup.
pub async fn status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatusBody>, AppError> {
    let configured = state.is_configured().await;
    let username = match crate::middleware::auth::session_cookie(&headers) {
        Some(token) => state.validate_session(&token).await?,
        None => None,
    };
    let authenticated = username.is_some();
    let is_admin = match &username {
        Some(user) => state.is_admin(user).await?,
        None => false,
    };
    let privileges = match &username {
        Some(user) => Some(state.privileges(user).await?),
        None => None,
    };

    Ok(Json(StatusBody {
        configured,
        authenticated,
        username,
        is_admin,
        signup_enabled: state.signup_enabled().await?,
        privileges,
    }))
}

/// `GET /api/auth/policy` — public constants, no authentication required.
///
/// # Errors
///
/// Returns [`AppError::Auth`] if `auth.json` cannot be read to answer
/// `signup_enabled`.
pub async fn policy(State(state): State<AppState>) -> Result<Json<PolicyBody>, AppError> {
    Ok(Json(PolicyBody {
        password_min_length: odysseus_store::auth::PASSWORD_MIN_LENGTH,
        reserved_usernames: odysseus_store::auth::RESERVED_USERNAMES.to_vec(),
        signup_enabled: state.signup_enabled().await?,
        session_days: odysseus_store::auth::TOKEN_TTL_SECONDS / 86_400,
    }))
}

/// Lowercased and trimmed, matching `auth_routes.py:169`.
fn normalise_username(raw: &str) -> String {
    raw.trim().to_lowercase()
}

/// Mirrors the reserved-name check in the store, so the route can answer 403
/// rather than surfacing a store error as a 500.
fn is_reserved(username: &str) -> bool {
    let lower = username.to_lowercase();
    odysseus_store::auth::RESERVED_USERNAMES.contains(&lower.as_str())
}

/// Builds the `Set-Cookie` value for a new session.
///
/// `HttpOnly` so script cannot read it, `SameSite=Lax` so it survives a top-level
/// navigation from a link but not a cross-site POST, `Secure` when the connection
/// is HTTPS. The frontend reads no cookie by name, so the name is ours to choose;
/// Python's is `odysseus_session` and is kept for parity.
fn session_cookie(token: &str, remember: bool, secure: bool) -> String {
    let mut cookie = format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Lax",
        crate::middleware::auth::SESSION_COOKIE
    );
    if secure {
        cookie.push_str("; Secure");
    }
    if remember {
        use std::fmt::Write as _;
        // `write!` into the existing String rather than `format!`-and-append:
        // one allocation instead of two on the login path.
        let _ = write!(
            cookie,
            "; Max-Age={}",
            odysseus_store::auth::TOKEN_TTL_SECONDS
        );
    }
    cookie
}

/// Whether the session cookie gets `Secure`.
///
/// `SECURE_COOKIES` stays authoritative when it holds an explicit `true`/`false`,
/// which is the escape hatch for an install answering on plain HTTP alongside
/// HTTPS. Otherwise derive it from the request, so an HTTPS login gets a Secure
/// cookie with no configuration (`auth_routes.py:89-113`).
fn secure_cookie(headers: &HeaderMap) -> bool {
    if let Ok(configured) = std::env::var("SECURE_COOKIES") {
        match configured.trim().to_lowercase().as_str() {
            "true" => return true,
            "false" => return false,
            _ => {}
        }
    }
    // A chained proxy sends a list; the client-facing hop comes first.
    let forwarded = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .split(',')
        .next()
        .unwrap_or_default()
        .trim();
    forwarded.eq_ignore_ascii_case("https")
}
