# Odysseus — Test Migration

**Source:** 829 Python test files / 109,715 LOC + 5 `.mjs` files.
**This is the single strongest asset for incremental migration.** Every phase has an
existing behavioural oracle. Porting the tests *first* turns each slice into a
specification rather than a guess.

---

## 1. Source test infrastructure

| Property | Finding |
|---|---|
| Framework | `pytest` + `pytest-asyncio` (`asyncio_mode = "auto"`) |
| Taxonomy | `area_*` + dynamic `sub_*` markers derived from filenames at collection time by `tests/_taxonomy.py` |
| Areas | `security`, `routes`, `services`, `cli`, `js`, `helpers`, `unit`, `uncategorized` |
| Fast lane | `not slow` marker, opt-in, requires duration evidence to apply |
| Focused runner | `tests/run_focus.py` — validates area/sub-area names, `--fast`, `--durations`, `--last-failed`, `--dry-run` |
| Order-sensitivity | `tests/run_order_report.py` — seeded shuffle, report-only, surfaces hidden coupling |
| Helpers | `tests/helpers/` — `cli_loader`, `import_state`, `sqlite_db`, `db_stubs`, `calendar_routes`, `embedding_lanes` |
| JS tests | 3 in `tests/streaming/` + 1 at `tests/` root + 2 in `tests/helpers/` |
| HTTP client | `starlette.testclient.TestClient` (hence the `httpx2` pin) |
| Standard | `tests/TESTING_STANDARD.md` — taxonomy, determinism/isolation, behavioural-vs-source-text policy |

### 1.1 CI today

`.github/workflows/ci.yml` runs exactly three things:

```yaml
python -m compileall -q app.py core routes src services scripts tests
node --check static/app.js static/js/**/*.js        # skips static/lib
python -m pytest -q                                 # skipped for docs-only PRs
```

**The 5 `.mjs` tests are not run by CI at all.** That is a real gap: the streaming
invariant fuzz test and the settings-shell tests only run if someone invokes node
manually. Closing this is a Phase 0 deliverable.

### 1.2 Test distribution (largest files)

| File | LOC | Covers |
|---|---|---|
| `test_foreground_model_routing.py` | 3,649 | Fallback chain routing |
| `test_model_routes.py` | 2,179 | Model endpoints |
| `test_cookbook_helpers.py` | 1,028 | Cookbook |
| `test_review_regressions.py` | 1,449 | Mixed regression set |
| `test_external_context_tool_gate.py` | 1,473 | Tool gate on untrusted context |
| `test_security_regressions.py` | 1,546 | Security |
| `test_email_urgency_checkpoint.py` | 1,328 | Email triage checkpointing |
| `test_llm_core_fallback.py` | 1,493 | Provider fallback |
| `test_pr_blocker_audit.py` | 964 | Repo tooling |

---

## 2. Target test strategy

| Layer | Tool | Location |
|---|---|---|
| Unit | `#[test]` + `proptest` | Colocated `#[cfg(test)] mod tests` |
| Property/fuzz | `proptest` | Replaces the hand-rolled fuzz corpus |
| Integration (DB) | Real temp SQLite via `rusqlite` | `crates/*/tests/*.rs` |
| API | `tower::ServiceExt::oneshot` | `crates/odysseus-web/tests/api_*.rs` |
| SSR | `leptos::ssr::render_to_string` | `crates/odysseus-ui/tests/ssr_*.rs` |
| Hydration | `wasm-bindgen-test` | `crates/odysseus-ui/tests/hydrate_*.rs` |
| Frontend interaction | Port the pure logic; `wasm-bindgen-test` for DOM | |
| Streaming | `tokio::io::duplex` + `async-stream` | |
| End-to-end | Playwright (Rust bindings) | `apps/odysseus-server/tests/e2e/` |
| Benchmarks | `criterion` | `benches/` |

### 2.1 What maps cleanly

