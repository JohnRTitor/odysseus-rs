# TASKS.md — Execution Checklist

**This is the working checklist.** One box per atomic unit of work. Check a box **only**
in the same commit that completes it.

## How to work this list

1. Pick the lowest unchecked box whose phase dependencies are met.
2. Do that one thing.
3. Verify its acceptance criteria.
4. `git commit` — one atomic change, conventional-commit message referencing the task ID.
5. Check the box **in that same commit**.
6. Update `INCOMPLETE.md` if the task changed a feature's status.

**Rules:**

- **One box per commit.** No bundling unrelated work.
- **Never** check a box whose acceptance criteria are unmet.
- **Never** weaken or delete a test to make a box passable.
- If a box is discovered to be wrong, add a note under it rather than silently re-scoping.
- If a task turns out to be impossible as specified, mark it `[!]` and record why — do not mark it done.

**Commit message format:** `<type>(<scope>): <summary> [T<NNNN>]`
e.g. `feat(store): baseline migration from live sqlite_master [T0105]`

**Status:** 34 / 489 complete · Phase 0 in progress

**Spike outcomes:** see `SPIKE_RESULTS.md` — S1/S2 and S7 resolved, 9 pending.

**Spike outcomes:** see `SPIKE_RESULTS.md`. S1/S2 and S7 resolved; 9 pending.

---

## Legend

`[ ]` pending · `[x]` done · `[!]` blocked (needs a note)

---

## Phase 0 — Foundation

**Goal:** the Rust binary boots on `:7000`, opens the real `data/app.db`, authenticates
against the real `auth.json`, and serves an SSR Leptos shell.

**This phase proves the four riskiest infrastructural assumptions before any AI code
is written.**

### 0.1 Workspace & tooling

- [x] T0001 Create the cargo workspace: `crates/{domain,store,llm,agent,tools,services,integrations,web,ui,ml}`, `apps/{odysseus-server,odysseus-cli}`
- [x] T0002 Pin toolchain + edition (2024). Add `rust-toolchain.toml`
- [x] T0003 Add workspace lints: `#![deny(clippy::all)]`, forbid `unwrap()`/`expect()` in library crates
- [x] T0004 Set up `cargo-deny` (advisories, licenses, bans) + `cargo-audit`
- [ ] T0005 Add `criterion`, `proptest`, `insta`, `tempfile`, `pretty_assertions`, `mockall` as workspace dev-deps
- [x] T0006 Set up `cargo-nextest` with a `ci` profile target < 30 s
- [x] T0007 Write `crates/*/README.md` stating each crate's concern and what it must not import
- [ ] T0008 Add a CI job enforcing the dependency rules in `SOURCE_MAPPING.md` §9 (no `rusqlite` in `odysseus-ui`, no `axum` in `odysseus-agent`, etc.)

### 0.2 Dependency spikes (**blocking** — resolve before T0100)

- [x] T0020 **S1** `ort` EPs — **RESOLVED: not gated.** `api-22` is a version-gating feature (alongside `api-17`…`api-28`), not a paid tier. `cuda`/`tensorrt`/`coreml`/`directml` are all in the OSS crate. **Correction:** EP types are `ort::ep::TensorRT` / `ort::ep::CUDA`, not `*ExecutionProvider`. CUDA runtime still unverified (no GPU here)
- [x] T0021 **S2** `ort` version — confirm `2.0.0-rc.13` builds; pin it. Confirm the session-build API shape
- [ ] T0022 **S3** SAM ONNX — produce `image_encoder.onnx` (ViT-B) + `mask_decoder.onnx`; measure the encoder pass
- [ ] T0023 **S4** `any-tts` — does it provide a Kokoro backend? If yes, `kokoroxide` becomes moot (F338)
- [ ] T0024 **S5** `rembg-rs` — load U2Net, produce an alpha matte. Fail ⇒ F169 REMOVE
- [ ] T0025 **S6** `whisper-rs` — faster-whisper parity on CPU and CUDA. Fail ⇒ F339 REMOVE
- [x] T0026 **S7** `rusqlite` — FTS5 enabled? `sqlite-vec` loads as an extension? **Blocks F125 and F124**
- [ ] T0027 **S8** `lettre`/`async-imap`/`mailparse` — Gmail OAuth IMAP + STARTTLS SMTP round-trip. Fail ⇒ re-plan Phase 15
- [ ] T0028 **S9** SD scope decision — is local SD needed given the inpaint/harmonize proxy path exists? Record the decision
- [ ] T0029 **S10** `mupdf`/`poppler-rs`/`lopdf` — PDF rasterisation + AcroForm fill
- [ ] T0030 **S11** sqlite-vec metric — capture the golden set from live ChromaDB before the service is gone. **Do this before anything else touches vectors**

### 0.3 Storage layer

- [x] T0040 `odysseus-store`: `rusqlite` pool with a `spawn_blocking` discipline (D11). **No DB call on the async runtime**
- [x] T0041 `PRAGMA foreign_keys=ON` on every connection (matches `database.py:142-147`)
- [x] T0042 `PRAGMA busy_timeout` set — required once real concurrency exists
- [x] T0043 Emit `0001_baseline.sql` from **live `sqlite_master`**, not from the models. Regenerable via `tools/dump-baseline`
- [x] T0044 Diff baseline against each SQLAlchemy model; record every discrepancy as a fixture (`DATA_MIGRATION.md` §2.2)
- [x] T0045 `chat_messages_fts` + 3 triggers, scrubbing base64 media from the index
- [x] T0046 Fernet encrypt/decrypt at the column boundary for the 7 encrypted columns. **`.app_key` must stay readable**
- [ ] T0047 Naive-UTC datetime conversion helpers; round-trip tests on every datetime column
- [ ] T0048 `chmod 0600` on the DB **and** its `-journal`/`-wal`/`-shm` sidecars
- [x] T0049 **TEST: open a copy of the real `app.db`** — 33 tables, 81 indexes, 3 triggers, 0 FK violations, `.app_key` loads and round-trips
- [x] T0050 **TEST: `PRAGMA foreign_key_check` returns zero rows**
- [ ] T0051 **TEST: an existing encrypted column decrypts with the real `.app_key`**

