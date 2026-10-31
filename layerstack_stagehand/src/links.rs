// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Light and shadow linking over a draw list's items, as Hydra's light-link
//! categories.
//!
//! Each light's `collection:lightLink` and `collection:shadowLink`
//! (`UsdLuxLightAPI`, both `CollectionAPI` instances with an
//! `includeRoot = true` fallback) is a `UsdCollectionMembershipQuery`
//! ([`MembershipQuery::compute`]). As `UsdImaging_CollectionCache` keeps
//! them, equal queries share one category ([`LinkCategory`]), and a query
//! whose rule map is exactly `/` with `expandPrims` (the fallback: nothing
//! authored) is trivial: it links everything and has no category
//! (`_IsQueryTrivial`, `GetIdForCollection`'s empty token). A target is
//! classified against each distinct category, as
//! `ComputeCollectionsContainingPath` gives an rprim its categories
//! (`HdRprim::GetCategories`), and a light links a target when its
//! category is empty or among the target's (`HdLight`'s `lightLink` and
//! `shadowLink` parameters).
//!
//! OpenUSD: `pxr/usdImaging/usdImaging/collectionCache.cpp`,
//! `UsdImagingPrimAdapter::GetLightParamValue`,
//! `UsdImagingDelegate::GetCategories` and `GetInstanceCategories`,
//! `UsdImagingInstanceAdapter::GetInstanceCategories`;
//! `pxr/usd/usdLux/lightAPI.h` (the linking collections);
//! `pxr/usd/usd/collectionMembershipQuery.h` (`IsPathIncluded`). AOUSD Core
//! §15 (collections).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use layerstack::{Changes, PathId, PropertyPath, TargetPath};
use layerstack_schemas::{
    ExpansionRule, MembershipProblem, MembershipQuery, MembershipRule, Scene,
};

use crate::draw::{ItemId, stage_metadata, under};
use crate::light::LightId;

/// Which of a light's two linking collections (`UsdLuxLightAPI`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinkKind {
    /// `collection:lightLink`: what the light illuminates.
    Light,
    /// `collection:shadowLink`: what casts the light's shadows.
    Shadow,
}

impl LinkKind {
    /// Both, in order.
    pub const ALL: [Self; 2] = [Self::Light, Self::Shadow];

    /// The collection's name: `lightLink` or `shadowLink`.
    #[must_use]
    pub fn collection(self) -> &'static str {
        match self {
            Self::Light => "lightLink",
            Self::Shadow => "shadowLink",
        }
    }
}

/// One distinct link membership among a draw list's lights: Hydra's
/// light-link category. Lights whose link collections compute equal
/// membership queries share one. Stable while a light's link uses it,
/// never reused for another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkCategory(pub u32);

/// What a link decides for: the path of an item a link collection is
/// matched against ([`crate::DrawItem::link_paths`]).
///
/// - A gprim outside any instance: its path.
/// - A native instancing prototype's item: each of its instances, by its
///   instance proxy's path, as `UsdImagingInstanceAdapter::
///   GetInstanceCategories` matches each instance by its stage path.
/// - A point instancer's item: the instancer's path, for every instance:
///   `UsdImaging` gives a point instancer's prototypes no categories of
///   their own (`UsdImagingDelegate::GetCategories`), so a link naming a
///   prototype prim links nothing there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LinkTarget {
    /// The item.
    pub item: ItemId,
    /// The path matched.
    pub path: PathId,
}

/// A category's members and the links using it.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkSet {
    /// Its category.
    pub category: LinkCategory,
    /// The light links computing its membership, each a light and which of
    /// its collections.
    pub links: BTreeSet<(LightId, LinkKind)>,
    /// The targets it includes (`IsPathIncluded`).
    pub members: BTreeSet<LinkTarget>,
    query: MembershipQuery,
}

impl LinkSet {
    /// The membership query its links compute.
    #[must_use]
    pub fn query(&self) -> &MembershipQuery {
        &self.query
    }
}

