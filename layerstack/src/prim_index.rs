// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition result types.
//!
//! Composition produces per-prim indexes (similar to OpenUSD's internal
//! `PrimIndex`) which the [`crate::stage::Stage`] queries for population and
//! value resolution.
//!
//! Spec: AOUSD Core §10 (composition arcs and strength ordering) and §12 (value resolution).

use alloc::{boxed::Box, vec::Vec};
use core::cmp::Ordering;

use hashbrown::HashMap;

use crate::{
    doc::{FieldValue, LayerId, LayerOffset, Value},
    interner::TokenId,
    listop::ListOp,
    path::{PathId, TargetPath},
    property::{PropertySpec, PropertyType, TimeSample},
    spec_path::SpecPath,
    spline::SplineData,
};

/// Composition arc kind (LIVERPS ordering).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArcKind {
    /// Local opinions from the layer stack.
    Local,
    /// Inherits arc.
    Inherits,
    /// Variants arc.
    Variants,
    /// Relocates arc (not implemented in v0.1).
    Relocates,
    /// References arc.
    References,
    /// Payloads arc.
    Payloads,
    /// Specializes arc.
    Specializes,
}

impl ArcKind {
    pub(crate) fn strength_rank(self) -> u8 {
        match self {
            Self::Local => 0,
            Self::Inherits => 1,
            Self::Variants => 2,
            Self::Relocates => 3,
            Self::References => 4,
            Self::Payloads => 5,
            Self::Specializes => 6,
        }
    }
}

/// Where one specializes arc on an opinion's arc path is authored.
///
/// Opinions introduced by specializes arcs are globally weaker than every
/// other opinion of the prim, including opinions of other references and
/// payloads, and include the opinions of arcs authored inside the
/// specialized prim (AOUSD Core §10.4.1). OpenUSD implements this by leaving
/// an inert placeholder where the arc is authored and propagating the
/// specializes node to the root of the prim index, where it ranks after
/// every other arc (`pxr/usd/pcp/primIndex.cpp`, `_EvalImpliedSpecializes`;
/// `pxr/usd/pcp/strengthOrdering.cpp`, `PcpCompareSiblingNodeStrength`).
///
/// An origin identifies one such propagated node by the position of its
/// placeholder, ranked the way an [`OpinionKey`] of the placeholder would
/// be, and by the specializes arc's own index in its site's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecializesOrigin {
    /// Namespace depth of the prim the specializes node is propagated at.
    ///
    /// Deeper is stronger, as for [`OpinionKey::namespace_depth`].
    pub namespace_depth: u16,
    /// Arc kind the placeholder ranks under: the outermost arc that brings
    /// the authoring site into the prim index, or [`ArcKind::Specializes`]
    /// for a specializes authored in the composed prim's own layer stack.
    pub arc_kind: ArcKind,
    /// Nested arc kind the placeholder ranks under, as in
    /// [`OpinionKey::nested_arc_kind`]. A placeholder directly under an arc
    /// target is nested as [`ArcKind::Specializes`], so it ranks after the
    /// other arcs of that target, as OpenUSD orders a node's children.
    pub nested_arc_kind: Option<ArcKind>,
    /// Index of the outermost arc in its arc list.
    pub arc_list_index: u16,
    /// Index of the specializes arc in the authoring site's specializes list.
    pub specializes_index: u16,
    /// `true` when the specialized path is mapped into the namespace of the
    /// arc that introduces the authoring site (an implied specializes), as
    /// opposed to the propagated arc itself. OpenUSD ranks the implied node
    /// first (`PcpCompareSiblingNodeStrength`).
    pub implied: bool,
}

