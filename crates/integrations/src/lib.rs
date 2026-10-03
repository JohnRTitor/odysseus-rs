//! External integrations: MCP, outbound webhooks, provider OAuth, device flows.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: axum, leptos, UI types
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
