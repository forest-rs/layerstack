// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Prospective property authoring sites and stronger value-bearing sources.
//!
//! This module owns composition-strength inspection, not value evaluation or
//! permission to write. Raw transactions may deliberately author masked values.

use alloc::{sync::Arc, vec::Vec};
use core::fmt;

use crate::{
    LayerId, LayerStore, Opinion, OpinionKey, PathId, PropertyKind, PropertyPath, Stage, Time,
    Value, edit::EditTarget, value_resolution::sample_bracket,
};

/// The authored slot a stronger source supplies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthoringSourceKind {
    /// An authored default, including a value block.
    Default,
    /// Nonempty samples; their keys and kinds are inspected without decoding.
    TimeSamples,
    /// A scalar spline, including blocked segments and extrapolation.
    Spline,
    /// A marker suppressing weaker animation while permitting weaker defaults.
    AnimationBlock,
    /// A clip set eligible through prepared metadata, or awaiting residency.
    ValueClips {
        /// The clip-set name on the metadata owner.
        set_name: Arc<str>,
    },
}

/// Whether a stronger source can compose over a weaker value.
///
/// This describes source shape, not compatibility with an unspecified proposed
/// value. A dense foreign-kind value may be ignored by a dictionary/array fold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceComposability {
    /// A dense value or block, without sparse operations of its own.
    Dense,
    /// A dictionary, array edit, or expression with a weaker reference.
    Composable,
    /// Mixed sample kinds, or clip metadata/residency not yet sufficient.
    Unknown,
}

/// One stronger value-bearing site, without materializing its numeric values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoringSource {
    /// Authoring layer, variant-qualified spec and composition-node identity.
    /// For clips this is the metadata anchor, not the raw sample layer.
    pub key: OpinionKey,
    /// The authored slot considered at the requested time.
    pub kind: AuthoringSourceKind,
    /// Whether this source has sparse/composable operations.
    pub composability: SourceComposability,
}

/// Where a prospective attribute write participates in this stage snapshot.
///
/// Sources describe stronger authored slots, not the contributors of the current
/// resolved value. A stronger sparse source need not completely mask a write;
/// callers that need actual contributors use the value explanation APIs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoringInspection {
    /// The strongest occurrence of the mapped target site, including a site
    /// whose layer has no prim or property spec yet. `None` means the target
    /// does not contribute: for example a muted layer or unselected variant.
    pub target: Option<OpinionKey>,
    /// Stronger value-bearing sources in composition order. Empty when the
    /// target does not contribute. Same-site slots are excluded: a write edits
    /// that site rather than competing with it.
    pub stronger_sources: Vec<AuthoringSource>,
}

impl AuthoringInspection {
    /// Whether the target contributes with no stronger value-bearing source.
    /// Does not validate the proposed value's type or the destination's editability.
    #[must_use]
    pub fn is_strongest(&self) -> bool {
        self.target.is_some() && self.stronger_sources.is_empty()
    }
}

/// An invalid prospective authoring query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthoringInspectionError {
    /// The store's token/path domains do not belong to this stage snapshot.
    ForeignStore,
    /// The stage has no composed prim at this path.
    MissingPrim(PathId),
    /// The store does not contain the target layer.
    MissingLayer(LayerId),
    /// The edit target does not map the property into its namespace.
    Unmappable(PropertyPath),
    /// A composed relationship cannot be inspected as an attribute value.
    NotAttribute(PropertyPath),
    /// A numeric query requires a finite time code.
    InvalidTime,
}

impl fmt::Display for AuthoringInspectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignStore => {
                f.write_str("authoring inspection store belongs to another token/path domain")
            }
            Self::MissingPrim(path) => write!(f, "no composed prim at {path:?}"),
            Self::MissingLayer(layer) => write!(f, "target layer {layer:?} is absent"),
            Self::Unmappable(path) => write!(f, "edit target does not map {path:?}"),
            Self::NotAttribute(path) => write!(f, "{path:?} is a relationship"),
            Self::InvalidTime => f.write_str("authoring inspection requires a finite time"),
        }
    }
}

impl core::error::Error for AuthoringInspectionError {}

