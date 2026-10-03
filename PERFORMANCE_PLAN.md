# Odysseus — Performance Plan

**Optimisation target: local/LAN deployment.** Low idle RSS, low allocation overhead,
efficient CPU, fast startup, low request latency, efficient streaming, low browser-side
overhead, minimal unnecessary background work.

**Explicitly not optimised for:** multi-tenant scale, horizontal scaling, cold-start
autoscaling, multi-region. The source does not require them and optimising for them
would be dishonest scope creep.

---

## 0. Measurement rules

Every claim in this document follows the same three-part structure:

- **CURRENT** — evidence from source, with a file:line reference.
- **EXPECTED TARGET** — why Rust may improve it, or an explicit statement that it will
  not.
- **VALIDATION** — how it is actually measured.

**No memory or CPU numbers are asserted without measurement.** Where a number appears, it
is either measured from the source's own configuration or explicitly marked as a
hypothesis to be validated in Phase 0.

### 0.1 Baseline capture (Phase 0, blocking)

Before any optimisation claim, record:

```
# Idle RSS, no requests
python -c "import app; ..."   →  ps rss
# Under load
oha/hey/ab against a fixed endpoint mix
# Startup
time to first byte on GET /
# Streaming
p50/p95/p99 chunk latency during a 500-token agent run
```

**Deliverable:** `docs/baseline/` with the raw output committed. Any later "Rust is 3×
faster" claim must point at a file in that directory. Without this, the performance work
is unfalsifiable.

---

## 1. Hotspot inventory

Each classified by cause, per the requested taxonomy:
Python/runtime · framework · application logic · database · caching · model/AI
infrastructure · document processing · frontend · unnecessary polling · duplicated
state · unnecessary serialization · filesystem activity · network activity.

| # | Hotspot | Category | Severity |
|---|---|---|---|
| H1 | Import-time initialisation | Python runtime | **High** |
| H2 | 30+ unconditional background loops | Application logic / polling | **High** |
| H3 | SSE stream parsing | Serialization | Med |
| H4 | Tool-call text parsing | CPU (regex) | Med |
| H5 | JSON request/response | Serialization | Med |
| H6 | SQLAlchemy ORM per query | Database | Med |
| H7 | `SessionManager` in-RAM cache drift | Duplicated state | Med |
| H8 | 45 startup migration functions | Database / filesystem | Med |
| H9 | `builtin_actions` ChromaDB query per task run | AI infra | Med |
| H10 | Email 4-layer cache | Caching | Med |
| H11 | Browser polling (30 s / 20 s / 1.5 s) | Polling | **High** |
| H12 | 13 modules loaded twice; 1 loaded 3× | Frontend | **High** |
| H13 | `innerHTML` + full re-render during streaming | Frontend | **High** |
| H14 | 7.2 MB vendored UMD | Frontend | Med |
| H15 | 1.29 MB single CSS file | Frontend | Med |
| H16 | Web fetch downloads | Network / filesystem | Med |
| H17 | Search + content caches | Caching | Low |
| H18 | Task scheduler adaptive sleep | Polling | Low |
| H19 | 3GB+ vector store (ChromaDB process) | AI infra | **High** |
| H20 | Log volume | Filesystem | Low |
| H21 | Email pollers | Polling / network | Med |

---

## 2. Per-hotspot analysis

### H1 — Import-time initialisation · **Python runtime**

**CURRENT.** `core/database.py:2737` calls `init_db()` at **module import time**.
`core/__init__.py` re-exports `llm_call`, `llm_call_async`, `stream_llm`,
`list_model_ids`, `normalize_model_id`, `LLMConfig` from `src.llm_core` — so importing
`core.database` pulls in the entire LLM stack. `src/config.py:208` runs
`validate_config()` at import, creating 4 directories as a side effect.
`core/database.py:2061-2149` runs ~45 migration functions at startup.

**EXPECTED TARGET.** Rust has no import side effects and no dynamic module graph.
`bootstrap()` runs explicitly in `main`, and lazy statics (`OnceLock`) initialise only
what is touched. The migration chain becomes a numbered, forward-only list where each
step is a no-op on an up-to-date DB.

**VALIDATION.** Time from process start to first byte on `GET /`, cold and warm.
Expect the single largest improvement here.

---

