// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retained range unions for wide, source-edited parents.

use super::{BoundingBox, PurposeBounds, Range3d};
use crate::{gf, usd_geom::ImageablePurpose};
use alloc::{vec, vec::Vec};
use layerstack::{HashSet, PathId};

pub(super) const MIN_CHILDREN: usize = 64;

/// A tournament tree per observed purpose, in the parent's component frame.
/// Leaves include excluded children, allowing visibility/purpose edits to
/// replace an empty contribution without rebuilding the child index.
#[derive(Clone, Debug)]
pub(super) struct Reduction {
    children: Vec<PathId>,
    buckets: Vec<(ImageablePurpose, Vec<Range3d>)>,
    pub(super) dirty: HashSet<PathId>,
}
impl Reduction {
    pub(super) fn new(children: &[PathId]) -> Self {
        let mut children = children.to_vec();
        children.sort_unstable();
        Self {
            children,
            buckets: Vec::new(),
            dirty: HashSet::new(),
        }
    }
    pub(super) fn mark(&mut self, child: PathId, child_count: usize) -> bool {
        if self.children.len() != child_count || self.children.binary_search(&child).is_err() {
            return false;
        }
        self.dirty.insert(child);
        true
    }
    pub(super) fn clear_dirty_leaves(&mut self) {
        for &child in &self.dirty {
            let index = self.index(child);
            for (_, tree) in &mut self.buckets {
                tree[index] = Range3d::default();
            }
        }
    }
    pub(super) fn set(
        &mut self,
        child: PathId,
        purpose: &ImageablePurpose,
        range: Range3d,
    ) -> bool {
        let index = self.index(child);
        if let Some((_, tree)) = self.buckets.iter_mut().find(|(p, _)| p == purpose) {
            tree[index] = range;
        } else {
            // USD defines four purposes. Unknown tokens remain supported by
            // the ordinary reduction, without quadratic storage here.
            if self.buckets.len() == 4 {
                return false;
            }
            let mut tree = vec![Range3d::default(); 2 * self.children.len()];
            tree[index] = range;
            self.buckets.push((purpose.clone(), tree));
        }
        true
    }
    pub(super) fn update_dirty(&mut self) {
        for &child in &self.dirty {
            let mut index = self.index(child) / 2;
            while index != 0 {
                for (_, tree) in &mut self.buckets {
                    tree[index] = union(tree[index * 2], tree[index * 2 + 1]);
                }
                index /= 2;
            }
        }
        self.dirty.clear();
        // Purpose changes must not accumulate arrays for abandoned buckets.
        // Preserve finite reversed ranges: OpenUSD unions their endpoints too.
        self.buckets
            .retain(|(_, tree)| tree[1] != Range3d::default());
    }
    pub(super) fn build(&mut self) {
        for (_, tree) in &mut self.buckets {
            for index in (1..self.children.len()).rev() {
                tree[index] = union(tree[index * 2], tree[index * 2 + 1]);
            }
        }
    }
    pub(super) fn bounds(&self, matrix: gf::Matrix4) -> PurposeBounds {
        self.buckets
            .iter()
            .map(|(purpose, tree)| {
                (
                    purpose.clone(),
                    BoundingBox {
                        range: tree[1],
                        matrix,
                    },
                )
            })
            .collect()
    }
    fn index(&self, child: PathId) -> usize {
        self.children.len()
            + self
                .children
                .binary_search(&child)
                .expect("indexed reduction child")
    }
}
fn union(mut left: Range3d, right: Range3d) -> Range3d {
    left.union_with(right);
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacements_shrink_ranges_and_reclaim_purpose_buckets() {
        use ImageablePurpose::{Default, Guide, Proxy, Render};
        let purposes = [Default, Render, Proxy, Guide];
        let mut store = layerstack::InMemoryStore::default();
        let children: Vec<_> = (0..71)
            .map(|i| store.path(&alloc::format!("/P{i}")))
            .collect();
        let mut reduction = Reduction::new(&children);
        let mut values = vec![(0, Range3d::default()); children.len()];
        let mut seed = 17_u32;
        for step in 0..1000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let index = seed as usize % children.len();
            let purpose = step % 4;
            let size = f64::from(seed % 17) - 3.0;
            let range = if step % 7 == 0 {
                Range3d::default()
            } else {
                Range3d {
                    min: [-size; 3],
                    max: [size; 3],
                }
            };
            values[index] = (purpose, range);
            assert!(reduction.mark(children[index], children.len()));
            reduction.clear_dirty_leaves();
            assert!(reduction.set(children[index], &purposes[purpose], range));
            reduction.update_dirty();
            let actual = reduction.bounds(gf::IDENTITY);
            for (p, purpose) in purposes.iter().enumerate() {
                let mut expected = Range3d::default();
                for &(bucket, range) in &values {
                    if bucket == p {
                        expected.union_with(range);
                    }
                }
                let found = actual
                    .iter()
                    .find(|(bucket, _)| bucket == purpose)
                    .map_or(Range3d::default(), |(_, bbox)| bbox.range);
                assert_eq!(found, expected, "step {step} purpose {purpose:?}");
            }
        }
        // Retired purposes release their arrays, rather than growing forever.
        for &child in &children {
            assert!(reduction.mark(child, children.len()));
        }
        reduction.clear_dirty_leaves();
        reduction.update_dirty();
        assert!(reduction.buckets.is_empty());
        for purpose in &purposes {
            assert!(reduction.set(
                children[0],
                purpose,
                Range3d {
                    min: [-1.0; 3],
                    max: [1.0; 3]
                }
            ));
        }
        assert!(!reduction.set(
            children[0],
            &ImageablePurpose::from_token("custom"),
            Range3d::default()
        ));
        assert_eq!(reduction.buckets.len(), 4);
    }
}
