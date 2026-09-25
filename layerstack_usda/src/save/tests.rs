// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec;

use layerstack::doc::{LayerId, Reference, SublayerEntry};
use layerstack::path::PropertyPath;
use layerstack::property::PropertySpec;
use layerstack::spline::{CurveType, Extrapolation, SplineDataType};
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};

use super::*;

/// Resolves every asset to a fresh, empty layer, so arcs survive import.
struct AnyAsset(u64);

impl AssetResolver for AnyAsset {
    fn resolve(
        &mut self,
        asset_path: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        self.0 += 1;
        Ok(ResolvedAsset {
            layer_id: LayerId(self.0),
            resolved_path: Arc::from(format!("/resolved/{asset_path}").as_str()),
            layer: None,
        })
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

struct Imported {
    layer: Layer,
    tokens: TokenInterner,
    paths: PathInterner,
}

impl Imported {
    fn new(source: &str) -> Self {
        let parsed = crate::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let result = crate::emit::emit(
            &parsed.layer,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut AnyAsset(100),
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        Self {
            layer: result.layer,
            tokens,
            paths,
        }
    }

    fn property(&mut self, path: &str) -> &mut PropertySpec {
        let path = PropertyPath::parse(path, &mut self.tokens, &mut self.paths).unwrap();
        self.layer.property_mut(path).unwrap()
    }

    fn save(&self) -> Result<String, SaveError> {
        save_usda(&self.layer, &self.tokens, &self.paths)
    }
}

const SOURCE: &str = r#"#usda 1.0
(
    "Authored for the save tests."
    defaultPrim = "Asset"
    doc = """Layer docs."""
    metersPerUnit = 0.01
    customLayerData = {
        string source = "./textures/a.png"
    }
)

reorder rootPrims = ["Asset", "Other"]

def Xform "Asset" (
    prepend apiSchemas = ["MaterialBindingAPI", "CollectionAPI:lod"]
    kind = "component"
    instanceable = false
    customData = {
        dictionary profilesInfo = {
            string[] "vnd.example" = ["usd.geom.mesh"]
        }
    }
)
{
    reorder nameChildren = ["Low", "High"]
    reorder properties = ["b", "a"]
    rel material:binding = </Asset/Looks/M>
    custom uniform token exedra:mode = "fast" (
        allowedTokens = ["fast", "slow"]
    )
    float a = 1 (
        limits = {
            dictionary soft = {
                float min = 0
                float max = 5
            }
        }
    )
    float b.connect = </Asset.a>
    asset tex = @./textures/a.png@
    string note = "/Asset/Looks/M"
    prepend rel exedra:targets = [</Asset/High>, </Asset.a>]
    int[] ids = [1, 2, 3]
    point3f[] points = []
    double c = None

    def Scope "High"
    {
    }

    def Scope "Low"
    {
    }
}

over "Other"
{
}
"#;

#[test]
fn saves_an_edited_layer_as_authored() {
    let mut imported = Imported::new(SOURCE);
    // One source edit through the layer API.
    imported.property("/Asset.a").default = Some(LayerValue::Float(2.5));

    let text = imported.save().unwrap();
    let expected = r#"#usda 1.0
(
    defaultPrim = "Asset"
    "Authored for the save tests."
    doc = "Layer docs."
    metersPerUnit = 0.01
    customLayerData = {
        string "source" = "./textures/a.png"
    }
)

reorder rootPrims = ["Asset", "Other"]

def Xform "Asset" (
    prepend apiSchemas = ["MaterialBindingAPI", "CollectionAPI:lod"]
    kind = "component"
    customData = {
        dictionary "profilesInfo" = {
            string[] "vnd.example" = ["usd.geom.mesh"]
        }
    }
    instanceable = false
)
{
    reorder properties = ["b", "a"]
    reorder nameChildren = ["Low", "High"]
    rel material:binding = </Asset/Looks/M>
    custom uniform token exedra:mode = "fast" (
        allowedTokens = ["fast", "slow"]
    )
    float a = 2.5 (
        limits = {
            dictionary "soft" = {
                float "min" = 0
                float "max" = 5
            }
        }
    )
    float b.connect = </Asset.a>
    asset tex = @./textures/a.png@
    string note = "/Asset/Looks/M"
    prepend rel exedra:targets = [</Asset/High>, </Asset.a>]
    int[] ids = [1, 2, 3]
    point3f[] points = []
    double c = None

    def Scope "High"
    {
    }

    def Scope "Low"
    {
    }
}

over "Other"
{
}
"#;
    assert_eq!(text, expected, "saved text");

    // Saving the reimported file changes nothing.
    assert_eq!(Imported::new(&text).save().unwrap(), text, "stable");
}

/// Each unsupported feature is reported with its source path, and nothing
/// is written.
#[test]
fn rejects_unsupported_features_with_their_source_paths() {
    let cases: &[(&str, &str, Unsupported)] = &[
        (
            "#usda 1.0\ndef \"A\" (\n    variants = {\n        string v = \"x\"\n    }\n)\n{\n}\n",
            "/A",
            Unsupported::VariantSelections,
        ),
        (
            "#usda 1.0\ndef \"A\" (\n    variantSets = \"v\"\n)\n{\n    variantSet \"v\" = {\n        \"x\" {\n        }\n    }\n}\n",
            "/A",
            Unsupported::VariantSets,
        ),
        (
            "#usda 1.0\ndef \"A\"\n{\n    pathExpression p = \"/A//\"\n}\n",
            "/A.p",
            Unsupported::Value("pathExpression"),
        ),
        (
            "#usda 1.0\ndef \"A\" (\n    customData = {\n        string[] e = []\n    }\n)\n{\n}\n",
            "/A#customData/e",
            Unsupported::Value("untyped empty array"),
        ),
        (
            "#usda 1.0\ndef \"A\"\n{\n    int[] a = edit [append 4]\n}\n",
            "/A.a",
            Unsupported::ArrayEdit,
        ),
    ];
    for (source, path, feature) in cases {
        assert_eq!(
            Imported::new(source).save(),
            Err(SaveError::Unsupported {
                path: path.to_string(),
                feature: *feature,
            }),
            "{source}"
        );
    }
}

/// Features the USDA importer does not produce (a spline, a variant spec,
/// a `varying` relationship, a mixed list op), built through the layer API.
#[test]
fn rejects_unsupported_slots_built_through_the_api() {
    let source = "#usda 1.0\ndef \"A\"\n{\n    float a = 1\n    rel r = </A>\n}\n";

    let mut imported = Imported::new(source);
    imported.property("/A.a").spline = Some(layerstack::SplineData {
        data_type: SplineDataType::Float,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Held,
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        knots: vec![],
    });
    assert_eq!(
        imported.save(),
        Err(SaveError::Unsupported {
            path: "/A.a".into(),
            feature: Unsupported::Spline
        }),
        "spline"
    );

    let mut imported = Imported::new(source);
    imported.property("/A.r").variability = Variability::Varying;
    assert!(
        matches!(
            imported.save(),
            Err(SaveError::Unsupported {
                feature: Unsupported::VaryingRelationship,
                ..
            })
        ),
        "varying relationship"
    );

    let mut imported = Imported::new(source);
    let targets = imported.property("/A.r").targets.as_mut().unwrap();
    targets.prepend = targets.explicit.clone().unwrap();
    assert_eq!(
        imported.save(),
        Err(SaveError::Unsupported {
            path: "/A.r".into(),
            feature: Unsupported::MixedListOp
        }),
        "explicit list and edits"
    );
}

/// Arcs are written by their authored asset paths; an arc into another
/// layer built from its layer id alone has none to write.
#[test]
fn arcs_need_authored_asset_paths() {
    let source = "#usda 1.0\ndef \"A\"\n{\n}\n";
    let mut imported = Imported::new(source);
    imported
        .layer
        .sublayers
        .push(SublayerEntry::new(LayerId(9)));
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/".into(),
            problem: Invalid::ArcWithoutAsset
        }),
        "a sublayer by layer id"
    );
    imported.layer.sublayers = vec![SublayerEntry::with_asset(
        LayerId(9),
        "./base.usda",
        layerstack::LayerOffset {
            offset: 1.5,
            scale: 1.0,
        },
    )];
    let prim = Path::parse_absolute("/A", &mut imported.tokens).unwrap();
    let prim = imported.paths.intern(prim);
    let spec = imported.layer.prims.get_mut(&prim).unwrap();
    spec.add_reference(Reference::with_asset(LayerId(9), prim, "./x.usda"));
    // An internal reference: into the layer itself, without an asset path.
    spec.add_reference(Reference::to_default_prim(LayerId(1)));
    let text = imported.save().unwrap();
    assert!(
        text.contains("    subLayers = [\n        @./base.usda@ (offset = 1.5)\n    ]\n"),
        "{text}"
    );
    assert!(
        text.contains("    append references = [@./x.usda@</A>, <>]\n"),
        "{text}"
    );