### 0.4 Auth

- [x] T0060 bcrypt password verify against the real `auth.json` format (cost 12)
- [x] T0061 `auth.json` read/write with atomic replace + `fsync`
- [x] T0062 Legacy single-user shape migration; lowercase normalisation; reserved-username purge
- [x] T0063 Session tokens: `sessions.json`, 7-day TTL, **orphan check** (username must still exist)
- [ ] T0064 **TEST: login works against the real `auth.json` + `sessions.json`** (acceptance gate)
- [x] T0065 API tokens: `ody_` + 43 chars, bcrypt hash, `prefix` = 8 chars, id = 8 hex
- [x] T0066 Token cache: `RwLock<HashMap<prefix, Vec<Candidate>>>` + dirty flag, constant-time compare
- [x] T0067 14 scopes + 4 profiles; write scope auto-inserts its read scope
- [x] T0068 TOTP 2FA — **fails closed** when enabled but secret missing; 8 single-use backup codes
- [x] T0069 Privileges: 7 booleans + `max_messages_per_day` + `allowed_models`; refuse to demote the last admin
- [ ] T0070 User CRUD; delete revokes API tokens **first** (fail-closed), then sessions; rename rewrites live sessions

### 0.5 Web layer

- [x] T0080 Axum app with `AppState` built once — **no module-level singletons**
- [x] T0081 `SecurityHeadersLayer` — per-request nonce; `X-Content-Type-Options`, `Referrer-Policy`, `Permissions-Policy`, HSTS
- [x] T0082 **All CSP branches** (report / tool_render / pdf_preview / default): research reports, tool render, PDF preview (`default-src 'none'` + `SAMEORIGIN`), default (`DENY` + nonce)
- [x] T0083 `CorsLayer` from `ALLOWED_ORIGINS`
- [x] T0084 `CompressionLayer` that **does not compress `text/event-stream`**
- [x] T0085 `TimeoutLayer` — 45 s + the **10 exempt prefixes**. Found+fixed a Python bug: `/api/chat` matched `/api/chatgpt-subscription/*`
- [x] T0086 Slow-request log (0.75 s threshold)
- [ ] T0087 `AuthLayer` implementing the **exact 7-step gate order** (`API_MIGRATION.md` §2.1)
- [ ] T0088 `_is_trusted_loopback` — reject any of the 7 proxy-forwarded headers
- [ ] T0089 Internal-tool header + `compare_digest` + optional `X-Odysseus-Owner` impersonation
- [ ] T0090 Exempt paths: 10 exact + `/static` prefix + the task-webhook regex `^/api/tasks/[^/]+/webhook/[^/]+/?$`
- [ ] T0091 Identity helpers: `raw_user` / `effective_user` / `is_delegated` / `require_user` / `require_owner` / `require_privilege` / `require_admin` / `require_api_token_scope`
- [ ] T0092 **TEST: bearer token resolves `effective_user` correctly (never the `"api"` pseudo-user for ownership)** — port of `test_email_owner_scope.py`
- [ ] T0093 **TEST: CORS preflight reaches CORS, not the auth gate** — `test_cors_preflight.py`
- [ ] T0094 **TEST: tunnel/proxy headers defeat loopback trust** — `test_cors_preflight.py` sibling

### 0.6 Leptos shell

- [ ] T0100 `odysseus-ui` crate with `ssr` + `hydrate` features; Tailwind wired
- [ ] T0101 `<Routes/>` + shell route; `leptos_axum` handler
- [ ] T0102 `leptos_meta` for title / favicon / `theme-color`, per-route
- [ ] T0103 **Pre-paint theme script** — sets CSS vars from `localStorage` before first paint. **Non-negotiable**
- [ ] T0104 Theme HSL derivation → the 11 `--hl-*` syntax vars (port `index.html:39-59` exactly)
- [ ] T0105 Loader overlay + boot sequencing
- [ ] T0106 PWA manifest + rewritten service worker
- [ ] T0107 **TEST: SSR renders; no FOUC (CSS vars set before first paint)**

### 0.7 CSP prerequisite (**must land before any Leptos page ships**)

- [ ] T0110 Audit every JS-set `style=` write in the source
- [ ] T0111 Convert dynamic styles to class or `data-*` + CSS. Zero inline `style` attributes
- [ ] T0112 Tighten `style-src` to nonce-only, then **TEST** a full page under the tightened policy

### 0.8 Performance baseline (**blocking for all later claims**)

- [ ] T0120 Capture the Python baseline into `docs/baseline/`: idle RSS, `GET /` TTFB, startup, endpoint-mix `oha`, streaming chunk latency
- [ ] T0121 Record the same for a Rust skeleton build
- [ ] T0123 **TEST: startup < 150 ms, first byte < 200 ms**

### 0.9 Phase 0 exit

- [ ] T0130 Update `INCOMPLETE.md`: record all 11 spike outcomes
- [ ] T0131 `cargo clippy -- -D warnings` clean
- [ ] T0132 Full test suite green
- [ ] T0133 Commit Phase 0 as a series of atomic commits; `INCOMPLETE.md` shows Phase 0 features implemented

---

## Phase 1 — Sessions

**Goal:** create, list, read, update, archive, delete, export sessions. The first real
vertical slice.

