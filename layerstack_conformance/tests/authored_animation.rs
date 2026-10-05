// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored dictionaries and bounded spline semantics, checked against OpenUSD.

use layerstack::spline::{SplineData, SplineDataType, SplineQueryError, TangentAlgorithm};
use layerstack::{LayerOffset, Value};
use layerstack_conformance::save_corpus::Imported;
use std::{fs, process::Command};

const SOURCE: &str = r#"#usda 1.3
def "A" (
    references = @./missing/asset.usda@</B> (offset = 7; scale = 2; customData = {
        dictionary label = {
            token kind = "oak"
            int[] ids = [1, 2]
        }
    })
)
{
    double x.spline = {
        bezier,
        pre: loop repeat(1),
        post: loop oscillate(1),
        0: 0; pre (0, 0, custom); post curve (0.3, 2, custom); {
            token kind = "leaf"
            int[] ids = [1, 2]
            dictionary nested = {
                double signedZero = -0
            }
        },
        1: 3; pre (0.3, 4, custom); post curve (0.3, 2, custom),
        2: 5; pre (0.3, 4, custom); post held,
    }
    int[] sparse = edit [resize 2]
    timecode clock.spline = {
        bezier,
        0: 10; pre (0.3, 1, custom); post curve (0.3, 2, custom); { string note = "time" },
        2: 20; pre (0.3, 3, custom); post held,
    }
}
"#;

const TIMES: [f64; 16] = [
    -4., -3., -2., -1., -0.25, 0., 0.25, 0.75, 1., 1.25, 1.75, 2., 2.25, 3., 4., 5.,
];

fn spline(layer: &mut Imported, name: &str) -> SplineData {
    layer
        .property(name)
        .spline
        .clone()
        .expect("authored spline")
}

#[test]
fn authored_reference_and_knot_metadata_survive_both_formats_and_retiming() {
    let mut original = Imported::usda(SOURCE);
    let arc = &original.prim("/A").references.explicit.as_ref().unwrap()[0];
    assert!(arc.is_unresolved());
    assert_eq!(arc.custom_data.len(), 1);
    let expected = original.save_usda().unwrap();
    assert!(expected.starts_with("#usda 1.3"));
    for mut imported in [
        Imported::usda(&expected),
        Imported::usdc(&original.save_usdc().unwrap()),
    ] {
        assert_eq!(imported.save_usda().unwrap(), expected);
        let x = spline(&mut imported, "/A.x");
        assert_eq!(x.pre_loop_boundary, Some(1.));
        assert_eq!(x.post_loop_boundary, Some(1.));
        assert_eq!(x.knots[0].post_tan_algorithm, TangentAlgorithm::Custom);
        assert_eq!(x.knots[0].custom_data.len(), 3);
        let clock = spline(&mut imported, "/A.clock");
        assert_eq!(clock.data_type, SplineDataType::TimeCode);
        let offset = LayerOffset {
            offset: 7.,
            scale: 2.,
        };
        for name in ["/A.x", "/A.clock"] {
            let source = spline(&mut imported, name);
            let retimed = source.retimed(offset).unwrap();
            assert_eq!(retimed.knots[0].custom_data, source.knots[0].custom_data);
            if source.data_type == SplineDataType::TimeCode {
                assert_eq!(retimed.knots[0].value, 27.);
                assert_eq!(retimed.knots[0].post_tan_slope, 2.);
            } else {
                assert_eq!(retimed.pre_loop_boundary, Some(9.));
                assert_eq!(retimed.post_loop_boundary, Some(9.));
                assert_eq!(retimed.knots[0].post_tan_slope, 1.);
            }
            imported.property(name).spline = Some(retimed);
        }
        let saved = imported.save_usda().unwrap();
        assert_eq!(
            Imported::usdc(&imported.save_usdc().unwrap())
                .save_usda()
                .unwrap(),
            saved
        );
        assert_eq!(Imported::usda(&saved).save_usda().unwrap(), saved);
    }
}

