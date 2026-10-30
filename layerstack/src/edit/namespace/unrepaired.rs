// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The authored paths a namespace edit leaves naming the edited object:
//! those in the specs no composed prim index uses ([`Dormant`]), which
//! `UsdNamespaceEditor` does not repair either.
//!
//! The report is read from the specs the edit already holds, after its
//! repairs, so it costs one visit of the dormant specs and composes
//! nothing; a delete, which edits only the root layer stack, also reads
//! the arcs into it that the other layers the stage reaches author.
//! AOUSD Core §8 (namespace), §10.5 (variant selection).

use super::{Dormant, PathMove, has_prefix, path_names};
use crate::diagnostic::{Describe, Names, with_store};
use crate::spec_path::VariantSelectionSite;
use crate::{
    FieldEntry, FieldValue, Layer, LayerId, LayerStore, ListOp, Path, PathId, PrimSpec,
    PropertyEntry, PropertyKind, PropertyPath, Reference, ReferenceTarget, SpecPath, TargetPath,
    TokenId, Value, VariantSets, VariantSpec,
};
use alloc::vec::Vec;
use core::fmt;

/// An authored path a [`NamespaceEdit`](crate::NamespaceEdit) leaves
/// naming the moved or deleted object, or something beneath it: one
/// authored in a spec no composed prim uses, such as a variant no prim
/// selects. Once a later edit composes that spec (selecting the variant),
/// the path names the old location, which is gone.
///
/// The edit writes these specs exactly as `UsdNamespaceEditor` does: it
/// moves or removes them with their prim but repairs only the paths of
/// the specs a composed prim index uses (`_DependencyCollector` and
/// `_GatherPathBearingFieldEditsForStage`, from the specs `Usd_Resolver`
/// visits, and `PcpGatherDependentNamespaceEdits` for arcs, in
/// `pxr/usd/usd/namespaceEditor.cpp`). The report says where they are,
/// so a caller can repair or remove them, or warn, before a selection
/// composes them.
///
/// Spec: AOUSD Core §8 (namespace), §10.5 (variant selection).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct UnrepairedPath {
    /// The layer that authors it.
    pub layer: LayerId,
    /// The prim, variant or property spec holding it, where the edit
    /// leaves that spec (`/World/Lamp2{look=off}.light` once `/World/Lamp`
    /// is renamed `Lamp2`): the address of the field in the layer after
    /// the edit applies.
    pub spec: SpecPath,
    /// The field that authors it.
    pub field: PathField,
    /// The absolute path it names: the edited object or a path beneath it,
    /// as authored. For a path expression, the literal prefix of a pattern
    /// (`/World/Lamp` of `/World/Lamp//*`) or the prim path of an
    /// expression reference; relative ones are made absolute at their
    /// spec's prim.
    pub path: TargetPath,
}

impl UnrepairedPath {
    /// The path in words, its layer and paths written as text with
    /// `store`, the store the edit was prepared against: `/World/Lamp in
    /// the references of /World/Host{v=off}Fixture in @./scene.usda@`.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl fmt::Display + 'a {
        with_store(self, store)
    }
}

impl Describe for UnrepairedPath {
    fn describe(&self, f: &mut fmt::Formatter<'_>, names: Names<'_>) -> fmt::Result {
        write!(f, "{} in ", names.target(self.path))?;
        match self.field {
            PathField::Metadata(key) => write!(f, "the metadata field {}", names.token(key))?,
            field => write!(f, "the {}", field.description())?,
        }
        write!(
            f,
            " of {} in {}",
            names.spec(&self.spec),
            names.layer(self.layer)
        )
    }
}

impl fmt::Display for UnrepairedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.describe(f, Names(None))
    }
}

/// The field of a spec that authors an [`UnrepairedPath`].
///
/// Spec: AOUSD Core §7.6.2 (prim spec fields), §7.6.4–§7.6.5 (property
/// spec fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PathField {
    /// A prim or variant spec's `references`.
    References,
    /// A prim or variant spec's `payload`.
    Payloads,
    /// A prim or variant spec's `inheritPaths`.
    Inherits,
    /// A prim or variant spec's `specializes`.
    Specializes,
    /// A relationship's `targetPaths`.
    Targets,
    /// An attribute's `connectionPaths`.
    Connections,
    /// A path expression in an attribute's `default`, or a relationship's
    /// legacy path `default`.
    Default,
    /// A path expression in an attribute's `timeSamples`.
    TimeSamples,
    /// A metadata field of the spec, by name: a path list op, or a path
    /// expression at any depth of its value.
    Metadata(TokenId),
}