- [ ] T0200 `odysseus-store/src/sessions.rs` — CRUD, `get_sessions_for_user`
- [ ] T0201 Remove the unbounded metadata dict; bounded LRU cache over SQLite
- [ ] T0202 Make `message_count` authoritative by construction — compute it in the insert transaction. **Return `Result`; stop swallowing write errors** (`session_manager.py:305-308`)
- [ ] T0203 Drop the per-read `COUNT(*)` now that the drift cause is gone
- [ ] T0204 `session_actions.rs`, `session_image_cleanup.rs`
- [ ] T0205 `sessions.rs` route module — 21 endpoints (`API_MIGRATION.md` §4.2)
- [ ] T0206 `POST /api/sessions/save` → documented no-op returning 200
- [ ] T0207 Export: md / txt / json / html, sanitised filename, `Content-Disposition: attachment`
- [ ] T0208 `SessionManager.ensure_task_session`
- [ ] T0209 `cleanup_empty_sessions` (auto-archive 30 d, min age 1 h)
- [ ] T0210 FTS5 message search; LIKE fallback when FTS5 is absent
- [ ] T0211 `_parse_msg_content` JSON heuristic ported **exactly** — a string that *looks* like JSON must stay a string
- [ ] T0212 Leptos session list, SSR + hydration
- [ ] T0213 Session create/rename/archive/delete/favorite UI
- [ ] T0214 History paging: 8 mobile / 24 desktop; `HISTORY_DISPLAY_CHAR_LIMIT = 160000`, tail 20000
- [ ] T0215 Skeleton loader (8–16 fake bubbles sized to viewport)
- [ ] T0216 Incognito sessions in `sessionStorage` (distinct from localStorage)
- [ ] T0217 Session folders, max 5 visible, `SIDEBAR_MAX_VISIBLE = 10`
- [ ] T0218 **TEST: create → read back exact values; owner isolation; auth denial**
- [ ] T0219 **TEST: SSR list renders real sessions from a real DB**
- [ ] T0220 **TEST: FTS index contains no base64 media** — `test_sqlite_base64_media` sibling

---

## Phase 2 — Models & endpoints

- [ ] T0300 `model_endpoints` CRUD, owner-scoped, admin-gated
- [ ] T0301 `api_key` column encrypted/decrypted
- [ ] T0302 Provider detection — hostname-exact (`anthropic.com.example` must not match)
- [ ] T0303 Model discovery: Ollama / LM Studio / llama.cpp / vLLAM
- [ ] T0304 Host port-scan discovery
- [ ] T0305 Model capability detection; LM Studio probe with 60 s TTL
- [ ] T0306 Endpoint resolution + normalisation — one implementation, not four
- [ ] T0307 `_first_chat_model` skips `text-embedding`, `tts-`, `whisper`, `dall-e`, `moderation`, `rerank`, `clip`, `stable-diffusion`
- [ ] T0308 Provider OAuth device flow — 3 providers × 3 routes; `device_code` stays server-side, browser gets only `poll_id`
- [ ] T0309 Copilot headers (`x-initiator`, `Openai-Intent`, `Copilot-Integration-Id`)
- [ ] T0310 ChatGPT subscription + PKCE + refresh (120 s skew)
- [ ] T0311 Provider list cache (30 s)
- [ ] T0312 Model picker UI; shares the favourites key with the models list
- [ ] T0313 Model capabilities route
- [ ] T0314 **TEST: 4 LLM request shapes (Anthropic / Ollama / ChatGPT / OpenAI) asserted against fixtures**
- [ ] T0315 **TEST: local cache affinity applied only to self-hosted endpoints** — `test_cache_affinity_local_only.py`
- [ ] T0316 **TEST: device-flow `device_code` never reaches the browser**

---

## Phase 3 — Chat, non-streaming

- [ ] T0400 `llm_call`, `llm_call_async` with retry (`MAX_RETRIES = 3`)
- [ ] T0401 `_sanitize_llm_messages` — drop orphan `tool`, prune unanswered `tool_calls`, merge consecutive user messages
- [ ] T0402 System-message consolidation into index 0
- [ ] T0403 `_llama_slot` foreground/background priority gate (`ODYSSEUS_LOCAL_MODEL_GATE`)
- [ ] T0404 Dead-host cooldown: 20 s, threshold 2, reset on success, lock-guarded
- [ ] T0405 Response cache keyed on `{url, model, messages, temp, max_tokens, header_identity}` — **credential identity hashed, never the secret**
- [ ] T0406 Anthropic payload: temp clamp [0,1], omit temp for Opus 4.7+, prompt caching, `Bearer`→`x-api-key`
- [ ] T0407 OpenAI payload: `stream_options.include_usage` except OpenRouter/Groq; `max_completion_tokens` for o1/o3/o4/gpt-4.5/gpt-5; `think:false` for Ollama thinking models
- [ ] T0408 Chat route, JSON only
- [ ] T0409 Session context assembly + date injection as a `user` message
- [ ] T0410 Persist the assistant message
- [ ] T0411 **TEST: `None` vs `""` content distinction** — Gemini/Ollama 400 on empty-string with `tool_calls`
- [ ] T0412 **TEST: response cache cannot serve one account's reply under another's route**
- [ ] T0413 **TEST: KV-cache prefix stays byte-identical across turns**

---

## Phase 4 — SSE streaming + agent loop (**highest risk**)

