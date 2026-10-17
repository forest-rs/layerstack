// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Namespace mappings across composition arcs.
//!
//! A [`MapFunction`] maps prim paths in the namespace of an arc's target (its
//! source side) to the namespace of the site that authors the arc (its target
//! side), with the time offset the arc applies. A reference from `/Shot/Lamp`
//! to `/Lamp` maps `/Lamp` to `/Shot/Lamp`; a class arc additionally maps
//! every other path to itself (a root identity).
//!
//! Mappings must stay one-to-one: a path that maps through one pair but
//! whose result maps back through a closer pair does not map at all.
//!
//! Spec: AOUSD Core §10.2 (arcs map namespace), §10.3.1.1 (layer offsets
//! concatenate along an arc chain). OpenUSD: `PcpMapFunction`
//! (`pxr/usd/pcp/mapFunction.cpp`).

use alloc::{sync::Arc, vec::Vec};

use crate::{
    doc::LayerOffset,
    path::{PathId, PathInterner},
};

/// One mapping pair: a source path and the target it maps to, or `None`
/// for a block that stops the source (and what is beneath it) from mapping
/// through a pair of one of its ancestors.
pub(crate) type Pair = (PathId, Option<PathId>);

/// A namespace mapping with a time offset. See the [module docs](self).
///
/// A mapping composed with a relocations mapping is kept as the list of
/// the mappings it composes, applied one after another: each step checks
/// that its result maps back, which a mapping composed into one list of
/// pairs no longer does. OpenUSD: `PcpMapFunction::DeferredComposition`,
/// which `PcpLayerStack::GetExpressionForRelocatesAtPath` returns.
#[derive(Clone, Debug)]
pub(crate) struct MapFunction {
    /// Canonical pairs, ordered by source depth, then source and target ids;
    /// the root identity pair is held in `root_identity` instead. Unused
    /// for a deferred composition.
    pairs: Arc<[Pair]>,
    root_identity: bool,
    offset: LayerOffset,
    /// For a deferred composition, the mappings composed, outermost first.
    parts: Option<Arc<[Self]>>,
}

impl PartialEq for MapFunction {
    fn eq(&self, other: &Self) -> bool {
        self.root_identity == other.root_identity
            && self.offset == other.offset
            && self.pairs == other.pairs
            && self.parts == other.parts
    }
}

fn depth(paths: &PathInterner, path: PathId) -> usize {
    paths.resolve(path).depth()
}

fn has_prefix(paths: &PathInterner, path: PathId, prefix: PathId) -> bool {
    path == prefix
        || paths
            .resolve(path)
            .segments()
            .starts_with(paths.resolve(prefix).segments())
}

/// `path` with its prefix `from` replaced by `to`.
fn replace_prefix(paths: &PathInterner, path: PathId, from: PathId, to: PathId) -> PathId {
    if path == from {
        return to;
    }
    paths
        .intern_reparented(path, from, to)
        .expect("path is beneath the replaced prefix")
}

/// The pair whose source is the longest prefix of `path`, or `None`.
fn best_source(
    paths: &PathInterner,
    pairs: &[Pair],
    path: PathId,
    min_depth: usize,
) -> Option<usize> {
    let path_depth = depth(paths, path);
    // Pairs are ordered by source depth: look from the deepest source that
    // can still be a prefix of `path`.
    let end = pairs.partition_point(|(source, _)| depth(paths, *source) <= path_depth);
    for at in (0..end).rev() {
        let source = pairs[at].0;
        if depth(paths, source) < min_depth {
            return None;
        }
        if has_prefix(paths, path, source) {
            return Some(at);
        }
    }
    None
}

/// The parent of `path`, interned; `path` itself for the root.
fn parent_of(paths: &PathInterner, path: PathId) -> PathId {
    paths.parent(path).unwrap_or(path)
}

/// The pair whose target is the longest prefix of `path`, or `None`.
fn best_target(
    paths: &PathInterner,
    pairs: &[Pair],
    path: PathId,
    min_depth: usize,
) -> Option<usize> {
    let mut best = None;
    let mut best_depth = min_depth;
    for (at, (_, target)) in pairs.iter().enumerate() {
        let Some(target) = *target else {
            continue;
        };
        let target_depth = depth(paths, target);
        if target_depth >= best_depth && has_prefix(paths, path, target) {
            best_depth = target_depth;
            best = Some(at);
        }
    }
    best
}

