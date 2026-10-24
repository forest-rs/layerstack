// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Payloads to dynamic file formats read the layers their composed
//! arguments name, through the format the document registers.
//!
//! [`TestDynamic`] ports OpenUSD's `Test_PcpDynamicFileFormatPlugin_FileFormat`
//! (`pxr/usd/pcp/testenv/TestPcpDynamicFileFormatPlugin.cpp`), metadata
//! fields and attribute defaults as its inputs (the
//! `TEST_PCP_DYNAMIC_FILE_FORMAT_TOKENS_USE_ATTRIBUTE_INPUTS` setting). The
//! tests here run it on small fixtures of their own; [`openusd`] runs it on
//! OpenUSD's own fixtures, with its test's assertions.

use super::*;
use core::f64::consts::PI;
use layerstack::{
    DynamicArgs, DynamicFileFormat, DynamicFormatContext, LayerOffset, MetadataDefinition,
    MetadataTarget, PrimSpec, PropertySpec, PropertyType, Reference, ReferenceTarget,
    SchemaRegistry,
};
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

mod openusd;

const DEPTH: &str = "TestPcp_depth";
const NUM: &str = "TestPcp_num";
const RADIUS: &str = "TestPcp_radius";
const HEIGHT: &str = "TestPcp_height";
const ARG_DICT: &str = "TestPcp_argDict";
const PAYLOAD_ID: &str = "TestPcp_payloadId";
const CONTEXT_ID: &str = "contextId";

/// The test format. Read with `TestPcp_depth` and `TestPcp_num` above 0,
/// it generates `/Root` with a `geom` child referencing the source as read
/// without arguments, and a ring of `TestPcp_num` `Xform_<payloadId>_<depth - 1>_<i>`
/// children, each with a payload to the source again, one level shallower;
/// otherwise it reads the source as USDA.
///
/// Its arguments compose from the fields (or attributes) of the same names;
/// `TestPcp_argDict`, keyed by a payload's `TestPcp_payloadId` argument
/// (or attributes prefixed with it), overrides them for that payload.
#[derive(Debug)]
pub(super) struct TestDynamic {
    /// Whether the inputs are attribute defaults rather than fields.
    attributes: bool,
    /// The ids of the argument compositions so far, which change
    /// processing must hand back.
    contexts: Mutex<std::collections::HashSet<u64>>,
    next_context: AtomicU64,
}

impl TestDynamic {
    pub(super) fn new(attributes: bool) -> Self {
        Self {
            attributes,
            contexts: Mutex::default(),
            next_context: AtomicU64::new(1),
        }
    }

    /// The value of `name`: from `dict` when it holds one of `kind`, else
    /// composed from the field or attribute `name`.
    /// OpenUSD: `_ExtractArg`.
    fn extract(
        &self,
        name: &str,
        context: &mut DynamicFormatContext<'_>,
        dict: &[(Arc<str>, Value)],
        kind: fn(&Value) -> bool,
    ) -> Option<Value> {
        if let Some((_, value)) = dict
            .iter()
            .find(|(key, value)| &**key == name && kind(value))
        {
            return Some(value.clone());
        }
        let value = if self.attributes {
            context.compose_attribute_default(name)
        } else {
            context.compose_value(name).ok().flatten()
        };
        value.filter(kind)
    }

    /// The arguments `payload_id` overrides. OpenUSD: `_ExtractArgDict`.
    fn arg_dict(
        &self,
        context: &mut DynamicFormatContext<'_>,
        payload_id: &str,
    ) -> Vec<(Arc<str>, Value)> {
        if self.attributes {
            let mut dict = Vec::new();
            for name in [DEPTH, NUM, HEIGHT, RADIUS] {
                if let Some(value) =
                    context.compose_attribute_default(&format!("{payload_id}:{name}"))
                {
                    dict.push((Arc::from(name), value));
                }
            }
            return dict;
        }
        let Ok(Some(Value::Dictionary(dict))) = context.compose_value(ARG_DICT) else {
            return Vec::new();
        };
        match dict.into_iter().find(|(key, _)| &**key == payload_id) {
            Some((_, Value::Dictionary(entries))) => entries,
            _ => Vec::new(),
        }
    }

    /// Whether `dependency` came from one of this format's compositions.
    fn composed(&self, dependency: &Value) -> Option<Vec<(Arc<str>, Value)>> {
        let Value::Dictionary(data) = dependency else {
            return None;
        };
        let id = data.iter().find_map(|(key, value)| match value {
            Value::UInt64(id) if &**key == CONTEXT_ID => Some(*id),
            _ => None,
        })?;
        self.contexts
            .lock()
            .unwrap()
            .contains(&id)
            .then(|| data.clone())
    }
}

