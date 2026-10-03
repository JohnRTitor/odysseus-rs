# Odysseus Rust Rewrite — Master Plan

**Status:** planning complete, implementation not started
**Source audited:** `/home/masum/Dev-Environment/Python/odysseus` @ `0b66c720`, `APP_VERSION = "1.0.3"`
**Target:** `/home/masum/Dev-Environment/Rust/odysseus-rs`

---

## 1. Executive architecture summary

One Axum process serves Leptos SSR, hydration assets, all application APIs, and all
SSE streams. Business logic lives in domain crates, never in UI components and never
in Axum handlers.

```
Browser
  │
  ▼
Axum (single process, apps/odysseus-server)
  ├── leptos_axum::generate_route_list / render_app_to_stream   → Leptos SSR
  ├── /_leptos/*                                               → hydration assets
  ├── /static/*                                                → static assets
  ├── /api/*                                                   → application APIs
  ├── SSE endpoints (13 of them, no WebSockets)                → streaming
  └── /api/auth/*, session middleware                           → auth/sessions
        │
        ▼
  odysseus-web        transport: routing, extractors, middleware, SSE encoding
        │
        ▼
  odysseus-services   application services: chat, research, email, calendar,
      │               documents, memory, search, tasks, gallery
      ├── odysseus-agent     agent loop, tool selection, approvals, compaction
      ├── odysseus-llm       provider adapters, 4 stream parsers, fallback router
      ├── odysseus-tools     tool registry, 71 schemas, implementations
      ├── odysseus-integrations  MCP, webhooks, external APIs, OAuth device flows
      ├── odysseus-store     rusqlite + sqlite-vec, 27 tables, migrations
      └── odysseus-domain   pure types, SSE event vocabulary, errors — no IO
```

### Crates

| Crate | Concern | Must not contain |
|---|---|---|
| `odysseus-domain` | Pure types, the SSE event enum, error types, tool schemas | Any IO, any framework |
| `odysseus-store` | `rusqlite` pools, migrations, 27-table schema, `sqlite-vec`, FTS5 | HTTP, business rules |
| `odysseus-llm` | Provider detection, 4 stream parsers, fallback router, host health, response cache | DB access, agent logic |
| `odysseus-agent` | The agent loop, tool selection funnel, approval sealing, context budgets | HTTP types, UI types |
| `odysseus-tools` | Tool registry, schema definitions, implementations, execution envelope | Agent-loop control flow |
| `odysseus-services` | Domain services | Axum types, Leptos types |
| `odysseus-integrations` | MCP client, outbound webhooks, integration presets, device flows | UI types |
| `odysseus-web` | Axum router, middleware, auth extractor, SSE encoding, app state | Business rules |
| `odysseus-ui` | Leptos components, routes, server functions | SQL, raw SQL, HTTP client code |

### Documented constraint on "one process"

The Rust binary is the only **application** process. The following are external by
nature and remain external services:

- **Model inference endpoints** (Ollama, llama.cpp, vLLM, cloud providers) — already
  external HTTP upstreams today.
- **Remote SSH targets** for cookbook operations.
- **MCP subprocesses** — stdio servers are separate processes by protocol definition.
- **`sqlite-vec`/ONNX model files** — data, not processes.

`scripts/diffusion_server.py` and `swift/odysseus-mlx-image-bridge/` are already
*optional out-of-process backends* in the current app, not part of the app process.
Anything replacing them inherits that placement.

---

## 2. Measured scale of the source

All numbers measured, not estimated.

| Layer | Size |
|---|---|
| Python | 1,159 files / **248,281 LOC** |
| Frontend JS | 165 files / **135,402 LOC / 6.23 MB** (`static/js/`) |
| Frontend entry | `static/app.js` 4,583 lines / 190 KB |
| CSS | **41,401 LOC / 1.29 MB** (single `static/style.css`) |
| HTML shell | `static/index.html` 2,591 lines / 236 KB (Jinja2 template) |
| Vendored UMD libs | 7.2 MB (`static/lib/`) |
| API endpoints | **493** (181 GET, 220 POST, 23 PUT, 10 PATCH, 41 DELETE) |
| SSE endpoints | 13 · **WebSockets: 0** |
| ORM tables | 27 + 1 FTS5 virtual table |
| Raw-sqlite3 sidecar tables | 15 (`data/scheduled_emails.db`) |
| Python tests | **829** files / **109,715 LOC** |
| JS tests | 5 `.mjs` files (**not run by CI**) |
| Background loops at startup | 30+ (`app.py:1056-1270`) |

