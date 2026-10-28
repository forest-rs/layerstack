// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dangling relationship targets and attribute connections: composed paths
//! that name nothing usable on the stage, counted by property name.
//!
//! A target is checked as composition left it: the list
//! [`Relationship::targets`](crate::Relationship::targets) gives, the one OpenUSD's
//! `UsdRelationship::GetTargets` and `UsdAttribute::GetConnections` return.
//! What it names is decided the way `UsdStage::GetObjectAtPath` decides it:
//! a prim the stage populates, or a property such a prim has, authored or
//! defined by its schemas. Paths composition dropped (outside the arc that
//! brings their spec in, or naming an instance of the class that authors
//! them) are not in that list; [`Relationship::explain_targets`](crate::Relationship::explain_targets) reports them.
//!
//! Spec: AOUSD Core §7.3.3 (properties), §11.3 (population: inactive prims
//! and unloaded payloads), §12.4 (target list resolution).

use alloc::vec::Vec;

use hashbrown::HashMap;

use super::Stage;
use crate::{LayerStore, PathInterner, PropertyKind, PropertyPath, TargetPath, TokenId};

/// Why a composed target or connection path names nothing usable.
///
/// A path that resolves names a prim the stage populates, active, or a
/// property of one: `UsdStage::GetObjectAtPath` returns a valid object for
/// it. A loaded prim and an unloaded one resolve alike, and so do instance
/// proxies, the prims beneath an instance (`/Plants/Fern_00/Frond`), which
/// the stage composes per instance.
///
/// The kinds keep apart what does not exist from what the stage leaves
/// out, which exists in the scene description but is not composed: an
/// inactive prim's descendants, an unloaded payload's contents and the
/// prims a population mask leaves out.
///
/// Divergence: layerstack names no prototype prims (`/__Prototype_1` in
/// OpenUSD, a name each stage chooses when it opens), so a path into one
/// is [`Self::NoPrim`] here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DanglingKind {
    /// No prim at the path, or at the prim path of a property path, and no
    /// inactive prim, unloaded payload or population mask above it to
    /// explain why. OpenUSD: `GetObjectAtPath` returns an invalid object,
    /// and the nearest prim above that it does return is active and loaded.
    NoPrim,
    /// A property path whose prim resolves, active and loaded, but has no
    /// property of that name: none authored, none its schemas define.
    /// OpenUSD: `GetObjectAtPath` returns an invalid object;
    /// `GetPrimAtPath` of its prim path a valid one.
    NoProperty,
    /// The path is an inactive prim, a property of one, or beneath one.
    /// OpenUSD returns a valid object for the inactive prim and its
    /// properties (`UsdPrim::IsActive` is false) and none beneath it, which
    /// the stage does not populate.
    ///
    /// Spec: AOUSD Core §11.3.1 (an inactive prim's descendants are not
    /// populated).
    Inactive,
    /// The path names nothing, and the nearest prim above it (or its prim,
    /// for a property path) is not loaded: an unloaded payload might
    /// compose it. OpenUSD: `GetObjectAtPath` returns an invalid object,
    /// and `UsdPrim::IsLoaded` of that nearest prim is false.
    ///
    /// Spec: AOUSD Core §10.3.2.7 (payloads), §11.3.
    Unloaded,
    /// The path names no prim because the stage's population mask leaves
    /// it out ([`crate::StageOptions::mask`]). OpenUSD:
    /// `UsdStagePopulationMask::Includes` is false for it.
    Masked,
}

/// Dangling targets counted by [`DanglingKind`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DanglingCounts {
    /// [`DanglingKind::NoPrim`].
    pub no_prim: usize,
    /// [`DanglingKind::NoProperty`].
    pub no_property: usize,
    /// [`DanglingKind::Inactive`].
    pub inactive: usize,
    /// [`DanglingKind::Unloaded`].
    pub unloaded: usize,
    /// [`DanglingKind::Masked`].
    pub masked: usize,
}

impl DanglingCounts {
    /// Every dangling target, of any kind.
    #[must_use]
    pub fn total(&self) -> usize {
        self.no_prim + self.no_property + self.inactive + self.unloaded + self.masked
    }

    /// The count of `kind`.
    #[must_use]
    pub fn of(&self, kind: DanglingKind) -> usize {
        match kind {
            DanglingKind::NoPrim => self.no_prim,
            DanglingKind::NoProperty => self.no_property,
            DanglingKind::Inactive => self.inactive,
            DanglingKind::Unloaded => self.unloaded,
            DanglingKind::Masked => self.masked,
        }
    }

