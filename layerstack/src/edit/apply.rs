// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Applying transactions: lowering edits to storage steps, recording their
//! inverses, and rolling back on failure.
//!
//! Every edit is lowered to [`Raw`] steps, each of which changes one slot
//! of a layer's storage and returns the step that restores it. The
//! inverse of a transaction is the list of those restoring steps, newest
//! first, so undoing touches exactly the slots the transaction touched.
//! Each restoring step is [`Guarded`] by the step it undoes: it applies
//! only while its slot still holds what that step wrote.

use crate::sort;
use alloc::{boxed::Box, sync::Arc, vec::Vec};
use hashbrown::HashMap;

use super::{
    error::{EditError, Rejection, Slot},
    layer_fields,
    prim_fields::{self, ListPosition, PrimValue},
    same::Same,
    spec::{Loc, SpecRef, conforms, spec_at, spec_at_mut},
    target::Address,
    transaction::{Op, Precondition, Transaction, authored},
};
use crate::{
    doc::{
        FieldEntry, FieldValue, Layer, LayerId, LayerStore, PrimSpec, Specifier, Value,
        VariantSetSpec, VariantSpec,
    },
    interner::TokenId,
    listop::{ListOp, resolve_list_chain},
    path::{PathId, PathInterner, TargetPath},
    property::{
        PropertyEntry, PropertyKind, PropertySpec, PropertyType, Variability, get_property,
    },
    schema::{PropertyDefinition, SchemaKind},
    spec_path::{SpecPath, VariantSelectionSite},
    stage::Stage,
};

/// Authored animation slots, kept separate from declaration and metadata guards.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct AnimationState {
    pub(crate) samples: crate::TimeSamples,
    pub(crate) spline: Option<Box<crate::SplineData>>,
}
impl Same for AnimationState {
    fn same(&self, other: &Self) -> bool {
        self.samples.same(&other.samples) && self.spline.same(&other.spline)
    }
}

/// One storage step. Applying it returns the step that undoes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Raw {
    LayerMetadata {
        layer: LayerId,
        key: TokenId,
        value: Option<FieldValue>,
        index: usize,
    },
    /// Replaces layer relocates as one structural, guarded slot.
    LayerRelocates {
        layer: LayerId,
        relocates: Option<Vec<crate::Relocate>>,
    },
    /// Replaces a layer's sublayers, with their offsets, as one slot.
    Sublayers {
        layer: LayerId,
        sublayers: Vec<crate::SublayerEntry>,
    },
    LayerFields {
        layer: LayerId,
        default_prim: Option<TokenId>,
        metadata: Vec<FieldEntry>,
    },
    /// Replaces every prim spec stored at `path` ([`Layer::prims`] and
    /// [`Layer::variant_prims`]) with `main` and `branches`.
    PrimSlots {
        layer: LayerId,
        path: PathId,
        main: Option<PrimSpec>,
        branches: Option<Vec<PrimSpec>>,
    },
    /// Inserts `name` at `index` among the child names of the spec at
    /// `parent`, or removes it (`None`).
    Child {
        layer: LayerId,
        parent: Loc,
        name: TokenId,
        index: Option<usize>,
    },
    /// Stores (`Some`) or removes (`None`) the variant `variant` of the set
    /// `set` held by the spec at `host`: a prim spec, or the variant spec a
    /// nested set belongs to. Storing creates the set if needed; removing
    /// drops the set when `drop_set` and it is left empty. Neither edits
    /// the holder's `variantSetNames`.
    Variant {
        layer: LayerId,
        host: Loc,
        set: TokenId,
        variant: TokenId,
        spec: Option<VariantSpec>,
        drop_set: bool,
    },
    /// Stores (`Some`) or removes (`None`) the whole variant set spec `set`
    /// held by the spec at `host`, with the variant specs in it; a new set
    /// is inserted at `index` among the holder's sets, so that undoing a
    /// removal restores the order. The prim specs authored in its branches
    /// are stored apart, in [`Raw::PrimSlots`] steps.
    VariantSet {
        layer: LayerId,
        host: Loc,
        set: TokenId,
        spec: Option<VariantSetSpec>,
        index: usize,
    },
    /// Stores (`Some`) or removes (`None`) the property `name` of the spec
    /// at `loc`; a new property is inserted at `index`.
    Property {
        layer: LayerId,
        loc: Loc,
        name: TokenId,
        spec: Option<PropertySpec>,
        index: usize,
    },
    /// Sets or clears the default of the attribute `name` at `loc`.
    Default {
        layer: LayerId,
        loc: Loc,
        name: TokenId,
        value: Option<Value>,
    },
    /// Replaces both animation slots without touching declaration or metadata.
    Animation {
        layer: LayerId,
        loc: Loc,
        name: TokenId,
        state: Box<AnimationState>,
    },
    /// Sets (`Some`) or removes (`None`) the time sample at `time` of the
    /// attribute `name` at `loc`. Removing the last sample leaves an empty
    /// sample list when `keep_empty`, and no `timeSamples` field otherwise.
    Sample {
        layer: LayerId,
        loc: Loc,
        name: TokenId,
        time: f64,
        value: Option<Value>,
        keep_empty: bool,
    },
    /// Sets (`Some`) or removes (`None`) the metadata field `key` of the
    /// spec at `loc`, or of its property `property`; a new field is
    /// inserted at `index`.
    Field {
        layer: LayerId,
        loc: Loc,
        property: Option<TokenId>,
        key: TokenId,
        value: Option<FieldValue>,
        index: usize,
    },
    /// Sets (`Some`) or removes (`None`) the targets or connections of the
    /// property `name` at `loc`.
    Targets {
        layer: LayerId,
        loc: Loc,
        name: TokenId,
        targets: Option<ListOp<TargetPath>>,
    },
    /// Sets or clears the selection for `set` authored at `loc`.
    Selection {
        layer: LayerId,
        loc: Loc,
        set: TokenId,
        variant: Option<TokenId>,
    },
    /// Replaces a prim field kept in a dedicated member of the spec at
    /// `loc` (see [`prim_fields`]).
    PrimField {
        layer: LayerId,
        loc: Loc,
        value: PrimValue,
    },
}

impl Raw {
    pub(crate) fn layer(&self) -> LayerId {
        match self {
            Self::LayerRelocates { layer, .. }
            | Self::Sublayers { layer, .. }
            | Self::LayerFields { layer, .. }
            | Self::LayerMetadata { layer, .. }
            | Self::PrimSlots { layer, .. }
            | Self::Child { layer, .. }
            | Self::Variant { layer, .. }
            | Self::VariantSet { layer, .. }
            | Self::Property { layer, .. }
            | Self::Default { layer, .. }
            | Self::Animation { layer, .. }
            | Self::Sample { layer, .. }
            | Self::Field { layer, .. }
            | Self::Targets { layer, .. }
            | Self::Selection { layer, .. }
            | Self::PrimField { layer, .. } => *layer,
        }
    }

    /// Structural scopes preserve child edits without widening to all siblings.
    fn change_path(&self, store: &dyn LayerStore) -> Option<PathId> {
        match self {
            Self::LayerRelocates { .. }
            | Self::Sublayers { .. }
            | Self::LayerFields { .. }
            | Self::LayerMetadata { .. } => None,
            Self::PrimSlots { path, .. } => Some(*path),
            Self::Child { parent, name, .. } => {
                let path = store.paths().resolve(parent.prim_path()).join(&[*name]);
                store.paths().lookup(&path)
            }
            Self::Variant { host, .. } | Self::VariantSet { host, .. } => Some(host.prim_path()),
            Self::Selection { loc, .. }
            | Self::PrimField { loc, .. }
            | Self::Property { loc, .. }
            | Self::Default { loc, .. }
            | Self::Animation { loc, .. }
            | Self::Sample { loc, .. }
            | Self::Targets { loc, .. }
            | Self::Field { loc, .. } => Some(loc.prim_path()),
        }
    }

    /// The namespace path of the prim whose opinions a value step changes;
    /// `None` for a step that changes namespace or composition arcs.
    fn opinion_site(&self) -> Option<PathId> {
        match self {
            Self::LayerRelocates { .. }
            | Self::Sublayers { .. }
            | Self::LayerFields { .. }
            | Self::LayerMetadata { .. }
            | Self::PrimSlots { .. }
            | Self::Child { .. }
            | Self::Variant { .. }
            | Self::VariantSet { .. }
            | Self::Selection { .. }
            | Self::PrimField { .. } => None,
            Self::Property { loc, .. }
            | Self::Default { loc, .. }
            | Self::Animation { loc, .. }
            | Self::Sample { loc, .. }
            | Self::Targets { loc, .. }
            | Self::Field { loc, .. } => Some(loc.prim_path()),
        }
    }
}

impl Raw {
    /// Whether applying `self` left its slot as `written` leaves it: the
    /// same authored content ([`Same`], so floats compare by bits),
    /// whatever position a list entry is at.
    fn same_state(&self, written: &Self) -> bool {
        match (self, written) {
            (
                Self::LayerMetadata {
                    layer, key, value, ..
                },
                Self::LayerMetadata {
                    layer: l,
                    key: k,
                    value: v,
                    ..
                },
            ) => (layer, key) == (l, k) && value.same(v),
            (
                Self::LayerRelocates { layer, relocates },
                Self::LayerRelocates {
                    layer: l,
                    relocates: r,
                },
            ) => layer == l && relocates == r,
            (
                Self::Sublayers { layer, sublayers },
                Self::Sublayers {
                    layer: l,
                    sublayers: s,
                },
            ) => layer == l && sublayers == s,
            (
                Self::LayerFields {
                    layer,
                    default_prim,
                    metadata,
                },
                Self::LayerFields {
                    layer: l,
                    default_prim: d,
                    metadata: m,
                },
            ) => layer == l && default_prim == d && metadata.same(m),
            (
                Self::PrimSlots {
                    layer,
                    path,
                    main,
                    branches,
                },
                Self::PrimSlots {
                    layer: l,
                    path: p,
                    main: m,
                    branches: b,
                },
            ) => (layer, path) == (l, p) && main.same(m) && branches.same(b),
            (
                Self::Child {
                    layer,
                    parent,
                    name,
                    index,
                },
                Self::Child {
                    layer: l,
                    parent: p,
                    name: n,
                    index: i,
                },
            ) => (layer, parent, name, index.is_some()) == (l, p, n, i.is_some()),
            (
                Self::Variant {
                    layer,
                    host,
                    set,
                    variant,
                    spec,
                    ..
                },
                Self::Variant {
                    layer: l,
                    host: h,
                    set: s,
                    variant: v,
                    spec: sp,
                    ..
                },
            ) => (layer, host, set, variant) == (l, h, s, v) && spec.same(sp),
            (
                Self::VariantSet {
                    layer,
                    host,
                    set,
                    spec,
                    ..
                },
                Self::VariantSet {
                    layer: l,
                    host: h,
                    set: s,
                    spec: sp,
                    ..
                },
            ) => (layer, host, set) == (l, h, s) && spec.same(sp),
            (
                Self::Property {
                    layer,
                    loc,
                    name,
                    spec,
                    ..
                },
                Self::Property {
                    layer: l,
                    loc: lc,
                    name: n,
                    spec: sp,
                    ..
                },
            ) => (layer, loc, name) == (l, lc, n) && spec.same(sp),
            (
                Self::Default {
                    layer,
                    loc,
                    name,
                    value,
                },
                Self::Default {
                    layer: l,
                    loc: lc,
                    name: n,
                    value: v,
                },
            ) => (layer, loc, name) == (l, lc, n) && value.same(v),
            (
                Self::Animation {
                    layer,
                    loc,
                    name,
                    state,
                },
                Self::Animation {
                    layer: l,
                    loc: lc,
                    name: n,
                    state: s,
                },
            ) => (layer, loc, name) == (l, lc, n) && state.same(s),
            (
                Self::Sample {
                    layer,
                    loc,
                    name,
                    time,
                    value,
                    ..
                },
                Self::Sample {
                    layer: l,
                    loc: lc,
                    name: n,
                    time: t,
                    value: v,
                    ..
                },
            ) => (layer, loc, name) == (l, lc, n) && time.same(t) && value.same(v),
            (
                Self::Field {
                    layer,
                    loc,
                    property,
                    key,
                    value,
                    ..
                },
                Self::Field {
                    layer: l,
                    loc: lc,
                    property: pr,
                    key: k,
                    value: v,
                    ..
                },
            ) => (layer, loc, property, key) == (l, lc, pr, k) && value.same(v),
            (
                Self::Targets {
                    layer,
                    loc,
                    name,
                    targets,
                },
                Self::Targets {
                    layer: l,
                    loc: lc,
                    name: n,
                    targets: t,
                },
            ) => (layer, loc, name, targets) == (l, lc, n, t),
            (
                Self::Selection {
                    layer,
                    loc,
                    set,
                    variant,
                },
                Self::Selection {
                    layer: l,
                    loc: lc,
                    set: s,
                    variant: v,
                },
            ) => (layer, loc, set, variant) == (l, lc, s, v),
            (
                Self::PrimField { layer, loc, value },
                Self::PrimField {
                    layer: l,
                    loc: lc,
                    value: v,
                },
            ) => (layer, loc, value) == (l, lc, v),
            _ => false,
        }
    }

