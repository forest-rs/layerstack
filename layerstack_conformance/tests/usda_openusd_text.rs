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

use std::path::PathBuf;
use std::sync::Arc;

use layerstack::doc::{LayerId, LayerOffset, SublayerEntry};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{
    AssetResolveError, AssetResolver, CompositionError, InMemoryStore, ResolvedAsset, Stage,
    StageOptions, UnresolvedSublayer, Value,
};
use layerstack_conformance::authored::{Names, dump_layer};
use layerstack_conformance::save_corpus::Imported;
use layerstack_conformance::workspace_root;

fn fixture_dir() -> PathBuf {
    workspace_root().join("layerstack_conformance/fixtures/openusd_usda_text")
}

fn fixture(name: &str) -> (Imported, Imported) {
    let dir = fixture_dir();
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

/// Asset paths are read whole, whatever they hold: OpenUSD writes a path
/// holding `@` between `@@@` delimiters, and one or two `@` before the
/// closing three belong to the path.
///
/// Spec: AOUSD Core §16.2.9; `_StringFromAssetPath`
/// (`pxr/usd/sdf/fileIO_Common.cpp`), `Sdf_EvalAssetPath`.
#[test]
fn asset_paths_read_as_openusd_wrote_them() {
    let mut layer = read_alike("asset_paths");
    for (name, path) in [
        ("at", "tex@1001.png"),
        ("trailingAt", "a@"),
        ("url", "http://host/dir/a b.png"),
        ("fragment", "a.usdz[b#c.png]"),
        ("quotes", "it's \"q\".png"),
    ] {
        assert_eq!(
            default_value(&mut layer, &format!("/Assets.{name}")),
            Value::Asset(path.into()),
            "{name}"
        );
    }
}

/// A value of each type reads as its USDC does; `frame4d` is a matrix.
///
/// Spec: AOUSD Core §6.3–§6.5 (value types and their roles).
#[test]
fn value_types_read_as_openusd_wrote_them() {
    let mut layer = read_alike("value_types");
    let frame: [f64; 16] = core::array::from_fn(|i| (i + 1) as f64);
    assert_eq!(
        default_value(&mut layer, "/Values.frame4d"),
        Value::Matrix4d(Box::new(frame))
    );
    assert_eq!(
        default_value(&mut layer, "/Values.uchar"),
        Value::UChar(200)
    );
}

/// `half` literals round to nearest even through `float` and keep
/// subnormals, as OpenUSD's reading of the same text does.
///
/// Spec: AOUSD Core §6.3 (`half`); `GfHalf(float)`
/// (`pxr/base/gf/ilmbase_half.cpp`), `Sdf_ParserHelpers` (`parserHelpers.cpp`).
#[test]
fn half_literals_round_as_openusd_rounds_them() {
    let mut layer = read_alike("half_literals");
    for (name, bits) in [
        ("nearest", 0x2e66),
        ("third", 0x3554),
        ("tieToEven", 0x3c00),
        ("tieToOdd", 0x3c02),
        ("subnormal", 0x0001),
        ("subnormalRounded", 0x0002),
        ("aboveHalfSmallest", 0x0001),
        ("halfSmallest", 0x0000),
        ("subnormalToNormal", 0x0400),
        ("largest", 0x7bff),
        ("overflow", 0x7c00),
        ("negativeOverflow", 0xfc00),
        ("negativeZero", 0x8000),
        ("infinity", 0x7c00),
        ("negativeInfinity", 0xfc00),
        ("notANumber", 0x7e00),
    ] {
        assert_eq!(
            default_value(&mut layer, &format!("/Halves.{name}")),
            Value::Half(bits),
            "{name}"
        );
    }
}

/// A number takes the declared type as in OpenUSD's reading of the same
/// text: for `bool`, true when nonzero; for an integer type, truncated
/// toward zero; for a floating-point type, signed zero kept.
///
/// Spec: AOUSD Core §6.3; `Sdf_ParserHelpers::_GetImpl`
/// (`pxr/usd/sdf/parserHelpers.h`), `GfNumericCast`.
#[test]
fn numbers_take_the_declared_type_as_openusd_reads_them() {
    let mut layer = read_alike("numeric_literals");
    for (name, value) in [
        ("negativeZero", Value::Bool(false)),
        ("two", Value::Bool(true)),
        ("fraction", Value::Bool(true)),
        ("negativeInfinity", Value::Bool(true)),
        ("notANumber", Value::Bool(true)),
        (
            "array",
            Value::Array([false, true, false, true, false].map(Value::Bool).to_vec()),
        ),
        ("byte", Value::UChar(255)),
        ("truncated", Value::Int(1)),
        ("negativeTruncated", Value::Int(-1)),
        ("unsignedTruncated", Value::UInt(1)),
        ("vector", Value::Vec3i([1, -2, 3])),
    ] {
        assert_eq!(
            default_value(&mut layer, &format!("/Numbers.{name}")),
            value,
            "{name}"
        );
    }
}

/// Every attribute statement OpenUSD rejects for its declared type (see
/// `numeric_rejected.txt`, checked by `generate.py`) is reported, and its
/// value is not imported as another type.
#[test]
fn values_openusd_rejects_are_reported() {
    let dir = workspace_root().join("layerstack_conformance/fixtures/openusd_usda_text");
    let statements = std::fs::read_to_string(dir.join("numeric_rejected.txt")).unwrap();
    for statement in statements.lines() {
        let source = format!(
            "#usda 1.0\ndef \"P\"\n{{\n    {}\n}}\n",
            statement.replace("\\n", "\n")
        );
        let parsed = layerstack_usda::parser::parse(&source);
        assert!(parsed.diagnostics.is_empty(), "{statement}");
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let result = layerstack_usda::emit::emit(
            &parsed.layer,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut layerstack_conformance::save_corpus::AnyAsset::default(),
        );
        assert!(!result.diagnostics.is_empty(), "{statement}: not reported");
        for prim in result.layer.prims.values() {
            for entry in &prim.properties {
                assert!(
                    entry.spec.default.is_none()
                        && entry.spec.time_samples.as_ref().is_none_or(Vec::is_empty),
                    "{statement}: a value was imported"
                );
            }
        }
    }
}

/// Resolves assets to the USDA fixtures beside the layer being read; any
/// other asset path does not resolve.
struct FixtureResolver;

impl AssetResolver for FixtureResolver {
    fn resolve(
        &mut self,
        asset_path: &str,
        _: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let path = fixture_dir().join(asset_path);
        let text = std::fs::read_to_string(&path).map_err(|_| AssetResolveError::NotFound)?;
        let parsed = layerstack_usda::parser::parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let id = LayerId(2);
        let emitted = layerstack_usda::emit::emit(&parsed.layer, id, tokens, paths, self);
        Ok(ResolvedAsset {
            layer_id: id,
            resolved_path: Arc::from(path.to_string_lossy().as_ref()),
            layer: Some(emitted.layer),
        })
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// A sublayer whose asset does not resolve is kept by both readers and
/// reported when the layer stack is gathered, and the rest of the layer
/// stack composes, as OpenUSD reports `PcpErrorInvalidSublayerPath` for the
/// same files (`generate.py` checks that).
///
/// Spec: AOUSD Core §10.3.1, §10.6.
#[test]
fn unresolved_sublayers_are_reported_by_both_readers() {
    let text = std::fs::read_to_string(fixture_dir().join("unresolved_sublayer.usda")).unwrap();
    let bytes = std::fs::read(fixture_dir().join("unresolved_sublayer.usdc")).unwrap();
    for format in ["usda", "usdc"] {
        let mut store = InMemoryStore::default();
        let root = LayerId(1);
        let (layer, resolved) = if format == "usda" {
            let parsed = layerstack_usda::parser::parse(&text);
            let emitted = layerstack_usda::emit::emit(
                &parsed.layer,
                root,
                &mut store.tokens,
                &mut store.paths,
                &mut FixtureResolver,
            );
            assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
            (emitted.layer, emitted.resolved_layers)
        } else {
            let read = layerstack_usdc::read_usdc(
                &bytes,
                root,
                &mut store.tokens,
                &mut store.paths,
                &mut FixtureResolver,
            )
            .unwrap();
            (read.layer, read.resolved_layers)
        };
        assert_eq!(
            layer.sublayers,
            [
                SublayerEntry::unresolved(
                    "./missing.usda",
                    LayerOffset {
                        offset: 5.0,
                        scale: 1.0
                    }
                ),
                SublayerEntry::new(LayerId(2)),
            ],
            "{format}"
        );
        store.insert_layer(layer);
        for layer in resolved {
            store.insert_layer(layer);
        }
        let stage = Stage::compose(&mut store, root, StageOptions::default());
        assert_eq!(
            stage.composition_errors(),
            [CompositionError::UnresolvedSublayer(UnresolvedSublayer {
                layer: root,
                asset: "./missing.usda".into(),
            })],
            "{format}"
        );
        for prim in ["/Root", "/Weak"] {
            assert!(stage.has_prim(store.path(prim)), "{format}: {prim}");
        }
    }
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
