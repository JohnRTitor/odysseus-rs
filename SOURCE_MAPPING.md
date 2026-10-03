# Odysseus — Source-to-Target Mapping

Every meaningful current module and its intended destination, with the reasoning for
why it belongs there.

**Concern taxonomy used throughout:**

| Concern | Meaning | Crate rule |
|---|---|---|
| **UI** | Presentation only | `odysseus-ui` may not contain SQL, HTTP client code, or business rules |
| **Web/transport** | HTTP framing, routing, extraction, middleware, SSE encoding | `odysseus-web` may not contain business rules |
| **Application/domain** | Business orchestration and rules | `odysseus-services`, `odysseus-agent`, `odysseus-tools` |
| **Domain core** | Pure types, vocabularies, invariants | `odysseus-domain` has zero IO |
| **Infrastructure** | External systems, processes, network | `odysseus-integrations`, `odysseus-llm`, `odysseus-ml` |
| **Persistence** | Schema, queries, migrations, transactions | `odysseus-store` has no HTTP |

**Principle:** the current architecture is *not* reproduced in Rust. The Python tree is
organised by "which file happens to hold this", with module-level singletons threaded
through `set_*` functions. The Rust tree is organised by *concern*, with dependencies
injected.

---

## 1. `app.py` (1,306 lines) → `apps/odysseus-server` + `odysseus-web`

| Current | Target | Concern | Why |
|---|---|---|---|
| `app = FastAPI(...)` `:128` | `apps/odysseus-server/src/main.rs` + `odysseus-web/src/lib.rs` | Web | Framework composition root. Rust splits: process entry vs. router builder. |
| `register_static_mime_types()` `:18-31` | **REMOVED** | — | Windows registry workaround. Rust's MIME detection is not affected. |
| `WindowsProactorEventLoopPolicy` `:14-15` | **REMOVED** | — | asyncio-specific. Tokio has no equivalent problem. |
| `CORS` middleware `:137-153` | `odysseus-web/src/middleware/cors.rs` | Web | Config-driven allowlist. |
| `GZipMiddleware` `:163` | tower-http `CompressionLayer` | Web | Same `text/event-stream` exclusion required. |
| `SecurityHeadersMiddleware` `:166` | `middleware/security.rs` | Web | Carries the per-request CSP nonce. **Port the three CSP branches verbatim.** |
| `_RequestTimeoutMiddleware` `:194-205` | `middleware/timeout.rs` | Web | 45 s + the 10 exempt prefixes are load-bearing. |
| `_InteractiveActivityMiddleware` `:214-222` | **REMOVED** — folded into `odysseus-services/src/interactive_gate.rs` | Application | It calls `task_scheduler.stop_background_tasks_for_foreground`. That is application logic, not transport. |
| `_SlowRequestLogMiddleware` `:229-251` | `middleware/slow_log.rs` | Web | Diagnostic only. |
| `AuthMiddleware` `:364-485` | `middleware/auth.rs` | Web | The gate order is a security invariant. Port the order, not the code shape. |
| `_token_cache` `:298-337` | `odysseus-store/src/tokens.rs` | Persistence | A cache over DB rows. Belongs with the data it caches. |
| `_is_trusted_loopback` `:348-362` | `middleware/auth.rs` | Web | Security-critical, transport-adjacent. |
| `app.mount("/static", _RevalidatingStatic)` `:510` | `odysseus-web/src/routes/assets.rs` | Web | Replaced by content-hashed filenames — `no-cache` becomes unnecessary. |
| `/api/generated-image/{filename}` `:513` | `routes/generated_image.rs` | Web | Response shaping + owner check. |
| ~40 `include_router()` calls `:642-864` | `odysseus-web/src/router.rs` | Web | Becomes a declarative route tree. |
| All `@app.get("/")`, `/notes`, `/calendar`, … `:892-949` | `odysseus-ui` shell routes | UI | Leptos routes replace path→DOM-click dispatch. |
| `_startup_event()` `:1033-1272` | `apps/odysseus-server/src/bootstrap.rs` | Application | 30+ background loops → an explicit, audited task list. |
| `_shutdown_event()` `:1274-1297` | `apps/odysseus-server/src/bootstrap.rs` | Application | |
| `uvicorn.run(...)` `:1300-1306` | `main.rs` | Web | `APP_BIND` / `APP_PORT` preserved. |

---

## 2. `core/` → `odysseus-store` + `odysseus-domain` + `odysseus-web`

