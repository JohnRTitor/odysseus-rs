//! The application state and the router.
//!
//! One `AppState` is built once in `main` and passed as `Arc<AppState>`
//! (`AGENTS.md` §4: dependency injection, not module-level singletons). The
//! Python source threads singletons through a dozen `set_*` functions and stores
//! them on `app.state`; none of that is reproduced here.

use std::{sync::Arc, time::Duration};

use {
    axum::{Router, routing::get},
    odysseus_store::Store,
};

use crate::middleware::{compression_layer, cors_layer, security, slow_log, timeout};

/// Everything a handler may need, constructed once at startup.
///
/// Cloning is cheap: every field is an `Arc` or a plain value.
#[derive(Debug, Clone)]
pub struct AppState {
    /// SQLite persistence.
    pub store: Arc<Store>,
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
    /// migrated, or locked down.
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

        Ok(Self {
            store: Arc::new(store),
            request_timeout: timeout::timeout_from_env(),
            slow_request_threshold: slow_log::threshold_from_env(),
        })
    }
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
    let app = Router::new()
        .route("/api/health", get(crate::routes::system::health))
        .route("/api/version", get(crate::routes::system::version))
        .route("/api/ready", get(crate::routes::system::ready))
        .route("/api/runtime", get(crate::routes::system::runtime))
        .route("/", get(crate::routes::system::ui_not_yet_built))
        .with_state(state.clone());

    app.layer(axum::middleware::from_fn_with_state(
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
        odysseus_store::Store,
        std::sync::Arc,
    };

    /// A state backed by a throwaway database.
    ///
    /// `Store::connect` creates the file lazily, so the directory must outlive the
    /// state. Leaking one `TempDir` per test is acceptable at this size and
    /// avoids the alternative of threading cleanup through every caller.
    fn test_state() -> AppState {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.keep();
        AppState {
            store: Arc::new(Store::open(path.join("app.db"))),
            request_timeout: std::time::Duration::from_secs(45),
            slow_request_threshold: std::time::Duration::from_secs_f64(0.75),
        }
    }

    #[test]
    fn router_builds() {
        let state = test_state();
        let _ = router(&state);
    }
}
