// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored asset discovery and copy-based path rewriting.
//!
//! Discovery visits every variant, including unselected branches, and asset
//! values in metadata, defaults, samples, dictionaries and sparse edit literals.
//! It does not compose, load files or evaluate resolver-specific patterns.
//! Hosts resolve expressions, UDIMs and clip templates through the explicit
//! callback in [`collect_dependencies`]. Source layers remain untouched.
//!
//! Spec: AOUSD Core §9 (asset resolution), §7.3.6 (variants), §10.3
//! (composition arcs). OpenUSD: `UsdUtilsExtractExternalReferences`,
//! `UsdUtilsComputeAllDependencies`, `UsdUtilsModifyAssetPaths`.

use crate::spec_path::VariantSelectionSite;
use crate::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, FieldEntry, FieldValue, Layer, LayerId, LayerStore,
    ListOp, PathId, PathInterner, PrimSpec, PropertyEntry, Reference, SpecPath, TokenId,
    TokenInterner, Value, VariantSetSpec,
};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::String,
    sync::Arc,
    vec::Vec,
};

/// The authored role of an external identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AssetDependencyKind {
    /// Sublayer include.
    Sublayer,
    /// Reference arc, including deleted or otherwise inactive list items.
    Reference,
    /// Payload arc, including unloaded payloads.
    Payload,
    /// Asset-typed value in scene description.
    Value,
    /// A `templateAssetPath` string inside clip metadata; a host expands it.
    ClipTemplate,
}

/// One authored asset use, retaining the layer that anchors relative paths.
/// Repeated identical uses in one field are coalesced; different specs and roles
/// remain separate. Handles belong to the supplied store's interners.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetDependency {
    /// Layer that authors this identifier.
    pub layer: LayerId,
    /// Variant-qualified prim/property spec, or `None` for layer metadata.
    pub spec: Option<SpecPath>,
    /// Metadata/property name, where applicable.
    pub field: Option<TokenId>,
    /// Authored identifier, before expression/pattern evaluation.
    pub identifier: Arc<str>,
    /// How the identifier was authored.
    pub kind: AssetDependencyKind,
    /// Imported arc target, if known; not proof that the layer is present.
    pub layer_hint: Option<LayerId>,
}

/// A host's resolution of one dependency (patterns can return many targets).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DependencyTarget {
    /// Loaded USD layer, whose own authored dependencies will be inspected.
    Layer(LayerId),
    /// Resolved non-layer asset identifier, such as an image or audio file.
    Asset(Arc<str>),
    /// The host could not resolve/load this use; retain a useful reason.
    Unresolved(Arc<str>),
    /// Explicitly excluded by host policy.
    Ignored,
}

/// The resolution of one authored use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedDependency {
    /// Authored source and identifier.
    pub source: AssetDependency,
    /// Host results, in host order. An empty result is treated as unresolved.
    pub targets: Vec<DependencyTarget>,
}

/// Recursive inventory without implicit I/O or resolver policy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencyReport {
    /// Loaded layers visited once, sorted by identifier.
    pub layers: Vec<LayerId>,
    /// Authored uses and their explicit host resolutions.
    pub dependencies: Vec<ResolvedDependency>,
    /// Referenced layer IDs not present in the store, sorted.
    pub missing_layers: Vec<LayerId>,
}

