// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshot-owned prototype inventory and exact composed-record sharing.
//! Occurrence graphs retain their namespace and provenance. Only equal opinion
//! records share storage; path-dependent values cannot alias by accident.

use super::*;
use crate::prim_index::PrimIndexData;
use crate::sort;
use alloc::collections::BTreeSet;
use core::hash::{BuildHasher, Hash, Hasher};

/// A cheap sharing bucket, not an identity. Exact equality remains mandatory.
/// Sources and mapped paths separate namespace-dependent records without hashing
/// geometry payloads. Equal records always hash equally; collisions are harmless.
struct RecordBucket<'a>(&'a PrimIndexData);
impl Hash for RecordBucket<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.fields.hash(state);
        for source in &self.0.sources {
            source.key.hash(state);
            source.layer_offset.offset.to_bits().hash(state);
            source.layer_offset.scale.to_bits().hash(state);
        }
        for opinion in &self.0.opinions {
            opinion.source.hash(state);
            opinion.field.hash(state);
            match &opinion.value {
                OpinionValue::Property(spec) => {
                    if let Some(targets) = &spec.targets {
                        hash_targets(targets, state);
                    }
                    // Declared numeric/string attributes cannot contain mapped
                    // expressions. Skip their buffers and sample histories too.
                    if spec.type_name.as_ref().is_none_or(|ty| {
                        matches!(
                            ty.default_scalar(),
                            Value::PathExpression(_) | Value::Dictionary(_)
                        )
                    }) {
                        if let Some(value) = &spec.default {
                            hash_expression_paths(value, state);
                        }
                        if let Some(samples) = spec.time_samples.get() {
                            for (_, value) in samples {
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
                if let FieldValue::Value(value) = value {
                    hash_expression_paths(value, state);
                }
            }
        }
        // Native numeric arrays cannot contain paths; retain their O(1) bucket cost.
        _ => {}
    }
}

/// What makes two instances share a prototype: the arcs that bring in the
/// instance's descendants, each with its kind, site and offset, and the
/// variant selections ([`InstanceIndexKey`]), clip definitions and populated
/// descendants. OpenUSD: `PcpInstanceKey`, `Usd_InstanceKey`
/// (`usd/instanceKey.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct InstanceKey {
    root: InstanceRootKey,
    topology: Vec<(Vec<TokenId>, bool)>,
}

/// Identity contributed by an instance root independently of its population
/// mask. Keep one per prototype so a partial composition can compare its
/// roots even when it only visited some descendants.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct InstanceRootKey {
    index: InstanceIndexKey,
    clips: Vec<crate::value_clips::ClipInstanceKey>,
}

/// The part of an instance's [`InstanceKey`] that the instance root's own
/// prim index decides: its strongest-first contributing direct arc
/// subtrees, each with its kind, layer stack, site and offset, and its
/// variant selections.
///
/// Composition finds it on the thread that indexes the root, while the
/// index is at hand ([`InstanceIndexKeys`]); [`Stage::instance_key`] finds
/// it again for an index built otherwise.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InstanceIndexKey {
    arcs: Vec<(ArcKind, crate::LayerStackIdentifier, SpecPath, u64, u64)>,
    selections: Vec<(TokenId, TokenId)>,
}

/// The [`InstanceIndexKey`] of each instance root a composition indexed
/// itself, for [`Stage::prepare_prototypes`]; roots it copied from another
/// instance's descendants have none.
pub(crate) type InstanceIndexKeys = HashMap<PathId, InstanceIndexKey>;

