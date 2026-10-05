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
    leptos_meta::{Link, Meta, Stylesheet, Title, provide_meta_context},
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

/// The boot loader's behaviour, ported from `static/index.html:259-293`.
///
/// Inline and nonce-gated rather than bundled, because its 5 s fallback exists for
/// exactly the case where the bundle never arrives — a script inside the bundle
/// could not cover its own absence. See [`LOADER_SCRIPT`]'s own header for the CSP
/// constraint that rules out the original's `innerHTML` interpolation.
pub const LOADER_SCRIPT: &str = include_str!("loader.js");

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
    // `leptos_meta` collects `<Title>`/`<Meta>`/`<Link>` into a context and then
    // **injects them by searching the rendered HTML for `</head>`** — see
    // `MetaContext::inject_meta_context`, which panics with "you are using
    // leptos_meta without a </head> tag" when it cannot find one. So the document
    // tags below are load-bearing, not decoration: `leptos_meta::Html` is a
    // *client-side* component that sets attributes on an existing
    // `document.documentElement`, and under SSR it emits nothing.
    provide_meta_context();

    // The nonce is attached by the security-headers middleware, which inserts it
    // into request extensions before rendering. Under `hydrate` there is no
    // request, so the script is emitted without one — and a CSP shipping with a
    // hydrated page must therefore allow it by hash rather than by nonce. That
    // decision belongs to T0112, which tightens `script-src`; it is named here
    // because this component is where the two halves have to meet.
    let nonce = use_context::<Nonce>();
    let nonce_attr = nonce.map(|n| n.0.clone());

    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <Title text="Odysseus"/>
                <Meta charset="utf-8"/>
                <Meta name="viewport" content="width=device-width, initial-scale=1"/>
                <Meta
                    name="format-detection"
                    content="telephone=no, date=no, address=no, email=no"
                />
                <Meta name="theme-color" content="#282c34"/>
                <Link rel="icon" href="/static/favicon.svg"/>
                // The compiled Tailwind output. cargo-leptos names it after
                // `output-name` in the workspace metadata, and `crates/web`
                // serves that directory at `/pkg`. It is a `<Stylesheet>` rather
                // than a `<Link>` so `leptos_meta` owns its placement and a
                // per-route override can replace it later.
                //
                // Without this the page renders completely unstyled — the shell
                // is semantic HTML and nothing else. INCOMPLETE.md 2.4 records
                // that rather than claiming styling that is not built.
                <Stylesheet id="leptos" href="/pkg/odysseus-ui.css"/>
                // Emitted inside <head> and before any styled content so the
                // browser runs it during head parsing. That ordering *is* the
                // mechanism: a script arriving after the stylesheet has painted
                // cannot prevent the flash of the wrong theme.
                <script inner_html=PREPAINT_SCRIPT nonce=nonce_attr.clone()></script>
            </head>
            <body>
                // T0105. Rendered server-side, before the router, because its whole
                // job is to cover the gap between the first byte and hydration —
                // if it were client-rendered it would arrive after the flash it
                // exists to hide.
                //
                // Every style is a Tailwind class. The Python original carries
                // eight inline `style=` declarations on these two elements, which
                // `style-src 'self'` now refuses outright (T0112). This is T0111
                // applied prospectively: the port does not re-introduce the escape
                // it would then have to be un-gated to support.
                <div
                    id="app-loader"
                    class="fixed inset-0 z-[99999] flex flex-col items-center \
                           justify-center gap-2 bg-[var(--bg,#282c34)] \
                           transition-opacity duration-300"
                >
                    <div
                        id="loader-wave"
                        class="relative whitespace-pre pt-[10px] font-mono text-[11px] \
                               leading-none opacity-50 \
                               text-[color-mix(in_srgb,#9cdef2_62%,var(--brand-color,var(--red,#e06c75)))]"
                    >
                        "▁▂▃"
                    </div>
                </div>
                // Inline and nonce-gated rather than part of the wasm hydrate path,
                // because the 5 s fallback inside it exists precisely for a boot
                // that never reaches the app bundle. See `loader.js`.
                <script inner_html=LOADER_SCRIPT nonce=nonce_attr></script>
                <Router>
                    <Routes fallback=NotFound>
                        <Route path=path!("") view=Home/>
                        <Route path=path!("/settings") view=Settings/>
                    </Routes>
                </Router>
            </body>
        </html>
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
    // Rendering a 404 page with a 200 status is a real defect, not a nit: caches
    // and crawlers key on the status, and a monitoring check for 4xx would see a
    // healthy site. The server layer provides `ResponseOptions` because it owns the
    // response; under `hydrate` there is no server response to amend, so the
    // whole block is cfg'd out rather than left to fail on a missing dependency.
    #[cfg(feature = "ssr")]
    if let Some(options) = use_context::<leptos_axum::ResponseOptions>() {
        options.set_status(http::StatusCode::NOT_FOUND);
    }

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
    use {
        super::{LOADER_SCRIPT, PREPAINT_SCRIPT},
        crate::theme::THEMES,
    };

    /// T0105. The loader's whole purpose is to cover the gap between the first
    /// byte and hydration, so it has to be in the server's HTML — not something a
    /// client-side effect adds later, which would arrive after the flash it exists
    /// to hide.
    #[test]
    fn the_loader_overlay_is_present_and_needs_no_inline_style() {
        // Asserted against the *source*, since these are the two elements the
        // Python gave eight inline `style=` declarations. Under `style-src 'self'`
        // (T0112) the browser drops such an attribute with no error, leaving an
        // unpositioned, unstyled overlay — the worst possible failure, because a
        // loader that does not cover is worse than no loader.
        let app = include_str!("app.rs");
        for id in ["app-loader", "loader-wave"] {
            assert!(
                app.contains(&format!("id=\"{id}\"")),
                "the {id} overlay element must be server-rendered"
            );
        }
    }

    /// The CSP trap this port had to avoid: the Python interpolated a `style=`
    /// attribute into an `innerHTML` string, and CSP checks attributes in markup
    /// parsed that way just as it does in server HTML. A regression here is
    /// invisible server-side, so it is asserted here.
    #[test]
    fn the_loader_script_never_writes_an_inline_style_attribute() {
        assert!(
            !LOADER_SCRIPT.contains("style="),
            "the loader script writes a style attribute; CSP blocks style= in \
             markup parsed via innerHTML, so the moving dot would lose its position"
        );
        assert!(
            !LOADER_SCRIPT.contains("cssText"),
            "cssText assignment is another way to smuggle in an inline style block"
        );
        // The sanctioned alternative: the CSSOM, which CSP does not gate.
        assert!(
            LOADER_SCRIPT.contains("setProperty"),
            "the per-frame offset must move to the dot through a CSS custom property"
        );
    }

    /// The nine frames are data, not decoration. `render()` indexes them by `i`, so a
    /// dropped or reordered frame changes the animation's rhythm rather than
    /// failing visibly.
    #[test]
    fn the_loader_frames_match_the_python_source_exactly() {
        let expected = [
            ("▁▂▃", 9),
            ("▂▃▄", 7),
            ("▃▄▅", 5),
            ("▄▅▆", 3),
            ("▅▆▅", 1),
            ("▆▅▄", 3),
            ("▅▄▃", 5),
            ("▄▃▂", 7),
            ("▃▂▁", 9),
        ];
        for (wave, y) in expected {
            assert!(
                LOADER_SCRIPT.contains(&format!("wave: '{wave}', y: {y}")),
                "missing or altered frame {wave} at y={y}"
            );
        }
        assert_eq!(
            LOADER_SCRIPT.matches("wave: '").count(),
            expected.len(),
            "the frame list must hold exactly {} entries",
            expected.len()
        );
    }

    /// The 5 s fallback must **remove** the node, not just fade it. The app reads
    /// the loader's presence as "startup in progress" and holds off clearing the
    /// composer while it exists, so a fade that leaves the node wedges the app
    /// permanently — the fallback would then be worse than no fallback.
    #[test]
    fn the_loader_fallback_removes_the_node_rather_than_only_fading_it() {
        assert!(
            LOADER_SCRIPT.contains("FALLBACK_MS = 5000"),
            "the fallback timing is part of the contract"
        );
        assert!(
            LOADER_SCRIPT.contains("node.remove()"),
            "the fallback must remove the overlay, or the app never finishes booting"
        );
        let remove_at = LOADER_SCRIPT
            .find("node.remove()")
            .expect("removal present");
        let fade_at = LOADER_SCRIPT
            .find("node.style.opacity = '0'")
            .expect("fade present");
        assert!(
            fade_at < remove_at,
            "the fade must run first and the node be removed after it, or it pops"
        );
    }

    /// The stop hook is how the app halts the interval. It is exposed on `window`
    /// because the interval is owned by this script, so the caller has no other
    /// handle — the Python does the same with `__odysseusLoaderWaveStop`.
    #[test]
    fn the_loader_exposes_the_stop_hook_the_shell_depends_on() {
        assert!(
            LOADER_SCRIPT.contains("window.__odysseusLoaderWaveStop"),
            "the app stops the wave through this hook"
        );
    }

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
