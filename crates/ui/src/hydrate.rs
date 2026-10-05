//! The wasm entry point: hydrate the server-rendered document in place.
//!
//! This is the browser half of the split build. The server renders `App` to HTML
//! (see `crates/web/src/routes/system.rs::ui_shell`), and this code attaches
//! behaviour to *that existing HTML* rather than replacing it — which is why it
//! must call `hydrate_body` and not `mount_to_body`. Mounting fresh would discard
//! the server's markup and throw away the first-paint win `REWRITE_PLAN.md:188`
//! (D2) is buying, including the pre-paint theme script's CSS variables.
//!
//! **Why this is a module and not a `[[bin]]`.** cargo-leptos compiles the crate's
//! **lib** target to `wasm32-unknown-unknown` and runs `wasm-bindgen` over the
//! resulting cdylib; the bin target is the *server*. So the wasm entry has to be
//! reachable from `lib.rs`, or it would never end up in the artifact the browser
//! loads. The `#[wasm_bindgen(start)]` function below is what wasm-bindgen looks
//! for.
//!
//! The whole module is behind `#[cfg(feature = "hydrate")]` because the `hydrate`
//! feature is what selects Leptos's client rendering mode, and that mode is
//! mutually exclusive with `ssr`: a single build cannot ask Leptos to both render
//! to HTML and hydrate.

use wasm_bindgen::prelude::*;

use crate::App;

/// Leptos's expected wasm entry symbol.
///
/// `#[wasm_bindgen(start)]` runs this as the module's start routine, i.e. once the
/// document has been parsed — so the elements being hydrated are the ones the
/// server sent, not a half-built tree.
///
/// `pub` on a private module looks wrong to `unreachable_pub`, and clippy is right
/// that nothing can name it — but wasm-bindgen resolves the symbol through the
/// generated module exports, not through Rust path visibility, so narrowing this to
/// `pub(crate)` would not change what is reachable and would only obscure that the
/// symbol is intentionally exported to the JS side.
#[expect(
    unreachable_pub,
    reason = "wasm-bindgen exports this by attribute, not by Rust visibility"
)]
#[wasm_bindgen(start)]
pub fn hydrate() {
    // `console_error_panic_hook` turns a panic in a component into a readable
    // console error instead of the bare "Uncaught RuntimeError" wasm otherwise
    // produces, which is close to undiagnosable from a browser. Set before
    // anything else so it also covers hydration itself.
    console_error_panic_hook::set_once();

    leptos::mount::hydrate_body(App);
}
