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

use alloc::{boxed::Box, vec::Vec};

use super::{
    error::{EditError, Rejection, Slot},
    same::Same,
    spec::{Loc, SpecMut, SpecRef, conforms, spec_at, spec_at_mut},
    target::Address,
    transaction::{Op, Precondition, Transaction, authored},
};
use crate::{
    doc::{
        FieldEntry, FieldValue, Layer, LayerId, LayerStore, PrimSpec, Specifier, Value, VariantSpec,
    },
    interner::TokenId,
    path::{PathId, PathInterner},
    property::{
        PropertyEntry, PropertyKind, PropertySpec, PropertyType, Variability, get_property,
    },
    spec_path::{SpecPath, VariantSelectionSite},
    stage::Stage,
};

/// One storage step. Applying it returns the step that undoes it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Raw {
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
    /// `set` on the prim spec at `host`. Storing creates the set, and lists
    /// it in the host's variant set order, if needed; removing drops the
    /// set when `drop_set` and it is left empty, and the set's order entry
    /// when `drop_order`.
    Variant {
        layer: LayerId,
        host: Loc,
        set: TokenId,
        variant: TokenId,
        spec: Option<VariantSpec>,
        drop_set: bool,
        drop_order: bool,
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
    /// Sets or clears the selection for `set` authored at `loc`.
    Selection {
        layer: LayerId,
        loc: Loc,
        set: TokenId,
        variant: Option<TokenId>,
    },
}

impl Raw {
    pub(crate) fn layer(&self) -> LayerId {
        match self {
            Self::PrimSlots { layer, .. }
            | Self::Child { layer, .. }
            | Self::Variant { layer, .. }
            | Self::Property { layer, .. }
            | Self::Default { layer, .. }
            | Self::Sample { layer, .. }
            | Self::Field { layer, .. }
            | Self::Selection { layer, .. } => *layer,
        }
    }

    /// The namespace path of the prim whose opinions a value step changes;
    /// `None` for a step that changes namespace or composition arcs.
    fn opinion_site(&self) -> Option<PathId> {
        match self {
            Self::PrimSlots { .. }
            | Self::Child { .. }
            | Self::Variant { .. }
            | Self::Selection { .. } => None,
            Self::Property { loc, .. }
            | Self::Default { loc, .. }
            | Self::Sample { loc, .. }
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
            _ => false,
        }
    }

