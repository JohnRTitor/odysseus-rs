# odysseus-ui

## Concern

The Leptos frontend: components, routes, and client state.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. No component is defined,
no route is declared, and the crate does not depend on `leptos` yet, so the SSR
shell is still to come. The `/` route in `odysseus-web` answers with an explicit
503 saying exactly that.

The reason this boundary matters more than most: the frontend and the database
are the two halves a browser app most naturally fuses, which is why the whole
data layer is off-limits here rather than merely discouraged.

## Must not import

- `rusqlite`
- `reqwest`
- SQL

Source of truth for this list: the architecture table in `AGENTS.md` §8.