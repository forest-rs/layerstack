// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Variant fallback selections.
//!
//! A variant set without an authored selection contributes nothing (AOUSD
//! Core §10.3.2.5). A stage may name fallbacks instead
//! ([`StageOptions::variant_fallbacks`]): for each variant set name, an
//! ordered list of variant names. Where composition finds no selection for a
//! set, the first fallback that names a variant of the set at the prim is
//! selected, and that branch composes as if selected by an opinion.
//! Composition passes the fallbacks explicitly to every function that
//! resolves variant selections.
//!
//! OpenUSD calls these variant fallbacks (`PcpCache::SetVariantFallbacks`,
//! `UsdStage::SetGlobalVariantFallbacks`); `_ComposeVariantSelection` and
//! `_ChooseBestFallbackAmongOptions` in `pxr/usd/pcp/primIndex.cpp` use one
//! only when no opinion in the prim index selects a variant of the set.
//!
//! [`StageOptions::variant_fallbacks`]: crate::StageOptions::variant_fallbacks

use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};

use crate::{
    doc::{LayerStore, PrimSpec},
    expression_variables::{SiteContext, site_selections},
    interner::TokenId,
};

/// Variant fallback selections: for each variant set name, the variant
/// names to select, in order of preference, where no selection is
/// authored.
pub type VariantFallbacks = HashMap<TokenId, Vec<TokenId>>;

