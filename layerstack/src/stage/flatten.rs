// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Flattening: a composed stage written out as one layer, with a report of
//! how.
//!
//! [`Stage::flatten`] turns the composed stage into a [`Layer`] that holds
//! no composition arcs: every composed prim becomes a prim spec with its
//! composed specifier, type name and metadata, and every composed property a
//! property spec with its resolved values, in stage time. Opening the layer
//! on its own composes the stage it was flattened from. The layer is an
//! ordinary [`Layer`], so the existing savers write it as USDA or USDC.
//!
//! Flattening is a transformation whose outcome can be inspected, not a
//! function that either succeeds or stops at the first problem:
//!
//! - The caller declares what it needs in [`FlattenRequirements`]: whether
//!   instances stay shared, whether animation must be exact, how asset
//!   paths are written and which losses are acceptable. A flatten that
//!   cannot meet a requirement is refused with every unmet requirement
//!   listed ([`FlattenError::Refused`]); it never returns a layer that
//!   silently breaks one.
//! - A flatten returns the layer with a [`FlattenReport`]: counts of what
//!   was written exactly, and a [`Finding`] for everything else: each
//!   deliberate transformation, each loss and each asset the layer still
//!   refers to, with the composed path and the opinion that caused it.
//! - [`Stage::verify_flattened`] checks a flattened layer, composed on its
//!   own (after a save and a re-read, when the caller wants those covered
//!   too), against the stage, and reports what it compared.
//!
//! What is written follows OpenUSD's `UsdStage::Flatten`
//! (`pxr/usd/usd/stage.cpp`, `usdcat --flatten`):
//!
//! - Every composed prim gets a spec, children in composed order, with its
//!   composed specifier, type name and metadata, including `active`,
//!   `instanceable`, `reorder nameChildren` and `reorder properties`; a
//!   composed list op, such as `apiSchemas`, is written as the explicit
//!   list it resolves to. Inactive prims are not composed, so they are not
//!   written. Class prims are written as classes.
//! - An attribute keeps its composed type, variability and metadata. Its
//!   `custom` is the weakest opinion's, as OpenUSD's flatten writes it,
//!   although the stage reports a property custom when any opinion is
//!   (AOUSD Core §12.2.4). A property the prim's schemas define
//!   ([`StageOptions::schemas`](crate::StageOptions::schemas)) is written as
//!   the schema declares it, as OpenUSD writes it.
//! - An attribute's time samples are written when its strongest value
//!   source is time samples, or a sparse array edit default over weaker
//!   samples: every sample in stage time, with sparse array edits composed
//!   into the dense arrays they give at each sample time. Its spline is
//!   written when that is its strongest value source, retimed into stage
//!   time. Its default is written whenever an opinion authors one, as the
//!   resolved default or a value block. `timecode` values are in stage
//!   time, as the stage resolves them.
//! - A relationship keeps its composed targets, and an attribute its
//!   composed connections, as explicit lists in stage namespace.
//! - References, payloads, inherits, specializes, variant sets, variant
//!   selections, sublayers and relocates are gone: composition has applied
//!   them.
//! - Instances keep sharing ([`Instancing::Preserve`]): each group of
//!   instances that compose the same prototype gets one root prim
//!   `Flattened_Prototype_N`, written first, holding the prototype's
//!   descendants, and each instance an internal reference to it in place
//!   of its descendants, as OpenUSD writes them.
//! - Asset paths are anchored to the layer that authors each, as OpenUSD's
//!   flatten anchors them, when the requirements ask for it
//!   ([`AssetPaths::Anchored`]); either way every asset path the layer
//!   names is reported as an external dependency.
//! - The layer metadata is the root layer's: `defaultPrim`, `upAxis`,
//!   `metersPerUnit`, `timeCodesPerSecond`, `startTimeCode`,
//!   `endTimeCode`, `documentation`, `reorder rootPrims` and the rest
//!   (AOUSD Core §12.2.7).
//!
//! What a flattened layer cannot hold is a [`Loss`], never dropped
//! silently: value clips, which composition does not read; times authored
//! at another `timeCodesPerSecond`, which composition does not rescale; and
//! an attribute no opinion gives a type, which OpenUSD's flatten omits too.
//!
//! Where it differs from OpenUSD:
//!
//! - OpenUSD also reads the session layer's metadata; a [`Stage`] has no
//!   session layer.
//! - OpenUSD's flatten drops metadata no schema registers, which it cannot
//!   set on a spec; this keeps it.
//!
//! Spec: AOUSD Core §10 (composition arcs), §11 (stage population and
//! instancing, §11.3.3), §12 (value resolution; §12.3.2.1 layer offsets).

mod report;
mod verify;

pub use report::{
    AssetPaths, ExternalDependency, Finding, FindingCategory, FindingKind, FindingSource,
    FlattenError, FlattenRefusal, FlattenReport, FlattenRequirements, Instancing, Loss, LossPolicy,
    ObjectPath, Preserved, Requirement, Transformation, UnmetRequirement,
};
pub use verify::{FlattenVerification, Mismatch, MismatchKind, SkipReason, Skipped, VerifiedScope};

use alloc::{borrow::Cow, format, string::String, sync::Arc, vec::Vec};

use hashbrown::{HashMap, HashSet};

use super::{
    ResolvedValue, Stage,
    stage_time::{map_leaves, map_opinion, retime_value, to_stage_time},
};
use crate::{
    asset::AssetResolver,
    doc::{FieldEntry, FieldValue, Layer, LayerId, LayerStore, PrimSpec, Reference, Value},
    interner::TokenId,
    listop::ListOp,
    path::{Path, PathId, PropertyPath, TargetPath},
    prim_index::{ArcKind, FieldKey, Opinion},
    prim_index_graph::NodeId,
    property::{PropertyEntry, PropertyKind, PropertySpec, PropertyType, Variability},
    spec_path::{SpecComponent, SpecPath},
};

/// A flattened stage: the layer, and the report of how it was written.
#[derive(Clone, Debug, PartialEq)]
pub struct Flattened {
    /// The flattened layer, with no composition arcs.
    pub layer: Layer,
    /// Everything the flatten found.
    pub report: FlattenReport,
}

/// Prim metadata fields that hold value clips (OpenUSD's
/// `UsdGetClipRelatedFields`, `pxr/usd/usd/clipsAPI.h`).
const CLIP_FIELDS: &[&str] = &[
    "clips",
    "clipSets",
    "clipActive",
    "clipAssetPaths",
    "clipManifestAssetPath",
    "clipPrimPath",
    "clipTemplateAssetPath",
    "clipTemplateEndTime",
    "clipTemplateStartTime",
    "clipTemplateStride",
    "clipTemplateActiveOffset",
    "clipTimes",
];

