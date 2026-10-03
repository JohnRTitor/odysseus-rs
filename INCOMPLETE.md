# INCOMPLETE.md — Authoritative Status Register

**This file is authoritative.** It lists every feature that is `PARTIAL` or
`UNIMPLEMENTED`. It must be updated whenever implementation begins.

**A feature must not be removed from this file until it satisfies all 11 criteria in
`REWRITE_PLAN.md` §11 (Definition of Done).**

**Last updated:** planning complete, implementation not started.
**Implementation progress:** 0 of 24 phases.

---

## Status rules

| Status | In this file? |
|---|---|
| **IMPLEMENTED** | **No** — removed only after meeting the full Definition of Done |
| **PARTIAL** | **Yes** |
| **UNIMPLEMENTED** | **Yes** |
| **REMOVED** | Listed separately in §3 with a documented reason |
| **REPLACED** | Listed separately in §4 with old + new behaviour |

**Prohibited statuses** — none of these may be used to close an item:
"stubbed", "mocked", "placeholder", "coming soon", "TODO", "scaffolded", "wip",
"basic version", "simplified", "for now".

---

## 1. Summary

| Metric | Count |
|---|---|
| Total features inventoried | 300 |
| **IMPLEMENTED** | **0** |
| **PARTIAL** | **0** |
| **UNIMPLEMENTED** | **286** |
| **REMOVED** | 10 |
| **REPLACED** | 4 |
| Phases complete | 0 of 24 |
| Blocked by a failed spike | 4 (conditional) |

**Every feature is currently incomplete because implementation has not started.** That
is the honest state. This file shrinks as phases land.

---

## 2. UNIMPLEMENTED register

### 2.1 Blocked on a Phase 0 spike outcome

These have an *additional* dependency: a Phase 0 spike must pass first. If the spike
fails, the item becomes REMOVED with the spike result as the documented reason.

| Feature | Status | Missing | Blocked By | Planned Phase |
|---|---|---|---|---|
| F168 Image mask (SAM) | UNIMPLEMENTED | ONNX SAM inference in Rust | **Spike S3** — upstream `export_onnx_model.py` exports only prompt encoder + mask decoder, **not** the ViT image encoder. Needs a separate `image_encoder.onnx` conversion. `ort` EP availability unproven (S1) | 0 (S1,S3) → 17 |
| F169 Image remove-bg | UNIMPLEMENTED | Rust U2Net alpha-matte inference | **Spike S5** — `rembg-rs` 0.1.4 is minimally maintained (14 commits, 1 issue, 0 PRs). HOLD → REMOVE if it fails | 0 (S5) → 17 |
| F338 Kokoro local TTS | UNIMPLEMENTED | Native Kokoro inference | **Spike S4** — `kokoroxide` 0.1.5 is explicitly **WIP** (7 stars, 0 issues). If `any-tts` provides a Kokoro backend, this is resolved; otherwise `kokoroxide` becomes REMOVE | 0 (S4) → 22 |
| F176 Local diffusion generation | UNIMPLEMENTED | Full SD pipeline in Rust: CLIP tokenizer + `text_encoder` + `unet` + `vae_decoder` + scheduler port. **No ONNX equivalent exists for the scheduler** (~200–400 lines of stateful math) | **Spike S9** — scope decision. `/api/image/inpaint` and `/api/image/harmonize` already proxy to a registered `ModelEndpoint`, so a local SD pipeline may duplicate existing capability. `ort` is pinned to `2.0.0-rc.13` (an RC); TensorRT engine cache is required for the claimed 1.5–2.5 s | 0 (S9) → 17 |
| F124 Local embeddings | UNIMPLEMENTED | Rust embedding runtime replacing Python `fastembed` | **Spike S7** — `rusqlite` must have FTS5 **and** `sqlite-vec` must load as an extension. All 4 vector collections depend on this | 0 (S7) → 7 |
| F118 Vector threshold re-tuning | UNIMPLEMENTED | Re-tuned thresholds for the sqlite-vec distance metric | **Spike S11** — golden-set validation (`DATA_MIGRATION.md` §5.3). ChromaDB-calibrated values do not transfer | 0 (S11) → 7 |
| F140 PDF page rasterisation | UNIMPLEMENTED | PyMuPDF replacement | **Spike S10** — `mupdf`/`poppler-rs` FFI. Introduces a C dependency, partially undercutting the no-toolchain goal | 0 (S10) → 8 |
| F148 AcroForm fill + PDF export | UNIMPLEMENTED | AcroForm field population + annotation stamping | **Spike S10** — `lopdf`. Signature rows must stay owner-filtered; temp files must be unlinked after response | 0 (S10) → 8 |
| F262 Email IMAP/SMTP parity | UNIMPLEMENTED | Working Gmail OAuth IMAP + STARTTLS SMTP round-trip | **Spike S8** — `lettre` (SMTP), `async-imap` (IMAP), `mailparse` (MIME). Adequate in principle; must be proven against a real server | 0 (S8) → 15 |