#[test]
fn unsupported_and_invalid_spline_semantics_remain_explicit() {
    let mut imported = Imported::usda(SOURCE);
    let mut x = spline(&mut imported, "/A.x");
    x.loop_params = Some(layerstack::spline::LoopParams {
        proto_start: 0.,
        proto_end: 1.,
        num_pre_loops: 1,
        num_post_loops: 1,
        value_offset: 3.,
    });
    let mut inner = x.clone();
    inner.pre_loop_boundary = None;
    inner.post_loop_boundary = None;
    let mut baked = inner.clone();
    baked.bake_inner_loops(32).unwrap();
    assert_eq!(inner.evaluate_checked(0.5), baked.evaluate_checked(0.5));
    assert_eq!(inner.evaluate(0.5), baked.evaluate(0.5));
    x.loop_params = None;
    let far = -9_007_199_254_740_992.;
    assert_eq!(
        x.evaluate_checked(far),
        Err(SplineQueryError::UnsupportedLoopRange)
    );
    assert_eq!(
        x.evaluate_pre_value(far),
        Err(SplineQueryError::UnsupportedLoopRange)
    );
    assert_eq!(
        x.evaluate_derivative(far),
        Err(SplineQueryError::UnsupportedLoopRange)
    );
    assert_eq!(
        x.evaluate_pre_derivative(far),
        Err(SplineQueryError::UnsupportedLoopRange)
    );
    assert_eq!(x.evaluate(far), None);
    assert_eq!(
        x.sample_adaptive(-1., 3., 0.1, 100),
        Err(SplineQueryError::UnsupportedLoops)
    );
    x.knots[0].post_tan_width = 2.;
    x.knots[1].pre_tan_width = 2.;
    assert_eq!(
        x.evaluate_checked(0.5),
        Err(SplineQueryError::RegressiveTangents)
    );
    assert_eq!(x.evaluate(0.5), None);
    assert_eq!(
        x.retimed(LayerOffset {
            offset: 0.,
            scale: -1.
        }),
        None
    );
    let arc = &mut imported.prim("/A").references.explicit.as_mut().unwrap()[0];
    let original = arc.clone();
    arc.custom_data
        .push(("nan".into(), Value::Double(f64::NAN)));
    assert_ne!(*arc, original);
    assert_eq!(*arc, arc.clone());
    let reordered = arc.clone();
    arc.custom_data.reverse();
    assert_eq!(*arc, reordered);
}

#[test]
fn stale_auto_ease_tangents_are_preserved_in_usdc_and_reported_for_usda() {
    let mut layer = Imported::usda(&SOURCE.replace(", custom", ", autoEase"));
    layer.property("/A.x").spline.as_mut().unwrap().knots[1].post_tan_slope = 123.;
    assert_eq!(
        layer.save_usda(),
        Err(layerstack_usda::save::SaveError::Document(
            layerstack_usda::writer::WriteError::StaleAutoEaseTangent {
                path: "/A.x".into(),
            },
        ))
    );
    let mut saved = Imported::usdc(&layer.save_usdc().unwrap());
    let knot = &spline(&mut saved, "/A.x").knots[1];
    assert_eq!(knot.post_tan_slope, 123.);
    assert_eq!(knot.post_tan_algorithm, TangentAlgorithm::AutoEase);
}

