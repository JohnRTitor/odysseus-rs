//! The application state and the router.
//!
//! One `AppState` is built once in `main` and passed as `Arc<AppState>`
//! (`AGENTS.md` §4: dependency injection, not module-level singletons). The
//! Python source threads singletons through a dozen `set_*` functions and stores
//! them on `app.state`; none of that is reproduced here.

use std::{sync::Arc, time::Duration};

use {
    axum::{Router, routing::get},
    odysseus_store::{
        Store, StoreError,
        api_tokens::{ApiTokenStore, VerifiedToken},
        auth::{AuthOwners, AuthStore},
        ratelimit,
    },
    tower_http::services::ServeDir,
};

use crate::middleware::{auth, compression_layer, cors_layer, security, slow_log, timeout};

/// Everything a handler may need, constructed once at startup.
///
/// Cloning is cheap: every field is an `Arc` or a plain value.
#[derive(Debug, Clone)]
pub struct AppState {
    /// SQLite persistence.
    pub store: Arc<Store>,
    /// `auth.json` + `sessions.json`: users, sessions and TOTP state.
    pub auth: Arc<AuthStore>,
    /// `api_tokens`: bearer credentials and the prefix cache.
    pub api_tokens: Arc<ApiTokenStore>,
    /// The per-process internal-tool token, from `ODYSSEUS_INTERNAL_TOKEN`.
    ///
    /// Generated at startup when the variable is unset, exactly as
    /// `core/middleware.py:20` does. Never persisted, never logged.
    pub internal_tool_token: Arc<str>,
    /// `LOCALHOST_BYPASS`, the dev-only "skip auth on loopback" switch.
    pub localhost_bypass: bool,
    /// Login throttle: 15 per 60 s per client (`auth_routes.py:119`).
    ///
    /// Without it, `POST /api/auth/login` is an unthrottled online brute-force
    /// oracle against a cost-12 bcrypt hash. TOTP makes it worse rather than
    /// better: `SKEW = 1` leaves 3 valid codes live at any instant, so ~333k
    /// guesses per 30-second step, and bcrypt does not slow that path.
    pub login_limiter: Arc<ratelimit::RateLimiter>,
    /// First-run setup throttle: 3 per 300 s per client (`auth_routes.py:121`).
    ///
    /// Lower than login because setup mints the administrator and is reachable on
    /// a fresh install, where the operator is by definition still on the network.
    pub setup_limiter: Arc<ratelimit::RateLimiter>,
    /// The hard request deadline, from `REQUEST_HARD_TIMEOUT`.
    pub request_timeout: Duration,
    /// Threshold above which a request is logged as slow, from
    /// `ODYSSEUS_SLOW_REQUEST_LOG_SECONDS`.
    pub slow_request_threshold: Duration,
}

impl AppState {
    /// Builds the state from the environment.
    ///
    /// Runs migrations before returning, so the first request can never race the
    /// schema. The Python source did this as an import side effect
    /// (`core/database.py:2737`) — precisely the pattern being replaced.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Store`] if the database cannot be opened,
    /// migrated, or locked down, and [`crate::AppError::Config`] if no data
    /// directory can be determined.
    pub fn bootstrap() -> Result<Self, crate::AppError> {
        let store = Store::open_default();
        let report = store.migrate()?;
        if report.is_noop() {
            tracing::debug!(version = report.to, "schema already current");
        } else {
            tracing::info!(
                from = report.from,
                to = report.to,
                applied = ?report.applied,
                "applied migrations"
            );
        }
        store.restrict_permissions()?;

        let auth = AuthStore::new(store.data_dir()?);
        auth.prepare()?;
        auth.harden_permissions()?;

        Ok(Self {
            api_tokens: Arc::new(ApiTokenStore::new(store.clone())),
            store: Arc::new(store),
            auth: Arc::new(auth),
            internal_tool_token: Arc::from(internal_tool_token()),
            localhost_bypass: flag("LOCALHOST_BYPASS"),
            // Constructed once and shared, exactly as Python builds them per route
            // at import time. The limits themselves are auth-route policy, so they
            // live with the routes rather than in the generic limiter.
            login_limiter: Arc::new(ratelimit::RateLimiter::new(
                crate::routes::auth::LOGIN_MAX_REQUESTS,
                Duration::from_secs(crate::routes::auth::LOGIN_WINDOW_SECONDS),
            )),
            setup_limiter: Arc::new(ratelimit::RateLimiter::new(
                crate::routes::auth::SETUP_MAX_REQUESTS,
                Duration::from_secs(crate::routes::auth::SETUP_WINDOW_SECONDS),
            )),
            request_timeout: timeout::timeout_from_env(),
            slow_request_threshold: slow_log::threshold_from_env(),
        })
    }

