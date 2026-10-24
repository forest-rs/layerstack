// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authored relationship targets and attribute connections, layer by
//! layer, as the stage reads them: each path a property spec authors, the
//! composed prims that read the spec, and the stage path each maps to.
//!
//! [`Stage::dangling_targets`] checks the composed lists, the stage's own
//! namespace; this walk starts from the specs instead, so a finding names
//! the layer and spec to edit. A path authored in a referenced layer is in
//! that layer's namespace: composition maps it across the arcs that bring
//! the spec in, once for each composed prim that reads it, and the mapped
//! path is the one `UsdRelationship::GetTargets` returns.
//!
//! Spec: AOUSD Core §10.3.2 (an arc maps its target's namespace), §10.6
//! (target paths an arc cannot map), §12.4 (list op resolution: an
//! explicit list replaces weaker opinions).

use alloc::vec::Vec;

use hashbrown::{HashMap, HashSet};

use super::{DanglingKind, DropReason, Stage};
use crate::spec_path::VariantSelectionSite;
use crate::{
    ArcKind, CompositionError, LayerId, LayerStore, ListOp, NodeId, PathId, PropertyKind,
    PropertyPath, PropertySpec, SpecPath, TargetPath, TokenId,
};

/// Which inserting list of a target list op authors a path.
///
/// Deleted and reordered paths are not among them: they remove or order
/// what weaker opinions insert, and name nothing the stage reads. OpenUSD
/// lets a deleted path name nothing (`_PathTranslateCallback` in
/// `pxr/usd/pcp/targetIndex.cpp`), and a stronger layer deletes a weaker
/// layer's dangling path exactly so.
///
/// Spec: AOUSD Core §12.4 (list ops).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TargetListPosition {
    /// The explicit list (`rel shade = [</A>]`).
    Explicit,
    /// The prepended items (`prepend rel shade = </A>`).
    Prepended,
    /// The appended items (`append rel shade = </A>`).
    Appended,
    /// The added items (`add rel shade = </A>`), the legacy `add`.
    Added,
}

impl TargetListPosition {
    /// The position as text: `explicit`, `prepended`, `appended` or
    /// `added`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Prepended => "prepended",
            Self::Appended => "appended",
            Self::Added => "added",
        }
    }
}

/// How one composed prim reads an authored path (see [`AuthoredTarget`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetRead {
    /// Composition maps the path to this stage path, which the composed
    /// list holds unless a stronger opinion deletes it. Check it with
    /// [`Stage::dangling_kind`]. Through the stage's own layer stack the
    /// path is the authored one.
    Mapped(TargetPath),
    /// Composition drops the path, and reports it in
    /// [`Stage::composition_errors`]: it is outside the namespace the arc
    /// maps, or names an instance of the class that authors it.
    Dropped(DropReason),
    /// A stronger opinion of the property authors an explicit list, which
    /// replaces this one: the stage reads none of its paths, and they are
    /// neither mapped nor checked. OpenUSD clears their target path errors
    /// (`PcpBuildFilteredTargetIndex`).
    Replaced,
}

/// One authored relationship target or attribute connection, as one
/// composed prim reads its spec.
///
/// A spec read by several prims (a referenced layer used twice, a class
/// several prims inherit, the prims beneath every instance of an asset)
/// gives one entry per prim, each with its own mapped path.
///
/// To fix one, edit the spec: `layer` and `spec` name it,
/// [`Address::spec`](crate::edit::Address::spec) addresses it, and
/// `authored` is the path in that layer's namespace, which
/// [`Transaction::remove_target`](crate::edit::Transaction::remove_target)
/// takes out of its list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredTarget {
    /// The layer of the property spec.
    pub layer: LayerId,
    /// The property spec, in the layer's namespace, with any variant
    /// selections: `/Asset{look=worn}/Spot.shade`.
    pub spec: SpecPath,
    /// A relationship's target, or an attribute's connection.
    pub kind: PropertyKind,
    /// The list that authors the path.
    pub position: TargetListPosition,
    /// The path as authored, in the layer's namespace.
    pub authored: TargetPath,
    /// The composed property that reads the spec.
    pub source: PropertyPath,
    /// The node of `source`'s prim graph the spec is read through
    /// ([`Prim::composition_graph`](crate::Prim::composition_graph));
    /// [`EditTarget::for_node_layer`](crate::edit::EditTarget::for_node_layer)
    /// with it edits the spec through the stage's namespace.
    pub node: NodeId,
    /// How `source` reads the path.
    pub read: TargetRead,
}