/// Completes `selections`, the variant selections composition found for a
/// prim whose specs are `specs`, with `fallbacks`.
///
/// `selections` holds every authored selection; fallbacks only complete
/// it. The variant sets are visited in the order the specs declare them
/// (`variantSets`, [`PrimSpec::variant_set_order`]), `specs` strongest
/// first. For each declared set with a fallback and no selection, the
/// first name in its fallback list that names a variant of the set in any
/// of `specs` is selected, and the selections authored inside that branch
/// are added before the next set is visited: they count as authored for
/// the sets they name, so a set decided by a fallback earlier in the order
/// keeps it, and a later one takes the branch's selection. The result
/// depends only on the specs and the order of each fallback list.
///
/// Spec: AOUSD Core §10.3.2.5.1 selects only from opinions; fallbacks follow
/// OpenUSD, which evaluates every authored selection before any fallback,
/// then the fallback tasks in node strength and `variantSets` order,
/// re-evaluating the pending ones as authored after each fallback branch is
/// added (`_EvalNodeAuthoredVariant`, `_EvalNodeFallbackVariant` and
/// `_ChooseBestFallbackAmongOptions` in `pxr/usd/pcp/primIndex.cpp`).
///
/// Each spec comes with the context its branches' selections, variable
/// expressions among them, are read in ([`SiteContext`]).
pub(crate) fn apply_variant_fallbacks(
    store: &dyn LayerStore,
    fallbacks: &VariantFallbacks,
    selections: &mut HashMap<TokenId, TokenId>,
    specs: &[(&PrimSpec, SiteContext<'_>)],
) {
    if fallbacks.is_empty() {
        return;
    }
    let mut filled: HashSet<TokenId> = HashSet::new();
    loop {
        let next = specs
            .iter()
            .flat_map(|(spec, _)| spec.variant_set_order.iter().copied())
            .find(|set| {
                !selections.contains_key(set)
                    && fallbacks.contains_key(set)
                    && !filled.contains(set)
            });
        let Some(set) = next else {
            return;
        };
        filled.insert(set);
        let chosen = fallbacks[&set].iter().copied().find(|variant| {
            specs.iter().any(|(spec, _)| {
                spec.variant_sets
                    .get(&set)
                    .is_some_and(|set_spec| set_spec.variants.contains_key(variant))
            })
        });
        let Some(variant) = chosen else {
            continue;
        };
        selections.insert(set, variant);
        // Selections authored in the chosen branch, and in branches they
        // select in turn.
        let mut pending = alloc::vec![(set, variant)];
        while let Some((set, variant)) = pending.pop() {
            for (spec, context) in specs {
                let Some(branch) = spec
                    .variant_sets
                    .get(&set)
                    .and_then(|set_spec| set_spec.variants.get(&variant))
                else {
                    continue;
                };
                let inner = site_selections(store, &branch.variant_selections, *context);
                for (inner_set, inner_variant) in inner.iter() {
                    if !selections.contains_key(inner_set) {
                        selections.insert(*inner_set, *inner_variant);
                        pending.push((*inner_set, *inner_variant));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{format, string::String, vec, vec::Vec};

    use crate::{
        doc::{InMemoryStore, Layer, LayerId, PrimSpec, Value, VariantSetSpec, VariantSpec},
        path::PropertyPath,
        property::PropertySpec,
        stage::{Stage, StageOptions},
    };

    /// Composes `/Block`, with the variant set `shape` of `cube` and
    /// `sphere`, each authoring `sides`, and the `authored` selection, using
    /// `fallbacks` for `shape`; returns `sides`.
    fn sides(fallbacks: &[&str], authored: Option<&str>) -> Option<Value> {
        let mut store = InMemoryStore::default();
        let (shape, sides) = (store.tokens.intern("shape"), store.tokens.intern("sides"));
        let block = store.path("/Block");
        let mut spec = PrimSpec::def();
        let mut set = VariantSetSpec::default();
        for (name, count) in [("cube", 6), ("sphere", 0)] {
            let variant = VariantSpec {
                properties: PrimSpec::def()
                    .with_property(
                        sides,
                        PropertySpec::attribute().with_default(Value::Int(count)),
                    )
                    .properties,
                ..VariantSpec::default()
            };
            set.variants.insert(store.tokens.intern(name), variant);
        }
        spec.variant_sets.insert(shape, set);
        spec.variant_set_order.push(shape);
        if let Some(name) = authored {
            spec.variant_selections
                .insert(shape, store.tokens.intern(name));
        }
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(block, spec);
        store.insert_layer(layer);
        let names = fallbacks
            .iter()
            .map(|name| store.tokens.intern(name))
            .collect();
        let options = StageOptions {
            variant_fallbacks: [(shape, names)].into_iter().collect(),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        stage
            .resolve_field_path(PropertyPath::new(block, sides))
            .map(|resolved| resolved.value)
    }

    /// Spec: OpenUSD's `_ChooseBestFallbackAmongOptions` selects the first
    /// fallback naming a variant of the set, only without an authored
    /// selection.
    #[test]
    fn first_fallback_naming_a_variant_applies_without_a_selection() {
        assert_eq!(sides(&[], None), None);
        assert_eq!(sides(&["cone", "cube"], None), Some(Value::Int(6)));
        assert_eq!(sides(&["cone"], None), None);
        assert_eq!(sides(&["cube"], Some("sphere")), Some(Value::Int(0)));
    }

    /// Composes `/Mast`, declaring the variant sets `shape` then `size` (or
    /// `size` then `shape`, `size_declared_first`), whose `shape=cube`
    /// branch selects `size=small`, with the fallbacks `shape=[cube]` and
    /// `size=[large, small]`. `size_first` interns `size` before `shape`.
    /// Returns `scale`, and the prim stack's spec paths followed by the
    /// stage's reported selections.
    fn mast(size_first: bool, size_declared_first: bool) -> (Option<Value>, Vec<String>) {
        let mut store = InMemoryStore::default();
        let (shape, size) = if size_first {
            let size = store.tokens.intern("size");
            (store.tokens.intern("shape"), size)
        } else {
            let shape = store.tokens.intern("shape");
            (shape, store.tokens.intern("size"))
        };
        let [cube, sphere, small, large, scale] =
            ["cube", "sphere", "small", "large", "scale"].map(|t| store.tokens.intern(t));
        let mast = store.path("/Mast");
        let mut spec = PrimSpec::def();
        let mut shapes = VariantSetSpec::default();
        let mut cube_branch = VariantSpec::default();
        cube_branch.variant_selections.insert(size, small);
        shapes.variants.insert(cube, cube_branch);
        shapes.variants.insert(sphere, VariantSpec::default());
        let mut sizes = VariantSetSpec::default();
        for (name, value) in [(small, 1), (large, 2)] {
            let variant = VariantSpec {
                properties: PrimSpec::def()
                    .with_property(
                        scale,
                        PropertySpec::attribute().with_default(Value::Int(value)),
                    )
                    .properties,
                ..VariantSpec::default()
            };
            sizes.variants.insert(name, variant);
        }
        spec.variant_sets.insert(shape, shapes);
        spec.variant_sets.insert(size, sizes);
        spec.variant_set_order = if size_declared_first {
            vec![size, shape]
        } else {
            vec![shape, size]
        };
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(mast, spec);
        store.insert_layer(layer);
        let options = StageOptions {
            with_provenance: true,
            variant_fallbacks: [(shape, vec![cube]), (size, vec![large, small])]
                .into_iter()
                .collect(),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        let value = stage
            .resolve_field_path(PropertyPath::new(mast, scale))
            .map(|resolved| resolved.value);
        let mut stack: Vec<String> = stage
            .explain_prim(mast)
            .unwrap_or_default()
            .iter()
            .map(|key| key.spec_path.display(&store.tokens))
            .collect();
        // The stage reports the selections composition made, fallbacks
        // included, as `UsdVariantSet::GetVariantSelection` does.
        let mut selections: Vec<String> = stage
            .variant_selections(mast, &store)
            .iter()
            .map(|(set, variant)| {
                format!(
                    "{}={}",
                    store.tokens.resolve(*set),
                    store.tokens.resolve(*variant)
                )
            })
            .collect();
        selections.sort();
        stack.push(selections.join(","));
        (value, stack)
    }

    /// Fallbacks are chosen in `variantSets` order, whatever the interning
    /// order of the set names. A selection authored in a fallback branch
    /// decides a set declared after it (OpenUSD evaluates the pending
    /// fallbacks as authored after adding each fallback branch), but not
    /// one declared before it, whose fallback branch is kept. The branches
    /// rank in the declared order of their sets.
    #[test]
    fn fallback_order_follows_declared_sets_not_token_order() {
        let cases = [
            (
                false,
                1,
                [
                    "/Mast",
                    "/Mast{shape=cube}",
                    "/Mast{size=small}",
                    "shape=cube,size=small",
                ],
            ),
            (
                true,
                2,
                [
                    "/Mast",
                    "/Mast{size=large}",
                    "/Mast{shape=cube}",
                    "shape=cube,size=large",
                ],
            ),
        ];
        for (size_declared_first, scale, stack) in cases {
            for size_first in [false, true] {
                let case = format!(
                    "size interned first {size_first}, declared first {size_declared_first}"
                );
                let (value, composed) = mast(size_first, size_declared_first);
                assert_eq!(value, Some(Value::Int(scale)), "{case}");
                assert_eq!(composed, stack, "{case}");
            }
        }
    }
}
