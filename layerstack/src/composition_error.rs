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

use crate::sort;
use alloc::{string::String, vec::Vec};
use core::{cmp::Ordering, fmt};

use hashbrown::HashMap;

use crate::{
    composition_limits::CompositionLimit,
    diagnostic::{Describe, Names, with_store},
    doc::{LayerId, LayerStore},
    interner::{TokenId, TokenInterner},
    path::{PathId, PathInterner, PropertyPath, TargetPath},
    prim_index::ArcKind,
    property::PropertyKind,
    spec_path::SpecPath,
};

/// An error found while composing a stage.
///
/// [`CompositionError::display`] writes it in words, its paths and names
/// as text.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CompositionError {
    /// The schema registry of [`StageOptions::schemas`] was built on
    /// another token interner than the store's
    /// ([`SchemaRegistry::belongs_to`]): its ids name other strings in the
    /// store. The registry was not used, so the stage has no schemas, as if
    /// it had been composed without one. Build the registry on the store's
    /// `tokens`.
    ///
    /// This is not an error in scene description, and OpenUSD, whose
    /// schema registry is a process-wide singleton, has no counterpart;
    /// composition reports it here because it cannot fail.
    ///
    /// [`StageOptions::schemas`]: crate::StageOptions::schemas
    /// [`SchemaRegistry::belongs_to`]: crate::SchemaRegistry::belongs_to
    ForeignSchemaRegistry,
    /// A sublayer that would form a cycle in a layer stack. The sublayer was
    /// ignored.
    SublayerCycle(SublayerCycle),
    /// A composition arc that would form a cycle. The arc was ignored.
    ArcCycle(ArcCycle),
    /// A reference or payload with no authored prim path whose target
    /// layer's `defaultPrim` does not name a prim. The arc contributed
    /// nothing.
    UnresolvedDefaultPrim(UnresolvedDefaultPrim),
    /// A reference or payload to a prim path that has no specs in the
    /// target layer stack. The arc contributed nothing.
    UnresolvedPrimPath(UnresolvedPrimPath),
    /// A reference or payload whose asset path could not be resolved. The
    /// arc contributed nothing.
    UnresolvedAsset(UnresolvedAsset),
    /// A reference or payload whose target layer the stage mutes
    /// ([`StageOptions::muted_layers`]). The arc contributed nothing.
    ///
    /// [`StageOptions::muted_layers`]: crate::StageOptions::muted_layers
    MutedAsset(MutedAsset),
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
    /// A property spec of a different kind (attribute or relationship) than
    /// the property's strongest spec. The spec was ignored.
    InconsistentPropertyType(InconsistentPropertyType),
    /// A relationship target or attribute connection that cannot be mapped
    /// across the arc that brings its spec in. The path was ignored.
    InvalidExternalTargetPath(InvalidExternalTargetPath),
    /// A relationship target or attribute connection authored in a class
    /// that targets an instance of that class. The path was ignored.
    InvalidInstanceTargetPath(InvalidInstanceTargetPath),
    /// A variable expression in an asset path or variant selection that
    /// failed to evaluate. The sublayer, arc or selection was ignored.
    VariableExpressionError(VariableExpressionError),
    /// A layer whose `subLayers` and `subLayerOffsets` counts differ. Every
    /// one of its sublayers took the identity offset.
    InvalidSublayerAndOffsetCount(InvalidSublayerAndOffsetCount),
    /// A layer a layer stack names by id that the store neither holds nor
    /// reports loading: the stage's root or session layer, or a sublayer.
    /// It contributed nothing.
    MissingLayer(MissingLayer),
    /// Composition reached one of its [`StageOptions::limits`] at a prim:
    /// the prim composed no children, and, for
    /// [`CompositionLimit::PrimIndexNodes`], no prim composition would
    /// index after it was composed. The stage holds the prims composed
    /// before it stopped.
    ///
    /// [`StageOptions::limits`]: crate::StageOptions::limits
    LimitExceeded(LimitExceeded),
}

impl CompositionError {
    /// Returns the composed prim whose composition found the error, or
    /// `None` for an error in a layer stack.
    #[must_use]
    pub fn prim(&self) -> Option<PathId> {
        match self {
            Self::ForeignSchemaRegistry
            | Self::SublayerCycle(_)
            | Self::UnresolvedSublayer(_)
            | Self::InvalidAuthoredRelocation(_)
            | Self::InvalidConflictingRelocation(_)
            | Self::InvalidSameTargetRelocations(_)
            | Self::InvalidSublayerAndOffsetCount(_)
            | Self::MissingLayer(_) => None,
            Self::ArcCycle(cycle) => Some(cycle.prim),
            Self::UnresolvedDefaultPrim(error) => Some(error.prim),
            Self::UnresolvedPrimPath(error) => Some(error.prim),
            Self::UnresolvedAsset(error) => Some(error.prim),
            Self::MutedAsset(error) => Some(error.prim),
            Self::OpinionAtRelocationSource(error) => Some(error.prim),
            Self::ArcToProhibitedChild(error) => Some(error.prim),
            Self::InconsistentPropertyType(error) => Some(error.prim),
            Self::InvalidExternalTargetPath(error) => Some(error.prim),
            Self::InvalidInstanceTargetPath(error) => Some(error.prim),
            Self::VariableExpressionError(error) => error.prim,
            Self::LimitExceeded(error) => Some(error.prim),
        }
    }

