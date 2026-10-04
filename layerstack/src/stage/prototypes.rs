// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshot-owned prototype inventory and exact composed-record sharing.
//! Occurrence graphs retain their namespace and provenance. Only equal opinion
//! records share storage; path-dependent values cannot alias by accident.

use super::*;
use crate::prim_index::PrimIndexData;
use core::hash::{BuildHasher, Hash, Hasher};

/// A cheap sharing bucket, not an identity. Exact equality remains mandatory.
/// Sources and mapped paths separate namespace-dependent records without hashing
/// geometry payloads. Equal records always hash equally; collisions are harmless.
struct RecordBucket<'a>(&'a PrimIndexData);
impl Hash for RecordBucket<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.fields.hash(state);
        self.0.sources.hash(state);
        for opinion in &self.0.opinions {
            opinion.key.hash(state);
            opinion.field.hash(state);
            opinion.layer_offset.offset.to_bits().hash(state);
            opinion.layer_offset.scale.to_bits().hash(state);
            match &opinion.value {
                OpinionValue::Property(spec) => {
                    if let Some(targets) = &spec.targets {
                        hash_targets(targets, state);
                    }
                    // Declared numeric/string attributes cannot contain mapped
                    // expressions. Skip their buffers and sample histories too.
                    if spec.type_name.as_ref().is_none_or(|ty| {
                        matches!(
                            ty.default_scalar,
                            Value::PathExpression(_) | Value::Dictionary(_)
                        )
                    }) {
                        if let Some(value) = &spec.default {
                            hash_expression_paths(value, state);
                        }
                        if let Some(samples) = &spec.time_samples {
                            for (_, value) in samples.as_slice() {
                                hash_expression_paths(value, state);
                            }
                        }
                    }
                    for field in &spec.metadata {
                        hash_field_paths(&field.value, state);
                    }
                }
                OpinionValue::Field(field) => hash_field_paths(field, state),
            }
        }
    }
}
fn hash_targets<H: Hasher>(targets: &ListOp<TargetPath>, state: &mut H) {
    for target in targets.items() {
        target.hash(state);
    }
}
fn hash_field_paths<H: Hasher>(field: &FieldValue, state: &mut H) {
    match field {
        FieldValue::Value(value) => hash_expression_paths(value, state),
        FieldValue::PathListOp(targets) => hash_targets(targets, state),
        _ => {}
    }
}
fn hash_expression_paths<H: Hasher>(value: &Value, state: &mut H) {
    match value {
        Value::PathExpression(text) => text.hash(state),
        Value::Array(values) => {
            for value in values.iter() {
                hash_expression_paths(value, state);
            }
        }
        Value::Dictionary(fields) => {
            for (_, value) in fields {
                hash_expression_paths(value, state);
            }
        }
        // Native numeric arrays cannot contain paths; retain their O(1) bucket cost.
        _ => {}
    }
}

/// What makes two instances share a prototype: the arcs that bring in the
/// instance's descendants, each with its kind, site and offset, and the
/// variant selections, clip definitions and populated descendants. OpenUSD:
/// `PcpInstanceKey`, `Usd_InstanceKey` (`usd/instanceKey.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct InstanceKey {
    arcs: Vec<(ArcKind, crate::LayerStackIdentifier, SpecPath, u64, u64)>,
    selections: Vec<(TokenId, TokenId)>,
    clips: Vec<crate::value_clips::ClipInstanceKey>,
    topology: Vec<(Vec<TokenId>, bool)>,
}

/// Identifies a prototype within one composed stage snapshot.
/// IDs are not persistent across composition or transferable between stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrototypeId(usize);

/// One descendant of a prototype, addressed relative to its instance root.
#[derive(Clone, Debug)]
pub struct PrototypePrim {
    relative_path: Vec<TokenId>,
    representative: PathId,
}
impl PrototypePrim {
    /// Path components relative to the instance root; never empty.
    pub fn relative_path(&self) -> &[TokenId] {
        &self.relative_path
    }
    /// An occurrence to query through the stage's ordinary APIs.
    /// Namespace-dependent targets and values belong to this occurrence.
    pub fn representative(&self) -> PathId {
        self.representative
    }
}