| Current | Target | Concern | Why |
|---|---|---|---|
| `core/database.py` (2,737 L) — 27 models | `odysseus-store/src/schema/` | Persistence | One module per table group. |
| ↳ `init_db()` + 45 `_migrate_*` `:2061-2149` | `odysseus-store/src/migrations/` | Persistence | Single baseline from live `sqlite_master`, then forward-only. |
| ↳ `EncryptedText` TypeDecorator `:150-172` | `odysseus-store/src/crypto.rs` | Persistence | Transparent encrypt/decrypt at the column boundary. |
| ↳ `get_db()` / `get_db_session()` `:2568-2593` | `odysseus-store/src/pool.rs` | Persistence | Becomes a `Pool<Connection>` + D11 `spawn_blocking` discipline. |
| ↳ `get_upcoming_events()` `:2698` | `odysseus-services/src/calendar.rs` | Application | **Documented tenant-isolation footgun**: `owner=None` means no scoping at all. Fix rather than port. |
| ↳ `chat_messages_fts` + 3 triggers `:2178-2261` | `odysseus-store/src/search.rs` | Persistence | Triggers must keep scrubbing base64 media from the index. |
| ↳ `lock_email_account_owner_mutations()` `:474-517` | `odysseus-store/src/email_locks.rs` | Persistence | `BEGIN IMMEDIATE` on SQLite; `FOR UPDATE` on row-locking DBs. |
| `core/auth.py` (680 L) | `odysseus-store/src/auth.rs`, `odysseus-web/src/auth.rs` | Persistence + Web | Split: user/token **storage** vs. request **verification**. |
| ↳ `AuthManager` singleton | **REMOVED** — `AppState.auth: Arc<AuthStore>` | — | No module-level singletons. |
| ↳ `DEFAULT_PRIVILEGES` / `ADMIN_PRIVILEGES` `:24-48` | `odysseus-domain/src/privileges.rs` | Domain core | Pure data. |
| ↳ TOTP `:488-562` | `odysseus-store/src/totp.rs` | Persistence | Fails closed when enabled-but-no-secret. Preserve. |
| `core/session_manager.py` (782 L) | `odysseus-services/src/sessions.rs` + `odysseus-store/src/sessions.rs` | Application + Persistence | The manager held both an in-RAM cache and persistence. Split. |
| ↳ `self.sessions: Dict[str, Session]` cache | `odysseus-services/src/session_cache.rs` | Application | An LRU metadata cache in front of SQLite, not a source of truth. |
| ↳ `_persist_message()` `:244-309` | `odysseus-store/src/messages.rs` | Persistence | Preserve: upload-ref reservation, base64 stripping, `last_message_at` bump only here. |
| ↳ **error swallowing** `:305-308` | **REMOVED — fix** | — | `logger.error` + `rollback()` with no re-raise is exactly the drift `sync_session_metadata` exists to compensate for. Return `Result`. |
| `core/models.py` (191 L) | `odysseus-domain/src/models.rs` | Domain core | `ChatMessage`/`Session` become plain data. The module-level `SessionManager` singleton is removed. |
| ↳ `get_context_messages()` `:153-183` | `odysseus-agent/src/context.rs` | Application | Excludes `metadata.source == "slash"`; carries an HMAC-verified approval grant forward. |
| `core/middleware.py` (152 L) | `odysseus-web/src/middleware/` | Web | |
| ↳ `INTERNAL_TOOL_TOKEN` `:20-21` | `odysseus-web/src/auth.rs` | Web | Per-process, never persisted. |
| ↳ `require_admin()` `:57-82` | `odysseus-web/src/auth.rs` | Web | |
| `core/constants.py` | **REMOVED** | — | 12-line re-export shim of `src/constants.py`. |
| `core/atomic_io.py` (66 L) | `odysseus-store/src/atomic.rs` | Persistence | uuid-suffixed temp file + `fsync` + `os.replace` → `NamedTempFile` + `sync_all` + `rename`. |
| `core/exceptions.py` | `odysseus-domain/src/error.rs` | Domain core | 4 exception types → a typed error enum with HTTP status mapping. |
| `core/log_safety.py` | `odysseus-services/src/log.rs` | Application | `redact_url` shared by every error path. |
| `core/platform_compat.py` (452 L) | `odysseus-services/src/platform.rs` | Infrastructure | `safe_chmod`, detached-subprocess kwargs, `pid_alive`, `find_bash`. |
| `core/__init__.py` | **REMOVED** | — | Re-exports the LLM stack, so importing `core.database` pulled in the whole AI layer. Rust has no import side effects. |

---

## 3. `src/` → multiple crates

### 3.1 Agent & LLM → `odysseus-agent` + `odysseus-llm`