    /// The stable code naming this error's kind, for hosts and bindings
    /// that branch on it without matching the enum: `composition.` and the
    /// variant's name in `snake_case`, less any `Error` suffix.
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | [`Self::ForeignSchemaRegistry`] | `composition.foreign_schema_registry` |
    /// | [`Self::SublayerCycle`] | `composition.sublayer_cycle` |
    /// | [`Self::ArcCycle`] | `composition.arc_cycle` |
    /// | [`Self::UnresolvedDefaultPrim`] | `composition.unresolved_default_prim` |
    /// | [`Self::UnresolvedPrimPath`] | `composition.unresolved_prim_path` |
    /// | [`Self::UnresolvedAsset`] | `composition.unresolved_asset` |
    /// | [`Self::MutedAsset`] | `composition.muted_asset` |
    /// | [`Self::UnresolvedSublayer`] | `composition.unresolved_sublayer` |
    /// | [`Self::InvalidAuthoredRelocation`] | `composition.invalid_authored_relocation` |
    /// | [`Self::InvalidConflictingRelocation`] | `composition.invalid_conflicting_relocation` |
    /// | [`Self::InvalidSameTargetRelocations`] | `composition.invalid_same_target_relocations` |
    /// | [`Self::OpinionAtRelocationSource`] | `composition.opinion_at_relocation_source` |
    /// | [`Self::ArcToProhibitedChild`] | `composition.arc_to_prohibited_child` |
    /// | [`Self::InconsistentPropertyType`] | `composition.inconsistent_property_type` |
    /// | [`Self::InvalidExternalTargetPath`] | `composition.invalid_external_target_path` |
    /// | [`Self::InvalidInstanceTargetPath`] | `composition.invalid_instance_target_path` |
    /// | [`Self::VariableExpressionError`] | `composition.variable_expression` |
    /// | [`Self::InvalidSublayerAndOffsetCount`] | `composition.invalid_sublayer_and_offset_count` |
    /// | [`Self::MissingLayer`] | `composition.missing_layer` |
    /// | [`Self::LimitExceeded`] | `composition.limit_exceeded` |
    ///
    /// Codes are part of the public contract: a released code keeps its
    /// spelling and meaning, and a new variant brings a new code.
    ///
    /// Spec: AOUSD Core §10.6 (composition errors). OpenUSD names the same
    /// kinds `PcpErrorType` (`PcpErrorType_ArcCycle`, ...).
    #[must_use]
    pub fn code(&self) -> &'static str {
        CODES[usize::from(self.rank())]
    }

    /// The position of this error's kind in the order the variants are
    /// declared.
    fn rank(&self) -> u8 {
        match self {
            Self::ForeignSchemaRegistry => 0,
            Self::SublayerCycle(_) => 1,
            Self::ArcCycle(_) => 2,
            Self::UnresolvedDefaultPrim(_) => 3,
            Self::UnresolvedPrimPath(_) => 4,
            Self::UnresolvedAsset(_) => 5,
            Self::MutedAsset(_) => 6,
            Self::UnresolvedSublayer(_) => 7,
            Self::InvalidAuthoredRelocation(_) => 8,
            Self::InvalidConflictingRelocation(_) => 9,
            Self::InvalidSameTargetRelocations(_) => 10,
            Self::OpinionAtRelocationSource(_) => 11,
            Self::ArcToProhibitedChild(_) => 12,
            Self::InconsistentPropertyType(_) => 13,
            Self::InvalidExternalTargetPath(_) => 14,
            Self::InvalidInstanceTargetPath(_) => 15,
            Self::VariableExpressionError(_) => 16,
            Self::InvalidSublayerAndOffsetCount(_) => 17,
            Self::MissingLayer(_) => 18,
            Self::LimitExceeded(_) => 19,
        }
    }
}

impl CompositionError {
    /// The error in words, its paths and names written as text with
    /// `store`, the store the stage was composed from ("/World/Moss: the
    /// reference to @./moss.usda@ authored at /World/Moss in layer 0 does
    /// not resolve; it contributes nothing").
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

/// [`CompositionError::code`] by [`CompositionError::rank`].
const CODES: [&str; 20] = [
    "composition.foreign_schema_registry",
    "composition.sublayer_cycle",
    "composition.arc_cycle",
    "composition.unresolved_default_prim",
    "composition.unresolved_prim_path",
    "composition.unresolved_asset",
    "composition.muted_asset",
    "composition.unresolved_sublayer",
    "composition.invalid_authored_relocation",
    "composition.invalid_conflicting_relocation",
    "composition.invalid_same_target_relocations",
    "composition.opinion_at_relocation_source",
    "composition.arc_to_prohibited_child",
    "composition.inconsistent_property_type",
    "composition.invalid_external_target_path",
    "composition.invalid_instance_target_path",
    "composition.variable_expression",
    "composition.invalid_sublayer_and_offset_count",
    "composition.missing_layer",
    "composition.limit_exceeded",
];

/// The arc as an error names it.
pub(crate) fn arc_name(arc: ArcKind) -> &'static str {
    match arc {
        ArcKind::Local => "local opinion",
        ArcKind::Inherits => "inherit",
        ArcKind::Variants => "variant",
        ArcKind::Relocates => "relocate",
        ArcKind::References => "reference",
        ArcKind::Payloads => "payload",
        ArcKind::Specializes => "specializes",
    }
}

