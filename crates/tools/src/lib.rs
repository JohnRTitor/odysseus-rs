//! Tool registry, schemas, implementations, and the execution envelope.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: axum, leptos, agent-loop control flow
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
