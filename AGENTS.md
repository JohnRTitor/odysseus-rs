# AGENTS.md — Rust Rules for Odysseus

Read `REWRITE_PLAN.md` first for architecture. This file covers **how to write the code**.

**Toolchain:** `rustc 1.98.1`, `cargo 1.98.0`, edition 2024. MSRV is not a constraint: use the newest stable features (let-chains included). No `#[allow]`s, feature gates, or fallbacks for older compilers.

---

## 0. Non-negotiables

| # | Rule |
|---|---|
| **N1** | `INCOMPLETE.md` is authoritative. A feature leaves it only when all 11 Definition-of-Done criteria in `REWRITE_PLAN.md` §11 hold. |
| **N2** | No fake completeness: no stubs, mocks, placeholders, hard-coded data, dummy handlers, or "coming soon" UI presented as working. Incomplete features render **disabled with a reason**. |
| **N3** | One task per commit. Check its box in `TASKS.md` (and update `INCOMPLETE.md` if status changed) in the *same* commit. |
| **N4** | Never weaken a test to reach green: no skip/xfail, deleted assertion, or loosened tolerance without a linked reason. |
| **N5** | `clippy -D warnings` and the full test suite pass before any commit. |
| **N6** | Security invariants (§7) are not negotiable. A regression blocks the feature. |
| **N7** | No performance claim without a baseline file in `docs/baseline/` (`PERFORMANCE_PLAN.md` §0.1). |

---

## 1. Control flow

- **`let-else` for guards.** Refutable pattern + bail-out = `let … else { return/continue }`. Never `match` with a trivial early-return arm, never `if let … {} else { return }`. Keep the happy path unindented. Past ~4 consecutive `let-else`, extract a helper or use `match`.
- **Let-chains** flatten nested `if let`: `if let Some(a) = x && let Some(b) = y && b.ok(&a) { … }`. Later clauses see earlier bindings, including from nested patterns.
- **Assert let-chain outcomes in tests.** They guard security decisions; a silently wrong comparison is worse than nesting.
- Use `?` for real errors; use `let-else` when failure means skip/default/log-and-continue.
- Use `Option::zip` for two independent optionals; fold `matches!` checks into let-chains.
- Max 4 block-indentation levels and no `if let` nesting deeper than 2; extract a function.

## 2. Iteration

- Prefer iterator chains over `for` loops that push into a `Vec`. Fuse (`filter`/`map`) before `collect`.
- If you `collect()` and then immediately iterate or take `.len()`, you didn't need to collect.
- Use `.sum()`/`.count()`/`.any()`/`.all()` over manual accumulators. For non-trivial accumulation (e.g. `HashMap` counts), a `for` + `entry` is clearer than a dense `fold`.
- Use a real `for` loop when you `break`/`continue` on non-trivial logic, mutate external state (guards, senders), need `.await` inside, or the closure would nest.
- Chains must improve clarity, not just line count.

## 3. Formatting

Always run exactly:

```bash
cargo fmt -- --config imports_granularity=One,unstable_features=true
```

**No `rustfmt.toml`, ever**: on stable, rustfmt silently ignores unstable options from a config file, so `--check` would pass while real formatting differs. CI must run the full command above followed by `git diff --exit-code`, never bare `cargo fmt --check`.

---

## 4. Types and API design

