//! Blocking-call helpers.
//!
//! AGENTS.md §5.1 (decision D11): `rusqlite` is synchronous and must never be
//! called from the async runtime. Every DB access from an async context goes
//! through this module, which is the only sanctioned way in.
//!
//! Getting this wrong silently blocks the executor and is the single most
//! likely cause of the Rust app being slower than the Python app it replaces.
//!
//! [`with_conn`] is the shape almost every call site wants: open a connection,
//! do the work, close it — all inside one blocking closure, so nothing can be
//! held across an `await`.

use {rusqlite::Connection, tokio::task::JoinError};

use crate::{Store, StoreError};

/// Runs a blocking closure on Tokio's blocking thread pool.
///
/// Prefer [`with_conn`] for database work; this exists for the other blocking
/// operations the store owns.
///
/// A panic inside the closure is resumed on the caller so the original message
/// and location survive instead of surfacing as an opaque `JoinError`. A
/// cancellation — which happens when the runtime shuts down — is reported as
/// [`JoinError`].
///
/// # Errors
///
/// Returns [`JoinError`] if the task was cancelled or panicked.
pub async fn spawn_blocking<F, T>(f: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await
}

/// Opens a connection, runs `f`, and returns its result — on the blocking pool.
///
/// The connection is opened *and closed* inside one blocking closure, which is
/// what makes D11 structurally enforced rather than merely documented: there is
/// no `Connection` value a caller could accidentally hold across an `await`.
///
/// # Errors
///
/// Returns `E` from `f`. `StoreError` converts into `E`, so an open failure and
/// a query failure travel the same path.
pub async fn with_conn<F, T, E>(store: Store, f: F) -> Result<T, E>
where
    F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: From<StoreError> + Send + 'static,
{
    match tokio::task::spawn_blocking(move || {
        let conn = store.connect()?;
        f(&conn)
    })
    .await
    {
        Ok(result) => result,
        // Preserve the original panic payload rather than degrading it to a
        // string: a `JoinError` from a panicking task carries the real message.
        Err(join_err) if join_err.is_panic() => {
            std::panic::resume_unwind(join_err.into_panic());
        }
        Err(_) => Err(StoreError::Interrupted.into()),
    }
}