| Current | Target | Concern | Why |
|---|---|---|---|
| `src/agent_loop.py` (6,455 L) | `odysseus-agent/src/loop.rs` | Application | The single ~3,000-line generator decomposes into: `security.rs`, `classify.rs`, `selection.rs`, `route_state.rs`, `round_loop.rs`, `finalize.rs`. Closures become methods on a `RunState` struct. |
| ↳ `_build_system_prompt()` `:2226` | `odysseus-agent/src/system_prompt.rs` | Application | |
| ↳ `_resolve_tool_blocks()` `:2944` | `odysseus-agent/src/resolve.rs` | Application | |
| ↳ `_append_tool_results()` `:2995` | `odysseus-agent/src/threading.rs` | Application | The `None`-vs-`""` and most-recent-reasoning rules must survive exactly. |
| ↳ `_compute_final_metrics()` `:3121` | `odysseus-agent/src/metrics.rs` | Application | |
| ↳ `_run_verifier_subagent()` `:3287` | `odysseus-agent/src/verifier.rs` | Application | |
| ↳ `_classify_agent_request()` `:1390`, `_detect_admin_intent()` `:1088` | `odysseus-agent/src/intents.rs` | Application | |
| ↳ `TOOL_SECTIONS` (70 blocks) `:560` | `odysseus-agent/src/prompt/tool_sections.rs` | Domain core | Pure text data. |
| ↳ `_DOMAIN_RULES` / `_DOMAIN_TOOL_MAP` `:468,:529` | `odysseus-agent/src/prompt/domains.rs` | Domain core | |
| ↳ loop breaker `:5556-5621` | `odysseus-agent/src/breaker.rs` | Application | |
| ↳ intent-nudge supervisor `:5479-5551` | `odysseus-agent/src/nudge.rs` | Application | |
| `src/llm_core.py` (3,730 L) | `odysseus-llm/src/` | Infrastructure | |
| ↳ `_detect_provider()` `:966`, `_provider_label()` `:1115`, `_provider_headers()` `:1097` | `odysseus-llm/src/detect.rs` | Infrastructure | Host-exact matching. |
| ↳ `llm_call` `:1969` / `llm_call_async` `:2262` | `odysseus-llm/src/client.rs` | Infrastructure | |
| ↳ `_stream_llm_inner` `:2582` | `odysseus-llm/src/stream/mod.rs` | Infrastructure | Emits `odysseus-domain::events::StreamEvent`. |
| ↳ 4 parsers `:2670,:2803,:2893,:3029` | `odysseus-llm/src/stream/{chatgpt,ollama,anthropic,openai}.rs` | Infrastructure | Keep Gemini's omitted-`index` allocation fix. |
| ↳ `stream_llm_with_fallback` `:3511` | `odysseus-llm/src/fallback.rs` | Infrastructure | Metadata-commit rule is the critical invariant. |
| ↳ `_local_model_slot` `:82` | `odysseus-llm/src/slot.rs` | Application | Priority gate, not transport. |
| ↳ dead-host cooldown `:483-497` | `odysseus-llm/src/health.rs` | Infrastructure | |
| ↳ response cache `:177,:196` | `odysseus-llm/src/cache.rs` | Infrastructure | Credential-identity hashing preserved. |
| ↳ `_sanitize_llm_messages` `:1673` | `odysseus-llm/src/sanitize.rs` | Domain core | Pure function over messages. |
| ↳ `_HarmonyStreamRouter` `:290` | `odysseus-llm/src/stream/harmony.rs` | Infrastructure | gpt-oss `<|channel|>` routing. |
| ↳ `_DegenerateStreamGuard` `:360` | `odysseus-llm/src/stream/degenerate.rs` | Infrastructure | ≥28 same-token repeats / 4-gram ≥10 / dominant token ≥78% of 96. |
| ↳ Kimi Code UA pool `:811-964` | `odysseus-llm/src/kimi.rs` | Infrastructure | |
| `src/chat_processor.py` | `odysseus-agent/src/preface.rs` | Application | The KV-cache rule lives in the module docstring. Must be preserved as code comments + a test. |
| `src/chat_handler.py` | `odysseus-services/src/chat_preprocess.rs` | Application | YouTube + uploads + VL fallback in parallel. |
| `src/chat_helpers.py` | `odysseus-llm/src/validate.rs` + `odysseus-services/src/chat_preprocess.rs` | Domain + Application | `extract_urls` needs balanced-paren preservation for Wikipedia. |
| `src/context_budget.py`, `src/context_compactor.py` | `odysseus-agent/src/{budget,compaction}.rs` | Application | Speculative per-route compaction. |
| `src/tool_index.py` (629 L) | `odysseus-agent/src/selection.rs` + `odysseus-store/src/vectors.rs` | Application + Persistence | 1.5 s timeout → `tokio::time::timeout`; on timeout leave `None` so the keyword fallback runs. |
| `src/foreground_model_routing.py` | `odysseus-llm/src/routing.rs` | Infrastructure | |
| `src/tool_parsing.py` (1,525 L) | `odysseus-tools/src/parsing.rs` | Domain core | 8 ordered patterns. `regex` crate has no backtracking, so ReDoS is structurally impossible. |
| `src/tool_schemas.py` (1,601 L) | `odysseus-tools/src/schemas.rs` | Domain core | 71 schemas as static data. |
| `src/tool_execution.py` (1,417 L) | `odysseus-tools/src/{envelope,dispatch,paths,format}.rs` | Application | `execute_tool_block` requires a typed `SecurityContext` — enforce at the type level. |
| `src/tool_capabilities.py` (708 L) | `odysseus-tools/src/{capabilities,integrity,run_context}.rs` | Domain + Application | |
| `src/tool_approvals.py` (513 L) | `odysseus-agent/src/approvals.rs` | Application | Sealed digest + one-shot claim. |
| `src/tool_approval_scopes.py` | `odysseus-agent/src/approval_scopes.rs` | Application | HMAC session-scope grants. |
| `src/tool_policy.py`, `src/tool_security.py`, `src/tool_utils.py` | `odysseus-tools/src/policy.rs` | Domain + Application | |
| `src/teacher_escalation.py` (810 L) | `odysseus-agent/src/teacher.rs` | Application | |
| `src/action_intents.py` | `odysseus-agent/src/intents.rs` | Application | |
| `src/context_budget.rs`/`model_context.py` | `odysseus-agent/src/budget.rs` | Application | |

