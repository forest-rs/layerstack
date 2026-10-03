// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Combine authored specs without composing or flattening a stage.
//!
//! Strong scalar fields survive; dictionaries, sample maps and list operations
//! merge. Layers must share token/path interners and asset anchoring. This module
//! neither resolves assets nor rebases relative paths or time codes. Copy or
//! stitch detached layers, then publish through the host's normal layer refresh.
//! AOUSD Core §7.3–7.6, §12.2, §12.4; OpenUSD `UsdUtilsStitchLayers/StitchInfo`.

use crate::{
    FieldEntry, FieldValue, HashMap, Layer, ListOp, PrimSpec, PropertyEntry, PropertySpec,
    Specifier, TokenId, TokenInterner, Value, VariantSetSpec, VariantSpec,
};
use alloc::{sync::Arc, vec::Vec};

/// Work requiring attention while combining authored descriptions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StitchReport {
    /// List-op pairs requiring OpenUSD's historical approximation: convert
    /// `add` to `append` and discard `reorder` before composition.
    pub approximated_list_ops: usize,
    /// Conflicting layer time-rate/precision fields whose strong value survived.
    pub retained_metadata_conflicts: Vec<TokenId>,
}
/// An input which cannot be stitched into a coherent authored spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StitchError {
    /// Attribute and relationship specs cannot occupy the same property path.
    PropertyKindMismatch,
    /// Sample times must be finite, strictly increasing and unique.
    InvalidTimeSamples,
}
impl core::fmt::Display for StitchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid layer stitching input: {self:?}")
    }
}
impl core::error::Error for StitchError {}

/// Stitches `weak` into `strong`, preserving its identity and incrementing its
/// generation on success. Errors leave `strong` unchanged.
///
/// Recurses through prims, properties, variant sets and variant-qualified prim
/// contexts. Strong sample values win at duplicate times. Start/end times expand
/// to the minimum/maximum; time-rate conflicts are reported. Authored sublayers
/// and relocates are strong-field values (weak values fill absent/empty slots),
/// not recursively loaded layers. The model cannot distinguish an absent list
/// from an authored empty list for these two fields.
///
/// Matches OpenUSD 26.08's specifier exception: a weak `def`/`class` replaces
/// even a strong defining specifier; weak `over` leaves the strong one intact.
/// This is the actual stitch utility behavior, not ordinary specifier composition.
/// List-op legacy approximations are counted in the returned report.
/// No asset resolution, value interpolation or sparse-array materialization occurs.
/// Both layers and `tokens` must belong to the same interner namespace.
pub fn stitch_layers(
    strong: &mut Layer,
    weak: &Layer,
    tokens: &TokenInterner,
) -> Result<StitchReport, StitchError> {
    validate_layer(strong)?;
    validate_layer(weak)?;
    let mut result = strong.clone();
    let mut report = StitchReport::default();
    fields(
        &mut result.metadata,
        &weak.metadata,
        tokens,
        &mut report,
        true,
    );
    fill(&mut result.default_prim, &weak.default_prim);
    if result.sublayers.is_empty() {
        result.sublayers.clone_from(&weak.sublayers);
    }
    if result.relocates.is_empty() {
        result.relocates.clone_from(&weak.relocates);
    }
    // Sorting avoids dependence on hash iteration when first inserting branch contexts.
    let mut paths: Vec<_> = weak
        .prims
        .keys()
        .chain(weak.variant_prims.keys())
        .copied()
        .collect();
    paths.sort_unstable();
    paths.dedup();
    for path in paths {
        for source in weak.prim_specs(path) {
            let existing = result
                .prim_specs(path)
                .find(|p| p.outer_variant_sites == source.outer_variant_sites)
                .cloned();
            let Some(mut merged) = existing else {
                result.insert_prim(path, source.clone());
                continue;
            };
            merged
                .outer_variant_sites
                .clone_from(&source.outer_variant_sites);
            prim(&mut merged, source, tokens, &mut report, true)?;
            result.insert_prim(path, merged);
        }
    }
    result.touch();
    *strong = result;
    Ok(report)
}