### H2 — 30+ unconditional background loops · **application logic / polling**

**CURRENT.** `app.py:1056-1270` starts, unconditionally at startup:

| Loop | Interval | Gate |
|---|---|---|
| `upload_cleanup_func` | periodic | none |
| `start_bg_monitor` | 5 s | none |
| `_startup_mcp_connections` | once | `ODYSSEUS_DISABLE_MCP` |
| `_warmup_tool_index` | once | `ODYSSEUS_STARTUP_WARMUPS` (default **off**) |
| `_warmup_endpoints` | once | same |
| `_keepalive_loop` | 60 s | `ODYSSEUS_MODEL_KEEPALIVE` (default **off**) |
| `_null_owner_sweep_loop` | 3600 s | none |
| `_skill_audit_nightly_loop` | daily at ~02:00 | `skill_audit_nightly` setting |
| `cookbook_serve_lifecycle_loop` | periodic | none |
| `task_scheduler._loop` | adaptive 1–60 s | `ODYSSEUS_INPROCESS_TASKS` |
| `_note_pings_loop` | 60 s | none |
| email pollers | periodic | `ODYSSEUS_INPROCESS_POLLERS` |

Note the design intent already present: warmups and keepalive are **opt-in**, with a
comment stating they "compete with the first seconds of real UI use on slow or busy
machines". That reasoning applies to more of these loops than currently gated.

**EXPECTED TARGET.** Tokio tasks are individually cheap (≈2 KB stack), so the win is
**not** raw memory. The win is:
1. **Audit each loop.** Move genuinely-optional work behind settings, matching the
   existing `ODYSSEUS_STARTUP_WARMUPS` precedent.
2. Replace fixed-interval polling with event-driven notification where possible
   (see H11, H21).
3. The hourly owner sweep (`_null_owner_sweep_loop`) exists only to catch data created
   during a bypassed-auth window — **not applicable** in Rust. Remove; run once at
   migration.

**HONEST POSITION.** Removing background work is an **application-logic** win available
in any language. Claiming it as a Rust win would be dishonest. What Rust contributes is
making the remaining loops cheap enough that gating is optional.

**VALIDATION.** `tokio-console` task count and per-task poll counts at idle. Count of
`wakeups` per task per hour. Compare against the Python baseline.

---

### H3 — SSE stream parsing · **serialization**

**CURRENT.** Four hand-written streaming parsers, each doing per-chunk string
operations:
- ChatGPT Responses: `event:`-named SSE parsing (`llm_core.py:2670`)
- Ollama: line-delimited JSON (`:2803`)
- Anthropic: block-stream with `content_block_delta` / `input_json_delta` (`:2893`)
- OpenAI-compatible: accumulates `delta.tool_calls[]` into `_tc_acc[index]` (`:3029`)

Plus `_HarmonyStreamRouter` (`:290`) doing `<|channel|>` suffix matching with a
`_harmony_suffix_hold_len` partial-marker guard, and `_DegenerateStreamGuard` (`:360`)
scanning for ≥28 same-token repeats / 4-gram repeated ≥10 times / dominant token ≥78%
of a 96-token window.

**EXPECTED TARGET.** Byte-level scanning (`memchr`-style) instead of
`String::find` over copied chunks. Fewer allocations per chunk. The Gemini
omitted-`index` fix (§H3 note below) becomes a `HashMap`-free counter because the type
system can express it.

**HONEST POSITION.** These parsers are already correct and already tested. Rust gives
throughput and allocation improvement, **not** correctness. The existing
`test_llm_core_streaming.py`, `test_llm_core_sse_no_space.py`,
`test_anthropic_response_parse.py`, `test_llm_core_usage_finish_delta.py`,
`test_native_tool_result_threading.py` are the real gate.

**VALIDATION.** p50/p95/p99 chunk-processing latency during a 500-token stream, measured
in-process. Allocation count per chunk (`criterion` bench). tokio-console.

---

### H4 — Tool-call text parsing · **CPU**

**CURRENT.** `tool_parsing.py:1287` `parse_tool_blocks` runs up to 8 ordered regex
patterns, first-match-wins. Already hardened against ReDoS: `_iter_delimited` (`:1178`)
is a forward-only scan replacing `re.finditer`'s O(n²) lazy rescan, and
`_strip_think_blocks` (`agent_loop.py:1253`) is a linear reimplementation of a quadratic
regex. Dedicated tests exist (`test_redos_xml_tool_parsers.py`,
`test_redos_llm_parsers.py`, `test_redos_think_blocks.py`).