fn is_int(value: &Value) -> bool {
    matches!(value, Value::Int(_))
}

fn is_double(value: &Value) -> bool {
    matches!(value, Value::Double(_))
}

fn int_arg(args: &FormatArgs, name: &str) -> Option<i32> {
    args.get(name).and_then(|value| value.parse().ok())
}

fn double_arg(args: &FormatArgs, name: &str) -> Option<f64> {
    args.get(name).and_then(|value| value.parse().ok())
}

/// Depths and counts below 1 all read as none.
fn both_below_one(old: Option<&Value>, new: Option<&Value>) -> bool {
    matches!((old, new), (Some(Value::Int(old)), Some(Value::Int(new))) if *old < 1 && *new < 1)
}

/// Resolves nothing: the sources read as USDA author no external arcs.
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

impl FileFormat for TestDynamic {
    fn name(&self) -> &str {
        "Test_PcpDynamicFileFormat"
    }
    fn extensions(&self) -> &[&str] {
        &["testpcpdynamic"]
    }
    fn dynamic(&self) -> Option<&dyn DynamicFileFormat> {
        Some(self)
    }
    /// OpenUSD: `Test_PcpDynamicFileFormatPlugin_FileFormat::Read`.
    fn read(&self, source: &FormatSource<'_>) -> Result<Layer, IoError> {
        let args = source.args;
        let num = int_arg(args, NUM).unwrap_or(1);
        let depth = int_arg(args, DEPTH).unwrap_or(0);
        let payload_id = args.get(PAYLOAD_ID).unwrap_or("");
        // The arcs it authors name the source's own path.
        let path = FormatArgs::strip(source.identifier);
        if depth <= 0 {
            let text = core::str::from_utf8(source.bytes.unwrap_or_default())
                .map_err(|_| source.rejected("the source is not text"))?;
            let read = layerstack_usda::read_usda(
                text,
                source.layer,
                source.tokens,
                source.paths,
                &mut NoAssets,
            );
            if read.emitted.rejected {
                return Err(source.rejected("the source is not USDA"));
            }
            return Ok(read.emitted.layer);
        }
        let token = |name: &str| source.token(name);
        let mut layer = Layer::new(source.layer);
        layer.define_prim(
            source.path("/Root")?,
            PrimSpec::def().with_type_name(token("Xform")),
            source.paths,
        );
        layer.set_default_prim(Some(token("Root")));
        let mut geom = PrimSpec::def();
        geom.references.make_mut().add.push(Reference::deferred(
            source.layer,
            path,
            ReferenceTarget::DefaultPrim,
            LayerOffset::IDENTITY,
        ));
        layer.define_prim(source.path("/Root/geom")?, geom, source.paths);
        if depth > 1 {
            let radius = double_arg(args, RADIUS).unwrap_or(3.0);
            let height = double_arg(args, HEIGHT).unwrap_or(3.0);
            for i in 0..num {
                let angle = PI * 2.0 * f64::from(i) / f64::from(num);
                let mut spec = PrimSpec::def().with_type_name(token("Xform"));
                // Fields and properties in name order, as OpenUSD writes
                // them, so the baselines read alike.
                let inputs = [
                    (DEPTH, "int", Value::Int(depth - 1)),
                    (HEIGHT, "double", Value::Double(height)),
                    (NUM, "int", Value::Int(num)),
                    (RADIUS, "double", Value::Double(radius / 2.0)),
                ];
                for (name, type_name, value) in inputs {
                    spec = if self.attributes {
                        spec.with_property(
                            token(name),
                            PropertySpec::typed_attribute(PropertyType::parse(type_name).unwrap())
                                .custom()
                                .uniform()
                                .with_default(value),
                        )
                    } else {
                        spec.with_field(token(name), value)
                    };
                }
                spec = spec
                    .with_property(
                        token("xformOp:translate"),
                        PropertySpec::typed_attribute(PropertyType::parse("double3").unwrap())
                            .with_default(Value::Vec3d([
                                radius * angle.cos(),
                                radius * angle.sin(),
                                height,
                            ])),
                    )
                    .with_property(
                        token("xformOpOrder"),
                        PropertySpec::typed_attribute(PropertyType::parse("token[]").unwrap())
                            .with_default(Value::Array(vec![Value::Token(token(
                                "xformOp:translate",
                            ))])),
                    );
                let asset = if payload_id.is_empty() {
                    String::from(path)
                } else {
                    FormatArgs::new()
                        .with(PAYLOAD_ID, payload_id)
                        .identifier(path)
                };
                spec.payloads.make_mut().add.push(Reference::deferred(
                    source.layer,
                    asset,
                    ReferenceTarget::DefaultPrim,
                    LayerOffset::IDENTITY,
                ));
                let name = format!("/Root/Xform_{payload_id}_{}_{i}", depth - 1);
                layer.define_prim(source.path(&name)?, spec, source.paths);
            }
        }
        Ok(layer)
    }
}