/// Stitches fields on one prim, without copying its properties, child names,
/// variant sets or variant descendants, as `UsdUtilsStitchInfo` does.
/// References, inherits, payloads, variant selections and ordering metadata are
/// fields and do merge. Both specs must denote the same variant context.
pub fn stitch_prim_info(
    strong: &mut PrimSpec,
    weak: &PrimSpec,
    tokens: &TokenInterner,
) -> StitchReport {
    let mut report = StitchReport::default();
    // This path visits no properties and therefore cannot fail.
    prim(strong, weak, tokens, &mut report, false).expect("info-only prim merge");
    report
}

/// Stitches one property's fields and sample map. Strong defaults, declaration
/// qualifiers and spline opinions survive; absent values are filled from weak.
/// Metadata dictionaries and connection/target list ops merge recursively.
/// Errors leave `strong` unchanged. Call the owning layer's `touch` after using
/// this utility to modify an attached property outside a transaction.
pub fn stitch_property_info(
    strong: &mut PropertySpec,
    weak: &PropertySpec,
    tokens: &TokenInterner,
) -> Result<StitchReport, StitchError> {
    let mut result = strong.clone();
    let mut report = StitchReport::default();
    property(&mut result, weak, tokens, &mut report)?;
    *strong = result;
    Ok(report)
}
fn fill<T: Clone>(strong: &mut Option<T>, weak: &Option<T>) {
    if strong.is_none() {
        strong.clone_from(weak);
    }
}
fn append_unique<T: Clone + PartialEq>(strong: &mut Vec<T>, weak: &[T]) {
    for value in weak {
        if !strong.contains(value) {
            strong.push(value.clone());
        }
    }
}
fn map_missing<T: Clone>(strong: &mut HashMap<TokenId, T>, weak: &HashMap<TokenId, T>) {
    for (&key, value) in weak {
        strong.entry(key).or_insert_with(|| value.clone());
    }
}
fn validate_property(property: &PropertySpec) -> Result<(), StitchError> {
    if let Some(samples) = &property.time_samples
        && (samples.iter().any(|(t, _)| !t.is_finite())
            || samples.windows(2).any(|s| s[0].0 >= s[1].0))
    {
        return Err(StitchError::InvalidTimeSamples);
    }
    Ok(())
}
fn validate_variants(sets: &HashMap<TokenId, VariantSetSpec>) -> Result<(), StitchError> {
    for set in sets.values() {
        for variant in set.variants.values() {
            for property in &variant.properties {
                validate_property(&property.spec)?;
            }
            validate_variants(&variant.variant_sets)?;
        }
    }
    Ok(())
}
fn validate_layer(layer: &Layer) -> Result<(), StitchError> {
    for spec in layer
        .prims
        .values()
        .chain(layer.variant_prims.values().flatten())
    {
        for property in &spec.properties {
            validate_property(&property.spec)?;
        }
        validate_variants(&spec.variant_sets)?;
    }
    Ok(())
}
fn arc<T: Clone + Eq>(
    strong: &ListOp<T>,
    weak: &ListOp<T>,
    report: &mut StitchReport,
) -> ListOp<T> {
    let absent = |op: &ListOp<T>| {
        op.explicit.is_none()
            && op.prepend.is_empty()
            && op.append.is_empty()
            && op.delete.is_empty()
            && op.add.is_empty()
            && op.reorder.is_empty()
    };
    if absent(strong) {
        weak.clone()
    } else if absent(weak) {
        strong.clone()
    } else {
        listop(strong, weak, report)
    }
}
fn fixed<T: Clone + PartialEq>(op: &ListOp<T>) -> ListOp<T> {
    let mut result = op.clone();
    append_unique(&mut result.append, &op.add);
    result.add.clear();
    result.reorder.clear();
    result
}
fn listop<T: Clone + Eq>(
    strong: &ListOp<T>,
    weak: &ListOp<T>,
    report: &mut StitchReport,
) -> ListOp<T> {
    // AOUSD §12.4; SdfListOp::ApplyOperations(listop) composition, followed by
    // usdUtils/stitch.cpp's _FixListOp only when exact representation fails.
    if strong.explicit.is_some() {
        return strong.clone();
    }
    let legacy = !strong.add.is_empty()
        || !strong.reorder.is_empty()
        || (weak.explicit.is_none() && (!weak.add.is_empty() || !weak.reorder.is_empty()));
    if legacy {
        report.approximated_list_ops += 1;
        return listop(&fixed(strong), &fixed(weak), report);
    }
    if let Some(items) = &weak.explicit {
        return ListOp::explicit(strong.apply_to(items));
    }
    let mut result = weak.clone();
    for value in &strong.delete {
        result.prepend.retain(|v| v != value);
        result.append.retain(|v| v != value);
        if !result.delete.contains(value) {
            result.delete.push(value.clone());
        }
    }
    for value in strong.prepend.iter().chain(&strong.append) {
        result.delete.retain(|v| v != value);
        result.prepend.retain(|v| v != value);
        result.append.retain(|v| v != value);
    }
    let mut prepend = strong.prepend.clone();
    prepend.extend(result.prepend);
    // If strong prepends and appends the same item, append wins.
    prepend.retain(|v| !strong.append.contains(v));
    result.prepend = prepend;
    result.append.extend_from_slice(&strong.append);
    result
}
fn field_value(strong: &mut FieldValue, weak: &FieldValue, report: &mut StitchReport) {
    match (strong, weak) {
        (FieldValue::Value(Value::Dictionary(a)), FieldValue::Value(Value::Dictionary(b))) => {
            *a = crate::doc::combine_dictionaries(a, b);
        }
        (FieldValue::TokenListOp(a), FieldValue::TokenListOp(b)) => *a = listop(a, b, report),
        (FieldValue::PathListOp(a), FieldValue::PathListOp(b)) => *a = listop(a, b, report),
        (FieldValue::StringListOp(a), FieldValue::StringListOp(b)) => *a = listop(a, b, report),
        (FieldValue::IntListOp(a), FieldValue::IntListOp(b)) => *a = listop(a, b, report),
        (FieldValue::UIntListOp(a), FieldValue::UIntListOp(b)) => *a = listop(a, b, report),
        (FieldValue::Int64ListOp(a), FieldValue::Int64ListOp(b)) => *a = listop(a, b, report),
        (FieldValue::UInt64ListOp(a), FieldValue::UInt64ListOp(b)) => *a = listop(a, b, report),
        _ => {}
    }
}
fn fields(
    strong: &mut Vec<FieldEntry>,
    weak: &[FieldEntry],
    tokens: &TokenInterner,
    report: &mut StitchReport,
    layer: bool,
) {
    for field in weak {
        if let Some(existing) = strong.iter_mut().find(|f| f.name == field.name) {
            if layer {
                let name = tokens.resolve(field.name);
                if matches!(name, "startTimeCode" | "endTimeCode")
                    && let (
                        FieldValue::Value(Value::Double(a)),
                        FieldValue::Value(Value::Double(b)),
                    ) = (&mut existing.value, &field.value)
                {
                    *a = if name == "startTimeCode" {
                        a.min(*b)
                    } else {
                        a.max(*b)
                    };
                    continue;
                }
                if matches!(
                    name,
                    "framesPerSecond" | "timeCodesPerSecond" | "framePrecision"
                ) && existing.value != field.value
                    && !report.retained_metadata_conflicts.contains(&field.name)
                {
                    report.retained_metadata_conflicts.push(field.name);
                }
            }
            field_value(&mut existing.value, &field.value, report);
        } else {
            strong.push(field.clone());
        }
    }
}
fn property(
    strong: &mut PropertySpec,
    weak: &PropertySpec,
    tokens: &TokenInterner,
    report: &mut StitchReport,
) -> Result<(), StitchError> {
    if strong.kind != weak.kind {
        return Err(StitchError::PropertyKindMismatch);
    }
    fill(&mut strong.type_name, &weak.type_name);
    fill(&mut strong.default, &weak.default);
    fill(&mut strong.spline, &weak.spline);
    for samples in [&strong.time_samples, &weak.time_samples]
        .into_iter()
        .flatten()
    {
        if samples.iter().any(|(t, _)| !t.is_finite())
            || samples.windows(2).any(|s| s[0].0 >= s[1].0)
        {
            return Err(StitchError::InvalidTimeSamples);
        }
    }
    if let Some(weak_samples) = &weak.time_samples {
        let strong_samples = strong.time_samples.as_deref().unwrap_or(&[]);
        let mut merged = Vec::with_capacity(strong_samples.len() + weak_samples.len());
        let (mut a, mut b) = (0, 0);
        while a < strong_samples.len() && b < weak_samples.len() {
            if strong_samples[a].0 <= weak_samples[b].0 {
                merged.push(strong_samples[a].clone());
                if strong_samples[a].0 == weak_samples[b].0 {
                    b += 1;
                }
                a += 1;
            } else {
                merged.push(weak_samples[b].clone());
                b += 1;
            }
        }
        merged.extend_from_slice(&strong_samples[a..]);
        merged.extend_from_slice(&weak_samples[b..]);
        strong.time_samples = Some(merged.into());
    }
    if let Some(weak_targets) = &weak.targets {
        strong.targets = Some(
            strong
                .targets
                .as_ref()
                .map_or_else(|| weak_targets.clone(), |s| listop(s, weak_targets, report)),
        );
    }
    fields(
        strong.metadata.make_mut(),
        &weak.metadata,
        tokens,
        report,
        false,
    );
    Ok(())
}
fn properties(
    strong: &mut Vec<PropertyEntry>,
    weak: &[PropertyEntry],
    tokens: &TokenInterner,
    report: &mut StitchReport,
) -> Result<(), StitchError> {
    for source in weak {
        if let Some(existing) = strong.iter_mut().find(|p| p.name == source.name) {
            property(
                Arc::make_mut(&mut existing.spec),
                &source.spec,
                tokens,
                report,
            )?;
        } else {
            validate_property(&source.spec)?;
            strong.push(source.clone());
        }
    }
    Ok(())
}
fn variant_sets(
    strong: &mut HashMap<TokenId, VariantSetSpec>,
    weak: &HashMap<TokenId, VariantSetSpec>,
    tokens: &TokenInterner,
    report: &mut StitchReport,
) -> Result<(), StitchError> {
    for (&set, source) in weak {
        let target = strong.entry(set).or_default();
        for (&name, source) in &source.variants {
            variant(
                target.variants.entry(name).or_default(),
                source,
                tokens,
                report,
            )?;
        }
    }
    Ok(())
}
fn variant(
    strong: &mut VariantSpec,
    weak: &VariantSpec,
    tokens: &TokenInterner,
    report: &mut StitchReport,
) -> Result<(), StitchError> {
    fields(&mut strong.fields, &weak.fields, tokens, report, false);
    properties(&mut strong.properties, &weak.properties, tokens, report)?;
    append_unique(&mut strong.authored_children, &weak.authored_children);
    strong.references = arc(&strong.references, &weak.references, report);
    strong.inherits = arc(&strong.inherits, &weak.inherits, report);
    strong.specializes = arc(&strong.specializes, &weak.specializes, report);
    strong.payloads = arc(&strong.payloads, &weak.payloads, report);
    map_missing(&mut strong.variant_selections, &weak.variant_selections);
    append_unique(&mut strong.variant_set_order, &weak.variant_set_order);
    fill(&mut strong.property_order, &weak.property_order);
    variant_sets(&mut strong.variant_sets, &weak.variant_sets, tokens, report)
}
fn prim(
    strong: &mut PrimSpec,
    weak: &PrimSpec,
    tokens: &TokenInterner,
    report: &mut StitchReport,
    children: bool,
) -> Result<(), StitchError> {
    if weak.specifier.is_some_and(|s| s != Specifier::Over) || strong.specifier.is_none() {
        strong.specifier = weak.specifier;
    }
    fill(&mut strong.type_name, &weak.type_name);
    fill(&mut strong.active, &weak.active);
    fill(&mut strong.instanceable, &weak.instanceable);
    fill(&mut strong.property_order, &weak.property_order);
    fill(&mut strong.prim_order, &weak.prim_order);
    fields(&mut strong.fields, &weak.fields, tokens, report, false);
    strong.references = arc(&strong.references, &weak.references, report);
    strong.inherits = arc(&strong.inherits, &weak.inherits, report);
    strong.specializes = arc(&strong.specializes, &weak.specializes, report);
    strong.payloads = arc(&strong.payloads, &weak.payloads, report);
    map_missing(&mut strong.variant_selections, &weak.variant_selections);
    let names = listop(
        &ListOp::prepended(strong.variant_set_order.clone())
            .with_deleted(strong.deleted_variant_sets.clone()),
        &ListOp::prepended(weak.variant_set_order.clone())
            .with_deleted(weak.deleted_variant_sets.clone()),
        report,
    );
    strong.variant_set_order = names.prepend;
    strong.deleted_variant_sets = names.delete;
    if children {
        append_unique(&mut strong.authored_children, &weak.authored_children);
        properties(&mut strong.properties, &weak.properties, tokens, report)?;
        variant_sets(&mut strong.variant_sets, &weak.variant_sets, tokens, report)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryStore, LayerId, PropertyType};
    use alloc::{boxed::Box, vec};
    #[test]
    fn cpp_specifier_exception_and_recursive_dictionary_sample_merge() {
        let mut store = InMemoryStore::default();
        let p = store.path("/P");
        let a = store.tokens.intern("a");
        let data = store.tokens.intern("customData");
        let ty = PropertyType::new("double", false, Value::Double(0.));
        let mut strong = Layer::new(LayerId(1));
        let mut weak = Layer::new(LayerId(2));
        strong.insert_prim(
            p,
            PrimSpec::def()
                .with_field(
                    data,
                    Value::Dictionary(vec![(
                        "nested".into(),
                        Value::Dictionary(vec![("a".into(), Value::Int(1))]),
                    )]),
                )
                .with_property(
                    a,
                    PropertySpec::typed_attribute(ty.clone())
                        .with_default(Value::Double(1.))
                        .with_time_samples(vec![(0., Value::Double(1.)), (2., Value::Double(2.))]),
                ),
        );
        weak.insert_prim(
            p,
            PrimSpec::class()
                .with_field(
                    data,
                    Value::Dictionary(vec![(
                        "nested".into(),
                        Value::Dictionary(vec![
                            ("a".into(), Value::Int(9)),
                            ("b".into(), Value::Int(2)),
                        ]),
                    )]),
                )
                .with_property(
                    a,
                    PropertySpec::typed_attribute(ty)
                        .with_default(Value::Double(9.))
                        .with_time_samples(vec![(1., Value::Double(8.)), (2., Value::Double(9.))]),
                ),
        );
        stitch_layers(&mut strong, &weak, &store.tokens).unwrap();
        assert_eq!(strong.prims[&p].specifier, Some(Specifier::Class));
        let prop = strong.prims[&p].property(a).unwrap();
        assert_eq!(prop.default, Some(Value::Double(1.)));
        assert_eq!(
            prop.time_samples.as_ref().unwrap().as_slice(),
            &[
                (0., Value::Double(1.)),
                (1., Value::Double(8.)),
                (2., Value::Double(2.))
            ]
        );
        assert_eq!(
            strong.prims[&p].field(data),
            Some(&FieldValue::Value(Value::Dictionary(vec![(
                "nested".into(),
                Value::Dictionary(vec![
                    ("a".into(), Value::Int(1)),
                    ("b".into(), Value::Int(2))
                ])
            )])))
        );
    }
    #[test]
    fn exact_listops_preserve_behavior_over_arbitrary_bases() {
        let operations = [
            ListOp::new(),
            ListOp::explicit(vec![]),
            ListOp::explicit(vec![2, 1]),
            ListOp::prepended(vec![2]).with_appended(vec![3]),
            ListOp::deleted(vec![1, 2]),
            ListOp::appended(vec![1]),
        ];
        for strong in &operations {
            for weak in &operations {
                let mut report = StitchReport::default();
                let merged = listop(strong, weak, &mut report);
                assert_eq!(report.approximated_list_ops, 0);
                for base in [vec![], vec![1, 2, 3], vec![3, 2, 1], vec![0, 4, 2]] {
                    assert_eq!(
                        merged.apply_to(&base),
                        strong.apply_to(&weak.apply_to(&base))
                    );
                }
            }
        }
    }
    #[test]
    fn historical_adds_append_and_reorders_are_discarded_only_when_needed() {
        let mut r = StitchReport::default();
        let merged = listop(
            &ListOp::added(vec![1]).with_reordered(vec![1, 2]),
            &ListOp::appended(vec![2]),
            &mut r,
        );
        assert_eq!(merged, ListOp::appended(vec![2, 1]));
        assert_eq!(r.approximated_list_ops, 1);
        let merged = listop(&ListOp::explicit(vec![7]), &ListOp::added(vec![3]), &mut r);
        assert_eq!(merged, ListOp::explicit(vec![7]));
        assert_eq!(r.approximated_list_ops, 1);
    }
    #[test]
    fn time_envelope_conflicts_and_info_do_not_copy_children() {
        let mut tokens = TokenInterner::default();
        let start = tokens.intern("startTimeCode");
        let end = tokens.intern("endTimeCode");
        let fps = tokens.intern("framesPerSecond");
        let mut strong = Layer::new(LayerId(1));
        let mut weak = Layer::new(LayerId(2));
        strong
            .set_metadata(start, Value::Double(10.))
            .set_metadata(end, Value::Double(20.))
            .set_metadata(fps, Value::Double(24.));
        weak.set_metadata(start, Value::Double(0.))
            .set_metadata(end, Value::Double(30.))
            .set_metadata(fps, Value::Double(30.));
        let r = stitch_layers(&mut strong, &weak, &tokens).unwrap();
        assert_eq!(
            strong.metadata(start),
            Some(&FieldValue::Value(Value::Double(0.)))
        );
        assert_eq!(
            strong.metadata(end),
            Some(&FieldValue::Value(Value::Double(30.)))
        );
        assert_eq!(r.retained_metadata_conflicts, vec![fps]);
        let mut a = PrimSpec::def();
        let b = PrimSpec::def()
            .with_children(vec![tokens.intern("Child")])
            .with_property(
                tokens.intern("v"),
                PropertySpec::attribute().with_default(Value::Matrix2d(Box::new([1., 0., 0., 1.]))),
            );
        stitch_prim_info(&mut a, &b, &tokens);
        assert!(a.authored_children.is_empty());
        assert!(a.properties.is_empty());
    }
    #[test]
    fn invalid_properties_are_atomic() {
        let tokens = TokenInterner::default();
        let mut a = PropertySpec::attribute().with_default(Value::Int(1));
        let before = a.clone();
        assert_eq!(
            stitch_property_info(&mut a, &PropertySpec::relationship(), &tokens),
            Err(StitchError::PropertyKindMismatch)
        );
        assert_eq!(a, before);
        let b = PropertySpec::attribute().with_time_samples(vec![(f64::NAN, Value::Int(3))]);
        assert_eq!(
            stitch_property_info(&mut a, &b, &tokens),
            Err(StitchError::InvalidTimeSamples)
        );
        assert_eq!(a, before);
    }
    #[test]
    fn variant_contexts_arcs_and_weak_only_properties_survive() {
        let mut store = InMemoryStore::default();
        let host = store.path("/Model");
        let child = store.path("/Model/Geom");
        let base = store.path("/Base");
        let other = store.path("/Other");
        let set = store.tokens.intern("lod");
        let high = store.tokens.intern("high");
        let low = store.tokens.intern("low");
        let x = store.tokens.intern("x");
        let y = store.tokens.intern("y");
        let mut strong = Layer::new(LayerId(1));
        let mut weak = Layer::new(LayerId(2));
        let mut prim = PrimSpec::def();
        prim.inherits = ListOp::prepended(vec![base]);
        prim.variant_sets
            .entry(set)
            .or_default()
            .variants
            .entry(high)
            .or_default()
            .properties
            .push(PropertyEntry {
                name: x,
                spec: Arc::new(PropertySpec::attribute().with_default(Value::Int(1))),
            });
        strong.insert_prim(host, prim);
        let mut prim = PrimSpec::over();
        prim.inherits = ListOp::appended(vec![other]);
        let mut targets = PropertySpec::relationship();
        targets.targets = Some(ListOp::added(vec![crate::TargetPath::Prim(base)]));
        prim.variant_sets
            .entry(set)
            .or_default()
            .variants
            .entry(high)
            .or_default()
            .properties
            .push(PropertyEntry {
                name: y,
                spec: Arc::new(targets.clone()),
            });
        prim.variant_sets
            .entry(set)
            .or_default()
            .variants
            .entry(low)
            .or_default();
        weak.insert_prim(host, prim);
        for (variant, value) in [(high, 10), (low, 20)] {
            let mut child_spec = PrimSpec::def()
                .with_property(x, PropertySpec::attribute().with_default(Value::Int(value)));
            child_spec
                .outer_variant_sites
                .push(crate::spec_path::VariantSelectionSite {
                    host_path: host,
                    set,
                    variant,
                });
            weak.insert_prim(child, child_spec);
        }
        let report = stitch_layers(&mut strong, &weak, &store.tokens).unwrap();
        assert_eq!(report.approximated_list_ops, 0);
        assert_eq!(strong.prims[&host].inherits.apply_to(&[]), [base, other]);
        let branch = &strong.prims[&host].variant_sets[&set].variants[&high];
        assert_eq!(branch.properties.len(), 2);
        assert_eq!(
            *branch.properties[1].spec, targets,
            "an absent destination field copies legacy edits verbatim"
        );
        assert_eq!(strong.prim_specs(child).count(), 2);
        for (variant, value) in [(high, 10), (low, 20)] {
            let p = strong
                .prim_specs(child)
                .find(|p| p.outer_variant_sites[0].variant == variant)
                .unwrap();
            assert_eq!(p.property(x).unwrap().default, Some(Value::Int(value)));
        }
    }
    #[test]
    fn layer_errors_preserve_content_and_generations() {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let a = store.tokens.intern("a");
        let mut strong = Layer::new(LayerId(1));
        let mut weak = Layer::new(LayerId(2));
        strong.insert_prim(
            path,
            PrimSpec::def().with_property(a, PropertySpec::attribute()),
        );
        weak.insert_prim(
            path,
            PrimSpec::class().with_property(a, PropertySpec::relationship()),
        );
        let before = strong.clone();
        let generation = strong.generation();
        assert_eq!(
            stitch_layers(&mut strong, &weak, &store.tokens),
            Err(StitchError::PropertyKindMismatch)
        );
        assert_eq!(strong, before);
        assert_eq!(strong.generation(), generation);
    }
}