impl Describe for CompositionError {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        let target = |target: Option<PathId>| match target {
            Some(path) => names.path(path),
            None => String::from("<>"),
        };
        match self {
            Self::ForeignSchemaRegistry => f.write_str(
                "the schema registry was built on another token interner than the store's; \
                 the stage was composed without schemas",
            ),
            Self::SublayerCycle(error) => write!(
                f,
                "{}: sublayer {} would form a cycle; it is ignored",
                names.layer(error.layer),
                names.layer(error.sublayer)
            ),
            Self::ArcCycle(error) => {
                write!(f, "{}: an arc cycle is ignored:", names.path(error.prim))?;
                for site in &error.sites {
                    if let Some(arc) = site.arc {
                        write!(f, " -> {}", arc_name(arc))?;
                    }
                    write!(
                        f,
                        " {} in {}",
                        names.path(site.path),
                        names.layer(site.layer_stack)
                    )?;
                }
                Ok(())
            }
            Self::UnresolvedDefaultPrim(error) => {
                write!(
                    f,
                    "{}: the {} to {} has no prim path, and its defaultPrim ",
                    names.path(error.prim),
                    arc_name(error.arc),
                    names.layer(error.layer)
                )?;
                match error.path {
                    Some(path) => write!(f, "names {}, which has no prim spec", names.path(path))?,
                    None => f.write_str("is not authored or not a prim path")?,
                }
                f.write_str("; it contributes nothing")
            }
            Self::UnresolvedPrimPath(error) => {
                write!(
                    f,
                    "{}: the {} authored at {}",
                    names.path(error.prim),
                    arc_name(error.arc),
                    names.spec(&error.introducing_spec)
                )?;
                if let Some(layer) = error.introducing_layer {
                    write!(f, " in {}", names.layer(layer))?;
                }
                write!(
                    f,
                    " targets {} in {}, which has no prim there; it contributes nothing",
                    names.path(error.path),
                    names.layer(error.layer)
                )
            }
            Self::UnresolvedAsset(error) => write!(
                f,
                "{}: the {} to @{}@ authored at {} in {} does not resolve; it contributes nothing",
                names.path(error.prim),
                arc_name(error.arc),
                error.asset_path,
                names.spec(&error.introducing_spec),
                names.layer(error.introducing_layer)
            ),
            Self::MutedAsset(error) => write!(
                f,
                "{}: the {} authored at {} in {} targets {}, which the stage mutes; \
                 it contributes nothing",
                names.path(error.prim),
                arc_name(error.arc),
                names.spec(&error.introducing_spec),
                names.layer(error.introducing_layer),
                names.layer(error.layer)
            ),
            Self::UnresolvedSublayer(error) => write!(
                f,
                "{}: sublayer @{}@ does not resolve; it is ignored",
                names.layer(error.layer),
                error.asset_path
            ),
            Self::LimitExceeded(error) => match error.limit {
                CompositionLimit::PrimIndexNodes => write!(
                    f,
                    "{}: composition reached its limit of {} prim index nodes \
                     (`prim_index_nodes`); the prim's children, and the prims \
                     after it, are not composed",
                    names.path(error.prim),
                    error.value
                ),
                CompositionLimit::NodesPerPrimIndex => write!(
                    f,
                    "{}: the prim's index reached {} nodes, the limit of one index \
                     (`nodes_per_prim_index`); it has no more arcs, and its children are \
                     not composed",
                    names.path(error.prim),
                    error.value
                ),
                CompositionLimit::ComposedDepth => write!(
                    f,
                    "{}: the prim is {} levels below the deepest spec its sites author, \
                     the composed depth limit (`composed_depth`); its children are not \
                     composed",
                    names.path(error.prim),
                    error.value
                ),
            },
            Self::MissingLayer(error) => match error.sublayer_of {
                Some(parent) => write!(
                    f,
                    "{}: sublayer {} is not in the store; it is ignored",
                    names.layer(parent),
                    names.layer(error.layer)
                ),
                None => write!(
                    f,
                    "{} is not in the store; its layer stack has no opinions from it",
                    names.layer(error.layer)
                ),
            },
            Self::InvalidSublayerAndOffsetCount(error) => write!(
                f,
                "{}: {} sublayers and {} sublayer offsets; every sublayer takes the identity offset",
                names.layer(error.layer),
                error.sublayers,
                error.offsets
            ),
            Self::InvalidAuthoredRelocation(error) => write!(
                f,
                "{}: the relocate {} -> {} is invalid: {}; it is ignored",
                names.layer(error.layer),
                names.path(error.source),
                target(error.target),
                error.reason
            ),
            Self::InvalidConflictingRelocation(error) => write!(
                f,
                "{}: the relocate {} -> {} conflicts with {} -> {} in {}: {}; it is ignored",
                names.layer(error.layer),
                names.path(error.source),
                target(error.target),
                names.path(error.conflict_source),
                target(error.conflict_target),
                names.layer(error.conflict_layer),
                error.reason
            ),
            Self::InvalidSameTargetRelocations(error) => {
                write!(f, "relocates move")?;
                for (i, (layer, source)) in error.sources.iter().enumerate() {
                    let sep = if i == 0 { " " } else { ", " };
                    write!(
                        f,
                        "{sep}{} (in {})",
                        names.path(*source),
                        names.layer(*layer)
                    )?;
                }
                write!(
                    f,
                    " to the one target {}; they are all ignored",
                    names.path(error.target)
                )
            }
            Self::OpinionAtRelocationSource(error) => write!(
                f,
                "{}: {} authors opinions at the relocation source {}; they are ignored",
                names.path(error.prim),
                names.layer(error.layer),
                names.path(error.path)
            ),
            Self::ArcToProhibitedChild(error) => write!(
                f,
                "{}: the {} to {} in {} targets the relocation source {} or beneath it; \
                 it contributes nothing",
                names.path(error.prim),
                arc_name(error.arc),
                names.path(error.target),
                names.layer(error.layer_stack),
                names.path(error.relocation_source)
            ),
            Self::InconsistentPropertyType(error) => write!(
                f,
                "{}: the spec at {} in {} is not {}, as the defining spec at {} in {} is; \
                 it is ignored",
                names.property(PropertyPath::new(error.prim, error.property)),
                names.spec(&error.conflicting_spec),
                names.layer(error.conflicting_layer),
                match error.defining_kind {
                    PropertyKind::Attribute => "an attribute",
                    PropertyKind::Relationship => "a relationship",
                },
                names.spec(&error.defining_spec),
                names.layer(error.defining_layer)
            ),
            Self::InvalidExternalTargetPath(error) => write!(
                f,
                "{}: the target {} authored at {} in {} cannot be mapped across the {}; \
                 it is ignored",
                names.property(PropertyPath::new(error.prim, error.property)),
                names.target(error.target),
                names.spec(&error.spec),
                names.layer(error.layer),
                arc_name(error.arc)
            ),
            Self::InvalidInstanceTargetPath(error) => write!(
                f,
                "{}: the target {} authored at {} in {} names an instance of its class; \
                 it is ignored",
                names.property(PropertyPath::new(error.prim, error.property)),
                names.target(error.target),
                names.spec(&error.spec),
                names.layer(error.layer)
            ),
            Self::VariableExpressionError(error) => {
                if let Some(prim) = error.prim {
                    write!(f, "{}: ", names.path(prim))?;
                }
                let context = match error.context {
                    ExpressionContext::Sublayer => "sublayer",
                    ExpressionContext::Reference => "reference",
                    ExpressionContext::Payload => "payload",
                    ExpressionContext::VariantSelection => "variant selection",
                    ExpressionContext::AssetValue => "asset value",
                };
                write!(
                    f,
                    "the {context} expression {} in {} does not evaluate ({}); it is ignored",
                    error.expression,
                    names.layer(error.layer),
                    error.error
                )
            }
        }
    }
}

