//! Pure domain types, the SSE event vocabulary, and errors. Has no IO and depends on nothing internal.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: anything with IO, any framework
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