/// Why no composed prim reads a property spec (see [`UnreadTargetSpec`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnreadReason {
    /// The spec is inside a variant branch the stage does not select where
    /// it reads the variant set's prim from this layer.
    ///
    /// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes).
    UnselectedVariant,
    /// The layer is in the stage's own layer stack, whose namespace is the
    /// stage's, and the stage composes no prim at the spec's prim path:
    /// why, as for a target path ([`Stage::dangling_kind`] of the prim
    /// path), such as [`DanglingKind::Inactive`] beneath an inactive prim.
    ///
    /// Spec: AOUSD Core §11.3 (population).
    PrimNotComposed(DanglingKind),
    /// The layer is in the stage's own layer stack and the spec is beneath
    /// an instance, whose descendants compose from its prototype: the
    /// opinions the stage's layer stack authors beneath an instance are
    /// ignored.
    ///
    /// Spec: AOUSD Core §11.3.3 (instancing).
    BeneathInstance,
    /// No composed prim reaches the spec: no arc brings this part of the
    /// layer in, or only beneath a prim the stage leaves out, or
    /// composition ignores the spec (a relocation source, or a property
    /// whose spec kinds differ, `CompositionError::InconsistentPropertyType`).
    NotReached,
}

/// A property spec with authored targets or connections that no composed
/// prim reads, so none of its paths is checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnreadTargetSpec {
    /// The layer of the property spec.
    pub layer: LayerId,
    /// The property spec, in the layer's namespace.
    pub spec: SpecPath,
    /// A relationship's targets, or an attribute's connections.
    pub kind: PropertyKind,
    /// The paths its explicit, prepended, appended and added lists author.
    pub targets: usize,
    /// Why nothing reads it.
    pub reason: UnreadReason,
}

/// Every relationship target and attribute connection the stage's layers
/// author, as the stage reads them, by layer ([`Stage::authored_targets`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthoredTargets {
    /// Each layer walked: the stage's own layer stack, strongest first,
    /// then the other layers composition used ([`Stage::used_layers`]), by
    /// id. A layer that authors no target is listed, empty.
    pub layers: Vec<LayerTargets>,
}

impl AuthoredTargets {
    /// The walk of `layer`, if the stage composes it.
    #[must_use]
    pub fn layer(&self, layer: LayerId) -> Option<&LayerTargets> {
        self.layers.iter().find(|entry| entry.layer == layer)
    }

    /// Every authored path read, over all layers, in layer order.
    pub fn targets(&self) -> impl Iterator<Item = &AuthoredTarget> {
        self.layers.iter().flat_map(|layer| &layer.targets)
    }
}

/// The relationship targets and attribute connections one layer authors,
/// as the stage reads them (see [`AuthoredTargets`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerTargets {
    /// The layer.
    pub layer: LayerId,
    /// Property specs that author at least one path in an inserting list
    /// ([`TargetListPosition`]), read or not.
    pub specs: usize,
    /// Each authored path of each read spec, once per composed prim that
    /// reads it: by spec, in the order the stage first reads them (the
    /// first prim that reads each in stage order, [`Stage::traverse_all`],
    /// then that prim's properties by name); then by reading prim, in
    /// stage order; then by list, explicit, prepended, appended and added,
    /// each in authored order. The order depends on the scene description
    /// alone, not on the file format or how the layers were built.
    pub targets: Vec<AuthoredTarget>,
    /// The specs nothing reads, in namespace order: by prim path, one
    /// inside a variant branch after the one outside it, then by property
    /// name.
    pub unread: Vec<UnreadTargetSpec>,
}

/// One opinion of a composed property that authors targets.
struct Read<'s> {
    source: PropertyPath,
    node: NodeId,
    /// The opinion's list op, mapped into the stage namespace.
    mapped: &'s ListOp<TargetPath>,
    replaced: bool,
}

