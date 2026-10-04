//! The authentication gate.
//!
//! Ports `AuthMiddleware` from `app.py:364-485`. **The step order is the security
//! property**, not an incidental detail, so each step below cites the Python line
//! it corresponds to, and `crates/web/tests/auth_gate.rs` walks the order rather
//! than testing the steps in isolation.
//!
//! | Step | `app.py` | Rule |
//! |---|---|---|
//! | 1 | 374 | CORS preflight passes **before** the gate |
//! | 2 | 376 | exempt path |
//! | 3 | 380-395 | internal-tool header + `compare_digest` + trusted loopback |
//! | 4 | 403 | `LOCALHOST_BYPASS` + trusted loopback |
//! | 5 | 404-411 | not configured → 302 `/login`, or 401 for `/api/` |
//! | 6 | 414-468 | `Authorization: Bearer ody_` |
//! | 7 | 470-485 | session cookie |
//!
//! Step 1 preceding step 2 is load-bearing: this layer is composed *inside* CORS,
//! so gating a preflight here 401s it before CORS can answer, which blocks every
//! cross-origin browser client before its real request is sent.
//!
//! Step 3's trusted-loopback check preceding any trust decision is what stops a
//! Cloudflare tunnel from inheriting local trust — see [`is_trusted_loopback`].

use {
    axum::{
        extract::{ConnectInfo, Request, State},
        http::{HeaderMap, Method, StatusCode, header},
        middleware::Next,
        response::{IntoResponse, Response},
    },
    std::net::IpAddr,
};

use crate::{AppState, identity::AuthContext};

/// The cookie carrying the browser session.
pub const SESSION_COOKIE: &str = "session";

/// The in-process token header, from `core/middleware.py:22`.
pub const INTERNAL_TOOL_HEADER: &str = "x-odysseus-internal-token";

/// The optional owner-impersonation header, from `app.py:386`.
pub const OWNER_HEADER: &str = "x-odysseus-owner";

/// Environment variable holding the in-process internal-tool token.
pub const INTERNAL_TOKEN_ENV: &str = "ODYSSEUS_INTERNAL_TOKEN";

/// Headers that prove a request arrived via a proxy or tunnel.
///
/// `cloudflared` connects to the app **from** `127.0.0.1`, so without this check
/// every tunneled request would look like loopback and could bypass auth
/// (`app.py:342-346`).
pub const PROXY_FWD_HEADERS: [&str; 7] = [
    "cf-connecting-ip",
    "cf-ray",
    "cf-visitor",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-real-ip",
    "forwarded",
];

/// Paths that skip the gate entirely, from `app.py:264-276`.
///
/// Eleven entries, not ten: eight `/api/auth/*` routes plus `/api/health`,
/// `/api/version` and `/login`.
pub const AUTH_EXEMPT_EXACT: [&str; 11] = [
    "/api/auth/setup",
    "/api/auth/signup",
    "/api/auth/login",
    "/api/auth/logout",
    "/api/auth/status",
    "/api/auth/features",
    "/api/auth/settings",
    "/api/auth/integrations/presets",
    "/api/health",
    "/api/version",
    "/login",
];

/// Prefixes whose routes and children skip the gate, from `app.py:277`.
pub const AUTH_EXEMPT_PREFIXES: [&str; 1] = ["/static"];

/// Whether `path` is exactly `prefix` or below that route.
///
/// Ported rather than inlined because a bare `starts_with` would exempt
/// `/staticfoo` — a real route could be added there and silently become
/// unauthenticated.
#[must_use]
pub fn path_is_route_or_child(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Whether `path` matches `^/api/tasks/[^/]+/webhook/[^/]+/?$`, from `app.py:288`.
///
/// Hand-rolled rather than a `regex` dependency: the pattern is three literal
/// separators around two non-empty, slash-free segments, so a split check is
/// auditable at a glance and cannot be made catastrophic by a crafted path.
/// `[^/]+` means "one or more characters that are not a slash", which is exactly
/// "a non-empty segment" — note that it also admits `?` and `#`, which is why
/// the caller must pass [`axum::http::Uri::path`], never the full URI.
///
/// These routes prove identity with a path-embedded `webhook_token` the handler
/// checks itself, 404ing on mismatch — so the path *is* the credential, and
/// exempting it here is what lets the handler do that check at all.
#[must_use]
pub fn is_task_webhook_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/tasks/") else {
        return false;
    };
    // `/?$` — the trailing slash is optional, and `/` alone is never valid.
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let mut segments = rest.split('/');
    match (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) {
        (Some(task_id), Some("webhook"), Some(token), None) => {
            !task_id.is_empty() && !token.is_empty()
        }
        _ => false,
    }
}