/// Puts `errors` in the order a stage reports them (see
/// [`Stage::composition_errors`]): the errors in layer stacks first, then
/// each composed prim's, in stage traversal order (`position`) and then, for
/// prims traversal does not reach, in namespace order
/// ([`Path::cmp_with_tokens`]); each prim's by kind and then in the order
/// found.
///
/// Composition finds errors in an order that depends on how it visits
/// hashed collections, and a stage recomposed in part merges in the errors
/// of the prims it recomposed. Sorting by what each error is about gives one
/// order for both.
///
/// [`Stage::composition_errors`]: crate::Stage::composition_errors
/// [`Path::cmp_with_tokens`]: crate::Path::cmp_with_tokens
pub(crate) fn order_errors(
    errors: &mut [CompositionError],
    position: &HashMap<PathId, usize>,
    paths: &PathInterner,
    tokens: &TokenInterner,
) {
    let place = |prim: PathId| position.get(&prim).copied().unwrap_or(usize::MAX);
    sort::sort_by(errors, |a, b| {
        let prims = match (a.prim(), b.prim()) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => place(a)
                .cmp(&place(b))
                .then_with(|| paths.resolve(a).cmp_with_tokens(paths.resolve(b), tokens)),
        };
        prims.then_with(|| a.rank().cmp(&b.rank()))
    });
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
#[non_exhaustive]
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

/// A reference or payload whose authored prim path names no prim in the
/// target layer stack, found while composing `prim`.
///
/// The arc is followed: it targets `path` in the layer stack whose root
/// layer is `layer`, but composing that site brings in no prim spec,
/// neither from a layer of that stack nor through the arcs authored on
/// `path` and its ancestors there (a site reached only across a cycle has
/// none). It contributes no opinions and the rest of the prim is composed
/// as normal. An arc with no authored prim path reports
/// [`UnresolvedDefaultPrim`] instead.
///
/// Spec: AOUSD Core §10.3.2.1 ("If there are no specs in any of the layers
/// of the referenced layer stack for the reference prim path, it is a
/// composition error and that reference is ignored"), §10.3.2.2 (payloads
/// behave as references), §10.6. OpenUSD reports it as
/// `PcpErrorUnresolvedPrimPath` (`_EvalUnresolvedPrimPathError` in
/// `pxr/usd/pcp/primIndex.cpp`), once for each composed prim whose
/// composition reaches the arc, including arcs nested in other arcs'
/// targets. An arc is checked for the prim it is authored on, not for the
/// descendants it reaches ancestrally.
///
/// The introducing layer and variant-qualified authored spec identify the
/// source independently of the composed occurrence. Diagnostics own that
/// spec and implement `Clone`; clone retained diagnostics explicitly.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UnresolvedPrimPath {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The arc: [`ArcKind::References`] or [`ArcKind::Payloads`].
    pub arc: ArcKind,
    /// The root layer of the target layer stack: the referenced asset, or
    /// the root of the authoring layer stack for an internal arc.
    pub layer: LayerId,
    /// The prim path the arc names.
    pub path: PathId,
    /// Layer whose list edit adds the unresolved reference or payload.
    /// `None` means no introducing authored spec could be identified.
    pub introducing_layer: Option<LayerId>,
    /// Spec that introduces the arc, including any enclosing variant selections.
    /// This remains in its authored namespace when `prim` is a referenced copy.
    pub introducing_spec: SpecPath,
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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
/// authoring layer, whatever its target. An asset that is not found and
/// one that is found but cannot be read are both reported this way.
///
/// Spec: AOUSD Core §10.3.2.1 ("If a layer stack cannot be computed for a
/// reference's layer asset path, it is a composition error and that
/// reference is ignored"), §10.3.2.2 (payloads), §10.6. OpenUSD reports it
/// as `PcpErrorInvalidAssetPath` (`_EvalRefOrPayloadArcs` in
/// `pxr/usd/pcp/primIndex.cpp`), whose `sourceLayer` and `site` are
/// `introducing_layer` and `introducing_spec` here.
///
/// [`Reference::unresolved`]: crate::Reference::unresolved
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UnresolvedAsset {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The arc: [`ArcKind::References`] or [`ArcKind::Payloads`].
    pub arc: ArcKind,
    /// The asset path as authored, or as a variable expression evaluates
    /// it, normalized as list editing compares it (`./a/../b.usd` reads
    /// `./b.usd`). A relative path is relative to `introducing_layer`.
    pub asset_path: String,
    /// The layer whose list edit adds the arc, which `asset_path` is anchored
    /// to. For an arc reached through another arc it is a layer of that
    /// arc's target, not of the stage's layer stack.
    pub introducing_layer: LayerId,
    /// The spec in `introducing_layer` that authors the arc, including any
    /// enclosing variant selections (`/Rock{look=mossy}`). This remains in
    /// its authored namespace when `prim` is a referenced copy.
    pub introducing_spec: SpecPath,
}