/// Whether a pair other than `best` maps `target` back better than `best`
/// does: then the path `best` maps to `target` is not one-to-one.
fn has_better_target(
    paths: &PathInterner,
    pairs: &[Pair],
    target: PathId,
    best: Option<usize>,
    invert: bool,
) -> bool {
    let min_depth = best.map_or(0, |at| {
        let (source, target) = pairs[at];
        let side = if invert { Some(source) } else { target };
        side.map_or(0, |path| depth(paths, path))
    });
    let better = if invert {
        best_source(paths, pairs, target, min_depth)
    } else {
        best_target(paths, pairs, target, min_depth)
    };
    better.is_some() && better != best
}

/// Maps `path` through `pairs` (source to target, or back when `invert`).
fn map_path(
    paths: &PathInterner,
    pairs: &[Pair],
    root_identity: bool,
    path: PathId,
    invert: bool,
) -> Option<PathId> {
    let best = if invert {
        best_target(paths, pairs, path, 0)
    } else {
        best_source(paths, pairs, path, 0)
    };
    let result = match best {
        None => root_identity.then_some(path)?,
        Some(at) => {
            let (source, target) = pairs[at];
            if invert {
                replace_prefix(paths, path, target?, source)
            } else {
                replace_prefix(paths, path, source, target?)
            }
        }
    };
    (!has_better_target(paths, pairs, result, best, invert)).then_some(result)
}

/// Orders pairs by source depth, then by ids, as canonical pairs are.
fn sort_pairs(paths: &PathInterner, pairs: &mut [Pair]) {
    pairs.sort_by(|a, b| {
        depth(paths, a.0)
            .cmp(&depth(paths, b.0))
            .then(a.0.cmp(&b.0))
            .then(a.1.cmp(&b.1))
    });
}

/// Whether removing `pairs[at]` changes no mapping.
fn is_redundant(paths: &PathInterner, pairs: &[Pair], at: usize, root: PathId) -> bool {
    let (source, target) = pairs[at];
    if source == root {
        return false;
    }
    let parent = parent_of(paths, source);
    let best = best_source(paths, pairs, parent, 0);
    let Some(target) = target else {
        // A block is redundant when its source would not map anyway.
        let Some(best) = best else {
            return true;
        };
        let (best_source_path, Some(best_target_path)) = pairs[best] else {
            return true;
        };
        let mapped = replace_prefix(paths, source, best_source_path, best_target_path);
        return has_better_target(paths, pairs, mapped, Some(best), false);
    };
    let (source_path, target_path) = (paths.resolve(source), paths.resolve(target));
    if source_path.leaf() != target_path.leaf() {
        return false;
    }
    let Some(best) = best else {
        return false;
    };
    let (best_source_path, Some(best_target_path)) = pairs[best] else {
        return false;
    };
    if best_source_path == root && best_target_path == root && source == target {
        return true;
    }
    let levels =
        |path: PathId, base: PathId| depth(paths, path) as isize - depth(paths, base) as isize;
    if levels(target, best_target_path) != levels(source, best_source_path) {
        return false;
    }
    if paths.intern_reparented(source, best_source_path, best_target_path) != Some(target) {
        return false;
    }
    let target_parent = parent_of(paths, target);
    !has_better_target(paths, pairs, target_parent, Some(best), false)
}

/// Removes redundant pairs from sorted `pairs`, returning whether the root
/// identity pair was among them (it is removed too).
fn canonicalize(paths: &PathInterner, pairs: &mut Vec<Pair>, root: PathId) -> bool {
    let mut at = 0;
    while at < pairs.len() {
        if is_redundant(paths, pairs, at, root) {
            pairs.remove(at);
        } else {
            at += 1;
        }
    }
    if pairs.first() == Some(&(root, Some(root))) {
        pairs.remove(0);
        return true;
    }
    false
}

impl MapFunction {
    /// Maps every path to itself, with no time offset.
    pub(crate) fn identity() -> Self {
        Self {
            pairs: Arc::default(),
            root_identity: true,
            offset: LayerOffset::IDENTITY,
            parts: None,
        }
    }

    /// `self`, composed with other mappings by deferring: each applied in
    /// turn (see the type docs).
    pub(crate) fn deferred(self) -> Self {
        if self.is_identity() || self.parts.is_some() {
            return self;
        }
        Self {
            pairs: Arc::default(),
            root_identity: self.root_identity,
            offset: self.offset,
            parts: Some(Arc::from([self])),
        }
    }

    /// The mappings this one applies, outermost first.
    fn parts(&self) -> &[Self] {
        match &self.parts {
            Some(parts) => parts,
            None => core::slice::from_ref(self),
        }
    }

