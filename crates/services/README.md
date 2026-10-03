# odysseus-services

## Concern

The application services — chat, research, email, calendar, documents, memory,
search, and tasks. This is where a decision about *what should happen* belongs:
anything that decides *what a status code means* belongs in `odysseus-web`, and
anything that decides *whether it is allowed* belongs in the crate that owns the
data.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. It declares no
dependencies, so the "may import all non-web crates" edge is not wired up and no
service is implemented.

## Must not import

- `axum`
- `leptos`

Source of truth for this list: the architecture table in `AGENTS.md` §8.