/// A reference or payload whose target layer the stage mutes
/// ([`StageOptions::muted_layers`]), found while composing `prim`.
///
/// Muting hides a layer on purpose, so the arc is not followed: it adds no
/// node to the prim index and contributes no opinions, and the rest of the
/// prim is composed as normal; [`LiveStage::unmute_layer`] recomposes the
/// prims whose arcs reach it. An internal arc is never muted, and a muted sublayer or
/// session layer is skipped without an error, as OpenUSD skips it
/// (`PcpLayerStack::_BuildLayerStack`).
///
/// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads), §10.6
/// (composition errors). OpenUSD reports it as `PcpErrorMutedAssetPath`
/// (`_EvalRefOrPayloadArcs` in `pxr/usd/pcp/primIndex.cpp`, "Asset @...@
/// was muted for reference introduced by ..."), whose `sourceLayer` and
/// `site` are `introducing_layer` and `introducing_spec` here and whose
/// `resolvedAssetPath`, the muted layer's identifier, is `layer`.
///
/// [`StageOptions::muted_layers`]: crate::StageOptions::muted_layers
/// [`LiveStage::unmute_layer`]: crate::LiveStage::unmute_layer
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct MutedAsset {
    /// The composed prim whose composition reached the arc.
    pub prim: PathId,
    /// The arc: [`ArcKind::References`] or [`ArcKind::Payloads`].
    pub arc: ArcKind,
    /// The muted layer the arc's asset path resolves to.
    pub layer: LayerId,
    /// The layer whose list edit adds the arc. For an arc reached through
    /// another arc it is a layer of that arc's target, not of the stage's
    /// layer stack.
    pub introducing_layer: LayerId,
    /// The spec in `introducing_layer` that authors the arc, including any
    /// enclosing variant selections. This remains in its authored namespace
    /// when `prim` is a referenced copy.
    pub introducing_spec: SpecPath,
}

/// A sublayer whose asset path could not be resolved, found when gathering
/// a layer stack.
///
/// Importers keep such a sublayer as a [`SublayerEntry::unresolved`] instead
/// of dropping it, and record the miss in the store
/// ([`InMemoryStore::mark_asset_missing`]); one the store has no answer for
/// yet is asked for instead ([`Stage::asset_requests`]). It contributes no
/// layer, and the rest of the layer stack, including the sublayers after
/// it, is gathered as usual. Each layer stack that reaches it reports it.
///
/// [`InMemoryStore::mark_asset_missing`]: crate::InMemoryStore::mark_asset_missing
/// [`Stage::asset_requests`]: crate::Stage::asset_requests
///
/// Spec: AOUSD Core §10.3.1 (sublayers), §10.6 (composition errors).
/// OpenUSD reports it as `PcpErrorInvalidSublayerPath`
/// (`PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`), with
/// the layer that names the sublayer and the authored path.
///
/// [`SublayerEntry::unresolved`]: crate::SublayerEntry::unresolved
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UnresolvedSublayer {
    /// The layer whose `subLayers` names the sublayer.
    pub layer: LayerId,
    /// The sublayer's asset path as authored.
    pub asset_path: String,
}

/// A layer that a layer stack names by id but the store neither holds
/// ([`LayerStore::layer`]) nor reports loading
/// ([`LayerStore::is_layer_pending`]), found when gathering the stack.
///
/// The root of a layer stack (the stage's root layer, its session layer,
/// or a reference or payload target composition reached by id) or a
/// sublayer a [`SublayerEntry::new`] names by id. The layer contributes no
/// opinions and no sublayers, and the rest of the stack is gathered as
/// usual. A muted layer ([`StageOptions::muted_layers`]) is hidden on
/// purpose and is not reported; a sublayer authored by asset path that
/// does not resolve is an [`UnresolvedSublayer`] instead.
///
/// Spec: AOUSD Core §10.3.1 (sublayers), §10.6 (composition errors).
/// OpenUSD has no layer ids: `UsdStage::Open` fails on a root layer it
/// cannot open, and a sublayer it cannot open is
/// `PcpErrorInvalidSublayerPath` ("Could not load sublayer ... skipping",
/// `PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`).
///
/// [`LayerStore::layer`]: crate::LayerStore::layer
/// [`LayerStore::is_layer_pending`]: crate::LayerStore::is_layer_pending
/// [`SublayerEntry::new`]: crate::SublayerEntry::new
/// [`StageOptions::muted_layers`]: crate::StageOptions::muted_layers
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct MissingLayer {
    /// The layer the store does not hold.
    pub layer: LayerId,
    /// The layer whose `subLayers` names it; `None` for the root or
    /// session layer of a layer stack.
    pub sublayer_of: Option<LayerId>,
}

