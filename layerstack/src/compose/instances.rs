// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition owns representative reuse; authored layers and instance roots
//! stay independent. Eligibility proves that only the occurrence graph's root
//! site changes. Context-dependent subtrees use ordinary composition instead.

use super::*;
use crate::doc::{FieldEntry, FieldValue, PrimSpec, Value};

#[derive(Default)]
pub(super) struct InstanceReuse {
    /// Representative descendant -> occurrence descendant, before composition.
    members: Vec<(PathId, PathId)>,
}

impl InstanceReuse {
    pub(super) fn discover(
        store: &dyn LayerStore,
        root: LayerId,
        paths: &mut BTreeSet<PathId>,
        options: &StageOptions,
    ) -> Self {
        // AOUSD Core §11.3.3: local instance roots remain ordinary prims;
        // descendant composition belongs to their shared prototype. Start with
        // single-layer, context-free external references. Reject masks, variants,
        // clips, expressions, ancestral arcs, relocates and nested instancing
        // until their complete context is represented by this early key.
        let Some(layer) = store.layer(root) else {
            return Self::default();
        };
        if options.mask.is_some()
            || !layer
                .prims
                .values()
                .any(|spec| spec.instanceable == Some(true))
            || !layer.sublayers.is_empty()
            || !layer.relocates.is_empty()
            || !layer.variant_prims.is_empty()
            || !fields_context_free(store, &layer.metadata)
        {
            return Self::default();
        }
        let mut candidates = HashMap::new();
        let mut source_eligibility = HashMap::new();
        for (&path, spec) in &layer.prims {
            if !plain_spec(store, spec) || spec.active == Some(false) {
                return Self::default();
            }
            if spec.references.is_empty() {
                if spec.instanceable == Some(true) {
                    return Self::default();
                }
                continue;
            }
            let refs = spec.references.apply_to(&[]);
            if spec.instanceable != Some(true) || refs.len() != 1 {
                return Self::default();
            }
            let reference = &refs[0];
            let ReferenceTarget::Prim(target) = reference.target else {
                return Self::default();
            };
            if reference.layer == root
                || reference.asset.is_none()
                || reference.is_expression()
                || reference.is_unresolved()
            {
                return Self::default();
            }
            let eligible = *source_eligibility
                .entry(reference.layer)
                .or_insert_with(|| {
                    store.layer(reference.layer).is_some_and(|source| {
                        source.sublayers.is_empty()
                            && source.relocates.is_empty()
                            && source.variant_prims.is_empty()
                            && fields_context_free(store, &source.metadata)
                            && source.prims.values().all(|spec| {
                                plain_spec(store, spec)
                                    && spec.references.is_empty()
                                    && spec.instanceable != Some(true)
                            })
                    })
                });
            if !eligible {
                return Self::default();
            }
            let source = store.layer(reference.layer).expect("eligible layer");
            // Instance-root local activation remains independent. A weaker
            // inactive target root can be overridden by only some occurrences,
            // so their descendant visibility is not a shared context.
            if source
                .prims
                .get(&target)
                .is_none_or(|spec| spec.active == Some(false))
            {
                return Self::default();
            }
            candidates.insert(
                path,
                (
                    reference.layer,
                    target,
                    reference.layer_offset.offset.to_bits(),
                    reference.layer_offset.scale.to_bits(),
                    store.paths().resolve(path).depth(),
                ),
            );
        }
        // A local reference below an instance is inert, not another shared
        // prototype occurrence. Keep nested instance declarations on the
        // ordinary path until nested ownership is proven explicitly.
        for &path in candidates.keys() {
            let mut cursor = store.paths().resolve(path).parent();
            while let Some(parent) = cursor {
                if store
                    .paths()
                    .lookup(&parent)
                    .is_some_and(|path| candidates.contains_key(&path))
                {
                    return Self::default();
                }
                cursor = parent.parent();
            }
        }
        let mut groups = HashMap::new();
        let mut representative = HashMap::new();
        // Population order makes the representative deterministic. Only roots
        // that actually populated can provide a representative.
        for &path in paths.iter() {
            if let Some(key) = candidates.get(&path) {
                let first = *groups.entry(*key).or_insert(path);
                representative.insert(path, first);
            }
        }
        let mut descendants: HashMap<PathId, Vec<PathId>> = HashMap::new();
        let mut omitted = HashSet::new();
        for &path in paths.iter() {
            let mut cursor = store.paths().resolve(path).parent();
            while let Some(parent) = cursor {
                if let Some(owner) = store.paths().lookup(&parent)
                    && let Some(&first) = representative.get(&owner)
                {
                    if owner == first {
                        descendants.entry(first).or_default().push(path);
                    } else {
                        omitted.insert(path);
                    }
                    break;
                }
                cursor = parent.parent();
            }
        }
        let mut members = Vec::new();
        for (&occurrence, &first) in &representative {
            if first == occurrence {
                continue;
            }
            let first_root = store.paths().resolve(first);
            let root = store.paths().resolve(occurrence);
            for &source in descendants.get(&first).into_iter().flatten() {
                let relative = store
                    .paths()
                    .resolve(source)
                    .strip_prefix(first_root)
                    .expect("descendant");
                if let Some(dest) = store.paths().lookup(&root.join(relative)) {
                    members.push((source, dest));
                }
            }
        }
        paths.retain(|path| !omitted.contains(path));
        Self { members }
    }

