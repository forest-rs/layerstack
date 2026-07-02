// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composition result types.
//!
//! Composition produces per-prim indexes (similar to `OpenUSD`'s internal
//! `PrimIndex`) which the [`crate::stage::Stage`] queries for population and
//! value resolution.
//!
//! Spec: AOUSD Core §10 (composition arcs and strength ordering) and §12 (value resolution).

use alloc::vec::Vec;
use core::cmp::Ordering;

use hashbrown::HashMap;

use crate::{
    doc::{FieldValue, LayerId, LayerOffset},
    interner::TokenId,
    path::PathId,
    property::PropertyType,
    spec_path::SpecPath,
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

/// A comparable strength key for a single authored opinion.
///
/// Spec: AOUSD Core §10.4 (strength ordering and tie-breakers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpinionKey {
    /// `true` for local opinions (layer stack), `false` for opinions introduced by arcs.
    pub is_local: bool,
    /// Arc kind of this opinion (for non-local opinions).
    pub arc_kind: ArcKind,
    /// Optional nested arc kind for opinions introduced inside another arc.
    ///
    /// This supports a single level of arc nesting (e.g. reference→inherit),
    /// treating shorter arc chains as stronger than longer ones when the outer
    /// arc kind ties.
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

        let arc = self
            .arc_kind
            .strength_rank()
            .cmp(&other.arc_kind.strength_rank());
        if arc != Ordering::Equal {
            return arc;
        }

        match (self.nested_arc_kind, other.nested_arc_kind) {
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) => {
                let nested = a.strength_rank().cmp(&b.strength_rank());
                if nested != Ordering::Equal {
                    return nested;
                }
            }
            (None, None) => {}
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
    /// The field token being authored.
    pub field: TokenId,
    /// The authored value.
    pub value: FieldValue,
    /// Accumulated layer offset from all arcs leading to this opinion (§12.3.2.1).
    pub layer_offset: LayerOffset,
}

/// A per-prim composition result, keyed by field token.
#[derive(Clone, Debug, Default)]
pub(crate) struct PrimIndex {
    pub(crate) opinions_by_field: HashMap<TokenId, Vec<Opinion>>,
    pub(crate) property_types_by_field: HashMap<TokenId, (OpinionKey, PropertyType)>,
    pub(crate) sources: Vec<OpinionKey>,
}

impl PrimIndex {
    pub(crate) fn add_opinion(&mut self, opinion: Opinion) {
        self.opinions_by_field
            .entry(opinion.field)
            .or_default()
            .push(opinion);
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
        match self.property_types_by_field.get_mut(&field) {
            Some((existing_key, existing_type))
                if existing_key.cmp_strongest_first(&key).is_gt() =>
            {
                *existing_key = key;
                *existing_type = property_type;
            }
            Some(_) => {}
            None => {
                self.property_types_by_field
                    .insert(field, (key, property_type));
            }
        }
    }

    pub(crate) fn property_type_for(&self, field: &TokenId) -> Option<&PropertyType> {
        self.property_types_by_field
            .get(field)
            .map(|(_, property_type)| property_type)
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
    use alloc::vec::Vec;

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
