// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Registry-declared metadata types survive both USDA import paths.

use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, InMemoryStore, LayerId, MetadataDefinition,
    MetadataTarget, MetadataType, PathInterner, ResolvedAsset, SchemaRegistry, TokenInterner,
    Value,
};
use layerstack_usda::{emit, parser, read_usda, read_usda_with_registry};

struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &TokenInterner,
        _: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

fn registry(store: &mut InMemoryStore) -> SchemaRegistry {
    let mut builder = SchemaRegistry::builder();
    for (name, ty, targets) in [
        ("hostLayer", "double", vec![MetadataTarget::Layer]),
        ("hostAttribute", "int", vec![MetadataTarget::Attribute]),
        (
            "hostRelationship",
            "token",
            vec![MetadataTarget::Relationship],
        ),
        ("hostToken", "token", vec![MetadataTarget::Prim]),
        ("hostFloat", "float", vec![MetadataTarget::Prim]),
        ("hostI64", "int64", vec![MetadataTarget::Prim]),
        ("hostDefault", "float", vec![MetadataTarget::Prim]),
        ("hostArray", "float[]", vec![MetadataTarget::Prim]),
        ("hostEmpty", "token[]", vec![MetadataTarget::Prim]),
        ("hostVector", "float3", vec![MetadataTarget::Prim]),
        ("hostVectors", "float3[]", vec![MetadataTarget::Prim]),
        ("hostMatrix", "matrix4d", vec![MetadataTarget::Prim]),
        ("hostMatrices", "matrix4d[]", vec![MetadataTarget::Prim]),
        ("hostDictionary", "dictionary", vec![MetadataTarget::Prim]),
        ("hostNames", "stringlistop", vec![MetadataTarget::Prim]),
        ("hostTokens", "tokenlistop", vec![MetadataTarget::Prim]),
        ("hostInts", "intlistop", vec![MetadataTarget::Prim]),
        ("hostUnsigned", "uintlistop", vec![MetadataTarget::Prim]),
        ("hostBig", "uint64listop", vec![MetadataTarget::Prim]),
        ("hostSigned", "int64listop", vec![MetadataTarget::Prim]),
    ] {
        let mut definition = MetadataDefinition::new(
            store.tokens().intern(name),
            MetadataType::parse(ty).unwrap(),
            targets,
        );
        definition.default = (name == "hostDefault").then_some(Value::Float(99.));
        builder
            .register_metadata(definition, store.tokens())
            .unwrap();
    }
    builder.build(store.tokens())
}

fn field<'a>(
    store: &InMemoryStore,
    fields: &'a [layerstack::FieldEntry],
    name: &str,
) -> &'a FieldValue {
    layerstack::doc::get_field(fields, &store.tokens().lookup(name).unwrap()).unwrap()
}