/// What makes two instances share a prototype: the arcs that bring in the
/// instance's descendants, each with its kind, site and offset, and the
/// variant selections. OpenUSD: `PcpInstanceKey` (`pxr/usd/pcp/instanceKey.h`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct InstanceKey {
    arcs: Vec<(ArcKind, LayerId, SpecPath, u64, u64)>,
    selections: Vec<(TokenId, TokenId)>,
}

/// Maps paths beneath an instance onto the prototype that stands for it.
#[derive(Clone, Copy)]
struct Remap {
    from: PathId,
    to: PathId,
}

impl Stage {
    /// Flattens the stage into one layer with no composition arcs, as
    /// OpenUSD's `UsdStage::Flatten` does (`usdcat --flatten`), meeting
    /// `requirements`.
    ///
    /// `root` is the root layer the stage was composed from, whose layer
    /// metadata the flattened layer takes; `id` is the flattened layer's
    /// own id, which the internal references of instances name. See the
    /// [module docs](crate::stage::flatten) for what is written and
    /// reported.
    ///
    /// ```
    /// use layerstack::{
    ///     InMemoryStore, Layer, LayerId, PrimSpec, Reference, Stage, StageOptions,
    ///     stage::flatten::{FlattenRequirements, FindingKind, Transformation},
    /// };
    ///
    /// let mut store = InMemoryStore::default();
    /// let (tree, asset) = (store.path("/Tree"), store.path("/Asset"));
    /// let mut root = Layer::new(LayerId(1));
    /// root.insert_prim(tree, PrimSpec::def().with_reference(Reference::new(LayerId(1), asset)));
    /// root.insert_prim(asset, PrimSpec::def());
    /// store.insert_layer(root);
    ///
    /// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let flat = stage
    ///     .flatten(&mut store, LayerId(1), LayerId(2), &FlattenRequirements::default())
    ///     .expect("nothing is lost");
    /// assert!(flat.layer.prims[&tree].references.explicit.is_none());
    /// assert!(flat.report.is_lossless());
    /// assert_eq!(flat.report.preserved.prims, 3, "the pseudo-root, /Tree and /Asset");
    /// ```
    ///
    /// # Errors
    ///
    /// [`FlattenError::MissingRootLayer`] when `root` is not in `store`,
    /// and [`FlattenError::Refused`], with every unmet requirement and the
    /// full report, when the stage composes something `requirements` does
    /// not allow the flatten to lose.
    ///
    /// Spec: AOUSD Core §11 (stage population), §12 (value resolution),
    /// §12.2.7 (layer metadata comes from the root layer).
    pub fn flatten(
        &self,
        store: &mut dyn LayerStore,
        root: LayerId,
        id: LayerId,
        requirements: &FlattenRequirements<'_>,
    ) -> Result<Flattened, FlattenError> {
        let root_layer = store
            .layer(root)
            .ok_or(FlattenError::MissingRootLayer(root))?;
        let mut out = Layer::new(id);
        out.default_prim = root_layer.default_prim;
        out.metadata = root_layer.metadata.clone();
        let pseudo_root = store.paths_mut().intern(Path::root());
        // `reorder rootPrims`, which OpenUSD copies with the rest of the
        // pseudo-root's metadata.
        let root_order = store
            .layer(root)
            .and_then(|layer| layer.prims.get(&pseudo_root))
            .and_then(|spec| spec.prim_order.clone());
        let mut flattener = Flattener {
            stage: self,
            store,
            requirements,
            out,
            prototypes: HashMap::new(),
            prototype_of: HashMap::new(),
            anchored: HashMap::new(),
            unanchored: HashSet::new(),
            rates: HashMap::new(),
            root_rate: 24.0,
            report: FlattenReport::default(),
        };
        flattener.root_rate = flattener.time_codes_per_second(root);
        // The layer metadata's asset paths are anchored to the root layer.
        let mut metadata = core::mem::take(&mut flattener.out.metadata);
        for entry in &mut metadata {
            if let FieldValue::Value(value) = &mut entry.value {
                if let Some(anchored) = flattener.anchor_value(value, root) {
                    *value = anchored;
                }
                flattener.note_assets("/", value, None);
            }
            flattener.report.preserved.metadata_fields += 1;
        }
        flattener.out.metadata = metadata;
        let prototypes = match requirements.instancing {
            Instancing::Preserve => flattener.find_prototypes(pseudo_root),
            Instancing::Expand => Vec::new(),
        };
        let mut root_children = Vec::new();
        for (source, prototype) in prototypes {
            flattener.copy_prototype(source, prototype);
            root_children.extend(flattener.store.paths().resolve(prototype).leaf());
        }
        for &child in self.children_of(pseudo_root).unwrap_or_default() {
            let name = flattener.store.paths().resolve(child).leaf();
            flattener.copy_prim(child, child, None);
            root_children.extend(name);
        }
        let mut pseudo_root_spec = PrimSpec::default().with_children(root_children);
        pseudo_root_spec.prim_order = root_order;
        flattener.out.insert_prim(pseudo_root, pseudo_root_spec);
        flattener.report.preserved.prims += 1;
        flattener.finish()
    }
}

struct Flattener<'a, 'r> {
    stage: &'a Stage,
    store: &'a mut dyn LayerStore,
    requirements: &'a FlattenRequirements<'r>,
    out: Layer,
    /// The prototype path of each instance key.
    prototypes: HashMap<InstanceKey, PathId>,
    /// The prototype each instance references.
    prototype_of: HashMap<PathId, PathId>,
    /// Each anchored asset path written, with the path it was authored as.
    anchored: HashMap<Arc<str>, Arc<str>>,
    /// The authored asset paths that could not be anchored.
    unanchored: HashSet<Arc<str>>,
    /// Each layer's time codes per second, as read.
    rates: HashMap<LayerId, f64>,
    /// The root layer's time codes per second.
    root_rate: f64,
    report: FlattenReport,
}

