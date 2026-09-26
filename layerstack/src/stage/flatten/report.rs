// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What a flatten must guarantee, and what it did.
//!
//! [`FlattenRequirements`] declares the guarantees a caller needs;
//! [`FlattenReport`] records every finding of a flatten, whether it
//! succeeded or was refused.

use alloc::{string::String, vec::Vec};
use core::fmt;

use crate::{
    asset::AssetResolver,
    doc::{LayerId, LayerOffset},
    property::Variability,
    schema::SchemaRegistry,
};

/// The guarantees a flatten must meet.
///
/// [`Stage::flatten`](crate::Stage::flatten) refuses, listing every unmet
/// requirement ([`FlattenError::Refused`]), rather than return a layer that
/// breaks one. The default preserves instancing and animation exactly,
/// writes asset paths as authored and refuses any loss.
#[derive(Clone, Copy, Debug)]
pub struct FlattenRequirements<'a> {
    /// Whether instances keep sharing their prototypes.
    pub instancing: Instancing,
    /// Whether every time sample, spline and layer offset must reach the
    /// flattened layer exactly, in stage time. When `false`, animation a
    /// layer cannot hold yet is left out and reported as lost.
    pub exact_animation: bool,
    /// How asset paths are written.
    pub asset_paths: AssetPaths<'a>,
    /// Which losses refuse the flatten.
    pub losses: LossPolicy,
    /// The schemas whose property definitions the flattened layer
    /// declares, as OpenUSD's flatten declares them: a property the prim's
    /// schema defines is written not `custom`, an attribute with the
    /// schema's variability ([`Transformation::DefinedBySchema`]). `None`
    /// reads no schema.
    pub schemas: Option<&'a SchemaRegistry>,
}

impl PartialEq for FlattenRequirements<'_> {
    fn eq(&self, other: &Self) -> bool {
        let same_schemas = match (self.schemas, other.schemas) {
            (Some(a), Some(b)) => core::ptr::eq(a, b),
            (None, None) => true,
            _ => false,
        };
        self.instancing == other.instancing
            && self.exact_animation == other.exact_animation
            && self.asset_paths == other.asset_paths
            && self.losses == other.losses
            && same_schemas
    }
}

impl FlattenRequirements<'_> {
    /// These requirements relaxed just enough to accept `unmet`, the
    /// unmet requirements of a refused flatten: exact animation or
    /// anchoring is given up when `unmet` names it, and, since every unmet
    /// requirement is a loss, losses no remaining requirement names are
    /// accepted ([`LossPolicy::RefuseRequired`]). Nothing else changes.
    /// Flattening the same stage again with the result succeeds and
    /// reports the same losses.
    #[must_use]
    pub fn accepting(&self, unmet: &[UnmetRequirement]) -> Self {
        let mut relaxed = *self;
        for unmet in unmet {
            relaxed.losses = LossPolicy::RefuseRequired;
            match unmet.requirement {
                Requirement::ExactAnimation => relaxed.exact_animation = false,
                Requirement::AnchoredAssetPaths => relaxed.asset_paths = AssetPaths::AsAuthored,
                Requirement::NoLoss => {}
            }
        }
        relaxed
    }
}

impl Default for FlattenRequirements<'_> {
    fn default() -> Self {
        Self {
            instancing: Instancing::Preserve,
            exact_animation: true,
            asset_paths: AssetPaths::AsAuthored,
            losses: LossPolicy::RefuseAny,
            schemas: None,
        }
    }
}

/// How instances are flattened.
///
/// Spec: AOUSD Core §11.3.3 (instances share a prototype).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Instancing {
    /// Each group of instances that compose the same prototype shares one
    /// root prim `Flattened_Prototype_N`, which each instance references
    /// internally, as OpenUSD's `UsdStage::Flatten` writes them.
    #[default]
    Preserve,
    /// Each instance is written with its own copy of its descendants; the
    /// flattened layer has no prototypes and no internal references.
    Expand,
}

/// How asset paths in values and metadata are written.
///
/// Spec: AOUSD Core §9.4 (a relative asset path is anchored to the layer
/// that authors it).
#[derive(Clone, Copy, Default)]
pub enum AssetPaths<'a> {
    /// As authored. A relative path keeps meaning what it meant only while
    /// the flattened layer sits beside the layer that authored it; every
    /// asset path is reported as an external dependency.
    #[default]
    AsAuthored,
    /// Anchored to the layer that authors each, through the resolver that
    /// knows where each layer is ([`AssetResolver::anchor_asset_path`]), as
    /// OpenUSD's flatten anchors them (`SdfAnchorAssetPaths`), in values,
    /// arrays, dictionaries and metadata alike. An asset path that cannot
    /// be anchored is an unmet requirement ([`Loss::UnanchoredAssetPath`]).
    Anchored(&'a dyn AssetResolver),
}

