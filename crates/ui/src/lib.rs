//! The Leptos frontend: components, routes, and client state.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: rusqlite, reqwest, SQL, raw SQL
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]

pub mod theme;

pub use theme::{SyntaxVars, THEMES, Theme, syntax_vars, theme};
