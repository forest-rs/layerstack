// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The reverse of collection membership: which collections on a stage
//! include a prim or property.
//!
//! OpenUSD answers only the forward question
//! (`UsdCollectionAPI::ComputeMembershipQuery`, then
//! `UsdCollectionMembershipQuery::IsPathIncluded`). [`MembershipIndex`]
//! compiles every collection's [`MembershipQuery`] once and inverts the
//! rule maps, so the reverse question costs one lookup per ancestor of the
//! path rather than one query per collection. Its answer for a path is, by
//! definition, the collections whose [`MembershipQuery::is_included`]
//! includes it, with the same [`Membership`].
//!
//! Spec: AOUSD Core §15 (collections); OpenUSD's `UsdCollectionAPI`.

use alloc::{string::String, vec::Vec};

use hashbrown::{HashMap, HashSet};
use layerstack::{Changes, PathId, TargetPath};

use super::{
    CollectionIdentity, Membership, MembershipProblem, MembershipQuery, MembershipRule,
    collection_name, decide, rule_parent,
};
use crate::Scene;
use crate::usd::CollectionApi;

/// A collection that includes a path, and how it includes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inclusion<'a> {
    /// The including collection: its owning prim and instance name.
    pub collection: &'a CollectionIdentity,
    /// How it includes the path, as [`MembershipQuery::is_included`]
    /// reports it: never [`Membership::Excluded`].
    pub membership: Membership,
    /// The rule-map entry that decided it: the path itself, or the
    /// nearest ancestor the collection names. A collection with
    /// `includeRoot` decides every path no nearer entry does at the
    /// pseudo-root, so this names the pseudo-root for those (light-linking
    /// collections often have it). `None` for a collection decided by its
    /// `membershipExpression`.
    ///
    /// To ask only about collections that name the path or one of its
    /// ancestors, skip inclusions decided at the pseudo-root; to ask only
    /// about collections owned within a subtree, test
    /// [`CollectionIdentity::owner`].
    pub decided_at: Option<TargetPath>,
}

/// What [`MembershipIndex::apply_changes`] did, by collection, each list in
/// namespace order of the owning prim, then name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MembershipIndexUpdate {
    /// Collections found on prims the changes created or resynced.
    pub added: Vec<CollectionIdentity>,
    /// Collections whose prim was removed, or which it no longer applies.
    pub removed: Vec<CollectionIdentity>,
    /// Surviving collections whose query was compiled again.
    pub recompiled: Vec<CollectionIdentity>,
    /// Of [`Self::recompiled`], those whose query differs from before:
    /// their membership may have changed.
    pub changed: Vec<CollectionIdentity>,
}

impl MembershipIndexUpdate {
    /// Whether the changes left the index as it was.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// The size of a [`MembershipIndex`] and the work it has done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MembershipIndexStats {
    /// Collections indexed.
    pub collections: usize,
    /// Of those, decided by their `membershipExpression`: evaluated for
    /// each query rather than inverted.
    pub by_expression: usize,
    /// Distinct paths with a rule-map entry.
    pub rule_paths: usize,
    /// Rule-map entries, over all collections.
    pub rule_entries: usize,
    /// Queries compiled since [`MembershipIndex::build`], the build's own
    /// included.
    pub compiled: u64,
}

#[derive(Debug)]
struct Indexed {
    identity: CollectionIdentity,
    query: MembershipQuery,
    /// Other collections whose fields or existence the query read: those
    /// it includes, its expression references, and those it failed to.
    reads: Vec<CollectionIdentity>,
    /// An expression reference names no collection: one made later could
    /// be it, so every collection edit may change this one.
    reads_any: bool,
    /// Its place in namespace order, owner then name.
    rank: u32,
}

