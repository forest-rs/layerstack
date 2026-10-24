// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A layer saves as USDA in the order OpenUSD 26.08's text writer gives it,
//! whatever order it was authored or read in, byte for byte.
//!
//! `scripts/usda_field_order_oracle.py` authors layers step by step in
//! deliberately scrambled orders through OpenUSD's `Sdf` API and records
//! each layer's `ExportToString` (see the script for every step). Here
//! each step is one `Transaction` on an empty layer; the saved USDA must be
//! OpenUSD's text. OpenUSD sorts metadata fields, properties, variants and
//! dictionary entries, and keeps child prims, variant sets, list items and
//! targets in their order (`pxr/usd/sdf/fileIO_Common.h`,
//! `usdaFileFormat.cpp`).
//!
//! `scrambled.usda`, a file written out of order, and `scrambled.usdc`,
//! OpenUSD's crate file of it (which keeps the file's order), must both
//! save as OpenUSD exports them, and so must `unregistered.usda`, whose
//! unregistered metadata sorts in byte order on the layer and in
//! dictionary order on a prim.
//!
//! Numbers are spelled as OpenUSD spells them: the `numbers` case, and
//! the hashes of OpenUSD's text of generated arrays (every half but the
//! NaNs, and floats, doubles and time codes from random bits and short
//! decimals).
//! OpenUSD's text of each case reads back as the same layer.
//!
//! Spec: AOUSD Core §6.6.2 (dictionaries are unordered), §8.2 (element
//! ordering), §11.3.2 (property order is the sorted names, then
//! `propertyOrder`), §16.2 (USDA), §16.2.5 (numeric literals).

#![allow(missing_docs, reason = "integration tests")]

use std::sync::Arc;

use layerstack::doc::{FieldValue, Layer, LayerId, Reference, Specifier, Value};
use layerstack::listop::ListOp;
use layerstack::path::TargetPath;
use layerstack::property::{PropertySpec, PropertyType};
use layerstack::spec_path::SpecPath;
use layerstack::{Address, InMemoryStore, LayerOffset, ListPosition, Transaction};
use layerstack_conformance::save_corpus::Imported;
use layerstack_conformance::workspace_root;
use layerstack_usda::writer::Value as WriterValue;
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../fixtures/usda_field_order/oracle.json");
const SCENE: LayerId = LayerId(1);
const ASSET: LayerId = LayerId(2);

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

/// A number of a step: JSON's, or text read as Rust reads a `f64`
/// (`"1e-7"`, `"-0"`, `"inf"`, `"nan"`), as the oracle reads it with
/// Python's `float`.
fn number(json: &Json) -> f64 {
    match json.as_str() {
        Some(text) => text.parse().expect("a number"),
        None => json.as_f64().expect("a number"),
    }
}

/// A `float`, rounded from the number's `double` as C++ casts it.
#[allow(clippy::cast_possible_truncation, reason = "`float` values")]
fn float(json: &Json) -> f32 {
    number(json) as f32
}

/// A `half`, rounded through `float` as `GfHalf` is from a `double`.
fn half(json: &Json) -> u16 {
    layerstack::half::from_f32(float(json))
}

fn items(json: &Json) -> &[Json] {
    json.as_array().expect("an array")
}