/// USDA and native USDC imports must preserve typed dictionaries and algorithms;
/// every saved file is compared with native canonical authored state. Numerical
/// checks include left/right values and derivatives at each loop boundary.
#[test]
fn authored_animation_matches_native_openusd() {
    let python = std::env::var("LAYERSTACK_USD_PYTHON").unwrap_or_else(|_| "python3".into());
    if !Command::new(&python)
        .args(["-c", "from pxr import Sdf, Ts, Usd"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipped native oracle: set LAYERSTACK_USD_PYTHON");
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "layerstack-authored-animation-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let mut cases = Vec::new();
    for mode in ["repeat", "reset", "oscillate"] {
        for boundary in ["", "(1)", "(0.5)"] {
            cases.push(
                SOURCE
                    .replace(
                        "pre: loop repeat(1)",
                        &format!("pre: loop {mode}{boundary}"),
                    )
                    .replace(
                        "post: loop oscillate(1)",
                        &format!("post: loop {mode}{boundary}"),
                    ),
            );
        }
    }
    cases.push(
        SOURCE
            .replace("0: 0;", "0: -2 & 0;")
            .replace("2: 5;", "2: 5 & 8;"),
    );
    cases.push(SOURCE.replace(", custom", ", autoEase"));
    cases.push(
        SOURCE
            .replace(", custom", ", autoEase")
            .replace("1: 3", "0.25: 3")
            .replace("repeat(1)", "repeat")
            .replace("oscillate(1)", "oscillate"),
    );
    for (i, source) in cases.iter().enumerate() {
        fs::write(dir.join(format!("source{i}.usda")), source).unwrap();
        let layer = Imported::usda(source);
        fs::write(
            dir.join(format!("saved{i}.usda")),
            layer.save_usda().unwrap(),
        )
        .unwrap();
        fs::write(
            dir.join(format!("saved{i}.usdc")),
            layer.save_usdc().unwrap(),
        )
        .unwrap();
    }
    let script = r#"
import json, sys
from pathlib import Path
from pxr import Sdf, Usd, Tf
folder, count, times = Path(sys.argv[1]), int(sys.argv[2]), json.loads(sys.argv[3])
rows = []
def query(fn, time):
    try: return fn(time)
    except Tf.ErrorException:
        # OpenUSD 26.8's left derivative hits an internal verification for a
        # loopBoundaryTime that names no knot. Its value query is blocked.
        assert spline.EvalPreValue(time) is None or spline.Eval(time) is None
        return None
for i in range(count):
    original = Sdf.Layer.FindOrOpen(str(folder / f'source{i}.usda'))
    original.Export(str(folder / f'native{i}.usdc'))
    expected = original.ExportToString()
    for ext in ['usda', 'usdc']:
        saved = Sdf.Layer.FindOrOpen(str(folder / f'saved{i}.{ext}'))
        assert saved.ExportToString() == expected, (i, ext, saved.ExportToString(), expected)
    spline = original.GetPropertyAtPath('/A.x').GetInfo('spline')
    rows.append([[spline.Eval(t), spline.EvalPreValue(t), query(spline.EvalDerivative, t), query(spline.EvalPreDerivative, t)] for t in times])
root = Sdf.Layer.CreateNew(str(folder / 'root.usda'))
root.subLayerPaths = ['source0.usda']
root.subLayerOffsets[0] = Sdf.LayerOffset(7, 2)
root.Save()
flat = Usd.Stage.Open(root).Flatten()
flat.Export(str(folder / 'retimed.usdc'))
print(json.dumps(rows))
"#;
    let output = Command::new(&python)
        .arg("-c")
        .arg(script)
        .arg(&dir)
        .arg(cases.len().to_string())
        .arg(serde_json::to_string(&TIMES).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Vec<[Option<f64>; 4]>> = serde_json::from_slice(&output.stdout).unwrap();
    for (i, row) in rows.iter().enumerate() {
        let mut layer = Imported::usdc(&fs::read(dir.join(format!("native{i}.usdc"))).unwrap());
        let source = spline(&mut layer, "/A.x");
        let mut original = Imported::usda(&cases[i]);
        assert_eq!(layer.save_usda().unwrap(), original.save_usda().unwrap());
        let text = spline(&mut original, "/A.x");
        for (time, expected) in TIMES.iter().zip(row) {
            for spline in [&source, &text] {
                let actual = [
                    spline.evaluate_checked(*time).unwrap(),
                    spline.evaluate_pre_value(*time).unwrap(),
                    spline.evaluate_derivative(*time).unwrap(),
                    spline.evaluate_pre_derivative(*time).unwrap(),
                ];
                for (a, b) in actual.into_iter().zip(expected) {
                    match (a, b) {
                        (Some(a), Some(b)) => {
                            assert!((a - b).abs() < 1e-8, "case {i} time {time}: {a} != {b}");
                        }
                        (None, None) => {}
                        _ => panic!("case {i} time {time}: {a:?} != {b:?}"),
                    }
                }
            }
        }
    }
    let mut flat = Imported::usdc(&fs::read(dir.join("retimed.usdc")).unwrap());
    let mut original = Imported::usda(&cases[0]);
    for name in ["/A.x", "/A.clock"] {
        let ours = spline(&mut original, name)
            .retimed(LayerOffset {
                offset: 7.,
                scale: 2.,
            })
            .unwrap();
        let mut theirs = spline(&mut flat, name);
        let mut ours = ours;
        for (a, b) in ours.knots.iter_mut().zip(&mut theirs.knots) {
            assert_eq!(a.custom_data.len(), b.custom_data.len());
            a.custom_data.clear();
            b.custom_data.clear();
        }
        assert_eq!(theirs, ours);
    }
    fs::remove_dir_all(dir).unwrap();
}
