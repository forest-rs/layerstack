// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit, stage-local payload and layer controls. The host retains ownership
//! of assets, storage and scheduling; these controls never load or evict data.
//! AOUSD Core §10.3.2.7 (payloads), §11 (population); OpenUSD
//! `UsdStageLoadRules`, `UsdStage::MuteAndUnmuteLayers`.

use crate::{
    AssetAvailability, Layer, LayerId, LayerStore, Path, PathId, PathInterner, TokenInterner,
    interner::TokenId,
};
use alloc::{
    borrow::ToOwned,
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use core::ops::Bound;
use hashbrown::HashSet;

/// Whether loading a prim also selects payloads below it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadPolicy {
    /// Load the prim and all descendants.
    WithDescendants,
    /// Load the prim and required ancestors, leaving descendants unloaded.
    WithoutDescendants,
}

/// Whether payloads at a prim and beneath it participate in composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadRule {
    /// Include payloads at the prim and its descendants.
    All,
    /// Include payloads at this prim, with descendants excluded unless another
    /// rule selects them. Loading a descendant also loads the required ancestors.
    Only,
    /// Exclude payloads here and below, unless a more specific rule selects them.
    None,
}

/// Explicit payload inclusion rules in the composed stage namespace.
/// Paths use the stage store's token interner. Default construction loads all.
/// Rules do not imply that a host has loaded the referenced layer into its store.
///
/// OpenUSD: `UsdStageLoadRules`.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PayloadLoadRules {
    rules: BTreeMap<Path, PayloadRule>,
}

impl PayloadLoadRules {
    /// Rules excluding all payloads until explicitly selected.
    #[must_use]
    pub fn load_none() -> Self {
        Self {
            rules: BTreeMap::from([(Path::root(), PayloadRule::None)]),
        }
    }

    /// Authored literal rules, ordered by [`Path`]'s order for maps (its
    /// segments' token ids), not namespace order.
    pub fn rules(&self) -> impl Iterator<Item = (&Path, PayloadRule)> {
        self.rules.iter().map(|(p, r)| (p, *r))
    }

    /// Insert or replace a literal rule without modifying other rules.
    /// Unlike the loading convenience methods, this does not remove descendants.
    pub fn add_rule(&mut self, path: Path, rule: PayloadRule) {
        self.rules.insert(path, rule);
    }

    /// Load the subtree, replacing any rules at or beneath `path`.
    pub fn load_with_descendants(&mut self, path: Path) {
        self.replace_subtree(path, PayloadRule::All);
    }

    /// Load only this prim and required ancestors, replacing descendant rules.
    pub fn load_without_descendants(&mut self, path: Path) {
        self.replace_subtree(path, PayloadRule::Only);
    }

    /// Unload the subtree, replacing any rules at or beneath `path`.
    pub fn unload(&mut self, path: Path) {
        self.replace_subtree(path, PayloadRule::None);
    }

    /// Replaces the rules at and beneath `path` with `rule` at `path`.
    /// Those rules are one range of the map, as a path's descendants
    /// follow it in [`Path`]'s order: the cost is theirs, not the map's.
    fn replace_subtree(&mut self, path: Path, rule: PayloadRule) {
        let doomed: Vec<Path> = self
            .prefixed(path.segments())
            .map(|(p, _)| p.clone())
            .collect();
        for doomed in doomed {
            self.rules.remove(&doomed);
        }
        self.rules.insert(path, rule);
    }