/// The opinions read from one spec, most specs being read once, and the
/// rank of the first: the order the stage first reads the spec in.
struct SpecReads<'s> {
    first: u32,
    reads: smallvec::SmallVec<[Read<'s>; 1]>,
}

/// A spec to look up read specs by, borrowed for the lookup alone: it
/// hashes as the map's `(LayerId, &SpecPath)` keys do.
#[derive(Hash)]
struct SpecKey<'a>(LayerId, &'a SpecPath);

impl hashbrown::Equivalent<(LayerId, &SpecPath)> for SpecKey<'_> {
    fn equivalent(&self, key: &(LayerId, &SpecPath)) -> bool {
        self.0 == key.0 && self.1 == key.1
    }
}

/// The paths composition dropped from one opinion: `(path, reason)`, the
/// authored path for a path outside the arc, the mapped one for an
/// instance of the class.
type Drops = Vec<(TargetPath, DropReason)>;

impl Stage {
    /// Walks every relationship target and attribute connection authored
    /// in the layers the stage composes, and reads each the way the stage
    /// does (see [`AuthoredTargets`]). `store` holds the layers the stage
    /// was composed from.
    ///
    /// A property spec counts when its explicit, prepended, appended or
    /// added list authors a path ([`TargetListPosition`]). Each composed
    /// prim that reads the spec, every prim the stage populates
    /// ([`Stage::traverse_all`], instance proxies included), maps the
    /// spec's paths across the arcs that bring it in, as composition
    /// mapped them for its opinion: [`TargetRead::Mapped`] with the stage
    /// path, [`TargetRead::Dropped`] for a path composition dropped, or
    /// [`TargetRead::Replaced`] when a stronger explicit list replaces the
    /// opinion. A spec nothing reads is an [`UnreadTargetSpec`] with why.
    ///
    /// Nothing here decides whether a mapped path names anything: check it
    /// with [`Stage::dangling_kind`], as `layerstack_schemas`'
    /// `LayerDanglingReport` does.
    ///
    /// The work is one pass over the composed properties' opinions, with
    /// each prim's target properties sorted by name, and one over each
    /// layer's prim specs; per read spec, its paths are paired with the
    /// opinion's mapped list. Only the unread specs are sorted by path.
    /// Nothing is cached.
    ///
    /// OpenUSD: the specs of `UsdProperty::GetPropertyStack`, each mapped
    /// to the stage through its node's `PcpNodeRef::GetMapToRoot`, as
    /// `UsdRelationship::GetTargets` maps them (`PcpBuildFilteredTargetIndex`).
    ///
    /// Spec: AOUSD Core §10.3.2, §10.6, §12.4.
    #[must_use]
    pub fn authored_targets(&self, store: &dyn LayerStore) -> AuthoredTargets {
        let reads = self.target_reads(store);
        let drops = self.target_drops();
        let mut layers = self.layer_stack().to_vec();
        for layer in self.used_layers(false) {
            if !layers.contains(&layer) {
                layers.push(layer);
            }
        }
        let mut sites: Option<HashSet<(LayerId, &SpecPath)>> = None;
        let mut out = AuthoredTargets::default();
        for layer_id in layers {
            let mut entry = LayerTargets {
                layer: layer_id,
                specs: 0,
                targets: Vec::new(),
                unread: Vec::new(),
            };
            let Some(layer) = store.layer(layer_id) else {
                out.layers.push(entry);
                continue;
            };
            // The read specs, by first read, and the unread ones.
            let mut read: Vec<(&SpecReads<'_>, SpecPath, &PropertySpec)> = Vec::new();
            let mut unread: Vec<(SpecPath, &PropertySpec)> = Vec::new();
            let mut paths_read = 0;
            for_each_target_spec(layer, store, |spec, property, count| {
                entry.specs += 1;
                match reads.get(&SpecKey(layer_id, &spec)) {
                    Some(reads) => {
                        paths_read += reads.reads.len() * count;
                        read.push((reads, spec, property));
                    }
                    None => unread.push((spec, property)),
                }
            });
            let order = by_first(read.iter().map(|(reads, ..)| reads.first), reads.len());
            entry.targets.reserve_exact(paths_read);
            for at in order {
                let (reads, spec, property) = &read[at];
                let (reads, spec, property) = (*reads, spec, *property);
                let lists = inserted_lists(property.targets.as_ref().expect("a target spec"));
                for read in &reads.reads {
                    let key = (read.source.prim_path(), read.source.property(), layer_id);
                    let dropped = drops.get(&(key.0, key.1, key.2, spec));
                    let dropped = dropped.map_or(&[][..], Vec::as_slice);
                    let mapped = inserted_lists(read.mapped);
                    for ((position, authored), (_, mapped)) in lists.iter().zip(mapped) {
                        let read_as: Vec<TargetRead> = if read.replaced {
                            authored.iter().map(|_| TargetRead::Replaced).collect()
                        } else {
                            // The arc names a drop no error explains, which
                            // composition does not make.
                            let arc = || {
                                self.explain_prim_graph(read.source.prim_path())
                                    .and_then(|graph| graph.node(read.node))
                                    .map_or(ArcKind::Local, |node| node.arc_kind())
                            };
                            pair(authored, mapped, dropped, arc)
                        };
                        for (authored, read_as) in authored.iter().zip(read_as) {
                            entry.targets.push(AuthoredTarget {
                                layer: layer_id,
                                spec: spec.clone(),
                                kind: property.kind,
                                position: *position,
                                authored: *authored,
                                source: read.source,
                                node: read.node,
                                read: read_as,
                            });
                        }
                    }
                }
            }
            sort_specs(&mut unread, store);
            for (spec, property) in unread {
                let sites = sites.get_or_insert_with(|| self.read_sites());
                let reason = self.unread_reason(layer_id, &spec, sites, store);
                entry.unread.push(UnreadTargetSpec {
                    layer: layer_id,
                    spec,
                    kind: property.kind,
                    targets: inserted_count(property),
                    reason,
                });
            }
            out.layers.push(entry);
        }
        out
    }

    /// Every opinion of a composed property that authors targets, by the
    /// layer and spec it was read from: prims in stage order, each
    /// prim's properties by name, each property's opinions strongest
    /// first.
    fn target_reads(&self, store: &dyn LayerStore) -> HashMap<(LayerId, &SpecPath), SpecReads<'_>> {
        use crate::prim_index::FieldKey;

        let tokens = store.tokens();
        let mut reads: HashMap<(LayerId, &SpecPath), SpecReads<'_>> = HashMap::new();
        let Some(root) = store.paths().lookup(&crate::Path::root()) else {
            return reads;
        };
        let mut properties: Vec<(TokenId, &[crate::Opinion])> = Vec::new();
        for prim in self.traverse_all(root) {
            let Some(index) = self.prims.get(&prim) else {
                continue;
            };
            properties.clear();
            for (field, range) in &index.fields {
                let FieldKey::Property(name) = *field else {
                    continue;
                };
                let opinions = index.opinions_in(range);
                if opinions
                    .iter()
                    .any(|opinion| opinion.value.targets().is_some())
                {
                    properties.push((name, opinions));
                }
            }
            if properties.len() > 1 {
                properties.sort_by(|(a, _), (b, _)| {
                    super::dictionary_cmp(tokens.resolve(*a), tokens.resolve(*b))
                });
            }
            for &(name, opinions) in &properties {
                let mut replaced = false;
                for opinion in opinions {
                    let Some(mapped) = opinion.value.as_property().and_then(|s| s.targets.as_ref())
                    else {
                        continue;
                    };
                    let next = u32::try_from(reads.len()).unwrap_or(u32::MAX);
                    reads
                        .entry((opinion.key.layer_id, &opinion.key.spec_path))
                        .or_insert_with(|| SpecReads {
                            first: next,
                            reads: smallvec::SmallVec::new(),
                        })
                        .reads
                        .push(Read {
                            source: PropertyPath::new(prim, name),
                            node: opinion.key.node,
                            mapped,
                            replaced,
                        });
                    replaced |= mapped.explicit.is_some();
                }
            }
        }
        reads
    }