impl SpecializesOrigin {
    /// Compares origins with "strongest first" ordering.
    #[must_use]
    pub fn cmp_strongest_first(&self, other: &Self) -> Ordering {
        other
            .namespace_depth
            .cmp(&self.namespace_depth)
            .then_with(|| {
                self.arc_kind
                    .strength_rank()
                    .cmp(&other.arc_kind.strength_rank())
            })
            .then_with(|| cmp_nested_arc_kind(self.nested_arc_kind, other.nested_arc_kind))
            .then_with(|| self.arc_list_index.cmp(&other.arc_list_index))
            .then_with(|| self.specializes_index.cmp(&other.specializes_index))
            .then_with(|| other.implied.cmp(&self.implied))
    }
}

/// Orders nested arc kinds: no nesting is strongest, then LIVERPS order.
fn cmp_nested_arc_kind(a: Option<ArcKind>, b: Option<ArcKind>) -> Ordering {
    match (a, b) {
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(a), Some(b)) => a.strength_rank().cmp(&b.strength_rank()),
        (None, None) => Ordering::Equal,
    }
}

/// A comparable strength key for a single authored opinion.
///
/// Spec: AOUSD Core §10.4 (strength ordering and tie-breakers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpinionKey {
    /// `true` for local opinions (layer stack), `false` for opinions introduced by arcs.
    pub is_local: bool,
    /// The specializes arcs on this opinion's arc path, outermost first.
    ///
    /// Empty for opinions that no specializes arc introduces. Otherwise the
    /// opinion belongs to the specializes node the last origin names, and
    /// the remaining fields rank it within that node: [`Self::arc_kind`] is
    /// [`ArcKind::Specializes`] and [`Self::nested_arc_kind`] is the arc
    /// inside the specialized prim that introduces it, if any.
    ///
    /// Chains are ordered by their first differing origin (see
    /// [`SpecializesOrigin`]); a chain that extends another ranks right after
    /// it, so an opinion that no specializes introduces outranks every
    /// specializes node, and a nested node follows only its own enclosing
    /// node.
    ///
    /// Spec: AOUSD Core §10.4.1.
    pub specializes: Vec<SpecializesOrigin>,
    /// Arc kind of this opinion (for non-local opinions).
    pub arc_kind: ArcKind,
    /// Optional nested arc kind for opinions introduced inside another arc.
    ///
    /// This supports a single level of arc nesting (e.g. reference→inherit),
    /// treating shorter arc chains as stronger than longer ones when the outer
    /// arc kind ties. Arcs nested more deeply keep the outermost arc kind in
    /// [`OpinionKey::arc_kind`] and the first nested arc kind here, so a
    /// reference authored inside referenced content is
    /// `(References, Some(References))` and stays weaker than the referenced
    /// site's own opinions.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering within an arc's target).
    pub nested_arc_kind: Option<ArcKind>,
    /// Namespace depth of the site where the opinion is introduced (tie-breaker).
    ///
    /// For example, opinions introduced via a reference arc authored at `/A/B`
    /// are stronger than otherwise-identical opinions introduced at `/A`,
    /// regardless of which descendant prim paths they affect.
    ///
    /// Spec: AOUSD Core §10.4 (strength ordering tie-breakers).
    pub namespace_depth: u16,
    /// `true` for authored (vs implied) opinions.
    pub authored: bool,
    /// Index within an arc list (e.g. the Nth reference).
    pub arc_list_index: u16,
    /// Strength position within the relevant layer stack (0 is strongest).
    pub layer_strength: u16,
    /// Source layer identifier.
    pub layer_id: LayerId,
    /// Concrete prim path used to look up the authored `PrimSpec`.
    pub lookup_path: PathId,
    /// Source spec path identity used for composed provenance.
    pub spec_path: SpecPath,
}

impl OpinionKey {
    /// Returns a clone with a different provenance path.
    #[must_use]
    pub fn with_spec_path(&self, spec_path: SpecPath) -> Self {
        let mut out = self.clone();
        out.spec_path = spec_path;
        out
    }

    /// Compares keys with "strongest first" ordering.
    #[must_use]
    pub fn cmp_strongest_first(&self, other: &Self) -> Ordering {
        match (self.is_local, other.is_local) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }

