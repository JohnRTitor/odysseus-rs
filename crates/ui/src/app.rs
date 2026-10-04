//! The application shell and its routes.
//!
//! Phase 0's last unproven assumption is that the server can render a Leptos app
//! at all, so this is deliberately a **shell**, not the product. It proves SSR,
//! hydration and the pre-paint theme script work; it does not pretend to be chat.
//!
//! `AGENTS.md` N2 is the constraint that shapes this file: an incomplete feature
//! renders **disabled with a reason** rather than as a plausible-looking stub. So
//! every route here states what it will eventually do and that it is not built yet,
//! and none of them accepts input or shows invented data.
//!
//! Layering note: `SSR` renders these components to HTML on the server;
//! `hydrate` attaches the client behaviour to that HTML in the browser. The two
//! builds share this file, so anything here must be valid in both — which is why
//! there is no `window` or `document` access outside the pre-paint script, and that
//! script is a string constant rather than Rust code. See [`prepaint_script`].

use {
    leptos::prelude::*,
    leptos_meta::{Link, Meta, Title, provide_meta_context},
    leptos_router::{
        components::{Route, Router, Routes},
        path,
    },
};

/// The pre-paint theme script, verbatim from `static/index.html:19-71`.
///
/// **This must stay a string, not Rust.** It runs in the browser before first paint,
/// so it cannot be compiled from Rust — but it also cannot be *tested* as Rust,
/// which is why the colour arithmetic it depends on is ported and tested in
/// [`crate::theme`]. Those two copies are the real duplication risk in this file and
/// the reason `theme.rs` exists at all; see T0107 for the test that compares them.
///
/// It is inlined rather than fetched because a `fetch` would not complete before
/// paint, which is the entire problem this script exists to solve.
pub const PREPAINT_SCRIPT: &str = include_str!("prepaint.js");

/// What `/settings` will eventually hold. Named so the string is greppable from both
/// the Rust and the rendered HTML.
pub const NOT_BUILT_SETTINGS: &str = "Account, provider and appearance settings.";

/// Root component: metadata, the pre-paint script, then the router.
///
/// The script is emitted **before** any styled content so the browser executes it
/// during head parsing, which is what makes the CSS custom properties correct on
/// the first painted frame rather than after a flash of the wrong theme.
#[expect(
    clippy::must_use_candidate,
    reason = "`#[component]` expands the function and does not carry a `#[must_use]` placed before it through"
)]
#[component]
pub fn App() -> impl IntoView {
    // `leptos_meta` collects the head elements through a context and emits the
    // `<html>/<head>/<body>` document itself during SSR, so there is no `<Html>` or
    // `<head>` element here to write. `provide_meta_context` is what registers that
    // collector; without it every `<Meta>` below renders into nothing.
    provide_meta_context();

    // The nonce is attached by the security-headers middleware, which inserts it
    // into request extensions before rendering. Under `hydrate` there is no
    // request, so the script is emitted without one — and a CSP shipping with a
    // hydrated page must therefore allow it by hash rather than by nonce. That
    // decision belongs to T0112, which tightens `script-src`; it is named here
    // because this component is where the two halves have to meet.
    let nonce = use_context::<Nonce>();
    let nonce_attr = nonce.map(|n| n.0);

    view! {
        <Title text="Odysseus"/>
        <Meta charset="utf-8"/>
        <Meta name="viewport" content="width=device-width, initial-scale=1"/>
        <Meta name="format-detection" content="telephone=no, date=no, address=no, email=no"/>
        <Meta name="theme-color" content="#282c34"/>
        <Link rel="icon" href="/static/favicon.svg"/>
        // Emitted first among the body-adjacent elements so the browser runs it
        // during head parsing. That ordering *is* the mechanism: a script that
        // arrives after the stylesheet has painted cannot prevent the flash of the
        // wrong theme.
        <script inner_html=PREPAINT_SCRIPT nonce=nonce_attr></script>
        <Router>
            <Routes fallback=NotFound>
                <Route path=path!("") view=Home/>
                <Route path=path!("/settings") view=Settings/>
            </Routes>
        </Router>
    }
}

/// The CSP nonce for this request, stashed by the web layer before rendering.
///
/// A newtype rather than a bare `String` so it cannot be confused with any other
/// string that happens to be in context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nonce(pub String);

