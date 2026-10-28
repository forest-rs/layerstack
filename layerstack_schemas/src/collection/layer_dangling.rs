// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dangling targets by the layer that authors them: the per-layer
//! companion of [`DanglingReport`](super::DanglingReport).
//!
//! The composed report checks what the stage holds; this one checks what
//! each layer authors, so every finding names a spec to edit. Each
//! relationship target, attribute connection and collection include or
//! exclude a property spec authors is read as the stage reads it
//! ([`Stage::authored_targets`](layerstack::Stage::authored_targets)):
//! mapped across the arcs that bring the spec in, once for each composed
//! prim that reads it, then checked with
//! [`Stage::dangling_kind`](layerstack::Stage::dangling_kind), the same
//! check the composed report makes.
//!
//! Spec: AOUSD Core §10.3.2 (arcs map namespace), §12.4 (target lists),
//! §15 (collections).

use alloc::vec::Vec;

use layerstack::{
    AuthoredTarget, DanglingCounts, DanglingKind, DropReason, LayerId, NodeId, PropertyPath,
    SpecPath, TargetListPosition, TargetPath, TargetRead, UnreadTargetSpec,
};

use super::collection_name;
use crate::Scene;
use crate::usd::CollectionApi;

/// Why an authored target, connection, include or exclude names nothing,
/// as one composed prim reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AuthoredTargetProblem {
    /// The mapped path dangles ([`DanglingKind`]), as it would in the
    /// composed report.
    Dangling(DanglingKind),
    /// A collection include of a collection path whose prim resolves
    /// without that collection
    /// ([`CollectionTargetProblem::NoCollection`](super::CollectionTargetProblem::NoCollection)).
    NoCollection,
    /// Composition dropped the path, which names nothing on the stage: it
    /// is outside the namespace the arc maps, or names an instance of the
    /// class that authors it. [`layerstack::Stage::composition_errors`]
    /// reports it too.
    Dropped(DropReason),
}

/// One authored path that names nothing, with the spec that authors it.
///
/// To fix it, edit the spec in its layer: with layerstack's
/// `Transaction`, `remove_target(Address::spec(layer, spec), authored)`
/// takes `authored` out of its list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerDanglingTarget {
    /// The property spec, in its layer's namespace, with any variant
    /// selections.
    pub spec: SpecPath,
    /// The list that authors the path.
    pub position: TargetListPosition,
    /// The path as authored, in the layer's namespace.
    pub authored: TargetPath,
    /// The composed property that reads the spec.
    pub source: PropertyPath,
    /// The node of `source`'s prim graph the spec is read through.
    pub node: NodeId,
    /// The stage path composition maps `authored` to; `None` when it
    /// drops it.
    pub target: Option<TargetPath>,
    /// Whether the composed list of `source` holds `target`, so that the
    /// composed report counts it too: `false` when a stronger opinion
    /// deletes it, or composition dropped it.
    pub composed: bool,
    /// Why it names nothing.
    pub problem: AuthoredTargetProblem,
}

impl LayerDanglingTarget {
    /// The example as text, within its layer: the spec, the composed
    /// property that reads it when that is another path, the list, the
    /// authored path, the stage path when that differs, and the problem.
    ///
    /// `/Spot.shade appended /Gone (NoPrim)` from a sublayer;
    /// `/Asset.shade (as /World/A.shade) explicit /Asset/Gone ->
    /// /World/A/Gone (NoPrim)` through a reference; `/Asset.shade (as
    /// /World/A.shade) explicit /Outside (OutsideArc(References))` for a
    /// path the reference drops.
    ///
    /// Its paths are written as text with `store`, the store the stage was
    /// composed from, as every diagnostic's `display` writes its ids.
    #[must_use]
    pub fn display<'a>(
        &'a self,
        store: &'a dyn layerstack::LayerStore,
    ) -> impl core::fmt::Display + 'a {
        crate::diagnostic::Shown(move |f: &mut core::fmt::Formatter<'_>| {
            let (paths, tokens) = (store.paths(), store.tokens());
            f.write_str(&self.spec.display(paths, tokens))?;
            let concrete = PropertyPath::new(self.spec.prim_path(), self.source.property());
            if self.source != concrete {
                write!(f, " (as {})", self.source.display(paths, tokens))?;
            }
            write!(
                f,
                " {} {}",
                self.position.as_str(),
                self.authored.display(paths, tokens)
            )?;
            if let Some(target) = self.target.filter(|target| *target != self.authored) {
                write!(f, " -> {}", target.display(paths, tokens))?;
            }
            match self.problem {
                AuthoredTargetProblem::Dangling(kind) => write!(f, " ({kind:?})"),
                AuthoredTargetProblem::NoCollection => f.write_str(" (NoCollection)"),
                AuthoredTargetProblem::Dropped(reason) => write!(f, " ({reason:?})"),
            }
        })
    }
}