    /// Whether `auth.json` holds at least one user.
    ///
    /// **Fails closed.** An unreadable `auth.json` reports `true`, because the
    /// alternative — reporting "unconfigured" — hands out the first-run loopback
    /// access that step 5 of the gate grants.
    pub async fn is_configured(&self) -> bool {
        let auth = Arc::clone(&self.auth);
        match tokio::task::spawn_blocking(move || auth.is_configured()).await {
            Ok(Ok(configured)) => configured,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "auth.json unreadable; treating as configured");
                true
            }
            Err(e) => {
                tracing::error!(error = %e, "auth.json probe task failed; treating as configured");
                true
            }
        }
    }

    /// Whether `username` names an existing account.
    ///
    /// Used by internal-tool impersonation, which applies only to a real
    /// account (`app.py:387`).
    pub async fn account_exists(&self, username: &str) -> bool {
        let auth = Arc::clone(&self.auth);
        let name = username.to_owned();
        match tokio::task::spawn_blocking(move || auth.user_exists(&name)).await {
            Ok(Ok(exists)) => exists,
            // Fail closed: an unreadable auth.json must not make a real account
            // look absent, because that would silently downgrade impersonation to
            // the generic internal-tool user.
            Ok(Err(e)) => {
                tracing::error!(error = %e, "user lookup failed; not impersonating");
                false
            }
            Err(e) => {
                tracing::error!(error = %e, "user lookup task failed; not impersonating");
                false
            }
        }
    }

    /// Verifies a bearer `ody_` token, returning it with its owner and scopes.
    ///
    /// The owner comes from the token row, resolved against `auth.json`, so a
    /// token whose owner was deleted resolves to nothing and is refused.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Store`] if the prefix cache cannot be rebuilt. The
    /// caller must treat that as a rejection: Python's `except` around the same
    /// block logs and falls through to a 401.
    pub async fn verify_api_token(
        &self,
        presented: &str,
    ) -> Result<Option<VerifiedToken>, crate::AppError> {
        let tokens = Arc::clone(&self.api_tokens);
        let resolver = AuthOwners::new(AuthStore::clone(&self.auth));
        let raw = presented.to_owned();
        let outcome =
            tokio::task::spawn_blocking(move || tokens.verify_blocking(&raw, &resolver)).await;
        match outcome {
            Ok(result) => {
                result.map_err(|e| crate::AppError::Store(StoreError::Auth(e.to_string())))
            }
            Err(e) => {
                tracing::error!(error = %e, "api token verification task failed");
                Err(crate::AppError::Store(StoreError::Interrupted))
            }
        }
    }

    /// Resolves a session cookie to its username.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Auth`] if `sessions.json` cannot be read; the caller
    /// treats it as a rejection rather than as "no such session".
    pub async fn validate_session(&self, token: &str) -> Result<Option<String>, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let raw = token.to_owned();
        match tokio::task::spawn_blocking(move || auth.username_for_token(&raw)).await {
            Ok(result) => result.map_err(crate::AppError::Auth),
            Err(e) => {
                tracing::error!(error = %e, "session lookup task failed");
                Err(crate::AppError::Store(StoreError::Interrupted))
            }
        }
    }

    /// Creates the first administrator. Refused once any account exists.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] for a reserved or empty username, a short
    /// password, or [`odysseus_store::auth::AuthError::AlreadyConfigured`].
    pub async fn setup_first_admin(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let (name, secret) = (username.to_owned(), password.to_owned());
        run_blocking("first-run setup", move || {
            auth.setup_first_run(&name, &secret)
        })
        .await
    }

    /// Verifies a login, always doing exactly one bcrypt comparison.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if `auth.json` cannot be read. A wrong
    /// password and an unknown user are both `Ok(false)`, and cost the same.
    pub async fn verify_login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<bool, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let (name, secret) = (username.to_owned(), password.to_owned());
        run_blocking("password verification", move || {
            auth.verify_password_or_dummy(&name, &secret)
        })
        .await
    }

    /// Whether `username` has a second factor enrolled.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if the account or file cannot be read.
    pub async fn totp_enabled(&self, username: &str) -> Result<bool, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let name = username.to_owned();
        run_blocking("totp flag read", move || auth.totp_enabled(&name)).await
    }

    /// Checks a TOTP or backup code, spending a backup code if one matches.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if `auth.json` cannot be read. A wrong
    /// code is `Ok(false)` — never an error, so a caller cannot read a failure
    /// as a permit.
    pub async fn verify_second_factor(
        &self,
        username: &str,
        code: &str,
    ) -> Result<bool, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let (name, secret) = (username.to_owned(), code.to_owned());
        run_blocking("second-factor check", move || {
            auth.totp_verify(&name, &secret, unix_now())
        })
        .await
    }

    /// Issues a session token for an already-authenticated user.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if the account is unknown or
    /// `sessions.json` cannot be written.
    pub async fn issue_session(&self, username: &str) -> Result<String, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let name = username.to_owned();
        run_blocking("session issue", move || auth.issue_token(&name)).await
    }

    /// Revokes one session token. A token that is already gone is not an error.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if `sessions.json` cannot be written.
    pub async fn revoke_session(&self, token: &str) -> Result<(), crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let raw = token.to_owned();
        run_blocking("session revoke", move || auth.revoke_token(&raw)).await
    }

    /// Whether self-registration is currently allowed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if `auth.json` cannot be read.
    pub async fn signup_enabled(&self) -> Result<bool, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        run_blocking("signup flag read", move || auth.signup_enabled()).await
    }

    /// Whether `username` is an administrator.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if the account or file cannot be read.
    pub async fn is_admin(&self, username: &str) -> Result<bool, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let name = username.to_owned();
        run_blocking("admin flag read", move || auth.is_admin(&name)).await
    }

    /// The effective privilege map for `username`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::AppError::Auth`] if the account or file cannot be read.
    pub async fn privileges(
        &self,
        username: &str,
    ) -> Result<odysseus_store::auth::Privileges, crate::AppError> {
        let auth = Arc::clone(&self.auth);
        let name = username.to_owned();
        run_blocking("privilege read", move || auth.privileges(&name)).await
    }
}