impl Stage {
    /// Inspects a prospective attribute write through `target` at `time`.
    ///
    /// Unlike a value explanation, the target need not author a spec. The graph
    /// and resident layer-stack ordering establish its prospective strength.
    /// Repeated occurrences of the same mapped site use its strongest position.
    /// Default queries ignore samples, splines, animation blocks and clips;
    /// numeric queries inspect samples before splines before defaults.
    ///
    /// This reads snapshot metadata only: no asset loading, numeric-array decoding,
    /// interpolation or layer mutation. Mapping may intern paths in `store`.
    /// Inspect again after applying edits or changing residency; pending transaction
    /// operations are not part of the stage snapshot.
    ///
    /// AOUSD Core §10.4 (strength), §12.3 (source selection), §12.3.7 (clips).
    ///
    /// ```
    /// use layerstack::{InMemoryStore, Layer, LayerId, PrimSpec, Stage, StageOptions,
    ///     SublayerEntry, Time, edit::EditTarget};
    /// let mut store = InMemoryStore::default();
    /// let prim = store.path("/Settings");
    /// let property = store.property_path("/Settings.gain");
    /// let mut root = Layer::new(LayerId(1));
    /// root.insert_prim(prim, PrimSpec::def());
    /// root.sublayers_mut().push(SublayerEntry::new(LayerId(2)));
    /// store.insert_layer(root);
    /// store.insert_layer(Layer::new(LayerId(2)));
    /// let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    /// let inspection = stage.inspect_property_authoring(
    ///     &mut store, property, &EditTarget::for_layer(LayerId(2)), Time::Default,
    /// ).unwrap();
    /// assert!(inspection.is_strongest()); // The weak layer need not author a spec.
    /// ```
    ///
    /// # Errors
    /// Returns an error for absent prims/layers, unmappable targets, relationships
    /// and nonfinite numeric times. An inactive mapped site is a successful
    /// inspection with [`AuthoringInspection::target`] set to `None`.
    pub fn inspect_property_authoring(
        &self,
        store: &mut dyn LayerStore,
        property: PropertyPath,
        target: &EditTarget,
        time: Time,
    ) -> Result<AuthoringInspection, AuthoringInspectionError> {
        if self
            .store_identity
            .as_ref()
            .is_some_and(|identity| *identity != store.identity())
        {
            return Err(AuthoringInspectionError::ForeignStore);
        }
        if matches!(time, Time::At { code, .. } if !code.is_finite()) {
            return Err(AuthoringInspectionError::InvalidTime);
        }
        let prim = property.prim_path();
        let index = self
            .prims
            .get(&prim)
            .ok_or(AuthoringInspectionError::MissingPrim(prim))?;
        store
            .layer(target.layer())
            .ok_or(AuthoringInspectionError::MissingLayer(target.layer()))?;
        if self
            .resolve_property_declaration(prim, property.property())
            .is_some_and(|declaration| declaration.kind == PropertyKind::Relationship)
            || self
                .property_definition_ref(prim, property.property())
                .is_some_and(|definition| definition.kind == PropertyKind::Relationship)
        {
            return Err(AuthoringInspectionError::NotAttribute(property));
        }
        let mapped = target
            .map_property_to_spec_path(property, store.paths_mut())
            .ok_or(AuthoringInspectionError::Unmappable(property))?;
        let mapped_prim = mapped.prim_spec();
        let mut sites = Vec::new();
        for (id, node) in index.graph.nodes() {
            if node.site() != &mapped_prim
                || node.arc_kind() == crate::ArcKind::Relocates
                || index.graph.is_inert_specializes_site(id)
            {
                continue;
            }
            let Some(layer_strength) = node.layer_strength(target.layer()) else {
                continue;
            };
            // Instance descendants cannot regain occurrence-local opinions by
            // authoring a previously specless layer (AOUSD Core §11.3.3).
            if self.instances.iter().any(|ancestor| {
                *ancestor != prim
                    && store
                        .paths()
                        .resolve(*ancestor)
                        .is_prefix_of(store.paths().resolve(prim))
                    && !index.graph.has_direct_dependency(
                        id,
                        u16::try_from(store.paths().resolve(*ancestor).depth()).unwrap_or(u16::MAX),
                    )
            }) {
                continue;
            }
            sites.push(OpinionKey {
                node: id,
                layer_strength,
                layer_id: target.layer(),
                lookup_path: mapped_prim.prim_path(),
                spec_path: mapped_prim.clone(),
            });
        }
        // When composition selected a canonical occurrence of a duplicated
        // site, use that occurrence rather than a graph-only duplicate. Scope
        // this evidence to its stack: a different stack may legitimately have
        // no specs at the same namespace path yet.
        sites.retain(|site| {
            let node = index.graph.node(site.node).expect("candidate graph node");
            let matching_source = |source: &&OpinionKey| {
                index.graph.node(source.node).is_some_and(|source_node| {
                    source_node.site() == node.site()
                        && source_node.layer_stack_identifier() == node.layer_stack_identifier()
                })
            };
            let mut sources = index.sources.iter().filter(matching_source).peekable();
            sources.peek().is_none() || sources.any(|source| source.node == site.node)
        });
        index.graph.sort_keys(&mut sites);
        let Some(site) = sites.into_iter().next() else {
            return Ok(AuthoringInspection {
                target: None,
                stronger_sources: Vec::new(),
            });
        };
        let mut stronger_sources = Vec::new();
        let ordinary = index
            .property_opinions(property.property())
            .unwrap_or_default();
        for opinion in ordinary {
            if !index.graph.cmp_keys(&opinion.key, &site).is_lt() {
                continue;
            }
            if let Some((kind, composability)) = classify(opinion, time) {
                stronger_sources.push(AuthoringSource {
                    key: opinion.key.clone(),
                    kind,
                    composability,
                });
            }
        }
        if matches!(time, Time::At { .. }) {
            for (key, set_name, composability) in
                self.clips.authoring_sources(prim, property.property())
            {
                if !index.graph.cmp_keys(&key, &site).is_lt() {
                    continue;
                }
                // Ordinary authored values win over clips at their own site;
                // stronger animation blocks suppress all weaker clips.
                if ordinary.iter().any(|opinion| {
                    opinion.value.blocks_animation()
                        && index.graph.cmp_keys(&opinion.key, &key).is_le()
                        || opinion.key.node == key.node
                            && opinion.key.layer_strength == key.layer_strength
                            && classify(opinion, time).is_some()
                }) {
                    continue;
                }
                stronger_sources.push(AuthoringSource {
                    key,
                    kind: AuthoringSourceKind::ValueClips { set_name },
                    composability,
                });
            }
        }
        stronger_sources.sort_by(|a, b| index.graph.cmp_keys(&a.key, &b.key));
        Ok(AuthoringInspection {
            target: Some(site),
            stronger_sources,
        })
    }
}