### 2.2 Blocked on another phase

| Feature | Status | Missing | Blocked By | Planned Phase |
|---|---|---|---|---|
| F230 Calendar event extraction from email | UNIMPLEMENTED | Event extraction from email bodies | **Phase 15 (email)** — `builtin_actions.py:1518` | 16 |
| F208 Household email task actions | UNIMPLEMENTED | `summarize_emails`, `draft_email_replies`, `email_auto_translate`, `check_email_urgency`, `extract_email_events` | **Phase 15 (email)** — 5 of 12 `HOUSEKEEPING_DEFAULTS` | 13 |
| F204 Task output target: email | UNIMPLEMENTED | Email delivery of task results | **Phase 15** — `task_routes.py:982` | 13 |
| F203 Note reminder delivery via email | UNIMPLEMENTED | Email dispatch of note reminders | **Phase 15** — `note_routes.py:843` | 12 |
| F262b Agent email tools (16) | UNIMPLEMENTED | The 16 `BUILTIN_EMAIL_TOOLS` exposed to the agent | **Phase 15 + 19** — `tool_security.py:17` | 5→15 |
| F315 Codex email integration | UNIMPLEMENTED | `/api/codex/emails/*` (list, read, draft-document, draft, send) | **Phase 15** + the path-string re-invocation redesign | 21 |
| F316 Claude email integration | UNIMPLEMENTED | Same as F315 | **Phase 15** | 21 |
| F332 Contact resolution for email | UNIMPLEMENTED | `resolve_contact` | **Phase 15** | 15 |
| F256 Google OAuth for email | UNIMPLEMENTED | XOAUTH2 device/redirect flow. Callback is auth-exempt (signed `state`) | **Phase 15** | 15 |
| F084 Mid-loop skill toolset unlock | UNIMPLEMENTED | `requires_toolsets` promotion to the next round | **Phase 6** (tool security) + **Phase 14** (skills) | 14 |
| F221b CalDAV SSRF-hardened URL validation | UNIMPLEMENTED | Redirect-disabled CalDAV client with all-A/AAAA resolution | **Phase 10** (consolidated SSRF policy) | 16 |

### 2.3 All other features

Every remaining feature in `FEATURE_INVENTORY.md` is `UNIMPLEMENTED` because
implementation has not started. Grouped by phase for planning convenience; see
`FEATURE_INVENTORY.md` for full per-feature detail.

| Phase | Feature IDs | Count |
|---|---|---|
| 0 | F001, F002*, F003, F004, F006, F007, F008, F009, F010, F011, F012, F020, F021, F022, F023, F024, F025, F026, F027, F028, F029, F030, F031, F032, F033, F034, F035, F187, F293, F303, F359, F360, F361, F362 | 34 |
| 1 | F125 + remaining session CRUD under F200-series | see inventory |
| 2 | F090, F091, F102, F103, F104, F105, F319 | 7 |
| 3 | F040, F091, F092, F094, F097, F098, F101, F114, F115 | 9 |
| 4 | F041, F042, F043, F044, F045, F047, F048, F049, F050, F051, F054, F055, F056, F058, F060, F061, F062, F063, F085, F093, F094, F096, F099, F344, F356 | 25 |
| 5 | F064, F065, F066, F067, F068, F069, F070, F071, F072, F073, F083, F262b | 12 |
| 6 | F074, F075, F076, F077, F078, F079, F080, F081, F082 | 9 |
| 7 | F110, F111, F112, F113, F116, F117, F118, F119, F120, F121, F122, F123, F124, F357 | 14 |
| 8 | F130–F148, F345 | 20 |
| 9 | F150–F157, F341 | 9 |
| 10 | F180–F186, F188 | 8 |
| 11 | F190–F196 | 7 |
| 12 | F200–F203 | 4 |
| 13 | F204–F218, F350–F355 | 21 |
| 14 | F270–F280, F052, F053, F084 | 15 |
| 15 | F240–F262, F332 | 24 |
| 16 | F220–F232 | 13 |
| 17 | F158–F175, F166*, F167*, F170*, F171–F176 | 19 |
| 18 | cookbook serving (see inventory) | — |
| 19 | F262b, F320–F325 | 7 |
| 20 | F290–F293, F298–F299, F302, F304, F358 | 10 |
| 21 | F296, F297, F310–F318 | 10 |
| 22 | F005, F029b, F294, F295, F300, F301, F305, F330–F345, F357b | 20 |