/// A prim at which composition reached one of its
/// [`StageOptions::limits`] (see [`crate::composition_limits`]).
///
/// For [`CompositionLimit::ComposedDepth`], `prim` is `value` levels
/// below the deepest spec its layers author at a site of its index (or an
/// ancestor of one) and names children, which are not composed; the
/// stage's other prims compose as usual. For
/// [`CompositionLimit::NodesPerPrimIndex`], `prim`'s index holds `value`
/// nodes and lacks the arcs that would have added more; its children are
/// not composed, and other prims compose as usual. For
/// [`CompositionLimit::PrimIndexNodes`], `prim` is the prim whose index
/// brought the nodes composition built to `value`: it is the last prim
/// composed, without children. Either way the prim is
/// on the stage, and the limit is the same on any number of threads.
///
/// Spec: AOUSD Core §10.6 (composition errors), §11 (stage population).
/// OpenUSD has no counterpart: Pcp composes without a limit, until the
/// process runs out of memory when the namespace is endless.
///
/// [`StageOptions::limits`]: crate::StageOptions::limits
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct LimitExceeded {
    /// The limit reached.
    pub limit: CompositionLimit,
    /// The limit's value: its field in [`crate::CompositionLimits`].
    pub value: usize,
    /// The prim at which composition reached it.
    pub prim: PathId,
}

/// A layer whose `subLayers` and `subLayerOffsets` counts differ, found
/// when gathering a layer stack holding it ([`Layer::unpaired_sublayer_offsets`]).
///
/// The offsets cannot be paired with the sublayers, so every sublayer of
/// `layer` takes the identity offset; the sublayers are gathered as usual.
///
/// Spec: AOUSD Core §7.6.1.2.2 (`subLayerOffsets` parallels `subLayers`),
/// §10.6 (composition errors). OpenUSD 26.11 reports it as
/// `PcpErrorInvalidSublayerAndOffsetCount` (`PcpLayerStack::_BuildLayerStack`,
/// `pxr/usd/pcp/layerStack.cpp`), before the layer's sublayers' errors.
///
/// [`Layer::unpaired_sublayer_offsets`]: crate::Layer::unpaired_sublayer_offsets
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct InvalidSublayerAndOffsetCount {
    /// The layer whose sublayers and offsets do not pair.
    pub layer: LayerId,
    /// How many sublayers `layer` authors.
    pub sublayers: usize,
    /// How many sublayer offsets `layer` authors.
    pub offsets: usize,
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
#[non_exhaustive]
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
#[non_exhaustive]
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

impl fmt::Display for InvalidRelocationReason {
    /// The reason in words, of the relocate: "its target is its source".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RootPrimSource => "its source is a root prim",
            Self::TargetIsSource => "its target is its source",
            Self::TargetIsAncestor => "its target is an ancestor of its source",
            Self::TargetIsDescendant => "its target is a descendant of its source",
        })
    }
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
#[non_exhaustive]
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
#[non_exhaustive]
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

impl fmt::Display for RelocationConflict {
    /// The conflict in words, of the entry and the other one: "its target
    /// is the other's source".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TargetIsConflictSource => "its target is the other's source",
            Self::SourceIsConflictTarget => "its source is the other's target",
            Self::TargetIsConflictSourceDescendant => "its target is beneath the other's source",
            Self::SourceIsConflictSourceDescendant => "its source is beneath the other's source",
        })
    }
}

/// `layerRelocates` entries of one layer stack that move different sources
/// to the same target, found when computing that layer stack's relocation
/// table. Every one of them is ignored.
///
/// Spec: AOUSD Core §10.3.2.6 ("each target may only have one possible
/// source path"). OpenUSD reports it as
/// `PcpErrorInvalidSameTargetRelocations`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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

/// A property spec whose kind differs from the kind of the property's
/// strongest spec, found while composing the property `property` of `prim`.
///
/// A property is an attribute or a relationship. Its strongest spec (the
/// defining spec) decides which; every weaker spec of the other kind is
/// ignored, so it contributes nothing to the property's stack, value or
/// targets, and each is reported. The property's other specs compose as
/// normal.
///
/// Spec: AOUSD Core §7.6.3 (a property spec is an attribute spec or a
/// relationship spec), §10.6. OpenUSD reports it as
/// `PcpErrorInconsistentPropertyType` and ignores the conflicting spec
/// (`PcpPropertyIndex`'s `_GetPrimProperty` in
/// `pxr/usd/pcp/propertyIndex.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct InconsistentPropertyType {
    /// The composed prim that owns the property.
    pub prim: PathId,
    /// The property's name.
    pub property: TokenId,
    /// The layer of the defining spec.
    pub defining_layer: LayerId,
    /// The defining spec's path in that layer.
    pub defining_spec: SpecPath,
    /// The defining spec's kind, which the property takes.
    pub defining_kind: PropertyKind,
    /// The layer of the ignored spec.
    pub conflicting_layer: LayerId,
    /// The ignored spec's path in that layer.
    pub conflicting_spec: SpecPath,
}

/// A relationship target or attribute connection path authored inside an
/// arc's target that cannot be mapped into the composed namespace, found
/// while composing the property `property` of `prim`.
///
/// An arc maps its target prim, and the paths beneath it, onto its
/// destination. A reference or payload to another layer stack maps no
/// other path. An internal reference or payload, an inherit and a
/// specializes map every other path to itself, except a path beneath the
/// destination, which would not map back to where it was authored. A
/// target path that does not map is removed from the property's list edit
/// (a deletion is dropped without an error); the rest of the property
/// composes as normal.
///
/// Spec: AOUSD Core §10.3.2 (an arc maps its target's namespace onto its
/// destination), §10.6. OpenUSD reports it as
/// `PcpErrorInvalidExternalTargetPath` and ignores the path
/// (`_PathTranslateCallback` in `pxr/usd/pcp/targetIndex.cpp`; the mapping
/// is `PcpMapFunction`, with the root identity added for internal and class
/// arcs in `pxr/usd/pcp/primIndex.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct InvalidExternalTargetPath {
    /// The composed prim that owns the property.
    pub prim: PathId,
    /// The property's name.
    pub property: TokenId,
    /// The target path as authored, in the namespace of the arc's target.
    pub target: TargetPath,
    /// The property spec that authors the path, as the property's stack
    /// names it ([`crate::Property::opinions`]).
    pub spec: SpecPath,
    /// The arc the path could not be mapped across.
    pub arc: ArcKind,
    /// The layer of the property spec that authors the path.
    pub layer: LayerId,
}