| Python | Rust |
|---|---|
| Pure function tests | `#[test]` — near-mechanical |
| `TestClient` route tests | `oneshot(request)` — near-mechanical |
| `pytest-asyncio` | `#[tokio::test]` — near-mechanical |
| `tempfile` sqlite fixtures | `tempfile` + a `TestDb` helper |
| `monkeypatch` | Constructor injection — **better**, because the source is already DI-shaped |
| `unittest.mock` | `mockall` / hand-written fakes |
| `pytest.raises` | `#[should_panic]` / `Result::unwrap_err()` |
| Freezegun / `datetime` patching | `time` crate injectable clock |

### 2.2 What needs redesign

| Python pattern | Problem | Rust approach |
|---|---|---|
| `sys.modules` stubbing (`tests/helpers/import_state.py`) | Exists only because Python imports have global side effects | **Disappears.** No import side effects in Rust. |
| `clear_fake_database_modules` | Guarded fake-module cleanup | **Disappears.** |
| Order-sensitive tests (`run_order_report.py`) | Global module state | **Disappears** for the same reason |
| `TestClient` | Requires `httpx2` pin | `tower::ServiceExt` — no extra dep |
| Thread/lock concurrency tests | `threading.Lock` | `tokio::sync` primitives |
| Network-touching tests | Mocked at the httpx layer | `wiremock` / a local `axum` stub server |

---

## 3. Priority test groups

### 3.1 Group A — Security regression (**must be ported 1:1, non-negotiable**)

These are the tests that keep the rewrite honest. **Port all of them before any
security-relevant feature is marked DONE.**

| Source test | Covers |
|---|---|
| `test_security_regressions.py` (1,546 L) | Mixed security set |
| `test_tool_path_confinement.py` | Path denylist + allowlist containment + hardlink check |
| `test_workspace_confine.py` | `vet_workspace` refuses filesystem roots |
| `test_agent_state_dir_confinement.py` (1,044 L) | Agent state dir confinement |
| `test_tool_approvals.py` | Sealed digest, one-shot claim, TTL |
| `test_tool_approval_*` (7 files) | Single-action scope, task scope, frontend routing |
| `test_external_context_tool_gate.py` (1,473 L) | Untrusted context blocks POST_EXTERNAL effects |
| `test_prompt_injection_audit.py` | Prompt-injection resistance |
| `test_url_safety.py` | Permissive SSRF policy |
| `test_integration_api_call_ssrf.py` | Strict SSRF for API calls |
| `test_webhook_ssrf_resilience.py`, `test_webhook_dns_rebinding_pin.py` | DNS-rebinding pin |
| `test_skill_importer_ssrf_redirect.py`, `_dns_pinning.py`, `_security.py` | Importer SSRF |
| `test_caldav_url_hardening.py`, `test_caldav_redirect_hardening.py` | CalDAV URL safety |
| `test_gallery_endpoint_ssrf.py`, `test_gallery_result_image_ssrf.py` | Image proxy SSRF |
| `test_ai_image_url_safety.py` | AI image URL safety |
| `test_totp_failclosed.py` | 2FA fails closed |
| `test_auth_*` (11 files) | Auth, sessions, revocation, root path, policy |
| `test_redos_*.py` (5 files) | ReDoS |
| `test_cors_preflight.py` | CORS preflight precedes auth |
| `test_null_owner_gates.py`, `test_null_owner_claim` | Owner enforcement |
| `test_security_headers_middleware.py`, `_pdf_preview.py` | CSP |
| `test_manage_mcp_command_allowlist.py` | MCP command allowlist |
| `test_reminder_ntfy_ssrf.py` | Ntfy SSRF |
| `test_vault_password_not_in_argv.py` | Secrets not in argv |
| `test_api_token_tool_authority.py`, `test_api_token_user_route_gate.py` | Token authority |
| `test_serve_html_with_nonce.py` | CSP nonce |

**Target:** `crates/odysseus-web/tests/security/`, `crates/odysseus-tools/tests/confinement.rs`,
`crates/odysseus-agent/tests/approvals.rs`.

**Gating rule:** a security test that has no Rust equivalent **blocks** the DONE status
of every feature it covers.

### 3.2 Group B — Streaming parsers

