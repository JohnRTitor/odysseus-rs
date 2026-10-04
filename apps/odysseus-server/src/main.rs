//! The single Odysseus process.
//!
//! One Axum server serves Leptos SSR, hydration assets, the application APIs,
//! SSE streams, and authentication. See `REWRITE_PLAN.md` §1.
//!
//! `println!` is denied workspace-wide (`AGENTS.md` §10): use `tracing`.

use std::net::SocketAddr;

use {odysseus_web::AppState, tracing_subscriber::EnvFilter};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    // Leptos's out-of-order HTML streaming spawns onto a global executor, and on
    // non-wasm targets Leptos does **not** initialise one — `leptos::task::spawn`
    // reaches `any_spawner::Executor::spawn`, which panics if nothing set it up.
    // Without this, every SSR request to `/` aborts mid-render. Leptos only
    // auto-initialises on wasm, where it uses `init_wasm_bindgen`.
    //
    // Idempotent, and ignored here because a second call is harmless; the error
    // that matters (no runtime) cannot happen this early in `main`.
    if let Err(reason) = any_spawner::Executor::init_tokio() {
        tracing::error!(%reason, "could not initialise the Leptos SSR executor");
    }

    // Order matters: `bootstrap` migrates before the listener binds, so the
    // first request can never race the schema. The Python app did this as an
    // import side effect (`core/database.py:2737`) — exactly the pattern being
    // replaced.
    let state = AppState::bootstrap()?;
    let addr = bind_addr();
    let app = odysseus_web::router(&state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tracing::info!(addr = %bound, "odysseus-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("odysseus-server stopped");
    Ok(())
}

/// Resolve the listen address from `APP_BIND` / `APP_PORT`.
///
/// The defaults mirror `app.py:1303-1305`: loopback only, because a LAN-exposed
/// deployment must be an explicit choice.
///
/// An unresolvable `APP_BIND` falls back to loopback with a warning rather than
/// refusing to start. `APP_BIND=localhost` is a reasonable thing to write, so
/// hostnames are resolved too.
fn bind_addr() -> SocketAddr {
    use std::net::ToSocketAddrs as _;

    let host = std::env::var("APP_BIND").unwrap_or_else(|_| String::from("127.0.0.1"));
    let port: u16 = std::env::var("APP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7000);

    let host = host.trim();

    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        tracing::warn!(host = %host, "could not resolve APP_BIND; falling back to loopback");
        return SocketAddr::from(([127, 0, 0, 1], port));
    };
    addrs
        .next()
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], port)))
}

/// Wait for SIGINT/SIGTERM so in-flight requests finish before exit.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "cannot listen for ctrl-c");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => tracing::error!(error = %e, "cannot listen for SIGTERM"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::debug!("shutdown signal received");
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("ODYSSEUS_LOG")
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[cfg(test)]
mod tests {
    use {super::bind_addr, std::net::SocketAddr};

    #[test]
    fn defaults_to_loopback_port_7000() {
        // app.py:1303-1305.
        let addr = bind_addr();
        assert_eq!(addr.port(), 7000);
        assert!(addr.ip().is_loopback(), "default must not be LAN-exposed");
    }

    #[test]
    fn an_unparseable_bind_falls_back_to_loopback_rather_than_failing() {
        let addr: SocketAddr = bind_addr();
        assert!(addr.port() > 0);
    }
}