/// A relationship target or attribute connection path authored in an
/// inherited class that targets an instance of that class, found while
/// composing the property `property` of `prim`.
///
/// A class maps every path outside itself to itself (see
/// [`InvalidExternalTargetPath`]), so a path authored in the class to a
/// prim that also inherits the class names one instance from all of them.
/// Such a path is removed from the property's list edit of every prim that
/// inherits the class; the rest of the property composes as normal.
///
/// Spec: AOUSD Core §10.3.2.4 (inherits), §10.6. OpenUSD reports it as
/// `PcpErrorInvalidInstanceTargetPath` and ignores the path
/// (`_TargetInClassAndTargetsInstance` in `pxr/usd/pcp/targetIndex.cpp`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct InvalidInstanceTargetPath {
    /// The composed prim that owns the property.
    pub prim: PathId,
    /// The property's name.
    pub property: TokenId,
    /// The target path, which names an instance of the class.
    pub target: TargetPath,
    /// The class's property spec that authors the path, as the property's
    /// stack names it ([`crate::Property::opinions`]).
    pub spec: SpecPath,
    /// The layer of the class's property spec that authors the path.
    pub layer: LayerId,
}

/// Where a variable expression that failed to evaluate is authored (see
/// [`VariableExpressionError`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExpressionContext {
    /// A sublayer asset path.
    Sublayer,
    /// A reference asset path.
    Reference,
    /// A payload asset path.
    Payload,
    /// A variant selection.
    VariantSelection,
    /// An asset path value: an attribute's default or time sample, an
    /// element of an asset array, or an entry of a metadata dictionary
    /// such as `customData` or `assetInfo`. It reads as authored, which
    /// names no asset (see [`crate::variable_expression`]).
    AssetValue,
}

