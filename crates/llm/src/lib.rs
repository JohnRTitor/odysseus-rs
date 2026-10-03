//! LLM provider adapters, streaming parsers, and the fallback router.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: axum, leptos, DB access, agent logic
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
