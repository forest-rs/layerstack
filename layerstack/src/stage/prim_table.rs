// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::boxed::Box;
use hashbrown::HashMap;

use crate::{PathId, prim_index::PrimIndex, sync::Memo};

/// A projection whose opinions and non-local sites already belong to the
/// representative. Only the local graph site names another occurrence.
/// AOUSD Core §11.3.3 (instances share prototypes).
#[derive(Debug)]
pub(crate) struct InstanceLeaf {
    pub(crate) representative: PathId,
    index: Memo<Box<PrimIndex>>,
}

impl InstanceLeaf {
    pub(crate) fn new(representative: PathId) -> Self {
        Self {
            representative,
            index: Memo::default(),
        }
    }

    fn index<'a>(&'a self, prims: &'a HashMap<PathId, PrimIndex>, path: PathId) -> &'a PrimIndex {
        self.index
            .get_or_init(|| Box::new(prims[&self.representative].at_instance_leaf(path)))
    }
}

/// The populated index table of a stage. Stage-local edits compose with
/// dependencies and use only the ordinary indexes. A stand-alone stage can
/// borrow an index from the representative until a query needs its local site.
#[derive(Debug, Default)]
pub(super) struct PrimTable {
    pub(super) physical: HashMap<PathId, PrimIndex>,
    virtual_leaves: HashMap<PathId, InstanceLeaf>,
}

impl From<HashMap<PathId, PrimIndex>> for PrimTable {
    fn from(physical: HashMap<PathId, PrimIndex>) -> Self {
        Self {
            physical,
            virtual_leaves: HashMap::new(),
        }
    }
}

impl PrimTable {
    pub(super) fn install_virtual_leaves(&mut self, leaves: HashMap<PathId, InstanceLeaf>) {
        self.virtual_leaves = leaves;
    }

    pub(super) fn get(&self, path: &PathId) -> Option<&PrimIndex> {
        self.physical.get(path).or_else(|| {
            self.virtual_leaves
                .get(path)
                .map(|leaf| leaf.index(&self.physical, *path))
        })
    }

    /// Only for queries that use opinions or type information, whose records
    /// are identical for these leaves. Graph reads use the occurrence index.
    pub(super) fn shared_index(&self, path: &PathId) -> Option<&PrimIndex> {
        self.physical.get(path).or_else(|| {
            let leaf = self.virtual_leaves.get(path)?;
            self.physical.get(&leaf.representative)
        })
    }

    pub(super) fn contains_key(&self, path: &PathId) -> bool {
        self.physical.contains_key(path) || self.virtual_leaves.contains_key(path)
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &PathId> {
        self.physical.keys().chain(self.virtual_leaves.keys())
    }

    pub(super) fn len(&self) -> usize {
        self.physical.len() + self.virtual_leaves.len()
    }

    pub(super) fn resolved_count(&self) -> usize {
        self.physical.len()
            + self
                .virtual_leaves
                .values()
                .filter(|leaf| leaf.index.get().is_some())
                .count()
    }

    pub(super) fn get_mut(&mut self, path: &PathId) -> Option<&mut PrimIndex> {
        assert!(
            self.virtual_leaves.is_empty(),
            "mutable stages retain occurrence indexes"
        );
        self.physical.get_mut(path)
    }

    pub(super) fn insert(&mut self, path: PathId, index: PrimIndex) -> Option<PrimIndex> {
        assert!(
            self.virtual_leaves.is_empty(),
            "mutable stages retain occurrence indexes"
        );
        self.physical.insert(path, index)
    }

    pub(super) fn remove(&mut self, path: &PathId) -> Option<PrimIndex> {
        assert!(
            self.virtual_leaves.is_empty(),
            "mutable stages retain occurrence indexes"
        );
        self.physical.remove(path)
    }
}

#[cfg(test)]
impl core::ops::Index<&PathId> for PrimTable {
    type Output = PrimIndex;

    fn index(&self, path: &PathId) -> &Self::Output {
        self.get(path).expect("populated prim")
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        EditTarget, InMemoryStore, Layer, LayerId, LiveStage, PrimIndexGraph, PrimSpec,
        PropertyDefinition, PropertyPath, PropertySpec, PropertyType, Reference, SchemaDefinition,
        SchemaRegistry, Stage, StageOptions, TargetPath, Time, Transaction, Value,
    };
    use alloc::{sync::Arc, vec, vec::Vec};