    /// Counts one more of `kind`.
    pub fn add(&mut self, kind: DanglingKind) {
        let count = match kind {
            DanglingKind::NoPrim => &mut self.no_prim,
            DanglingKind::NoProperty => &mut self.no_property,
            DanglingKind::Inactive => &mut self.inactive,
            DanglingKind::Unloaded => &mut self.unloaded,
            DanglingKind::Masked => &mut self.masked,
        };
        *count += 1;
    }
}

/// One dangling target: the property that names it, and why it dangles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DanglingTarget {
    /// The relationship or attribute whose composed list names it.
    pub source: PropertyPath,
    /// The composed target or connection path.
    pub target: TargetPath,
    /// Why it dangles.
    pub kind: DanglingKind,
}

impl DanglingTarget {
    /// The example in words, its paths written as text with `store`, the
    /// store the stage was composed from: `/Placements/Spot.shade ->
    /// /Plants/Fern/Petal (NoPrim)`.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        crate::diagnostic::with_store(self, store)
    }
}

impl crate::diagnostic::Describe for DanglingTarget {
    fn describe(
        &self,
        f: &mut core::fmt::Formatter<'_>,
        names: crate::diagnostic::Names<'_>,
    ) -> core::fmt::Result {
        write!(
            f,
            "{} -> {} ({:?})",
            names.property(self.source),
            names.target(self.target),
            self.kind
        )
    }
}

/// The targets or connections of every property of one name.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DanglingByName {
    /// The property name, on any prim.
    pub name: TokenId,
    /// Properties of this name with at least one composed target.
    pub properties: usize,
    /// Composed targets checked, over all of them.
    pub targets: usize,
    /// Of those, the dangling ones, by kind: exact.
    pub dangling: DanglingCounts,
    /// The first dangling targets, at most the number asked for, in stage
    /// order: prims in namespace order, a prim's properties in property
    /// order, a property's targets in composed order.
    pub examples: Vec<DanglingTarget>,
}

/// Dangling relationship targets and attribute connections over a stage,
/// by property name ([`Stage::dangling_targets`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DanglingTargetReport {
    /// Relationships with at least one composed target, by name, in
    /// property-name order.
    pub relationships: Vec<DanglingByName>,
    /// Attributes with at least one composed connection, by name, in
    /// property-name order.
    pub connections: Vec<DanglingByName>,
    /// Prims walked.
    pub prims: usize,
    /// Properties whose targets or connections were read.
    pub properties: usize,
    /// Composed targets and connections checked.
    pub targets: usize,
}

impl DanglingTargetReport {
    /// Every dangling target and connection, of any kind.
    #[must_use]
    pub fn total(&self) -> DanglingCounts {
        let mut total = DanglingCounts::default();
        for group in self.relationships.iter().chain(&self.connections) {
            let counts = group.dangling;
            total.no_prim += counts.no_prim;
            total.no_property += counts.no_property;
            total.inactive += counts.inactive;
            total.unloaded += counts.unloaded;
            total.masked += counts.masked;
        }
        total
    }

    /// Whether nothing dangles.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.total().total() == 0
    }

    /// The relationship group of `name`, if any relationship of that name
    /// has targets.
    #[must_use]
    pub fn relationship(&self, name: TokenId) -> Option<&DanglingByName> {
        self.relationships.iter().find(|group| group.name == name)
    }

    /// The connection group of `name`, if any attribute of that name has
    /// connections.
    #[must_use]
    pub fn connection(&self, name: TokenId) -> Option<&DanglingByName> {
        self.connections.iter().find(|group| group.name == name)
    }
}

impl Stage {
    /// Why the composed target or connection path `target` dangles, or
    /// `None` when it resolves (see [`DanglingKind`]).
    ///
    /// A path is a prim on the stage, or a property of one that an opinion
    /// declares or its schemas define ([`Property::kind`](crate::Property::kind)), as
    /// [`Stage::object`] finds them; one of an inactive prim is
    /// [`DanglingKind::Inactive`]. For a path that names nothing, the
    /// nearest prim above it that the stage populates says why: inactive,
    /// not loaded, or present while the population mask leaves the path
    /// out; otherwise the prim, or for a property path its prim, does not
    /// exist.
    ///
    /// OpenUSD: `UsdStage::GetObjectAtPath`, `UsdPrim::IsActive`,
    /// `UsdPrim::IsLoaded`.
    #[must_use]
    pub fn dangling_kind(&self, target: TargetPath, paths: &PathInterner) -> Option<DanglingKind> {
        let prim = target.prim_path();
        if self.has_prim(prim) {
            if !self.is_active(prim) {
                return Some(DanglingKind::Inactive);
            }
            let TargetPath::Property(property) = target else {
                return None;
            };
            if self.property_kind(property).is_some() {
                return None;
            }
            return Some(if self.is_loaded(prim, paths) {
                DanglingKind::NoProperty
            } else {
                DanglingKind::Unloaded
            });
        }
        let mut above = paths.resolve(prim).parent();
        while let Some(path) = above {
            if let Some(id) = paths.lookup(&path)
                && self.has_prim(id)
            {
                if !self.is_active(id) {
                    return Some(DanglingKind::Inactive);
                }
                if !self.is_loaded(id, paths) {
                    return Some(DanglingKind::Unloaded);
                }
                break;
            }
            above = path.parent();
        }
        let masked = self
            .population_mask()
            .is_some_and(|mask| !mask.includes(prim, paths));
        Some(if masked {
            DanglingKind::Masked
        } else {
            DanglingKind::NoPrim
        })
    }