        // Specializes nodes rank after every other node. Two chains are
        // ordered by their first differing origin, outermost first, as
        // OpenUSD orders sibling specializes nodes by their originating nodes
        // (`PcpCompareSiblingNodeStrength` in
        // `pxr/usd/pcp/strengthOrdering.cpp`). A chain that extends another
        // is a node nested in that node's specialized prim and ranks right
        // after it, before the enclosing node's weaker siblings
        // (AOUSD Core §10.4.1).
        let specializes = self
            .specializes
            .iter()
            .zip(&other.specializes)
            .map(|(a, b)| a.cmp_strongest_first(b))
            .find(|ordering| ordering.is_ne())
            .unwrap_or_else(|| self.specializes.len().cmp(&other.specializes.len()));
        if specializes != Ordering::Equal {
            return specializes;
        }

        let arc = self
            .arc_kind
            .strength_rank()
            .cmp(&other.arc_kind.strength_rank());
        if arc != Ordering::Equal {
            return arc;
        }

        let nested = cmp_nested_arc_kind(self.nested_arc_kind, other.nested_arc_kind);
        if nested != Ordering::Equal {
            return nested;
        }

        let depth = other.namespace_depth.cmp(&self.namespace_depth);
        if depth != Ordering::Equal {
            return depth;
        }

        match (self.authored, other.authored) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }

        let arc_list = self.arc_list_index.cmp(&other.arc_list_index);
        if arc_list != Ordering::Equal {
            return arc_list;
        }

        let layer_strength = self.layer_strength.cmp(&other.layer_strength);
        if layer_strength != Ordering::Equal {
            return layer_strength;
        }

        let layer_id = self.layer_id.cmp(&other.layer_id);
        if layer_id != Ordering::Equal {
            return layer_id;
        }

        self.spec_path.cmp(&other.spec_path)
    }
}

/// An authored opinion for a destination prim+field, with a strength key.
#[derive(Clone, Debug, PartialEq)]
pub struct Opinion {
    /// Strength key used for sorting.
    pub key: OpinionKey,
    /// The field token being authored: a prim metadata field name or a
    /// property name.
    pub field: TokenId,
    /// The authored content this opinion contributes.
    pub value: OpinionValue,
    /// Accumulated layer offset from all arcs leading to this opinion (§12.3.2.1).
    pub layer_offset: LayerOffset,
}

/// The authored content one [`Opinion`] contributes.
///
/// A composed prim keeps prim metadata fields and properties in one name
/// space, keyed by [`Opinion::field`]. A metadata opinion carries its single
/// value; a property opinion carries the whole authored [`PropertySpec`], so
/// its default, time samples, spline, targets and metadata stay separate and
/// value precedence is applied at query time.
///
/// Spec: AOUSD Core §12.2 (metadata resolution), §12.3 (attribute value
/// resolution), §12.4 (relationships and connections).
#[derive(Clone, Debug, PartialEq)]
pub enum OpinionValue {
    /// A prim metadata field value.
    Field(FieldValue),
    /// A property spec with all of its authored slots.
    Property(Box<PropertySpec>),
}

impl OpinionValue {
    /// Returns the value a default-time query reads from this opinion: a
    /// metadata [`FieldValue::Value`], or a property's authored
    /// [`PropertySpec::default`].
    ///
    /// Spec: AOUSD Core §12.3.1 (default values ignore time samples).
    #[must_use]
    pub fn default_value(&self) -> Option<&Value> {
        match self {
            Self::Field(FieldValue::Value(value)) => Some(value),
            Self::Field(_) => None,
            Self::Property(spec) => spec.default.as_ref(),
        }
    }

    /// Returns the authored, non-empty time samples of a property opinion.
    ///
    /// An explicitly authored empty sample map contributes no value, as in
    /// OpenUSD's `_HasTimeSamples` (`pxr/usd/usd/stage.cpp`).
    #[must_use]
    pub fn time_samples(&self) -> Option<&[TimeSample]> {
        match self {
            Self::Property(spec) => spec.time_samples.as_deref().filter(|s| !s.is_empty()),
            Self::Field(_) => None,
        }
    }