    #[test]
    fn default_stage_projects_flat_instances_and_keeps_earlier_source_values() {
        let mut store = InMemoryStore::default();
        let root = LayerId(1);
        let asset = LayerId(2);
        let source_root = store.path("/Asset");
        let source = store.path("/Asset/Leaf");
        let field = store.tokens().intern("number");
        let id = PropertyPath::new(source, field);
        let type_name = store.tokens().intern("VirtualLeaf");
        let schema_field = store.tokens().intern("fallbackNumber");
        let _ = store.tokens().intern("clips");
        let mut schemas = SchemaRegistry::builder();
        schemas.register(
            SchemaDefinition::typed(type_name)
                .with_property(PropertyDefinition::attribute(schema_field).with_fallback(23)),
        );
        let schemas = Arc::new(schemas.build(store.tokens()));
        let name = store.tokens().intern("Leaf");
        let mut layer = Layer::new(asset);
        layer.insert_prim(source_root, PrimSpec::def().with_children(vec![name]));
        layer.insert_prim(
            source,
            PrimSpec::def().with_type_name(type_name).with_property(
                field,
                PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
                    .with_default(Value::Int(7))
                    .with_time_samples(vec![(1.0, Value::Int(9))]),
            ),
        );
        store.insert_layer(layer);
        let mut layer = Layer::new(root);
        let (first, second) = (store.path("/First"), store.path("/Second"));
        for path in [first, second] {
            layer.insert_prim(
                path,
                PrimSpec::def()
                    .with_instanceable(true)
                    .with_reference(Reference::with_asset(asset, source_root, "asset.usda")),
            );
        }
        store.insert_layer(layer);

        let stage = Stage::compose(&mut store, root, StageOptions::default());
        assert!(
            stage.composition_errors().is_empty(),
            "{:?}",
            stage.composition_errors()
        );
        assert_eq!(stage.composition_storage().materialized_prim_indexes, 4);
        let stage = Stage::compose(
            &mut store,
            root,
            StageOptions {
                schemas: Some(schemas),
                with_provenance: true,
                ..StageOptions::default()
            },
        );
        let leaf = store.path("/Second/Leaf");
        let first_leaf = store.path("/First/Leaf");
        assert_eq!(stage.traverse(second).collect::<Vec<_>>(), [second, leaf]);
        assert_eq!(stage.prim(second, &store).unwrap().child_paths(), [leaf]);
        assert!(stage.view(&store).prim("/Second/Missing").is_none());

        let stats = stage.composition_storage();
        assert_eq!(stats.prim_indexes, 5);
        assert_eq!(stats.materialized_prim_indexes, 4);
        let attribute = |stage: &Stage, store: &InMemoryStore| {
            stage
                .prim(leaf, store)
                .unwrap()
                .attribute(field)
                .unwrap()
                .value::<i32>(Time::Default)
        };
        assert_eq!(attribute(&stage, &store), Some(7));
        let prim = stage.prim(leaf, &store).unwrap();
        assert_eq!(
            prim.attribute(schema_field)
                .unwrap()
                .value::<i32>(Time::Default),
            Some(23)
        );
        let sample = prim.attribute(field).unwrap();
        assert_eq!(sample.sample_times(), [1.0]);
        let sampled = sample.get(Time::held(1.0)).unwrap();
        assert_eq!(sampled.value, Value::Int(9));
        assert_eq!(sampled.provenance.unwrap().layer, asset);
        let stamp = stage.prim_snapshot(leaf);
        assert!(stamp.is_current(&stage));
        assert_eq!(stage.composition_storage().materialized_prim_indexes, 4);

        let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
        assert_eq!(
            live.stage().composition_storage().materialized_prim_indexes,
            5
        );
        let full = stage.prim(leaf, &store).unwrap().composition_graph();
        let eager = live.stage().prim(leaf, &store).unwrap().composition_graph();
        let sites = |graph: &PrimIndexGraph| {
            graph
                .nodes()
                .map(|(_, node)| node.site().clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(sites(full), sites(eager));
        assert_eq!(
            full.node(full.root().unwrap()).unwrap().site().prim_path(),
            leaf
        );
        assert_eq!(stage.composition_storage().materialized_prim_indexes, 5);

        let mut transaction = Transaction::default();
        transaction.set_default(EditTarget::for_layer(asset).property(id), Value::Int(11));
        let applied = live.apply(&mut store, &transaction).unwrap();
        assert_eq!(applied.recomposed, [first_leaf, leaf]);
        assert_eq!(attribute(live.stage(), &store), Some(11));
        assert_eq!(attribute(&stage, &store), Some(7));
        live.apply(&mut store, &applied.inverse).unwrap();
        assert_eq!(attribute(live.stage(), &store), Some(7));
        assert!(stamp.is_current(&stage));

        let relationship = store.tokens().intern("selfTarget");
        store.layer_mut(asset).unwrap().set_property(
            PropertyPath::new(source, relationship),
            PropertySpec::relationship()
                .with_targets(crate::ListOp::explicit(vec![TargetPath::Prim(source)])),
        );
        let with_target = Stage::compose(&mut store, root, StageOptions::default());
        assert_eq!(
            with_target.composition_storage().materialized_prim_indexes,
            5
        );
        let target = with_target
            .prim(leaf, &store)
            .unwrap()
            .relationship(relationship)
            .unwrap();
        assert_eq!(target.targets(), [TargetPath::Prim(leaf)]);
    }
}