/// Every collection on a stage, compiled once and inverted: which
/// collections include a prim or property.
///
/// [`Self::build`] compiles every `CollectionAPI` instance of every prim
/// on the stage (`UsdStage::TraverseAll`, abstract and undefined prims
/// included) with [`MembershipQuery::compute`].
/// [`Self::collections_including`] then answers for any prim or property
/// path, exactly as asking each collection's
/// [`MembershipQuery::is_included`] would, but by walking the path's
/// ancestors through the inverted rule maps. A collection decided by its
/// `membershipExpression` cannot be inverted: each query evaluates it
/// ([`MembershipIndexStats::by_expression`] counts them).
///
/// The index holds compiled queries, not the scene: pass the scene it was
/// built from, or that scene after the edits given to
/// [`Self::apply_changes`].
///
/// ```
/// use std::sync::Arc;
///
/// use layerstack::edit::EditTarget;
/// use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions, TargetPath};
/// use layerstack_schemas::usd::{CollectionApi, CollectionApiEdit};
/// use layerstack_schemas::{MembershipIndex, Scene, SchemaEdit};
///
/// let mut store = InMemoryStore::default();
/// store.insert_layer(Layer::new(LayerId(1)));
/// let options = StageOptions {
///     schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
///     ..StageOptions::default()
/// };
/// let mut live = LiveStage::compose(&mut store, LayerId(1), options);
/// let parts = store.path("/Parts");
/// let stones = store.path("/Parts/Stones");
/// let flint = store.path("/Parts/Stones/Flint");
/// let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
/// for prim in [parts, stones, flint] {
///     edit.define_prim(prim, "Scope").expect("outside any instance");
/// }
/// CollectionApi::apply(&mut edit, parts, "stones")
///     .expect("applies")
///     .set_includes(&mut edit, &[TargetPath::Prim(stones)]);
/// CollectionApi::apply(&mut edit, parts, "mossy")
///     .expect("applies")
///     .set_includes(&mut edit, &[TargetPath::Prim(flint)]);
/// let transaction = edit.finish();
/// live.apply(&mut store, &transaction).expect("applies");
///
/// let scene = Scene::new(live.stage(), &store);
/// let mut index = MembershipIndex::build(&scene);
/// let names: Vec<&str> = index
///     .collections_including(&scene, TargetPath::Prim(flint))
///     .iter()
///     .map(|inclusion| inclusion.collection.name.as_str())
///     .collect();
/// assert_eq!(names, ["mossy", "stones"]);
/// assert!(index.collections_including(&scene, TargetPath::Prim(parts)).is_empty());
///
/// // `stones` excludes the flint: the index follows the edit's report.
/// let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
/// CollectionApiEdit::new(&edit, parts, "stones")
///     .expect("outside any instance")
///     .expect("applied")
///     .set_excludes(&mut edit, &[TargetPath::Prim(flint)]);
/// let transaction = edit.finish();
/// let applied = live.apply(&mut store, &transaction).expect("applies");
/// let scene = Scene::new(live.stage(), &store);
/// let update = index.apply_changes(&scene, &applied.changes);
/// assert_eq!(update.changed.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["stones"]);
/// let names: Vec<&str> = index
///     .collections_including(&scene, TargetPath::Prim(flint))
///     .iter()
///     .map(|inclusion| inclusion.collection.name.as_str())
///     .collect();
/// assert_eq!(names, ["mossy"]);
/// ```
///
/// Spec: AOUSD Core §15 (collections); OpenUSD's
/// `UsdCollectionAPI::GetAllCollections` and `ComputeMembershipQuery`.
#[derive(Debug, Default)]
pub struct MembershipIndex {
    slots: Vec<Option<Indexed>>,
    free: Vec<u32>,
    by_identity: HashMap<CollectionIdentity, u32>,
    /// Each rule-map path, with the collections that have an entry there.
    rules: HashMap<TargetPath, Vec<(u32, MembershipRule)>>,
    /// Collections decided by their expression.
    expressions: Vec<u32>,
    compiled: u64,
}

/// The base names of a collection's own properties: an edit to any field
/// of `collection:<name>:<base>` can change its membership.
const COLLECTION_FIELDS: [&str; 6] = [
    "includes",
    "excludes",
    "expansionRule",
    "includeRoot",
    "membershipExpression",
    "mode",
];

/// The collection whose own property `property` is
/// (`collection:a:b:includes` is collection `a:b`'s).
fn field_owner(property: &str) -> Option<&str> {
    let (name, base) = property.strip_prefix("collection:")?.rsplit_once(':')?;
    (!name.is_empty() && COLLECTION_FIELDS.contains(&base)).then_some(name)
}

impl MembershipIndex {
    /// Compiles every collection on the stage of `scene`.
    #[must_use]
    pub fn build(scene: &Scene<'_>) -> Self {
        let mut index = Self::default();
        if let Some(root) = scene.root() {
            for prim in scene.stage().traverse_all(root) {
                for name in scene.instances(prim, CollectionApi::SCHEMA) {
                    index.insert(
                        scene,
                        CollectionIdentity {
                            owner: prim,
                            name: name.into(),
                        },
                    );
                }
            }
        }
        index.rank(scene);
        index
    }

