// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explaining a relationship's targets or an attribute's connections: what
//! each opinion authors, what composition kept, and what it dropped and
//! why.

use alloc::vec::Vec;

use super::Stage;
use crate::{
    ArcKind, CompositionError, LayerId, LayerStore, ListOp, PropertyPath, SpecPath, TargetPath,
};

/// Why a property's targets or connections are what they are: the composed
/// list, and every opinion that authors one, strongest first.
///
/// Composition maps each authored path across the arcs that bring its spec
/// in, and drops a path an arc cannot map: one outside the namespace a
/// reference or payload brings in, or one naming an instance of the class
/// that authors it. A dropped path is also reported in
/// [`Stage::composition_errors`]; here it is listed with the opinion that
/// authors it.
///
/// OpenUSD reports these drops only while building a property's target
/// index (`PcpBuildFilteredTargetIndex`); `UsdRelationship::GetTargets`
/// returns the kept paths alone.
///
/// Spec: AOUSD Core §10.3.2 (an arc maps its target's namespace), §12.4
/// (list op resolution).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TargetsExplanation {
    /// The composed targets or connections, as
    /// [`Relationship::targets`](crate::Relationship::targets) gives them.
    pub targets: Vec<TargetPath>,
    /// Every opinion that authors targets or connections, strongest first.
    pub opinions: Vec<TargetOpinion>,
}

/// One opinion of a [`TargetsExplanation`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TargetOpinion {
    /// The layer of the property spec.
    pub layer: LayerId,
    /// The property spec, in the namespace it is authored in.
    pub spec: SpecPath,
    /// The arc that brings the spec in; `None` for the stage's own layer
    /// stack.
    pub arc: Option<ArcKind>,
    /// The list op as authored, in the spec's namespace.
    pub authored: ListOp<TargetPath>,
    /// Each authored path composition kept, with the stage path it maps
    /// to, in authored order (the explicit, added, prepended, then
    /// appended items). Deleted and reordered paths are mapped where they
    /// can be and are not listed.
    pub kept: Vec<(TargetPath, TargetPath)>,
    /// The paths this opinion authors that composition dropped.
    pub dropped: Vec<DroppedTarget>,
    /// Whether this opinion contributes: `false` when a stronger opinion's
    /// explicit list replaces it. A replaced opinion's paths are not
    /// checked, so it has neither `kept` nor `dropped` paths; OpenUSD's
    /// target index reports no error for them either.
    pub contributes: bool,
}

/// A path an opinion authors that composition dropped (see
/// [`TargetsExplanation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DroppedTarget {
    /// The path as authored, in the namespace of its spec.
    pub authored: TargetPath,
    /// Why it was dropped.
    pub reason: DropReason,
}

/// Why composition dropped an authored target or connection path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DropReason {
    /// The path is outside the namespace the arc maps: a reference or
    /// payload maps only its target prim and what lies beneath it.
    ///
    /// Spec: AOUSD Core §10.3.2. OpenUSD: `PcpErrorInvalidExternalTargetPath`.
    OutsideArc(ArcKind),
    /// The path names an instance of the class that authors it, which the
    /// class cannot target.
    ///
    /// OpenUSD: `PcpErrorInvalidInstanceTargetPath`.
    InstanceOfClass,
}

impl core::fmt::Display for DropReason {
    /// Why the path was dropped, in words: `outside the namespace the
    /// reference maps`, or `an instance of the class authoring it`.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutsideArc(arc) => write!(
                f,
                "outside the namespace the {} maps",
                crate::composition_error::arc_name(*arc)
            ),
            Self::InstanceOfClass => f.write_str("an instance of the class authoring it"),
        }
    }
}

impl Stage {
    /// Explains the targets of the relationship, or the connections of the
    /// attribute, at `property_path` (see [`TargetsExplanation`]); `None`
    /// when the property is not composed. `store` holds the layers the
    /// stage was composed from, for the opinions' authored lists.
    ///
    /// The work is linear in the property's opinions and the stage's
    /// target path errors, which are filtered once.
    #[must_use]
    pub(crate) fn explain_targets(
        &self,
        property_path: PropertyPath,
        store: &dyn LayerStore,
    ) -> Option<TargetsExplanation> {
        let opinions = self.explain_property_path(property_path)?;
        let graph = self.explain_prim_graph(property_path.prim_path());
        let (prim, property) = (property_path.prim_path(), property_path.property());
        // The property's dropped paths, by the layer and spec that author
        // them.
        let drops: Vec<(LayerId, &SpecPath, DroppedTarget)> = self
            .composition_errors()
            .iter()
            .filter_map(|error| match error {
                CompositionError::InvalidExternalTargetPath(e)
                    if (e.prim, e.property) == (prim, property) =>
                {
                    Some((
                        e.layer,
                        &e.spec,
                        DroppedTarget {
                            authored: e.target,
                            reason: DropReason::OutsideArc(e.arc),
                        },
                    ))
                }
                CompositionError::InvalidInstanceTargetPath(e)
                    if (e.prim, e.property) == (prim, property) =>
                {
                    Some((
                        e.layer,
                        &e.spec,
                        DroppedTarget {
                            authored: e.target,
                            reason: DropReason::InstanceOfClass,
                        },
                    ))
                }
                _ => None,
            })
            .collect();
        let mut replaced = false;
        let mut explained = Vec::new();
        for opinion in opinions {
            let Some(mapped) = opinion.value().targets() else {
                continue;
            };
            let (layer, spec) = (opinion.layer_id(), &opinion.spec_path());
            let authored = store
                .layer(layer)
                .and_then(|l| l.property_at(spec))
                .and_then(|p| p.targets.clone())
                .unwrap_or_default();
            let arc = graph
                .and_then(|graph| graph.node(opinion.node()))
                .map(|node| node.arc_kind())
                .filter(|kind| *kind != ArcKind::Local);
            let (kept, dropped) = if replaced {
                (Vec::new(), Vec::new())
            } else {
                let dropped: Vec<DroppedTarget> = drops
                    .iter()
                    .filter(|(l, s, _)| *l == layer && *s == spec)
                    .map(|(_, _, d)| *d)
                    .collect();
                (kept_pairs(&authored, mapped, &dropped), dropped)
            };
            explained.push(TargetOpinion {
                layer,
                spec: spec.clone(),
                arc,
                authored: *authored,
                kept,
                dropped,
                contributes: !replaced,
            });
            replaced |= mapped.explicit.is_some();
        }
        Some(TargetsExplanation {
            targets: self
                .resolve_target_list_path(property_path)
                .map(|resolved| resolved.value)
                .unwrap_or_default(),
            opinions: explained,
        })
    }
}

/// Pairs each authored inserted path that was not dropped with the path it
/// maps to: mapping keeps each list's order and only removes the dropped
/// paths, so the two lists zip once those are skipped.
fn kept_pairs(
    authored: &ListOp<TargetPath>,
    mapped: &ListOp<TargetPath>,
    dropped: &[DroppedTarget],
) -> Vec<(TargetPath, TargetPath)> {
    let lists = |op: &ListOp<TargetPath>| {
        [
            op.explicit.clone().unwrap_or_default(),
            op.add.clone(),
            op.prepend.clone(),
            op.append.clone(),
        ]
    };
    let mut pairs = Vec::new();
    for (authored, mapped) in lists(authored).into_iter().zip(lists(mapped)) {
        let kept = authored
            .into_iter()
            .filter(|path| !dropped.iter().any(|d| d.authored == *path));
        pairs.extend(kept.zip(mapped));
    }
    pairs
}