    /// Counts the dangling relationship targets and attribute connections
    /// of every prim on the stage, by property name, with the first
    /// `examples` of each name (see [`DanglingTargetReport`]).
    ///
    /// The walk visits every prim the stage populates, as
    /// [`Stage::traverse_all`] does: inactive prims, unloaded payload
    /// prims, abstract and undefined prims, and the prims beneath
    /// instances, each occurrence with its own targets (OpenUSD:
    /// `UsdStage::Traverse` with
    /// `UsdTraverseInstanceProxies(UsdPrimAllPrimsPredicate)`). Each
    /// property's composed list is checked once with
    /// [`Stage::dangling_kind`]; a target naming another relationship is
    /// a property that resolves, and is not forwarded
    /// (`UsdRelationship::GetTargets`, not `GetForwardedTargets`), so a
    /// dangling path is counted once, on the relationship that names it.
    ///
    /// The work is one composed-list resolution per authored property,
    /// and per target a lookup or two; a dangling one walks up its path to
    /// the nearest populated prim. Nothing is cached.
    ///
    /// To find the layer and spec that author each path, walk the specs
    /// instead: [`Stage::authored_targets`] reads every authored path as
    /// this stage does, layer by layer, and `layerstack_schemas`'
    /// `LayerDanglingReport` checks them as this report does.
    ///
    /// OpenUSD: `UsdRelationship::GetTargets`,
    /// `UsdAttribute::GetConnections`, `UsdStage::GetObjectAtPath`.
    ///
    /// Spec: AOUSD Core §12.4 (target list resolution).
    #[must_use]
    pub fn dangling_targets(
        &self,
        store: &dyn LayerStore,
        examples: usize,
    ) -> DanglingTargetReport {
        let paths = store.paths();
        let mut report = DanglingTargetReport::default();
        let mut groups: HashMap<(PropertyKind, TokenId), DanglingByName> = HashMap::new();
        let Some(root) = paths.lookup(&crate::Path::root()) else {
            return report;
        };
        for prim in self.traverse_all(root) {
            report.prims += 1;
            for name in self.authored_property_names(prim, store) {
                let property = PropertyPath::new(prim, name);
                let Some(targets) = self.resolve_target_list_path(property) else {
                    continue;
                };
                report.properties += 1;
                if targets.value.is_empty() {
                    continue;
                }
                let Some(kind) = self
                    .resolve_property_declaration(prim, name)
                    .map(|declaration| declaration.kind)
                else {
                    continue;
                };
                let group = groups
                    .entry((kind, name))
                    .or_insert_with(|| DanglingByName {
                        name,
                        properties: 0,
                        targets: 0,
                        dangling: DanglingCounts::default(),
                        examples: Vec::new(),
                    });
                group.properties += 1;
                group.targets += targets.value.len();
                report.targets += targets.value.len();
                for target in targets.value {
                    let Some(kind) = self.dangling_kind(target, paths) else {
                        continue;
                    };
                    group.dangling.add(kind);
                    if group.examples.len() < examples {
                        group.examples.push(DanglingTarget {
                            source: property,
                            target,
                            kind,
                        });
                    }
                }
            }
        }
        for ((kind, _), group) in groups {
            match kind {
                PropertyKind::Relationship => report.relationships.push(group),
                PropertyKind::Attribute => report.connections.push(group),
            }
        }
        let tokens = store.tokens();
        for groups in [&mut report.relationships, &mut report.connections] {
            groups.sort_by(|a, b| {
                super::dictionary_cmp(tokens.resolve(a.name), tokens.resolve(b.name))
            });
        }
        report
    }
}

#[cfg(test)]
mod tests;
