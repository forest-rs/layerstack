// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dangling targets over a scene: relationship targets and attribute
//! connections by property name (layerstack's
//! [`Stage::dangling_targets`](layerstack::Stage::dangling_targets)), and
//! each collection's includes and excludes that name nothing.
//!
//! A path resolves, or dangles and why, as
//! [`Stage::dangling_kind`](layerstack::Stage::dangling_kind) decides for
//! any target ([`DanglingKind`]). An include of a collection path is
//! checked for the collection too: its prim must have that `CollectionAPI`
//! instance applied, as an expression reference's collection must.
//!
//! Spec: AOUSD Core §15 (collections); OpenUSD's `UsdCollectionAPI`
//! (`_ComputeMembershipQueryImpl`, which warns of an include on a prim the
//! stage does not have and reads an absent collection as empty).

use alloc::vec::Vec;

use layerstack::{DanglingCounts, DanglingKind, DanglingTargetReport, TargetPath};

use super::{CollectionIdentity, collection_name};
use crate::Scene;
use crate::usd::CollectionApi;

/// Which of a collection's relationships authors a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CollectionList {
    /// `collection:<name>:includes`.
    Includes,
    /// `collection:<name>:excludes`.
    Excludes,
}

/// Why a collection's include or exclude names nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CollectionTargetProblem {
    /// The path dangles as any target can ([`DanglingKind`]). For an
    /// include of a collection path, this is why its prim does not resolve.
    Dangling(DanglingKind),
    /// An include of a collection path (`/Lights/Sun.collection:shadowLink`)
    /// whose prim resolves without that `CollectionAPI` instance applied.
    /// OpenUSD reads such a collection as empty, and warns of nothing.
    NoCollection,
}

/// A dangling include or exclude of a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DanglingCollectionTarget {
    /// The relationship that authors it.
    pub list: CollectionList,
    /// The composed path.
    pub target: TargetPath,
    /// Why it names nothing.
    pub problem: CollectionTargetProblem,
}

impl DanglingCollectionTarget {
    /// The example as text, within its collection: `includes /Gone
    /// (NoPrim)`, or `includes /Lights/Fill.collection:glare
    /// (NoCollection)`.
    #[must_use]
    pub fn display(
        &self,
        paths: &layerstack::PathInterner,
        tokens: &layerstack::TokenInterner,
    ) -> alloc::string::String {
        let list = match self.list {
            CollectionList::Includes => "includes",
            CollectionList::Excludes => "excludes",
        };
        let problem = match self.problem {
            CollectionTargetProblem::Dangling(kind) => alloc::format!("{kind:?}"),
            CollectionTargetProblem::NoCollection => alloc::string::String::from("NoCollection"),
        };
        alloc::format!("{list} {} ({problem})", self.target.display(paths, tokens))
    }
}

/// One collection's includes and excludes, and those that name nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DanglingCollection {
    /// The collection: its owning prim and instance name.
    pub collection: CollectionIdentity,
    /// Composed includes checked: the collection's own, not those of the
    /// collections it includes, which their own entries report.
    pub includes: usize,
    /// Composed excludes checked.
    pub excludes: usize,
    /// Includes that dangle, by kind: an include of a collection path is
    /// counted here when its prim does not resolve.
    pub dangling_includes: DanglingCounts,
    /// Excludes that dangle, by kind.
    pub dangling_excludes: DanglingCounts,
    /// Includes of a collection path whose prim resolves without that
    /// collection ([`CollectionTargetProblem::NoCollection`]).
    pub missing_collections: usize,
    /// The first dangling includes, then excludes, in composed order: at
    /// most the number asked for.
    pub examples: Vec<DanglingCollectionTarget>,
}

impl DanglingCollection {
    /// Every include and exclude that names nothing.
    #[must_use]
    pub fn total(&self) -> usize {
        self.dangling_includes.total() + self.dangling_excludes.total() + self.missing_collections
    }
}