### 3.2 Agent tool implementations → `odysseus-tools`

| Current | Target | Why |
|---|---|---|
| `src/agent_tools/__init__.py` — `TOOL_HANDLERS` `:37`, `TOOL_TAGS` `:79`, `MAX_AGENT_ROUNDS = 50` `:74` | `odysseus-tools/src/registry.rs` | One registry, one trait: `Tool { schemas, tags, execute(ctx, args) -> Result }`. Removes the 3-surface inconsistency. |
| `filesystem_tools.py` (1,140 L) | `odysseus-tools/src/fs/{read,write,edit,patch,ls,glob,grep}.rs` | `_EmptyBodyWouldTruncate` guard and the hard-link staging trick must survive. |
| `document_tools.py` (894 L) | `odysseus-tools/src/document.rs` | |
| `admin_tools.py` (804 L) | `odysseus-tools/src/admin.rs` | `_mcp_allowed_commands` allowlist. |
| `session_tools.py` (560 L) | `odysseus-tools/src/session.rs` | |
| `subprocess_tools.py` (400 L) | `odysseus-tools/src/subprocess.rs` | tmux marker slicing + `rc` extraction. |
| `web_tools.py`, `model_interaction_tools.py`, `interaction_tools.py`, `coding_tools.py`, `bg_job_tools.py` | `odysseus-tools/src/{web,model,interaction,coding,bgjob}.rs` | |
| `src/tools/cookbook.py` (1,713 L), `system.py`, `calendar.py`, `notes.py`, `vault.py`, `contacts.py`, `research.py`, `search.py`, `image.py` | `odysseus-tools/src/domains/*.rs` | |
| `src/tool_implementations.py` (shim) | **REMOVED** | Re-export indirection. |
| `src/builtin_actions.py` (3,436 L) | `odysseus-services/src/actions.rs` | **Not the agent tool layer** — this is the scheduled-task automation registry. Different concern, different crate. |
| `src/builtin_mcp.py` | `odysseus-integrations/src/builtin_mcp.rs` | |

### 3.3 MCP → `odysseus-integrations`

| Current | Target | Why |
|---|---|---|
| `src/mcp_manager.py` (709 L) | `odysseus-integrations/src/mcp/{manager,transport,schema,params}.rs` | stdio/SSE/HTTP transports; `_generation` counter; builtin auto-reconnect. |
| `src/mcp_oauth.py` | `odysseus-integrations/src/mcp/oauth.rs` | RFC 9728 + DCR + PKCE. |
| `mcp_servers/*.py` (3,541 L) | **REMOVED as subprocess servers** | The builtin email/memory/rag/image servers become native Rust tools. `email_server.py` (2,912 L) → `odysseus-services/src/email/tools.rs`. |
| `mcp_servers/email_server.py` `email_*` tools | `odysseus-services/src/email/tools.rs` | 16 tools; `BUILTIN_EMAIL_TOOLS` stays the single source of truth. |

### 3.4 Persistence-adjacent services