impl DynamicFileFormat for TestDynamic {
    fn name(&self) -> &str {
        "Test_PcpDynamicFileFormat"
    }
    fn extensions(&self) -> &[&str] {
        &["testpcpdynamic"]
    }
    /// OpenUSD: `ComposeFieldsForFileFormatArguments`.
    fn compose_args(
        &self,
        asset_path: &str,
        context: &mut DynamicFormatContext<'_>,
    ) -> DynamicArgs {
        let id = self.next_context.fetch_add(1, Ordering::Relaxed);
        self.contexts.lock().unwrap().insert(id);
        let mut data: Vec<(Arc<str>, Value)> = vec![(Arc::from(CONTEXT_ID), Value::UInt64(id))];
        let payload_id = FormatArgs::split(asset_path)
            .ok()
            .and_then(|(_, args)| args.get(PAYLOAD_ID).map(String::from));
        let mut dict = Vec::new();
        if let Some(payload_id) = payload_id {
            dict = self.arg_dict(context, &payload_id);
            data.push((Arc::from(PAYLOAD_ID), Value::String(payload_id.into())));
        }
        let mut composed = DynamicArgs {
            args: FormatArgs::new(),
            dependency: Value::Dictionary(data),
        };
        let args = &mut composed.args;
        for name in [DEPTH, NUM] {
            if let Some(Value::Int(value)) = self.extract(name, context, &dict, is_int) {
                if value < 1 {
                    return composed;
                }
                args.insert(name, value.to_string());
            }
        }
        for name in [RADIUS, HEIGHT] {
            if let Some(Value::Double(value)) = self.extract(name, context, &dict, is_double) {
                args.insert(name, value.to_string());
            }
        }
        composed
    }
    /// OpenUSD: `CanFieldChangeAffectFileFormatArguments`.
    fn can_field_change_affect_args(
        &self,
        field: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        let Some(data) = self.composed(dependency) else {
            return false;
        };
        if field == ARG_DICT {
            let Some(Value::String(payload_id)) = data
                .iter()
                .find(|(key, _)| &**key == PAYLOAD_ID)
                .map(|(_, value)| value)
            else {
                return false;
            };
            let payload_dict = |value: Option<&Value>| match value {
                Some(Value::Dictionary(dict)) => dict
                    .iter()
                    .find(|(key, _)| **key == **payload_id)
                    .map(|(_, value)| value.clone()),
                _ => None,
            };
            return payload_dict(old) != payload_dict(new);
        }
        !((field == DEPTH || field == NUM) && both_below_one(old, new))
    }
    /// OpenUSD: `CanAttributeDefaultValueChangeAffectFileFormatArguments`.
    fn can_attribute_default_change_affect_args(
        &self,
        attribute: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        self.composed(dependency).is_some()
            && !((attribute == DEPTH || attribute == NUM) && both_below_one(old, new))
    }
}

/// The plugin metadata the test format reads, as its `plugInfo.json`
/// declares it (`TestPcpDynamicFileFormatPlugin_plugInfo.json`).
pub(super) fn registry(store: &mut InMemoryStore) -> Arc<SchemaRegistry> {
    let mut builder = SchemaRegistry::builder();
    for (name, type_name, default) in [
        (DEPTH, "int", Some(Value::Int(1))),
        (NUM, "int", Some(Value::Int(1))),
        (RADIUS, "double", Some(Value::Double(3.0))),
        (HEIGHT, "double", Some(Value::Double(3.0))),
        (ARG_DICT, "dictionary", None),
    ] {
        builder
            .register_metadata(
                MetadataDefinition {
                    default,
                    ..MetadataDefinition::new(
                        store.tokens.intern(name),
                        layerstack::MetadataType::parse(type_name).unwrap(),
                        [MetadataTarget::Prim],
                    )
                    .with_documentation("Used to set number of copies.")
                },
                &store.tokens,
            )
            .unwrap();
    }
    Arc::new(builder.build(&store.tokens))
}

/// Opens `root` with the test format registered.
pub(super) fn open<B: Storage>(storage: B, root: &str, attributes: bool) -> StageDocument<B> {
    let mut store = InMemoryStore::default();
    let schemas = registry(&mut store);
    let formats = FileFormats::default()
        .with(Arc::new(TestDynamic::new(attributes)))
        .unwrap();
    StageDocument::open_in_with(
        storage,
        store,
        root,
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
        LoadOptions {
            formats,
            ..Default::default()
        },
    )
    .unwrap()
}