    /// The spec path and slot this step writes, for errors.
    fn slot(&self, paths: &PathInterner) -> (SpecPath, Slot) {
        match self {
            Self::LayerMetadata { key, .. } => (
                SpecPath::from_prim_path(
                    paths.lookup(&crate::Path::root()).expect("interned root"),
                ),
                Slot::Metadata(*key),
            ),
            Self::LayerRelocates { .. } => (
                SpecPath::from_prim_path(
                    paths.lookup(&crate::Path::root()).expect("interned root"),
                ),
                Slot::Relocates,
            ),
            Self::Sublayers { .. } => (
                SpecPath::from_prim_path(
                    paths.lookup(&crate::Path::root()).expect("interned root"),
                ),
                Slot::Sublayers,
            ),
            Self::LayerFields { .. } => (
                SpecPath::from_prim_path(
                    paths.lookup(&crate::Path::root()).expect("interned root"),
                ),
                Slot::Spec,
            ),
            Self::PrimSlots { path, .. } => (SpecPath::from_prim_path(*path), Slot::Spec),
            Self::Child { parent: loc, .. }
            | Self::Variant { host: loc, .. }
            | Self::VariantSet { host: loc, .. } => (loc.spec_path(paths), Slot::Spec),
            Self::Property { loc, name, .. } => {
                (loc.spec_path(paths).with_property(*name), Slot::Spec)
            }
            Self::Default { loc, name, .. } => {
                (loc.spec_path(paths).with_property(*name), Slot::Default)
            }
            Self::Animation { loc, name, .. } => {
                (loc.spec_path(paths).with_property(*name), Slot::Animation)
            }
            Self::Sample {
                loc, name, time, ..
            } => (
                loc.spec_path(paths).with_property(*name),
                Slot::TimeSample(*time),
            ),
            Self::Field {
                loc, property, key, ..
            } => {
                let path = loc.spec_path(paths);
                let path = match property {
                    Some(name) => path.with_property(*name),
                    None => path,
                };
                (path, Slot::Metadata(*key))
            }
            Self::Targets { loc, name, .. } => {
                (loc.spec_path(paths).with_property(*name), Slot::Targets)
            }
            Self::Selection { loc, set, .. } => {
                (loc.spec_path(paths), Slot::VariantSelection(*set))
            }
            Self::PrimField { loc, value, .. } => (loc.spec_path(paths), Slot::Prim(value.field())),
        }
    }
}

/// A step of an inverse transaction and the step it undoes: `step` applies
/// only while its slot holds what `written` wrote there, so undoing never
/// overwrites a later edit of the same slot.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Guarded {
    pub(crate) step: Raw,
    pub(crate) written: Raw,
}

/// What applying a transaction did, for stages to recompose.
#[derive(Debug)]
pub(crate) struct Outcome {
    /// Complete source property field changes, those of
    /// [`Self::property_specs`] aside, or None if any operation is broader.
    pub(crate) properties: Option<Vec<(LayerId, PathId, super::PropertyChange)>>,
    /// Exact existing-attribute value slots, or None for other edit kinds.
    pub(crate) values: Option<Vec<PropertyValueEdit>>,
    pub(crate) inverse: Transaction,
    /// Every layer the transaction changed.
    pub(crate) layers: Vec<LayerId>,
    /// `(layer, prim path)` of every spec whose opinions changed.
    pub(crate) touched: Vec<(LayerId, PathId)>,
    /// Whether specs were created or removed or variant selections
    /// changed, which changes namespace or composition arcs.
    pub(crate) structural: bool,
    /// Local prim slots changed, or None if an edit needs general composition.
    /// The slots of [`Self::child_lists`] are left out.
    pub(crate) local_structure: Option<Vec<PathId>>,
    /// Prims whose spec slots changed only the child names they list
    /// ([`lists_children_only`]), sorted: their prim indexes and namespace
    /// stay as they were, and only their child lists are composed again.
    pub(crate) child_lists: Vec<PathId>,
    /// Source prims whose schema identity (`apiSchemas`), model kind
    /// (`kind`) or value clips (`clips`, `clipSets`) changed.
    pub(crate) resync_sites: Vec<(LayerId, PathId)>,
    /// `(layer, source prim path, property name)` of every property spec
    /// added or removed, sorted: each changes that property's spec stack,
    /// not its prim's.
    pub(crate) property_specs: Vec<(LayerId, PathId, TokenId)>,
    /// The edits of inputs dynamic file format arguments may compose from.
    pub(crate) argument_inputs: Vec<ArgumentInput>,
    /// `(layer, source prim path)` of every spec the transaction only
    /// created, removed or listed children of as an inert `over`
    /// ([`inert_prims`]), in every layer it changed: a prim composed
    /// before and after such an edit changed its spec stack alone.
    pub(crate) inert_sites: Vec<(LayerId, PathId)>,
    /// `(layer, source prim path, change)` of every property field the
    /// transaction changed, property spec additions and removals aside.
    pub(crate) property_fields: Vec<(LayerId, PathId, super::PropertyChange)>,
    /// `(layer, source prim path)` of every other prim site a step
    /// changed: a prim composing from one of them changed more than its
    /// spec stack, whatever inert sites it composes from too.
    pub(crate) other_sites: Vec<(LayerId, PathId)>,
    /// The layers of the stage's own layer stack whose metadata the
    /// transaction changed only in fields that compose nothing
    /// ([`composes_nothing`]): the pseudo-root changed, info-only.
    pub(crate) layer_info: Vec<LayerId>,
}

/// An edit of a prim metadata field or an attribute default, the inputs
/// a dynamic file format composes payload arguments from
/// ([`crate::dynamic_format`]), with the values before and after.
///
/// OpenUSD: the info changes and default value changes
/// `PcpChanges::DidChange` checks against dynamic file format dependencies
/// (`pxr/usd/pcp/changes.cpp`).
#[derive(Clone, Debug)]
pub(crate) struct ArgumentInput {
    pub(crate) layer: LayerId,
    /// The prim path of the spec, its variant selections dropped.
    pub(crate) prim: PathId,
    pub(crate) name: ArgumentName,
    pub(crate) old: Option<Value>,
    pub(crate) new: Option<Value>,
}

/// What an [`ArgumentInput`] edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArgumentName {
    /// A prim metadata field.
    Field(TokenId),
    /// The default of an attribute, including one created or removed.
    Default(TokenId),
}

/// The argument input `guarded` edits, if any.
fn argument_input(guarded: &Guarded) -> Option<ArgumentInput> {
    let plain = |value: &Option<FieldValue>| match value {
        Some(FieldValue::Value(value)) => Some(value.clone()),
        _ => None,
    };
    let (layer, loc, name, old, new) = match (&guarded.written, &guarded.step) {
        (
            Raw::Field {
                layer,
                loc,
                property: None,
                key,
                value,
                ..
            },
            inverse,
        ) => {
            let old = match inverse {
                Raw::Field { value, .. } => plain(value),
                _ => None,
            };
            (*layer, loc, ArgumentName::Field(*key), old, plain(value))
        }
        (
            Raw::Default {
                layer,
                loc,
                name,
                value,
            },
            inverse,
        ) => {
            let old = match inverse {
                Raw::Default { value, .. } => value.clone(),
                _ => None,
            };
            (
                *layer,
                loc,
                ArgumentName::Default(*name),
                old,
                value.clone(),
            )
        }
        (
            Raw::Property {
                layer,
                loc,
                name,
                spec,
                ..
            },
            inverse,
        ) => {
            let old = match inverse {
                Raw::Property { spec, .. } => spec.as_ref().and_then(|spec| spec.default.clone()),
                _ => None,
            };
            let new = spec.as_ref().and_then(|spec| spec.default.clone());
            (*layer, loc, ArgumentName::Default(*name), old, new)
        }
        _ => return None,
    };
    Some(ArgumentInput {
        layer,
        prim: loc.prim_path(),
        name,
        old,
        new,
    })
}

/// Existing property value slots changed by an atomic transaction. A property
/// creation is deliberately not a value edit, even if it authors a default.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PropertyValueEdit {
    pub(crate) layer: LayerId,
    pub(crate) path: SpecPath,
    pub(crate) name: TokenId,
    pub(crate) default: bool,
    pub(crate) samples: bool,
}

impl PropertyValueEdit {
    pub(crate) fn property<'a>(&self, store: &'a dyn LayerStore) -> Option<&'a PropertySpec> {
        let loc = Loc::of(&self.path);
        let spec = spec_at(store.layer(self.layer)?, &loc)?;
        get_property(spec.properties(), self.name)
    }
}

/// The property field `raw` changes, as `(layer, source prim path,
/// change)`: `Some(None)` for a property spec added or removed, which
/// [`Outcome::property_specs`] reports, and `None` for a step that is not
/// about a property.
#[expect(
    clippy::option_option,
    reason = "a property spec step is about a property, but changes no field"
)]
fn property_field(raw: &Raw) -> Option<Option<(LayerId, PathId, super::PropertyChange)>> {
    use super::{PropertyChange, PropertyField};
    let (layer, loc, name, field) = match raw {
        Raw::Default {
            layer, loc, name, ..
        } => (*layer, loc, *name, PropertyField::Default),
        Raw::Sample {
            layer, loc, name, ..
        } => (*layer, loc, *name, PropertyField::TimeSamples),
        Raw::Animation {
            layer, loc, name, ..
        } => (*layer, loc, *name, PropertyField::Animation),
        Raw::Targets {
            layer, loc, name, ..
        } => (*layer, loc, *name, PropertyField::Targets),
        Raw::Field {
            layer,
            loc,
            property: Some(name),
            key,
            ..
        } => (*layer, loc, *name, PropertyField::Metadata(*key)),
        Raw::Property { .. } => return Some(None),
        _ => return None,
    };
    Some(Some((
        layer,
        loc.prim_path(),
        PropertyChange { name, field },
    )))
}

/// The property changes of `guarded` when it is a [`Raw::PrimSlots`] step
/// that rewrites a prim spec, keeping it, and changes nothing of it but
/// its properties (and the child names it lists), as a namespace edit
/// rewrites a spec whose relationship targets it repairs: the names of
/// the properties added, removed or redeclared, whose spec stacks change,
/// and the fields changed of the others.
///
/// OpenUSD records each as an entry of the property's own path
/// (`didAddProperty`, `didRemoveProperty`, or an info change of the
/// field), not of the prim's.
fn slot_property_changes(guarded: &Guarded) -> Option<(Vec<TokenId>, Vec<super::PropertyChange>)> {
    use super::{PropertyChange, PropertyField};
    let (
        Raw::PrimSlots {
            main: Some(after),
            branches,
            ..
        },
        Raw::PrimSlots {
            main: Some(before),
            branches: before_branches,
            ..
        },
    ) = (&guarded.written, &guarded.step)
    else {
        return None;
    };
    if !branches.same(before_branches) || after.properties == before.properties {
        return None;
    }
    let bare = |spec: &PrimSpec| {
        let mut spec = spec.clone();
        spec.authored_children.clear();
        spec.properties.clear();
        spec
    };
    if !bare(after).same(&bare(before)) {
        return None;
    }
    let find = |spec: &PrimSpec, name| {
        spec.properties
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.spec.clone())
    };
    let mut specs = Vec::new();
    let mut fields = Vec::new();
    let names = after
        .properties
        .iter()
        .chain(&before.properties)
        .map(|entry| entry.name);
    for name in names {
        if specs.contains(&name) || fields.iter().any(|f: &PropertyChange| f.name == name) {
            continue;
        }
        let (Some(a), Some(b)) = (find(after, name), find(before, name)) else {
            specs.push(name);
            continue;
        };
        if a == b {
            continue;
        }
        if (a.kind, a.custom, a.variability, &a.type_name)
            != (b.kind, b.custom, b.variability, &b.type_name)
        {
            specs.push(name);
            continue;
        }
        let mut field = |changed: bool, field| {
            if changed {
                fields.push(PropertyChange { name, field });
            }
        };
        field(a.default != b.default, PropertyField::Default);
        field(a.time_samples != b.time_samples, PropertyField::TimeSamples);
        field(a.spline != b.spline, PropertyField::Animation);
        field(a.targets != b.targets, PropertyField::Targets);
        let keys = a.metadata.iter().chain(b.metadata.iter()).map(|f| f.name);
        let mut seen = Vec::new();
        for key in keys {
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            let value = |spec: &PropertySpec| {
                spec.metadata
                    .iter()
                    .find(|f| f.name == key)
                    .map(|f| f.value.clone())
            };
            field(value(&a) != value(&b), PropertyField::Metadata(key));
        }
    }
    Some((specs, fields))
}