/// A borrowed prototype inventory, excluding instance-root local opinions.
#[derive(Clone, Copy, Debug)]
pub struct Prototype<'a> {
    id: PrototypeId,
    record: &'a PrototypeRecord,
}
impl<'a> Prototype<'a> {
    /// Snapshot-local identity.
    pub fn id(self) -> PrototypeId {
        self.id
    }
    /// Instance roots that share this composition identity, in traversal order.
    pub fn instances(self) -> &'a [PathId] {
        &self.record.instances
    }
    /// Descendants in the first instance's traversal order.
    /// Query an occurrence when targets or path expressions depend on namespace.
    pub fn prims(self) -> &'a [PrototypePrim] {
        &self.record.prims
    }
}
#[derive(Debug, Default)]
struct PrototypeRecord {
    instances: Vec<PathId>,
    prims: Vec<PrototypePrim>,
}
#[derive(Debug, Default)]
pub(super) struct PrototypeTable {
    records: Vec<PrototypeRecord>,
    instances: HashMap<PathId, PrototypeId>,
    members: Vec<Vec<PathId>>,
}

/// Work performed by the most recent complete composition of this snapshot.
/// Incremental value refreshes do not change these initial composition counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompositionWork {
    /// Prim indexes constructed by ordinary composition, including pruned prims.
    pub composed_prim_indexes: usize,
    /// Descendant indexes materialized from a proven equivalent representative.
    pub reused_prim_indexes: usize,
    /// Authored namespace entries visited by discovery queries (duplicates count).
    pub inspected_source_paths: usize,
    /// Source slots indexed, including initial indexing and journal updates.
    pub indexed_source_paths: usize,
}

/// Counts retained composed records, excluding payload allocations and graphs.
/// Logical counts include occurrences; physical counts count shared buffers once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompositionStorage {
    /// Number of composed prim occurrences.
    pub prim_indexes: usize,
    /// Number of distinct opinion-record buffers.
    pub record_buffers: usize,
    /// Opinions counted once per occurrence.
    pub logical_opinions: usize,
    /// Opinions counted once per retained buffer.
    pub physical_opinions: usize,
    /// Field ranges counted once per retained buffer.
    pub physical_fields: usize,
    /// Source keys counted once per retained buffer.
    pub physical_sources: usize,
}

impl Stage {
    /// Reports ordinary composition versus early representative reuse.
    pub fn composition_work(&self) -> CompositionWork {
        self.composition_work
    }
    pub(crate) fn with_composition_work(mut self, work: CompositionWork) -> Self {
        self.composition_work = work;
        self
    }

