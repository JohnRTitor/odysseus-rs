# odysseus-ml

## Concern

ML inference via ONNX Runtime, isolated into its own crate so the runtime can be
swapped without touching anything that calls it.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. It declares no
dependencies — not `ort` — so no model is loaded and no inference runs. The
execution-provider spikes (T0020, T0021) that decide whether the runtime is
viable are recorded in `TASKS.md`.

## Must not import

No entry for this crate exists in the `AGENTS.md` §8 architecture table, so
there is nothing to transcribe. Its own `src/lib.rs` doc comment states the
boundary as "`axum`, `leptos`, business rules", which is unverified against §8
and may simply be a copy of the `odysseus-web` row — `odysseus-web`'s actual
entry is "business rules" alone.

**This gap is reported, not filled.** Adding a row to `AGENTS.md` §8 is outside
the scope of this task; until that happens the §8 table is the source of truth
and `odysseus-ml` is simply absent from it.