fn property_edits(steps: &[Guarded]) -> Option<Vec<(LayerId, PathId, super::PropertyChange)>> {
    steps
        .iter()
        .map(|step| property_field(&step.written))
        .collect::<Option<Vec<_>>>()
        .map(|edits| edits.into_iter().flatten().collect())
}

fn value_edits(steps: &[Guarded], store: &dyn LayerStore) -> Option<Vec<PropertyValueEdit>> {
    let mut edits: Vec<PropertyValueEdit> = Vec::new();
    for guarded in steps {
        let (layer, loc, name, default) = match &guarded.written {
            Raw::Default {
                layer, loc, name, ..
            } => (*layer, loc, *name, true),
            Raw::Sample {
                layer, loc, name, ..
            } => (*layer, loc, *name, false),
            _ => return None,
        };
        let path = loc.spec_path(store.paths()).with_property(name);
        if let Some(edit) = edits
            .iter_mut()
            .find(|e| e.layer == layer && e.path == path)
        {
            edit.default |= default;
            edit.samples |= !default;
        } else {
            edits.push(PropertyValueEdit {
                layer,
                path,
                name,
                default,
                samples: !default,
            });
        }
    }
    Some(edits)
}

/// Optional complete source candidate snapshot at transaction start. The
/// journal keeps it across deletions and discards it after source-slot writes
/// could add paths. The caller owns the index and its initial eligibility.
pub(crate) trait SourceNamespace {
    fn subtree(&self, layer: LayerId, root: PathId) -> Option<Vec<PathId>>;
}

/// The inverse steps recorded so far and their effects.
#[derive(Default)]
pub(super) struct Journal<'a> {
    // Deletions leave this snapshot a candidate superset. Source-slot writes
    // can add paths, so they discard it before any later subtree lookup.
    namespace: Option<&'a dyn SourceNamespace>,
    steps: Vec<Guarded>,
    layers: Vec<LayerId>,
    /// The layers whose edits may change namespace or arcs.
    structural_layers: Vec<LayerId>,
    /// The layers whose edits include a layer metadata field that
    /// composes nothing ([`composes_nothing`]): structural unless the
    /// layer is one of the stage's own layer stack.
    layer_info: Vec<LayerId>,
    touched: Vec<(LayerId, PathId)>,
    /// `touched` as a set: a bulk edit touches as many prims as it has
    /// steps.
    touched_set: hashbrown::HashSet<(LayerId, PathId)>,
    /// The prim specs outside variant branches that [`add_child`] has
    /// looked at: `None` once it scanned the spec's child names, then, from
    /// the second child added, the names as a set, kept current by the
    /// `Child` steps run since. A step replacing the spec drops its entry.
    /// Creating many children of one prim does not scan its growing list
    /// each time, and creating one does not index the list at all.
    children: HashMap<(LayerId, PathId), Option<hashbrown::HashSet<TokenId>>>,
    /// The prim paths, by layer, of the prim and variant specs whose
    /// fields the steps run so far may have changed, the type name and
    /// `apiSchemas` among them: a later edit plans against the prim
    /// definition those fields now give ([`planned_definition`]).
    prim_fields: hashbrown::HashSet<(LayerId, PathId)>,
}

impl Journal<'_> {
    pub(super) fn run(&mut self, store: &mut dyn LayerStore, step: Raw) -> Result<(), Rejection> {
        self.run_step(store, step).map(|_| ())
    }

    /// Applies `step`, records its inverse, and returns the inverse.
    fn run_step(&mut self, store: &mut dyn LayerStore, step: Raw) -> Result<&Raw, Rejection> {
        let inverse = apply_raw(store, &step)?;
        match &step {
            Raw::Child {
                layer,
                parent: Loc::Prim { path, sites },
                name,
                index,
            } if sites.is_empty() => {
                if let Some(Some(names)) = self.children.get_mut(&(*layer, *path)) {
                    if index.is_some() {
                        names.insert(*name);
                    } else {
                        names.remove(name);
                    }
                }
            }
            Raw::PrimSlots { layer, path, .. } => {
                self.children.remove(&(*layer, *path));
            }
            Raw::Variant { layer, host, .. } | Raw::VariantSet { layer, host, .. } => {
                let host = host.prim_path();
                self.children.remove(&(*layer, host));
            }
            _ => {}
        }
        if let Raw::PrimSlots { main, branches, .. } = &step
            && (main.is_some() || branches.is_some())
        {
            self.namespace = None;
        }
        let prim_fields = match &step {
            Raw::PrimSlots { layer, path, .. } => Some((*layer, *path)),
            Raw::PrimField { layer, loc, .. }
            | Raw::Field {
                layer,
                loc,
                property: None,
                ..
            } => Some((*layer, loc.prim_path())),
            Raw::Variant { layer, host, .. } | Raw::VariantSet { layer, host, .. } => {
                Some((*layer, host.prim_path()))
            }
            _ => None,
        };
        if let Some(site) = prim_fields {
            self.prim_fields.insert(site);
        }
        let layer = step.layer();
        if !self.layers.contains(&layer) {
            self.layers.push(layer);
        }
        match step.opinion_site() {
            Some(prim) => {
                if self.touched_set.insert((layer, prim)) {
                    self.touched.push((layer, prim));
                }
            }
            None if matches!(&step, Raw::LayerMetadata { key, .. }
                if composes_nothing(store.tokens().resolve(*key))) =>
            {
                if !self.layer_info.contains(&layer) {
                    self.layer_info.push(layer);
                }
            }
            None if !self.structural_layers.contains(&layer) => {
                self.structural_layers.push(layer);
            }
            None => {}
        }
        self.steps.push(Guarded {
            step: inverse,
            written: step,
        });
        Ok(&self.steps.last().expect("just pushed").step)
    }

    /// Applies an inverse step, failing if its slot no longer holds what
    /// the step it undoes wrote.
    fn run_guarded(
        &mut self,
        store: &mut dyn LayerStore,
        guarded: &Guarded,
        index: usize,
    ) -> Result<(), EditError> {
        let found = self
            .run_step(store, guarded.step.clone())
            .map_err(|reason| EditError::Rejected { op: index, reason })?;
        if found.same_state(&guarded.written) {
            return Ok(());
        }
        let (path, slot) = guarded.step.slot(store.paths());
        Err(EditError::StaleValue {
            layer: guarded.step.layer(),
            path,
            slot,
        })
    }

    fn roll_back(self, store: &mut dyn LayerStore) {
        for Guarded { step, .. } in self.steps.into_iter().rev() {
            let restored = apply_raw(store, &step);
            debug_assert!(
                restored.is_ok(),
                "an inverse step applies right after its step"
            );
        }
    }
}

/// Applies `txn` to `store`; see [`Transaction::apply`]. `stage` supplies
/// composed property declarations for stage addresses.
pub(crate) fn apply(
    store: &mut dyn LayerStore,
    txn: &Transaction,
    stage: Option<&Stage>,
    namespace: Option<&dyn SourceNamespace>,
) -> Result<Outcome, EditError> {
    check_preconditions(store, txn)?;
    let mut journal = Journal {
        namespace,
        ..Default::default()
    };
    for (index, op) in txn.ops.iter().enumerate() {
        let applied = match op {
            Op::Raw(guarded) => journal.run_guarded(store, guarded, index),
            _ => apply_op(store, op, stage, &mut journal)
                .map_err(|reason| EditError::Rejected { op: index, reason }),
        };
        if let Err(error) = applied {
            journal.roll_back(store);
            return Err(error);
        }
    }
    // A field of a layer the stage does not compose as its own may reach
    // it through an arc whose target it names (`defaultPrim` aside, a
    // layer's metadata composes into no prim, but no stage records which
    // layers it reads metadata of): such an edit stays structural.
    let mut layer_info = Vec::new();
    for layer in core::mem::take(&mut journal.layer_info) {
        if stage.is_some_and(|stage| stage.layer_stack().contains(&layer)) {
            layer_info.push(layer);
        } else if !journal.structural_layers.contains(&layer) {
            journal.structural_layers.push(layer);
        }
    }
    let mut inert_sites = Vec::new();
    for id in &journal.layers {
        let paths = journal
            .steps
            .iter()
            .filter(|g| g.written.layer() == *id)
            .map(|g| g.written.change_path(store))
            .collect::<Option<Vec<_>>>();
        let root = store.paths().lookup(&crate::Path::root());
        // The pseudo-root's child names change as a child is listed or
        // unlisted (`Raw::Child`), or as its spec is written whole, as
        // creating its spec or a namespace edit does.
        let root_has_child_edits = journal.steps.iter().any(|g| match &g.written {
            Raw::Child { layer, parent, .. } => *layer == *id && Some(parent.prim_path()) == root,
            Raw::PrimSlots { layer, path, .. } => *layer == *id && Some(*path) == root,
            _ => false,
        });
        let root_only_supports_children = root_has_child_edits
            && journal
                .steps
                .iter()
                .filter(|g| g.written.layer() == *id)
                .all(|g| {
                    let supports = |step: &Raw| match step {
                        Raw::PrimSlots {
                            path,
                            main,
                            branches,
                            ..
                        } if Some(*path) == root => {
                            branches.as_ref().is_none_or(|b| b.is_empty())
                                && main.as_ref().is_none_or(|spec| {
                                    let mut spec = spec.clone();
                                    spec.authored_children.clear();
                                    spec == PrimSpec::default()
                                })
                        }
                        _ => step.change_path(store) != root,
                    };
                    supports(&g.written) && supports(&g.step)
                });
        inert_sites.extend(
            inert_prims(store, &journal.steps, *id, root, true)
                .0
                .into_iter()
                .map(|path| (*id, path)),
        );
        let mut ignored: Vec<PathId> = root
            .filter(|_| root_only_supports_children)
            .into_iter()
            .collect();
        if let Some(stage) = stage
            && stage.layer_stack().contains(id)
        {
            ignored.extend(support_only_prims(store, &journal.steps, *id, stage, root));
        }
        if let Some(layer) = store.layer_mut(*id) {
            layer.record_scoped_change(journal.structural_layers.contains(id), paths, ignored);
        }
    }
    let Journal {
        mut steps,
        layers,
        structural_layers,
        touched,
        layer_info: _,
        namespace: _,
        touched_set: _,
        children: _,
        prim_fields: _,
    } = journal;
    let mut local_structure = Some(Vec::new());
    let mut child_lists = Vec::new();
    let mut resync_sites = Vec::new();
    let mut property_specs = Vec::new();
    let root = store.paths().lookup(&crate::Path::root());
    for guarded in &steps {
        // OpenUSD: `UsdStage::_HandleLayersDidChange` resyncs the property
        // path of a property spec added or removed (`didAddProperty`,
        // `didAddPropertyWithOnlyRequiredFields`, `didRemoveProperty` in
        // `SdfChangeList`), which `PcpChanges::DidChange` records as a
        // change of that property's spec stack. A change of a prim field
        // that `resyncs_prim` resyncs the prim.
        if let Raw::Property {
            layer, loc, name, ..
        } = &guarded.written
        {
            property_specs.push((*layer, loc.prim_path(), *name));
        }
        let declaration = match &guarded.written {
            Raw::Field {
                layer,
                loc,
                property: None,
                key,
                ..
            } if resyncs_prim(store.tokens().resolve(*key)) => Some((*layer, loc.prim_path())),
            _ => None,
        };
        if let Some(site) = declaration {
            resync_sites.push(site);
        }
        match &guarded.written {
            Raw::PrimSlots { path, .. } => {
                if let Some(paths) = &mut local_structure {
                    match lists_children_only(guarded, root) {
                        Some(parent) => child_lists.push(parent),
                        None => paths.push(*path),
                    }
                }
            }
            Raw::LayerRelocates { .. }
            | Raw::Sublayers { .. }
            | Raw::LayerFields { .. }
            | Raw::LayerMetadata { .. }
            | Raw::Variant { .. }
            | Raw::VariantSet { .. }
            | Raw::Selection { .. }
            | Raw::Field { .. }
            | Raw::PrimField { .. } => {
                local_structure = None;
            }
            _ => {}
        }
    }
    if let Some(paths) = &mut local_structure {
        sort::sort_unstable(paths);
        paths.dedup();
        // A slot another step changes otherwise is not a child list alone.
        sort::sort_unstable(&mut child_lists);
        child_lists.dedup();
        child_lists.retain(|path| paths.binary_search(path).is_err());
    } else {
        child_lists.clear();
    }
    sort::sort_unstable(&mut resync_sites);
    resync_sites.dedup();
    sort::sort_unstable(&mut inert_sites);
    let mut other_sites: Vec<(LayerId, PathId)> = steps
        .iter()
        .flat_map(|g| {
            [&g.written, &g.step].map(|raw| raw.change_path(store).map(|path| (raw.layer(), path)))
        })
        .flatten()
        .filter(|site| inert_sites.binary_search(site).is_err())
        .collect();
    sort::sort_unstable(&mut other_sites);
    other_sites.dedup();
    sort::sort_unstable(&mut property_specs);
    property_specs.dedup();
    let argument_inputs = steps.iter().filter_map(argument_input).collect();
    let mut property_fields: Vec<_> = steps
        .iter()
        .filter_map(|g| property_field(&g.written).flatten())
        .collect();
    for guarded in &steps {
        if let Raw::PrimSlots { layer, path, .. } = &guarded.written
            && let Some((specs, fields)) = slot_property_changes(guarded)
        {
            property_specs.extend(specs.into_iter().map(|name| (*layer, *path, name)));
            property_fields.extend(fields.into_iter().map(|field| (*layer, *path, field)));
        }
    }
    sort::sort_unstable(&mut property_specs);
    property_specs.dedup();
    let values = value_edits(&steps, store);
    let properties = property_edits(&steps);
    steps.reverse();
    Ok(Outcome {
        properties,
        values,
        local_structure,
        child_lists,
        resync_sites,
        property_specs,
        argument_inputs,
        inert_sites,
        other_sites,
        property_fields,
        layer_info,
        inverse: Transaction {
            preconditions: Vec::new(),
            ops: steps.into_iter().map(|g| Op::Raw(Box::new(g))).collect(),
        },
        layers,
        touched,
        structural: !structural_layers.is_empty(),
    })
}

