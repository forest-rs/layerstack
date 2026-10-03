// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Detached topology, manifests and explicit value-clip sequence metadata.
//!
//! Runtime evaluation is provided by [`crate::Stage`] once the host loads and
//! binds the authored assets. OpenUSD 26.8 `UsdClipsAPI::GenerateClipManifestFromLayers` and
//! `UsdUtilsStitchClips` define the ordinary-input behavior. Asset strings are
//! already anchored by the caller; no filesystem lookup or path rebasing occurs.
//! All input layers share the supplied token and path interners.
//! AOUSD Core §7.6 (authored specs), §12.2.5 (metadata dictionaries),
//! §12.3.2.1 (affine time mapping); clip metadata is an OpenUSD extension.
use crate::stitch::{self, StitchError, StitchReport};
use crate::{
    FieldEntry, FieldValue, Layer, LayerId, LayerOffset, PathId, PathInterner, PrimSpec,
    PropertyKind, PropertyPath, PropertySpec, SublayerEntry, TokenId, TokenInterner, Value,
    VariantSetSpec,
};
use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};

/// A clip layer, with an explicitly authored asset and local-to-stage time map.
#[derive(Clone, Copy, Debug)]
pub struct ClipSource<'a> {
    /// Detached source description. The authoring utilities never change it.
    pub layer: &'a Layer,
    /// Asset path as it must appear relative to the eventual root layer.
    pub asset_path: &'a str,
    /// Stage time = clip-local time * scale + offset. Scale must be positive.
    pub offset: LayerOffset,
}
/// Explicit identities and metadata for a new detached clip bundle.
#[derive(Clone, Debug)]
pub struct ClipBundleOptions {
    /// Identity of the returned root layer.
    pub root_id: LayerId,
    /// Identity of the returned topology layer.
    pub topology_id: LayerId,
    /// Identity of the returned manifest layer.
    pub manifest_id: LayerId,
    /// Prim subtree on which clips apply and where their data lives.
    pub clip_prim_path: PathId,
    /// A single identifier naming the clip set, commonly default.
    pub clip_set: String,
    /// Authored topology sublayer asset string, anchored by the caller.
    pub topology_asset_path: String,
    /// Authored manifest asset string, anchored by the caller.
    pub manifest_asset_path: String,
    /// Optional root start-time override; clip mappings are unchanged.
    pub start_time: Option<f64>,
    /// Optional root end-time override; clip mappings are unchanged.
    pub end_time: Option<f64>,
}
/// Complete detached descriptions ready for host export and publication.
#[derive(Clone, Debug)]
pub struct ClipBundle {
    /// Root containing clip metadata, timeline bounds and topology sublayer.
    pub root: Layer,
    /// Stitched static declarations, defaults, relationships and model topology.
    pub topology: Layer,
    /// Animated attribute declarations, spline annotations and topology defaults.
    pub manifest: Layer,
    /// Diagnostics from stitching the static descriptions.
    pub stitch_report: StitchReport,
}
/// Why a clip authoring operation cannot form a coherent interchange record.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipAuthoringError {
    /// No clip sources were supplied for a sequence bundle.
    EmptySequence,
    /// The pseudo-root is not a valid clip prim path.
    InvalidPrimPath(PathId),
    /// The chosen clip set is not a single identifier.
    InvalidClipSet(String),
    /// An authored asset string is empty.
    EmptyAssetPath,
    /// Output layer identities overlap each other or an input identity.
    ConflictingLayerId(LayerId),
    /// A source's offset has nonfinite parameters or a nonpositive scale.
    InvalidOffset(LayerId),
    /// A clip layer has an invalid start/end bound or an invalid mapped bound.
    InvalidTimeRange(LayerId),
    /// Two clips activate at the same stage time.
    DuplicateActivation,
    /// Overlapping clip intervals use incompatible affine time maps, or one
    /// stage time maps to two different clip-local times.
    ConflictingTimeMapping,
    /// The clip prim never occurs in any input description.
    MissingClipPrim(PathId),
    /// Animated attribute declarations disagree across input layers.
    ConflictingDeclaration(PropertyPath),
    /// An animated attribute has no declared type.
    MissingAttributeType(PropertyPath),
    /// Atomic spec creation rejected the manifest description.
    ManifestEdit(crate::edit::EditError),
    /// Stitching rejected a malformed source description.
    Stitch(StitchError),
}
impl core::fmt::Display for ClipAuthoringError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "clip authoring: {self:?}")
    }
}
impl core::error::Error for ClipAuthoringError {}
impl From<StitchError> for ClipAuthoringError {
    fn from(error: StitchError) -> Self {
        Self::Stitch(error)
    }
}