/// A step value as the layer holds it.
fn value(store: &InMemoryStore, json: &Json) -> Value {
    let item = &json["value"];
    let floats = |v: &Json| items(v).iter().map(float).collect::<Vec<_>>();
    let doubles = |v: &Json| items(v).iter().map(number).collect::<Vec<_>>();
    let halves = |v: &Json| items(v).iter().map(half).collect::<Vec<_>>();
    let array = |f: &dyn Fn(&Json) -> Value| Value::array(items(item).iter().map(f).collect());
    // `(r, i, j, k)` in the step, stored `[i, j, k, r]`.
    fn quat<T: Copy>([r, i, j, k]: [T; 4]) -> [T; 4] {
        [i, j, k, r]
    }
    match json["type"].as_str().unwrap() {
        "bool" => Value::Bool(item.as_bool().unwrap()),
        "int" => Value::Int(i32::try_from(item.as_i64().unwrap()).unwrap()),
        "half" => Value::Half(half(item)),
        "float" => Value::Float(float(item)),
        "double" => Value::Double(number(item)),
        "timecode" => Value::TimeCode(number(item)),
        "token" => Value::Token(store.tokens.intern(item.as_str().unwrap())),
        "string" => Value::String(Arc::from(item.as_str().unwrap())),
        "half[]" => array(&|v| Value::Half(half(v))),
        "float[]" => array(&|v| Value::Float(float(v))),
        "double[]" => array(&|v| Value::Double(number(v))),
        "timecode[]" => array(&|v| Value::TimeCode(number(v))),
        "half3" => Value::Vec3h(halves(item).try_into().unwrap()),
        "float3" => Value::Vec3f(floats(item).try_into().unwrap()),
        "double4" => Value::Vec4d(doubles(item).try_into().unwrap()),
        "quath" => Value::Quath(quat(halves(item).try_into().unwrap())),
        "quatf" => Value::Quatf(quat(floats(item).try_into().unwrap())),
        "quatd" => Value::Quatd(quat(doubles(item).try_into().unwrap())),
        "matrix2d" => Value::Matrix2d(Box::new(
            items(item)
                .iter()
                .flat_map(doubles)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        )),
        "point3f[]" => array(&|v| Value::Vec3f(floats(v).try_into().unwrap())),
        "texCoord2f[]" => array(&|v| Value::Vec2f(floats(v).try_into().unwrap())),
        "dictionary" => Value::Dictionary(
            item.as_object()
                .unwrap()
                .iter()
                .map(|(key, entry)| (Arc::from(key.as_str()), value(store, entry)))
                .collect(),
        ),
        other => panic!("no value type {other}"),
    }
}