/// Runs a blocking auth-store call on the blocking pool.
///
/// One place, so every `AppState` auth method gets the D11 treatment and the
/// `JoinError` → 500 mapping without repeating it. The `what` label appears in
/// the log so a failure names the operation rather than a type.
async fn run_blocking<T, F>(what: &'static str, f: F) -> Result<T, crate::AppError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, odysseus_store::auth::AuthError> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result.map_err(crate::AppError::Auth),
        Err(e) => {
            tracing::error!(%what, error = %e, "auth task failed");
            Err(crate::AppError::Store(StoreError::Interrupted))
        }
    }
}

/// Seconds since the Unix epoch, for TOTP's 30-second time step.
///
/// The store's `totp_verify` takes `now` as a parameter, which is where time is
/// pinned for testing. At the route layer we pass the real clock: a route test
/// only needs to reach the "password accepted, code required" branch, which does
/// not depend on which step we are in. A test that needed a *valid* code would
/// have to go through the store with an injected `now` instead.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Reads a boolean environment flag, matching Python's `.lower() == "true"`.
fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v.eq_ignore_ascii_case("true"))
}

/// The per-process internal-tool token, generating one when unset.
///
/// `core/middleware.py:20` uses `os.environ.get(...) or secrets.token_hex(32)`.
fn internal_tool_token() -> String {
    if let Ok(existing) = std::env::var(crate::middleware::auth::INTERNAL_TOKEN_ENV)
        && !existing.is_empty()
    {
        return existing;
    }
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).unwrap_or_else(|e| {
        // Without entropy there is no safe fallback: a predictable token would be
        // worse than refusing to start.
        tracing::error!(error = %e, "no entropy for the internal-tool token");
        std::process::exit(1);
    });
    bytes.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// Build the full router with the middleware stack applied.
