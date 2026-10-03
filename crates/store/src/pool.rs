//! Per-connection SQLite configuration.
//!
//! The Python source attaches a class-level `Engine` listener so these apply to
//! *every* connection (`core/database.py:142-147`). There is no shared engine
//! in this design, so the same guarantee is reproduced here, per connection.

use rusqlite::Connection;

use crate::{Result, StoreError};

/// Applies every pragma the application depends on.
///
/// # Errors
///
/// Returns an error if any pragma fails. `foreign_keys` in particular is a
/// correctness requirement, not a tuning knob: `ON DELETE CASCADE` on
/// `chat_messages` and the `ON DELETE SET NULL` chains in the schema are no-ops
/// without it.
pub fn configure(conn: &Connection) -> Result<()> {
    // `foreign_keys` is a no-op inside a transaction, so it must be set per
    // connection before any other statement.
    set(conn, "PRAGMA foreign_keys = ON")?;
    // Required once `spawn_blocking` (AGENTS.md §5.1) allows real write
    // concurrency; without it a contending writer returns SQLITE_BUSY
    // immediately instead of waiting.
    set(conn, "PRAGMA busy_timeout = 5000")?;
    // Explicit transactions for multi-statement writes. Rust gives us this by
    // default anyway, but pinning it keeps behaviour identical if the default
    // ever changes.
    set(conn, "PRAGMA legacy_alter_table = OFF")?;
    Ok(())
}

/// Registers the `sqlite-vec` extension so `vec0` virtual tables can be used.
///
/// # Safety
///
/// `sqlite3_auto_extension` registers a process-global initialiser. Calling
/// this more than once is harmless (the extension is idempotent), and the
/// function pointer is `'static`, so this is sound. It is isolated in one
/// function so callers are not tempted to sprinkle `unsafe` around.
///
/// # Errors
///
/// Returns an error if `vec_version()` does not answer afterwards, which means
/// the extension failed to register.
#[expect(
    unsafe_code,
    reason = "sqlite3_auto_extension takes a raw C function pointer; `sqlite-vec` \
              exports `sqlite3_vec_init` as a `'static` extern \"C\" symbol with no \
              captured state, which is exactly the shape the API expects. Correctness \
              is verified by the `vec_version()` query below, so a bad registration \
              fails loudly instead of silently."
)]
pub fn register_vec_extension() -> Result<()> {
    // SAFETY: `sqlite3_vec_init` is an `extern "C"` function exported by the
    // `sqlite-vec` crate. Transmuting it to a plain `fn()` pointer is the exact
    // conversion SQLite's C API requires. The pointer is `'static`, the
    // registration is process-global, and it is idempotent, so calling this
    // more than once is sound.
    unsafe {
        let init: unsafe extern "C" fn(
            *mut rusqlite::ffi::sqlite3,
            *mut *mut i8,
            *const rusqlite::ffi::sqlite3_api_routines,
        ) -> i32 = std::mem::transmute(sqlite_vec::sqlite3_vec_init as *const ());
        rusqlite::ffi::sqlite3_auto_extension(Some(init));
    }
    let conn = Connection::open_in_memory()?;
    let version: String = conn
        .query_row("SELECT vec_version()", [], |row| row.get(0))
        .map_err(|e| StoreError::Vec(e.to_string()))?;
    tracing::debug!(%version, "sqlite-vec extension registered");
    Ok(())
}

fn set(conn: &Connection, sql: &str) -> Result<()> {
    conn.execute_batch(sql)?;
    Ok(())
}