/// Whether `path` skips the gate, from `app.py:291-296`.
#[must_use]
pub fn is_auth_exempt(path: &str) -> bool {
    AUTH_EXEMPT_EXACT.contains(&path)
        || AUTH_EXEMPT_PREFIXES
            .iter()
            .any(|p| path_is_route_or_child(path, p))
        || is_task_webhook_path(path)
}

/// True for a genuine CORS preflight: `OPTIONS` plus `Access-Control-Request-Method`.
///
/// Such a request is credential-less by design and must reach CORS. A bare
/// `OPTIONS` is **not** a preflight and is still authenticated, so `OPTIONS`
/// cannot become an unauthenticated probe.
#[must_use]
pub fn is_cors_preflight(method: &Method, headers: &HeaderMap) -> bool {
    method == Method::OPTIONS && headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
}

/// True **only** for a direct loopback connection carrying no proxy headers.
///
/// A bare `is_loopback()` is unsafe behind a tunnel or reverse proxy: those
/// connect *from* loopback, so a remote visitor would inherit local trust and
/// slip past `LOCALHOST_BYPASS` or spoof the internal-tool path. Odysseus's own
/// in-process loopback calls carry none of these headers, so they still qualify
/// (`app.py:348-362`).
#[must_use]
pub fn is_trusted_loopback(host: Option<IpAddr>, headers: &HeaderMap) -> bool {
    host.is_some_and(|ip| ip.is_loopback() || ip.to_canonical().is_loopback())
        && !PROXY_FWD_HEADERS
            .iter()
            .any(|name| headers.contains_key(*name))
}

/// The peer's address, from the connection info `main` installs.
///
/// **Never** read the peer from a header. Starlette's `request.client` is
/// transport-level, and a header would be attacker-controlled — accepting one
/// here would hand every remote caller loopback trust, which is precisely the
/// failure [`is_trusted_loopback`] exists to prevent. Absent connection info is
/// therefore untrusted, not trusted.
fn peer_ip(request: &Request) -> Option<IpAddr> {
    request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
}

/// Whether the request's peer is loopback, **ignoring** proxy headers.
///
/// Used only where the Python source itself uses a bare loopback check
/// (`auth_helpers.require_user`). Never use it to grant a bypass.
fn peer_is_loopback(request: &Request) -> bool {
    peer_ip(request).is_some_and(|ip| ip.is_loopback())
}

