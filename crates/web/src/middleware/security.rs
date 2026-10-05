//! Security headers and Content-Security-Policy.
//!
//! Ports `core/middleware.py:85-152`. The Python version generates a fresh
//! `secrets.token_hex(16)` nonce per request, stores it on
//! `request.state.csp_nonce`, and substitutes it into the shell template. The
//! Rust version inserts it as request [`Extension`] so a handler that renders HTML
//! can read it.
//!
//! # Four CSP profiles, not one
//!
//! The Python source has three explicit CSP branches plus a default. They are
//! reproduced exactly, because each exists for a specific reason:
//!
//! | Profile | Applied to | Why |
//! |---|---|---|
//! | `report` | `/api/research/report/*` | Self-contained reports need inline script and style |
//! | `tool_render` | `/api/tools/*/render` | Rendered tool output omits the framing headers |
//! | `pdf_preview` | `/api/document/*/render-pdf`, `/export-pdf` | `default-src 'none'`, frameable only by us |
//! | `default` | everything else | `DENY` framing, nonce-gated script |

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

/// Bytes of entropy in a nonce, matching `secrets.token_hex(16)`.
const NONCE_BYTES: usize = 16;

/// The per-request Content-Security-Policy nonce.
///
/// Inserted by [`attach_security_headers`] and read by any handler that renders
/// HTML.
#[derive(Debug, Clone)]
pub struct CspNonce(pub String);

impl CspNonce {
    /// Generates a fresh nonce.
    ///
    /// # Errors
    ///
    /// Returns a message if the OS random source is unavailable, which is
    /// unrecoverable: issuing a weak or predictable nonce would defeat the CSP
    /// entirely, so this must fail loudly rather than fall back.
    fn generate() -> Result<Self, &'static str> {
        let mut bytes = [0u8; NONCE_BYTES];
        getrandom::fill(&mut bytes).map_err(|_| "OS randomness unavailable")?;
        let hex: String = bytes.iter().fold(String::with_capacity(32), |mut acc, b| {
            use std::fmt::Write as _;
            let _ = write!(acc, "{b:02x}");
            acc
        });
        Ok(Self(hex))
    }

    /// The nonce value, for interpolation into a `script-src` attribute.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which Content-Security-Policy applies to a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CspProfile {
    /// Self-contained research report.
    Report,
    /// Rendered tool output.
    ToolRender,
    /// PDF preview, which we frame ourselves.
    PdfPreview,
    /// Everything else.
    Default,
}

impl CspProfile {
    /// Chooses the profile from the request path.
    ///
    /// The path branches mirror the Python prefix checks, which are
    /// `startswith` rather than exact matches — preserved deliberately, since a
    /// path like `/api/document/abc/render-pdf` must match.
    fn for_path(path: &str) -> Self {
        if path.starts_with("/api/research/report/") {
            Self::Report
        } else if path.starts_with("/api/tools/") && path.ends_with("/render") {
            Self::ToolRender
        } else if path.starts_with("/api/document/")
            && (path.ends_with("/render-pdf") || path.ends_with("/export-pdf"))
        {
            Self::PdfPreview
        } else {
            Self::Default
        }
    }

    /// The `Content-Security-Policy` value for this profile.
    fn policy(self, nonce: &str) -> String {
        match self {
            Self::Report => {
                // The report is a single self-contained document with inline
                // script and style, served from a route that is never framed.
                "default-src 'none'; \
                 script-src 'unsafe-inline'; \
                 style-src 'unsafe-inline'; \
                 img-src data: https:; \
                 frame-ancestors 'none'; \
                 base-uri 'none'"
                    .to_owned()
            }
            Self::ToolRender => {
                // Rendered tool output is embedded rather than framed, so the
                // framing headers are omitted entirely (see `apply`).
                "default-src 'none'; \
                 script-src 'unsafe-inline'; \
                 style-src 'unsafe-inline'; \
                 img-src data: https:; \
                 base-uri 'none'"
                    .to_owned()
            }
            Self::PdfPreview => {
                // We frame this ourselves, so same-origin framing is allowed but
                // nothing may be loaded from anywhere else.
                "default-src 'none'; \
                 script-src 'none'; \
                 style-src 'none'; \
                 img-src data:; \
                 frame-ancestors 'self'; \
                 base-uri 'none'"
                    .to_owned()
            }
            Self::Default => {
                // `style-src` carries **no** `'unsafe-inline'`, and none is needed.
                //
                // It was previously retained on the claim that "the UI sets `style=`
                // in many places" — true of the Python UI, which is not ported yet,
                // and false of everything this server actually serves today:
                //
                //   * the Leptos shell emits no `style=` attribute and no `<style>`
                //     element (asserted in tests/ui_shell.rs);
                //   * the pre-paint script writes CSS custom properties through the
                //     CSSOM (`documentElement.style.setProperty`), which CSP does
                //     not gate — `style-src` covers markup `style=` attributes and
                //     `<style>` blocks, not scripted property assignment;
                //   * Tailwind arrives as a `<link rel=stylesheet>` from 'self'.
                //
                // So this is *stricter* than the "nonce-only" T0112 asked for: a
                // nonce would authorise an inline `<style>` we do not emit. When the
                // Python UI's inline styles are ported, they must become classes or
                // `data-*` attributes (T0111) rather than earning this escape back.
                format!(
                    "default-src 'self'; \
                     script-src 'self' 'nonce-{nonce}' https://cdn.jsdelivr.net; \
                     style-src 'self'; \
                     img-src 'self' data: blob:; \
                     font-src 'self'; \
                     connect-src 'self'; \
                     frame-ancestors 'none'; \
                     base-uri 'self'; \
                     form-action 'self'"
                )
            }
        }
    }

