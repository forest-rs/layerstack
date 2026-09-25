// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Writer tests: everything written is read back through this crate's
//! reader (header, TOC, sections and value decoding), and the encoding
//! choices are checked at the byte level.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::header::parse_header;
use crate::section::{CrateSections, parse_sections};
use crate::toc::parse_toc;
use crate::value_rep::{CrateListOp, CrateValue, RawValueRep, decode_value};
use crate::value_type::ValueType;
use crate::version::CrateVersion;

/// A spec as read back: path, form and `(field, rep)` pairs.
type ReadSpec = (String, SpecForm, Vec<(String, RawValueRep)>);

/// A decoded file: sections plus each spec's fields with raw reps.
struct Decoded {
    data: Vec<u8>,
    sections: CrateSections,
}

impl Decoded {
    fn new(data: Vec<u8>) -> Self {
        let header = parse_header(&data).unwrap();
        let toc = parse_toc(&data, header.toc_offset).unwrap();
        let sections = parse_sections(
            &data,
            &toc,
            header.crate_version(),
            &mut crate::DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        Self { data, sections }
    }

    fn version(&self) -> CrateVersion {
        parse_header(&self.data).unwrap().crate_version()
    }

    /// `(path, form, [(field, rep)])` per spec, in file order.
    fn specs(&self) -> Vec<ReadSpec> {
        let s = &self.sections;
        s.specs
            .iter()
            .map(|spec| {
                let mut fields = vec![];
                let mut i = spec.fieldset_index as usize;
                while s.fieldsets[i] >= 0 {
                    let field = s.fields[s.fieldsets[i] as usize];
                    fields.push((
                        s.tokens[field.token_index as usize].clone(),
                        RawValueRep::new(field.value_rep),
                    ));
                    i += 1;
                }
                (s.paths[spec.path_index as usize].clone(), spec.form, fields)
            })
            .collect()
    }

    fn rep(&self, path: &str, field: &str) -> RawValueRep {
        let specs = self.specs();
        let (_, _, fields) = specs.iter().find(|s| s.0 == path).expect("spec");
        fields.iter().find(|f| f.0 == field).expect("field").1
    }

    /// Decodes a field and converts it back to a writer value.
    fn value(&self, path: &str, field: &str) -> Value {
        let rep = self.rep(path, field);
        let decoded = decode_value(&rep, &self.data, &self.sections).unwrap();
        from_crate(rep.value_type().unwrap(), rep.is_array(), &decoded)
    }
}

fn f32s(data: &[u8]) -> Vec<f32> {
    data.chunks(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn f64s(data: &[u8]) -> Vec<f64> {
    data.chunks(8)
        .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn i32s(data: &[u8]) -> Vec<i32> {
    data.chunks(4)
        .map(|c| i32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn u16s(data: &[u8]) -> Vec<u16> {
    data.chunks(2)
        .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn arr<T: Copy + Default, const N: usize>(v: &[T]) -> [T; N] {
    let mut out = [T::default(); N];
    out.copy_from_slice(v);
    out
}

fn mat<const N: usize>(v: &[f64]) -> [[f64; N]; N] {
    let mut out = [[0.0; N]; N];
    for (r, row) in out.iter_mut().enumerate() {
        row.copy_from_slice(&v[r * N..(r + 1) * N]);
    }
    out
}

fn strings(items: &[CrateValue]) -> Vec<String> {
    items
        .iter()
        .map(|v| match v {
            CrateValue::String(s) | CrateValue::Token(s) | CrateValue::AssetPath(s) => s.clone(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect()
}

fn list_op(op: &CrateListOp) -> ListOp<String> {
    ListOp {
        explicit: op.explicit_items.as_deref().map(strings),
        prepended: strings(&op.prepended_items),
        appended: strings(&op.appended_items),
        deleted: strings(&op.deleted_items),
    }
}

/// Converts a decoded value (with its representation's type) back to the
/// writer's model.
fn from_crate(ty: ValueType, is_array: bool, v: &CrateValue) -> Value {
    use ValueType as T;
    if is_array {
        let CrateValue::Array(items) = v else {
            panic!("expected array, got {v:?}");
        };
        let bytes = || -> Vec<u8> {
            items
                .iter()
                .flat_map(|i| match i {
                    CrateValue::Opaque { data, .. } => data.clone(),
                    other => panic!("expected math element, got {other:?}"),
                })
                .collect()
        };
        macro_rules! scalars {
            ($variant:ident, $pat:ident) => {
                items
                    .iter()
                    .map(|i| match i {
                        CrateValue::$pat(x) => *x,
                        other => panic!("unexpected element {other:?}"),
                    })
                    .collect()
            };
        }
        return match ty {
            T::Bool => Value::BoolArray(scalars!(BoolArray, Bool)),
            T::UChar => Value::UCharArray(scalars!(UCharArray, UChar)),
            T::Int => Value::IntArray(scalars!(IntArray, Int)),
            T::UInt => Value::UIntArray(scalars!(UIntArray, UInt)),
            T::Int64 => Value::Int64Array(scalars!(Int64Array, Int64)),
            T::UInt64 => Value::UInt64Array(scalars!(UInt64Array, UInt64)),
            T::Half => Value::HalfArray(scalars!(HalfArray, Half)),
            T::Float => Value::FloatArray(scalars!(FloatArray, Float)),
            T::Double => Value::DoubleArray(scalars!(DoubleArray, Double)),
            T::TimeCode => Value::TimeCodeArray(scalars!(TimeCodeArray, TimeCode)),
            T::String => Value::StringArray(strings(items)),
            T::Token => Value::TokenArray(strings(items)),
            T::AssetPath => Value::AssetArray(strings(items)),
            T::Vec2h => Value::Vec2hArray(u16s(&bytes()).chunks(2).map(arr).collect()),
            T::Vec3h => Value::Vec3hArray(u16s(&bytes()).chunks(3).map(arr).collect()),
            T::Vec4h => Value::Vec4hArray(u16s(&bytes()).chunks(4).map(arr).collect()),
            T::Quath => Value::QuathArray(u16s(&bytes()).chunks(4).map(arr).collect()),
            T::Vec2f => Value::Vec2fArray(f32s(&bytes()).chunks(2).map(arr).collect()),
            T::Vec3f => Value::Vec3fArray(f32s(&bytes()).chunks(3).map(arr).collect()),
            T::Vec4f => Value::Vec4fArray(f32s(&bytes()).chunks(4).map(arr).collect()),
            T::Quatf => Value::QuatfArray(f32s(&bytes()).chunks(4).map(arr).collect()),
            T::Vec2d => Value::Vec2dArray(f64s(&bytes()).chunks(2).map(arr).collect()),
            T::Vec3d => Value::Vec3dArray(f64s(&bytes()).chunks(3).map(arr).collect()),
            T::Vec4d => Value::Vec4dArray(f64s(&bytes()).chunks(4).map(arr).collect()),
            T::Quatd => Value::QuatdArray(f64s(&bytes()).chunks(4).map(arr).collect()),
            T::Vec2i => Value::Vec2iArray(i32s(&bytes()).chunks(2).map(arr).collect()),
            T::Vec3i => Value::Vec3iArray(i32s(&bytes()).chunks(3).map(arr).collect()),
            T::Vec4i => Value::Vec4iArray(i32s(&bytes()).chunks(4).map(arr).collect()),
            T::Matrix2d => Value::Matrix2dArray(f64s(&bytes()).chunks(4).map(mat).collect()),
            T::Matrix3d => Value::Matrix3dArray(f64s(&bytes()).chunks(9).map(mat).collect()),
            T::Matrix4d => Value::Matrix4dArray(f64s(&bytes()).chunks(16).map(mat).collect()),
            other => panic!("unexpected array type {other:?}"),
        };
    }
    match (ty, v) {
        (T::ValueBlock, CrateValue::None) => Value::Block,
        (_, CrateValue::Bool(x)) => Value::Bool(*x),
        (_, CrateValue::UChar(x)) => Value::UChar(*x),
        (_, CrateValue::Int(x)) => Value::Int(*x),
        (_, CrateValue::UInt(x)) => Value::UInt(*x),
        (_, CrateValue::Int64(x)) => Value::Int64(*x),
        (_, CrateValue::UInt64(x)) => Value::UInt64(*x),
        (_, CrateValue::Half(x)) => Value::Half(*x),
        (_, CrateValue::Float(x)) => Value::Float(*x),
        (T::TimeCode, CrateValue::TimeCode(x)) => Value::TimeCode(*x),
        (_, CrateValue::Double(x)) => Value::Double(*x),
        (_, CrateValue::String(x)) => Value::String(x.clone()),
        (_, CrateValue::Token(x)) => Value::Token(x.clone()),
        (_, CrateValue::AssetPath(x)) => Value::Asset(x.clone()),
        (_, CrateValue::Specifier(x)) => Value::Specifier(match x {
            0 => Specifier::Def,
            1 => Specifier::Over,
            _ => Specifier::Class,
        }),
        (_, CrateValue::Variability(x)) => Value::Variability(if *x == 0 {
            Variability::Varying
        } else {
            Variability::Uniform
        }),
        (_, CrateValue::Permission(x)) => Value::Permission(if *x == 0 {
            Permission::Public
        } else {
            Permission::Private
        }),
        (_, CrateValue::Opaque { value_type, data }) => match value_type {
            T::Vec2h => Value::Vec2h(arr(&u16s(data))),
            T::Vec3h => Value::Vec3h(arr(&u16s(data))),
            T::Vec4h => Value::Vec4h(arr(&u16s(data))),
            T::Quath => Value::Quath(arr(&u16s(data))),
            T::Vec2f => Value::Vec2f(arr(&f32s(data))),
            T::Vec3f => Value::Vec3f(arr(&f32s(data))),
            T::Vec4f => Value::Vec4f(arr(&f32s(data))),
            T::Quatf => Value::Quatf(arr(&f32s(data))),
            T::Vec2d => Value::Vec2d(arr(&f64s(data))),
            T::Vec3d => Value::Vec3d(arr(&f64s(data))),
            T::Vec4d => Value::Vec4d(arr(&f64s(data))),
            T::Quatd => Value::Quatd(arr(&f64s(data))),
            T::Vec2i => Value::Vec2i(arr(&i32s(data))),
            T::Vec3i => Value::Vec3i(arr(&i32s(data))),
            T::Vec4i => Value::Vec4i(arr(&i32s(data))),
            T::Matrix2d => Value::Matrix2d(mat(&f64s(data))),
            T::Matrix3d => Value::Matrix3d(mat(&f64s(data))),
            T::Matrix4d => Value::Matrix4d(mat(&f64s(data))),
            other => panic!("unexpected math type {other:?}"),
        },
        (_, CrateValue::Dictionary(entries)) => Value::Dictionary(
            entries
                .iter()
                .map(|(k, v)| {
                    // Nested reps are not exposed; infer from the value.
                    let (ty, is_array) = match v {
                        CrateValue::Array(items) => (
                            match items.first() {
                                Some(CrateValue::Int(_)) => T::Int,
                                Some(CrateValue::Token(_)) => T::Token,
                                Some(CrateValue::Double(_)) => T::Double,
                                Some(CrateValue::Opaque { value_type, .. }) => *value_type,
                                other => panic!("unsupported nested array {other:?}"),
                            },
                            true,
                        ),
                        _ => (T::Unknown, false),
                    };
                    (k.clone(), from_crate(ty, is_array, v))
                })
                .collect(),
        ),
        (_, CrateValue::TokenVector(items)) => Value::TokenVector(items.clone()),
        (T::TokenListOp, CrateValue::ListOp(op)) => Value::TokenListOp(list_op(op)),
        (T::StringListOp, CrateValue::ListOp(op)) => Value::StringListOp(list_op(op)),
        (T::PathListOp, CrateValue::ListOp(op)) => Value::PathListOp(list_op(op)),
        other => panic!("unexpected value {other:?}"),
    }
}

fn root() -> Spec {
    Spec::new("/", SpecForm::PseudoRoot)
        .with_field("primChildren", Value::TokenVector(vec!["Root".into()]))
}

fn attribute(prim: &str, name: &str, value: Value) -> Spec {
    Spec::new(alloc::format!("{prim}.{name}"), SpecForm::Attribute).with_field("default", value)
}

fn layer_with(values: &[(&str, Value)]) -> Vec<Spec> {
    let mut specs = vec![
        root(),
        Spec::new("/Root", SpecForm::Prim)
            .with_field("specifier", Value::Specifier(Specifier::Def))
            .with_field(
                "properties",
                Value::TokenVector(values.iter().map(|(n, _)| (*n).to_string()).collect()),
            ),
    ];
    for (name, value) in values {
        specs.push(attribute("/Root", name, value.clone()));
    }
    specs
}

fn every_kind() -> Vec<(&'static str, Value)> {
    let ramp = |n: usize| (0..n).map(|i| i as f32 * 0.37 + 0.1).collect::<Vec<f32>>();
    vec![
        ("block", Value::Block),
        ("bool", Value::Bool(true)),
        ("uchar", Value::UChar(200)),
        ("int", Value::Int(-7)),
        ("uint", Value::UInt(u32::MAX)),
        ("int64Small", Value::Int64(-5)),
        ("int64Big", Value::Int64(-(1 << 40))),
        ("uint64Small", Value::UInt64(7)),
        ("uint64Big", Value::UInt64(u64::MAX)),
        ("half", Value::Half(0x3c00)),
        ("halfFrac", Value::Half(0x3555)),
        ("halfNegZero", Value::Half(0x8000)),
        ("halfSubnormal", Value::Half(0x0001)),
        ("halfNan", Value::Half(0x7c01)),
        ("float", Value::Float(0.1)),
        ("doubleInline", Value::Double(-0.5)),
        ("double", Value::Double(0.1)),
        ("doubleNegZero", Value::Double(-0.0)),
        ("doubleInf", Value::Double(f64::INFINITY)),
        ("string", Value::String("say \"hi\"\n".into())),
        ("token", Value::Token("tok".into())),
        ("asset", Value::Asset("textures/a.png".into())),
        ("specifier", Value::Specifier(Specifier::Class)),
        ("variability", Value::Variability(Variability::Uniform)),
        ("permission", Value::Permission(Permission::Private)),
        ("vec2h", Value::Vec2h([0x3c00, 0xc000])),
        ("vec2hFrac", Value::Vec2h([0x3800, 0x3555])),
        ("vec3hFrac", Value::Vec3h([0x3800, 0, 0])),
        ("vec4h", Value::Vec4h([0, 0x3c00, 0x4000, 0x4200])),
        ("vec2f", Value::Vec2f([0.5, 1.0])),
        ("vec3fSmall", Value::Vec3f([0.0, -1.0, 2.0])),
        ("vec3fNegZero", Value::Vec3f([-0.0, 1.0, 2.0])),
        ("vec4f", Value::Vec4f([1.0, 2.0, 3.0, 200.0])),
        ("vec2d", Value::Vec2d([-128.0, 127.0])),
        ("vec3d", Value::Vec3d([0.1, 0.2, 0.3])),
        ("vec4d", Value::Vec4d([1.0, 2.0, 3.0, 4.0])),
        ("vec2i", Value::Vec2i([-3, 4])),
        ("vec3i", Value::Vec3i([1000, 0, 0])),
        ("vec4i", Value::Vec4i([1, 2, 3, 4])),
        ("quath", Value::Quath([0, 0, 0, 0x3c00])),
        ("quatf", Value::Quatf([0.0, 0.0, 0.0, 1.0])),
        ("quatd", Value::Quatd([0.5, 0.5, 0.5, 0.5])),
        ("matrix2d", Value::Matrix2d([[1.0, 2.0], [3.0, 4.0]])),
        (
            "matrix3dDiagonal",
            Value::Matrix3d([[2.0, 0.0, 0.0], [0.0, -1.0, 0.0], [0.0, 0.0, 1.0]]),
        ),
        (
            "matrix4dIdentity",
            Value::Matrix4d([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ]),
        ),
        (
            "matrix4d",
            Value::Matrix4d([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [2.5, 3.0, 4.0, 1.0],
            ]),
        ),
        ("boolArray", Value::BoolArray(vec![true, false, true])),
        ("ucharArray", Value::UCharArray(vec![1, 2, 255])),
        ("intArray", Value::IntArray(vec![1, -2, 3])),
        (
            "intArrayLong",
            Value::IntArray(
                (0..40)
                    .map(|i| i * i - 100)
                    .chain([i32::MIN, i32::MAX])
                    .collect(),
            ),
        ),
        (
            "uintArrayLong",
            Value::UIntArray((0..20).map(|i| u32::MAX - i).collect()),
        ),
        (
            "int64ArrayLong",
            Value::Int64Array((0..20).map(|i| i << 35).collect()),
        ),
        ("uint64Array", Value::UInt64Array(vec![u64::MAX, 0])),
        ("halfArray", Value::HalfArray(vec![0x3c00; 20])),
        (
            "floatArray",
            Value::FloatArray(vec![f32::INFINITY, f32::NEG_INFINITY, -0.0, 0.1]),
        ),
        (
            "floatArrayInts",
            Value::FloatArray((0..20).map(|i| (i * 3 - 30) as f32).collect()),
        ),
        (
            "floatArrayNegZero",
            Value::FloatArray(
                (0..20)
                    .map(|i| if i == 5 { -0.0 } else { i as f32 })
                    .collect(),
            ),
        ),
        (
            "floatArrayLut",
            Value::FloatArray((0..20).map(|i| [0.5, 1.5, 2.5][i % 3]).collect()),
        ),
        ("floatArrayPlain", Value::FloatArray(ramp(20))),
        (
            "doubleArrayInts",
            Value::DoubleArray((0..20).map(|i| f64::from(i * -1000)).collect()),
        ),
        (
            "doubleArrayLut",
            Value::DoubleArray((0..20).map(|i| [0.1, 0.2][i % 2]).collect()),
        ),
        ("timeCodeArray", Value::TimeCodeArray(vec![1.0, 24.5])),
        (
            "stringArray",
            Value::StringArray(vec!["a".into(), "b c".into()]),
        ),
        (
            "tokenArray",
            Value::TokenArray(vec!["x".into(), "y".into()]),
        ),
        (
            "assetArray",
            Value::AssetArray(vec!["a.png".into(), "b/c.wav".into()]),
        ),
        ("emptyArray", Value::Vec3fArray(vec![])),
        ("vec2hArray", Value::Vec2hArray(vec![[1, 2]])),
        ("vec3hArray", Value::Vec3hArray(vec![[1, 2, 3]])),
        ("vec4hArray", Value::Vec4hArray(vec![[1, 2, 3, 4]])),
        (
            "vec2fArray",
            Value::Vec2fArray(vec![[0.0, 1.0], [0.5, 0.5]]),
        ),
        ("vec3fArray", Value::Vec3fArray(vec![[0.0, 1.0, 2.0]; 20])),
        ("vec4fArray", Value::Vec4fArray(vec![[1.0, 0.5, 0.25, 1.0]])),
        ("vec2dArray", Value::Vec2dArray(vec![[1.0, 2.0]])),
        ("vec3dArray", Value::Vec3dArray(vec![[1.0, 2.0, 3.0]])),
        ("vec4dArray", Value::Vec4dArray(vec![[1.0, 2.0, 3.0, 4.0]])),
        ("vec2iArray", Value::Vec2iArray(vec![[1, 2]])),
        ("vec3iArray", Value::Vec3iArray(vec![[1, 2, 3]])),
        ("vec4iArray", Value::Vec4iArray(vec![[1, 2, 3, 4]])),
        ("quathArray", Value::QuathArray(vec![[1, 2, 3, 4]])),
        ("quatfArray", Value::QuatfArray(vec![[0.0, 0.0, 0.0, 1.0]])),
        ("quatdArray", Value::QuatdArray(vec![[0.0, 0.0, 0.0, 1.0]])),
        (
            "matrix2dArray",
            Value::Matrix2dArray(vec![[[1.0, 2.0], [3.0, 4.0]]]),
        ),
        (
            "matrix3dArray",
            Value::Matrix3dArray(vec![[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]]),
        ),
        ("matrix4dArray", Value::Matrix4dArray(vec![[[1.0; 4]; 4]])),
        (
            "dictionary",
            Value::Dictionary(vec![
                ("z".into(), Value::Int(3)),
                ("a".into(), Value::String("x".into())),
                (
                    "nested".into(),
                    Value::Dictionary(vec![
                        ("deep".into(), Value::Vec3f([1.0, 2.5, 3.0])),
                        ("list".into(), Value::TokenArray(vec!["p".into()])),
                        ("d".into(), Value::Double(0.1)),
                    ]),
                ),
                ("empty".into(), Value::Dictionary(vec![])),
            ]),
        ),
        ("emptyDictionary", Value::Dictionary(vec![])),
        (
            "tokenVector",
            Value::TokenVector(vec!["a".into(), "b".into()]),
        ),
        (
            "tokenListOp",
            Value::TokenListOp(ListOp::prepend(vec!["MaterialBindingAPI".into()])),
        ),
        (
            "tokenListOpEdits",
            Value::TokenListOp(ListOp {
                explicit: None,
                prepended: vec!["a".into()],
                appended: vec!["b".into(), "c".into()],
                deleted: vec!["d".into()],
            }),
        ),
        (
            "tokenListOpExplicitEmpty",
            Value::TokenListOp(ListOp::explicit(vec![])),
        ),
        (
            "stringListOp",
            Value::StringListOp(ListOp::explicit(vec!["shading".into()])),
        ),
        (
            "pathListOp",
            Value::PathListOp(ListOp::explicit(vec![
                "/Root/Looks/Mat.outputs:surface".into(),
                "/Other".into(),
            ])),
        ),
    ]
}

/// Dictionary entries come back in key order.
fn sorted_dictionaries(value: Value) -> Value {
    match value {
        Value::Dictionary(mut entries) => {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Dictionary(
                entries
                    .into_iter()
                    .map(|(k, v)| (k, sorted_dictionaries(v)))
                    .collect(),
            )
        }
        other => other,
    }
}

#[test]
fn every_value_kind_round_trips() {
    let values = every_kind();
    let bytes = write_crate(&layer_with(&values)).unwrap();
    let file = Decoded::new(bytes);
    for (name, value) in values {
        let read = file.value(&alloc::format!("/Root.{name}"), "default");
        let expected = sorted_dictionaries(value);
        // Compare bit patterns so NaN, -0 and infinities are exact.
        assert_eq!(
            alloc::format!("{read:?}"),
            alloc::format!("{expected:?}"),
            "{name}"
        );
    }
}

#[test]
fn nan_payloads_survive() {
    let nan = f32::from_bits(0x7fc0_0001);
    let values = [("f", Value::FloatArray(vec![nan; 20]))];
    let file = Decoded::new(write_crate(&layer_with(&values)).unwrap());
    let Value::FloatArray(read) = file.value("/Root.f", "default") else {
        panic!("float array");
    };
    assert!(
        read.iter().all(|x| x.to_bits() == nan.to_bits()),
        "NaN bits"
    );
}

/// Bytes 0–5 of a representation: the inlined payload or data offset.
fn payload(rep: RawValueRep) -> usize {
    usize::try_from(rep.payload_offset()).unwrap()
}

#[test]
fn inlining_follows_openusd() {
    let file = Decoded::new(write_crate(&layer_with(&every_kind())).unwrap());
    let inlined = |name: &str| {
        file.rep(&alloc::format!("/Root.{name}"), "default")
            .is_inlined()
    };
    for name in [
        "block",
        "bool",
        "uchar",
        "int",
        "uint",
        "int64Small",
        "uint64Small",
        "half",
        "halfFrac",
        "halfNegZero",
        "halfSubnormal",
        "halfNan",
        "float",
        "doubleInline",
        "doubleNegZero",
        "string",
        "token",
        "asset",
        "specifier",
        "variability",
        "permission",
        "vec2h",
        "vec2hFrac",
        "vec4h",
        "vec3fSmall",
        "vec2d",
        "vec2i",
        "vec4i",
        "matrix3dDiagonal",
        "matrix4dIdentity",
        "emptyDictionary",
    ] {
        assert!(inlined(name), "{name} is inlined");
    }
    for name in [
        "int64Big",
        "uint64Big",
        "double",
        "doubleInf",
        "vec3hFrac",
        "vec3fNegZero",
        "vec4f",
        "vec3i",
        "quatf",
        "matrix2d",
        "matrix4d",
        "dictionary",
    ] {
        assert!(!inlined(name), "{name} is stored out of line");
    }
    // (0, -1, 2) is three int8s, low byte first.
    let rep = file.rep("/Root.vec3fSmall", "default");
    assert_eq!(payload(rep), 0x02_ff_00, "int8 components");
    // Types of at most four bytes are always inlined as their own bytes
    // (`_IsAlwaysInlined`): a `half2` holds its two halves, integral or not,
    // never `int8` components.
    let bits = |name: &str| payload(file.rep(&alloc::format!("/Root.{name}"), "default"));
    assert_eq!(bits("half"), 0x3c00, "half bits");
    assert_eq!(bits("halfNegZero"), 0x8000, "-0 half keeps its sign");
    assert_eq!(bits("halfNan"), 0x7c01, "half NaN payload");
    assert_eq!(bits("vec2h"), 0xc000_3c00, "half2 (1, -2) bits");
    assert_eq!(bits("vec2hFrac"), 0x3555_3800, "half2 (0.5, 1/3) bits");
    assert_eq!(bits("uint"), 0xffff_ffff, "uint bits");
    // A `half4` is eight bytes: integral components are `int8`s.
    assert_eq!(bits("vec4h"), 0x03_02_01_00, "half4 int8 components");
    // A scalar asset path is a token index; strings are string indexes.
    let asset = payload(file.rep("/Root.asset", "default"));
    assert_eq!(file.sections.tokens[asset], "textures/a.png", "token index");
}

#[test]
fn array_encodings_follow_openusd() {
    let file = Decoded::new(write_crate(&layer_with(&every_kind())).unwrap());
    let rep = |name: &str| file.rep(&alloc::format!("/Root.{name}"), "default");
    let compressed = |name: &str| rep(name).is_compressed();
    for name in [
        "intArrayLong",
        "uintArrayLong",
        "int64ArrayLong",
        "halfArray",
        "floatArrayInts",
        "floatArrayLut",
        "doubleArrayInts",
        "doubleArrayLut",
    ] {
        assert!(compressed(name), "{name} is compressed");
    }
    for name in [
        "intArray",
        "floatArray",
        "floatArrayPlain",
        "floatArrayNegZero",
        "vec3fArray",
        "boolArray",
    ] {
        assert!(!compressed(name), "{name} is not compressed");
    }
    let code = |name: &str| file.data[payload(rep(name)) + 8];
    assert_eq!(code("floatArrayInts"), b'i', "integer-coded floats");
    assert_eq!(code("doubleArrayInts"), b'i', "integer-coded doubles");
    assert_eq!(code("floatArrayLut"), b't', "lookup table");
    // Uncompressed arrays are 8-byte aligned; integer arrays need not be.
    for name in ["floatArray", "vec3fArray", "boolArray", "tokenArray"] {
        assert_eq!(payload(rep(name)) % 8, 0, "{name} aligned");
    }
    // Empty arrays have payload 0.
    let empty = rep("emptyArray");
    assert!(empty.is_array() && payload(empty) == 0, "empty array");
}

#[test]
fn identical_values_are_stored_once() {
    let points = Value::Vec3fArray(vec![[0.5, 1.5, 2.5]; 3]);
    let values = [("a", points.clone()), ("b", points)];
    let file = Decoded::new(write_crate(&layer_with(&values)).unwrap());
    assert_eq!(
        file.rep("/Root.a", "default"),
        file.rep("/Root.b", "default"),
        "same representation"
    );
    // Equal fields share one field entry.
    let fields = &file.sections.fields;
    let defaults = fields
        .iter()
        .filter(|f| file.sections.tokens[f.token_index as usize] == "default")
        .count();
    assert_eq!(defaults, 1, "one default field");
}

#[test]
fn spec_layout_matches_sdf_crate_data() {
    // Given in scrambled order, laid out as `Sdf_CrateData::Save` does.
    let specs = vec![
        attribute("/Root/B", "z", Value::Int(1)),
        Spec::new("/Root/B", SpecForm::Prim),
        attribute("/Root", "z", Value::Int(1)),
        attribute("/Root", "a", Value::Int(1)),
        Spec::new("/Root", SpecForm::Prim),
        Spec::new("/Root/A", SpecForm::Prim),
        root(),
    ];
    let bytes = write_crate(&specs).unwrap();
    let file = Decoded::new(bytes.clone());
    let order: Vec<String> = file.specs().into_iter().map(|s| s.0).collect();
    assert_eq!(
        order,
        [
            "/",
            "/Root",
            "/Root/A",
            "/Root/B",
            "/Root.a",
            "/Root.z",
            "/Root/B.z"
        ],
        "prims in path order, then properties by name"
    );
    let mut reversed = specs;
    reversed.reverse();
    assert_eq!(
        write_crate(&reversed).unwrap(),
        bytes,
        "input order is irrelevant"
    );
    assert_eq!(write_crate(&reversed).unwrap(), bytes, "deterministic");
    // The path table was built in the same order, parents first; the tree
    // decodes back to every path.
    assert_eq!(
        file.sections.paths,
        [
            "/",
            "/Root",
            "/Root/A",
            "/Root/B",
            "/Root.a",
            "/Root.z",
            "/Root/B.z"
        ],
        "path table"
    );
    assert_eq!(file.sections.tokens[0], ";-)", "sentinel token 0");
}

#[test]
fn relationships_connections_and_list_ops() {
    let specs = vec![
        Spec::new("/", SpecForm::PseudoRoot)
            .with_field("primChildren", Value::TokenVector(vec!["Root".into()])),
        Spec::new("/Root", SpecForm::Prim)
            .with_field("specifier", Value::Specifier(Specifier::Def))
            .with_field("typeName", Value::Token("Mesh".into()))
            .with_field(
                "apiSchemas",
                Value::TokenListOp(ListOp::prepend(vec!["MaterialBindingAPI".into()])),
            )
            .with_field(
                "properties",
                Value::TokenVector(vec!["material:binding".into(), "inputs:x".into()]),
            ),
        Spec::new("/Root.material:binding", SpecForm::Relationship)
            .with_field("variability", Value::Variability(Variability::Uniform))
            .with_field(
                "targetPaths",
                Value::PathListOp(ListOp::explicit(vec!["/Looks/Mat".into()])),
            ),
        Spec::new("/Root.inputs:x", SpecForm::Attribute)
            .with_field("custom", Value::Bool(false))
            .with_field("typeName", Value::Token("float".into()))
            .with_field("variability", Value::Variability(Variability::Varying))
            .with_field(
                "connectionPaths",
                Value::PathListOp(ListOp::explicit(vec!["/Looks/Mat/Tex.outputs:r".into()])),
            ),
    ];
    let file = Decoded::new(write_crate(&specs).unwrap());
    assert_eq!(
        file.value("/Root.material:binding", "targetPaths"),
        Value::PathListOp(ListOp::explicit(vec!["/Looks/Mat".into()])),
        "targets"
    );
    assert_eq!(
        file.value("/Root.inputs:x", "connectionPaths"),
        Value::PathListOp(ListOp::explicit(vec!["/Looks/Mat/Tex.outputs:r".into()])),
        "connections"
    );
    assert_eq!(
        file.value("/Root", "apiSchemas"),
        Value::TokenListOp(ListOp::prepend(vec!["MaterialBindingAPI".into()])),
        "apiSchemas"
    );
    let specs = file.specs();
    let form = |p: &str| specs.iter().find(|s| s.0 == p).unwrap().1;
    assert_eq!(
        form("/Root.material:binding"),
        SpecForm::Relationship,
        "form"
    );
    // Target paths enter the path table with their ancestors, but get no
    // specs.
    for path in [
        "/Looks",
        "/Looks/Mat",
        "/Looks/Mat/Tex",
        "/Looks/Mat/Tex.outputs:r",
    ] {
        assert!(
            file.sections.paths.iter().any(|p| p == path),
            "{path} in table"
        );
    }
    assert_eq!(specs.len(), 4, "only the given specs");
}

#[test]
fn version_is_upgraded_only_for_timecodes() {
    let plain = Decoded::new(write_crate(&layer_with(&[("d", Value::Double(24.0))])).unwrap());
    assert_eq!(
        plain.version(),
        CrateVersion::NEW_FILE_DEFAULT,
        "0.8.0 by default"
    );
    for value in [
        Value::TimeCode(24.0),
        Value::TimeCodeArray(vec![1.0]),
        Value::Dictionary(vec![("t".into(), Value::TimeCode(1.0))]),
    ] {
        let specs = layer_with(&[("t", value)]);
        assert_eq!(
            required_version(&specs),
            CrateVersion::TIMECODES,
            "timecode needs 0.9.0"
        );
        let file = Decoded::new(write_crate(&specs).unwrap());
        assert_eq!(file.version(), CrateVersion::TIMECODES, "header version");
    }
    let file = Decoded::new(write_crate(&layer_with(&[("t", Value::TimeCode(24.0))])).unwrap());
    assert_eq!(
        file.value("/Root.t", "default"),
        Value::TimeCode(24.0),
        "value"
    );
}

#[test]
fn full_reader_assembles_the_layer() {
    use layerstack::doc::{LayerId, Value as DocValue};
    use layerstack::{AssetResolveError, AssetResolver, InMemoryStore, ResolvedAsset};

    struct NoAssets;
    impl AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &mut layerstack::TokenInterner,
            _: &mut layerstack::PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }

    let mut specs = layer_with(&[("points", Value::Vec3fArray(vec![[0.0, 1.0, 2.0]; 2]))]);
    specs[2]
        .fields
        .insert(0, Field::new("typeName", Value::Token("point3f[]".into())));
    let bytes = write_crate(&specs).unwrap();
    let mut store = InMemoryStore::default();
    let result = crate::read_usdc(
        &bytes,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    )
    .unwrap();
    let path = layerstack::path::Path::parse_absolute("/Root", &mut store.tokens).unwrap();
    let id = store.paths.lookup(&path).unwrap();
    let prim = &result.layer.prims[&id];
    let name = store.tokens.intern("points");
    assert_eq!(
        prim.property(name).and_then(|spec| spec.default.as_ref()),
        Some(&DocValue::Array(vec![DocValue::Vec3f([0.0, 1.0, 2.0]); 2])),
        "points"
    );
}

#[test]
fn rejects_invalid_input() {
    let err = |specs: Vec<Spec>| write_crate(&specs).unwrap_err();
    assert_eq!(
        err(vec![Spec::new("/A", SpecForm::Prim)]),
        UsdcWriteError::MissingPseudoRoot,
        "pseudo-root required"
    );
    assert!(
        matches!(
            err(vec![root(), Spec::new("/A/B", SpecForm::Prim)]),
            UsdcWriteError::MissingParent { .. }
        ),
        "parent required"
    );
    assert!(
        matches!(
            err(vec![root(), Spec::new("/A.x", SpecForm::Attribute)]),
            UsdcWriteError::MissingParent { .. }
        ),
        "property owner required"
    );
    assert!(
        matches!(
            err(vec![root(), root()]),
            UsdcWriteError::DuplicateSpec { .. }
        ),
        "duplicate spec"
    );
    assert!(
        matches!(
            err(vec![root(), Spec::new("/A", SpecForm::Attribute)]),
            UsdcWriteError::SpecPathMismatch { .. }
        ),
        "attribute needs a property path"
    );
    assert!(
        matches!(
            err(vec![root(), Spec::new("/A", SpecForm::Variant)]),
            UsdcWriteError::UnsupportedSpecForm { .. }
        ),
        "variants unsupported"
    );
    assert!(
        matches!(
            err(vec![root(), Spec::new("/A{v=x}", SpecForm::Prim)]),
            UsdcWriteError::InvalidPath { .. }
        ),
        "variant paths unsupported"
    );
    let bad_value = |value: Value| err(layer_with(&[("x", value)]));
    assert!(
        matches!(
            bad_value(Value::Token("a\0b".into())),
            UsdcWriteError::NulInText { .. }
        ),
        "NUL in token"
    );
    assert!(
        matches!(
            bad_value(Value::Dictionary(vec![
                ("k".into(), Value::Int(1)),
                ("k".into(), Value::Int(2))
            ])),
            UsdcWriteError::DuplicateDictionaryKey { .. }
        ),
        "duplicate key"
    );
    assert!(
        matches!(
            bad_value(Value::PathListOp(ListOp {
                explicit: Some(vec!["/A".into()]),
                prepended: vec!["/B".into()],
                ..ListOp::default()
            })),
            UsdcWriteError::InvalidListOp { .. }
        ),
        "explicit plus edits"
    );
    assert!(
        matches!(
            bad_value(Value::TokenListOp(ListOp::prepend(vec![
                "a".into(),
                "a".into()
            ]))),
            UsdcWriteError::InvalidListOp { .. }
        ),
        "repeated item"
    );
    assert!(
        matches!(
            bad_value(Value::PathListOp(ListOp::explicit(vec!["relative".into()]))),
            UsdcWriteError::InvalidPath { .. }
        ),
        "target paths are absolute"
    );
    let mut specs = layer_with(&[("x", Value::Int(1))]);
    specs[2].fields.push(Field::new("default", Value::Int(2)));
    assert!(
        matches!(err(specs), UsdcWriteError::DuplicateField { .. }),
        "duplicate field"
    );
}
