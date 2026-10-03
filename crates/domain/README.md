# odysseus-domain

## Concern

Pure domain types, the SSE event vocabulary, and shared error definitions — the
vocabulary every other crate speaks.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. It declares no
dependencies, not even an internal one, and exports no items. Its crate docs
name the SSE event vocabulary as its concern; nothing implements that yet, and
`INCOMPLETE.md` is the authoritative record of feature status.

The "no IO" half of this boundary is the part that costs something to keep:
because the crate has no dependencies at all today, that property is currently
free rather than defended. It has to stay true when the first type lands.

## Must not import

- anything with IO

Source of truth for this list: the architecture table in `AGENTS.md` §8.