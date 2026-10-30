// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! USDC layers whose `subLayers` and `subLayerOffsets` counts differ,
//! against usd-core 26.11.
//!
//! USDA writes each offset with its sublayer, so only a crate file can
//! author the two fields apart. OpenUSD 26.11 then gives every sublayer of
//! the layer the identity offset and reports
//! `PcpErrorInvalidSublayerAndOffsetCount` with both counts
//! (`PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`); a
//! missing `subLayerOffsets` field counts as none. layerstack reads the
//! layer the same way and reports `InvalidSublayerAndOffsetCount`.
#![cfg(not(target_arch = "wasm32"))]

use crate::support::scratch::fresh_scratch_dir;
use std::path::Path;
use std::process::Command;

use layerstack::{CompositionError, LayerStack, StageOptions};
use layerstack_io::{Filesystem, StageDocument};
use layerstack_usdc::writer::{Spec, SpecForm, Value, write_crate};

/// A sublayer authoring samples, which offsets retime.
const SUBLAYER: &str = "#usda 1.0\ndef \"A\"\n{\n    double t.timeSamples = {\n        0: 0,\n        10: 10,\n    }\n}\n";

/// A root layer case: its name, and its `subLayerOffsets` (`None` for no
/// field) for the sublayers `./a.usda` and `./b.usda`.
struct Case {
    name: &'static str,
    offsets: Option<&'static [(f64, f64)]>,
}

const CASES: [Case; 5] = [
    Case {
        name: "paired",
        offsets: Some(&[(5.0, 2.0), (1.0, 1.0)]),
    },
    Case {
        name: "fewer",
        offsets: Some(&[(5.0, 2.0)]),
    },
    Case {
        name: "more",
        offsets: Some(&[(5.0, 2.0), (1.0, 1.0), (3.0, 1.0)]),
    },
    Case {
        name: "empty",
        offsets: Some(&[]),
    },
    Case {
        name: "absent",
        offsets: None,
    },
];

/// For each root layer named after the first argument, the counts OpenUSD
/// reports (`paired` when it reports none) and its layer stack's offsets.
const ORACLE: &str = r#"
import sys
from pxr import Pcp, Sdf
for path in sys.argv[1:]:
    layer = Sdf.Layer.FindOrOpen(path)
    identifier = Pcp.LayerStackIdentifier(layer)
    stack, errors = Pcp.Cache(identifier).ComputeLayerStack(identifier)
    counts = [e for e in errors if e.errorType == Pcp.ErrorType_InvalidSublayerAndOffsetCount]
    words = str(counts[0]).split() if counts else []
    reported = f"{words[3]} {words[6]}" if counts else "paired"
    offsets = " ".join(f"{o.offset}/{o.scale}" for o in stack.layerOffsets)
    print(f"{reported} {offsets}")
"#;

/// The crate root layer of `case`.
fn root_layer(case: &Case) -> Vec<u8> {
    let mut root = Spec::new("/", SpecForm::PseudoRoot).with_field(
        "subLayers",
        Value::StringVector(vec!["./a.usda".into(), "./b.usda".into()]),
    );
    if let Some(offsets) = case.offsets {
        root = root.with_field(
            "subLayerOffsets",
            Value::LayerOffsetVector(offsets.to_vec()),
        );
    }
    write_crate(&[root]).unwrap()
}

/// What layerstack reports for `case` and its layer stack's offsets, in
/// the oracle's words.
fn layerstack_reading(dir: &Path, case: &Case) -> String {
    let storage = Filesystem::new(dir, []).unwrap();
    let document = StageDocument::open(
        storage,
        &format!("{}.usdc", case.name),
        StageOptions::default(),
    )
    .unwrap();
    let stage = document.stage().stage();
    let counts: Vec<String> = stage
        .composition_errors()
        .iter()
        .filter_map(|error| match error {
            CompositionError::InvalidSublayerAndOffsetCount(error) => {
                Some(format!("{} {}", error.sublayers, error.offsets))
            }
            _ => None,
        })
        .collect();
    let reported = match counts.as_slice() {
        [] => "paired".to_owned(),
        [counts] => counts.clone(),
        many => panic!("{}: {many:?}", case.name),
    };
    let stack = LayerStack::gather(document.store(), stage.root_layer().unwrap());
    let offsets: Vec<String> = (0..3)
        .map(|index| {
            let offset = stack.offset_at(index);
            format!("{:?}/{:?}", offset.offset, offset.scale)
        })
        .collect();
    format!("{reported} {}", offsets.join(" "))
}

/// Each unpaired count is reported and read with identity offsets, as
/// usd-core 26.11 does, and a paired layer keeps its offsets.
#[test]
fn unpaired_sublayer_offsets_are_reported_as_openusd_reports_them() {
    let dir = fresh_scratch_dir("sublayer_offset_counts");
    for name in ["a.usda", "b.usda"] {
        std::fs::write(dir.join(name), SUBLAYER).unwrap();
    }
    let mut roots = Vec::new();
    for case in &CASES {
        let root = dir.join(format!("{}.usdc", case.name));
        std::fs::write(&root, root_layer(case)).unwrap();
        roots.push(root);
    }
    let ours: Vec<String> = CASES
        .iter()
        .map(|case| layerstack_reading(&dir, case))
        .collect();
    assert_eq!(ours[0], "paired 0.0/1.0 5.0/2.0 1.0/1.0");
    assert_eq!(ours[1], "2 1 0.0/1.0 0.0/1.0 0.0/1.0");

    let explicit_python = std::env::var("LAYERSTACK_USD_PYTHON").ok();
    let python = explicit_python.as_deref().unwrap_or("python3");
    let available = Command::new(python)
        .args([
            "-c",
            "from pxr import Pcp; Pcp.ErrorType_InvalidSublayerAndOffsetCount",
        ])
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(
        available || explicit_python.is_none(),
        "explicit LAYERSTACK_USD_PYTHON cannot import pxr 26.11"
    );
    if !available {
        eprintln!("skipped sublayer offset oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let output = Command::new(python)
        .args(["-c", ORACLE])
        .args(&roots)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenUSD failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let theirs = String::from_utf8(output.stdout).unwrap();
    for ((case, ours), theirs) in CASES.iter().zip(&ours).zip(theirs.lines()) {
        assert_eq!(ours, theirs, "{}", case.name);
    }
}