    let spec = imported.layer.prims.get_mut(&prim).unwrap();
    spec.add_reference(Reference::new(LayerId(9), prim));
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/A".into(),
            problem: Invalid::ArcWithoutAsset
        }),
        "a reference by layer id"
    );
}

/// An arc list that repeats an item within one operation is rejected before
/// any output, for every arc family: OpenUSD refuses to open such a layer.
/// The same item in a `delete` and a `prepend` is kept, as OpenUSD keeps
/// it.
///
/// Spec: AOUSD Core §6.6.3 (list operations).
#[test]
fn rejects_repeated_arcs_within_one_operation() {
    let source = "#usda 1.0\ndef \"A\"\n{\n}\n\nclass \"C\"\n{\n}\n";
    let mut imported = Imported::new(source);
    let a = Path::parse_absolute("/A", &mut imported.tokens).unwrap();
    let a = imported.paths.intern(a);
    let c = Path::parse_absolute("/C", &mut imported.tokens).unwrap();
    let c = imported.paths.intern(c);
    let arc = Reference::with_asset(LayerId(9), c, "./r.usda");
    /// Repeats an item in one of the prim's arc lists.
    type Repeat = fn(&mut PrimSpec, PathId, &Reference);
    let cases: [(&str, Repeat); 4] = [
        ("inherits", |spec, c, _| {
            spec.inherits.explicit = Some(vec![c, c]);
        }),
        ("specializes", |spec, c, _| {
            spec.specializes.prepend = vec![c, c];
        }),
        ("references", |spec, _, arc| {
            spec.references.append = vec![arc.clone(), arc.clone()];
        }),
        ("payload", |spec, _, arc| {
            spec.payloads.delete = vec![arc.clone(), arc.clone()];
        }),
    ];
    for (key, edit) in cases {
        let mut layer = imported.layer.clone();
        edit(layer.prims.get_mut(&a).unwrap(), c, &arc);
        assert_eq!(
            save_usda(&layer, &imported.tokens, &imported.paths),
            Err(SaveError::Document(WriteError::InvalidListOp {
                path: format!("/A#{key}")
            })),
            "{key}"
        );
    }

    let spec = imported.layer.prims.get_mut(&a).unwrap();
    spec.inherits.delete = vec![c];
    spec.inherits.prepend = vec![c];
    let text = imported.save().unwrap();
    assert!(
        text.contains("    delete inherits = </C>\n    prepend inherits = </C>\n"),
        "{text}"
    );
}