const VALID: &str = r#"#usda 1.0
(hostLayer = 1)
def "P" (
 hostToken = "blue"
 hostFloat = 1
 hostArray = [1, 2]
 hostEmpty = []
 hostVector = (1, 2, 3)
 hostMatrix = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))
 hostMatrices = [((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]
 hostDictionary = { token label = "green" }
 prepend hostNames = ["first"]
 append hostNames = ["last"]
 hostTokens = ["red", "green"]
 hostInts = [-2147483648, 2147483647]
 hostUnsigned = [0, 4294967295]
 hostBig = [0, 18446744073709551615]
 hostSigned = [-9223372036854775808, 9223372036854775807]
) {
 float x (hostAttribute = 7)
 rel target (hostRelationship = "edge")
}
"#;

#[test]
fn registered_literals_use_declared_types_without_authoring_defaults() {
    let mut store = InMemoryStore::default();
    let registry = registry(&mut store);
    let legacy = read_usda(
        VALID,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
    );
    let path = store.path("/P");
    assert_eq!(
        field(
            &store,
            &legacy.emitted.layer.prims()[&path].fields,
            "hostFloat"
        ),
        &FieldValue::Value(Value::Int64(1)),
        "the standalone default API retains literal inference"
    );
    let compact = read_usda_with_registry(
        VALID,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &registry,
    );
    assert!(
        compact.parse_diagnostics.is_empty(),
        "{:?}",
        compact.parse_diagnostics
    );
    assert!(
        compact.lower_diagnostics.is_empty(),
        "{:?}",
        compact.lower_diagnostics
    );
    assert!(
        compact.emitted.diagnostics.is_empty(),
        "{:?}",
        compact.emitted.diagnostics
    );
    assert!(!compact.emitted.rejected);
    let parsed = parser::parse(VALID);
    assert!(parsed.diagnostics.is_empty());
    let ast = emit::emit_with_registry(
        &parsed.layer,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &registry,
    );
    assert!(ast.diagnostics.is_empty(), "{:?}", ast.diagnostics);
    assert_eq!(ast.layer, compact.emitted.layer);
    let layer = &ast.layer;
    let prim = &layer.prims()[&path];
    assert_eq!(
        field(&store, layer.metadata_fields(), "hostLayer"),
        &FieldValue::Value(Value::Double(1.))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostToken"),
        &FieldValue::Value(Value::Token(store.tokens().lookup("blue").unwrap()))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostFloat"),
        &FieldValue::Value(Value::Float(1.))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostVector"),
        &FieldValue::Value(Value::Vec3f([1., 2., 3.]))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostArray"),
        &FieldValue::Value(Value::from(vec![1_f32, 2.]))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostEmpty"),
        &FieldValue::Value(Value::Array(vec![]))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostNames"),
        &FieldValue::StringListOp(Box::new(layerstack::ListOp {
            prepend: vec!["first".into()],
            append: vec!["last".into()],
            ..Default::default()
        }))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostInts"),
        &FieldValue::IntListOp(Box::new(layerstack::ListOp::explicit(vec![
            i32::MIN,
            i32::MAX
        ])))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostUnsigned"),
        &FieldValue::UIntListOp(Box::new(layerstack::ListOp::explicit(vec![0, u32::MAX])))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostBig"),
        &FieldValue::UInt64ListOp(Box::new(layerstack::ListOp::explicit(vec![0, u64::MAX])))
    );
    assert_eq!(
        field(&store, &prim.fields, "hostSigned"),
        &FieldValue::Int64ListOp(Box::new(layerstack::ListOp::explicit(vec![
            i64::MIN,
            i64::MAX
        ])))
    );
    let attr = prim
        .properties
        .iter()
        .find(|entry| store.tokens().resolve(entry.name) == "x")
        .unwrap();
    assert_eq!(
        field(&store, &attr.spec.metadata, "hostAttribute"),
        &FieldValue::Value(Value::Int(7))
    );
    let rel = prim
        .properties
        .iter()
        .find(|entry| store.tokens().resolve(entry.name) == "target")
        .unwrap();
    assert_eq!(
        field(&store, &rel.spec.metadata, "hostRelationship"),
        &FieldValue::Value(Value::Token(store.tokens().lookup("edge").unwrap()))
    );
    assert!(
        layerstack::doc::get_field(&prim.fields, &store.tokens().lookup("hostDefault").unwrap())
            .is_none(),
        "registered defaults are not authored"
    );
}

#[test]
fn invalid_registered_literals_and_targets_reject_without_foreign_kind_fields() {
    for assignment in [
        "hostToken = 3",
        "hostFloat = []",
        "hostArray = 3",
        "hostArray = [1, \"bad\"]",
        "hostVector = (1, 2)",
        "hostVector = (1, 2, 3, 4)",
        "hostVector = (1, (2, 3), 4)",
        "hostVectors = [(1, (2, 3), 4)]",
        "hostMatrix = (1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)",
        "hostMatrices = [(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1)]",
        "hostMatrix = ((1, 0), (0, 1))",
        "hostMatrix = (((1, 2), 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))",
        "hostMatrices = [(((1, 2), 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))]",
        "hostDictionary = 1",
        "hostFloat = None",
        "hostInts = [2147483648]",
        "hostUnsigned = [-1]",
        "hostBig = [-1]",
        "hostSigned = [18446744073709551615]",
        "hostSigned = [-9223372036854775809]",
        "hostI64 = -9223372036854775809",
        "hostInts = [\"bad\"]",
        "hostTokens = 3",
        "hostNames = { string bad = \"wrong\" }",
        "prepend hostFloat = [1]",
    ] {
        let mut store = InMemoryStore::default();
        let registry = registry(&mut store);
        let source = format!("#usda 1.0\ndef \"P\" ({assignment}) {{}}\n");
        let result = read_usda_with_registry(
            &source,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
            &registry,
        );
        assert!(
            result.emitted.rejected,
            "{assignment}: {:?}",
            result.emitted.diagnostics
        );
        assert!(!result.emitted.diagnostics.is_empty(), "{assignment}");
        let path = store.path("/P");
        assert!(
            result.emitted.layer.prims()[&path].fields.is_empty(),
            "{assignment}"
        );
        assert!(
            result
                .emitted
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.span.end > diagnostic.span.start),
            "{assignment}: diagnostics retain the authored span"
        );
    }
}