    /// The rules at and beneath `prefix`, in [`Path`]'s order: a path's
    /// descendants follow it, so they are the range of the map that
    /// starts at it.
    fn prefixed<'a>(
        &'a self,
        prefix: &'a [TokenId],
    ) -> impl Iterator<Item = (&'a Path, PayloadRule)> + 'a {
        self.rules
            .range::<[TokenId], _>((Bound::Included(prefix), Bound::Unbounded))
            .take_while(move |(p, _)| p.segments().starts_with(prefix))
            .map(|(p, r)| (p, *r))
    }

    /// The rule at `path` or its nearest ancestor with one, looked up
    /// one ancestor at a time: the cost is the path's depth, not the
    /// number of rules.
    fn longest_prefix(&self, path: &Path) -> Option<(&Path, PayloadRule)> {
        let segments = path.segments();
        (0..=segments.len())
            .rev()
            .find_map(|depth| self.rules.get_key_value(&segments[..depth]))
            .map(|(p, r)| (p, *r))
    }

    /// The effective rule, including ancestor payloads required by a selected
    /// descendant. Literal exclusions between a prim and a deeper selection
    /// block that selection from making this prim loaded, matching OpenUSD.
    ///
    /// The nearest rule at or above `path` is found by its ancestors, and
    /// the rules beneath it are read only when that rule excludes it, and
    /// then only the outermost ones.
    ///
    /// OpenUSD: `UsdStageLoadRules::GetEffectiveRuleForPath`.
    #[must_use]
    pub fn effective_rule(&self, path: &Path) -> PayloadRule {
        if self.rules.is_empty() {
            return PayloadRule::All;
        }
        match self.longest_prefix(path) {
            None | Some((_, PayloadRule::All)) => return PayloadRule::All,
            Some((p, PayloadRule::Only)) if p == path => return PayloadRule::Only,
            _ => {}
        }
        // The outermost rules strictly beneath `path`: a selection there
        // loads `path` as its ancestor; one beneath an exclusion does not.
        let mut beneath = self
            .prefixed(path.segments())
            .filter(|(p, _)| *p != path)
            .peekable();
        while let Some((p, rule)) = beneath.next() {
            if rule != PayloadRule::None {
                return PayloadRule::Only;
            }
            while beneath
                .next_if(|(q, _)| q.segments().starts_with(p.segments()))
                .is_some()
            {}
        }
        PayloadRule::None
    }

    /// Removes the rules that do not change what any path's effective rule
    /// is: a rule the same as its nearest ancestor's, and `All` at the
    /// root, which is what no rule there means. As OpenUSD does, a single
    /// rule left once that root rule is gone is kept, redundant or not.
    ///
    /// OpenUSD: `UsdStageLoadRules::Minimize`.
    pub fn minimize(&mut self) {
        if self.rules.get(&Path::root()) == Some(&PayloadRule::All) {
            self.rules.remove(&Path::root());
        }
        if self.rules.len() <= 1 {
            return;
        }
        // The kept rules above the one visited, outermost first: the map
        // visits a path's ancestors before it.
        let mut kept: Vec<(Path, PayloadRule)> = Vec::new();
        self.rules.retain(|path, rule| {
            while kept.last().is_some_and(|(top, _)| !top.is_prefix_of(path)) {
                kept.pop();
            }
            let inherited = kept.last().map_or(PayloadRule::All, |(_, r)| *r);
            if *rule == inherited {
                return false;
            }
            kept.push((path.clone(), *rule));
            true
        });
    }

    /// Whether a payload authored at `path` is included by these rules.
    #[must_use]
    pub fn is_loaded(&self, path: &Path) -> bool {
        self.effective_rule(path) != PayloadRule::None
    }
}

/// An invalid atomic layer-muting request. No control changes are applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LayerMuteError {
    /// The stage's root layer cannot be muted.
    RootLayer,
}
impl core::fmt::Display for LayerMuteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::RootLayer => "the stage root layer cannot be muted",
        })
    }
}
impl core::error::Error for LayerMuteError {}