/// The identifiers of the layers read with arguments, each relative to
/// `base`, sorted.
pub(super) fn generated<B: Storage>(doc: &StageDocument<B>, base: &str) -> Vec<String> {
    let mut identifiers: Vec<String> = doc
        .store()
        .layer_ids()
        .iter()
        .filter_map(|&id| doc.identifier(id))
        .filter(|identifier| identifier.contains(FORMAT_ARGS_DELIMITER))
        .map(|identifier| String::from(identifier.strip_prefix(base).unwrap_or(identifier)))
        .collect();
    identifiers.sort_unstable();
    identifiers
}

/// The identifier of `asset` with `args`, as `generated` lists it.
pub(super) fn with_args(asset: &str, args: &[(&str, &str)]) -> String {
    args.iter()
        .fold(FormatArgs::new(), |all, (key, value)| {
            all.with(*key, *value)
        })
        .identifier(asset)
}

/// The prims the payloads beneath `root` generate down to `depth` with
/// `num` children each, `root` first; there must be `expected` of them.
/// OpenUSD: `_GeneratePrimIndexPaths`.
pub(super) fn dynamic_paths(
    root: &str,
    depth: i32,
    num: i32,
    expected: usize,
    payload_id: &str,
) -> Vec<String> {
    fn walk(path: &str, depth: i32, num: i32, payload_id: &str, out: &mut Vec<String>) {
        out.push(path.into());
        if depth - 1 > 0 {
            for i in 0..num {
                let child = format!("{path}/Xform_{payload_id}_{}_{i}", depth - 1);
                walk(&child, depth - 1, num, payload_id, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, depth, num, payload_id, &mut out);
    assert_eq!(out.len(), expected, "{out:?}");
    out
}

/// Each of `paths` is composed without errors and its index composes
/// dynamic arguments from exactly `names` (fields, or attributes when
/// `attributes`); its `geom` child composes none.
/// OpenUSD: `_ComputeAndVerifyDynamicPayloads`.
pub(super) fn verify<B: Storage>(
    doc: &StageDocument<B>,
    paths: &[String],
    names: &[&str],
    attributes: bool,
) {
    let stage = doc.stage().stage();
    let store = doc.store();
    let lookup = |path: &str| {
        let parsed = layerstack::Path::parse_absolute(path, &store.tokens).ok()?;
        store.paths.lookup(&parsed)
    };
    let mut expected: Vec<&str> = names.to_vec();
    expected.sort_unstable();
    for path in paths {
        let prim = lookup(path).unwrap_or_else(|| panic!("{path} is not composed"));
        let handle = doc.view().prim(prim);
        assert!(handle.is_some(), "{path} is not composed");
        let errors: Vec<_> = stage
            .composition_errors()
            .iter()
            .filter(|error| error.prim() == Some(prim))
            .collect();
        assert!(errors.is_empty(), "{path}: {errors:?}");
        let dependency = handle
            .and_then(|prim| prim.dynamic_dependency())
            .unwrap_or_else(|| panic!("{path} composes no dynamic arguments"));
        let relevant: Vec<&str> = if attributes {
            dependency.relevant_attributes().collect()
        } else {
            dependency.relevant_fields().collect()
        };
        assert_eq!(relevant, expected, "{path}");
        let geom = lookup(&format!("{path}/geom")).unwrap();
        let geom = doc.view().prim(geom);
        assert!(geom.is_some(), "{path}/geom is not composed");
        assert!(
            geom.and_then(|prim| prim.dynamic_dependency()).is_none(),
            "{path}/geom"
        );
    }
}

/// The fields (or attributes) every payload of the fixtures reads.
pub(super) const INPUTS: [&str; 4] = [DEPTH, HEIGHT, NUM, RADIUS];

const PARAMS: &str = r#"#usda 1.0
def "Params" (
    TestPcp_depth = 4
    TestPcp_num = 3
    TestPcp_radius = 50.0
    TestPcp_argDict = {
        dictionary "A" = {
            int TestPcp_num = 2
        }
    }
)
{
    int TestPcp_depth = 4
    int TestPcp_num = 3
    double TestPcp_radius = 50.0
    int A:TestPcp_num = 2
}
def "Near" (TestPcp_depth = 2 TestPcp_num = 3) {
    int TestPcp_depth = 2
    int TestPcp_num = 3
}
def "Far" (TestPcp_depth = 3 TestPcp_radius = 50) {
    int TestPcp_depth = 3
    double TestPcp_radius = 50
}
"#;

const PAYLOADS: &str = r#"#usda 1.0
def "ConePayload" (payload = @cone.testpcpdynamic@) {}
def "Siblings" (
    payload = [@params.usda@</Near>, @cone.testpcpdynamic@, @params.usda@</Far>]
) {}
def "Tagged" (payload = @cone.testpcpdynamic:SDF_FORMAT_ARGS:TestPcp_payloadId=A@) {}
"#;

const ROOT: &str = r#"#usda 1.0
def "Lab" (references = [@params.usda@</Params>, @payloads.usda@</ConePayload>]) {
    # Declared without defaults, for the edits to author them as defaults.
    custom string documentation
    custom double TestPcp_height
    over "geom" {
        custom int TestPcp_depth
    }
    over "Xform__3_0" {
        custom int TestPcp_depth
    }
}
def "Siblings" (references = @payloads.usda@</Siblings>) {}
def "Variant" (
    TestPcp_depth = 3
    variantSets = "x"
    variants = { string x = "a" }
)
{
    int TestPcp_depth = 3
    custom string documentation
    custom double TestPcp_radius
    variantSet "x" = {
        "a" (
            TestPcp_num = 2
            references = [@params.usda@</Params>, @payloads.usda@</ConePayload>]
        )
        {
            int TestPcp_num = 2
        }
    }
}
def "SubrootVariant" (
    TestPcp_depth = 1
    TestPcp_num = 1
    references = </Variant/Xform__2_1>
)
{
    int TestPcp_depth = 1
    int TestPcp_num = 1
}
def "Tagged" (references = [@params.usda@</Params>, @payloads.usda@</Tagged>]) {}
"#;

const CONE: &str = "#usda 1.0\n(\n    defaultPrim = \"cone\"\n)\n\ndef Cone \"cone\"\n{\n}\n";

fn lab() -> Memory {
    Memory::with(&[
        ("root.usda", ROOT),
        ("params.usda", PARAMS),
        ("payloads.usda", PAYLOADS),
        ("cone.testpcpdynamic", CONE),
    ])
}

const CONE_FILE: &str = "cone.testpcpdynamic";

#[test]
fn dynamic_payloads_read_the_layers_their_arguments_name() {
    for attributes in [false, true] {
        let doc = open(lab(), "root.usda", attributes);
        let mut names = INPUTS.to_vec();
        verify(
            &doc,
            &dynamic_paths("/Lab", 4, 3, 40, ""),
            &names,
            attributes,
        );
        let layers = generated(&doc, "");
        for args in [
            [(DEPTH, "4"), (NUM, "3"), (RADIUS, "50")].as_slice(),
            &[(DEPTH, "3"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "25")],
            &[(DEPTH, "2"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "12.5")],
            &[(DEPTH, "1"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "6.25")],
        ] {
            let identifier = with_args(CONE_FILE, args);
            assert!(
                layers.contains(&identifier),
                "{identifier} not in {layers:?}"
            );
        }
        // The payload's identifier argument picks its own overrides.
        let id_inputs: Vec<String> = INPUTS.iter().map(|name| format!("A:{name}")).collect();
        if attributes {
            names.extend(id_inputs.iter().map(String::as_str));
        } else {
            names.push(ARG_DICT);
        }
        let tagged = dynamic_paths("/Tagged", 4, 2, 15, "A");
        verify(&doc, &tagged[1..], &names, attributes);
        // `/Tagged` takes its count from the overrides, so it reads no
        // count of its own.
        names.retain(|name| *name != NUM);
        verify(&doc, &tagged[..1], &names, attributes);
        let identifier = with_args(
            CONE_FILE,
            &[(DEPTH, "4"), (NUM, "2"), (PAYLOAD_ID, "A"), (RADIUS, "50")],
        );
        assert!(
            layers.contains(&identifier),
            "{identifier} not in {layers:?}"
        );
    }
}

#[test]
fn weaker_sibling_payloads_supply_no_arguments() {
    for attributes in [false, true] {
        let doc = open(lab(), "root.usda", attributes);
        verify(
            &doc,
            &dynamic_paths("/Siblings", 2, 3, 4, ""),
            &INPUTS,
            attributes,
        );
        let layers = generated(&doc, "");
        // `</Near>` is stronger than the payload; `</Far>`, with its
        // depth and radius, is weaker.
        for args in [
            [(DEPTH, "2"), (NUM, "3")].as_slice(),
            &[(DEPTH, "1"), (HEIGHT, "3"), (NUM, "3"), (RADIUS, "1.5")],
        ] {
            let identifier = with_args(CONE_FILE, args);
            assert!(
                layers.contains(&identifier),
                "{identifier} not in {layers:?}"
            );
        }
    }
}

#[test]
fn ancestral_payloads_in_variants_read_the_opinions_of_the_arcs_above_them() {
    for attributes in [false, true] {
        let doc = open(lab(), "root.usda", attributes);
        verify(
            &doc,
            &dynamic_paths("/Variant", 3, 2, 7, ""),
            &INPUTS,
            attributes,
        );
        // Indexed for `/SubrootVariant`'s reference, the payload in
        // `/Variant`'s selected branch reads the depth `/Variant` authors
        // outside it, across the recursive index of the reference
        // (OpenUSD's `test_AncestralPayloadsAndVariants`); the inputs on
        // `/SubrootVariant` do not map to it.
        let identifier = with_args(CONE_FILE, &[(DEPTH, "3"), (NUM, "2"), (RADIUS, "50")]);
        // Every layer read is one a payload's arguments name; the payload
        // in the branch is read with the same arguments whether `/Variant`
        // or the reference's recursive index composes them.
        let layers: Vec<String> = generated(&doc, "")
            .iter()
            .map(|identifier| FormatArgs::split(identifier).unwrap().1.identifier(""))
            .collect();
        assert_eq!(
            layers,
            [
                ":SDF_FORMAT_ARGS:TestPcp_depth=1&TestPcp_height=3&TestPcp_num=1&TestPcp_radius=25",
                ":SDF_FORMAT_ARGS:TestPcp_depth=1&TestPcp_height=3&TestPcp_num=2&TestPcp_payloadId=A&TestPcp_radius=6.25",
                ":SDF_FORMAT_ARGS:TestPcp_depth=1&TestPcp_height=3&TestPcp_num=2&TestPcp_radius=12.5",
                ":SDF_FORMAT_ARGS:TestPcp_depth=1&TestPcp_height=3&TestPcp_num=3&TestPcp_radius=1.5",
                ":SDF_FORMAT_ARGS:TestPcp_depth=1&TestPcp_height=3&TestPcp_num=3&TestPcp_radius=6.25",
                ":SDF_FORMAT_ARGS:TestPcp_depth=2&TestPcp_height=3&TestPcp_num=2&TestPcp_payloadId=A&TestPcp_radius=12.5",
                ":SDF_FORMAT_ARGS:TestPcp_depth=2&TestPcp_height=3&TestPcp_num=2&TestPcp_radius=25",
                ":SDF_FORMAT_ARGS:TestPcp_depth=2&TestPcp_height=3&TestPcp_num=3&TestPcp_radius=12.5",
                ":SDF_FORMAT_ARGS:TestPcp_depth=2&TestPcp_num=3",
                ":SDF_FORMAT_ARGS:TestPcp_depth=3&TestPcp_height=3&TestPcp_num=2&TestPcp_payloadId=A&TestPcp_radius=25",
                ":SDF_FORMAT_ARGS:TestPcp_depth=3&TestPcp_height=3&TestPcp_num=3&TestPcp_radius=25",
                ":SDF_FORMAT_ARGS:TestPcp_depth=3&TestPcp_num=2&TestPcp_radius=50",
                ":SDF_FORMAT_ARGS:TestPcp_depth=4&TestPcp_num=2&TestPcp_payloadId=A&TestPcp_radius=50",
                ":SDF_FORMAT_ARGS:TestPcp_depth=4&TestPcp_num=3&TestPcp_radius=50",
            ]
        );
        let store = doc.store();
        let stage = doc.stage().stage();
        let prim = store
            .paths
            .lookup(&layerstack::Path::parse_absolute("/SubrootVariant", &store.tokens).unwrap())
            .unwrap();
        assert!(
            stage
                .composition_errors()
                .iter()
                .all(|error| error.prim() != Some(prim)),
            "{:?}",
            stage.composition_errors()
        );
        let layer = doc
            .store()
            .layer_ids()
            .into_iter()
            .find(|&id| doc.identifier(id) == Some(identifier.as_str()))
            .unwrap();
        let generated_prim = store
            .paths
            .lookup(&layerstack::Path::parse_absolute("/Root/Xform__2_1", &store.tokens).unwrap())
            .unwrap();
        assert!(
            doc.view()
                .prim(prim)
                .unwrap()
                .prim_stack()
                .iter()
                .any(|(source, path)| *source == layer && path.prim_path() == generated_prim),
            "/SubrootVariant does not read {identifier}"
        );
    }
}

#[test]
fn readers_leave_dynamic_payloads_to_composition() {
    let doc = open(lab(), "root.usda", false);
    let payloads = doc
        .store()
        .layer_ids()
        .into_iter()
        .find(|&id| doc.identifier(id) == Some("payloads.usda"))
        .unwrap();
    let layer = &doc.store().layer(payloads).unwrap();
    let mut arcs: Vec<(String, bool)> = layer
        .prims()
        .values()
        .flat_map(|spec| spec.payloads.items())
        .map(|arc| {
            let deferred = matches!(arc.source(), layerstack::ReferenceSource::Asset { .. });
            (String::from(arc.asset_path().unwrap_or_default()), deferred)
        })
        .collect();
    arcs.sort();
    assert_eq!(
        arcs,
        [
            (String::from("cone.testpcpdynamic"), true),
            (String::from("cone.testpcpdynamic"), true),
            (
                String::from("cone.testpcpdynamic:SDF_FORMAT_ARGS:TestPcp_payloadId=A"),
                true
            ),
            (String::from("params.usda"), false),
            (String::from("params.usda"), false),
        ]
    );
}

/// Authors `value` as the field `name` of the spec at `prim` of the layer
/// `layer` names, or, when `attributes`, as the default of its attribute
/// `name`, then loads the layers the stage asks for.
pub(super) fn author<B: Storage>(
    doc: &mut StageDocument<B>,
    layer: &str,
    prim: &str,
    name: &str,
    value: Value,
    attributes: bool,
) -> layerstack::Applied {
    use layerstack::{Address, PropertyPath, SpecPath, Transaction};
    let layer = doc
        .store()
        .layer_ids()
        .into_iter()
        .find(|&id| {
            doc.identifier(id)
                .is_some_and(|found| found.ends_with(layer))
        })
        .unwrap();
    let store = doc.store_mut();
    let path = store.path(prim);
    let token = store.tokens.intern(name);
    let mut txn = Transaction::new();
    if attributes {
        let at = Address::spec(
            layer,
            SpecPath::from_property_path(PropertyPath::new(path, token)),
        );
        let exists = store
            .layer(layer)
            .unwrap()
            .prims()
            .get(&path)
            .is_some_and(|spec| spec.property(token).is_some());
        if exists {
            txn.set_default(at, value);
        } else {
            let type_name = match value {
                Value::Int(_) => "int",
                Value::Double(_) => "double",
                _ => "string",
            };
            txn.create_property(
                at,
                PropertySpec::typed_attribute(PropertyType::parse(type_name).unwrap())
                    .custom()
                    .with_default(value),
            );
        }
    } else {
        txn.set_metadata(
            Address::spec(layer, SpecPath::from_prim_path(path)),
            token,
            layerstack::FieldValue::Value(value),
        );
    }
    doc.apply(&txn).unwrap().0
}

/// The paths of `prims`, sorted.
pub(super) fn displayed<B: Storage>(
    doc: &StageDocument<B>,
    prims: &[layerstack::PathId],
) -> Vec<String> {
    let store = doc.store();
    let mut out: Vec<String> = prims
        .iter()
        .map(|path| store.paths.display(*path, &store.tokens))
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn argument_input_edits_resync_the_prims_whose_arguments_they_change() {
    for attributes in [false, true] {
        let mut doc = open(lab(), "root.usda", attributes);
        let resynced =
            |doc: &mut StageDocument<Memory>, layer: &str, prim: &str, name: &str, value: Value| {
                let applied = author(doc, layer, prim, name, value, attributes);
                displayed(doc, &applied.changes.resynced)
            };
        // An input no argument reads changes nothing.
        let none: [&str; 0] = [];
        assert_eq!(
            resynced(
                &mut doc,
                "root.usda",
                "/Lab",
                "documentation",
                Value::String("text".into())
            ),
            none
        );
        // A height on `/Lab` is stronger than every other: all of its
        // payloads read another layer.
        assert_eq!(
            resynced(&mut doc, "root.usda", "/Lab", HEIGHT, Value::Double(2.0)),
            ["/Lab"]
        );
        verify(
            &doc,
            &dynamic_paths("/Lab", 4, 3, 40, ""),
            &INPUTS,
            attributes,
        );
        assert!(
            generated(&doc, "").contains(&with_args(
                CONE_FILE,
                &[(DEPTH, "4"), (HEIGHT, "2"), (NUM, "3"), (RADIUS, "50")]
            )),
            "{:?}",
            generated(&doc, "")
        );
        // The parameter prim's count reaches `/Lab` only: `/Tagged` takes
        // its count from its overrides, `/Variant` from its branch.
        assert_eq!(
            resynced(&mut doc, "params.usda", "/Params", NUM, Value::Int(2)),
            ["/Lab"]
        );
        verify(
            &doc,
            &dynamic_paths("/Lab", 4, 2, 15, ""),
            &INPUTS,
            attributes,
        );
        // With no depth, the format reads the depth alone.
        assert_eq!(
            resynced(&mut doc, "params.usda", "/Params", DEPTH, Value::Int(0)),
            ["/Lab", "/Tagged"]
        );
        verify(&doc, &["/Lab".into()], &[DEPTH], attributes);
        // Below one, every depth reads as none: the format says the change
        // cannot affect the arguments.
        assert_eq!(
            resynced(&mut doc, "params.usda", "/Params", DEPTH, Value::Int(-1)),
            none
        );
        assert_eq!(
            resynced(&mut doc, "params.usda", "/Params", DEPTH, Value::Int(4)),
            ["/Lab", "/Tagged"]
        );
        verify(
            &doc,
            &dynamic_paths("/Lab", 4, 2, 15, ""),
            &INPUTS,
            attributes,
        );
        // A generated prim's own over changes its subtree only.
        assert_eq!(
            resynced(
                &mut doc,
                "root.usda",
                "/Lab/Xform__3_0",
                DEPTH,
                Value::Int(4)
            ),
            ["/Lab/Xform__3_0"]
        );
        verify(
            &doc,
            &dynamic_paths("/Lab/Xform__3_0", 4, 2, 15, ""),
            &INPUTS,
            attributes,
        );
        verify(
            &doc,
            &dynamic_paths("/Lab/Xform__3_1", 3, 2, 7, ""),
            &INPUTS,
            attributes,
        );
        // `geom` composes no dynamic arguments: a depth there is inert.
        assert_eq!(
            resynced(&mut doc, "root.usda", "/Lab/geom", DEPTH, Value::Int(4)),
            none
        );
    }
}

#[test]
fn argument_input_edits_report_the_assets_they_request() {
    let mut doc = open(lab(), "root.usda", true);
    let text = Value::String("text".into());
    let applied = author(&mut doc, "root.usda", "/Lab", "documentation", text, true);
    assert!(applied.asset_requests.is_empty());
    // The new height gives `/Lab`'s payloads new arguments, so new
    // identifiers to load, which `StageDocument::apply` then loaded.
    let applied = author(
        &mut doc,
        "root.usda",
        "/Lab",
        HEIGHT,
        Value::Double(2.0),
        true,
    );
    assert!(!applied.asset_requests.is_empty());
    for request in &applied.asset_requests {
        let prim = doc.store().paths.display(request.prim, &doc.store().tokens);
        assert!(prim.starts_with("/Lab"), "{prim}");
        assert_eq!(request.arc, layerstack::ArcKind::Payloads);
        assert!(request.asset_path.contains(FORMAT_ARGS_DELIMITER));
    }
    assert!(doc.stage().stage().asset_requests().is_empty());
}

#[test]
fn argument_inputs_read_beyond_a_prims_own_specs_recompose_it() {
    for attributes in [false, true] {
        let mut doc = open(lab(), "root.usda", attributes);
        let prim = |doc: &StageDocument<Memory>, path: &str| {
            let store = doc.store();
            store
                .paths
                .lookup(&layerstack::Path::parse_absolute(path, &store.tokens).unwrap())
                .unwrap()
        };
        let subroot = prim(&doc, "/SubrootVariant");
        // `/SubrootVariant`'s reference reads `/Variant`'s spec for the
        // arguments of the payload in its branch, though no opinion of
        // `/Variant` is one of `/SubrootVariant`'s.
        let applied = author(
            &mut doc,
            "root.usda",
            "/Variant",
            "documentation",
            Value::String("text".into()),
            attributes,
        );
        assert!(applied.changes.resynced.is_empty());
        assert!(
            !applied.recomposed.contains(&subroot),
            "an irrelevant input"
        );
        let applied = author(
            &mut doc,
            "root.usda",
            "/Variant",
            RADIUS,
            Value::Double(60.0),
            attributes,
        );
        assert_eq!(
            displayed(&doc, &applied.changes.resynced),
            ["/SubrootVariant", "/Variant"]
        );
        let identifier = with_args(CONE_FILE, &[(DEPTH, "3"), (NUM, "2"), (RADIUS, "60")]);
        assert!(generated(&doc, "").contains(&identifier));
        let stage = doc.stage().stage();
        let errors: Vec<_> = stage
            .composition_errors()
            .iter()
            .filter(|error| error.prim() == Some(subroot))
            .collect();
        assert!(errors.is_empty(), "{errors:?}");
        let dependency = doc
            .view()
            .prim(subroot)
            .and_then(|prim| prim.dynamic_dependency())
            .unwrap();
        assert!(dependency.relevant_fields().len() + dependency.relevant_attributes().len() > 0);
    }
}