/// Whether the layer metadata field `key` composes nothing: the stage
/// reads it from its root and session layers when asked (stage metadata,
/// such as `startTimeCode` and `metersPerUnit`), and no prim index, child
/// list or arc reads it. A change of one in the stage's own layer stack
/// changes the pseudo-root, info-only, as OpenUSD reports it
/// (`UsdStage::_HandleLayersDidChange` records an info change of the
/// pseudo-root's entry; `PcpChanges::DidChange` finds no layer stack,
/// offset or significant change in it). Layer stack fields (`subLayers`,
/// `layerRelocates`, `expressionVariables`), `defaultPrim`, the time code
/// rates that scale layer offsets, and `fallbackPrimTypes`, which types
/// prims, compose.
fn composes_nothing(key: &str) -> bool {
    matches!(
        key,
        "documentation"
            | "comment"
            | "customLayerData"
            | "startTimeCode"
            | "endTimeCode"
            | "upAxis"
            | "metersPerUnit"
            | "kilogramsPerUnit"
    )
}

/// Whether a prim metadata field `key` resyncs its prim: `apiSchemas`
/// changes its schema identity, `kind` the model hierarchy of it and its
/// descendants, and `clips` or `clipSets` where its descendants' values
/// come from. These are the generic fields `UsdStage::_HandleLayersDidChange`
/// (`pxr/usd/usd/stage.cpp`) recomposes for, with `active`, `specifier`
/// and `typeName`, which change spec slots of their own here.
fn resyncs_prim(key: &str) -> bool {
    matches!(key, "apiSchemas" | "kind" | "clips" | "clipSets")
}

/// The prim path of `guarded` when it is a [`Raw::PrimSlots`] step that
/// changes only the child names its prim spec lists (`primChildren`), as
/// creating the pseudo-root's spec for its first listed child, or a
/// namespace edit renaming or reparenting a child, rewrites them. The
/// prim's spec stack, and so its prim index, is as it was; only its child
/// list composes anew. The pseudo-root exists with or without a spec
/// (`SdfLayer::GetPseudoRoot`), so its spec may come or go this way; any
/// other prim keeps its spec, and its `primOrder` too.
///
/// Spec: AOUSD Core §11.3.1 (child order from `primChildren`). OpenUSD
/// records a child spec added or removed at the child's path
/// (`didAddNonInertPrim` and its siblings in `SdfChangeList`), which
/// `PcpChanges::DidChange` resyncs, not at its parent's.
fn lists_children_only(guarded: &Guarded, root: Option<PathId>) -> Option<PathId> {
    let (
        Raw::PrimSlots {
            path,
            main,
            branches,
            ..
        },
        Raw::PrimSlots {
            main: before,
            branches: before_branches,
            ..
        },
    ) = (&guarded.written, &guarded.step)
    else {
        return None;
    };
    let pseudo_root = Some(*path) == root;
    if !branches.same(before_branches) || (!pseudo_root && (main.is_none() || before.is_none())) {
        return None;
    }
    let unlisted = |spec: &Option<PrimSpec>| {
        let mut spec = spec.clone().unwrap_or_default();
        spec.authored_children.clear();
        spec
    };
    unlisted(main).same(&unlisted(before)).then_some(*path)
}

/// The prims of `layer` whose specs the journal's steps only created,
/// removed or listed as the parent of a child, keeping them inert (`over`,
/// no opinions, no branches), or whose steps only change the child names
/// they list ([`lists_children_only`]), sorted, with the steps that
/// changed each path. Authoring at a deep prim creates such an `over` for
/// every ancestor without a spec. The pseudo-root is left out.
///
/// OpenUSD records such a spec as `didAddInertPrim` or
/// `didRemoveInertPrim` in `SdfChangeList` (`SdfSpec::IsInert`, children
/// aside), which `PcpChanges::DidChange` (`pxr/usd/pcp/changes.cpp`) treats
/// as a change of the prim's spec stack (`DidChangeSpecs`), not a resync of
/// its namespace, unless no prim composes there before or after.
fn inert_prims(
    store: &dyn LayerStore,
    steps: &[Guarded],
    layer: LayerId,
    root: Option<PathId>,
    opinions: bool,
) -> (Vec<PathId>, HashMap<PathId, Vec<usize>>) {
    let inert = |spec: &PrimSpec| {
        let mut spec = spec.clone();
        spec.authored_children.clear();
        if spec.specifier == Some(Specifier::Over) {
            spec.specifier = None;
        }
        spec == PrimSpec::default()
    };
    let supports = |step: &Raw, path: PathId| match step {
        Raw::PrimSlots {
            path: at,
            main,
            branches,
            ..
        } if *at == path => {
            branches.as_ref().is_none_or(|b| b.is_empty()) && main.as_ref().is_none_or(inert)
        }
        Raw::Child { .. } => true,
        Raw::PrimSlots { .. } => false,
        // A property spec added or removed changes that property's spec
        // stack, which is reported on its own (`Outcome::property_specs`),
        // and a property field edit that property's fields
        // (`Outcome::property_fields`).
        _ => opinions && property_field(step).is_some(),
    };
    // Metadata that composes nothing beyond its own value, authored on a
    // spec the steps create: the prim's spec stack changes, which reports
    // the prim info-only, all of its fields with it. OpenUSD creates the
    // spec inert and records the field as an info change of its entry.
    let plain_field = |step: &Raw, created: bool| match step {
        Raw::Field {
            property: None,
            key,
            ..
        } => opinions && created && !resyncs_prim(store.tokens().resolve(*key)),
        _ => false,
    };
    let steps: Vec<&Guarded> = steps
        .iter()
        .filter(|g| g.written.layer() == layer)
        .collect();
    // Each step's paths once, and the steps changing each path: a bulk
    // edit has as many candidates as steps, so a scan per candidate would
    // be quadratic.
    let mut by_path: HashMap<PathId, Vec<usize>> = HashMap::new();
    let mut candidates = Vec::new();
    for (at, g) in steps.iter().enumerate() {
        let written = g.written.change_path(store);
        let step = g.step.change_path(store);
        for path in [written, step.filter(|path| Some(*path) != written)]
            .into_iter()
            .flatten()
        {
            by_path.entry(path).or_default().push(at);
        }
        if (matches!(g.written, Raw::PrimSlots { .. } | Raw::Child { .. })
            || (opinions && property_field(&g.written).is_some()))
            && let Some(path) = written.filter(|path| Some(*path) != root)
        {
            candidates.push(path);
        }
    }
    sort::sort_unstable(&mut candidates);
    candidates.dedup();
    candidates.retain(|&path| {
        let changes = &by_path[&path];
        let created = changes.iter().any(|&at| {
            matches!(
                (&steps[at].written, &steps[at].step),
                (
                    Raw::PrimSlots { path: written, main: Some(_), .. },
                    Raw::PrimSlots { main: None, .. },
                ) if *written == path
            )
        });
        changes
            .iter()
            .all(|&at| lists_children_only(steps[at], root) == Some(path))
            || changes.iter().map(|&at| steps[at]).all(|g| {
                (supports(&g.written, path) && supports(&g.step, path))
                    || (plain_field(&g.written, created) && plain_field(&g.step, created))
                    || (opinions && slot_property_changes(g).is_some())
            })
    });
    (candidates, by_path)
}

/// The [`inert_prims`] of `layer`, a layer of `stage`'s own layer stack,
/// that compose from other specs too, and whose specs list only children
/// that compose already.
///
/// An inert spec changes its prim's own spec stack, not the namespace
/// beneath it: its children compose already, and a stronger spec listing
/// names a weaker one lists already adds nothing to the composed child
/// order. So does a spec whose steps only change the child names it lists
/// ([`lists_children_only`]). The pseudo-root is handled by its caller.
/// One listing a new child is recomposed with its namespace, which finds
/// the prims that compose the child through an arc to it.
///
/// Spec: AOUSD Core §10 (namespace composition: a prim exists where any
/// site holds a spec).
fn support_only_prims(
    store: &dyn LayerStore,
    steps: &[Guarded],
    layer: LayerId,
    stage: &Stage,
    root: Option<PathId>,
) -> Vec<PathId> {
    let (mut candidates, by_path) = inert_prims(store, steps, layer, root, false);
    // `by_path` indexes the steps of `layer`.
    let steps: Vec<&Guarded> = steps
        .iter()
        .filter(|g| g.written.layer() == layer)
        .collect();
    candidates.retain(|&path| {
        // A spec whose child names alone change keeps its prim's index too.
        let lists_only = by_path[&path]
            .iter()
            .all(|&at| lists_children_only(steps[at], root) == Some(path));
        lists_only
            || stage.prim_stack(path).is_some_and(|stack| {
                stack
                    .iter()
                    .any(|(id, spec)| *id != layer || spec.prim_path() != path)
            }) && store
                .layer(layer)
                .and_then(|l| l.prim_spec_in(path, &[]))
                .is_none_or(|spec| {
                    spec.authored_children.iter().all(|name| {
                        let child = store.paths().resolve(path).join(&[*name]);
                        store
                            .paths()
                            .lookup(&child)
                            .is_some_and(|child| stage.has_prim(child))
                    })
                })
    });
    candidates
}

fn check_preconditions(store: &mut dyn LayerStore, txn: &Transaction) -> Result<(), EditError> {
    for (index, precondition) in txn.preconditions.iter().enumerate() {
        match precondition {
            Precondition::LayerMetadata {
                layer,
                key,
                expected,
            } => {
                let found = authored_layer_metadata(store, *layer, *key)
                    .ok_or(EditError::UnresolvedPrecondition { index })?;
                if !found.same(expected) {
                    let root = store.paths().intern(crate::Path::root());
                    return Err(EditError::StaleValue {
                        layer: *layer,
                        path: SpecPath::from_prim_path(root),
                        slot: Slot::Metadata(*key),
                    });
                }
            }
            Precondition::Generation { layer, generation } => {
                let found = store
                    .layer(*layer)
                    .ok_or(EditError::UnresolvedPrecondition { index })?
                    .generation();
                if found != *generation {
                    return Err(EditError::StaleGeneration {
                        layer: *layer,
                        expected: *generation,
                        found,
                    });
                }
            }
            Precondition::Authored { at, slot, expected } => {
                let found =
                    authored(store, at, slot).ok_or(EditError::UnresolvedPrecondition { index })?;
                if !found.same(expected) {
                    let path = at
                        .resolve(store.paths())
                        .ok_or(EditError::UnresolvedPrecondition { index })?;
                    return Err(EditError::StaleValue {
                        layer: at.layer(),
                        path,
                        slot: *slot,
                    });
                }
            }
        }
    }
    Ok(())
}