fn shape(value: &Value) -> SourceComposability {
    if value.array_edit_ref().is_some() || matches!(value, Value::Dictionary(_)) {
        SourceComposability::Composable
    } else if let Value::PathExpression(text) = value {
        match crate::PathExpression::parse(text) {
            Ok(expression) if expression.contains_weaker_reference() => {
                SourceComposability::Composable
            }
            _ => SourceComposability::Dense,
        }
    } else {
        SourceComposability::Dense
    }
}

fn classify(opinion: &Opinion, time: Time) -> Option<(AuthoringSourceKind, SourceComposability)> {
    if let Time::At {
        code,
        interpolation,
    } = time
    {
        if let Some(samples) = opinion.value.time_samples() {
            let (lower, upper) =
                sample_bracket(samples, opinion.layer_offset.map_time(code), interpolation)?;
            let lower = shape(&samples[lower].1);
            let upper = shape(&samples[upper].1);
            return Some((
                AuthoringSourceKind::TimeSamples,
                if lower == upper {
                    lower
                } else {
                    SourceComposability::Unknown
                },
            ));
        }
        if opinion.value.spline().is_some() {
            return Some((AuthoringSourceKind::Spline, SourceComposability::Dense));
        }
        if opinion.value.blocks_animation() {
            return Some((
                AuthoringSourceKind::AnimationBlock,
                SourceComposability::Dense,
            ));
        }
    }
    opinion
        .value
        .default_value()
        .map(|value| (AuthoringSourceKind::Default, shape(value)))
}

#[cfg(test)]
mod tests;
