// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The USDA reader over text OpenUSD writes.
//!
//! Each fixture of `fixtures/openusd_usda_text` is one layer OpenUSD 26.08
//! exported as USDA and as USDC (see `generate.py` there). The USDA reader
//! must read the text as the USDC reader reads the crate file: the same
//! authored layer, dumped with names resolved, and no diagnostics. Some
//! values are also checked directly, so that a mistake shared by both
//! readers cannot pass.

use layerstack::Value;
use layerstack_conformance::authored::{Names, dump_layer};
use layerstack_conformance::save_corpus::Imported;
use layerstack_conformance::workspace_root;

fn fixture(name: &str) -> (Imported, Imported) {
    let dir = workspace_root().join("layerstack_conformance/fixtures/openusd_usda_text");
    let text = std::fs::read_to_string(dir.join(format!("{name}.usda"))).unwrap();
    let bytes = std::fs::read(dir.join(format!("{name}.usdc"))).unwrap();
    (Imported::usda(&text), Imported::usdc(&bytes))
}

fn dump(imported: &Imported) -> Vec<String> {
    dump_layer(
        &imported.layer,
        Names {
            tokens: &imported.tokens,
            paths: &imported.paths,
        },
    )
}

/// Reads fixture `name` from both formats and requires the same layer.
fn read_alike(name: &str) -> Imported {
    let (usda, usdc) = fixture(name);
    assert_eq!(dump(&usda), dump(&usdc), "{name}: USDA and USDC differ");
    usda
}

fn default_value(imported: &mut Imported, path: &str) -> Value {
    let prim_path = path.split('.').next().unwrap();
    let name = path.rsplit('.').next().unwrap();
    let prim = layerstack::path::Path::parse_absolute(prim_path, &mut imported.tokens).unwrap();
    let prim = imported.paths.lookup(&prim).unwrap();
    let name = imported.tokens.intern(name);
    let spec = imported.layer.prims[&prim]
        .property(name)
        .expect("property");
    spec.default.clone().expect("default value")
}

/// OpenUSD's `Quote` escapes quotes, backslashes and control characters, and
/// writes text with a newline triple-quoted.
///
/// Spec: AOUSD Core §16.2.5; `Sdf_FileIOUtility::Quote`
/// (`pxr/usd/sdf/fileIO_Common.cpp`).
#[test]
fn string_escapes_read_as_openusd_wrote_them() {
    let mut layer = read_alike("strings");
    let tricky: String = (1_u8..32)
        .map(char::from)
        .chain("\x7f say \"hi\" it's \\ \u{e9} \u{65e5}".chars())
        .collect();
    assert_eq!(
        default_value(&mut layer, "/Strings.tricky"),
        Value::String(tricky.as_str().into())
    );
    assert_eq!(
        default_value(&mut layer, "/Strings.multiline"),
        Value::String("one\ntwo \"\"\" three\n".into())
    );
    assert_eq!(
        default_value(&mut layer, "/Strings.bothQuotes"),
        Value::String("it's \"x\"".into())
    );
}

/// OpenUSD writes sparse array edits as `edit [op; op]`, including the
/// `fill` forms of `minsize` and `resize`.
///
/// Source: `Vt_ArrayEditStreamImpl` (`pxr/base/vt/arrayEdit.cpp`) and
/// `ArrayEditValue` (`pxr/usd/sdf/textFileFormatParser.h`).
#[test]
fn array_edits_read_as_openusd_wrote_them() {
    use layerstack::{ArrayEdit, ArrayEditOp as Op, ArrayEditOperand as Operand, ArrayIndex};

    let mut layer = read_alike("array_edits");
    let at = ArrayIndex::Position;
    let literal = |v: i32| Operand::Literal(Value::Int(v));
    let copy = |i: i64| Operand::CopyFrom(at(i));
    let expected = ArrayEdit {
        ops: vec![
            Op::Write {
                src: literal(9),
                index: at(0),
            },
            Op::Write {
                src: copy(-1),
                index: at(1),
            },
            Op::Insert {
                src: literal(5),
                index: at(2),
            },
            Op::Insert {
                src: copy(0),
                index: at(-2),
            },
            Op::Insert {
                src: literal(1),
                index: at(0),
            },
            Op::Insert {
                src: copy(-1),
                index: at(0),
            },
            Op::Insert {
                src: literal(4),
                index: ArrayIndex::End,
            },
            Op::Insert {
                src: copy(2),
                index: ArrayIndex::End,
            },
            Op::Erase { index: at(-3) },
            Op::MinSize { len: 2 },
            Op::MinSizeFill {
                len: 6,
                fill: Value::Int(7),
            },
            Op::MaxSize { len: 20 },
            Op::Resize { len: 8 },
            Op::ResizeFill {
                len: 10,
                fill: Value::Int(-1),
            },
        ],
    };
    assert_eq!(
        default_value(&mut layer, "/Edits.ints"),
        Value::ArrayEdit(expected)
    );
    let Value::ArrayEdit(points) = default_value(&mut layer, "/Edits.points") else {
        panic!("points is an array edit");
    };
    assert_eq!(
        points.ops[1],
        Op::ResizeFill {
            len: 4,
            fill: Value::Vec3f([0.5, 0.0, -1.0]),
        }
    );
}