    /// The [`InstanceKey`] of `instance`: the strongest-first nodes of its
    /// graph whose arcs are authored at the instance, beneath nodes that are
    /// not, with its variant selections.
    ///
    /// OpenUSD: `PcpInstanceKey::_Collector`, which records each instanceable
    /// node that no instanceable node is above (`pxr/usd/pcp/instanceKey.cpp`).
    pub(super) fn instance_key(&self, instance: PathId, store: &dyn LayerStore) -> InstanceKey {
        let depth = u16::try_from(store.paths().resolve(instance).depth()).unwrap_or(u16::MAX);
        let mut arcs = Vec::new();
        if let Some(graph) = self.explain_prim_graph(instance) {
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
                    node.layer_stack_identifier(),
                    node.site().clone(),
                    offset.offset.to_bits(),
                    offset.scale.to_bits(),
                ));
            }
        }
        let mut selections: Vec<(TokenId, TokenId)> = self
            .variant_selections(instance, store)
            .into_iter()
            .collect();
        selections.sort_unstable();
        let clips = self.prims.get(&instance).map_or_else(Vec::new, |index| {
            crate::value_clips::instance_keys(instance, index, store)
        });
        // Different relative population masks cannot share a complete inventory
        // or a flattened prototype. Global payload policy is already reflected
        // in the composed arcs and this populated namespace.
        let prefix = store.paths().resolve(instance);
        let mut topology: Vec<_> = self
            .traverse(instance)
            .filter(|prim| *prim != instance)
            .map(|prim| {
                (
                    store
                        .paths()
                        .resolve(prim)
                        .strip_prefix(prefix)
                        .expect("descendant")
                        .to_vec(),
                    self.is_instance(prim),
                )
            })
            .collect();
        topology.sort_unstable();
        InstanceKey {
            arcs,
            selections,
            clips,
            topology,
        }
    }

    /// Returns the prototype of a composed instance root.
    pub fn instance_prototype(&self, instance: PathId) -> Option<PrototypeId> {
        self.prototypes.instances.get(&instance).copied()
    }
    /// Iterates snapshot-owned prototypes in first-instance traversal order.
    pub fn prototypes(&self) -> impl Iterator<Item = Prototype<'_>> {
        self.prototypes
            .records
            .iter()
            .enumerate()
            .map(|(id, record)| Prototype {
                id: PrototypeId(id),
                record,
            })
    }
    /// Reports exact retained record counts without decoding numeric payloads.
    pub fn composition_storage(&self) -> CompositionStorage {
        let mut seen = HashSet::new();
        let mut result = CompositionStorage::default();
        for index in self.prims.values() {
            result.prim_indexes += 1;
            result.logical_opinions += index.opinions.len();
            if seen.insert(Arc::as_ptr(&index.data)) {
                result.record_buffers += 1;
                result.physical_opinions += index.opinions.len();
                result.physical_fields += index.fields.len();
                result.physical_sources += index.sources.len();
            }
        }
        result
    }
    pub(super) fn prepare_prototypes(&mut self, store: &dyn LayerStore) {
        let mut table = PrototypeTable::default();
        if self.instances.is_empty() {
            self.prototypes = table;
            return;
        }
        let mut identities = HashMap::new();
        let mut relative_groups: Vec<HashMap<Vec<TokenId>, Vec<PathId>>> = Vec::new();
        let root = store.paths().lookup(&crate::Path::root());
        let instances: Vec<_> = root
            .into_iter()
            .flat_map(|root| self.traverse(root))
            .filter(|p| self.instances.contains(p))
            .collect();
        for instance in instances {
            let key = self.instance_key(instance, store);
            let id = *identities.entry(key).or_insert_with(|| {
                let id = PrototypeId(table.records.len());
                table.records.push(PrototypeRecord::default());
                relative_groups.push(HashMap::new());
                id
            });
            table.instances.insert(instance, id);
            let record = &mut table.records[id.0];
            let first = record.instances.is_empty();
            record.instances.push(instance);
            let prefix = store.paths().resolve(instance);
            for prim in self.traverse(instance).filter(|p| *p != instance) {
                let relative = store
                    .paths()
                    .resolve(prim)
                    .strip_prefix(prefix)
                    .expect("descendant")
                    .to_vec();
                if first {
                    record.prims.push(PrototypePrim {
                        relative_path: relative.clone(),
                        representative: prim,
                    });
                }
                relative_groups[id.0]
                    .entry(relative)
                    .or_default()
                    .push(prim);
            }
        }
        table.members = relative_groups
            .into_iter()
            .flat_map(HashMap::into_values)
            .filter(|members| members.len() > 1)
            .collect();
        self.prototypes = table;
        self.reshare_prototype_records();
    }
    /// Returns exact comparison work so tests can assert scaling without timing.
    pub(super) fn reshare_prototype_records(&mut self) -> usize {
        self.reshare_records(None)
    }
    /// Value-only edits need to revisit only relative paths whose records changed.
    pub(super) fn reshare_changed_prototype_records(&mut self, changed: &HashSet<PathId>) -> usize {
        self.reshare_records(Some(changed))
    }
    fn reshare_records(&mut self, changed: Option<&HashSet<PathId>>) -> usize {
        // AOUSD Core §11.4: descendants exclude opinions authored directly
        // beneath an instance. Equality additionally guards mapped targets,
        // anchored expressions, source sites, offsets and graph-node identities.
        let builder = hashbrown::DefaultHashBuilder::default();
        let mut comparisons = 0;
        for members in &self.prototypes.members {
            if changed.is_some_and(|paths| !members.iter().any(|member| paths.contains(member))) {
                continue;
            }
            let mut buckets: HashMap<u64, Vec<Arc<PrimIndexData>>> = HashMap::new();
            for member in members {
                let Some(index) = self.prims.get_mut(member) else {
                    continue;
                };
                let candidates = buckets
                    .entry(builder.hash_one(RecordBucket(&index.data)))
                    .or_default();
                if let Some(shared) = candidates.iter().find(|data| {
                    comparisons += 1;
                    data.same_records(&index.data)
                }) {
                    index.data = Arc::clone(shared);
                } else {
                    candidates.push(Arc::clone(&index.data));
                }
            }
        }
        comparisons
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InMemoryStore, Layer, LiveStage, PrimSpec, Reference};
    use alloc::vec;

    fn scene() -> (InMemoryStore, Vec<PathId>, PathId, TokenId) {
        let mut store = InMemoryStore::default();
        let asset = store.path("/Asset");
        let child = store.path("/Asset/Child");
        let name = store.tokens.intern("Child");
        let field = store.tokens.intern("value");
        let mut source = Layer::new(LayerId(2));
        source.insert_prim(asset, PrimSpec::def().with_children(vec![name]));
        source.insert_prim(child, PrimSpec::def().with_field(field, Value::Int(7)));
        store.insert_layer(source);
        let mut root = Layer::new(LayerId(1));
        let instances: Vec<_> = (0..100)
            .map(|n| {
                let path = store.path(&alloc::format!("/I{n}"));
                root.insert_prim(
                    path,
                    PrimSpec::def()
                        .with_instanceable(true)
                        .with_field(field, Value::Int(n))
                        .with_reference(Reference::with_asset(LayerId(2), asset, "asset.usda")),
                );
                path
            })
            .collect();
        store.insert_layer(root);
        (store, instances, child, field)
    }

    #[test]
    fn shares_descendants_and_preserves_local_roots() {
        let (mut store, instances, _, field) = scene();
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let prototypes: Vec<_> = stage.prototypes().collect();
        assert_eq!(prototypes.len(), 1);
        assert_eq!(prototypes[0].instances().len(), 100);
        assert_eq!(prototypes[0].prims().len(), 1);
        for (n, instance) in instances.iter().enumerate() {
            assert_eq!(
                stage.instance_prototype(*instance),
                Some(prototypes[0].id())
            );
            assert_eq!(
                stage.resolve_field(*instance, field).unwrap().value,
                Value::Int(i32::try_from(n).unwrap())
            );
            let child = stage.traverse(*instance).nth(1).unwrap();
            assert_eq!(
                stage.resolve_field(child, field).unwrap().value,
                Value::Int(7)
            );
        }
        let stats = stage.composition_storage();
        assert!(stats.record_buffers <= 103, "{stats:?}");
        assert!(stats.logical_opinions > stats.physical_opinions);
    }

    #[test]
    fn shared_records_detach_on_incremental_edits() {
        let (mut store, instances, source, field) = scene();
        let old = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .insert_prim(source, PrimSpec::def().with_field(field, Value::Int(19)));
        live.synchronize(&mut store);
        for instance in instances {
            let child = old.traverse(instance).nth(1).unwrap();
            assert_eq!(
                old.resolve_field(child, field).unwrap().value,
                Value::Int(7)
            );
            assert_eq!(
                live.stage().resolve_field(child, field).unwrap().value,
                Value::Int(19)
            );
        }
        assert!(live.stage().composition_storage().record_buffers <= 103);
    }

    #[test]
    fn sharing_keeps_float_representation_separate_from_public_equality() {
        let (mut store, instances, source, field) = scene();
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_field(field, Value::Float(f32::NAN)),
        );
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert!(
            stage.composition_storage().record_buffers <= 103,
            "identical NaN records still share"
        );
        let child = stage.traverse(instances[0]).nth(1).unwrap();
        let records = &stage.prims[&child].data;
        assert!(records.same_records(records));
        let mut positive = records.as_ref().clone();
        positive
            .opinions
            .iter_mut()
            .find(|op| op.field == field)
            .unwrap()
            .value = OpinionValue::Field(Value::Float(0.).into());
        let mut negative = positive.clone();
        negative
            .opinions
            .iter_mut()
            .find(|op| op.field == field)
            .unwrap()
            .value = OpinionValue::Field(Value::Float(-0.).into());
        assert!(
            !positive.same_records(&negative),
            "signed zero must not alias"
        );
    }

    #[test]
    fn mapped_connections_bucket_linearly_and_keep_each_occurrences_namespace() {
        let (mut store, instances, _, _) = scene();
        let source = store.property_path("/Asset/Child.outputs:link");
        let target = store.property_path("/Asset/Child.outputs:target");
        store.layers.get_mut(&LayerId(2)).unwrap().set_property(
            source,
            PropertySpec::attribute()
                .with_targets(ListOp::explicit(vec![TargetPath::Property(target)])),
        );
        let mut stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        for instance in &instances {
            let child = stage.traverse(*instance).nth(1).unwrap();
            let connection = PropertyPath::new(child, source.property());
            assert_eq!(
                stage.resolve_target_list_path(connection).unwrap().value,
                vec![TargetPath::Property(PropertyPath::new(
                    child,
                    target.property()
                ))]
            );
        }
        let comparisons = stage.reshare_prototype_records();
        assert!(
            comparisons < instances.len(),
            "unique remapped records must not compare every earlier occurrence: {comparisons}"
        );
        let source_only = HashSet::from([source.prim_path()]);
        assert_eq!(
            stage.reshare_changed_prototype_records(&source_only),
            0,
            "an edit outside prototype members must not scan their candidate records"
        );
    }

    #[test]
    fn bucket_collisions_preserve_signed_zero_and_share_identical_nan_records() {
        let (mut store, instances, _, field) = scene();
        let mut stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        let children: Vec<_> = instances
            .iter()
            .map(|p| stage.traverse(*p).nth(1).unwrap())
            .collect();
        for (i, child) in children.iter().enumerate() {
            let value = match i {
                0 => 0.,
                1 => -0.,
                _ => f32::from_bits(0x7fc0_1234),
            };
            stage
                .prims
                .get_mut(child)
                .unwrap()
                .opinions
                .iter_mut()
                .find(|op| op.field == field)
                .unwrap()
                .value = OpinionValue::Field(Value::Float(value).into());
        }
        let builder = hashbrown::DefaultHashBuilder::default();
        let bucket = builder.hash_one(RecordBucket(&stage.prims[&children[0]].data));
        assert!(
            children
                .iter()
                .all(|p| builder.hash_one(RecordBucket(&stage.prims[p].data)) == bucket),
            "numeric values intentionally collide in the cheap path bucket"
        );
        stage.reshare_prototype_records();
        assert!(!Arc::ptr_eq(
            &stage.prims[&children[0]].data,
            &stage.prims[&children[1]].data
        ));
        assert!(!Arc::ptr_eq(
            &stage.prims[&children[1]].data,
            &stage.prims[&children[2]].data
        ));
        assert!(Arc::ptr_eq(
            &stage.prims[&children[2]].data,
            &stage.prims[&children[3]].data
        ));
    }

    #[test]
    fn relative_population_masks_separate_prototype_inventories() {
        let (mut store, instances, _, _) = scene();
        let leaf = store.path("/Asset/Other");
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .insert_prim(leaf, PrimSpec::def());
        let asset = store.path("/Asset");
        let child = store.tokens.intern("Child");
        let other = store.tokens.intern("Other");
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .insert_prim(asset, PrimSpec::def().with_children(vec![child, other]));
        let first_child = store.path("/I0/Child");
        let second_child = store.path("/I1/Child");
        let second_other = store.path("/I1/Other");
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                mask: Some(PopulationMask {
                    include: alloc::vec![first_child, second_child, second_other],
                }),
                ..StageOptions::default()
            },
        );
        assert_eq!(stage.prototypes().count(), 2);
        assert_ne!(
            stage.instance_prototype(instances[0]),
            stage.instance_prototype(instances[1])
        );
        let sizes: Vec<_> = stage.prototypes().map(|p| p.prims().len()).collect();
        assert_eq!(sizes, [1, 2]);
    }

    #[test]
    fn offsets_separate_prototype_identity() {
        let (mut store, instances, _, _) = scene();
        let asset = store.path("/Asset");
        let changed = instances[0];
        let mut reference = Reference::with_asset(LayerId(2), asset, "asset.usda");
        reference.layer_offset = crate::LayerOffset {
            offset: 10.,
            scale: 2.,
        };
        store.layers.get_mut(&LayerId(1)).unwrap().insert_prim(
            changed,
            PrimSpec::def()
                .with_instanceable(true)
                .with_reference(reference),
        );
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert_eq!(stage.prototypes().count(), 2);
        assert_ne!(
            stage.instance_prototype(changed),
            stage.instance_prototype(instances[1])
        );
    }
}

