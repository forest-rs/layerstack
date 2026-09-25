// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition errors.
//!
//! A composition error is an error in the scene description of a composition
//! operator. It does not stop composition: the offending operator is ignored,
//! every other operator is evaluated, and the error is reported alongside the
//! composed stage (see [`Stage::composition_errors`]).
//!
//! Spec: AOUSD Core §10.6 (composition errors).
//!
//! [`Stage::composition_errors`]: crate::Stage::composition_errors

use alloc::vec::Vec;

use crate::{doc::LayerId, path::PathId, prim_index::ArcKind};

/// An error found while composing a stage.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CompositionError {
    /// A sublayer that would form a cycle in a layer stack. The sublayer was
    /// ignored.
    SublayerCycle(SublayerCycle),
    /// A composition arc that would form a cycle. The arc was ignored.
    ArcCycle(ArcCycle),
    /// A reference or payload with no authored prim path whose target
    /// layer's `defaultPrim` does not name a prim. The arc contributed
    /// nothing.
    UnresolvedDefaultPrim(UnresolvedDefaultPrim),
}

impl CompositionError {
    /// Returns the composed prim whose composition found the error, or
    /// `None` for an error in a layer stack.
    #[must_use]
    pub fn prim(&self) -> Option<PathId> {
        match self {
            Self::SublayerCycle(_) => None,
            Self::ArcCycle(cycle) => Some(cycle.prim),
            Self::UnresolvedDefaultPrim(error) => Some(error.prim),
        }
    }
}

/// A reference or payload with no authored prim path
/// ([`ReferenceTarget::DefaultPrim`]) whose target could not be resolved,
/// found while composing `prim`.
///
/// Such an arc targets the prim named by the `defaultPrim` of `layer`, the
/// root layer of the target layer stack (the authoring layer, for an
/// internal arc). It does not resolve when `layer` has no `defaultPrim`, when
/// `defaultPrim` is not a prim path ([`Layer::default_prim_path`]), or when
/// no layer of the target layer stack has a prim spec at the path it names.
/// The arc contributes no opinions and the rest of the prim is composed as
/// normal; authoring a usable `defaultPrim` later is a change that
/// [`LiveStage::notify_default_prim_edit`] recomposes.
///
/// Spec: AOUSD Core §10.3.2.1 ("If there are no specs in any of the layers
/// of the referenced layer stack for the reference prim path, it is a
/// composition error and that reference is ignored"), §10.6. OpenUSD reports
/// both cases as `PcpErrorUnresolvedPrimPath` (`_EvalRefOrPayloadArcs` and
/// `_EvalUnresolvedPrimPathError` in `pxr/usd/pcp/primIndex.cpp`), with the
/// unresolved path `<defaultPrim>` or the path `defaultPrim` names.
///
/// [`ReferenceTarget::DefaultPrim`]: crate::ReferenceTarget::DefaultPrim
/// [`Layer::default_prim_path`]: crate::Layer::default_prim_path
/// [`LiveStage::notify_default_prim_edit`]: crate::LiveStage::notify_default_prim_edit
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct UnresolvedDefaultPrim {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The arc: [`ArcKind::References`] or [`ArcKind::Payloads`].
    pub arc: ArcKind,
    /// The layer whose `defaultPrim` was consulted.
    pub layer: LayerId,
    /// The prim path `defaultPrim` names when no prim spec exists there, or
    /// `None` when `layer` has no `defaultPrim` or it is not a prim path.
    pub path: Option<PathId>,
}

/// A composition arc that would form a cycle, found while composing `prim`.
///
/// Composing a prim follows arcs recursively: each arc's target may author
/// arcs of its own. An arc is a cycle when its target site is in the same
/// layer stack as a site already on that chain of arcs (translated to the
/// namespace depth of `prim`) and either path is a prefix of the other. This
/// covers an arc back to a site already visited (`/A` references `/B`, which
/// references `/A`) and arcs to a namespace ancestor or descendant (`/A/B`
/// inherits `/A`), which would otherwise nest without end. The arc is
/// ignored and the rest of the prim is composed as normal.
///
/// A cycle is reported for each prim whose composition reaches it, so one
/// authored cycle can produce several errors.
///
/// Spec: AOUSD Core §10.6 (composition errors). The Core specification's
/// composition algorithm (§10.2.1) recurses into each arc's target without
/// defining cycles; this follows OpenUSD, which reports them as
/// `PcpErrorArcCycle` (`_CheckForCycle` in `pxr/usd/pcp/primIndex.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArcCycle {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The sites on the chain of arcs, starting with `prim`'s own site in the
    /// stage's layer stack and ending with the target of the ignored arc.
    /// Paths are at the namespace depth of `prim`, except the last, which is
    /// the ignored arc's target as authored (or as mapped into the
    /// destination namespace).
    pub sites: Vec<ArcCycleSite>,
}

/// One site on the chain of arcs of an [`ArcCycle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArcCycleSite {
    /// The root layer of the layer stack holding the site.
    pub layer_stack: LayerId,
    /// The site's prim path within that layer stack.
    pub path: PathId,
    /// The arc that leads to this site from the previous one, or `None` for
    /// the first site (the composed prim itself).
    pub arc: Option<ArcKind>,
}

/// A sublayer that would form a cycle when constructing a layer stack.
///
/// `sublayer` is already on the chain of sublayers from the layer stack's
/// root layer to `layer`, so it is ignored at this position. Each repeated
/// visit is reported, so a cycle reachable along several sublayer paths is
/// reported once per path.
///
/// Spec: AOUSD Core §10.3.1 (a sublayer that would form a cycle is a
/// composition error and is ignored). OpenUSD reports this as
/// `PcpErrorSublayerCycle` (`pxr/usd/pcp/errors.h`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SublayerCycle {
    /// The layer whose `subLayers` names `sublayer`.
    pub layer: LayerId,
    /// The sublayer that closes the cycle.
    pub sublayer: LayerId,
}
