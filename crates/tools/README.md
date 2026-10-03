# odysseus-tools

## Concern

The tool registry, the JSON schemas the model sees, the tool implementations,
and the execution envelope that wraps every call.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. It declares no
dependencies, so neither the `domain` nor the `store` edge is wired up, no tool
is registered, and the execution envelope — including the rule that tool
execution cannot run without an explicit security context (AGENTS.md §7) — does
not exist yet.

## Must not import

- `axum`
- agent-loop control flow

Source of truth for this list: the architecture table in `AGENTS.md` §8.