#[cfg(test)]
mod early_reuse_tests {
    use super::*;
    use crate::{
        FieldEntry, InMemoryStore, LiveStage, PopulationMask, PrimSpec, PropertyPath, PropertySpec,
        PropertyType, Reference, TargetPath, VariantSetSpec, VariantSpec,
    };
    use alloc::vec;

    fn scene() -> (InMemoryStore, PathId, TokenId) {
        let mut store = InMemoryStore::default();
        let asset = store.path("/Asset");
        let a = store.path("/Asset/A");
        let b = store.path("/Asset/A/B");
        let c = store.path("/Asset/A/C");
        let hidden = store.path("/Asset/Hidden");
        let field = store.tokens.intern("value");
        let a_name = store.tokens.intern("A");
        let b_name = store.tokens.intern("B");
        let c_name = store.tokens.intern("C");
        let hidden_name = store.tokens.intern("Hidden");
        let mut source = crate::Layer::new(LayerId(2));
        source.insert_prim(
            asset,
            PrimSpec::def().with_children(vec![a_name, hidden_name]),
        );
        source.insert_prim(a, PrimSpec::def().with_children(vec![c_name, b_name]));
        source.insert_prim(
            b,
            PrimSpec::def().with_property(
                field,
                PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                    .with_default(Value::Int(7)),
            ),
        );
        source.insert_prim(c, PrimSpec::def().with_field(field, Value::Int(11)));
        source.insert_prim(
            hidden,
            PrimSpec {
                active: Some(false),
                ..PrimSpec::def()
            },
        );
        store.insert_layer(source);
        let mut layer = crate::Layer::new(LayerId(1));
        for (name, value) in [("Left", 1), ("Right", 2)] {
            let root = store.path(&alloc::format!("/{name}"));
            layer.insert_prim(
                root,
                PrimSpec::def()
                    .with_instanceable(true)
                    .with_field(field, Value::Int(value))
                    .with_reference(Reference::with_asset(LayerId(2), asset, "asset.usda")),
            );
            let local = store.path(&alloc::format!("/{name}/A/B"));
            layer.insert_prim(
                local,
                PrimSpec::over().with_property(
                    field,
                    PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                        .with_default(Value::Int(99)),
                ),
            );
            let local_only = store.path(&alloc::format!("/{name}/LocalOnly"));
            layer.insert_prim(local_only, PrimSpec::def());
        }
        store.insert_layer(layer);
        (store, b, field)
    }