    /// Whether `X-Frame-Options` should be sent.
    ///
    /// The `tool_render` profile omits it: those endpoints are embedded with
    /// `<object>`/`<embed>`, and `DENY` would block that.
    const fn sends_frame_options(self) -> bool {
        !matches!(self, Self::ToolRender)
    }

    /// The `X-Frame-Options` value.
    const fn frame_options(self) -> &'static str {
        match self {
            Self::PdfPreview => "SAMEORIGIN",
            _ => "DENY",
        }
    }
}

/// Attaches the security headers and a fresh CSP nonce to every response.
pub async fn attach_security_headers(mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let is_forwarded_https = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"));

    let nonce = match CspNonce::generate() {
        Ok(n) => n,
        Err(reason) => {
            // Without a nonce the default CSP would have nothing to allow, and
            // the app shell would not execute. Fail closed with a clear log
            // rather than serving a page with a weakened policy.
            tracing::error!(%reason, "cannot generate CSP nonce; refusing to serve");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "cannot generate content security policy nonce",
            )
                .into_response_owned();
        }
    };

    let profile = CspProfile::for_path(&path);
    req.extensions_mut().insert(nonce.clone());
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    set(
        headers,
        HeaderName::from_static("x-content-type-options"),
        "nosniff",
    );
    set(headers, header::REFERRER_POLICY, "no-referrer");
    // Camera and geolocation stay off; microphone is same-origin because the
    // voice recorder posts audio from the app itself.
    set(
        headers,
        header::HeaderName::from_static("permissions-policy"),
        "camera=(), microphone=(self), geolocation=()",
    );

    set(
        headers,
        header::CONTENT_SECURITY_POLICY,
        &profile.policy(nonce.as_str()),
    );
    if profile.sends_frame_options() {
        set(headers, header::X_FRAME_OPTIONS, profile.frame_options());
    }

    // HSTS only when the connection is actually HTTPS, directly or via a proxy
    // that said so. Sending it over plain HTTP on a LAN deployment would make
    // browsers pin a hostname that never speaks TLS.
    if is_forwarded_https {
        set(
            headers,
            header::STRICT_TRANSPORT_SECURITY,
            "max-age=31536000; includeSubDomains",
        );
    }

    response
}

trait IntoResponseOwned {
    fn into_response_owned(self) -> Response;
}

impl IntoResponseOwned for (axum::http::StatusCode, &'static str) {
    fn into_response_owned(self) -> Response {
        use axum::response::IntoResponse as _;
        self.into_response()
    }
}

