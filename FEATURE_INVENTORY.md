# Odysseus — Feature Inventory

**Source:** `/home/masum/Dev-Environment/Python/odysseus` @ `0b66c720`
**Status of this document:** authoritative feature/status matrix.
**Granularity:** feature-family. One row per coherent user-facing capability, not per
endpoint. Endpoint-level detail lives in `API_MIGRATION.md`; this document's job is the
status signal.

## Status vocabulary

| Status | Meaning |
|---|---|
| **IMPLEMENTED** | Real implementation, frontend+backend integrated, real data flow, no essential placeholder remains |
| **PARTIAL** | Some meaningful implementation exists but one or more required pieces are missing. **Must not be counted as complete.** |
| **UNIMPLEMENTED** | Not implemented in the Rust rewrite (yet) |
| **REMOVED** | Intentionally removed. Reason documented. |
| **REPLACED** | Existing behaviour intentionally replaced. Old and new behaviour both documented. |

**Every row currently reads `UNIMPLEMENTED` or `REMOVED`**, because implementation has
not started. The `Target phase` column is the plan.

---

## Index by domain

| Domain | IDs | Rows |
|---|---|---|
| [A. Application shell & navigation](#a-application-shell--navigation) | F001–F012 | 12 |
| [B. Authentication, accounts & sessions](#b-authentication-accounts--sessions) | F020–F035 | 16 |
| [C. Chat & streaming](#c-chat--streaming) | F040–F058 | 19 |
| [D. Agent, tools & approvals](#d-agent-tools--approvals) | F060–F085 | 26 |
| [E. Models, endpoints & providers](#e-models-endpoints--providers) | F090–F105 | 16 |
| [F. Context, memory, RAG & embeddings](#f-context-memory-rag--embeddings) | F110–F125 | 16 |
| [G. Documents, editor & PDF](#g-documents-editor--pdf) | F130–F148 | 19 |
| [H. Uploads, vision & gallery](#h-uploads-vision--gallery) | F150–F170 | 21 |
| [I. Search & web](#i-search--web) | F180–F188 | 9 |
| [J. Research](#j-research) | F190–F196 | 7 |
| [K. Notes, tasks & scheduling](#k-notes-tasks--scheduling) | F200–F218 | 19 |
| [L. Calendar & CalDAV](#l-calendar--caldav) | F220–F232 | 13 |
| [M. Email](#m-email) | F240–F262 | 23 |
| [N. Skills](#n-skills) | F270–F280 | 11 |
| [O. Settings, admin & configuration](#o-settings-admin--configuration) | F290–F305 | 16 |
| [P. Integrations, webhooks, tokens & external APIs](#p-integrations-webhooks-tokens--external-apis) | F310–F325 | 16 |
| [Q. Contacts, vault, compare & misc](#q-contacts-vault-compare--misc) | F330–F345 | 16 |
| [R. System, health & diagnostics](#r-system-health--diagnostics) | F350–F362 | 13 |

**Total: 300 features.**

---

## A. Application shell & navigation

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F001 | Single-page app shell | `static/index.html` (2,591 L), `app.py:892-949` | One HTML shell served at `/`, `/notes`, `/calendar`, `/cookbook`, `/email`, `/memory`, `/gallery`, `/tasks`, `/library`, `/login` | none | Med | Low | `odysseus-ui/src/app.rs`, shell route | 0 | UNIMPLEMENTED |
| F002 | Client-side routing | `app.js:1105-1234` | Boot-time `switch` on `location.pathname`; routes implemented as synthetic DOM clicks (`getElementById('tool-cookbook-btn')?.click()`) | none | Low | Med | `leptos_router` routes | 0 | REPLACED — real router replaces DOM-click routing |
| F003 | Sidebar / icon-rail layout | `sidebar-layout.js` (613 L) | 3-state `full`/`mini`/`off` in `localStorage['odysseus-sidebar-mode']`, mirrored to `<html>` classes; publishes `--icon-rail-w`/`--sidebar-w` CSS vars | localStorage | Med | Low | `components/layout/sidebar.rs` | 0 | UNIMPLEMENTED |
| F004 | Modal window manager | `modalManager.js` (1,560 L) | Register/toggle/minimize/close lifecycle, monotonic `_modalTopZ` from 300, minimized-chip dock with chain physics, `odysseus:modal-opened` event | none | High | Med | `components/layout/modal.rs` + dialog primitive | 0 | UNIMPLEMENTED |
| F005 | Window tiling & snapping | `tileManager.js`, `modalSnap.js` | Drag to edge/corner → snap; `topPortalZ()` derives dropdown z from the live stack | none | Med | Low | `components/layout/tile.rs` | 22 | UNIMPLEMENTED |
| F006 | Escape-key arbiter | `ui.js` (capture-phase, priority cascade) | Hovered floating window → `escMenuStack` LIFO → ignore-if-typing → thinking toggle → editor guard → settings collapse → close top modal | none | Med | Med | `components/ui/*` + `leptos` event handler | 0 | UNIMPLEMENTED |
| F007 | Ad-hoc dropdown stack | `escMenuStack.js` (DOM-free core) | LIFO dismiss registry; `bindMenuDismiss(el, onClose, isOutside)`; stashes `close` on `el._dismiss` | none | Low | Low | `components/ui/dropdown.rs` | 0 | UNIMPLEMENTED |
| F008 | Toast notifications | `ui.js:300` (`showToast`), `showError` | Swipe-to-dismiss, auto-sizing | none | Low | Low | `components/ui/toast.rs` | 0 | UNIMPLEMENTED |
| F009 | Dialogs (styled confirm/prompt) | `ui.js:584,681` | `styledConfirm`, `styledPrompt` | none | Low | Low | `components/ui/dialog.rs` | 0 | UNIMPLEMENTED |
| F010 | Theme system | `theme.js` (2,115 L) | 17 built-in themes + max 8 custom; `applyColors()` writes `--bg/--fg/--panel/--border/--red` + 10 syntax vars to `documentElement`; 4 fonts; UI density; UI text scale; 12 animated background patterns; frosted glass | localStorage | High | Med | `components/layout/theme.rs` + Tailwind CSS vars | 0 | UNIMPLEMENTED |
| F011 | Pre-paint theme script (FOUC avoidance) | `index.html:17-206` | Nonce'd inline script reads `localStorage['odysseus-theme']` and sets CSS vars synchronously before first paint; derives syntax colours from HSL; swaps favicon + `theme-color` | localStorage | Med | **High** | `odysseus-ui` head script + `leptos_meta` | 0 | UNIMPLEMENTED |
| F012 | PWA manifest & service worker | `static/sw.js` (239 L), `index.html:182-206,2589` | 3 strategies (stale-while-revalidate for `/`, network-first for js/css, cache-first for other static); 57-entry precache + 55-entry panel precache; per-route manifest blob | Cache API | Med | Low | Leptos hashed assets + `sw.js` rewrite | 0 | UNIMPLEMENTED |

---

## B. Authentication, accounts & sessions

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F020 | Password hashing | `core/auth.py:73-78` | bcrypt, `gensalt()` default cost 12; `PASSWORD_MIN_LENGTH = 8` | `auth.json` | Low | High | `odysseus-store/src/auth.rs`, `bcrypt` crate | 0 | UNIMPLEMENTED |
| F021 | User store | `core/auth.py` (no DB table) | `data/auth.json`, lowercase-normalised keys; legacy single-user shape migrated on load; reserved usernames dropped on load (`internal-tool`,`api`,`demo`,`system`,`__odysseus_local__`) | `auth.json` | Med | High | `odysseus-store/src/auth.rs` | 0 | UNIMPLEMENTED |
| F022 | Browser session tokens | `core/auth.py:53,585-646` | `secrets.token_hex(32)`, TTL 7 days, `data/sessions.json`; validation includes an orphan check (username must still exist) | `sessions.json` | Low | High | `odysseus-store/src/auth.rs` | 0 | UNIMPLEMENTED |
| F023 | Login / logout / signup / first-run setup | `routes/auth_routes.py:127-205` | `SESSION_COOKIE`; `/api/auth/setup` creates the first admin; open-signup toggle | `auth.json` | Med | High | `odysseus-web/src/routes/auth.rs` | 0 | UNIMPLEMENTED |
| F024 | Auth middleware + exemptions | `app.py:364-485` | Order: CORS preflight → exempt path → internal-tool token (loopback + `compare_digest`) → `LOCALHOST_BYPASS` → not-configured redirect → bearer `ody_` → cookie | n/a | High | **Critical** | `odysseus-web/src/middleware/auth.rs` | 0 | UNIMPLEMENTED |
| F025 | Trusted-loopback detection | `app.py:348-362` | Direct loopback **and** none of `cf-connecting-ip, cf-ray, cf-visitor, x-forwarded-for, x-forwarded-host, x-real-ip, forwarded`. Prevents tunnel/reverse-proxy bypass | n/a | Low | **Critical** | `odysseus-web/src/middleware/auth.rs` | 0 | UNIMPLEMENTED |
| F026 | API tokens (bearer) | `routes/api_token_routes.py`, `app.py:298-337,416-468` | `ody_` + `token_urlsafe(32)`, bcrypt-hashed, `token_prefix` = first 8 chars, id = `uuid4()[:8]`; in-process prefix→candidates cache rebuilt only when dirty; fire-and-forget `last_used_at` | `api_tokens` | High | **Critical** | `odysseus-store/src/api_tokens.rs` | 1 | PARTIAL — store, cache and scopes land; no HTTP route yet (T0087) |
| F027 | API token scopes | `routes/api_token_routes.py:26-72` | 14 scopes; write scope auto-inserts its read scope; 4 profiles | `api_tokens.scopes` | Med | High | `odysseus-store/src/api_tokens.rs` (scopes are stored, so they belong beside the token) | 1 | PARTIAL — normalisation, profiles and `has_scope` land; no enforcement point in a route yet (T0091) |
| F028 | Delegated-credential semantics | `src/auth_helpers.py:44-60` | `is_delegated_credential` prevents a bearer token inheriting its owner's admin/shell powers; `effective_user` resolves the real human for data attribution | n/a | Med | **Critical** | `odysseus-web/src/auth.rs` | 0 | UNIMPLEMENTED |
| F029 | TOTP 2FA | `core/auth.py:483-562` | `pyotp`; pending-secret then confirm; `valid_window=1`; 8 backup codes consumed on use; **fails closed** when enabled but secret missing | `auth.json` | Med | High | `odysseus-store/src/totp.rs`, `totp-rs` | 0 | UNIMPLEMENTED |
| F030 | Per-user privileges | `core/auth.py:24-48,378-466` | 7 booleans + `max_messages_per_day` + `allowed_models` (+restricted/block_all flags); admins get all; `set_admin` refuses to remove the last admin and stashes/restores privileges | `auth.json` | Med | High | `odysseus-domain/src/privileges.rs` | 0 | UNIMPLEMENTED |
| F031 | User CRUD + rename | `routes/auth_routes.py:301-367,610-680` | Delete revokes API tokens from the DB **first** (fail-closed), then purges sessions; rename rewrites the auth key and every live session | `auth.json`, `api_tokens`, `sessions.json` | Med | High | `odysseus-web/src/routes/auth.rs` | 0 | UNIMPLEMENTED |
| F032 | User ownership sweep | `core/database.py:1396-1517` | Blanket `UPDATE <table> SET owner=? WHERE owner IS NULL` across 19 tables at startup + hourly; assigns the first admin; also rewrites `memory.json` and restructures `user_prefs.json` | 19 tables | Med | **High** | `odysseus-store/src/owner.rs` | 0 | UNIMPLEMENTED |
| F033 | Owner resolution helpers | `src/auth_helpers.py:10-43`, `routes/email_helpers.py` | `get_current_user` (raw) vs `effective_user` (real human) vs `require_user` / `require_owner` / `require_privilege` / `require_admin` / `require_api_token_scope` | n/a | Med | **Critical** | `odysseus-web/src/auth.rs` | 0 | UNIMPLEMENTED |
| F034 | Internal-tool impersonation | `app.py:382-397` | `X-Odysseus-Internal-Token` + `X-Odysseus-Owner`; impersonation only if the target user exists | n/a | Low | **Critical** | `odysseus-web/src/middleware/auth.rs` | 0 | UNIMPLEMENTED |
| F035 | Encryption at rest | `core/database.py:150-172`, `src/secret_storage.py` | Fernet via `EncryptedText` TypeDecorator; 6 columns; key in `data/.app_key` (44 bytes) | `app.db`, `.app_key` | Med | High | `odysseus-store/src/crypto.rs` | 0 | UNIMPLEMENTED |

---

## C. Chat & streaming

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F040 | Non-streaming chat | `routes/chat_routes.py:771` | Full context build + tool policy; persists the assistant message; fires background memory/webhook/auto-name | `chat_messages` | Med | Med | `odysseus-services/src/chat.rs` | 3 | UNIMPLEMENTED |
| F041 | SSE streaming chat/agent | `routes/chat_routes.py:962` | Accepts FormData **or** JSON; 20+ FormData fields; emits 40+ event types; terminates `data: [DONE]` | `chat_messages` | High | **Critical** | `odysseus-services/src/chat.rs` + `odysseus-web/src/sse.rs` | 4 | UNIMPLEMENTED |
| F042 | SSE event vocabulary | `src/agent_loop.py:3454-3461`, `src/llm_core.py:2582` | `delta`, `thinking`, `tool_call_delta`, `tool_calls`, `usage`, `model_actual`, `tool_start/progress/output`, `agent_step/prep/terminal`, `ask_user`, `plan_update`, `doc_*`, `ui_control`, `web_sources`, `rag_sources`, `memories_used`, `compacted`, `context_trimmed`, `metrics`, `fallback`, `loop_breaker_triggered`, `rounds_exhausted`, `budget_exceeded`, `research_*`, `message_saved`, … | n/a | High | **Critical** | `odysseus-domain/src/events.rs` (typed enum) | 4 | UNIMPLEMENTED |
| F043 | Detached agent runs | `src/agent_runs.py`, `chat_routes.py:2625-2631` | Streams survive client disconnect; replay buffer + live subscribe; `/api/chat/resume/{sid}`, `/api/chat/stop/{sid}`, `/api/chat/stream_status/{sid}`; `X-Odysseus-Run-Id` header. **Compare mode is deliberately NOT detached** | in-memory + DB | High | High | `odysseus-services/src/agent_runs.rs` | 4 | UNIMPLEMENTED |
| F044 | Per-session stream state | `chat.js:622-630` | 6 parallel Maps (`_backgroundStreams`, `_activeStreams`, `_streamRunIds`, `_streamGenerations`, `_sendStates`, `_pendingRunStops`) + a generation counter so a superseded preflight send bails **before** the POST | in-memory | Med | High | `odysseus-ui` signals | 4 | UNIMPLEMENTED |
| F045 | Agent-mode auto-escalation | `chat.js:1889-1901`, `src/action_intents.py` | ~200-word regex over the user message flips chat→agent; an open document forces agent mode | n/a | Low | Med | `odysseus-agent/src/intents.rs` | 4 | UNIMPLEMENTED |
| F046 | Group chat | `group.js` (1,250 L) | N participant sessions; fans out via `/api/session/{id}/inject_messages` | `chat_messages` | Med | Med | `odysseus-ui/src/components/group.rs` | 22 | UNIMPLEMENTED |
| F047 | Message editing & forking | `history_routes.py:346,596` | Edit cascades; fork = copy-on-write branch | `chat_messages` | Med | Med | `odysseus-services/src/history.rs` | 4 | UNIMPLEMENTED |
| F048 | Message history rendering | `chatRenderer.js:2561` `addMessage` | Reconstructs multi-round agent transcripts from `metadata.tool_events[]` grouped by `ev.round`, interleaved with `round_texts[]`/`round_models[]`/`round_endpoint_ids[]`, plus `pendingAskUser`. **Metadata is a first-class transcript format** | `chat_messages.metadata` | High | High | `odysseus-ui/src/components/chat/transcript.rs` | 4 | UNIMPLEMENTED |
| F049 | Streaming renderer | `streamingRenderer.js` (206 L) | Frozen blocks rendered once; live tail re-renders per token except an open fence which uses **append-mode** (`Text.appendData`, no re-parse); `token-new` fade spans; self-heals if `innerHTML` is clobbered; **degradation latch** permanently falls back to full re-render on throw | n/a | Med | Med | `odysseus-ui/src/components/chat/stream_renderer.rs` | 4 | UNIMPLEMENTED |
| F050 | Streaming segmenter | `streamingSegmenter.js` | Pure split/freeze logic, fuzz-tested token-by-token | n/a | Low | Low | `odysseus-ui/src/components/chat/segmenter.rs` | 4 | UNIMPLEMENTED |
| F051 | Live thinking throttle | `liveThinkingThrottle.js` | DI-first trailing-edge coalescer; one DOM commit per 100 ms for reasoning text | n/a | Low | Low | `odysseus-ui` | 4 | UNIMPLEMENTED |
| F052 | Slash commands | `slashCommands.js` (6,520 L) | Declarative registry: 47 top-level commands, 2 levels of sub-commands, per-command `alias[]/category/help/usage/default/hidden/noUserBubble/handler`; `LEGACY_ALIASES` maps ~35 flat aliases (incl. `rm/mv/cp/cat`); stateful setup wizard; skills contribute **dynamic** commands | `data/skills/` | High | Med | `odysseus-ui/src/components/slash.rs` | 14 | UNIMPLEMENTED |
| F053 | Slash autocomplete | `slashAutocomplete.js` | Popup keyboard navigation | n/a | Low | Low | `odysseus-ui` | 14 | UNIMPLEMENTED |
| F054 | Composer ↑-recall | `composerArrowUpRecall.js` | Recalls the last user message on an empty composer | n/a | Low | Low | `odysseus-ui` | 4 | UNIMPLEMENTED |
| F055 | Message metrics display | `chatRenderer.js` `displayMetrics` | Token counts, TPS, context %, cost | `chat_messages.metadata` | Med | Low | `odysseus-ui` | 4 | UNIMPLEMENTED |
| F056 | Model provenance | `chatModelProvenance.js` | requested-vs-actual model tracking; `model_actual` event | n/a | Low | Med | `odysseus-ui` | 4 | UNIMPLEMENTED |
| F057 | Live TTS during stream | `tts-ai.js` (700 L) | Sentence-by-sentence synthesis during a live stream | `data/tts_cache/` | Med | Med | `odysseus-ui` + `odysseus-services/src/tts.rs` | 22 | UNIMPLEMENTED |
| F058 | Lightweight message rewrite | `routes/chat_routes.py:2719` | LLM rewrite of the last AI message; **no tools, no agent loop**; JSON in → SSE out | `chat_messages` | Low | Low | `odysseus-services/src/chat.rs` | 4 | UNIMPLEMENTED |

---

## D. Agent, tools & approvals

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F060 | Agent loop core | `src/agent_loop.py:3420` (6,455 L file) | One ~3,000-line async generator with ~30 nested closures. Phases: security/policy assembly → turn classification → tool selection → per-route request state → approval replay → round loop → finalization | `chat_messages` | **Very high** | **Critical** | `odysseus-agent/src/loop.rs` | 4 | UNIMPLEMENTED |
| F061 | Round loop | `agent_loop.py:4773-6311` | Budget check → tool-policy gate → security-context gate (creates a sealed approval if not allowed) → `tool_start` → detached execution with a progress queue → result post-processing → feed-back | n/a | High | **Critical** | `odysseus-agent/src/loop.rs` | 5 | UNIMPLEMENTED |
| F062 | Tool-call resolution | `agent_loop.py:2944-2992,5246` | Native function calls win; text parsing runs **only** when `used_native` is False; `converted_calls` is 1:1-aligned with `tool_blocks` (index-mis-attachment regression) | n/a | Med | **Critical** | `odysseus-agent/src/resolve.rs` | 5 | UNIMPLEMENTED |
| F063 | Tool result threading | `agent_loop.py:2995-3120` | Native: assistant msg with `content = round_response or None` (**`None`, not `""`** — Gemini/Ollama 400), `reasoning_content` only on the most recent assistant turn (Nemotron re-injects all prior reasoning as `<think>`), Gemini3 `thought_signature`. Text: optional assistant msg (skipped if empty — Anthropic 400s a non-final empty assistant message) + a single untrusted wrapper | n/a | High | **Critical** | `odysseus-agent/src/threading.rs` | 5 | UNIMPLEMENTED |
| F064 | Native function schemas | `src/tool_schemas.py` — **71** schemas | Sent as `tools`. `generate_image` deliberately absent (XML/fence-only); `grep`/`glob`/`ls`/`get_workspace`/`manage_notes`/`vault_*`/`resolve_contact` inconsistent across surfaces | n/a | High | Med | `odysseus-tools/src/schemas.rs` | 5 | UNIMPLEMENTED |
| F065 | Fenced-tag tool surface | `src/agent_tools/__init__.py:79` — ~65 tags | Fenced code-block language tags + XML-invocable names | n/a | Med | Med | `odysseus-tools/src/tags.rs` | 5 | UNIMPLEMENTED |
| F066 | Class-based handlers | `src/agent_tools/__init__.py:37` — ~29 handlers | `TOOL_HANDLERS` registry of `XTool().execute` | n/a | Med | Med | `odysseus-tools/src/handlers.rs` | 5 | UNIMPLEMENTED |
| F067 | Tool-call text parser | `src/tool_parsing.py` (1,525 L) | 8 ordered patterns, first-match-wins: DSML→`<invoke>`, fenced, `[TOOL_CALL]`, `<invoke>`/StepFun/`<function_model>`/Gemma, `<tool_code>` (MiniMax-M2.5), raw leaked OpenAI JSON, prose+bare JSON, plain `ui_control` line | n/a | High | Med | `odysseus-tools/src/parsing.rs` | 5 | UNIMPLEMENTED |
| F068 | ReDoS hardening | `tool_parsing.py:1178` `_iter_delimited`, `agent_loop.py:1253` `_strip_think_blocks` | Forward-only scans pairing openers with reachable closers, replacing `re.finditer`'s O(n²) lazy rescan. Dedicated regression tests exist | n/a | Med | High | `odysseus-tools/src/scan.rs` + `regex` crate (no backtracking) | 5 | UNIMPLEMENTED |
| F069 | `strip_tool_blocks` | `tool_parsing.py:1483` | Mirrors `_fenced_tool_call` decisions so persisted history matches what was executed | n/a | Med | Med | `odysseus-tools/src/parsing.rs` | 5 | UNIMPLEMENTED |
| F070 | Tool-name alias table | `tool_parsing.py:232` | ~140 entries (`shell|terminal|command|run → bash`, `cat → read_file`, `teacher → ask_teacher`, …) | n/a | Low | Low | `odysseus-tools/src/aliases.rs` | 5 | UNIMPLEMENTED |
| F071 | Native→text bridge | `tool_parsing.py:1370` `function_call_to_tool_block` | Flattens JSON args back into each tool's legacy text format (`bash`→`command`, `create_document`→`title\nlanguage\ncontent`, `edit_document`→`<<<FIND>>>` blocks, `web_search`→JSON to preserve the freshness filter). Rejects non-object args for email tools | n/a | Med | Med | `odysseus-tools/src/bridge.rs` | 5 | UNIMPLEMENTED |
| F072 | Execution envelope | `tool_execution.py:810,828-841,965` | **Refuses to run without an explicit `security_context`** (`TypeError` on missing/wrong sentinel). Re-validates the exact approval, re-checks `vet_workspace(sealed) == sealed`, binds `_active_workspace` via `contextvars` | n/a | High | **Critical** | `odysseus-tools/src/envelope.rs` | 5 | UNIMPLEMENTED |
| F073 | Path confinement | `tool_execution.py:357,406,466,494` | sensitive-subpath denylist (`.ssh`, `.gnupg`, `id_rsa`, …) → app-state denylist → hardlink check → allowlist containment via `commonpath`; `vet_workspace` refuses filesystem roots (a root workspace would make every absolute path "inside") | n/a | High | **Critical** | `odysseus-tools/src/paths.rs` | 5 | UNIMPLEMENTED |
| F074 | Effect classification | `tool_capabilities.py` (708 L) | 15 `ToolEffect` classes from `READ_PUBLIC` to `USER_INTERACTION`; declarative `_register` registry (`RuntimeError` on duplicate); `capabilities_for_action` parses the action discriminator from any accepted input shape. **Ambiguous actions fail high** — unknown action on a multiplexed tool returns both read and write private | n/a | High | **Critical** | `odysseus-tools/src/capabilities.rs` | 6 | UNIMPLEMENTED |
| F075 | Result integrity | `tool_capabilities.py:37,506` | `SYSTEM` / `WORKSPACE_UNTRUSTED` / `EXTERNAL_UNTRUSTED`. **Failure status does not make content trusted** — any non-`SYSTEM` result carrying producer content arms the gate | n/a | Med | **Critical** | `odysseus-tools/src/integrity.rs` | 6 | UNIMPLEMENTED |
| F076 | `ToolRunSecurityContext` | `tool_capabilities.py:615-708` | Server-owned per-run integrity state. `decision_for`: delegated credentials block privileged tools **first and unconditionally** (no approval can lift it); then approval-gate bypass; then external-untrusted-context blocks `POST_EXTERNAL_BLOCKED_EFFECTS` or unknown tools | n/a | High | **Critical** | `odysseus-tools/src/run_context.rs` | 6 | UNIMPLEMENTED |
| F077 | Sealed tool approvals | `tool_approvals.py` (513 L) | Digest over a 14-field binding payload; `.matches()` recomputes; `.claim()` is one-shot under a lock; TTL 10 min; max 2048 pending; one card per (owner, session); `consume()` verifies ownership **before** destructive pop; `retire_for_session()` returns whether the card carried external provenance so the new turn inherits the taint but not the grant | in-memory | High | **Critical** | `odysseus-agent/src/approvals.rs` | 6 | UNIMPLEMENTED |
| F078 | Approval UI cards | `chatRenderer.js` `renderAskUserCard`, `tool_approvals.py:167` | Browser-display-only payload: full sealed content, 16-char digest, effects, workspace, document version, three options (allow-once / allow-session / deny) | n/a | Med | **Critical** | `odysseus-ui/src/components/chat/approval_card.rs` | 6 | UNIMPLEMENTED |
| F079 | Document-content digest binding | `tool_approvals.py:94` | sha256 of the document body; a document mutated between card and click invalidates the approval | n/a | Low | **Critical** | `odysseus-agent/src/approvals.rs` | 6 | UNIMPLEMENTED |
| F080 | Per-turn tool policy | `tool_policy.py` (242 L) | Frozen `ToolPolicy {disabled_tools, hidden_tools, reasons, mode, block_all_tool_calls, disable_mcp}`; `detect_guide_only_turn` (7 regexes: "no-tools mode", "ask before using tools") disables **and** hides every known name; `email_tool_policy_names` returns **both** spellings so a denylist written in one cannot be bypassed by the other | n/a | Med | **Critical** | `odysseus-tools/src/policy.rs` | 6 | UNIMPLEMENTED |
| F081 | Plan mode | `agent_loop.py:4330-4337`, `tool_security.py:91-169` | Allowlist inverted into a denylist, unioned with the static mutator backstop so a failed schema import still blocks writes. `bash`/`python` deliberately **not** in the read-only allowlist | n/a | Med | High | `odysseus-agent/src/plan_mode.rs` | 6 | UNIMPLEMENTED |
| F082 | Non-admin / delegated tool blocks | `tool_security.py:42,267-280` | `NON_ADMIN_BLOCKED_TOOLS`; non-admins get MCP dropped entirely (dynamic namespaces cannot be enumerated safely); `owner_is_admin_or_single_user` treats the pre-setup window as non-admin | n/a | Med | **Critical** | `odysseus-tools/src/policy.rs` | 6 | UNIMPLEMENTED |
| F083 | Tool selection funnel | `agent_loop.py:3875-4070` | 4 layers: (1) RAG retrieval via `tool_index` with a **1.5 s timeout** — on timeout `_relevant_tools` is deliberately left `None` so the keyword fallback still runs; (2) MCP re-index, also timeout-guarded; (3) keyword fallback (`ALWAYS_AVAILABLE` = only `manage_memory`, `ask_user`, `update_plan`); (4) deterministic domain seeding | n/a | High | Med | `odysseus-agent/src/selection.rs` | 5 | UNIMPLEMENTED |
| F084 | Mid-loop skill toolset unlock | `agent_loop.py:5841-5878` | After `manage_skills action=view`, that skill's `requires_toolsets` are added for the **next** round so the model has a schema for the `grep` the skill told it to use | n/a | Low | Med | `odysseus-agent/src/selection.rs` | 14 | UNIMPLEMENTED |
| F085 | Loop breaker & stall detector | `agent_loop.py:5556-5621` | Signature = sorted `tool:content[:120]`; repeat-without-real-text → `_stuck_rounds++` (bail at 4); identical `(tool,args)` ≥15 times → runaway; on trip emits `loop_breaker_triggered`, sets `_force_answer`, appends a system directive | n/a | Med | Med | `odysseus-agent/src/breaker.rs` | 4 | UNIMPLEMENTED |

**Loop-breaker constants preserved:** `_MAX_INTENT_NUDGES = 2`, `_stuck_rounds` bail at 4,
runaway threshold 15, intent-match length < 400 chars, auto-document creation at ≥30
lines of code fence, completion verifier max 2 rounds.

---

## E. Models, endpoints & providers

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F090 | Model endpoint CRUD | `routes/model_routes.py:1932-2010,2495-2666` | `model_endpoints` table; admin-gated; dependent-row check before delete | `model_endpoints` | Med | Med | `odysseus-services/src/endpoints.rs` | 2 | UNIMPLEMENTED |
| F091 | Provider detection | `src/llm_core.py:966-1130` | Hostname-matched (not substring, so `anthropic.com.example` can't misclassify): ollama, anthropic, opencode-go/zen, openrouter, groq, nvidia, moonshot, chatgpt-subscription, copilot, cerebras, mistral; **openai is the default fallback** | n/a | Med | Med | `odysseus-llm/src/detect.rs` | 3 | UNIMPLEMENTED |
| F092 | Provider request shaping | `llm_core.py:1027-1620` | Anthropic (temp clamp [0,1], omit temp for Opus 4.7+, prompt caching with `cache_control` on the last tool schema, `Bearer`→`x-api-key`); Ollama native (`num_ctx` from `get_context_length`); ChatGPT Responses; OpenAI-compatible (`stream_options.include_usage` except OpenRouter/Groq; `max_completion_tokens` for o1/o3/o4/gpt-4.5/gpt-5; `think:false` for Ollama thinking models so tool calls aren't swallowed) | n/a | High | **High** | `odysseus-llm/src/shape.rs` | 3 | UNIMPLEMENTED |
| F093 | Local cache affinity | `llm_core.py:1004,1027` | Adds `session_id` + `cache_prompt:true` **only** for self-hosted OpenAI-compatible endpoints. Prevents llama.cpp LRU slot thrash (#2927); avoids 400/422 on strict clouds (#3793) | n/a | Low | High | `odysseus-llm/src/shape.rs` | 3 | UNIMPLEMENTED |
| F094 | Local generation stability | `llm_core.py:1070` | MLX MiniMax: `temperature ≤ 0.2`, `top_p 0.9`, `top_k 20`, `repetition_penalty 1.12`, `stop:["","","</s>"]`, `max_tokens 2048` | n/a | Low | Med | `odysseus-llm/src/shape.rs` | 3 | UNIMPLEMENTED |
| F095 | Four stream parsers | `llm_core.py:2582-3300` | ChatGPT Responses (`event:`-named SSE), Ollama NDJSON (line-delimited, **not** SSE), Anthropic block-stream (`content_block_*`, `input_json_delta`, cache token accounting), OpenAI-compatible (accumulates `delta.tool_calls[]`; handles Gemini's **omitted** `index` on parallel calls by allocating a fresh slot on each `function.name`, else all parallel calls collapse into slot 0) | n/a | High | **Critical** | `odysseus-llm/src/stream/{chatgpt,ollama,anthropic,openai}.rs` | 4 | UNIMPLEMENTED |
| F096 | Fallback chain router | `llm_core.py:3511-3678` | Ordered candidates; `candidate_request_factory` may be sync or async; **metadata commit rule** buffers `model_actual`/`usage`/`model_info` until first substantive output; `fallback` notice event (without it a broken provider is invisible because the reply shows under the selected model's name); eligibility gated by status set `{408,425,429,500,502,503,504,507,508,529}` **and** an explicit `fallback_eligible` flag (WriteTimeout/ProtocolError/NetworkError set it False) | n/a | High | **Critical** | `odysseus-llm/src/fallback.rs` | 4 | UNIMPLEMENTED |
| F097 | Host health & cooldown | `llm_core.py:483-497` | `DEAD_HOST_COOLDOWN = 20.0`, `_HOST_FAIL_THRESHOLD = 2`; any success resets; guarded by a lock because sync `llm_call` runs in the FastAPI threadpool | n/a | Low | Med | `odysseus-llm/src/health.rs` | 3 | UNIMPLEMENTED |
| F098 | LLM response cache | `llm_core.py:177,196` | sha256 of `{url, model, messages, temp, max_tokens, header_identity}`. `header_identity` hashes **credential identity** so one account's response can't be served under another route with the same URL+model — without ever putting secrets in the key | in-memory | Med | Med | `odysseus-llm/src/cache.rs` | 3 | UNIMPLEMENTED |
| F099 | Foreground/background model slot | `llm_core.py:82` `_local_model_slot` | A module-global `asyncio.Lock` serialising **local** endpoints; foreground requests cancel an in-flight background local call; background polls at 250 ms while foreground waits or `interactive_gate.has_foreground_activity()`. Toggle `ODYSSEUS_LOCAL_MODEL_GATE`. Fixes "streams crossed" | n/a | Med | High | `odysseus-llm/src/slot.rs` | 4 | UNIMPLEMENTED |
| F100 | Local-model priority gate | `src/foreground_model_routing.py` | Builds the ordered foreground fallback candidate chain | n/a | Med | Med | `odysseus-llm/src/routing.rs` | 3 | UNIMPLEMENTED |
| F101 | Endpoint resolution & normalisation | `src/endpoint_resolver.py` (672 L) | Consolidates 4+ duplicate copies of `normalize_base`/`resolve_endpoint`. `_first_chat_model` skips non-chat models (`text-embedding`, `tts-`, `whisper`, `dall-e`, `moderation`, `rerank`, `clip`, `stable-diffusion`) because an OpenAI-style endpoint often lists `text-embedding-ada-002` first | `model_endpoints`, `settings.json` | High | Med | `odysseus-llm/src/resolve.rs` | 3 | UNIMPLEMENTED |
| F102 | Model discovery | `src/model_discovery.py`, `routes/model_routes.py:1618-1934` | Ollama / LM Studio / llama.cpp / vLLAM detection; `/api/discover` host port-scan; `/api/ping`; 30 s provider cache | n/a | Med | Med | `odysseus-services/src/discovery.rs` | 2 | UNIMPLEMENTED |
| F103 | Model capability detection | `src/model_capabilities.py` (934 L), `src/model_capability_readers/` | Vision/tool support detection; LM Studio `/api/v1/models` capability probe with 60 s TTL | n/a | Med | Med | `odysseus-llm/src/capabilities.rs` | 2 | UNIMPLEMENTED |
| F104 | GitHub Copilot OAuth | `src/copilot.py` (256 L), `routes/copilot_routes.py` | Device flow using the public VS Code client id (Copilot's token endpoint only accepts GitHub allow-listed ids); `COPILOT_API_VERSION`, `Openai-Intent`, `x-initiator`. **No token exchange and no refresh** — the device-flow token is the bearer | `provider_auth_sessions` | Med | Med | `odysseus-integrations/src/copilot.rs` | 2 | UNIMPLEMENTED |
| F105 | ChatGPT subscription / Codex | `src/chatgpt_subscription.py` (315 L), `routes/chatgpt_subscription_routes.py` | Device flow + PKCE + refresh (120 s skew); `ReauthRequired`/`RateLimited`/`AuthNotFound`; JWT expiry decode without verification; per-`auth_id` lock with double-check after reload | `provider_auth_sessions` | High | Med | `odysseus-integrations/src/chatgpt.rs` | 2 | UNIMPLEMENTED |

---

## F. Context, memory, RAG & embeddings

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F110 | Context preface assembly | `src/chat_processor.py:263-525` | Order: preset system prompt → untrusted-context policy → pinned memories (≤5 core identity/contact) → hybrid-retrieved memory → RAG personal docs → web search (LLM extracts a concise query first, temp 0.1, 50 tokens) → auto-fetch of non-YouTube URLs (skipped >2000 chars or >3 URLs) → skills index. Everything untrusted becomes a `user`-role message with `metadata.trusted=False`, **never `system`** | n/a | High | **High** | `odysseus-agent/src/preface.rs` | 7 | UNIMPLEMENTED |
| F111 | KV-cache prefix discipline | `chat_processor.py:283-295`, `routes/chat_helpers.py:744-757` | Nothing turn-varying may be folded into `system`; the current date/time is injected as a `user` message immediately before the last user turn. Same discipline in `task_scheduler.py:1590-1601` | n/a | Low | **High** | `odysseus-agent/src/preface.rs` | 7 | UNIMPLEMENTED |
| F112 | Token budget computation | `src/context_budget.py`, `settings.py` | `agent_input_token_budget = 6000` (auto sentinel), `agent_input_token_hard_max = 200_000`, `max_context_messages = 90`; per-route trimming via `trim_for_context` | n/a | Med | Med | `odysseus-agent/src/budget.rs` | 7 | UNIMPLEMENTED |
| F113 | Context compaction | `src/context_compactor.py` (21 KB) | Speculative compaction **per fallback route**; the winner's state is applied afterwards so a route that cannot answer never rewrites history | `chat_messages` | High | High | `odysseus-agent/src/compaction.rs` | 7 | UNIMPLEMENTED |
| F114 | Message sanitisation | `llm_core.py:1673` `_sanitize_llm_messages` | Drops orphan `role:"tool"`, prunes unanswered `tool_calls`, merges consecutive user messages (with a synthetic assistant "Reference context received." when the first was untrusted), preserves multimodal block lists | n/a | Med | High | `odysseus-llm/src/sanitize.rs` | 3 | UNIMPLEMENTED |
| F115 | System-message consolidation | `llm_core.py:1987,2600` | All `system` messages joined into one at index 0, because some models (e.g. Qwen3.5) reject system messages that aren't first | n/a | Low | Med | `odysseus-llm/src/sanitize.rs` | 3 | UNIMPLEMENTED |
| F116 | Memory CRUD | `routes/memory/memory_routes.py` | 13 endpoints; categories `fact, identity, preference, contact, project, goal, task`; pin/unpin; timeline; per-session view | `memories` | Med | Med | `odysseus-services/src/memory.rs` | 7 | UNIMPLEMENTED |
| F117 | Hybrid memory retrieval | `src/chat_processor.py:158` `_hybrid_retrieve` | BM25-style IDF over the memory corpus **plus** optional vector search, mixed `0.55*vector + 0.40*keyword + 0.05*recency` (recency explicitly a tiebreaker only); category boosts identity 1.4×, contact 1.3×, preference 1.2×; gates `vs<0.20 and kw<0.08 → skip` | `memories` | High | **High** | `odysseus-services/src/memory.rs` | 7 | UNIMPLEMENTED |
| F118 | **Vector threshold re-tuning** | ChromaDB → sqlite-vec | The mix weights, boosts and gates above were calibrated against **ChromaDB's distance metric**. sqlite-vec uses a different metric; unre-tuned values cause a silent retrieval-quality regression. **Gated task with golden-set validation, never hand-tuned** | vectors | High | **High** | `odysseus-store/src/vectors.rs` | 0 spike S11 / 7 | UNIMPLEMENTED |
| F119 | Memory extraction | `services/memory/memory_extractor.py` (678 L) | LLM fact extraction from chat; `_fingerprint_entries` builds an order-independent sha256 over `(id, text, category)` into `memory_tidy_state.json` so the audit short-circuits when nothing changed (saves 30–120 s/call) | `memories`, `memory_tidy_state.json` | Med | Med | `odysseus-services/src/memory_extract.rs` | 7 | UNIMPLEMENTED |
| F120 | Memory audit / tidy | `routes/memory/memory_routes.py:288`, `consolidate_memory` | Tidy/audit with its own 120 s timeout | `memories` | Med | Med | `odysseus-services/src/memory.rs` | 7 | UNIMPLEMENTED |
| F121 | Memory import | `routes/memory/memory_routes.py:339` | Upload `.txt/.md/.pdf/.csv/.log/.json/.py/.js/.html`, 10 MB; `.json` fast-path round-trips without an LLM call | `memories` | Med | Low | `odysseus-services/src/memory.rs` | 7 | UNIMPLEMENTED |
| F122 | Personal-document RAG | `routes/personal_routes.py`, `services/docs/`, `src/rag_vector.py` | Add/remove indexed directories, re-extract, upload (25 MB), per-file delete. Paths confined via `realpath` + `commonpath`. Serialised by a per-router lock. `similarity >= 0.35`, 10,000-char cap | `data/chroma` → sqlite-vec | High | Med | `odysseus-services/src/personal_docs.rs` | 7 | UNIMPLEMENTED |
| F123 | Document chunking | `src/personal_docs.py:71,92` | `CHUNK_SIZE = 1000`, `CHUNK_OVERLAP = 200`; tokenizer; keyword + semantic fusion | vectors | Med | Low | `odysseus-store/src/chunk.rs` | 7 | UNIMPLEMENTED |
| F124 | Local embeddings | `src/embeddings.py`, `src/embedding_lanes.py` | `fastembed` (Python ONNX) → Rust. Multiple embedding **lanes** with a priority merge in `retrieve`; per-lane collections for rag/memory/tool_index | vectors | Med | **High** | `odysseus-store/src/embed.rs` | 0 spike S7 / 7 | UNIMPLEMENTED |
| F125 | Chat transcript full-text search | `core/database.py:2178-2261`, `src/session_search.py` | FTS5 virtual table + 3 triggers; triggers replace base64/`data:image/`/`data:audio/` content with `'[inline media omitted from search index]'`; LIKE fallback; `_sanitize_fts_query` | `chat_messages_fts` | Med | Med | `odysseus-store/src/search.rs` | 1 | UNIMPLEMENTED |

---

## G. Documents, editor & PDF

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F130 | Document CRUD | `routes/document/document_routes.py` | Create/list/get/update/patch/soft-delete/archive; per-session listing; owner-scoped | `documents` | Med | Med | `odysseus-services/src/documents.rs` | 8 | UNIMPLEMENTED |
| F131 | Document versioning | `document_routes.py:765-851` | Version list/get/restore; every update creates a `document_versions` row | `document_versions` | Med | Low | `odysseus-services/src/documents.rs` | 8 | UNIMPLEMENTED |
| F132 | Tabbed editor | `document.js` (11,207 L file) `renderTabs:283` | Drag reorder, per-doc state, inline title edit | `documents` | High | Med | `odysseus-ui/src/components/document/editor.rs` | 8 | UNIMPLEMENTED |
| F133 | Markdown/HTML/CSV syntax highlighting | `static/lib/highlight.min.js`, `theme.js` syntax vars | 11 `--hl-*` CSS vars derived from theme HSL | n/a | Med | Low | Tailwind + shadcn code block | 8 | UNIMPLEMENTED |
| F134 | Markdown renderer | `markdown.js` (1,229 L) | **Hand-rolled ordered regex pipeline** with placeholder substitution (no `marked`/`markdown-it`): fenced code → inline code (before link/autolink) → agent-mangled entity-anchor repair → images → links → autolinks → tables/lists/headings → restore placeholders | n/a | High | **High** | `odysseus-ui/src/components/markdown.rs` + `pulldown-cmark` | 8 | UNIMPLEMENTED |
| F135 | HTML sanitisation | `markdown.js:257` `sanitizeAllowedHtml` | Uses inert `<template>` parsing — **the security boundary** for rendered content | n/a | Med | **Critical** | `odysseus-ui` + `ammonia` crate | 8 | UNIMPLEMENTED |
| F136 | Agent deep-link anchors | `markdown.js:664-694` | Three regexes upgrade `[Name] [#kind-id]`, bare `[#kind-id]`, and bare `#kind-id` into markdown links. `ANCHOR_KIND = (session\|document\|note\|image\|email\|event\|task\|skill\|research)`. **This is the deep-link contract** | n/a | Med | Med | `odysseus-ui/src/components/markdown.rs` | 8 | UNIMPLEMENTED |
| F137 | Thinking-block rendering | `markdown.js:292-581` | `normalizeThinkingMarkup`, `extractThinkingBlocks`, `hasUnclosedThinkTag`, `startsWithReasoningPrefix`, `processWithThinking` | n/a | Med | Med | `odysseus-ui` | 8 | UNIMPLEMENTED |
| F138 | Code-block action buttons | `markdown.js` | `.run-code` (only `python\|py\|javascript\|js\|html\|bash\|sh\|shell\|zsh`), `.edit-code`, `.copy-code` | n/a | Low | Med | `odysseus-ui` | 8 | UNIMPLEMENTED |
| F139 | Document ↔ chat streaming bridge | `document.js:10309,10485`, `chat.js:3770-3793` | `streamDocOpen(title, language)` / `streamDocDelta(content)` driven by `doc_stream_open`/`doc_stream_delta`. **Ordering matters** — doc-stream events fire before `doc_update`, else the editor enters diff mode (`agent_loop.py:5906`) | n/a | Med | High | `odysseus-ui` | 8 | UNIMPLEMENTED |
| F140 | AI edit / suggest blocks | `document_tools.py:256,264` | `<<<FIND>>>/<<<REPLACE>>>/<<<END>>>` and `<<<SUGGEST>>>` + `<<<REASON>>>` | `documents` | Med | High | `odysseus-agent` + `odysseus-ui` | 8 | UNIMPLEMENTED |
| F141 | Optimistic-concurrency guard | `document_tools.py:84` `_approved_document_version_error` | A document mutated between approval card and click invalidates the approval | `documents` | Med | **Critical** | `odysseus-agent` | 8 | UNIMPLEMENTED |
| F142 | Document library modal | `documentLibrary.js` (3,422 L) | Chats/Documents/Research/Archive tabs; `topPortalZ` for dropdown layering | various | Med | Low | `odysseus-ui` | 8 | UNIMPLEMENTED |
| F143 | Document export ZIP | `document_routes.py:559` | `StreamingResponse application/zip`, `documents.zip` | files | Low | Low | `odysseus-web` | 8 | UNIMPLEMENTED |
| F144 | Document tidy / AI tidy | `routes/document/document_routes.py:852,967`, `src/document_actions.py` | Conservative junk/dedup; `_content_fingerprint` strips volatile `upload_id="…"` and `id=ann-…` so N imports collapse. **Deliberately no length-based deletion** (short notes are valid) | `documents` | Med | Low | `odysseus-services/src/document_tidy.rs` | 8 | UNIMPLEMENTED |
| F145 | PDF import → form fields | `document_routes.py:221` | Upload PDF, extract AcroForm fields into an editable doc | `documents` | Med | Med | `odysseus-services/src/pdf.rs` | 8 | UNIMPLEMENTED |
| F146 | PDF page rasterisation | `document_routes.py:1197` | PyMuPDF → `image/png`, `_PDF_RENDER_SCALE`, `Cache-Control: public, max-age=3600` | n/a | Med | Med | `mupdf`/`poppler-rs` FFI — **introduces a C dependency** | 0 spike S10 / 8 | UNIMPLEMENTED |
| F147 | PDF annotation pane | `document.js:1149,1390,1781` | ~900 lines; markdown-embedded annotations via `{{annot:...}}`; `_aiFillAnnotations` → `POST /ai-fill-annotations`; undo snapshots | `documents` | High | Med | `odysseus-ui` + `odysseus-services` | 8 | UNIMPLEMENTED |
| F148 | AcroForm fill + PDF export | `document_routes.py:1378,1478` | Fills AcroForm fields from markdown + stamps signature/annotation annotations; signature rows **owner-filtered**; temp files unlinked via `BackgroundTask`; CSP `default-src 'none'; frame-ancestors 'self'` | `signatures` | High | Med | `lopdf` crate | 0 spike S10 / 8 | UNIMPLEMENTED |

---

## H. Uploads, vision & gallery

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F150 | Upload handler | `src/upload_handler.py` (58 KB) | Saves, dedupes, limits, cleans; content-sniffing by magic bytes; extension/ID validation | `data/uploads/` | High | Med | `odysseus-services/src/uploads.rs` | 9 | UNIMPLEMENTED |
| F151 | Upload route + limits | `routes/upload_routes.py:257` | Per-IP concurrent limit (429), per-minute sliding window; auto-promotes chat images to `GalleryImage`; returns `gallery_id`. **Config and handler disagree on the rate limit (5/min vs 60/min)** — an existing inconsistency | `data/uploads/` | Med | Med | `odysseus-web/src/routes/uploads.rs` | 9 | UNIMPLEMENTED |
| F152 | Upload download + thumb | `upload_routes.py:353` | `?thumb=1` → cached 320px JPEG, EXIF-transposed; `X-Content-Type-Options: nosniff`; owner mismatch → **404, not 403** (does not confirm existence) | files | Med | High | `odysseus-web` | 9 | UNIMPLEMENTED |
| F153 | Upload cleanup | `upload_routes.py:320` | Reference-safe; **503 if index integrity is unverified**; admin-only | `data/uploads/` | Med | Med | `odysseus-services` | 9 | UNIMPLEMENTED |
| F154 | Vision OCR/description | `upload_routes.py:451,496` | Cached in `data/uploads/.vision/{id}.txt`; `?force=1`; user-editable OCR overrides the model's caption and syncs the gallery caption; non-image → 400 | files | Med | Med | `odysseus-services/src/vision.rs` | 9 | UNIMPLEMENTED |
| F155 | Multimodal content assembly | `src/document_processor.py:395-607` `build_user_content` | Per attachment: resolve → **path confinement** (`_inside_upload_dir`/`inside_base_dir`) → image → base64 `image_url` data URI (extension fallback from MIME subtype) → audio → else text-inlined within a shared `MAX_INLINE_ATTACHMENT_CHARS = 24000` budget | n/a | High | **Critical** | `odysseus-agent/src/multimodal.rs` | 9 | UNIMPLEMENTED |
| F156 | VL fallback for non-vision models | `src/chat_handler.py:233-275` | User-editable `.vision/<id>.txt` cache **overrides** the model's own caption | files | Med | Med | `odysseus-services` | 9 | UNIMPLEMENTED |
| F157 | Attachment reference reservation | `src/attachment_refs.py`, `session_manager.py:257-266` | `reserve_message_upload_references` raises `ValueError` if a referenced upload is gone. Persisted content collapses multimodal blocks to `[Attachment: name | id=… | mime=… | size=… | sha256=…]`, replacing base64 with `[inline media omitted from persisted history]` — keeps media bytes out of `chat_messages.content` **and** the FTS index | `chat_messages` | Med | High | `odysseus-agent` | 9 | UNIMPLEMENTED |
| F158 | Gallery CRUD | `routes/gallery/gallery_routes.py:352-1136` | Upload (SHA-256 dedupe **scoped to owner**, 100 MB cap, image+video allowlist, EXIF extraction), replace, rename, rotate, patch, delete, favorite, tags, stats, library | `gallery_images`, `gallery_albums` | High | Med | `odysseus-services/src/gallery.rs` | 17 | UNIMPLEMENTED |
| F159 | Gallery albums | `gallery_routes.py:812-870,2140-2200` | CRUD + add/remove images | `gallery_albums` | Med | Low | `odysseus-services` | 17 | UNIMPLEMENTED |
| F160 | Gallery ZIP download | `gallery_routes.py:993` | `gallery-photos.zip` | files | Low | Low | `odysseus-web` | 17 | UNIMPLEMENTED |
| F161 | Gallery AI tagging | `gallery_routes.py:897,2230,1048-1100` | `ai-tag-batch`, per-image `ai-tag`, clear user/AI tags, dedupe tags | `gallery_images.ai_tags` | Med | Low | `odysseus-services` | 17 | UNIMPLEMENTED |
| F162 | Generated-image serving | `app.py:513-553` | Content-hash filenames → immutable cache; **owner verified via the gallery row**; generated-but-not-yet-imported images have no row → allowed; row with a different owner → 404 | files | Low | High | `odysseus-web` | 17 | UNIMPLEMENTED |
| F163 | **Image inpaint** | `gallery_routes.py:1257` | Proxy to a registered `ModelEndpoint`; reshapes for OpenAI `/v1/images/edits` (multipart, inverted mask) vs self-hosted `/v1/images/inpaint`. **SSRF-hardened** and the base must resolve to a registered endpoint visible to the user (**admins not exempt**) | n/a | Med | High | `odysseus-services/src/image_proxy.rs` | 17 | UNIMPLEMENTED |
| F164 | **Image harmonize** | `gallery_routes.py:1515` | Same proxy pattern | n/a | Med | High | `odysseus-services/src/image_proxy.rs` | 17 | UNIMPLEMENTED |
| F165 | **Image sharpen** | `gallery_routes.py:1715` | Local PIL `UnsharpMask` | n/a | Low | Low | `image` crate | 17 | UNIMPLEMENTED |
| F166 | Image denoise (Real-ESRGAN) | `gallery_routes.py:1740` | Local `realesr-general-x4v3`, outscale=1 | n/a | — | — | — | 17 | **REMOVED** — see INCOMPLETE.md; no maintained Rust path |
| F167 | Image upscale-local (Real-ESRGAN) | `gallery_routes.py:1791` | Local Real-ESRGAN 2×/4× | n/a | — | — | — | 17 | **REMOVED** — see INCOMPLETE.md |
| F168 | Image mask (SAM) | `gallery_routes.py:1837` | SAM grounding → binary mask (points/box/text) | n/a | High | Med | `odysseus-ml/src/sam.rs` (`ort`) | 0 spike S3 / 17 | UNIMPLEMENTED |
| F169 | Image remove-bg (rembg) | `gallery_routes.py:1963` | rembg + optional user `hint_mask` crop/alpha-multiply | n/a | Med | High | `odysseus-ml/src/remove_bg.rs` — `rembg-rs` **HOLD** | 0 spike S5 / 17 | UNIMPLEMENTED |
| F170 | Image enhance-face (GFPGAN) | `gallery_routes.py:2056` | GFPGAN + PIL fallback | n/a | — | — | — | 17 | **REMOVED** — no identified Rust path |
| F171 | Image editor (canvas) | `galleryEditor.js` (4,386 L) + `editor/` (56 files) | Layered canvas editor: create/active layer, composite, undo/redo, flatten, crop, flood-fill, lasso, wand, transform, 27 KB adjustment popup, histogram | `editor_drafts` | Very high | Med | `odysseus-ui/src/components/editor/` (client-only, lazy) | 17 | UNIMPLEMENTED |
| F172 | Editor drafts | `routes/editor_draft_routes.py` | Server-backed projects | `editor_drafts` | Med | Low | `odysseus-services` | 17 | UNIMPLEMENTED |
| F173 | Signatures | `routes/signature_routes.py`, `gallery.js` | Reusable image stamps; PNG/SVG stored **encrypted** | `signatures` | Med | Med | `odysseus-services` | 22 | UNIMPLEMENTED |
| F174 | Image style transfer | `gallery_routes.py:604` | Privilege-gated | n/a | Med | Med | `odysseus-services` | 17 | UNIMPLEMENTED |
| F175 | Image generation (`generate_image`) | `src/ai_interaction.py:944-1190`, `src/image_model_ids.py` | Deliberately **absent from native schemas** (XML/fence-only); auto-detects the image model; saves to gallery | `gallery_images` | Med | Med | `odysseus-tools` | 17 | UNIMPLEMENTED |
| F176 | Local diffusion generation | `scripts/diffusion_server.py` (1,506 L) | Already an **optional out-of-process backend** | external | High | High | spike S9 | 17 | **HOLD** — scope decision pending |

---

## I. Search & web

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F180 | Web search providers | `services/search/providers.py` (641 L) | 7 providers: `searxng`, `brave`, `duckduckgo`, `google_pse`, `tavily`, `serper`, `disabled`. Per-provider SafeSearch mapping; SearXNG 3-stage degradation (news→general, drop language, drop engines, then HTML scrape) | n/a | High | Med | `odysseus-services/src/search/providers.rs` | 10 | UNIMPLEMENTED |
| F181 | Comprehensive web search | `services/search/core.py:250-478` | Provider chain `[primary] + fallbacks` (default `["duckduckgo"]`) × 2 attempts → `rank_search_results` → URL filters → `ThreadPoolExecutor(max_workers=4)` parallel fetch **preserving `source_index`** so `[CONTENT n]` labels match the model's `[n]` citations | `data/cache/search` | High | **High** | `odysseus-services/src/search.rs` | 10 | UNIMPLEMENTED |
| F182 | Result ranking | `services/search/ranking.py:154-160` | `2.0*title + 1.0*snippet + 1.5*domain + 1.0*recency + news_quality_adjustment`; hand-tuned adjustment (+1.2 trusted news, −0.8 facebook/yahoo/msn, −1.5 sports-on-non-sports, −1.0 subject absent); word-boundary matching | n/a | Med | Med | `odysseus-services/src/search/ranking.rs` | 10 | UNIMPLEMENTED |
| F183 | Web page content extraction | `services/search/content.py` (442 L) | HTML/PDF/text/JSON extraction + summarisers; SSRF-pinned transport; adds `Accept-Encoding: identity` to its own headers, tied to the streamed cap | `data/cache/content` | High | **Critical** | `odysseus-services/src/fetch.rs` | 10 | UNIMPLEMENTED |
| F184 | Web fetch size caps | `src/outbound_fetch.py:271-354`, `constants.py:83-84` | Soft 2 MB (agent can raise per call), hard 20 MB. Manual redirect loop (max 5), each hop re-resolved and re-pinned; **forces `Accept-Encoding: identity` and refuses any compressed response** (a compressed body can decode past `Content-Length`, defeating the cap); streamed `iter_bytes()` with a running cap; keeps the prefix on truncation | n/a | High | **Critical** | `odysseus-services/src/fetch.rs` | 10 | UNIMPLEMENTED |
| F185 | SSRF policy consolidation | `url_safety.py` (permissive), `url_security.py` (strict), pinned variants in `webhook_manager`/`integrations`/`outbound_fetch`/`caldav_sync`/`skill_importer` | **Three coexisting policies.** Consolidate into one configurable policy with an injectable resolver. `url_safety` is permissive by design (local-first: loopback/LAN is the primary use case); always rejects non-HTTP(S), link-local, multicast; `block_private` additionally rejects private/loopback/CGNAT `100.64.0.0/10` (CPython doesn't classify CGNAT as `is_private`) | n/a | High | **Critical** | `odysseus-services/src/net/ssrf.rs` | 10 | REPLACED — one policy, three modes |
| F186 | Search query enhancement | `services/search/query.py` | Query enhancement, entity extraction, TTL (news 30 min, else 24 h) | cache | Low | Low | `odysseus-services` | 10 | UNIMPLEMENTED |
| F187 | Chat message search | `routes/chat_routes.py:2695` | `GET /api/search?q=&limit=` across sessions, owner-scoped | `chat_messages_fts` | Low | Med | `odysseus-web` | 1 | UNIMPLEMENTED |
| F188 | Standalone web search API | `routes/search/search_routes.py:46` | `POST /api/search` → `{context, sources}` for Compare-mode pre-search | cache | Low | Low | `odysseus-web` | 10 | UNIMPLEMENTED |

**Route collision to resolve:** `GET /api/search` is defined twice — `chat_routes.py:2695`
(message search) and `search/search_routes.py:46`. First-registered wins. Target:
`GET /api/search/messages` and `POST /api/search/web` — **REPLACED**, documented.

---

## J. Research

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F190 | Deep research engine | `src/deep_research.py` (929 L), `services/research/` | Multi-round search→read→synthesise; `DeepResearcher(max_rounds=8, max_time=600)`; parses `### Sources` from the report via `_SOURCE_LINK_RE` | `data/deep_research/{sid}.json` | High | Med | `odysseus-services/src/research.rs` | 11 | UNIMPLEMENTED |
| F191 | Research job lifecycle | `routes/research/research_routes.py:492` | `ResearchStartRequest{query, max_rounds(0=auto→20), search_provider, endpoint_id, model, max_time(60-1800), extraction_timeout(15-3600), extraction_concurrency(1-12), category}`; session id `rp-<12hex>`; endpoint fallback chain `research→utility→default→chat→owner-scoped-first-enabled` | `data/deep_research/` | High | Med | `odysseus-services` | 11 | UNIMPLEMENTED |
| F192 | Research SSE progress | `research_routes.py:579` | 1.5 s poll → `{status, …progress, final}` | n/a | Med | Med | `odysseus-web/src/sse.rs` | 11 | UNIMPLEMENTED |
| F193 | Research synapse visualiser | `researchSynapse.js` | Animated inline SVG (520×220) of a run: root query node, sub-question branches, source leaves popping in as rounds progress. Used in two places | n/a | Med | Low | `odysseus-ui` | 11 | UNIMPLEMENTED |
| F194 | Research library & reports | `research_routes.py:323-455` | Standalone self-contained HTML report (relaxed CSP), library list, detail, archive, delete, hide/unhide images, result claim/peek | `data/deep_research/` | Med | Med | `odysseus-services` | 11 | UNIMPLEMENTED |
| F195 | Research spinoff | `research_routes.py:635` | Turn a report into a chat session. **Spinoff sessions suppress memory and RAG** (`chat_helpers.py:693-695`) because the seeded report *is* the knowledge base | `sessions` | Low | Med | `odysseus-services` | 11 | UNIMPLEMENTED |
| F196 | Visual report generation | `src/visual_report.py` (1,933 L) | Generates the report HTML; `markdown` + `nh3` allowlist sanitisation (report content is untrusted LLM output over crawled pages, and report pages run under a relaxed CSP) | files | High | **Critical** | `odysseus-services/src/report.rs` + `ammonia` | 11 | UNIMPLEMENTED |

---

## K. Notes, tasks & scheduling

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F200 | Notes & todos CRUD | `routes/note/note_routes.py` | 11 endpoints; checklist `items`, colour, label, pin, archive, due date, repeat, image; pinned + `sort_order` ordering | `notes` | Med | Med | `odysseus-services/src/notes.rs` | 12 | UNIMPLEMENTED |
| F201 | Notes panel UI | `notes.js` (5,365 L) | Google Keep-style **sidebar panel** (not a modal); confetti on completion; 6 reminder-state localStorage keys; fullscreen toggle | n/a | Med | Low | `odysseus-ui/src/components/notes.rs` | 12 | UNIMPLEMENTED |
| F202 | AI-generated notes | `notes.js:4527` | Runs a **headless background agent** — POSTs `/api/chat_stream` with `mode=agent`, `disabled_tools=["manage_notes"]`, then **drains the SSE reader to EOF without rendering** | `notes.ai_content_hash` | Med | Med | `odysseus-services` | 12 | UNIMPLEMENTED |
| F203 | Note reminders | `note_routes.py:843`, `src/reminder_personas.py` | LLM synthesis + webhook/email dispatch; `note_id` starting `test-` is admin-only | n/a | Med | Med | `odysseus-services` | 12 | UNIMPLEMENTED |
| F204 | Scheduled tasks CRUD | `routes/task/task_routes.py` | ~30 endpoints; `TaskCreate` with schedule kinds `cron/once/daily/weekly/monthly`, `task_type` `llm/action/research`, trigger_type `schedule/event`, `then_task_id` chaining | `scheduled_tasks`, `task_runs` | High | Med | `odysseus-services/src/tasks.rs` | 13 | UNIMPLEMENTED |
| F205 | `compute_next_run` | `task_scheduler.py:113-230` | 6 schedule kinds. `monthly` clamps to the last day of short months for both current and next month. **Fails closed** on malformed `scheduled_time` (returns `None`, never 500s). Timezone-aware via `CrewMember.timezone` → local arithmetic → `_to_utc_naive()`; without `tz_name`, legacy naive-UTC behaviour is preserved so existing tasks do not shift | `scheduled_tasks` | High | Med | `odysseus-services/src/cron.rs` + `cron` crate | 13 | UNIMPLEMENTED |
| F206 | Scheduler main loop | `task_scheduler.py:673-735` | 10 s startup grace; adaptive sleep `clamp(delta, 1.0, 60.0)` so `* * * * *` fires within ~1 s of the boundary, not 60 s late. **If foreground activity is detected, due tasks are pushed to `now + 15min` and skipped** | n/a | Med | High | `odysseus-services` | 13 | UNIMPLEMENTED |
| F207 | Strict task serialisation | `task_scheduler.py:737-792` | `_run_semaphore = Semaphore(1)` — a **hard guarantee of exactly one model-backed task at a time**. `TaskRun` row created as `status="queued"` **before** waiting, so the UI shows queue position. Housekeeping actions are exempt via `_MODEL_BACKED_ACTIONS` | `task_runs` | Med | High | `odysseus-services` | 13 | UNIMPLEMENTED |
| F208 | Task execution & recovery | `task_scheduler.py:816-1174` | Admin gate (pause + error the run), foreground-quiet wait, `queued→running` flip, `_cancel_if_foreground_active` monitor (250 ms poll, cancels if the user returns). `TaskDeferred` → push `next_run` (20 min, 40 after >2 defers) + delete the run row; `CancelledError` → `aborted`; `TaskNoop` → `skipped` with a reason (a comment notes the old `db.delete(run)` made users think tasks vanished). Error path **advances `next_run` even on failure** (prevents busy-loop) and has a **fallback recovery commit in a fresh session** | `task_runs` | High | High | `odysseus-services` | 13 | UNIMPLEMENTED |
| F209 | Task → agent run bridge | `task_scheduler.py:1858-2012` | Resolves the fallback chain; runs `stream_agent_loop(workload="background")`; parses SSE accumulating `delta` (skipping `thinking`) and `tool_output`. **Approval boundary**: if the agent asks for exact approval it consumes it with `decision="deny"` and returns "Run this task interactively to approve" — scheduled tasks have no interactive surface. Grace summarisation if the model exhausted rounds with no final text | `task_runs` | High | High | `odysseus-services` | 13 | UNIMPLEMENTED |
| F210 | Task chaining | `task_scheduler.py:1075-1087,2159-2174` | `then_task_id`; `_has_chain_cycle` (max depth 10); same-owner validation | `scheduled_tasks` | Med | Med | `odysseus-services` | 13 | UNIMPLEMENTED |
| F211 | Event bus | `src/event_bus.py` (119 L) | **Not a pub/sub broker** — a threshold counter bus backed by the DB. `fire_event` resolves an ownerless event to the first admin, increments `trigger_counter`, and when it reaches `trigger_count` **persists `next_run` before** handing off to the scheduler, so a restart mid-queue does not lose the event | `scheduled_tasks` | Med | Med | `odysseus-services` | 13 | UNIMPLEMENTED |
| F212 | Built-in task actions | `src/builtin_actions.py` (3,436 L) | Registry `BUILTIN_ACTIONS` + `BUILTIN_ACTION_INFO`. Housekeeping: `tidy_sessions`, `tidy_documents`, `consolidate_memory`, `tidy_research`, `audit_skills`. Email: `summarize_emails`, `draft_email_replies`, `email_auto_translate`, `check_email_urgency` (~900 lines with SQLite-atomic checkpointing). Calendar: `extract_email_events`, `classify_events` (heuristic + LLM tagging). Assistant: `daily_brief`. Skills: `learn_sender_signatures`, `test_skills` (advisory only — never rewrites or demotes), `audit_skills`. Remote: `ssh_command`, `run_script`, `run_local`. Serving: `cookbook_serve` | varies | High | Med | `odysseus-services/src/actions.rs` | 13 | UNIMPLEMENTED |
| F213 | `TaskNoop`/`TaskDeferred` control flow | `builtin_actions.py:412,424` | Deliberately `BaseException` subclasses, not `Exception` | n/a | Low | Med | `odysseus-domain/src/errors.rs` | 13 | UNIMPLEMENTED |
| F214 | Housekeeping defaults | `task_scheduler.py:251-263,2313-2519` | 12 seeded actions per owner; 5 event-triggered (every 5), 5 cron with `ship_paused: True`. `ensure_defaults` is a large reconciliation: legacy-name remap, retire retired actions, dedupe by score, force `notifications_enabled=False` and `output_target="none"`, orphan `TaskRun` sweep | `scheduled_tasks` | High | Med | `odysseus-services` | 13 | UNIMPLEMENTED |
| F215 | Task webhook trigger | `task_routes.py:1045` | **`POST /api/tasks/{task_id}/webhook/{token}` is UNAUTHENTICATED — the path token IS the credential.** 404 on mismatch (enumeration-safe); auto-pauses admin-only-action tasks for non-admins; 409 if already running. Middleware-exempt via regex `^/api/tasks/[^/]+/webhook/[^/]+/?$` | `scheduled_tasks.webhook_token` | Med | **Critical** | `odysseus-web` | 13 | UNIMPLEMENTED |
| F216 | Natural-language task parsing | `task_routes.py:1089` | NL → structured task draft. **Returns a draft only, never creates a live task** | n/a | Med | Low | `odysseus-services` | 13 | UNIMPLEMENTED |
| F217 | Tasks UI | `tasks.js` (3,188 L) | 30 s notification poll (first fire at 1.5 s); badge CSS classes on both the sidebar button and the rail icon; custom time/date pickers; UTC↔local conversion; bulk select/delete with animated removal; run history | n/a | Med | Low | `odysseus-ui/src/components/tasks.rs` | 13 | UNIMPLEMENTED |
| F218 | Personal assistant | `routes/assistant_routes.py`, `assistant.js` | A specially-flagged `CrewMember(is_default_assistant)` whose pinned session is resolved via `/api/assistant/session` and fed to the normal `selectSession()` path | `crew_members` | Med | Low | `odysseus-services` | 13 | UNIMPLEMENTED |

---

## L. Calendar & CalDAV

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F220 | Calendar CRUD | `routes/calendar_routes.py` | 16 endpoints; calendars + events; rrule expansion; `scope=series` deletes | `calendars`, `calendar_events` | Med | Med | `odysseus-services/src/calendar.rs` | 16 | UNIMPLEMENTED |
| F221 | Event recurrence | `calendar_routes.py:1139`, `dateutil.rrule` | RRULE expansion, EXDATE, all-day vs UTC (`is_utc` NOT NULL), `importance`, `event_type` | `calendar_events` | Med | Med | `odysseus-services` + `rrule` crate | 16 | UNIMPLEMENTED |
| F220b | ICS import / export | `calendar_routes.py:1411,1561` | Upload `.ics` (10 MB) with `calendar_name`; export with a sanitised filename + `Content-Disposition: attachment` | n/a | Med | Med | `odysseus-services/src/ics.rs` + `icalendar` port | 16 | UNIMPLEMENTED |
| F222 | Natural-language event parsing | `calendar_routes.py:1619` | Paste natural text → event | n/a | Med | Low | `odysseus-services` | 16 | UNIMPLEMENTED |
| F223 | CalDAV pull sync | `src/caldav_sync.py:271-492` | `_LOOKBACK_DAYS=90` back, `_LOOKAHEAD_DAYS=365` forward; principal discovery with fallback to treating the URL as one calendar; stable hashed local calendar id (`_stable_cal_id(remote_url, owner, account_id)`) so re-syncs are idempotent; upsert by UID with a pending dict so duplicate UIDs in one batch update rather than violate UNIQUE; **skips rows with a local write-back pending**; clamps `dtend <= dtstart` | `calendars`, `calendar_events` | High | High | `odysseus-services/src/caldav.rs` | 16 | UNIMPLEMENTED |
| F224 | CalDAV prune safety | `caldav_sync.py:257-268,469-481` | Deletes local `origin=="caldav"` rows in-window whose UID was not seen — **but only when `_should_prune_window(seen_uids, parse_failed)`**. Any parse failure skips the prune, because an incomplete `seen_uids` would delete events that still exist upstream | `calendar_events` | Med | **Critical** | `odysseus-services/src/caldav.rs` | 16 | UNIMPLEMENTED |
| F225 | CalDAV write-back | `src/caldav_writeback.py` (311 L) | Pure injectable functions designed for unit testing with a fake client. `writeback_event` **never raises** — returns `{"skipped": …}` when not CalDAV-backed; best-effort, the local DB stays source of truth. Stores `remote_href`/`remote_etag`, clears `caldav_sync_pending`, or records `tombstone.last_error` | `calendar_events`, `caldav_deleted_events` | High | Med | `odysseus-services/src/caldav.rs` | 16 | UNIMPLEMENTED |
| F226 | CalDAV URL hardening | `caldav_sync.py:90-130` | Requires http/https, rejects credentials-in-URL and fragments; resolves **all** A/AAAA records and rejects loopback/link-local/multicast/unspecified/reserved; private IPs need `ODYSSEUS_ALLOW_PRIVATE_CALDAV=1`; blocks `metadata.google.internal`; **disables redirects** | n/a | Med | **Critical** | `odysseus-services/src/net/ssrf.rs` | 16 | UNIMPLEMENTED |
| F227 | Multi-account CalDAV | `caldav_sync.py:582-614` | `caldav_accounts[]` with automatic migration from the legacy single `caldav` pref key; passwords Fernet-encrypted | settings | Med | Med | `odysseus-services` | 16 | UNIMPLEMENTED |
| F228 | Calendar UI | `calendar.js` (3,722 L) | Month/week/year grid; exports only `openCalendar`/`openCalendarTo`/`closeCalendar`/`isCalendarOpen`. **Holds most of the app's custom date/time pickers**, re-exported by `tasks.js` | n/a | High | Med | `odysseus-ui/src/components/calendar.rs` (client-only) | 16 | UNIMPLEMENTED |
| F229 | Event classification | `builtin_actions.py:1355,1322` | Heuristic + LLM tagging of importance/type/colour | `calendar_events` | Med | Low | `odysseus-services` | 16 | UNIMPLEMENTED |
| F230 | Calendar event extraction from email | `builtin_actions.py:1518` | Extracts events from email bodies | `calendar_events` | Med | Med | — | 16 | UNIMPLEMENTED — **blocked by Phase 15 (email)** |
| F231 | Recurring-event reminders | `task_scheduler.py` `_note_pings_loop` | Per-owner `action_ping_notes` every 60 s. Comment at `:552-558` explains the calendar-event scanner was **removed** to avoid duplicate notifications | n/a | Low | Low | `odysseus-services` | 13 | UNIMPLEMENTED |
| F232 | Calendar badge | `calendar.js` | `odysseus-calendar-badge-seen`, `odysseus-calendar-cache` | localStorage | Low | Low | `odysseus-ui` | 16 | UNIMPLEMENTED |

---

## M. Email

**Entire domain deferred to Phase 15.** See REWRITE_PLAN §9.2 for the dependent-feature
consequences.

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F240 | IMAP listing | `routes/email_routes.py:2326` | `folder, limit, offset, filter(all/unread/unanswered/undone), from, account_id, has_attachments, cached_only, cache_bust`; cache key includes owner | `data/scheduled_emails.db` | High | Med | `odysseus-services/src/email/imap.rs` | 15 | UNIMPLEMENTED |
| F241 | IMAP search | `email_routes.py:2769` | IMAP SEARCH on body/subject/from; auto-swaps INBOX→All Mail on Gmail. Declared `def` (sync) so blocking IMAP runs in the threadpool | `data/scheduled_emails.db` | High | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F242 | Message read + threading | `email_routes.py:3175`, `src/email_thread_parser.py` (25 KB) | Full message + thread context; quote boundary detection | caches | High | Med | `odysseus-services/src/email/thread.rs` | 15 | UNIMPLEMENTED |
| F243 | Message mutations | `email_routes.py:3732-4004` | mark-read/unread, flag, archive, move, delete, delete-permanent, mark-answered, clear-answered, unflag-spam | IMAP + `email_tags` | Med | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F244 | Attachments | `email_routes.py:3279-3710` | Metadata list (cached), single extraction, **all-attachments ZIP** (skips signature images, dedupes names), inline MIME image by Content-ID (415 if not an image), open-as-document (`.pdf`→PDF doc, `.docx`→markdown, `.txt/.md`→markdown), local staged path | files | High | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F245 | SMTP send | `email_routes.py:4518` | Builds MIME `mixed`/`alternative`, `In-Reply-To`/`References`, Odysseus headers; dispatched via `BackgroundTasks` (returns immediately) | IMAP/SMTP | High | High | `lettre` | 15 | UNIMPLEMENTED |
| F246 | Drafts | `email_routes.py:4722` | Save to Drafts | IMAP | Med | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F247 | Scheduled sends | `email_routes.py:4293-4382` | Schedule send; list; cancel | `scheduled_emails` | Med | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F248 | Agent draft approval | `email_routes.py:4406-4469` | Pending drafts; approve; reject | `scheduled_emails` | Med | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F249 | Compose staging | `email_routes.py:4019-4231` | 4 staging paths: direct upload (25 MB cap, returns `{token, filename, size}`), from an Odysseus artifact, from an artifact ZIP, reuse an existing attachment. Staged uploads are discarded by token | files | Med | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F250 | AI summarise | `email_routes.py:4883` | Pulls attachment text when `uid` given; cached | `email_summaries` | Med | Low | `odysseus-services` | 15 | UNIMPLEMENTED |
| F251 | AI translate | `email_routes.py:5000` | Translate body; cached by `(body_hash, owner, target_language)`; records `same_language` | `email_translations` | Med | Low | `odysseus-services` | 15 | UNIMPLEMENTED |
| F252 | AI reply draft | `email_routes.py:5147` | Style-matched reply draft; reply cache keyed on `message_id`; **bypasses cache when `user_hint` is present**; `fast` mode | `email_ai_replies` | Med | Low | `odysseus-services` | 15 | UNIMPLEMENTED |
| F253 | Writing-style extraction | `email_routes.py:4774,5441-5452` | Derive writing style from samples; read/write style | `sender_signatures` | Med | Low | `odysseus-services` | 15 | UNIMPLEMENTED |
| F254 | Email accounts | `email_routes.py:5466-5986` | Config + accounts CRUD + test + set-default. **Partial unique index** `ux_email_accounts_one_default_per_owner ON email_accounts (COALESCE(owner,'')) WHERE is_default = 1`. Duplicate-default normalisation **raises on failure** rather than silently accepting a race | `email_accounts` | High | High | `odysseus-store` + `odysseus-services` | 15 | UNIMPLEMENTED |
| F255 | Email account owner locks | `database.py:474-517` | SQLite → `BEGIN IMMEDIATE`; row-locking DBs → `SELECT … FOR UPDATE` on `email_account_owner_locks`, owners sorted canonically to avoid deadlock | `email_account_owner_locks` | Med | Med | `odysseus-store` | 15 | UNIMPLEMENTED |
| F256 | Google OAuth | `email_routes.py:6009-6033` | `/authorize` → 302 with a signed `state`; `/callback` → 302 to `/?section=integrations&email_oauth_error=…`; XOAUTH2 token exchange | `email_accounts` (encrypted) | Med | **Critical** | `odysseus-integrations/src/google_oauth.rs` | 15 | UNIMPLEMENTED |
| F257 | Unsubscribe automation | `email_routes.py:2585-2677` | Scan; execute (**mailto `List-Unsubscribe` only, header re-read server-side**); cleanup | IMAP | Med | High | `odysseus-services` | 15 | UNIMPLEMENTED |
| F258 | Email urgency triage | `routes/email_pollers.py` (1,565 L), `builtin_actions.py:2250` | ~900 lines with **SQLite-atomic checkpointing** via `_run_email_urgency_state_transaction` and `_merge_email_urgency_state`, so a crash mid-run does not re-alert | `email_urgency_alerts` | High | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F259 | Email AI cache layers | `email_routes.py:1517-1530` | 4 layers: `_LIST_CACHE` 8 s, `_READ_CACHE` 60 s, `_IMAP_POOL`, prefetch task | in-memory | Med | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F260 | Email message index | `email_routes.py` | Local index so unread-state and search work without IMAP; `date_epoch` indexed DESC | `email_message_index` | Med | Low | `odysseus-store` | 15 | UNIMPLEMENTED |
| F261 | Email pollers | `routes/email_pollers.py` | Background IMAP polling; `ODYSSEUS_INPROCESS_POLLERS` gate for cron-driven deployments | n/a | High | Med | `odysseus-services` | 15 | UNIMPLEMENTED |
| F262 | Email MCP server | `mcp_servers/email_server.py` (2,912 L) | 16 email tools exposed to the agent (`BUILTIN_EMAIL_TOOLS` is the single source of truth for fence tags, dispatch, schemas, and the non-admin blocklist) | n/a | High | Med | `odysseus-integrations` | 19 | UNIMPLEMENTED |

---

## N. Skills

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F270 | Skills storage | `services/memory/skills.py` (716 L) | `data/skills/<category>/<name>/SKILL.md` with YAML frontmatter; usage counters in `data/skills/_usage.json` sidecar | `data/skills/` | Med | Med | `odysseus-services/src/skills.rs` | 14 | UNIMPLEMENTED |
| F271 | Skill relevance | `skills.py:35-42` | Jaccard-similarity fallback when embeddings are unavailable | n/a | Low | Low | `odysseus-services` | 14 | UNIMPLEMENTED |
| F272 | Skills CRUD | `routes/skills_routes.py:1380-1899` | 22 endpoints; list, get, raw markdown, save markdown, update, delete, search | `data/skills/` | Med | Med | `odysseus-web` | 14 | UNIMPLEMENTED |
| F273 | Skill invocation | `skills_routes.py:1413` | Build a skill-pinned prompt for `/slash` | n/a | Med | Med | `odysseus-services` | 14 | UNIMPLEMENTED |
| F274 | Skill testing | `skills_routes.py:1478,1561,1682` | Background agent run + LLM judge; records `pass`/`needs_work`/`fail`/`inconclusive`; **advisory only — never rewrites or demotes**; nudges confidence (pass .95 / needs_work .6 / fail .4); gated-action approval; poll `test-status` | `data/skills/_usage.json` | Med | Med | `odysseus-services` | 14 | UNIMPLEMENTED |
| F275 | Skill audit-all | `skills_routes.py:1701-1790` | Background test→judge→self-edit→teacher→demote-to-draft; `scope: drafts\|unchecked\|all\|selected`; cancellable; poll `audit-all/status` | `data/skills/` | High | Med | `odysseus-services` | 14 | UNIMPLEMENTED |
| F276 | Nightly skill audit | `app.py:1238-1261` | At ~02:00 local, tests+judges a batch of the least-recently-checked skills, auto-fixing/escalating weak ones (**never deletes**); rotates through the library; gated by `skill_audit_nightly`, `skill_audit_hour`, `skill_audit_batch` | `data/skills/` | Med | Low | `odysseus-services` | 14 | UNIMPLEMENTED |
| F277 | Skill import from URL | `services/memory/skill_importer.py` (487 L) | Fetches SKILL.md bundles from GitHub/skills.sh with SSRF-pinned transport; `_GITHUB_HOSTS` allowlist; `MAX_FILES=64`, `MAX_TOTAL_BYTES=2MB`, `MAX_FILE_BYTES=400KB`, suffix allowlist | `data/skills/` | Med | **Critical** | `odysseus-services` | 14 | UNIMPLEMENTED |
| F278 | Skill format & escaping | `services/memory/skill_format.py` (483 L), `skill_extractor.py` (305 L) | Frontmatter parse/serialise, timestamp handling, JSON extraction with stray-brace recovery | n/a | Med | Med | `odysseus-services` | 14 | UNIMPLEMENTED |
| F279 | Built-in skills | `skills_routes.py:1250-1349` | Builtin skill catalog + per-user override + reset | settings | Low | Low | `odysseus-services` | 14 | UNIMPLEMENTED |
| F280 | Skills UI | `skills.js` (2,025 L) | Tab **inside the memory modal**; only 2 exports — the UI is built imperatively. Full lifecycle + background audit with status polling | n/a | Med | Low | `odysseus-ui/src/components/skills.rs` | 14 | UNIMPLEMENTED |

---

## O. Settings, admin & configuration

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F290 | Settings store | `src/settings.py` (339 L) | `data/settings.json` + `data/features.json`; 2 s TTL cache; ~60 `DEFAULT_SETTINGS` keys; `RETIRED_SETTING_KEYS` tombstone; `is_setting_overridden` reads the **raw** file to distinguish "explicitly set" from "materialised default" | `settings.json` | Med | Med | `odysseus-store/src/settings.rs` | 20 | UNIMPLEMENTED |
| F291 | Settings API + clamping | `auth_routes.py:694-760` | Feature toggles (8: `web_search`, `web_fetch`, `deep_research`, `memory`, `document_editor`, `rag`, `sensitive_filter`, `gallery`); numeric clamp (`agent_max_rounds` 1-200, `agent_max_tool_calls` 0-1000); `scrub_settings()` for non-admins | `settings.json` | Med | High | `odysseus-web` | 20 | UNIMPLEMENTED |
| F292 | Settings registry & UI | `settings/registry.js` + `settings.js` (5,659 L) + `admin.js` (3,197 L) | **The only subsystem with a real declarative registry.** 5 groups, 13 panels, each declared **twice** in HTML (sidebar button + content div). `getSettingsRegistryIssues()` validates every registry panel has a matching DOM node. 4 panels have `controller: 'admin'` and defer to `admin.js` — the natural Leptos seam | n/a | High | Med | `odysseus-ui/src/components/settings/` | 20 | UNIMPLEMENTED |
| F293 | Shared config cache | `appConfig.js` (86 L) | Promise-slots for settings+tools; clears the slot on rejection so a boot blip doesn't poison the session; `sessionStorage` prefetch handoff from the login page; `invalidateSettings()`/`invalidateTools()` | sessionStorage | Low | Low | `odysseus-ui` context | 0 | UNIMPLEMENTED |
| F294 | User preferences | `routes/prefs_routes.py` | 3 endpoints; single-user no-clobber semantics; atomic write; `_users: {user: {...}}` structure | `user_prefs.json` | Low | Med | `odysseus-store` | 22 | UNIMPLEMENTED |
| F295 | Presets & templates | `routes/preset_routes.py`, `src/preset_manager.py` | 8 endpoints; custom presets, templates, expand, groups; atomic save; fill missing defaults | `presets.json` | Med | Low | `odysseus-store` | 22 | UNIMPLEMENTED |
| F296 | Generic integrations | `routes/auth_routes.py:762-827`, `src/integrations.py` | 8 presets: miniflux, gitea, linkding, homeassistant, ntfy, discord_webhook, vaultwarden, freshrss. `api_key` Fernet-encrypted at rest; `mask_integration_secret` | `integrations` + `integrations.json` | High | **Critical** | `odysseus-services/src/integrations.rs` | 21 | UNIMPLEMENTED |
| F297 | Integration API calls | `integrations.py:487-745` | SSRF guard with a **recording resolver** so the IPs validated are the IPs the connect is pinned to (closing DNS-rebinding TOCTOU); `_validated_ips()` de-dupes because `getaddrinfo` returns one address per socktype on glibc. 30 s timeout. Binary-search truncation for lists (largest prefix fitting 12,000 chars), insertion-order truncation for dicts. HTTP ≥400 returns `{"error":…, "exit_code":1, "untrusted_content":True}` — **explicit provenance so the agent gate does not treat HTTP failure as content-free** | n/a | High | **Critical** | `odysseus-services/src/integrations.rs` | 21 | UNIMPLEMENTED |
| F298 | Settings scrub | `src/settings_scrub.py` | Removes secrets for non-admin reads | n/a | Low | **Critical** | `odysseus-store` | 20 | UNIMPLEMENTED |
| F299 | Admin diagnostics panel | `admin.js` | Users, tokens, webhooks, MCP servers, providers, model endpoints, db stats, diagnostics, backup, wipe | various | Med | Med | `odysseus-ui/src/components/settings/admin.rs` | 20 | UNIMPLEMENTED |
| F300 | Admin danger-zone wipes | `routes/admin_wipe/admin_wipe_routes.py` | `DELETE /wipe/{kind}`; gallery wipe has dedicated regression tests | various | Med | **Critical** | `odysseus-web` | 22 | UNIMPLEMENTED |
| F301 | Workspace vetting | `routes/workspace_routes.py` | `GET /browse`, `GET /vet`; `vet_workspace` refuses filesystem roots | n/a | Med | **Critical** | `odysseus-web` | 22 | UNIMPLEMENTED |
| F302 | Sensitive-content censor | `censor.js` (400 L), `sensitive_filter` feature | Blur overlay for text/images; `odysseus-sensitive-blur` | n/a | Med | Low | `odysseus-ui` | 20 | UNIMPLEMENTED |
| F303 | Accessibility helpers | `a11y.js` (220 L) | Live-region announcer, focus helpers | n/a | Low | Low | `odysseus-ui` | 0 | UNIMPLEMENTED |
| F304 | Keyboard shortcuts | `keyboard-shortcuts.js` (480 L) | 17 default keybinds; AltGr guard; persisted `keybinds` setting | `settings.json` | Med | Low | `odysseus-ui` | 20 | UNIMPLEMENTED |
| F305 | Emoji / font / colour pickers | `emojiPicker.js`, `emojiShortcodes.js`, `colorPicker.js`, `langIcons.js`, `routes/emoji_routes.py`, `routes/font_routes.py` | Twemoji SVG proxy (same-origin, lazy-cached, hardening-tested); colour picker; language icons | cache | Med | Low | `odysseus-ui` | 22 | UNIMPLEMENTED |

---

## P. Integrations, webhooks, tokens & external APIs

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F310 | Outgoing webhooks | `src/webhook_manager.py` (455 L) | 4 events: `session.created`, `chat.completed`, `chat.message`, `webhook.test`. HMAC-SHA256 `X-Odysseus-Signature` over the exact body; body `{event, timestamp, data}`; per-delivery client (no shared client), `follow_redirects=False`, 10 s timeout | `webhooks` | Med | **Critical** | `odysseus-integrations/src/webhooks.rs` | 21 | UNIMPLEMENTED |
| F311 | Webhook SSRF + DNS pinning | `webhook_manager.py:30-211` | Blocks localhost/0.0.0.0/metadata endpoints and `.local/.internal/.lan/.intranet/.localhost`; resolves DNS and **fails closed if unresolvable**; unwraps IPv4-mapped IPv6; `_PinnedAsyncBackend` overrides **only** `connect_tcp` so SNI and `Host` stay the original hostname | n/a | High | **Critical** | `odysseus-integrations/src/webhooks.rs` | 21 | UNIMPLEMENTED |
| F312 | Webhook error sanitisation | `webhook_manager.py:272-320,438` | Redacts IPv6 (incl. bracketed authority), then IPv4, then `https?://[^\s/]+` → `[redacted-url]`. Errors logged **without the URL** | n/a | Med | **Critical** | `odysseus-integrations` | 21 | UNIMPLEMENTED |
| F313 | Webhook fire-and-forget | `webhook_manager.py:335-343,360` | `_bg_tasks` strong-reference set prevents GC from eating a delivery; works from both sync and async contexts via `run_coroutine_threadsafe` | n/a | Low | Med | `odysseus-integrations` | 21 | UNIMPLEMENTED |
| F314 | Public sync chat API | `routes/webhook/webhook_routes.py:237` | `POST /api/v1/chat` for n8n/Make/Activepieces. 3 modes: resume an owned session / direct `api_key` + auto-resolved provider (13 known providers) / preconfigured endpoint. `base_url` validated with `validate_public_http_url`. **Explicit scope `chat` re-check, not middleware** | n/a | Med | **Critical** | `odysseus-web` | 21 | UNIMPLEMENTED |
| F315 | Codex plugin bridge | `routes/codex_routes.py:154` | `/api/codex/*`. Borrows `email_router`/`memory_router`/`calendar_router`/`document_router` and **re-invokes their handlers by path string** inside `_as_owner()`. Scope sets for todos/email/memory/calendar/documents/cookbook. Cookie callers additionally require admin | various | High | **Critical** | `odysseus-web/src/routes/codex.rs` | 21 | UNIMPLEMENTED |
| F316 | Claude integration | `routes/codex_routes.py:889` | `/api/claude/*` | various | Med | **Critical** | `odysseus-web` | 21 | UNIMPLEMENTED |
| F317 | Codex plugin ZIP | `codex_routes.py:218` | `odysseus-codex-plugin.zip` download | n/a | Low | Low | `odysseus-web` | 21 | UNIMPLEMENTED |
| F318 | Companion pairing | `companion/routes.py` | `/api/companion/*`: ping, info, models (owner-scoped, hides `hidden_models`, **never returns keys**), `GET /pair` admin HTML (GET never mints — CSRF), `POST /pair` mints a chat-scoped token with QR data-URI; SameSite=Lax | `api_tokens` | Med | High | `odysseus-web` | 21 | UNIMPLEMENTED |
| F319 | Device-flow router factory | `routes/device_flow.py:193` | 3 routes per provider (`device/start`, `device/poll`, `device/cancel`); in-memory `PendingDeviceFlowStore` (thread-safe, TTL prune, `slow_down` backoff); **the `device_code` stays server-side — the browser only gets a `poll_id`** | in-memory | Med | High | `odysseus-integrations/src/device_flow.rs` | 2 | UNIMPLEMENTED |
| F320 | MCP client | `src/mcp_manager.py` (709 L) | Transports: stdio, SSE, HTTP — each with an `AsyncExitStack` with a `registered` flag so a mid-handshake failure still closes the stack. A `_generation` counter drives RAG re-indexing and prompt-cache invalidation. On failure a **builtin** server gets one automatic reconnect+retry; a user server does not | `mcp_servers` | High | High | `odysseus-integrations/src/mcp.rs` | 19 | UNIMPLEMENTED |
| F321 | MCP content handling | `mcp_manager.py:509` | Flattens content blocks; **image blocks become `images:[{data,mimeType}]`** (Playwright screenshots); MCP `isError` sets `result["untrusted_content"] = True`, which **arms the tool gate** | n/a | Med | **Critical** | `odysseus-integrations/src/mcp.rs` | 19 | UNIMPLEMENTED |
| F321b | MCP namespace schema export | `mcp_manager.py:570` | Namespaces `mcp__<server>__<tool>`; **skips builtin Python servers** (they use the code-block format) while including `builtin_browser` (needs function calling) | n/a | Med | Med | `odysseus-integrations` | 19 | UNIMPLEMENTED |
| F322 | MCP param hint hardening | `mcp_manager.py:46,60` | Renders JSON-Schema args as a compact prompt hint with hard caps (12 params, 40 chars/token, 300 total) because **MCP schemas are untrusted third-party input** (#2660) | n/a | Med | **Critical** | `odysseus-integrations` | 19 | UNIMPLEMENTED |
| F323 | MCP plan-mode blocking | `mcp_manager.py:107,623` | `mcp_tool_is_readonly` prefers server `annotations.readOnlyHint`/`destructiveHint`, falls back to a verb-prefix heuristic (`list|get|read|search|…|summar`), **failing closed**. Returns both a schema-hiding map and a runtime-rejection set | n/a | Med | **Critical** | `odysseus-integrations` | 19 | UNIMPLEMENTED |
| F324 | MCP OAuth | `src/mcp_oauth.py` (211 L) | RFC 9728 discovery + Dynamic Client Registration + auth-code with PKCE + refresh, bridged to the browser callback at `{base}/api/mcp/oauth/callback`. Tokens **and the dynamic client registration** persisted in the encrypted `mcp_servers.oauth_tokens` column. Module-level `_pending` futures keyed by `state`, `_prune_stale()` bounding abandoned flows at `AUTH_WAIT_SECONDS=300` | `mcp_servers` | High | **Critical** | `odysseus-integrations/src/mcp_oauth.rs` | 19 | UNIMPLEMENTED |
| F325 | Built-in MCP servers | `src/builtin_mcp.py` (395 L) | Registers `image_gen`, `memory`, `rag`, `email` stdio servers at startup, plus `builtin_browser` via `npx -y @playwright/mcp@latest --headless --caps vision`. `_BG_TASKS` strong-reference set so GC cannot kill a connect task mid-registration. `ODYSSEUS_DISABLE_MCP`, `ODYSSEUS_BROWSER_MCP_REQUIRE_CACHE` kill switches. Comment at `:66-72` records that bash/python/filesystem/web_search were **folded into native in-process execution** and their subprocess wrappers deleted | n/a | Med | Med | `odysseus-integrations` | 19 | UNIMPLEMENTED |

---

## Q. Contacts, vault, compare & misc

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F330 | Vault (Bitwarden-compatible) | `routes/vault/vault_routes.py` | config, login, unlock, lock, logout. Password must not appear in argv (regression-tested) | `vault.json` | Med | **Critical** | `odysseus-services/src/vault.rs` | 22 | UNIMPLEMENTED |
| F331 | Contacts (CardDAV) | `routes/contacts/contacts_routes.py` | 10 endpoints: list, search, add, import, export, config, clear, update, delete. vCard parse/unfold (regression-tested). Passwords encrypted | `contacts.json` | Med | **Critical** | `odysseus-services/src/contacts.rs` | 22 | UNIMPLEMENTED |
| F332 | Contact resolution | `tool_schemas.py` `resolve_contact`, `routes/email_routes.py:4470` | Resolve a contact by name for email recipients | n/a | Low | Med | `odysseus-tools` | 15 | UNIMPLEMENTED |
| F333 | Blind model comparison | `routes/compare/compare_routes.py`, `compare/state.js` | Two models answer the same prompt; blind mode with a hidden mapping; vote; record; history; delete. Redaction tested | `comparisons` | Med | Med | `odysseus-services/src/compare.rs` | 22 | UNIMPLEMENTED |
| F334 | Compare UI | `compare/` (5 files) | `state.js` is the closest thing to a real store in the frontend — a single mutable object with ~35 fields mutated by 9 sibling modules, plus `reset()`. Parallel streaming, per-pane sessions/metrics, abort controllers | n/a | Med | Low | `odysseus-ui/src/components/compare.rs` | 22 | UNIMPLEMENTED |
| F335 | Backup export/import | `routes/backup_routes.py` | Export/import user data. Cross-user dedup and skill dedup are regression-tested | all | Med | **Critical** | `odysseus-web` | 22 | UNIMPLEMENTED |
| F336 | Cleanup service | `src/cleanup_service.py` | `ARCHIVE_AFTER_DAYS=7`, `DELETE_AFTER_DAYS=14`, `MIN_MESSAGES_TO_KEEP=20`, `PRESERVE_RECENT_COUNT=10`, `PROTECTED_KEYWORDS`, `ESTIMATED_MESSAGE_SIZE_BYTES=512`. Strict owner filtering; preview endpoint before executing | `sessions` | Med | **Critical** | `odysseus-services/src/cleanup.rs` | 22 | UNIMPLEMENTED |
| F337 | TTS service | `services/tts/` | 4 providers: `disabled`, `browser` (Web Speech API), `local` (Kokoro-82M), `endpoint:<id>`. File cache in `data/tts_cache/` keyed by `sha256(provider|model|voice|speed|text)`; extension sniffed from magic bytes; LRU-ish eviction to 80 % of `ODYSSEUS_TTS_CACHE_MAX_BYTES` (default 500 MB). `_safe_speed()` defends against agent-written corrupt settings | `data/tts_cache/` | Med | Med | `any-tts` crate (KEEP) | 0 spike S4 / 22 | UNIMPLEMENTED |
| F338 | Kokoro local TTS | `services/tts/`: `_KokoroPipeline` | 82M model, **CUDA-only**, emits 24 kHz mono 16-bit WAV | n/a | Med | Med | `any-tts` if it provides a Kokoro backend, else `kokoroxide` **HOLD** | 0 spike S4 / 22 | UNIMPLEMENTED |
| F339 | STT service | `services/stt/` | 4 providers: `disabled`, `browser`, `local` (faster-whisper, CUDA if available else CPU/int8), `endpoint:<id>` (OpenAI-compatible `/audio/transcriptions`). Reads live settings per call | n/a | Med | Med | `whisper-rs` crate | 0 spike S6 / 22 | UNIMPLEMENTED |
| F340 | Voice recorder | `voiceRecorder.js` | Composer mic recording → `/api/stt/transcribe` | n/a | Low | Low | `odysseus-ui` | 22 | UNIMPLEMENTED |
| F341 | YouTube ingestion | `services/youtube/`, `src/youtube_handler.py` | Transcript via `youtube-transcript-api`; comments via `yt-dlp` subprocess. `extract_youtube_id` handles `/watch?v=`, `youtu.be/`, `/embed/`, `/shorts/`, `/live/`, `/v/` across 4 hosts incl. `music.youtube.com`. `YOUTUBE_INSTRUCTION_PROMPT` tells the LLM not to web-search the video | n/a | Med | Med | `odysseus-services/src/youtube.rs` | 9 | UNIMPLEMENTED |
| F342 | Sandboxed user mini-apps | `routes/model_routes.py:2694-2720`, `user_tools` tables | 13 user tools as HTML mini-apps with a KV data store; scoped by `scope`; pinned/active flags | `user_tools`, `user_tool_data` | Med | Med | `odysseus-services/src/user_tools.rs` | 22 | UNIMPLEMENTED |
| F343 | Crew members | `crew_members` table | AI personas: name, avatar, personality, model, endpoint, greeting, enabled tools, session, sort order, `is_default_assistant`, timezone | `crew_members` | Med | Low | `odysseus-services` | 13 | UNIMPLEMENTED |
| F344 | Group indicators & rail sync | `group.js`, `sidebar-layout.js` | 5 `window.*` globals (`_syncResearchIndicator`, `_syncRagIndicator`, `_syncModelPickerAutohide`, `_syncGroupIndicator`, `_syncRailDynamic`) | n/a | Low | Med | `odysseus-ui` | 4 | UNIMPLEMENTED |
| F345 | Code runner | `codeRunner.js` | `.run-code` button → `POST /api/shell/exec` | n/a | Low | Med | `odysseus-ui` | 8 | UNIMPLEMENTED |

---

## R. System, health & diagnostics

| ID | Feature | Source | Behaviour | Data | Complexity | Risk | Target | Phase | Status |
|---|---|---|---|---|---|---|---|---|---|
| F350 | Shell execution | `routes/shell_routes.py:960` | `POST /api/shell/exec` → `{stdout, stderr, exit_code}`. **All routes admin-gated** (`_require_admin` rejects `current_user` in `{None, "api"}`), explicit "RCE-after-signup" guard, `_reject_cross_site` via `Sec-Fetch-Site` | n/a | Med | **Critical** | `odysseus-services/src/shell.rs` | 13 | UNIMPLEMENTED |
| F351 | Shell SSE streaming | `shell_routes.py:978` | Modes: plain, `use_pty`, `use_tmux` (POSIX tmux / Windows detached + logfile tail). Yields `{stream: stdout\|stderr, data}` then `{exit_code}` | n/a | High | **Critical** | `odysseus-services/src/shell.rs` + SSE | 13 | UNIMPLEMENTED |
| F352 | Shell service | `services/shell/` | `asyncio.create_subprocess_shell`, `max_output=200_000`, kills on timeout. **Duplicates `src/tools/system.py` and `subprocess_tools.py` — consolidate** | n/a | Med | Med | `odysseus-services/src/shell.rs` | 13 | UNIMPLEMENTED |
| F353 | Detached background jobs | `src/bg_jobs.py` (297 L) | **Status derives from an on-disk exit-code file, not a live PID** — restart-safe by design. Writes `{id}.cmd.sh` + a wrapper so an `exit` or unbalanced paren cannot break the wrapper. `detached_popen_kwargs` (`setsid` / `DETACHED_PROCESS`). `_pid_alive` delegates to `platform_compat.pid_alive` — a bare `os.kill(pid, 0)` **kills** the job on Windows. `max_runtime_s=3600`, `_MAX_OUTPUT_CHARS=16000` head+tail | `data/bg_jobs.json` | Med | High | `odysseus-services/src/bg_jobs.rs` | 13 | UNIMPLEMENTED |
| F354 | Background job auto-continue | `src/bg_monitor.py` (168 L) | `POLL_INTERVAL_S=5`, `_FOLLOWUP_MAX_ROUNDS=12`. `mark_followed_up()` called **only after** `_run_followup` returns True, so transient failures retry. `_run_followup` returns False when the session is mid-stream (`agent_runs.is_active`) so concurrent writes cannot interleave. Mirrors the live chat's `tool_event` shape and **preserves `ask_user` cards** from a tainted continuation | `chat_messages` | Med | High | `odysseus-services/src/bg_monitor.rs` | 13 | UNIMPLEMENTED |
| F355 | `#!bg` tool marker | `tool_execution.py:1081` | A trailing `#!bg` background marker → `bg_jobs.launch`, returns a job id and instructs the model **not to poll** | n/a | Low | Med | `odysseus-tools` | 13 | UNIMPLEMENTED |
| F356 | Interactive foreground gate | `src/interactive_gate.py` (219 L) | `has_foreground_activity()` = active requests > 0 OR within quiet window OR recent browser activity OR an active chat stream. **Passive-path allowlist** (`/api/activity/heartbeat`, `/api/client-perf`, `/api/tasks/notifications`, `/api/tasks/runs/recent`, `/api/research/active`, `/api/email/urgency-state`, `/api/email/unread-state`, prefixes `/api/chat/stream_status`, `/api/health`, `/api/prefs`) — otherwise the UI's own idle polling blocks background work. `BACKGROUND_TASK_QUIET_MS=1500` | n/a | Med | Med | `odysseus-services/src/interactive_gate.rs` | 4 | UNIMPLEMENTED |
| F357 | Teacher escalation | `src/teacher_escalation.py` (810 L) | Student (self-hosted) finishes → evaluate → if failed, escalate to a SOTA teacher that produces a corrective reply **and** writes a `SKILL.md`. Tier 1 (free) = 5 tool-error + 6 give-up regexes; Tier 2 (opt-in) = LLM self-eval. The failed trace is wrapped as untrusted before being sent. `run_teacher_inline` yields SSE so the user **sees** the teacher's tool calls live; recurses with `_is_teacher_run=True` to prevent recursion. If the teacher also fails → `escalation_failed` | `data/skills/` | High | High | `odysseus-agent/src/teacher.rs` | 7 | UNIMPLEMENTED |
| F358 | Service health | `src/service_health.py` (506 L) | Non-intrusive health report; no test push, no real search. Timing budget ladder: `_PROBE_TIMEOUT=4`, `_PROBE_CONCURRENCY=8`, `_FANOUT_BUDGET=8`, `_SUBSYSTEM_DEADLINE=10`, `_AGGREGATE_DEADLINE=14` — **the endpoint cannot hang** regardless of configuration. Probes: ChromaDB→sqlite-vec, SearXNG, ntfy, email, providers. `_safe_url()` strips userinfo/query/fragment; errors map to 9 controlled categories. All network calls injected so tests never touch the network | n/a | Med | Med | `odysseus-services/src/health.rs` | 20 | UNIMPLEMENTED |
| F359 | Readiness / health / version | `app.py:951-997`, `src/readiness.py` | `/api/health` (liveness), `/api/ready` (503 unless every critical subsystem is whole), `/api/version`, `/api/runtime` (docker detection, ollama URL) | n/a | Low | Low | `odysseus-web` | 0 | UNIMPLEMENTED |
| F360 | Request timeout middleware | `app.py:179-205` | 45 s hard cap → 504. **10 exempt prefixes**: `/api/chat`, `/api/shell/stream`, `/api/research`, `/api/model/download`, `/api/model/probe`, `/api/model-endpoints`, `/api/cookbook/setup`, `/api/upload`, `/api/image`, `/api/memory/audit` | n/a | Low | Med | `odysseus-web/src/middleware/timeout.rs` | 0 | UNIMPLEMENTED |
| F361 | Security headers & CSP | `core/middleware.py:85-152` | Per-request `secrets.token_hex(16)` nonce in `request.state.csp_nonce`; `X-Content-Type-Options`, `Referrer-Policy: no-referrer`, `Permissions-Policy: camera=(), microphone=(self), geolocation=()`; HSTS on https; **three CSP branches** (research reports, tool render, PDF preview `default-src 'none'` + `SAMEORIGIN`, default `X-Frame-Options: DENY` + nonce script-src) | n/a | Med | **Critical** | `odysseus-web/src/middleware/security.rs` | 0 | UNIMPLEMENTED |
| F362 | Client perf reporting | `app.py:960-986` | Low-volume frontend timing reports for stalls before SSE logs | logs | Low | Low | `odysseus-web` | 0 | UNIMPLEMENTED |

---

## Removed / replaced summary

| Item | Status | Reason |
|---|---|---|
| `services/faces/` | **REMOVED** | 1-line docstring, zero implementation — never existed |
| 15 route shim files | **REMOVED** | Pure re-export wrappers with regression tests |
| `gallery_people` | **REMOVED** | Referenced in the owner sweep; no table or model exists |
| `_event_pings_loop` | **REMOVED** | Never created in `start()` |
| `escalate_and_learn()` | **REMOVED** | Retired no-op returning `None` |
| `MAX_DEBATE_ROUNDS` | **REMOVED** | Vestigial after the `#3629` split |
| `ping_events` / `ping_notes` | **REMOVED** | Removed from the registry but still referenced |
| `SESSIONS_FILE` for chat sessions | **REMOVED** | Vestigial; chat sessions live in `app.db` |
| `src/config.py` | **REMOVED** | Superseded by `src/settings.py`; `validate_config()` ran at import |
| `GET /api/search` collision | **REPLACED** | Was message search (first-registered wins); becomes `/api/search/messages` + `POST /api/search/web` |
| Three SSRF policies | **REPLACED** | One configurable policy with three modes |
| DOM-click routing | **REPLACED** | Real `leptos_router` routes |
| Global `window.*` state | **REPLACED** | Leptos context + signals |
| Manual `?v=` cache-busting | **REPLACED** | Content-hashed bundles |
| `/api/image/denoise` | **REMOVED** | Real-ESRGAN; `resup` is a CLI not a library; no maintained Rust path |
| `/api/image/upscale-local` | **REMOVED** | Same |
| `/api/image/enhance-face` | **REMOVED** | GFPGAN; no identified Rust path |
| Local diffusion generation | **HOLD** | Scope decision pending (spike S9); the inpaint/harmonize proxy path may already cover the use case |