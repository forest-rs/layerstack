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
//! claims, other schemas carried as data, explicitly empty lists, a
//! subroot `defaultPrim`, animation, and composition arcs written by their
//! authored asset paths (placements of a shared asset, a retimed payload,
//! inherits and specializes, and arcs whose assets are missing), and
//! variant sets with their selections, branch prim specs and nested sets,
//! within the supported subset of [`layerstack_usda::save`]. [`unsupported_cases`] are
//! encodings outside that subset, each with the error naming its source
//! path.
//!
//! A case with [`SaveCase::composition`] is also composed with its assets:
//! the saved layer must compose as the expected layer does, in this
//! workspace ([`composed`]) and in OpenUSD.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use layerstack::doc::{FieldValue, Layer, LayerId, Value};
use layerstack::interner::TokenInterner;
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathInterner, PropertyPath};
use layerstack::property::PropertyEntry;
use layerstack::spec_path::{SpecComponent, SpecPath, VariantSelectionSite};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, InterpolationType, ResolvedAsset,
    ResolvedValue, Stage, StageOptions,
};
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
/// import and reach the save, except assets under `./missing/`, which do
/// not resolve and stay unresolved arcs.
#[derive(Debug, Default)]
pub struct AnyAsset(u64);

/// The prefix of the asset paths no resolver of the corpus finds.
pub const MISSING: &str = "./missing/";