/// The 401 for an `/api/` path.
///
/// The body is `{"error": …}`, not a bare JSON string: the frontend branches on
/// the `error` key (`app.py:409`, `:466`, `:481`).
fn api_denied(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// The 302 to `/login` for a browser path.
///
/// Explicitly **302**, not `Redirect::temporary`'s 307. `app.py:406` and `:483`
/// both use 302, and the difference is observable: a 307 preserves the request
/// method, so a POST to a gated page would be re-POSTed at `/login`.
fn login_redirect() -> Response {
    (StatusCode::FOUND, [(header::LOCATION, "/login")]).into_response()
}

fn unauthenticated(path: &str) -> Response {
    if path.starts_with("/api/") {
        api_denied("Not authenticated")
    } else {
        login_redirect()
    }
}

/// The gate. Composed **inside** CORS by the caller.
///
/// Never returns `Err`: every rejection is a response. An `AppState` whose stores
/// are unreadable fails *closed* — 401, never a pass.
pub async fn require_authentication(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let host = peer_ip(&request);

    // --- Step 1: CORS preflight reaches CORS, not this gate (app.py:374). ---
    if is_cors_preflight(request.method(), &headers) {
        return next.run(request).await;
    }

    // --- Step 2: exempt paths (app.py:376). ---
    if is_auth_exempt(&path) {
        return next.run(request).await;
    }

    // --- Step 3: internal-tool header (app.py:380-395). ---
    //
    // Constant-time match AND trusted loopback. Both, never either: a leaked
    // token from a remote host must not authenticate, and a loopback caller
    // without the token must not.
    if let Some(presented) = headers.get(INTERNAL_TOOL_HEADER)
        && constant_time_eq(presented.as_bytes(), state.internal_tool_token.as_bytes())
        && is_trusted_loopback(host, &headers)
    {
        // Impersonation applies only to an account that exists; this is owner
        // attribution for notes and calendar, never authority (app.py:386-390).
        let impersonate = headers
            .get(OWNER_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let resolved = match impersonate {
            Some(name) => state.account_exists(&name).await.then_some(name),
            None => None,
        };
        request
            .extensions_mut()
            .insert(AuthContext::internal_tool(resolved));
        return next.run(request).await;
    }

    // --- Step 4: LOCALHOST_BYPASS, direct loopback only (app.py:403). ---
    if state.localhost_bypass && is_trusted_loopback(host, &headers) {
        request.extensions_mut().insert(AuthContext::default());
        return next.run(request).await;
    }

    let configured = state.is_configured().await;

    // --- Step 5: no users yet (app.py:404-411). ---
    if !configured {
        return if path.starts_with("/api/") {
            api_denied("Setup required")
        } else {
            login_redirect()
        };
    }

    // --- Step 6: bearer API token (app.py:414-468). ---
    if let Some(raw) = bearer_token(&headers) {
        return match state.verify_api_token(raw).await {
            Ok(Some(verified)) => {
                request.extensions_mut().insert(AuthContext::bearer(
                    verified.id,
                    verified.owner,
                    verified.scopes,
                ));
                next.run(request).await
            }
            // Python's `try/except` logs and falls through to the 401; the
            // distinction between "no match" and "lookup failed" is not
            // observable to the client, and both deny.
            Ok(None) | Err(_) => api_denied("Invalid API token"),
        };
    }

    // --- Step 7: browser session (app.py:470-485). ---
    let Some(token) = session_cookie(&headers) else {
        return unauthenticated(&path);
    };
    match state.validate_session(&token).await {
        Ok(Some(user)) => {
            request
                .extensions_mut()
                .insert(AuthContext::cookie_session(user));
            next.run(request).await
        }
        Ok(None) | Err(_) => unauthenticated(&path),
    }
}

/// Extracts the raw `ody_…` token from an `Authorization: Bearer` header.
///
/// Returns `None` when the header is absent or is not an `ody_` bearer, so a
/// cookie session is not disturbed by an unrelated `Authorization` header.
///
/// The `ody_` prefix is **kept**. `app.py:416` takes `auth_header[7:]`, which
/// strips only `"Bearer "`, and the store's prefix cache is keyed on the first
/// eight characters of the result — i.e. `ody_` plus four token characters.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    raw.strip_prefix("Bearer ody_").map(|_| &raw[7..])
}

/// Extracts the session cookie value.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.trim().to_owned())
}

/// Constant-time equality, standing in for Python's `secrets.compare_digest`.
///
/// A length mismatch returns `false` without inspecting the contents; the token is
/// a fixed 64 hex characters, so its length is not the secret.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Whether the peer is loopback, for route-level gates that mirror
/// `auth_helpers.require_user`.
///
/// Exposed so handlers do not re-derive it from a header.
#[must_use]
pub fn request_is_loopback(request: &Request) -> bool {
    peer_is_loopback(request)
}

#[cfg(test)]
mod tests {
    use {super::*, axum::http::HeaderValue, std::net::Ipv4Addr};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn every_exempt_path_is_exempt() {
        for path in AUTH_EXEMPT_EXACT {
            assert!(is_auth_exempt(path), "{path} must be exempt");
        }
    }

    #[test]
    fn a_non_exempt_path_is_not_exempt() {
        for path in [
            "/api/chat",
            "/api/auth/users",
            "/api/ready",
            "/login/",
            "/loginfo",
            "/",
        ] {
            assert!(!is_auth_exempt(path), "{path} must NOT be exempt");
        }
    }

    #[test]
    fn the_static_prefix_matches_children_but_not_a_sibling() {
        assert!(is_auth_exempt("/static"));
        assert!(is_auth_exempt("/static/app.js"));
        assert!(is_auth_exempt("/static/nested/deep.css"));
        // The regression a bare starts_with would introduce.
        assert!(!is_auth_exempt("/staticfoo"));
        assert!(!is_auth_exempt("/static-export"));
    }