/// The step's call as a transaction on the scene layer.
fn transaction(store: &InMemoryStore, step: &Json) -> Transaction {
    let text = |key: &str| step[key].as_str().unwrap();
    let token = |key: &str| store.tokens.intern(text(key));
    let path = text("path");
    let at = || {
        let spec = SpecPath::parse(path, &store.tokens, &store.paths).expect("a spec path");
        Address::spec(SCENE, spec)
    };
    let target = |text: &str| TargetPath::parse(text, &store.tokens, &store.paths).unwrap();
    let targets = || {
        let items = step["targets"].as_array().unwrap();
        ListOp::explicit(items.iter().map(|t| target(t.as_str().unwrap())).collect())
    };
    let names = || {
        let items = step["names"].as_array().unwrap();
        items
            .iter()
            .map(|n| store.tokens.intern(n.as_str().unwrap()))
            .collect()
    };
    let mut edit = Transaction::new();
    match text("op") {
        "prim" => {
            let specifier = match step["specifier"].as_str().unwrap_or("over") {
                "def" => Specifier::Def,
                "over" => Specifier::Over,
                "class" => Specifier::Class,
                other => panic!("no specifier {other}"),
            };
            let type_name = step["type"].as_str().map(|t| store.tokens.intern(t));
            edit.create_prim(at(), specifier, type_name)
        }
        "attribute" => {
            let declared = PropertyType::parse(text("type")).expect("a value type");
            let mut spec = PropertySpec::typed_attribute(declared);
            if step["custom"].as_bool().unwrap_or(false) {
                spec = spec.custom();
            }
            if step["uniform"].as_bool().unwrap_or(false) {
                spec = spec.uniform();
            }
            if !step["default"].is_null() {
                spec = spec.with_default(value(store, &step["default"]));
            }
            edit.create_property(at(), spec)
        }
        "relationship" => {
            let mut spec = PropertySpec::relationship().with_targets(targets());
            if step["custom"].as_bool().unwrap_or(false) {
                spec = spec.custom();
            }
            edit.create_property(at(), spec)
        }
        "sample" => edit.set_time_sample(at(), number(&step["time"]), value(store, &step["value"])),
        "connect" => edit.set_targets(at(), targets()),
        "metadata" if path == "/" => edit.set_layer_metadata(
            SCENE,
            token("key"),
            FieldValue::Value(value(store, &step["value"])),
        ),
        "metadata" => edit.set_metadata(
            at(),
            token("key"),
            FieldValue::Value(value(store, &step["value"])),
        ),
        "active" => edit.set_active(at(), step["value"].as_bool().unwrap()),
        "instanceable" => edit.set_instanceable(at(), step["value"].as_bool().unwrap()),
        "api_schema" => edit.add_applied_schema(at(), token("name")),
        op @ ("reference" | "payload") => {
            let prim = store.path(text("target"));
            let mut arc = match step["asset"].as_str() {
                Some(asset) => Reference::with_asset(ASSET, prim, asset),
                None => Reference::new(SCENE, prim),
            };
            let or = |key: &str, default: f64| {
                let json = &step[key];
                if json.is_null() {
                    default
                } else {
                    number(json)
                }
            };
            arc.layer_offset = LayerOffset {
                offset: or("offset", 0.0),
                scale: or("scale", 1.0),
            };
            if op == "reference" {
                edit.add_reference(at(), arc, ListPosition::BackOfPrepend)
            } else {
                edit.add_payload(at(), arc, ListPosition::BackOfPrepend)
            }
        }
        "inherit" => edit.add_inherit(
            at(),
            store.path(text("target")),
            ListPosition::BackOfPrepend,
        ),
        "specialize" => edit.add_specialize(
            at(),
            store.path(text("target")),
            ListPosition::BackOfPrepend,
        ),
        "variant_set" => edit.add_variant_set(at(), token("name"), ListPosition::BackOfPrepend),
        "select" => edit.set_variant_selection(at(), token("set"), Some(token("variant"))),
        "property_order" => edit.set_property_order(at(), names()),
        "prim_order" => edit.set_prim_order(at(), names()),
        other => panic!("no step {other}"),
    };
    edit
}

/// Each case, authored step by step, saves as OpenUSD's text.
#[test]
fn scrambled_authoring_saves_as_openusd_writes_it() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    assert!(cases.len() >= 6, "the oracle's cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut store = InMemoryStore::default();
        store.insert_layer(Layer::new(SCENE));
        store.insert_layer(Layer::new(ASSET));
        for (i, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let edit = transaction(&store, step);
            if let Err(error) = edit.apply(&mut store) {
                panic!("{name}: step {i} ({step}) is rejected: {error:?}");
            }
        }
        let text = layerstack_usda::save::save_usda(
            store.layer(SCENE).unwrap(),
            &store.tokens,
            &store.paths,
        )
        .expect("saves");
        let expected = case["text"].as_str().unwrap();
        assert_eq!(text, expected, "{name}");
        // OpenUSD's text reads back as the same layer: every number it
        // spells is read.
        assert_eq!(
            Imported::usda(expected).save_usda().unwrap(),
            expected,
            "{name} read back"
        );
    }
}

/// `splitmix64`, as the oracle draws its generated numbers.
struct SplitMix64(u64);

impl Iterator for SplitMix64 {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        Some(z ^ (z >> 31))
    }
}

/// A decimal of up to six digits, `m * 10^k` or `m / 10^k`, rounded as the
/// oracle's `decimal` rounds it.
#[allow(clippy::cast_precision_loss, reason = "`m` is below 10^6")]
fn decimal(bits: u64) -> f64 {
    let m = (bits % 1_999_999) as f64 - 999_999.0;
    let mut power = 1.0;
    for _ in 0..(bits >> 32) % 23 {
        power *= 10.0;
    }
    if bits >> 63 == 0 {
        m * power
    } else {
        m / power
    }
}