impl AssetResolver for AnyAsset {
    fn resolve(
        &mut self,
        asset_path: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        if asset_path.starts_with(MISSING) {
            return Err(AssetResolveError::NotFound);
        }
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

    /// The variant `variant` of set `set` on the prim spec at `path`
    /// outside any variant, for editing.
    ///
    /// # Panics
    ///
    /// Panics if the prim spec has no such variant.
    pub fn variant(
        &mut self,
        path: &str,
        set: &str,
        variant: &str,
    ) -> &mut layerstack::doc::VariantSpec {
        let set = self.tokens.intern(set);
        let variant = self.tokens.intern(variant);
        self.prim(path)
            .variant_sets
            .get_mut(&set)
            .and_then(|set| set.variants.get_mut(&variant))
            .expect("variant spec")
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
    /// For a case about what the layer composes: the assets its arcs name,
    /// as `(asset path, USDA text)` (asset paths under [`MISSING`] are left
    /// out). The saved layer, composed with them, must compose as the
    /// expected layer does.
    pub composition: Option<&'static [(&'static str, &'static str)]>,
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
            composition: None,
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
            composition: None,
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
            composition: None,
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
            composition: None,
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
            composition: None,
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
            composition: None,
            minimum_openusd: None,
        },
        SaveCase {
            name: "shared_asset_placements",
            covers: "two placements referencing one shared asset, by its `defaultPrim` and \
                     by an explicit prim path, with a local override on one; the edit \
                     changes the override",
            source: PLACEMENTS,
            edit: |layer| {
                layer.property("/Scene/MarkerB/Beacon.radius").default = Some(Value::Double(1.0));
            },
            expected: PLACEMENTS_EDITED,
            weaker: None,
            minimum_openusd: None,
            composition: Some(&[("./assets/marker.usda", MARKER_ASSET)]),
        },
        SaveCase {
            name: "retimed_payload",
            covers: "a prepended payload with a layer offset and scale, and an explicit \
                     payload with a prim path; the edit moves the offset",
            source: RETIMED_PAYLOAD,
            edit: |layer| {
                layer.prim("/Shot").payloads.prepend[0].layer_offset.offset = 48.0;
            },
            expected: RETIMED_PAYLOAD_EDITED,
            weaker: None,
            minimum_openusd: None,
            composition: Some(&[
                ("./assets/pulse.usda", PULSE_ASSET),
                ("./assets/proxy.usda", PROXY_ASSET),
            ]),
        },
        SaveCase {
            name: "inherit_specialize",
            covers: "an inherit and a specialize of local classes; the edit adds an inherit \
                     ahead of the first, which changes what the prim composes",
            source: INHERIT_SPECIALIZE,
            edit: |layer| {
                let accent = Path::parse_absolute("/_Accent", &mut layer.tokens).unwrap();
                let accent = layer.paths.intern(accent);
                layer
                    .prim("/Item")
                    .inherits
                    .explicit
                    .as_mut()
                    .expect("explicit inherits")
                    .insert(0, accent);
            },
            expected: INHERIT_SPECIALIZE_EDITED,
            weaker: None,
            minimum_openusd: None,
            composition: Some(&[]),
        },
        SaveCase {
            name: "unresolved_arcs",
            covers: "a reference and a sublayer whose assets are missing, kept as authored \
                     with their prim path and offsets next to resolved ones; the edit moves \
                     the prim that authors the reference",
            source: UNRESOLVED_ARCS,
            edit: |layer| {
                layer.property("/Site/Anchor.xformOp:translate").default =
                    Some(Value::Vec3d([0.0, 2.0, 0.0]));
            },
            expected: UNRESOLVED_ARCS_EDITED,
            weaker: None,
            minimum_openusd: None,
            composition: Some(&[
                ("./assets/marker.usda", MARKER_ASSET),
                ("./assets/lighting.usda", LIGHTING_ASSET),
            ]),
        },
        SaveCase {
            name: "typed_values",
            covers: "string and `int64` list-op metadata (`clipSets`, `inactiveIds`) and \
                     `uchar`, `uint64`, `half`, half vector, quaternion and `matrix2d` / \
                     `matrix3d` values and arrays, with a `matrix4d` array; the edit \
                     prepends an inactive id",
            source: TYPED_VALUES,
            edit: |layer| {
                let inactive = layer.tokens.intern("inactiveIds");
                let spec = layer.prim("/Swarm");
                let Some(FieldValue::Int64ListOp(op)) =
                    layerstack::doc::get_field_mut(&mut spec.fields, &inactive)
                else {
                    panic!("inactiveIds list op");
                };
                op.prepend.insert(0, 11);
            },
            expected: TYPED_VALUES_EDITED,
            weaker: None,
            composition: None,
            minimum_openusd: None,
        },
        SaveCase {
            name: "animated_attribute",
            covers: "time samples next to a default, a blocked sample, sample-only \
                     attributes of scalar, array (an empty one included) and `timecode` \
                     types, samples with a connection, and the layer's time metadata; the \
                     edit adds a sample",
            source: ANIMATED,
            edit: |layer| {
                let samples = layer
                    .property("/Rig.intensity")
                    .time_samples
                    .as_mut()
                    .expect("intensity samples");
                samples.push((24.0, Value::Float(0.25)));
            },
            expected: ANIMATED_EDITED,
            weaker: None,
            composition: Some(&[]),
            minimum_openusd: None,
        },
        SaveCase {
            name: "variant_sets",
            covers: "variant sets of two branches with their selections: a branch with a \
                     reference, metadata, time samples and a child prim, a variant set \
                     nested in a branch with a selection of its own, a set declared \
                     without variants, and a selection authored on a prim inside a \
                     branch of its ancestor, and a branch child prim with a variant \
                     set of its own; the edit changes an attribute inside a branch",
            source: VARIANT_SETS,
            edit: |layer| {
                let height = layer.tokens.intern("height");
                let summer = layer.variant("/Forest/Oak", "season", "summer");
                layerstack::property::get_property_mut(&mut summer.properties, height)
                    .expect("summer height")
                    .default = Some(Value::Double(5.0));
            },
            expected: VARIANT_SETS_EDITED,
            weaker: None,
            composition: Some(&[("./assets/leaves.usda", LEAVES_ASSET)]),
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
            "path_expression",
            "#usda 1.0\ndef \"A\"\n{\n    pathExpression p = \"/A//\"\n}\n",
            unsupported("/A.p", Unsupported::Value("pathExpression")),
        ),
    ]
}