/// How one light's link changed in a diff: the targets it links now and
/// did not before, and those it no longer links.
///
/// Listed for a light the diff neither adds nor removes, among the targets
/// of items the diff neither adds nor removes: a new light's or a new
/// item's links are read from the list ([`crate::DrawList::is_linked`]),
/// and a removed item is in no link. A trivial link (no category) links
/// every target, so a new instance of a kept prototype item is added to
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkChange {
    /// The light.
    pub light: LightId,
    /// Which of its links.
    pub kind: LinkKind,
    /// Targets linked now and not before, sorted.
    pub added: Vec<LinkTarget>,
    /// Targets linked before and not now, sorted.
    pub removed: Vec<LinkTarget>,
}

/// The draw list's link state.
#[derive(Debug, Default)]
pub(crate) struct Links {
    sets: BTreeMap<LinkCategory, LinkSet>,
    /// Each listed light's links: their categories, `None` for a trivial
    /// one.
    of: BTreeMap<(LightId, LinkKind), Option<LinkCategory>>,
    /// Each item's targets' paths ([`crate::DrawItem::link_paths`]), as of
    /// the last diff.
    targets: BTreeMap<ItemId, Vec<PathId>>,
    next: u32,
}

/// Whether a query links everything: its rule map is exactly the
/// pseudo-root with `expandPrims`, the `UsdLuxLightAPI` fallback.
/// OpenUSD: `_IsQueryTrivial` (`collectionCache.cpp`).
fn trivial(query: &MembershipQuery, root: PathId) -> bool {
    let mut rules = query.rules();
    matches!(
        (rules.next(), rules.next()),
        (Some((TargetPath::Prim(path), MembershipRule::Include(ExpansionRule::ExpandPrims))), None)
            if *path == root
    )
}

/// What a diff gives the link pass.
pub(crate) struct LinkInputs<'c> {
    /// The change records, for the collections other links read; `None`
    /// for a time change, which changes no collection.
    pub changes: Option<&'c Changes>,
    /// The resynced roots.
    pub roots: &'c [PathId],
    /// Light links whose own collection properties a change reached.
    pub relink: BTreeSet<(LightId, LinkKind)>,
    /// Items read again entirely (resynced, or an unlisted change): their
    /// targets are classified again, as an expression's predicates may
    /// read what changed.
    pub reclassify: BTreeSet<ItemId>,
    /// Items the diff adds and removes.
    pub added: &'c BTreeSet<ItemId>,
    pub removed: &'c BTreeSet<ItemId>,
    /// Lights the diff adds.
    pub lights_added: &'c BTreeSet<LightId>,
}

/// What the link pass did.
#[derive(Default)]
pub(crate) struct LinkOutcome {
    pub changes: Vec<LinkChange>,
    /// Lights whose categories changed: their items were updated.
    pub relinked: BTreeSet<LightId>,
    pub evaluated: u32,
    pub classified: u32,
}

/// Whether `name`, a property of the prim of `collection`, is one of the
/// collection's own properties (`collection:<name>:includes` and the
/// like), or names the prim's change without an inventory (`None`).
fn of_collection(collection: &str, name: Option<&str>) -> bool {
    name.is_none_or(|name| {
        name.strip_prefix(collection)
            .is_some_and(|rest| rest.starts_with(':'))
    })
}

/// Whether `changes` reach the collection at `collection`: a resync over
/// its prim, a change of one of its properties, or an unlisted change of
/// its prim or one above it.
fn collection_touched(
    scene: &Scene<'_>,
    changes: &Changes,
    roots: &[PathId],
    collection: PropertyPath,
) -> bool {
    let tokens = scene.store().tokens();
    let prim = collection.prim_path();
    let name = tokens.resolve(collection.property());
    roots.iter().any(|&root| under(scene, prim, root))
        || changes.changed_info_only.iter().any(|&changed| {
            if stage_metadata(scene, changed) {
                return false;
            }
            match changes
                .property_changes
                .iter()
                .find(|properties| properties.prim == changed)
            {
                None => under(scene, prim, changed),
                Some(properties) => {
                    changed == prim
                        && properties
                            .fields
                            .iter()
                            .any(|field| of_collection(name, Some(tokens.resolve(field.name))))
                }
            }
        })
        || changes.resynced_properties.iter().any(|property| {
            property.prim_path() == prim
                && of_collection(name, Some(tokens.resolve(property.property())))
        })
}