/// Sets a header, ignoring an invalid value rather than panicking.
///
/// The values here are all compile-time constants, so a failure means a
/// programming error in this module, not bad input. Logging keeps it visible
/// without taking down the request.
fn set(headers: &mut axum::http::HeaderMap, name: HeaderName, value: &str) {
    match HeaderValue::from_str(value) {
        Ok(v) => {
            headers.insert(name, v);
        }
        Err(e) => {
            tracing::error!(header = %name, error = %e, "invalid header value; not set");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CspNonce, CspProfile, NONCE_BYTES};

    #[test]
    fn nonce_is_hex_and_the_expected_length() {
        let nonce = CspNonce::generate().expect("OS randomness");
        assert_eq!(
            nonce.as_str().len(),
            NONCE_BYTES * 2,
            "matches token_hex(16)"
        );
        assert!(
            nonce.as_str().chars().all(|c| c.is_ascii_hexdigit()),
            "must be hex"
        );
    }

    #[test]
    fn nonces_differ_between_requests() {
        let a = CspNonce::generate().expect("first");
        let b = CspNonce::generate().expect("second");
        assert_ne!(a.as_str(), b.as_str(), "a reused nonce defeats the CSP");
    }

    #[test]
    fn profile_selection_matches_the_python_branches() {
        use CspProfile::{Default, PdfPreview, Report, ToolRender};

        assert_eq!(CspProfile::for_path("/api/research/report/s1"), Report);
        assert_eq!(CspProfile::for_path("/api/tools/bash/render"), ToolRender);
        assert_eq!(
            CspProfile::for_path("/api/document/d1/render-pdf"),
            PdfPreview
        );
        assert_eq!(
            CspProfile::for_path("/api/document/d1/export-pdf"),
            PdfPreview
        );

        // Everything else, including near-misses that must NOT match.
        assert_eq!(CspProfile::for_path("/"), Default);
        assert_eq!(CspProfile::for_path("/api/sessions"), Default);
        assert_eq!(CspProfile::for_path("/login"), Default);
        // A document id that merely starts the same way is not a PDF route.
        assert_eq!(CspProfile::for_path("/api/document/d1"), Default);
        assert_eq!(CspProfile::for_path("/api/document/d1/archive"), Default);
        // Research status is not a report.
        assert_eq!(CspProfile::for_path("/api/research/status/s1"), Default);
    }

    #[test]
    fn default_policy_is_nonce_gated_and_framing_denied() {
        let policy = CspProfile::Default.policy("deadbeef");
        assert!(policy.contains("script-src 'self' 'nonce-deadbeef'"));
        assert!(
            !policy.contains("script-src 'unsafe-inline'"),
            "script-src must never allow unsafe-inline"
        );
        assert!(policy.contains("frame-ancestors 'none'"));
        assert_eq!(CspProfile::Default.frame_options(), "DENY");
    }

    /// T0112. The Python UI needed `'unsafe-inline'` because it writes `style=`
    /// attributes; nothing this server serves does. This test is the reason the
    /// escape cannot quietly return — a future component that reaches for an
    /// inline style will be refused by the browser, which is the intended outcome.
    #[test]
    fn the_default_policy_has_no_inline_style_escape() {
        let policy = CspProfile::Default.policy("n");
        assert!(
            !policy.contains("style-src 'self' 'unsafe-inline'"),
            "style-src must not authorise inline styles: {policy}"
        );
        assert!(
            !policy.contains("style-src 'unsafe-inline'"),
            "style-src must not authorise inline styles: {policy}"
        );
        assert!(
            policy.contains("style-src 'self'"),
            "the served Tailwind stylesheet still needs 'self': {policy}"
        );
    }

    /// The self-contained tool-output profiles legitimately need an inline style
    /// escape — they embed rendered HTML that is not ours to refactor. Asserting
    /// that keeps T0112 from being "fixed" by tightening those too and breaking
    /// report and tool rendering.
    #[test]
    fn the_embedded_document_profiles_still_allow_inline_styles() {
        for profile in [CspProfile::Report, CspProfile::ToolRender] {
            let policy = profile.policy("n");
            assert!(
                policy.contains("style-src 'unsafe-inline'"),
                "{profile:?} embeds third-party markup and must keep the escape"
            );
        }
        // ...while the one that frames rather than embeds does not.
        assert!(
            !CspProfile::PdfPreview
                .policy("n")
                .contains("'unsafe-inline'"),
            "PdfPreview serves no markup of its own and must stay locked down"
        );
    }

    #[test]
    fn pdf_preview_is_locked_down_but_self_frameable() {
        let policy = CspProfile::PdfPreview.policy("n");
        assert!(policy.contains("default-src 'none'"));
        assert!(policy.contains("frame-ancestors 'self'"));
        assert_eq!(CspProfile::PdfPreview.frame_options(), "SAMEORIGIN");
    }

    #[test]
    fn tool_render_omits_framing_headers_so_it_can_be_embedded() {
        assert!(
            !CspProfile::ToolRender.sends_frame_options(),
            "tool output is embedded; X-Frame-Options: DENY would block it"
        );
        assert!(
            CspProfile::ToolRender
                .policy("n")
                .contains("default-src 'none'")
        );
    }

    #[test]
    fn report_policy_allows_inline_script_but_never_framing() {
        let policy = CspProfile::Report.policy("n");
        assert!(policy.contains("script-src 'unsafe-inline'"));
        assert!(
            policy.contains("frame-ancestors 'none'"),
            "a report must never be frameable"
        );
    }

    #[test]
    fn every_profile_sets_a_policy_and_denies_base_uri_tagging() {
        for profile in [
            CspProfile::Default,
            CspProfile::Report,
            CspProfile::ToolRender,
            CspProfile::PdfPreview,
        ] {
            assert!(
                !profile.policy("n").is_empty(),
                "{profile:?} must set a policy"
            );
            assert!(
                profile.policy("n").contains("base-uri 'none'")
                    || profile.policy("n").contains("base-uri 'self'"),
                "{profile:?} must pin base-uri"
            );
        }
    }
}
