// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Preservation corpus for saving an authored [`Layer`].
//!
//! Each [`SaveCase`] is an authored USDA layer, one edit made through the
//! [`Layer`] API, and the layer OpenUSD should read after that edit, written
//! out by hand. The `layer_save` test checks each case within the workspace;
//! the `export_interop` test and `scripts/export_interop.sh` check it
//! against OpenUSD when a Python with OpenUSD's `pxr` is available: the case
//! is imported from its USDA and from OpenUSD's USDC of it, edited, saved as
//! USDA and USDC, and every saved file must read in OpenUSD exactly as the
//! expected layer does (see `scripts/layer_snapshot.py`).
//!
//! The cases cover authored data a saved layer must keep without
//! interpreting it: UI hints, authorship-style unknown schemas, profile
//! claims, other schemas carried as data, explicitly empty lists and a
//! subroot `defaultPrim`, within the supported subset of
//! [`layerstack_usda::save`]. [`unsupported_cases`] are encodings outside
//! that subset, each with the error naming its source path.

use std::sync::Arc;

use layerstack::doc::{FieldValue, Layer, LayerId, Value};
use layerstack::interner::TokenInterner;
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathInterner, PropertyPath};
use layerstack::{AssetResolveError, AssetResolver, ResolvedAsset};
use layerstack_usda::save::{SaveError, Unsupported};

use crate::export_fixtures::OpenUsdRelease;

/// An imported layer with the interners its identifiers belong to.
#[derive(Debug)]
pub struct Imported {
    /// The layer.
    pub layer: Layer,
    /// Its token interner.
    pub tokens: TokenInterner,
    /// Its path interner.
    pub paths: PathInterner,
}

/// Resolves every asset to a fresh, empty layer, so that arcs survive
/// import and reach the save, which must reject them.
#[derive(Debug, Default)]
pub struct AnyAsset(u64);

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
            layer_id: LayerId(1000 + self.0),
            resolved_path: Arc::from(format!("/resolved/{asset_path}").as_str()),
            layer: None,
        })
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