/// The spec path and storage location `at` names, rejecting the
/// pseudo-root and property paths on it ([`Rejection::PseudoRoot`]).
///
/// Every edit addressed to a spec passes through here, so the check is
/// made once. The pseudo-root spec holds a layer's metadata and its root
/// prims, and no prim fields or properties: layer metadata has its own
/// edits ([`Transaction::set_layer_metadata`]), and the root prim order,
/// the one prim field it holds, resolves through
/// [`resolve_including_pseudo_root`].
///
/// OpenUSD rejects these edits on the pseudo-root, at the Usd level
/// (`UsdStage::_SetMetadata`, `UsdPrim::AddAppliedSchema`) and the Sdf
/// level (`_CanEditInfoOnSpec`, `SdfPrimSpec::_ValidateEdit`,
/// `SdfAttributeSpec::New`), except for fields registered on layers,
/// which it writes as the layer's metadata.
///
/// Spec: AOUSD Core §5.1 (the pseudo-root), §7.6.1 (layer metadata).
///
/// [`Transaction::set_layer_metadata`]: super::Transaction::set_layer_metadata
pub(super) fn resolve(
    store: &mut dyn LayerStore,
    at: &Address,
) -> Result<(SpecPath, Loc), Rejection> {
    let (path, loc) = resolve_including_pseudo_root(store, at)?;
    if store.paths().resolve(path.prim_path()).depth() == 0 {
        return Err(Rejection::PseudoRoot);
    }
    Ok((path, loc))
}

/// The spec path and storage location `at` names, the pseudo-root's
/// included: for an edit of the root prim order, the one prim field the
/// pseudo-root spec holds (OpenUSD: `SdfLayer::SetRootPrimOrder`, which
/// `UsdPrim::SetChildrenReorder` on the pseudo-root calls).
pub(super) fn resolve_including_pseudo_root(
    store: &mut dyn LayerStore,
    at: &Address,
) -> Result<(SpecPath, Loc), Rejection> {
    if store.layer(at.layer()).is_none() {
        return Err(Rejection::NoSuchLayer(at.layer()));
    }
    let path = at
        .resolve(store.paths())
        .ok_or_else(|| Rejection::Unmappable(at.named_path()))?;
    let loc = Loc::of(&path);
    Ok((path, loc))
}

pub(super) fn layer(store: &dyn LayerStore, id: LayerId) -> Result<&Layer, Rejection> {
    store.layer(id).ok_or(Rejection::NoSuchLayer(id))
}

fn apply_op(
    store: &mut dyn LayerStore,
    op: &Op,
    stage: Option<&Stage>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    match op {
        Op::LayerMetadata {
            layer: id,
            key,
            value,
        } => {
            let _ = store.paths().intern(crate::Path::root());
            let name = store.tokens().resolve(*key);
            if name == "defaultPrim" {
                if !matches!(value, None | Some(FieldValue::Value(Value::Token(_)))) {
                    return Err(Rejection::ReservedField(*key));
                }
            } else if matches!(
                name,
                "subLayers" | "subLayerOffsets" | "relocates" | "layerRelocates"
            ) {
                return Err(Rejection::ReservedField(*key));
            }
            let index = field_index(&layer(store, *id)?.metadata, *key);
            let previous = authored_layer_metadata(store, *id, *key).expect("resident layer");
            if value.is_none() && previous.is_none() {
                return Ok(());
            }
            journal.run(
                store,
                Raw::LayerMetadata {
                    layer: *id,
                    key: *key,
                    value: value.clone(),
                    index,
                },
            )
        }
        Op::Raw(guarded) => journal.run(store, guarded.step.clone()),
        Op::CreatePrim {
            at,
            specifier,
            type_name,
        } => {
            let (path, loc) = resolve(store, at)?;
            if path.property().is_some() {
                return Err(Rejection::NotAPrim(path));
            }
            let id = at.layer();
            if spec_at(layer(store, id)?, &loc).is_some() {
                return Err(Rejection::SpecExists(path));
            }
            if let Loc::Variant { .. } = loc {
                return ensure(store, id, &loc, journal);
            }
            let (parent, name) = loc.parent(store.paths()).ok_or(Rejection::NotAPrim(path))?;
            ensure(store, id, &parent, journal)?;
            let spec = PrimSpec {
                specifier: Some(*specifier),
                type_name: *type_name,
                ..PrimSpec::default()
            }
            .with_outer_variant_sites(loc.sites().to_vec());
            insert_prim_spec(store, id, loc.prim_path(), spec, journal)?;
            add_child(store, id, &parent, name, journal)
        }
        Op::CreateProperty { at, spec } => {
            let (path, loc) = resolve(store, at)?;
            let name = path
                .property()
                .ok_or(Rejection::NotAProperty(path.clone()))?;
            let id = at.layer();
            if let Some(ty) = &spec.type_name {
                check_registered(&path, ty)?;
                let values = spec.default.iter().chain(
                    spec.time_samples
                        .get()
                        .into_iter()
                        .flatten()
                        .map(|(_, value)| value),
                );
                for value in values {
                    check_type(&path, ty, value)?;
                }
            }
            ensure(store, id, &loc, journal)?;
            let properties = properties_at(store, id, &loc)?;
            if get_property(properties, name).is_some() {
                return Err(Rejection::SpecExists(path));
            }
            let index = properties.len();
            journal.run(
                store,
                Raw::Property {
                    layer: id,
                    loc,
                    name,
                    spec: Some((**spec).clone()),
                    index,
                },
            )
        }
        Op::RemoveSpec { at } => {
            let (path, loc) = resolve(store, at)?;
            let id = at.layer();
            let Some(spec) = spec_at(layer(store, id)?, &loc) else {
                return Ok(());
            };
            match path.property() {
                Some(name) => {
                    let Some(index) = spec.properties().iter().position(|e| e.name == name) else {
                        return Ok(());
                    };
                    journal.run(
                        store,
                        Raw::Property {
                            layer: id,
                            loc,
                            name,
                            spec: None,
                            index,
                        },
                    )
                }
                None => remove_subtree(store, id, &loc, journal),
            }
        }
        Op::BlockAttribute { at } => {
            set_value(store, at, None, &Value::Blocked, stage, journal)?;
            let (path, loc, name) = property_address(store, at)?;
            let id = at.layer();
            let attr =
                attribute_at(store, id, &loc, name, &path)?.expect("set_value ensured attribute");
            if attr.time_samples.is_authored() || attr.spline.is_some() {
                journal.run(
                    store,
                    Raw::Animation {
                        layer: id,
                        loc,
                        name,
                        state: Box::default(),
                    },
                )?;
            }
            Ok(())
        }
        Op::SetDefault { at, value } => {
            let mapped = at.layer_value(value, store);
            let value = mapped.as_ref().unwrap_or(value);
            set_value(store, at, None, value, stage, journal)
        }
        Op::SetTargets { at, targets } => set_targets(store, at, targets.as_ref(), stage, journal),
        Op::EditTargetList {
            at,
            target,
            position,
        } => edit_target_list(store, at, *target, *position, stage, journal),
        Op::AddAppliedSchema { at, name } => add_applied_schema(store, at, *name, journal),
        Op::RemoveAppliedSchema { at, name } => {
            prim_fields::remove_applied_schema(store, at, *name, journal)
        }
        Op::SetPrimField { at, value } => prim_fields::set_field(store, at, value.clone(), journal),
        Op::CopySpec { from, to } => super::copy::copy_spec(store, from, to, journal),
        Op::EditSublayers { layer, edit } => {
            layer_fields::edit_sublayers(store, *layer, edit, journal)
        }
        Op::SetRelocates { layer, relocates } => {
            layer_fields::set_relocates(store, *layer, relocates.as_deref(), journal)
        }
        Op::EditArc { at, edit } => prim_fields::edit_arc(store, at, edit, journal),
        Op::AddVariantSet { at, name, position } => {
            super::variant_sets::add_variant_set(store, at, *name, *position, journal)
        }
        Op::RemoveVariantSet { at, name } => {
            super::variant_sets::remove_variant_set(store, at, *name, journal)
        }
        Op::SetTimeSample { at, time, value } => {
            let mapped = at.layer_value(value, store);
            let value = mapped.as_ref().unwrap_or(value);
            set_value(store, at, Some(at.layer_time(*time)), value, stage, journal)
        }
        Op::ClearDefault { at } => {
            let (path, loc, name) = property_address(store, at)?;
            let id = at.layer();
            match attribute_at(store, id, &loc, name, &path)? {
                Some(attr) if attr.default.is_some() => journal.run(
                    store,
                    Raw::Default {
                        layer: id,
                        loc,
                        name,
                        value: None,
                    },
                ),
                _ => Ok(()),
            }
        }
        Op::RemoveTimeSample { at, time } => {
            let (path, loc, name) = property_address(store, at)?;
            let id = at.layer();
            let time = at.layer_time(*time);
            let has_sample = attribute_at(store, id, &loc, name, &path)?
                .and_then(|attr| attr.time_samples.get())
                .is_some_and(|samples| samples.iter().any(|(t, _)| t.total_cmp(&time).is_eq()));
            if !has_sample {
                return Ok(());
            }
            journal.run(
                store,
                Raw::Sample {
                    layer: id,
                    loc,
                    name,
                    time,
                    value: None,
                    keep_empty: false,
                },
            )
        }
        Op::SetMetadata { at, key, value } => {
            check_field(store, *key)?;
            let mapped = at.layer_field(value, store);
            let value = mapped.as_ref().unwrap_or(value);
            let (path, loc) = resolve(store, at)?;
            let id = at.layer();
            let property = path.property();
            let index = match property {
                Some(name) => {
                    let spec = spec_at(layer(store, id)?, &loc)
                        .and_then(|spec| get_property(spec.properties(), name))
                        .ok_or(Rejection::NoSuchSpec(path.clone()))?;
                    field_index(&spec.metadata, *key)
                }
                None => {
                    ensure(store, id, &loc, journal)?;
                    let spec = spec_at(layer(store, id)?, &loc)
                        .ok_or(Rejection::NoSuchSpec(path.clone()))?;
                    field_index(spec.fields(), *key)
                }
            };
            journal.run(
                store,
                Raw::Field {
                    layer: id,
                    loc,
                    property,
                    key: *key,
                    value: Some(value.clone()),
                    index,
                },
            )
        }
        Op::ClearMetadata { at, key } => {
            let (path, loc) = resolve(store, at)?;
            let id = at.layer();
            let property = path.property();
            let Some(spec) = spec_at(layer(store, id)?, &loc) else {
                return Ok(());
            };
            let fields = match property {
                Some(name) => match get_property(spec.properties(), name) {
                    Some(spec) => spec.metadata.as_slice(),
                    None => return Ok(()),
                },
                None => spec.fields(),
            };
            let Some(index) = fields.iter().position(|e| e.name == *key) else {
                return Ok(());
            };
            journal.run(
                store,
                Raw::Field {
                    layer: id,
                    loc,
                    property,
                    key: *key,
                    value: None,
                    index,
                },
            )
        }
        Op::SetVariantSelection { at, set, variant } => {
            let (path, loc) = resolve(store, at)?;
            if path.property().is_some() {
                return Err(Rejection::NotAPrim(path));
            }
            let id = at.layer();
            match variant {
                Some(_) => ensure(store, id, &loc, journal)?,
                None => {
                    let current = spec_at(layer(store, id)?, &loc)
                        .and_then(|spec| spec.variant_selection(*set));
                    if current.is_none() {
                        return Ok(());
                    }
                }
            }
            journal.run(
                store,
                Raw::Selection {
                    layer: id,
                    loc,
                    set: *set,
                    variant: *variant,
                },
            )
        }
    }
}

/// The position a field `key` has, or would be appended at.
pub(super) fn field_index(fields: &[FieldEntry], key: TokenId) -> usize {
    fields
        .iter()
        .position(|e| e.name == key)
        .unwrap_or(fields.len())
}

