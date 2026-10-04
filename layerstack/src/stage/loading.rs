// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit, stage-local payload and layer controls. The host retains ownership
//! of assets, storage and scheduling; these controls never load or evict data.
//! AOUSD Core §10.3.2.7 (payloads), §11 (population); OpenUSD
//! `UsdStageLoadRules`, `UsdStage::MuteAndUnmuteLayers`.

use crate::{
    AssetAvailability, Layer, LayerId, LayerStore, Path, PathId, PathInterner, TokenInterner,
};
use alloc::{
    borrow::ToOwned,
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};

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
/// OpenUSD: `UsdStageLoadRules::GetEffectiveRuleForPath`.
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

    /// Authored literal rules, ordered by interned path components.
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

    fn replace_subtree(&mut self, path: Path, rule: PayloadRule) {
        self.rules.retain(|p, _| p.strip_prefix(&path).is_none());
        self.rules.insert(path, rule);
    }

    /// The effective rule, including ancestor payloads required by a selected
    /// descendant. Literal exclusions between a prim and a deeper selection
    /// block that selection from making this prim loaded, matching OpenUSD.
    #[must_use]
    pub fn effective_rule(&self, path: &Path) -> PayloadRule {
        let prefix = self
            .rules
            .iter()
            .filter(|(p, _)| path.strip_prefix(p).is_some())
            .max_by_key(|(p, _)| p.depth());
        match prefix {
            None | Some((_, PayloadRule::All)) => return PayloadRule::All,
            Some((p, PayloadRule::Only)) if p == path => return PayloadRule::Only,
            _ => {}
        }
        for (p, rule) in &self.rules {
            if p == path || p.strip_prefix(path).is_none() || *rule == PayloadRule::None {
                continue;
            }
            let blocked = self.rules.keys().any(|between| {
                between != path
                    && between != p
                    && between.strip_prefix(path).is_some()
                    && p.strip_prefix(between).is_some()
            });
            if !blocked {
                return PayloadRule::Only;
            }
        }
        PayloadRule::None
    }

    /// Whether a payload authored at `path` is included by these rules.
    #[must_use]
    pub fn is_loaded(&self, path: &Path) -> bool {
        self.effective_rule(path) != PayloadRule::None
    }
}

/// An invalid atomic layer-muting request. No control changes are applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerMuteError {
    /// The stage's root layer cannot be muted.
    RootLayer,
    /// A layer was requested in both the mute and unmute sets.
    ConflictingRequest,
}
impl core::fmt::Display for LayerMuteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::RootLayer => "the stage root layer cannot be muted",
            Self::ConflictingRequest => "a layer cannot be muted and unmuted in one request",
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
    fn tokens(&self) -> &TokenInterner {
        self.inner.tokens()
    }
    fn tokens_mut(&mut self) -> &mut TokenInterner {
        self.inner.tokens_mut()
    }
    fn paths(&self) -> &PathInterner {
        self.inner.paths()
    }
    fn paths_mut(&mut self) -> &mut PathInterner {
        self.inner.paths_mut()
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
        let path = Path::parse_absolute(&absolute, store.tokens_mut()).ok()?;
        let id = store.paths_mut().intern(path);
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

    /// The root layer's effective time-code rate, including framesPerSecond
    /// fallback and the USD default of 24. Sublayer rates do not set stage rate.
    #[must_use]
    pub fn time_codes_per_second(&self, store: &dyn LayerStore) -> f64 {
        self.root_layer()
            .and_then(|root| store.layer(root))
            .map_or(24.0, |l| l.time_codes_per_second(store.tokens()))
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
    /// OpenUSD: `UsdStage::FindLoadable`.
    #[must_use]
    pub fn loadable_paths(&self, paths: &PathInterner, root: PathId) -> Vec<PathId> {
        let root = paths.resolve(root);
        let mut out: Vec<_> = self
            .loadable
            .iter()
            .copied()
            .filter(|p| self.is_active(*p) && paths.resolve(*p).strip_prefix(root).is_some())
            .collect();
        out.sort_by(|a, b| paths.resolve(*a).cmp(paths.resolve(*b)));
        out
    }

    /// Currently populated payload prims included by the snapshot's rules.
    /// Inclusion does not prove that the host supplied every requested asset;
    /// inspect composition errors for unresolved targets.
    /// Corresponds to the visible part of `UsdStage::GetLoadSet`. OpenUSD can
    /// also retain cached included payload paths outside a changed population
    /// mask; this snapshot query omits those unpopulated paths.
    #[must_use]
    pub fn loaded_payload_paths(&self, paths: &PathInterner) -> Vec<PathId> {
        let mut out: Vec<_> = self
            .loadable
            .iter()
            .copied()
            .filter(|p| self.has_prim(*p) && self.options.load_rules.is_loaded(paths.resolve(*p)))
            .collect();
        out.sort_by(|a, b| paths.resolve(*a).cmp(paths.resolve(*b)));
        out
    }
}