    /// This mapping with its parts composed into one list of pairs.
    pub(crate) fn normalized(&self, paths: &PathInterner) -> Self {
        let Some(parts) = &self.parts else {
            return self.clone();
        };
        let mut out = parts
            .last()
            .expect("a deferred mapping has parts")
            .normalized(paths);
        for part in parts.iter().rev().skip(1) {
            out = part.normalized(paths).compose(paths, &out);
        }
        out.offset = self.offset;
        out
    }

    /// The mapping of `pairs` (a pair whose source and target are both the
    /// root is the root identity), with `offset`.
    pub(crate) fn new(
        paths: &PathInterner,
        pairs: impl IntoIterator<Item = Pair>,
        offset: LayerOffset,
    ) -> Self {
        let root = paths.intern_segments(&[]);
        let mut pairs: Vec<Pair> = pairs.into_iter().collect();
        sort_pairs(paths, &mut pairs);
        pairs.dedup();
        let root_identity = canonicalize(paths, &mut pairs, root);
        Self {
            pairs: pairs.into(),
            root_identity,
            offset,
            parts: None,
        }
    }

    /// Whether every path maps to itself with no offset.
    pub(crate) fn is_identity(&self) -> bool {
        self.offset.is_identity()
            && match &self.parts {
                Some(parts) => parts
                    .iter()
                    .all(|part| part.root_identity && part.pairs.is_empty()),
                None => self.root_identity && self.pairs.is_empty(),
            }
    }

    /// Whether paths outside every pair's source map to themselves.
    pub(crate) fn has_root_identity(&self) -> bool {
        match &self.parts {
            Some(parts) => parts.iter().all(|part| part.root_identity),
            None => self.root_identity,
        }
    }

    /// The time offset this mapping applies.
    pub(crate) fn offset(&self) -> LayerOffset {
        self.offset
    }

    /// This mapping with every other path mapping to itself.
    pub(crate) fn with_root_identity(&self, paths: &PathInterner) -> Self {
        Self {
            root_identity: true,
            ..self.normalized(paths)
        }
    }

    /// This mapping with the root identity removed.
    pub(crate) fn without_root_identity(&self, paths: &PathInterner) -> Self {
        Self {
            root_identity: false,
            ..self.normalized(paths)
        }
    }

    /// Maps `path` from the source namespace to the target namespace.
    pub(crate) fn map_source_to_target(
        &self,
        paths: &PathInterner,
        path: PathId,
    ) -> Option<PathId> {
        match &self.parts {
            // The innermost mapping applies first.
            Some(parts) => parts
                .iter()
                .rev()
                .try_fold(path, |path, part| part.map_source_to_target(paths, path)),
            None => map_path(paths, &self.pairs, self.root_identity, path, false),
        }
    }

    /// Maps `path` from the target namespace back to the source namespace.
    pub(crate) fn map_target_to_source(
        &self,
        paths: &PathInterner,
        path: PathId,
    ) -> Option<PathId> {
        match &self.parts {
            Some(parts) => parts
                .iter()
                .try_fold(path, |path, part| part.map_target_to_source(paths, path)),
            None => map_path(paths, &self.pairs, self.root_identity, path, true),
        }
    }

    /// The mapping that applies `inner`, then `self`.
    pub(crate) fn compose(&self, paths: &PathInterner, inner: &Self) -> Self {
        if self.is_identity() {
            return inner.clone();
        }
        if inner.is_identity() {
            return self.clone();
        }
        if self.parts.is_some() || inner.parts.is_some() {
            let parts: Vec<Self> = self.parts().iter().chain(inner.parts()).cloned().collect();
            return Self {
                pairs: Arc::default(),
                root_identity: self.has_root_identity() && inner.has_root_identity(),
                offset: self.offset.compose(inner.offset),
                parts: Some(parts.into()),
            };
        }
        let root = paths.intern_segments(&[]);
        let root_identity = self.root_identity && inner.root_identity;
        let mut pairs: Vec<Pair> = Vec::with_capacity(self.pairs.len() + inner.pairs.len() + 1);
        if root_identity {
            pairs.push((root, Some(root)));
        }
        for &(source, target) in inner.pairs.iter() {
            let target = target
                .and_then(|target| map_path(paths, &self.pairs, self.root_identity, target, false));
            pairs.push((source, target));
        }
        let mapped_sources = pairs.len();
        for &(source, target) in self.pairs.iter() {
            let Some(source) = map_path(paths, &inner.pairs, inner.root_identity, source, true)
            else {
                continue;
            };
            if pairs[..mapped_sources].iter().any(|(s, _)| *s == source) {
                continue;
            }
            pairs.push((source, target));
        }
        sort_pairs(paths, &mut pairs);
        pairs.dedup();
        canonicalize(paths, &mut pairs, root);
        Self {
            pairs: pairs.into(),
            root_identity,
            offset: self.offset.compose(inner.offset),
            parts: None,
        }
    }