/// Fields the layer model keeps in dedicated members.
const RESERVED_FIELDS: &[&str] = &[
    "active",
    "connectionPaths",
    "custom",
    "default",
    "inherits",
    "instanceable",
    "payload",
    "primChildren",
    "primOrder",
    "properties",
    "propertyChildren",
    "propertyOrder",
    "references",
    "specializes",
    "specifier",
    "spline",
    "targetPaths",
    "timeSamples",
    "typeName",
    "variability",
    "variantSelection",
    "variantSetChildren",
    "variantSetNames",
    "variantSets",
    "variants",
];

fn check_field(store: &dyn LayerStore, key: TokenId) -> Result<(), Rejection> {
    if RESERVED_FIELDS.contains(&store.tokens().resolve(key)) {
        return Err(Rejection::ReservedField(key));
    }
    Ok(())
}

/// The spec path, storage location and property name of a property
/// address.
fn property_address(
    store: &mut dyn LayerStore,
    at: &Address,
) -> Result<(SpecPath, Loc, TokenId), Rejection> {
    let (path, loc) = resolve(store, at)?;
    let name = path
        .property()
        .ok_or(Rejection::NotAProperty(path.clone()))?;
    Ok((path, loc, name))
}

/// The attribute `name` at `loc`, if authored; rejects a relationship.
fn attribute_at<'a>(
    store: &'a dyn LayerStore,
    id: LayerId,
    loc: &Loc,
    name: TokenId,
    path: &SpecPath,
) -> Result<Option<&'a PropertySpec>, Rejection> {
    let property = spec_at(layer(store, id)?, loc).and_then(|s| get_property(s.properties(), name));
    match property {
        Some(spec) if spec.kind != PropertyKind::Attribute => {
            Err(Rejection::NotAnAttribute(path.clone()))
        }
        other => Ok(other),
    }
}

fn properties_at<'a>(
    store: &'a dyn LayerStore,
    id: LayerId,
    loc: &Loc,
) -> Result<&'a [PropertyEntry], Rejection> {
    Ok(spec_at(layer(store, id)?, loc)
        .map(SpecRef::properties)
        .unwrap_or_default())
}

/// Rejects an unregistered type, which an attribute cannot be created
/// with or given a value of (AOUSD Core §6.2, §7.6.4.1.1).
fn check_registered(path: &SpecPath, ty: &PropertyType) -> Result<(), Rejection> {
    if ty.value_type().is_some() {
        return Ok(());
    }
    Err(Rejection::UnknownValueType {
        path: path.clone(),
        type_name: Arc::from(ty.type_name()),
    })
}

fn check_type(path: &SpecPath, ty: &PropertyType, value: &Value) -> Result<(), Rejection> {
    check_registered(path, ty)?;
    if conforms(ty, value) {
        Ok(())
    } else {
        Err(Rejection::TypeMismatch {
            path: path.clone(),
            declared: Arc::from(ty.type_name()),
            offered: crate::value_type::offered_type_name(value),
        })
    }
}

/// The schema definition of the property `name` of the stage prim `at`
/// addresses, in the prim definition the transaction's steps so far leave
/// it: `None` for a spec address, a stage without schemas, or a property
/// the definition does not hold.
///
/// While no step has written the fields of a spec at one of the prim's
/// sites ([`Stage::spec_sites`]), that is the definition the stage
/// composed. Otherwise the prim's type name and applied schemas are read
/// again from those sites as the steps left them: the strongest type name
/// (an empty one and `__AnyType__` are no opinion), mapped through the
/// layer's `fallbackPrimTypes` when it names no concrete schema, and the
/// `apiSchemas` list ops composed. So a prim a transaction defines or
/// retypes, or applies a schema to, has that schema's properties to plan
/// its later edits against, and an opinion weaker than another site's
/// stays masked, as OpenUSD's recomposition after each call gives them.
///
/// OpenUSD: `UsdStage::_CreateAttributeSpecForEditing` and
/// `UsdRelationship::_CreateSpec` read `UsdPrim::GetPrimDefinition`,
/// which `Usd_PrimTypeInfoCache` builds from `typeName` and `apiSchemas`.
///
/// Spec: AOUSD Core §12.2 (strongest opinion), §12.4 (list ops),
/// §13.3.1 (typeless prims), §13.3.2.3 (the prim definition).
fn planned_definition(
    store: &dyn LayerStore,
    at: &Address,
    name: TokenId,
    stage: Option<&Stage>,
    journal: &Journal<'_>,
) -> Option<PropertyDefinition> {
    let (stage, (prim, _)) = stage.zip(at.stage_path())?;
    if journal.prim_fields.is_empty() {
        return stage.property_definition(prim, name);
    }
    let target = at.resolve(store.paths())?.prim_spec();
    let sites = stage.spec_sites(prim, at.layer(), &target);
    if !sites
        .iter()
        .any(|(layer, site)| journal.prim_fields.contains(&(*layer, site.prim_path())))
    {
        return stage.property_definition(prim, name);
    }
    let schemas = stage.schemas()?;
    let tokens = store.tokens();
    let any_type = tokens.lookup("__AnyType__");
    let api_schemas = tokens.lookup("apiSchemas");
    let specs: Vec<SpecRef<'_>> = sites
        .iter()
        .filter_map(|(layer, site)| spec_at(store.layer(*layer)?, &Loc::of(site)))
        .collect();
    let type_name = specs.iter().find_map(|spec| match spec {
        SpecRef::Prim(spec) => spec
            .type_name
            .filter(|&ty| ty != TokenId::EMPTY && Some(ty) != any_type),
        SpecRef::Variant(_) => None,
    });
    let concrete = |ty| {
        schemas
            .schema(ty)
            .is_some_and(|schema| schema.kind == SchemaKind::ConcreteTyped)
    };
    let type_name = match type_name {
        Some(ty) if !concrete(ty) => stage
            .fallback_type_map(store)
            .get(&ty)
            .copied()
            .or(Some(ty)),
        other => other,
    };
    let lists = specs.iter().filter_map(|spec| {
        let field = crate::doc::get_field(spec.fields(), &api_schemas?);
        match field {
            Some(FieldValue::TokenListOp(list)) => Some(&**list),
            _ => None,
        }
    });
    let applied = resolve_list_chain::<TokenId>(&[], lists);
    schemas.property_definition(type_name, &applied, name, tokens)
}

/// Sets the default (`time` is `None`) or the time sample at the layer
/// time `time` of the attribute `at`, creating its spec when missing.
///
/// A created spec takes its type and variability from the prim's schemas
/// when they define the attribute ([`planned_definition`], which sees the
/// type and applied schemas the transaction's earlier edits author), and
/// otherwise from the composed declaration, as OpenUSD creates it
/// (`UsdPrim::CreateAttribute`, which a schema's `Create*Attr` calls with
/// the definition's type and variability).
///
/// Spec: AOUSD Core §13.3.2.3 (the prim definition declares the prim's
/// schema properties).
fn set_value(
    store: &mut dyn LayerStore,
    at: &Address,
    time: Option<f64>,
    value: &Value,
    stage: Option<&Stage>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let (path, loc, name) = property_address(store, at)?;
    let id = at.layer();
    let defined = planned_definition(store, at, name, stage, journal);
    if defined
        .as_ref()
        .is_some_and(|d| d.kind != PropertyKind::Attribute)
    {
        return Err(Rejection::NotAnAttribute(path));
    }
    let declared = stage
        .zip(at.stage_path())
        .and_then(|(stage, (prim, _))| stage.resolve_property_declaration(prim, name))
        .map(|d| (d.type_name, d.variability));
    let declared = match defined {
        Some(defined) if defined.type_name.is_some() => {
            Some((defined.type_name, defined.variability))
        }
        _ => declared,
    };
    let existing = attribute_at(store, id, &loc, name, &path)?;
    let ty = existing
        .as_ref()
        .and_then(|spec| spec.type_name.clone())
        .or_else(|| declared.as_ref().and_then(|(ty, _)| ty.clone()))
        .ok_or(Rejection::UndeclaredType(path.clone()))?;
    check_type(&path, &ty, value)?;
    if existing.is_some() {
        let step = match time {
            None => Raw::Default {
                layer: id,
                loc,
                name,
                value: Some(value.clone()),
            },
            Some(time) => Raw::Sample {
                layer: id,
                loc,
                name,
                time,
                value: Some(value.clone()),
                keep_empty: false,
            },
        };
        return journal.run(store, step);
    }
    ensure(store, id, &loc, journal)?;
    let mut spec = PropertySpec::typed_attribute(ty);
    spec.variability = declared.map_or(Variability::Varying, |(_, variability)| variability);
    match time {
        None => spec.default = Some(value.clone()),
        Some(time) => spec.time_samples = alloc::vec![(time, value.clone())].into(),
    }
    let index = properties_at(store, id, &loc)?.len();
    journal.run(
        store,
        Raw::Property {
            layer: id,
            loc,
            name,
            spec: Some(spec),
            index,
        },
    )
}

/// Adds `name` to the `apiSchemas` list op of the prim spec `at` (see
/// [`Transaction::add_applied_schema`]).
///
/// OpenUSD: `UsdPrim::AddAppliedSchema`.
fn add_applied_schema(
    store: &mut dyn LayerStore,
    at: &Address,
    name: TokenId,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let key = store.tokens().intern("apiSchemas");
    let (path, loc) = resolve(store, at)?;
    if path.property().is_some() {
        return Err(Rejection::NotAPrim(path));
    }
    let id = at.layer();
    ensure(store, id, &loc, journal)?;
    let spec = spec_at(layer(store, id)?, &loc).ok_or(Rejection::NoSuchSpec(path.clone()))?;
    let mut list = match crate::doc::get_field(spec.fields(), &key) {
        Some(FieldValue::TokenListOp(list)) => list.clone(),
        _ => Box::new(ListOp::default()),
    };
    match &mut list.explicit {
        Some(explicit) if explicit.contains(&name) => return Ok(()),
        Some(explicit) => explicit.push(name),
        None if list.prepend.contains(&name) || list.append.contains(&name) => return Ok(()),
        None => list.prepend.push(name),
    }
    let index = field_index(spec.fields(), key);
    journal.run(
        store,
        Raw::Field {
            layer: id,
            loc,
            property: None,
            key,
            value: Some(FieldValue::TokenListOp(list)),
            index,
        },
    )
}

/// Sets (`Some`) or clears (`None`) the targets or connections of the
/// property `at`, creating the property spec when setting and it is
/// missing (see [`Transaction::set_targets`]). Every target is mapped
/// through `at` first; one it does not map rejects the edit.
///
/// Spec: AOUSD Core §7.6.4.2.3, §7.6.5.1.1, §12.4.
fn set_targets(
    store: &mut dyn LayerStore,
    at: &Address,
    targets: Option<&ListOp<TargetPath>>,
    stage: Option<&Stage>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let targets = match targets {
        None => None,
        Some(targets) => {
            let mut mapped = targets.clone();
            for list in mapped.lists_mut() {
                for target in list.iter_mut() {
                    *target = map_target(store, at, *target)?;
                }
            }
            Some(mapped)
        }
    };
    write_targets(store, at, targets.as_ref(), stage, journal)
}

/// `target` named in the layer's namespace, as the address names its
/// paths (see [`Transaction::set_targets`]).
fn map_target(
    store: &mut dyn LayerStore,
    at: &Address,
    target: TargetPath,
) -> Result<TargetPath, Rejection> {
    at.map_target(target, store.tokens(), store.paths())
        .ok_or(Rejection::UnmappableTarget(target))
}

/// Adds `target` at `position` to the targets or connections authored on
/// the property spec at `at`, or removes it (`None`), as OpenUSD's list
/// editors do (see [`prim_fields::edit_list`]). A missing spec is created
/// as [`Transaction::set_targets`] creates it, so removing a target from a
/// property this layer does not author yet authors its deletion.
///
/// OpenUSD: `UsdRelationship::AddTarget` (`Usd_InsertListItem`) and
/// `RemoveTarget` (`SdfListEditorProxy::Remove`), `UsdAttribute`'s
/// `AddConnection` and `RemoveConnection` (`pxr/usd/usd/relationship.cpp`,
/// `attribute.cpp`, `valueUtils.h`).
///
/// Spec: AOUSD Core §7.6.4.2.3 (`connectionPaths`), §7.6.5.1.1
/// (`targetPaths`), §12.4 (list ops).
fn edit_target_list(
    store: &mut dyn LayerStore,
    at: &Address,
    target: TargetPath,
    position: Option<ListPosition>,
    stage: Option<&Stage>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let (_, loc, name) = property_address(store, at)?;
    let target = map_target(store, at, target)?;
    let authored = spec_at(layer(store, at.layer())?, &loc)
        .and_then(|spec| get_property(spec.properties(), name))
        .and_then(|spec| spec.targets.clone())
        .unwrap_or_default();
    let edited = prim_fields::edit_list(*authored, target, position);
    write_targets(store, at, Some(&edited), stage, journal)
}