/// A layer to compose: USDA text or a USDC file.
#[derive(Clone, Copy, Debug)]
pub enum Root<'a> {
    /// USDA text.
    Usda(&'a str),
    /// A USDC file.
    Usdc(&'a [u8]),
}

/// Resolves the asset paths of `assets` to their USDA, emitted on demand
/// (arcs inside an asset resolve through the same table); any other asset
/// path does not resolve.
struct Assets<'a> {
    assets: &'a [(&'a str, &'a str)],
    ids: BTreeMap<String, LayerId>,
    loaded: Vec<Layer>,
}

impl AssetResolver for Assets<'_> {
    fn resolve(
        &mut self,
        asset_path: &str,
        _: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let resolved_path = Arc::from(asset_path);
        if let Some(&layer_id) = self.ids.get(asset_path) {
            return Ok(ResolvedAsset {
                layer_id,
                resolved_path,
                layer: None,
            });
        }
        let &(_, text) = self
            .assets
            .iter()
            .find(|(path, _)| *path == asset_path)
            .ok_or(AssetResolveError::NotFound)?;
        let layer_id = LayerId(100 + self.ids.len() as u64);
        self.ids.insert(asset_path.into(), layer_id);
        let parsed = layerstack_usda::parser::parse(text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let result = layerstack_usda::emit::emit(&parsed.layer, layer_id, tokens, paths, self);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        self.loaded.extend(result.resolved_layers);
        Ok(ResolvedAsset {
            layer_id,
            resolved_path,
            layer: Some(result.layer),
        })
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// Stage times [`composed`] resolves every attribute at, besides its
/// default.
pub const PROBE_TIMES: [f64; 9] = [-5.0, 0.0, 5.0, 12.0, 24.0, 30.0, 48.0, 53.0, 100.0];

/// What this workspace composes from `root` over `assets` (see
/// [`SaveCase::composition`]): per prim in traversal order, its path,
/// specifier and type, then per property (by name) its resolved value,
/// its value at each of [`PROBE_TIMES`] and its targets; and the number of
/// composition errors. Tokens are spelled out, so the text compares across
/// stores.
///
/// # Panics
///
/// Panics if the layer or an asset does not read cleanly.
pub fn composed(root: Root<'_>, assets: &[(&str, &str)]) -> String {
    let mut store = InMemoryStore::default();
    let mut resolver = Assets {
        assets,
        ids: BTreeMap::new(),
        loaded: Vec::new(),
    };
    let (layer, resolved) = match root {
        Root::Usda(text) => {
            let parsed = layerstack_usda::parser::parse(text);
            assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
            let result = layerstack_usda::emit::emit(
                &parsed.layer,
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut resolver,
            );
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            (result.layer, result.resolved_layers)
        }
        Root::Usdc(bytes) => {
            let result = layerstack_usdc::read_usdc(
                bytes,
                LayerId(1),
                &mut store.tokens,
                &mut store.paths,
                &mut resolver,
            )
            .expect("USDC reads");
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            (result.layer, result.resolved_layers)
        }
    };
    for layer in resolver.loaded.into_iter().chain(resolved) {
        store.insert_layer(layer);
    }
    store.insert_layer(layer);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());

    let mut out = String::new();
    let root_path = store.paths.intern(Path::root());
    let prims: Vec<_> = stage.traverse(root_path).collect();
    for prim in prims {
        let shown = store.paths.display(prim, &store.tokens);
        let type_name = stage
            .resolve_type_name(prim, &store)
            .map(|t| store.tokens.resolve(t).to_string());
        let specifier = stage.resolve_specifier(prim, &store);
        let _ = writeln!(out, "{shown} {specifier:?} {type_name:?}");
        let mut names = BTreeSet::new();
        for (layer_id, spec_path) in stage.prim_stack(prim).unwrap_or_default() {
            let Some(properties) = stack_properties(&store, layer_id, &spec_path) else {
                continue;
            };
            names.extend(
                properties
                    .iter()
                    .map(|p| store.tokens.resolve(p.name).to_string()),
            );
        }
        for name in names {
            let path = store.property_path(&format!("{shown}.{name}"));
            let value = match stage.resolve_property_path(path).map(|r| r.value) {
                Some(ResolvedValue::Scalar(v)) => spell(&v, &store.tokens),
                other => format!("{other:?}"),
            };
            let _ = write!(out, "  .{name} = {value}");
            for time in PROBE_TIMES {
                let at = stage
                    .resolve_property_path_at_time(path, time, InterpolationType::Held)
                    .map(|r| spell(&r.value, &store.tokens));
                let _ = write!(out, " @{time}: {at:?}");
            }
            let targets: Vec<String> = stage
                .resolve_target_list_path(path)
                .map(|r| r.value)
                .unwrap_or_default()
                .iter()
                .map(|t| t.display(&store.paths, &store.tokens))
                .collect();
            let _ = writeln!(out, " targets {targets:?}");
        }
    }
    let _ = writeln!(out, "errors: {}", stage.composition_errors().len());
    out
}

/// The properties a prim stack entry authors: those of the prim spec at
/// `spec_path`, in the variant branches that enclose it (`/A{v=x}B`), or of
/// the variant it ends with (`/A{v=x}`).
fn stack_properties<'a>(
    store: &'a InMemoryStore,
    layer_id: LayerId,
    spec_path: &SpecPath,
) -> Option<&'a [PropertyEntry]> {
    let mut segments = Vec::new();
    let mut sites = Vec::new();
    let mut last = None;
    for component in spec_path.components() {
        match *component {
            SpecComponent::Prim(name) => {
                sites.extend(last.take());
                segments.push(name);
            }
            SpecComponent::VariantSelection { set, variant } => {
                sites.extend(last.take());
                let host_path = store.paths.lookup(&Path::root().join(&segments))?;
                last = Some(VariantSelectionSite {
                    host_path,
                    set,
                    variant,
                });
            }
        }
    }
    let id = store.paths.lookup(&Path::root().join(&segments))?;
    // Selections on the prim itself name one of its spec's variants.
    sites.retain(|site| site.host_path != id);
    let spec = store.layers.get(&layer_id)?.prim_spec_in(id, &sites)?;
    Some(match last {
        Some(site) => {
            &spec
                .variant_sets
                .get(&site.set)?
                .variants
                .get(&site.variant)?
                .properties
        }
        None => &spec.properties,
    })
}

