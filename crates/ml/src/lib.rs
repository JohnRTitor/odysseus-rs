//! ML inference (ONNX Runtime). Isolated so the runtime can be swapped in one crate.
//!
//! **Boundary** (see `AGENTS.md` §4 and `SOURCE_MAPPING.md` §9). This crate must
//! not import: axum, leptos, business rules
//!
//! Violating this is an architectural defect, not a style preference.

#![deny(missing_docs)]