/// A variable expression that failed to evaluate: a sublayer, reference or
/// payload asset path, a variant selection, or an asset path value,
/// authored as an expression that does not parse, uses a variable that is
/// not set, or does not evaluate to a string.
///
/// The sublayer or arc is ignored, and a variant selection is ignored in
/// favor of the next weaker selection; the rest of the layer stack or prim
/// composes as normal. An expression that evaluates to no value or to an
/// empty string is not an error: the sublayer or arc is dropped silently.
/// An asset path value that fails reads as authored, naming no asset;
/// OpenUSD reports it as a warning when the value is read
/// (`Usd_AssetPathContext::ReportErrors` in `pxr/usd/usd/stage.cpp`), with
/// the context `asset value` here.
///
/// Spec: AOUSD Core §7.6.1.7 reserves `expressionVariables`; §10.6
/// (composition errors). OpenUSD reports it as
/// `PcpErrorVariableExpressionError` (`Pcp_EvaluateVariableExpression` in
/// `pxr/usd/pcp/utils.cpp`), with the context `sublayer`, `reference`,
/// `payload` or `variant`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct VariableExpressionError {
    /// The composed prim whose composition reached the expression, or
    /// `None` for a sublayer asset path, found gathering a layer stack.
    pub prim: Option<PathId>,
    /// Where the expression is authored.
    pub context: ExpressionContext,
    /// The layer that authors the expression.
    pub layer: LayerId,
    /// The expression as authored, backticks included.
    pub expression: String,
    /// The evaluation errors, joined with `"; "`.
    pub error: String,
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use crate::{InMemoryStore, PropertyPath};

    #[test]
    fn codes_are_distinct_dotted_snake_case() {
        let mut seen = alloc::collections::BTreeSet::new();
        for code in CODES {
            let (family, case) = code.split_once('.').expect("a family and a case");
            assert_eq!(family, "composition", "{code}");
            assert!(
                !case.is_empty() && case.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "{code}"
            );
            assert!(!case.ends_with("_error"), "{code} names its type");
            assert!(seen.insert(code), "{code} repeats");
        }
        assert_eq!(CODES[16], "composition.variable_expression");
        let error = CompositionError::UnresolvedSublayer(UnresolvedSublayer {
            layer: LayerId(0),
            asset_path: "./sub.usda".into(),
        });
        assert_eq!(error.code(), "composition.unresolved_sublayer");
        assert_eq!(
            CompositionError::ForeignSchemaRegistry.code(),
            "composition.foreign_schema_registry"
        );
    }

    #[test]
    fn errors_name_their_paths_in_words() {
        let store = InMemoryStore::default();
        let moss = store.path("/World/Moss");
        let unresolved = CompositionError::UnresolvedAsset(UnresolvedAsset {
            prim: moss,
            arc: ArcKind::References,
            asset_path: "./moss.usda".into(),
            introducing_layer: LayerId(0),
            introducing_spec: SpecPath::from_prim_path(moss),
        });
        assert_eq!(
            unresolved.display(&store).to_string(),
            "/World/Moss: the reference to @./moss.usda@ authored at /World/Moss in layer 0 \
             does not resolve; it contributes nothing"
        );
        let height = store.property_path("/World/Moss.height");
        let inconsistent = CompositionError::InconsistentPropertyType(InconsistentPropertyType {
            prim: moss,
            property: height.property(),
            defining_layer: LayerId(1),
            defining_spec: SpecPath::from_property_path(height),
            defining_kind: PropertyKind::Attribute,
            conflicting_layer: LayerId(2),
            conflicting_spec: SpecPath::from_property_path(PropertyPath::new(
                moss,
                height.property(),
            )),
        });
        assert_eq!(
            inconsistent.display(&store).to_string(),
            "/World/Moss.height: the spec at /World/Moss.height in layer 2 is not an attribute, \
             as the defining spec at /World/Moss.height in layer 1 is; it is ignored"
        );
        let (rock, pebble) = (store.path("/World/Rock"), store.path("/World/Rock/Pebble"));
        let invalid = CompositionError::InvalidAuthoredRelocation(InvalidAuthoredRelocation {
            layer: LayerId(1),
            source: pebble,
            target: Some(rock),
            reason: InvalidRelocationReason::TargetIsAncestor,
        });
        assert_eq!(
            invalid.display(&store).to_string(),
            "layer 1: the relocate /World/Rock/Pebble -> /World/Rock is invalid: its target is \
             an ancestor of its source; it is ignored"
        );
        let conflicting =
            CompositionError::InvalidConflictingRelocation(InvalidConflictingRelocation {
                layer: LayerId(1),
                source: rock,
                target: Some(moss),
                conflict_layer: LayerId(2),
                conflict_source: moss,
                conflict_target: Some(pebble),
                reason: RelocationConflict::TargetIsConflictSource,
            });
        assert_eq!(
            conflicting.display(&store).to_string(),
            "layer 1: the relocate /World/Rock -> /World/Moss conflicts with /World/Moss -> \
             /World/Rock/Pebble in layer 2: its target is the other's source; it is ignored"
        );
    }

    /// A layer a stack names by id that the store does not hold is an
    /// error, not an empty stage: OpenUSD 26.11's `UsdStage::Open` fails on
    /// a root layer it cannot open, and reports a sublayer it cannot open
    /// as `PcpErrorInvalidSublayerPath` and skips it.
    #[test]
    fn missing_root_and_sublayer_layers_are_reported() {
        use crate::{Layer, PrimSpec, Reference, Stage, StageOptions, SublayerEntry};

        let missing = |errors: &[CompositionError]| -> Vec<(LayerId, Option<LayerId>)> {
            errors
                .iter()
                .filter_map(|error| match error {
                    CompositionError::MissingLayer(missing) => {
                        Some((missing.layer, missing.sublayer_of))
                    }
                    _ => None,
                })
                .collect()
        };

        // An absent root: the stage is empty and says why.
        let mut store = InMemoryStore::default();
        let stage = Stage::compose(&mut store, LayerId(99), StageOptions::default());
        assert_eq!(missing(stage.composition_errors()), [(LayerId(99), None)]);
        let error = &stage.composition_errors()[0];
        assert_eq!(error.code(), "composition.missing_layer");
        assert_eq!(error.prim(), None);
        assert_eq!(
            error.display(&store).to_string(),
            "layer 99 is not in the store; its layer stack has no opinions from it"
        );

        // An absent sublayer by id is skipped; its siblings still compose.
        let mut store = InMemoryStore::default();
        let (kept, referenced) = (store.path("/Kept"), store.path("/Referenced"));
        let mut root = Layer::new(LayerId(1));
        root.sublayers_mut().extend([
            SublayerEntry::new(LayerId(2)),
            SublayerEntry::new(LayerId(3)),
        ]);
        let mut spec = PrimSpec::def();
        spec.references.make_mut().explicit =
            Some(alloc::vec![Reference::new(LayerId(4), referenced)]);
        root.insert_prim(store.path("/Ref"), spec);
        store.insert_layer(root);
        let mut sibling = Layer::new(LayerId(3));
        sibling.insert_prim(kept, PrimSpec::def());
        store.insert_layer(sibling);
        let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
        assert!(stage.has_prim(kept));
        // The reference's target layer is the root of its own stack.
        assert_eq!(
            missing(stage.composition_errors()),
            [(LayerId(4), None), (LayerId(2), Some(LayerId(1)))]
        );
        assert_eq!(
            stage.composition_errors()[1].display(&store).to_string(),
            "layer 1: sublayer layer 2 is not in the store; it is ignored"
        );

        // A muted layer is hidden on purpose, not missing.
        store.insert_layer(Layer::new(LayerId(2)));
        store.insert_layer(Layer::new(LayerId(4)));
        let options = StageOptions {
            muted_layers: [LayerId(2), LayerId(4)].into_iter().collect(),
            ..StageOptions::default()
        };
        let stage = Stage::compose(&mut store, LayerId(1), options);
        assert_eq!(missing(stage.composition_errors()), []);
    }

    /// A store that names its layers ([`LayerStore::layer_identifier`])
    /// has them written `@identifier@` in diagnostics, as OpenUSD writes
    /// layers; a layer it does not name keeps its id.
    #[test]
    fn errors_name_layers_by_identifier() {
        let mut store = InMemoryStore::default();
        let error = CompositionError::UnresolvedSublayer(UnresolvedSublayer {
            layer: LayerId(1),
            asset_path: "./sub.usda".into(),
        });
        assert_eq!(
            error.display(&store).to_string(),
            "layer 1: sublayer @./sub.usda@ does not resolve; it is ignored"
        );
        store.set_layer_identifier(LayerId(1), "./root.usda");
        assert_eq!(store.layer_identifier(LayerId(1)), Some("./root.usda"));
        assert_eq!(
            error.display(&store).to_string(),
            "@./root.usda@: sublayer @./sub.usda@ does not resolve; it is ignored"
        );
        let missing = CompositionError::MissingLayer(MissingLayer {
            layer: LayerId(2),
            sublayer_of: Some(LayerId(1)),
        });
        assert_eq!(
            missing.display(&store).to_string(),
            "@./root.usda@: sublayer layer 2 is not in the store; it is ignored"
        );
    }
}