/// Generates the standard topology filename by inserting .topology before
/// the final dot, matching C++ exactly. None means the input has no dot.
#[must_use]
pub fn clip_topology_name(name: &str) -> Option<String> {
    companion_name(name, "topology")
}
/// Generates the standard manifest filename by inserting .manifest before
/// the final dot, matching C++ exactly. None means the input has no dot.
#[must_use]
pub fn clip_manifest_name(name: &str) -> Option<String> {
    companion_name(name, "manifest")
}
fn companion_name(name: &str, component: &str) -> Option<String> {
    let i = name.rfind('.')?;
    Some(format!("{}.{component}{}", &name[..i], &name[i..]))
}
fn animated(property: &PropertySpec) -> bool {
    property.kind == PropertyKind::Attribute
        && (property
            .time_samples
            .as_ref()
            .is_some_and(|samples| !samples.is_empty())
            || property.spline.is_some())
}
fn empty_spline(source: &crate::spline::SplineData) -> crate::spline::SplineData {
    use crate::spline::{CurveType, Extrapolation, SplineData};
    SplineData {
        data_type: source.data_type,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Held,
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        knots: Vec::new(),
    }
}
fn ensure_parent_specs(layer: &mut Layer, path: PathId, paths: &mut PathInterner) {
    let mut at = path;
    while let Some(parent_path) = paths.resolve(at).parent() {
        let leaf = paths.resolve(at).leaf();
        let pseudo_root = parent_path.depth() == 0;
        let parent = paths.intern(parent_path);
        // AOUSD §7.3.1: the pseudo-root carries child names, not a prim specifier.
        let spec = layer.prims.entry(parent).or_insert_with(|| {
            if pseudo_root {
                PrimSpec::default()
            } else {
                PrimSpec::over()
            }
        });
        if let Some(leaf) = leaf
            && !spec.authored_children.contains(&leaf)
        {
            spec.authored_children.push(leaf);
        }
        at = parent;
    }
}

struct ManifestStore<'a> {
    layer: Layer,
    tokens: &'a mut TokenInterner,
    paths: &'a mut PathInterner,
}
impl crate::LayerStore for ManifestStore<'_> {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        (id == self.layer.id).then_some(&self.layer)
    }
    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        (id == self.layer.id).then_some(&mut self.layer)
    }
    fn tokens(&self) -> &TokenInterner {
        self.tokens
    }
    fn tokens_mut(&mut self) -> &mut TokenInterner {
        self.tokens
    }
    fn paths(&self) -> &PathInterner {
        self.paths
    }
    fn paths_mut(&mut self) -> &mut PathInterner {
        self.paths
    }
}
fn collect_manifest_properties(
    properties: &[crate::PropertyEntry],
    path: PathId,
    sites: &[crate::spec_path::VariantSelectionSite],
    paths: &PathInterner,
    declarations: &mut BTreeMap<crate::SpecPath, PropertySpec>,
) -> Result<(), ClipAuthoringError> {
    for property in properties {
        if let Some(samples) = &property.spec.time_samples
            && (samples.iter().any(|(time, _)| !time.is_finite())
                || samples.windows(2).any(|p| p[0].0 >= p[1].0))
        {
            return Err(ClipAuthoringError::Stitch(StitchError::InvalidTimeSamples));
        }
        if !animated(&property.spec) {
            continue;
        }
        let at = PropertyPath::new(path, property.name);
        let ty = property
            .spec
            .type_name
            .clone()
            .ok_or(ClipAuthoringError::MissingAttributeType(at))?;
        let spec_path = crate::SpecPath::from_variant_selection_sites(path, sites, paths)
            .with_property(property.name);
        let has_samples = property
            .spec
            .time_samples
            .as_ref()
            .is_some_and(|samples| !samples.is_empty());
        if let Some(existing) = declarations.get_mut(&spec_path) {
            let previous = existing.type_name.as_ref().expect("manifest type");
            if previous.type_name != ty.type_name || previous.is_array != ty.is_array {
                return Err(ClipAuthoringError::ConflictingDeclaration(at));
            }
            if has_samples {
                existing.spline = None;
                existing.time_samples = Some(Vec::new().into());
            }
        } else {
            let mut declaration = PropertySpec::typed_attribute(ty);
            declaration.custom = false;
            declaration.variability = property.spec.variability;
            if has_samples {
                declaration.time_samples = Some(Vec::new().into());
            } else {
                declaration.spline = property.spec.spline.as_ref().map(empty_spline);
            }
            declarations.insert(spec_path, declaration);
        }
    }
    Ok(())
}
fn collect_manifest_variants(
    sets: &crate::HashMap<TokenId, VariantSetSpec>,
    path: PathId,
    sites: &mut Vec<crate::spec_path::VariantSelectionSite>,
    paths: &PathInterner,
    declarations: &mut BTreeMap<crate::SpecPath, PropertySpec>,
) -> Result<(), ClipAuthoringError> {
    for (&set, variants) in sets {
        for (&variant, spec) in &variants.variants {
            sites.push(crate::spec_path::VariantSelectionSite {
                host_path: path,
                set,
                variant,
            });
            collect_manifest_properties(&spec.properties, path, sites, paths, declarations)?;
            collect_manifest_variants(&spec.variant_sets, path, sites, paths, declarations)?;
            sites.pop();
        }
    }
    Ok(())
}