/// A value with its tokens spelled out.
fn spell(value: &Value, tokens: &TokenInterner) -> String {
    match value {
        Value::Token(t) => format!("token {:?}", tokens.resolve(*t)),
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(|v| spell(v, tokens)).collect();
            format!("[{}]", items.join(", "))
        }
        other => format!("{other:?}"),
    }
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

const MARKER_ASSET: &str = r#"#usda 1.0
(
    defaultPrim = "Marker"
)

def Xform "Marker"
{
    def Sphere "Beacon"
    {
        double radius = 0.5
        color3f[] primvars:displayColor = [(0.2, 0.2, 0.2)]
    }
}
"#;

const PLACEMENTS: &str = r#"#usda 1.0
(
    defaultPrim = "Scene"
)

def Xform "Scene"
{
    def Xform "MarkerA" (
        prepend references = @./assets/marker.usda@
    )
    {
        double3 xformOp:translate = (1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Xform "MarkerB" (
        prepend references = @./assets/marker.usda@</Marker>
    )
    {
        double3 xformOp:translate = (-1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]

        over "Beacon"
        {
            double radius = 0.75
        }
    }
}
"#;

const PLACEMENTS_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Scene"
)

def Xform "Scene"
{
    def Xform "MarkerA" (
        prepend references = @./assets/marker.usda@
    )
    {
        double3 xformOp:translate = (1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }

    def Xform "MarkerB" (
        prepend references = @./assets/marker.usda@</Marker>
    )
    {
        double3 xformOp:translate = (-1, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]

        over "Beacon"
        {
            double radius = 1
        }
    }
}
"#;

const PULSE_ASSET: &str = r#"#usda 1.0
(
    defaultPrim = "Pulse"
)

def Xform "Pulse"
{
    float intensity.timeSamples = {
        0: 0,
        10: 1,
    }
}
"#;

const PROXY_ASSET: &str = r#"#usda 1.0

def Scope "Proxy"
{
    custom token role = "stand-in"
}
"#;

const RETIMED_PAYLOAD: &str = r#"#usda 1.0
(
    defaultPrim = "Shot"
)

def Xform "Shot" (
    prepend payload = @./assets/pulse.usda@ (offset = 24; scale = 0.5)
)
{
    def Scope "Stand" (
        payload = @./assets/proxy.usda@</Proxy>
    )
    {
    }
}
"#;

const RETIMED_PAYLOAD_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Shot"
)

def Xform "Shot" (
    prepend payload = @./assets/pulse.usda@ (offset = 48; scale = 0.5)
)
{
    def Scope "Stand" (
        payload = @./assets/proxy.usda@</Proxy>
    )
    {
    }
}
"#;

const INHERIT_SPECIALIZE: &str = r#"#usda 1.0
(
    defaultPrim = "Item"
)

class "_Base"
{
    float size = 1
    token finish = "matte"
}

class "_Accent"
{
    token finish = "gloss"
}

class "_Tint"
{
    color3f tint = (0.5, 0.5, 0.5)
}

def Xform "Item" (
    inherits = </_Base>
    prepend specializes = </_Tint>
)
{
    float size = 2
}
"#;

const INHERIT_SPECIALIZE_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Item"
)

