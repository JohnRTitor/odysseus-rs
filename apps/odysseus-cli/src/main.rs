//! Command-line access to the same operations as the web UI.
//!
//! The Python source ships ~30 `odysseus-*` CLI scripts with 24 dedicated test
//! files under `tests/cli/`, so the CLI is a real user surface rather than a
//! developer convenience. Subcommands land alongside the service they wrap.
//!
//! `println!` is denied workspace-wide (AGENTS.md §10); use `tracing`.

fn main() {
    tracing::info!("odysseus-cli: subcommands arrive with the services they wrap (T2300+)");
    tracing::info!("see TASKS.md for the phase that wires each command");
}