/// Whether `changes` touch any collection property at all: what a query
/// with a missing reference, which a new collection could satisfy, reads.
fn any_collection_touched(scene: &Scene<'_>, changes: &Changes, roots: &[PathId]) -> bool {
    let tokens = scene.store().tokens();
    !roots.is_empty()
        || changes.changed_info_only.iter().any(|&changed| {
            !stage_metadata(scene, changed)
                && changes
                    .property_changes
                    .iter()
                    .find(|properties| properties.prim == changed)
                    .is_none_or(|properties| {
                        properties
                            .fields
                            .iter()
                            .any(|field| tokens.resolve(field.name).starts_with("collection:"))
                    })
        })
        || changes.resynced_properties.iter().any(|property| {
            tokens
                .resolve(property.property())
                .starts_with("collection:")
        })
}

/// Whether a query reads a collection `changes` reach: one it includes or
/// its expression references, or any when a reference is missing.
fn reads_touched(
    scene: &Scene<'_>,
    changes: &Changes,
    roots: &[PathId],
    query: &MembershipQuery,
) -> bool {
    query
        .included_collections()
        .iter()
        .chain(query.referenced_collections())
        .any(|&collection| collection_touched(scene, changes, roots, collection))
        || (query
            .problems()
            .iter()
            .any(|problem| matches!(problem, MembershipProblem::MissingReference { .. }))
            && any_collection_touched(scene, changes, roots))
}

impl Links {
    /// The set of `category`.
    pub(crate) fn set(&self, category: LinkCategory) -> Option<&LinkSet> {
        self.sets.get(&category)
    }

    /// Every set, by category.
    pub(crate) fn sets(&self) -> impl Iterator<Item = &LinkSet> {
        self.sets.values()
    }

    /// The category of a light's link: `Some(None)` for a trivial one,
    /// `None` for a light not listed.
    pub(crate) fn category(&self, light: LightId, kind: LinkKind) -> Option<Option<LinkCategory>> {
        self.of.get(&(light, kind)).copied()
    }

    /// Whether the light's link links `target`.
    pub(crate) fn is_linked(&self, light: LightId, kind: LinkKind, target: LinkTarget) -> bool {
        match self.of.get(&(light, kind)) {
            Some(None) => self
                .targets
                .get(&target.item)
                .is_some_and(|paths| paths.contains(&target.path)),
            Some(Some(category)) => self.sets[category].members.contains(&target),
            None => false,
        }
    }

    /// The categories including `target`, sorted.
    pub(crate) fn categories_of(&self, target: LinkTarget) -> Vec<LinkCategory> {
        self.sets
            .values()
            .filter(|set| set.members.contains(&target))
            .map(|set| set.category)
            .collect()
    }

