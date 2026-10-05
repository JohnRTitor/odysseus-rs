//! The Leptos frontend: components, routes, and client state.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: rusqlite, reqwest, SQL, raw SQL
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]

pub mod app;
pub mod theme;

#[cfg(feature = "hydrate")]
mod hydrate;

pub use {
    app::{App, Nonce, PREPAINT_SCRIPT},
    theme::{SyntaxVars, THEMES, Theme, syntax_vars, theme},
};
