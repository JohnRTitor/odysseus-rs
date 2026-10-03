//! The single Odysseus process.
//!
//! One Axum server serves Leptos SSR, hydration assets, the application APIs,
//! SSE streams, and authentication. See `REWRITE_PLAN.md` §1.
//!
//! `println!` is denied workspace-wide (AGENTS.md §10): use `tracing` instead.

use {odysseus_store::Store, tracing_subscriber::EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    // Phase 0 order matters: open the store and run migrations BEFORE the
    // listener binds, so the first request never races the schema. The Python
    // app did this as an import side effect (`core/database.py:2737`), which is
    // exactly the pattern we are replacing.
    let store = Store::open_default();
    store.migrate()?;

    // Bind is configurable for localhost and LAN. Mirrors APP_BIND / APP_PORT.
    let host = std::env::var("APP_BIND").unwrap_or_else(|_| "127.0.0.1".to_owned());
    let port: u16 = std::env::var("APP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7000);
    let addr = format!("{host}:{port}");

    tracing::info!(%addr, "odysseus-server starting");

    // The router is built in Phase 0.5 (T0080-T0094). Until then there is
    // nothing to serve, so bind nothing -- an unreachable port is far more
    // honest than a 200 that returns nothing.
    let _ = addr;
    Err("router not wired yet (T0080)".into())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("ODYSSEUS_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}