impl DependencyReport {
    /// Whether every discovered use was resolved or explicitly ignored.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.missing_layers.is_empty()
            && self.dependencies.iter().all(|d| {
                !d.targets
                    .iter()
                    .any(|t| matches!(t, DependencyTarget::Unresolved(_)))
            })
    }
    /// Unique resolved non-layer asset identifiers in lexical order.
    #[must_use]
    pub fn assets(&self) -> Vec<Arc<str>> {
        self.dependencies
            .iter()
            .flat_map(|d| &d.targets)
            .filter_map(|t| match t {
                DependencyTarget::Asset(a) => Some(a.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Lists external identifiers in one authored layer, without following them.
/// Plain strings are not assets, except the documented clip-template field.
/// Empty identifiers are retained only for unnamed external layer links. Ordering is deterministic for shared interners.
#[must_use]
pub fn extract_layer_dependencies(
    layer: &Layer,
    paths: &PathInterner,
    tokens: &TokenInterner,
) -> Vec<AssetDependency> {
    let mut out = Vec::new();
    for sub in &layer.sublayers {
        push(
            &mut out,
            layer.id,
            None,
            None,
            sub.asset.as_deref().unwrap_or(""),
            AssetDependencyKind::Sublayer,
            Some(sub.layer),
        );
    }
    fields(&layer.metadata, layer.id, None, &mut out, false);
    for (&path, prim) in &layer.prims {
        extract_prim(prim, path, layer.id, paths, tokens, &mut out);
    }
    for (&path, prims) in &layer.variant_prims {
        for prim in prims {
            extract_prim(prim, path, layer.id, paths, tokens, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Recursively inventories loaded layers and host-resolved external assets.
/// Each reachable layer is inspected once, including cycles. `resolve` receives
/// the exact authoring layer/spec even when an identifier occurs elsewhere.
/// A host may return multiple targets for UDIMs or clip templates. Resolution
/// never loads a layer implicitly: the host must already supply it in `store`.
#[must_use]
pub fn collect_dependencies(
    store: &dyn LayerStore,
    root: LayerId,
    mut resolve: impl FnMut(&AssetDependency) -> Vec<DependencyTarget>,
) -> DependencyReport {
    let mut report = DependencyReport::default();
    let mut pending = BTreeSet::from([root]);
    let mut visited = BTreeSet::new();
    while let Some(id) = pending.pop_first() {
        if !visited.insert(id) {
            continue;
        }
        let Some(layer) = store.layer(id) else {
            report.missing_layers.push(id);
            continue;
        };
        report.layers.push(id);
        // Unnamed in-memory arcs still establish dependencies. Named arcs use
        // host results: import-time IDs can be stale after an asset path edit.
        for sub in &layer.sublayers {
            if sub.asset.is_none() && sub.layer != id {
                pending.insert(sub.layer);
            }
        }
        for prim in layer
            .prims
            .values()
            .chain(layer.variant_prims.values().flatten())
        {
            for arc in prim.references.items().chain(prim.payloads.items()).chain(
                prim.variant_branches()
                    .flat_map(|b| b.spec.references.items().chain(b.spec.payloads.items())),
            ) {
                if arc.asset.is_none() && arc.layer != id {
                    pending.insert(arc.layer);
                }
            }
        }
        for source in extract_layer_dependencies(layer, store.paths(), store.tokens()) {
            let mut targets = resolve(&source);
            if targets.is_empty() {
                targets.push(DependencyTarget::Unresolved(
                    "resolver returned no targets".into(),
                ));
            }
            for target in &targets {
                if let DependencyTarget::Layer(layer) = target {
                    pending.insert(*layer);
                }
            }
            report
                .dependencies
                .push(ResolvedDependency { source, targets });
        }
    }
    report.layers.sort();
    report.missing_layers.sort();
    report.dependencies.sort_by(|a, b| a.source.cmp(&b.source));
    report
}

/// Rewrites a copy of a layer, with a callback for every distinct authored use.
/// A failed callback returns no modified layer. Empty replacements retain empty
/// asset values/array slots; arcs retain their authored target and time offset.
/// The caller supplies new imported target IDs when moving across stores.
///
/// No generation counters change in the source; the returned copy is touched
/// once if any identifier changed. Templates remain templates unless the host
/// rewrites them into another valid template.
pub fn rewrite_layer_assets<E>(
    layer: &Layer,
    paths: &PathInterner,
    tokens: &TokenInterner,
    mut rewrite: impl FnMut(&AssetDependency) -> Result<Arc<str>, E>,
) -> Result<Layer, E> {
    let mut replacements = BTreeMap::new();
    for dependency in extract_layer_dependencies(layer, paths, tokens) {
        replacements.insert(dependency.clone(), rewrite(&dependency)?);
    }
    let mut copy = layer.clone();
    for sub in &mut copy.sublayers {
        let old = sub.asset.as_deref().unwrap_or("");
        let key = dependency(
            layer.id,
            None,
            None,
            old,
            AssetDependencyKind::Sublayer,
            Some(sub.layer),
        );
        if let Some(new) = replacements.get(&key)
            && &**new != old
        {
            sub.asset = Some(String::from(&**new));
        }
    }
    rewrite_fields(&mut copy.metadata, layer.id, None, &replacements, false);
    for (&path, prim) in &mut copy.prims {
        rewrite_prim(prim, path, layer.id, paths, tokens, &replacements);
    }
    for (&path, prims) in &mut copy.variant_prims {
        for prim in prims {
            rewrite_prim(prim, path, layer.id, paths, tokens, &replacements);
        }
    }
    if replacements
        .iter()
        .any(|(dependency, new)| dependency.identifier != *new)
    {
        copy.touch();
    }
    Ok(copy)
}

fn dependency(
    layer: LayerId,
    spec: Option<&SpecPath>,
    field: Option<TokenId>,
    identifier: &str,
    kind: AssetDependencyKind,
    hint: Option<LayerId>,
) -> AssetDependency {
    AssetDependency {
        layer,
        spec: spec.cloned(),
        field,
        identifier: identifier.into(),
        kind,
        layer_hint: hint.filter(|id| *id != layer && *id != LayerId::UNRESOLVED),
    }
}
fn push(
    out: &mut Vec<AssetDependency>,
    layer: LayerId,
    spec: Option<&SpecPath>,
    field: Option<TokenId>,
    asset: &str,
    kind: AssetDependencyKind,
    hint: Option<LayerId>,
) {
    if !asset.is_empty() || hint.is_some_and(|id| id != layer && id != LayerId::UNRESOLVED) {
        out.push(dependency(layer, spec, field, asset, kind, hint));
    }
}
fn arcs(
    arcs: &ListOp<Reference>,
    kind: AssetDependencyKind,
    layer: LayerId,
    spec: &SpecPath,
    out: &mut Vec<AssetDependency>,
) {
    for arc in arcs.items() {
        push(
            out,
            layer,
            Some(spec),
            None,
            arc.asset.as_deref().unwrap_or(""),
            kind,
            Some(arc.layer),
        );
    }
}
fn fields(
    fields: &[FieldEntry],
    layer: LayerId,
    spec: Option<&SpecPath>,
    out: &mut Vec<AssetDependency>,
    clip: bool,
) {
    for field in fields {
        if let FieldValue::Value(value) = &field.value {
            values(value, layer, spec, Some(field.name), out, clip);
        }
    }
}
fn properties(
    props: &[PropertyEntry],
    layer: LayerId,
    spec: &SpecPath,
    out: &mut Vec<AssetDependency>,
) {
    for prop in props {
        let at = spec.with_property(prop.name);
        fields(&prop.spec.metadata, layer, Some(&at), out, false);
        if let Some(value) = &prop.spec.default {
            values(value, layer, Some(&at), Some(prop.name), out, false);
        }
        if let Some(samples) = &prop.spec.time_samples {
            for (_, value) in samples.iter() {
                values(value, layer, Some(&at), Some(prop.name), out, false);
            }
        }
    }
}
fn extract_prim(
    prim: &PrimSpec,
    path: PathId,
    layer: LayerId,
    paths: &PathInterner,
    tokens: &TokenInterner,
    out: &mut Vec<AssetDependency>,
) {
    let spec = SpecPath::from_variant_selection_sites(path, &prim.outer_variant_sites, paths);
    for field in &prim.fields {
        fields(
            core::slice::from_ref(field),
            layer,
            Some(&spec),
            out,
            tokens.resolve(field.name) == "clips",
        );
    }
    properties(&prim.properties, layer, &spec, out);
    arcs(
        &prim.references,
        AssetDependencyKind::Reference,
        layer,
        &spec,
        out,
    );
    arcs(
        &prim.payloads,
        AssetDependencyKind::Payload,
        layer,
        &spec,
        out,
    );
    for branch in prim.variant_branches() {
        let sites = branch.sites(&prim.outer_variant_sites, path);
        let at = SpecPath::from_variant_selection_sites(path, &sites, paths);
        for field in &branch.spec.fields {
            fields(
                core::slice::from_ref(field),
                layer,
                Some(&at),
                out,
                tokens.resolve(field.name) == "clips",
            );
        }
        properties(&branch.spec.properties, layer, &at, out);
        arcs(
            &branch.spec.references,
            AssetDependencyKind::Reference,
            layer,
            &at,
            out,
        );
        arcs(
            &branch.spec.payloads,
            AssetDependencyKind::Payload,
            layer,
            &at,
            out,
        );
    }
}
fn edit_literals(edit: &ArrayEdit, mut visit: impl FnMut(&Value)) {
    for op in &edit.ops {
        match op {
            ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(v),
                ..
            }
            | ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(v),
                ..
            }
            | ArrayEditOp::MinSizeFill { fill: v, .. }
            | ArrayEditOp::ResizeFill { fill: v, .. } => visit(v),
            _ => {}
        }
    }
}
fn values(
    value: &Value,
    layer: LayerId,
    spec: Option<&SpecPath>,
    field: Option<TokenId>,
    out: &mut Vec<AssetDependency>,
    clip: bool,
) {
    match value {
        Value::Asset(asset) => push(
            out,
            layer,
            spec,
            field,
            asset,
            AssetDependencyKind::Value,
            None,
        ),
        Value::Array(items) => {
            for v in items {
                values(v, layer, spec, field, out, clip);
            }
        }
        Value::Dictionary(entries) => {
            for (key, v) in entries {
                if clip && &**key == "templateAssetPath" {
                    if let Value::String(asset) = v {
                        push(
                            out,
                            layer,
                            spec,
                            field,
                            asset,
                            AssetDependencyKind::ClipTemplate,
                            None,
                        );
                    }
                } else {
                    values(v, layer, spec, field, out, clip);
                }
            }
        }
        Value::ArrayEdit(edit) => edit_literals(edit, |v| values(v, layer, spec, field, out, clip)),
        Value::TypedArrayEdit(edit) => {
            edit_literals(edit.edit(), |v| values(v, layer, spec, field, out, clip));
        }
        _ => {}
    }
}
type Replacements = BTreeMap<AssetDependency, Arc<str>>;
fn rewrite_value(
    value: &mut Value,
    layer: LayerId,
    spec: Option<&SpecPath>,
    field: Option<TokenId>,
    map: &Replacements,
    clip: bool,
) {
    match value {
        Value::Asset(asset) => {
            if let Some(new) = map.get(&dependency(
                layer,
                spec,
                field,
                asset,
                AssetDependencyKind::Value,
                None,
            )) {
                *asset = new.clone();
            }
        }
        Value::Array(items) => {
            for value in items {
                rewrite_value(value, layer, spec, field, map, clip);
            }
        }
        Value::Dictionary(entries) => {
            for (key, value) in entries {
                if clip && &**key == "templateAssetPath" {
                    if let Value::String(asset) = value
                        && let Some(new) = map.get(&dependency(
                            layer,
                            spec,
                            field,
                            asset,
                            AssetDependencyKind::ClipTemplate,
                            None,
                        ))
                    {
                        *asset = new.clone();
                    }
                } else {
                    rewrite_value(value, layer, spec, field, map, clip);
                }
            }
        }
        Value::ArrayEdit(edit) => rewrite_edit(edit, layer, spec, field, map, clip),
        Value::TypedArrayEdit(edit) => {
            let mut program = edit.edit().clone();
            rewrite_edit(&mut program, layer, spec, field, map, clip);
            if &program != edit.edit() {
                *edit = Arc::new(crate::TypedArrayEdit::new(
                    program,
                    edit.value_type().clone(),
                ));
            }
        }
        _ => {}
    }
}
fn rewrite_edit(
    edit: &mut ArrayEdit,
    layer: LayerId,
    spec: Option<&SpecPath>,
    field: Option<TokenId>,
    map: &Replacements,
    clip: bool,
) {
    for op in &mut edit.ops {
        match op {
            ArrayEditOp::Write {
                src: ArrayEditOperand::Literal(v),
                ..
            }
            | ArrayEditOp::Insert {
                src: ArrayEditOperand::Literal(v),
                ..
            }
            | ArrayEditOp::MinSizeFill { fill: v, .. }
            | ArrayEditOp::ResizeFill { fill: v, .. } => {
                rewrite_value(v, layer, spec, field, map, clip);
            }
            _ => {}
        }
    }
}
fn rewrite_fields(
    fields: &mut [FieldEntry],
    layer: LayerId,
    spec: Option<&SpecPath>,
    map: &Replacements,
    clip: bool,
) {
    for field in fields {
        if let FieldValue::Value(value) = &mut field.value {
            rewrite_value(value, layer, spec, Some(field.name), map, clip);
        }
    }
}
fn rewrite_properties(
    props: &mut [PropertyEntry],
    layer: LayerId,
    spec: &SpecPath,
    map: &Replacements,
) {
    for prop in props {
        let at = spec.with_property(prop.name);
        if !map
            .iter()
            .any(|(d, new)| d.spec.as_ref() == Some(&at) && d.identifier != *new)
        {
            continue;
        }
        let mut metadata = Vec::new();
        for (index, field) in prop.spec.metadata.iter().enumerate() {
            let mut copy = field.clone();
            rewrite_fields(
                core::slice::from_mut(&mut copy),
                layer,
                Some(&at),
                map,
                false,
            );
            if copy != *field {
                metadata.push((index, copy));
            }
        }
        let mut default = prop.spec.default.clone();
        if let Some(value) = &mut default {
            rewrite_value(value, layer, Some(&at), Some(prop.name), map, false);
        }
        let mut samples = Vec::new();
        if let Some(values) = &prop.spec.time_samples {
            for (index, (_, value)) in values.iter().enumerate() {
                let mut copy = value.clone();
                rewrite_value(&mut copy, layer, Some(&at), Some(prop.name), map, false);
                if !copy.same_representation(value) {
                    samples.push((index, copy));
                }
            }
        }
        let default_changed = match (&default, &prop.spec.default) {
            (Some(a), Some(b)) => !a.same_representation(b),
            _ => false,
        };
        if metadata.is_empty() && samples.is_empty() && !default_changed {
            continue;
        }
        let property = Arc::make_mut(&mut prop.spec);
        if default_changed {
            property.default = default;
        }
        for (index, field) in metadata {
            property.metadata.make_mut()[index] = field;
        }
        if !samples.is_empty()
            && let Some(values) = &mut property.time_samples
        {
            for (index, value) in samples {
                values.make_mut()[index].1 = value;
            }
        }
    }
}
fn rewrite_arcs(
    arcs: &mut ListOp<Reference>,
    kind: AssetDependencyKind,
    layer: LayerId,
    spec: &SpecPath,
    map: &Replacements,
) {
    for arc in arcs
        .explicit
        .iter_mut()
        .flatten()
        .chain(&mut arcs.prepend)
        .chain(&mut arcs.append)
        .chain(&mut arcs.delete)
        .chain(&mut arcs.add)
        .chain(&mut arcs.reorder)
    {
        let old = arc.asset.as_deref().unwrap_or("");
        let key = dependency(layer, Some(spec), None, old, kind, Some(arc.layer));
        if let Some(new) = map.get(&key)
            && &**new != old
        {
            arc.asset = Some(String::from(&**new));
        }
    }
}
fn rewrite_sets(
    sets: &mut crate::HashMap<TokenId, VariantSetSpec>,
    sites: &[VariantSelectionSite],
    path: PathId,
    layer: LayerId,
    paths: &PathInterner,
    tokens: &TokenInterner,
    map: &Replacements,
) {
    for (&set, variants) in sets {
        for (&variant, branch) in &mut variants.variants {
            let mut sites = sites.to_vec();
            sites.push(VariantSelectionSite {
                host_path: path,
                set,
                variant,
            });
            let at = SpecPath::from_variant_selection_sites(path, &sites, paths);
            for field in &mut branch.fields {
                let clip = tokens.resolve(field.name) == "clips";
                rewrite_fields(core::slice::from_mut(field), layer, Some(&at), map, clip);
            }
            rewrite_properties(&mut branch.properties, layer, &at, map);
            rewrite_arcs(
                &mut branch.references,
                AssetDependencyKind::Reference,
                layer,
                &at,
                map,
            );
            rewrite_arcs(
                &mut branch.payloads,
                AssetDependencyKind::Payload,
                layer,
                &at,
                map,
            );
            rewrite_sets(
                &mut branch.variant_sets,
                &sites,
                path,
                layer,
                paths,
                tokens,
                map,
            );
        }
    }
}
fn rewrite_prim(
    prim: &mut PrimSpec,
    path: PathId,
    layer: LayerId,
    paths: &PathInterner,
    tokens: &TokenInterner,
    map: &Replacements,
) {
    let at = SpecPath::from_variant_selection_sites(path, &prim.outer_variant_sites, paths);
    for field in &mut prim.fields {
        let clip = tokens.resolve(field.name) == "clips";
        rewrite_fields(core::slice::from_mut(field), layer, Some(&at), map, clip);
    }
    rewrite_properties(&mut prim.properties, layer, &at, map);
    rewrite_arcs(
        &mut prim.references,
        AssetDependencyKind::Reference,
        layer,
        &at,
        map,
    );
    rewrite_arcs(
        &mut prim.payloads,
        AssetDependencyKind::Payload,
        layer,
        &at,
        map,
    );
    rewrite_sets(
        &mut prim.variant_sets,
        &prim.outer_variant_sites,
        path,
        layer,
        paths,
        tokens,
        map,
    );
}

#[cfg(test)]
mod tests;