    fn assert_matches_full(store: &mut InMemoryStore, reused: usize) -> Stage {
        let options = StageOptions {
            with_dependencies: true,
            with_provenance: true,
            ..StageOptions::default()
        };
        let optimized = Stage::compose(store, LayerId(1), options.clone());
        let include = optimized.prims.keys().copied().collect();
        let full = Stage::compose(
            store,
            LayerId(1),
            StageOptions {
                mask: Some(PopulationMask { include }),
                ..options
            },
        );
        assert_eq!(optimized.composition_work().reused_prim_indexes, reused);
        assert_eq!(full.composition_work().reused_prim_indexes, 0);
        assert_eq!(optimized.children, full.children);
        assert_eq!(optimized.inactive, full.inactive);
        assert_eq!(optimized.inactive_children, full.inactive_children);
        assert_eq!(optimized.instances, full.instances);
        assert_eq!(optimized.prims.len(), full.prims.len());
        assert_eq!(
            alloc::format!("{:?}", optimized.composition_errors()),
            alloc::format!("{:?}", full.composition_errors())
        );
        for (path, index) in &optimized.prims {
            let expected = &full.prims[path];
            assert!(
                index.data.same_records(&expected.data),
                "records differ at {:?}",
                store.paths.resolve(*path)
            );
            assert_eq!(
                alloc::format!("{:?}", index.graph),
                alloc::format!("{:?}", expected.graph),
                "graph differs at {:?}",
                store.paths.resolve(*path)
            );
            let mut layers = optimized.layers_affecting_prim(*path);
            let mut expected_layers = full.layers_affecting_prim(*path);
            layers.sort_unstable();
            expected_layers.sort_unstable();
            assert_eq!(layers, expected_layers);
        }
        optimized
    }

