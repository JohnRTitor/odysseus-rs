# `docs/baseline/` — performance baselines

`AGENTS.md` N7: **no performance claim without a baseline file in
`docs/baseline/`.** This directory is that record. A number quoted in
`PERFORMANCE_PLAN.md`, `INCOMPLETE.md` or `SPIKE_RESULTS.md` must point at a file
here, and the number here must have been produced by a command recorded beside it.

## Rules for adding a file

1. **Measure or record `UNAVAILABLE`. Never estimate.** A file that says
   "estimated" is worse than no file, because it looks like evidence. If a
   measurement needs a GPU, a Python environment or credentials, say that and why —
   see `2026-10-04-python.md`, which is a record of *not* capturing something.
2. **Record the machine.** A number without a CPU is not a number.
3. **Record the command.** Every figure names the invocation that produced it, so
   a reader can re-run it and get a different-but-comparable answer.
4. **Record `git` revision and `rustc` version** for anything compiled.
5. **Report per-entry footprint AND allocation count**, not just bytes
   (`AGENTS.md` §12.2). Bytes alone hide a layout change that trades one
   allocation for several smaller ones.

## Schema

Machine-readable captures are JSON with a `schema_version`. The four top-level
blocks correspond to `PERFORMANCE_PLAN.md` §0.1:

| Block | §0.1 item | Where it is measured |
|---|---|---|
| `in_process_benchmarks` | per-operation cost | `cargo bench -p odysseus-store --bench auth_hotpath` |
| `layout` + `allocations` | §12.1/§12.2 per-entry footprint | `crates/store/tests/layout_ceilings.rs`, `crates/store/tests/alloc_baseline.rs` |
| `service.startup` | time to first byte | scratch harness, §0.1's "Startup" |
| `service.memory` + `service.load` | idle RSS, under load | scratch harness, §0.1's "Under load" |
| `unavailable` | streaming chunk latency, Python baseline | anything that could not be measured, with the reason |

## Current captures

| File | Date | Contents |
|---|---|---|
| `2026-10-04-rust.json` | 2026-10-04 | Rust skeleton: bcrypt, `atomic_write_bytes`, rate limiter, token scopes, layout, allocations, startup, RSS, load |
| `2026-10-04-python.md` | 2026-10-04 | **T0120 — NOT CAPTURED.** Why the Python app cannot be run here, and what that costs |

## Re-running the in-repo measurements

```sh
# per-operation timings (criterion; writes target/criterion/**)
cargo bench -p odysseus-store --bench auth_hotpath -- --warm-up-time 1 --measurement-time 3
cargo bench -p odysseus-store --bench layout

# layout ceilings — these fail the build if a type grows, so they are a gate,
# not just a measurement
cargo test -p odysseus-store --test layout_ceilings

# allocation counts. One test function in the binary, so the process-global
# counters cannot be disturbed by a sibling; nextest isolates per process anyway.
cargo test -p odysseus-store --test alloc_baseline
```

## Re-running the service measurements

These need the server running, so they are **not** an in-repo target — the
numbers belong in a JSON file, not in a criterion baseline, and a bench that
spawns a listening socket is a test that fails in CI for reasons unrelated to the
code. The harness used for `2026-10-04-rust.json` spawns the server itself and
drives it over loopback:

```sh
cargo build --release -p odysseus-server
# then, from a scratch crate outside the repo:
#   spawn the binary with ODYSSEUS_DATA_DIR=<fresh dir> APP_PORT=<free port>
#   poll the port until /api/health answers  → startup
#   read /proc/<pid>/status + smaps_rollup     → idle RSS, anon vs file-backed
#   drive GET over the five routes in
#   crates/web/src/app.rs at concurrency 1/8/32/64 with keep-alive, framing each
#   response by Content-Length or chunked    → TTFB percentiles
```

Two things that harness must do, both learned by getting them wrong first:

- **Spawn the server itself.** An earlier run measured a *stale listener* left
  over from a previous attempt and reported a 2.45 ms "cold start" that was really
  a connect to an already-running process. Always spawn, and check the child is
  alive.
- **Frame each response.** Reading a keep-alive connection without draining the
  body makes the next read start mid-body, so the status line parses as garbage and
  only 60 % of responses count. Drain by `Content-Length` or chunked framing.

## Reading the load numbers honestly

The five routes measured today (`/api/health`, `/api/version`, `/api/ready`,
`/api/runtime`, `/`) are **routing and middleware only**. No route in that mix
touches SQLite, calls an LLM, or opens an SSE stream. The figures are a floor, not
a workload: they say how fast the process can say "no" or "hello", which is the
cheapest thing it ever does. Re-measure when the real mix exists.