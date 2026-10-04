// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Snapshot-owned prototype inventory and exact composed-record sharing.
//! Occurrence graphs retain their namespace and provenance. Only equal opinion
//! records share storage; path-dependent values cannot alias by accident.

use super::*;
use crate::prim_index::PrimIndexData;

/// What makes two instances share a prototype: the arcs that bring in the
/// instance's descendants, each with its kind, site and offset, and the
/// variant selections, clip definitions and populated descendants. OpenUSD:
/// `PcpInstanceKey`, `Usd_InstanceKey` (`usd/instanceKey.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct InstanceKey {
    arcs: Vec<(ArcKind, LayerId, SpecPath, u64, u64)>,
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
                    node.layer_stack(),
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
    pub(super) fn reshare_prototype_records(&mut self) {
        // AOUSD Core §11.4: descendants exclude opinions authored directly
        // beneath an instance. Equality additionally guards mapped targets,
        // anchored expressions, source sites, offsets and graph-node identities.
        for members in &self.prototypes.members {
            let mut candidates: Vec<Arc<PrimIndexData>> = Vec::new();
            for member in members {
                let Some(index) = self.prims.get_mut(member) else {
                    continue;
                };
                if let Some(shared) = candidates
                    .iter()
                    .find(|data| data.same_records(&index.data))
                {
                    index.data = Arc::clone(shared);
                } else {
                    candidates.push(Arc::clone(&index.data));
                }
            }
        }
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