    #[test]
    fn early_reuse_matches_full_graphs_ordering_activation_and_local_stripping() {
        let (mut store, _, field) = scene();
        let right = store.path("/Right");
        let hidden = store.tokens.intern("Hidden");
        let a = store.tokens.intern("A");
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&right)
            .unwrap()
            .prim_order = Some(vec![hidden, a]);
        let stage = assert_matches_full(&mut store, 4);
        for (name, value) in [("Left", 1), ("Right", 2)] {
            let root = store.path(&alloc::format!("/{name}"));
            assert_eq!(
                stage.resolve_field(root, field).unwrap().value,
                Value::Int(value)
            );
            let child = store.path(&alloc::format!("/{name}/A/B"));
            assert_eq!(
                stage
                    .resolve_field_path(PropertyPath::new(child, field))
                    .unwrap()
                    .value,
                Value::Int(7)
            );
            assert!(!stage.has_prim(store.path(&alloc::format!("/{name}/LocalOnly"))));
            let hidden = store.path(&alloc::format!("/{name}/Hidden"));
            assert!(stage.has_prim(hidden));
            assert!(!stage.is_active(hidden));
            assert!(!stage.traverse(root).any(|prim| prim == hidden));
            assert!(stage.traverse_all(root).any(|prim| prim == hidden));
        }
    }

    #[test]
    fn namespace_dependent_targets_and_expressions_use_full_composition() {
        let (mut store, source, field) = scene();
        let target = TargetPath::prim(source);
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_property(
                field,
                PropertySpec::relationship().with_targets(ListOp::explicit(vec![target])),
            ),
        );
        let stage = assert_matches_full(&mut store, 0);
        for name in ["Left", "Right"] {
            let child = store.path(&alloc::format!("/{name}/A/B"));
            assert_eq!(
                stage
                    .resolve_target_list_path(PropertyPath::new(child, field))
                    .unwrap()
                    .value,
                vec![TargetPath::prim(child)]
            );
        }
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_property(
                field,
                PropertySpec::typed_attribute(PropertyType::new(
                    "pathExpression",
                    false,
                    Value::PathExpression(Arc::from("")),
                ))
                .with_default(Value::PathExpression(Arc::from("../C"))),
            ),
        );
        let stage = assert_matches_full(&mut store, 0);
        for name in ["Left", "Right"] {
            let child = store.path(&alloc::format!("/{name}/A/B"));
            assert_eq!(
                stage
                    .resolve_field_path(PropertyPath::new(child, field))
                    .unwrap()
                    .value,
                Value::PathExpression(Arc::from(alloc::format!("/{name}/A/C")))
            );
        }
    }

    #[test]
    fn source_edits_update_every_occurrence_and_preserve_old_snapshot() {
        let (mut store, source, field) = scene();
        let old = assert_matches_full(&mut store, 4);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_property(
                field,
                PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                    .with_default(Value::Int(23)),
            ),
        );
        live.synchronize(&mut store);
        for name in ["Left", "Right"] {
            let child = store.path(&alloc::format!("/{name}/A/B"));
            assert_eq!(
                old.resolve_field_path(PropertyPath::new(child, field))
                    .unwrap()
                    .value,
                Value::Int(7)
            );
            assert_eq!(
                live.stage()
                    .resolve_field_path(PropertyPath::new(child, field))
                    .unwrap()
                    .value,
                Value::Int(23)
            );
        }
        assert_matches_full(&mut store, 4);
        let new_source = store.path("/Asset/A/New");
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            new_source,
            PrimSpec::def().with_field(field, Value::Int(29)),
        );
        live.synchronize(&mut store);
        for name in ["Left", "Right"] {
            assert!(
                live.stage()
                    .has_prim(store.path(&alloc::format!("/{name}/A/New")))
            );
        }
        assert_matches_full(&mut store, 5);
    }

    #[test]
    fn clips_and_nested_arcs_exclude_early_reuse() {
        let (mut store, source, _) = scene();
        let clips = store.tokens.intern("clips");
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_field(clips, Value::Dictionary(vec![])),
        );
        assert_matches_full(&mut store, 0);
        let remote = store.path("/Other");
        let mut other = crate::Layer::new(LayerId(3));
        other.insert_prim(remote, PrimSpec::def());
        store.insert_layer(other);
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_reference(Reference::with_asset(LayerId(3), remote, "other.usda")),
        );
        assert_matches_full(&mut store, 0);
    }
    #[test]
    fn retargeting_one_instance_rebuilds_its_group_without_changing_others() {
        let (mut store, _, field) = scene();
        let old = assert_matches_full(&mut store, 4);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let asset = store.path("/Asset");
        let source = store.path("/Asset/A/B");
        let mut other = store.layers[&LayerId(2)].clone();
        other.id = LayerId(3);
        other.insert_prim(
            source,
            PrimSpec::def().with_property(
                field,
                PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                    .with_default(Value::Int(31)),
            ),
        );
        store.insert_layer(other);
        let right = store.path("/Right");
        store.layers.get_mut(&LayerId(1)).unwrap().insert_prim(
            right,
            PrimSpec::def()
                .with_instanceable(true)
                .with_reference(Reference::with_asset(LayerId(3), asset, "other.usda")),
        );
        live.synchronize(&mut store);
        for (name, expected) in [("Left", 7), ("Right", 31)] {
            let child = store.path(&alloc::format!("/{name}/A/B"));
            let property = PropertyPath::new(child, field);
            assert_eq!(
                old.resolve_field_path(property).unwrap().value,
                Value::Int(7)
            );
            assert_eq!(
                live.stage().resolve_field_path(property).unwrap().value,
                Value::Int(expected)
            );
        }
        assert_matches_full(&mut store, 0);
    }

    #[test]
    fn selected_variants_use_the_full_context_path() {
        let (mut store, source, field) = scene();
        let mode = store.tokens.intern("mode");
        let selected = store.tokens.intern("selected");
        let mut spec = PrimSpec::def();
        spec.variant_selections.insert(mode, selected);
        spec.variant_sets.insert(
            mode,
            VariantSetSpec {
                variants: HashMap::from([(
                    selected,
                    VariantSpec {
                        fields: vec![FieldEntry {
                            name: field,
                            value: Value::Int(37).into(),
                        }],
                        ..VariantSpec::default()
                    },
                )]),
            },
        );
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .insert_prim(source, spec);
        let stage = assert_matches_full(&mut store, 0);
        for name in ["Left", "Right"] {
            let child = store.path(&alloc::format!("/{name}/A/B"));
            assert_eq!(
                stage.resolve_field(child, field).unwrap().value,
                Value::Int(37)
            );
        }
    }
    #[test]
    fn reused_inactive_roots_preserve_all_children_and_exclude_descendants() {
        let (mut store, _, field) = scene();
        let hidden = store.path("/Asset/A/NestedHidden");
        let secret = store.path("/Asset/A/NestedHidden/Secret");
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            hidden,
            PrimSpec {
                active: Some(false),
                ..PrimSpec::def()
            },
        );
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .insert_prim(secret, PrimSpec::def().with_field(field, Value::Int(41)));
        let stage = assert_matches_full(&mut store, 5);
        for name in ["Left", "Right"] {
            let root = store.path(&alloc::format!("/{name}"));
            let parent = store.path(&alloc::format!("/{name}/A"));
            let hidden = store.path(&alloc::format!("/{name}/A/NestedHidden"));
            let secret = store.path(&alloc::format!("/{name}/A/NestedHidden/Secret"));
            assert!(stage.has_prim(hidden));
            assert!(!stage.is_active(hidden));
            assert!(!stage.has_prim(secret));
            assert!(stage.all_children_of(parent).unwrap().contains(&hidden));
            assert!(!stage.children_of(parent).unwrap().contains(&hidden));
            assert!(stage.all_children_of(hidden).is_none());
            assert!(stage.traverse_all(root).any(|prim| prim == hidden));
            assert!(!stage.traverse(root).any(|prim| prim == hidden));
        }
    }
    #[test]
    fn instance_root_activation_overrides_do_not_reuse_pruned_representatives() {
        let (mut store, _, field) = scene();
        let asset = store.path("/Asset");
        store
            .layers
            .get_mut(&LayerId(2))
            .unwrap()
            .prims
            .get_mut(&asset)
            .unwrap()
            .active = Some(false);
        let right = store.path("/Right");
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .prims
            .get_mut(&right)
            .unwrap()
            .active = Some(true);
        let stage = assert_matches_full(&mut store, 0);
        let left = store.path("/Left");
        let left_child = store.path("/Left/A/B");
        let right_child = store.path("/Right/A/B");
        assert!(stage.has_prim(left));
        assert!(!stage.is_active(left));
        assert!(!stage.has_prim(left_child));
        assert!(stage.is_active(right));
        assert_eq!(
            stage
                .resolve_field_path(PropertyPath::new(right_child, field))
                .unwrap()
                .value,
            Value::Int(7)
        );
    }
}