/// Every arc form is written as authored: list-op forms and explicit empty
/// lists, external and internal references and payloads, prim paths or
/// the `defaultPrim`, layer offsets and scales, and inherit and specialize
/// paths (relative ones as the absolute paths they name).
///
/// Spec: AOUSD Core §10.3.1 (sublayers), §10.3.2.1–§10.3.2.4 (references,
/// payloads, inherits, specializes), §16.2.17.5 (arc syntax).
#[test]
fn saves_composition_arcs_as_authored() {
    let source = r#"#usda 1.0
(
    defaultPrim = "A"
    subLayers = [
        @./strong.usda@ (offset = -2.5; scale = 0.5),
        @./weak.usdc@
    ]
)

def "A" (
    kind = "group"
    delete inherits = </C>
    append inherits = <../D>
    payload = [@./p.usda@</P> (scale = 2), </A/B>]
    prepend references = [@./r.usda@ (offset = 3), <>]
    append references = @./r.usda@</R>
    specializes = None
)
{
    def "B" (
        references = None
        payload = @./q.usda@
        inherits = </C>
    )
    {
    }
}

class "C"
{
}

class "D"
{
}
"#;
    let text = Imported::new(source).save().unwrap();
    let expected = r#"#usda 1.0
(
    defaultPrim = "A"
    subLayers = [
        @./strong.usda@ (offset = -2.5; scale = 0.5),
        @./weak.usdc@
    ]
)

def "A" (
    kind = "group"
    delete inherits = </C>
    append inherits = </D>
    payload = [@./p.usda@</P> (scale = 2), </A/B>]
    prepend references = [@./r.usda@ (offset = 3), <>]
    append references = @./r.usda@</R>
    specializes = None
)
{
    def "B" (
        inherits = </C>
        payload = @./q.usda@
        references = None
    )
    {
    }
}

class "C"
{
}

class "D"
{
}
"#;
    assert_eq!(text, expected, "saved text");
    assert_eq!(Imported::new(&text).save().unwrap(), text, "stable");
}

/// Resolves no asset, as when an imported layer's dependencies are absent.
struct NoAsset;