*(`*` = status REPLACED or REMOVED; see §3/§4.)*

---

## 3. REMOVED — with documented reasons

Every removal states: what existed, why it is removed, and what replaces it (if
anything).

| ID | Item | What existed | Reason removed | Replaced by |
|---|---|---|---|---|
| — | `services/faces/` | A package with a single docstring line: *"Face detection + embedding service (standalone worker + helpers)."* Zero implementation. | **Never implemented.** There is no behaviour to preserve. | Nothing. If face detection is wanted later it is a new feature, not a migration. |
| — | 15 route shim files | `routes/X_routes.py` → `routes/x/X_routes.py` pure re-export wrappers | Pure indirection with no logic. Rust has no equivalent problem to work around. | Direct module paths. |
| — | `gallery_people` table | Referenced in the 19-table owner sweep (`database.py:1447`) | **No such table or model exists anywhere in the codebase.** The sweep reference is dead. | Nothing. The sweep statement is dropped. |
| — | `_event_pings_loop` | A per-owner `action_ping_events` loop every 10 min (`task_scheduler.py:624`) | **Never created in `start()`.** Unreachable code. | Nothing. Calendar reminders are Notes (`task_scheduler.py:552-558`). |
| — | `escalate_and_learn()` | `teacher_escalation.py:435` — a skill-distillation entry point | **Retired no-op** returning `None`. Replaced by the inline teacher flow, which writes `SKILL.md` directly. | `run_teacher_inline` (F357). |
| — | `MAX_DEBATE_ROUNDS` | `ai_interaction.py:31` constant, value 5 | Vestigial after the `#3629` split moved debate code out. Nothing reads it. | Nothing. |
| — | `ping_events` / `ping_notes` actions | Scheduled-task actions | Removed from `BUILTIN_ACTIONS` but still referenced by `_note_pings_loop`. Inconsistent. | Note due-dates + calendar reminders. |
| — | `SESSIONS_FILE` for chat sessions | `session_manager.py:76` — *"sessions_file kept for backward compat, not used"*; also passed into `app.py:682` and returned by a diagnostics route at `session_routes.py:938` | **Vestigial.** Chat sessions have always lived in `app.db`. | The `app.db` `sessions` table. `POST /api/sessions/save` becomes a documented no-op returning 200. |
| — | `src/config.py` | 208 lines of `pydantic-settings` | **Superseded** by `src/settings.py`. Its `validate_config()` ran at import (`config.py:208`), creating 4 directories as a side effect. Two config sources for one system is a bug source. | `src/settings.py` → `odysseus-store/src/settings.rs`. |
| — | `POST /api/auth/signup-toggle` | `auth_routes.py:635` | Explicitly deprecated in the source. Replaced by `PUT /api/auth/open-signup`. | `PUT /api/auth/open-signup`. |
| F166 | `/api/image/denoise` | Local Real-ESRGAN (`realesr-general-x4v3`, outscale=1) at `gallery_routes.py:1740` | **No maintained Rust path.** The only candidate, `resup`, is documented on docs.rs as *"not a library"* — it is a CLI frontend for `realesrgan-ncnn-vulkan`, so it cannot be linked. Binding ncnn/Vulkan directly is substantial FFI work with a GPU-capability requirement. | Nothing. `/api/image/inpaint` and `/api/image/harmonize` proxy to a registered `ModelEndpoint` and remain functional. |
| F167 | `/api/image/upscale-local` | Local Real-ESRGAN 2×/4× at `gallery_routes.py:1791` | Same as F166. | Same as F166. |
| F170 | `/api/image/enhance-face` | GFPGAN with a PIL fallback at `gallery_routes.py:2056` | **No identified Rust path.** No maintained crate exists. | Nothing. |

### 3.1 Conditional removals

These become REMOVED if the named spike fails. The evidence is recorded now so the
decision is not re-litigated later.

| ID | Item | Evidence | Becomes |
|---|---|---|---|
| F169 | Image remove-bg | `rembg-rs` 0.1.4 (Dec 2025); repository has **14 commits, 1 issue, 1 PR**. Too little evidence to call dependable. | REMOVE if S5 fails |
| F338 | Kokoro local TTS | `kokoroxide` 0.1.5 (Oct 2025); **explicitly marked WIP**; 29 commits, 7 stars, 1 open PR, 0 issues. | REMOVE if S4 fails and `any-tts` has no Kokoro backend |
| F176 | Local diffusion generation | SD is not one ONNX graph. Requires tokenizer + 4 exported graphs + a scheduler port. `ort` is at `2.0.0-rc.13` (an RC). | REMOVE if S9 concludes a local pipeline duplicates the existing proxy path |

