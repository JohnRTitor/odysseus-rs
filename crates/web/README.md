# odysseus-web

## Concern

The Axum transport layer: routing, middleware, auth extraction, SSE encoding. It
is the only crate that knows an HTTP status code exists — application crates
return typed errors and the mapping to a status happens here (AGENTS.md §5).

Implemented today:

- `app` — `AppState` (built once, passed as `Arc<AppState>`; no module-level
  singletons) and the router, with the middleware stack applied outside-in as
  security headers, CORS, compression, timeout, then slow-request logging.
- `middleware` — CORS from `ALLOWED_ORIGINS`, gzip compression that refuses to
  touch `text/event-stream`, per-request CSP nonces and security headers, a hard
  request deadline, and slow-request diagnostics. CORS sits outside the auth gate
  by design, since a genuine preflight carries no credentials.
- `error` — `AppError`, mapping typed errors to statuses in the
  `{"error": CODE, "message": str}` shape the frontend branches on.
- `routes::system` — the five unauthenticated endpoints: `/api/health`,
  `/api/version`, `/api/ready`, `/api/runtime`, and `/`, which returns an
  explicit 503 explaining the UI is unbuilt rather than a placeholder page.

The auth middleware named in the crate docs is not present yet; the gate is
still to be added.

## Must not import

- business rules

Source of truth for this list: the architecture table in `AGENTS.md` §8.