    /// The target paths composition dropped, by the composed property and
    /// the spec that authors them.
    fn target_drops(&self) -> HashMap<(PathId, TokenId, LayerId, &SpecPath), Drops> {
        let mut drops: HashMap<_, Drops> = HashMap::new();
        for error in self.composition_errors() {
            let (key, dropped) = match error {
                CompositionError::InvalidExternalTargetPath(e) => (
                    (e.prim, e.property, e.layer, &e.spec),
                    (e.target, DropReason::OutsideArc(e.arc)),
                ),
                CompositionError::InvalidInstanceTargetPath(e) => (
                    (e.prim, e.property, e.layer, &e.spec),
                    (e.target, DropReason::InstanceOfClass),
                ),
                _ => continue,
            };
            drops.entry(key).or_default().push(dropped);
        }
        drops
    }

    /// The prim and variant sites the stage reads from each layer.
    fn read_sites(&self) -> HashSet<(LayerId, &SpecPath)> {
        self.prims
            .values()
            .flat_map(|index| index.sources.iter())
            .map(|source| (source.layer_id, &source.spec_path))
            .collect()
    }

    /// Why no composed prim reads the property spec `spec` of `layer`.
    fn unread_reason(
        &self,
        layer: LayerId,
        spec: &SpecPath,
        sites: &HashSet<(LayerId, &SpecPath)>,
        store: &dyn LayerStore,
    ) -> UnreadReason {
        let paths = store.paths();
        // A selection whose host the stage reads from this layer, in no
        // branch of it.
        let selections: Vec<VariantSelectionSite> = spec.variant_selection_sites().collect();
        for at in 0..selections.len() {
            let host = selections[at].host_path;
            let outside = SpecPath::from_variant_selection_sites(host, &selections[..at], paths);
            let inside = SpecPath::from_variant_selection_sites(host, &selections[..=at], paths);
            if sites.contains(&(layer, &outside)) && !sites.contains(&(layer, &inside)) {
                return UnreadReason::UnselectedVariant;
            }
        }
        if !self.has_local_layer(layer) {
            return UnreadReason::NotReached;
        }
        let prim = spec.prim_path();
        let mut above = paths.resolve(prim).parent();
        while let Some(path) = above {
            if paths.lookup(&path).is_some_and(|id| self.is_instance(id)) {
                return UnreadReason::BeneathInstance;
            }
            above = path.parent();
        }
        match self.dangling_kind(TargetPath::Prim(prim), paths) {
            Some(kind) => UnreadReason::PrimNotComposed(kind),
            None => UnreadReason::NotReached,
        }
    }
}

