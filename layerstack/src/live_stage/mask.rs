// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dependency-driven population owns the mask, never asset I/O.
//! AOUSD Core §11.3; OpenUSD `UsdStage::ExpandPopulationMask`.
use super::*;
use crate::{PrimPredicate, PropertyKind, PropertyPath};

/// Evidence from synchronizing and recursively expanding a population mask.
#[derive(Debug, Default)]
pub struct MaskExpansionReport {
    /// Pending source and control changes synchronized before searching.
    pub synchronized: Changes,
    /// Dependency roots admitted during expansion, in discovery order.
    /// The final mask removes redundant descendants of admitted ancestors.
    pub added_roots: Vec<PathId>,
    /// Changes from each successive mask expansion, also published as notices.
    /// An empty vector means the synchronized mask needed no expansion.
    pub changes: Vec<Changes>,
}
impl LiveStage {
    /// Recursively admits relationship targets and attribute connections from
    /// default-predicate prims. Cycles stop once all target prims are included.
    /// Does not load assets, override payload rules or unmute layers.
    pub fn expand_population_mask(&mut self, store: &mut dyn LayerStore) -> MaskExpansionReport {
        self.expand_population_mask_with(
            store,
            PrimPredicate::DEFAULT,
            |_, _, _| true,
            |_, _, _| true,
        )
    }
    /// Expands through selected relationships and connections until the mask
    /// reaches a fixed point. Filters receive the current snapshot and store,
    /// so properties discovered by later rounds are inspectable too.
    /// Ancestors already included by the mask follow OpenUSD's `Includes`
    /// semantics; targeting such an ancestor does not select its whole subtree.
    pub fn expand_population_mask_with(
        &mut self,
        store: &mut dyn LayerStore,
        predicate: PrimPredicate,
        mut relationships: impl FnMut(&Stage, &dyn LayerStore, PropertyPath) -> bool,
        mut attributes: impl FnMut(&Stage, &dyn LayerStore, PropertyPath) -> bool,
    ) -> MaskExpansionReport {
        self.notify_changed_layers(store);
        let mut report = MaskExpansionReport {
            synchronized: self.recompose_changes(store),
            ..Default::default()
        };
        while let Some(mut mask) = self.options.mask.clone() {
            let Some(root) = store.paths().lookup(&crate::Path::root()) else {
                break;
            };
            if mask.includes_subtree(root, store.paths()) {
                break;
            }
            let mut targets = BTreeSet::new();
            for prim in self.stage.prim_range(root, store, predicate) {
                for name in self.stage.property_names(prim, store) {
                    let Some(declaration) = self.stage.resolve_property_declaration(prim, name)
                    else {
                        continue;
                    };
                    let property = PropertyPath::new(prim, name);
                    let include = match declaration.kind {
                        PropertyKind::Relationship => relationships(&self.stage, store, property),
                        PropertyKind::Attribute => attributes(&self.stage, store, property),
                    };
                    if !include {
                        continue;
                    }
                    let resolved_targets = match declaration.kind {
                        PropertyKind::Relationship => {
                            self.stage.forwarded_relationship_targets(property)
                        }
                        PropertyKind::Attribute => self
                            .stage
                            .resolve_target_list_path(property)
                            .map(|r| r.value)
                            .unwrap_or_default(),
                    };
                    targets.extend(
                        resolved_targets
                            .into_iter()
                            .map(|target| target.prim_path())
                            .filter(|p| !mask.includes(*p, store.paths())),
                    );
                }
            }
            if targets.is_empty() {
                break;
            }
            report.added_roots.extend(targets.iter().copied());
            mask.include.extend(targets);
            mask.include = minimal_roots(store, &mask.include);
            self.set_population_mask(Some(mask));
            report.changes.push(self.recompose_changes(store));
        }
        report
    }
}