/// The authored paths of one layer, and those that name nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerDangling {
    /// The layer.
    pub layer: LayerId,
    /// Property specs that author a target, connection, include or exclude
    /// in an inserting list, read or not.
    pub specs: usize,
    /// Authored paths checked: each path of each read spec, once per
    /// composed prim that reads it.
    pub targets: usize,
    /// Of those, the mapped paths that dangle, by kind. An include of a
    /// collection path is counted here when its prim does not resolve.
    pub dangling: DanglingCounts,
    /// Of those, the paths composition dropped
    /// ([`AuthoredTargetProblem::Dropped`]).
    pub dropped: usize,
    /// Of those, the includes of a collection that is not there
    /// ([`AuthoredTargetProblem::NoCollection`]).
    pub missing_collections: usize,
    /// Authored paths not checked, because a stronger explicit list
    /// replaces their opinion ([`TargetRead::Replaced`]): the stage reads
    /// none of them. Counted once per reading prim.
    pub replaced: usize,
    /// Property specs with paths that no composed prim reads, whose paths
    /// are not checked: in an unselected variant, beneath an inactive
    /// prim, or where no arc reaches.
    pub unread_specs: usize,
    /// The first paths that name nothing, at most the number asked for, in
    /// the walk's order: by spec, then reading prim, then list.
    pub examples: Vec<LayerDanglingTarget>,
    /// The first specs nothing reads, at most the number asked for, with
    /// why ([`layerstack::UnreadReason`]).
    pub unread: Vec<UnreadTargetSpec>,
}

impl LayerDangling {
    /// Every authored path that names nothing: dangling, dropped, or an
    /// include of a missing collection. Replaced paths and unread specs
    /// are not among them.
    #[must_use]
    pub fn total(&self) -> usize {
        self.dangling.total() + self.dropped + self.missing_collections
    }
}

/// Dangling targets, connections, includes and excludes by the layer that
/// authors them ([`Self::compute`]); the per-layer companion of
/// [`DanglingReport`](super::DanglingReport).
///
/// ```
/// use std::sync::Arc;
///
/// use layerstack::{InMemoryStore, Layer, LayerId, ListOp, PrimSpec, PropertySpec, Stage};
/// use layerstack::{StageOptions, SublayerEntry, TargetPath};
/// use layerstack_schemas::{AuthoredTargetProblem, LayerDanglingReport, Scene};
///
/// let mut store = InMemoryStore::default();
/// let options = StageOptions {
///     schemas: Some(Arc::new(layerstack_schemas::openusd(store.tokens()))),
///     ..StageOptions::default()
/// };
/// let (lamp, gone) = (store.path("/Lamp"), store.path("/Gone"));
/// let shines_on = store.tokens().intern("shinesOn");
/// // A scope's sublayer authors the dangling target.
/// let mut scope = Layer::new(LayerId(2));
/// scope.insert_prim(
///     lamp,
///     PrimSpec::def().with_property(
///         shines_on,
///         PropertySpec::relationship()
///             .with_targets(ListOp::appended(vec![TargetPath::Prim(gone)])),
///     ),
/// );
/// let mut root = Layer::new(LayerId(1));
/// root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
/// store.insert_layer(root);
/// store.insert_layer(scope);
/// let stage = Stage::compose(&mut store, LayerId(1), options);
///
/// let report = LayerDanglingReport::compute(&Scene::new(&stage, &store), 3);
/// assert!(!report.is_clean());
/// let layer = report.layer(LayerId(2)).unwrap();
/// assert_eq!(layer.dangling.no_prim, 1);
/// let example = &layer.examples[0];
/// assert_eq!(example.problem, AuthoredTargetProblem::Dangling(layerstack::DanglingKind::NoPrim));
/// assert_eq!(
///     example.display(&store).to_string(),
///     "/Lamp.shinesOn appended /Gone (NoPrim)"
/// );
/// assert_eq!(report.layer(LayerId(1)).unwrap().total(), 0);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LayerDanglingReport {
    /// Every layer the stage composes, in the order of
    /// [`layerstack::AuthoredTargets::layers`]: the stage's own layer
    /// stack, strongest first, then the other layers it uses, by id.
    pub layers: Vec<LayerDangling>,
}

