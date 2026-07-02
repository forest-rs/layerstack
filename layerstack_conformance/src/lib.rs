// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

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

use std::path::PathBuf;

pub mod listop_vectors;
pub mod pcp;
pub mod usda_real;
pub mod usdc;
pub mod usdz;

pub fn workspace_root() -> PathBuf {
    let cwd_root = PathBuf::from(".");
    if cwd_root
        .join("core-spec-supplemental-release_dec2025")
        .is_dir()
    {
        return cwd_root;
    }

    let parent_root = PathBuf::from("..");
    if parent_root
        .join("core-spec-supplemental-release_dec2025")
        .is_dir()
    {
        return parent_root;
    }

    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}