- **Make illegal states unrepresentable.** Enums over bool flags or stringly-typed state; newtypes for IDs, units, and validated values (`UserId`, `Token`), not bare `String`/`u64`. Parse, don't validate: construct validated types once at the boundary.
- **Borrow in, own out.** Parameters take `&str`, `&[T]`, `&Path` (not `&String`/`&Vec<T>`). Use `impl AsRef`/`Into` only when it removes real call-site friction. Use `Cow` when ownership is conditional.
- **No `.clone()` to silence the borrow checker.** Restructure, or add a targeted helper.
- **Minimal visibility.** Default to private, then `pub(crate)`. Public items need `///` docs; fallible ones document `# Errors`, panicking ones `# Panics` (library code shouldn't panic).
- Mark `#[non_exhaustive]` on public enums/structs that will grow. Add `#[must_use]` on results and builders that are useless if ignored.
- Derive `Debug` on all types. **Types holding secrets** (tokens, passwords, keys) wrap them in `secrecy::SecretString`/`SecretBox` or hand-write a redacting `Debug`; compare secrets with `subtle` (constant time).
- Follow the Rust API Guidelines for naming (`as_`/`to_`/`into_`, `iter`/`iter_mut`/`into_iter`, no `get_` prefix on plain getters).
- `Default` over zero-arg `new()`. Builders for 4+ optional fields.
- Use `serde(deny_unknown_fields)` on inbound config and request types; make renames explicit, never rely on casing accidents.
- Use `&mut`/ownership to enforce state transitions (typestate) where invalid order would be a bug.

## 5. Error handling

- **`thiserror` in libraries**, `anyhow` only in `main` and tests. Use `#[from]`/`#[source]` to preserve the chain; no `String` or `Box<dyn Error>` error types in library APIs.
- Every fallible public function returns `Result`. No silent swallowing (cf. Python `_persist_message`, `session_manager.py:305`: swallowed write errors compensated by a re-`COUNT(*)`; do not reproduce it).
- **Fail closed** on security decisions: ambiguous input is denied.
- Messages must not leak secrets, tokens, internal URLs, or IPs (redaction ladder: `webhook_manager.py:272-320`).
- Map errors to HTTP status **only** in `odysseus-web`.
- No `unwrap()`/`expect()`/`panic!`/`todo!`/`unimplemented!`/`dbg!` in non-test code. `expect` is allowed only for a proven invariant, with the invariant as the message.

## 6. Async and concurrency

- **No blocking on the runtime (D11).** `rusqlite`, file IO, `bcrypt`, Fernet, PDF rendering, ONNX inference, and subprocess spawn go through `tokio::task::spawn_blocking(...).await??`. This is the most likely cause of "Rust is slower than Python"; check `tokio-console` for executor stalls.
- **Never hold a lock across `.await`.** Use a scoped block to drop `std::sync` guards first; use `tokio::sync::Mutex` only when you truly must hold across an await. Prefer message passing or `RwLock` over wide mutexes.
- **Cancellation safety.** Every long-lived future tolerates cancellation. The agent loop runs in a detached task that survives client disconnect; tool execution is torn down so no orphaned subprocess survives. `spawn_blocking` work can't be cancelled: design around it. Check cancel-safety of every branch in `select!`.
- **Structured concurrency.** Spawn through `JoinSet`/tracked handles, never fire-and-forget; propagate shutdown with `CancellationToken`; handle `JoinError`.
- **Bound everything.** Bounded channels (`mpsc::channel(n)`, never unbounded) and explicit timeouts on every network call, subprocess, and external await (`tokio::time::timeout`).
- `broadcast` for fan-out, `mpsc` for point-to-point; replace polling loops with subscriptions (`PERFORMANCE_PLAN.md` H11).
- Types crossing tasks must be `Send + Sync`; don't work around it with `unsafe impl`.

---

## 7. Security invariants (each has a test; port behaviour and test together)

- Auth middleware gate order is exactly 7 steps (`app.py:364-485`).
- CORS preflight passes **before** the auth gate (`app.py:374`); enforced by layer-stack ordering.
- Trusted-loopback rejects any of 7 proxy headers (`app.py:348-362`).
- Bearer tokens resolve `effective_user`, never `"api"`, for ownership (`auth_helpers.py:15`, `AuthContext::effective_user`).
- A delegated credential never inherits admin/shell authority (`auth_helpers.py:44`, `is_delegated`).
- Tool execution **cannot run** without an explicit security context (`tool_execution.py:828`); enforced by the type system, not a runtime check.
- Ambiguous tool actions **fail high**: both read and write private (`tool_capabilities.py:490`, `capabilities_for_action`).
- A tool-result failure does **not** make content trusted (`tool_capabilities.py:506`, `observe_tool_result`).
- Approval digests are one-shot; a mutated document invalidates them (`tool_approvals.py:85,213`, `ExactToolApproval::claim`).
- `email_tool_policy_names` returns **both** spellings so a denylist can't be bypassed (`tool_security.py:200`).
- Web fetch forces `Accept-Encoding: identity` and refuses compressed bodies, otherwise the byte cap is defeated (`outbound_fetch.py:289,313`).
- SSRF: connect is **pinned** to validated IPs, SNI/`Host` preserved; only `connect_tcp` is overridden (`webhook_manager.py:177`).
- CalDAV: redirects disabled; **any parse failure skips the prune**, since an incomplete UID set would delete live events (`caldav_sync.py:268`).
- Owner mismatch on a file returns **404, not 403** (`upload_routes.py:353`).
- Vault/credential passwords never appear in process argv (`test_vault_password_not_in_argv.py`).
- Missing `coreml`/`directml` EP must **refuse**, not silently fall back to CPU (`INCOMPLETE.md` F176).

Additional rules:
- All SQL is parameterized; never format user input into a query string.
- Never log request bodies, auth headers, or secret-bearing types.
- Validate and bound all external input (size, depth, length) at the boundary.

---

## 8. Architecture

| Crate | May import | Must NOT import |
|---|---|---|
| `odysseus-domain` | nothing internal | anything with IO |
| `odysseus-store` | `rusqlite`, `sqlite-vec` | `axum`, `leptos`, `reqwest` |
| `odysseus-llm` | `reqwest`, `tokio` | `axum`, `leptos`, business logic |
| `odysseus-agent` | `domain`, `llm`, `tools` | `axum`, `leptos` |
| `odysseus-tools` | `domain`, `store` | `axum`, agent-loop control flow |
| `odysseus-services` | all non-web crates | `axum`, `leptos` |
| `odysseus-integrations` | `domain`, `services` | `axum`, `leptos` |
| `odysseus-web` | everything | business rules |
| `odysseus-ui` | `domain` | `rusqlite`, `reqwest`, SQL |

Violations are architectural defects.

- Handlers extract, delegate, and shape the response; components render and dispatch. No business logic in either.
- **Dependency injection, not singletons.** Build one `AppState` in `main`, pass `Arc<AppState>`; don't port Python's `set_*` globals. No `static mut`, no `lazy_static` mutable state.
- Config comes from `figment` + `clap`; no bespoke parsers.
- A server function never replaces a public API route (`API_MIGRATION.md` §1.2).

## 9. Lints and `unsafe`

Define lints once in the workspace `Cargo.toml` (`[workspace.lints]`, inherited with `lints.workspace = true`) and clippy thresholds in `clippy.toml` (not `rustfmt.toml`).

```toml
[workspace.lints.rust]
unsafe_code = "deny"
missing_debug_implementations = "warn"
unreachable_pub = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
dbg_macro = "deny"
print_stdout = "deny"   # use tracing
await_holding_lock = "deny"
```

```toml
# clippy.toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
```

- Silence a lint only with `#[expect(lint, reason = "…")]`, never a bare `#[allow]`, and fix pedantic noise at the cause when practical.
- `unsafe` is allowed only with `#[expect(unsafe_code, reason = …)]`, a `// SAFETY:` comment stating the invariant, and a test.

## 10. Logging

- Use `tracing` (never `println!`/`eprintln!`) with structured fields: `info!(user = %id, "…")`.
- `#[instrument(skip_all, fields(…))]` on service entry points; never record secret-bearing args.
- Levels: `error` = needs a human, `warn` = degraded but handled, `info` = lifecycle, `debug`/`trace` = per-request/per-token (never per-token above `trace`).

## 11. Testing

- Unit tests in `#[cfg(test)] mod tests`; integration tests in `crates/*/tests/`; doc-tests for public examples.
- `proptest` for parsers and sanitizers (port and extend the Python token-by-token streaming fuzz tests).
- `insta` snapshots for SSE event sequences; an unreviewed snapshot change fails CI.
- Route tests use `tower::ServiceExt::oneshot`. Persistence tests use a real temp SQLite, never mocks.
- Inject a resolver and a `Clock`: tests never touch the network or wall clock. No `sleep`-based sync; use `tokio::time::pause()` or channels/notify.
- Tests must be deterministic and independent of order.
- Every new feature has a test that **would fail if the feature were stubbed**.
- Write the Python-suite test first where one exists; it is the specification.

## 12. Dependencies and performance

- Optimise for local/LAN: low idle RSS, low allocation, fast startup, low latency. Measure before and after; record in `docs/baseline/`.
- No allocation per streamed token in hot paths (streaming renderer uses append-mode inside an open fence, `UI_MIGRATION.md` §3.3.1).
- Pre-size collections when the length is known; avoid `format!` in hot loops; prefer `&str`/`Cow` over `String`.
- Check whether the stdlib or an existing dependency covers a need before adding a crate. Declare versions in `[workspace.dependencies]`; set `default-features = false` and enable only what is used.
- Run `cargo deny check advisories licenses bans` and `cargo audit` before committing dependency changes.

---

## 13. Commands

```bash
# Everything, in order (must all pass before commit)
cargo fmt -- --config imports_granularity=One,unstable_features=true \
  && cargo clippy --all-targets --all-features -- -D warnings \
  && cargo nextest run --profile ci \
  && git diff --exit-code

cargo test -p <crate> --test <name>   # single test target
cargo build --release
cargo leptos build --release
cargo deny check advisories licenses bans && cargo audit
cargo bench --no-run                  # compile gate
```

## 14. Workflow

1. Take the lowest unchecked box in `TASKS.md` whose phase dependencies are met.
2. Read the relevant Python source in `/home/masum/Dev-Environment/Python/odysseus` first; a port is correct only if the original behaviour was read, not guessed.
3. Write the test first (where the Python suite has one), then implement per §1–§12.
4. Run §13; review `git diff` and confirm nothing in §7 regressed.
5. Commit atomically with the task ID: `feat(store): baseline migration from live sqlite_master [T0043]`. Check the `TASKS.md` box in that commit.

**Never** mark a task done by declaring its feature removed to shrink a count.
