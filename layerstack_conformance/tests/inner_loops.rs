// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Finite inner-loop values, dual-valued boundaries and bounded materialization.
use layerstack::LayerOffset;
use layerstack::spline::{Extrapolation, SplineData, SplineDataType, SplineQueryError};
use layerstack_conformance::save_corpus::Imported;
use std::{fs, process::Command};

const SOURCE: &str = r#"#usda 1.0
def "A" {
    custom double x
    double x.spline = {
        loop: (1, 3, 1, 1, 100),
        -2: 0; pre (0, 0); post linear,
        0: 500; post linear,
        1: 8 & 10; post linear,
        2: 20; post linear,
        3: 999; post linear,
        4: 777; post linear,
        5: 55; post linear,
        7: 70; post linear,
    }
}
"#;
const ROWS: [(f64, f64, f64); 18] = [
    (-3., 0., 0.),
    (-2., 0., 0.),
    (-1.01, -91.08, -91.08),
    (-1., -90., -92.),
    (-0.5, -85., -85.),
    (0., -80., -80.),
    (1., 10., 8.),
    (2., 20., 20.),
    (3., 110., 108.),
    (4., 120., 120.),
    (4.999, 207.912, 207.912),
    (5., 210., 208.),
    (5.001, 209.93, 209.93),
    (6., 140., 140.),
    (7., 70., 70.),
    (8., 70., 70.),
    (1.5, 15., 15.),
    (2.5, 64., 64.),
];
fn spline() -> SplineData {
    Imported::usda(SOURCE)
        .property("/A.x")
        .spline
        .clone()
        .unwrap()
}
fn close(actual: Option<f64>, expected: f64) {
    assert!(
        (actual.unwrap() - expected).abs() < 1e-8,
        "{actual:?} != {expected}"
    );
}
#[test]
fn boundary_values_match_native_and_baking_preserves_both_sides() {
    let source = spline();
    let mut baked = source.clone();
    assert_eq!(baked.bake_inner_loops(9), Ok(9));
    assert!(baked.loop_params.is_none());
    for (time, value, pre) in ROWS {
        for curve in [&source, &baked] {
            close(curve.evaluate_checked(time).unwrap(), value);
            close(curve.evaluate_pre_value(time).unwrap(), pre);
        }
    }
    // Derivatives describe the actual echoed curve, including replaced authoring.
    // OpenUSD 26.8's derivative resolver sometimes uses shadowed authored knots.
    for time in [-1.5, -0.5, 0.5, 1.5, 2.5, 3.5, 4.5, 5.5, 8.] {
        assert_eq!(
            source.evaluate_derivative(time),
            baked.evaluate_derivative(time)
        );
        let epsilon = 1e-5;
        let numerical = (source.evaluate_checked(time + epsilon).unwrap().unwrap()
            - source.evaluate_checked(time - epsilon).unwrap().unwrap())
            / (2. * epsilon);
        close(source.evaluate_derivative(time).unwrap(), numerical);
    }
}
#[test]
fn baking_budget_and_invalid_ranges_leave_source_unchanged() {
    let mut source = spline();
    let original = source.clone();
    assert_eq!(
        source.bake_inner_loops(8),
        Err(SplineQueryError::SampleBudgetExceeded)
    );
    assert_eq!(source, original);
    let params = source.loop_params.as_mut().unwrap();
    params.num_pre_loops = i32::MAX;
    params.num_post_loops = i32::MAX;
    // Billions of echoes are evaluated without building billions of knots.
    close(source.evaluate_checked(2_000_003.).unwrap(), 100_000_110.);
    let original = source.clone();
    assert_eq!(
        source.bake_inner_loops(32),
        Err(SplineQueryError::SampleBudgetExceeded)
    );
    assert_eq!(source, original);
    source.loop_params.as_mut().unwrap().value_offset = f64::NAN;
    assert_eq!(
        source.evaluate_checked(1.),
        Err(SplineQueryError::InvalidInput)
    );
    source = spline();
    source.pre_loop_boundary = Some(0.);
    assert_eq!(
        source.evaluate_checked(1.),
        Err(SplineQueryError::InvalidInput)
    );
}
#[test]
fn timecode_retiming_scales_loop_offsets_and_preserves_curve() {
    let mut source = spline();
    source.data_type = SplineDataType::TimeCode;
    let retimed = source
        .retimed(LayerOffset {
            offset: 7.,
            scale: 2.,
        })
        .unwrap();
    assert_eq!(retimed.loop_params.unwrap().value_offset, 200.);
    for (time, value, pre) in ROWS {
        close(
            retimed.evaluate_checked(time * 2. + 7.).unwrap(),
            value * 2. + 7.,
        );
        close(
            retimed.evaluate_pre_value(time * 2. + 7.).unwrap(),
            pre * 2. + 7.,
        );
    }
}
#[test]
fn disabled_inner_loops_and_outer_loops_remain_independent() {
    let mut source = spline();
    source.loop_params.as_mut().unwrap().proto_start = 1.25; // no prototype-start knot
    let mut plain = source.clone();
    plain.loop_params = None;
    assert_eq!(source.evaluate_checked(0.5), plain.evaluate_checked(0.5));
    source = spline();
    source.pre_extrapolation = Extrapolation::LoopRepeat;
    source.post_extrapolation = Extrapolation::LoopOscillate;
    let mut baked = source.clone();
    baked.bake_inner_loops(9).unwrap();
    for time in [-30., -12., -3., 8., 15., 30.] {
        assert_eq!(source.evaluate_checked(time), baked.evaluate_checked(time));
        assert_eq!(
            source.evaluate_pre_value(time),
            baked.evaluate_pre_value(time)
        );
    }
}
fn interpolation_sources() -> Vec<String> {
    let curve = SOURCE.replace("post linear", "post curve (0.25, 2)");
    vec![
        SOURCE.into(),
        SOURCE.replace("post linear", "post held"),
        SOURCE.replace("post linear", "post none"),
        curve.clone(),
        curve.replace("loop:", "hermite,\n        loop:"),
    ]
}
#[test]
fn virtual_windows_preserve_held_blocked_bezier_and_hermite_segments() {
    for source in interpolation_sources() {
        let mut imported = Imported::usda(&source);
        let original = imported.property("/A.x").spline.clone().unwrap();
        let mut baked = original.clone();
        baked.bake_inner_loops(9).unwrap();
        for (time, _, _) in ROWS {
            assert_eq!(
                original.evaluate_checked(time),
                baked.evaluate_checked(time)
            );
            assert_eq!(
                original.evaluate_pre_value(time),
                baked.evaluate_pre_value(time)
            );
        }
    }
}
#[test]
fn native_inner_loop_values_and_serialization() {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Sdf"])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let dir = std::env::temp_dir().join(format!("layerstack-inner-loops-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let sources = interpolation_sources();
    for (i, source) in sources.iter().enumerate() {
        fs::write(dir.join(format!("source{i}.usda")), source).unwrap();
        let imported = Imported::usda(source);
        fs::write(
            dir.join(format!("saved{i}.usda")),
            imported.save_usda().unwrap(),
        )
        .unwrap();
        fs::write(
            dir.join(format!("saved{i}.usdc")),
            imported.save_usdc().unwrap(),
        )
        .unwrap();
    }
    let script = r#"
import json, sys
from pathlib import Path
from pxr import Sdf
p = Path(sys.argv[1])
rows = []
for i in range(int(sys.argv[3])):
    layer = Sdf.Layer.FindOrOpen(str(p / f'source{i}.usda'))
    spline = layer.GetPropertyAtPath('/A.x').GetInfo('spline')
    for suffix in ['usda', 'usdc']:
        saved = Sdf.Layer.FindOrOpen(str(p / f'saved{i}.{suffix}'))
        assert saved.ExportToString() == layer.ExportToString()
    rows.append([[spline.Eval(t), spline.EvalPreValue(t)] for t in json.loads(sys.argv[2])])
print(json.dumps(rows))
"#;
    let times: Vec<_> = ROWS.iter().map(|r| r.0).collect();
    let output = Command::new(python)
        .arg("-c")
        .arg(script)
        .arg(&dir)
        .arg(serde_json::to_string(&times).unwrap())
        .arg(sources.len().to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Vec<[Option<f64>; 2]>> = serde_json::from_slice(&output.stdout).unwrap();
    for (case, (source, rows)) in sources.into_iter().zip(rows).enumerate() {
        let mut imported = Imported::usda(&source);
        let source = imported.property("/A.x").spline.clone().unwrap();
        for (&time, row) in times.iter().zip(rows) {
            for (side, (actual, expected)) in [
                source.evaluate_checked(time).unwrap(),
                source.evaluate_pre_value(time).unwrap(),
            ]
            .into_iter()
            .zip(row)
            .enumerate()
            {
                // Native 26.8 resolves held incoming values from shadowed raw
                // knots at these exact times, rather than the left limit of
                // its evaluated curve. Preserve baked-curve semantics and pin
                // the native discrepancy instead of silently dropping checks.
                let discrepancy = match time {
                    -1. => Some((400., 0.)),
                    1. => Some((500., -80.)),
                    3. => Some((600., 20.)),
                    5. => Some((700., 120.)),
                    7. => Some((55., 210.)),
                    _ => None,
                };
                if case == 1
                    && side == 1
                    && let Some((native, baked)) = discrepancy
                {
                    assert_eq!(expected, Some(native));
                    close(actual, baked);
                    continue;
                }
                match expected {
                    Some(expected) => close(actual, expected),
                    None => assert_eq!(actual, None),
                }
            }
        }
    }
    fs::remove_dir_all(dir).unwrap();
}