impl fmt::Debug for AssetPaths<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AsAuthored => f.write_str("AsAuthored"),
            Self::Anchored(_) => f.write_str("Anchored(..)"),
        }
    }
}

impl PartialEq for AssetPaths<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::AsAuthored, Self::AsAuthored) => true,
            (Self::Anchored(a), Self::Anchored(b)) => core::ptr::addr_eq(*a, *b),
            _ => false,
        }
    }
}

/// Which losses refuse a flatten.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LossPolicy {
    /// Any loss refuses the flatten.
    #[default]
    RefuseAny,
    /// Only a loss that breaks another declared requirement (such as
    /// [`FlattenRequirements::exact_animation`]) refuses; other losses are
    /// reported and the flatten succeeds.
    RefuseRequired,
}

/// A requirement a flatten could not meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    /// [`FlattenRequirements::exact_animation`].
    ExactAnimation,
    /// [`AssetPaths::Anchored`].
    AnchoredAssetPaths,
    /// [`LossPolicy::RefuseAny`].
    NoLoss,
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ExactAnimation => "exact animation",
            Self::AnchoredAssetPaths => "anchored asset paths",
            Self::NoLoss => "no loss",
        })
    }
}

/// A requirement a flatten could not meet, with the finding that breaks it.
#[derive(Clone, Debug, PartialEq)]
pub struct UnmetRequirement {
    /// The requirement.
    pub requirement: Requirement,
    /// The loss that breaks it.
    pub finding: Finding,
}

impl fmt::Display for UnmetRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.requirement, self.finding)
    }
}

/// Why a composed stage was not flattened.
#[derive(Clone, Debug, PartialEq)]
pub enum FlattenError {
    /// The root layer is not in the store.
    MissingRootLayer(LayerId),
    /// A declared requirement cannot be met.
    Refused(FlattenRefusal),
}

/// A refused flatten: every requirement it could not meet, and the report
/// of everything it found.
#[derive(Clone, Debug, PartialEq)]
pub struct FlattenRefusal {
    /// Every unmet requirement, in the order the stage was traversed.
    pub unmet: Vec<UnmetRequirement>,
    /// Everything the flatten found, as it would have reported it.
    pub report: FlattenReport,
}

impl fmt::Display for FlattenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRootLayer(id) => write!(f, "root layer {} is not in the store", id.0),
            Self::Refused(refusal) => {
                write!(
                    f,
                    "flatten refused: {} unmet requirement",
                    refusal.unmet.len()
                )?;
                if refusal.unmet.len() != 1 {
                    f.write_str("s")?;
                }
                for unmet in &refusal.unmet {
                    write!(f, "; {unmet}")?;
                }
                Ok(())
            }
        }
    }
}

impl core::error::Error for FlattenError {}

/// Everything a flatten found.
///
/// What was written exactly is counted ([`FlattenReport::preserved`]);
/// everything else is a [`Finding`]: a deliberate transformation, a loss,
/// or an asset the flattened layer still depends on. Paths are composed
/// stage paths as text, so a report means the same thing to any store.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FlattenReport {
    /// What was written exactly as composed.
    pub preserved: Preserved,
    /// Every transformation, loss and external dependency, in the order
    /// the stage was traversed.
    pub findings: Vec<Finding>,
}

impl FlattenReport {
    /// The findings of `category`.
    pub fn findings_in(&self, category: FindingCategory) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.kind.category() == category)
    }

    /// The deliberate transformations.
    pub fn transformed(&self) -> impl Iterator<Item = &Finding> {
        self.findings_in(FindingCategory::Transformed)
    }

    /// The losses: composed content the flattened layer does not hold.
    pub fn lost(&self) -> impl Iterator<Item = &Finding> {
        self.findings_in(FindingCategory::Lost)
    }

    /// The assets the flattened layer still refers to.
    pub fn external(&self) -> impl Iterator<Item = &Finding> {
        self.findings_in(FindingCategory::External)
    }

    /// Whether nothing was lost.
    #[must_use]
    pub fn is_lossless(&self) -> bool {
        self.lost().next().is_none()
    }

    /// The root prims the flatten added as instance prototypes, as paths.
    pub fn prototypes(&self) -> impl Iterator<Item = &str> {
        let mut seen: Vec<&str> = Vec::new();
        self.findings
            .iter()
            .filter_map(move |finding| match &finding.kind {
                FindingKind::Transformed(Transformation::InstanceShared { prototype })
                    if !seen.contains(&prototype.as_str()) =>
                {
                    seen.push(prototype);
                    Some(prototype.as_str())
                }
                _ => None,
            })
    }
}