impl Imported {
    /// Imports USDA text.
    ///
    /// # Panics
    ///
    /// Panics if the parser or the layer emitter reports a diagnostic.
    pub fn usda(source: &str) -> Self {
        let parsed = layerstack_usda::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let result = layerstack_usda::emit::emit(
            &parsed.layer,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut AnyAsset::default(),
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        Self {
            layer: result.layer,
            tokens,
            paths,
        }
    }

    /// Imports a USDC file.
    ///
    /// # Panics
    ///
    /// Panics if the reader fails or reports a diagnostic.
    pub fn usdc(bytes: &[u8]) -> Self {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let result = layerstack_usdc::read_usdc(
            bytes,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut AnyAsset::default(),
        )
        .expect("USDC reads");
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        Self {
            layer: result.layer,
            tokens,
            paths,
        }
    }

    /// Saves the layer as USDA.
    ///
    /// # Errors
    ///
    /// See [`layerstack_usda::save::layer_document`].
    pub fn save_usda(&self) -> Result<String, SaveError> {
        layerstack_usda::save::save_usda(&self.layer, &self.tokens, &self.paths)
    }

    /// Saves the layer as USDC.
    ///
    /// # Errors
    ///
    /// See [`layerstack_usdc::writer::save_layer`].
    pub fn save_usdc(&self) -> Result<Vec<u8>, layerstack_usdc::writer::UsdcWriteError> {
        layerstack_usdc::writer::save_layer(&self.layer, &self.tokens, &self.paths)
    }

    /// The authored prim spec at `path`, for editing.
    ///
    /// # Panics
    ///
    /// Panics if the layer authors no prim spec there.
    pub fn prim(&mut self, path: &str) -> &mut layerstack::doc::PrimSpec {
        let path = Path::parse_absolute(path, &mut self.tokens).expect("prim path");
        let id = self.paths.intern(path);
        self.layer.prims.get_mut(&id).expect("prim spec")
    }

    /// The authored property spec at `path`, for editing.
    ///
    /// # Panics
    ///
    /// Panics if the layer authors no property spec there.
    pub fn property(&mut self, path: &str) -> &mut layerstack::property::PropertySpec {
        let path =
            PropertyPath::parse(path, &mut self.tokens, &mut self.paths).expect("property path");
        self.layer.property_mut(path).expect("property spec")
    }
}

/// One corpus case: a source layer, an edit and the expected result.
#[derive(Clone, Copy, Debug)]
pub struct SaveCase {
    /// Short name, used for file names.
    pub name: &'static str,
    /// What the case preserves.
    pub covers: &'static str,
    /// The authored USDA layer.
    pub source: &'static str,
    /// One edit through the [`Layer`] API.
    pub edit: fn(&mut Imported),
    /// The source with that edit, as OpenUSD should read the saved layer.
    pub expected: &'static str,
    /// A weaker layer to compose the saved layer over, when the case is
    /// about what the saved opinions block or edit.
    pub weaker: Option<&'static str>,
    /// The earliest OpenUSD release whose metadata registry the case
    /// assumes, with the reason (see
    /// [`crate::export_fixtures::minimum_openusd`]); an older oracle skips
    /// the case.
    pub minimum_openusd: Option<(OpenUsdRelease, &'static str)>,
}

/// The preservation corpus.
pub fn cases() -> Vec<SaveCase> {
    vec![
        SaveCase {
            name: "ui_hints",
            covers: "nested `limits` and `uiHints` dictionaries on prims, attributes and \
                     relationships, and an authored empty `limits`; the edit strengthens \
                     the soft minimum and keeps the weaker soft maximum and hard limits",
            source: UI_HINTS,
            edit: |layer| {
                let limits = layer.tokens.intern("limits");
                let spec = layer.property("/Widget.exedra:size");
                let Some(FieldValue::Value(Value::Dictionary(entries))) =
                    layerstack::doc::get_field_mut(&mut spec.metadata, &limits)
                else {
                    panic!("limits dictionary");
                };
                let soft = entries.iter_mut().find(|(k, _)| &**k == "soft").unwrap();
                let Value::Dictionary(soft) = &mut soft.1 else {
                    panic!("soft limits dictionary");
                };
                soft.iter_mut().find(|(k, _)| &**k == "min").unwrap().1 = Value::Float(2.0);
            },
            expected: UI_HINTS_EDITED,
            weaker: None,
            minimum_openusd: Some((
                (25, 11),
                "`uiHints` (usdUI) and `limits` are registered from OpenUSD 25.11",
            )),
        },
        SaveCase {
            name: "authorship",
            covers: "unknown multiple-apply schema instances, namespaced uniform properties, \
                     paired arrays and path-looking strings; the edit hides one schema \
                     instance through an explicit `apiSchemas` list, which keeps its \
                     properties",
            source: AUTHORSHIP,
            edit: |layer| {
                let api_schemas = layer.tokens.intern("apiSchemas");
                let primary = layer.tokens.intern("ExedraAuthorshipAPI:primary");
                layer.prim("/Asset").set_field(
                    api_schemas,
                    FieldValue::TokenListOp(ListOp {
                        explicit: Some(vec![primary]),
                        ..ListOp::default()
                    }),
                );
            },
            expected: AUTHORSHIP_EDITED,
            weaker: None,
            minimum_openusd: None,
        },
        SaveCase {
            name: "profiles",
            covers: "`ClaimsAPI` profile records under `customData` (where the pinned \
                     implementation keeps them) and as the documented `profilesInfo` prim \
                     metadata, which OpenUSD does not register; an unrelated edit neither \
                     adds nor changes a claim",
            source: PROFILES,
            edit: |layer| {
                let kind = layer.tokens.intern("kind");
                let component = layer.tokens.intern("component");
                layer
                    .prim("/Kit")
                    .set_field(kind, FieldValue::Value(Value::Token(component)));
            },
            expected: PROFILES_EDITED,
            weaker: None,
            minimum_openusd: None,
        },
        SaveCase {
            name: "schemas_as_data",
            covers: "a MaterialX version identifier, semantic label instances, \
                     coordinate-system bindings as relationships and ordered LOD children, \
                     all without their evaluators; the edit adds a semantic label",
            source: SCHEMAS_AS_DATA,
            edit: |layer| {
                let labels = ["Q15026", "Q11707"].map(|l| Value::Token(layer.tokens.intern(l)));
                layer
                    .property("/World/Chair.semantics:labels:wikidata")
                    .default = Some(Value::Array(labels.to_vec()));
            },
            expected: SCHEMAS_AS_DATA_EDITED,
            weaker: None,
            minimum_openusd: None,
        },
        SaveCase {
            name: "explicit_empty_lists",
            covers: "explicitly empty target, connection and `apiSchemas` lists (`= None` \
                     and `= []`) next to a bare declaration and a deleted target; composed \
                     over a weaker layer, the empty lists block its opinions and the bare \
                     declaration does not",
            source: EXPLICIT_EMPTY,
            edit: |layer| {
                layer.property("/A.x").default = Some(Value::Float(2.0));
            },
            expected: EXPLICIT_EMPTY_EDITED,
            weaker: Some(EXPLICIT_EMPTY_WEAKER),
            minimum_openusd: None,
        },
        SaveCase {
            name: "subroot_default_prim",
            covers: "a `defaultPrim` naming a prim below the root, absolute in the source \
                     and relative after the edit, written as authored",
            source: SUBROOT_DEFAULT_PRIM,
            edit: |layer| {
                layer.layer.default_prim = Some(layer.tokens.intern("Model/Geo"));
            },
            expected: SUBROOT_DEFAULT_PRIM_EDITED,
            weaker: None,
            minimum_openusd: None,
        },
    ]
}

/// Authored encodings the save does not support yet: a name, the source
/// and the error, which names the source path.
pub fn unsupported_cases() -> Vec<(&'static str, &'static str, SaveError)> {
    let unsupported = |path: &str, feature| SaveError::Unsupported {
        path: path.into(),
        feature,
    };
    vec![
        (
            "sublayers",
            "#usda 1.0\n(\n    subLayers = [\n        @./base.usda@\n    ]\n)\n",
            unsupported("/", Unsupported::Sublayers),
        ),
        (
            "references",
            "#usda 1.0\ndef \"A\"\n{\n    def \"B\" (\n        prepend references = @./model.usda@</Model>\n    )\n    {\n    }\n}\n",
            unsupported("/A/B", Unsupported::References),
        ),
        (
            "payloads",
            "#usda 1.0\ndef \"A\" (\n    payload = @./heavy.usdc@\n)\n{\n}\n",
            unsupported("/A", Unsupported::Payloads),
        ),
        (
            "inherits",
            "#usda 1.0\nclass \"C\"\n{\n}\n\ndef \"A\" (\n    inherits = </C>\n)\n{\n}\n",
            unsupported("/A", Unsupported::Inherits),
        ),
        (
            "specializes",
            "#usda 1.0\nclass \"C\"\n{\n}\n\ndef \"A\" (\n    specializes = </C>\n)\n{\n}\n",
            unsupported("/A", Unsupported::Specializes),
        ),
        (
            "variant_sets",
            "#usda 1.0\ndef \"A\" (\n    variantSets = \"lod\"\n)\n{\n    variantSet \"lod\" = {\n        \"high\" {\n            def \"Geom\"\n            {\n            }\n        }\n    }\n}\n",
            unsupported("/A/Geom", Unsupported::VariantSpec),
        ),
        (
            "variant_selections",
            "#usda 1.0\ndef \"A\" (\n    variants = {\n        string lod = \"high\"\n    }\n)\n{\n}\n",
            unsupported("/A", Unsupported::VariantSelections),
        ),
        (
            "time_samples",
            "#usda 1.0\ndef \"A\"\n{\n    double x = 1\n    double x.timeSamples = {\n        0: 1,\n        1: 2,\n    }\n}\n",
            unsupported("/A.x", Unsupported::TimeSamples),
        ),
        (
            "array_edit",
            "#usda 1.0\ndef \"A\"\n{\n    int[] ids = edit [append 4]\n}\n",
            unsupported("/A.ids", Unsupported::ArrayEdit),
        ),
        (
            "untyped_empty_array",
            "#usda 1.0\ndef \"A\" (\n    customData = {\n        dictionary profilesInfo = {\n            dictionary profileCompatibility = {\n                string[] \"profile.studio.vfx.25.08\" = []\n            }\n        }\n    }\n)\n{\n}\n",
            unsupported(
                "/A#customData/profilesInfo/profileCompatibility/profile.studio.vfx.25.08",
                Unsupported::Value("untyped empty array"),
            ),
        ),
        (
            "half",
            "#usda 1.0\ndef \"A\"\n{\n    half3 h = (1, 2, 3)\n}\n",
            unsupported("/A.h", Unsupported::Value("half vector")),
        ),
    ]
}

const UI_HINTS: &str = r#"#usda 1.0
(
    defaultPrim = "Widget"
)

def Xform "Widget" (
    uiHints = {
        string displayGroup = "Controls"
    }
)
{
    custom float exedra:size = 3 (
        displayGroup = "Shape"
        limits = {
            dictionary hard = {
                float max = 10
                float min = 0
            }
            dictionary soft = {
                float max = 5
                float min = 1
            }
        }
        uiHints = {
            string displayName = "Size"
            dictionary valueLabels = {
                float Large = 5
                float Small = 1
            }
        }
    )
    custom int exedra:count = 3 (
        limits = {
        }
    )
    custom rel exedra:driver = </Widget.exedra:size> (
        uiHints = {
            bool hidden = true
        }
    )
}
"#;

const UI_HINTS_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Widget"
)

def Xform "Widget" (
    uiHints = {
        string displayGroup = "Controls"
    }
)
{
    custom float exedra:size = 3 (
        displayGroup = "Shape"
        limits = {
            dictionary hard = {
                float max = 10
                float min = 0
            }
            dictionary soft = {
                float max = 5
                float min = 2
            }
        }
        uiHints = {
            string displayName = "Size"
            dictionary valueLabels = {
                float Large = 5
                float Small = 1
            }
        }
    )
    custom int exedra:count = 3 (
        limits = {
        }
    )
    custom rel exedra:driver = </Widget.exedra:size> (
        uiHints = {
            bool hidden = true
        }
    )
}
"#;

const AUTHORSHIP: &str = r#"#usda 1.0

def Xform "Asset" (
    prepend apiSchemas = ["ExedraAuthorshipAPI:primary", "ExedraAuthorshipAPI:secondary"]
)
{
    uniform string authorship:primary:tool = "Exedra 3.1"
    uniform string authorship:primary:source = "/Users/artist/scenes/asset.usd"
    uniform string authorship:secondary:tool = "Painter"
    uniform string authorship:secondary:source = "./textures/../asset_v2.usda"
    uniform string authorship:secondary:target = "</World/Other.prop>"
    uniform string[] authorship:contributors = ["ana", "bo", "cy"]
    uniform int[] authorship:contributions = [3, 1, 4]
    uniform asset authorship:license = @./LICENSE.txt@
}
"#;

const AUTHORSHIP_EDITED: &str = r#"#usda 1.0

def Xform "Asset" (
    apiSchemas = ["ExedraAuthorshipAPI:primary"]
)
{
    uniform string authorship:primary:tool = "Exedra 3.1"
    uniform string authorship:primary:source = "/Users/artist/scenes/asset.usd"
    uniform string authorship:secondary:tool = "Painter"
    uniform string authorship:secondary:source = "./textures/../asset_v2.usda"
    uniform string authorship:secondary:target = "</World/Other.prop>"
    uniform string[] authorship:contributors = ["ana", "bo", "cy"]
    uniform int[] authorship:contributions = [3, 1, 4]
    uniform asset authorship:license = @./LICENSE.txt@
}
"#;

const PROFILES: &str = r#"#usda 1.0
(
    defaultPrim = "Kit"
)

def Xform "Kit" (
    prepend apiSchemas = ["ClaimsAPI"]
    customData = {
        dictionary profilesInfo = {
            dictionary capabilityUsages = {
                string "usd.geom.mesh" = "hard"
                string "usd.shading.mtlx" = "soft"
            }
            dictionary profileCompatibility = {
                string[] "vnd.apple.visionos_v1" = ["usd.shading.mtlx"]
            }
        }
    }
)
{
    def Mesh "Body" (
        profilesInfo = {
            dictionary capabilityUsages = {
                string "usd.geom.subdiv" = "enhancement"
            }
        }
    )
    {
    }

    def Xform "Unclaimed"
    {
    }
}
"#;

const PROFILES_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Kit"
)

