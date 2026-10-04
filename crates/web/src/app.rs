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
    },
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
        .route("/api/health", get(crate::routes::system::health))
        .route("/api/version", get(crate::routes::system::version))
        .route("/api/ready", get(crate::routes::system::ready))
        .route("/api/runtime", get(crate::routes::system::runtime))
        .route("/", get(crate::routes::system::ui_not_yet_built))
        .with_state(state.clone())
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
