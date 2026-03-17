//! Conformance harness for `layerstack`.
//!
//! This crate is intentionally `std`-based and may use external dependencies to
//! load and validate golden test vectors.
//!
//! Spec reference: `specs/aousd_core_spec_1.0.1_2025-12-12.pdf`.

#![allow(
    missing_docs,
    reason = "conformance harness types are internal-focused"
)]

pub mod listop_vectors;
pub mod pcp;
pub mod usda_real;