impl Flattener<'_, '_> {
    /// The flattened layer, or the refusal when a requirement is unmet.
    fn finish(self) -> Result<Flattened, FlattenError> {
        let requirements = self.requirements;
        let unmet: Vec<UnmetRequirement> = self
            .report
            .lost()
            .filter_map(|finding| {
                let FindingKind::Lost(loss) = finding.kind else {
                    return None;
                };
                let declared = loss.requirement().filter(|requirement| match requirement {
                    Requirement::ExactAnimation => requirements.exact_animation,
                    Requirement::AnchoredAssetPaths => {
                        matches!(requirements.asset_paths, AssetPaths::Anchored(_))
                    }
                    Requirement::NoLoss => true,
                });
                let requirement = declared.or(match requirements.losses {
                    LossPolicy::RefuseAny => Some(Requirement::NoLoss),
                    LossPolicy::RefuseRequired => None,
                })?;
                Some(UnmetRequirement {
                    requirement,
                    finding: finding.clone(),
                })
            })
            .collect();
        if unmet.is_empty() {
            Ok(Flattened {
                layer: self.out,
                report: self.report,
            })
        } else {
            Err(FlattenError::Refused(FlattenRefusal {
                unmet,
                report: self.report,
            }))
        }
    }

    fn display(&self, path: PathId) -> String {
        self.store.paths().display(path, self.store.tokens())
    }

    fn property_display(&self, prim: PathId, name: TokenId) -> String {
        PropertyPath::new(prim, name).display(self.store.paths(), self.store.tokens())
    }

    /// The layer and spec of `opinion`, for a finding.
    fn source(&self, opinion: &Opinion, property: bool) -> FindingSource {
        let spec = if property {
            opinion.key.spec_path.with_property(opinion.field)
        } else {
            opinion.key.spec_path.prim_spec()
        };
        FindingSource {
            layer: opinion.key.layer_id,
            spec: spec.display(self.store.tokens()),
        }
    }

    /// Records a finding at `path`.
    fn note(&mut self, path: String, kind: FindingKind, source: Option<FindingSource>) {
        self.report.findings.push(Finding {
            path: ObjectPath::from_composed(&path),
            kind,
            source,
        });
    }

    fn transformed(&mut self, path: String, t: Transformation, source: Option<FindingSource>) {
        self.note(path, FindingKind::Transformed(t), source);
    }

    fn lost(&mut self, path: String, loss: Loss, source: Option<FindingSource>) {
        self.note(path, FindingKind::Lost(loss), source);
    }

    /// A layer's time codes per second: its `timeCodesPerSecond`, else its
    /// `framesPerSecond`, else 24 (OpenUSD's `SdfLayer::GetTimeCodesPerSecond`).
    ///
    /// Spec: AOUSD Core §7.6.1 (layer metadata).
    fn time_codes_per_second(&mut self, layer: LayerId) -> f64 {
        if let Some(&rate) = self.rates.get(&layer) {
            return rate;
        }
        let tcps = self.store.tokens_mut().intern("timeCodesPerSecond");
        let fps = self.store.tokens_mut().intern("framesPerSecond");
        let read = |key| {
            let entry = self
                .store
                .layer(layer)?
                .metadata
                .iter()
                .find(|e| e.name == key)?;
            match &entry.value {
                FieldValue::Value(Value::Double(v)) => Some(*v),
                FieldValue::Value(Value::Float(v)) => Some(f64::from(*v)),
                FieldValue::Value(Value::Int(v)) => Some(f64::from(*v)),
                _ => None,
            }
        };
        let rate = read(tcps).or_else(|| read(fps)).unwrap_or(24.0);
        self.rates.insert(layer, rate);
        rate
    }

    /// Whether OpenUSD would rescale the times `layer` authors: its time
    /// codes per second differ from the root layer's.
    fn rescaled(&mut self, layer: LayerId) -> bool {
        self.time_codes_per_second(layer) != self.root_rate
    }

    /// Records that `opinion`'s default or metadata value holds `timecode`
    /// values the resolved value moved into stage time. Returns `true` when
    /// it holds none, so the value is written as authored.
    ///
    /// Spec: AOUSD Core §12.3.2.1.
    fn note_retimed(
        &mut self,
        path: &str,
        opinion: &Opinion,
        source: Option<&FindingSource>,
    ) -> bool {
        let Some(value) = opinion.value.default_value() else {
            return true;
        };
        if retime_value(value, opinion.layer_offset).is_none() {
            return true;
        }
        self.transformed(
            path.into(),
            Transformation::TimeCodesRetimed {
                offset: opinion.layer_offset,
            },
            source.cloned(),
        );
        false
    }

    /// `value` with its asset paths anchored to the layer `layer`, when the
    /// requirements anchor them; `None` when nothing changes. Records what
    /// was anchored and what could not be.
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths are anchored to the
    /// layer that authors them). OpenUSD: `SdfAnchorAssetPaths`, which its
    /// flatten applies to every value and metadatum.
    fn anchor_value(&mut self, value: &Value, layer: LayerId) -> Option<Value> {
        let AssetPaths::Anchored(resolver) = self.requirements.asset_paths else {
            return None;
        };
        let (anchored, unanchored) = (&mut self.anchored, &mut self.unanchored);
        map_leaves(value, &mut |leaf| {
            anchor_leaf(leaf, layer, resolver, anchored, unanchored)
        })
    }