/// `/` — the shell.
///
/// Phase 0's acceptance: the server renders this to real HTML. The content is
/// minimal on purpose; the interesting parts are the boot markers below.
#[expect(
    clippy::must_use_candidate,
    reason = "`#[component]` expands the function and does not carry a `#[must_use]` placed before it through"
)]
#[component]
pub fn Home() -> impl IntoView {
    view! {
        <main id="app-shell" data-testid="app-shell">
            <h1>"Odysseus"</h1>
            <p data-testid="build-status">
                "Server-rendered shell. Chat, sessions and tools are not built yet."
            </p>
            <noscript>
                <p>"This application requires JavaScript for everything except this page."</p>
            </noscript>
        </main>
    }
}

/// `/settings` — reserved, and rendered honestly.
///
/// N2: a route that is not built says so and says what it will be, rather than
/// rendering a spinner, a fake settings list, or a 200 that pretends. A concrete
/// component per placeholder rather than one parameterised by a string, because the
/// macro's handling of a `&'static str` prop is awkward and each future placeholder
/// will want its own disabled controls anyway.
#[expect(
    clippy::must_use_candidate,
    reason = "`#[component]` expands the function and does not carry a `#[must_use]` placed before it through"
)]
#[component]
pub fn Settings() -> impl IntoView {
    view! {
        <main id="app-shell" data-testid="not-built">
            <h1>"Settings"</h1>
            <p data-testid="not-built-feature">{NOT_BUILT_SETTINGS}</p>
            <p><a href="/">"Back to the shell"</a></p>
        </main>
    }
}

/// The catch-all, so an unknown path is a 404 we render rather than axum's plain one.
#[expect(
    clippy::must_use_candidate,
    reason = "`#[component]` expands the function and does not carry a `#[must_use]` placed before it through"
)]
#[component]
pub fn NotFound() -> impl IntoView {
    view! {
        <main id="app-shell" data-testid="not-found">
            <h1>"Not found"</h1>
            <p>"No route matches this path."</p>
            <p><a href="/">"Back to the shell"</a></p>
        </main>
    }
}

#[cfg(test)]
mod tests {
    use {super::PREPAINT_SCRIPT, crate::theme::THEMES};

    #[test]
    fn the_prepaint_script_is_actually_inlined() {
        // An empty or stub script would satisfy `include_str!` and reintroduce the
        // flash of unstyled content this exists to prevent.
        assert!(
            PREPAINT_SCRIPT.len() > 500,
            "the pre-paint script is suspiciously short ({} bytes) — is it a stub?",
            PREPAINT_SCRIPT.len()
        );
    }

    #[test]
    fn the_prepaint_script_sets_css_variables_before_they_can_be_read() {
        // It has to write to `document.documentElement.style`, which is the only
        // element whose custom properties the stylesheet resolves against.
        assert!(
            PREPAINT_SCRIPT.contains("document.documentElement"),
            "the script must set properties on the root element"
        );
    }

    #[test]
    fn the_prepaint_script_derives_every_syntax_variable_the_theme_module_does() {
        // The duplication risk between this string and `crate::theme`: if the script
        // sets a variable `theme.rs` does not derive, the SSR-rendered page and the
        // hydrated page disagree. Counting the names on both sides makes that
        // failure loud instead of visual.
        let from_script = PREPAINT_SCRIPT
            .match_indices("--hl-")
            .map(|(i, _)| {
                let rest = &PREPAINT_SCRIPT[i..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                    .unwrap_or(rest.len());
                rest[..end].to_owned()
            })
            .collect::<std::collections::BTreeSet<_>>();

        let from_rust = crate::theme::syntax_vars(&THEMES[0].1)
            .expect("the first palette entry must derive")
            .iter()
            .map(|(n, _)| n.to_owned())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(
            from_script, from_rust,
            "the pre-paint script and theme.rs must set the same --hl-* variables"
        );
        assert_eq!(
            from_script.len(),
            10,
            "ten syntax variables, per index.html"
        );
    }

    #[test]
    fn the_prepaint_script_reads_the_theme_before_it_paints() {
        // `localStorage` is where the saved theme lives. If the script does not read
        // it, the derivation runs on defaults and the user's choice is ignored until
        // something else re-applies it.
        assert!(
            PREPAINT_SCRIPT.contains("localStorage"),
            "the saved theme must be read from localStorage"
        );
    }
}