def Xform "Kit" (
    prepend apiSchemas = ["ClaimsAPI"]
    customData = {
        dictionary profilesInfo = {
            dictionary capabilityUsages = {
                string "usd.geom.mesh" = "hard"
                string "usd.shading.mtlx" = "soft"
            }
            dictionary profileCompatibility = {
                string[] "vnd.apple.visionos_v1" = ["usd.shading.mtlx"]
            }
        }
    }
    kind = "component"
)
{
    def Mesh "Body" (
        profilesInfo = {
            dictionary capabilityUsages = {
                string "usd.geom.subdiv" = "enhancement"
            }
        }
    )
    {
    }

    def Xform "Unclaimed"
    {
    }
}
"#;

const SCHEMAS_AS_DATA: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Z"
)

def Xform "World" (
    prepend apiSchemas = ["CoordSysAPI:worldSpace", "CoordSysAPI:paint"]
)
{
    rel coordSys:worldSpace:binding = </World/Space>
    rel coordSys:paint:binding = </World/Chair/Paint>

    def Xform "Space"
    {
    }

    def Xform "Chair" (
        prepend apiSchemas = ["SemanticsLabelsAPI:taxonomy", "SemanticsLabelsAPI:wikidata"]
    )
    {
        reorder nameChildren = ["LOD0", "LOD1", "LOD2", "Paint"]
        token[] semantics:labels:taxonomy = ["furniture", "chair"]
        token[] semantics:labels:wikidata = ["Q15026"]

        def Xform "Paint"
        {
        }

        def Xform "LOD2"
        {
        }

        def Xform "LOD0"
        {
        }

        def Xform "LOD1"
        {
        }
    }

    def Material "Look" (
        prepend apiSchemas = ["MaterialXConfigAPI"]
    )
    {
        uniform string config:mtlx:version = "1.39"
        token outputs:mtlx:surface.connect = </World/Look/Surface.outputs:out>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            token outputs:out
        }
    }
}
"#;