    pub(super) fn materialize(
        self,
        store: &dyn LayerStore,
        prims: &mut HashMap<PathId, PrimIndex>,
        children: &mut HashMap<PathId, Vec<PathId>>,
        inactive: &mut HashSet<PathId>,
        inactive_children: &mut HashMap<PathId, Vec<PathId>>,
        mut deps: Option<&mut DependencyBuilder>,
        root: LayerId,
    ) -> usize {
        if self.members.is_empty() {
            return 0;
        }
        let mut reused = 0;
        for (source, dest) in self.members {
            // Activation/pruning of the representative applies equally to its
            // occurrences. Their roots remain separately composed and active.
            let Some(index) = prims.get(&source) else {
                continue;
            };
            let mut occurrence = index.clone();
            occurrence
                .graph
                .set_root_site(SpecPath::from_prim_path(dest, store.paths()));
            if let Some(builder) = deps.as_deref_mut() {
                builder.add_layer_opinion(root, dest);
                for key in &occurrence.sources {
                    builder.add_layer_opinion(key.layer_id, dest);
                }
            }
            prims.insert(dest, occurrence);
            // Child ordering and activation are relative to the prototype.
            // Remap both inventories: inactive roots are inspectable, while
            // their descendants remain unpopulated (AOUSD Core §11.3.1).
            if inactive.contains(&source) {
                inactive.insert(dest);
            }
            if let Some(list) = children.get(&source) {
                let mapped = map_children(store, source, dest, list);
                children.insert(dest, mapped);
            } else {
                children.remove(&dest);
            }
            if let Some(list) = inactive_children.get(&source) {
                let mapped = map_children(store, source, dest, list);
                inactive_children.insert(dest, mapped);
            }
            reused += 1;
        }
        // Each instance root was independently composed and ordered. Its raw
        // child list still includes omitted inactive descendants; retain that
        // complete list separately before applying active traversal policy.
        if !inactive.is_empty() {
            for (&parent, list) in children.iter_mut() {
                if list.iter().any(|child| inactive.contains(child)) {
                    inactive_children
                        .entry(parent)
                        .or_insert_with(|| list.clone());
                    list.retain(|child| !inactive.contains(child));
                }
            }
            children.retain(|_, list| !list.is_empty());
        }
        reused
    }
}

fn map_children(
    store: &dyn LayerStore,
    source: PathId,
    dest: PathId,
    list: &[PathId],
) -> Vec<PathId> {
    let source = store.paths().resolve(source);
    let dest = store.paths().resolve(dest);
    list.iter()
        .filter_map(|child| {
            let relative = store.paths().resolve(*child).strip_prefix(source)?;
            store.paths().lookup(&dest.join(relative))
        })
        .collect()
}

fn plain_spec(store: &dyn LayerStore, spec: &PrimSpec) -> bool {
    spec.inherits.is_empty()
        && spec.specializes.is_empty()
        && spec.payloads.is_empty()
        && spec.outer_variant_sites.is_empty()
        && spec.variant_sets.is_empty()
        && spec.variant_selections.is_empty()
        && spec.variant_set_order.is_empty()
        && spec.deleted_variant_sets.is_empty()
        && fields_context_free(store, &spec.fields)
        && spec.properties.iter().all(|entry| {
            let property = &entry.spec;
            property.targets.is_none()
                && property.default.as_ref().is_none_or(value_context_free)
                && property.time_samples.as_ref().is_none_or(|samples| {
                    samples.iter().all(|(_, value)| value_context_free(value))
                })
                && fields_context_free(store, &property.metadata)
        })
}

fn fields_context_free(store: &dyn LayerStore, fields: &[FieldEntry]) -> bool {
    fields.iter().all(|entry| {
        let name = store.tokens().resolve(entry.name);
        !matches!(name, "clips" | "clipSets" | "expressionVariables")
            && !name.starts_with("clips:")
            && match &entry.value {
                FieldValue::Value(value) => value_context_free(value),
                FieldValue::PathListOp(_) => false,
                _ => true,
            }
    })
}

fn value_context_free(value: &Value) -> bool {
    match value {
        Value::PathExpression(_)
        | Value::Asset(_)
        | Value::ArrayEdit(_)
        | Value::TypedArrayEdit(_) => false,
        Value::Array(values) => values.iter().all(value_context_free),
        Value::Dictionary(entries) => entries.iter().all(|(_, value)| value_context_free(value)),
        // Packed typed arrays contain numeric values, not paths or expressions.
        _ => true,
    }
}