impl AssetResolver for NoAsset {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// A reference, payload or sublayer whose asset did not resolve on import
/// is kept as an unresolved arc (`Reference::unresolved`,
/// `SublayerEntry::unresolved`), and saved as authored like any other; it
/// is not taken as absent.
#[test]
fn saves_unresolved_arcs_as_authored() {
    for source in [
        "#usda 1.0\ndef \"A\"\n{\n    def \"B\" (\n        references = @./absent.usda@</Model> (offset = 4)\n    )\n    {\n    }\n}\n",
        "#usda 1.0\ndef \"A\" (\n    prepend payload = @./absent.usdc@\n)\n{\n}\n",
        "#usda 1.0\n(\n    subLayers = [\n        @./absent.usda@ (scale = 3)\n    ]\n)\n\ndef \"A\"\n{\n}\n",
    ] {
        let parsed = crate::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let result = crate::emit::emit(
            &parsed.layer,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut NoAsset,
        );
        let arcs = result
            .layer
            .prims
            .values()
            .flat_map(|p| {
                p.references
                    .explicit
                    .iter()
                    .flatten()
                    .chain(&p.payloads.prepend)
            })
            .filter(|arc| arc.is_unresolved())
            .count()
            + result
                .layer
                .sublayers
                .iter()
                .filter(|s| s.is_unresolved())
                .count();
        assert_eq!(arcs, 1, "the importer keeps the unresolved arc");
        let saved = save_usda(&result.layer, &tokens, &paths).unwrap();
        assert_eq!(saved, Imported::new(source).save().unwrap(), "{source}");
        assert!(saved.contains("@./absent."), "{saved}");
    }
}

/// A layer the formats cannot hold as it stands.
#[test]
fn rejects_invalid_layers() {
    let source = "#usda 1.0\ndef \"A\"\n{\n    float a = 1\n}\n";

    let mut imported = Imported::new(source);
    let b = Path::parse_absolute("/A/B", &mut imported.tokens).unwrap();
    let b = imported.paths.intern(b);
    imported.layer.insert_prim(b, PrimSpec::def());
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/A/B".into(),
            problem: Invalid::UnlistedPrim
        }),
        "a prim no parent lists"
    );

    let mut imported = Imported::new(source);
    let a = Path::parse_absolute("/A", &mut imported.tokens).unwrap();
    let a = imported.paths.intern(a);
    let name = imported.tokens.intern("Missing");
    let spec = imported.layer.prims.get_mut(&a).unwrap();
    spec.authored_children.push(name);
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/A/Missing".into(),
            problem: Invalid::MissingChildSpec
        }),
        "a listed child without a spec"
    );

    let mut imported = Imported::new(source);
    imported.layer.prims.get_mut(&a).unwrap().specifier = None;
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/A".into(),
            problem: Invalid::MissingSpecifier
        }),
        "no specifier"
    );

    let mut imported = Imported::new("#usda 1.0\ndef \"A\"\n{\n    rel r\n}\n");
    imported.property("/A.r").time_samples = Some(vec![(0.0, LayerValue::Bool(true))]);
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/A.r".into(),
            problem: Invalid::RelationshipValue
        }),
        "a relationship with time samples"
    );

    let mut imported = Imported::new(source);
    imported.property("/A.a").type_name = None;
    assert_eq!(
        imported.save(),
        Err(SaveError::Invalid {
            path: "/A.a".into(),
            problem: Invalid::MissingTypeName
        }),
        "untyped attribute"
    );

    // The writers' validation still applies to what the layer authors.
    let mut imported = Imported::new(source);
    let key = imported.tokens.intern("hidden");
    imported
        .layer
        .prims
        .get_mut(&a)
        .unwrap()
        .set_field(key, LayerValue::Blocked);
    assert_eq!(
        imported.save(),
        Err(SaveError::Document(WriteError::MisplacedBlock {
            path: "/A#hidden".into()
        })),
        "a blocked metadata field"
    );
}