const SCHEMAS_AS_DATA_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Z"
)

def Xform "World" (
    prepend apiSchemas = ["CoordSysAPI:worldSpace", "CoordSysAPI:paint"]
)
{
    rel coordSys:worldSpace:binding = </World/Space>
    rel coordSys:paint:binding = </World/Chair/Paint>

    def Xform "Space"
    {
    }

    def Xform "Chair" (
        prepend apiSchemas = ["SemanticsLabelsAPI:taxonomy", "SemanticsLabelsAPI:wikidata"]
    )
    {
        reorder nameChildren = ["LOD0", "LOD1", "LOD2", "Paint"]
        token[] semantics:labels:taxonomy = ["furniture", "chair"]
        token[] semantics:labels:wikidata = ["Q15026", "Q11707"]

        def Xform "Paint"
        {
        }

        def Xform "LOD2"
        {
        }

        def Xform "LOD0"
        {
        }

        def Xform "LOD1"
        {
        }
    }

    def Material "Look" (
        prepend apiSchemas = ["MaterialXConfigAPI"]
    )
    {
        uniform string config:mtlx:version = "1.39"
        token outputs:mtlx:surface.connect = </World/Look/Surface.outputs:out>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            token outputs:out
        }
    }
}
"#;

const EXPLICIT_EMPTY: &str = r#"#usda 1.0
(
    defaultPrim = "A"
)

