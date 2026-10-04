//! Liveness, readiness, version, and runtime introspection.
//!
//! These four are the only endpoints the Python source places outside the auth
//! gate (`app.py:264-276`), and all four are genuinely safe to expose: they
//! reveal nothing beyond what a client needs to decide whether the server is up.
//!
//! `/api/backgrounds` is deliberately **not** in this module. The Python source
//! serves it with no authentication (`app.py:940-943`); it is a sandbox page for
//! prototyping background effects and is gated to admin in the Rust port, which
//! is recorded as a deliberate fix in `INCOMPLETE.md`.

use {
    axum::{Json, extract::State},
    serde::Serialize,
};

use crate::AppState;

/// Application version, kept in step with the Python `APP_VERSION`.
pub const APP_VERSION: &str = "0.1.0";

/// Liveness. Always 200 if the process is serving.
///
/// Returns a fixed timestamp-free body: `/api/health` is polled, and formatting a
/// clock on every poll is measurable noise.
pub async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "healthy" }))
}

/// The application version.
pub async fn version() -> Json<VersionBody> {
    Json(VersionBody {
        version: APP_VERSION,
    })
}

/// Readiness: 200 only when every critical subsystem is whole, else 503.
///
/// Distinct from `/api/health`, which is liveness. An orchestrator must be able
/// to tell "the process is up but the database is unusable" from "the process
/// works". The Python source does the same (`app.py:988-997`).
///
/// # Errors
///
/// Returns [`crate::AppError::Unavailable`] if the database cannot be opened,
/// which is what makes the endpoint 503 rather than 200.
pub async fn ready(State(state): State<AppState>) -> Result<Json<ReadinessBody>, crate::AppError> {
    // Opening a connection is the cheapest real check that the database is
    // usable; it runs on the blocking pool per D11.
    //
    // The `?` is load-bearing. `spawn_blocking` returns
    // `Result<Result<Connection, StoreError>, JoinError>`, and peeling only the
    // outer `JoinError` — then discarding the inner — reported `ready: true`
    // while every DB-backed route was 500ing, so a degraded instance was never
    // recycled. §5 forbids swallowing a write error, and this is the same shape.
    odysseus_store::blocking::spawn_blocking({
        let store = state.store.clone();
        move || store.connect()
    })
    .await
    .map_err(|e| crate::AppError::Unavailable(format!("database check failed: {e}")))?
    .map_err(|e| crate::AppError::Unavailable(format!("database unusable: {e}")))?;

    Ok(Json(ReadinessBody {
        ready: true,
        version: APP_VERSION,
        schema_version: odysseus_store::schema::TARGET_VERSION,
    }))
}

/// Runtime facts a client needs to decide how to reach a model.
pub async fn runtime() -> Json<RuntimeBody> {
    Json(RuntimeBody {
        in_docker: in_docker(),
        ollama_base_url: ollama_base_url(),
    })
}

/// The UI has not been built yet.
///
/// # Errors
///
/// Always returns [`crate::AppError::Status`] with 503 and an explanatory
/// message. It is written as a `Result` rather than a plain 503 response so the
/// body goes through the same error path as every other failure, and so adding a
/// real route later is a one-line change.
///
/// Returns 503 with an explicit reason rather than a placeholder page. A stub
/// page would violate `AGENTS.md` §0 N2; a bare 404 would leave a user with no
/// way to tell "not implemented" from "wrong port".
pub async fn ui_not_yet_built() -> Result<axum::response::Response, crate::AppError> {
    Err(crate::AppError::Status {
        status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
        message: String::from(
            "The Odysseus UI has not been built in the Rust port yet. \
             See TASKS.md phase 0.6 (T0100-T0107). The API is live: try /api/health, \
             /api/version, /api/ready.",
        ),
    })
}

/// `/api/version` body.
#[derive(Debug, Serialize)]
pub struct VersionBody {
    /// The application version.
    pub version: &'static str,
}

/// `/api/ready` body.
#[derive(Debug, Serialize)]
pub struct ReadinessBody {
    /// True only when every critical subsystem is whole.
    pub ready: bool,
    /// The application version.
    pub version: &'static str,
    /// The database schema version.
    pub schema_version: u32,
}