---

## 4. REPLACED — old behaviour and new behaviour

| ID | Item | OLD behaviour | NEW behaviour | Reason |
|---|---|---|---|---|
| F002 | Client-side routing | No router. Boot-time `switch` on `location.pathname`; routes implemented as **synthetic DOM clicks** — `/cookbook` is `document.getElementById('tool-cookbook-btn')?.click()` (`app.js:1105-1234`). Deep links use `location.hash`. `history.replaceState` only, never `push` — **no back/forward navigation between sessions**. | `leptos_router` with real routes; `leptos_meta` for title/favicon/theme-color; push+replace history so browser back works. | A synthetic-click router cannot express nesting, params, or guards. The missing back button is a real UX defect this fixes. |
| F187 | Chat message search route | `GET /api/search` defined **twice** — `chat_routes.py:2695` (message search) and `search/search_routes.py:46`. FastAPI first-registered wins, so `GET` is message search and `POST` is web search. | `GET /api/search/messages` and `POST /api/search/web`. | The duplicate path is a latent bug. Disambiguating it is a fix, not a regression. |
| F185 | SSRF policy | **Three coexisting policies**: `url_safety.py` (permissive — loopback/LAN is the primary use case), `url_security.py` (strict, for API-token-supplied endpoints), and four separate pinned implementations in `outbound_fetch`, `webhook_manager`, `integrations`, `caldav_sync`, `skill_importer`. | One `net/ssrf.rs` policy with three modes (Permissive / Strict / Pinned) and an injectable resolver. | Three implementations of a security boundary means three chances to get it wrong. |
| — | Browser globals | **234 `window.*` writes.** 5 documented, ~229 more, including re-entrancy guards (`_suppressSidebarClose` ×14), pinned state (`_persistentChatSession` ×10), and cross-module function callbacks. | Leptos context + signals. | Untestable coupling. `sidebar-layout.js:5-13` deliberately defers its own export because a `let` binding cannot be mutated across module boundaries — a workaround for the wrong language. |
| — | Asset cache-busting | Manual `?v=YYYYMMDDlabel` query strings, kept in sync across every importer. A mismatch causes duplicate module instances. **`chatRenderer.js` currently loads 3×; 11 modules 2× each.** `app.js:42-46` documents the production incident. | Content-hashed filenames. | Structurally eliminates the bug class. Build-time duplicate detection. |
| — | Vector store | ChromaDB as an **external HTTP service** (`app.py:559-578`), lazily initialised, returning `None` when unreachable so routes 503 instead of busy-retrying. 4 collections. | `sqlite-vec` in-process. | Removes a whole service and its RSS; removes a startup ordering problem. **Cost:** thresholds must be re-validated (F118). |

---

## 5. Definition-of-Done gate

A row leaves this file only when **all eleven** hold:

1. Source functionality understood, edge cases documented
2. Target implementation exists
3. Frontend wired
4. Backend wired
5. Persistence / external integration wired
6. Real data flows end-to-end — **no mocks**
7. Error handling exists for the failure modes
8. Relevant edge cases addressed
9. Tests exist where appropriate
10. No placeholder implementation remains
11. Feature actually exposed to users

**Additional gates:**

| Gate | Requirement |
|---|---|
| Security | Every Group A security test passes (see `TEST_MIGRATION.md` §3.1) |
| Streaming | SSE event sequence matches the fixture byte-for-byte |
| Performance | No regression vs. `docs/baseline/` (`PERFORMANCE_PLAN.md` §7) |
| UI | SSR renders and hydration mounts for every route |
| Unmigrated siblings | Their controls render **disabled with a reason** |

---

## 6. Phase 23 gate — decommissioning the Python app

The Python application may be retired **only** when:

1. This file contains **zero** `UNIMPLEMENTED` and zero `PARTIAL` rows, **except**
   documented `REMOVED` items with accepted reasons.
2. Every `REMOVED` reason has been reviewed and accepted.
3. Every spike has a recorded outcome.
4. The full test suite passes.
5. A user has run the Rust app through a normal week of use.

**No phase may be marked complete by declaring features removed to make the count
reach zero.** That inverts the whole point of this file.

---

## 7. Change log

| Date | Change | By |
|---|---|---|
| Planning | Initial register created from the source audit at `0b66c720`. All 300 features classified; implementation not started. | plan |

**Update this file and `TASKS.md` in the same commit as any implementation work.**