/// The inserting lists of `op`, in [`TargetListPosition`] order.
fn inserted_lists(op: &ListOp<TargetPath>) -> [(TargetListPosition, &[TargetPath]); 4] {
    [
        (
            TargetListPosition::Explicit,
            op.explicit.as_deref().unwrap_or_default(),
        ),
        (TargetListPosition::Prepended, &op.prepend),
        (TargetListPosition::Appended, &op.append),
        (TargetListPosition::Added, &op.add),
    ]
}

/// The positions of `firsts` in the order of their values: distinct ranks
/// below `ranks`. When they are a good part of the ranks, each is placed
/// at its rank, which orders them without comparing; otherwise they are
/// sorted.
fn by_first(firsts: impl ExactSizeIterator<Item = u32>, ranks: usize) -> Vec<usize> {
    let len = firsts.len();
    if len.saturating_mul(8) < ranks {
        let mut pairs: Vec<(u32, usize)> = firsts.zip(0..).collect();
        pairs.sort_unstable();
        return pairs.into_iter().map(|(_, at)| at).collect();
    }
    let mut slots = alloc::vec![usize::MAX; ranks];
    for (at, first) in firsts.enumerate() {
        slots[first as usize] = at;
    }
    slots.retain(|at| *at != usize::MAX);
    slots
}