- [ ] T0500 `odysseus-domain/src/events.rs` — typed SSE enum, **1:1 with the wire format**
- [ ] T0501 **TEST: round-trip — serialised events match the Python output byte-for-byte**
- [ ] T0502 OpenAI-compatible stream parser; accept `data:` and `data: `
- [ ] T0503 **Gemini omitted-`index` fix** — allocate a fresh slot on each `function.name`, else all parallel calls collapse into slot 0
- [ ] T0504 Anthropic block-stream parser + cache token accounting
- [ ] T0505 Ollama NDJSON parser
- [ ] T0506 ChatGPT Responses `event:`-named parser
- [ ] T0507 `_HarmonyStreamRouter` — suffix-hold guard prevents partial-marker leakage
- [ ] T0508 `_DegenerateStreamGuard` — ≥28 same-token / 4-gram ×10 / dominant ≥78% of 96
- [ ] T0509 `<think>` auto-detect + stray-closing-tag repair
- [ ] T0510 Fallback chain with the **metadata-commit rule**
- [ ] T0511 `fallback` notice event
- [ ] T0512 Eligibility: status set `{408,425,429,500,502,503,504,507,508,529}` **and** `fallback_eligible`
- [ ] T0513 Agent loop: security assembly → classification → selection → route state → approval replay → round loop → finalize
- [ ] T0514 Turn classification; low-signal fast path (no tools, no prompt assembly)
- [ ] T0515 Per-route request state — **speculative compaction per candidate**
- [ ] T0516 Loop breaker + stall detector (bail at 4; runaway at 15)
- [ ] T0517 Intent-nudge supervisor (max 2)
- [ ] T0518 Auto-document creation at ≥30 lines of code fence
- [ ] T0519 Completion verifier (max 2 rounds, default off)
- [ ] T0520 Detached agent runs — replay buffer + live subscribe; `X-Odysseus-Run-Id`
- [ ] T0521 Compare mode deliberately **not** detached
- [ ] T0522 `/api/chat/stream`, `/resume`, `/stop`, `/status`
- [ ] T0523 Tool cancellation on client disconnect (`GeneratorExit` handling)
- [ ] T0524 Chat UI: message list, transcript reconstruction from metadata
- [ ] T0525 **Streaming renderer — port all five behaviours** (frozen blocks, fence append-mode, `token-new` spans, self-heal, degradation latch)
- [ ] T0526 Stream segmenter + **fuzz** (`proptest`, ported from `invariant.test.mjs`)
- [ ] T0527 Live thinking throttle (100 ms coalescing)
- [ ] T0528 Per-session stream state (6 maps) + **generation counter** so a superseded preflight send bails before the POST
- [ ] T0529 Chat composer + auto-escalation regex
- [ ] T0530 Markdown renderer (`pulldown-cmark` + hand-written passes 1–3)
- [ ] T0531 `sanitizeAllowedHtml` → `ammonia`. **Security boundary**
- [ ] T0532 Deep-link anchor repair (3 regexes) — **the deep-link contract**
- [ ] T0533 Thinking-block rendering
- [ ] T0534 **TEST: every Group B streaming test passes**
- [ ] T0535 **TEST: every Group C fallback test passes**
- [ ] T0536 **TEST: streaming fuzz — no quadratic behaviour, no orphaned nodes**
- [ ] T0537 **TEST: cancellation on disconnect leaves no orphaned subprocess**

---

## Phase 5 — Tool system

- [ ] T0600 Unified registry: one `Tool` trait — schemas + tags + `execute`. **Removes the 3-surface inconsistency**
- [ ] T0601 71 native function schemas as static data
- [ ] T0602 ~65 fenced tags
- [ ] T0603 Fenced-tag parse/format — `_fenced_tool_call` shared by parse and strip so they cannot disagree
- [ ] T0604 8 ordered parser patterns, first-match-wins
- [ ] T0605 ReDoS-safe linear scanning (structurally guaranteed: no backtracking regex)
- [ ] T0606 `strip_tool_blocks` mirrors parse decisions
- [ ] T0607 ~140 tool-name aliases
- [ ] T0608 Native→text bridge — flatten JSON args to each tool's legacy format
- [ ] T0609 Execution envelope — **refuses to run without an explicit security context, enforced by the type system**
- [ ] T0610 Path confinement: sensitive denylist → app-state denylist → hardlink check → `commonpath` containment
- [ ] T0611 `vet_workspace` refuses filesystem roots
- [ ] T0612 Filesystem tools; `_EmptyBodyWouldTruncate` guard; hard-link staging for new files
- [ ] T0613 Tool selection 4-layer funnel
- [ ] T0614 Tool-index timeout (1.5 s) → on timeout leave `None` so the keyword fallback runs
- [ ] T0615 `ALWAYS_AVAILABLE` = exactly `{manage_memory, ask_user, update_plan}`
- [ ] T0616 Word-boundary keyword matching (`tell` must not fire on everything; `fix` must not fire inside `prefix`)
- [ ] T0617 Document, admin, session, subprocess, web, model, interaction, coding, bg-job tools
- [ ] T0618 `src/tools/*` domain tools → `odysseus-tools/src/domains/*`
- [ ] T0619 **TEST: all Group A tool-confined tests pass**
- [ ] T0620 **TEST: 8 parser patterns each recognised**
- [ ] T0621 **TEST: envelope rejects execution with no security context**

---

## Phase 6 — Tool security (**ships atomically with Phase 5**)

- [ ] T0700 15 `ToolEffect` classes; declarative registry with duplicate detection
- [ ] T0701 `capabilities_for_action` — parse the action discriminator from any accepted shape
- [ ] T0702 **Ambiguous actions fail high** — unknown action on a multiplexed tool returns both read and write private
- [ ] T0703 `ResultIntegrity` — `SYSTEM` / `WORKSPACE_UNTRUSTED` / `EXTERNAL_UNTRUSTED`
- [ ] T0704 **Failure status does not make content trusted**
- [ ] T0705 `ToolRunSecurityContext.decision_for` — delegated credentials blocked **first and unconditionally**; no approval can lift it
- [ ] T0706 Sealed approvals — digest over the 14-field binding payload
- [ ] T0707 `.claim()` one-shot under a lock; TTL 10 min; max 2048
- [ ] T0708 `consume()` verifies ownership **before** destructive pop
- [ ] T0709 `retire_for_session()` returns whether the card carried external provenance
- [ ] T0710 Document-content digest binding
- [ ] T0711 Approval card UI — three options, display-only payload
- [ ] T0712 Per-turn `ToolPolicy`; guide-only mode disables **and** hides
- [ ] T0713 `email_tool_policy_names` returns **both** spellings
- [ ] T0714 Plan mode — allowlist inverted, unioned with the static mutator backstop
- [ ] T0715 Non-admin + delegated tool blocks; non-admins get MCP dropped entirely
- [ ] T0716 Approval replay — run the sealed action **before round 1** so the model sees the real result
- [ ] T0717 **TEST: every Group A approval/policy/capability test passes**
- [ ] T0718 **TEST: ambiguous action fails high**
- [ ] T0719 **TEST: a document mutated between card and click invalidates the approval**
- [ ] T0720 **TEST: a delegated credential cannot be approved into privilege**

---

## Phase 7 — Context, memory, RAG

