# odysseus-llm

## Concern

LLM provider adapters, the streaming parsers that turn a byte stream into
tokens, and the fallback router that picks a provider when one fails.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`, and it declares no
dependencies — not even the `reqwest` / `tokio` its boundary permits. No
provider is adapted and no stream is parsed yet.

## Must not import

- `axum`
- `leptos`
- business logic

Source of truth for this list: the architecture table in `AGENTS.md` §8.