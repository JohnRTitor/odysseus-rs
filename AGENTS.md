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
| `odysseus-ml` | `domain`, `ort`, `ort-sys` | `axum`, `leptos`, business rules |

`odysseus-ml` had no row here until T0008's follow-up, though the crate, this
table's own §9 counterpart (`SOURCE_MAPPING.md` §9), and the workspace all
existed. It may import `odysseus-domain` for shared value types, and owns the
`ort` / `ort-sys` runtime so a swap of the inference backend is a single-crate
change. **Nothing outside `odysseus-ml` may declare `ort` or `ort-sys`**, and
that rule is enforced in CI.

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
- A new stored/cached type gets a layout test: a `size_of::<T>()` ceiling, or an allocation-count assertion when §12.1 changed its representation. Silent growth of a per-item struct is a regression, not a style preference.
- Write the Python-suite test first where one exists; it is the specification.

## 12. Dependencies and performance

- Optimise for local/LAN: low idle RSS, low allocation, fast startup, low latency. Measure before and after; record in `docs/baseline/`.
- No allocation per streamed token in hot paths (streaming renderer uses append-mode inside an open fence, `UI_MIGRATION.md` §3.3.1).
- Pre-size collections when the length is known; avoid `format!` in hot loops; prefer `&str`/`Cow` over `String`.

### 12.1 Data layout for stored things

Rules 1–5 restate the five changes Cloudflare made to the 1.1.1.1 cache
(blog.cloudflare.com/dns-cache-memory-optimization-1111), which reported −56% bytes/entry, +43%
insert throughput and −19% lookup latency. **Those are their numbers, not ours** — an Odysseus
claim still needs `docs/baseline/` (N7). The transferable lesson: in a long-lived, many-instance
type, the bookkeeping around a value costs more than the value. It applies to session messages,
cached LLM responses, tool results, approval records, and vector rows (`PERFORMANCE_PLAN.md` §4).

1. **Immutable data is `Box<[T]>`/`Box<str]`, never `Vec`/`String`.** A `Vec` carries a capacity
   field that means nothing once the value is stored, and its slack tail is unreclaimable. Build
   with `Vec`/`with_capacity`, convert at the boundary with `.into_boxed_slice()`. Never
   `shrink_to_fit` a `Vec` you are about to store — the allocator keeps the block.
2. **One buffer, not N collections.** Parallel `Vec`s cost a pointer + length each, plus padding
   between them. Store one `Box<[T]>` or one `Box<[u8]>` and reach sections through `u16`/`u32`
   offsets, sized against the measured maximum (§11 requires bounding that input).