    /// The class arc `class_arc` implied across `transfer`: the class arc
    /// of the namespace `transfer` maps to, which always has the root
    /// identity.
    ///
    /// OpenUSD: `PcpMapFunction::ImpliedClass` and
    /// `_ComposeImpliedClassPathPairs` (`pxr/usd/pcp/mapFunction.cpp`).
    pub(crate) fn implied_class(paths: &PathInterner, transfer: &Self, class_arc: &Self) -> Self {
        if transfer.is_identity() || class_arc.is_identity() {
            return class_arc.clone();
        }
        let (transfer, class_arc) = (&transfer.normalized(paths), &class_arc.normalized(paths));
        let root = paths.intern_segments(&[]);
        let transfer_pairs = transfer.pairs.clone();
        let class_pairs = class_arc.pairs.clone();
        let mut pairs = Vec::with_capacity(1 + class_pairs.len() * (1 + transfer_pairs.len()));
        pairs.push((root, Some(root)));
        for &(source, target) in class_pairs.iter() {
            let Some(mapped) = map_path(
                paths,
                &transfer.pairs,
                transfer.root_identity,
                source,
                false,
            ) else {
                continue;
            };
            let target = target.and_then(|target| {
                map_path(
                    paths,
                    &transfer.pairs,
                    transfer.root_identity,
                    target,
                    false,
                )
            });
            pairs.push((mapped, target));
        }
        for &(transfer_source, transfer_target) in transfer_pairs.iter() {
            for &(class_source, class_target) in class_pairs.iter() {
                let Some(class_target) = class_target else {
                    continue;
                };
                if transfer_source == class_target
                    || !has_prefix(paths, transfer_source, class_target)
                {
                    continue;
                }
                let source = replace_prefix(paths, transfer_source, class_target, class_source);
                if map_path(
                    paths,
                    &class_arc.pairs,
                    class_arc.root_identity,
                    source,
                    false,
                ) != Some(transfer_source)
                {
                    continue;
                }
                let Some(source) = map_path(
                    paths,
                    &transfer.pairs,
                    transfer.root_identity,
                    source,
                    false,
                ) else {
                    continue;
                };
                pairs.push((source, transfer_target));
            }
        }
        sort_pairs(paths, &mut pairs);
        pairs.dedup();
        canonicalize(paths, &mut pairs, root);
        Self {
            pairs: pairs.into(),
            root_identity: true,
            offset: class_arc.offset,
            parts: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TokenInterner, path::Path};
    use alloc::string::String;

    struct Fixture {
        tokens: TokenInterner,
        paths: PathInterner,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                tokens: TokenInterner::default(),
                paths: PathInterner::default(),
            }
        }

        fn path(&mut self, text: &str) -> PathId {
            let path = Path::parse_absolute(text, &self.tokens).unwrap();
            self.paths.intern(path)
        }

        fn map(&mut self, pairs: &[(&str, &str)]) -> MapFunction {
            let pairs: Vec<Pair> = pairs
                .iter()
                .map(|(source, target)| (self.path(source), Some(self.path(target))))
                .collect();
            MapFunction::new(&self.paths, pairs, LayerOffset::IDENTITY)
        }

        fn forward(&mut self, map: &MapFunction, path: &str) -> Option<String> {
            let path = self.path(path);
            map.map_source_to_target(&self.paths, path)
                .map(|id| self.paths.display(id, &self.tokens))
        }