Largest Python files: `src/agent_loop.py` 6,455 · `routes/email_routes.py` 6,152 ·
`routes/cookbook_routes.py` 4,583 · `src/llm_core.py` 3,730 · `routes/model_routes.py` 2,717.

---

## 3. Current architecture assessment

### 3.1 What works well and must be preserved

These are load-bearing behaviours, not incidental complexity:

1. **The KV-cache discipline.** `src/chat_processor.py:283-295` documents that nothing
   turn-varying may be folded into `system`, because llama.cpp/LM Studio key the prefix
   cache on byte-identical prefixes. The current date/time is injected as a `user`-role
   message immediately before the last user turn (`routes/chat_helpers.py:744-757`) for
   exactly this reason. The same discipline appears in `task_scheduler.py:1590-1601`.
2. **The fallback metadata-commit rule.** `src/llm_core.py:3648-3678` buffers
   `model_actual`/`usage`/`model_info` until the first substantive output. After that
   point a candidate can never be abandoned, because already-streamed tokens would
   duplicate. This is subtle and easy to break.
3. **Prompt-cache affinity is local-only.** `src/llm_core.py:1027` adds `session_id` +
   `cache_prompt:true` only for self-hosted OpenAI-compatible endpoints, because strict
   cloud providers 400 on it (issue #3793).
4. **Sealed exact-action tool approvals.** `src/tool_approvals.py` binds an approval to
   a digest over `{owner, session_id, origin_run_id, tool_name, content, workspace,
   document_id, document_version, document_digest, external_untrusted_context_seen,
   selected_tools, continuation_query, effects, result_integrity}`. `.claim()` is
   one-shot under a lock. A document mutated between card and click invalidates it.
5. **The execution envelope refuses to run without a security context.**
   `src/tool_execution.py:828-841` raises `TypeError` on a missing/wrong sentinel.
   This forces callers to be explicit and must be preserved as a type-level guarantee.
6. **Untrusted content never enters `system`.** `untrusted_context_message()` wraps
   every untrusted payload as a `user`-role message with `metadata.trusted = False`.
7. **SSRF DNS-rebinding pinning.** `src/outbound_fetch.py:119-163`, `webhook_manager.py:177`,
   `integrations.py` all validate DNS, then *pin the connect* to the validated IPs while
   preserving SNI and `Host`.
8. **Web fetch forces `Accept-Encoding: identity` and refuses any compressed response**
   (`outbound_fetch.py:289,313-319`), because a compressed body can decode to far more
   than `Content-Length` and defeat the streamed size cap.
9. **`_strip_think_blocks` and `_iter_delimited` are linear rescans, not regexes**
   (`agent_loop.py:1253`, `tool_parsing.py:1178`), replacing quadratic regexes. There
   are dedicated ReDoS regression tests that must survive.
10. **The error-logging discipline in `src/ai_interaction.py`-adjacent code**: a failed
    URL fetch is still injected as context but reduced to a transport-owned status
    string, never the exception text.
11. **`_RevalidatingStatic`** forces `Cache-Control: no-cache` on `.js/.css/.html` —
    correct given unversioned assets. Replaced by content-hashed filenames in Rust.
12. **`appConfig.js`** is the one properly architected frontend module: a shared
    settings/tools promise cache with invalidation. Its header comment documents the
    exact triple-instance bug that motivated it. This is the pattern to generalise.

### 3.2 Problems the Rust rewrite should fix

| Problem | Evidence | Target |
|---|---|---|
| Module-level singletons threaded through `set_*` functions | `app.py:588-608`, `ai_interaction.py:34-66`, `core/models.py:22-38` | Explicit dependency injection via an `AppState` constructed once |
| 45 hand-written migration functions run at startup, no Alembic | `core/database.py:2061-2149` | Single baseline migration + forward-only chain |
| Two migrations silently break on non-SQLite `DATABASE_URL` | `core/database.py:868`, `1207`, `2295` (raw `sqlite3` + `.replace("sqlite:///","")`) | Dialect-portable baseline |
| `init_db()` runs at **import** time | `core/database.py:2737` | Explicit `bootstrap()` in `main` |
| 15 route shim files | `routes/X_routes.py` → `routes/x/x_routes.py` | None |
| 234 `window.*` globals | `app.js:58-62` and ~229 more | Leptos context/signals |
| No router; routes are synthetic DOM clicks | `app.js:1105-1234`: `/cookbook` → `getElementById('tool-cookbook-btn')?.click()` | `leptos_router` |
| Manual `?v=` cache-busting has already caused triple module instantiation | `app.js:42-46` documents the production incident; `chatRenderer.js` loads 3×, 11 other modules 2× each | Content-hashed bundles |
| `API_BASE` re-declared in ~25 modules with 3 conventions | `calendar.js:21`, `sessions.js:12`, … vs `appConfig.js:14-16` | One typed API client |
| 3 coexisting SSRF policies | `url_safety.py` (permissive), `url_security.py` (strict), pinned variants | One configurable policy |
| Route-ordering hazards | `/api/skills/{id}` declared before `/audit-all/status`; `/api/tasks/{id}` before `/runs/recent`; `GET /api/search` defined twice | Declarative routing, no order dependence |
| `/api/hwfit/*` shells out over SSH with **no admin gate** | `hwfit_routes.py:185-412` | Admin-gated, or documented as a fix |
| 30+ background loops started unconditionally | `app.py:1056-1270` | Audited task set, opt-in where appropriate |
| `MAX_DEBATE_ROUNDS`, `escalate_and_learn()`, `SESSIONS_FILE` for chat sessions, `src/config.py` | vestigial | Not migrated; documented |

### 3.3 Dead / vestigial code — documented, NOT migrated

| Item | Evidence | Disposition |
|---|---|---|
| `services/faces/` | 1-line docstring, zero implementation | **REMOVED** — never implemented |
| 15 route shims | `routes/*_routes.py` → `routes/*/*_routes.py` | **REMOVED** — pure re-export |
| `gallery_people` | Listed in owner sweep `database.py:1447`; no such table or model exists | **REMOVED** — dead reference |
| `_event_pings_loop` | `task_scheduler.py:624`; never created in `start()` | **REMOVED** — unreachable |
| `escalate_and_learn()` | `teacher_escalation.py:435`; retired no-op returning `None` | **REMOVED** |
| `MAX_DEBATE_ROUNDS` | `ai_interaction.py:31`; vestigial after the `#3629` split | **REMOVED** |
| `ping_events` / `ping_notes` | Removed from `BUILTIN_ACTIONS` but still referenced in `_note_pings_loop` | **REMOVED** |
| `SESSIONS_FILE` for chat sessions | `session_manager.py:76` "kept for backward compat, not used"; chat sessions live in `app.db` | **REMOVED** |
| `src/config.py` | Largely dead; superseded by `src/settings.py`. Its `validate_config()` runs at import (`config.py:208`) | **REMOVED** |
| `GET /api/search` duplicate | `chat_routes.py:2695` and `search/search_routes.py:46`; first-registered wins | **REPLACED** — disambiguated |
| `escalate_and_learn` skill distillation | Now only reachable via the inline teacher flow | **REPLACED** — inline path only |

---

## 4. Locked architectural decisions

| # | Decision | Rationale |
|---|---|---|
| D1 | **Rust owns `:7000` from day one.** No proxy to Python. Unmigrated features render explicitly disabled. | Cleanest target; single process from the first commit. Cost: a real product regression for the migration duration, mitigated by an honest "not yet migrated" UI state (see §9). |
| D2 | **Leptos SSR + hydration**, shell and content server-rendered, canvas/editor surfaces client-only. | TTFB and first-paint benefit. See §6. |
| D3 | **Source-owned shadcn-style UI primitives** in `components/ui/`, Tailwind CSS. No Node runtime in deployment. | Required. |
| D4 | **`rusqlite` + `sqlite-vec`.** ChromaDB service removed entirely. | True single process. Cost: threshold re-tuning — see D4a. |
| D4a | Vector thresholds are a **gated migration task** with golden-set validation, never "tuned by hand". | ChromaDB and sqlite-vec have different distance metrics. |
| D5 | **Email: full parity, deferred to last.** `lettre` (SMTP), `async-imap` (IMAP), `mailparse` (MIME). | Parity is achievable; the crates are adequate. |
| D6 | **Single baseline migration** generated from live `data/app.db` `sqlite_master`, verified against a copy of a real database. | The live DB is ground truth; the SQLAlchemy models have known drift. |
| D7 | **ML inference via `ort`**, not `candle`. | ONNX Runtime has the execution-provider ecosystem and graph fusion. |
| D8 | `resup` **REMOVED** — docs.rs states it is "not a library", it is a CLI frontend for `realesrgan-ncnn-vulkan`. | Verified fact, not judgment. |
| D9 | `rembg-rs`, `kokoroxide` **HOLD** pending Phase 0 spike → REMOVE if the spike fails. | Both are minimally maintained (`rembg-rs`: 14 commits; `kokoroxide`: explicitly WIP, 7 stars). |
| D10 | `ort` must be pinned to **`2.0.0-rc.13`**. `"2.0"` does not resolve. | Verified against crates.io. Building on an RC is a deliberate risk. |
| D11 | **No DB calls on the async runtime.** Pool + `spawn_blocking`, or a dedicated DB thread. | `rusqlite` is synchronous. Non-negotiable design rule. |
| D12 | Phase 5 (tools) and Phase 6 (security) ship **atomically**. | Porting the tool layer without the security layer silently opens RCE. |

---

## 5. Incremental implementation plan

24 phases. Each ends with a working application. Ordering is derived from the actual
dependency graph in the source, not from the directory layout.

| Phase | Slice | Depends on | Why this position |
|---|---|---|---|
| 0 | Workspace, config, DB baseline, auth, Axum + Leptos shell, dependency spikes | — | Everything depends on it. De-risks the four infrastructural assumptions first. |
| 1 | Sessions CRUD + SSR list | 0 | First real vertical slice; proves schema + auth + SSR parity. |
| 2 | Model endpoints, model discovery, picker | 0 | Chat needs a model before anything can be said. |
| 3 | Chat, non-streaming | 1, 2 | Proves the LLM abstraction without streaming complexity. |
| 4 | **SSE streaming + agent loop core** | 3 | Highest-risk component. Do it early, not late. |
| 5 | Tool registry, schemas, text parsing, execution envelope | 4 | Depends on the loop. |
| 6 | Tool capabilities, policy, sealed approvals | 5 | **Ships atomically with 5.** |
| 7 | Context budgets, compaction, memory, RAG, sqlite-vec | 4, 6 | Depends on loop + tool gating. |
| 8 | Documents + editor | 7 | Document tools need approvals from 6. |
| 9 | Uploads + vision | 8 | |
| 10 | Web search + fetch + SSRF policy consolidation | 3 | Independent of the loop. |
| 11 | Deep research | 4, 10 | Needs both the loop and search. |
| 12 | Notes / todos | 1 | Simple CRUD; good early win after sessions. |
| 13 | Scheduled tasks + event bus | 4, 11 | Needs the loop to trigger runs. |
| 14 | Skills | 6, 7 | Skills declare `requires_toolsets`, so depends on the tool layer. |
| 15 | Email (57 endpoints, `scheduled_emails.db`) | 0, 3 | Last major subsystem. **Largest UNIMPLEMENTED window — see §9.2.** |
| 16 | Calendar + CalDAV | 15 | `extract_email_events` depends on email. |
| 17 | Gallery + image editor + ONNX image ops | 0, 8 | |
| 18 | Cookbook / model serving | 2, 17 | Needs model endpoints and image ops. |
| 19 | MCP | 5, 6 | Needs the tool namespace machinery. |
| 20 | Settings + admin + users + 2FA | 0, 1 | Auth-adjacent; earlier is safer. |
| 21 | Webhooks, API tokens, companion, Codex/Claude, integrations | 2, 6, 15, 16 | Depends on their target domains. |
| 22 | Vault, contacts, compare, backup, prefs, presets, diagnostics, TTS/STT, signatures, editor drafts, workspace, hwfit, cleanup, wipes, fonts, emoji | varies | Tail. |
| 23 | Decommission the Python app | 0–22 | **Gated on `INCOMPLETE.md` being empty.** |

### 5.1 Recommended first implementation slice: Phase 0 + Phase 1

Rust/Axum boots on `:7000`, serves a Leptos SSR shell, opens the existing `data/app.db`
read-write, authenticates against the existing `auth.json` + `sessions.json`, and lists
real sessions from the real database.

This proves the four riskiest *infrastructural* assumptions before a single line of AI
code is written:

1. The 27-table schema is readable/writable from Rust (D6 baseline is correct).
2. bcrypt + Fernet + existing sessions interoperate (`core/auth.py` compat).
3. Leptos SSR renders under a real CSP.
4. Single-process single-Axum-server is achievable on localhost and LAN.

### 5.2 Phase 0 dependency spikes (must resolve before Phase 0 commits)

| Spike | Question | Failure consequence |
|---|---|---|
| S1 `ort` EPs | Are `TensorRTExecutionProvider` / `CUDAExecutionProvider` present in the OSS crate, or gated behind `api-22`? | Phases 17/18 image ops become REMOVE |
| S2 `ort` versions | Pin `2.0.0-rc.13`; confirm session build API | Blocks any ONNX work |
| S3 SAM ONNX | Produce `image_encoder.onnx` (ViT-B) + `mask_decoder.onnx`; measure encoder pass | `/api/image/mask` REMOVE |
| S4 `any-tts` | Does it provide a Kokoro backend? If yes, `kokoroxide` is moot rather than REMOVE | TTS verdict changes |
| S5 `rembg-rs` | Load U2Net, produce an alpha matte | `/api/image/remove-bg` REMOVE |
| S6 `whisper-rs` | faster-whisper parity on CPU + CUDA | STT REMOVE |
| S7 `rusqlite` build | FTS5 enabled? `sqlite-vec` loadable as an extension? | Chat search / all vectors break |
| S8 `lettre`/`async-imap` | Gmail OAuth IMAP + STARTTLS SMTP round-trip | Email parity blocked |
| S9 SD scope decision | Is local SD needed at all, given `/api/image/inpaint` and `/api/image/harmonize` already proxy to a registered `ModelEndpoint`? | Avoids a diffusers port that duplicates existing capability |
| S10 `mupdf`/`poppler-rs` | PDF page rasterisation; AcroForm fill via `lopdf` | PDF export REMOVE |
| S11 SQLite-vec distance | Re-tune the thresholds in §4/D4a against a golden set | Silent retrieval-quality regression |

### 5.3 Dependencies between phases

```
Phase 0 ──┬─► 1 ──┬─► 3 ──┬─► 4 ──┬─► 5 ──► 6 ──┬─► 7 ──┬─► 8 ──┬─► 9
          │        │        │        │           │        │        │
          ├─► 2 ───┴────────┘        │           │        │        └─► 17 ──► 18
          │                          │           │        │
          ├─► 10 ───────────────────► 11         │        └─► 14
          │                          │           │
          ├─► 12                    13 ◄────────┘
          │
          └─► 20, 22
                    15 ──► 16 ──┐
                     ▲         ├──► 21
                     └─── 6 ───┘
              19 ◄── 5, 6
```

Critical path: **0 → 1 → 3 → 4 → 5 → 6 → 7 → 8 → 9**. Email (15) is deliberately off
the critical path and last.

---

## 6. SSR / hydration strategy

Odysseus is one application shell with modal and panel overlays. Most of the surface is
irreducibly client-interactive. There is no Astro-style islands split available in
Leptos, and pretending otherwise would be dishonest — Leptos is fine-grained reactive,
so server-rendered non-reactive markup costs nothing after hydration. The win is
first paint and TTFB, not partial hydration.

| Surface | SSR | Hydrate | Notes |
|---|---|---|---|
| App shell, sidebar, nav | Yes | Yes | |
| Session list | Yes | Yes | Real data from `app.db` |
| Chat history / message rendering | Yes | Yes | Bounded by `HISTORY_DISPLAY_CHAR_LIMIT = 160000` |
| Document text content | Yes | Yes | First paint of long documents matters |
| Settings shell + panel metadata | Yes | Yes | Registry-driven, like `settings/registry.js` |
| Theme | Inline critical CSS | Yes | Must avoid FOUC — see below |
| Gallery canvas editor (11K lines, 56 modules) | **No** | Lazy client-only | Current `panels.js` lazy-loader pattern already proves this works |
| Calendar grid / drag | **No** | Yes | |
| Email grid / reader / composer | **No** | Yes | |
| Compare panes | **No** | Yes | |
| Markdown / Mermaid / KaTeX rendering | No | Yes | Lazy lib injection, as today |

### 6.1 FOUC avoidance

`index.html:17-206` runs a nonce'd inline script that reads
`localStorage['odysseus-theme']` and sets CSS variables **synchronously before first
paint**. Leptos must replicate this with an inline `<script nonce>` in the document
head, plus `leptos_meta` for `<title>`/favicon/theme-color. This is non-negotiable —
the current behaviour is correct and users would notice.

### 6.2 Blocking CSP issue

Current policy (`core/middleware.py:115-153`) is `script-src 'self' 'nonce-…'` with
`style-src` retaining `'unsafe-inline'`, with an explicit comment that migrating requires
auditing every JS-set `style=` attribute.

Leptos emits `style:` attributes for dynamic styling. Under a nonce-only `style-src`
this breaks. Resolution: a **CSS-variable refactor** converting dynamic styles to
custom properties set on an element, shipped before the first Leptos page. This is
Phase 0 scope, not deferred.

---

## 7. Performance and memory strategy

See `PERFORMANCE_PLAN.md` for the full treatment. Summary of the honest position:

Rust will improve throughput (serialization, SSE parsing, query overhead) and startup
(import graph, not lazy init). It will **not** automatically reduce idle RSS unless the
30+ unconditional background loops are audited — that is an application-logic win
available in any language, and claiming it as a Rust win would be dishonest.

---

## 8. Testing strategy

See `TEST_MIGRATION.md`. The 829-file Python suite is the single strongest argument for
incremental migration: every phase has an existing behavioural oracle to port.

---

## 9. No partial feature exposure

### 9.1 The rule

A feature is exposed as functional only when its full intended slice works
end-to-end. Otherwise the UI control is rendered **disabled** with an explanatory
tooltip, and the feature is recorded in `INCOMPLETE.md`.

Explicitly prohibited:

- Empty components or placeholder pages presented as implemented.
- Hard-coded or mocked backend responses.
- Dummy handlers, `TODO`-driven behaviour, "coming soon" UI that implies a feature exists.
- Interfaces that render but do not perform their real function.
- Partially connected frontend/backend flows.
- Code paths that compile but are not wired in.
- Disabled functionality described as complete.

The disabled-state UX must distinguish three cases honestly:

| Case | Display |
|---|---|
| Not yet migrated | "Not yet available in the Rust app" + phase number |
| Deliberately removed | "Removed — see INCOMPLETE.md" + documented reason |
| Blocked by a failed spike | "Blocked: dependency verification failed" |

### 9.2 Known consequence of deferring email to last

Deferring email (D5) creates the largest UNIMPLEMENTED window in the plan. The
following depend on email and therefore stay UNIMPLEMENTED until Phase 15/16:

| Dependent feature | Phase | Source |
|---|---|---|
| 5 of 12 housekeeping task actions (`summarize_emails`, `draft_email_replies`, `email_auto_translate`, `check_email_urgency`, `extract_email_events`) | 13 | `task_scheduler.py:251-263` |
| Task output targets (email) | 13 | `task_routes.py:982` |
| Calendar event extraction from email | 16 | `builtin_actions.py:1518` |
| Codex/Claude email integration | 21 | `codex_routes.py:338-390` |
| 16 agent email tools | 5+ | `tool_security.py:17` `BUILTIN_EMAIL_TOOLS` |
| Note reminder delivery via email | 12 | `note_routes.py:843` |
| `mcp_servers/email_server.py` (2,912 lines) | 19 | — |

This is a deliberate, documented trade-off, not an oversight.

---

## 10. Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Agent loop port changes behaviour invisibly | **Critical** | Port the SSE event vocabulary 1:1; golden-event tests per event type; the vocabulary is the contract |
| Tool layer ported without the security layer → RCE | **Critical** | Phases 5+6 atomic (D12); port `test_security_regressions.py` as the merge gate |
| SQLite-vec threshold drift → silent retrieval-quality regression | **High** | Spike S11 + golden set; thresholds are a gated task, never hand-tuned (D4a) |
| `ort` EPs unavailable in the OSS crate | **High** | Spike S1 before committing Phase 17 |
| `ort` is an RC (`2.0.0-rc.13`) | **High** | Pin explicitly (D10); isolate behind an `odysseus-ml` crate boundary so a swap is cheap |
| Email crates fail against Gmail OAuth IMAP | **High** | Spike S8 |
| Loss of 5 of 8 local image endpoints | Medium | Documented REMOVE; the 2 proxy endpoints and `sharpen` survive, which covers the common self-hosted path |
| Leptos CSP `style-src` conflict | Medium | CSS-variable refactor in Phase 0 |
| 45 Python migrations → single baseline | Medium | Generate from live `sqlite_master`; diff against SQLAlchemy models and record every discrepancy |
| `data/scheduled_emails.db` 15 raw tables outside the ORM | Medium | Fold into `app.db` or keep separate — decide in Phase 15, document |
| `mupdf`/`poppler-rs` introduces a C dependency | Medium | Partially undercuts "no toolchain"; document honestly |
| Hard cutover (D1) causes a long visible feature regression | Medium | Honest disabled-state UX; `INCOMPLETE.md` is the public status board |
| 234 `window.*` globals hide coupling that a naive port will miss | Medium | Enumerate every global during Phase 4–5; `critic-correctness` pass per phase |
| Route-ordering dependencies break on naive port | Low | Declarative routing eliminates the class |
| Long transcripts exceed SSR payload | Low | Keep `HISTORY_DISPLAY_CHAR_LIMIT`; page messages as today |

---

## 11. Definition of done

A feature is DONE only when all of the following hold:

1. Source functionality has been understood and its edge cases documented.
2. The target implementation exists.
3. Frontend is wired.
4. Backend is wired.
5. Persistence and/or external integration is wired.
6. Real data flows end-to-end — no mocks.
7. Error handling exists for the failure modes.
8. Relevant edge cases are addressed.
9. Tests exist where appropriate.
10. No placeholder implementation remains.
11. The feature is actually exposed to users.

A feature must not leave `INCOMPLETE.md` until all eleven hold.

---

## 12. Document index

| Document | Purpose |
|---|---|
| `REWRITE_PLAN.md` | This file — strategy, architecture, phases, decisions, risks |
| `FEATURE_INVENTORY.md` | Complete feature/status matrix at feature-family granularity |
| `SOURCE_MAPPING.md` | Current module → target crate/module, with rationale |
| `API_MIGRATION.md` | 493 endpoints → target routes, SSE strategy |
| `UI_MIGRATION.md` | 165 JS files → Leptos component architecture |
| `DATA_MIGRATION.md` | 27 tables + FTS5 + 15 sidecar tables + files + state |
| `PERFORMANCE_PLAN.md` | Current/target/validation per hotspot |
| `TEST_MIGRATION.md` | 829 Python + 5 JS test files → Rust tests |
| `INCOMPLETE.md` | **Authoritative** register of everything not DONE |
| `TASKS.md` | Checkable execution checklist; one box per atomic unit |