///
/// # Layer order
///
/// Outside-in: security headers, CORS, timeout, slow-request log.
///
/// CORS is **outside** any authentication gate by design. A genuine preflight
/// carries no credentials, so gating it on auth rejects it before CORS can
/// answer, blocking every cross-origin client before its real request is sent
/// (`app.py:374`).
///
/// The authentication layer is added by [`with_auth`], which the caller composes
/// *inside* CORS. Wiring it here would be wrong: `layer` applies outermost-last,
/// so anything applied after this point ends up outside CORS.
pub fn router(state: &AppState) -> Router {
    secured(state)
}

/// The routes, before any middleware.
///
/// Split out so [`secured`] and [`router_without_auth`] cannot drift apart in
/// their route tables — a route added to one and not the other would be a
/// silently unauthenticated endpoint.
fn base(state: &AppState) -> Router {
    Router::new()
        .merge(crate::routes::auth::routes())
        .route("/api/health", get(crate::routes::system::health))
        .route("/api/version", get(crate::routes::system::version))
        .route("/api/ready", get(crate::routes::system::ready))
        .route("/api/runtime", get(crate::routes::system::runtime))
        // Static assets are mounted before the fallback, because a `fallback`
        // catches everything: without these two the `/pkg/*.css` and `/pkg/*.wasm`
        // that cargo-leptos emits would be answered by the Leptos shell, and the
        // browser would receive an HTML document where it asked for CSS.
        .nest_service("/static", ServeDir::new(static_dir()))
        .nest_service("/pkg", ServeDir::new(pkg_dir()))
        .route("/", get(crate::routes::system::ui_shell))
        .fallback(crate::routes::system::ui_shell)
        .with_state(state.clone())
}

/// Where hand-written static assets live (`favicon.svg`, the service worker).
///
/// `static/`, mirroring the Python layout — **not** cargo-leptos' `assets-dir`.
/// That directory is deleted and recreated on every build, so a committed asset
/// placed there does not survive; `site/pkg` is the only directory
/// `cargo leptos build` is allowed to own.
///
/// An environment override exists so a deployment can point at a directory outside
/// the binary without a rebuild. It is read once at router construction rather than
/// per request: a missing override is a deployment mistake that should surface as a
/// 404 on a real asset, not as a per-request env lookup.
fn static_dir() -> std::path::PathBuf {
    std::env::var_os("ODYSSEUS_STATIC_DIR")
        .map_or_else(|| workspace_dir().join("static"), std::path::PathBuf::from)
}

/// Where cargo-leptos writes the compiled CSS and the wasm bundle.
///
/// `site-root` + `site-pkg-dir` from `[[workspace.metadata.leptos]]`. `LEPTOS_SITE_PKG_DIR`
/// is cargo-leptos' own override, so it is honoured too rather than being second-guessed.
fn pkg_dir() -> std::path::PathBuf {
    std::env::var_os("LEPTOS_SITE_PKG_DIR")
        .or_else(|| std::env::var_os("ODYSSEUS_PKG_DIR"))
        .map_or_else(|| site_dir().join("pkg"), std::path::PathBuf::from)
}

/// The cargo-leptos `site-root`, i.e. `<workspace>/site`.
fn site_dir() -> std::path::PathBuf {
    workspace_dir().join("site")
}

/// The workspace root, derived from this crate's manifest directory.
///
/// `CARGO_MANIFEST_DIR` is `<root>/crates/web`, so the workspace root is two levels
/// up. Derived rather than hard-coded so a checkout at any path works — and so the
/// served directories cannot drift from the leptos metadata by being typed twice.
fn workspace_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf()
}