/// A stage-local view: muting hides content without changing or evicting the
/// host's layers or asset bindings. A muted layer's sublayers disappear with it.
/// The root stays available, even for an invalid root entry in `StageOptions`.
pub(crate) struct ControlledStore<'a> {
    pub(crate) inner: &'a mut dyn LayerStore,
    pub(crate) root: LayerId,
    pub(crate) muted: &'a BTreeSet<LayerId>,
}
impl LayerStore for ControlledStore<'_> {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        if id != self.root && self.muted.contains(&id) {
            None
        } else {
            self.inner.layer(id)
        }
    }
    fn layer_mut(&mut self, id: LayerId) -> Option<&mut Layer> {
        if id != self.root && self.muted.contains(&id) {
            None
        } else {
            self.inner.layer_mut(id)
        }
    }
    fn is_layer_pending(&self, id: LayerId) -> bool {
        // A muted layer is hidden, whether or not it has loaded.
        (id == self.root || !self.muted.contains(&id)) && self.inner.is_layer_pending(id)
    }
    fn tokens(&self) -> &TokenInterner {
        self.inner.tokens()
    }
    fn paths(&self) -> &PathInterner {
        self.inner.paths()
    }
    fn layer_identifier(&self, id: LayerId) -> Option<&str> {
        self.inner.layer_identifier(id)
    }
    fn layer_read_only(&self, id: LayerId) -> Option<crate::ReadOnlyReason> {
        self.inner.layer_read_only(id)
    }
    fn asset_layer(&self, anchor: LayerId, asset: &str) -> Option<LayerId> {
        self.inner.asset_layer(anchor, asset)
    }
    fn asset_availability(&self, anchor: LayerId, asset: &str) -> AssetAvailability {
        self.inner.asset_availability(anchor, asset)
    }
}

impl super::Stage {
    /// Local layer stack in strongest-to-weakest occurrence order, excluding
    /// muted layers. This does not include reference/payload target stacks.
    #[must_use]
    pub fn layer_stack(&self) -> &[LayerId] {
        &self.local_layers
    }

    /// Layer identities used by the snapshot's composition, optionally including
    /// resident clip sources. The host may retain many other unused layers.
    /// No dependency-recording option is required; this query performs no I/O.
    #[must_use]
    pub fn used_layers(&self, include_clips: bool) -> BTreeSet<LayerId> {
        let mut result = self.used_layers.clone();
        if include_clips {
            result.extend(self.clips.layers());
        }
        result
    }

    /// Whether this snapshot's local stack contains the layer.
    #[must_use]
    pub fn has_local_layer(&self, layer: LayerId) -> bool {
        self.local_layers.contains(&layer)
    }

    /// The root layer's declared default prim, if populated. This differs from
    /// authored metadata presence: a declared path may be absent from the stage.
    #[must_use]
    pub fn default_prim(&self, store: &mut dyn LayerStore) -> Option<PathId> {
        let path = store.layer(self.root_layer()?)?.default_prim?;
        let path = store.tokens().resolve(path).to_owned();
        let absolute = if path.starts_with('/') {
            path
        } else {
            alloc::format!("/{path}")
        };
        let path = Path::parse_absolute(&absolute, store.tokens()).ok()?;
        let id = store.paths().intern(path);
        self.has_prim(id).then_some(id)
    }

    /// Whether the root layer authors a defaultPrim identifier, even if it does
    /// not name a populated prim. OpenUSD `UsdStage::HasDefaultPrim`.
    #[must_use]
    pub fn has_authored_default_prim(&self, store: &dyn LayerStore) -> bool {
        self.root_layer()
            .and_then(|root| store.layer(root))
            .is_some_and(|l| l.default_prim.is_some())
    }

    /// The session/root stack's effective time-code rate, including authored
    /// timeCodesPerSecond, framesPerSecond fallback and the USD default of 24.
    /// Sublayer rates do not set the stage rate.
    #[must_use]
    pub fn time_codes_per_second(&self, store: &dyn LayerStore) -> f64 {
        self.root_layer().map_or(24.0, |root| {
            crate::LayerStackIdentifier {
                root,
                session: self
                    .options
                    .session_layer
                    .filter(|id| !self.options.muted_layers.contains(id)),
            }
            .time_codes_per_second(store)
        })
    }

