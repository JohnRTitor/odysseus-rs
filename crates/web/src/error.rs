//! Error type for the transport layer.
//!
//! Application crates return typed errors; this is where they become HTTP
//! status codes (`AGENTS.md` §7: "Map errors to HTTP status **only** in
//! `odysseus-web`").
//!
//! The JSON body shape is `{"error": CODE, "message": str}` because the frontend
//! branches on `error` — see the Python handlers, which all return that shape.

use {
    axum::{
        http::StatusCode,
        response::{IntoResponse, Response},
    },
    serde::{Deserialize, Serialize},
};

/// Result alias for web-layer handlers.
pub type AppResult<T> = std::result::Result<T, AppError>;

/// A transport-layer error carrying the status it maps to.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The requested session does not exist.
    #[error("SESSION_NOT_FOUND: {0}")]
    SessionNotFound(String),

    /// A file upload failed validation.
    #[error("INVALID_FILE_UPLOAD: {0}")]
    InvalidUpload(String),

    /// An upstream model provider failed.
    #[error("LLM_SERVICE_ERROR: {0}")]
    Llm(String),

    /// A web search provider failed.
    #[error("WEB_SEARCH_ERROR: {0}")]
    Search(String),

    /// The caller is not authenticated.
    ///
    /// The frontend intercepts 401 globally and redirects to `/login`, so this
    /// status must be reserved for genuine authentication failures — never for
    /// "not allowed", which is [`AppError::Forbidden`].
    #[error("Not authenticated")]
    Unauthenticated,

    /// The caller is authenticated but lacks authority.
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// The requested resource does not exist.
    ///
    /// Also used for an owner mismatch on a file: returning 403 would confirm the
    /// resource exists, which is an information leak (`AGENTS.md` §6).
    #[error("Not found: {0}")]
    NotFound(String),

    /// The request body or query failed validation.
    #[error("Invalid request: {0}")]
    BadRequest(String),

    /// A precondition the server could not satisfy yet.
    ///
    /// Used where the Python code returns 503, for example when the vector store
    /// is unreachable — an empty state is reported, not busy-retried.
    #[error("Unavailable: {0}")]
    Unavailable(String),

    /// The request exceeded the server deadline.
    #[error("Request exceeded the configured timeout")]
    Timeout,

    /// Any other failure that carries its own status.
    #[error("{message}")]
    Status {
        /// The status to return.
        status: StatusCode,
        /// The human-readable message.
        message: String,
    },

    /// A storage-layer failure.
    #[error(transparent)]
    Store(#[from] odysseus_store::StoreError),

    /// An `auth.json` / `sessions.json` failure.
    ///
    /// Kept distinct from [`AppError::Store`] because these sidecars are the
    /// credential store: a caller must be able to tell "the database is
    /// unavailable" from "the credentials are unreadable", and both must fail
    /// closed rather than degrade to an unauthenticated state.
    #[error(transparent)]
    Auth(#[from] odysseus_store::auth::AuthError),
}

impl AppError {
    /// The status this error maps to.
    ///
    /// The four application exceptions mirror the Python exception handlers in
    /// `app.py:617-630` exactly: `SessionNotFoundError` → 404,
    /// `InvalidFileUploadError` → 400, `LLMServiceError` → 502,
    /// `WebSearchError` → 502.
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::SessionNotFound(_) | Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::InvalidUpload(_) | Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Llm(_) | Self::Search(_) => StatusCode::BAD_GATEWAY,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::Status { status, .. } => *status,
            // Credential-store failures are 500, never 401: a 401 would tell the
            // client its credentials were wrong, sending it to /login when the
            // real problem is that the server cannot read auth.json.
            Self::Store(_) | Self::Auth(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The stable machine-readable code the frontend branches on.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::SessionNotFound(_) => "SESSION_NOT_FOUND",
            Self::InvalidUpload(_) => "INVALID_FILE_UPLOAD",
            Self::Llm(_) => "LLM_SERVICE_ERROR",
            Self::Search(_) => "WEB_SEARCH_ERROR",
            Self::Unauthenticated => "Not authenticated",
            Self::Forbidden(_) => "Forbidden",
            Self::NotFound(_) => "Not found",
            Self::BadRequest(_) => "Bad request",
            Self::Unavailable(_) => "Unavailable",
            Self::Timeout => "Timeout",
            Self::Status { .. } => "Error",
            Self::Store(_) | Self::Auth(_) => "Internal error",
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ErrorBody {
    error: &'static str,
    message: String,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();

        // A 5xx is our fault and needs a trace; a 4xx is the caller's and does
        // not. Logging every 401 would bury real errors.
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        } else {
            tracing::debug!(error = %self, "request rejected");
        }

        let body = ErrorBody {
            error: self.code(),
            // The `Display` of a store or auth error embeds the absolute path it
            // failed on (`io error at {path}: …`). Sending that to the client
            // discloses the filesystem layout and the OS username, and — because
            // "missing", "io error" and "malformed" render differently — it is a
            // reliable oracle for the state of the credential store. §5 forbids
            // leaking internal paths and URLs, so a 5xx gets a fixed message.
            //
            // The detail is not lost: it is in the `tracing::error!` above, which
            // is where an operator reads it.
            message: if status.is_server_error() {
                String::from("Internal error")
            } else {
                self.to_string()
            },
        };
        // The body has a fixed shape, so this cannot fail.
        let json = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
        (
            status,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json,
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::AppError,
        axum::{http::StatusCode, response::IntoResponse},
        serde::Deserialize,
    };

    /// The `{"error": CODE, "message": str}` shape the frontend branches on.
    ///
    /// `IntoResponse` had no test at all before this: the module doc states the
    /// shape, and nothing checked that the body actually had it.
    ///
    /// Deserialises into a `String`-owned mirror rather than [`ErrorBody`],
    /// whose `error` field is `&'static str` and so cannot be built from a
    /// runtime buffer.
    async fn body_of(error: AppError) -> (StatusCode, WireBody) {
        let response = error.into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        let parsed: WireBody = serde_json::from_slice(&bytes).expect("json body");
        (status, parsed)
    }

    #[derive(Debug, Deserialize)]
    struct WireBody {
        error: String,
        message: String,
    }

    #[tokio::test]
    async fn a_5xx_never_echoes_a_filesystem_path_to_the_client() {
        // `StoreError::Io`'s Display embeds the absolute path. A corrupt
        // `auth.json` would otherwise answer with the install layout and the OS
        // username, and the differing wording per failure mode is an oracle for
        // the credential store's state.
        let leaky = odysseus_store::StoreError::Io {
            path: std::path::PathBuf::from("/home/alice/.odysseus/data/app.db"),
            source: std::io::Error::other("permission denied"),
        };
        let (status, body) = body_of(AppError::Store(leaky)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.message, "Internal error");
        assert!(
            !body.message.contains("/home/alice"),
            "the path leaked: {}",
            body.message
        );
        assert!(
            !body.message.contains("app.db"),
            "the filename leaked: {}",
            body.message
        );
    }

    #[tokio::test]
    async fn a_5xx_still_carries_a_stable_machine_readable_code() {
        // Redacting the message must not lose the code the frontend switches on.
        let (status, body) =
            body_of(AppError::Store(odysseus_store::StoreError::Interrupted)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "Internal error");
    }

    #[tokio::test]
    async fn a_4xx_keeps_its_message_because_the_caller_can_act_on_it() {
        // The opposite rule from the 5xx case, and it must be pinned: the
        // frontend shows these.
        let (status, body) = body_of(AppError::Forbidden("nope".into())).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body.message, "Forbidden: nope");
    }

    #[tokio::test]
    async fn an_auth_store_failure_is_a_500_and_never_a_401() {
        // A 401 here would tell the client its credentials were wrong and send it
        // to /login when the real problem is that auth.json is unreadable. No
        // test covered this arm before.
        let (status, _) = body_of(AppError::Store(odysseus_store::StoreError::Interrupted)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_ne!(status, StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn application_errors_map_to_the_python_exception_handler_statuses() {
        // Mirrors app.py:617-630.
        assert_eq!(
            AppError::SessionNotFound("s1".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            AppError::InvalidUpload("bad".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Llm("down".into()).status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            AppError::Search("down".into()).status(),
            StatusCode::BAD_GATEWAY
        );
    }

    #[test]
    fn unauthorized_is_distinct_from_forbidden() {
        // The frontend redirects to /login on 401. If "not allowed" returned 401
        // an admin-only feature would bounce the user to the login page.
        assert_eq!(AppError::Unauthenticated.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            AppError::Forbidden("admin only".into()).status(),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn owner_mismatch_is_not_found_not_forbidden() {
        // AGENTS.md §6: returning 403 for an owner mismatch would confirm the
        // resource exists.
        let err = AppError::NotFound("no such upload".into());
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_ne!(err.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn codes_are_stable_and_machine_readable() {
        assert_eq!(
            AppError::SessionNotFound("s".into()).code(),
            "SESSION_NOT_FOUND"
        );
        assert_eq!(
            AppError::InvalidUpload("f".into()).code(),
            "INVALID_FILE_UPLOAD"
        );
        assert_eq!(AppError::Llm("l".into()).code(), "LLM_SERVICE_ERROR");
        assert_eq!(AppError::Search("s".into()).code(), "WEB_SEARCH_ERROR");
    }
}