**EXPECTED TARGET.** The Rust `regex` crate has **no backtracking**, so the quadratic
class of bug is structurally impossible. A hand-written tokenizer for fenced blocks
would be faster still.

**HONEST POSITION.** Python already fixed the important problem here. Rust makes the fix
unforgettable rather than faster. **Keep the existing ReDoS tests as the gate.**

**VALIDATION.** `criterion` bench on the corpus in `tests/streaming/corpus.mjs`.
Fuzz with pathological inputs; assert linear time.

---

### H5 — JSON request/response · **serialization**

**CURRENT.** ~63 `/api/*` namespaces, ~490 endpoints, pydantic v2 validation on every
request and response.

**EXPECTED TARGET.** `serde` with `#[derive]` is materially faster and allocates less
than pydantic. Expect the largest relative improvement on list endpoints
(`/api/sessions`, `/api/notes`, `/api/gallery/library`, `/api/email/list`).

**VALIDATION.** `oha` (or `hey`) against a fixed endpoint mix, p50/p95/p99, 1 and 10
concurrent connections. Compare against the recorded Python baseline.

---

### H6 — SQLAlchemy ORM per query · **database**

**CURRENT.** 27 tables, ~90 indexes. Hot paths: `get_sessions_for_user` (in-RAM cache),
`sync_session_metadata` (a live `COUNT(*)` per call — deliberately not the denormalised
column, per the comment at `session_manager.py:457-465`), `get_session` (hydrate +
sync + re-hydrate + touch on **every** read).

**EXPECTED TARGET.** `rusqlite` prepared statements; no per-row object allocation for
read-only paths. Query planning identical — SQLite does the same work.

**HONEST POSITION.** The ORM overhead is real but modest next to SQLite itself. The
bigger issue is **H7**.

**ARCHITECTURAL CONSTRAINT (D11).** `rusqlite` is synchronous. **No DB call may run on
the async runtime.** Pool + `spawn_blocking`, or a dedicated DB thread with a channel.
Getting this wrong silently blocks the executor and is the single most likely cause of
"the Rust app is slower than Python".

**VALIDATION.** Bench `get_sessions_for_user` and `sync_session_metadata`. tokio-console
to confirm `spawn_blocking` depth stays bounded.

---

### H7 — `SessionManager` cache drift · **duplicated state**

**CURRENT.** `session_manager.py:86-219`. An in-RAM `Dict[str, Session]` caches session
**metadata**, loaded at boot for the 100 most-recently-accessed non-archived sessions.
`get_session` (`:421-452`) hydrates if absent, calls `sync_session_metadata` (a live
`COUNT(*)`), re-hydrates if `len(cached.history) < cached.message_count`, then bumps
`last_accessed` and **commits on every read**.

`_persist_message` (`:305-308`) **swallows errors**: `logger.error` + `db.rollback()`, no
re-raise. `sync_session_metadata` exists specifically to compensate for that drift.

**This is the clearest architectural problem in the source.** Denormalised
`message_count` drifts because writes can fail silently, so reads re-count to correct
it.

**EXPECTED TARGET.** Remove the duplication. SQLite is fast enough that the in-RAM cache
buys nothing on a local single-user workload. Make `message_count` authoritative-by-
construction: compute it in the same transaction as the insert, and return `Result` from
the write path instead of swallowing. Drop the per-read `COUNT(*)`.

**HONEST POSITION.** This is an application-logic fix, not a language win. But it is a
real latency and consistency improvement available in either language.

**VALIDATION.** Bench session list and history hydration. Assert `message_count` equals
`COUNT(*)` after a fault-injection run that forces write failures.

---

### H8 — 45 startup migration functions · **database / filesystem**

**CURRENT.** `init_db()` (`database.py:2061-2149`) runs ~45 functions at every startup,
most using `PRAGMA table_info` to check then `ALTER TABLE … ADD COLUMN`. Each opens its
own connection.

**EXPECTED TARGET.** One baseline migration (D6) that is a no-op on an up-to-date DB,
then a short forward-only chain. A single versioned row means startup does one indexed
read instead of 45 metadata queries.