impl PathField {
    /// The field as text, as [`UnrepairedPath::display`] writes it.
    fn description(self) -> &'static str {
        match self {
            Self::References => "references",
            Self::Payloads => "payloads",
            Self::Inherits => "inherits",
            Self::Specializes => "specializes",
            Self::Targets => "relationship targets",
            Self::Connections => "attribute connections",
            Self::Default => "default value",
            Self::TimeSamples => "time samples",
            Self::Metadata(_) => "metadata",
        }
    }
}

/// What an edit changed, to tell the authored paths that still name it.
pub(super) enum Edited<'a> {
    /// A move. `local` are the moves of the layer's own layer stack (none
    /// outside every stage's stack); `source` is the root-stack move that
    /// the references and payloads into `root_layers` follow.
    Move {
        local: &'a [PathMove],
        source: &'a PathMove,
        root_layers: &'a [LayerId],
    },
    /// A delete of the root-stack object.
    Delete(TargetPath),
    /// A delete of the root-stack object, seen from a layer outside the
    /// root layer stack, whose arcs into `root_layers` it does not repair.
    DeleteFrom {
        object: TargetPath,
        root_layers: &'a [LayerId],
    },
}

impl Edited<'_> {
    /// The objects the paths of the layer's own namespace may name.
    fn local(&self) -> impl Iterator<Item = TargetPath> + '_ {
        let (moves, object) = match self {
            Self::Move { local, .. } => (*local, None),
            Self::Delete(object) => (&[][..], Some(*object)),
            Self::DeleteFrom { .. } => (&[][..], None),
        };
        moves.iter().map(|m| m.from).chain(object)
    }

    /// Whether `path`, of the layer's own namespace, names an edited object.
    fn names(&self, store: &dyn LayerStore, path: TargetPath) -> bool {
        self.local().any(|from| has_prefix(store, path, from))
    }

    /// The prim path the arc `arc`, authored in `author`, names in an
    /// edited namespace, as `repair_refs` and the delete's repair find it.
    fn names_arc(
        &self,
        store: &dyn LayerStore,
        author: LayerId,
        arc: &Reference,
    ) -> Option<PathId> {
        let ReferenceTarget::Prim(path) = arc.target else {
            return None;
        };
        let target = TargetPath::Prim(path);
        let internal = arc.layer == author && arc.asset.is_none();
        let names = if internal {
            self.names(store, target)
        } else {
            let (from, root_layers) = match self {
                Self::Move {
                    source,
                    root_layers,
                    ..
                } => (source.from, *root_layers),
                Self::DeleteFrom {
                    object,
                    root_layers,
                } => (*object, *root_layers),
                Self::Delete(_) => return None,
            };
            arc.stored_target_layer(store)
                .is_some_and(|layer| root_layers.contains(&layer))
                && has_prefix(store, target, from)
        };
        names.then_some(path)
    }
}

/// Appends to `out` the paths that the specs of `layer` in `dormant`
/// author naming what `edited` changed: the specs the edit's repairs pass
/// over. `layer` is the edited layer, or a working copy of the slots the
/// edit can change, which holds every slot authoring paths; `dormant`
/// names its specs after the edit's moves ([`Dormant::after`]).
pub(super) fn collect(
    store: &dyn LayerStore,
    layer: &Layer,
    dormant: &Dormant,
    edited: &Edited<'_>,
    out: &mut Vec<UnrepairedPath>,
) {
    if dormant.is_empty() {
        return;
    }
    let specs = layer.prims.iter().chain(
        layer
            .variant_prims
            .iter()
            .flat_map(|(path, specs)| specs.iter().map(move |spec| (path, spec))),
    );
    let mut visit = Visit {
        store,
        layer: layer.id,
        edited,
        out,
    };
    for (path, spec) in specs {
        let outer = spec.outer_variant_sites();
        let mut sites = outer.to_vec();
        if dormant.contains(*path, outer) {
            visit.prim(*path, &mut sites, spec);
        } else {
            visit.branches(*path, &mut sites, spec.variant_sets(), Some(dormant));
        }
    }
}