impl InstanceIndexKey {
    /// The key part of the instance root `index`, at namespace depth
    /// `depth`, with the stage's variant `fallbacks`.
    ///
    /// Nested contributing nodes are included when payload participation can
    /// distinguish otherwise equal outer arcs. OpenUSD:
    /// `PcpInstanceKey::_Collector`, which records each instanceable node,
    /// pruning below it only when there are no payload arcs
    /// (`pxr/usd/pcp/instanceKey.cpp`).
    pub(crate) fn of(
        index: &PrimIndex,
        depth: usize,
        store: &dyn LayerStore,
        fallbacks: &VariantFallbacks,
    ) -> Self {
        let depth = u16::try_from(depth).unwrap_or(u16::MAX);
        let mut arcs = Vec::new();
        let graph = &index.graph;
        let nodes: HashSet<NodeId> = graph
            .instanceable_nodes(store.paths(), &index.sources, depth)
            .collect();
        // Loaded payload participation is part of the key even beneath
        // another contributing arc (OpenUSD PcpInstanceKey::_Collector).
        let has_payload = graph
            .nodes()
            .any(|(_, node)| node.arc_kind() == ArcKind::Payloads);
        for id in graph.strength_order() {
            let Some(node) = graph.node(id) else { continue };
            if !nodes.contains(&id) {
                continue;
            }
            let mut parent = node.parent();
            let mut parent_instanceable = false;
            while let Some(id) = parent {
                if nodes.contains(&id) {
                    parent_instanceable = true;
                    break;
                }
                parent = graph.node(id).and_then(|node| node.parent());
            }
            if !has_payload && parent_instanceable {
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
        let mut selections: Vec<(TokenId, TokenId)> =
            crate::compose::strength_ordered_variant_selections(store, fallbacks, index)
                .into_iter()
                .collect();
        sort::sort_unstable(&mut selections);
        Self { arcs, selections }
    }
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
#[derive(Debug)]
struct PrototypeRecord {
    root_key: InstanceRootKey,
    instances: Vec<PathId>,
    prims: Vec<PrototypePrim>,
}
#[derive(Debug, Default)]
pub(super) struct PrototypeTable {
    records: Vec<PrototypeRecord>,
    instances: HashMap<PathId, PrototypeId>,
}

/// Work performed by the most recent complete composition of this snapshot.
/// Incremental value refreshes do not change these initial composition counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CompositionWork {
    /// Prim indexes constructed by ordinary composition, including pruned prims.
    pub composed_prim_indexes: usize,
    /// Descendant indexes materialized from a proven equivalent representative.
    pub reused_prim_indexes: usize,
    /// The nodes of the prim indexes composed (culled nodes included) and
    /// materialized: the work [`crate::CompositionLimits::prim_index_nodes`]
    /// bounds. The same on any number of threads.
    pub prim_index_nodes: usize,
}

/// Counts retained composed records, excluding payload allocations and graphs.
/// Logical counts include occurrences; physical counts count shared buffers once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
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

    /// The [`InstanceIndexKey`] of the instance root `instance`.
    fn instance_index_key(&self, instance: PathId, store: &dyn LayerStore) -> InstanceIndexKey {
        let depth = store.paths().resolve(instance).depth();
        match self.prims.get(&instance) {
            Some(index) => InstanceIndexKey::of(index, depth, store, &self.variant_fallbacks),
            None => InstanceIndexKey {
                arcs: Vec::new(),
                selections: Vec::new(),
            },
        }
    }

    /// The [`InstanceKey`] of `instance`: its [`InstanceIndexKey`], clip
    /// definitions and populated namespace.
    pub(super) fn instance_key(&self, instance: PathId, store: &dyn LayerStore) -> InstanceKey {
        let index = self.instance_index_key(instance, store);
        self.instance_key_with(instance, index, store)
    }

    /// The [`InstanceKey`] of `instance`, whose [`InstanceIndexKey`] is
    /// `index`.
    fn instance_key_with(
        &self,
        instance: PathId,
        index: InstanceIndexKey,
        store: &dyn LayerStore,
    ) -> InstanceKey {
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
        sort::sort_unstable(&mut topology);
        InstanceKey {
            root: InstanceRootKey { index, clips },
            topology,
        }
    }

    /// Returns the prototype of a composed instance root.
    pub(crate) fn instance_prototype(&self, instance: PathId) -> Option<PrototypeId> {
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

    /// Whether replaced prims may change instance identity, membership or
    /// visibility. The caller separately checks any visible child lists it replaces.
    pub(super) fn prototype_indexes_changed(&self, partial: &Self, replaced: &[PathId]) -> bool {
        let had_instances = !self.instances.is_empty();
        if !had_instances && partial.instances.is_empty() {
            return false;
        }
        // Only a root index contributes arcs, selections and clip definitions to
        // its key. A descendant's record can change independently of its place
        // in the populated namespace (AOUSD Core §11.3.3).
        replaced.iter().any(|&path| {
            if self.is_active(path) != partial.is_active(path) {
                return true;
            }
            let was_instance = self.instances.contains(&path);
            if was_instance != partial.instances.contains(&path) {
                return true;
            }
            was_instance
                && self
                    .instance_prototype(path)
                    .zip(partial.instance_prototype(path))
                    .is_none_or(|(old, new)| {
                        self.prototypes.records[old.0].root_key
                            != partial.prototypes.records[new.0].root_key
                    })
        })
    }

    /// Groups the stage's instances into prototypes by [`InstanceKey`], in
    /// traversal order, reading each root's [`InstanceIndexKey`] from
    /// `keys` when composition found it, and shares equal descendant
    /// records.
    ///
    /// Spec: AOUSD Core §11.3.3 (instances share prototypes). OpenUSD:
    /// `Usd_InstanceCache`.
    pub(super) fn prepare_prototypes(
        &mut self,
        store: &dyn LayerStore,
        mut keys: InstanceIndexKeys,
    ) {
        #[cfg(test)]
        {
            self.prototype_preparations += 1;
        }
        let mut table = PrototypeTable::default();
        if self.instances.is_empty() {
            self.prototypes = table;
            return;
        }
        let mut identities = HashMap::new();
        let root = store.paths().lookup(&Path::root());
        let instances = root
            .into_iter()
            .flat_map(|root| self.traverse(root))
            .filter(|p| self.instances.contains(p));
        for instance in instances {
            let index = match keys.remove(&instance) {
                Some(index) => {
                    debug_assert_eq!(
                        index,
                        self.instance_index_key(instance, store),
                        "composition's key of an instance root"
                    );
                    index
                }
                None => self.instance_index_key(instance, store),
            };
            let key = self.instance_key_with(instance, index, store);
            let id = *identities.entry(key).or_insert_with_key(|key| {
                let id = PrototypeId(table.records.len());
                table.records.push(PrototypeRecord {
                    root_key: key.root.clone(),
                    instances: Vec::new(),
                    prims: Vec::new(),
                });
                id
            });
            table.instances.insert(instance, id);
            let record = &mut table.records[id.0];
            let first = record.instances.is_empty();
            record.instances.push(instance);
            if first {
                let prefix = store.paths().resolve(instance);
                record.prims = self
                    .traverse(instance)
                    .filter(|prim| *prim != instance)
                    .map(|prim| PrototypePrim {
                        relative_path: store
                            .paths()
                            .resolve(prim)
                            .strip_prefix(prefix)
                            .expect("descendant")
                            .to_vec(),
                        representative: prim,
                    })
                    .collect();
            }
        }
        self.prototypes = table;
        self.reshare_prototype_records(store.paths());
    }

    fn reshare_prototype_records(&mut self, paths: &crate::PathInterner) -> usize {
        let mut comparisons = 0;
        for record in &self.prototypes.records {
            if record.instances.len() > 1 {
                for prim in &record.prims {
                    comparisons += record.reshare(&mut self.prims, paths, &prim.relative_path);
                }
            }
        }
        comparisons
    }

    /// Find each affected relative path through its enclosing instance roots.
    /// Nested instances can place one occurrence in more than one group.
    pub(super) fn reshare_changed_prototype_records(
        &mut self,
        paths: &crate::PathInterner,
        changed: impl IntoIterator<Item = PathId>,
    ) -> usize {
        if self.prototypes.instances.is_empty() {
            return 0;
        }
        // Visit overlapping groups in the same prototype order as a full compose.
        let mut groups = BTreeSet::new();
        for prim in changed {
            if !self.is_active(prim) {
                continue;
            }
            for root in core::iter::successors(paths.parent(prim), |&p| paths.parent(p)) {
                if let Some(id) = self.prototypes.instances.get(&root)
                    && self.prototypes.records[id.0].instances.len() > 1
                {
                    let relative = paths
                        .resolve(prim)
                        .strip_prefix(paths.resolve(root))
                        .expect("ancestor");
                    groups.insert((id.0, relative));
                }
            }
        }
        let mut comparisons = 0;
        for (id, relative) in groups {
            comparisons += self.prototypes.records[id].reshare(&mut self.prims, paths, relative);
        }
        comparisons
    }
}

impl PrototypeRecord {
    // The instance key includes the full relative topology. Expand a single
    // group on demand instead of storing every descendant path a second time.
    fn reshare(
        &self,
        prims: &mut HashMap<PathId, PrimIndex>,
        paths: &crate::PathInterner,
        relative: &[TokenId],
    ) -> usize {
        // AOUSD Core §11.4: only exactly equal records may share storage,
        // including mapped targets, anchored expressions and source sites.
        let builder = hashbrown::DefaultHashBuilder::default();
        let mut comparisons = 0;
        let mut buckets: HashMap<u64, Vec<Arc<PrimIndexData>>> = HashMap::new();
        for &instance in &self.instances {
            // An exact partial composition can retain a boundary child name
            // without composing that child's index.
            let Some(index) = paths
                .lookup_joined(instance, relative)
                .and_then(|member| prims.get_mut(&member))
            else {
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
        comparisons
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::Transaction;
    use crate::{EditTarget, InMemoryStore, Layer, LiveStage, PrimSpec, Reference, SublayerEntry};
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
        let comparisons = stage.reshare_prototype_records(store.paths());
        assert!(
            comparisons < instances.len(),
            "unique remapped records must not compare every earlier occurrence: {comparisons}"
        );
        let source_only = [source.prim_path()];
        assert_eq!(
            stage.reshare_changed_prototype_records(store.paths(), source_only),
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
        stage.reshare_prototype_records(store.paths());
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

    /// A stage's prototypes: each one's instances and descendants.
    #[derive(Debug, PartialEq)]
    struct Inventory {
        instances: Vec<PathId>,
        prims: Vec<(Vec<TokenId>, PathId)>,
    }

    fn inventory(stage: &Stage) -> Vec<Inventory> {
        stage
            .prototypes()
            .map(|prototype| Inventory {
                instances: prototype.instances().to_vec(),
                prims: prototype
                    .prims()
                    .iter()
                    .map(|prim| (prim.relative_path().to_vec(), prim.representative()))
                    .collect(),
            })
            .collect()
    }

    fn assert_fresh_prototypes(stage: &Stage, store: &mut InMemoryStore) {
        let fresh = Stage::compose(store, LayerId(1), StageOptions::default());
        assert_eq!(inventory(stage), inventory(&fresh));
        assert_eq!(stage.composition_storage(), fresh.composition_storage());
    }

    #[test]
    fn unrelated_index_and_subtree_edits_keep_prototypes() {
        let (mut store, _, _, _) = scene();
        let root = LayerId(1);
        let weaker = LayerId(3);
        let property = store.property_path("/Settings.label");
        let spec = |text| {
            PropertySpec::typed_attribute(PropertyType::parse("string").unwrap())
                .with_default(Value::string(text))
        };
        let mut layer = Layer::new(weaker);
        layer.insert_prim(
            property.prim_path(),
            PrimSpec::def().with_property(property.property(), spec("original")),
        );
        store.insert_layer(layer);
        store
            .layers
            .get_mut(&root)
            .unwrap()
            .sublayers_mut()
            .push(SublayerEntry::new(weaker));
        let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
        assert_eq!(
            live.stage().prototypes().next().unwrap().instances().len(),
            100
        );

        // An external opinion edit replaces the index alone.
        store
            .layers
            .get_mut(&weaker)
            .unwrap()
            .set_property(property, spec("source edit"));
        assert_eq!(live.notify_changed_layers(&store), [weaker]);
        assert_eq!(live.recompose(&mut store), [property.prim_path()]);
        assert_eq!(live.stage().prototype_preparations, 1);
        assert_fresh_prototypes(live.stage(), &mut store);

        // The first stronger override creates a prim spec as well as the property.
        let mut edit = Transaction::new();
        edit.set_default(
            EditTarget::for_layer(root).property(property),
            Value::string("override"),
        );
        live.apply(&mut store, &edit).unwrap();
        let work = live.recomposition_work();
        assert!(
            !work.full_rebuild && work.replaced_prim_indexes > 0,
            "{work:?}"
        );
        assert_eq!(live.stage().prototype_preparations, 1);
        assert_fresh_prototypes(live.stage(), &mut store);
    }

    #[test]
    fn changed_instance_indexes_and_members_refresh_prototypes() {
        let (mut store, instances, _, _) = scene();
        let root = LayerId(1);
        let asset = LayerId(2);
        let property = store.property_path("/Asset/Child.newValue");
        let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
        store.layers.get_mut(&asset).unwrap().set_property(
            property,
            PropertySpec::attribute().with_default(Value::Int(19)),
        );
        // Every instance's descendant gets a new composed record.
        assert_eq!(live.notify_changed_layers(&store), [asset]);
        let paths = live.recompose(&mut store);
        assert_eq!(paths.len(), instances.len());
        assert_eq!(live.stage().prototype_preparations, 1);
        assert_fresh_prototypes(live.stage(), &mut store);

        let mut edit = Transaction::new();
        edit.set_instanceable(EditTarget::for_layer(root).prim(instances[0]), false);
        live.apply(&mut store, &edit).unwrap();
        assert!(!live.recomposition_work().full_rebuild);
        assert!(!live.stage().is_instance(instances[0]));
        assert_eq!(live.stage().prototype_preparations, 2);
        assert_fresh_prototypes(live.stage(), &mut store);
    }

    #[test]
    fn reordered_boundary_refreshes_prototype_order_and_representative() {
        use alloc::collections::BTreeSet;

        let (mut store, instances, _, _) = scene();
        let root_layer = LayerId(1);
        let options = StageOptions::default();
        let errors = [store.path("/ErrorA"), store.path("/ErrorB")];
        let missing = store.path("/Missing");
        let layer = store.layers.get_mut(&root_layer).unwrap();
        for path in errors {
            layer.insert_prim(
                path,
                PrimSpec::def().with_reference(Reference::with_asset(
                    LayerId(2),
                    missing,
                    "asset.usda",
                )),
            );
        }
        let mut stage = Stage::compose(&mut store, root_layer, options.clone());
        let root = store.path("/");
        let first = |stage: &Stage| stage.prototypes().next().unwrap().instances()[0];
        assert_eq!(first(&stage), instances[0]);
        let error_paths = |stage: &Stage| {
            stage
                .composition_errors()
                .iter()
                .filter_map(CompositionError::prim)
                .collect::<Vec<_>>()
        };
        assert_eq!(error_paths(&stage), errors);
        let order = ["I1", "I0", "ErrorB", "ErrorA"].map(|name| store.tokens().intern(name));
        store
            .layers
            .get_mut(&root_layer)
            .unwrap()
            .insert_prim(root, PrimSpec::default().with_prim_order(order.to_vec()));
        // Only the pseudo-root and its complete child order are replaced.
        let boundaries = Boundaries {
            parents: [root].into_iter().collect(),
            retained: false,
        };
        let child_index = crate::prim_indexing::ChildIndex::default();
        let partial = Stage::compose_local_paths(
            &mut store,
            root_layer,
            options,
            BTreeSet::from([root]),
            &boundaries,
            &child_index,
        );
        let orders = stage.merge_local_subtrees(
            &store,
            partial,
            &[root],
            &[],
            vec![(root, Vec::new())],
            &[],
            &child_index,
        );
        assert_eq!(orders.composed, 1);
        assert_eq!(first(&stage), instances[1]);
        assert_eq!(error_paths(&stage), [errors[1], errors[0]]);
        assert_eq!(stage.prototype_preparations, 2);
        assert_fresh_prototypes(&stage, &mut store);
    }

    /// Instances of `/Asset`, which holds an instance of `/Leaf`.
    fn nested_scene() -> (InMemoryStore, Vec<PathId>) {
        let mut store = InMemoryStore::default();
        let asset = store.path("/Asset");
        let inner = store.path("/Asset/Inner");
        let leaf = store.path("/Leaf");
        let bit = store.path("/Leaf/Bit");
        let inner_name = store.tokens.intern("Inner");
        let bit_name = store.tokens.intern("Bit");
        let mut source = Layer::new(LayerId(2));
        source.insert_prim(asset, PrimSpec::def().with_children(vec![inner_name]));
        source.insert_prim(
            inner,
            PrimSpec::def()
                .with_instanceable(true)
                .with_reference(Reference::with_asset(LayerId(2), leaf, "asset.usda")),
        );
        source.insert_prim(leaf, PrimSpec::def().with_children(vec![bit_name]));
        source.insert_prim(bit, PrimSpec::def());
        store.insert_layer(source);
        let mut root = Layer::new(LayerId(1));
        let instances = (0..8)
            .map(|n| {
                let path = store.path(&alloc::format!("/I{n}"));
                root.insert_prim(
                    path,
                    PrimSpec::def()
                        .with_instanceable(true)
                        .with_reference(Reference::with_asset(LayerId(2), asset, "asset.usda")),
                );
                path
            })
            .collect();
        store.insert_layer(root);
        (store, instances)
    }

    #[test]
    fn masked_nested_descendant_edits_preserve_topology_and_record_sharing() {
        let (mut store, _) = nested_scene();
        let leaf = store.path("/Leaf");
        let other = store.path("/Leaf/Other");
        let bit_name = store.tokens.intern("Bit");
        let other_name = store.tokens.intern("Other");
        let value = store.property_path("/Leaf/Bit.newValue");
        let inners = [store.path("/I0/Inner"), store.path("/I1/Inner")];
        let bits = [
            store.path("/I0/Inner/Bit"),
            store.path("/I1/Inner/Bit"),
            store.path("/I2/Inner/Bit"),
        ];
        let sibling = store.path("/I0/Inner/Other");
        let masked_sibling = store.path("/I2/Inner/Other");
        let root = LayerId(1);
        let asset = LayerId(2);
        let source = store.layers.get_mut(&asset).unwrap();
        source.insert_prim(
            leaf,
            PrimSpec::def().with_children(vec![bit_name, other_name]),
        );
        source.insert_prim(other, PrimSpec::def());
        let options = StageOptions {
            mask: Some(PopulationMask {
                include: vec![inners[0], inners[1], bits[2]],
            }),
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, root, options.clone());
        let before = inventory(live.stage());
        assert_eq!(before.len(), 4);
        assert!(!live.stage().has_prim(masked_sibling));
        let untouched = Arc::clone(&live.stage().prims[&sibling].data);

        // The exact partial composition lists both children of each nested
        // instance, while composing only the affected Bit indexes.
        store.layers.get_mut(&asset).unwrap().set_property(
            value,
            PropertySpec::attribute().with_default(Value::Int(19)),
        );
        live.notify_layer_prim_edits(asset, &[value.prim_path()]);
        assert_eq!(live.recompose(&mut store).len(), bits.len());
        let stage = live.stage();
        assert_eq!(stage.prototype_preparations, 1);
        assert_eq!(inventory(stage), before);
        assert!(!stage.has_prim(masked_sibling));
        assert!(Arc::ptr_eq(&stage.prims[&sibling].data, &untouched));
        assert!(Arc::ptr_eq(
            &stage.prims[&bits[0]].data,
            &stage.prims[&bits[1]].data
        ));
        for bit in bits {
            assert_eq!(
                stage
                    .resolve_field_path(PropertyPath::new(bit, value.property()))
                    .unwrap()
                    .value,
                Value::Int(19)
            );
        }
        let fresh = Stage::compose(&mut store, root, options);
        assert_eq!(inventory(stage), inventory(&fresh));
        for bit in bits {
            assert!(stage.prims[&bit].data.same_records(&fresh.prims[&bit].data));
        }
    }

    #[test]
    fn composition_keys_indexed_roots_and_prototypes_match_serial_keys() {
        let (mut store, instances) = nested_scene();
        let first_inner = store.path("/I0/Inner");
        let copied_inner = store.path("/I1/Inner");
        let composed = crate::compose::compose_stage_retained(
            &mut store,
            LayerId(1),
            StageOptions::default(),
            None,
            false,
            &Boundaries::default(),
            &crate::prim_indexing::ChildIndex::default(),
        );
        // Every outer root is indexed; only the first one's descendants
        // are, and the rest are copies of them.
        for instance in &instances {
            assert!(composed.instance_keys.contains_key(instance));
        }
        assert!(composed.instance_keys.contains_key(&first_inner));
        assert!(!composed.instance_keys.contains_key(&copied_inner));

        let expected = {
            let mut stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
            let found = inventory(&stage);
            // Records are shared among the occurrences of one relative path,
            // one in each instance of a prototype: each outer instance's
            // `Inner` and `Inner/Bit`, and each inner instance's `Bit`.
            let mut members = Vec::new();
            for proto in stage.prototypes() {
                for prim in proto.prims() {
                    members.push(
                        proto
                            .instances()
                            .iter()
                            .map(|&root| {
                                store
                                    .paths()
                                    .lookup_joined(root, prim.relative_path())
                                    .unwrap()
                            })
                            .collect::<Vec<_>>(),
                    );
                }
            }
            members.sort();
            let occurrences = |suffix: &str| -> Vec<PathId> {
                (0..instances.len())
                    .map(|n| store.path(&alloc::format!("/I{n}{suffix}")))
                    .collect()
            };
            let mut expected_members = vec![
                occurrences("/Inner"),
                occurrences("/Inner/Bit"),
                occurrences("/Inner/Bit"),
            ];
            expected_members.sort();
            assert_eq!(members, expected_members);
            stage.prepare_prototypes(&store, InstanceIndexKeys::new());
            assert_eq!(inventory(&stage), found, "keys found again on one thread");
            found
        };
        assert_eq!(expected.len(), 2);
        assert_eq!(expected[0].instances, instances);
        assert_eq!(expected[1].instances.len(), instances.len());
        assert_eq!(expected[1].instances[0], first_inner);
        for threads in [2, 4, 16] {
            let options = StageOptions {
                threads,
                ..StageOptions::default()
            };
            let stage = Stage::compose(&mut store, LayerId(1), options);
            assert_eq!(inventory(&stage), expected, "{threads} threads");
        }
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

    #[test]
    fn stronger_instance_root_specs_regroup_only_when_the_root_key_changes() {
        let (mut store, instances, _, field) = scene();
        let root = LayerId(1);
        let asset = LayerId(2);
        let session = LayerId(3);
        let first = instances[0];
        let property = store.property_path("/I0.label");
        let alternate = store.path("/Alternate");
        let alternate_child = store.path("/Alternate/Child");
        let child_name = store.tokens.intern("Child");
        let source = store.layers.get_mut(&asset).unwrap();
        source.insert_prim(alternate, PrimSpec::def().with_children(vec![child_name]));
        source.insert_prim(
            alternate_child,
            PrimSpec::def().with_field(field, Value::Int(43)),
        );
        store.insert_layer(Layer::new(session));
        let options = StageOptions {
            session_layer: Some(session),
            ..StageOptions::default()
        };
        let mut live = LiveStage::compose(&mut store, root, options.clone());
        let original = inventory(live.stage());
        let footprint: Vec<_> = live.stage().traverse(first).collect();
        let target = EditTarget::for_layer(session);

        // The new stronger root spec must be indexed, but the arc still
        // brings in the same prototype and retains its original ordering.
        let mut edit = Transaction::new();
        edit.create_property(
            target.property(property),
            PropertySpec::typed_attribute(PropertyType::parse("string").unwrap())
                .with_default(Value::string("local")),
        );
        live.apply(&mut store, &edit).unwrap();
        let work = live.recomposition_work();
        assert!(
            !work.full_rebuild && work.replaced_prim_indexes != 0,
            "{work:?}"
        );
        assert_eq!(live.stage().prototype_preparations, 1);
        assert_eq!(inventory(live.stage()), original);

        // A stronger reference changes the prototype without adding or
        // removing any paths from the instance's visible namespace.
        let mut edit = Transaction::new();
        edit.set_references(
            target.prim(first),
            ListOp::explicit(vec![Reference::with_asset(asset, alternate, "asset.usda")]),
        );
        live.apply(&mut store, &edit).unwrap();
        let stage = live.stage();
        assert!(!live.recomposition_work().full_rebuild);
        assert_eq!(stage.traverse(first).collect::<Vec<_>>(), footprint);
        assert_eq!(stage.prototype_preparations, 2);
        assert_eq!(stage.prototypes().count(), 2);
        assert_ne!(
            stage.instance_prototype(first),
            stage.instance_prototype(instances[1])
        );
        assert_eq!(
            stage.resolve_field(footprint[1], field).unwrap().value,
            Value::Int(43)
        );
        let fresh = Stage::compose(&mut store, root, options);
        assert_eq!(inventory(stage), inventory(&fresh));
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
                PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
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
                    PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
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
            .rare_mut()
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
    fn namespace_dependent_targets_and_expressions_reroot_in_reused_descendants() {
        let (mut store, source, field) = scene();
        let target = TargetPath::prim(source);
        store.layers.get_mut(&LayerId(2)).unwrap().insert_prim(
            source,
            PrimSpec::def().with_property(
                field,
                PropertySpec::relationship().with_targets(ListOp::explicit(vec![target])),
            ),
        );
        let stage = assert_matches_full(&mut store, 4);
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
                PropertySpec::typed_attribute(PropertyType::parse("pathExpression").unwrap())
                    .with_default(Value::PathExpression(Arc::from("../C"))),
            ),
        );
        let stage = assert_matches_full(&mut store, 4);
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
                PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
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
    fn clips_exclude_early_reuse_and_nested_arcs_do_not() {
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
        // A nested arc is one more instanceable site, alike for both.
        assert_matches_full(&mut store, 4);
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
                PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
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
        spec.rare_mut().variant_selections.insert(mode, selected);
        spec.rare_mut()
            .variant_set_names
            .make_mut()
            .prepend
            .push(mode);
        spec.rare_mut().variant_sets.insert(
            mode,
            VariantSetSpec {
                variants: crate::Variants::from([(
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
        // Both instances select the same branch through the same sites:
        // the second reuses the first's descendants.
        let stage = assert_matches_full(&mut store, 4);
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
        // An inactive instanceable prim is no instance, as OpenUSD's
        // `UsdPrim::IsInstance` reports.
        assert!(!stage.is_instance(left));
        assert!(!stage.has_prim(left_child));
        assert!(stage.is_active(right));
        assert!(stage.is_instance(right));
        assert_eq!(
            stage
                .resolve_field_path(PropertyPath::new(right_child, field))
                .unwrap()
                .value,
            Value::Int(7)
        );
    }
}