    #[test]
    fn the_webhook_pattern_matches_only_its_own_shape() {
        for path in [
            "/api/tasks/abc/webhook/tok",
            "/api/tasks/abc/webhook/tok/",
            "/api/tasks/t-1/webhook/secret-token",
        ] {
            assert!(is_task_webhook_path(path), "{path} must match");
        }
        for path in [
            "/api/tasks/abc/webhook",
            "/api/tasks//webhook/tok",
            "/api/tasks/abc/webhook/",
            "/api/tasks/abc/webhook/tok/extra",
            "/api/tasks/abc/other/tok",
            "/api/tasks/abc",
            "/api/tasksX/abc/webhook/tok",
        ] {
            assert!(!is_task_webhook_path(path), "{path} must NOT match");
        }
    }

    #[test]
    fn the_webhook_pattern_is_given_a_query_free_path() {
        // Python's `[^/]+` admits `?`, so the regex would match this string too.
        // The gate passes `uri().path()`, which never has a query, so the
        // behaviour is unobservable in production — but the contract is worth
        // pinning, because passing `uri().to_string()` would exempt
        // `/api/tasks/a/webhook/b?x` and route it to a handler expecting a path.
        assert!(is_task_webhook_path("/api/tasks/abc/webhook/tok?x=1"));
        let uri: axum::http::Uri = "/api/tasks/abc/webhook/tok?x=1".parse().unwrap();
        assert_eq!(uri.path(), "/api/tasks/abc/webhook/tok");
        assert!(is_task_webhook_path(uri.path()));
    }

    #[test]
    fn loopback_trust_requires_a_loopback_peer_and_no_proxy_headers() {
        let clean = headers(&[]);
        assert!(is_trusted_loopback(
            Some("127.0.0.1".parse().unwrap()),
            &clean
        ));
        assert!(is_trusted_loopback(Some("::1".parse().unwrap()), &clean));
        assert!(is_trusted_loopback(
            Some("127.0.0.1".parse().unwrap()),
            &clean
        ));
        assert!(is_trusted_loopback(
            Some("::ffff:127.0.0.1".parse().unwrap()),
            &clean
        ));

        assert!(!is_trusted_loopback(
            Some("10.0.0.5".parse().unwrap()),
            &clean
        ));
        assert!(!is_trusted_loopback(
            Some("192.168.1.9".parse().unwrap()),
            &clean
        ));
        assert!(!is_trusted_loopback(None, &clean));
        // A v4-mapped v6 loopback address is loopback too.
        assert!(is_trusted_loopback(
            Some("::ffff:127.0.0.1".parse().unwrap()),
            &clean
        ));
    }

    #[test]
    fn every_proxy_header_defeats_loopback_trust() {
        for name in PROXY_FWD_HEADERS {
            let map = headers(&[(name, "anything")]);
            assert!(
                !is_trusted_loopback(Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), &map),
                "{name} must defeat loopback trust"
            );
        }
    }

    #[test]
    fn a_present_but_empty_proxy_header_still_defeats_trust() {
        // Python's `request.headers.get(_h)` returns "" (falsy) for an empty
        // value, so Python would *not* reject. HeaderMap's `contains_key` does.
        // The stricter reading is the safe one and the divergence is recorded
        // here rather than silently resolved.
        let map = headers(&[("x-forwarded-for", "")]);
        assert!(!is_trusted_loopback(
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            &map
        ));
    }

    #[test]
    fn a_preflight_needs_both_the_method_and_the_header() {
        let opts = Method::OPTIONS;
        let acrm = headers(&[("access-control-request-method", "POST")]);
        assert!(is_cors_preflight(&opts, &acrm));
        assert!(!is_cors_preflight(&Method::GET, &acrm));
        assert!(!is_cors_preflight(&opts, &headers(&[])));
        // A credentialed OPTIONS is not a preflight.
        assert!(!is_cors_preflight(
            &opts,
            &headers(&[("origin", "http://x")])
        ));
    }

    #[test]
    fn only_an_ody_bearer_header_takes_the_token_path() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer ody_abcdef")])),
            Some("ody_abcdef"),
            "the ody_ prefix must survive: the prefix cache keys on the first 8 chars"
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer sk-other")])),
            None
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Basic abc")])),
            None
        );
        assert_eq!(bearer_token(&headers(&[])), None);
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        assert_eq!(
            session_cookie(&headers(&[(
                "cookie",
                "theme=dark; session=abc123; other=1"
            )]))
            .as_deref(),
            Some("abc123")
        );
        assert_eq!(session_cookie(&headers(&[("cookie", "theme=dark")])), None);
        assert_eq!(session_cookie(&headers(&[])), None);
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