/// `/api/runtime` body.
#[derive(Debug, Serialize)]
pub struct RuntimeBody {
    /// True when running inside a container.
    pub in_docker: bool,
    /// Where an Ollama instance is expected.
    pub ollama_base_url: String,
}

/// Detect a container, mirroring `app.py:1001-1008`.
fn in_docker() -> bool {
    if std::path::Path::new("/.dockerenv").exists() {
        return true;
    }
    // Fall back to cgroup inspection, as the Python source does when
    // `/.dockerenv` is absent (some runtimes do not create it).
    std::fs::read_to_string("/proc/1/cgroup").is_ok_and(|cgroup| {
        ["docker", "containerd", "kubepods"]
            .iter()
            .any(|marker| cgroup.contains(marker))
    })
}

/// Resolve the Ollama base URL, mirroring `app.py:1009-1016`.
///
/// Inside a container the host's Ollama is reached over `host.docker.internal`,
/// because `127.0.0.1` there is the container itself.
fn ollama_base_url() -> String {
    for var in ["OLLAMA_BASE_URL", "OLLAMA_URL"] {
        let Some(value) = std::env::var(var).ok() else {
            continue;
        };
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    if in_docker() {
        String::from("http://host.docker.internal:11434/v1")
    } else {
        String::from("http://127.0.0.1:11434/v1")
    }
}

#[cfg(test)]
mod tests {
    use {
        super::ready,
        axum::{
            Router,
            body::Body,
            http::{Request, StatusCode},
            routing::get,
        },
        odysseus_store::Store,
        std::sync::Arc,
        tower::ServiceExt as _,
    };

    use crate::AppState;

    fn state_over(store: Store) -> AppState {
        let auth = odysseus_store::auth::AuthStore::new(store.data_dir().expect("data dir"));
        AppState {
            api_tokens: Arc::new(odysseus_store::api_tokens::ApiTokenStore::new(
                store.clone(),
            )),
            auth: Arc::new(auth),
            store: Arc::new(store),
            login_limiter: Arc::new(odysseus_store::ratelimit::RateLimiter::new(
                crate::routes::auth::LOGIN_MAX_REQUESTS,
                std::time::Duration::from_secs(crate::routes::auth::LOGIN_WINDOW_SECONDS),
            )),
            setup_limiter: Arc::new(odysseus_store::ratelimit::RateLimiter::new(
                crate::routes::auth::SETUP_MAX_REQUESTS,
                std::time::Duration::from_secs(crate::routes::auth::SETUP_WINDOW_SECONDS),
            )),
            internal_tool_token: Arc::from("t"),
            localhost_bypass: false,
            request_timeout: std::time::Duration::from_secs(45),
            slow_request_threshold: std::time::Duration::from_secs_f64(0.75),
        }
    }

    async fn probe(state: AppState) -> StatusCode {
        Router::new()
            .route("/api/ready", get(ready))
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/api/ready")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("oneshot")
            .status()
    }

    #[tokio::test]
    async fn readiness_is_200_when_the_database_opens() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state_over(Store::open(dir.keep().join("app.db")));
        assert_eq!(probe(state).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn readiness_is_503_when_the_database_cannot_be_opened() {
        // The whole reason `/api/ready` exists separately from `/api/health`. This
        // branch was unreachable: the handler peeled the outer `JoinError` and
        // discarded the inner `Result`, so it reported `ready: true` while every
        // DB-backed route was failing, and an orchestrator never recycled the
        // instance.
        let dir = tempfile::tempdir().expect("tempdir");
        // A regular file where a directory must be: `connect` calls
        // `create_dir_all` on the parent, which cannot succeed.
        let blocker = dir.keep().join("not-a-dir");
        std::fs::write(&blocker, b"x").expect("write blocker");

        let state = state_over(Store::open(blocker.join("nested").join("app.db")));
        assert_eq!(
            probe(state).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "an unusable database must not report ready"
        );
    }
}