class "_Base"
{
    float size = 1
    token finish = "matte"
}

class "_Accent"
{
    token finish = "gloss"
}

class "_Tint"
{
    color3f tint = (0.5, 0.5, 0.5)
}

def Xform "Item" (
    inherits = [</_Accent>, </_Base>]
    prepend specializes = </_Tint>
)
{
    float size = 2
}
"#;

const LIGHTING_ASSET: &str = r#"#usda 1.0

over "Site"
{
    float exposure.timeSamples = {
        0: 1,
        10: 2,
    }
}
"#;

const UNRESOLVED_ARCS: &str = r#"#usda 1.0
(
    defaultPrim = "Site"
    subLayers = [
        @./missing/notes.usda@ (offset = 5),
        @./assets/lighting.usda@ (offset = 10; scale = 2)
    ]
)

def Xform "Site"
{
    def Xform "Anchor" (
        prepend references = [@./missing/anchor.usda@</Anchor> (offset = 2), @./assets/marker.usda@]
    )
    {
        double3 xformOp:translate = (0, 1, 0)
    }
}
"#;

const UNRESOLVED_ARCS_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Site"
    subLayers = [
        @./missing/notes.usda@ (offset = 5),
        @./assets/lighting.usda@ (offset = 10; scale = 2)
    ]
)

def Xform "Site"
{
    def Xform "Anchor" (
        prepend references = [@./missing/anchor.usda@</Anchor> (offset = 2), @./assets/marker.usda@]
    )
    {
        double3 xformOp:translate = (0, 2, 0)
    }
}
"#;

const TYPED_VALUES: &str = r#"#usda 1.0