| Source test | Covers |
|---|---|
| `test_llm_core_streaming.py` | OpenAI-compatible stream |
| `test_llm_core_sse_no_space.py` | `data:` vs `data: ` |
| `test_anthropic_response_parse.py` | Anthropic block stream |
| `test_llm_core_ollama.py`, `_ollama_thinking.py` | Ollama NDJSON |
| `test_llm_core_usage_finish_delta.py` | Usage accounting |
| `test_native_tool_result_threading.py` | `None` vs `""` content |
| `test_llm_core_sanitize_tool_calls.py` | Message sanitisation |
| `test_llm_core_*_reasoning*.py` (4 files) | Reasoning content |
| `test_gemma_tool_call_parsing.py` | Gemma `<\|tool_call\|>` |
| `test_function_model_tool_call.py` | `<function_model>` |
| `test_misfenced_read_file_tool_call.py` | Misfenced rescue |
| `test_tool_parsing_*` (8 files) | 8 parser patterns |
| `test_strip_think.py`, `test_strip_reasoning_prose_dataloss.py` | Think-block stripping |
| `test_harmony_tool_aliasing.py` | gpt-oss channel routing |

**Target:** `crates/odysseus-llm/tests/stream_*.rs`.

**Critical addition:** the **Gemini omitted-`index` case** (`llm_core.py:3243-3258`).
Without an explicit `index`, all parallel tool calls collapse into slot 0 and corrupt
each other. This needs a dedicated test — the Python suite covers it implicitly.

### 3.3 Group C — Fallback routing

| Source test | Covers |
|---|---|
| `test_llm_core_fallback.py` (1,493 L) | Candidate chain, eligibility, metadata commit |
| `test_foreground_model_routing.py` (3,649 L) | Chain construction |
| `test_kimi_code_user_agent.py`, `test_kimi_code_hosts.py` | Kimi UA pool |
| `test_warmup_ping_urls.py` | Warmup |

**Critical assertions:**
1. The metadata-commit rule — after the first substantive token, a candidate can never
   be abandoned.
2. `fallback` notice emission — without it a broken provider is invisible.
3. `fallback_eligible: False` on WriteTimeout/ProtocolError/NetworkError.
4. Status set `{408,425,429,500,502,503,504,507,508,529}`.

### 3.4 Group D — Scheduling

| Source test | Covers |
|---|---|
| `test_compute_next_run_monthly_clamp.py` | Month-end clamping |
| `test_scheduler_scheduled_time_validation.py` | Fails closed on malformed input |
| `test_scheduler_restart_doublefire.py` | No double-fire after restart |
| `test_scheduled_poll_race.py` | Poll race |
| `test_task_scheduler_cancel.py` | Cancellation |
| `test_task_scheduler_cache.py` | Single-flight cache |
| `test_task_scheduler_session_delivery.py` | Session delivery |
| `test_scheduler_prompt_cache_time.py` | KV-cache time injection |
| `test_digest_windows.py` | Contiguous digest buckets |
| `test_task_chain_owner_scope.py` | Chain owner validation |

### 3.5 Group E — Data & migrations

| Source test | Covers |
|---|---|
| `test_database_utcnow.py` | Naive-UTC |
| `test_sqlite_foreign_keys.py` | FK pragma |
| `test_migrate_faiss_to_chroma.py` | Prior vector migration (instructive) |
| `test_embedding_lanes_*.py` (6 files) | Embedding lanes |
| `test_chroma_client.py` | Vector client |
| `test_atomic_io.py` | Atomic writes |
| `test_app_db_permissions.py` | DB file permissions |
| `test_api_key_file_permissions.py` | Key file permissions |
| `test_replace_messages_upload_reservations.py` | Upload refs |
| `test_parse_msg_content_jsonlike_string.py` | The metadata heuristic |
| `test_truncate_message_count_regression.py` | `message_count` drift |
| `test_history_order_by_timestamp_regression.py` | Ordering |
| `test_email_account_default_serialization.py` | Partial unique index |

**New tests required (no Python equivalent):**
1. Baseline migration opens a **copy of a real `app.db`**; `PRAGMA table_info` matches
   for all 27 tables + FTS5.