/// Dangling targets over a scene, by relationship and connection name and
/// by collection ([`Self::compute`]).
///
/// A collection's includes and excludes are relationships, so they are
/// also counted in [`Self::targets`], by relationship name
/// (`collection:<name>:includes`); [`Self::collections`] groups them by
/// collection, and checks that each included collection exists.
///
/// ```
/// use std::sync::Arc;
///
/// use layerstack::{InMemoryStore, Layer, LayerId, ListOp, PrimSpec, PropertySpec, Stage};
/// use layerstack::{StageOptions, TargetPath};
/// use layerstack_schemas::{DanglingReport, Scene};
///
/// let mut store = InMemoryStore::default();
/// let options = StageOptions {
///     schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
///     ..StageOptions::default()
/// };
/// let (lamp, gone) = (store.path("/Lamp"), store.path("/Gone"));
/// let shines_on = store.tokens.intern("shinesOn");
/// let mut layer = Layer::new(LayerId(1));
/// layer.insert_prim(
///     lamp,
///     PrimSpec::def().with_property(
///         shines_on,
///         PropertySpec::relationship()
///             .with_targets(ListOp::explicit(vec![TargetPath::Prim(gone)])),
///     ),
/// );
/// store.insert_layer(layer);
/// let stage = Stage::compose(&mut store, LayerId(1), options);
///
/// let report = DanglingReport::compute(&Scene::new(&stage, &store), 3);
/// let group = report.targets.relationship(shines_on).unwrap();
/// assert_eq!(group.dangling.no_prim, 1);
/// assert_eq!(group.examples[0].target, TargetPath::Prim(gone));
/// assert!(report.collections.is_empty());
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DanglingReport {
    /// Relationship targets and attribute connections, by property name.
    pub targets: DanglingTargetReport,
    /// Every `CollectionAPI` instance on the stage, in namespace order of
    /// the owning prim, then name.
    pub collections: Vec<DanglingCollection>,
}

impl DanglingReport {
    /// Counts the dangling targets of `scene`, with the first `examples` of
    /// each relationship name, connection name and collection.
    ///
    /// The collections are those of every prim the stage populates
    /// (`Stage::traverse_all`, instance proxies included), as
    /// [`crate::MembershipIndex::build`] finds them. Each include and
    /// exclude is checked with `Stage::dangling_kind`, and each include of
    /// a collection path, whose prim resolves, for that collection
    /// ([`CollectionTargetProblem::NoCollection`]).
    ///
    /// The work is the target walk of `Stage::dangling_targets`, then per
    /// collection its two composed target lists; nothing is compiled or
    /// cached.
    ///
    /// OpenUSD: `UsdCollectionAPI::GetIncludesRel`, `GetExcludesRel` and
    /// `GetTargets`, with `UsdStage::GetObjectAtPath`.
    #[must_use]
    pub fn compute(scene: &Scene<'_>, examples: usize) -> Self {
        let stage = scene.stage();
        let store = scene.store();
        let paths = store.paths();
        let tokens = store.tokens();
        let mut collections = Vec::new();
        if let Some(root) = scene.root() {
            for prim in stage.traverse_all(root) {
                let mut instances = CollectionApi::instances(scene, prim);
                instances.sort_unstable_by(|a, b| a.instance().cmp(b.instance()));
                for collection in instances {
                    let mut entry = DanglingCollection {
                        collection: CollectionIdentity {
                            owner: prim,
                            name: collection.instance().into(),
                        },
                        includes: 0,
                        excludes: 0,
                        dangling_includes: DanglingCounts::default(),
                        dangling_excludes: DanglingCounts::default(),
                        missing_collections: 0,
                        examples: Vec::new(),
                    };
                    let lists = [
                        (CollectionList::Includes, collection.includes()),
                        (CollectionList::Excludes, collection.excludes()),
                    ];
                    for (list, targets) in lists {
                        match list {
                            CollectionList::Includes => entry.includes = targets.len(),
                            CollectionList::Excludes => entry.excludes = targets.len(),
                        }
                        for target in targets {
                            let kind = stage.dangling_kind(target, paths);
                            let included = match target {
                                TargetPath::Property(property)
                                    if list == CollectionList::Includes =>
                                {
                                    collection_name(tokens.resolve(property.property()))
                                        .map(|name| (property.prim_path(), name))
                                }
                                _ => None,
                            };
                            let problem = match (kind, included) {
                                (None | Some(DanglingKind::NoProperty), Some((owner, name)))
                                    if !scene.has_api(owner, CollectionApi::SCHEMA, Some(name)) =>
                                {
                                    entry.missing_collections += 1;
                                    CollectionTargetProblem::NoCollection
                                }
                                (None, _) => continue,
                                (Some(kind), _) => {
                                    match list {
                                        CollectionList::Includes => {
                                            entry.dangling_includes.add(kind);
                                        }
                                        CollectionList::Excludes => {
                                            entry.dangling_excludes.add(kind);
                                        }
                                    }
                                    CollectionTargetProblem::Dangling(kind)
                                }
                            };
                            if entry.examples.len() < examples {
                                entry.examples.push(DanglingCollectionTarget {
                                    list,
                                    target,
                                    problem,
                                });
                            }
                        }
                    }
                    collections.push(entry);
                }
            }
        }
        Self {
            targets: stage.dangling_targets(store, examples),
            collections,
        }
    }

    /// Whether nothing dangles: no target, connection, include or exclude.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.targets.is_clean() && self.collections.iter().all(|c| c.total() == 0)
    }

    /// The entry of the collection `name` on `owner`.
    #[must_use]
    pub fn collection(&self, owner: layerstack::PathId, name: &str) -> Option<&DanglingCollection> {
        self.collections
            .iter()
            .find(|entry| entry.collection.owner == owner && entry.collection.name == name)
    }
}