- [ ] T0800 Token budget: `agent_input_token_budget = 6000` auto sentinel, hard max 200 000, max 90 messages
- [ ] T0801 Per-route trimming; strip internal `_protected` markers
- [ ] T0802 Context compaction — speculative per candidate; only the winner's state applies
- [ ] T0803 Context preface assembly — **untrusted content never enters `system`**
- [ ] T0804 **KV-cache rule: nothing turn-varying in the system prefix**
- [ ] T0805 Memory CRUD + pin + timeline + per-session
- [ ] T0806 Hybrid retrieval — `0.55*vector + 0.40*keyword + 0.05*recency` **with re-validated thresholds (F118)**
- [ ] T0807 Category boosts; relevance gate
- [ ] T0808 Memory extraction with order-independent fingerprint short-circuit
- [ ] T0809 Memory audit/tidy (120 s own timeout)
- [ ] T0810 Memory import; `.json` fast path without an LLM call
- [ ] T0811 sqlite-vec: 4 collections + **full reindex from source**
- [ ] T0812 `odysseus-ml` embeddings crate (`ort`), isolated so a swap is one-crate
- [ ] T0813 **Golden set captured before ChromaDB is removed (S11)**
- [ ] T0814 Personal-doc RAG: directory add/remove with `realpath`+`commonpath` confinement, per-router lock
- [ ] T0815 Chunking: `CHUNK_SIZE = 1000`, overlap 200
- [ ] T0816 RAG routes; admin except upload, which needs `can_use_documents`
- [ ] T0817 Teacher escalation — inline SSE so the user sees it; recursion guard
- [ ] T0818 Tier 1 regex eval; Tier 2 opt-in LLM eval
- [ ] T0819 **TEST: golden-set top-10 agreement meets the agreed threshold**
- [ ] T0820 **TEST: untrusted content never lands in a `system` message**
- [ ] T0821 **TEST: the system prefix is byte-identical across turns**

---

## Phase 8 — Documents & editor

- [ ] T0900 Document CRUD + versioning; every update creates a version row
- [ ] T0901 Leptos tabbed editor; drag reorder; inline title edit
- [ ] T0902 Markdown/HTML/CSV syntax highlighting driven by theme vars
- [ ] T0903 Document library modal
- [ ] T0904 Document ↔ chat streaming bridge — **doc-stream events before `doc_update`**, else the editor enters diff mode
- [ ] T0905 AI edit/suggest block parsing; optimistic-concurrency guard
- [ ] T0906 Document tidy / AI tidy — no length-based deletion
- [ ] T0907 Export ZIP
- [ ] T0908 Code-runner button
- [ ] T0909 PDF import → form fields (`lopdf`)
- [ ] T0910 PDF rasterisation (`mupdf`/`poppler`)
- [ ] T0911 PDF annotation pane; markdown-embedded `{{annot:…}}`
- [ ] T0912 AcroForm fill + export — signature rows owner-filtered; temp files unlinked
- [ ] T0913 PDF CSP branch: `default-src 'none'; frame-ancestors 'self'`
- [ ] T0914 **TEST: `pulldown-cmark` + hand-written passes preserve deep-link anchors**
- [ ] T0915 **TEST: `ammonia` blocks every vector from the Python XSS suite**
- [ ] T0916 **TEST: optimistic concurrency rejects a stale document version**

---

## Phase 9 — Uploads & vision

- [ ] T1000 Upload handler: save, dedupe, magic-byte sniffing, extension/ID validation
- [ ] T1001 Upload route — **the last multipart endpoint**; per-IP concurrency + sliding window
- [ ] T1002 **Resolve the 5/min vs 60/min rate-limit inconsistency** — one value, documented
- [ ] T1003 Auto-promote chat images to `GalleryImage`
- [ ] T1004 Download + thumbnail; owner mismatch → **404 not 403**
- [ ] T1005 Reference-safe cleanup; **503 if index integrity is unverified**
- [ ] T1006 Vision OCR/description with the `.vision/{id}.txt` cache; user edits override the model caption
- [ ] T1007 Multimodal assembly with a shared 24 000-char inline budget
- [ ] T1008 Path confinement on every attachment
- [ ] T1009 Attachment reference reservation — raise if a referenced upload is gone
- [ ] T1010 Base64 stripped from persisted history and the FTS index
- [ ] T1011 VL fallback for non-vision models
- [ ] T1012 YouTube transcript + comments
- [ ] T1013 **TEST: base64 never appears in `chat_messages.content`**
- [ ] T1014 **TEST: owner mismatch returns 404**

---

## Phase 10 — Search & web

- [ ] T1100 7 search providers with per-provider SafeSearch mapping
- [ ] T1101 SearXNG 3-stage degradation (news→general, drop language, drop engines, HTML scrape)
- [ ] T1102 Comprehensive search: chain × 2 attempts → rank → filter → parallel fetch
- [ ] T1103 **Preserve `source_index`** so `[CONTENT n]` labels match the model's `[n]` citations
- [ ] T1104 Result ranking formula incl. the hand-tuned news adjustment
- [ ] T1105 Page content extraction (HTML/PDF/text/JSON)
- [ ] T1106 Web fetch caps: soft 2 MB, hard 20 MB; manual redirect loop (max 5), each hop re-resolved and re-pinned
- [ ] T1107 **Force `Accept-Encoding: identity` and refuse any compressed response**
- [ ] T1108 SSRF policy consolidation — one policy, three modes, injectable resolver
- [ ] T1109 Search config, providers, query routes
- [ ] T1110 Search UI
- [ ] T1111 **TEST: a gzip response is refused** — `test_web_fetch_size_caps.py`
- [ ] T1112 **TEST: every redirect hop is re-validated and re-pinned**
- [ ] T1113 **TEST: loopback / link-local / metadata / CGNAT blocked per mode**

---

## Phase 11 — Deep research