    /// Brings the links up to the lights and targets now listed: computes
    /// the queries of new lights and of those a change reaches, classifies
    /// new targets (and those of items read again entirely) against each
    /// category, and a new category's every target.
    #[expect(
        clippy::too_many_lines,
        reason = "one pass: queries, targets, categories, then the listing"
    )]
    pub(crate) fn apply(
        &mut self,
        scene: &Scene<'_>,
        lights: &BTreeSet<LightId>,
        updates: BTreeMap<ItemId, Vec<PathId>>,
        inputs: &LinkInputs<'_>,
    ) -> LinkOutcome {
        let mut outcome = LinkOutcome::default();
        let root = scene.store().paths().intern(layerstack::Path::root());
        let persistent = |target: &LinkTarget| {
            !inputs.added.contains(&target.item) && !inputs.removed.contains(&target.item)
        };

        // The links to compute again: those of new lights, of lights a
        // change reached, and of every light whose category reads a
        // collection a change reached.
        let mut relink: BTreeSet<(LightId, LinkKind)> = BTreeSet::new();
        for &light in lights {
            for kind in LinkKind::ALL {
                if !self.of.contains_key(&(light, kind)) || inputs.relink.contains(&(light, kind)) {
                    relink.insert((light, kind));
                }
            }
        }
        if let Some(changes) = inputs.changes {
            for set in self.sets.values() {
                if reads_touched(scene, changes, inputs.roots, &set.query) {
                    relink.extend(set.links.iter().copied());
                }
            }
        }

        // Each relinked light's query; a switch when its membership is not
        // the one it has. Its members before, for the listing, when the
        // light was listed before.
        type Switch = (
            (LightId, LinkKind),
            Option<LinkCategory>,
            MembershipQuery,
            Option<BTreeSet<LinkTarget>>,
        );
        let mut switches: Vec<Switch> = Vec::new();
        for &(light, kind) in &relink {
            if !lights.contains(&light) {
                continue;
            }
            let query = MembershipQuery::compute(scene, light.path, kind.collection());
            outcome.evaluated += 1;
            let old = self.of.get(&(light, kind)).copied();
            let same = match old {
                Some(Some(category)) => self.sets[&category].query == query,
                Some(None) => trivial(&query, root),
                None => false,
            };
            if same {
                continue;
            }
            let before = old.map(|category| match category {
                Some(category) => self.sets[&category].members.clone(),
                None => every(&self.targets).collect(),
            });
            switches.push(((light, kind), old.flatten(), query, before));
        }

        // Lights gone, and those switching, leave their categories.
        let gone: Vec<(LightId, LinkKind)> = self
            .of
            .keys()
            .filter(|(light, _)| !lights.contains(light))
            .copied()
            .collect();
        for key in gone {
            if let Some(Some(category)) = self.of.remove(&key)
                && let Some(set) = self.sets.get_mut(&category)
            {
                set.links.remove(&key);
            }
        }
        for (key, old, _, _) in &switches {
            if let Some(category) = old
                && let Some(set) = self.sets.get_mut(category)
            {
                set.links.remove(key);
            }
        }
        self.sets.retain(|_, set| !set.links.is_empty());

        // Targets gone leave every category; new ones, and those of items
        // read again entirely, are classified against each.
        let mut gained: BTreeMap<LinkCategory, BTreeSet<LinkTarget>> = BTreeMap::new();
        let mut lost: BTreeMap<LinkCategory, BTreeSet<LinkTarget>> = BTreeMap::new();
        let mut gone_targets: Vec<LinkTarget> = Vec::new();
        let mut new_targets: Vec<LinkTarget> = Vec::new();
        for (&item, paths) in &updates {
            let old = self.targets.get(&item).map_or(&[][..], Vec::as_slice);
            let target = |path: &PathId| LinkTarget { item, path: *path };
            gone_targets.extend(old.iter().filter(|path| !paths.contains(path)).map(target));
            new_targets.extend(paths.iter().filter(|path| !old.contains(path)).map(target));
        }
        for target in &gone_targets {
            for set in self.sets.values_mut() {
                if set.members.remove(target) && persistent(target) {
                    lost.entry(set.category).or_default().insert(*target);
                }
            }
        }
        for (item, paths) in updates {
            if paths.is_empty() {
                self.targets.remove(&item);
            } else {
                self.targets.insert(item, paths);
            }
        }
        let mut classify: BTreeSet<LinkTarget> = new_targets.iter().copied().collect();
        for &item in &inputs.reclassify {
            for &path in self.targets.get(&item).into_iter().flatten() {
                classify.insert(LinkTarget { item, path });
            }
        }
        for target in &classify {
            for set in self.sets.values_mut() {
                outcome.classified += 1;
                let included = set
                    .query
                    .is_included(scene, TargetPath::Prim(target.path))
                    .is_included();
                let changed = if included {
                    set.members.insert(*target)
                } else {
                    set.members.remove(target)
                };
                if changed && persistent(target) {
                    let into = if included { &mut gained } else { &mut lost };
                    into.entry(set.category).or_default().insert(*target);
                }
            }
        }

        // Switched links join the category of an equal query, or make one,
        // classifying every target; a trivial query has none.
        let mut switched: BTreeMap<(LightId, LinkKind), Option<BTreeSet<LinkTarget>>> =
            BTreeMap::new();
        for (key, _, query, before) in switches {
            let category = if trivial(&query, root) {
                None
            } else if let Some(set) = self.sets.values_mut().find(|set| set.query == query) {
                set.links.insert(key);
                Some(set.category)
            } else {
                let category = LinkCategory(self.next);
                self.next += 1;
                let mut members = BTreeSet::new();
                for target in every(&self.targets) {
                    outcome.classified += 1;
                    if query
                        .is_included(scene, TargetPath::Prim(target.path))
                        .is_included()
                    {
                        members.insert(target);
                    }
                }
                self.sets.insert(
                    category,
                    LinkSet {
                        category,
                        links: BTreeSet::from([key]),
                        members,
                        query,
                    },
                );
                Some(category)
            };
            self.of.insert(key, category);
            outcome.relinked.insert(key.0);
            switched.insert(key, before);
        }

        // The listing, for the lights kept.
        let all: Option<BTreeSet<LinkTarget>> = switched
            .iter()
            .any(|(&(light, kind), before)| {
                before.is_some() && self.of.get(&(light, kind)) == Some(&None)
            })
            .then(|| every(&self.targets).collect());
        for (&(light, kind), &category) in &self.of {
            if inputs.lights_added.contains(&light) {
                continue;
            }
            let (added, removed): (Vec<LinkTarget>, Vec<LinkTarget>) =
                match switched.get(&(light, kind)) {
                    Some(None) => continue,
                    Some(Some(before)) => {
                        let after = match category {
                            Some(category) => &self.sets[&category].members,
                            None => all
                                .as_ref()
                                .expect("gathered for a switch to a trivial link"),
                        };
                        (
                            after
                                .difference(before)
                                .filter(|target| persistent(target))
                                .copied()
                                .collect(),
                            before
                                .difference(after)
                                .filter(|target| persistent(target))
                                .copied()
                                .collect(),
                        )
                    }
                    None => match category {
                        None => (
                            new_targets
                                .iter()
                                .filter(|target| persistent(target))
                                .copied()
                                .collect(),
                            gone_targets
                                .iter()
                                .filter(|target| persistent(target))
                                .copied()
                                .collect(),
                        ),
                        Some(category) => (
                            gained
                                .get(&category)
                                .into_iter()
                                .flatten()
                                .copied()
                                .collect(),
                            lost.get(&category).into_iter().flatten().copied().collect(),
                        ),
                    },
                };
            if !added.is_empty() || !removed.is_empty() {
                outcome.changes.push(LinkChange {
                    light,
                    kind,
                    added,
                    removed,
                });
            }
        }
        outcome
    }
}