impl LayerDanglingReport {
    /// Checks every target, connection, include and exclude the layers of
    /// `scene` author, with the first `examples` of each layer's findings
    /// and of its unread specs.
    ///
    /// The paths are those of
    /// [`Stage::authored_targets`](layerstack::Stage::authored_targets):
    /// the explicit, prepended, appended and added lists of every property
    /// spec, each read by every composed prim that reads its spec, mapped
    /// to the stage. Deleted and reordered paths are not checked: they
    /// name nothing the stage reads, and deleting a weaker layer's
    /// dangling path is how a stronger one hides it. Each mapped path is
    /// checked with `Stage::dangling_kind`, and an include of a collection
    /// path, by a prim with that `CollectionAPI` instance applied, for the
    /// included collection, as [`DanglingReport`](super::DanglingReport)
    /// checks the composed lists. A path composition dropped dangles as
    /// [`AuthoredTargetProblem::Dropped`].
    ///
    /// A mapped dangling path appears in the composed report too, unless a
    /// stronger opinion deletes it ([`LayerDanglingTarget::composed`]); a
    /// dropped one never does.
    ///
    /// The work is the walk, then one check per path read; an example's
    /// `composed` reads its property's composed list. Nothing is cached.
    ///
    /// OpenUSD: no single call; the specs of `GetPropertyStack`, mapped
    /// through `PcpNodeRef::GetMapToRoot` as `GetTargets` maps them, then
    /// checked with `UsdStage::GetObjectAtPath`.
    #[must_use]
    pub fn compute(scene: &Scene<'_>, examples: usize) -> Self {
        let stage = scene.stage();
        let store = scene.store();
        let paths = store.paths();
        let walk = stage.authored_targets(store);
        let mut layers = Vec::with_capacity(walk.layers.len());
        for walked in walk.layers {
            let mut entry = LayerDangling {
                layer: walked.layer,
                specs: walked.specs,
                targets: 0,
                dangling: DanglingCounts::default(),
                dropped: 0,
                missing_collections: 0,
                replaced: 0,
                unread_specs: walked.unread.len(),
                examples: Vec::new(),
                unread: walked.unread.into_iter().take(examples).collect(),
            };
            for target in &walked.targets {
                let (mapped, problem) = match target.read {
                    TargetRead::Replaced => {
                        entry.replaced += 1;
                        continue;
                    }
                    TargetRead::Dropped(reason) => {
                        entry.targets += 1;
                        entry.dropped += 1;
                        (None, AuthoredTargetProblem::Dropped(reason))
                    }
                    TargetRead::Mapped(mapped) => {
                        entry.targets += 1;
                        let kind = stage.dangling_kind(mapped, paths);
                        let missing = matches!(kind, None | Some(DanglingKind::NoProperty))
                            && included_collection(scene, target, mapped).is_some_and(
                                |(owner, name)| {
                                    !scene.has_api(owner, CollectionApi::SCHEMA, Some(name))
                                },
                            );
                        let problem = match kind {
                            _ if missing => {
                                entry.missing_collections += 1;
                                AuthoredTargetProblem::NoCollection
                            }
                            None => continue,
                            Some(kind) => {
                                entry.dangling.add(kind);
                                AuthoredTargetProblem::Dangling(kind)
                            }
                        };
                        (Some(mapped), problem)
                    }
                };
                if entry.examples.len() < examples {
                    let composed = mapped.is_some_and(|mapped| {
                        scene.property(target.source).is_some_and(|property| {
                            let list = match property.as_relationship() {
                                Some(relationship) => relationship.targets(),
                                None => property
                                    .as_attribute()
                                    .map(layerstack::Attribute::connections)
                                    .unwrap_or_default(),
                            };
                            list.contains(&mapped)
                        })
                    });
                    entry.examples.push(LayerDanglingTarget {
                        spec: target.spec.clone(),
                        position: target.position,
                        authored: target.authored,
                        source: target.source,
                        node: target.node,
                        target: mapped,
                        composed,
                        problem,
                    });
                }
            }
            layers.push(entry);
        }
        Self { layers }
    }

    /// Whether no layer authors a path that names nothing (see
    /// [`LayerDangling::total`]).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.layers.iter().all(|layer| layer.total() == 0)
    }

    /// The entry of `layer`, if the stage composes it.
    #[must_use]
    pub fn layer(&self, layer: LayerId) -> Option<&LayerDangling> {
        self.layers.iter().find(|entry| entry.layer == layer)
    }
}

/// The collection `mapped` names, when `target` is an include of a
/// collection path by a collection applied on its reading prim: the
/// included collection's prim and instance name.
fn included_collection<'a>(
    scene: &Scene<'a>,
    target: &AuthoredTarget,
    mapped: TargetPath,
) -> Option<(layerstack::PathId, &'a str)> {
    let TargetPath::Property(included) = mapped else {
        return None;
    };
    let tokens = scene.store().tokens();
    let owner = tokens
        .resolve(target.source.property())
        .strip_prefix("collection:")?
        .strip_suffix(":includes")?;
    if owner.is_empty()
        || !scene.has_api(
            target.source.prim_path(),
            CollectionApi::SCHEMA,
            Some(owner),
        )
    {
        return None;
    }
    collection_name(tokens.resolve(included.property())).map(|name| (included.prim_path(), name))
}