/// Appends to `out` the paths that every spec of `layer` at the prim
/// paths `slots` authors naming what `edited` changed: for a layer the
/// edit does not repair at all.
pub(super) fn collect_all(
    store: &dyn LayerStore,
    layer: &Layer,
    slots: &[PathId],
    edited: &Edited<'_>,
    out: &mut Vec<UnrepairedPath>,
) {
    let mut visit = Visit {
        store,
        layer: layer.id,
        edited,
        out,
    };
    for path in slots {
        let main = layer.prims.get(path).into_iter();
        for spec in main.chain(layer.variant_prims.get(path).into_iter().flatten()) {
            let mut sites = spec.outer_variant_sites().to_vec();
            visit.prim(*path, &mut sites, spec);
        }
    }
}

/// One layer's visit of the specs the edit's repairs pass over.
struct Visit<'a, 'e> {
    store: &'a dyn LayerStore,
    layer: LayerId,
    edited: &'a Edited<'e>,
    out: &'a mut Vec<UnrepairedPath>,
}

impl Visit<'_, '_> {
    /// Every path the dormant prim spec at `path` (inside the branches
    /// `sites`) authors, in its variants too.
    fn prim(&mut self, path: PathId, sites: &mut Vec<VariantSelectionSite>, spec: &PrimSpec) {
        self.owner(
            path,
            sites,
            Owner {
                properties: &spec.properties,
                fields: &spec.fields,
                references: &spec.references,
                payloads: &spec.payloads,
                inherits: spec.inherits(),
                specializes: spec.specializes(),
            },
        );
        self.branches(path, sites, spec.variant_sets(), None);
    }

    /// The variants of `sets`, held by the prim at `path` inside the
    /// branches `sites`: every path each dormant branch authors, or with
    /// no `dormant`, every branch's.
    fn branches(
        &mut self,
        path: PathId,
        sites: &mut Vec<VariantSelectionSite>,
        sets: &VariantSets,
        dormant: Option<&Dormant>,
    ) {
        for (set, spec) in sets {
            for (name, variant) in &spec.variants {
                sites.push(VariantSelectionSite {
                    host_path: path,
                    set: *set,
                    variant: *name,
                });
                match dormant {
                    Some(dormant) if !dormant.contains(path, sites) => {
                        self.branches(path, sites, &variant.variant_sets, Some(dormant));
                    }
                    _ => self.variant(path, sites, variant),
                }
                sites.pop();
            }
        }
    }

    /// Every path the variant spec `variant` authors, nested variants too.
    fn variant(
        &mut self,
        path: PathId,
        sites: &mut Vec<VariantSelectionSite>,
        variant: &VariantSpec,
    ) {
        self.owner(
            path,
            sites,
            Owner {
                properties: &variant.properties,
                fields: &variant.fields,
                references: &variant.references,
                payloads: &variant.payloads,
                inherits: &variant.inherits,
                specializes: &variant.specializes,
            },
        );
        self.branches(path, sites, &variant.variant_sets, None);
    }

    fn owner(&mut self, path: PathId, sites: &[VariantSelectionSite], owner: Owner<'_>) {
        let spec = SpecPath::from_variant_selection_sites(path, sites, self.store.paths());
        for (field, arcs) in [
            (PathField::References, owner.references),
            (PathField::Payloads, owner.payloads),
        ] {
            for arc in arcs.items() {
                if let Some(named) = self.edited.names_arc(self.store, self.layer, arc) {
                    self.push(&spec, field, TargetPath::Prim(named));
                }
            }
        }
        for (field, list) in [
            (PathField::Inherits, owner.inherits),
            (PathField::Specializes, owner.specializes),
        ] {
            for named in list.items() {
                self.path(&spec, field, TargetPath::Prim(*named));
            }
        }
        self.fields(&spec, path, owner.fields);
        for entry in owner.properties {
            self.property(&spec, path, entry);
        }
    }

    fn property(&mut self, owner: &SpecPath, prim: PathId, entry: &PropertyEntry) {
        let spec = owner.with_property(entry.name);
        let property = &entry.spec;
        let field = match property.kind {
            PropertyKind::Relationship => PathField::Targets,
            PropertyKind::Attribute => PathField::Connections,
        };
        for target in property.targets.iter().flat_map(|targets| targets.items()) {
            self.path(&spec, field, *target);
        }
        match &property.default {
            Some(Value::PathText(text)) => self.path_text(&spec, text),
            Some(value) => self.value(&spec, PathField::Default, prim, value),
            None => {}
        }
        for (_, value) in property.time_samples.get().into_iter().flatten() {
            self.value(&spec, PathField::TimeSamples, prim, value);
        }
        self.fields(&spec, prim, &property.metadata);
    }

