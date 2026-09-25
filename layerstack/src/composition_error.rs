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

use alloc::{string::String, vec::Vec};

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
    /// A reference or payload whose asset path could not be resolved. The
    /// arc contributed nothing.
    UnresolvedAsset(UnresolvedAsset),
    /// A sublayer whose asset path could not be resolved. The sublayer was
    /// ignored.
    UnresolvedSublayer(UnresolvedSublayer),
    /// A `layerRelocates` entry that is invalid on its own. The entry was
    /// ignored.
    InvalidAuthoredRelocation(InvalidAuthoredRelocation),
    /// A `layerRelocates` entry that conflicts with another entry of its
    /// layer stack. The entry was ignored.
    InvalidConflictingRelocation(InvalidConflictingRelocation),
    /// `layerRelocates` entries of one layer stack that move different
    /// sources to one target. Every one of them was ignored.
    InvalidSameTargetRelocations(InvalidSameTargetRelocations),
    /// Opinions authored at the source of a relocation, in the layer stack
    /// that relocates it. The opinions were ignored.
    OpinionAtRelocationSource(OpinionAtRelocationSource),
    /// An arc whose target is a prim that relocates remove from its layer
    /// stack's namespace. The arc contributed nothing.
    ArcToProhibitedChild(ArcToProhibitedChild),
}

impl CompositionError {
    /// Returns the composed prim whose composition found the error, or
    /// `None` for an error in a layer stack.
    #[must_use]
    pub fn prim(&self) -> Option<PathId> {
        match self {
            Self::SublayerCycle(_)
            | Self::UnresolvedSublayer(_)
            | Self::InvalidAuthoredRelocation(_)
            | Self::InvalidConflictingRelocation(_)
            | Self::InvalidSameTargetRelocations(_) => None,
            Self::ArcCycle(cycle) => Some(cycle.prim),
            Self::UnresolvedDefaultPrim(error) => Some(error.prim),
            Self::UnresolvedAsset(error) => Some(error.prim),
            Self::OpinionAtRelocationSource(error) => Some(error.prim),
            Self::ArcToProhibitedChild(error) => Some(error.prim),
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

/// A reference or payload whose asset path could not be resolved, found
/// while composing `prim`.
///
/// Importers keep such an arc as a [`Reference::unresolved`] instead of
/// dropping it; it targets no layer stack and contributes no opinions, and
/// the rest of the prim is composed as normal. It never falls back to the
/// authoring layer, whatever its target.
///
/// Spec: AOUSD Core §10.3.2.1 ("If a layer stack cannot be computed for a
/// reference's layer asset path, it is a composition error and that
/// reference is ignored"), §10.6. OpenUSD reports it as
/// `PcpErrorInvalidAssetPath` (`_EvalRefOrPayloadArcs` in
/// `pxr/usd/pcp/primIndex.cpp`).
///
/// [`Reference::unresolved`]: crate::Reference::unresolved
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnresolvedAsset {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The arc: [`ArcKind::References`] or [`ArcKind::Payloads`].
    pub arc: ArcKind,
    /// The asset path as authored.
    pub asset: String,
}

/// A sublayer whose asset path could not be resolved, found when gathering
/// a layer stack.
///
/// Importers keep such a sublayer as a [`SublayerEntry::unresolved`] instead
/// of dropping it. It contributes no layer, and the rest of the layer stack,
/// including the sublayers after it, is gathered as usual. Each layer stack
/// that reaches it reports it.
///
/// Spec: AOUSD Core §10.3.1 (sublayers), §10.6 (composition errors).
/// OpenUSD reports it as `PcpErrorInvalidSublayerPath`
/// (`PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`), with
/// the layer that names the sublayer and the authored path.
///
/// [`SublayerEntry::unresolved`]: crate::SublayerEntry::unresolved
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UnresolvedSublayer {
    /// The layer whose `subLayers` names the sublayer.
    pub layer: LayerId,
    /// The sublayer's asset path as authored.
    pub asset: String,
}

/// A `layerRelocates` entry that is invalid whatever else its layer stack
/// authors, found when computing the relocation table of a layer stack
/// holding `layer`.
///
/// Spec: AOUSD Core §10.3.2.6 ("If an entry in the layerRelocates field
/// violates any of the following restrictions, it is a composition error
/// and that entry is ignored"). OpenUSD reports it as
/// `PcpErrorInvalidAuthoredRelocation` (`Pcp_IsValidRelocatesEntry` in
/// `pxr/usd/pcp/layerStack.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InvalidAuthoredRelocation {
    /// The layer whose `layerRelocates` authors the entry.
    pub layer: LayerId,
    /// The entry's source path.
    pub source: PathId,
    /// The entry's target path; `None` for a relocate to `<>`.
    pub target: Option<PathId>,
    /// Why the entry is invalid.
    pub reason: InvalidRelocationReason,
}

/// Why a `layerRelocates` entry is invalid on its own (see
/// [`InvalidAuthoredRelocation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvalidRelocationReason {
    /// The source is a root prim, whose parent, the pseudo-root, authors no
    /// arcs.
    RootPrimSource,
    /// The target is the source itself.
    TargetIsSource,
    /// The target is a namespace ancestor of the source.
    TargetIsAncestor,
    /// The target is a namespace descendant of the source.
    TargetIsDescendant,
}

/// A `layerRelocates` entry that conflicts with another valid entry of the
/// same layer stack, found when computing that layer stack's relocation
/// table. Each side of a conflict is reported on its own, and both entries
/// are ignored.
///
/// Spec: AOUSD Core §10.3.2.6 (each source has one target and each target
/// one source; a source must use the ancestral relocated path). OpenUSD
/// reports it as `PcpErrorInvalidConflictingRelocation`
/// (`_ValidateAndRemoveConflictingRelocates` in
/// `pxr/usd/pcp/layerStack.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InvalidConflictingRelocation {
    /// The layer whose `layerRelocates` authors the ignored entry.
    pub layer: LayerId,
    /// The ignored entry's source path.
    pub source: PathId,
    /// The ignored entry's target path; `None` for a relocate to `<>`.
    pub target: Option<PathId>,
    /// The layer authoring the entry it conflicts with.
    pub conflict_layer: LayerId,
    /// The conflicting entry's source path.
    pub conflict_source: PathId,
    /// The conflicting entry's target path.
    pub conflict_target: Option<PathId>,
    /// How the two entries conflict.
    pub reason: RelocationConflict,
}