| Current | Target | Concern | Why |
|---|---|---|---|
| `src/rag_manager.py`, `rag_vector.py`, `rag_singleton.py` | `odysseus-services/src/personal_docs.rs` | Application | ChromaDB client → `sqlite-vec`. |
| `src/chroma_client.py` | **REMOVED** | — | External ChromaDB HTTP client. |
| `src/memory_vector.py`, `memory.py`, `memory_provider.py` | `odysseus-services/src/memory.rs` + `odysseus-store/src/vectors.rs` | Application + Persistence | |
| `src/embeddings.py`, `embedding_lanes.py` | `odysseus-store/src/embed.rs` | Persistence | `fastembed` → Rust ONNX. |
| `src/upload_handler.py` (58 KB) | `odysseus-services/src/uploads.rs` | Application | |
| `src/document_processor.py` (607 L) | `odysseus-agent/src/multimodal.rs` + `odysseus-services/src/docs/` | Application | `build_user_content` is agent-facing. |
| `src/personal_docs.py` (487 L) | `odysseus-services/src/personal_docs.rs` | Application | |
| `src/document_actions.py` | `odysseus-services/src/document_tidy.rs` | Application | |
| `src/session_search.py` | `odysseus-store/src/search.rs` | Persistence | |
| `src/session_actions.py`, `session_image_cleanup.py` | `odysseus-services/src/sessions.rs` | Application | |
| `src/settings.py` | `odysseus-store/src/settings.rs` | Persistence | |
| `src/settings_scrub.py` | `odysseus-store/src/settings.rs` | Persistence | |
| `src/preset_manager.py` | `odysseus-store/src/presets.rs` | Persistence | |
| `src/cleanup_service.py` | `odysseus-services/src/cleanup.rs` | Application | |
| `src/config.py` | **REMOVED** | — | Superseded; ran at import. |
| `src/constants.py` | `odysseus-domain/src/constants.rs` | Domain core | |
| `src/exceptions.py`, `runtime_paths.py`, `text_helpers.py`, `owner_identity.py`, `owner_identity` | `odysseus-domain/src/`, `odysseus-services/src/paths.rs` | Domain + Application | |

### 3.5 Network, security & integrations

| Current | Target | Concern | Why |
|---|---|---|---|
| `src/outbound_fetch.py` (354 L) | `odysseus-services/src/net/fetch.rs` | Infrastructure | The canonical SSRF-safe capped fetcher. `Accept-Encoding: identity` + compressed-response refusal preserved. |
| `src/url_safety.py` (108 L) | `odysseus-services/src/net/ssrf.rs` | Infrastructure | **Merged** with `url_security.py` into one policy with modes. |
| `src/url_security.py` (94 L) | `odysseus-services/src/net/ssrf.rs` | Infrastructure | Strict mode. |
| `src/prompt_security.py` (99 L) | `odysseus-agent/src/prompt_security.rs` | Domain | `UNTRUSTED_CONTEXT_POLICY`. |
| `src/webhook_manager.py` (455 L) | `odysseus-integrations/src/webhooks.rs` | Infrastructure | |
| `src/integrations.py` (810 L) | `odysseus-services/src/integrations.rs` | Application | 8 presets + `execute_api_call`. |
| `src/event_bus.py` | `odysseus-services/src/events.rs` | Application | Threshold-counter bus, not pub/sub. |
| `src/rate_limiter.py`, `src/secret_storage.py`, `src/api_key_manager.py` | `odysseus-services/src/{ratelimit,secrets,keys}.rs` | Application | |
| `src/outbound_fetch` SSRF resolver injection | `odysseus-services/src/net/resolver.rs` | Infrastructure | Injectable resolver is what makes tests DNS-free. Preserve. |

### 3.6 Scheduling & background work

| Current | Target | Concern | Why |
|---|---|---|---|
| `src/task_scheduler.py` (2,675 L) | `odysseus-services/src/tasks/{mod,loop,execute,run,checkin,cron}.rs` | Application | 2,675 lines → 6 modules around an explicit `Scheduler` struct. |
| ↳ `compute_next_run()` `:113-230` | `tasks/cron.rs` | Domain | Pure function. 6 schedule kinds, tz-aware, fails closed. |
| ↳ `_execute_task_locked()` `:816-1174` | `tasks/execute.rs` | Application | |
| ↳ `_run_agent_loop()` `:1858-2012` | `tasks/run.rs` | Application | Approval-denied boundary. |
| ↳ `_execute_checkin()` `:1331-1508` | `tasks/checkin.rs` | Application | |
| ↳ `_cached()` single-flight `:65-110` | `tasks/cache.rs` | Application | `asyncio.shield` → `tokio::sync::OnceCell`-style shared future. |
| `src/interactive_gate.py` | `odysseus-services/src/interactive_gate.rs` | Application | |
| `src/bg_jobs.py` | `odysseus-services/src/bg_jobs.rs` | Application | |
| `src/bg_monitor.py` | `odysseus-services/src/bg_monitor.rs` | Application | |
| `src/cookbook_serve_lifecycle.py` | `odysseus-services/src/cookbook/lifecycle.rs` | Application | |

### 3.7 AI interaction & models