    /// `opinions` with the asset paths of each anchored to the layer that
    /// authors it, when the requirements anchor them.
    fn anchor_opinions<'o>(&mut self, opinions: &'o [Opinion]) -> Cow<'o, [Opinion]> {
        let AssetPaths::Anchored(resolver) = self.requirements.asset_paths else {
            return Cow::Borrowed(opinions);
        };
        let (anchored, unanchored) = (&mut self.anchored, &mut self.unanchored);
        let mut out: Option<Vec<Opinion>> = None;
        for (i, opinion) in opinions.iter().enumerate() {
            let layer = opinion.key.layer_id;
            let mapped = map_opinion(opinion, &mut |leaf| {
                anchor_leaf(leaf, layer, resolver, anchored, unanchored)
            });
            match (mapped, &mut out) {
                (Some(mapped), Some(out)) => out.push(mapped),
                (Some(mapped), None) => {
                    let mut started = opinions[..i].to_vec();
                    started.push(mapped);
                    out = Some(started);
                }
                (None, Some(out)) => out.push(opinion.clone()),
                (None, None) => {}
            }
        }
        out.map_or(Cow::Borrowed(opinions), Cow::Owned)
    }

    /// Records each asset path in `value` as an external dependency, each
    /// anchored one as a transformation, and each that could not be
    /// anchored when the requirements anchor them as a loss.
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths are anchored to the
    /// layer that authors them).
    fn note_assets(&mut self, path: &str, value: &Value, source: Option<&FindingSource>) {
        let mut found = Vec::new();
        asset_paths(value, &mut found);
        let mut seen: Vec<Arc<str>> = Vec::new();
        for asset in found {
            if asset.is_empty() || seen.contains(&asset) {
                continue;
            }
            seen.push(asset.clone());
            if let Some(authored) = self.anchored.get(&asset) {
                let t = Transformation::AssetPathAnchored {
                    authored: String::from(&**authored),
                    anchored: String::from(&*asset),
                };
                self.transformed(path.into(), t, source.cloned());
            } else if self.unanchored.contains(&asset) {
                self.lost(path.into(), Loss::UnanchoredAssetPath, source.cloned());
            }
            self.note(
                path.into(),
                FindingKind::External(ExternalDependency::AssetPath(String::from(&*asset))),
                source.cloned(),
            );
        }
    }

    /// Groups the stage's instances by [`InstanceKey`], in traversal order,
    /// and names a prototype for each group: `/Flattened_Prototype_N`,
    /// skipping names the stage uses. Returns each prototype with the first
    /// instance of its group, whose descendants it copies.
    ///
    /// OpenUSD: `_GenerateFlattenedPrototypePath` in `pxr/usd/usd/stage.cpp`.
    fn find_prototypes(&mut self, pseudo_root: PathId) -> Vec<(PathId, PathId)> {
        let mut found = Vec::new();
        let mut next = 1_usize;
        let instances: Vec<PathId> = self
            .stage
            .traverse(pseudo_root)
            .filter(|prim| self.stage.instances.contains(prim))
            .collect();
        for instance in instances {
            let key = self.instance_key(instance);
            let prototype = if let Some(&prototype) = self.prototypes.get(&key) {
                prototype
            } else {
                let prototype = loop {
                    let name = self
                        .store
                        .tokens_mut()
                        .intern(format!("Flattened_Prototype_{next}"));
                    next += 1;
                    let path = self.store.paths_mut().intern(Path::root().join(&[name]));
                    if !self.stage.has_prim(path) {
                        break path;
                    }
                };
                self.prototypes.insert(key, prototype);
                found.push((instance, prototype));
                prototype
            };
            self.prototype_of.insert(instance, prototype);
        }
        found
    }

    /// The [`InstanceKey`] of `instance`: the strongest-first nodes of its
    /// graph whose arcs are authored at the instance, beneath nodes that are
    /// not, with its variant selections.
    ///
    /// OpenUSD: `PcpInstanceKey::_Collector`, which records each instanceable
    /// node that no instanceable node is above (`pxr/usd/pcp/instanceKey.cpp`).
    fn instance_key(&self, instance: PathId) -> InstanceKey {
        let depth = u16::try_from(self.store.paths().resolve(instance).depth()).unwrap_or(u16::MAX);
        let mut arcs = Vec::new();
        if let Some(graph) = self.stage.explain_prim_graph(instance) {
            for id in graph.strength_order() {
                let Some(node) = graph.node(id) else { continue };
                if id == NodeId::ROOT || node.namespace_depth() < depth {
                    continue;
                }
                let parent_instanceable = node
                    .parent()
                    .filter(|&parent| parent != NodeId::ROOT)
                    .and_then(|parent| graph.node(parent))
                    .is_some_and(|parent| parent.namespace_depth() >= depth);
                if parent_instanceable {
                    continue;
                }
                let offset = node.layer_offset();
                arcs.push((
                    node.arc_kind(),
                    node.layer_stack(),
                    node.site().clone(),
                    offset.offset.to_bits(),
                    offset.scale.to_bits(),
                ));
            }
        }
        let mut selections: Vec<(TokenId, TokenId)> = self
            .stage
            .variant_selections(instance, &*self.store)
            .into_iter()
            .collect();
        selections.sort_unstable();
        InstanceKey { arcs, selections }
    }

    /// The variant selections whose arcs are authored at `prim`, strongest
    /// first, as names.
    ///
    /// Spec: AOUSD Core §10.5 (variant selection).
    fn baked_selections(&self, prim: PathId) -> Vec<(String, String)> {
        let depth = u16::try_from(self.store.paths().resolve(prim).depth()).unwrap_or(u16::MAX);
        let tokens = self.store.tokens();
        let Some(graph) = self.stage.explain_prim_graph(prim) else {
            return Vec::new();
        };
        let mut selections = Vec::new();
        for id in graph.strength_order() {
            let Some(node) = graph.node(id) else { continue };
            if node.arc_kind() != ArcKind::Variants || node.namespace_depth() != depth {
                continue;
            }
            if let Some(SpecComponent::VariantSelection { set, variant }) =
                node.site().components().last().copied()
            {
                let selection = (
                    String::from(tokens.resolve(set)),
                    String::from(tokens.resolve(variant)),
                );
                if !selections.contains(&selection) {
                    selections.push(selection);
                }
            }
        }
        selections
    }

    fn child_path(&mut self, parent: PathId, name: TokenId) -> PathId {
        let path = self.store.paths().resolve(parent).join(&[name]);
        self.store.paths_mut().intern(path)
    }

    /// Writes `prototype` from the descendants of `source`, the first of its
    /// instances. The prototype prim itself is an `over` with no opinions,
    /// as OpenUSD writes it (`_CopyPrototypePrim`).
    fn copy_prototype(&mut self, source: PathId, prototype: PathId) {
        let remap = Remap {
            from: source,
            to: prototype,
        };
        let mut spec = PrimSpec::over();
        for &child in self.stage.children_of(source).unwrap_or_default() {
            let Some(name) = self.store.paths().resolve(child).leaf() else {
                continue;
            };
            let dest = self.child_path(prototype, name);
            self.copy_prim(child, dest, Some(remap));
            spec.authored_children.push(name);
        }
        self.out.insert_prim(prototype, spec);
        self.report.preserved.prims += 1;
    }

    /// Writes the composed prim `source` as the spec at `dest`, with its
    /// descendants, or with an internal reference to its prototype for an
    /// instance.
    ///
    /// OpenUSD: `_CopyPrim` in `pxr/usd/usd/stage.cpp`.
    fn copy_prim(&mut self, source: PathId, dest: PathId, remap: Option<Remap>) {
        let store: &dyn LayerStore = &*self.store;
        let mut spec = PrimSpec {
            specifier: self.stage.resolve_specifier(source, store),
            type_name: self.stage.resolve_type_name(source, store),
            ..PrimSpec::default()
        };
        self.copy_prim_metadata(source, &mut spec, remap);
        let selections = self.baked_selections(source);
        if !selections.is_empty() {
            let path = self.display(source);
            self.transformed(
                path,
                Transformation::VariantSelectionsBaked { selections },
                None,
            );
        }

        if let Some(&prototype) = self.prototype_of.get(&source) {
            // Spec: AOUSD Core §11.3.3 (the instance shares its prototype).
            spec.references.explicit = Some(alloc::vec![Reference::new(self.out.id, prototype)]);
            let (path, prototype) = (self.display(source), self.display(prototype));
            self.transformed(path, Transformation::InstanceShared { prototype }, None);
        } else {
            if self.stage.instances.contains(&source) {
                let path = self.display(source);
                self.transformed(path, Transformation::InstanceExpanded, None);
            }
            for &child in self.stage.children_of(source).unwrap_or_default() {
                let Some(name) = self.store.paths().resolve(child).leaf() else {
                    continue;
                };
                let child_dest = self.child_path(dest, name);
                self.copy_prim(child, child_dest, remap);
                spec.authored_children.push(name);
            }
        }

        for name in self.stage.authored_property_names(source, &*self.store) {
            if let Some(property) = self.copy_property(source, name, remap) {
                spec.properties.push(PropertyEntry {
                    name,
                    spec: property,
                });
                self.report.preserved.properties += 1;
            }
        }
        self.out.insert_prim(dest, spec);
        self.report.preserved.prims += 1;
    }

    /// The composed prim metadata, the dedicated members included.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution).
    fn copy_prim_metadata(&mut self, prim: PathId, spec: &mut PrimSpec, remap: Option<Remap>) {
        let stage = self.stage;
        let Some(index) = stage.prims.get(&prim) else {
            return;
        };
        {
            let store: &dyn LayerStore = &*self.store;
            // The strongest source spec that authors each dedicated member.
            for source in &index.sources {
                let Some(authored) = store.layer(source.layer_id).and_then(|layer| {
                    layer.source_prim_spec(source.lookup_path, &source.spec_path, store.paths())
                }) else {
                    continue;
                };
                spec.active = spec.active.or(authored.active);
                spec.instanceable = spec.instanceable.or(authored.instanceable);
                if spec.prim_order.is_none() {
                    spec.prim_order.clone_from(&authored.prim_order);
                }
            }
            spec.property_order = stage.resolve_property_order(prim, store);
        }

        let mut keys: Vec<TokenId> = index
            .opinions_by_field
            .keys()
            .filter_map(|key| match key {
                FieldKey::Metadata(key) => Some(*key),
                FieldKey::Property(_) => None,
            })
            .collect();
        {
            let tokens = self.store.tokens();
            keys.sort_unstable_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
        }
        let path = self.display(prim);
        let mut clips_noted = false;
        for key in keys {
            let opinions = stage.explain_field(prim, key).unwrap_or_default();
            let strongest = opinions.first();
            let source = strongest.map(|opinion| self.source(opinion, false));
            if CLIP_FIELDS.contains(&self.store.tokens().resolve(key)) {
                // Spec: AOUSD Core §12.3.4 (value clips) is not composed.
                if !clips_noted {
                    self.lost(path.clone(), Loss::ValueClips, source);
                    clips_noted = true;
                }
                continue;
            }
            let authored = strongest.and_then(|opinion| opinion.value.as_field());
            let resolved = if authored.is_some_and(FieldValue::is_list_op) {
                stage.resolve_value(prim, key)
            } else {
                let anchored = self.anchor_opinions(opinions);
                stage.resolve_default(key, &anchored, index.property_type_for(&key), None)
            };
            let Some(resolved) = resolved else {
                continue;
            };
            if authored.is_some_and(|value| !is_explicit(value)) {
                let field = String::from(self.store.tokens().resolve(key));
                self.transformed(
                    path.clone(),
                    Transformation::ListOpMadeExplicit { field },
                    source.clone(),
                );
            } else if strongest
                .is_none_or(|opinion| self.note_retimed(&path, opinion, source.as_ref()))
            {
                self.report.preserved.metadata_fields += 1;
            }
            let value = self.field_value(resolved.value, authored, remap);
            if let FieldValue::Value(value) = &value {
                self.note_assets(&path, value, source.as_ref());
            }
            spec.fields.push(FieldEntry { name: key, value });
        }
    }

    /// Writes the composed property `name` of `prim`, or records why it
    /// cannot be.
    ///
    /// OpenUSD: `_CopyProperty` in `pxr/usd/usd/stage.cpp`.
    fn copy_property(
        &mut self,
        prim: PathId,
        name: TokenId,
        remap: Option<Remap>,
    ) -> Option<PropertySpec> {
        let stage = self.stage;
        let declaration = stage.resolve_property_declaration(prim, name)?;
        let opinions = stage.prims.get(&prim)?.property_opinions(name)?;
        let property = PropertyPath::new(prim, name);
        let path = self.property_display(prim, name);
        let mut spec = PropertySpec::of_kind(declaration.kind);
        self.declare(prim, name, &declaration, opinions, &mut spec, &path);
        spec.metadata = self.property_metadata(name, opinions, remap, &path);

        let targets = stage
            .resolve_target_list_path(property)
            .map(|resolved| resolved.value)
            .unwrap_or_default();
        if !targets.is_empty() {
            let strongest = opinions.iter().find(|op| op.value.targets().is_some());
            if strongest
                .and_then(|op| op.value.targets())
                .is_some_and(|list| list.explicit.is_some())
            {
                self.report.preserved.targets += targets.len();
            } else {
                let field = match declaration.kind {
                    PropertyKind::Relationship => "targetPaths",
                    PropertyKind::Attribute => "connectionPaths",
                };
                let source = strongest.map(|op| self.source(op, true));
                self.transformed(
                    path.clone(),
                    Transformation::ListOpMadeExplicit {
                        field: field.into(),
                    },
                    source,
                );
            }
            let targets = targets
                .into_iter()
                .map(|target| self.remap_target(target, remap))
                .collect();
            spec.targets = Some(ListOp::explicit(targets));
        }
        if declaration.kind == PropertyKind::Relationship {
            return Some(spec);
        }

        let Some(property_type) = declaration.type_name else {
            // OpenUSD omits it with a warning.
            let source = opinions.first().map(|op| self.source(op, true));
            self.lost(path, Loss::UntypedAttribute, source);
            return None;
        };
        spec.type_name = Some(property_type);
        let anchored = self.anchor_opinions(opinions);
        let opinions: &[Opinion] = &anchored;

        // Spec: AOUSD Core §12.3.2 (per opinion, time samples, then a
        // spline, then the default).
        let value_source = opinions.iter().position(|opinion| {
            opinion.value.time_samples().is_some()
                || opinion.value.spline().is_some()
                || opinion.value.default_value().is_some()
        });
        if let Some(position) = value_source {
            let source = &opinions[position];
            let finding_source = self.source(source, true);
            let rescaled = opinions[position..]
                .iter()
                .filter(|op| op.value.time_samples().is_some() || op.value.spline().is_some())
                .find(|op| self.rescaled(op.key.layer_id));
            if let Some(rescaled) = rescaled {
                // Spec: AOUSD Core §12.3.2.1 (layer time is stage time
                // through each layer's offset); OpenUSD also scales by
                // the ratio of the layers' `timeCodesPerSecond`
                // (`PcpLayerStack`), which composition here does not.
                let source = self.source(rescaled, true);
                self.lost(path.clone(), Loss::TimeCodesPerSecond, Some(source));
            } else if source.value.time_samples().is_none()
                && let Some(spline) = source.value.spline()
            {
                let offset = source.layer_offset;
                if offset.is_identity() {
                    spec.spline = Some(spline.clone());
                    self.report.preserved.splines += 1;
                } else if let Some(retimed) = spline.retimed(offset) {
                    // Spec: AOUSD Core §12.3.2.1: the spline in stage time.
                    spec.spline = Some(retimed);
                    let t = Transformation::SplineRetimed { offset };
                    self.transformed(path.clone(), t, Some(finding_source));
                } else {
                    self.lost(path.clone(), Loss::RetimedSpline, Some(finding_source));
                }
            } else if let Some(samples) =
                composed_samples(&opinions[position..], spec.type_name.as_ref())
            {
                spec.time_samples =
                    Some(self.stage_samples(&path, source, samples, &finding_source));
            }
        }

        // Spec: AOUSD Core §12.3.1 (the default), §12.3.6 (a block).
        // OpenUSD writes the default whenever one is authored.
        if let Some(authored) = opinions
            .iter()
            .find(|opinion| opinion.value.default_value().is_some())
        {
            let index = stage.prims.get(&prim)?;
            let resolved =
                stage.resolve_default(name, opinions, index.property_type_for(&name), None);
            let value = match resolved {
                Some(resolved) => match resolved.value {
                    ResolvedValue::Scalar(value) => value,
                    ResolvedValue::Dictionary(entries) => Value::Dictionary(entries),
                    _ => Value::Blocked,
                },
                None => Value::Blocked,
            };
            let source = self.source(authored, true);
            // The resolved value is in stage time already.
            if self.note_retimed(&path, authored, Some(&source)) {
                self.report.preserved.defaults += 1;
            }
            self.note_assets(&path, &value, Some(&source));
            spec.default = Some(value);
        }
        Some(spec)
    }

    /// Writes the property's `custom` and variability as OpenUSD's flatten
    /// writes them: for a property the prim's schema defines, not custom
    /// and, for an attribute, the schema's variability; otherwise the
    /// weakest opinion's `custom` (`_GetPropCustomImpl` in
    /// `pxr/usd/usd/stage.cpp`) and the composed variability.
    ///
    /// Spec: AOUSD Core §12.2.3 (variability), §12.2.4 (`custom`), §13.3
    /// (schema properties).
    fn declare(
        &mut self,
        prim: PathId,
        name: TokenId,
        declaration: &super::PropertyDeclaration,
        opinions: &[Opinion],
        spec: &mut PropertySpec,
        path: &str,
    ) {
        let source = opinions.first().map(|op| self.source(op, true));
        if let Some(variability) = self.schema_variability(prim, name) {
            spec.custom = false;
            spec.variability = match declaration.kind {
                PropertyKind::Attribute => variability,
                PropertyKind::Relationship => declaration.variability,
            };
            if spec.custom != declaration.custom || spec.variability != declaration.variability {
                let t = Transformation::DefinedBySchema {
                    variability: spec.variability,
                };
                self.transformed(path.into(), t, source);
            }
            return;
        }
        spec.custom = opinions
            .iter()
            .rev()
            .find_map(|op| op.value.as_property())
            .is_some_and(|weakest| weakest.custom);
        spec.variability = declaration.variability;
        if spec.custom != declaration.custom {
            let t = Transformation::CustomFromWeakestOpinion {
                custom: spec.custom,
            };
            self.transformed(path.into(), t, source);
        }
    }

    /// The variability the prim's schemas declare for `name`, when one
    /// defines it ([`Stage::property_definition`]).
    ///
    /// Spec: AOUSD Core §13.3.2.3 (the prim definition).
    fn schema_variability(&self, prim: PathId, name: TokenId) -> Option<Variability> {
        self.stage
            .property_definition(prim, name, &*self.store)
            .map(|definition| definition.variability)
    }

    /// Records how the composed samples of an attribute whose strongest
    /// value source is `source` were written: exactly, retimed through
    /// `source`'s layer offset, or with sparse array edits baked.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (a layer's time `t` is stage time
    /// `t * scale + offset`).
    fn stage_samples(
        &mut self,
        path: &str,
        source: &Opinion,
        composed: ComposedSamples,
        finding_source: &FindingSource,
    ) -> Vec<(f64, Value)> {
        let offset = source.layer_offset;
        if composed.baked {
            self.transformed(
                path.into(),
                Transformation::ArrayEditsBaked,
                Some(finding_source.clone()),
            );
        }
        if !offset.is_identity() {
            self.transformed(
                path.into(),
                Transformation::SamplesRetimed { offset },
                Some(finding_source.clone()),
            );
            if composed.retimed_timecodes {
                self.transformed(
                    path.into(),
                    Transformation::TimeCodesRetimed { offset },
                    Some(finding_source.clone()),
                );
            }
        } else if !composed.baked {
            self.report.preserved.time_samples += composed.samples.len();
        }
        for (_, value) in &composed.samples {
            self.note_assets(path, value, Some(finding_source));
        }
        composed.samples
    }

    /// The composed metadata of a property.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution).
    fn property_metadata(
        &mut self,
        name: TokenId,
        opinions: &[Opinion],
        remap: Option<Remap>,
        path: &str,
    ) -> Vec<FieldEntry> {
        let mut keys: Vec<TokenId> = Vec::new();
        for opinion in opinions {
            if let Some(spec) = opinion.value.as_property() {
                for entry in &spec.metadata {
                    if !keys.contains(&entry.name) {
                        keys.push(entry.name);
                    }
                }
            }
        }
        {
            let tokens = self.store.tokens();
            keys.sort_unstable_by(|a, b| tokens.resolve(*a).cmp(tokens.resolve(*b)));
        }
        let anchored = self.anchor_opinions(opinions);
        let mut out = Vec::new();
        for key in keys {
            let Some(resolved) = self
                .stage
                .resolve_property_metadata_over(&anchored, None, name, key)
            else {
                continue;
            };
            let strongest = opinions
                .iter()
                .find_map(|opinion| Some((opinion, opinion.value.as_property()?.metadata(key)?)));
            let source = strongest.map(|(opinion, _)| self.source(opinion, true));
            let authored = strongest.map(|(_, value)| value);
            let retimed = strongest.is_some_and(|(opinion, value)| {
                matches!(value, FieldValue::Value(value) if retime_value(value, opinion.layer_offset).is_some())
            });
            if authored.is_some_and(|value| !is_explicit(value)) {
                let field = String::from(self.store.tokens().resolve(key));
                self.transformed(
                    path.into(),
                    Transformation::ListOpMadeExplicit { field },
                    source.clone(),
                );
            } else if let (true, Some((opinion, _))) = (retimed, strongest) {
                self.transformed(
                    path.into(),
                    Transformation::TimeCodesRetimed {
                        offset: opinion.layer_offset,
                    },
                    source.clone(),
                );
            } else {
                self.report.preserved.metadata_fields += 1;
            }
            let value = self.field_value(resolved.value, authored, remap);
            if let FieldValue::Value(value) = &value {
                self.note_assets(path, value, source.as_ref());
            }
            out.push(FieldEntry { name: key, value });
        }
        out
    }

    /// A resolved metadata value as the explicit field that authors it.
    fn field_value(
        &mut self,
        resolved: ResolvedValue,
        strongest: Option<&FieldValue>,
        remap: Option<Remap>,
    ) -> FieldValue {
        fn explicit<T>(items: Vec<T>) -> ListOp<T> {
            ListOp::explicit(items)
        }
        match resolved {
            ResolvedValue::Scalar(value) => FieldValue::Value(value),
            ResolvedValue::Dictionary(entries) => FieldValue::Value(Value::Dictionary(entries)),
            ResolvedValue::TokenList(items) => FieldValue::TokenListOp(explicit(items)),
            ResolvedValue::PathList(items) => FieldValue::PathListOp(explicit(
                items
                    .into_iter()
                    .map(|target| self.remap_target(target, remap))
                    .collect(),
            )),
            ResolvedValue::ValueList(items) => {
                let items = items.into_iter();
                match strongest {
                    Some(FieldValue::IntListOp(_)) => FieldValue::IntListOp(explicit(
                        items
                            .filter_map(|v| match v {
                                Value::Int(v) => Some(v),
                                _ => None,
                            })
                            .collect(),
                    )),
                    Some(FieldValue::UIntListOp(_)) => FieldValue::UIntListOp(explicit(
                        items
                            .filter_map(|v| match v {
                                Value::UInt(v) => Some(v),
                                _ => None,
                            })
                            .collect(),
                    )),
                    Some(FieldValue::Int64ListOp(_)) => FieldValue::Int64ListOp(explicit(
                        items
                            .filter_map(|v| match v {
                                Value::Int64(v) => Some(v),
                                _ => None,
                            })
                            .collect(),
                    )),
                    Some(FieldValue::UInt64ListOp(_)) => FieldValue::UInt64ListOp(explicit(
                        items
                            .filter_map(|v| match v {
                                Value::UInt64(v) => Some(v),
                                _ => None,
                            })
                            .collect(),
                    )),
                    _ => FieldValue::StringListOp(explicit(
                        items
                            .filter_map(|v| match v {
                                Value::String(v) => Some(v),
                                _ => None,
                            })
                            .collect(),
                    )),
                }
            }
        }
    }

    /// Maps a target beneath the instance a prototype copies onto the
    /// prototype, as OpenUSD's flatten does (`_RemapTargetPaths`, which
    /// replaces the prefix whether or not the prim is copied yet); other
    /// targets stay in stage namespace.
    fn remap_target(&mut self, target: TargetPath, remap: Option<Remap>) -> TargetPath {
        let Some(remap) = remap else {
            return target;
        };
        let paths = self.store.paths();
        let from = paths.resolve(remap.from);
        let Some(rest) = paths.resolve(target.prim_path()).strip_prefix(from) else {
            return target;
        };
        let mapped = paths.resolve(remap.to).join(rest);
        let prim = self.store.paths_mut().intern(mapped);
        match target {
            TargetPath::Prim(_) => TargetPath::Prim(prim),
            TargetPath::Property(property) => {
                TargetPath::Property(PropertyPath::new(prim, property.property()))
            }
        }
    }
}