    /// The host-owned session root selected for this stage, if any. A muted
    /// session remains selected but contributes no opinions until unmuted.
    #[must_use]
    pub fn session_layer(&self) -> Option<LayerId> {
        self.options.session_layer
    }

    /// The effective controls captured by this immutable snapshot.
    #[must_use]
    pub fn options(&self) -> &super::StageOptions {
        &self.options
    }

    /// The snapshot's muted layers. The root layer is never in this set.
    #[must_use]
    pub fn muted_layers(&self) -> &BTreeSet<LayerId> {
        &self.options.muted_layers
    }

    /// Whether the snapshot excludes this layer's content.
    #[must_use]
    pub fn is_layer_muted(&self, layer: LayerId) -> bool {
        self.options.muted_layers.contains(&layer)
    }

    /// The snapshot's population mask; `None` selects the full namespace.
    #[must_use]
    pub fn population_mask(&self) -> Option<&super::PopulationMask> {
        self.options.mask.as_ref()
    }

    /// The snapshot's payload inclusion rules.
    #[must_use]
    pub fn load_rules(&self) -> &PayloadLoadRules {
        &self.options.load_rules
    }

    /// Active prims with payloads admitted by their composed variant/arc contexts,
    /// whether currently loaded or not. Descendants behind an unloaded payload
    /// cannot be discovered until its host loads that payload.
    /// In namespace order ([`PathInterner::cmp_paths`]).
    /// OpenUSD: `UsdStage::FindLoadable`.
    #[must_use]
    pub fn loadable_paths(&self, root: PathId, store: &dyn LayerStore) -> Vec<PathId> {
        let paths = store.paths();
        let root = paths.resolve(root);
        let mut out: Vec<_> = self
            .loadable
            .iter()
            .copied()
            .filter(|p| self.is_active(*p) && paths.resolve(*p).strip_prefix(root).is_some())
            .collect();
        paths.sort_paths(&mut out, store.tokens());
        out
    }

    /// Currently populated payload prims included by the snapshot's rules.
    /// Inclusion does not prove that the host supplied every requested asset;
    /// inspect composition errors for unresolved targets.
    /// Corresponds to the visible part of `UsdStage::GetLoadSet`. OpenUSD can
    /// also retain cached included payload paths outside a changed population
    /// mask; this snapshot query omits those unpopulated paths. In namespace
    /// order ([`PathInterner::cmp_paths`]).
    #[must_use]
    pub fn loaded_payload_paths(&self, store: &dyn LayerStore) -> Vec<PathId> {
        let paths = store.paths();
        let mut out: Vec<_> = self
            .loadable
            .iter()
            .copied()
            .filter(|p| self.has_prim(*p) && self.options.load_rules.is_loaded(paths.resolve(*p)))
            .collect();
        paths.sort_paths(&mut out, store.tokens());
        out
    }
}

impl super::Stage {
    pub(crate) fn capture_controls(&mut self, options: super::StageOptions) {
        self.options = options;
    }