#[test]
fn a_field_on_a_kind_of_spec_it_is_not_registered_for_is_kept_untyped() {
    // OpenUSD keeps the text of a field the spec's definition does not
    // register (`SdfUnregisteredValue`); the literal keeps its own type.
    let mut store = InMemoryStore::default();
    let registry = registry(&mut store);
    let result = read_usda_with_registry(
        "#usda 1.0\ndef \"P\" (hostLayer = 3) {}",
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &registry,
    );
    assert!(!result.emitted.rejected, "{:?}", result.emitted.diagnostics);
    let path = store.path("/P");
    assert_eq!(
        field(
            &store,
            &result.emitted.layer.prims()[&path].fields,
            "hostLayer"
        ),
        &FieldValue::Value(Value::Int64(3))
    );
}

#[test]
fn registered_list_none_is_an_explicit_empty_op() {
    let mut store = InMemoryStore::default();
    let registry = registry(&mut store);
    let result = read_usda_with_registry(
        "#usda 1.0\ndef \"P\" (hostInts = None) {}",
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &registry,
    );
    assert!(!result.emitted.rejected, "{:?}", result.emitted.diagnostics);
    let path = store.path("/P");
    assert_eq!(
        field(
            &store,
            &result.emitted.layer.prims()[&path].fields,
            "hostInts"
        ),
        &FieldValue::IntListOp(Box::new(layerstack::ListOp::explicit(vec![])))
    );
}

#[test]
fn integer_list_literals_use_checked_cpp_numeric_coercion() {
    // OpenUSD 26.8 Sdf parser accepts finite fractional list items using
    // GfNumericCast's range check followed by truncation toward zero.
    let mut store = InMemoryStore::default();
    let registry = registry(&mut store);
    let source = "#usda 1.0\ndef \"P\" (hostInts = [1.5, -1.5]; hostUnsigned = [-0.5, 1.9]; hostSigned = [-0, 2.5]) {}";
    let result = read_usda_with_registry(
        source,
        LayerId(1),
        store.tokens(),
        store.paths(),
        &mut NoAssets,
        &registry,
    );
    assert!(!result.emitted.rejected, "{:?}", result.emitted.diagnostics);
    let path = store.path("/P");
    let fields = &result.emitted.layer.prims()[&path].fields;
    assert_eq!(
        field(&store, fields, "hostInts"),
        &FieldValue::IntListOp(Box::new(layerstack::ListOp::explicit(vec![1, -1])))
    );
    assert_eq!(
        field(&store, fields, "hostUnsigned"),
        &FieldValue::UIntListOp(Box::new(layerstack::ListOp::explicit(vec![0, 1])))
    );
    assert_eq!(
        field(&store, fields, "hostSigned"),
        &FieldValue::Int64ListOp(Box::new(layerstack::ListOp::explicit(vec![0, 2])))
    );
}

#[test]
fn empty_arrays_cannot_validate_unsupported_registered_type_names() {
    // OpenUSD registers no field of these types ("… is not a registered
    // value type"), so no declaration of one can be made.
    for ty in [
        "color5f[]",
        "colornonsensef[]",
        "point2f[]",
        "dictionary[]",
        "float[][]",
        "hostOpaque[]",
        "hostOpaque",
    ] {
        assert_eq!(MetadataType::parse(ty), None, "{ty}");
    }
}
