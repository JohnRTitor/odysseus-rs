//! The Axum transport layer: routing, middleware, auth extraction, SSE encoding.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: business rules
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