    /// Returns the authored spline of a property opinion.
    #[must_use]
    pub fn spline(&self) -> Option<&SplineData> {
        match self {
            Self::Property(spec) => spec.spline.as_ref(),
            Self::Field(_) => None,
        }
    }

    /// Returns the authored target paths: a property's connection or target
    /// path list, or a metadata [`FieldValue::PathListOp`].
    #[must_use]
    pub fn targets(&self) -> Option<&ListOp<TargetPath>> {
        match self {
            Self::Field(FieldValue::PathListOp(list)) => Some(list),
            Self::Field(_) => None,
            Self::Property(spec) => spec.targets.as_ref(),
        }
    }

    /// Returns the metadata field value, for a metadata opinion.
    #[must_use]
    pub fn as_field(&self) -> Option<&FieldValue> {
        match self {
            Self::Field(value) => Some(value),
            Self::Property(_) => None,
        }
    }

    /// Returns the property spec, for a property opinion.
    #[must_use]
    pub fn as_property(&self) -> Option<&PropertySpec> {
        match self {
            Self::Property(spec) => Some(spec),
            Self::Field(_) => None,
        }
    }

    /// Returns `true` when this opinion authors a value block as its default.
    #[must_use]
    pub fn is_blocked_default(&self) -> bool {
        matches!(self.default_value(), Some(Value::Blocked))
    }
}

impl From<FieldValue> for OpinionValue {
    fn from(value: FieldValue) -> Self {
        Self::Field(value)
    }
}

impl From<PropertySpec> for OpinionValue {
    fn from(spec: PropertySpec) -> Self {
        Self::Property(Box::new(spec))
    }
}

/// The identity of a composed field: a prim metadata field or a property.
///
/// A prim's metadata fields and its properties are different objects even
/// when they share a name (`def "P" (kind = "component") { double kind }`),
/// so they are indexed and resolved apart.
///
/// Spec: AOUSD Core §7.3 (property specs are children of the prim spec;
/// metadata are fields of the spec itself), §7.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FieldKey {
    /// A prim metadata field.
    Metadata(TokenId),
    /// A property.
    Property(TokenId),
}

impl FieldKey {
    /// The key an opinion is indexed under.
    pub(crate) fn of(opinion: &Opinion) -> Self {
        match opinion.value {
            OpinionValue::Field(_) => Self::Metadata(opinion.field),
            OpinionValue::Property(_) => Self::Property(opinion.field),
        }
    }
}

/// A per-prim composition result, keyed by field identity.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrimIndex {
    pub(crate) opinions_by_field: HashMap<FieldKey, Vec<Opinion>>,
    /// Every contributing property declaration, keyed by field. The
    /// composed type is the strongest surviving declaration's, so filtering
    /// out an opinion's declaration lets a weaker one take over.
    pub(crate) property_types_by_field: HashMap<TokenId, Vec<(OpinionKey, PropertyType)>>,
    pub(crate) sources: Vec<OpinionKey>,
}

impl PrimIndex {
    pub(crate) fn add_opinion(&mut self, opinion: Opinion) {
        self.opinions_by_field
            .entry(FieldKey::of(&opinion))
            .or_default()
            .push(opinion);
    }

    /// The opinions of the property `name`.
    pub(crate) fn property_opinions(&self, name: TokenId) -> Option<&[Opinion]> {
        self.opinions_by_field
            .get(&FieldKey::Property(name))
            .map(Vec::as_slice)
    }

    /// The opinions of the prim metadata field `key`.
    pub(crate) fn metadata_opinions(&self, key: TokenId) -> Option<&[Opinion]> {
        self.opinions_by_field
            .get(&FieldKey::Metadata(key))
            .map(Vec::as_slice)
    }

    pub(crate) fn add_source(&mut self, key: OpinionKey) {
        self.sources.push(key);
    }