/// The time samples OpenUSD's flatten writes for an attribute.
struct ComposedSamples {
    /// The samples, in stage time.
    samples: Vec<(f64, Value)>,
    /// Whether sparse array edits were composed into dense arrays.
    baked: bool,
    /// Whether `timecode` values were moved into stage time.
    retimed_timecodes: bool,
}

/// The time samples of an attribute whose strongest value source is the
/// first of `opinions`, composed as OpenUSD's flatten composes them
/// (`_TimeSampleMapResolver` in `pxr/usd/usd/stage.cpp`): each opinion's
/// samples in stage time; a sample of sparse array edits composes over the
/// weaker samples held at its time, and a weaker default or sample over
/// the edits of a stronger default, until nothing left can compose; the
/// edits that remain apply to an empty array. `None` when no samples
/// result.
///
/// Spec: AOUSD Core §12.3.2 (time samples), §12.3.2.1 (layer offsets),
/// §12.3.6 (blocks); sparse array edits are an OpenUSD extension
/// (`VtArrayEdit`).
fn composed_samples(
    opinions: &[Opinion],
    property_type: Option<&PropertyType>,
) -> Option<ComposedSamples> {
    let mut partial: Vec<(f64, Value)> = Vec::new();
    let mut partial_default: Option<Value> = None;
    let mut baked = false;
    let mut retimed_timecodes = false;
    for opinion in opinions {
        let can_compose = if let Some(samples) = opinion.value.time_samples() {
            let offset = opinion.layer_offset;
            let weaker: Vec<(f64, Value)> = samples
                .iter()
                .map(|(time, value)| {
                    let retimed = retime_value(value, offset);
                    retimed_timecodes |= retimed.is_some();
                    (
                        to_stage_time(offset, *time),
                        retimed.unwrap_or_else(|| value.clone()),
                    )
                })
                .collect();
            if let Some(default) = partial_default.take() {
                partial = weaker
                    .into_iter()
                    .map(|(time, value)| {
                        let composed = compose_over(&default, &value, property_type);
                        baked |= composed.is_some();
                        (time, composed.unwrap_or_else(|| default.clone()))
                    })
                    .collect();
            } else if partial.is_empty() {
                partial = weaker;
            } else {
                partial = compose_series(&partial, &weaker, property_type, &mut baked);
            }
            can_compose_over(&partial, partial_default.as_ref())
        } else if opinion.value.spline().is_some() {
            break;
        } else if let Some(default) = opinion.value.default_value() {
            if *default == Value::Blocked {
                break;
            }
            // A default's `timecode` values are in its layer's time too
            // (`_FieldValueToStageXf` maps it before it composes).
            let retimed = retime_value(default, opinion.layer_offset);
            retimed_timecodes |= retimed.is_some();
            let default = retimed.as_ref().unwrap_or(default);
            if partial.is_empty() {
                partial_default = Some(match partial_default.take() {
                    Some(stronger) => compose_over(&stronger, default, property_type)
                        .inspect(|_| baked = true)
                        .unwrap_or(stronger),
                    None => default.clone(),
                });
            } else {
                for (_, sample) in &mut partial {
                    if let Some(composed) = compose_over(sample, default, property_type) {
                        baked = true;
                        *sample = composed;
                    }
                }
            }
            can_compose_over(&partial, partial_default.as_ref())
        } else {
            true
        };
        if !can_compose {
            break;
        }
    }
    if partial.is_empty() {
        return None;
    }
    // What still composes composes over the empty array.
    for (_, sample) in &mut partial {
        if let Value::ArrayEdit(edit) = sample {
            *sample = Value::Array(crate::array_edit::apply_to_array(edit, &[], property_type));
            baked = true;
        }
    }
    Some(ComposedSamples {
        samples: partial,
        baked,
        retimed_timecodes,
    })
}