def Xform "A" (
    apiSchemas = []
)
{
    rel declared
    rel blocked = None
    rel emptied = []
    float x = 1
    float blockedInput.connect = None
    float emptiedInput.connect = []
    delete rel pruned = </Elsewhere/Gone>
}
"#;

const EXPLICIT_EMPTY_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "A"
)

def Xform "A" (
    apiSchemas = []
)
{
    rel declared
    rel blocked = None
    rel emptied = None
    float x = 2
    float blockedInput.connect = None
    float emptiedInput.connect = None
    delete rel pruned = </Elsewhere/Gone>
}
"#;

const EXPLICIT_EMPTY_WEAKER: &str = r#"#usda 1.0

over "A" (
    prepend apiSchemas = ["CollectionAPI:weaker"]
)
{
    rel declared = </Elsewhere>
    rel blocked = </Elsewhere>
    rel emptied = </Elsewhere>
    float blockedInput.connect = </Elsewhere.out>
    float emptiedInput.connect = </Elsewhere.out>
    rel pruned = [</Elsewhere/Gone>, </Elsewhere/Kept>]
}

def "Elsewhere"
{
    float out

    def "Gone"
    {
    }

    def "Kept"
    {
    }
}
"#;

const SUBROOT_DEFAULT_PRIM: &str = r#"#usda 1.0
(
    defaultPrim = "/Model/Geo"
)

def Xform "Model"
{
    def Mesh "Geo"
    {
    }
}
"#;

const SUBROOT_DEFAULT_PRIM_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Model/Geo"
)

def Xform "Model"
{
    def Mesh "Geo"
    {
    }
}
"#;
