// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scratch files under Cargo's integration-test scratch space.
//!
//! Cargo gives integration tests `CARGO_TARGET_TMPDIR`, an absolute path
//! inside the target directory. Under WASI only the crate directory and its
//! parent, the workspace, are preopened (see `.cargo/config.toml`), and an
//! absolute path that names neither resolves inside the crate directory:
//! writing through it would leave a copy of the host's directory tree in the
//! source tree. There the path is spelled relative to the workspace instead.
#![allow(dead_code, reason = "each test uses part of the support module")]

use std::path::{Path, PathBuf};

/// `name` under Cargo's integration-test scratch space, spelled so the
/// running target can reach it. Nothing is created or removed.
///
/// Name scratch files after the test module that writes them: every module
/// shares one scratch space.
pub(crate) fn scratch_dir(name: &str) -> PathBuf {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let base = if cfg!(target_os = "wasi") {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate is inside the workspace");
        Path::new("..").join(
            tmp.strip_prefix(workspace)
                .expect("target directory is inside the workspace"),
        )
    } else {
        tmp.to_path_buf()
    };
    base.join(name)
}

/// [`scratch_dir`], created empty: whatever an earlier run left there is
/// removed first.
pub(crate) fn fresh_scratch_dir(name: &str) -> PathBuf {
    let dir = scratch_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