def PointInstancer "Swarm" (
    prepend clipSets = ["motion"]
    prepend inactiveIds = [3, 7]
)
{
    custom uniform half exedra:weight = 0.5
    custom half3 exedra:tint = (1, 0.5, 0.25)
    custom color3h[] exedra:shades = [(0.25, 0.125, 0.75), (1, 1, 1)]
    custom half[] exedra:ramp = [0, 0.5, 1]
    custom uchar exedra:level = 200
    custom uchar[] exedra:bytes = [0, 255]
    custom uint64 exedra:serial = 18446744073709551615
    custom uint64[] exedra:tags = [1, 2]
    custom quatf exedra:spin = (1, 0, 0, 0)
    custom quath[] exedra:turns = [(0.5, 0.5, 0.5, 0.5)]
    custom quatd exedra:pose = (0, 1, 0, 0)
    custom matrix2d exedra:shear = ( (1, 0.5), (0, 1) )
    custom matrix3d exedra:basis = ( (0, 1, 0), (1, 0, 0), (0, 0, 1) )
    custom matrix4d[] exedra:frames = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1) )]
}
"#;

const TYPED_VALUES_EDITED: &str = r#"#usda 1.0

def PointInstancer "Swarm" (
    prepend clipSets = ["motion"]
    prepend inactiveIds = [11, 3, 7]
)
{
    custom uniform half exedra:weight = 0.5
    custom half3 exedra:tint = (1, 0.5, 0.25)
    custom color3h[] exedra:shades = [(0.25, 0.125, 0.75), (1, 1, 1)]
    custom half[] exedra:ramp = [0, 0.5, 1]
    custom uchar exedra:level = 200
    custom uchar[] exedra:bytes = [0, 255]
    custom uint64 exedra:serial = 18446744073709551615
    custom uint64[] exedra:tags = [1, 2]
    custom quatf exedra:spin = (1, 0, 0, 0)
    custom quath[] exedra:turns = [(0.5, 0.5, 0.5, 0.5)]
    custom quatd exedra:pose = (0, 1, 0, 0)
    custom matrix2d exedra:shear = ( (1, 0.5), (0, 1) )
    custom matrix3d exedra:basis = ( (0, 1, 0), (1, 0, 0), (0, 0, 1) )
    custom matrix4d[] exedra:frames = [( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1) )]
}
"#;

const ANIMATED: &str = r#"#usda 1.0
(
    defaultPrim = "Rig"
    endTimeCode = 48
    startTimeCode = 0
    timeCodesPerSecond = 24
)

def Xform "Rig"
{
    double3 xformOp:translate = (0, 0, 0)
    double3 xformOp:translate.timeSamples = {
        0: (0, 0, 0),
        24: (1, 2, 0),
        36: None,
        48: (4, 0, -1.5),
    }
    uniform token[] xformOpOrder = ["xformOp:translate"]
    float intensity.timeSamples = {
        0: 1,
        12: 0.5,
    }
    float[] weights.timeSamples = {
        1: [0.25, 0.75],
        2: [],
    }
    token mode.timeSamples = {
        0: "idle",
        10: "run",
    }
    timecode cue.timeSamples = {
        10: 12.5,
    }
    float driven.timeSamples = {
        5: 2,
    }
    float driven.connect = </Rig.intensity>
}
"#;

const ANIMATED_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Rig"
    endTimeCode = 48
    startTimeCode = 0
    timeCodesPerSecond = 24
)

def Xform "Rig"
{
    double3 xformOp:translate = (0, 0, 0)
    double3 xformOp:translate.timeSamples = {
        0: (0, 0, 0),
        24: (1, 2, 0),
        36: None,
        48: (4, 0, -1.5),
    }
    uniform token[] xformOpOrder = ["xformOp:translate"]
    float intensity.timeSamples = {
        0: 1,
        12: 0.5,
        24: 0.25,
    }
    float[] weights.timeSamples = {
        1: [0.25, 0.75],
        2: [],
    }
    token mode.timeSamples = {
        0: "idle",
        10: "run",
    }
    timecode cue.timeSamples = {
        10: 12.5,
    }
    float driven.timeSamples = {
        5: 2,
    }
    float driven.connect = </Rig.intensity>
}
"#;