    /// The spec path and slot this step writes, for errors.
    fn slot(&self, paths: &PathInterner) -> (SpecPath, Slot) {
        match self {
            Self::PrimSlots { path, .. } => (SpecPath::from_prim_path(*path, paths), Slot::Spec),
            Self::Child { parent: loc, .. } | Self::Variant { host: loc, .. } => {
                (loc.spec_path(paths), Slot::Spec)
            }
            Self::Property { loc, name, .. } => {
                (loc.spec_path(paths).with_property(*name), Slot::Spec)
            }
            Self::Default { loc, name, .. } => {
                (loc.spec_path(paths).with_property(*name), Slot::Default)
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
            Self::Selection { loc, set, .. } => {
                (loc.spec_path(paths), Slot::VariantSelection(*set))
            }
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
    pub(crate) inverse: Transaction,
    /// Every layer the transaction changed.
    pub(crate) layers: Vec<LayerId>,
    /// `(layer, prim path)` of every spec whose opinions changed.
    pub(crate) touched: Vec<(LayerId, PathId)>,
    /// Whether specs were created or removed or variant selections
    /// changed, which changes namespace or composition arcs.
    pub(crate) structural: bool,
}

/// The inverse steps recorded so far and their effects.
#[derive(Default)]
struct Journal {
    steps: Vec<Guarded>,
    layers: Vec<LayerId>,
    /// The layers whose edits may change namespace or arcs.
    structural_layers: Vec<LayerId>,
    touched: Vec<(LayerId, PathId)>,
}

impl Journal {
    fn run(&mut self, store: &mut dyn LayerStore, step: Raw) -> Result<(), Rejection> {
        self.run_step(store, step).map(|_| ())
    }

    /// Applies `step`, records its inverse, and returns the inverse.
    fn run_step(&mut self, store: &mut dyn LayerStore, step: Raw) -> Result<&Raw, Rejection> {
        let inverse = apply_raw(store, &step)?;
        let layer = step.layer();
        if !self.layers.contains(&layer) {
            self.layers.push(layer);
        }
        match step.opinion_site() {
            Some(prim) if !self.touched.contains(&(layer, prim)) => {
                self.touched.push((layer, prim));
            }
            Some(_) => {}
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
) -> Result<Outcome, EditError> {
    check_preconditions(store, txn)?;
    let mut journal = Journal::default();
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
    for id in &journal.layers {
        let structural = journal.structural_layers.contains(id);
        if let Some(layer) = store.layer_mut(*id) {
            if structural {
                layer.touch_structure();
            } else {
                layer.touch_values();
            }
        }
    }
    let Journal {
        mut steps,
        layers,
        structural_layers,
        touched,
    } = journal;
    steps.reverse();
    Ok(Outcome {
        inverse: Transaction {
            preconditions: Vec::new(),
            ops: steps.into_iter().map(|g| Op::Raw(Box::new(g))).collect(),
        },
        layers,
        touched,
        structural: !structural_layers.is_empty(),
    })
}

fn check_preconditions(store: &mut dyn LayerStore, txn: &Transaction) -> Result<(), EditError> {
    for (index, precondition) in txn.preconditions.iter().enumerate() {
        match precondition {
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
                        .resolve(store.paths_mut())
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

/// The spec path and storage location `at` names.
fn resolve(store: &mut dyn LayerStore, at: &Address) -> Result<(SpecPath, Loc), Rejection> {
    if store.layer(at.layer()).is_none() {
        return Err(Rejection::NoSuchLayer(at.layer()));
    }
    let path = at.resolve(store.paths_mut()).ok_or(Rejection::Unmappable)?;
    let loc = Loc::of(&path, store.paths_mut()).ok_or(Rejection::UnsupportedPath(path.clone()))?;
    Ok((path, loc))
}

fn layer(store: &dyn LayerStore, id: LayerId) -> Result<&Layer, Rejection> {
    store.layer(id).ok_or(Rejection::NoSuchLayer(id))
}

fn apply_op(
    store: &mut dyn LayerStore,
    op: &Op,
    stage: Option<&Stage>,
    journal: &mut Journal,
) -> Result<(), Rejection> {
    match op {
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
            let (parent, name) = loc
                .parent(store.paths_mut())
                .ok_or(Rejection::NotAPrim(path))?;
            ensure(store, id, &parent, journal)?;
            let spec = PrimSpec {
                specifier: Some(*specifier),
                type_name: *type_name,
                outer_variant_sites: loc.sites().to_vec(),
                ..PrimSpec::default()
            };
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
                let values = spec
                    .default
                    .iter()
                    .chain(spec.time_samples.iter().flatten().map(|(_, value)| value));
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
        Op::SetDefault { at, value } => set_value(store, at, None, value, stage, journal),
        Op::SetTimeSample { at, time, value } => {
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
                .and_then(|attr| attr.time_samples.as_deref())
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
fn field_index(fields: &[FieldEntry], key: TokenId) -> usize {
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

fn check_type(path: &SpecPath, ty: &PropertyType, value: &Value) -> Result<(), Rejection> {
    if conforms(ty, value) {
        Ok(())
    } else {
        Err(Rejection::TypeMismatch {
            path: path.clone(),
            declared: ty.type_name.clone(),
        })
    }
}

/// Sets the default (`time` is `None`) or the time sample at the layer
/// time `time` of the attribute `at`, creating its spec when missing.
fn set_value(
    store: &mut dyn LayerStore,
    at: &Address,
    time: Option<f64>,
    value: &Value,
    stage: Option<&Stage>,
    journal: &mut Journal,
) -> Result<(), Rejection> {
    let (path, loc, name) = property_address(store, at)?;
    let id = at.layer();
    let declared = stage
        .zip(at.stage_path())
        .and_then(|(stage, (prim, _))| stage.resolve_property_declaration(prim, name));
    let existing = attribute_at(store, id, &loc, name, &path)?.cloned();
    let ty = existing
        .as_ref()
        .and_then(|spec| spec.type_name.clone())
        .or_else(|| declared.as_ref().and_then(|d| d.type_name.clone()))
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
    spec.variability = declared.map_or(Variability::Varying, |d| d.variability);
    match time {
        None => spec.default = Some(value.clone()),
        Some(time) => spec.time_samples = Some(alloc::vec![(time, value.clone())]),
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

/// Creates the spec at `loc` if it is missing: a prim spec as an `over`,
/// after its missing ancestors, or a variant spec, after its host.
///
/// OpenUSD: `SdfCreatePrimInLayer`.
fn ensure(
    store: &mut dyn LayerStore,
    id: LayerId,
    loc: &Loc,
    journal: &mut Journal,
) -> Result<(), Rejection> {
    if spec_at(layer(store, id)?, loc).is_some() {
        return Ok(());
    }
    match loc {
        Loc::Prim { path, sites } => {
            let parent = loc.parent(store.paths_mut());
            if let Some((parent, _)) = &parent {
                ensure(store, id, parent, journal)?;
            }
            let spec = PrimSpec {
                specifier: parent.is_some().then_some(Specifier::Over),
                outer_variant_sites: sites.clone(),
                ..PrimSpec::default()
            };
            insert_prim_spec(store, id, *path, spec, journal)?;
            match parent {
                Some((parent, name)) => add_child(store, id, &parent, name, journal),
                None => Ok(()),
            }
        }
        Loc::Variant { .. } => {
            let (host, site) = loc.variant_parts().expect("a variant location");
            ensure(store, id, &host, journal)?;
            let Some(SpecRef::Prim(host_spec)) = spec_at(layer(store, id)?, &host) else {
                return Err(diverged(store, &host));
            };
            let set = host_spec.variant_sets.get(&site.set);
            if set.is_some_and(|set| set.variants.contains_key(&site.variant)) {
                // A branch of that name enclosed by other branches of the
                // host: `/P{a=x}{b=y}` and `/P{a=z}{b=y}`.
                return Err(Rejection::UnsupportedPath(loc.spec_path(store.paths())));
            }
            let (drop_set, drop_order) = (
                set.is_none(),
                !host_spec.variant_set_order.contains(&site.set),
            );
            journal.run(
                store,
                Raw::Variant {
                    layer: id,
                    host: host.clone(),
                    set: site.set,
                    variant: site.variant,
                    spec: Some(VariantSpec {
                        outer_variant_sites: host.sites().to_vec(),
                        ..VariantSpec::default()
                    }),
                    drop_set,
                    drop_order,
                },
            )
        }
    }
}

/// Stores the prim spec `spec` at `path`, keeping [`Layer::prims`] for the
/// spec outside every branch, as [`Layer::insert_prim`] does.
fn insert_prim_spec(
    store: &mut dyn LayerStore,
    id: LayerId,
    path: PathId,
    spec: PrimSpec,
    journal: &mut Journal,
) -> Result<(), Rejection> {
    let found = layer(store, id)?;
    let mut main = found.prims.get(&path).cloned();
    let mut branches = found.variant_prims.get(&path).cloned();
    match main.take() {
        None => main = Some(spec),
        Some(existing) if !spec.outer_variant_sites.is_empty() => {
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
fn add_child(
    store: &mut dyn LayerStore,
    id: LayerId,
    parent: &Loc,
    name: TokenId,
    journal: &mut Journal,
) -> Result<(), Rejection> {
    let spec = spec_at(layer(store, id)?, parent).ok_or_else(|| diverged(store, parent))?;
    let children = spec.children();
    if children.contains(&name) {
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
fn remove_subtree(
    store: &mut dyn LayerStore,
    id: LayerId,
    loc: &Loc,
    journal: &mut Journal,
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
            return own && spec.outer_variant_sites == sites;
        }
        let inner = spec.outer_variant_sites.strip_prefix(sites);
        inner.is_some_and(|inner| {
            inner.iter().all(|site: &VariantSelectionSite| {
                paths.resolve(site.host_path).depth() >= root_depth
            })
        })
    };
    let found = layer(store, id)?;
    let mut candidates: Vec<PathId> = found
        .prims
        .keys()
        .chain(found.variant_prims.keys())
        .copied()
        .collect();
    candidates.sort_unstable();
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
            if let Some((parent, name)) = loc.parent(store.paths_mut())
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
                drop_order: false,
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
            drop_order,
            ..
        } => {
            let Some(SpecMut::Prim(host_spec)) =
                store.layer_mut(id).and_then(|l| spec_at_mut(l, host))
            else {
                return Err(diverged_at(store, host));
            };
            match spec {
                Some(spec) => {
                    let created_set = !host_spec.variant_sets.contains_key(set);
                    let listed = host_spec.variant_set_order.contains(set);
                    let old = host_spec
                        .variant_sets
                        .entry(*set)
                        .or_default()
                        .variants
                        .insert(*variant, spec.clone());
                    if !listed {
                        host_spec.variant_set_order.push(*set);
                    }
                    Ok(Raw::Variant {
                        layer: id,
                        host: host.clone(),
                        set: *set,
                        variant: *variant,
                        spec: old,
                        drop_set: created_set,
                        drop_order: !listed,
                    })
                }
                None => {
                    let Some(set_spec) = host_spec.variant_sets.get_mut(set) else {
                        return Err(diverged_at(store, host));
                    };
                    let Some(old) = set_spec.variants.remove(variant) else {
                        return Err(diverged_at(store, host));
                    };
                    if *drop_set && set_spec.variants.is_empty() {
                        host_spec.variant_sets.remove(set);
                    }
                    if *drop_order && host_spec.variant_set_order.last() == Some(set) {
                        host_spec.variant_set_order.pop();
                    }
                    Ok(Raw::Variant {
                        layer: id,
                        host: host.clone(),
                        set: *set,
                        variant: *variant,
                        spec: Some(old),
                        drop_set: false,
                        drop_order: false,
                    })
                }
            }
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
                        spec.clone(),
                    )),
                    position,
                ),
                (Some(spec), None) => {
                    let index = (*index).min(properties.len());
                    properties.insert(
                        index,
                        PropertyEntry {
                            name: *name,
                            spec: spec.clone(),
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
                spec: old,
                index: old_index,
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
                        &mut properties[index].spec.default,
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
                        &mut properties[index].spec.time_samples,
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
                            &mut properties[at].spec.metadata
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
                None => selections.remove(set),
            };
            Ok(Raw::Selection {
                layer: id,
                loc: loc.clone(),
                set: *set,
                variant: old,
            })
        }
    }
}

/// Sets or removes the sample at `time` in `samples`, returning the old
/// sample value and the `keep_empty` that undoes it; `None` when removing
/// a sample that is not there.
fn set_sample(
    samples: &mut Option<Vec<(f64, Value)>>,
    time: f64,
    value: Option<Value>,
    keep_empty: bool,
) -> Option<(Option<Value>, bool)> {
    let had_samples = samples.is_some();
    match value {
        Some(value) => {
            let list = samples.get_or_insert_with(Vec::new);
            match list.binary_search_by(|(t, _)| t.total_cmp(&time)) {
                Ok(index) => Some((Some(core::mem::replace(&mut list[index].1, value)), true)),
                Err(index) => {
                    list.insert(index, (time, value));
                    Some((None, had_samples))
                }
            }
        }
        None => {
            let list = samples.as_mut()?;
            let index = list.binary_search_by(|(t, _)| t.total_cmp(&time)).ok()?;
            let (_, old) = list.remove(index);
            if list.is_empty() && !keep_empty {
                *samples = None;
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
