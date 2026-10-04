// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! AOUSD Core §12.4; OpenUSD `UsdRelationship::GetForwardedTargets`.
use super::*;
use crate::{PropertyKind, TargetPath};
impl Stage {
    /// Composed authored or schema-defined property kind.
    pub fn property_kind(&self, path: PropertyPath) -> Option<PropertyKind> {
        self.property_definition_ref(path.prim_path(), path.property())
            .map(|d| d.kind)
            .or_else(|| {
                self.resolve_property_declaration(path.prim_path(), path.property())
                    .map(|d| d.kind)
            })
    }
    /// Ordered, deduplicated relationship targets with recursive forwarding.
    /// Cycles contribute no terminal target along the cyclic edge. Missing prim
    /// and attribute targets remain terminal paths, matching OpenUSD.
    pub fn forwarded_relationship_targets(&self, root: PropertyPath) -> Vec<TargetPath> {
        if self.property_kind(root) != Some(PropertyKind::Relationship) {
            return Vec::new();
        }
        let mut pending = alloc::vec![TargetPath::Property(root)];
        let mut seen = HashSet::new();
        let mut unique = HashSet::new();
        let mut targets = Vec::new();
        while let Some(target) = pending.pop() {
            if let TargetPath::Property(path) = target
                && self.property_kind(path) == Some(PropertyKind::Relationship)
            {
                if seen.insert(path)
                    && let Some(resolved) = self.resolve_target_list_path(path)
                {
                    pending.extend(resolved.value.into_iter().rev());
                }
            } else if unique.insert(target) {
                targets.push(target);
            }
        }
        targets
    }
}