/// Whether anything in the partial result can compose over weaker
/// opinions: a sparse array edit.
fn can_compose_over(partial: &[(f64, Value)], default: Option<&Value>) -> bool {
    if partial.is_empty() {
        default.is_some_and(|value| matches!(value, Value::ArrayEdit(_)))
    } else {
        partial
            .iter()
            .any(|(_, value)| matches!(value, Value::ArrayEdit(_)))
    }
}

/// `stronger` composed over `weaker`: a sparse array edit over a dense
/// array or another edit; `None` when `stronger` does not compose.
fn compose_over(
    stronger: &Value,
    weaker: &Value,
    property_type: Option<&PropertyType>,
) -> Option<Value> {
    let Value::ArrayEdit(edit) = stronger else {
        return None;
    };
    match weaker {
        Value::ArrayEdit(weaker) => Some(Value::ArrayEdit(edit.compose_over(weaker))),
        Value::Array(items) => Some(Value::Array(crate::array_edit::apply_to_array(
            edit,
            items,
            property_type,
        ))),
        _ => None,
    }
}

/// Composes a stronger sample series over a weaker one, as
/// `SdfComposeTimeSampleSeries` does (`pxr/usd/sdf/composeTimeSampleSeries.h`):
/// at each time either series has a sample, the stronger sample held there
/// composes over the weaker one held there; a weaker sample under a
/// stronger one that does not compose is hidden. Times within `1e-6` are
/// the same time.
fn compose_series(
    strong: &[(f64, Value)],
    weak: &[(f64, Value)],
    property_type: Option<&PropertyType>,
    baked: &mut bool,
) -> Vec<(f64, Value)> {
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-6;
    // The sample held at `time`: the one at or before it, else the first.
    let held = |series: &[(f64, Value)], next: usize, time: f64| -> usize {
        if next == series.len() || (!same(series[next].0, time) && next != 0) {
            next - 1
        } else {
            next
        }
    };
    let mut out = Vec::with_capacity(strong.len() + weak.len());
    let (mut s, mut w) = (0, 0);
    while s < strong.len() || w < weak.len() {
        let strong_time = strong.get(s).map_or(f64::INFINITY, |(t, _)| *t);
        let weak_time = weak.get(w).map_or(f64::INFINITY, |(t, _)| *t);
        if strong_time <= weak_time {
            let under = &weak[held(weak, w, strong_time)].1;
            let value = &strong[s].1;
            let composed = compose_over(value, under, property_type);
            *baked |= composed.is_some();
            out.push((strong_time, composed.unwrap_or_else(|| value.clone())));
        } else {
            let over = &strong[held(strong, s, weak_time)].1;
            if let Some(composed) = compose_over(over, &weak[w].1, property_type) {
                *baked = true;
                out.push((weak_time, composed));
            }
        }
        if s == strong.len() {
            w += 1;
        } else if w == weak.len() {
            s += 1;
        } else if same(strong_time, weak_time) {
            s += 1;
            w += 1;
        } else if strong_time < weak_time {
            s += 1;
        } else {
            w += 1;
        }
    }
    out
}