- [ ] T1200 Research engine: multi-round search→read→synthesise; `### Sources` parsing
- [ ] T1201 Job lifecycle with all parameter clamps
- [ ] T1202 Endpoint fallback chain `research→utility→default→chat→owner-scoped-first-enabled`
- [ ] T1203 **Replace the 1.5 s poll with a `broadcast` channel**
- [ ] T1204 Library, detail, archive, delete, image hiding
- [ ] T1205 Standalone HTML report (relaxed CSP branch)
- [ ] T1206 Visual report generation + `ammonia` sanitisation (content is untrusted LLM output over crawled pages)
- [ ] T1207 Spinoff — **suppresses memory and RAG**
- [ ] T1208 Synapse visualiser
- [ ] T1209 Research UI + job list
- [ ] T1210 **TEST: report HTML is sanitised**
- [ ] T1211 **TEST: stream latency < 200 ms (poll removed)**

---

## Phase 12 — Notes & todos

- [ ] T1300 Notes CRUD, checklist, colour, label, pin, archive, due date, repeat, image, sort order
- [ ] T1301 Reminder dispatch; `note_id` starting `test-` is admin-only
- [ ] T1302 Notes panel (sidebar, not a modal)
- [ ] T1303 AI-generated notes — headless background agent, SSE drained to EOF without rendering
- [ ] T1304 Confetti, reminder-state keys, fullscreen toggle
- [ ] T1305 **TEST: checklist toggle persists**
- [ ] T1306 **TEST: `test-` note_id is admin-only**

---

## Phase 13 — Scheduled tasks & shell

- [ ] T1400 Task CRUD; all 6 schedule kinds; `task_type` llm/action/research
- [ ] T1401 `compute_next_run` — month-end clamping, tz-aware, **fails closed on malformed input**
- [ ] T1402 Scheduler loop: 10 s grace, adaptive sleep `clamp(delta, 1.0, 60.0)`
- [ ] T1403 **Foreground activity pushes due tasks to `now + 15min` and skips them**
- [ ] T1404 `Semaphore(1)` — exactly one model-backed task at a time
- [ ] T1405 `TaskRun` created as `queued` **before** waiting on the semaphore
- [ ] T1406 `TaskDeferred` / `TaskNoop` as control-flow types, not `Exception`
- [ ] T1407 Error path advances `next_run` even on failure + fallback recovery commit
- [ ] T1408 `_cancel_if_foreground_active` monitor (250 ms poll)
- [ ] T1409 Task→agent bridge; **approval forced to `deny`** with an explanatory message
- [ ] T1410 Grace summarisation when rounds are exhausted
- [ ] T1411 Task chaining: cycle detection (max depth 10) + same-owner validation
- [ ] T1412 Event bus — threshold counter; **`next_run` persisted before handoff**
- [ ] T1413 Built-in actions registry (`TaskNoop`/`TaskDeferred` semantics)
- [ ] T1414 Housekeeping defaults + `ensure_defaults` reconciliation
- [ ] T1415 Task webhook trigger — unauthenticated, 404 on mismatch, 409 if running; preserve the exemption regex
- [ ] T1416 NL task parsing — **returns a draft only, never creates a live task**
- [ ] T1417 Shell exec + SSE stream (plain / pty / tmux), all admin-gated, `Sec-Fetch-Site` check
- [ ] T1418 Detached bg jobs — **status from an on-disk exit file, not a live PID**; user command isolated in its own script
- [ ] T1419 `pid_alive` delegates to a platform helper — a bare kill(0) **kills the job on Windows**
- [ ] T1420 Bg monitor — `mark_followed_up` **only after success**, so failures retry
- [ ] T1421 `#!bg` marker
- [ ] T1422 Assistant routes + default-assistant dedupe sweep
- [ ] T1423 Tasks UI — **replace the 30 s poll with a broadcast subscription**
- [ ] T1424 **TEST: no double-fire after restart**
- [ ] T1425 **TEST: a webhook-token mismatch returns 404, not 401/403**
- [ ] T1426 **TEST: a malformed `scheduled_time` never 500s**
- [ ] T1427 **TEST: only one model-backed task runs at a time**

---

## Phase 14 — Skills

- [ ] T1500 `SKILL.md` storage with YAML frontmatter; `_usage.json` sidecar
- [ ] T1501 Jaccard-similarity relevance fallback
- [ ] T1502 Skills CRUD + raw markdown + search
- [ ] T1503 Invocation → skill-pinned prompt
- [ ] T1504 Skill testing — background agent run + LLM judge; **advisory only, never rewrites or demotes**
- [ ] T1505 Confidence nudges: pass .95 / needs_work .6 / fail .4
- [ ] T1506 Audit-all: test→judge→self-edit→teacher→demote-to-draft; cancellable
- [ ] T1507 Nightly audit — auto-fixes, **never deletes**
- [ ] T1508 Import from URL — `_GITHUB_HOSTS` allowlist, 64 files / 2 MB / 400 KB caps, suffix allowlist, SSRF-pinned
- [ ] T1509 Builtin skills + per-user override
- [ ] T1510 **Route order: `/audit-all/status` must resolve before `/{skill_id}`** — structural in Axum
- [ ] T1511 Mid-loop `requires_toolsets` unlock
- [ ] T1512 Slash commands: 47 top-level, 2 levels of subs, aliases, legacy aliases, setup wizard
- [ ] T1513 Slash autocomplete
- [ ] T1514 Skills UI (tab in the memory modal)
- [ ] T1515 **TEST: a skill test failure never deletes or demotes the skill**
- [ ] T1516 **TEST: import rejects >64 files, >2 MB, a non-allowlisted host, and a bad suffix**

---

## Phase 15 — Email (**largest subsystem; largest UNIMPLEMENTED window**)