| Current | Target | Concern | Why |
|---|---|---|---|
| `src/ai_interaction.py` (1,489 L) | `odysseus-agent/src/interaction/{pipeline,memory,rag,ui_control,image}.rs` | Application | `do_ui_control` is a **control plane** — the LLM manipulates the UI. |
| ↳ `dispatch_ai_tool` `:1466` | `odysseus-agent/src/interaction/mod.rs` | Application | |
| `src/model_discovery.py` | `odysseus-services/src/discovery.rs` | Application | |
| `src/model_capabilities.py` + `model_capability_readers/` | `odysseus-llm/src/capabilities.rs` | Infrastructure | |
| `src/endpoint_resolver.py` (672 L) | `odysseus-llm/src/resolve.rs` | Infrastructure | Its stated purpose is de-duplication. Preserve. |
| `src/deep_research.py`, `research_handler.py`, `research_utils.py` | `odysseus-services/src/research/{engine,handler,utils}.rs` | Application | |
| `src/visual_report.py` (1,933 L) | `odysseus-services/src/report.rs` | Application | `nh3` → `ammonia`. |
| `src/copilot.py`, `chatgpt_subscription.py` | `odysseus-integrations/src/{copilot,chatgpt}.rs` | Infrastructure | |
| `src/user_time.py` | `odysseus-domain/src/time.rs` | Domain core | |

### 3.8 Content, media & remaining `src/`

| Current | Target | Concern | Why |
|---|---|---|---|---|
| `src/pdf_forms.py`, `pdf_form_doc.py`, `pdf_runtime.py`, `office_doc.py`, `markitdown_runtime.py` | `odysseus-services/src/pdf/{forms,office,convert}.rs` | Application | `lopdf` / `mupdf` / `poppler`. |
| `src/attachment_refs.py`, `generated_images.py`, `image_model_ids.py` | `odysseus-domain/src/attachments.rs` | Domain | |
| `src/agent_runs.py` | `odysseus-services/src/agent_runs.rs` | Application | Detached run registry. |
| `src/index_walk.py`, `topic_analyzer.py`, `question_type_detection` | `odysseus-services/src/{walk,topic}.rs` | Application | |
| `src/tls_overrides.py` | `odysseus-llm/src/tls.rs` | Infrastructure | |
| `src/optional_deps.py` | **REMOVED** | — | Python optional-import shim. Rust uses cargo features. |
| `src/goal_based_extractor.py`, `reminder_personas.py` | `odysseus-services/src/{}` | Application | |
| `src/host_docker_access.py` | `odysseus-services/src/docker.rs` | Infrastructure | |
| `src/service_health.py` (506 L) | `odysseus-services/src/health.rs` | Application | Timing budget ladder preserved. |

---

## 4. `services/` → `odysseus-services`

| Current | Target | Concern | Why |
|---|---|---|---|
| `services/search/providers.py` | `odysseus-services/src/search/providers.rs` | Infrastructure | 7 provider impls. |
| `services/search/core.py` | `odysseus-services/src/search/mod.rs` | Application | Chain × attempts → rank → filter → parallel fetch. |
| `services/search/content.py` | `odysseus-services/src/fetch.rs` | Infrastructure | |
| `services/search/ranking.py` | `odysseus-services/src/search/ranking.rs` | Domain core | Pure scoring. |
| `services/search/query.py` | `odysseus-services/src/search/query.rs` | Application | |
| `services/search/{cache,analytics,service}.py` | `odysseus-services/src/search/{cache,analytics}.rs` | Application | |
| `services/shell/` | `odysseus-services/src/shell.rs` | Application | **Consolidates** the duplicate `src/tools/system.py` path. |
| `services/stt/` | `odysseus-services/src/stt.rs` | Application | `whisper-rs`. |
| `services/tts/` | `odysseus-services/src/tts.rs` | Application | `any-tts`. |
| `services/docs/` | `odysseus-services/src/personal_docs.rs` | Application | |
| `services/memory/` | `odysseus-services/src/memory.rs` + `odysseus-services/src/skills.rs` | Application | |
| `services/research/` | `odysseus-services/src/research.rs` | Application | |
| `services/hwfit/hardware.py` (907 L) | `odysseus-services/src/hwfit/hardware.rs` | Infrastructure | nvidia-smi / rocm / Apple / Windows probes + remote SSH. |
| `services/hwfit/fit.py` (876 L) | `odysseus-services/src/hwfit/fit.rs` | Domain core | **Pure computation** — highly portable. Bandwidth tables + harmonic offload blend. |
| `services/hwfit/{models,image_models,profiles,hf_discovery}.py` | `odysseus-services/src/hwfit/*.rs` | Domain + Infrastructure | |
| `services/youtube/` | `odysseus-services/src/youtube.rs` | Application | `yt-dlp` stays a subprocess. |
| `services/faces/` | **REMOVED** | — | Empty stub. |

---

## 5. `routes/` → `odysseus-web/src/routes/`

All 493 route decorators collapse into a declarative tree. Route *ordering hazards*
disappear because matching is structural rather than first-registered.