/// Every target of `targets`, in order.
fn every(targets: &BTreeMap<ItemId, Vec<PathId>>) -> impl Iterator<Item = LinkTarget> + '_ {
    targets
        .iter()
        .flat_map(|(&item, paths)| paths.iter().map(move |&path| LinkTarget { item, path }))
}

#[cfg(test)]
mod tests {
    use alloc::collections::{BTreeMap, BTreeSet};
    use alloc::string::{String, ToString};
    use alloc::sync::Arc;
    use alloc::vec::Vec;

    use layerstack::{
        EditTarget, InMemoryStore, LayerId, ListPosition, LiveStage, NoAssets, PathId,
        PropertyPath, StageOptions, TargetPath, Time, Transaction, Value,
    };
    use layerstack_schemas::Scene;

    use super::{LinkKind, LinkTarget};
    use crate::{DrawDiff, DrawList, DrawOptions, LightId};

    const SCENE: &str =
        include_str!("../../layerstack_conformance/fixtures/stagehand_links/scene.usda");

    struct Fixture {
        store: InMemoryStore,
        live: LiveStage,
        list: DrawList,
    }

    impl Fixture {
        fn new() -> (Self, DrawDiff) {
            let mut store = InMemoryStore::default();
            let read = layerstack_usda::read_usda(
                SCENE,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
            );
            assert!(!read.has_errors());
            store.insert_layer(read.emitted.layer);
            let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
            let live = LiveStage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    schemas: Some(schemas),
                    ..StageOptions::default()
                },
            );
            let mut list = DrawList::new(DrawOptions {
                time: Time::at(0.0),
                ..DrawOptions::default()
            });
            let diff = list.refresh(&Scene::new(live.stage(), &store));
            (Self { store, live, list }, diff)
        }

        fn apply(&mut self, edit: &Transaction) -> DrawDiff {
            let applied = self.live.apply(&mut self.store, edit).unwrap();
            let scene = Scene::new(self.live.stage(), &self.store);
            self.list.apply_changes(&scene, &applied.changes)
        }

        fn path(&self, text: &str) -> PathId {
            self.store.path(text)
        }

        fn property(&self, prim: &str, name: &str) -> layerstack::Address {
            let path = PropertyPath::new(self.path(prim), self.store.tokens().intern(name));
            EditTarget::for_layer(LayerId(1)).property(path)
        }

        /// The target of the item drawing `path`, matched at `path`.
        fn target(&self, path: &str) -> LinkTarget {
            let path = self.path(path);
            let item = self
                .list
                .items()
                .find(|item| item.link_paths.contains(&path))
                .unwrap_or_else(|| panic!("no item is matched at {path:?}"));
            LinkTarget {
                item: item.id,
                path,
            }
        }

        /// The paths the light at `light` links by `kind`, sorted.
        fn linked(&self, light: &str, kind: LinkKind) -> Vec<String> {
            let light = LightId::from(self.path(light));
            let mut paths: Vec<String> = self
                .list
                .items()
                .flat_map(|item| {
                    item.link_paths.iter().map(|&path| LinkTarget {
                        item: item.id,
                        path,
                    })
                })
                .filter(|&target| self.list.is_linked(light, kind, target))
                .map(|target| {
                    self.store
                        .paths()
                        .display(target.path, self.store.tokens())
                        .to_string()
                })
                .collect();
            paths.sort();
            paths
        }
    }

    /// Every (light, link, target) of `stagehand_links/scene.usda` against
    /// what usd-core answers: the targets the draw list matches (gprims,
    /// instance proxies, the point instancer), `IsPathIncluded` for each,
    /// trivial links without a category, and equal queries sharing one.
    #[test]
    fn links_match_openusd() {
        use serde_json::Value as Json;
        let oracle: Json = serde_json::from_str(include_str!(
            "../../layerstack_conformance/fixtures/stagehand_links/oracle.json"
        ))
        .unwrap();
        assert_eq!(
            oracle["version"].as_str(),
            Some(layerstack_schemas::OPENUSD_VERSION)
        );
        let (fixture, _) = Fixture::new();
        let list = &fixture.list;
        let mut targets: BTreeMap<String, LinkTarget> = BTreeMap::new();
        for item in list.items() {
            for &path in &item.link_paths {
                let text = fixture
                    .store
                    .paths()
                    .display(path, fixture.store.tokens())
                    .to_string();
                assert!(
                    targets
                        .insert(
                            text,
                            LinkTarget {
                                item: item.id,
                                path
                            }
                        )
                        .is_none(),
                    "one target per path"
                );
            }
        }
        let expected: BTreeSet<&str> = oracle["targets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|target| target["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            targets.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            expected
        );
        let links = oracle["links"].as_array().unwrap();
        let mut categories = Vec::new();
        for link in links {
            let light = LightId::from(fixture.path(link["light"].as_str().unwrap()));
            let item = list.light(light).expect("every light is listed");
            let (kind, collection) = match link["link"].as_str().unwrap() {
                "lightLink" => (LinkKind::Light, &item.light_link),
                _ => (LinkKind::Shadow, &item.shadow_link),
            };
            let label = (link["light"].as_str().unwrap(), kind);
            assert_eq!(
                collection.category.is_none(),
                link["trivial"].as_bool().unwrap(),
                "{label:?}"
            );
            let included: BTreeSet<&str> = link["included"]
                .as_array()
                .unwrap()
                .iter()
                .map(|path| path.as_str().unwrap())
                .collect();
            for (path, &target) in &targets {
                assert_eq!(
                    list.is_linked(light, kind, target),
                    included.contains(path.as_str()),
                    "{label:?} {path}"
                );
                if let Some(category) = collection.category {
                    assert_eq!(
                        list.link_categories(target).contains(&category),
                        included.contains(path.as_str())
                    );
                }
            }
            categories.push(collection.category);
        }
        // Links share a category exactly when OpenUSD's queries are equal.
        for (i, link) in links.iter().enumerate() {
            for (j, other) in links.iter().enumerate() {
                assert_eq!(
                    categories[i] == categories[j],
                    link["shares"] == other["shares"],
                    "{} {} / {} {}",
                    link["light"],
                    link["link"],
                    other["light"],
                    other["link"]
                );
            }
        }
        // One set per distinct nontrivial query: the twins share theirs.
        let distinct: BTreeSet<u64> = links
            .iter()
            .filter(|link| !link["trivial"].as_bool().unwrap())
            .map(|link| link["shares"].as_u64().unwrap())
            .collect();
        assert_eq!(list.link_sets().count(), distinct.len());
    }

    #[test]
    fn a_link_edit_evaluates_only_that_link() {
        let (mut fixture, refresh) = Fixture::new();
        let targets = fixture
            .list
            .items()
            .map(|item| item.link_paths.len())
            .sum::<usize>();
        let lights = fixture.list.lights().count();
        assert_eq!(refresh.work.links_evaluated as usize, 2 * lights);
        assert!(
            refresh.links_changed.is_empty(),
            "new lights are not listed"
        );
        let spot = LightId::from(fixture.path("/World/Lights/Spot"));
        let twin = LightId::from(fixture.path("/World/Lights/Twin"));
        let shared = fixture.list.light(spot).unwrap().light_link.category;
        assert!(shared.is_some());
        assert_eq!(
            fixture.list.light(twin).unwrap().light_link.category,
            shared
        );

        // The spot links the rocks too: a membership of its own.
        let mut edit = Transaction::new();
        let rocks = TargetPath::Prim(fixture.path("/World/Rocks"));
        edit.add_target(
            fixture.property("/World/Lights/Spot", "collection:lightLink:includes"),
            rocks,
            ListPosition::BackOfPrepend,
        );
        let diff = fixture.apply(&edit);
        assert_eq!(diff.work.links_evaluated, 1);
        assert_eq!(diff.work.links_classified as usize, targets);
        assert_eq!(diff.work.items_read, 0);
        assert_eq!(diff.lights_changed, [spot]);
        assert_eq!(diff.links_changed.len(), 1);
        let change = &diff.links_changed[0];
        assert_eq!((change.light, change.kind), (spot, LinkKind::Light));
        assert_eq!(
            change.added,
            [
                fixture.target("/World/Rocks/Rock"),
                fixture.target("/World/Rocks/Pebble")
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
        );
        assert!(change.removed.is_empty());
        let own = fixture.list.light(spot).unwrap().light_link.category;
        assert!(own.is_some() && own != shared);
        assert_eq!(
            fixture.list.light(twin).unwrap().light_link.category,
            shared
        );
        assert_eq!(
            fixture.linked("/World/Lights/Spot", LinkKind::Light),
            [
                "/World/Robot/Body",
                "/World/Robot/Head",
                "/World/Rocks/Pebble",
                "/World/Rocks/Rock"
            ]
        );

        // Back to the twin's membership: it joins the twin's category,
        // classifying nothing.
        let mut edit = Transaction::new();
        edit.remove_target(
            fixture.property("/World/Lights/Spot", "collection:lightLink:includes"),
            rocks,
        );
        let diff = fixture.apply(&edit);
        assert_eq!(diff.work.links_evaluated, 1);
        assert_eq!(diff.work.links_classified, 0);
        assert_eq!(
            fixture.list.light(spot).unwrap().light_link.category,
            shared
        );
        assert_eq!(diff.links_changed[0].removed.len(), 2);
        assert!(
            fixture.list.link_set(own.unwrap()).is_none(),
            "unused, dropped"
        );

        // A shadow link edit evaluates the shadow link alone; linking
        // nothing but the robot from everything removes every other target.
        let mut edit = Transaction::new();
        edit.set_default(
            fixture.property("/World/Lights/Spot", "collection:shadowLink:includeRoot"),
            Value::Bool(false),
        );
        let diff = fixture.apply(&edit);
        assert_eq!(diff.work.links_evaluated, 1);
        assert_eq!(diff.links_changed.len(), 1);
        assert_eq!(diff.links_changed[0].kind, LinkKind::Shadow);
        assert_eq!(diff.links_changed[0].removed.len(), targets);
        assert!(
            fixture
                .linked("/World/Lights/Spot", LinkKind::Shadow)
                .is_empty()
        );
    }

    #[test]
    fn a_collection_another_includes_relinks_its_users() {
        let (mut fixture, _) = Fixture::new();
        let collected = LightId::from(fixture.path("/World/Lights/Collected"));
        let mut edit = Transaction::new();
        edit.set_targets(
            fixture.property("/World", "collection:robots:includes"),
            layerstack::ListOp::explicit(alloc::vec![TargetPath::Prim(
                fixture.path("/World/Rocks")
            )]),
        );
        let diff = fixture.apply(&edit);
        // Only the light including `/World.collection:robots` is evaluated.
        assert_eq!(diff.work.links_evaluated, 1);
        assert_eq!(diff.links_changed.len(), 1);
        assert_eq!(diff.links_changed[0].light, collected);
        assert_eq!(diff.links_changed[0].added.len(), 2);
        assert_eq!(diff.links_changed[0].removed.len(), 2);
        assert_eq!(
            fixture.linked("/World/Lights/Collected", LinkKind::Light),
            ["/World/Rocks/Pebble", "/World/Rocks/Rock"]
        );
    }

    #[test]
    fn a_new_item_is_classified_against_each_category_once() {
        let (mut fixture, _) = Fixture::new();
        let categories = fixture.list.link_sets().count();
        let arm = fixture.path("/World/Robot/Arm");
        let mut edit = layerstack_schemas::SchemaEdit::new(
            fixture.live.stage(),
            &mut fixture.store,
            EditTarget::for_layer(LayerId(1)),
        );
        edit.define_prim(arm, "Cube").unwrap();
        let transaction = edit.finish();
        let diff = fixture.apply(&transaction);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.work.links_evaluated, 0);
        assert_eq!(diff.work.links_classified as usize, categories);
        assert!(
            diff.links_changed.is_empty(),
            "a new item's links are read from the list"
        );
        let target = fixture.target("/World/Robot/Arm");
        let spot = LightId::from(fixture.path("/World/Lights/Spot"));
        let rocky = LightId::from(fixture.path("/World/Lights/Rocky"));
        let everything = LightId::from(fixture.path("/World/Lights/Everything"));
        assert!(fixture.list.is_linked(spot, LinkKind::Light, target));
        assert!(!fixture.list.is_linked(rocky, LinkKind::Light, target));
        assert!(fixture.list.is_linked(everything, LinkKind::Light, target));
        assert!(fixture.list.is_linked(spot, LinkKind::Shadow, target));

        // Removed again: it leaves every category, listing nothing.
        let mut edit = Transaction::new();
        edit.remove_spec(EditTarget::for_layer(LayerId(1)).prim(arm));
        let diff = fixture.apply(&edit);
        assert_eq!(diff.removed.len(), 1);
        assert!(diff.links_changed.is_empty());
        assert_eq!(diff.work.links_classified, 0);
        assert!(!fixture.list.is_linked(spot, LinkKind::Light, target));
        assert!(
            fixture
                .list
                .link_sets()
                .all(|set| set.members.iter().all(|member| member.path != arm))
        );
    }

    #[test]
    fn links_do_not_vary_with_time() {
        let (mut fixture, _) = Fixture::new();
        let before: Vec<_> = fixture.list.link_sets().cloned().collect();
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        // The rock is invisible at 10: hidden, still linked.
        let diff = fixture.list.set_time(&scene, Time::at(10.0));
        assert!(!diff.visibility.is_empty());
        assert_eq!(diff.work.links_evaluated + diff.work.links_classified, 0);
        assert!(diff.links_changed.is_empty());
        let after: Vec<_> = fixture.list.link_sets().cloned().collect();
        assert_eq!(before, after);
    }
}