    pub(crate) fn add_property_type(
        &mut self,
        field: TokenId,
        key: OpinionKey,
        property_type: PropertyType,
    ) {
        self.property_types_by_field
            .entry(field)
            .or_default()
            .push((key, property_type));
    }

    /// Returns the type of the strongest declaration of `field`.
    pub(crate) fn property_type_for(&self, field: &TokenId) -> Option<&PropertyType> {
        self.property_types_by_field
            .get(field)?
            .iter()
            .min_by(|(a, _), (b, _)| a.cmp_strongest_first(b))
            .map(|(_, property_type)| property_type)
    }

    /// Keeps only the sources, opinions and property declarations whose key
    /// satisfies `keep`, dropping fields left without opinions or
    /// declarations.
    pub(crate) fn retain_keys(&mut self, mut keep: impl FnMut(&OpinionKey) -> bool) {
        self.sources.retain(|key| keep(key));
        for opinions in self.opinions_by_field.values_mut() {
            opinions.retain(|opinion| keep(&opinion.key));
        }
        self.opinions_by_field
            .retain(|_, opinions| !opinions.is_empty());
        for declarations in self.property_types_by_field.values_mut() {
            declarations.retain(|(key, _)| keep(key));
        }
        self.property_types_by_field
            .retain(|_, declarations| !declarations.is_empty());
    }