**VALIDATION.** Startup time with a current DB vs. an empty DB. Assert the version row
check is O(1).

---

### H9 — ChromaDB query per task run · **AI infrastructure**

**CURRENT.** `tool_index.get_tools_for_query` (`tool_index.py:474`) hits ChromaDB on
**every agent turn**, wrapped in `asyncio.wait_for(timeout=1.5)`. Deliberately, on
timeout `_relevant_tools` is left `None` so the keyword fallback still runs
(`agent_loop.py:3922-3932`).

**EXPECTED TARGET.** sqlite-vec is an **in-process loadable extension** — no network
hop, no separate process. Removes both the latency and the timeout path.

**HONEST POSITION.** This is the strongest argument for the sqlite-vec decision, and it
is about latency and process count, not language.

**VALIDATION.** p50/p95 tool-selection latency. Compare against the Python baseline
including its timeout rate.

---

### H10 — Email 4-layer cache · **caching**

**CURRENT.** `email_routes.py:1517-1530`: `_LIST_CACHE` 8 s, `_READ_CACHE` 60 s,
`_IMAP_POOL`, plus a prefetch task.

**EXPECTED TARGET.** Bounded TTL caches with `Instant`-based expiry. Behaviour
identical; implementation cheaper.

**VALIDATION.** Cache hit rate + IMAP round-trip count under a representative session.

---

### H11 — Browser polling · **unnecessary polling**

**CURRENT.** Three timers fire with no user action:

| Poll | Interval | Source |
|---|---|---|
| Task notifications | 30 s (first fire at 1.5 s) | `tasks.js` `startNotificationPolling` |
| Research active jobs | 20 s | `research/jobs.js:43` |
| Research library sync | ≥120 s | `research/jobs.js:33` |
| Research SSE stream | 1.5 s | `research_routes.py:579` |
| Email unread | on demand | `emailInbox.js` |
| Activity heartbeat | continuous | `app.py:163` |

**EXPECTED TARGET.** Replace all four with `tokio::sync::broadcast` channels. The server
already knows when a task completes or a research round advances — the client should be
told, not ask. This removes idle network traffic entirely when the tab is open but idle.

**HONEST POSITION.** Also an application-logic win. But it is a large one for a
single-user local app, and it makes the LAN case much better.

**VALIDATION.** Count of HTTP requests per minute with a browser tab open and idle,
before vs after. Target: zero.

---

### H12 — Duplicate module instances · **frontend**

**CURRENT.** Manual `?v=YYYYMMDDlabel` cache-busting per importer. A mismatch makes the
browser load the same file as two separate module instances with separate state.
`app.js:42-46` documents the production incident (`cookbook.js` loaded twice, two
`_envState` objects, broken server selection). Currently live:
**`chatRenderer.js` loads 3×; 11 other modules load 2× each.**

`appConfig.js:6-12` documents the consequence: each of the three `chatRenderer`
instances issues its own `/api/tools` fetch.

**EXPECTED TARGET.** Content-hashed filenames. The entire bug class is structurally
impossible. Duplicate-instance detection is a build-time error.

**VALIDATION.** Assert in CI that no two emitted assets share a module graph node.
Runtime: count network requests on cold load before vs after.

---

### H13 — Streaming DOM churn · **frontend**

**CURRENT.** `streamingRenderer.js` is the best-engineered module in the repo and
already: renders frozen blocks once, appends to a stable text node inside an open fence
(append-mode), wraps only new characters in `.token-new`, self-heals an orphaned tail
marker, and latches to full re-render on any throw. `liveThinkingThrottle.js` coalesces
reasoning text to one DOM commit per 100 ms.

**EXPECTED TARGET.** Leptos's fine-grained reactivity naturally does only the minimal DOM
update. **Port these five behaviours exactly** (see `UI_MIGRATION.md` §3.3.1).

**HONEST POSITION.** The current implementation is already good. Rust is unlikely to
beat it by much. The win is maintainability, not speed.

**VALIDATION.** Long-task count and total scripting time during a 500-token stream.
Fuzz with `tests/streaming/invariant.test.mjs` ported to Rust.

---

### H14 — Vendored UMD libraries · **frontend**