/// Authors `targets`, already in the layer's namespace, on the property
/// spec at `at`, creating the spec when it is missing: from the prim's
/// schema definition of the property, else from the stage's composed
/// declaration, else as a `custom` relationship.
///
/// OpenUSD: `UsdRelationship::_CreateSpec(fallbackCustom = true)`, after
/// `UsdStage::_CreatePropertySpecForEditing`, which stamps the definition
/// or the strongest authored spec (`pxr/usd/usd/relationship.cpp`,
/// `stage.cpp`).
///
/// Spec: AOUSD Core §7.6.3.1.1 (`custom`), §12.2.4.
fn write_targets(
    store: &mut dyn LayerStore,
    at: &Address,
    targets: Option<&ListOp<TargetPath>>,
    stage: Option<&Stage>,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let (path, loc, name) = property_address(store, at)?;
    let id = at.layer();
    let existing = spec_at(layer(store, id)?, &loc)
        .and_then(|spec| get_property(spec.properties(), name))
        .map(|spec| spec.targets.is_some());
    match (existing, targets) {
        (Some(_), Some(_)) | (Some(true), None) => journal.run(
            store,
            Raw::Targets {
                layer: id,
                loc,
                name,
                targets: targets.cloned(),
            },
        ),
        (Some(false) | None, None) => Ok(()),
        (None, Some(targets)) => {
            let prim = at.stage_path().map(|(prim, _)| prim);
            let defined = planned_definition(store, at, name, stage, journal);
            let declared = stage
                .zip(prim)
                .and_then(|(stage, prim)| stage.resolve_property_declaration(prim, name));
            let mut spec = match (defined, declared) {
                (Some(defined), _) if defined.kind == PropertyKind::Relationship => {
                    PropertySpec::relationship()
                }
                (Some(defined), _) => {
                    let ty = defined
                        .type_name
                        .ok_or(Rejection::UndeclaredType(path.clone()))?;
                    check_registered(&path, &ty)?;
                    let mut spec = PropertySpec::typed_attribute(ty);
                    spec.variability = defined.variability;
                    spec
                }
                (None, Some(declared)) if declared.kind == PropertyKind::Attribute => {
                    let ty = declared
                        .type_name
                        .ok_or(Rejection::UndeclaredType(path.clone()))?;
                    check_registered(&path, &ty)?;
                    let mut spec = PropertySpec::typed_attribute(ty);
                    spec.variability = declared.variability;
                    spec.custom = declared.custom;
                    spec
                }
                (None, Some(declared)) => PropertySpec {
                    custom: declared.custom,
                    ..PropertySpec::relationship()
                },
                // OpenUSD: `UsdRelationship::_CreateSpec(fallbackCustom =
                // true)` stamps a relationship nothing declares `custom`.
                (None, None) => PropertySpec::relationship().custom(),
            };
            spec.targets = Some(Box::new(targets.clone()));
            ensure(store, id, &loc, journal)?;
            let index = properties_at(store, id, &loc)?.len();
            journal.run(
                store,
                Raw::Property {
                    layer: id,
                    loc,
                    name,
                    spec: Some(spec),
                    index,
                },
            )
        }
    }
}

/// Creates the spec at `loc` if it is missing: a prim spec as an `over`,
/// after its missing ancestors, or a variant spec, after its host.
///
/// OpenUSD: `SdfCreatePrimInLayer`.
pub(super) fn ensure(
    store: &mut dyn LayerStore,
    id: LayerId,
    loc: &Loc,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    if spec_at(layer(store, id)?, loc).is_some() {
        return Ok(());
    }
    match loc {
        Loc::Prim { path, sites } => {
            let parent = loc.parent(store.paths());
            if let Some((parent, _)) = &parent {
                ensure(store, id, parent, journal)?;
            }
            let spec = PrimSpec {
                specifier: parent.is_some().then_some(Specifier::Over),
                ..PrimSpec::default()
            }
            .with_outer_variant_sites(sites.clone());
            insert_prim_spec(store, id, *path, spec, journal)?;
            match parent {
                Some((parent, name)) => add_child(store, id, &parent, name, journal),
                None => Ok(()),
            }
        }
        Loc::Variant { .. } => {
            // The prim spec hosting the set, or for a set nested in another
            // branch (`/P{a=x}{b=y}`), that branch's variant spec.
            let (holder, site) = loc.variant_parts().expect("a variant location");
            ensure(store, id, &holder, journal)?;
            let Some(holder_spec) = spec_at(layer(store, id)?, &holder) else {
                return Err(diverged(store, &holder));
            };
            let created_set = !holder_spec.variant_sets().contains_key(&site.set);
            journal.run(
                store,
                Raw::Variant {
                    layer: id,
                    host: holder.clone(),
                    set: site.set,
                    variant: site.variant,
                    spec: Some(VariantSpec::default()),
                    drop_set: created_set,
                },
            )?;
            // `_FindOrCreateVariantSpec` lists a set it creates, at the
            // front of the holder's `variantSetNames`, with the list
            // editor's `Prepend`; an existing set's listing is left alone.
            //
            // Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`). OpenUSD:
            // `pxr/usd/sdf/primSpec.cpp`.
            if created_set {
                super::variant_sets::prepend_variant_set_name(
                    store, id, holder, site.set, journal,
                )?;
            }
            Ok(())
        }
    }
}

/// Stores the prim spec `spec` at `path`, keeping [`Layer::prims`] for the
/// spec outside every branch, as [`Layer::insert_prim`] does.
pub(super) fn insert_prim_spec(
    store: &mut dyn LayerStore,
    id: LayerId,
    path: PathId,
    spec: PrimSpec,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let found = layer(store, id)?;
    let mut main = found.prims.get(&path).cloned();
    let mut branches = found.variant_prims.get(&path).cloned();
    match main.take() {
        None => main = Some(spec),
        Some(existing) if !spec.outer_variant_sites().is_empty() => {
            main = Some(existing);
            branches.get_or_insert_with(Vec::new).push(spec);
        }
        Some(displaced) => {
            main = Some(spec);
            branches.get_or_insert_with(Vec::new).push(displaced);
        }
    }
    journal.run(
        store,
        Raw::PrimSlots {
            layer: id,
            path,
            main,
            branches,
        },
    )
}

/// Lists `name` among the children of the spec at `parent`, unless it is.
pub(super) fn add_child(
    store: &mut dyn LayerStore,
    id: LayerId,
    parent: &Loc,
    name: TokenId,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    let spec = spec_at(layer(store, id)?, parent).ok_or_else(|| diverged(store, parent))?;
    let children = spec.children();
    let listed = match parent {
        Loc::Prim { path, sites } if sites.is_empty() => {
            match journal.children.get_mut(&(id, *path)) {
                Some(Some(names)) => names.contains(&name),
                Some(scanned) => {
                    let names: hashbrown::HashSet<TokenId> = children.iter().copied().collect();
                    let listed = names.contains(&name);
                    *scanned = Some(names);
                    listed
                }
                None => {
                    journal.children.insert((id, *path), None);
                    children.contains(&name)
                }
            }
        }
        _ => children.contains(&name),
    };
    if listed {
        return Ok(());
    }
    let index = children.len();
    journal.run(
        store,
        Raw::Child {
            layer: id,
            parent: parent.clone(),
            name,
            index: Some(index),
        },
    )
}

/// Removes the prim or variant spec at `loc` with every spec authored
/// beneath it, and its name from its parent's children.
pub(super) fn remove_subtree(
    store: &mut dyn LayerStore,
    id: LayerId,
    loc: &Loc,
    journal: &mut Journal<'_>,
) -> Result<(), Rejection> {
    // Every prim spec beneath `loc`: at or under its prim path, enclosed by
    // its branches, and by no other branch hosted above it.
    let root = loc.prim_path();
    let root_depth = store.paths().resolve(root).depth();
    let (sites, own) = match loc {
        Loc::Prim { sites, .. } => (sites.as_slice(), true),
        Loc::Variant { sites, .. } => (sites.as_slice(), false),
    };
    let beneath = |paths: &PathInterner, path: PathId, spec: &PrimSpec| -> bool {
        let Some(rel) = paths.resolve(path).strip_prefix(paths.resolve(root)) else {
            return false;
        };
        if rel.is_empty() {
            return own && spec.outer_variant_sites() == sites;
        }
        let inner = spec.outer_variant_sites().strip_prefix(sites);
        inner.is_some_and(|inner| {
            inner.iter().all(|site: &VariantSelectionSite| {
                paths.resolve(site.host_path).depth() >= root_depth
            })
        })
    };
    let found = layer(store, id)?;
    // Earlier deletions only make the snapshot a superset. Check candidates
    // against current storage below, including when removing an ancestor of
    // a previously removed spec. The journal discards the snapshot after a
    // source-slot write could have introduced paths it does not contain.
    let mut candidates: Vec<PathId> = journal
        .namespace
        .and_then(|index| index.subtree(id, root))
        .unwrap_or_else(|| {
            found
                .prims
                .keys()
                .chain(found.variant_prims.keys())
                .copied()
                .collect()
        });
    sort::sort_unstable(&mut candidates);
    candidates.dedup();
    let mut steps = Vec::new();
    for path in candidates {
        let paths = store.paths();
        let main = found.prims.get(&path);
        let branches = found.variant_prims.get(&path);
        let removes = main
            .into_iter()
            .chain(branches.into_iter().flatten())
            .any(|spec| beneath(paths, path, spec));
        if !removes {
            continue;
        }
        let mut kept = main
            .into_iter()
            .chain(branches.into_iter().flatten())
            .filter(|spec| !beneath(paths, path, spec))
            .cloned();
        // The spec outside every branch keeps the main slot; otherwise the
        // first remaining spec takes it, as ingestion leaves it.
        let new_main = kept.next();
        let rest: Vec<PrimSpec> = kept.collect();
        steps.push(Raw::PrimSlots {
            layer: id,
            path,
            main: new_main,
            branches: (!rest.is_empty()).then_some(rest),
        });
    }
    match loc {
        Loc::Prim { .. } => {
            if let Some((parent, name)) = loc.parent(store.paths())
                && spec_at(layer(store, id)?, &parent)
                    .is_some_and(|spec| spec.children().contains(&name))
            {
                steps.push(Raw::Child {
                    layer: id,
                    parent,
                    name,
                    index: None,
                });
            }
        }
        Loc::Variant { .. } => {
            let (host, site) = loc.variant_parts().expect("a variant location");
            steps.push(Raw::Variant {
                layer: id,
                host,
                set: site.set,
                variant: site.variant,
                spec: None,
                drop_set: false,
            });
        }
    }
    for step in steps {
        journal.run(store, step)?;
    }
    Ok(())
}

fn diverged(store: &dyn LayerStore, loc: &Loc) -> Rejection {
    Rejection::Diverged(loc.spec_path(store.paths()))
}