    pub(crate) fn finalize(&mut self) {
        for opinions in self.opinions_by_field.values_mut() {
            opinions.sort_by(|a, b| a.key.cmp_strongest_first(&b.key));
        }
        self.sources.sort_by(|a, b| a.cmp_strongest_first(b));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::LayerId;
    use crate::{
        path::{Path, PathId, PathInterner},
        spec_path::SpecPath,
    };
    use alloc::format;
    use alloc::{vec, vec::Vec};

    fn key(
        is_local: bool,
        arc_kind: ArcKind,
        nested_arc_kind: Option<ArcKind>,
        namespace_depth: u16,
        authored: bool,
        arc_list_index: u16,
        layer_strength: u16,
        layer_id: u64,
        spec_path: u32,
    ) -> OpinionKey {
        let mut paths = PathInterner::default();
        let mut tokens = crate::interner::TokenInterner::default();
        let path_str = format!("/Spec{spec_path}");
        let path_id = paths.intern(Path::parse_absolute(&path_str, &mut tokens).expect("path"));
        OpinionKey {
            is_local,
            specializes: Vec::new(),
            arc_kind,
            nested_arc_kind,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id: LayerId(layer_id),
            lookup_path: PathId::from_raw(spec_path),
            spec_path: SpecPath::from_prim_path(path_id, &paths),
        }
    }

    fn assert_stronger(a: &OpinionKey, b: &OpinionKey) {
        assert_eq!(a.cmp_strongest_first(b), Ordering::Less);
        assert_eq!(b.cmp_strongest_first(a), Ordering::Greater);
    }

    #[test]
    fn local_beats_remote() {
        // Spec: local opinions are stronger than opinions introduced by arcs.
        let local = key(true, ArcKind::Local, None, 1, true, 0, 5, 10, 1);
        let remote = key(false, ArcKind::Inherits, None, 999, false, 999, 0, 0, 0);
        assert_stronger(&local, &remote);
    }

    #[test]
    fn arc_kind_follows_liverps_order() {
        // Spec ordering (strongest -> weakest): Inherits, Variants, Relocates, References,
        // Payloads, Specializes.
        // Spec: AOUSD Core §10 (LIVERPS ordering).
        let is_local = false;
        let namespace_depth = 3;
        let authored = true;
        let arc_list_index = 0;
        let layer_strength = 0;
        let layer_id = 1;
        let spec_path = 1;

        let inherits = key(
            is_local,
            ArcKind::Inherits,
            None,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id,
            spec_path,
        );
        let variants = key(
            is_local,
            ArcKind::Variants,
            None,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id,
            spec_path,
        );
        let relocates = key(
            is_local,
            ArcKind::Relocates,
            None,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id,
            spec_path,
        );
        let references = key(
            is_local,
            ArcKind::References,
            None,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id,
            spec_path,
        );
        let payloads = key(
            is_local,
            ArcKind::Payloads,
            None,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id,
            spec_path,
        );
        let specializes = key(
            is_local,
            ArcKind::Specializes,
            None,
            namespace_depth,
            authored,
            arc_list_index,
            layer_strength,
            layer_id,
            spec_path,
        );

        assert_stronger(&inherits, &variants);
        assert_stronger(&variants, &relocates);
        assert_stronger(&relocates, &references);
        assert_stronger(&references, &payloads);
        assert_stronger(&payloads, &specializes);
    }

    #[test]
    fn deeper_namespace_wins_ties() {
        // Spec: deeper namespace is stronger when arc kind ties.
        let shallow = key(false, ArcKind::References, None, 1, true, 0, 0, 0, 0);
        let deep = key(false, ArcKind::References, None, 2, true, 0, 0, 0, 0);
        assert_stronger(&deep, &shallow);
    }

    #[test]
    fn authored_beats_implied() {
        // Spec: authored arc beats implied.
        let implied = key(false, ArcKind::References, None, 1, false, 0, 0, 0, 0);
        let authored = key(false, ArcKind::References, None, 1, true, 0, 0, 0, 0);
        assert_stronger(&authored, &implied);
    }

    #[test]
    fn earlier_arc_in_list_is_stronger() {
        // Spec: otherwise, list order of arcs.
        let first = key(false, ArcKind::References, None, 1, true, 0, 0, 0, 0);
        let second = key(false, ArcKind::References, None, 1, true, 1, 0, 0, 0);
        assert_stronger(&first, &second);
    }

    #[test]
    fn stronger_layer_in_stack_wins_ties() {
        // Spec: layer stack order participates in tie-breaking.
        let stronger_layer = key(true, ArcKind::Local, None, 1, true, 0, 0, 0, 0);
        let weaker_layer = key(true, ArcKind::Local, None, 1, true, 0, 1, 0, 0);
        assert_stronger(&stronger_layer, &weaker_layer);
    }

    #[test]
    fn stable_ids_break_remaining_ties() {
        let a = key(true, ArcKind::Local, None, 1, true, 0, 0, 1, 1);
        let b = key(true, ArcKind::Local, None, 1, true, 0, 0, 2, 0);
        assert_stronger(&a, &b);

        let mut tokens = crate::interner::TokenInterner::default();
        let mut paths = PathInterner::default();
        let path_c = paths.intern(Path::parse_absolute("/A", &mut tokens).expect("path"));
        let path_d = paths.intern(Path::parse_absolute("/B", &mut tokens).expect("path"));
        let c = OpinionKey {
            is_local: true,
            specializes: Vec::new(),
            arc_kind: ArcKind::Local,
            nested_arc_kind: None,
            namespace_depth: 1,
            authored: true,
            arc_list_index: 0,
            layer_strength: 0,
            layer_id: LayerId(1),
            lookup_path: path_c,
            spec_path: SpecPath::from_prim_path(path_c, &paths),
        };
        let d = OpinionKey {
            spec_path: SpecPath::from_prim_path(path_d, &paths),
            lookup_path: path_d,
            ..c.clone()
        };
        assert_stronger(&c, &d);
    }

    #[test]
    fn sorting_produces_strongest_first() {
        let mut keys: Vec<OpinionKey> = alloc::vec![
            key(false, ArcKind::Specializes, None, 1, true, 0, 0, 0, 0),
            key(true, ArcKind::Local, None, 1, true, 0, 1, 0, 0),
            key(true, ArcKind::Local, None, 2, true, 0, 0, 0, 0),
            key(false, ArcKind::Variants, None, 3, true, 0, 0, 0, 0),
        ];

        keys.sort_by(|a, b| a.cmp_strongest_first(b));

        assert!(keys[0].is_local);
        assert_eq!(keys[0].namespace_depth, 2);

        assert!(keys[1].is_local);
        assert_eq!(keys[1].layer_strength, 1);

        assert_eq!(keys[2].arc_kind, ArcKind::Variants);
        assert_eq!(keys[3].arc_kind, ArcKind::Specializes);
    }

    fn origin(arc_kind: ArcKind, nested_arc_kind: Option<ArcKind>) -> SpecializesOrigin {
        SpecializesOrigin {
            namespace_depth: 1,
            arc_kind,
            nested_arc_kind,
            arc_list_index: 0,
            specializes_index: 0,
            implied: false,
        }
    }

    fn specialized(specializes: Vec<SpecializesOrigin>) -> OpinionKey {
        OpinionKey {
            specializes,
            ..key(false, ArcKind::Specializes, None, 1, true, 0, 0, 1, 1)
        }
    }

    #[test]
    fn specializes_reached_through_a_reference_are_weaker_than_payloads() {
        // Spec: AOUSD Core §10.4.1: a specializes is weaker than every other
        // arc, not only than the arc it is reached through.
        let payload = key(
            false,
            ArcKind::Payloads,
            Some(ArcKind::References),
            1,
            true,
            3,
            0,
            1,
            1,
        );
        let class = specialized(vec![origin(
            ArcKind::References,
            Some(ArcKind::Specializes),
        )]);
        assert_stronger(&payload, &class);
    }

    #[test]
    fn nested_specializes_nodes_are_weaker_than_their_enclosing_node() {
        let outer = origin(ArcKind::Specializes, None);
        let inner = SpecializesOrigin {
            namespace_depth: 2,
            ..origin(ArcKind::Specializes, Some(ArcKind::Specializes))
        };
        let enclosing = OpinionKey {
            nested_arc_kind: Some(ArcKind::References),
            ..specialized(vec![outer])
        };
        assert_stronger(&enclosing, &specialized(vec![outer, inner]));
    }

    #[test]
    fn nested_specializes_nodes_rank_before_weaker_siblings_of_their_node() {
        // `P` specializes `[A, B]` and `A` specializes `C`: `C` follows `A`,
        // before `B` (`PcpCompareSiblingNodeStrength`).
        let a = origin(ArcKind::Specializes, None);
        let b = SpecializesOrigin {
            arc_list_index: 1,
            specializes_index: 1,
            ..a
        };
        let c = SpecializesOrigin {
            namespace_depth: 1,
            ..origin(ArcKind::Specializes, Some(ArcKind::Specializes))
        };
        assert_stronger(&specialized(vec![a]), &specialized(vec![a, c]));
        assert_stronger(&specialized(vec![a, c]), &specialized(vec![b]));
        assert_stronger(&specialized(vec![a, c]), &specialized(vec![b, c]));
    }

    #[test]
    fn specializes_nodes_follow_their_placeholders() {
        // A deeper node is stronger; then the placeholder's own rank, so a
        // specializes under a nested reference outranks one authored beside
        // that reference; then the implied node outranks the propagated one
        // (`PcpCompareSiblingNodeStrength`).
        let deep = SpecializesOrigin {
            namespace_depth: 2,
            ..origin(ArcKind::Specializes, None)
        };
        let beside = origin(ArcKind::References, Some(ArcKind::Specializes));
        let nested = origin(ArcKind::References, Some(ArcKind::References));
        let direct = origin(ArcKind::Specializes, None);
        let implied = SpecializesOrigin {
            implied: true,
            ..direct
        };
        let order = [deep, nested, beside, implied, direct];
        for pair in order.windows(2) {
            assert_stronger(&specialized(vec![pair[0]]), &specialized(vec![pair[1]]));
        }
    }

    #[test]
    fn shorter_arc_chain_is_stronger() {
        let base = key(false, ArcKind::References, None, 1, true, 0, 0, 1, 1);
        let nested = key(
            false,
            ArcKind::References,
            Some(ArcKind::Inherits),
            1,
            true,
            0,
            0,
            1,
            1,
        );
        assert_stronger(&base, &nested);
    }
}