**CURRENT.** `static/lib/` = 7.2 MB. Loaded: `highlight.min.js` eagerly;
**katex and mermaid (3.5 MB) lazily injected** — `index.html:239-244` records that the
lazy move saved ~985 KB/page. `docx`, `mammoth`, `xlsx`, `html2pdf`, `qrcode` injected
on demand. Service worker explicitly does **not** precache mermaid (`sw.js:91-95`).

**EXPECTED TARGET.** Bundler-managed chunks. Same lazy strategy. Tree-shaking may drop
unused vendor surface.

**VALIDATION.** Cold-load transfer size by resource type, before vs after.

---

### H15 — Single 41K-line CSS file · **frontend**

**CURRENT.** 1.29 MB `static/style.css`, unminified, no build step. ROADMAP.md calls it
"basically Calypso's island atm" and lists CSS cleanup as a refactor target.

**EXPECTED TARGET.** Tailwind purges unused utilities by default. Expected large
reduction. The 11 `--hl-*` variables and theme derivation must be preserved.

**VALIDATION.** Compiled CSS size. Visual regression on the pre-paint theme path.

---

### H16 — Web fetch downloads · **network / filesystem**

**CURRENT.** `outbound_fetch.py` soft cap 2 MB (`WEB_FETCH_SOFT_MAX_BYTES`), hard cap
20 MB. Forces `Accept-Encoding: identity` and **refuses any compressed response**
(`:313-319`) because a compressed body can decode past `Content-Length` and defeat the
streamed cap. Content cache in `data/cache/content/`.

**EXPECTED TARGET.** Streaming with a running byte cap, early reject on declared
`Content-Length`. Same semantics.

**HONEST POSITION.** Already well designed. Port the invariants, don't "improve" them.

**VALIDATION.** `test_web_fetch_size_caps.py` is the gate. Assert a gzip response is
refused.

---

### H17 — Search/content caches · **caching**

**CURRENT.** `CACHE_MAX_ENTRIES = 1000` with LRU; TTL 30 min for news queries, 24 h
otherwise (`query.py:145-149`). On-disk JSON in `data/cache/{search,content}`.

**EXPECTED TARGET.** Same. Note the cache is **empty on a Rust cold start** — the first
searches re-fetch. Acceptable; measure it.

---

### H18 — Task scheduler sleep · **polling**

**CURRENT.** `task_scheduler.py:673-699`: 10 s startup grace, then adaptive sleep
`clamp(min(next_run) − now, 1.0, 60.0)` so `* * * * *` fires within ~1 s of the boundary
rather than up to 60 s late.

**EXPECTED TARGET.** `tokio::time::sleep` with the same clamp, or a `Notify` woken by
`next_run` writes. The adaptive sleep is already correct — preserve it.

**HONEST POSITION.** Minor.

**VALIDATION.** Scheduling jitter for `* * * * *`. `test_scheduled_poll_race.py` and
`test_scheduler_restart_doublefire.py` are the gates.

---

### H19 — ChromaDB process RSS · **AI infrastructure**

**CURRENT.** ChromaDB runs as a **separate process** (`app.py:559-578` — lazily
initialised, returns `None` if unreachable). Its RSS is not Odysseus's RSS, but it *is*
the deployment's total memory.

**EXPECTED TARGET.** `sqlite-vec` is a loadable extension inside the Odysseus process.
The separate process disappears entirely.

**HONEST POSITION.** This is the clearest single memory win of the whole migration — and
it comes from **removing a service**, not from Rust. Also removes a startup ordering
problem: ChromaDB being unreachable currently produces a clean 503 rather than
busy-retrying (`app.py:561-568`).

**VALIDATION.** Total RSS of the deployment before vs after, with an identical corpus.

---

### H20 — Log volume · **filesystem**

**CURRENT.** RotatingFileHandler, 5 MB × 3 backups (`app.py:114-117`). Not
multi-process-safe. `_SlowRequestLogMiddleware` warns at 0.75 s
(`ODYSSEUS_SLOW_REQUEST_LOG_SECONDS`).

**EXPECTED TARGET.** `tracing` + `tracing-appender`, same rotation. Structured fields
instead of format strings.

**VALIDATION.** Log throughput at high request rates; no dropped records.

---

### H21 — Email pollers · **polling / network**