3. **Drop what the reader already has.** Fields repeated on every child (session id, parent turn
   id, a record's owner, a re-sent content type) live once on the container and are inferred on
   read; "same as parent" is `Option::None`, never an empty string. **Exception:** anything an
   authz or ownership decision reads is stored and checked — inference there violates §7 and N6.
4. **Size enums by traffic, not by the worst case.** An enum is as large as its largest variant.
   Box the rare large variants, keep the hot ones inline. `clippy::large_enum_variant` is a floor,
   not a target. State stays an enum (§4); independent metadata booleans collapse into one
   `bitflags` field, which also deletes the struct padding that surrounded them.
5. **Keep bytes as bytes.** If a value was serialized once, store the serialized form and
   re-parse only where the fields are used. Build it in a reusable scratch buffer, then copy into
   one exact-size `Box<[u8]>`; accept sequential iteration over random indexing.
6. **Boxing is a last resort.** Every box is a separate allocation, rounds up to an allocator size
   class, and costs a pointer hop that breaks CPU locality. Rule 5 usually beats rule 4's boxing.
7. **Verify with `size_of`.** `assert!(size_of::<T>() <= N)` in a test is worth more than a comment
   claiming a struct is small.

### 12.2 Proving a layout change

- **Bytes are not RSS.** Report per-entry footprint *and* allocation count; a counting
  `GlobalAlloc` wrapper is what makes the second number obtainable, and "no extra allocation" is
  the goal, not "fewer bytes".
- Count allocations in `benches/` under criterion, or in an integration test run with
  `--test-threads=1`; a `#[global_allocator]` in a parallel test binary interleaves other tests'
  counts.
- Benchmark inputs approximate production — state the mix (type distribution, size range, count per
  entry) in the file — then confirm in resident memory on a real run, since per-entry savings
  dilute across the rest of the process.
- **A memory win is not a latency licence.** Report bytes, allocations, throughput and p50/p95
  latency for both arms. Contiguity usually buys speed back; if a layout change is *slower*, it
  needs the same baseline evidence as any other claim.
- Re-capture `docs/baseline/` after a layout change; the budget in `PERFORMANCE_PLAN.md` §4 is
  denominated in those numbers.

### 12.3 Dependencies

- Check whether the stdlib or an existing dependency covers a need before adding a crate. Declare versions in `[workspace.dependencies]`; set `default-features = false` and enable only what is used.
- Run `cargo deny check advisories licenses bans` and `cargo audit` before committing dependency changes.

#### 12.3.1 Licence policy — resolve, never silently drop

**A licence concern is a reason to investigate, not a reason to remove a feature.** No
crate or capability is dropped for licensing without a recorded, reviewed decision.
`INCOMPLETE.md` §3 requires a documented reason for every removal, and "the licence was
inconvenient" is not one.

Escalation ladder, in order — stop at the first that works:

1. **It was never incompatible.** Check what the project's own licence already
   permits before treating anything as a blocker. This project is
   `AGPL-3.0-or-later`, so linking an `AGPL-3.0`, `GPL-3.0` or `MPL-2.0` dependency
   is compatible by construction (AGPL §13 permits linking with GPL-3.0). The
   frequent outcome is that the only obstacle is our own allow-list in
   `deny.toml`, which is a config decision, not a legal one — so make the config
   change, with a written justification, and keep the feature.
2. **Substitute.** A different crate achieving the same capability under a
   compatible licence is a substitution, not a loss. Record what was chosen and
   why.
3. **Isolate behind a process boundary.** No linking relationship exists across a
   process boundary, which changes the question from linking to distribution. This
   is not a workaround invented for licensing — it is already this project's
   architecture twice over: `odysseus-ml` owns `ort`/`ort-sys` "isolated so a swap
   is a single-crate change" (§8/§9), and `REWRITE_PLAN.md:66-68` places the
   diffusion backend out-of-process with the note that "anything replacing them
   inherits that placement". A capability that cannot be linked may still be
   driven over a pipe.
4. **Reimplement.** Justified when the required surface is small — the point is
   that `rembg-rs` cost 240 packages to do a job whose graph is ~150 lines
   (spike S5).
5. **Drop**, only with the reason written down in `INCOMPLETE.md` §3 and a
   statement of which rungs were attempted and why each failed.

**Escalate rather than adjudicate.** Licence compatibility has edge cases
(codec patents, geo/databases, clause interactions, whether a dependency's
* transitive* obligations differ from its declared licence) that a maintainer
should not settle alone. When the answer is not obvious from the licences
themselves, record the question, the options, and a recommendation, and ask.

#### 12.3.2 A `-sys` crate's licence is not its own

`cargo deny` reads the licence declared in the Rust dependency graph. A `-sys`
crate declares the licence of *itself* — usually MIT — while binding to a C
library whose licence may be entirely different. `poppler-rs` reports MIT and
links GPL-2.0-or-later `libpoppler` via `poppler-sys-rs`; `cargo deny` waves it
through. This is a **false negative**, the dangerous direction: the tool reports
"fine" and a copyleft obligation ships.

So the rule cuts the other way from the rest of this section. When a `-sys` or
`-build` crate is added, the licence to verify is the **C library it links**, not
the Rust wrapper, and the linkage must be recorded explicitly. The obligation is
to *prove* the licence, never to avoid `-sys` crates in order to dodge the check —
refusing them would push us toward hand-writing FFI bindings, which is a worse
outcome than either the dependency or the paper trail.

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