/// Whether an authored field is written as authored: a plain value, or a
/// list op that is already an explicit list.
fn is_explicit(value: &FieldValue) -> bool {
    match value {
        FieldValue::Value(_) => true,
        FieldValue::TokenListOp(list) => list.explicit.is_some(),
        FieldValue::PathListOp(list) => list.explicit.is_some(),
        FieldValue::StringListOp(list) => list.explicit.is_some(),
        FieldValue::IntListOp(list) => list.explicit.is_some(),
        FieldValue::UIntListOp(list) => list.explicit.is_some(),
        FieldValue::Int64ListOp(list) => list.explicit.is_some(),
        FieldValue::UInt64ListOp(list) => list.explicit.is_some(),
    }
}

/// Collects the asset paths in `value`, alone, in arrays and in
/// dictionaries.
fn asset_paths(value: &Value, out: &mut Vec<Arc<str>>) {
    match value {
        Value::Asset(path) => out.push(path.clone()),
        Value::Array(items) => items.iter().for_each(|item| asset_paths(item, out)),
        Value::Dictionary(entries) => entries.iter().for_each(|(_, v)| asset_paths(v, out)),
        _ => {}
    }
}

/// Anchors one leaf value, an asset path, to `layer` through `resolver`,
/// recording the result.
fn anchor_leaf(
    leaf: &Value,
    layer: LayerId,
    resolver: &dyn AssetResolver,
    anchored: &mut HashMap<Arc<str>, Arc<str>>,
    unanchored: &mut HashSet<Arc<str>>,
) -> Option<Value> {
    let Value::Asset(path) = leaf else {
        return None;
    };
    if path.is_empty() {
        return None;
    }
    match resolver.anchor_asset_path(path, layer) {
        Some(to) if *to != **path => {
            let to: Arc<str> = Arc::from(to);
            anchored.insert(to.clone(), path.clone());
            Some(Value::Asset(to))
        }
        Some(_) => None,
        None => {
            unanchored.insert(path.clone());
            None
        }
    }
}

#[cfg(test)]
mod tests;