| Current | Target | Why |
|---|---|---|
| 15 `routes/X_routes.py` shims | **REMOVED** | Re-export indirection. |
| `routes/chat_routes.py` (2,829 L) | `routes/chat.rs` + `odysseus-services/src/chat.rs` | Handler in web; logic in services. |
| `routes/chat_helpers.py` `build_chat_context` `:605` | `odysseus-agent/src/context.rs` | Application. |
| `routes/session_routes.py` | `routes/sessions.rs` | |
| `routes/history/` | `routes/history.rs` | |
| `routes/auth_routes.py` | `routes/auth.rs` + `routes/settings.rs` | Split: identity vs. settings. |
| `routes/email_routes.py` (6,152 L) | `routes/email/mod.rs` — 9 sub-modules | 6,152 lines must not be one file. |
| `routes/email_helpers.py` (2,008 L) | `odysseus-services/src/email/helpers.rs` | |
| `routes/email_pollers.py` (1,565 L) | `odysseus-services/src/email/pollers.rs` | |
| `routes/calendar_routes.py` (1,775 L) | `routes/calendar.rs` + `odysseus-services/src/calendar.rs` | |
| `routes/cookbook_routes.py` (4,583 L) | `routes/cookbook.rs` + `odysseus-services/src/cookbook/` — 6 sub-modules | |
| `routes/cookbook_helpers.py` (1,482 L) | `odysseus-services/src/cookbook/helpers.rs` | |
| `routes/model_routes.py` (2,717 L) | `routes/models.rs` + `odysseus-services/src/endpoints.rs` | |
| `routes/shell_routes.py` (1,971 L) | `routes/shell.rs` + `odysseus-services/src/shell.rs` | |
| `routes/skills_routes.py` (1,899 L) | `routes/skills.rs` + `odysseus-services/src/skills.rs` | |
| `routes/document/` | `routes/documents.rs` + `odysseus-services/src/documents.rs` | |
| `routes/gallery/gallery_routes.py` (2,338 L) | `routes/gallery.rs` + `odysseus-services/src/gallery.rs` | |
| `routes/note/`, `routes/task/`, `routes/memory/`, `routes/research/`, `routes/search/`, `routes/history/`, `routes/contacts/`, `routes/vault/`, `routes/mcp/`, `routes/webhook/`, `routes/cleanup/`, `routes/admin_wipe/`, `routes/compare/` | `routes/{notes,tasks,memory,research,search,history,contacts,vault,mcp,webhooks,cleanup,admin,compare}.rs` | 1:1. |
| `routes/codex_routes.py` (910 L) | `routes/codex.rs` | The **path-string handler re-invocation** must become real service calls. |
| `routes/upload_routes.py`, `personal_routes.py`, `embedding_routes.py`, `preset_routes.py`, `prefs_routes.py`, `backup_routes.py`, `workspace_routes.py`, `diagnostics_routes.py`, `signature_routes.py`, `editor_draft_routes.py`, `assistant_routes.py`, `device_flow.py`, `copilot_routes.py`, `chatgpt_subscription_routes.py`, `emoji_routes.py`, `font_routes.py`, `stt_routes.py`, `tts_routes.py`, `hwfit_routes.py`, `api_token_routes.py` | `routes/<name>.rs` | 1:1. |
| `companion/` | `routes/companion.rs` | |
| `routes/_validators.py` | `odysseus-domain/src/validate.rs` | `validate_remote_host`, `validate_ssh_port` used by both routes and tools. |

---

## 6. `static/` → `odysseus-ui`

| Current | Target | Concern | Why |
|---|---|---|---|
| `static/index.html` (2,591 L) | `odysseus-ui/src/app.rs` + `shell/` | UI | Becomes component tree + `leptos_meta`. |
| ↳ inline theme script `:17-206` | `odysseus-ui/src/shell/prepaint.rs` | UI | FOUC avoidance is non-negotiable. |
| ↳ loader wave `:212-235` | `odysseus-ui/src/shell/loader.rs` | UI | |
| ↳ per-route manifest blob `:182-206` | `odysseus-ui/src/shell/manifest.rs` | UI | |
| ↳ 34 `<script type=module>` tags `:2556-2589` | **REMOVED** | — | Replaced by the bundler manifest + content hashes. |
| `static/app.js` (4,583 L) | `odysseus-ui/src/app.rs` | UI | Boot sequence becomes explicit and idempotent. |
| ↳ `_routeOpen` `:1105-1234` | `leptos_router` | UI | |
| ↳ 5 documented `window.*` installs `:58-62` | `odysseus-ui/src/context.rs` | UI | |
| ↳ global 401 fetch interceptor `:196-204` | `odysseus-ui/src/api.rs` | UI | |
| `static/js/**` (165 files) | `odysseus-ui/src/components/**` | UI | Full mapping in `UI_MIGRATION.md`. |
| `static/style.css` (41,401 L) | Tailwind + `components/ui/**` | UI | Replaced by a utility-first system. |
| `static/sw.js` | `odysseus-ui/public/sw.js` | UI | Rewritten for hashed assets. |
| `static/lib/` (7.2 MB) | Bundled deps | UI | highlight.js, katex, mermaid, docx, mammoth, xlsx, html2pdf, qrcode. Lazy-injected where they are today. |
| `static/fonts/` | `odysseus-ui/public/fonts/` | UI | Fira Code, Inter, OpenDyslexic — already self-hosted. |

