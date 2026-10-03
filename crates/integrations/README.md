# odysseus-integrations

## Concern

Everything that talks to a third party: MCP, outbound webhooks, provider OAuth,
and device-authorization flows.

**Today this crate is an empty scaffold.** `src/lib.rs` contains only the
crate-level documentation and `#![deny(missing_docs)]`. It declares no
dependencies, so neither the `domain` nor the `services` edge is wired up, and
none of the outbound security work this crate will eventually own is present —
in particular the SSRF pinning (AGENTS.md §7), which must override only
`connect_tcp` so SNI and `Host` survive.

## Must not import

- `axum`
- `leptos`

Source of truth for this list: the architecture table in `AGENTS.md` §8.