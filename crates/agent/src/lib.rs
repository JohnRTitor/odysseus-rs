//! The agent loop, tool selection, sealed approvals, and context budgets.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: axum, leptos, HTTP types
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
