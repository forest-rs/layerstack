// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prepared runtime value clips with explicit resident-asset lookup.
//!
//! Core preparation owns metadata composition, schedules and immutable raw
//! property snapshots. Hosts own loading and availability; numeric queries do
//! no I/O. Clip layers are raw sources: their arcs, variants and metadata are
//! not composed. Default-time queries do not read clips. AOUSD Core §12.3;
//! OpenUSD 26.08 `usd/clipSetDefinition.cpp`, `clipCache.cpp`, `clipSet.cpp`.

mod eval;
mod template;
use crate::prim_index::PrimIndex;
use crate::{
    AssetAvailability, FieldValue, HashMap, HashSet, InterpolationType, LayerId, LayerOffset,
    LayerStore, ListOp, Opinion, OpinionKey, OpinionValue, Path, PathId, PropertyPath,
    PropertySpec, PropertyType, SpecPath, TokenId, Value, Variability,
};
use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
pub use eval::ClipEvalError;
use eval::PreparedClipProperty;

/// Why the host should resolve this clip-related asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClipAssetRole {
    /// An explicitly listed value-clip layer.
    Value,
    /// An explicitly authored manifest layer.
    Manifest,
    /// One filename derived from a template's authored time range.
    TemplateCandidate,
}
/// Why an asset is unavailable to preparation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClipAssetUnavailable {
    /// The host confirmed that it does not exist.
    Missing,
    /// No host resolution result has been supplied.
    Unresolved,
    /// A binding exists, but its layer is not resident in the store.
    NotResident(LayerId),
}
/// Resident availability, without filesystem probing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClipAssetStatus {
    /// This layer was available to preparation.
    Loaded(LayerId),
    /// Host action is needed, or the asset was confirmed missing.
    Unavailable(ClipAssetUnavailable),
}
/// An inspectable clip asset use, anchored to the metadata's authoring layer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClipAssetRequest {
    /// Composed prim that owns this independent clip definition.
    pub owner: PathId,
    /// Clip-set name within that prim.
    pub set_name: Arc<str>,
    /// Metadata asset anchor layer.
    pub anchor: LayerId,
    /// Variant-qualified metadata source, retained for navigation.
    pub source: SpecPath,
    /// Authored or template-derived identifier, before host anchoring.
    pub identifier: Arc<str>,
    /// Value layer, manifest, or template candidate.
    pub role: ClipAssetRole,
    /// Availability observed when the stage was prepared.
    pub status: ClipAssetStatus,
}
/// A recoverable preparation limitation or malformed definition.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClipIssueKind {
    /// Missing or incompatible clip metadata.
    InvalidMetadata(&'static str),
    /// Invalid, excessive or nonfinite template range/pattern.
    InvalidTemplate,
    /// At least one candidate has not been resolved; no partial schedule is used.
    UnresolvedTemplate,
    /// A clip spline uses unsupported looping or regressive tangents.
    UnsupportedSpline,
    /// The evaluator rejected a malformed sample schedule.
    InvalidSchedule,
}
/// Structured preparation diagnostic; other independent sets remain usable.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClipIssue {
    /// Composed owner of the clip set.
    pub owner: PathId,
    /// Clip-set name.
    pub set_name: Arc<str>,
    /// Asset anchor layer.
    pub anchor: LayerId,
    /// Metadata source spec.
    pub source: SpecPath,
    /// Machine-readable limitation or violated invariant.
    pub kind: ClipIssueKind,
}
/// One fully projected discrete clip source and its per-sample asset anchors.
pub(crate) struct FlattenClip {
    pub(crate) opinion: Opinion,
    pub(crate) anchors: Vec<Option<LayerId>>,
}
/// Actual source of one bracketing clip sample.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipSampleSource {
    /// Raw clip layer, or manifest layer for its fallback; absent for a gap or
    /// an automatically generated manifest.
    pub layer: Option<LayerId>,
    /// Property path in the raw layer's namespace.
    pub spec_path: SpecPath,
    /// Sample position on the composed stage timeline.
    pub stage_time: f64,
    /// Mapped time on the clip's internal timeline.
    pub clip_time: f64,
    /// Index in the prepared value-clip array.
    pub clip_index: usize,
}
/// Manifest-selected runtime value representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipValueMode {
    /// Values come from discrete clip time samples.
    TimeSamples,
    /// Values come from native clip splines; no discrete sample inventory exists.
    Spline,
}
/// Clip resolution evidence, owned independently of query scratch storage.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipValueSource {
    /// Representation selected by the manifest declaration.
    pub mode: ClipValueMode,
    /// Clip-set identity within its owner.
    pub set_name: Arc<str>,
    /// Composed metadata owner.
    pub owner: PathId,
    /// Metadata's asset anchor; distinct from the actual value layer.
    pub anchor_layer: LayerId,
    /// Lower bracketing source.
    pub lower: ClipSampleSource,
    /// Upper bracketing source.
    pub upper: ClipSampleSource,
}
#[derive(Clone, Debug, Default)]
struct RawLayer {
    properties: HashMap<PathId, BTreeMap<TokenId, Arc<PropertySpec>>>,
}
impl RawLayer {
    fn capture(store: &dyn LayerStore, id: LayerId) -> Option<Self> {
        let layer = store.layer(id)?;
        Some(Self {
            properties: layer
                .prims
                .iter()
                .filter(|(_, spec)| spec.outer_variant_sites.is_empty())
                .map(|(&path, spec)| {
                    let attributes = spec
                        .properties
                        .iter()
                        .filter(|p| p.spec.kind == crate::PropertyKind::Attribute)
                        .map(|p| (p.name, p.spec.clone()))
                        .collect();
                    (path, attributes)
                })
                .collect(),
        })
    }
    fn at(&self, path: PathId) -> Option<&BTreeMap<TokenId, Arc<PropertySpec>>> {
        self.properties.get(&path)
    }
}
#[derive(Clone, Debug)]
struct Definition {
    owner: PathId,
    name: Arc<str>,
    anchor: OpinionKey,
    offset: LayerOffset,
    dictionary: Vec<(Arc<str>, Value)>,
    order: usize,
    sources: Vec<(LayerId, PathId)>,
}
#[derive(Clone, Debug)]
struct ReadySet {
    definition: Definition,
    clip_root: Path,
    clips: Vec<Option<(LayerId, Arc<RawLayer>)>>,
    manifest: Option<(LayerId, Arc<RawLayer>)>,
    active: Vec<(f64, usize)>,
    times: Vec<(f64, f64)>,
    interpolate_missing: bool,
}
#[derive(Clone, Debug)]
struct ClipProperty {
    owner: PathId,
    set_name: Arc<str>,
    key: OpinionKey,
    spec: SpecPath,
    clip_layers: Vec<Option<LayerId>>,
    manifest_layer: Option<LayerId>,
    has_sparse_samples: bool,
    property_type: Option<PropertyType>,
    evaluator: Arc<PreparedClipProperty>,
}
/// Immutable prepared state, kept private to stage integration.
#[derive(Clone, Debug, Default)]
pub(crate) struct Catalog {
    properties: HashMap<(PathId, TokenId), Vec<ClipProperty>>,
    sites: HashMap<PathId, HashSet<(LayerId, PathId)>>,
    bearing: HashSet<PathId>,
    loaded: HashSet<LayerId>,
    requests: Vec<ClipAssetRequest>,
    issues: Vec<ClipIssue>,
}
impl Catalog {
    pub(crate) fn prepare(store: &mut dyn LayerStore, prims: &HashMap<PathId, PrimIndex>) -> Self {
        let mut out = Self::default();
        let Some(clips_token) = store.tokens().lookup("clips") else {
            return out;
        };
        let mut owners: Vec<_> = prims.keys().copied().collect();
        owners.sort_by(|a, b| {
            store
                .paths()
                .resolve(*a)
                .cmp_with_tokens(store.paths().resolve(*b), store.tokens())
        });
        let mut raw = HashMap::new();
        let mut prepared = HashMap::new();
        for &owner in &owners {
            let index = &prims[&owner];
            if index.metadata_opinions(clips_token).is_none() {
                continue;
            }
            out.bearing.insert(owner);
            for source in index
                .metadata_opinions(clips_token)
                .into_iter()
                .flatten()
                .chain(
                    store
                        .tokens()
                        .lookup("clipSets")
                        .and_then(|t| index.metadata_opinions(t))
                        .into_iter()
                        .flatten(),
                )
            {
                out.sites
                    .entry(owner)
                    .or_default()
                    .insert((source.key.layer_id, source.key.lookup_path));
            }
            let definitions = definitions(owner, index, store, &mut out.issues);
            for definition in definitions {
                out.sites
                    .entry(owner)
                    .or_default()
                    .extend(definition.sources.iter().copied());
                if let Some(set) = out.prepare_set(store, definition, &mut raw) {
                    prepared.entry(owner).or_insert_with(Vec::new).push(set);
                }
            }
        }
        for &prim in &owners {
            let index = &prims[&prim];
            let mut ancestor = Some(prim);
            while let Some(owner) = ancestor {
                if let Some(sets) = prepared.get(&owner) {
                    for set in sets {
                        out.prepare_properties(store, prim, index, &prims[&owner], set);
                    }
                }
                // Metadata dependencies also retain definitions that currently
                // fail preparation, so loading/fixing them can invalidate users.
                if out.bearing.contains(&owner) {
                    out.bearing.insert(prim);
                    if let Some(sites) = out.sites.get(&owner).cloned() {
                        out.sites.entry(prim).or_default().extend(sites);
                    }
                }
                ancestor = store
                    .paths()
                    .resolve(owner)
                    .parent()
                    .and_then(|p| store.paths().lookup(&p));
            }
        }
        out.requests.sort();
        out.requests.dedup();
        out.issues.sort();
        out.issues.dedup();
        out
    }
    fn asset(
        &mut self,
        store: &dyn LayerStore,
        def: &Definition,
        identifier: &str,
        role: ClipAssetRole,
        raw: &mut HashMap<LayerId, Arc<RawLayer>>,
    ) -> Option<(LayerId, Arc<RawLayer>)> {
        let availability = store.asset_availability(def.anchor.layer_id, identifier);
        let status = match availability {
            AssetAvailability::Loaded(id) => {
                self.loaded.insert(id);
                if store.layer(id).is_some() {
                    ClipAssetStatus::Loaded(id)
                } else {
                    ClipAssetStatus::Unavailable(ClipAssetUnavailable::NotResident(id))
                }
            }
            AssetAvailability::Missing => {
                ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing)
            }
            AssetAvailability::Unresolved => {
                ClipAssetStatus::Unavailable(ClipAssetUnavailable::Unresolved)
            }
        };
        self.requests.push(ClipAssetRequest {
            owner: def.owner,
            set_name: def.name.clone(),
            anchor: def.anchor.layer_id,
            source: def.anchor.spec_path.clone(),
            identifier: Arc::from(identifier),
            role,
            status,
        });
        if let ClipAssetStatus::Loaded(id) = status {
            if let Some(layer) = raw.get(&id) {
                return Some((id, layer.clone()));
            }
            let layer = Arc::new(RawLayer::capture(store, id)?);
            raw.insert(id, layer.clone());
            Some((id, layer))
        } else {
            None
        }
    }
    fn issue(&mut self, def: &Definition, kind: ClipIssueKind) {
        self.issues.push(ClipIssue {
            owner: def.owner,
            set_name: def.name.clone(),
            anchor: def.anchor.layer_id,
            source: def.anchor.spec_path.clone(),
            kind,
        });
    }
    fn prepare_set(
        &mut self,
        store: &mut dyn LayerStore,
        def: Definition,
        raw: &mut HashMap<LayerId, Arc<RawLayer>>,
    ) -> Option<ReadySet> {
        let clip_root = match get(&def.dictionary, "primPath") {
            Some(Value::String(s)) => Path::parse_absolute(s, store.tokens_mut()).ok(),
            _ => None,
        };
        let Some(clip_root) = clip_root.filter(|p| *p != Path::root()) else {
            self.issue(&def, ClipIssueKind::InvalidMetadata("primPath"));
            return None;
        };
        let interpolate_missing = match get(&def.dictionary, "interpolateMissingClipValues") {
            None => false,
            Some(Value::Bool(v)) => *v,
            _ => {
                self.issue(
                    &def,
                    ClipIssueKind::InvalidMetadata("interpolateMissingClipValues"),
                );
                return None;
            }
        };
        let mut manifest_unavailable = false;
        let manifest = match get(&def.dictionary, "manifestAssetPath") {
            None => None,
            Some(Value::Asset(s)) if s.is_empty() => None,
            Some(Value::Asset(s)) => {
                let layer = self.asset(store, &def, s, ClipAssetRole::Manifest, raw);
                manifest_unavailable = layer.is_none();
                layer
            }
            _ => {
                self.issue(&def, ClipIssueKind::InvalidMetadata("manifestAssetPath"));
                return None;
            }
        };
        let (clips, active, times) = if let Some(paths) = assets(get(&def.dictionary, "assetPaths"))
        {
            let Some(active_pairs) = pairs(get(&def.dictionary, "active")) else {
                self.issue(&def, ClipIssueKind::InvalidMetadata("active"));
                return None;
            };
            let active: Option<Vec<_>> = active_pairs
                .into_iter()
                .map(|(time, index)| {
                    if index.is_finite()
                        && index >= 0.
                        && libm::floor(index) == index
                        && index < paths.len() as f64
                    {
                        #[allow(
                            clippy::cast_possible_truncation,
                            clippy::cast_sign_loss,
                            reason = "validated nonnegative index bounded by clip count"
                        )]
                        Some((time, index as usize))
                    } else {
                        None
                    }
                })
                .collect();
            let Some(active) = active else {
                self.issue(&def, ClipIssueKind::InvalidMetadata("active"));
                return None;
            };
            let times = match get(&def.dictionary, "times") {
                None => Vec::new(),
                Some(value) => match pairs(Some(value)) {
                    Some(v) => v,
                    None => {
                        self.issue(&def, ClipIssueKind::InvalidMetadata("times"));
                        return None;
                    }
                },
            };
            let mut clips = Vec::new();
            for path in paths {
                if path.is_empty() {
                    self.issue(&def, ClipIssueKind::InvalidMetadata("assetPaths"));
                    return None;
                }
                clips.push(self.asset(store, &def, &path, ClipAssetRole::Value, raw));
            }
            (clips, active, times)
        } else if let Some(Value::String(pattern)) = get(&def.dictionary, "templateAssetPath") {
            let result = template::expand(&def.dictionary, pattern);
            let Ok(template) = result else {
                self.issue(&def, ClipIssueKind::InvalidTemplate);
                return None;
            };
            let mut clips = Vec::new();
            let mut active = Vec::new();
            let mut times = Vec::new();
            let mut unresolved = false;
            if let Some(time) = template.front {
                times.push((external(def.offset, time), time));
            }
            for (time, identifier) in template.candidates {
                let availability = store.asset_availability(def.anchor.layer_id, &identifier);
                let layer = self.asset(
                    store,
                    &def,
                    &identifier,
                    ClipAssetRole::TemplateCandidate,
                    raw,
                );
                if matches!(availability, AssetAvailability::Missing) {
                    continue;
                }
                if layer.is_none() {
                    unresolved = true;
                    continue;
                }
                active.push((
                    external(def.offset, time + template.active_offset),
                    clips.len(),
                ));
                times.push((external(def.offset, time), time));
                clips.push(layer);
            }
            if let Some(time) = template.back {
                times.push((external(def.offset, time), time));
            }
            if unresolved {
                self.issue(&def, ClipIssueKind::UnresolvedTemplate);
                return None;
            }
            (clips, active, times)
        } else {
            self.issue(&def, ClipIssueKind::InvalidMetadata("assetPaths"));
            return None;
        };
        if manifest_unavailable || clips.is_empty() || active.is_empty() {
            return None;
        }
        Some(ReadySet {
            definition: def,
            clip_root,
            clips,
            manifest,
            active,
            times,
            interpolate_missing,
        })
    }
    fn prepare_properties(
        &mut self,
        store: &mut dyn LayerStore,
        prim: PathId,
        index: &PrimIndex,
        owner_index: &PrimIndex,
        set: &ReadySet,
    ) {
        let Some(anchor_node) = owner_index.graph.node(set.definition.anchor.node) else {
            return;
        };
        let source_prefix = store
            .paths()
            .resolve(set.definition.anchor.lookup_path)
            .clone();
        let source_spec = set.definition.anchor.spec_path.prim_spec();
        let target_node = index.graph.strength_order().into_iter().find(|&node| {
            let Some(target) = index.graph.node(node) else {
                return false;
            };
            target.layer_stack() == anchor_node.layer_stack()
                // C++ _ClipsApplyToNode compares variant-qualified paths. A
                // variant clip belongs at that arc, below local layer opinions.
                && target.site().components().starts_with(source_spec.components())
                && source_prefix.is_prefix_of(store.paths().resolve(target.site().prim_path()))
                && crate::expression_variables::node_variables(store, &index.graph, node)
                    == crate::expression_variables::node_variables(
                        store,
                        &owner_index.graph,
                        set.definition.anchor.node,
                    )
        });
        let Some(node) = target_node else {
            return;
        };
        let source = store
            .paths()
            .resolve(index.graph.node(node).unwrap().site().prim_path())
            .clone();
        let Some(suffix) = source.strip_prefix(&source_prefix) else {
            return;
        };
        let clip_path = store.paths_mut().intern(set.clip_root.join(suffix));
        let mut manifest_props: BTreeMap<TokenId, Arc<PropertySpec>> = BTreeMap::new();
        if let Some((_, manifest)) = &set.manifest {
            if let Some(props) = manifest.at(clip_path) {
                manifest_props = props.clone();
            }
        } else {
            // Auto-manifest declaration union: samples select sample mode and
            // erase a prior spline marker. Raw defaults never create eligibility.
            for (_, clip) in set.clips.iter().flatten() {
                if let Some(props) = clip.at(clip_path) {
                    for (&name, spec) in props {
                        let samples = spec.time_samples.as_ref().is_some_and(|s| !s.is_empty());
                        if !samples && spec.spline.is_none() {
                            continue;
                        }
                        if let Some(existing) = manifest_props.get_mut(&name) {
                            if samples {
                                Arc::make_mut(existing).spline = None;
                            }
                        } else {
                            manifest_props
                                .insert(name, Arc::new(auto_manifest_declaration(spec, samples)));
                        }
                    }
                }
            }
        }
        self.sites
            .entry(prim)
            .or_default()
            .extend(set.definition.sources.iter().copied());
        for (id, _) in set.clips.iter().flatten().chain(set.manifest.iter()) {
            self.sites.entry(prim).or_default().insert((*id, clip_path));
        }
        for (field, manifest) in manifest_props {
            if manifest.kind != crate::PropertyKind::Attribute
                || manifest.variability != Variability::Varying
            {
                continue;
            }
            let spline_mode = manifest.spline.is_some() && manifest.time_samples.is_none();
            let clips: Vec<Option<Arc<PropertySpec>>> = set
                .clips
                .iter()
                .map(|c| {
                    c.as_ref()
                        .and_then(|(_, layer)| layer.at(clip_path)?.get(&field).cloned())
                })
                .collect();
            // Inspect only sample kinds; payload arrays remain shared. Any
            // sparse endpoint requires temporal folding, even if the current
            // interpolated value holds a dense lower endpoint.
            let has_sparse_samples = clips
                .iter()
                .flatten()
                .flat_map(|property| {
                    property
                        .time_samples
                        .iter()
                        .flat_map(|samples| samples.iter())
                })
                .any(|(_, value)| value.array_edit_ref().is_some())
                || manifest
                    .default
                    .as_ref()
                    .is_some_and(|value| value.array_edit_ref().is_some());
            let sources: Vec<_> = set
                .clips
                .iter()
                .zip(&clips)
                .map(|(layer, property)| {
                    if property.as_ref().is_some_and(|p| {
                        if spline_mode {
                            p.spline.is_some()
                        } else {
                            p.time_samples.as_ref().is_some_and(|s| !s.is_empty())
                        }
                    }) {
                        layer.as_ref().map(|(id, _)| *id)
                    } else if manifest.default.is_some() {
                        set.manifest.as_ref().map(|(id, _)| *id)
                    } else {
                        None
                    }
                })
                .collect();
            let constructor = if spline_mode {
                PreparedClipProperty::new_spline
            } else {
                PreparedClipProperty::new
            };
            let prepared = constructor(
                set.active.clone(),
                set.times.clone(),
                clips,
                manifest.default.clone(),
                set.interpolate_missing,
            )
            .and_then(|prepared| {
                // The manifest clip has identity time mapping. Its exact
                // authored activation blocks are stage times, independent of
                // the metadata anchor offset (usd/clip.cpp::IsBlocked).
                let blocked = if let Some(samples) = &manifest.time_samples {
                    samples
                        .iter()
                        .filter_map(|(time, value)| {
                            matches!(value, Value::Blocked).then_some(*time)
                        })
                        .collect()
                } else {
                    manifest.spline.as_ref().map_or_else(Vec::new, |spline| {
                        spline.knots.iter().map(|knot| knot.time).collect()
                    })
                };
                prepared.with_manifest_blocks(blocked)
            });
            let evaluator = match prepared {
                Ok(evaluator) => evaluator,
                Err(error) => {
                    self.issue(
                        &set.definition,
                        if error == ClipEvalError::UnsupportedSpline {
                            ClipIssueKind::UnsupportedSpline
                        } else {
                            ClipIssueKind::InvalidSchedule
                        },
                    );
                    continue;
                }
            };
            let mut key = set.definition.anchor.clone();
            key.node = node;
            let spec =
                SpecPath::from_property_path(PropertyPath::new(clip_path, field), store.paths());
            self.properties
                .entry((prim, field))
                .or_default()
                .push(ClipProperty {
                    owner: set.definition.owner,
                    set_name: set.definition.name.clone(),
                    key,
                    spec,
                    clip_layers: sources,
                    manifest_layer: set.manifest.as_ref().map(|(id, _)| *id),
                    has_sparse_samples,
                    property_type: manifest.type_name.clone(),
                    evaluator: Arc::new(evaluator),
                });
        }
    }
    pub(crate) fn opinions(
        &self,
        prim: PathId,
        field: TokenId,
        time: f64,
        interp: InterpolationType,
        sample_queries: &[f64],
    ) -> Vec<Opinion> {
        self.properties
            .get(&(prim, field))
            .into_iter()
            .flatten()
            .map(|entry| project_opinion(entry, field, time, interp, sample_queries))
            .collect()
    }
    pub(crate) fn opinion_for(
        &self,
        prim: PathId,
        field: TokenId,
        entry_index: usize,
        time: f64,
        interp: InterpolationType,
        sample_queries: &[f64],
    ) -> Option<Opinion> {
        let entry = self.properties.get(&(prim, field))?.get(entry_index)?;
        Some(project_opinion(entry, field, time, interp, sample_queries))
    }
    pub(crate) fn evaluation_for(
        &self,
        prim: PathId,
        field: TokenId,
        entry_index: usize,
        time: f64,
        interp: InterpolationType,
    ) -> Option<ClipValueSource> {
        let entry = self.properties.get(&(prim, field))?.get(entry_index)?;
        entry_evaluation(entry, time, interp)
    }
    pub(crate) fn evaluation_error_for(
        &self,
        prim: PathId,
        field: TokenId,
        entry_index: usize,
        time: f64,
        interp: InterpolationType,
    ) -> Option<ClipEvalError> {
        self.properties
            .get(&(prim, field))?
            .get(entry_index)?
            .evaluator
            .evaluate(time, interp)
            .err()
    }
    pub(crate) fn sample_composes_for(
        &self,
        prim: PathId,
        field: TokenId,
        entry_index: usize,
        time: f64,
    ) -> Option<bool> {
        let entry = self.properties.get(&(prim, field))?.get(entry_index)?;
        Some(entry.evaluator.sample_is_sparse(time))
    }
    // Native QueryTimeSampleTypeid metadata at exact mapped sample positions;
    // synthetic activation/mapping positions fall back to manifest default
    // kind. This does not hold/interpolate values or project array payloads.
    pub(crate) fn sample_kinds_for(
        &self,
        prim: PathId,
        field: TokenId,
        entry_index: usize,
    ) -> Vec<(f64, bool)> {
        self.properties
            .get(&(prim, field))
            .and_then(|entries| entries.get(entry_index))
            .map_or_else(Vec::new, |entry| {
                entry
                    .evaluator
                    .sample_times()
                    .iter()
                    .map(|&time| (time, entry.evaluator.sample_is_sparse(time)))
                    .collect()
            })
    }
    /// Full discrete clip maps for explicit flattening, with each raw asset
    /// anchor. Runtime queries continue to project only requested endpoints.
    pub(crate) fn flatten_samples(
        &self,
        prim: PathId,
        field: TokenId,
        selected: &[usize],
    ) -> Result<Vec<FlattenClip>, ClipEvalError> {
        let mut out = Vec::new();
        for (index, entry) in self
            .properties
            .get(&(prim, field))
            .into_iter()
            .flatten()
            .enumerate()
        {
            if !selected.contains(&index) {
                continue;
            }
            if entry.evaluator.is_spline() {
                return Err(ClipEvalError::UnsupportedSpline);
            }
            let mut samples = Vec::new();
            let mut anchors = Vec::new();
            for &time in entry.evaluator.sample_times() {
                let value = entry.evaluator.evaluate(time, InterpolationType::Linear)?;
                let anchor = if value.lower_from_manifest {
                    entry.manifest_layer
                } else {
                    entry.clip_layers.get(value.lower_clip).copied().flatten()
                };
                anchors.push(anchor);
                samples.push((time, value.value.unwrap_or(Value::Blocked)));
            }
            let mut spec = PropertySpec::attribute();
            spec.type_name = entry.property_type.clone();
            spec.time_samples = Some(samples.into());
            out.push(FlattenClip {
                opinion: Opinion {
                    key: entry.key.clone(),
                    field,
                    value: OpinionValue::Property(Arc::new(spec)),
                    layer_offset: LayerOffset::IDENTITY,
                },
                anchors,
            });
        }
        Ok(out)
    }
    /// Prepared clip properties, including schema attributes absent from
    /// static authored topology (`UsdStage::_CopyPrim`).
    pub(crate) fn property_names(&self, prim: PathId) -> Vec<TokenId> {
        self.properties
            .keys()
            .filter_map(|&(path, name)| (path == prim).then_some(name))
            .collect()
    }
    pub(crate) fn source_sites(&self, prim: PathId) -> Vec<(LayerId, PathId)> {
        let mut sites: Vec<_> = self
            .sites
            .get(&prim)
            .into_iter()
            .flatten()
            .copied()
            .collect();
        sites.sort_unstable();
        sites
    }
    pub(crate) fn layers(&self) -> Vec<LayerId> {
        let mut layers: Vec<_> = self.loaded.iter().copied().collect();
        layers.sort_unstable();
        layers
    }
    pub(crate) fn asset_requests(&self) -> &[ClipAssetRequest] {
        &self.requests
    }
    pub(crate) fn issues(&self) -> &[ClipIssue] {
        &self.issues
    }
    pub(crate) fn merge_from(&mut self, partial: Self, recomposed: &[PathId]) {
        let affected: HashSet<_> = recomposed.iter().copied().collect();
        self.properties
            .retain(|(prim, _), _| !affected.contains(prim));
        self.properties.extend(
            partial
                .properties
                .into_iter()
                .filter(|((prim, _), _)| affected.contains(prim)),
        );
        for prim in recomposed {
            self.sites.remove(prim);
            self.bearing.remove(prim);
        }
        self.sites.extend(
            partial
                .sites
                .into_iter()
                .filter(|(prim, _)| affected.contains(prim)),
        );
        self.bearing.extend(
            partial
                .bearing
                .into_iter()
                .filter(|prim| affected.contains(prim)),
        );
        // Recompute below from current authored requests, so removed clip sets
        // stop keeping stale clip layers in the generation watch list.
        self.requests.retain(|r| !affected.contains(&r.owner));
        self.requests.extend(
            partial
                .requests
                .into_iter()
                .filter(|r| affected.contains(&r.owner)),
        );
        self.requests.sort();
        self.requests.dedup();
        self.loaded.clear();
        for request in &self.requests {
            if let ClipAssetStatus::Loaded(id)
            | ClipAssetStatus::Unavailable(ClipAssetUnavailable::NotResident(id)) =
                request.status
            {
                self.loaded.insert(id);
            }
        }
        self.issues.retain(|r| !affected.contains(&r.owner));
        self.issues.extend(
            partial
                .issues
                .into_iter()
                .filter(|r| affected.contains(&r.owner)),
        );
        self.issues.sort();
        self.issues.dedup();
    }
}
fn project_opinion(
    entry: &ClipProperty,
    field: TokenId,
    time: f64,
    interp: InterpolationType,
    sample_queries: &[f64],
) -> Opinion {
    // A selected clip remains a dense opinion when a numeric
    // evaluation fails. Diagnostics are queried separately, rather
    // than exposing a weaker ordinary or schema fallback.
    let evaluated = entry.evaluator.evaluate(time, interp);
    if evaluated.is_ok()
        && !entry.evaluator.is_spline()
        && (!sample_queries.is_empty() || entry.has_sparse_samples)
    {
        // AOUSD Core §12.5 and sparse-array temporal composition:
        // use the real clip endpoints at each planner query. A
        // synthetic query knot would hold the sparse lower value.
        // Only bounded requested endpoints are projected, never
        // all clip payload frames or two full policy caches.
        let mut endpoints = Vec::new();
        for query in core::iter::once(time)
            .chain(sample_queries.iter().copied())
            .filter(|query| query.is_finite())
        {
            if let Ok(evaluation) = entry.evaluator.evaluate(query, interp) {
                endpoints.extend([evaluation.lower, evaluation.upper]);
            }
        }
        endpoints.sort_by(f64::total_cmp);
        endpoints.dedup();
        let samples = endpoints
            .into_iter()
            .map(|endpoint| {
                let value = entry
                    .evaluator
                    .evaluate(endpoint, interp)
                    .ok()
                    .and_then(|evaluation| evaluation.value)
                    .unwrap_or(Value::Blocked);
                (endpoint, value)
            })
            .collect::<Vec<_>>();
        let mut spec = PropertySpec::attribute();
        spec.type_name = entry.property_type.clone();
        spec.time_samples = Some(samples.into());
        return Opinion {
            key: entry.key.clone(),
            field,
            value: OpinionValue::Property(Arc::new(spec)),
            layer_offset: LayerOffset::IDENTITY,
        };
    }
    let value = evaluated
        .ok()
        .and_then(|evaluation| evaluation.value)
        .unwrap_or(Value::Blocked);
    let mut spec = PropertySpec::attribute();
    spec.type_name = entry.property_type.clone();
    spec.time_samples = Some(alloc::vec![(time, value)].into());
    Opinion {
        key: entry.key.clone(),
        field,
        value: OpinionValue::Property(Arc::new(spec)),
        layer_offset: LayerOffset::IDENTITY,
    }
}
fn entry_evaluation(
    entry: &ClipProperty,
    time: f64,
    interp: InterpolationType,
) -> Option<ClipValueSource> {
    let value = entry.evaluator.evaluate(time, interp).ok()?;
    let source =
        |index: usize, stage_time: f64, clip_time: f64, from_manifest: bool| ClipSampleSource {
            layer: if from_manifest {
                entry.manifest_layer
            } else {
                entry.clip_layers.get(index).copied().flatten()
            },
            spec_path: entry.spec.clone(),
            stage_time,
            clip_time,
            clip_index: index,
        };
    Some(ClipValueSource {
        mode: if entry.evaluator.is_spline() {
            ClipValueMode::Spline
        } else {
            ClipValueMode::TimeSamples
        },
        set_name: entry.set_name.clone(),
        owner: entry.owner,
        anchor_layer: entry.key.layer_id,
        lower: source(
            value.lower_clip,
            value.lower,
            value.lower_internal,
            value.lower_from_manifest,
        ),
        upper: source(
            value.upper_clip,
            value.upper,
            value.upper_internal,
            value.upper_from_manifest,
        ),
    })
}