    fn fields(&mut self, spec: &SpecPath, prim: PathId, fields: &[FieldEntry]) {
        for entry in fields {
            let field = PathField::Metadata(entry.name);
            match &entry.value {
                FieldValue::Value(value) => self.value(spec, field, prim, value),
                FieldValue::PathListOp(paths) => {
                    for path in paths.items() {
                        self.path(spec, field, *path);
                    }
                }
                _ => {}
            }
        }
    }

    /// The path expressions in `value`, at any depth, made absolute at
    /// `prim`.
    fn value(&mut self, spec: &SpecPath, field: PathField, prim: PathId, value: &Value) {
        match value {
            Value::PathExpression(text) => {
                // An invalid expression fails the edit's own repair first.
                let Ok(expression) = crate::PathExpression::parse(text) else {
                    return;
                };
                let anchor = path_names(self.store, prim);
                for (prims, property) in expression.absolute_prefixes(&anchor) {
                    if let Some(named) = self.intern(&prims, property.as_deref()) {
                        self.path(spec, field, named);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    self.value(spec, field, prim, value);
                }
            }
            Value::Dictionary(values) => {
                for (_, value) in values {
                    if let FieldValue::Value(value) = value {
                        self.value(spec, field, prim, value);
                    }
                }
            }
            _ => {}
        }
    }

    /// A relationship's legacy path `default`, an `SdfPath` as text.
    fn path_text(&mut self, spec: &SpecPath, text: &str) {
        let tokens = self.store.tokens();
        let (prim, property) = match text.split_once('.') {
            Some((prim, property)) => (prim, Some(property)),
            None => (text, None),
        };
        let Ok(prim) = Path::parse_absolute(prim, tokens) else {
            return;
        };
        let names: Vec<_> = prim
            .segments()
            .iter()
            .map(|name| alloc::string::String::from(tokens.resolve(*name)))
            .collect();
        if let Some(named) = self.intern(&names, property) {
            self.path(spec, PathField::Default, named);
        }
    }

    /// The path of the prim `prims` (or its property `property`), when it
    /// names an edited object; interns nothing otherwise.
    fn intern(
        &self,
        prims: &[alloc::string::String],
        property: Option<&str>,
    ) -> Option<TargetPath> {
        let (tokens, paths) = (self.store.tokens(), self.store.paths());
        let edited = self.edited.local().any(|from| {
            let source = path_names(self.store, from.prim_path());
            match from.property_path() {
                Some(from) => prims == source && property == Some(tokens.resolve(from.property())),
                None => prims.starts_with(&source),
            }
        });
        if !edited {
            return None;
        }
        let names: Vec<TokenId> = prims.iter().map(|name| tokens.intern(name)).collect();
        let prim = paths.intern(Path::root().join(&names));
        Some(match property {
            Some(name) => TargetPath::Property(PropertyPath::new(prim, tokens.intern(name))),
            None => TargetPath::Prim(prim),
        })
    }

    /// Reports `path` when it names an edited object.
    fn path(&mut self, spec: &SpecPath, field: PathField, path: TargetPath) {
        if self.edited.names(self.store, path) {
            self.push(spec, field, path);
        }
    }

    fn push(&mut self, spec: &SpecPath, field: PathField, path: TargetPath) {
        self.out.push(UnrepairedPath {
            layer: self.layer,
            spec: spec.clone(),
            field,
            path,
        });
    }
}

/// The members of a prim or variant spec that hold paths.
struct Owner<'a> {
    properties: &'a [PropertyEntry],
    fields: &'a [FieldEntry],
    references: &'a ListOp<Reference>,
    payloads: &'a ListOp<Reference>,
    inherits: &'a ListOp<PathId>,
    specializes: &'a ListOp<PathId>,
}

/// Sorts `out` by layer, then spec and path in namespace order, then
/// field, and drops repeats (a path listed in two buckets of a list op).
pub(super) fn sort(store: &dyn LayerStore, out: &mut Vec<UnrepairedPath>) {
    let paths = store.paths();
    let spec_key = |spec: &SpecPath| spec.display(paths, store.tokens());
    let target_key = |path: &TargetPath| path.display(paths, store.tokens());
    crate::sort::sort_by_key(out, |entry| {
        (
            entry.layer,
            spec_key(&entry.spec),
            entry.field,
            target_key(&entry.path),
        )
    });
    out.dedup();
}