    /// Merges `partial`'s composition of the subtrees at `roots` (whose
    /// ids `within` holds) into this stage, with the child lists of the
    /// parents just outside them.
    #[allow(
        clippy::too_many_arguments,
        reason = "crate-internal merge step; the roots come as a list and as the set to test membership in"
    )]
    pub(crate) fn merge_control_subtrees(
        &mut self,
        store: &dyn LayerStore,
        mut partial: Self,
        affected: &[PathId],
        hierarchy: &[PathId],
        roots: &[PathId],
        within: &HashSet<PathId>,
        options: super::StageOptions,
        child_index: &crate::prim_indexing::ChildIndex,
    ) -> super::BoundaryOrders {
        // Membership walks a path's ancestors: an edit can resync as many
        // sibling roots as prims reference what it changed.
        let within = |path: PathId| {
            let mut current = Some(path);
            while let Some(at) = current {
                if within.contains(&at) {
                    return true;
                }
                current = store.paths().parent(at);
            }
            false
        };
        let mut boundaries = BTreeMap::new();
        for &root in roots {
            if let Some(parent) = store.paths().parent(root) {
                // Sibling roots share their parent: list its children once.
                if boundaries.contains_key(&parent) || within(parent) {
                    continue;
                }
                if partial.has_boundary_children(parent) {
                    boundaries.insert(parent, Vec::new());
                    continue;
                }
                let mut children = self
                    .all_children_of(parent)
                    .unwrap_or(&[])
                    .iter()
                    .copied()
                    .filter(|p| !within(*p))
                    .collect::<Vec<_>>();
                children.extend(
                    partial
                        .all_children_of(parent)
                        .unwrap_or(&[])
                        .iter()
                        .copied()
                        .filter(|p| within(*p)),
                );
                store.paths().sort_paths(&mut children, store.tokens());
                boundaries.insert(parent, children);
            }
        }
        self.loadable.retain(|p| !within(*p));
        self.loadable
            .extend(partial.loadable.drain().filter(|p| within(*p)));
        self.options = options;
        self.merge_local_subtrees(
            store,
            partial,
            affected,
            hierarchy,
            boundaries.into_iter().collect(),
            roots,
            child_index,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Layer, PrimSpec, Reference};
    use crate::{LiveStage, StageOptions};
    use alloc::sync::Arc;
    #[test]
    fn payload_control_preserves_unrelated_record_identity() {
        let mut store = crate::InMemoryStore::default();
        let host = store.path("/Host");
        let source = store.path("/Asset");
        let child = store.path("/Asset/Child");
        let unrelated = store.path("/Unrelated");
        let mut root = Layer::new(LayerId(1));
        let mut spec = PrimSpec::def();
        spec.payloads.make_mut().explicit = Some(alloc::vec![Reference::with_asset(
            LayerId(2),
            source,
            "asset.usda"
        )]);
        root.insert_prim(host, spec);
        root.insert_prim(unrelated, PrimSpec::def());
        store.insert_layer(root);
        let mut asset = Layer::new(LayerId(2));
        asset.insert_prim(source, PrimSpec::def());
        asset.insert_prim(child, PrimSpec::def());
        store.insert_layer(asset);
        let mut live = LiveStage::compose(&mut store, LayerId(1), StageOptions::default());
        let retained = live.stage().prims[&unrelated].data.clone();
        live.unload(&store, host);
        let changes = live.recompose_changes(&mut store);
        assert_eq!(changes.resynced, [host]);
        assert!(!live.recomposition_work().full_rebuild);
        assert!(Arc::ptr_eq(&retained, &live.stage().prims[&unrelated].data));
        live.load(&store, host, LoadPolicy::WithDescendants);
        live.recompose_changes(&mut store);
        assert!(Arc::ptr_eq(&retained, &live.stage().prims[&unrelated].data));
        assert!(live.stage().has_prim(store.path("/Host/Child")));
        assert!(live.mute_layer(LayerId(2)).unwrap());
        live.recompose_changes(&mut store);
        // The muted payload target is hidden, not missing.
        assert!(
            !live
                .stage()
                .composition_errors()
                .iter()
                .any(|error| matches!(error, crate::CompositionError::MissingLayer(_)))
        );
        assert!(!live.recomposition_work().full_rebuild);
        assert!(Arc::ptr_eq(&retained, &live.stage().prims[&unrelated].data));
        assert!(live.unmute_layer(LayerId(2)));
        live.recompose_changes(&mut store);
        assert!(live.stage().has_prim(store.path("/Host/Child")));
        assert!(Arc::ptr_eq(&retained, &live.stage().prims[&unrelated].data));
    }
}
