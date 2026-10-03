# Odysseus — API Migration

493 endpoints. This document records the transport strategy, the auth model, the SSE
decision, and the full domain-by-domain endpoint mapping.

---

## 1. Transport strategy

### 1.1 One Axum server serves everything

| Concern | Mechanism | Crate |
|---|---|---|
| Leptos SSR | `leptos_axum::generate_route_list` + `render_app_to_stream` | `odysseus-ui` |
| Hydration assets | Leptos asset pipeline, content-hashed | `odysseus-ui` |
| Application APIs | Axum routes under `/api/*` | `odysseus-web` |
| SSE | `axum::response::sse::Sse` + `tokio_stream` | `odysseus-web` |
| Auth/sessions | Axum middleware + extractors | `odysseus-web` |
| Static assets | `tower-http::ServeDir` | `odysseus-web` |

**No WebSockets.** The source has zero WebSocket endpoints; the only mention
(`chat_routes.py:631`) is a stale comment. All long-lived streams are SSE. Adding
WebSockets would be a redesign, not a migration.

### 1.2 Server functions vs. explicit API routes

The plan requires deciding per endpoint whether a Leptos **server function** or an
explicit **Axum route** is correct. Criteria:

| Use explicit Axum route | Use server function |
|---|---|
| Consumed by non-browser clients (companion app, n8n/Make, Codex plugin, Zapier) | Consumed only by this UI |
| Streamed via SSE | Returns a value |
| Part of a stable public contract | Internal form data |
| Needs a documented scope | Same-origin UI fetch |
| File upload/download | |