/// The paths the inserting lists of `property` author.
fn inserted_count(property: &PropertySpec) -> usize {
    property.targets.as_ref().map_or(0, |op| {
        inserted_lists(op)
            .iter()
            .map(|(_, items)| items.len())
            .sum()
    })
}

/// How one opinion reads each path of an authored list, given the list
/// composition mapped it to.
///
/// Mapping keeps the list's order and removes only the paths it drops,
/// each of which [`Stage::composition_errors`] reports for a contributing
/// opinion. So an authored path pairs with the next mapped one unless the
/// list has more paths left than the mapped list and the path is one the
/// errors name. A path dropped without an error, which composition does
/// not do, reads as dropped outside the arc `arc` names.
fn pair(
    authored: &[TargetPath],
    mapped: &[TargetPath],
    drops: &[(TargetPath, DropReason)],
    arc: impl Fn() -> ArcKind,
) -> Vec<TargetRead> {
    let mut reads = Vec::with_capacity(authored.len());
    let mut next = 0;
    for (at, path) in authored.iter().enumerate() {
        let left = authored.len() - at;
        let mapped_left = mapped.len() - next;
        if left > mapped_left {
            if let Some((_, reason)) = drops.iter().find(|(dropped, _)| dropped == path) {
                reads.push(TargetRead::Dropped(*reason));
                continue;
            }
            if mapped_left == 0 {
                reads.push(TargetRead::Dropped(DropReason::OutsideArc(arc())));
                continue;
            }
        }
        reads.push(TargetRead::Mapped(mapped[next]));
        next += 1;
    }
    reads
}

/// Calls `visit` with each property spec of `layer` whose inserting lists
/// author a path, its spec path and how many, in no particular order: the specs of
/// [`crate::Layer::prims`] and [`crate::Layer::variant_prims`], and those
/// of every variant branch of each.
fn for_each_target_spec<'l>(
    layer: &'l crate::Layer,
    store: &dyn LayerStore,
    mut visit: impl FnMut(SpecPath, &'l PropertySpec, usize),
) {
    let paths = store.paths();
    let mut visit_all = |site: &dyn Fn() -> SpecPath, properties: &'l [crate::PropertyEntry]| {
        let mut prim = None;
        for entry in properties {
            let count = inserted_count(&entry.spec);
            if count > 0 {
                let site = prim.get_or_insert_with(site);
                visit(site.with_property(entry.name), &entry.spec, count);
            }
        }
    };
    let prim_specs = layer.prims().iter().chain(
        layer
            .variant_prims
            .iter()
            .flat_map(|(path, specs)| specs.iter().map(move |spec| (path, spec))),
    );
    for (&path, spec) in prim_specs {
        let outer = &spec.outer_variant_sites;
        visit_all(
            &|| SpecPath::from_variant_selection_sites(path, outer, paths),
            &spec.properties,
        );
        for branch in spec.variant_branches() {
            let sites = branch.sites(outer, path);
            visit_all(
                &|| SpecPath::from_variant_selection_sites(path, &sites, paths),
                &branch.spec.properties,
            );
        }
    }
}

/// Sorts property specs in namespace order: by prim path, a spec inside
/// variant branches after the one outside them, by the branches'
/// selections, then by property name.
fn sort_specs(specs: &mut [(SpecPath, &PropertySpec)], store: &dyn LayerStore) {
    let (paths, tokens) = (store.paths(), store.tokens());
    let key = |spec: &SpecPath| {
        spec.variant_selection_sites()
            .map(|site| {
                (
                    paths.resolve(site.host_path).depth(),
                    tokens.resolve(site.set),
                    tokens.resolve(site.variant),
                )
            })
            .collect::<Vec<_>>()
    };
    specs.sort_by(|(a, _), (b, _)| {
        paths
            .cmp_paths(a.prim_path(), b.prim_path(), tokens)
            .then_with(|| key(a).cmp(&key(b)))
            .then_with(|| {
                let name = |spec: &SpecPath| spec.property().map(|p| tokens.resolve(p));
                match (name(a), name(b)) {
                    (Some(a), Some(b)) => super::dictionary_cmp(a, b),
                    (a, b) => a.cmp(&b),
                }
            })
    });
}

#[cfg(test)]
mod tests;