/// Applies one storage step and returns the step that undoes it.
fn apply_raw(store: &mut dyn LayerStore, step: &Raw) -> Result<Raw, Rejection> {
    let id = step.layer();
    let diverged_at = |store: &dyn LayerStore, loc: &Loc| diverged(store, loc);
    match step {
        Raw::LayerMetadata {
            key, value, index, ..
        } => {
            let previous =
                authored_layer_metadata(store, id, *key).ok_or(Rejection::NoSuchLayer(id))?;
            let is_default = store.tokens().resolve(*key) == "defaultPrim";
            let found = store.layer_mut(id).ok_or(Rejection::NoSuchLayer(id))?;
            let old_index = field_index(&found.metadata, *key);
            if is_default {
                found.default_prim = match value {
                    Some(FieldValue::Value(Value::Token(v))) => Some(*v),
                    _ => None,
                };
            } else if let Some(position) = found.metadata.iter().position(|e| e.name == *key) {
                if let Some(value) = value {
                    found.metadata[position].value = value.clone();
                } else {
                    found.metadata.remove(position);
                }
            } else if let Some(value) = value {
                found.metadata.insert(
                    (*index).min(found.metadata.len()),
                    FieldEntry {
                        name: *key,
                        value: value.clone(),
                    },
                );
            }
            Ok(Raw::LayerMetadata {
                layer: id,
                key: *key,
                value: previous,
                index: old_index,
            })
        }
        Raw::LayerRelocates { relocates, .. } => {
            let found = store.layer_mut(id).ok_or(Rejection::NoSuchLayer(id))?;
            Ok(Raw::LayerRelocates {
                layer: id,
                relocates: core::mem::replace(&mut found.relocates, relocates.clone()),
            })
        }
        Raw::Sublayers { sublayers, .. } => {
            let found = store.layer_mut(id).ok_or(Rejection::NoSuchLayer(id))?;
            // Each replacement sublayer has its own offset.
            found.unpaired_sublayer_offsets = None;
            Ok(Raw::Sublayers {
                layer: id,
                sublayers: core::mem::replace(&mut found.sublayers, sublayers.clone()),
            })
        }
        Raw::LayerFields {
            default_prim,
            metadata,
            ..
        } => {
            let found = store.layer_mut(id).ok_or(Rejection::NoSuchLayer(id))?;
            Ok(Raw::LayerFields {
                layer: id,
                default_prim: core::mem::replace(&mut found.default_prim, *default_prim),
                metadata: core::mem::replace(&mut found.metadata, metadata.clone()),
            })
        }
        Raw::PrimSlots {
            path,
            main,
            branches,
            ..
        } => {
            let found = store.layer_mut(id).ok_or(Rejection::NoSuchLayer(id))?;
            let old_main = match main {
                Some(spec) => found.prims.insert(*path, spec.clone()),
                None => found.prims.remove(path),
            };
            let old_branches = match branches {
                Some(specs) => found.variant_prims.insert(*path, specs.clone()),
                None => found.variant_prims.remove(path),
            };
            Ok(Raw::PrimSlots {
                layer: id,
                path: *path,
                main: old_main,
                branches: old_branches,
            })
        }
        Raw::Child {
            parent,
            name,
            index,
            ..
        } => {
            let Some(mut spec) = store.layer_mut(id).and_then(|l| spec_at_mut(l, parent)) else {
                return Err(diverged_at(store, parent));
            };
            let children = spec.children();
            let old = match index {
                Some(index) => {
                    children.insert((*index).min(children.len()), *name);
                    None
                }
                None => match children.iter().position(|child| child == name) {
                    Some(position) => {
                        children.remove(position);
                        Some(position)
                    }
                    None => return Err(diverged_at(store, parent)),
                },
            };
            Ok(Raw::Child {
                layer: id,
                parent: parent.clone(),
                name: *name,
                index: old,
            })
        }
        Raw::Variant {
            host,
            set,
            variant,
            spec,
            drop_set,
            ..
        } => {
            let Some(holder) = store.layer_mut(id).and_then(|l| spec_at_mut(l, host)) else {
                return Err(diverged_at(store, host));
            };
            let variant_sets = holder.into_variant_sets();
            match spec {
                Some(spec) => {
                    let created_set = !variant_sets.contains_key(set);
                    let old = variant_sets
                        .get_or_insert_default(*set)
                        .variants
                        .insert(*variant, spec.clone());
                    Ok(Raw::Variant {
                        layer: id,
                        host: host.clone(),
                        set: *set,
                        variant: *variant,
                        spec: old,
                        drop_set: created_set,
                    })
                }
                None => {
                    let Some(set_spec) = variant_sets.get_mut(set) else {
                        return Err(diverged_at(store, host));
                    };
                    let Some(old) = set_spec.variants.remove(variant) else {
                        return Err(diverged_at(store, host));
                    };
                    if *drop_set && set_spec.variants.is_empty() {
                        variant_sets.remove(set);
                    }
                    Ok(Raw::Variant {
                        layer: id,
                        host: host.clone(),
                        set: *set,
                        variant: *variant,
                        spec: Some(old),
                        drop_set: false,
                    })
                }
            }
        }
        Raw::VariantSet {
            host,
            set,
            spec,
            index,
            ..
        } => {
            let Some(holder) = store.layer_mut(id).and_then(|l| spec_at_mut(l, host)) else {
                return Err(diverged_at(store, host));
            };
            let variant_sets = holder.into_variant_sets();
            let (old, old_index) = match (spec, variant_sets.index_of(set)) {
                (Some(spec), Some(position)) => (variant_sets.insert(*set, spec.clone()), position),
                (Some(spec), None) => {
                    let index = (*index).min(variant_sets.len());
                    variant_sets.insert_at(index, *set, spec.clone());
                    (None, index)
                }
                (None, Some(position)) => (variant_sets.remove(set), position),
                (None, None) => return Err(diverged_at(store, host)),
            };
            Ok(Raw::VariantSet {
                layer: id,
                host: host.clone(),
                set: *set,
                spec: old,
                index: old_index,
            })
        }
        Raw::Property {
            loc,
            name,
            spec,
            index,
            ..
        } => {
            let Some(mut target) = store.layer_mut(id).and_then(|l| spec_at_mut(l, loc)) else {
                return Err(diverged_at(store, loc));
            };
            let properties = target.properties();
            let position = properties.iter().position(|e| e.name == *name);
            let (old, old_index) = match (spec, position) {
                (Some(spec), Some(position)) => (
                    Some(core::mem::replace(
                        &mut properties[position].spec,
                        Arc::new(spec.clone()),
                    )),
                    position,
                ),
                (Some(spec), None) => {
                    let index = (*index).min(properties.len());
                    properties.insert(
                        index,
                        PropertyEntry {
                            name: *name,
                            spec: Arc::new(spec.clone()),
                        },
                    );
                    (None, index)
                }
                (None, Some(position)) => (Some(properties.remove(position).spec), position),
                (None, None) => return Err(diverged_at(store, loc)),
            };
            Ok(Raw::Property {
                layer: id,
                loc: loc.clone(),
                name: *name,
                spec: old.map(Arc::unwrap_or_clone),
                index: old_index,
            })
        }
        Raw::Targets {
            loc, name, targets, ..
        } => {
            let property = store
                .layer_mut(id)
                .and_then(|l| spec_at_mut(l, loc))
                .and_then(|mut s| {
                    let properties = s.properties();
                    let index = properties.iter().position(|e| e.name == *name)?;
                    Some(core::mem::replace(
                        &mut Arc::make_mut(&mut properties[index].spec).targets,
                        targets.clone().map(Box::new),
                    ))
                });
            let Some(old) = property else {
                return Err(diverged_at(store, loc));
            };
            Ok(Raw::Targets {
                layer: id,
                loc: loc.clone(),
                name: *name,
                targets: old.map(|boxed| *boxed),
            })
        }
        Raw::Default {
            loc, name, value, ..
        } => {
            let attribute = store
                .layer_mut(id)
                .and_then(|l| spec_at_mut(l, loc))
                .and_then(|mut s| {
                    let properties = s.properties();
                    let index = properties.iter().position(|e| e.name == *name)?;
                    Some(core::mem::replace(
                        &mut Arc::make_mut(&mut properties[index].spec).default,
                        value.clone(),
                    ))
                });
            let Some(old) = attribute else {
                return Err(diverged_at(store, loc));
            };
            Ok(Raw::Default {
                layer: id,
                loc: loc.clone(),
                name: *name,
                value: old,
            })
        }
        Raw::Animation {
            loc, name, state, ..
        } => {
            let old = store
                .layer_mut(id)
                .and_then(|l| spec_at_mut(l, loc))
                .and_then(|mut s| {
                    let property = s.properties().iter_mut().find(|p| p.name == *name)?;
                    let property = Arc::make_mut(&mut property.spec);
                    Some(AnimationState {
                        samples: core::mem::replace(
                            &mut property.time_samples,
                            state.samples.clone(),
                        ),
                        spline: core::mem::replace(&mut property.spline, state.spline.clone()),
                    })
                })
                .ok_or_else(|| diverged_at(store, loc))?;
            Ok(Raw::Animation {
                layer: id,
                loc: loc.clone(),
                name: *name,
                state: Box::new(old),
            })
        }
        Raw::Sample {
            loc,
            name,
            time,
            value,
            keep_empty,
            ..
        } => {
            let outcome = store
                .layer_mut(id)
                .and_then(|l| spec_at_mut(l, loc))
                .and_then(|mut s| {
                    let properties = s.properties();
                    let index = properties.iter().position(|e| e.name == *name)?;
                    set_sample(
                        &mut Arc::make_mut(&mut properties[index].spec).time_samples,
                        *time,
                        value.clone(),
                        *keep_empty,
                    )
                });
            let Some((old, keep_empty)) = outcome else {
                return Err(diverged_at(store, loc));
            };
            Ok(Raw::Sample {
                layer: id,
                loc: loc.clone(),
                name: *name,
                time: *time,
                value: old,
                keep_empty,
            })
        }
        Raw::Field {
            loc,
            property,
            key,
            value,
            index,
            ..
        } => {
            let outcome = store
                .layer_mut(id)
                .and_then(|l| spec_at_mut(l, loc))
                .and_then(|mut s| {
                    let fields = match property {
                        Some(name) => {
                            let properties = s.properties();
                            let at = properties.iter().position(|e| e.name == *name)?;
                            Arc::make_mut(&mut properties[at].spec).metadata.make_mut()
                        }
                        None => s.fields(),
                    };
                    let position = fields.iter().position(|e| e.name == *key);
                    Some(match (value, position) {
                        (Some(value), Some(position)) => (
                            Some(core::mem::replace(
                                &mut fields[position].value,
                                value.clone(),
                            )),
                            position,
                        ),
                        (Some(value), None) => {
                            let index = (*index).min(fields.len());
                            fields.insert(
                                index,
                                FieldEntry {
                                    name: *key,
                                    value: value.clone(),
                                },
                            );
                            (None, index)
                        }
                        (None, Some(position)) => (Some(fields.remove(position).value), position),
                        (None, None) => return None,
                    })
                });
            let Some((old, old_index)) = outcome else {
                return Err(diverged_at(store, loc));
            };
            Ok(Raw::Field {
                layer: id,
                loc: loc.clone(),
                property: *property,
                key: *key,
                value: old,
                index: old_index,
            })
        }
        Raw::Selection {
            loc, set, variant, ..
        } => {
            let Some(mut target) = store.layer_mut(id).and_then(|l| spec_at_mut(l, loc)) else {
                return Err(diverged_at(store, loc));
            };
            let selections = target.variant_selections();
            let old = match variant {
                Some(variant) => selections.insert(*set, *variant),
                None => selections.remove(*set),
            };
            Ok(Raw::Selection {
                layer: id,
                loc: loc.clone(),
                set: *set,
                variant: old,
            })
        }
        Raw::PrimField { loc, value, .. } => {
            let Some(target) = store.layer_mut(id).and_then(|l| spec_at_mut(l, loc)) else {
                return Err(diverged_at(store, loc));
            };
            let old = prim_fields::swap(target, value.clone()).map_err(Rejection::NotOnVariant)?;
            Ok(Raw::PrimField {
                layer: id,
                loc: loc.clone(),
                value: old,
            })
        }
    }
}

/// Sets or removes the sample at `time` in `samples`, returning the old
/// sample value and the `keep_empty` that undoes it; `None` when removing
/// a sample that is not there.
fn set_sample(
    samples: &mut crate::TimeSamples,
    time: f64,
    value: Option<Value>,
    keep_empty: bool,
) -> Option<(Option<Value>, bool)> {
    let had_samples = samples.is_authored();
    match value {
        Some(value) => {
            let list = samples.make_mut();
            match list.binary_search_by(|(t, _)| t.total_cmp(&time)) {
                Ok(index) => Some((Some(core::mem::replace(&mut list[index].1, value)), true)),
                Err(index) => {
                    list.insert(index, (time, value));
                    Some((None, had_samples))
                }
            }
        }
        None => {
            let index = samples
                .get()?
                .binary_search_by(|(t, _)| t.total_cmp(&time))
                .ok()?;
            let list = samples.get_mut()?;
            let (_, old) = list.remove(index);
            if list.is_empty() && !keep_empty {
                *samples = crate::TimeSamples::none();
            }
            Some((Some(old), false))
        }
    }
}

impl Loc {
    /// The spec path of this location.
    pub(crate) fn spec_path(&self, paths: &PathInterner) -> SpecPath {
        SpecPath::from_variant_selection_sites(self.prim_path(), self.sites(), paths)
    }
}

// Layer fields are distinct from the pseudo-root prim's metadata.
fn authored_layer_metadata(
    store: &dyn LayerStore,
    layer: LayerId,
    key: TokenId,
) -> Option<Option<FieldValue>> {
    let layer = store.layer(layer)?;
    Some(if store.tokens().resolve(key) == "defaultPrim" {
        layer
            .default_prim
            .map(|v| FieldValue::Value(Value::Token(v)))
    } else {
        layer.metadata(key).cloned()
    })
}