/// How two `layerRelocates` entries conflict (see
/// [`InvalidConflictingRelocation`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RelocationConflict {
    /// The entry's target is the other entry's source.
    TargetIsConflictSource,
    /// The entry's source is the other entry's target.
    SourceIsConflictTarget,
    /// The entry's target is a namespace descendant of the other entry's
    /// source, so it is not a fully relocated path.
    TargetIsConflictSourceDescendant,
    /// The entry's source is a namespace descendant of the other entry's
    /// source, so it does not use the ancestral relocated path.
    SourceIsConflictSourceDescendant,
}

/// `layerRelocates` entries of one layer stack that move different sources
/// to the same target, found when computing that layer stack's relocation
/// table. Every one of them is ignored.
///
/// Spec: AOUSD Core §10.3.2.6 ("each target may only have one possible
/// source path"). OpenUSD reports it as
/// `PcpErrorInvalidSameTargetRelocations`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InvalidSameTargetRelocations {
    /// The shared target path.
    pub target: PathId,
    /// Each entry moving a prim there: the authoring layer and the source
    /// path, ordered by source path.
    pub sources: Vec<(LayerId, PathId)>,
}

/// A prim spec authored at the source of a relocation, in a layer of the
/// layer stack that relocates it, found while composing `prim`, the prim
/// the source moves to.
///
/// Once a prim is relocated, its source path does not exist in that layer
/// stack: opinions authored there, and beneath it, are ignored. The prim is
/// composed from the relocation target's own opinions and the source's
/// ancestral ones. Only the source itself is reported, as OpenUSD does.
///
/// Spec: AOUSD Core §10.3.2.6 ("If any opinions are authored in a layer
/// stack at a source path of a relocates statement in that layer stack, it
/// is a composition error and those opinions are ignored"). OpenUSD reports
/// it as `PcpErrorOpinionAtRelocationSource` (`_EvalNodeRelocations` in
/// `pxr/usd/pcp/primIndex.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpinionAtRelocationSource {
    /// The composed prim the relocation moves the source to.
    pub prim: PathId,
    /// The layer authoring a spec at the source.
    pub layer: LayerId,
    /// The relocation source path in that layer.
    pub path: PathId,
}

/// An arc whose target is a relocation source, or lies beneath one, in the
/// target's layer stack, found while composing `prim`.
///
/// Relocating a prim removes its source path from the namespace of the
/// relocating layer stack, so no arc can target it; an arc must target the
/// relocated path instead. The arc contributes nothing, and the rest of the
/// prim is composed as normal.
///
/// Spec: AOUSD Core §10.3.2.6 (opinions at a relocation source are
/// ignored). OpenUSD reports it as `PcpErrorArcToProhibitedChild`
/// (`_ComposeIsProhibitedPrimChild` in `pxr/usd/pcp/primIndex.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArcToProhibitedChild {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The kind of the ignored arc.
    pub arc: ArcKind,
    /// The root layer of the arc's target layer stack.
    pub layer_stack: LayerId,
    /// The arc's target path.
    pub target: PathId,
    /// The relocation source at or above the target.
    pub relocation_source: PathId,
}