2. `PRAGMA foreign_key_check` returns zero rows.
3. `.app_key` round-trip: an existing encrypted column decrypts.
4. Golden-set vector agreement at the chosen thresholds (`DATA_MIGRATION.md` §5.3).
5. Schema drift fixture recording every model-vs-live discrepancy.

### 3.6 Group F — Frontend (the 5 `.mjs` + more)

| Source | LOC | Target |
|---|---|---|
| `tests/streaming/segmenter.test.mjs` | — | `odysseus-ui/tests/segmenter.rs` |
| `tests/streaming/invariant.test.mjs` | — | `odysseus-ui/tests/stream_invariant.rs` |
| `tests/streaming/corpus.mjs` | — | Fixture file |
| `tests/streaming/markdownHarness.mjs` | — | `odysseus-ui/tests/markdown_harness.rs` |
| `tests/live_thinking_scheduler.test.mjs` | — | `odysseus-ui/tests/throttle.rs` |
| `tests/helpers/test_settings_shell.js` | — | `odysseus-ui/tests/settings_shell.rs` |
| `tests/helpers/test_settings_shell_coordinator.mjs` | — | same |

**`invariant.test.mjs` is the most valuable frontend test in the repo** — it fuzzes the
streaming segmenter token-by-token. Port it and keep fuzzing. Add `proptest`.

**JS-named tests that are actually Python** (~40 files, `test_*_js.py`): they assert on
JS source text or run node. Each must be re-classified:
- Source-text assertions → replace with real behavioural Rust tests.
- DOM behaviour → `wasm-bindgen-test`.
- Pure logic → plain Rust unit test.

Examples: `test_markdown_rendering_js.py`, `test_compare_js.py`,
`test_editor_diff_discard_on_update_js.py`, `test_email_summary_error_ui_js.py`,
`test_theme_css_url_escape_js.py`, `test_pr6020_browser_review_regressions.py`.

**Two behaviours needing explicit DOM tests** because they are the SSR/hydration
boundary:
1. **Pre-paint theme** — no FOUC. Verify CSS vars are set before first paint.
2. **Streaming renderer self-heal** — orphaned tail marker detection.

### 3.7 Group G — End-to-end (new; none exists today)

The source has **no** E2E tests. Add a minimal Playwright suite:

| Test | Scope |
|---|---|
| Boot → login → session list | Auth + SSR |
| Send a chat message → stream → persist | Core vertical slice |
| Tool approval card → allow → result | Security path |
| Document edit → save → version | Documents |
| Reload preserves state | SSR + hydration |
| Disabled-feature UX | **Phase 10 rule: an unmigrated feature must render disabled with a reason** |

The last one is not optional. It is the mechanism that prevents fake completeness.

---

## 4. Per-feature minimum acceptance tests

Every migrated feature needs at least one test that would fail if the feature were
stubbed.

| Feature class | Minimum |
|---|---|
| Any route | `oneshot` happy path + auth-denied path + owner-isolation path |
| Any mutation | Round-trip: write then read back the exact value |
| Any SSE endpoint | Event-sequence assertion against a fixture |
| Any tool | Schema present + dispatch + policy denial + approval-required path |
| Any file op | Confined-path test (success + escape attempt denied) |
| Any external call | SSRF test (loopback, link-local, metadata IP, DNS-rebinding) |
| Any cache | Hit + miss + expiry |
| Any threshold/constant | Boundary test at the exact calibrated value |
| Any UI surface | SSR renders; hydrated DOM contains the expected text |
| Any disabled feature | Assert the control is disabled **and** shows a reason |

**The last row is the Phase 10 guarantee encoded as a test.**

---

## 5. CI (target)

```yaml
jobs:
  fmt:      cargo fmt --check
  clippy:   cargo clippy --all-targets --all-features -- -D warnings
  test:     cargo test --workspace
  nextest:  cargo nextest run --workspace          # parallel, per-test timing
  build:    cargo build --release
  leptos:   cargo leptos build --release
  deny:     cargo deny check advisories licenses bans
  audit:    cargo audit
  wasm:     wasm-pack test --headless --chrome crates/odysseus-ui
  e2e:      cargo test -p odysseus-server --test e2e
  bench:    cargo bench --no-run                   # compile-only gate
  size:     # binary + asset size budget vs docs/baseline/
```