/// Build the full router **with** the authentication gate.
///
/// Outside-in: security headers → compression → CORS → **auth gate** → timeout →
/// slow-request log → routes.
///
/// CORS is outside the gate by design (`API_MIGRATION.md` §2.1). A genuine
/// preflight carries no credentials, so gating it would 401 it before CORS could
/// answer, blocking every cross-origin client before its real request is sent.
///
/// Note the ordering mechanism: `layer` wraps *outside* whatever it is applied
/// to, so the auth layer is applied **before** `cors_layer` in this chain in
/// order to end up inside it. Applying it after would silently put the gate
/// outside CORS — the exact inversion `app.py:363` has to work around.
pub fn secured(state: &AppState) -> Router {
    base(state)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            slow_log::log_slow_requests,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            timeout::enforce_timeout,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_authentication,
        ))
        .layer(cors_layer())
        .layer(compression_layer())
        .layer(axum::middleware::from_fn(security::attach_security_headers))
}

/// The full stack **without** the authentication gate.
///
/// Every route is reachable by an unauthenticated caller, so this exists only
/// for tests and for the pre-auth bring-up in `main`. It is named to be
/// conspicuous at the call site; [`router`] and [`secured`] are the same
/// function, so there is no way to get the unauthenticated stack by accident.
pub fn router_without_auth(state: &AppState) -> Router {
    base(state)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            slow_log::log_slow_requests,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            timeout::enforce_timeout,
        ))
        .layer(cors_layer())
        .layer(compression_layer())
        .layer(axum::middleware::from_fn(security::attach_security_headers))
}

#[cfg(test)]
mod tests {
    use {
        super::{AppState, router},
        odysseus_store::{Store, api_tokens::ApiTokenStore, auth::AuthStore},
        std::sync::Arc,
    };

    /// A state backed by a throwaway directory.
    ///
    /// `Store::connect` creates the file lazily, so the directory must outlive
    /// the state. Leaking one `TempDir` per test is acceptable at this size and
    /// avoids the alternative of threading cleanup through every caller.
    fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        let store = Store::open(path.join("app.db"));
        let auth = AuthStore::new(store.data_dir().expect("data dir"));
        auth.prepare().expect("prepare auth store");
        AppState {
            api_tokens: Arc::new(ApiTokenStore::new(store.clone())),
            auth: Arc::new(auth),
            store: Arc::new(store),
            internal_tool_token: Arc::from("test-internal-token"),
            localhost_bypass: false,
            login_limiter: Arc::new(odysseus_store::ratelimit::RateLimiter::new(
                crate::routes::auth::LOGIN_MAX_REQUESTS,
                std::time::Duration::from_secs(crate::routes::auth::LOGIN_WINDOW_SECONDS),
            )),
            setup_limiter: Arc::new(odysseus_store::ratelimit::RateLimiter::new(
                crate::routes::auth::SETUP_MAX_REQUESTS,
                std::time::Duration::from_secs(crate::routes::auth::SETUP_WINDOW_SECONDS),
            )),
            request_timeout: std::time::Duration::from_secs(45),
            slow_request_threshold: std::time::Duration::from_secs_f64(0.75),
        }
    }

    #[test]
    fn router_builds() {
        let state = test_state();
        let _ = router(&state);
    }

    #[tokio::test]
    async fn an_unconfigured_install_is_not_configured() {
        let state = test_state();
        assert!(!state.is_configured().await);
    }

    #[tokio::test]
    async fn a_created_user_makes_the_install_configured() {
        let state = test_state();
        state
            .auth
            .create_user("alice", "correct horse", true)
            .expect("create");
        assert!(state.is_configured().await);
        assert!(state.account_exists("alice").await);
        assert!(!state.account_exists("bob").await);
    }

    #[tokio::test]
    async fn a_valid_session_cookie_resolves_to_its_user() {
        let state = test_state();
        state
            .auth
            .create_user("alice", "correct horse", false)
            .expect("create");
        let token = state.auth.issue_token("alice").expect("issue");
        assert_eq!(
            state.validate_session(&token).await.expect("validate"),
            Some(String::from("alice"))
        );
        assert_eq!(
            state.validate_session("deadbeef").await.expect("validate"),
            None
        );
    }
}