/// What a flatten wrote exactly as composed, counted.
///
/// `prims` and `properties` count the specs written; what is transformed
/// inside one is a [`Finding`], not subtracted here. The other counts are of
/// fields, values, samples and paths written exactly as the strongest
/// opinion authors them; each one written otherwise is covered by a
/// finding instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Preserved {
    /// Prim specs written: the pseudo-root, every composed prim and every
    /// prototype.
    pub prims: usize,
    /// Property specs written.
    pub properties: usize,
    /// Layer, prim and property metadata fields written as authored.
    pub metadata_fields: usize,
    /// Attribute defaults written unchanged, value blocks included.
    pub defaults: usize,
    /// Time samples written at their authored times and values.
    pub time_samples: usize,
    /// Splines written unchanged.
    pub splines: usize,
    /// Relationship target and attribute connection paths written, from
    /// lists authored explicitly.
    pub targets: usize,
}

/// A composed prim, or a property of one, by path: text that means the
/// same thing in any store.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectPath {
    /// The composed prim path (`/World/Tree`), `/` for the layer itself.
    pub prim: String,
    /// The property name (`height`, `primvars:color`), for a property.
    pub property: Option<String>,
}

impl ObjectPath {
    /// Splits a composed prim or property path (`/A`, `/A.b`).
    pub(crate) fn from_composed(path: &str) -> Self {
        match path.split_once('.') {
            Some((prim, property)) => Self {
                prim: prim.into(),
                property: Some(property.into()),
            },
            None => Self {
                prim: path.into(),
                property: None,
            },
        }
    }
}

impl fmt::Display for ObjectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.prim)?;
        if let Some(property) = &self.property {
            write!(f, ".{property}")?;
        }
        Ok(())
    }
}

/// One thing a flatten did not write exactly as authored.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// The composed prim or property it concerns.
    pub path: ObjectPath,
    /// What was found.
    pub kind: FindingKind,
    /// The opinion that caused it, when one did.
    pub source: Option<FindingSource>,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.kind)?;
        if let Some(source) = &self.source {
            write!(f, " (layer {}, {})", source.layer.0, source.spec)?;
        }
        Ok(())
    }
}

/// The opinion behind a [`Finding`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindingSource {
    /// The layer that authors it, as the store the stage was composed from
    /// identifies it. The resolver that loaded the layer names it
    /// ([`AssetResolver::resolved_path`]).
    pub layer: LayerId,
    /// The spec in that layer that authors it (`/Asset{v=a}.height`).
    pub spec: String,
}

/// The kinds of [`Finding`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindingCategory {
    /// Written differently from how it was authored, on purpose, with the
    /// same composed meaning.
    Transformed,
    /// Composed but not written.
    Lost,
    /// An asset the flattened layer still refers to.
    External,
}

/// What a [`Finding`] records.
#[derive(Clone, Debug, PartialEq)]
pub enum FindingKind {
    /// A deliberate transformation.
    Transformed(Transformation),
    /// A loss.
    Lost(Loss),
    /// An external dependency.
    External(ExternalDependency),
}

impl FindingKind {
    /// The category of the finding.
    #[must_use]
    pub fn category(&self) -> FindingCategory {
        match self {
            Self::Transformed(_) => FindingCategory::Transformed,
            Self::Lost(_) => FindingCategory::Lost,
            Self::External(_) => FindingCategory::External,
        }
    }
}

impl fmt::Display for FindingKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transformed(t) => write!(f, "transformed: {t}"),
            Self::Lost(loss) => write!(f, "lost: {loss}"),
            Self::External(dependency) => write!(f, "external: {dependency}"),
        }
    }
}