---

## 7. `scripts/`, `integrations/`, `mcp_servers/`, native packaging

| Current | Target | Concern | Why |
|---|---|---|---|
| `scripts/diffusion_server.py` (1,506 L) | `odysseus-ml` or external backend | Infrastructure | Already an optional out-of-process backend. Spike S9 decides. |
| `scripts/mlx_image_server.py`, `swift/odysseus-mlx-image-bridge/` | **REMOVED** | — | Apple-only MLX bridge. Replaced by `ort` CoreML EP where viable. |
| `scripts/odysseus-*` (~30 CLI shims) | `apps/odysseus-cli` (optional) | Application | The CLI is a real user surface (`tests/cli/` has 24 test files). |
| `scripts/pr_blocker_audit.py`, `run_focus.py`, `run_order_report.py`, `bombadil-spec.ts` | **REMOVED** | — | Repo tooling, not product. `run_focus.py` concepts reappear as cargo test filters. |
| `integrations/codex/`, `integrations/claude/` | `odysseus-web/src/routes/codex.rs` + `odysseus-ui/public/plugin/` | Web + UI | The plugin ZIP payload ships as a static asset. |
| `Dockerfile`, `docker-compose*.yml` (3), `docker/` | `deploy/` | Infrastructure | Rust multi-stage image; compose files keep GPU variants. |
| `build-macos-app.sh`, `start-macos.sh`, `launch-windows.ps1`, `Odysseus.spec` | `deploy/` | Infrastructure | Rust equivalents; single binary simplifies all of these. |
| `flake.nix`, `nix/modules/`, `nix/scripts/` | `deploy/nix/` | Infrastructure | |
| `install-service.sh`, `odysseus-ui.service` | `deploy/systemd/` | Infrastructure | |
| `.env.example` (12.5 KB) | `deploy/.env.example` | Infrastructure | Preserve env var names for parity. |
| `specs/` | **REMOVED** | — | Design docs, superseded by these planning docs. |

---

## 8. Tests → colocated Rust tests + `crates/*/tests/`

| Current | Target | Why |
|---|---|---|
| `tests/` (829 files) | `crates/*/tests/*.rs` + `#[cfg(test)]` modules | Detailed mapping in `TEST_MIGRATION.md`. |
| `tests/conftest.py`, `_taxonomy.py`, `helpers/` | `crates/*/tests/common/mod.rs` | Shared fixtures: temp sqlite, import-state isolation, db stubs. |
| `tests/run_focus.py` | `cargo test -p <crate>`, cargo test filters | |
| `tests/run_order_report.py` | `cargo nextest` with a shuffle seed | |
| `tests/streaming/*.mjs`, `tests/helpers/*.js` | `odysseus-ui` Rust tests + `wasm-bindgen-test` | |
| `tests/cli/` (24 files) | `apps/odysseus-cli/tests/` | |
| `.github/workflows/ci.yml` | `.github/workflows/ci.yml` (rewritten) | `compileall` + `node --check` → `cargo fmt` + `clippy -D warnings` + `test` + `leptos build`. Add the JS tests that CI currently omits. |

---

## 9. Target crate dependency graph

```
odysseus-domain          (no deps)
    ▲   ▲   ▲
    │   │   │
odysseus-store           (rusqlite, sqlite-vec, bcrypt, fernet, ...)
    ▲   ▲
    │   └── odysseus-llm        (reqwest, tokio)
    │           ▲
odysseus-tools ──────────┤
    ▲                   │
odysseus-agent ──────────┤
    ▲                   │
odysseus-services ◄──────┤
    ▲                   │
odysseus-integrations ──┤
                        │
odysseus-web ───────────┤
                        │
odysseus-ui ─────────────┘
                        │
apps/odysseus-server ────┘
```

Enforced rules (to be asserted with `cargo` layer checks or `cargo deny`):

- `odysseus-domain` depends on nothing internal.
- `odysseus-store` never imports `axum`, `leptos`, or `reqwest`.
- `odysseus-ui` never imports `rusqlite` or `reqwest`.
- `odysseus-agent` never imports `axum`.
- Only `odysseus-web` knows about HTTP status codes.
- `odysseus-ml` (`ort`) is isolated so a swap is a single-crate change.