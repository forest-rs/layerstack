// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The minimized inputs of fuzz findings, run through the fuzz targets on
//! stable.
//!
//! Each finding's minimized input is kept in `fuzz/regressions/<target>/`
//! and runs here through the same function its fuzz target calls
//! (`fuzz/src/lib.rs`, included as a module), so a reader that panics on
//! one again fails this test without a nightly toolchain or libFuzzer.

use std::fs;

use layerstack_conformance::workspace_root;

#[allow(
    dead_code,
    unreachable_pub,
    reason = "the fuzz crate's functions, which this test calls by target name"
)]
#[path = "../../../fuzz/src/lib.rs"]
mod harness;

/// A fuzz target: the name of its regression directory, and the function
/// it calls.
struct Target {
    name: &'static str,
    run: fn(&[u8]),
}

const TARGETS: &[Target] = &[
    Target {
        name: "usda",
        run: harness::usda,
    },
    Target {
        name: "usda_round_trip",
        run: harness::usda_round_trip,
    },
    Target {
        name: "usdc",
        run: harness::usdc,
    },
    Target {
        name: "usdz",
        run: harness::usdz,
    },
    Target {
        name: "text",
        run: harness::text,
    },
    Target {
        name: "stage",
        run: harness::stage,
    },
];

#[test]
fn every_minimized_input_reads_without_panicking() {
    let root = workspace_root().join("fuzz/regressions");
    let mut ran = 0;
    for Target { name, run } in TARGETS {
        let Ok(entries) = fs::read_dir(root.join(name)) else {
            continue;
        };
        let mut inputs: Vec<_> = entries.map(|entry| entry.unwrap().path()).collect();
        inputs.sort();
        for input in inputs {
            let bytes = fs::read(&input).unwrap();
            eprintln!("{name}: {}", input.display());
            run(&bytes);
            ran += 1;
        }
    }
    assert!(ran > 0, "no minimized inputs under {}", root.display());
}
