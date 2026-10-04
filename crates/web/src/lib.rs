//! The Axum transport layer: routing, middleware, auth extraction, SSE encoding.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: business rules
//!
//! This is the **only** crate that knows about HTTP status codes. Application
//! crates return typed errors; the mapping to a status happens here
//! (`AGENTS.md` §7).
//!
//! # Layer order
//!
//! The stack in [`router`] is built outside-in in this order, and the order is a
//! security requirement rather than a style choice:
//!
//! 1. [`middleware::security`] — per-request CSP nonce, security headers
//! 2. [`middleware::cors`] — CORS
//! 3. [`middleware::timeout`] — request deadline
//! 4. [`middleware::slow_log`] — diagnostics
//! 5. [`middleware::auth`] — the authentication gate
//!
//! CORS sits **outside** the auth gate because a genuine preflight
//! (`OPTIONS` + `Access-Control-Request-Method`) carries no credentials by design
//! and must reach CORS to be answered. Gating it on auth 401s it before CORS can
//! respond, which blocks every cross-origin client before the real request is even
//! sent. See `API_MIGRATION.md` §2.1.

#![deny(missing_docs)]

pub mod app;
pub mod error;
pub mod identity;
pub mod middleware;
pub mod routes;

pub use {
    app::{AppState, router, router_without_auth, secured},
    error::{AppError, AppResult},
    identity::{AuthContext, BypassConfig},
};