/// Generated manifests retain only a spline mode/type annotation. Copying
/// source knots would incorrectly turn them into manifest activation blocks.
/// OpenUSD 26.08 `usd/clipSet.cpp::Usd_GenerateClipManifest`.
fn auto_manifest_declaration(spec: &PropertySpec, samples: bool) -> PropertySpec {
    // Declarations carry no source payload: avoid even a transient clone of
    // raw defaults, sample buffers, spline knots or unrelated metadata.
    let mut declaration = PropertySpec::attribute();
    declaration.type_name = spec.type_name.clone();
    declaration.variability = spec.variability;
    declaration.spline = if samples {
        None
    } else {
        spec.spline
            .as_ref()
            .map(|spline| crate::spline::SplineData {
                data_type: spline.data_type,
                default_curve_type: crate::spline::CurveType::Bezier,
                pre_extrapolation: crate::spline::Extrapolation::Held,
                post_extrapolation: crate::spline::Extrapolation::Held,
                loop_params: None,
                knots: Vec::new(),
            })
    };
    declaration
}
fn get<'a>(dictionary: &'a [(Arc<str>, Value)], name: &str) -> Option<&'a Value> {
    dictionary
        .iter()
        .find(|(n, _)| &**n == name)
        .map(|(_, v)| v)
}
fn assets(value: Option<&Value>) -> Option<Vec<Arc<str>>> {
    value?
        .array_ref()?
        .iter()
        .map(|value| match value.as_ref() {
            Value::Asset(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}
fn pairs(value: Option<&Value>) -> Option<Vec<(f64, f64)>> {
    value?
        .array_ref()?
        .iter()
        .map(|value| match value.as_ref() {
            Value::Vec2d(v) if v.iter().all(|n| n.is_finite()) => Some((v[0], v[1])),
            _ => None,
        })
        .collect()
}
fn external(offset: LayerOffset, time: f64) -> f64 {
    time * offset.scale + offset.offset
}
fn over(strong: &[(Arc<str>, Value)], weak: &[(Arc<str>, Value)]) -> Vec<(Arc<str>, Value)> {
    let mut result: BTreeMap<_, _> = weak.iter().cloned().collect();
    for (name, value) in strong {
        let value = match (value, result.get(name)) {
            (Value::Dictionary(s), Some(Value::Dictionary(w))) => Value::Dictionary(over(s, w)),
            _ => value.clone(),
        };
        result.insert(name.clone(), value);
    }
    result.into_iter().collect()
}
fn definitions(
    owner: PathId,
    index: &PrimIndex,
    store: &dyn LayerStore,
    issues: &mut Vec<ClipIssue>,
) -> Vec<Definition> {
    let clips = store
        .tokens()
        .lookup("clips")
        .and_then(|t| index.metadata_opinions(t))
        .unwrap_or_default();
    let orders = store
        .tokens()
        .lookup("clipSets")
        .and_then(|t| index.metadata_opinions(t))
        .unwrap_or_default();
    let mut composed: BTreeMap<Arc<str>, Definition> = BTreeMap::new();
    for node in index.graph.strength_order() {
        let mut layers: Vec<_> = clips
            .iter()
            .chain(orders)
            .filter(|o| o.key.node == node)
            .collect();
        layers.sort_by(|a, b| index.graph.cmp_keys(&b.key, &a.key));
        let mut node_sets: BTreeMap<Arc<str>, Definition> = BTreeMap::new();
        let mut names = Vec::new();
        for opinion in layers {
            if store.tokens().resolve(opinion.field) == "clipSets" {
                if let OpinionValue::Field(FieldValue::StringListOp(op)) = &opinion.value {
                    names = op.apply_to(&names);
                }
                continue;
            }
            let OpinionValue::Field(FieldValue::Value(Value::Dictionary(dictionary))) =
                &opinion.value
            else {
                continue;
            };
            let mut added = Vec::new();
            for (name, value) in dictionary {
                let Value::Dictionary(value) = value else {
                    issues.push(ClipIssue {
                        owner,
                        set_name: name.clone(),
                        anchor: opinion.key.layer_id,
                        source: opinion.key.spec_path.clone(),
                        kind: ClipIssueKind::InvalidMetadata("clips"),
                    });
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let mut transformed = value.clone();
                for (field, value) in &mut transformed {
                    if (&**field == "active" || &**field == "times")
                        && let Some(values) = pairs(Some(value))
                    {
                        *value = Value::array_from_iter(
                            values
                                .into_iter()
                                .map(|(a, b)| Value::Vec2d([external(opinion.layer_offset, a), b])),
                            None,
                        );
                    }
                }
                let anchor = assets(get(value, "assetPaths")).is_some()
                    || matches!(get(value, "templateAssetPath"), Some(Value::String(_)));
                let entry = node_sets.entry(name.clone()).or_insert_with(|| Definition {
                    owner,
                    name: name.clone(),
                    anchor: opinion.key.clone(),
                    offset: opinion.layer_offset,
                    dictionary: Vec::new(),
                    order: usize::MAX,
                    sources: Vec::new(),
                });
                // An unanchored definition remains mergeable, but cannot create a
                // runtime set until a typed asset-path opinion supplies an anchor.
                if anchor {
                    entry.anchor = opinion.key.clone();
                    entry.offset = opinion.layer_offset;
                    entry.order = 0;
                }
                entry.dictionary = over(&transformed, &entry.dictionary);
                entry
                    .sources
                    .push((opinion.key.layer_id, opinion.key.lookup_path));
                added.push(name.clone());
            }
            added.sort();
            names = ListOp::default().with_added(added).apply_to(&names);
        }
        for (name, mut definition) in node_sets {
            let Some(order) = names.iter().position(|n| n == &name) else {
                continue;
            };
            let anchored = definition.order != usize::MAX;
            definition.order = if anchored { order } else { usize::MAX };
            if let Some(strong) = composed.get_mut(&name) {
                strong.dictionary = over(&strong.dictionary, &definition.dictionary);
                strong.sources.extend(definition.sources);
                if strong.order == usize::MAX && anchored {
                    strong.anchor = definition.anchor;
                    strong.offset = definition.offset;
                    strong.order = order;
                }
            } else {
                composed.insert(name, definition);
            }
        }
    }
    let mut result: Vec<_> = composed
        .into_values()
        .filter(|d| d.order != usize::MAX)
        .collect();
    result.sort_by(|a, b| {
        index
            .graph
            .cmp_nodes(a.anchor.node, b.anchor.node)
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.name.cmp(&b.name))
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryStore, Layer, PrimSpec, Stage, StageOptions};
    use alloc::vec;

    #[test]
    fn automatic_spline_manifest_is_an_empty_annotation_without_mutating_source() {
        use crate::spline::{
            CurveType, Extrapolation, Knot, KnotInterp, SplineData, SplineDataType,
        };
        let source = Arc::new(PropertySpec::attribute().with_spline(SplineData {
            data_type: SplineDataType::Float,
            default_curve_type: CurveType::Bezier,
            pre_extrapolation: Extrapolation::Held,
            post_extrapolation: Extrapolation::Held,
            loop_params: None,
            knots: vec![Knot {
                time: 0.,
                value: 7.,
                pre_value: None,
                next_interp: KnotInterp::Held,
                curve_type: CurveType::Bezier,
                pre_tan_maya_form: false,
                post_tan_maya_form: false,
                pre_tan_width: 0.,
                post_tan_width: 0.,
                pre_tan_slope: 0.,
                post_tan_slope: 0.,
            }],
        }));
        let shared_source = source.clone();
        let manifest = auto_manifest_declaration(&source, false);
        let annotation = manifest.spline.as_ref().unwrap();
        assert_eq!(annotation.data_type, SplineDataType::Float);
        assert!(annotation.knots.is_empty());
        assert!(manifest.default.is_none() && manifest.time_samples.is_none());
        assert!(Arc::ptr_eq(&source, &shared_source));
        assert_eq!(source.spline.as_ref().unwrap().knots.len(), 1);
        assert!(auto_manifest_declaration(&source, true).spline.is_none());
    }

    fn template_stage(missing_middle: bool) -> (InMemoryStore, Stage, PathId, TokenId) {
        let mut store = InMemoryStore::default();
        let owner = Path::parse_absolute("/P", store.tokens_mut()).unwrap();
        let owner = store.paths_mut().intern(owner);
        let clip_path = Path::parse_absolute("/Clip", store.tokens_mut()).unwrap();
        let clip_path = store.paths_mut().intern(clip_path);
        let clips = store.tokens_mut().intern("clips");
        let x = store.tokens_mut().intern("x");
        let metadata = Value::Dictionary(vec![(
            "default".into(),
            Value::Dictionary(vec![
                ("primPath".into(), Value::String("/Clip".into())),
                (
                    "templateAssetPath".into(),
                    Value::String("clip.#.usda".into()),
                ),
                ("templateStartTime".into(), Value::Double(0.)),
                ("templateEndTime".into(), Value::Double(2.)),
                ("templateStride".into(), Value::Double(1.)),
            ]),
        )]);
        let mut root = Layer::new(LayerId(1));
        root.prims.insert(
            owner,
            PrimSpec::def()
                .with_field(clips, metadata)
                .with_property(x, PropertySpec::attribute()),
        );
        store.insert_layer(root);
        for (id, time) in [(LayerId(2), 0.), (LayerId(3), 2.)] {
            let mut layer = Layer::new(id);
            let mut property = PropertySpec::attribute();
            property.time_samples = Some(vec![(time, Value::Double(time * 10.))].into());
            layer
                .prims
                .insert(clip_path, PrimSpec::def().with_property(x, property));
            store.insert_layer(layer);
        }
        store.insert_asset_layer(LayerId(1), "clip.0.usda", LayerId(2));
        store.insert_asset_layer(LayerId(1), "clip.2.usda", LayerId(3));
        if missing_middle {
            store.mark_asset_missing(LayerId(1), "clip.1.usda");
        }
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        (store, stage, owner, x)
    }

    #[test]
    fn prepared_entry_ordinals_distinguish_sets_sharing_one_anchor() {
        let (mut store, _, prim, field) = template_stage(true);
        let property_path = store.property_path("/Clip.x");
        let spec = SpecPath::from_property_path(property_path, store.paths());
        let key = OpinionKey {
            node: crate::prim_index_graph::NodeId::ROOT,
            layer_strength: 0,
            layer_id: LayerId(1),
            lookup_path: prim,
            spec_path: SpecPath::from_prim_path(prim, store.paths()),
        };
        let prepared = |name: &str, layer, times: Vec<(f64, Value)>| {
            let mut raw = PropertySpec::attribute();
            raw.time_samples = Some(times.into());
            ClipProperty {
                owner: prim,
                set_name: Arc::from(name),
                key: key.clone(),
                spec: spec.clone(),
                clip_layers: vec![Some(layer)],
                manifest_layer: None,
                has_sparse_samples: false,
                property_type: None,
                evaluator: Arc::new(
                    PreparedClipProperty::new(
                        vec![(0., 0)],
                        vec![],
                        vec![Some(Arc::new(raw))],
                        None,
                        false,
                    )
                    .unwrap(),
                ),
            }
        };
        let mut catalog = Catalog::default();
        catalog.properties.insert(
            (prim, field),
            vec![
                prepared("first", LayerId(2), vec![(0., Value::Double(10.))]),
                prepared(
                    "second",
                    LayerId(3),
                    vec![(0., Value::Double(20.)), (10., Value::Double(30.))],
                ),
            ],
        );
        let initial = catalog.opinions(prim, field, 5., InterpolationType::Linear, &[]);
        assert_eq!(initial[0].key, initial[1].key);
        let second = catalog
            .opinion_for(prim, field, 1, 5., InterpolationType::Linear, &[])
            .unwrap();
        assert_eq!(
            second.value.time_samples().unwrap()[0].1,
            Value::Double(25.)
        );
        let source = catalog
            .evaluation_for(prim, field, 1, 5., InterpolationType::Linear)
            .unwrap();
        assert_eq!(source.set_name.as_ref(), "second");
        assert_eq!(source.lower.layer, Some(LayerId(3)));
        assert_eq!(
            catalog.sample_kinds_for(prim, field, 1),
            vec![(0., false), (10., false)]
        );
        assert_eq!(catalog.sample_kinds_for(prim, field, 0), vec![(0., false)]);
    }

    #[test]
    fn variant_clip_anchor_keeps_local_strength_and_descendant_reference_mapping() {
        use crate::{Reference, SublayerEntry, Time, VariantSetSpec, VariantSpec};
        let (mut store, _, prim, field) = template_stage(true);
        let mode = store.tokens_mut().intern("mode");
        let a = store.tokens_mut().intern("a");
        let child = store.property_path("/P/Q.x");
        let clip_child = store.property_path("/Clip/Q.x");
        let mut root = store.layer(LayerId(1)).unwrap().clone();
        root.sublayers.push(SublayerEntry::new(LayerId(4)));
        let owner = root.prims.get_mut(&prim).unwrap();
        let variant = VariantSpec {
            fields: core::mem::take(&mut owner.fields),
            ..VariantSpec::default()
        };
        owner.variant_selections.insert(mode, a);
        owner.variant_set_order.push(mode);
        owner.variant_sets.insert(
            mode,
            VariantSetSpec {
                variants: [(a, variant)].into_iter().collect(),
            },
        );
        root.prims.insert(
            child.prim_path(),
            PrimSpec::def().with_property(field, PropertySpec::attribute()),
        );
        store.insert_layer(root);
        let mut weaker = Layer::new(LayerId(4));
        let mut local = PropertySpec::attribute();
        local.default = Some(Value::Double(23.));
        weaker
            .prims
            .insert(prim, PrimSpec::over().with_property(field, local));
        store.insert_layer(weaker);
        let mut clip = store.layer(LayerId(2)).unwrap().clone();
        let mut samples = PropertySpec::attribute();
        samples.time_samples = Some(vec![(0., Value::Double(41.))].into());
        clip.prims.insert(
            clip_child.prim_path(),
            PrimSpec::def().with_property(field, samples),
        );
        store.insert_layer(clip);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let parent = PropertyPath::new(prim, field);
        assert_eq!(
            stage
                .read_property(parent, Time::at(0.), |v| Some(v.clone()))
                .unwrap()
                .value,
            Value::Double(23.)
        );
        assert_eq!(
            stage
                .read_property(child, Time::at(0.), |v| Some(v.clone()))
                .unwrap()
                .value,
            Value::Double(41.)
        );
        let world = store.property_path("/World.x");
        let world_child = store.property_path("/World/Q.x");
        let mut reference = Layer::new(LayerId(10));
        let mut world_spec = PrimSpec::def();
        world_spec.references = ListOp::explicit(vec![Reference::new(LayerId(1), prim)]);
        reference.prims.insert(world.prim_path(), world_spec);
        store.insert_layer(reference);
        let stage = Stage::compose(&mut store, LayerId(10), StageOptions::default());
        assert_eq!(
            stage
                .read_property(world, Time::at(0.), |v| Some(v.clone()))
                .unwrap()
                .value,
            Value::Double(23.)
        );
        assert_eq!(
            stage
                .read_property(world_child, Time::at(0.), |v| Some(v.clone()))
                .unwrap()
                .value,
            Value::Double(41.)
        );
    }

    #[test]
    fn confirmed_missing_template_candidates_compact_without_losing_inventory() {
        let (_, stage, prim, field) = template_stage(true);
        let requests = stage.clip_asset_requests();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|r| r.role == ClipAssetRole::TemplateCandidate)
        );
        assert_eq!(requests[1].identifier.as_ref(), "clip.1.usda");
        assert_eq!(
            requests[1].status,
            ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing)
        );
        assert!(stage.clip_issues().is_empty());
        assert_eq!(stage.property_sample_times(prim, field), vec![0., 2.]);
    }

    #[test]
    fn unresolved_template_candidates_report_inventory_without_partial_schedule() {
        let (_, stage, prim, field) = template_stage(false);
        let requests = stage.clip_asset_requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            requests[1].status,
            ClipAssetStatus::Unavailable(ClipAssetUnavailable::Unresolved)
        );
        assert_eq!(stage.clip_issues().len(), 1);
        assert_eq!(
            stage.clip_issues()[0].kind,
            ClipIssueKind::UnresolvedTemplate
        );
        assert!(stage.property_sample_times(prim, field).is_empty());
    }
}