const LEAVES_ASSET: &str = r#"#usda 1.0
(
    defaultPrim = "Leaves"
)

def Xform "Leaves"
{
    token shade = "green"

    def Scope "Blade"
    {
    }
}
"#;

const VARIANT_SETS: &str = r#"#usda 1.0
(
    defaultPrim = "Forest"
)

def Xform "Forest" (
    variants = {
        string density = "dense"
    }
    prepend variantSets = "density"
)
{
    def Xform "Oak" (
        variants = {
            string season = "summer"
        }
        prepend variantSets = ["season", "shape"]
    )
    {
        variantSet "season" = {
            "summer" (
                prepend references = @./assets/leaves.usda@
                variants = {
                    string size = "tall"
                }
                prepend variantSets = "size"
            ) {
                double height = 4
                float sway.timeSamples = {
                    0: 0,
                    24: 1,
                }

                def Mesh "Canopy"
                {
                    float3[] extent = [(-1, -1, -1), (1, 1, 1)]
                }

                variantSet "size" = {
                    "short" {
                        double spread = 2
                    }
                    "tall" {
                        double spread = 6

                        def Scope "Crown"
                        {
                        }
                    }
                }
            }
            "winter" (
                kind = "component"
            ) {
                double height = 2
            }
        }
    }

    def Xform "Pine" (
        prepend variantSets = "season"
    )
    {
        variantSet "season" = {
            "summer" {
                double height = 5
            }
            "winter" {
                double height = 6

                def Scope "Snow"
                {
                }
            }
        }
    }

    variantSet "density" = {
        "dense" {
            over "Pine" (
                variants = {
                    string season = "winter"
                }
            )
            {
            }

            def Scope "Rock" (
                variants = {
                    string moss = "covered"
                }
                prepend variantSets = "moss"
            )
            {
                int count = 3

                variantSet "moss" = {
                    "bare" {
                    }
                    "covered" {
                        def Scope "Moss"
                        {
                        }
                    }
                }
            }
        }
        "sparse" {
        }
    }
}
"#;

const VARIANT_SETS_EDITED: &str = r#"#usda 1.0
(
    defaultPrim = "Forest"
)

def Xform "Forest" (
    variants = {
        string density = "dense"
    }
    prepend variantSets = "density"
)
{
    def Xform "Oak" (
        variants = {
            string season = "summer"
        }
        prepend variantSets = ["season", "shape"]
    )
    {
        variantSet "season" = {
            "summer" (
                prepend references = @./assets/leaves.usda@
                variants = {
                    string size = "tall"
                }
                prepend variantSets = "size"
            ) {
                double height = 5
                float sway.timeSamples = {
                    0: 0,
                    24: 1,
                }

                def Mesh "Canopy"
                {
                    float3[] extent = [(-1, -1, -1), (1, 1, 1)]
                }

                variantSet "size" = {
                    "short" {
                        double spread = 2
                    }
                    "tall" {
                        double spread = 6

                        def Scope "Crown"
                        {
                        }
                    }
                }
            }
            "winter" (
                kind = "component"
            ) {
                double height = 2
            }
        }
    }

    def Xform "Pine" (
        prepend variantSets = "season"
    )
    {
        variantSet "season" = {
            "summer" {
                double height = 5
            }
            "winter" {
                double height = 6

                def Scope "Snow"
                {
                }
            }
        }
    }

    variantSet "density" = {
        "dense" {
            over "Pine" (
                variants = {
                    string season = "winter"
                }
            )
            {
            }

            def Scope "Rock" (
                variants = {
                    string moss = "covered"
                }
                prepend variantSets = "moss"
            )
            {
                int count = 3

                variantSet "moss" = {
                    "bare" {
                    }
                    "covered" {
                        def Scope "Moss"
                        {
                        }
                    }
                }
            }
        }
        "sparse" {
        }
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