        fn backward(&mut self, map: &MapFunction, path: &str) -> Option<String> {
            let path = self.path(path);
            map.map_target_to_source(&self.paths, path)
                .map(|id| self.paths.display(id, &self.tokens))
        }
    }

    #[test]
    fn a_reference_maps_only_its_target_namespace() {
        let mut f = Fixture::new();
        let map = f.map(&[("/Lamp", "/Shot/Lamp")]);
        assert_eq!(f.forward(&map, "/Lamp").as_deref(), Some("/Shot/Lamp"));
        assert_eq!(
            f.forward(&map, "/Lamp/Shade").as_deref(),
            Some("/Shot/Lamp/Shade")
        );
        assert_eq!(f.forward(&map, "/Other"), None);
        assert_eq!(
            f.backward(&map, "/Shot/Lamp/Shade").as_deref(),
            Some("/Lamp/Shade")
        );
        assert_eq!(f.backward(&map, "/Shot"), None);
    }

    #[test]
    fn a_root_identity_maps_everything_else_to_itself() {
        let mut f = Fixture::new();
        let map = f.map(&[("/", "/"), ("/_class_Model", "/Model")]);
        assert!(map.has_root_identity());
        assert_eq!(
            f.forward(&map, "/_class_Model/A").as_deref(),
            Some("/Model/A")
        );
        assert_eq!(f.forward(&map, "/Other").as_deref(), Some("/Other"));
        // `/Model` would map to itself, but maps back to `/_class_Model`:
        // not one-to-one, so it does not map.
        assert_eq!(f.forward(&map, "/Model"), None);
    }

    #[test]
    fn nested_pairs_must_stay_one_to_one() {
        let mut f = Fixture::new();
        let map = f.map(&[("/A", "/A/B")]);
        assert_eq!(f.forward(&map, "/A/B").as_deref(), Some("/A/B/B"));
        let map = f.map(&[("/A", "/B"), ("/C", "/B/C")]);
        assert_eq!(f.forward(&map, "/A/C"), None);
    }

    #[test]
    fn redundant_pairs_are_dropped() {
        let mut f = Fixture::new();
        let a = f.map(&[("/A", "/B"), ("/A/C", "/B/C")]);
        let b = f.map(&[("/A", "/B")]);
        assert_eq!(a, b);
        let identity = f.map(&[("/", "/"), ("/X", "/X")]);
        assert!(identity.is_identity());
    }

    #[test]
    fn composition_applies_the_inner_mapping_first() {
        let mut f = Fixture::new();
        let inner = f.map(&[("/Model", "/Lamp")]);
        let outer = f.map(&[("/Lamp", "/Shot/Lamp")]);
        let composed = outer.compose(&f.paths, &inner);
        assert_eq!(
            f.forward(&composed, "/Model/Shade").as_deref(),
            Some("/Shot/Lamp/Shade")
        );
        assert_eq!(f.forward(&composed, "/Lamp"), None);
        assert_eq!(
            f.backward(&composed, "/Shot/Lamp").as_deref(),
            Some("/Model")
        );
    }

    /// A deferred composition applies each mapping in turn, each checking
    /// that its own result maps back: `/Y` passes the composed pairs
    /// (`/X` maps to `/Z`, so nothing maps to `/Y` but `/Y`), but not the
    /// inner mapping, where `/X` maps to `/Y`.
    #[test]
    fn deferred_composition_checks_each_mapping() {
        let mut f = Fixture::new();
        let inner = f.map(&[("/", "/"), ("/X", "/Y")]);
        let outer = f.map(&[("/", "/"), ("/Y", "/Z")]);
        let composed = outer.compose(&f.paths, &inner);
        let deferred = outer.clone().deferred().compose(&f.paths, &inner);
        assert_eq!(f.forward(&composed, "/Y").as_deref(), Some("/Y"));
        assert_eq!(f.forward(&deferred, "/Y"), None);
        for map in [&composed, &deferred] {
            assert_eq!(f.forward(map, "/X/A").as_deref(), Some("/Z/A"));
            assert_eq!(f.backward(map, "/Z/A").as_deref(), Some("/X/A"));
            assert_eq!(f.forward(map, "/Other").as_deref(), Some("/Other"));
        }
        assert_eq!(deferred.normalized(&f.paths), composed);
    }

    #[test]
    fn an_implied_class_maps_through_the_transfer() {
        let mut f = Fixture::new();
        let transfer = f.map(&[("/", "/"), ("/Model", "/Model_1")]);
        let class_arc = f.map(&[("/", "/"), ("/Model/Class", "/Model/Inst")]);
        let implied = MapFunction::implied_class(&f.paths, &transfer, &class_arc);
        assert_eq!(
            f.forward(&implied, "/Model_1/Class").as_deref(),
            Some("/Model_1/Inst")
        );
        assert!(implied.has_root_identity());
    }

    #[test]
    fn offsets_compose() {
        let mut f = Fixture::new();
        let (a, b, c) = (f.path("/A"), f.path("/B"), f.path("/C"));
        let inner = MapFunction::new(
            &f.paths,
            [(a, Some(b))],
            LayerOffset {
                offset: 10.0,
                scale: 2.0,
            },
        );
        let outer = MapFunction::new(
            &f.paths,
            [(b, Some(c))],
            LayerOffset {
                offset: 5.0,
                scale: 1.0,
            },
        );
        let composed = outer.compose(&f.paths, &inner);
        assert_eq!(
            composed.offset(),
            LayerOffset {
                offset: 15.0,
                scale: 2.0
            }
        );
    }
}
