# odysseus-agent

## Concern

The agent loop, tool selection, sealed approvals, and context budgets — the
control flow that decides which tool runs next and why.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. It declares no
dependencies at all, so the `domain` / `llm` / `tools` edges its docs describe
are not wired up yet, and no part of the loop runs.

## Must not import

- `axum`
- `leptos`

Source of truth for this list: the architecture table in `AGENTS.md` §8.