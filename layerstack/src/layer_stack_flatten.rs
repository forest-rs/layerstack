// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Consolidate raw sublayer opinions while preserving composition arcs.
//!
//! This operation owns layer-stack reduction; stage population, asset loading,
//! and output publication remain with the caller. Every authored variant and
//! inactive spec survives. Lists retain their edits rather than resolving against
//! an empty list. Unsupported historical list edits and sparse-array edits are
//! refused, so success never silently approximates them.
//!
//! AOUSD Core §9, §10.3.1, §12.2–§12.4; OpenUSD
//! `UsdUtilsFlattenLayerStack` / `UsdFlattenLayerStack` (`usd/flattenUtils.cpp`).

use crate::doc::VariantSelections;
use crate::listop::append_unique;
use crate::sort;
use crate::spec_path::VariantSelectionSite;
use crate::{
    CompositionError, FieldEntry, FieldValue, Layer, LayerId, LayerOffset, LayerStack,
    LayerStackIdentifier, LayerStore, ListOp, PathId, PrimSpec, PropertyEntry, Reference,
    Specifier, TokenId, TokenInterner, Value, VariantSets, VariantSpec,
};
use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::hash::Hash;

/// Evidence about a successful, exact layer-stack consolidation.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct LayerStackFlattenReport {
    /// Input layers in strength order, with accumulated time offsets.
    pub sources: Vec<(LayerId, LayerOffset)>,
    /// Asset occurrences passed to the caller's anchoring callback.
    pub anchored_assets: usize,
    /// Source and output spellings for host-owned asset registration. After
    /// publishing the output layer, a host can register each known loaded layer
    /// under the output layer and `output_path`, preserving clip lookups.
    pub asset_paths: Vec<LayerStackFlattenAssetPath>,
}

/// One anchored asset occurrence, retaining its input resolution context.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerStackFlattenAssetPath {
    /// The layer authoring the original path.
    pub source_layer: LayerId,
    /// Its original authored spelling.
    pub source_path: String,
    /// The spelling returned by the caller's asset callback.
    pub output_path: String,
    /// A loaded layer already registered for the source context, when known.
    /// Ordinary image/texture assets need no loaded layer registration.
    pub resolved_layer: Option<LayerId>,
}

/// Detached consolidated layer and the evidence describing its inputs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct FlattenedLayerStack {
    /// A layer using the input store's token/path domains. Insert it through
    /// the host's normal publication API when ready.
    pub layer: Layer,
    /// Input and asset-anchoring evidence.
    pub report: LayerStackFlattenReport,
}

/// A consolidation requirement which cannot be satisfied exactly.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum LayerStackFlattenError {
    /// A requested input layer is missing.
    MissingLayer(LayerId),
    /// Recursive gathering encountered unresolved sublayers or a cycle.
    Gather(Vec<CompositionError>),
    /// A source has an invalid or unsupported timeline transformation.
    InvalidOffset(LayerId),
    /// A feature has no exact supported reduction.
    Unsupported {
        /// Layer containing the unsupported feature.
        layer: LayerId,
        /// Stable, human-readable feature name.
        feature: &'static str,
    },
    /// The asset callback could not anchor an authored path.
    Asset {
        /// Layer authoring the asset path.
        layer: LayerId,
        /// Authored asset path.
        path: String,
    },
    /// Attribute and relationship specs share one authored property path.
    PropertyKindMismatch,
    /// Local property declarations would change after strength reduction.
    PropertyDeclarationMismatch {
        /// Property whose custom or variability declarations conflict.
        property: TokenId,
    },
    /// Authored or mapped sample times are non-finite, unsorted, or duplicated.
    InvalidSamples,
}
impl core::fmt::Display for LayerStackFlattenError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "layer-stack consolidation refused: {self:?}")
    }
}
impl core::error::Error for LayerStackFlattenError {}