**CURRENT.** `email_pollers.py` (1,565 L) runs background IMAP polling, gated by
`ODYSSEUS_INPROCESS_POLLERS`. IMAP is high-latency; ROADMAP.md lists an email
performance audit as high priority.

**EXPECTED TARGET.** IMAP **IDLE** instead of polling, via `async-imap`. This is a
behavioural improvement Rust enables, not just a speed one. Must be optional — not all
servers support IDLE.

**HONEST POSITION.** Genuinely better. Requires care: IDLE needs reconnect handling and
a fallback to polling.

**VALIDATION.** IMAP round-trips per hour, before vs after. Reconnect correctness under
network loss.

---

## 3. Startup budget

| Stage | Target | Notes |
|---|---|---|
| Process start → migrations checked | < 50 ms | One versioned row read (H8) |
| → SQLite pool ready | < 20 ms | `PRAGMA foreign_keys=ON` per connection |
| → settings + auth loaded | < 20 ms | Two JSON reads |
| → Axum listening | **< 150 ms total** | |
| → first byte on `GET /` | **< 200 ms** | Includes SSR |
| MCP connect | **deferred**, post-listen | Already deferred today (`app.py:1067-1082`) |
| Email pollers | **deferred**, post-listen | |

The MCP deferral is already correct in the source and must be preserved: MCP servers can
be slow or blocked by local tooling, so connecting them after the server accepts traffic
avoids delaying the whole UI.

---

## 4. Idle RSS budget

| Component | Target | Notes |
|---|---|---|
| Binary + runtime | ~10–20 MB | Rust baseline |
| SQLite page cache | 5–20 MB | `PRAGMA cache_size` |
| sqlite-vec index | Proportional to corpus | |
| Per-Tokio-task overhead | ~2 KB × task count | |
| Session metadata cache | Bounded LRU, cap 100 | Replaces unbounded dict |
| Approval store | ≤ 2048 × ~2 KB | TTL-pruned |
| LLM response cache | Bounded LRU | |
| Tool index | In-process vectors | |
| **ChromaDB process** | **0** | **Eliminated (H19)** |
| **Total idle** | **Target < 100 MB** | vs. Python + ChromaDB |

The < 100 MB total is a **hypothesis to validate**, not a promise. The measurement
protocol is §0.1.

---

## 5. Optimisations deliberately NOT pursued

| Not doing | Why |
|---|---|
| Multiprocess / multi-worker | Single-user local app; adds IPC complexity for no gain |
| Connection pooling to a remote DB | SQLite is local |
| Redis / external cache | Unnecessary at this scale |
| CDN / edge caching | LAN and localhost |
| Reducing DB indexes | Queries are already fast; correctness of the FTS path matters more |
| Micro-optimising `format_tool_result` | Capped at 8000 chars; not a hot path |
| Rewriting the markdown regex pipeline for speed | Correctness and the deep-link contract matter more; `pulldown-cmark` + hand-written passes for steps 1–3 |
| Async-ifying SQLite | D11 mandates `spawn_blocking`; async SQLite would add a dependency for no local benefit |

---

## 6. Frontend performance

| Metric | Current | Target | Validation |
|---|---|---|---|
| Cold-load transfer | 6.23 MB JS + 1.29 MB CSS unminified | Minified + purged + chunked | Bundle size report in CI |
| Duplicate module instances | 13 modules | 0 | Build-time assertion |
| Streaming long tasks | Baseline from `PerformanceObserver` | No regression | `tests/streaming/invariant` fuzz |
| DOM ops per streamed token | Full re-render of the live tail | Minimal reactive update | Same |
| Reasoning DOM commits | Coalesced at 100 ms | Same | Same |
| Idle polling requests | ~4/min (H11) | **0** | Request count, idle tab |
| FOUC | Prevented by inline script | Preserved | Visual test |

---

## 7. Performance acceptance criteria

A performance claim is accepted only when:

1. A baseline exists in `docs/baseline/` for the same workload.
2. The same measurement tool and workload produced both numbers.
3. The workload is stated (endpoint mix, concurrency, stream length).
4. At least 5 runs; the median reported.
5. For memory: idle RSS **and** peak RSS under load.
6. The claim names the hotspot ID it addresses.

**A performance regression in a migrated feature blocks that feature's DONE status**,
per `REWRITE_PLAN.md` §11.