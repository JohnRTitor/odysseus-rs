//! HTTP route handlers, grouped by domain.
//!
//! Handlers extract, delegate, and shape the response. They contain no business
//! rules (`AGENTS.md` §4): anything that decides *what should happen* belongs in
//! `odysseus-services`, and anything that decides *what a status code means*
//! belongs in [`crate::error`].

pub mod system;