/// Consolidates the complete recursive local layer stack into one detached layer.
///
/// `resolve_asset` receives the authoring layer and each nonempty asset path and
/// returns its spelling anchored for the output destination, or `None` to refuse
/// consolidation. The callback may retain absolute/search paths or deliberately
/// rebase relative paths. This function performs no I/O. All inputs and the output
/// share the store's interners; the output ID must be distinct from every input.
///
/// References, payloads, inherits, specializes, variant selections, unselected
/// variants, inactive specs, and relocates remain authored. Dictionaries merge
/// recursively; scalar fields and complete sample maps use the strongest opinion.
/// Strong defaults mask weaker sample/spline fields. Time samples, timecode values,
/// splines and arc offsets are remapped through accumulated sublayer/rate offsets.
/// Deleted arc identifiers keep their original offsets until external composition.
/// Layer metadata reduces by field strength, matching the native utility, rather
/// than expanding its start/end range as stitching does.
///
/// # Errors
///
/// Refuses missing/cyclic inputs, invalid offsets/samples, property-kind conflicts,
/// mixed property declarations that would change composed custom/variability,
/// historical `add`/`reorder` list operations (`variantSetNames` among them),
/// sparse-array edits, asset expressions, empty authored sample maps, and path
/// expressions referring to a weaker opinion. Clip assets and active/times schedules are anchored and retimed;
/// clip dictionaries at one spec must share a timeline. Retimed template clips
/// are refused. Deferred timecode arrays with nonidentity offsets are refused
/// instead of hiding decoding failures. Inputs are never modified.
///
/// Spec: AOUSD Core §9, §12.2.5–§12.2.6, §12.3.2.1; native
/// `UsdFlattenLayerStack`. Unlike its historical approximation, this API refuses
/// legacy list operations. If composing the detached output directly in this
/// store, register known assets from the report under its new layer ID; persisted
/// output can instead be reloaded through the ordinary importer.
#[doc(alias = "UsdUtilsFlattenLayerStack")]
pub fn flatten_layer_stack(
    store: &dyn LayerStore,
    identifier: LayerStackIdentifier,
    output_id: LayerId,
    resolve_asset: &mut impl FnMut(LayerId, &str) -> Option<String>,
) -> Result<FlattenedLayerStack, LayerStackFlattenError> {
    let mut errors = Vec::new();
    let stack = LayerStack::gather_recording(store, &[identifier], &mut errors, None);
    // A layer the store does not hold is refused below, by id, as
    // `LayerStackFlattenError::MissingLayer`.
    errors.retain(|error| !matches!(error, CompositionError::MissingLayer(_)));
    if !errors.is_empty() {
        return Err(LayerStackFlattenError::Gather(errors));
    }
    let mut context = Context {
        tokens: store.tokens(),
        store,
        output: output_id,
        resolve_asset,
        report: LayerStackFlattenReport::default(),
        origins: Vec::new(),
        clip_timelines: Vec::new(),
        asset_layers: Vec::new(),
        deferred_assets: Vec::new(),
    };
    let mut result = Layer::new(output_id);
    for (&id, &offset) in stack.layers.iter().zip(&stack.offsets) {
        let input = store
            .layer(id)
            .ok_or(LayerStackFlattenError::MissingLayer(id))?;
        if id == output_id {
            return Err(unsupported(id, "output ID aliases an input layer"));
        }
        if !offset.offset.is_finite() || !offset.scale.is_finite() || offset.scale <= 0.0 {
            return Err(LayerStackFlattenError::InvalidOffset(id));
        }
        context.report.sources.push((id, offset));
        let mut layer = input.clone();
        context.fields(&mut layer.metadata, id, offset, None)?;
        merge_fields(&mut result.metadata, &layer.metadata);
        fill(&mut result.default_prim, &layer.default_prim);
        if let Some(relocates) = &layer.relocates {
            result
                .relocates
                .get_or_insert_with(Vec::new)
                .extend_from_slice(relocates);
        }
        let mut paths: Vec<_> = layer
            .prims
            .keys()
            .chain(layer.variant_prims.keys())
            .copied()
            .collect();
        sort::sort_unstable(&mut paths);
        paths.dedup();
        for path in paths {
            for source in layer.prim_specs(path) {
                let mut spec = source.clone();
                let key = Site {
                    path,
                    outer: spec.outer_variant_sites().to_vec(),
                    nested: Vec::new(),
                };
                context.prepare_prim(&mut spec, &key, id, offset)?;
                let old = result
                    .prim_specs(path)
                    .find(|old| old.outer_variant_sites() == spec.outer_variant_sites())
                    .cloned();
                if let Some(mut old) = old {
                    merge_prim(&mut old, &spec)?;
                    result.insert_prim(path, old);
                } else {
                    result.insert_prim(path, spec);
                }
            }
        }
    }
    for (&path, spec) in &mut result.prims {
        let key = Site {
            path,
            outer: spec.outer_variant_sites().to_vec(),
            nested: Vec::new(),
        };
        context.finish_prim(spec, &key);
    }
    for (&path, specs) in &mut result.variant_prims {
        for spec in specs {
            let key = Site {
                path,
                outer: spec.outer_variant_sites().to_vec(),
                nested: Vec::new(),
            };
            context.finish_prim(spec, &key);
        }
    }
    Ok(FlattenedLayerStack {
        layer: result,
        report: context.report,
    })
}