    /// The collections that include `path`, a prim or property, with how
    /// each includes it, in namespace order of the owning prim, then name.
    ///
    /// Each rule-map collection is decided by its nearest entry on `path`
    /// or an ancestor, as [`MembershipQuery::is_included`] decides it; the
    /// pseudo-root is in no rule map. Each expression-decided collection
    /// matches `path` against `scene`.
    ///
    /// A path no collection includes, and the pseudo-root, give an empty
    /// list, not an error. Rule maps decide by path, as the forward query
    /// does, so a path beneath an included prim is reported whether or not
    /// the stage holds a prim there.
    #[must_use]
    pub fn collections_including(&self, scene: &Scene<'_>, path: TargetPath) -> Vec<Inclusion<'_>> {
        let mut found: Vec<(u32, Inclusion<'_>)> = Vec::new();
        let pseudo_root = matches!(path, TargetPath::Prim(prim) if scene.parent(prim).is_none());
        if !pseudo_root {
            let mut decided: Vec<u32> = Vec::new();
            let mut at = Some(path);
            while let Some(current) = at {
                for &(slot, rule) in self.rules.get(&current).into_iter().flatten() {
                    if decided.contains(&slot) {
                        continue;
                    }
                    if let Some(membership) = decide(rule, current, path) {
                        decided.push(slot);
                        if membership.is_included() {
                            found.push(self.inclusion(slot, membership, Some(current)));
                        }
                    }
                }
                at = rule_parent(scene, current);
            }
        }
        for &slot in &self.expressions {
            let membership = self.entry(slot).query.is_included(scene, path);
            if membership.is_included() {
                found.push(self.inclusion(slot, membership, None));
            }
        }
        found.sort_unstable_by_key(|(rank, _)| *rank);
        found.into_iter().map(|(_, inclusion)| inclusion).collect()
    }

    /// Every indexed collection with its compiled query, in namespace order
    /// of the owning prim, then name.
    pub fn collections(&self) -> impl Iterator<Item = (&CollectionIdentity, &MembershipQuery)> {
        let mut live: Vec<&Indexed> = self.slots.iter().flatten().collect();
        live.sort_unstable_by_key(|entry| entry.rank);
        live.into_iter()
            .map(|entry| (&entry.identity, &entry.query))
    }

    /// The compiled query of the collection `name` on `owner`, if indexed.
    #[must_use]
    pub fn query(&self, owner: PathId, name: &str) -> Option<&MembershipQuery> {
        let key = CollectionIdentity {
            owner,
            name: name.into(),
        };
        self.by_identity
            .get(&key)
            .map(|slot| &self.entry(*slot).query)
    }

    /// The index's size and compile work.
    #[must_use]
    pub fn stats(&self) -> MembershipIndexStats {
        MembershipIndexStats {
            collections: self.by_identity.len(),
            by_expression: self.expressions.len(),
            rule_paths: self.rules.len(),
            rule_entries: self.rules.values().map(Vec::len).sum(),
            compiled: self.compiled,
        }
    }

    /// Brings the index up to date with one complete report of composed
    /// changes (`Applied::changes`, a `ChangeNotice`'s changes, or each
    /// report of `LiveStage::changes_since` in order), `scene` being the
    /// stage after them. Afterwards it answers as a fresh [`Self::build`]
    /// of `scene` would.
    ///
    /// What each part of the report invalidates:
    ///
    /// - a prim in `removed`, `created` or `changed_info_only` without a
    ///   property inventory, and every prim at or below a `resynced` root:
    ///   its collections are found again (added, removed, or compiled
    ///   again), and so is every collection that read one of its
    ///   collections (included it, referenced it from an expression, or
    ///   failed to);
    /// - a field of `collection:<name>:<base>` in a property inventory, for
    ///   `<base>` one of `includes`, `excludes`, `expansionRule`,
    ///   `includeRoot`, `membershipExpression` and `mode`: that collection
    ///   is compiled again, and so is every collection that read it;
    /// - a collection whose expression references a collection that does
    ///   not exist is compiled again after any of the above, since the
    ///   edit may have made it.
    ///
    /// Nothing else invalidates: other attributes and relationships, prim
    /// metadata in an inventory, and namespace changes among members leave
    /// every compiled query as it was. Rule maps are paths, and a query
    /// walks the scene it is given, so prims created below an included
    /// prim are members at once; expressions are evaluated per query
    /// against that scene, so they follow members' names, types, kinds and
    /// existence without recompiling.
    ///
    /// A collection is compiled whole: there is no partial update of one
    /// rule map, nor of an expression.
    ///
    /// Every report must reach the index, in the order the stage applied
    /// them; one report covering several edits, such as
    /// `LiveStage::changes_since` gives, counts as all of them. The index
    /// cannot tell that a report was skipped: it then keeps answering with
    /// the collections as they were before the skipped edits, until a
    /// later report happens to recompile them. To catch up after missing
    /// reports, apply `LiveStage::changes_since` from a cursor taken when
    /// the index was last current, or [`Self::build`] it again.
    ///
    /// OpenUSD: `UsdNotice::ObjectsChanged`'s resynced and changed-info-only
    /// paths.
    pub fn apply_changes(&mut self, scene: &Scene<'_>, changes: &Changes) -> MembershipIndexUpdate {
        let stage = scene.stage();
        let tokens = scene.store().tokens();
        let subtrees: HashSet<PathId> = changes.resynced.iter().copied().collect();
        let exact: HashSet<PathId> = changes
            .created
            .iter()
            .chain(&changes.removed)
            .copied()
            .chain(
                changes
                    .changed_info_only
                    .iter()
                    .copied()
                    .filter(|prim| changes.properties_for(*prim).is_none()),
            )
            .collect();
        let touched = |prim: PathId| {
            exact.contains(&prim) || {
                let mut at = Some(prim);
                let mut hit = false;
                while let Some(p) = at {
                    if subtrees.contains(&p) {
                        hit = true;
                        break;
                    }
                    at = scene.parent(p);
                }
                hit
            }
        };
        let mut edited: Vec<CollectionIdentity> = Vec::new();
        for inventory in &changes.property_changes {
            for field in &inventory.fields {
                if let Some(name) = field_owner(tokens.resolve(field.name)) {
                    edited.push(CollectionIdentity {
                        owner: inventory.prim,
                        name: name.into(),
                    });
                }
            }
        }
        edited.dedup();
        let anything = !subtrees.is_empty() || !exact.is_empty() || !edited.is_empty();

        // The collections now on the touched prims.
        let mut found: Vec<CollectionIdentity> = Vec::new();
        let mut visited: HashSet<PathId> = HashSet::new();
        let mut discover = |prim: PathId, found: &mut Vec<CollectionIdentity>| {
            if visited.insert(prim) {
                for name in scene.instances(prim, CollectionApi::SCHEMA) {
                    found.push(CollectionIdentity {
                        owner: prim,
                        name: name.into(),
                    });
                }
            }
        };
        for root in &subtrees {
            if scene.prim(*root).is_some() {
                for prim in stage.traverse_all(*root) {
                    discover(prim, &mut found);
                }
            }
        }
        for prim in &exact {
            if scene.prim(*prim).is_some() {
                discover(*prim, &mut found);
            }
        }
        let found: HashSet<CollectionIdentity> = found.into_iter().collect();

        let mut update = MembershipIndexUpdate::default();
        let mut dirty: Vec<u32> = Vec::new();
        let mut gone: Vec<u32> = Vec::new();
        for (slot, entry) in self.slots.iter().enumerate() {
            let Some(entry) = entry else { continue };
            let slot = u32::try_from(slot).expect("collection slots fit in u32");
            if touched(entry.identity.owner) && !found.contains(&entry.identity) {
                gone.push(slot);
            } else if touched(entry.identity.owner)
                || edited.contains(&entry.identity)
                || entry
                    .reads
                    .iter()
                    .any(|read| touched(read.owner) || edited.contains(read))
                || (entry.reads_any && anything)
            {
                dirty.push(slot);
            }
        }
        for slot in gone {
            update.removed.push(self.remove(slot).identity);
        }
        for slot in dirty {
            let old = self.remove(slot);
            let new = self.insert(scene, old.identity.clone());
            if let Some(entry) = self.slots[new as usize].as_mut() {
                entry.rank = old.rank;
            }
            if self.entry(new).query != old.query {
                update.changed.push(old.identity.clone());
            }
            update.recompiled.push(old.identity);
        }
        for identity in found {
            if !self.by_identity.contains_key(&identity) {
                self.insert(scene, identity.clone());
                update.added.push(identity);
            }
        }
        if !update.added.is_empty() || !update.removed.is_empty() {
            self.rank(scene);
        }
        let store = scene.store();
        for list in [
            &mut update.added,
            &mut update.removed,
            &mut update.recompiled,
            &mut update.changed,
        ] {
            list.sort_by(|a, b| {
                store
                    .paths()
                    .cmp_paths(a.owner, b.owner, store.tokens())
                    .then_with(|| a.name.cmp(&b.name))
            });
        }
        update
    }

    fn entry(&self, slot: u32) -> &Indexed {
        self.slots[slot as usize]
            .as_ref()
            .expect("a live collection slot")
    }

    fn inclusion(
        &self,
        slot: u32,
        membership: Membership,
        decided_at: Option<TargetPath>,
    ) -> (u32, Inclusion<'_>) {
        let entry = self.entry(slot);
        (
            entry.rank,
            Inclusion {
                collection: &entry.identity,
                membership,
                decided_at,
            },
        )
    }

    /// Compiles and indexes `identity`, returning its slot. It is ranked
    /// last until [`Self::rank`] runs.
    fn insert(&mut self, scene: &Scene<'_>, identity: CollectionIdentity) -> u32 {
        let query = MembershipQuery::compute(scene, identity.owner, &identity.name);
        self.compiled += 1;
        let tokens = scene.store().tokens();
        let mut reads: Vec<CollectionIdentity> = Vec::new();
        let mut read = |path: layerstack::PropertyPath| {
            if let Some(name) = collection_name(tokens.resolve(path.property())) {
                let read = CollectionIdentity {
                    owner: path.prim_path(),
                    name: String::from(name),
                };
                if read != identity && !reads.contains(&read) {
                    reads.push(read);
                }
            }
        };
        for path in query
            .included_collections()
            .iter()
            .chain(query.referenced_collections())
        {
            read(*path);
        }
        let mut reads_any = false;
        for problem in query.problems() {
            match problem {
                MembershipProblem::CircularInclusion { collection }
                | MembershipProblem::MissingPrim { collection }
                | MembershipProblem::InvalidExpression { collection, .. }
                | MembershipProblem::ReferenceCycle { collection, .. } => read(*collection),
                MembershipProblem::MissingReference { from, .. } => {
                    read(*from);
                    reads_any = true;
                }
                MembershipProblem::UnevaluableExpression { .. } => {}
            }
        }
        let slot = match self.free.pop() {
            Some(slot) => slot,
            None => {
                self.slots.push(None);
                u32::try_from(self.slots.len() - 1).expect("collection slots fit in u32")
            }
        };
        for (path, rule) in query.rules() {
            self.rules.entry(*path).or_default().push((slot, *rule));
        }
        if !query.uses_rule_map() && query.evaluator().is_some_and(|e| !e.is_empty()) {
            self.expressions.push(slot);
        }
        self.by_identity.insert(identity.clone(), slot);
        self.slots[slot as usize] = Some(Indexed {
            identity,
            query,
            reads,
            reads_any,
            rank: u32::MAX,
        });
        slot
    }

    /// Unindexes the collection in `slot`, and frees the slot.
    fn remove(&mut self, slot: u32) -> Indexed {
        let entry = self.slots[slot as usize]
            .take()
            .expect("a live collection slot");
        for (path, _) in entry.query.rules() {
            if let Some(entries) = self.rules.get_mut(path) {
                entries.retain(|(s, _)| *s != slot);
                if entries.is_empty() {
                    self.rules.remove(path);
                }
            }
        }
        self.expressions.retain(|s| *s != slot);
        self.by_identity.remove(&entry.identity);
        self.free.push(slot);
        entry
    }

    /// Ranks every collection in namespace order of its owner, then name.
    fn rank(&mut self, scene: &Scene<'_>) {
        let store = scene.store();
        let mut live: Vec<(u32, &CollectionIdentity)> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(slot, entry)| {
                entry
                    .as_ref()
                    .map(|e| (u32::try_from(slot).expect("fits"), &e.identity))
            })
            .collect();
        live.sort_by(|(_, a), (_, b)| {
            store
                .paths()
                .cmp_paths(a.owner, b.owner, store.tokens())
                .then_with(|| a.name.cmp(&b.name))
        });
        let order: Vec<u32> = live.into_iter().map(|(slot, _)| slot).collect();
        for (rank, slot) in order.into_iter().enumerate() {
            if let Some(entry) = self.slots[slot as usize].as_mut() {
                entry.rank = u32::try_from(rank).expect("fits");
            }
        }
    }
}