/// Time samples are written as authored: next to a default or alone, with
/// blocked samples, typed empty arrays and an empty sample map; a sample
/// time the formats cannot order is rejected before any output.
///
/// Spec: AOUSD Core §16.2.16.3 (time samples), §12.3.6 (blocked samples).
#[test]
fn saves_time_samples() {
    let source = r#"#usda 1.0
def "A"
{
    double x = 1
    double x.timeSamples = {
        0: 1,
        1.5: None,
        2: -0.25,
    }
    int[] ids.timeSamples = {
        0: [1, 2],
        1: [],
    }
    uniform timecode cue.timeSamples = {
        3: 4.5,
    }
    float quiet.timeSamples = {
    }
    custom token mode.timeSamples = {
        0: "idle",
    }
}
"#;
    let mut imported = Imported::new(source);
    let text = imported.save().unwrap();
    let expected = r#"#usda 1.0

def "A"
{
    double x = 1
    double x.timeSamples = {
        0: 1,
        1.5: None,
        2: -0.25,
    }
    int[] ids.timeSamples = {
        0: [1, 2],
        1: [],
    }
    uniform timecode cue.timeSamples = {
        3: 4.5,
    }
    float quiet.timeSamples = {
    }
    custom token mode
    token mode.timeSamples = {
        0: "idle",
    }
}
"#;
    assert_eq!(text, expected, "saved text");
    assert_eq!(Imported::new(&text).save().unwrap(), text, "stable");

    imported.property("/A.x").time_samples = Some(vec![
        (1.0, LayerValue::Double(1.0)),
        (0.0, LayerValue::Double(2.0)),
    ]);
    assert_eq!(
        imported.save(),
        Err(SaveError::Document(WriteError::InvalidTimeSamples {
            path: "/A.x".into()
        })),
        "unordered sample times"
    );
    imported.property("/A.x").time_samples = Some(vec![(0.0, LayerValue::Float(1.0))]);
    assert_eq!(
        imported.save(),
        Err(SaveError::Document(WriteError::TypeMismatch {
            path: "/A.x".into(),
            type_name: "double".into()
        })),
        "a sample of another type"
    );
}

/// List-op metadata of every element type and the value types beyond the
/// common ones are written as authored; a path list op, which USDA
/// metadata cannot spell, is still rejected.
///
/// Spec: AOUSD Core §6.2–§6.3 (value types), §6.6.3 (list operations),
/// §16.2.14 (list-op syntax).
#[test]
fn saves_list_op_metadata_and_every_value_type() {
    let source = r#"#usda 1.0

def "A" (
    prepend clipSets = ["motion"]
    delete inactiveIds = [3, 7]
)
{
    uchar c = 200
    uint64 u = 18446744073709551615
    half h = 0.1
    half3 t = (1, 0.5, -0)
    quatf q = (1, 0, 0.5, 0)
    quatd d = (0.5, 0.5, 0.5, 0.5)
    matrix2d m = ( (1, 0.5), (0, 1) )
    matrix3d n = ( (1, 0, 0), (0, 1, 0), (0, 0, 1) )
    uchar[] cs = [0, 255]
    uint64[] us = [1, 2]
    half[] hs = [0, 0.5, 1]
    color3h[] ts = [(0.25, 0.5, 1)]
    quath[] qs = [(1, 0, 0, 0)]
    quatf[] fs = []
    matrix4d[] ms = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1) )]
}
"#;
    let text = Imported::new(source).save().unwrap();
    // A half is written as the shortest decimal of its exact value, which
    // reads back as the same half.
    let expected = source.replace("half h = 0.1", "half h = 0.099975586");
    assert_eq!(text, expected, "saved text");
    assert_eq!(Imported::new(&text).save().unwrap(), text, "stable");

    let mut imported = Imported::new(source);
    let a = Path::parse_absolute("/A", &mut imported.tokens).unwrap();
    let a = imported.paths.intern(a);
    let key = imported.tokens.intern("exedraTargets");
    let target = TargetPath::parse("/A", &mut imported.tokens, &mut imported.paths).unwrap();
    imported.layer.prims.get_mut(&a).unwrap().set_field(
        key,
        FieldValue::PathListOp(LayerListOp {
            explicit: Some(vec![target]),
            ..LayerListOp::default()
        }),
    );
    assert_eq!(
        imported.save(),
        Err(SaveError::Unsupported {
            path: "/A#exedraTargets".into(),
            feature: Unsupported::ListOpMetadata("path list op")
        }),
        "a path list op"
    );
}

/// A property spec added through the API, a relationship between
/// attributes, lands where it was authored.
#[test]
fn keeps_interleaved_property_order() {
    let mut imported = Imported::new("#usda 1.0\ndef \"A\"\n{\n    int x = 1\n    int z = 2\n}\n");
    let a = Path::parse_absolute("/A", &mut imported.tokens).unwrap();
    let a = imported.paths.intern(a);
    let y = imported.tokens.intern("y");
    let target = TargetPath::parse("/A.x", &mut imported.tokens, &mut imported.paths).unwrap();
    let spec = imported.layer.prims.get_mut(&a).unwrap();
    spec.properties.insert(
        1,
        PropertyEntry {
            name: y,
            spec: PropertySpec::relationship().with_targets(LayerListOp {
                explicit: Some(vec![target]),
                ..LayerListOp::default()
            }),
        },
    );
    let text = imported.save().unwrap();
    assert!(
        text.contains("    int x = 1\n    rel y = </A.x>\n    int z = 2\n"),
        "{text}"
    );
}