/// A deliberate transformation: the flattened layer authors something
/// differently, and composes it the same.
#[derive(Clone, Debug, PartialEq)]
pub enum Transformation {
    /// A list op (a metadata field, relationship targets or attribute
    /// connections) written as the explicit list it composes to.
    ///
    /// Spec: AOUSD Core §12.2.6 (list op resolution).
    ListOpMadeExplicit {
        /// The field (`apiSchemas`, `targetPaths`, `connectionPaths`).
        field: String,
    },
    /// Time samples moved from layer time into stage time.
    ///
    /// Spec: AOUSD Core §12.3.2.1 (a layer's time `t` is stage time
    /// `t * scale + offset`).
    SamplesRetimed {
        /// The layer offset they were read through.
        offset: LayerOffset,
    },
    /// `timecode` values moved from layer time into stage time.
    ///
    /// Spec: AOUSD Core §12.3.2.1.
    TimeCodesRetimed {
        /// The layer offset they were read through.
        offset: LayerOffset,
    },
    /// The variant selections composition applied at the prim are baked
    /// in: the selected variants' opinions are written, and no variant set.
    ///
    /// Spec: AOUSD Core §10.5 (variant selection).
    VariantSelectionsBaked {
        /// Each variant set and the variant selected, in strength order.
        selections: Vec<(String, String)>,
    },
    /// An instance written as an internal reference to its prototype.
    ///
    /// Spec: AOUSD Core §11.3.3 (instances share a prototype).
    InstanceShared {
        /// The root prim holding the prototype (`/Flattened_Prototype_1`).
        prototype: String,
    },
    /// An instance written with its own copy of its descendants
    /// ([`Instancing::Expand`]).
    InstanceExpanded,
    /// An asset path anchored to the layer that authors it
    /// ([`AssetPaths::Anchored`]).
    ///
    /// Spec: AOUSD Core §9.4 (relative asset paths).
    AssetPathAnchored {
        /// The path as authored (`./bark.png`).
        authored: String,
        /// The path as written (`/assets/trees/bark.png`).
        anchored: String,
    },
    /// The property's `custom` written as OpenUSD's flatten writes it, the
    /// weakest opinion's; the stage's is `true` when any opinion's is.
    ///
    /// Spec: AOUSD Core §12.2.4 (`custom`).
    CustomFromWeakestOpinion {
        /// The `custom` written.
        custom: bool,
    },
    /// A property the prim's schema defines, written as its schema declares
    /// it: not `custom`, and an attribute with the schema's variability
    /// ([`FlattenRequirements::schemas`]).
    ///
    /// Spec: AOUSD Core §12.2.3 (variability), §12.2.4 (`custom`), §13.3
    /// (schema properties).
    DefinedBySchema {
        /// The variability written.
        variability: Variability,
    },
}

impl fmt::Display for Transformation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListOpMadeExplicit { field } => write!(f, "{field} made explicit"),
            Self::SamplesRetimed { offset } => write!(
                f,
                "time samples retimed (offset {}, scale {})",
                offset.offset, offset.scale
            ),
            Self::TimeCodesRetimed { offset } => write!(
                f,
                "timecode values retimed (offset {}, scale {})",
                offset.offset, offset.scale
            ),
            Self::VariantSelectionsBaked { selections } => {
                f.write_str("variant selections baked:")?;
                for (set, variant) in selections {
                    write!(f, " {set}={variant}")?;
                }
                Ok(())
            }
            Self::InstanceShared { prototype } => write!(f, "instance of {prototype}"),
            Self::InstanceExpanded => f.write_str("instance expanded"),
            Self::AssetPathAnchored { authored, anchored } => {
                write!(f, "@{authored}@ anchored as @{anchored}@")
            }
            Self::CustomFromWeakestOpinion { custom } => {
                write!(f, "custom = {custom}, from the weakest opinion")
            }
            Self::DefinedBySchema { variability } => {
                write!(f, "declared by its schema ({variability:?}, not custom)")
            }
        }
    }
}

/// Composed content a flattened layer does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loss {
    /// Time samples composed from sparse array edits, which would have to
    /// be composed into dense arrays sample by sample.
    ArrayEditSamples,
    /// A spline read through a layer offset or scale, whose knots would
    /// have to be retimed.
    RetimedSpline,
    /// Value clips (`clips`, `clipSets` and the legacy `clip*` fields),
    /// which composition does not read, so their values cannot be baked.
    ValueClips,
    /// An attribute that no opinion gives a type, which a layer cannot
    /// declare.
    UntypedAttribute,
    /// An asset path that [`AssetPaths::Anchored`] requires anchored and
    /// that could not be; it is written as authored.
    UnanchoredAssetPath,
}

impl Loss {
    /// The requirement the loss breaks besides [`Requirement::NoLoss`], if
    /// any.
    #[must_use]
    pub fn requirement(self) -> Option<Requirement> {
        match self {
            Self::ArrayEditSamples | Self::RetimedSpline | Self::ValueClips => {
                Some(Requirement::ExactAnimation)
            }
            Self::UnanchoredAssetPath => Some(Requirement::AnchoredAssetPaths),
            Self::UntypedAttribute => None,
        }
    }
}

impl fmt::Display for Loss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ArrayEditSamples => "time samples with sparse array edits",
            Self::RetimedSpline => "a spline through a layer offset",
            Self::ValueClips => "value clips",
            Self::UntypedAttribute => "an attribute without a type",
            Self::UnanchoredAssetPath => "an asset path that could not be anchored",
        })
    }
}

/// Something outside the flattened layer that it refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalDependency {
    /// An asset path value, as written.
    AssetPath(String),
}

impl fmt::Display for ExternalDependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AssetPath(path) => write!(f, "@{path}@"),
        }
    }
}