- [ ] T1600 Decide: fold `scheduled_emails.db` into `app.db`, or keep separate
- [ ] T1601 **Schema tests for all 15 email tables before touching them**
- [ ] T1602 `email_accounts` incl. the partial unique index `ux_email_accounts_one_default_per_owner`
- [ ] T1603 Duplicate-default normalisation that **raises** rather than silently accepting a race
- [ ] T1604 Owner locks: `BEGIN IMMEDIATE` on SQLite, `FOR UPDATE` on row-locking DBs, owners sorted to avoid deadlock
- [ ] T1605 IMAP listing + search (Gmail INBOX→All Mail swap)
- [ ] T1606 Message read + threading + quote-boundary detection
- [ ] T1607 Mutations: mark read/unread, flag, archive, move, delete, answered, spam
- [ ] T1608 Attachments: metadata, single, **all-attachments ZIP**, inline image by Content-ID (415 if not an image), open-as-document
- [ ] T1609 SMTP send via `lettre` — MIME `mixed`/`alternative`, `In-Reply-To`/`References`
- [ ] T1610 Drafts + scheduled sends
- [ ] T1611 Agent draft approval/reject
- [ ] T1612 Compose staging — 4 paths; convert the multipart upload to a staged-id JSON flow
- [ ] T1613 AI summarise / translate / reply / style extraction
- [ ] T1614 4-layer cache: 8 s list, 60 s read, IMAP pool, prefetch
- [ ] T1615 Local message index; unread-state without IMAP
- [ ] T1616 Google OAuth — `/authorize` 302 + signed `state`; **callback is auth-exempt**
- [ ] T1617 Unsubscribe scan/execute/cleanup — **mailto `List-Unsubscribe` only, header re-read server-side**
- [ ] T1618 Urgency triage with SQLite-atomic checkpointing
- [ ] T1619 Email pollers; **prefer IMAP IDLE** with a polling fallback
- [ ] T1620 16 agent email tools (`BUILTIN_EMAIL_TOOLS` is the single source of truth)
- [ ] T1621 Email UI: library, reader, composer, scheduled, unsubscribe, inbox
- [ ] T1622 **TEST: owner isolation on every endpoint** (stricter than `require_user`)
- [ ] T1623 **TEST: OAuth callback works with no session, validated by signed `state`**
- [ ] T1624 **TEST: a crash mid-triage does not re-alert** — `test_email_urgency_checkpoint.py`
- [ ] T1625 **TEST: two owners cannot share a default account**

---

## Phase 16 — Calendar & CalDAV

- [ ] T1700 Calendar + event CRUD; `scope=series` deletes
- [ ] T1701 RRULE expansion, EXDATE, all-day vs UTC
- [ ] T1702 ICS import/export; sanitised filename
- [ ] T1703 Natural-language event parsing
- [ ] T1704 CalDAV pull sync — 90 d lookback / 365 d lookahead; stable hashed calendar id; duplicate-UID pending dict
- [ ] T1705 **Prune safety — any parse failure skips the prune entirely**
- [ ] T1706 CalDAV write-back — pure injectable functions; **never raises**; local DB stays source of truth
- [ ] T1707 CalDAV URL hardening — all A/AAAA resolved, redirects disabled, `metadata.google.internal` blocked
- [ ] T1708 Multi-account CalDAV with migration from the legacy single key
- [ ] T1709 Event classification
- [ ] T1710 Recurring-event reminders (60 s note-ping loop)
- [ ] T1711 Calendar UI — month/week/year, custom date/time pickers
- [ ] T1712 **TEST: a parse failure does not delete events**
- [ ] T1713 **TEST: a private-IP CalDAV host requires the explicit env flag**
- [ ] T1714 **TEST: a write-back never raises and never loses local state**

---

## Phase 17 — Gallery & image

- [ ] T1800 Gallery CRUD; SHA-256 dedupe **scoped to owner**; EXIF; 100 MB cap
- [ ] T1801 Albums
- [ ] T1802 Generated-image serving — content-hash filenames, immutable cache, owner verified via the gallery row
- [ ] T1803 ZIP download
- [ ] T1804 AI tagging; clear/dedupe tags
- [ ] T1805 **Image inpaint proxy** — SSRF-hardened; base must resolve to a registered endpoint; **admins not exempt**
- [ ] T1806 **Image harmonize proxy**
- [ ] T1807 **Image sharpen** — `image` crate `UnsharpMask`
- [ ] T1808 Style transfer
- [ ] T1809 `generate_image` — deliberately absent from native schemas
- [ ] T1810 SAM masking via `ort` (S1/S3 dependent)
- [ ] T1811 remove-bg via `rembg-rs` (S5 dependent)
- [ ] T1812 Local diffusion (S9 dependent)
- [ ] T1813 **Removed endpoints render disabled with a documented reason** (F166, F167, F170)
- [ ] T1814 Canvas image editor — layers, composite, undo/redo, crop, flood-fill, lasso, wand, transform
- [ ] T1815 Editor drafts
- [ ] T1816 Gallery UI
- [ ] T1817 Editor bundle is client-only and lazy
- [ ] T1818 **TEST: an image proxy cannot reach a non-registered endpoint**
- [ ] T1819 **TEST: a dedupe cannot cross owners**

---

## Phase 18 — Cookbook / model serving

- [ ] T1900 SSH key management; remote host/port validation on every interpolated value
- [ ] T1901 Model download in tmux with ANSI-stripped log tailing
- [ ] T1902 Model serving in tmux / detached PowerShell
- [ ] T1903 Remote dependency setup (Windows/Termux/Linux detection)
- [ ] T1904 GPU inventory
- [ ] T1905 Cookbook state read/write (atomic)
- [ ] T1906 HF catalog + GGUF file listing with VRAM fit
- [ ] T1907 Ollama library (30 s cache)
- [ ] T1908 vLLM recipe manifest
- [ ] T1909 Hardware detection — nvidia-smi, rocm, Apple, Windows
- [ ] T1910 **hwfit routes: ADD an admin gate.** The current endpoints shell out over SSH with none — a deliberate fix
- [ ] T1911 Model fit ranking — bandwidth tables, harmonic offload blend, quant tables
- [ ] T1912 Serve profiles that never exceed the model's trained context
- [ ] T1913 Package search/install; system deps; rebuild engine
- [ ] T1914 Cookbook UI (7 modules) incl. cross-tab leader election
- [ ] T1915 Serve lifecycle — kill serves whose window-end has passed
- [ ] T1916 **TEST: an invalid SSH host is rejected before command construction**

---

## Phase 19 — MCP