**Added versus today:** `clippy -D warnings`, `cargo deny`, `cargo audit`,
`wasm-pack` (JS tests actually run), a size budget, and a bench compile gate.

**Removed:** `compileall`, `node --check`, `pytest`.

### 5.1 Test taxonomy in Rust

The `area_*`/`sub_*` markers become crate-scoped test names:

```bash
cargo test -p odysseus-tools --test security      # security-critical
cargo test -p odysseus-llm --test stream          # streaming
cargo test --lib agent::approvals                 # unit by module path
cargo nextest run --status-level fail --profile ci
```

`--profile ci` gives the fast lane. A `slow` equivalent is a `#[ignore]` with a
justification comment, applied **only** with duration evidence — mirroring the existing
policy in `tests/README.md`.

---

## 6. What is preserved, rewritten, or new

| Category | Disposition |
|---|---|
| ~600 pure-logic Python tests | **Preserved** as Rust unit tests, near-mechanical |
| ~200 security regression tests | **Preserved 1:1.** Blocking. |
| ~30 route tests | **Preserved** via `oneshot` |
| 5 `.mjs` tests | **Preserved + fuzzed**; added to CI (a real gap closed) |
| ~40 `test_*_js.py` source-text tests | **Rewritten** as behavioural tests |
| `sys.modules` stub helpers | **Removed** — no longer needed |
| `run_focus.py` | **Replaced** by cargo filters |
| `run_order_report.py` | **Replaced** by `cargo nextest` shuffle |
| Order-sensitivity as a class | **Eliminated** — no global import state |
| — | **New:** E2E suite, golden-set vector tests, migration tests, disabled-feature tests |
| — | **New:** `proptest` property tests for parsers and sanitisation |
| — | **New:** `wasm-bindgen-test` for hydration |
| — | **New:** size and clippy gates |

---

## 7. Test dependencies

| Crate | Purpose |
|---|---|
| `tokio` (`test-util`) | Paused time for scheduler tests |
| `tempfile` | Temp SQLite |
| `tower` (`util`) | `oneshot` |
| `proptest` | Fuzzing |
| `criterion` | Benchmarks |
| `wiremock` | External HTTP stubs |
| `insta` | Snapshot tests for SSE event sequences |
| `wasm-bindgen-test` | Hydration |
| `mockall` | Trait mocks |
| `pretty_assertions` | Diff output |

---

## 8. Migration risks

| Risk | Severity | Mitigation |
|---|---|---|
| Security tests not ported → false confidence | **Critical** | Porting them is a **blocking** prerequisite for DONE on any auth/tool/file/network feature |
| Losing the `sys.modules` stubbing pattern and its coverage | Med | The coverage was compensating for import side effects that no longer exist; verify each stub's tests pass without it |
| Snapshot tests silently accepting regressions | **High** | `insta` snapshots require explicit review; CI fails on unreviewed changes |
| Bench-only "improvements" | Med | `PERFORMANCE_PLAN.md` §7 requires a baseline file |
| E2E flakiness | Med | Pin a single browser version; retry only on known-flaky tests, never to mask a real failure |
| Property tests finding real bugs late | Med | Run `proptest` in the fast lane, not nightly-only |
| Test suite too slow to run pre-commit | Med | `cargo nextest --profile ci` target < 30 s |

---

## 9. Test acceptance criteria

Testing is DONE for a phase when:

1. Every source test covering migrated functionality has a Rust equivalent, or a
   documented reason it does not apply.
2. **All Group A security tests pass.**
3. All Group B/C streaming and fallback tests pass, including the Gemini omitted-`index`
   case.
4. Migration tests open a copy of a real `app.db` cleanly.
5. `cargo clippy -- -D warnings` is clean.
6. SSR renders and hydration mounts for every route.
7. E2E covers the vertical slice end-to-end with real data.
8. The disabled-feature test passes for every unmigrated feature.
9. `cargo nextest --profile ci` completes in under 30 s.
10. No test was weakened, skipped, or marked `xfail` to reach green.