/// Generates a declaration-only manifest for animated attributes within a prim
/// subtree. Relationships, static attributes, defaults and metadata are omitted.
/// Any time-sampled occurrence overrides spline annotations, even in a later
/// layer. Custom flags are false, matching C++'s creation of manifest attributes.
/// Empty input produces an empty manifest. Variant-authored attributes and
/// descendants keep their exact branch contexts; static branches are omitted.
/// Inputs remain unchanged on both success and failure.
pub fn generate_clip_manifest_from_layers(
    layers: &[&Layer],
    clip_prim_path: PathId,
    manifest_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
) -> Result<Layer, ClipAuthoringError> {
    if paths.resolve(clip_prim_path).segments().is_empty() {
        return Err(ClipAuthoringError::InvalidPrimPath(clip_prim_path));
    }
    let mut declarations = BTreeMap::new();
    for layer in layers {
        for (&path, spec) in layer.prims.iter().chain(
            layer
                .variant_prims
                .iter()
                .flat_map(|(p, specs)| specs.iter().map(move |s| (p, s))),
        ) {
            if !paths
                .resolve(clip_prim_path)
                .is_prefix_of(paths.resolve(path))
            {
                continue;
            }
            collect_manifest_properties(
                &spec.properties,
                path,
                &spec.outer_variant_sites,
                paths,
                &mut declarations,
            )?;
            let mut sites = spec.outer_variant_sites.clone();
            collect_manifest_variants(
                &spec.variant_sets,
                path,
                &mut sites,
                paths,
                &mut declarations,
            )?;
        }
    }
    let mut transaction = crate::Transaction::new();
    for (at, mut property) in declarations {
        property.time_samples = None;
        transaction.create_property(crate::Address::spec(manifest_id, at), property);
    }
    let mut store = ManifestStore {
        layer: Layer::new(manifest_id),
        tokens,
        paths,
    };
    transaction
        .apply(&mut store)
        .map_err(ClipAuthoringError::ManifestEdit)?;
    Ok(store.layer)
}
fn copy_property_defaults(manifest: &mut [crate::PropertyEntry], source: &[crate::PropertyEntry]) {
    for property in manifest {
        let default = source
            .iter()
            .find(|p| p.name == property.name)
            .and_then(|p| p.spec.default.clone());
        Arc::make_mut(&mut property.spec).default = default;
    }
}
fn copy_variant_defaults(
    manifest: &mut crate::HashMap<TokenId, VariantSetSpec>,
    source: &crate::HashMap<TokenId, VariantSetSpec>,
) {
    for (set, variants) in manifest {
        for (variant, spec) in &mut variants.variants {
            if let Some(source) = source.get(set).and_then(|set| set.variants.get(variant)) {
                copy_property_defaults(&mut spec.properties, &source.properties);
                copy_variant_defaults(&mut spec.variant_sets, &source.variant_sets);
            }
        }
    }
}
fn valid_samples(property: &PropertySpec) -> Result<(), ClipAuthoringError> {
    if let Some(samples) = &property.time_samples
        && (samples.iter().any(|(time, _)| !time.is_finite())
            || samples.windows(2).any(|p| p[0].0 >= p[1].0))
    {
        return Err(ClipAuthoringError::Stitch(StitchError::InvalidTimeSamples));
    }
    Ok(())
}
fn strip_variants(
    sets: &mut crate::HashMap<TokenId, VariantSetSpec>,
) -> Result<(), ClipAuthoringError> {
    for set in sets.values_mut() {
        for variant in set.variants.values_mut() {
            for property in &mut variant.properties {
                let p = Arc::make_mut(&mut property.spec);
                valid_samples(p)?;
                p.time_samples = None;
                p.spline = None;
            }
            strip_variants(&mut variant.variant_sets)?;
        }
    }
    Ok(())
}
fn static_layer(source: &Layer) -> Result<Layer, ClipAuthoringError> {
    let mut result = source.clone();
    for prim in result
        .prims
        .values_mut()
        .chain(result.variant_prims.values_mut().flatten())
    {
        for property in &mut prim.properties {
            let p = Arc::make_mut(&mut property.spec);
            valid_samples(p)?;
            p.time_samples = None;
            p.spline = None;
        }
        strip_variants(&mut prim.variant_sets)?;
    }
    Ok(result)
}
fn time_bound(
    layer: &Layer,
    tokens: &TokenInterner,
    name: &str,
    legacy: &str,
) -> Result<f64, ClipAuthoringError> {
    let field = |name| {
        tokens
            .lookup(name)
            .and_then(|key| crate::get_field(&layer.metadata, &key))
    };
    match field(name).or_else(|| field(legacy)) {
        Some(FieldValue::Value(Value::Double(v))) if v.is_finite() => Ok(*v),
        None => Ok(0.),
        _ => Err(ClipAuthoringError::InvalidTimeRange(layer.id)),
    }
}
/// Builds a complete explicit sequence. Input order determines clip asset
/// indices and static opinion strength; activation pairs are sorted by stage
/// time while retaining their original asset indices. Layer metadata bounds,
/// not sample extrema, determine each clip's interval (missing bounds mean 0).
/// Positive explicit offsets map local endpoints into stage time. Duplicate
/// activations, overlapping intervals with incompatible affine maps,
/// conflicting time mappings and nonfinite times are rejected.
/// Output ids are distinct and cannot overlap inputs. No sources are mutated.
pub fn stitch_clip_sequence(
    sources: &[ClipSource<'_>],
    options: &ClipBundleOptions,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
) -> Result<ClipBundle, ClipAuthoringError> {
    if sources.is_empty() {
        return Err(ClipAuthoringError::EmptySequence);
    }
    if paths.resolve(options.clip_prim_path).segments().is_empty() {
        return Err(ClipAuthoringError::InvalidPrimPath(options.clip_prim_path));
    }
    if !crate::ident::is_identifier(&options.clip_set) {
        return Err(ClipAuthoringError::InvalidClipSet(options.clip_set.clone()));
    }
    if options.topology_asset_path.is_empty()
        || options.manifest_asset_path.is_empty()
        || sources.iter().any(|s| s.asset_path.is_empty())
    {
        return Err(ClipAuthoringError::EmptyAssetPath);
    }
    let ids = [options.root_id, options.topology_id, options.manifest_id];
    for (i, &id) in ids.iter().enumerate() {
        if ids[..i].contains(&id) || sources.iter().any(|s| s.layer.id == id) {
            return Err(ClipAuthoringError::ConflictingLayerId(id));
        }
    }
    if !sources
        .iter()
        .any(|s| s.layer.prims.contains_key(&options.clip_prim_path))
    {
        return Err(ClipAuthoringError::MissingClipPrim(options.clip_prim_path));
    }
    let mut active = Vec::new();
    let mut times = Vec::new();
    let mut intervals: Vec<(f64, f64, LayerOffset)> = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let offset = source.offset;
        if !offset.offset.is_finite() || !offset.scale.is_finite() || offset.scale <= 0. {
            return Err(ClipAuthoringError::InvalidOffset(source.layer.id));
        }
        let start = time_bound(source.layer, tokens, "startTimeCode", "startFrame")?;
        let end = time_bound(source.layer, tokens, "endTimeCode", "endFrame")?;
        let stage_start = start * offset.scale + offset.offset;
        let stage_end = end * offset.scale + offset.offset;
        if end < start || !stage_start.is_finite() || !stage_end.is_finite() {
            return Err(ClipAuthoringError::InvalidTimeRange(source.layer.id));
        }
        // Clip times are a single global mapping, so incompatible per-clip
        // affine maps cannot coexist over an overlapping stage interval.
        for &(previous_start, previous_end, previous_offset) in &intervals {
            if stage_start == previous_start {
                return Err(ClipAuthoringError::DuplicateActivation);
            }
            let overlap_start = stage_start.max(previous_start);
            let overlap_end = stage_end.min(previous_end);
            if overlap_start < overlap_end && offset != previous_offset {
                return Err(ClipAuthoringError::ConflictingTimeMapping);
            }
            if overlap_start == overlap_end
                && offset.map_time(overlap_start) != previous_offset.map_time(overlap_start)
            {
                return Err(ClipAuthoringError::ConflictingTimeMapping);
            }
        }
        intervals.push((stage_start, stage_end, offset));
        active.push([stage_start, index as f64]);
        times.push([stage_start, start]);
        if end != start {
            times.push([stage_end, end]);
        }
    }
    active.sort_by(|a, b| a[0].total_cmp(&b[0]));
    if active.windows(2).any(|p| p[0][0] == p[1][0]) {
        return Err(ClipAuthoringError::DuplicateActivation);
    }
    times.sort_by(|a, b| a[0].total_cmp(&b[0]));
    if times
        .windows(2)
        .any(|p| p[0][0] == p[1][0] && p[0][1] != p[1][1])
    {
        return Err(ClipAuthoringError::ConflictingTimeMapping);
    }
    times.dedup();
    let start = options.start_time.unwrap_or(times[0][0]);
    let end = options.end_time.unwrap_or(times[times.len() - 1][0]);
    if !start.is_finite() || !end.is_finite() || end < start {
        return Err(ClipAuthoringError::InvalidTimeRange(options.root_id));
    }
    let layers: Vec<_> = sources.iter().map(|s| s.layer).collect();
    let mut manifest = generate_clip_manifest_from_layers(
        &layers,
        options.clip_prim_path,
        options.manifest_id,
        tokens,
        paths,
    )?;
    let mut topology = Layer::new(options.topology_id);
    let mut report = StitchReport::default();
    for source in sources {
        let part = stitch::stitch_layers(&mut topology, &static_layer(source.layer)?, tokens)?;
        report.approximated_list_ops += part.approximated_list_ops;
        report
            .retained_metadata_conflicts
            .extend(part.retained_metadata_conflicts);
    }
    for (&path, prim) in manifest.prims.iter_mut().chain(
        manifest
            .variant_prims
            .iter_mut()
            .flat_map(|(path, prims)| prims.iter_mut().map(move |prim| (path, prim))),
    ) {
        if let Some(source) = topology.prim_spec_in(path, &prim.outer_variant_sites) {
            copy_property_defaults(&mut prim.properties, &source.properties);
            copy_variant_defaults(&mut prim.variant_sets, &source.variant_sets);
        }
    }
    let clips = tokens.intern("clips");
    let start_key = tokens.intern("startTimeCode");
    let end_key = tokens.intern("endTimeCode");
    let metadata = Value::Dictionary(alloc::vec![
        (
            "active".into(),
            Value::array_from_iter(
                active.into_iter().map(Value::Vec2d),
                Some(&Value::Vec2d([0.; 2]))
            )
        ),
        (
            "assetPaths".into(),
            Value::array_from_iter(
                sources.iter().map(|s| Value::Asset(s.asset_path.into())),
                Some(&Value::Asset("".into()))
            )
        ),
        (
            "manifestAssetPath".into(),
            Value::Asset(options.manifest_asset_path.as_str().into())
        ),
        (
            "primPath".into(),
            Value::string(paths.display(options.clip_prim_path, tokens))
        ),
        (
            "times".into(),
            Value::array_from_iter(
                times.into_iter().map(Value::Vec2d),
                Some(&Value::Vec2d([0.; 2]))
            )
        ),
    ]);
    let mut root = Layer::new(options.root_id);
    root.metadata = alloc::vec![
        FieldEntry {
            name: start_key,
            value: Value::Double(start).into()
        },
        FieldEntry {
            name: end_key,
            value: Value::Double(end).into()
        }
    ];
    root.sublayers.push(SublayerEntry::with_asset(
        options.topology_id,
        options.topology_asset_path.clone(),
        LayerOffset::IDENTITY,
    ));
    root.insert_prim(
        options.clip_prim_path,
        PrimSpec::over().with_field(
            clips,
            Value::Dictionary(alloc::vec![(options.clip_set.as_str().into(), metadata)]),
        ),
    );
    ensure_parent_specs(&mut root, options.clip_prim_path, paths);
    Ok(ClipBundle {
        root,
        topology,
        manifest,
        stitch_report: report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryStore, PropertyType, TypedArray};
    use alloc::vec;
    fn fixture() -> (InMemoryStore, PathId, Layer, Layer, ClipBundleOptions) {
        let mut store = InMemoryStore::default();
        let path = store.path("/Model");
        let mesh = store.tokens.intern("Mesh");
        let animated = store.tokens.intern("animated");
        let static_name = store.tokens.intern("static");
        let start = store.tokens.intern("startTimeCode");
        let end = store.tokens.intern("endTimeCode");
        let mut a = Layer::new(LayerId(1));
        a.metadata = vec![
            FieldEntry {
                name: start,
                value: Value::Double(1.).into(),
            },
            FieldEntry {
                name: end,
                value: Value::Double(2.).into(),
            },
        ];
        a.insert_prim(
            path,
            PrimSpec::def()
                .with_type_name(mesh)
                .with_property(
                    animated,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        false,
                        Value::Float(0.),
                    ))
                    .with_default(Value::Float(7.))
                    .with_time_samples(vec![(1., Value::Float(1.)), (2., Value::Float(2.))]),
                )
                .with_property(
                    static_name,
                    PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                        .with_default(Value::Int(42)),
                ),
        );
        let mut b = a.clone();
        b.id = LayerId(2);
        b.metadata = vec![
            FieldEntry {
                name: start,
                value: Value::Double(3.).into(),
            },
            FieldEntry {
                name: end,
                value: Value::Double(4.).into(),
            },
        ];
        let attr = b
            .prims
            .get_mut(&path)
            .unwrap()
            .property_mut(animated)
            .unwrap();
        attr.default = Some(Value::Float(9.));
        attr.time_samples = Some(vec![(3., Value::Float(3.)), (4., Value::Float(4.))].into());
        let options = ClipBundleOptions {
            root_id: LayerId(10),
            topology_id: LayerId(11),
            manifest_id: LayerId(12),
            clip_prim_path: path,
            clip_set: "default".into(),
            topology_asset_path: "./root.topology.usda".into(),
            manifest_asset_path: "./root.manifest.usda".into(),
            start_time: None,
            end_time: None,
        };
        (store, path, a, b, options)
    }
    fn dictionary_value<'a>(value: &'a Value, key: &str) -> &'a Value {
        let Value::Dictionary(entries) = value else {
            panic!("dictionary")
        };
        &entries
            .iter()
            .find(|(name, _)| name.as_ref() == key)
            .unwrap()
            .1
    }
    fn clip_metadata<'a>(
        bundle: &'a ClipBundle,
        path: PathId,
        tokens: &TokenInterner,
    ) -> &'a Value {
        let FieldValue::Value(value) = bundle.root.prims[&path]
            .field(tokens.lookup("clips").unwrap())
            .unwrap()
        else {
            panic!("clips")
        };
        dictionary_value(value, "default")
    }
    fn pairs(value: &Value) -> Vec<[f64; 2]> {
        value
            .array_ref()
            .unwrap()
            .iter()
            .map(|value| match value.as_ref() {
                Value::Vec2d(pair) => *pair,
                _ => panic!("pair"),
            })
            .collect()
    }
    #[test]
    fn cpp_sequence_and_manifest_fixture() {
        let (mut store, path, a, b, options) = fixture();
        let before = a.clone();
        let sources = [
            ClipSource {
                layer: &a,
                asset_path: "./a.usda",
                offset: LayerOffset::IDENTITY,
            },
            ClipSource {
                layer: &b,
                asset_path: "./b.usda",
                offset: LayerOffset::IDENTITY,
            },
        ];
        let bundle =
            stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths).unwrap();
        let meta = clip_metadata(&bundle, path, &store.tokens);
        assert_eq!(
            pairs(dictionary_value(meta, "active")),
            vec![[1., 0.], [3., 1.]]
        );
        assert_eq!(
            pairs(dictionary_value(meta, "times")),
            vec![[1., 1.], [2., 2.], [3., 3.], [4., 4.]]
        );
        assert_eq!(dictionary_value(meta, "primPath"), &Value::string("/Model"));
        assert_eq!(
            dictionary_value(meta, "manifestAssetPath"),
            &Value::Asset("./root.manifest.usda".into())
        );
        assert_eq!(bundle.root.sublayers[0].layer, options.topology_id);
        let name = store.tokens.lookup("animated").unwrap();
        let static_name = store.tokens.lookup("static").unwrap();
        let manifest = bundle.manifest.prims[&path].property(name).unwrap();
        assert_eq!(manifest.default, Some(Value::Float(7.)));
        assert!(manifest.time_samples.is_none());
        assert!(!manifest.custom);
        assert!(bundle.manifest.prims[&path].property(static_name).is_none());
        assert_eq!(
            bundle.topology.prims[&path]
                .property(static_name)
                .unwrap()
                .default,
            Some(Value::Int(42))
        );
        assert!(
            bundle.topology.prims[&path]
                .property(name)
                .unwrap()
                .time_samples
                .is_none()
        );
        assert_eq!(a, before);
        assert_eq!(
            clip_topology_name("root.usda"),
            Some("root.topology.usda".into())
        );
        assert_eq!(
            clip_manifest_name("root.usda"),
            Some("root.manifest.usda".into())
        );
        assert_eq!(clip_manifest_name("root"), None);
        let Value::TypedArray(TypedArray::Vec2d(_)) = dictionary_value(meta, "times") else {
            panic!("typed vec2d array")
        };
    }
    #[test]
    fn offsets_keep_asset_indices_and_map_local_time() {
        let (mut store, path, a, b, options) = fixture();
        let sources = [
            ClipSource {
                layer: &a,
                asset_path: "a",
                offset: LayerOffset {
                    offset: 20.,
                    scale: 2.,
                },
            },
            ClipSource {
                layer: &b,
                asset_path: "b",
                offset: LayerOffset::IDENTITY,
            },
        ];
        let bundle =
            stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths).unwrap();
        let meta = clip_metadata(&bundle, path, &store.tokens);
        assert_eq!(
            pairs(dictionary_value(meta, "active")),
            vec![[3., 1.], [22., 0.]]
        );
        assert_eq!(
            pairs(dictionary_value(meta, "times")),
            vec![[3., 3.], [4., 4.], [22., 1.], [24., 2.]]
        );
        assert_eq!(
            crate::get_field(
                &bundle.root.metadata,
                &store.tokens.lookup("startTimeCode").unwrap()
            ),
            Some(&FieldValue::Value(Value::Double(3.)))
        );
    }
    #[test]
    fn precise_failures_leave_sources_unchanged() {
        let (mut store, _, a, b, options) = fixture();
        let before = a.clone();
        let source = |offset| ClipSource {
            layer: &a,
            asset_path: "a",
            offset,
        };
        assert_eq!(
            stitch_clip_sequence(&[], &options, &mut store.tokens, &mut store.paths).unwrap_err(),
            ClipAuthoringError::EmptySequence
        );
        assert_eq!(
            stitch_clip_sequence(
                &[source(LayerOffset {
                    offset: 0.,
                    scale: 0.
                })],
                &options,
                &mut store.tokens,
                &mut store.paths
            )
            .unwrap_err(),
            ClipAuthoringError::InvalidOffset(a.id)
        );
        assert_eq!(
            stitch_clip_sequence(
                &[source(LayerOffset::IDENTITY), source(LayerOffset::IDENTITY)],
                &options,
                &mut store.tokens,
                &mut store.paths
            )
            .unwrap_err(),
            ClipAuthoringError::DuplicateActivation
        );
        let mut bad = options.clone();
        bad.topology_id = bad.root_id;
        assert_eq!(
            stitch_clip_sequence(
                &[source(LayerOffset::IDENTITY)],
                &bad,
                &mut store.tokens,
                &mut store.paths
            )
            .unwrap_err(),
            ClipAuthoringError::ConflictingLayerId(bad.root_id)
        );
        let sources = [
            source(LayerOffset {
                offset: 2.,
                scale: 1.,
            }),
            ClipSource {
                layer: &b,
                asset_path: "b",
                offset: LayerOffset::IDENTITY,
            },
        ];
        assert_eq!(
            stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths)
                .unwrap_err(),
            ClipAuthoringError::DuplicateActivation
        );
        assert_eq!(a, before);
    }
    #[test]
    fn manifest_keeps_variant_branches_and_descendants() {
        let (mut store, path, mut a, b, options) = fixture();
        let set = store.tokens.intern("v");
        let branch = store.tokens.intern("A");
        let unused = store.tokens.intern("B");
        let x = store.tokens.intern("x");
        let y = store.tokens.intern("y");
        let child_name = store.tokens.intern("Child");
        let child = store.path("/Model/Child");
        let animated = || {
            PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.)))
                .with_default(Value::Float(21.))
                .with_time_samples(vec![(1., Value::Float(2.))])
        };
        let mut branches = crate::HashMap::new();
        branches.insert(
            branch,
            crate::VariantSpec {
                properties: vec![crate::PropertyEntry {
                    name: x,
                    spec: animated().into(),
                }],
                authored_children: vec![child_name],
                ..crate::VariantSpec::default()
            },
        );
        branches.insert(
            unused,
            crate::VariantSpec {
                properties: vec![crate::PropertyEntry {
                    name: x,
                    spec: PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        false,
                        Value::Float(0.),
                    ))
                    .with_default(Value::Float(8.))
                    .into(),
                }],
                ..crate::VariantSpec::default()
            },
        );
        a.prims
            .get_mut(&path)
            .unwrap()
            .variant_sets
            .insert(set, VariantSetSpec { variants: branches });
        let mut child_spec = PrimSpec::def().with_property(y, animated());
        child_spec
            .outer_variant_sites
            .push(crate::spec_path::VariantSelectionSite {
                host_path: path,
                set,
                variant: branch,
            });
        a.insert_prim(child, child_spec);
        let manifest = generate_clip_manifest_from_layers(
            &[&a],
            path,
            LayerId(12),
            &mut store.tokens,
            &mut store.paths,
        )
        .unwrap();
        let parent = &manifest.prims[&path];
        assert!(parent.variant_sets[&set].variants.contains_key(&branch));
        assert!(!parent.variant_sets[&set].variants.contains_key(&unused));
        assert!(
            parent.variant_sets[&set].variants[&branch]
                .properties
                .iter()
                .any(|p| p.name == x)
        );
        let sites = [crate::spec_path::VariantSelectionSite {
            host_path: path,
            set,
            variant: branch,
        }];
        assert!(
            manifest
                .prim_spec_in(child, &sites)
                .unwrap()
                .property(y)
                .is_some()
        );
        let sources = [
            ClipSource {
                layer: &a,
                asset_path: "a",
                offset: LayerOffset::IDENTITY,
            },
            ClipSource {
                layer: &b,
                asset_path: "b",
                offset: LayerOffset::IDENTITY,
            },
        ];
        let bundle =
            stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths).unwrap();
        assert_eq!(
            bundle.manifest.prims[&path].variant_sets[&set].variants[&branch].properties[0]
                .spec
                .default,
            Some(Value::Float(21.))
        );
        assert_eq!(
            bundle
                .manifest
                .prim_spec_in(child, &sites)
                .unwrap()
                .property(y)
                .unwrap()
                .default,
            Some(Value::Float(21.))
        );
    }
    #[test]
    fn malformed_source_samples_are_rejected_before_topology_strips_them() {
        let (mut store, _, mut a, _, options) = fixture();
        let outside = store.path("/Outside");
        let attribute = store.tokens.intern("value");
        let mut property =
            PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.)));
        property.time_samples = Some(vec![(f64::NAN, Value::Float(1.))].into());
        a.insert_prim(outside, PrimSpec::def().with_property(attribute, property));
        let source = ClipSource {
            layer: &a,
            asset_path: "a",
            offset: LayerOffset::IDENTITY,
        };
        assert_eq!(
            stitch_clip_sequence(&[source], &options, &mut store.tokens, &mut store.paths)
                .unwrap_err(),
            ClipAuthoringError::Stitch(StitchError::InvalidTimeSamples)
        );
    }
    #[test]
    fn overlapping_incompatible_offsets_are_rejected() {
        let (mut store, _, mut a, mut b, options) = fixture();
        let start = store.tokens.lookup("startTimeCode").unwrap();
        let end = store.tokens.lookup("endTimeCode").unwrap();
        for layer in [&mut a, &mut b] {
            crate::set_field_vec(&mut layer.metadata, start, Value::Double(0.).into());
            crate::set_field_vec(&mut layer.metadata, end, Value::Double(10.).into());
        }
        let sources = [
            ClipSource {
                layer: &a,
                asset_path: "a",
                offset: LayerOffset::IDENTITY,
            },
            ClipSource {
                layer: &b,
                asset_path: "b",
                offset: LayerOffset {
                    offset: 5.,
                    scale: 1.,
                },
            },
        ];
        assert_eq!(
            stitch_clip_sequence(&sources, &options, &mut store.tokens, &mut store.paths)
                .unwrap_err(),
            ClipAuthoringError::ConflictingTimeMapping
        );
    }
    #[test]
    fn samples_dominate_spline_annotations_across_clips() {
        let (mut store, path, mut a, b, options) = fixture();
        let name = store.tokens.lookup("animated").unwrap();
        let property = a.prims.get_mut(&path).unwrap().property_mut(name).unwrap();
        property.time_samples = None;
        property.spline = Some(crate::spline::SplineData {
            data_type: crate::spline::SplineDataType::Float,
            default_curve_type: crate::spline::CurveType::Hermite,
            pre_extrapolation: crate::spline::Extrapolation::Linear,
            post_extrapolation: crate::spline::Extrapolation::Linear,
            loop_params: None,
            knots: Vec::new(),
        });
        let only = generate_clip_manifest_from_layers(
            &[&a],
            path,
            options.manifest_id,
            &mut store.tokens,
            &mut store.paths,
        )
        .unwrap();
        let annotation = only.prims[&path]
            .property(name)
            .unwrap()
            .spline
            .as_ref()
            .unwrap();
        assert!(annotation.knots.is_empty());
        assert_eq!(
            annotation.default_curve_type,
            crate::spline::CurveType::Bezier
        );
        for layers in [vec![&a, &b], vec![&b, &a]] {
            let manifest = generate_clip_manifest_from_layers(
                &layers,
                path,
                options.manifest_id,
                &mut store.tokens,
                &mut store.paths,
            )
            .unwrap();
            assert!(
                manifest.prims[&path]
                    .property(name)
                    .unwrap()
                    .spline
                    .is_none()
            );
        }
    }
}