fn unsupported(layer: LayerId, feature: &'static str) -> LayerStackFlattenError {
    LayerStackFlattenError::Unsupported { layer, feature }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Site {
    path: PathId,
    outer: Vec<VariantSelectionSite>,
    nested: Vec<(TokenId, TokenId)>,
}
struct Origin {
    site: Site,
    payload: bool,
    reference: Reference,
    offset: LayerOffset,
}
struct Context<'a, F> {
    tokens: &'a TokenInterner,
    store: &'a dyn LayerStore,
    output: LayerId,
    resolve_asset: &'a mut F,
    report: LayerStackFlattenReport,
    origins: Vec<Origin>,
    clip_timelines: Vec<(Site, LayerOffset)>,
    asset_layers: Vec<(String, LayerId)>,
    /// The output asset paths of deferred arcs whose asset the store does
    /// not resolve: they stay deferred, anchored to the output layer.
    deferred_assets: Vec<String>,
}
impl<F: FnMut(LayerId, &str) -> Option<String>> Context<'_, F> {
    fn asset(&mut self, layer: LayerId, path: &str) -> Result<String, LayerStackFlattenError> {
        if path.is_empty() {
            return Ok(String::new());
        }
        if crate::variable_expression::is_expression(path) {
            return Err(unsupported(layer, "asset expressions"));
        }
        let mapped =
            (self.resolve_asset)(layer, path).ok_or_else(|| LayerStackFlattenError::Asset {
                layer,
                path: path.into(),
            })?;
        if mapped.is_empty() {
            return Err(unsupported(layer, "asset callback removed a nonempty path"));
        }
        self.report.anchored_assets += 1;
        self.report.asset_paths.push(LayerStackFlattenAssetPath {
            source_layer: layer,
            source_path: path.into(),
            output_path: mapped.clone(),
            resolved_layer: self.store.asset_layer(layer, path),
        });
        Ok(mapped)
    }
    fn value(
        &mut self,
        value: &mut Value,
        layer: LayerId,
        offset: LayerOffset,
    ) -> Result<(), LayerStackFlattenError> {
        match value {
            Value::Asset(path) => {
                *path = self.asset(layer, path)?.into();
            }
            Value::Array(values) => {
                for value in values {
                    self.value(value, layer, offset)?;
                }
            }
            Value::Dictionary(entries) => {
                for (_, value) in entries {
                    self.value(value, layer, offset)?;
                }
            }
            Value::ArrayEdit(_) | Value::TypedArrayEdit(_) => {
                return Err(unsupported(layer, "sparse array edits"));
            }
            Value::TypedArray(array)
                if !offset.is_identity()
                    && matches!(array, crate::TypedArray::Deferred(_))
                    && matches!(array.element_kind(), Value::TimeCode(_)) =>
            {
                return Err(unsupported(layer, "deferred timecode retiming"));
            }
            Value::PathExpression(text) => {
                if crate::path_expression::PathExpression::parse(text)
                    .map_or(true, |expression| expression.contains_weaker_reference())
                {
                    return Err(unsupported(
                        layer,
                        "invalid or weaker-referencing path expressions",
                    ));
                }
            }
            Value::TimeCode(time) => {
                *time = *time * offset.scale + offset.offset;
            }
            Value::TypedArray(_) if !offset.is_identity() => {
                if let Some(mapped) = crate::stage::stage_time::retime_value(value, offset) {
                    *value = mapped;
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn fields(
        &mut self,
        fields: &mut [FieldEntry],
        layer: LayerId,
        offset: LayerOffset,
        site: Option<&Site>,
    ) -> Result<(), LayerStackFlattenError> {
        for field in fields {
            if self.tokens.resolve(field.name) == "clips" {
                let site = site
                    .ok_or_else(|| unsupported(layer, "clip dictionaries outside prim metadata"))?;
                if self
                    .clip_timelines
                    .iter()
                    .any(|(prior, timeline)| prior == site && *timeline != offset)
                {
                    return Err(unsupported(
                        layer,
                        "clip dictionaries from different timelines",
                    ));
                }
                if !self.clip_timelines.iter().any(|(prior, _)| prior == site) {
                    self.clip_timelines.push((site.clone(), offset));
                }
                if let FieldValue::Value(value) = &mut field.value {
                    self.clips(value, layer, offset)?;
                }
            }
            match &mut field.value {
                FieldValue::Value(value) => self.value(value, layer, offset)?,
                FieldValue::TokenListOp(list) => validate_list(list, layer)?,
                FieldValue::PathListOp(list) => validate_list(list, layer)?,
                FieldValue::StringListOp(list) => validate_list(list, layer)?,
                FieldValue::IntListOp(list) => validate_list(list, layer)?,
                FieldValue::UIntListOp(list) => validate_list(list, layer)?,
                FieldValue::Int64ListOp(list) => validate_list(list, layer)?,
                FieldValue::UInt64ListOp(list) => validate_list(list, layer)?,
                FieldValue::UnregisteredListOp(list) => validate_list(list, layer)?,
            }
        }
        Ok(())
    }
    fn clips(
        &mut self,
        clips: &mut Value,
        layer: LayerId,
        offset: LayerOffset,
    ) -> Result<(), LayerStackFlattenError> {
        let Value::Dictionary(sets) = clips else {
            return Ok(());
        };
        for (_, set) in sets {
            let Value::Dictionary(entries) = set else {
                return Err(unsupported(layer, "malformed clip dictionary"));
            };
            for (key, value) in entries {
                if key.as_ref() == "templateAssetPath" {
                    if !offset.is_identity() {
                        return Err(unsupported(layer, "retimed template clips"));
                    }
                    if let Value::String(path) = value {
                        *path = self.asset(layer, path)?.into();
                    }
                }
                if !offset.is_identity() && matches!(key.as_ref(), "active" | "times") {
                    let mut schedule = match value {
                        Value::Array(entries) => entries.clone(),
                        Value::TypedArray(array) => array
                            .try_materialize()
                            .map_err(|_| unsupported(layer, "clip schedule decode failure"))?
                            .as_vec2d()
                            .ok_or_else(|| unsupported(layer, "malformed clip schedule"))?
                            .iter()
                            .copied()
                            .map(Value::Vec2d)
                            .collect(),
                        _ => return Err(unsupported(layer, "malformed clip schedule")),
                    };
                    for entry in &mut schedule {
                        let Value::Vec2d(pair) = entry else {
                            return Err(unsupported(layer, "malformed clip schedule"));
                        };
                        pair[0] = pair[0] * offset.scale + offset.offset;
                        if !pair[0].is_finite() {
                            return Err(LayerStackFlattenError::InvalidOffset(layer));
                        }
                    }
                    *value = Value::Array(schedule);
                }
            }
        }
        Ok(())
    }
    fn arcs(
        &mut self,
        list: &mut ListOp<Reference>,
        site: &Site,
        payload: bool,
        layer: LayerId,
        offset: LayerOffset,
    ) -> Result<(), LayerStackFlattenError> {
        validate_list(list, layer)?;
        for reference in list
            .explicit
            .iter_mut()
            .flatten()
            .chain(&mut list.prepend)
            .chain(&mut list.append)
            .chain(&mut list.delete)
        {
            let mapped_offset = offset.compose(reference.layer_offset);
            if !mapped_offset.offset.is_finite()
                || !mapped_offset.scale.is_finite()
                || mapped_offset.scale <= 0.0
            {
                return Err(LayerStackFlattenError::InvalidOffset(layer));
            }
            if let Some(path) = &reference.asset {
                let mapped = self.asset(layer, path)?;
                // A deferred arc's layer is the layer that authors it; its
                // target is the layer the store records for its asset.
                let target = reference
                    .stored_target_layer(self.store)
                    .unwrap_or(LayerId::UNRESOLVED);
                if reference.deferred
                    && target == LayerId::UNRESOLVED
                    && !self.deferred_assets.contains(&mapped)
                {
                    self.deferred_assets.push(mapped.clone());
                }
                if let Some((_, canonical)) = self
                    .asset_layers
                    .iter_mut()
                    .find(|(asset, _)| *asset == mapped)
                {
                    if self.store.layer(*canonical).is_none() && self.store.layer(target).is_some()
                    {
                        *canonical = target;
                    }
                } else {
                    self.asset_layers.push((mapped.clone(), target));
                }
                // Loaded hints do not participate in native authored identity.
                // Restore the canonical loaded layer after list reduction.
                reference.layer = LayerId::UNRESOLVED;
                reference.deferred = false;
                reference.asset = Some(mapped);
            } else if reference.layer == layer {
                reference.layer = self.output;
            }
            for (_, value) in &mut reference.custom_data {
                self.value(value, layer, LayerOffset::IDENTITY)?;
            }
            if !self.origins.iter().any(|origin| {
                origin.site == *site && origin.payload == payload && origin.reference == *reference
            }) {
                self.origins.push(Origin {
                    site: site.clone(),
                    payload,
                    reference: reference.clone(),
                    offset,
                });
            }
        }
        Ok(())
    }
    fn properties(
        &mut self,
        properties: &mut [PropertyEntry],
        layer: LayerId,
        offset: LayerOffset,
    ) -> Result<(), LayerStackFlattenError> {
        for entry in properties {
            let property = Arc::make_mut(&mut entry.spec);
            if let Some(default) = &mut property.default {
                self.value(default, layer, offset)?;
            }
            if let Some(samples) = property.time_samples.get_mut() {
                if samples.iter().any(|(time, _)| !time.is_finite())
                    || samples
                        .windows(2)
                        .any(|samples| samples[0].0 >= samples[1].0)
                {
                    return Err(LayerStackFlattenError::InvalidSamples);
                }
                if samples.is_empty() {
                    return Err(unsupported(layer, "empty authored sample maps"));
                }
                for (time, value) in samples.iter_mut() {
                    *time = *time * offset.scale + offset.offset;
                    if !time.is_finite() {
                        return Err(LayerStackFlattenError::InvalidSamples);
                    }
                    self.value(value, layer, offset)?;
                }
                if samples.windows(2).any(|p| p[0].0 >= p[1].0) {
                    return Err(LayerStackFlattenError::InvalidSamples);
                }
            }
            if let Some(spline) = &mut property.spline {
                **spline = spline
                    .retimed(offset)
                    .ok_or(LayerStackFlattenError::InvalidOffset(layer))?;
                if spline.knots.iter().any(|knot| !knot.time.is_finite())
                    || spline.knots.windows(2).any(|p| p[0].time >= p[1].time)
                {
                    return Err(LayerStackFlattenError::InvalidOffset(layer));
                }
                for knot in &mut spline.knots {
                    for (_, value) in &mut knot.custom_data {
                        self.value(value, layer, offset)?;
                    }
                }
            }
            if let Some(targets) = &property.targets {
                validate_list(targets, layer)?;
            }
            self.fields(property.metadata.make_mut(), layer, offset, None)?;
        }
        Ok(())
    }
    fn prepare_prim(
        &mut self,
        spec: &mut PrimSpec,
        site: &Site,
        layer: LayerId,
        offset: LayerOffset,
    ) -> Result<(), LayerStackFlattenError> {
        if spec
            .type_name
            .is_some_and(|name| self.tokens.resolve(name).is_empty())
        {
            spec.type_name = None;
        }
        self.fields(&mut spec.fields, layer, offset, Some(site))?;
        self.properties(&mut spec.properties, layer, offset)?;
        if let Some(op) = spec.references.get_mut() {
            self.arcs(op, site, false, layer, offset)?;
        }
        if let Some(op) = spec.payloads.get_mut() {
            self.arcs(op, site, true, layer, offset)?;
        }
        validate_list(spec.inherits(), layer)?;
        validate_list(spec.specializes(), layer)?;
        validate_list(spec.variant_set_names(), layer)?;
        match spec.authored_rare_mut() {
            Some(rare) => self.prepare_variants(&mut rare.variant_sets, site, layer, offset),
            None => Ok(()),
        }
    }
    fn prepare_variants(
        &mut self,
        sets: &mut VariantSets,
        site: &Site,
        layer: LayerId,
        offset: LayerOffset,
    ) -> Result<(), LayerStackFlattenError> {
        for (&set, variants) in sets {
            for (&variant, spec) in &mut variants.variants {
                let mut site = site.clone();
                site.nested.push((set, variant));
                self.fields(&mut spec.fields, layer, offset, Some(&site))?;
                self.properties(&mut spec.properties, layer, offset)?;
                if let Some(op) = spec.references.get_mut() {
                    self.arcs(op, &site, false, layer, offset)?;
                }
                if let Some(op) = spec.payloads.get_mut() {
                    self.arcs(op, &site, true, layer, offset)?;
                }
                validate_list(&spec.inherits, layer)?;
                validate_list(&spec.specializes, layer)?;
                validate_list(&spec.variant_set_names, layer)?;
                self.prepare_variants(&mut spec.variant_sets, &site, layer, offset)?;
            }
        }
        Ok(())
    }
    fn finish_arcs(&self, list: &mut ListOp<Reference>, site: &Site, payload: bool) {
        // Native `_ApplyLayerOffsetToRefOrPayloadListOp`: defer retiming until
        // reduction, and leave deleted identifiers unchanged.
        for reference in list
            .explicit
            .iter_mut()
            .flatten()
            .chain(&mut list.prepend)
            .chain(&mut list.append)
        {
            if let Some(origin) = self.origins.iter().find(|origin| {
                origin.site == *site && origin.payload == payload && origin.reference == *reference
            }) {
                reference.layer_offset = origin.offset.compose(reference.layer_offset);
            }
        }
        for reference in list
            .explicit
            .iter_mut()
            .flatten()
            .chain(&mut list.prepend)
            .chain(&mut list.append)
            .chain(&mut list.delete)
        {
            if let Some(asset) = &reference.asset
                && let Some((_, layer)) = self.asset_layers.iter().find(|(path, _)| path == asset)
            {
                reference.layer = *layer;
                // An asset no loaded layer stands for stays for composition
                // to resolve, now relative to the output layer
                // (`Reference::deferred`).
                if *layer == LayerId::UNRESOLVED && self.deferred_assets.contains(asset) {
                    reference.layer = self.output;
                    reference.deferred = true;
                }
            }
        }
    }
    fn children(&self, site: &Site) -> Vec<TokenId> {
        let mut children = Vec::new();
        for (layer, _) in self.report.sources.iter().rev() {
            let Some(prim) = self.store.layer(*layer).and_then(|layer| {
                layer
                    .prim_specs(site.path)
                    .find(|spec| spec.outer_variant_sites() == site.outer)
            }) else {
                continue;
            };
            if site.nested.is_empty() {
                append_unique(&mut children, &prim.authored_children);
                if let Some(order) = prim.prim_order() {
                    children = ListOp::default()
                        .with_reordered(order.to_vec())
                        .apply_to(&children);
                }
            } else {
                let mut sets = prim.variant_sets();
                let mut found = None;
                for (set, variant) in &site.nested {
                    found = sets.get(set).and_then(|set| set.variants.get(variant));
                    let Some(spec) = found else {
                        break;
                    };
                    sets = &spec.variant_sets;
                }
                if let Some(spec) = found {
                    append_unique(&mut children, &spec.authored_children);
                }
            }
        }
        children
    }
    fn finish_prim(&self, spec: &mut PrimSpec, site: &Site) {
        // Native `PcpComposeSiteChildNames`: union and order weakest first.
        spec.authored_children = self.children(site);
        if let Some(op) = spec.references.get_mut() {
            self.finish_arcs(op, site, false);
        }
        if let Some(op) = spec.payloads.get_mut() {
            self.finish_arcs(op, site, true);
        }
        if let Some(rare) = spec.authored_rare_mut() {
            self.finish_variants(&mut rare.variant_sets, site);
        }
    }
    fn finish_variants(&self, sets: &mut VariantSets, site: &Site) {
        for (&set, variants) in sets {
            for (&variant, spec) in &mut variants.variants {
                let mut site = site.clone();
                site.nested.push((set, variant));
                spec.authored_children = self.children(&site);
                if let Some(op) = spec.references.get_mut() {
                    self.finish_arcs(op, &site, false);
                }
                if let Some(op) = spec.payloads.get_mut() {
                    self.finish_arcs(op, &site, true);
                }
                self.finish_variants(&mut spec.variant_sets, &site);
            }
        }
    }
}
fn validate_list<T>(list: &ListOp<T>, layer: LayerId) -> Result<(), LayerStackFlattenError> {
    if !list.add.is_empty() || !list.reorder.is_empty() {
        return Err(unsupported(layer, "add/reorder list operations"));
    }
    Ok(())
}
fn fill<T: Clone>(strong: &mut Option<T>, weak: &Option<T>) {
    if strong.is_none() {
        strong.clone_from(weak);
    }
}
/// Compose edits as functions on an arbitrary weaker list, without collapsing it.
/// AOUSD Core §12.4; native `SdfListOp::ApplyOperations(listOp)`. Preparation
/// rejects added and reordered items (`validate_list`), so the edits compose.
fn list<T: Clone + Eq + Hash>(strong: &ListOp<T>, weak: &ListOp<T>) -> ListOp<T> {
    strong
        .compose_over(weak)
        .expect("prepared list ops have no added or reordered items")
}
fn merge_fields(strong: &mut Vec<FieldEntry>, weak: &[FieldEntry]) {
    for field in weak {
        if let Some(existing) = strong
            .iter_mut()
            .find(|existing| existing.name == field.name)
        {
            match (&mut existing.value, &field.value) {
                (
                    FieldValue::Value(Value::Dictionary(a)),
                    FieldValue::Value(Value::Dictionary(b)),
                ) => *a = crate::combine_dictionaries(a, b),
                (FieldValue::TokenListOp(a), FieldValue::TokenListOp(b)) => **a = list(a, b),
                (FieldValue::PathListOp(a), FieldValue::PathListOp(b)) => **a = list(a, b),
                (FieldValue::StringListOp(a), FieldValue::StringListOp(b)) => **a = list(a, b),
                (FieldValue::IntListOp(a), FieldValue::IntListOp(b)) => **a = list(a, b),
                (FieldValue::UIntListOp(a), FieldValue::UIntListOp(b)) => **a = list(a, b),
                (FieldValue::Int64ListOp(a), FieldValue::Int64ListOp(b)) => **a = list(a, b),
                (FieldValue::UInt64ListOp(a), FieldValue::UInt64ListOp(b)) => **a = list(a, b),
                _ => {}
            }
        } else {
            strong.push(field.clone());
        }
    }
}
fn merge_properties(
    strong: &mut Vec<PropertyEntry>,
    weak: &[PropertyEntry],
) -> Result<(), LayerStackFlattenError> {
    for entry in weak {
        if let Some(existing) = strong
            .iter_mut()
            .find(|existing| existing.name == entry.name)
        {
            let a = Arc::make_mut(&mut existing.spec);
            let b = &entry.spec;
            if a.kind != b.kind {
                return Err(LayerStackFlattenError::PropertyKindMismatch);
            }
            // Native flattening takes strongest declaration fields, unlike
            // composed custom (OR) and variability (weakest). Refuse the cases
            // where that reduction changes the resulting declaration.
            if (!a.custom && b.custom) || a.variability != b.variability {
                return Err(LayerStackFlattenError::PropertyDeclarationMismatch {
                    property: entry.name,
                });
            }
            fill(&mut a.type_name, &b.type_name);
            // Flatten retains a complete strongest animation field; default
            // opinions mask weaker animation, unlike stitching's sample union.
            if !a.time_samples.is_authored() && a.spline.is_none() && a.default.is_none() {
                a.time_samples.clone_from(&b.time_samples);
                a.spline.clone_from(&b.spline);
            }
            if matches!(a.default, Some(Value::AnimationBlock)) {
                if let Some(default) = &b.default {
                    a.default = Some(default.clone());
                }
            } else if let (Some(Value::Dictionary(strong)), Some(Value::Dictionary(weak))) =
                (&mut a.default, &b.default)
            {
                *strong = crate::combine_dictionaries(strong, weak);
            } else {
                fill(&mut a.default, &b.default);
            }
            if let Some(targets) = &b.targets {
                a.targets = Some(
                    a.targets
                        .as_ref()
                        .map_or_else(|| targets.clone(), |strong| Box::new(list(strong, targets))),
                );
            }
            merge_fields(a.metadata.make_mut(), &b.metadata);
        } else {
            strong.push(entry.clone());
        }
    }
    Ok(())
}
fn merge_selections(strong: &mut VariantSelections, weak: &VariantSelections) {
    for (set, variant) in weak {
        strong.as_map_mut().entry(set).or_insert(variant);
    }
}
fn merge_variants(
    strong: &mut VariantSets,
    weak: &VariantSets,
) -> Result<(), LayerStackFlattenError> {
    for (&set, variants) in weak {
        let target = strong.get_or_insert_default(set);
        for (&variant, source) in &variants.variants {
            let target = target.variants.get_or_insert_default(variant);
            merge_variant(target, source)?;
        }
    }
    Ok(())
}
fn merge_variant(a: &mut VariantSpec, b: &VariantSpec) -> Result<(), LayerStackFlattenError> {
    merge_fields(&mut a.fields, &b.fields);
    merge_properties(&mut a.properties, &b.properties)?;
    append_unique(&mut a.authored_children, &b.authored_children);
    a.variant_set_names = list(&a.variant_set_names, &b.variant_set_names).into();
    a.references = list(&a.references, &b.references).into();
    a.payloads = list(&a.payloads, &b.payloads).into();
    a.inherits = list(&a.inherits, &b.inherits).into();
    a.specializes = list(&a.specializes, &b.specializes).into();
    merge_selections(&mut a.variant_selections, &b.variant_selections);
    fill(&mut a.property_order, &b.property_order);
    merge_variants(&mut a.variant_sets, &b.variant_sets)
}
fn merge_prim(a: &mut PrimSpec, b: &PrimSpec) -> Result<(), LayerStackFlattenError> {
    if a.specifier.is_none() || a.specifier == Some(Specifier::Over) {
        a.specifier = b.specifier;
    }
    if a.type_name.is_none() {
        a.type_name = b.type_name;
    }
    fill(&mut a.active, &b.active);
    fill(&mut a.instanceable, &b.instanceable);
    merge_fields(&mut a.fields, &b.fields);
    merge_properties(&mut a.properties, &b.properties)?;
    append_unique(&mut a.authored_children, &b.authored_children);
    a.references = list(&a.references, &b.references).into();
    a.payloads = list(&a.payloads, &b.payloads).into();
    let Some(b) = b.rare() else {
        return Ok(());
    };
    let a = a.rare_mut();
    fill(&mut a.property_order, &b.property_order);
    fill(&mut a.prim_order, &b.prim_order);
    a.variant_set_names = list(&a.variant_set_names, &b.variant_set_names).into();
    a.inherits = list(&a.inherits, &b.inherits).into();
    a.specializes = list(&a.specializes, &b.specializes).into();
    merge_selections(&mut a.variant_selections, &b.variant_selections);
    merge_variants(&mut a.variant_sets, &b.variant_sets)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TimeSamples;
    use crate::{InMemoryStore, PropertySpec, SublayerEntry};
    use alloc::vec;

    #[test]
    fn composable_lists_preserve_action_on_arbitrary_weaker_bases() {
        let operations = [
            ListOp::default(),
            ListOp::prepended(vec![1, 2]),
            ListOp::appended(vec![2, 3]),
            ListOp::default().with_deleted(vec![1, 3]),
            ListOp::prepended(vec![1]).with_deleted(vec![2]),
            ListOp::explicit(vec![3, 1]),
        ];
        for strong in &operations {
            for weak in &operations {
                for base in [vec![], vec![0, 1, 2, 3, 4], vec![3, 2, 1]] {
                    assert_eq!(
                        list(strong, weak).apply_to(&base),
                        strong.apply_to(&weak.apply_to(&base))
                    );
                }
            }
        }
    }

    #[test]
    fn raw_specs_preserve_variants_inactive_arcs_and_strong_animation_fields() {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let child = store.path("/P/Hidden");
        let class = store.path("/Class");
        let x = store.tokens.intern("x");
        let y = store.tokens.intern("y");
        let v = store.tokens.intern("look");
        let a = store.tokens.intern("a");
        let b = store.tokens.intern("b");
        let mut weak = Layer::new(LayerId(2));
        weak.insert_prim(
            path,
            PrimSpec::def()
                .with_property(
                    x,
                    PropertySpec::attribute()
                        .with_time_samples(vec![(0., Value::Float(1.)), (2., Value::Float(2.))]),
                )
                .with_property(
                    y,
                    PropertySpec::attribute()
                        .with_time_samples(vec![(0., Value::Float(1.)), (2., Value::Float(2.))]),
                ),
        );
        weak.prims.get_mut(&path).unwrap().rare_mut().inherits =
            ListOp::prepended(vec![class]).into();
        let mut variant = VariantSpec::default();
        variant.properties.push(PropertyEntry {
            name: x,
            spec: Arc::new(PropertySpec::attribute().with_default(9)),
        });
        weak.prims
            .get_mut(&path)
            .unwrap()
            .rare_mut()
            .variant_sets
            .get_or_insert_default(v)
            .variants
            .insert(b, variant);
        weak.insert_prim(child, PrimSpec::def());
        weak.prims.get_mut(&child).unwrap().active = Some(false);
        let mut strong = Layer::new(LayerId(1));
        strong.sublayers.push(SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.,
                scale: 2.,
            },
        ));
        strong.insert_prim(
            path,
            PrimSpec::over()
                .with_property(x, PropertySpec::attribute().with_default(4))
                .with_property(
                    y,
                    PropertySpec::attribute().with_time_samples(vec![(1., Value::Float(8.))]),
                ),
        );
        strong
            .prims
            .get_mut(&path)
            .unwrap()
            .rare_mut()
            .variant_sets
            .get_or_insert_default(v)
            .variants
            .insert(a, VariantSpec::default());
        store.insert_layer(strong);
        store.insert_layer(weak);
        let result = flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| {
            Some(path.into())
        })
        .unwrap();
        assert!(result.layer.sublayers.is_empty());
        let spec = &result.layer.prims[&path];
        assert_eq!(spec.specifier, Some(Specifier::Def));
        assert_eq!(spec.inherits().prepend, vec![class]);
        assert_eq!(spec.variant_sets()[&v].variants.len(), 2);
        assert_eq!(result.layer.prims[&child].active, Some(false));
        let properties = &spec.properties;
        assert!(
            properties
                .iter()
                .find(|p| p.name == x)
                .unwrap()
                .spec
                .time_samples
                .get()
                .is_none()
        );
        assert_eq!(
            properties
                .iter()
                .find(|p| p.name == y)
                .unwrap()
                .spec
                .time_samples
                .get(),
            Some(&[(1., Value::Float(8.))][..])
        );
        assert_eq!(
            store.layers[&LayerId(2)].prims[&path].properties[0]
                .spec
                .time_samples
                .get()
                .unwrap()[0]
                .0,
            0.
        );
    }

    /// A deferred arc the store resolves flattens to its loaded layer; one
    /// it does not stays deferred, for composition to resolve relative to
    /// the output layer.
    #[test]
    fn deferred_arcs_stay_deferred_unless_the_store_resolves_them() {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let target = store.path("/Target");
        let deferred = |asset: &str| {
            Reference::deferred(
                LayerId(1),
                asset,
                crate::ReferenceTarget::Prim(target),
                LayerOffset::IDENTITY,
            )
        };
        let mut root = Layer::new(LayerId(1));
        let mut spec = PrimSpec::def();
        spec.references =
            ListOp::prepended(vec![deferred("./loaded.usda"), deferred("./later.usda")]).into();
        root.insert_prim(path, spec);
        store.insert_layer(root);
        store.insert_layer(Layer::new(LayerId(9)));
        store.insert_asset_layer(LayerId(1), "./loaded.usda", LayerId(9));
        let result = flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| {
            Some(alloc::format!("assets/{path}"))
        })
        .unwrap();
        let refs = &result.layer.prims[&path].references.prepend;
        assert_eq!(refs.len(), 2);
        assert_eq!(
            (refs[0].layer, refs[0].deferred, refs[0].asset.as_deref()),
            (LayerId(9), false, Some("assets/./loaded.usda"))
        );
        assert_eq!(
            (refs[1].layer, refs[1].deferred, refs[1].asset.as_deref()),
            (LayerId(3), true, Some("assets/./later.usda"))
        );
    }

    #[test]
    fn deleted_arc_identity_is_preserved_until_after_reduction() {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let target = store.path("/Target");
        let arc = Reference::with_asset(LayerId(8), target, "asset.usda");
        let mut strong = Layer::new(LayerId(1));
        strong.sublayers.push(SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.,
                scale: 2.,
            },
        ));
        let mut spec = PrimSpec::over();
        spec.references = ListOp::default().with_deleted(vec![arc.clone()]).into();
        strong.insert_prim(path, spec);
        let mut weak = Layer::new(LayerId(2));
        let mut spec = PrimSpec::def();
        spec.references = ListOp::prepended(vec![arc.clone()]).into();
        weak.insert_prim(path, spec);
        store.insert_layer(strong);
        store.insert_layer(weak);
        let result = flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| {
            Some(path.into())
        })
        .unwrap();
        let refs = &result.layer.prims[&path].references;
        assert!(refs.prepend.is_empty());
        assert_eq!(refs.delete, vec![arc]);
    }

    #[test]
    fn unsupported_edits_and_missing_inputs_are_refused() {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let target = store.path("/Class");
        let mut layer = Layer::new(LayerId(1));
        let mut spec = PrimSpec::def();
        spec.rare_mut().inherits.make_mut().add.push(target);
        layer.insert_prim(path, spec);
        store.insert_layer(layer);
        assert!(matches!(
            flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| Some(
                path.into()
            )),
            Err(LayerStackFlattenError::Unsupported {
                feature: "add/reorder list operations",
                ..
            })
        ));
        assert!(matches!(
            flatten_layer_stack(&store, LayerId(2).into(), LayerId(3), &mut |_, path| Some(
                path.into()
            )),
            Err(LayerStackFlattenError::MissingLayer(LayerId(2)))
        ));
    }

    #[test]
    fn sparse_edits_and_clip_dictionaries_with_mixed_timelines_are_refused() {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let name = store.tokens.intern("x");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            path,
            PrimSpec::def().with_property(
                name,
                PropertySpec::attribute().with_default(Value::ArrayEdit(crate::ArrayEdit {
                    ops: vec![crate::ArrayEditOp::Resize { len: 3 }],
                })),
            ),
        );
        store.insert_layer(layer);
        assert!(matches!(
            flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| Some(
                path.into()
            )),
            Err(LayerStackFlattenError::Unsupported {
                feature: "sparse array edits",
                ..
            })
        ));

        let clips = store.tokens.intern("clips");
        let mut strong = Layer::new(LayerId(1));
        strong.sublayers.push(SublayerEntry::with_offset(
            LayerId(2),
            LayerOffset {
                offset: 10.,
                scale: 2.,
            },
        ));
        let mut spec = PrimSpec::def();
        spec.fields.push(FieldEntry {
            name: clips,
            value: Value::Dictionary(vec![]).into(),
        });
        strong.insert_prim(path, spec);
        let mut weak = Layer::new(LayerId(2));
        let mut spec = PrimSpec::def();
        spec.fields.push(FieldEntry {
            name: clips,
            value: Value::Dictionary(vec![]).into(),
        });
        weak.insert_prim(path, spec);
        store.insert_layer(strong);
        store.insert_layer(weak);
        assert!(matches!(
            flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| Some(
                path.into()
            )),
            Err(LayerStackFlattenError::Unsupported {
                feature: "clip dictionaries from different timelines",
                ..
            })
        ));
    }
    #[test]
    fn declaration_changes_are_refused_instead_of_silently_adopting_strongest() {
        for change_variability in [false, true] {
            let mut store = InMemoryStore::default();
            let path = store.path("/P");
            let name = store.tokens.intern("x");
            let mut strong = Layer::new(LayerId(1));
            strong.sublayers.push(SublayerEntry::new(LayerId(2)));
            strong.insert_prim(
                path,
                PrimSpec::def().with_property(name, PropertySpec::attribute()),
            );
            let mut weak = Layer::new(LayerId(2));
            let mut declaration = PropertySpec::attribute().with_default(10);
            if change_variability {
                declaration.variability = crate::Variability::Uniform;
            } else {
                declaration.custom = true;
            }
            weak.insert_prim(path, PrimSpec::over().with_property(name, declaration));
            store.insert_layer(strong);
            store.insert_layer(weak);
            assert!(
                matches!(flatten_layer_stack(&store, LayerId(1).into(), LayerId(3),
                &mut |_, path| Some(path.into())),
                Err(LayerStackFlattenError::PropertyDeclarationMismatch { property }) if property == name)
            );
        }
    }

    #[test]
    fn retiming_cannot_collapse_distinct_sample_or_spline_times() {
        for with_spline in [false, true] {
            let mut store = InMemoryStore::default();
            let path = store.path("/P");
            let name = store.tokens.intern("x");
            let mut strong = Layer::new(LayerId(1));
            strong.sublayers.push(SublayerEntry::with_offset(
                LayerId(2),
                LayerOffset {
                    offset: 1e16,
                    scale: 1.,
                },
            ));
            strong.insert_prim(path, PrimSpec::def());
            let mut weak = Layer::new(LayerId(2));
            let mut property = PropertySpec::attribute()
                .with_time_samples(vec![(0., Value::Double(0.)), (1., Value::Double(10.))]);
            if with_spline {
                let mut spline = crate::SplineData {
                    data_type: crate::spline::SplineDataType::Double,
                    default_curve_type: crate::spline::CurveType::Bezier,
                    pre_extrapolation: crate::spline::Extrapolation::Held,
                    post_extrapolation: crate::spline::Extrapolation::Held,
                    loop_params: None,
                    pre_loop_boundary: None,
                    post_loop_boundary: None,
                    knots: vec![],
                };
                for time in [0., 1.] {
                    spline.knots.push(crate::spline::Knot {
                        time,
                        value: time,
                        next_interp: crate::spline::KnotInterp::Linear,
                        curve_type: crate::spline::CurveType::Bezier,
                        pre_value: None,
                        custom_data: vec![],
                        pre_tan_algorithm: crate::spline::TangentAlgorithm::None,
                        post_tan_algorithm: crate::spline::TangentAlgorithm::None,
                        pre_tan_maya_form: false,
                        post_tan_maya_form: false,
                        pre_tan_width: 0.,
                        post_tan_width: 0.,
                        pre_tan_slope: 0.,
                        post_tan_slope: 0.,
                    });
                }
                property.time_samples = TimeSamples::none();
                property.spline = Some(Box::new(spline));
            }
            weak.insert_prim(path, PrimSpec::over().with_property(name, property));
            store.insert_layer(strong);
            store.insert_layer(weak);
            let before = store.layers[&LayerId(2)].clone();
            let error =
                flatten_layer_stack(&store, LayerId(1).into(), LayerId(3), &mut |_, path| {
                    Some(path.into())
                })
                .unwrap_err();
            if with_spline {
                assert_eq!(error, LayerStackFlattenError::InvalidOffset(LayerId(2)));
            } else {
                assert_eq!(error, LayerStackFlattenError::InvalidSamples);
            }
            assert_eq!(store.layers[&LayerId(2)], before);
        }
    }
}