/// The oracle's generated array `name`.
#[allow(clippy::cast_possible_truncation, reason = "`float` bits and values")]
fn generated(name: &str, count: usize) -> WriterValue {
    let seed = match name {
        // But the NaNs, whose sign the C library OpenUSD's stream uses
        // spells or not.
        "every half" => {
            let nan = |bits: &u16| bits & 0x7C00 == 0x7C00 && bits & 0x3FF != 0;
            return WriterValue::HalfArray((0..=u16::MAX).filter(|bits| !nan(bits)).collect());
        }
        "float bits" => 1,
        "double bits" => 2,
        "decimal floats" => 3,
        "decimal doubles" => 4,
        "decimal time codes" => 5,
        other => panic!("no generated array {other}"),
    };
    let draws = SplitMix64(seed).take(count);
    match name {
        "float bits" => WriterValue::FloatArray(
            draws
                .map(|bits| f32::from_bits((bits >> 32) as u32))
                .collect(),
        ),
        "double bits" => WriterValue::DoubleArray(draws.map(f64::from_bits).collect()),
        "decimal floats" => {
            WriterValue::FloatArray(draws.map(|bits| decimal(bits) as f32).collect())
        }
        "decimal doubles" => WriterValue::DoubleArray(draws.map(decimal).collect()),
        _ => WriterValue::TimeCodeArray(draws.map(decimal).collect()),
    }
}

fn fnv1a64(text: &str) -> String {
    let hash = text.bytes().fold(0xCBF2_9CE4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01B3)
    });
    format!("{hash:016x}")
}

/// Every half but the NaNs, and floats, doubles and time codes drawn from random bits
/// and from short decimals, are spelled as OpenUSD spells them (compared
/// by hash of each array's text), and read back as themselves.
#[test]
fn generated_numbers_are_spelled_as_openusd_writes_them() {
    let oracle = oracle();
    let arrays = oracle["generated"].as_array().unwrap();
    assert!(arrays.len() >= 6, "the oracle's generated arrays");
    for array in arrays {
        let name = array["name"].as_str().unwrap();
        let count = usize::try_from(array["count"].as_u64().unwrap()).unwrap();
        let text = generated(name, count).to_usda_text();
        assert_eq!(
            fnv1a64(&text),
            array["fnv1a64"].as_str().unwrap(),
            "{name}: {}",
            &text[..200]
        );
        let type_name = array["type"].as_str().unwrap();
        let layer = format!("#usda 1.0\n\ndef \"P\"\n{{\n    {type_name} a = {text}\n}}\n\n");
        assert_eq!(
            Imported::usda(&layer).save_usda().unwrap(),
            layer,
            "{name} read back"
        );
    }
}

/// A file written out of order, read as USDA or as OpenUSD's crate file of
/// it, saves as OpenUSD's text of it; so does a file of unregistered
/// metadata.
#[test]
fn scrambled_files_save_as_openusd_writes_them() {
    let oracle = oracle();
    let dir = workspace_root().join("layerstack_conformance/fixtures/usda_field_order");
    let usda = std::fs::read_to_string(dir.join("scrambled.usda")).unwrap();
    let usdc = std::fs::read(dir.join("scrambled.usdc")).unwrap();
    let unregistered = std::fs::read_to_string(dir.join("unregistered.usda")).unwrap();
    for (format, imported) in [
        ("usda", Imported::usda(&usda)),
        ("usdc", Imported::usdc(&usdc)),
        ("unregistered", Imported::usda(&unregistered)),
    ] {
        let text = imported.save_usda().expect("saves");
        let expected = oracle["round_trip"][format].as_str().unwrap();
        assert_eq!(text, expected, "{format}");
        // Saved again, the text is unchanged.
        assert_eq!(Imported::usda(&text).save_usda().unwrap(), text, "{format}");
    }
}