- [ ] T2000 stdio / SSE / HTTP transports with exit-stack safety on mid-handshake failure
- [ ] T2001 `_generation` counter drives RAG re-indexing and cache invalidation
- [ ] T2002 Builtin reconnect + retry; **user servers do not auto-reconnect**
- [ ] T2003 Image blocks → `images:[{data,mimeType}]`
- [ ] T2004 MCP `isError` sets `untrusted_content` → **arms the tool gate**
- [ ] T2005 Namespace export; skip builtin servers; include `builtin_browser`
- [ ] T2006 Param hint hardening — 12 params, 40 chars/token, 300 total
- [ ] T2007 Plan-mode blocking — `readOnlyHint`/`destructiveHint` then verb heuristic, **failing closed**
- [ ] T2008 OAuth — RFC 9728 discovery, DCR, PKCE; tokens + registration persisted encrypted
- [ ] T2009 OAuth state futures pruned at 300 s
- [ ] T2010 MCP routes; `/authorize` 302 for local hosts else HTML paste-back
- [ ] T2011 Built-in servers registered as native Rust tools
- [ ] T2012 MCP UI
- [ ] T2013 **TEST: an MCP error result arms the tool gate**
- [ ] T2014 **TEST: an unknown-tool server is blocked, not allowed**

---

## Phase 20 — Settings & admin

- [ ] T2100 Settings store; ~60 defaults; retired-key tombstone; `is_setting_overridden` reads the raw file
- [ ] T2101 Settings API with numeric clamping
- [ ] T2102 Feature toggles (8)
- [ ] T2103 `scrub_settings` for non-admins — **security boundary**
- [ ] T2104 Settings registry: 5 groups, 13 panels, **validated against the component tree**
- [ ] T2105 Settings UI with 13 panels under one shell; admin panels defer to the admin controller
- [ ] T2106 Admin panel: users, tokens, webhooks, MCP, providers, endpoints, db stats, diagnostics, backup, wipe
- [ ] T2107 Shared config cache (the `appConfig.js` pattern)
- [ ] T2108 Keyboard shortcuts; AltGr guard
- [ ] T2109 Sensitive-content censor
- [ ] T2110 Service health — timing budget ladder so the endpoint **cannot hang**
- [ ] T2111 Accessibility helpers
- [ ] T2112 **TEST: a non-admin never receives a secret from `/api/auth/settings`**
- [ ] T2113 **TEST: every registry panel has a matching component** (ported from `getSettingsRegistryIssues`)

---

## Phase 21 — Integrations, webhooks, tokens

- [ ] T2200 Outgoing webhooks; HMAC-SHA256 over the exact body; per-delivery client; redirects disabled
- [ ] T2201 Webhook SSRF + DNS pinning — **override only `connect_tcp`, so SNI and `Host` survive**
- [ ] T2202 Error sanitisation — IPv6 (incl. bracketed), then IPv4, then URL; **log without the URL**
- [ ] T2203 Fire-and-forget with a strong-reference task set
- [ ] T2204 8 integration presets; `api_key` encrypted at rest; masked on read
- [ ] T2205 `execute_api_call` — recording resolver so the connect is **pinned to the validated IPs**
- [ ] T2206 Binary-search truncation for lists; insertion-order for dicts
- [ ] T2207 HTTP ≥400 returns `untrusted_content: true` — **explicit provenance**
- [ ] T2208 `POST /api/v1/chat` — 3 modes, 13 providers, `base_url` validated. **Explicit Axum route, never a server function**
- [ ] T2209 Codex routes — **replace path-string handler re-invocation with direct service calls**
- [ ] T2210 Claude routes
- [ ] T2211 Codex plugin ZIP
- [ ] T2212 Companion ping/info/models/pair; `GET /pair` **never mints**; QR data-URI
- [ ] T2213 API token management UI
- [ ] T2214 **TEST: a webhook cannot reach loopback/metadata, and the pinned IP is the validated IP**
- [ ] T2215 **TEST: an integration API call cannot reach a private IP**

---

## Phase 22 — Tail

- [ ] T2300 Vault — **password must never appear in argv**
- [ ] T2301 Contacts / CardDAV — vCard parse + unfold; passwords encrypted
- [ ] T2302 Blind compare + vote; redacted
- [ ] T2303 Compare UI
- [ ] T2304 Workspace browse/vet
- [ ] T2305 Backup export/import; cross-user and skill dedup
- [ ] T2306 Cleanup service with preview
- [ ] T2307 User prefs; presets + templates
- [ ] T2308 Admin danger-zone wipes
- [ ] T2309 TTS service — 4 providers; cache keyed on `sha256(provider|model|voice|speed|text)`; extension from magic bytes; LRU to 80 % of the cap; `_safe_speed` against agent-written corrupt settings
- [ ] T2310 STT service — 4 providers
- [ ] T2311 Voice recorder
- [ ] T2312 Sandboxable user mini-apps
- [ ] T2313 Signatures
- [ ] T2314 Emoji proxy (hardening-tested), fonts, colour picker, language icons
- [ ] T2315 Window tiling & snapping
- [ ] T2316 Onboarding tours
- [ ] T2317 **TEST: a vault password never appears in a process argument list**

---

## Phase 23 — Decommission

- [ ] T2400 `INCOMPLETE.md` has zero `UNIMPLEMENTED` and zero `PARTIAL` rows, except accepted `REMOVED`
- [ ] T2401 Every `REMOVED` reason reviewed and accepted
- [ ] T2402 Every spike has a recorded outcome
- [ ] T2403 Full test suite green
- [ ] T2404 `cargo clippy -- -D warnings` clean; `cargo deny` clean; `cargo audit` clean
- [ ] T2405 A user has run the Rust app through a normal week of use
- [ ] T2406 Remove the Python app, ChromaDB dependency, and stale config
- [ ] T2407 Final `INCOMPLETE.md` update + changelog entry

---

## Blocked

*None currently. Add here if a task proves impossible as specified — do not mark it done.*

---

## Notes

*Append observations here as work proceeds. Do not delete history.*