**Consequence:** the agent-facing, webhook, Codex/Claude, companion, and public
`/api/v1/chat` surfaces are explicit Axum routes — **not** server functions. Replacing
them with server functions would remove the documented contract that external
integrations depend on, which the brief explicitly forbids ("Do not replace important
APIs with server functions merely because they are convenient").

Server functions are used for: settings read/write, preference/preset mutations,
workspace browse/vet, and UI-only helper calls. Roughly 25 endpoints.

### 1.3 SSE endpoint decision table

13 SSE sites in the source. Each is classified:

| Current endpoint | Media type | Target | Rationale |
|---|---|---|---|
| `POST /api/chat_stream` | `text/event-stream` | **SSE** | Unidirectional server→client. One long-lived run per request. SSE is correct. |
| `GET /api/chat/resume/{sid}` | `text/event-stream` | **SSE** | Replay + live tail of a detached run. Unidirectional. |
| `POST /api/rewrite` | `text/event-stream` | **SSE** | Single-stream LLM rewrite. |
| `GET /api/shell/stream` | `text/event-stream` | **SSE** | Line-by-line process output. Unidirectional. |
| `GET /api/probe` | `text/event-stream` | **SSE** | Iterates models, 8 s each. Progress reporting. |
| `GET /api/model-endpoints/{id}/probe` | `text/event-stream` | **SSE** | Same. |
| `GET /api/research/stream/{sid}` | `text/event-stream` | **SSE** | 1.5 s poll → progress. Could be SSE push instead. |
| `POST /api/model/download` | (long poll, log tail) | **SSE** | Cookbook download progress. |
| `POST /api/cookbook/*` streaming | `text/event-stream` | **SSE** | |

**Verdict: keep SSE everywhere.** No WebSocket. Rationale:
- All streams are server→client only. SSE has automatic reconnection and works over
  plain HTTP without a protocol upgrade.
- SSE survives reverse proxies and LAN setups that commonly break WS upgrades.
- `tokio_stream::wrappers::ReceiverStream` + `Sse::keep_alive()` gives heartbeat and
  backpressure semantics that match the current behaviour.
- The gzip middleware already excludes `text/event-stream`, which prevents buffering —
  this must be preserved with tower-http's `CompressionLayer` predicate.

**Research progress:** the current implementation *polls* every 1.5 s
(`research_routes.py:579`). In Rust, replace the poll with a `tokio::sync::broadcast`
channel so the SSE stream pushes. Strictly better and removes 1.5 s latency. Same for
the task-notification poll (`tasks.js`, 30 s) and the research-job poll
(`research/jobs.js`, 20 s).

### 1.4 SSE event vocabulary is the contract

`odysseus-domain/src/events.rs` defines a typed enum mirroring the wire format 1:1.
**This is the single most important compatibility surface in the migration.** The
frontend's behaviour depends on exact event names and payload shapes.

Full vocabulary (from `agent_loop.py:3454-3461`, `llm_core.py:2582`, and the frontend
handlers at `chat.js:3214-3902`):

```
delta {text, thinking?}
agent_prep | agent_step | agent_terminal
tool_start | tool_progress | tool_output | tool_use | tool_result
tool_call_delta {index, name, arg_delta}
tool_calls {calls:[{id, name, arguments}]}
usage {input_tokens, output_tokens, model, gen_tps, prefill_tps}
model_actual {requested_model, model} | model_info
fallback {selected_model, answered_by, endpoint ids/labels, candidate_index, reason, failures}
ask_user {…} | plan_update {…} | ui_control {…}
doc_stream_open | doc_stream_delta | doc_update | doc_suggestions
web_sources | rag_sources | memories_used | findings
compacted | context_trimmed | metrics | usage
loop_breaker_triggered | intent_nudge_exhausted | rounds_exhausted | budget_exceeded
generated_image | message_saved | attachment_ref
research_progress | research_sources | research_findings | research_done
workspace_rejected | tool_approval_resolved
event: error {status, text, fallback_eligible}
data: [DONE]
```

**Migration rule:** add one enum variant per event; a round-trip test asserts the
serialised form matches the Python output byte-for-byte for a fixed input.

---

## 2. Auth model

### 2.1 Gate order (invariant)

`app.py:364-485` order is security-critical. Port it exactly:

```
1. CORS preflight (OPTIONS + Access-Control-Request-Method)  → pass
2. Exempt path (exact set | prefix set | regex)               → pass
3. Internal-tool header + compare_digest + trusted loopback   → pass (optional impersonation)
4. LOCALHOST_BYPASS && trusted loopback                       → pass
5. Not configured                                             → 302 /login or 401 {"error":"Setup required"}
6. Authorization: Bearer ody_                                 → API token path
7. SESSION_COOKIE                                             → validate_token
```

**Step 1 must precede step 2's siblings.** `AuthMiddleware` is the outermost
middleware, so gating preflight on auth 401s it before CORS can respond, which blocks
every cross-origin browser client.

**Step 3's trusted-loopback check must precede any trust decision.** `_is_trusted_loopback`
(`app.py:348-362`) rejects when the client host is not loopback **or** when any of
`cf-connecting-ip, cf-ray, cf-visitor, x-forwarded-for, x-forwarded-host, x-real-ip,
forwarded` is present. A bare `client.ip().is_loopback()` is unsafe behind a
Cloudflare tunnel or reverse proxy, because those connect *from* loopback and a remote
visitor would inherit local trust.

### 2.2 Identity helpers

| Helper | Target | Rule |
|---|---|---|
| `get_current_user` | `AuthContext.raw_user` | For bearer tokens this is the pseudo-user `"api"`. |
| `effective_user` | `AuthContext.effective_user` | Real human. Bearer → `api_token_owner`. **Use for data ownership.** |
| `is_delegated_credential` | `AuthContext.is_delegated` | **Use for authority checks** so a token never inherits its owner's admin/shell powers. |
| `require_user` | Axum extractor | Fail-closed. Returns `""` in auth-disabled/loopback/bypass modes. |
| `require_owner` | Axum extractor | Stricter variant used by email. |
| `require_privilege` | Axum extractor | Unknown keys fail **open** (preserve this behaviour deliberately). |
| `require_admin` | Axum extractor | Admin, or internal-tool header, or auth disabled. |
| `require_api_token_scope` | Axum extractor | Bearer callers only. |

> **Critical porting note:** bearer middleware forces `current_user = "api"`. Every
> owner-scoped route that reads the *raw* user rather than `effective_user` will see a
> phantom owner. There is a dedicated regression test for this
> (`test_email_owner_scope.py`, `test_null_owner_gates.py`) — port it first.

### 2.3 API token scopes

```
chat
todos:read      todos:write
documents:read  documents:write
email:read      email:draft    email:send
calendar:read   calendar:write
memory:read     memory:write
cookbook:read   cookbook:launch
```

Profiles: `chat`, `codex_todos`, `codex_documents`, `codex_email_drafts`.
Write scopes auto-insert their read scope. Token format `ody_` + 43 chars;
`token_prefix` = first 8 chars; id = `uuid4()[:8]`.

**Implementation:** an in-memory `RwLock<HashMap<prefix, Vec<Candidate>>>` rebuilt only
when a dirty flag is set, plus constant-time comparison and bcrypt verification —
mirroring the current cache but with a lock the async runtime never holds across an
await.

---

## 3. Middleware stack (Axum)

Applied outermost-first:

| Layer | Target | Notes |
|---|---|---|
| `SecurityHeadersLayer` | `middleware/security.rs` | Per-request nonce via `secrets.token_hex(16)` equivalent. **Three CSP branches.** |
| `CorsLayer` | `middleware/cors.rs` | `ALLOWED_ORIGINS` env, comma-split. |
| `TimeoutLayer` | `middleware/timeout.rs` | 45 s → 504. **10 exempt prefixes.** |
| `SlowRequestLogLayer` | `middleware/slow_log.rs` | `ODYSSEUS_SLOW_REQUEST_LOG_SECONDS`, default 0.75. |
| `AuthLayer` | `middleware/auth.rs` | Order per §2.1. |
| `CompressionLayer` | `router.rs` | Must **not** compress `text/event-stream`. |

`_InteractiveActivityMiddleware` does **not** become middleware — it is application
logic and moves into `interactive_gate` as an explicit hook.

---

## 4. Domain-by-domain endpoint mapping

Status legend: **U** = UNIMPLEMENTED (all rows, since nothing is built).
`Phase` is the target phase from `REWRITE_PLAN.md` §5.

### 4.1 Chat & streaming (9) — Phase 4

| Method | Current path | Target | Notes |
|---|---|---|---|
| POST | `/api/chat` | `/api/chat` | Non-streaming. |
| POST | `/api/chat_stream` | `/api/chat/stream` | SSE. **20+ FormData fields** — see §4.1.1. |
| GET | `/api/chat/resume/{sid}` | `/api/chat/{sid}/resume` | SSE. |
| POST | `/api/chat/stop/{sid}` | `/api/chat/{sid}/stop` | |
| GET | `/api/chat/stream_status/{sid}` | `/api/chat/{sid}/status` | |
| POST | `/api/inject_context/{sid}` | `/api/sessions/{sid}/context` | `Form(context)`. |
| GET | `/api/search` | `/api/search/messages` | **REPLACED** — resolves the duplicate-path collision. |
| POST | `/api/rewrite` | `/api/chat/rewrite` | SSE out. |

#### 4.1.1 `/api/chat_stream` FormData contract

This is the wire protocol. 20+ fields, `chat.js:1830-1935`:

```
message, session, tool_approval_id, tool_approval_decision,
selected_model, selected_endpoint_url, selected_endpoint_id,
attachments (JSON array), active_doc_id, active_email_uid,
active_email_folder, active_email_account,
mode (chat|agent), plan_mode, approved_plan,
use_web, allow_web_search, use_research, allow_bash,
use_rag, incognito, workspace, preset_id
```

Headers: `X-Tz-Offset`, `X-Tz-Name` (request); `X-Odysseus-Run-Id` (response).

**Target:** accept **JSON only**, not multipart. The only multipart field is
`attachments`, which is already a JSON array of ids. This removes multipart parsing
entirely. A transitional compatibility shim accepting `application/x-www-form-urlencoded`
should be kept for one release.

### 4.2 Sessions (21) — Phase 1

`GET /api/sessions` · `POST /api/session` · `PATCH /api/session/{sid}` ·
`POST /api/session/{sid}/inject_messages` · `POST /api/session/{sid}/delete` (beacon) ·
`POST /api/sessions/bulk-delete` · `DELETE /api/session/{sid}` · `DELETE /api/sessions/all` **(admin)** ·
`POST /api/session/{sid}/archive` · `POST /api/session/{sid}/unarchive` ·
`GET /api/sessions/archived` · `GET /api/session/{sid}/export` (download md/txt/json/html) ·
`POST /api/sessions/save` · `POST /api/session/openai` ·
`POST /api/session/{sid}/important` · `POST /api/session/{sid}/compact` ·
`POST /api/sessions/auto-sort` · `GET /api/session/{sid}/context_info`

All under `/api/session/**` or `/api/sessions/**` as today. `POST /api/sessions/save`
becomes a **no-op returning 200** — `save_sessions()` is documented as an explicit no-op
(`session_manager.py:709`) — but is retained for client compatibility and documented as
vestigial.

`GET /api/session/{sid}/export` produces a `Content-Disposition: attachment` response
with a sanitised filename (`test_session_export_filename.py`).

### 4.3 History (12) — Phase 4

`GET /api/history/{sid}` · `POST /api/session/{sid}/truncate` ·
`POST /api/session/{sid}/message` · `POST /api/session/{sid}/delete-messages` ·
`POST /api/session/{sid}/edit-message` · `POST /api/session/{sid}/mark-stopped` ·
`POST /api/session/{sid}/update-last-meta` · `POST /api/session/{sid}/merge-last-assistant` ·
`POST /api/session/{sid}/fork` · `GET /api/conversations/topics` ·
`GET /api/session/{sid}/context` · `POST /api/session/{sid}/compact`

### 4.4 Auth, users, 2FA, settings, integrations (30) — Phases 0, 20, 21

**Auth-exempt** (must stay exempt): `/api/auth/setup`, `/signup`, `/login`, `/logout`,
`/status`, `/features`, `/settings`, `/integrations/presets`, `/api/health`,
`/api/version`, `/login`.

```
POST   /api/auth/setup · signup · login · logout
GET    /api/auth/status · policy · 2fa/status · users · features · settings · integrations · integrations/presets
POST   /api/auth/change-password · 2fa/setup · 2fa/confirm · 2fa/disable · signup-toggle(deprecated)
PUT    /api/auth/open-signup · users/{u}/privileges · users/{u}/rename · users/{u}/admin
POST   /api/auth/users
DELETE /api/auth/users
POST   /api/auth/features · settings · integrations · integrations/{id}/test
PUT    /api/auth/integrations/{id}
DELETE /api/auth/integrations/{id}
```

`/api/auth/settings` returns `scrub_settings()` for non-admins — a security boundary
(`test_settings_scrub.py`). `POST /api/auth/settings` clamps `agent_max_rounds` to
1–200 and `agent_max_tool_calls` to 0–1000.

`POST /api/auth/signup-toggle` is deprecated (`auth_routes.py:635`) → **REMOVED**.

### 4.5 Email (57) — Phase 15

Full path list preserved. Notable:

| Group | Endpoints | Crates |
|---|---|---|
| Listing/state | `list`, `unread-state`, `search`, `contacts`, `folders`, `read/{uid}`, `urgency-state` | `odysseus-services/src/email/imap.rs` + `odysseus-store` |
| Mutations | `mark-read`, `mark-unread`, `flag`, `archive`, `move`, `delete`, `delete-permanent`, `mark-answered`, `clear-answered`, `unflag-spam`, `odysseus/reminders` | same |
| Attachments | `attachments/{uid}`, `attachment/{uid}/{i}`, `attachments-download/{uid}` (ZIP), `inline-image/{uid}`, `attachment-as-doc/{uid}/{i}`, `attachment-path/{uid}/{i}` | `odysseus-services` + `odysseus-web` |
| Compose | `compose-upload` (multipart), `compose-from-odysseus`, `compose-from-odysseus-zip`, `compose-from-attachment/{uid}/{i}`, `send`, `draft`, `schedule`, `scheduled`, `pending`, `pending/{sid}/approve`, `pending/{sid}`, `resolve-contact` | `lettre` + `odysseus-services` |
| AI | `summarize`, `translate`, `ai-reply`, `extract-style`, `style` (GET/PUT) | `odysseus-services` |
| Accounts | `config` (GET/PUT), `accounts` CRUD, `accounts/test`, `accounts/{id}/set-default`, `oauth/google/authorize`, `oauth/google/callback` | `odysseus-integrations/src/google_oauth.rs` |
| Unsubscribe | `unsubscribe/scan`, `unsubscribe/execute`, `unsubscribe/cleanup` | `odysseus-services` |

**Multipart → JSON:** `compose-upload` is the only multipart endpoint left in the whole
API after chat. Convert to `POST /api/email/compose` with a JSON body carrying the staged
upload id from `POST /api/uploads`.

**Auth note:** nearly all email endpoints use `require_owner`, which is stricter than
`require_user`. Two sync-declared endpoints (`GET /api/email/search`,
`POST /api/email/archive/{uid}`) run blocking IMAP in a threadpool. In Rust these are
naturally async — but IMAP calls must go through `spawn_blocking` (D11).

**`POST /api/email/send` returns immediately** and dispatches via `BackgroundTasks`.
Target: `tokio::spawn` with the SMTP `lettre` future, plus a result channel the
background poller checks.

**`GET /api/email/oauth/google/callback` is exempt from session auth** (validated by
signed `state`). Preserve exactly.

### 4.6 Documents & PDF (24) — Phase 8

```
POST   /api/document                              PUT/PATCH/DELETE /api/document/{doc_id}
GET    /api/document/{doc_id}                     POST /api/document/{doc_id}/archive
GET    /api/document/{doc_id}/versions            GET  /api/document/{doc_id}/version/{n}
POST   /api/document/{doc_id}/restore/{n}
GET    /api/documents/library                     GET  /api/documents/{session_id}
POST   /api/documents/import-pdf                  POST /api/documents/tidy
POST   /api/documents/ai-tidy                     POST /api/documents/export-zip
POST   /api/document/{id}/extract-pdf-text
POST   /api/document/{id}/export-pdf/preview      GET  /api/document/{id}/render-pages
GET    /api/document/{id}/page/{n}.png            POST /api/document/{id}/ai-fill-annotations
GET    /api/document/{id}/render-pdf              GET  /api/document/{id}/export-pdf
POST   /api/document/{id}/prepare-signed-reply
```

`GET /render-pdf` gets `frame-ancestors 'self'; default-src 'none'` and
`X-Frame-Options: SAMEORIGIN` — a distinct CSP branch. `export-pdf` uses
`attachment`. Temp files unlinked after response via a `Drop` guard or a
`tokio::fs::remove_file` in a spawned task.

### 4.7 Gallery & image (32) — Phase 17

```
POST   /api/gallery/upload · gallery/{id}/replace · gallery/{id}/rename · gallery/{id}/rotate
GET    /api/gallery/tags · gallery/library · gallery/albums · gallery/stats · gallery/{id}
PATCH  /api/gallery/{id}                         DELETE /api/gallery/{id}
POST   /api/gallery/albums · albums/{id} (PUT) · albums/{id}/add · albums/{id}/remove
DELETE /api/gallery/albums/{id}                  POST /api/gallery/{id}/favorite
POST   /api/gallery/download-zip                 POST /api/gallery/ai-upscale
POST   /api/gallery/style-transfer · ai-tag-batch · gallery/{id}/ai-tag
POST   /api/gallery/clear-user-tags · clear-ai-tags · dedupe-tags
```

Image proxies — all `/api/image/*`, all privilege-gated by `can_generate_images`:

| Path | Verdict |
|---|---|
| `/api/image/inpaint` | IMPLEMENTED target — Rust HTTP proxy, SSRF guard + endpoint-membership check |
| `/api/image/harmonize` | IMPLEMENTED target — same |
| `/api/image/sharpen` | IMPLEMENTED target — `image` crate `UnsharpMask` |
| `/api/image/denoise` | **REMOVED** |
| `/api/image/upscale-local` | **REMOVED** |
| `/api/image/mask` | UNIMPLEMENTED — `ort` + SAM (spike S3) |
| `/api/image/remove-bg` | UNIMPLEMENTED — `rembg-rs` HOLD (spike S5) |
| `/api/image/enhance-face` | **REMOVED** |

### 4.8 Calendar & CalDAV (16) — Phase 16

`GET/POST /api/calendar/config` · `GET/POST /api/calendar/config/accounts` ·
`PUT/DELETE /api/calendar/config/accounts/{id}` · `POST /api/calendar/test` ·
`POST /api/calendar/sync?direction=pull|push` · `GET/POST /api/calendar/calendars` ·
`PUT/DELETE /api/calendar/calendars/{id}` · `GET/POST /api/calendar/events` ·
`PUT/DELETE /api/calendar/events/{uid}` · `POST /api/calendar/import` (multipart `.ics`) ·
`GET /api/calendar/export/{cal_id}` (download) · `POST /api/calendar/quick-parse`

### 4.9 Cookbook / model serving (16) + shell (6) — Phases 13, 18

Nearly every cookbook route is `require_admin`. All interpolated `ssh`/`docker`/`tmux`
values pass through `validate_remote_host` / `validate_ssh_port`.

```
GET/POST /api/cookbook/ssh-key · test-ssh      POST /api/model/download  (SSE)
GET      /api/model/cached                     POST /api/model/serve
POST     /api/cookbook/setup                   GET  /api/cookbook/gpus
POST     /api/cookbook/kill-pid                GET/POST /api/cookbook/state
GET      /api/cookbook/hf-latest · hf-gguf-files · ollama/library
GET      /api/cookbook/vllm-recipe-manifest · vllm-recipe · tasks/status
GET      /api/cookbook/packages                POST /api/cookbook/packages/install
POST     /api/cookbook/install-system-deps · rebuild-engine
POST     /api/shell/exec                       POST /api/shell/stream (SSE)
GET      /api/hwfit/system · models · profiles · image-models
```

**`/api/hwfit/*` security gap:** all four endpoints shell out over SSH with **no admin
or scope gate** (`hwfit_routes.py:185-412`) — only `AuthMiddleware` applies. Ported as
**admin-gated**. Logged as a deliberate fix, not a silent behaviour change.

### 4.10 Models & providers (21) — Phase 2

All admin-gated. Probe SSE event vocabulary:
`{type: probe_start, endpoint, model_count, skipped, error}`,
`{type: probe_result, model, endpoint, status, …}`, `{type: probe_done, total, ok, hidden}`.

`GET/POST/PATCH/DELETE /api/model-endpoints[/{id}]` ·
`GET /api/model-endpoints/{id}/probe` (SSE) · `GET /api/model-endpoints/{id}/models` ·
`PATCH /api/model-endpoints/{id}/models` · `GET /api/model-endpoints/{id}/dependents` ·
`GET /api/models` · `/api/ping` · `/api/model-endpoints/probe-local` ·
`POST /api/probe-selected` · `GET /api/probe` (SSE) · `GET /api/providers` ·
`GET /api/discover` · `GET /api/default-chat` · `GET/POST /api/tools`

Device flow, 3 providers × 3 routes: `/api/copilot/device/{start,poll,cancel}`,
`/api/chatgpt-subscription/device/{start,poll,cancel}`.

### 4.11 Memory (13) — Phase 7

`GET /api/memory` · `POST /api/memory/{debug,add,search,extract,audit,import}` ·
`GET /api/memory/{timeline,by-session/{sid},{id}}` · `PUT/DELETE /api/memory/{id}` ·
`POST /api/memory/{id}/pin`

`/add`, `/import` require `can_manage_memory`. All move to JSON (no `Form(...)`).

### 4.12 Notes (11) — Phase 12

`GET/POST /api/notes` · `GET/PUT/DELETE /api/notes/{id}` ·
`POST /api/notes/{id}/pin` · `/archive` · `/items/{index}/toggle` ·
`POST /api/notes/fire-reminder` · `POST /api/notes/reorder`

`note_id` beginning `test-` on `fire-reminder` is admin-only.

### 4.13 Skills (22) — Phase 14

`GET /api/skills` · `/index` · `/slash-catalog` · `/builtin` · `/builtin/{name}` ·
`POST /api/skills/{id}/invoke` · `GET/POST /api/skills/{id}/markdown` ·
`POST /api/skills/{id}/test` · `/test-approval` · `GET /api/skills/{id}/test-status` ·
`POST /api/skills/audit-all` · `GET /api/skills/audit-all/status` · `/audit-all/cancel` ·
`PUT/DELETE /api/skills/{id}` · `POST /api/skills/search` · `add` · `import-from-url` ·
`PUT/DELETE /api/skills/builtin/{name}`

**Route-ordering hazard resolved:** `/audit-all/status` is declared *after*
`/{skill_id}` in Python and works only by declaration order. In Axum, use a literal
segment (`/audit-all/status`) which the router matches ahead of the `{skill_id}`
capture — structural, not incidental.

### 4.14 Tasks & assistant (30) — Phase 13

`GET/POST /api/tasks` · `/tasks/onboarding` · `/tasks/notifications` ·
`GET/PUT/DELETE /api/tasks/{id}` · `POST /api/tasks/{id}/{pause,resume,revert,run,stop,clear-cache}` ·
`GET /api/tasks/runs/recent` · `GET /api/tasks/{id}/runs` ·
`GET /api/tasks/meta/{output-targets,actions,events}` · `POST /api/tasks/parse` ·
`POST /api/tasks/{id}/webhook-regenerate` ·
`POST /api/tasks/{id}/webhook/{token}` **(unauthenticated — token in path)**

Assistant: `GET /api/assistant/session` · `GET/PATCH /api/assistant/settings` ·
`POST /api/assistant/run/{task_id}` · `GET /api/assistant/run-status/{task_id}` ·
`GET /api/assistant/available-timezones`

**`/webhook/{token}` is the one unauthenticated mutating endpoint.** Its exemption regex
`^/api/tasks/[^/]+/webhook/[^/]+/?$` must be preserved verbatim, including the trailing
optional slash. Behaviour: 404 on token mismatch (enumeration-safe), 409 if already
running, auto-pause admin-only-action tasks for non-admins.

### 4.15 Research (14) — Phase 11

`GET /api/research/active` · `/status/{sid}` · `/library` · `/detail/{sid}` ·
`POST /api/research/start` · `/cancel/{sid}` · `/result/{sid}` · `/result-peek/{sid}` ·
`/spinoff/{sid}` · `/{sid}/archive` · `/{sid}/hide-image` · `/{sid}/unhide-images` ·
`GET /api/research/report/{sid}` (HTML, relaxed CSP) · `DELETE /api/research/{sid}` ·
`GET /api/research/stream/{sid}` (**SSE**)

`/start` requires `can_use_research`; clamps `max_time` 60–1800,
`extraction_timeout` 15–3600, `extraction_concurrency` 1–12. Session id `rp-<12hex>`.
Target: replace the 1.5 s poll with `broadcast::Sender`.

### 4.16 Search & web (4) — Phase 10

```
GET  /api/search/config     GET /api/search/providers
POST /api/search/web        (was POST /api/search)
POST /api/search/query
```

### 4.17 Uploads & vision (6) — Phase 9

`POST /api/upload` (multipart, **the only remaining multipart endpoint**) ·
`POST /api/upload/cleanup` (admin) · `GET /api/upload/stats` (admin) ·
`GET /api/upload/{file_id}` · `GET/PUT /api/upload/{file_id}/vision`

Owner mismatch on `{file_id}` returns **404, not 403**.

### 4.18 Personal docs / RAG (6) — Phase 7

`GET /api/personal` · `POST /api/personal/{reload,add_directory,upload}` ·
`DELETE /api/personal/{remove_directory,file}`

All require user **and** admin except `/upload`, which requires `can_use_documents`.
Paths confined via `realpath` + `commonpath`. Serialised by a per-router lock.

### 4.19 MCP (11) — Phase 19

`GET/POST /api/mcp/servers` · `POST /api/mcp/servers/{id}/reconnect` ·
`PATCH/DELETE /api/mcp/servers/{id}` · `GET /api/mcp/tools` ·
`GET/PATCH /api/mcp/servers/{id}/tools` ·
`GET /api/mcp/oauth/authorize/{id}` · `GET /api/mcp/oauth/callback` ·
`POST /api/mcp/oauth/exchange/{id}`

All admin-gated. `/authorize` returns 302 for local hosts, else an HTML paste-back page.

### 4.20 Webhooks & public chat (6) — Phase 21

`GET/POST /api/webhooks` · `POST /api/webhooks/{id}/test` ·
`PATCH/DELETE /api/webhooks/{id}` · `POST /api/v1/chat`

`/api/v1/chat` is an explicit, documented public contract for n8n/Make/Activepieces.
Must remain an explicit Axum route (never a server function). Requires bearer scope
`chat`, re-checked in the handler.

### 4.21 Companion (5) — Phase 21

`GET /api/companion/{ping,info,models,pair}` · `POST /api/companion/pair`

`GET /pair` renders admin HTML and **never mints** (CSRF). `POST /pair` mints a
chat-scoped token, returns JSON with `?format=json` including a QR PNG data-URI.

### 4.22 Codex / Claude (~20) — Phase 21

`/api/codex/{capabilities,plugin.zip,todos,emails,memory,…}` and `/api/claude/*`.

**The path-string handler re-invocation must be replaced.** `codex_routes.py:124`
`_find_endpoint` looks up handlers by their URL path string and calls them inside
`_as_owner()`. In Rust, this becomes direct service-function calls with an explicit
owner parameter — no string matching, no request-state mutation.

### 4.23 Remaining tail (48) — Phase 22

Vault (6), Contacts (10), Compare (5), Cleanup (2), Backup (2), Prefs (3),
Presets (8), Workspace (2), Diagnostics (6), Signature (3), Editor drafts (5),
Admin wipe (1), Emoji (~3), Fonts (1), TTS (3), STT (1), HWFit (4).

### 4.24 App-level (18) — Phase 0

`/` · `/notes` · `/calendar` · `/cookbook` · `/email` · `/memory` · `/gallery` ·
`/tasks` · `/library` · `/backgrounds` · `/login` · `/api/version` · `/api/health` ·
`POST /api/client-perf` · `GET /api/ready` · `GET /api/runtime` ·
`GET /api/generated-image/{filename}` · `POST /api/activity/heartbeat`

`/backgrounds` is an unauthenticated sandbox page for prototyping background effects →
gated to admin in the Rust port (it currently has no auth).

---

## 5. Error response mapping

| Python | HTTP | Rust |
|---|---|---|
| `SessionNotFoundError` | 404 | `AppError::SessionNotFound` → 404 |
| `InvalidFileUploadError` | 400 | `AppError::InvalidUpload` → 400 |
| `LLMServiceError` | 502 | `AppError::Llm` → 502 |
| `WebSearchError` | 502 | `AppError::Search` → 502 |
| `HTTPException` | varies | `AppError::Http(StatusCode, String)` |

All error bodies are `{"error": CODE, "message": str}` — preserve the shape, since the
frontend branches on `error`.

---

## 6. Compatibility policy

| Concern | Policy |
|---|---|
| Path stability | Preserve every path except the two documented replacements (`/api/search/messages`, `/api/search/web`). |
| Request shape | JSON everywhere. One-release `x-www-form-urlencoded` shim on `/api/chat/stream` for legacy callers. |
| Multipart | Only `/api/upload` and `/api/email/compose-upload`; convert to staged-upload-id JSON by Phase 9. |
| Response shape | Byte-identical where a test exists. |
| SSE event names | Byte-identical, enforced by round-trip tests. |
| Deprecations | `/api/auth/signup-toggle`, `POST /api/sessions/save` → no-op 200, documented. |