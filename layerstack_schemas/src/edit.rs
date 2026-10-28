// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring schema properties and applied schemas as a [`Transaction`].

use core::fmt;

use alloc::{string::String, vec::Vec};

use layerstack::edit::{EditTarget, Transaction};
use layerstack::{
    CannotApply, LayerStore, ListOp, ListPosition, PathId, PrimPathArg, PropertyDeclaration,
    PropertyDefinition, PropertyKind, PropertyPath, PropertySpec, PropertyType, ResolvedValue,
    Specifier, Stage, TargetPath, Time, TokenId, TokenInterner, Value,
};

#[cfg(test)]
mod tests;

/// Why a [`SchemaEdit`] may not author a prim: no prim is defined, no
/// schema applied and no edit handle had for it. Nothing is authored.
///
/// It is the one checkpoint of instancing for every edit: a handle's
/// setters author only for a prim it was had for, so each [`SchemaEdit`]
/// route that names a prim by path checks it once, when it defines the
/// prim ([`SchemaEdit::define_prim`], `<Schema>::define`), applies a schema
/// ([`SchemaEdit::apply_api`], `<Api>::apply`) or gives a handle
/// ([`crate::PrimEdit::new`], `<Schema>Edit::new`, `UiHintsEdit::prim`,
/// `PortEdit::get`), and [`SchemaEdit::can_edit`] asks it
/// without authoring. A view reads the stage; its prim's handle is had
/// from the edit, by the same path. A handle answers for the edit it was
/// had from: had through one target, it is not checked again for another.
///
/// OpenUSD: `UsdStage::_ValidateEditPrim` and `_ValidateEditPrimAtPath`
/// (`pxr/usd/usd/stage.cpp`) refuse both with a coding error before any
/// prim or property spec is created or cleared, so for `DefinePrim`,
/// `ApplyAPI`, `CreateAttribute`, `UsdAttribute::Set`, `SetMetadata`,
/// `UsdRelationship::SetTargets` and every schema setter built on them,
/// when the edit target is a local layer that maps the prim's path to
/// itself. Through another target, such as a reference's node, the edit is
/// the caller's choice and is allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CannotEdit {
    /// The path is beneath the native instance `instance` (the outermost
    /// one): its descendants are instance proxies, composed from its
    /// prototype, so an opinion authored at their paths would not be on
    /// the stage. The instance itself may be edited.
    ///
    /// Spec: AOUSD Core §11.3.3 (scene-graph instancing).
    BeneathInstance {
        /// The instance.
        instance: PathId,
    },
    /// The path is in an instancing prototype: its root prim's name starts
    /// with `__Prototype_`, as OpenUSD names its prototypes.
    InPrototype,
}

impl fmt::Display for CannotEdit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BeneathInstance { .. } => "the path is beneath an instance",
            Self::InPrototype => "the path is in an instancing prototype",
        })
    }
}

impl core::error::Error for CannotEdit {}

/// Why [`SchemaEdit::apply_api`], or an `<Api>::apply`, applies no schema.
/// Nothing is authored.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplyError {
    /// The edit may not author the prim, beneath an instance or in a
    /// prototype; checked first, as OpenUSD's `ApplyAPI` refuses it.
    CannotEdit(CannotEdit),
    /// The schema cannot be applied to the prim: no prim is there, or the
    /// schema, its instance name or the prim's type does not allow it.
    CannotApply(CannotApply),
}

impl fmt::Display for ApplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CannotEdit(error) => error.fmt(f),
            Self::CannotApply(error) => error.fmt(f),
        }
    }
}

impl core::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::CannotEdit(error) => Some(error),
            Self::CannotApply(error) => Some(error),
        }
    }
}

impl From<CannotEdit> for ApplyError {
    fn from(value: CannotEdit) -> Self {
        Self::CannotEdit(value)
    }
}

impl From<CannotApply> for ApplyError {
    fn from(value: CannotApply) -> Self {
        Self::CannotApply(value)
    }
}

/// Collects schema edits into a [`Transaction`] for one [`EditTarget`].
///
/// Edit handles (`MeshEdit`, `ShapingApiEdit`, …) author through it;
/// nothing changes until the finished transaction is applied, typically
/// with `LiveStage::apply`. An attribute no layer authors yet is created
/// with its schema definition's type and variability, as OpenUSD's
/// `Create*Attr` creates it: the prim's definition as this edit leaves it,
/// with the type it defines the prim with and the schemas it applies.
/// There is no implicit edit target: every edit names this one.
///
/// ```
/// use std::sync::Arc;
///
/// use layerstack::edit::EditTarget;
/// use layerstack::{InMemoryStore, Layer, LayerId, LiveStage, StageOptions};
/// use layerstack_schemas::usd_lux::{ShapingApi, SphereLight};
/// use layerstack_schemas::{Scene, SchemaEdit};
///
/// let mut store = InMemoryStore::default();
/// store.insert_layer(Layer::new(LayerId(1)));
/// let options = StageOptions {
///     schemas: Some(Arc::new(layerstack_schemas::openusd(&store.tokens))),
///     ..StageOptions::default()
/// };
/// let mut live = LiveStage::compose(&mut store, LayerId(1), options);
/// let path = store.path("/Key");
///
/// let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
/// let light = SphereLight::define(&mut edit, path).expect("outside any instance");
/// light.light_api().set_intensity(&mut edit, 500.0);
/// let shaping = ShapingApi::apply(&mut edit, path).expect("any prim may be shaped");
/// shaping.set_shaping_cone_angle(&mut edit, 30.0);
/// let transaction = edit.finish();
/// live.apply(&mut store, &transaction).expect("applies");
///
/// let scene = Scene::new(live.stage(), &store);
/// let light = SphereLight::new(&scene, path).expect("a sphere light");
/// assert_eq!(light.light_api().intensity(), Some(500.0));
/// ```
pub struct SchemaEdit<'s> {
    stage: &'s Stage,
    store: &'s mut dyn LayerStore,
    target: EditTarget,
    transaction: Transaction,
    /// Prims this edit defines, with their type.
    defined: Vec<(PathId, String)>,
    /// Prims this edit gives the `def` specifier, typed or not.
    specified: Vec<PathId>,
    /// Applied schema names this edit adds, by prim.
    applied: Vec<(PathId, TokenId)>,
    /// Properties this edit creates, with an attribute's type name.
    created: Vec<(PathId, TokenId, Option<PropertyType>)>,
    /// The defaults this edit sets, latest last.
    defaults: Vec<(PathId, TokenId, Value)>,
    /// The time samples this edit sets, in stage time, latest last.
    #[cfg(feature = "usd-geom")]
    samples: Vec<TimeSampleEdit>,
    /// Prim metadata this edit authors, including target-local list operations.
    #[cfg(feature = "usd-geom")]
    metadata: Vec<(PathId, TokenId, layerstack::FieldValue)>,
    /// Property metadata this edit authors (`None` clears it), latest last.
    property_metadata: Vec<PropertyMetadataEdit>,
    /// Local property specs removed during this edit.
    removed: Vec<(PathId, TokenId)>,
    #[cfg(feature = "usd-ui")]
    ui_hints: Vec<(TargetPath, Value)>,
    /// The edit target's dictionary-valued metadata fields this edit
    /// authors, as it leaves them (an empty dictionary once cleared).
    dictionaries: Vec<(TargetPath, TokenId, Value)>,
}

/// One time sample an edit authors, in stage time.
#[cfg(feature = "usd-geom")]
#[derive(Clone, Debug)]
struct TimeSampleEdit {
    property: PropertyPath,
    time: f64,
    value: Value,
}

/// One property metadata field an edit authors or clears.
#[derive(Clone, Debug)]
#[cfg_attr(
    not(feature = "usd-shade"),
    expect(dead_code, reason = "read back by the shading edits")
)]
struct PropertyMetadataEdit {
    property: PropertyPath,
    key: TokenId,
    value: Option<layerstack::FieldValue>,
}

impl fmt::Debug for SchemaEdit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchemaEdit")
            .field("target", &self.target)
            .field("transaction", &self.transaction)
            .finish_non_exhaustive()
    }
}

impl<'s> SchemaEdit<'s> {
    /// An edit of `stage`, composed from `store`, authoring through
    /// `target`. `store` interns the names and tokens the edits author.
    pub fn new(stage: &'s Stage, store: &'s mut dyn LayerStore, target: EditTarget) -> Self {
        Self {
            stage,
            store,
            target,
            transaction: Transaction::new(),
            defined: Vec::new(),
            specified: Vec::new(),
            applied: Vec::new(),
            created: Vec::new(),
            defaults: Vec::new(),
            #[cfg(feature = "usd-geom")]
            samples: Vec::new(),
            #[cfg(feature = "usd-geom")]
            metadata: Vec::new(),
            property_metadata: Vec::new(),
            removed: Vec::new(),
            #[cfg(feature = "usd-ui")]
            ui_hints: Vec::new(),
            dictionaries: Vec::new(),
        }
    }

    /// The edit target every edit authors through.
    #[must_use]
    pub fn target(&self) -> &EditTarget {
        &self.target
    }

    /// The edits collected so far.
    #[must_use]
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }

    /// The collected edits, to apply.
    #[must_use]
    pub fn finish(self) -> Transaction {
        self.transaction
    }

    /// Applies a registered API schema by name, including codeless schemas,
    /// and returns the handle authoring the prim.
    ///
    /// Pass `None` for a single-apply schema, or the separate instance name for
    /// a multiple-apply schema (for example `"PipelineSlotAPI"`, `Some("main")`).
    /// A generated Rust type is unnecessary. The stage's registry must already
    /// contain the schema, using this store's token domain; loading a schema
    /// library and its plugin declarations remains the caller's responsibility.
    /// [`layerstack::schema::read_generated_schema`] supports runtime libraries.
    ///
    /// Checks the prim's composed type or a type defined earlier in this edit,
    /// schema kind, application restrictions and instance name. Rejection adds
    /// no operation. Success only collects `apiSchemas` authoring; publish the
    /// finished transaction explicitly. This shares the generated `…Api::apply`
    /// path, so newly applied properties also participate in this edit's schema
    /// overlay. AOUSD Core §13.2.1.2, §13.3.2; OpenUSD `UsdPrim::ApplyAPI`.
    ///
    /// # Errors
    ///
    /// [`ApplyError::CannotEdit`] when [`Self::can_edit`] refuses `path`,
    /// checked first; else [`ApplyError::CannotApply`] when no prim is
    /// there or the schema may not be applied to it.
    #[doc(alias = "ApplyAPI")]
    #[doc(alias = "UsdPrim::ApplyAPI")]
    pub fn apply_api(
        &mut self,
        path: PathId,
        schema: &str,
        instance: Option<&str>,
    ) -> Result<crate::PrimEdit, ApplyError> {
        self.apply(path, schema, instance)?;
        Ok(crate::PrimEdit::from_path_unchecked(path))
    }

    /// Whether this edit may author the prim at `path`: `Ok` unless the
    /// edit target is a layer of the stage's local layer stack that maps
    /// `path` to itself and the path is beneath an instance or in a
    /// prototype. The prim need not exist. Every route that names a prim
    /// by path checks this first (see [`CannotEdit`]); a host can ask it
    /// before offering an edit. It walks `path`'s ancestors once and
    /// authors nothing.
    ///
    /// # Errors
    ///
    /// [`CannotEdit::BeneathInstance`] with the outermost instance above
    /// `path`, or [`CannotEdit::InPrototype`].
    ///
    /// OpenUSD: `UsdStage::_ValidateEditPrim`, `_ValidateEditPrimAtPath`,
    /// `Usd_InstanceCache::IsPathInPrototype`.
    ///
    /// Spec: AOUSD Core §11.3.3 (scene-graph instancing).
    pub fn can_edit(&self, path: PathId) -> Result<(), CannotEdit> {
        let paths = self.store.paths();
        if !self.stage.has_local_layer(self.target.layer())
            || !self.target.maps_to_itself(path, paths)
        {
            return Ok(());
        }
        let root_name = paths.resolve(path).segments().first().copied();
        if root_name.is_some_and(|name| {
            self.store
                .tokens()
                .resolve(name)
                .starts_with("__Prototype_")
        }) {
            return Err(CannotEdit::InPrototype);
        }
        // The last instance found walking up is the outermost.
        let mut instance = None;
        let mut at = paths.parent(path);
        while let Some(ancestor) = at {
            if self
                .stage_prim(ancestor)
                .is_some_and(layerstack::Prim::is_instance)
            {
                instance = Some(ancestor);
            }
            at = paths.parent(ancestor);
        }
        match instance {
            Some(instance) => Err(CannotEdit::BeneathInstance { instance }),
            None => Ok(()),
        }
    }

    /// Defines the prim at `path` as the typed schema named `schema`, and
    /// its undefined ancestors, as OpenUSD's `UsdStage::DefinePrim` does,
    /// and returns the handle authoring it. The schema need not have a
    /// generated Rust type; a concrete schema's `define` (`Mesh::define`) is
    /// this with its name. An empty `schema` defines the prim without giving
    /// it a type.
    ///
    /// Walking from the root prim down to `path`, each ancestor that is
    /// not defined, on the edit's stage or earlier in this edit, gets the
    /// `def` specifier in the edit target, and no type. The prim itself
    /// gets `def` and the type when it is not defined or `schema` is not
    /// its type name (the composed one, or the last this edit gives it);
    /// otherwise nothing is authored. A spec the target lacks is created,
    /// its missing ancestor specs as `over`s, and an existing one, such as
    /// an `over`, is changed in place. So `/A/B` with `/A` undefined
    /// authors a `def "A"` holding a `def Xform "B"`, and a prim already defined
    /// as `schema`, in any layer, authors nothing. The pseudo-root (`/`)
    /// is never authored.
    ///
    /// The prim is authored beneath an inactive or unloaded ancestor too,
    /// as OpenUSD authors it; it stays off the stage until the ancestor is
    /// active and loaded (OpenUSD then reports that it failed to define
    /// it).
    ///
    /// # Errors
    ///
    /// What [`Self::can_edit`] refuses: [`CannotEdit::BeneathInstance`]
    /// for a path beneath an instance, and [`CannotEdit::InPrototype`] for
    /// one in a prototype, when the target is a layer of the stage's local
    /// layer stack that maps `path` to itself, as OpenUSD refuses them with
    /// a coding error; through another target the edit is the caller's
    /// choice. A refusal authors nothing.
    ///
    /// OpenUSD: `UsdStage::DefinePrim`, `_DefinePrim`,
    /// `_IsValidPathForCreatingPrim` (`pxr/usd/usd/stage.cpp`).
    ///
    /// Spec: AOUSD Core §7.6 (specifier field), §11.3.3 (instancing),
    /// §11.5 (defined prims), §12.2.1 (specifier resolution), §13.3.1
    /// (typed schemas).
    #[doc(alias = "DefinePrim")]
    #[doc(alias = "UsdStage::DefinePrim")]
    pub fn define_prim(
        &mut self,
        path: PathId,
        schema: &str,
    ) -> Result<crate::PrimEdit, CannotEdit> {
        self.can_edit(path)?;
        let type_name = (!schema.is_empty()).then(|| self.store.tokens().intern(schema));
        // `path` and its ancestors below the pseudo-root, root prim first.
        let paths = self.store.paths();
        let mut lineage: Vec<PathId> =
            core::iter::successors(Some(path), |at| paths.parent(*at)).collect();
        lineage.pop();
        lineage.reverse();
        if let Some((&prim, ancestors)) = lineage.split_last() {
            for &ancestor in ancestors {
                if !self.is_defined(ancestor) {
                    self.author_def(ancestor, None);
                }
            }
            let retyped = type_name.is_some_and(|ty| self.type_name(prim) != Some(ty));
            if !self.is_defined(prim) || retyped {
                self.author_def(prim, type_name);
                if type_name.is_some() {
                    self.defined.push((prim, schema.into()));
                }
            }
        }
        Ok(crate::PrimEdit::from_path_unchecked(path))
    }

    /// Whether the prim at `path` resolves a defining specifier (`def` or
    /// `class`) on the edit's stage, or this edit gives it `def`. Its
    /// ancestors' specifiers are not considered: defining a prim defines
    /// them first.
    ///
    /// Spec: AOUSD Core §11.5, §12.2.1.
    fn is_defined(&self, path: PathId) -> bool {
        self.specified.contains(&path)
            || self
                .stage_prim(path)
                .is_some_and(|prim| matches!(prim.specifier(), Specifier::Def | Specifier::Class))
    }

    /// Gives the prim at `path` the `def` specifier and, if given, the type
    /// `type_name` in the edit target, creating its spec if needed.
    ///
    /// OpenUSD: `SdfCreatePrimInLayer`, `SdfPrimSpec::SetSpecifier`,
    /// `SetTypeName`.
    fn author_def(&mut self, path: PathId, type_name: Option<TokenId>) {
        let at = self.target.prim(path);
        self.transaction.set_specifier(at.clone(), Specifier::Def);
        if let Some(type_name) = type_name {
            self.transaction.set_type_name(at, type_name);
        }
        self.specified.push(path);
    }

    /// Collects a fallible child group. Success appends its transaction; an error
    /// discards every child edit and restores the parent's schema overlay.
    /// Nested groups follow the same rule. Interned tokens and paths remain in
    /// the store; layer content changes only when the finished batch is applied.
    pub fn group<T, E>(
        &mut self,
        collect: impl FnOnce(&mut SchemaEdit<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut child = SchemaEdit {
            stage: self.stage,
            store: &mut *self.store,
            target: self.target.clone(),
            transaction: Transaction::new(),
            defined: self.defined.clone(),
            specified: self.specified.clone(),
            applied: self.applied.clone(),
            created: self.created.clone(),
            defaults: self.defaults.clone(),
            #[cfg(feature = "usd-geom")]
            samples: self.samples.clone(),
            #[cfg(feature = "usd-geom")]
            metadata: self.metadata.clone(),
            property_metadata: self.property_metadata.clone(),
            removed: self.removed.clone(),
            #[cfg(feature = "usd-ui")]
            ui_hints: self.ui_hints.clone(),
            dictionaries: self.dictionaries.clone(),
        };
        let result = collect(&mut child)?;
        self.transaction.append(child.transaction);
        self.defined = child.defined;
        self.specified = child.specified;
        self.applied = child.applied;
        self.created = child.created;
        self.defaults = child.defaults;
        #[cfg(feature = "usd-geom")]
        {
            self.samples = child.samples;
        }
        #[cfg(feature = "usd-geom")]
        {
            self.metadata = child.metadata;
        }
        self.property_metadata = child.property_metadata;
        self.removed = child.removed;
        #[cfg(feature = "usd-ui")]
        {
            self.ui_hints = child.ui_hints;
        }
        self.dictionaries = child.dictionaries;
        Ok(result)
    }

    /// The scene interner used to encode values collected by this edit.
    pub fn tokens(&mut self) -> &TokenInterner {
        self.store.tokens()
    }

    #[cfg(any(feature = "usd-geom", feature = "usd-shade"))]
    pub(crate) fn scene(&self) -> crate::Scene<'_> {
        crate::Scene::new(self.stage, self.store)
    }

    #[cfg(feature = "usd-geom")]
    pub(crate) fn inspect_authoring(
        &mut self,
        path: PathId,
        name: &str,
        time: Time,
    ) -> Result<layerstack::AuthoringInspection, layerstack::AuthoringInspectionError> {
        let name = self.tokens().intern(name);
        self.stage.inspect_property_authoring(
            &mut *self.store,
            PropertyPath::new(path, name),
            &self.target,
            time.into(),
        )
    }

    /// Reserves a child name against composed prims and earlier definitions in
    /// this transaction. Interning a path alone does not occupy its name.
    #[cfg(feature = "usd-geom")]
    pub(crate) fn unique_child_path(&mut self, parent: PathId, base: &str) -> Option<PathId> {
        let parsed =
            layerstack::Path::parse_absolute(&alloc::format!("/{base}"), self.store.tokens())
                .ok()?;
        if parsed.depth() != 1 {
            return None;
        }
        let parent = self.store.paths().resolve(parent).clone();
        for suffix in 0_u64.. {
            let name = if suffix == 0 {
                String::from(base)
            } else {
                alloc::format!("{base}_{suffix}")
            };
            let token = self.store.tokens().intern(name);
            let path = parent.join(&[token]);
            if let Some(id) = self.store.paths().lookup(&path) {
                if self.exists(id) {
                    continue;
                }
                return Some(id);
            }
            return Some(self.store.paths().intern(path));
        }
        None
    }

    /// The interned path of `parent`'s child `name`.
    #[cfg(feature = "usd-geom")]
    pub(crate) fn child_path(&mut self, parent: PathId, name: TokenId) -> PathId {
        let path = self.store.paths().resolve(parent).join(&[name]);
        self.store.paths().intern(path)
    }

    /// The target site's authored metadata, never a flattened composed list.
    #[cfg(feature = "usd-geom")]
    pub(crate) fn local_prim_metadata(
        &mut self,
        path: PathId,
        name: &str,
    ) -> Option<layerstack::FieldValue> {
        let name = self.tokens().intern(name);
        if let Some((_, _, value)) = self
            .metadata
            .iter()
            .rev()
            .find(|(p, n, _)| *p == path && *n == name)
        {
            return Some(value.clone());
        }
        let mapped = self.target.map_to_spec_path(path, self.store.paths())?;
        let layer = self.store.layer(self.target.layer())?;
        let fields = if !mapped.variant_chain().is_empty() {
            &layer.variant_spec_at(&mapped, self.store.paths())?.fields
        } else {
            &layer
                .prim_specs(mapped.prim_path())
                .find(|spec| {
                    layerstack::SpecPath::from_variant_selection_sites(
                        mapped.prim_path(),
                        spec.outer_variant_sites(),
                        self.store.paths(),
                    ) == mapped
                })?
                .fields
        };
        layerstack::get_field(fields, &name).cloned()
    }

    #[cfg(feature = "usd-geom")]
    pub(crate) fn set_prim_metadata(
        &mut self,
        path: PathId,
        name: &str,
        value: layerstack::FieldValue,
    ) {
        let name = self.tokens().intern(name);
        self.transaction
            .set_metadata(self.target.prim(path), name, value.clone());
        self.metadata.push((path, name, value));
    }

    fn property(&mut self, path: PathId, name: &str) -> layerstack::edit::Address {
        let name = self.store.tokens().intern(name);
        self.target.property(PropertyPath::new(path, name))
    }

    /// Whether a prim is at `path`: on the stage, or defined earlier in
    /// this edit. Edit handles and `apply` author only such prims, so no
    /// edit manufactures an `over` for a prim that does not exist.
    ///
    /// OpenUSD: authoring through an invalid `UsdPrim` fails.
    pub(crate) fn exists(&self, path: PathId) -> bool {
        self.stage_prim(path).is_some() || self.specified.contains(&path)
    }

    /// The parent of the prim at `path`; `None` for the pseudo-root.
    #[cfg(feature = "usd-geom")]
    pub(crate) fn parent(&self, path: PathId) -> Option<PathId> {
        self.store.paths().parent(path)
    }

    /// The children of the prim at `path` as this edit leaves them: the
    /// stage's, inactive ones included, in composed order, then those this
    /// edit defines, in the order it defines them.
    ///
    /// OpenUSD: `UsdPrim::GetAllChildren`.
    #[cfg(feature = "usd-geom")]
    pub(crate) fn all_children(&self, path: PathId) -> Vec<PathId> {
        let mut children: Vec<PathId> = self
            .stage_prim(path)
            .map(|prim| prim.all_children().map(layerstack::Prim::path).collect())
            .unwrap_or_default();
        for &defined in &self.specified {
            if self.store.paths().parent(defined) == Some(path) && !children.contains(&defined) {
                children.push(defined);
            }
        }
        children
    }

    /// The prim at `path` on the edit's stage, as it was before this edit.
    fn stage_prim(&self, path: PathId) -> Option<layerstack::Prim<'_>> {
        self.stage.prim(path, &*self.store)
    }

    /// The property at `path` on the edit's stage, as it was before this
    /// edit.
    fn stage_property(&self, path: PropertyPath) -> Option<layerstack::Property<'_>> {
        self.stage.view(&*self.store).property(path)
    }

    /// The interned prim path `path` names: text (`"/World/Rock"`) or an
    /// id. `None` for text that is not a well-formed absolute prim path, or
    /// that the store has not interned, which no prim of this edit or its
    /// stage has. Looking a path up never interns it.
    #[must_use]
    pub fn prim_path(&self, path: impl PrimPathArg) -> Option<PathId> {
        path.prim_path(self.store.paths(), self.store.tokens())
    }

    /// Whether the prim at `path` is of the typed schema `schema`, or of
    /// one derived from it (`IsA`), as this edit leaves it: the type of its
    /// last [`Self::define_prim`] in this edit, else its type on the stage.
    /// `false` when no prim is there, when it has no type, or when the
    /// stage has no schema registry. A type authored by other means, such
    /// as a raw [`Transaction`] edit, is not seen until it is applied.
    ///
    /// OpenUSD: `UsdPrim::IsA`.
    ///
    /// Spec: AOUSD Core §13.3.1 (typed schemas).
    #[must_use]
    pub fn is_a(&self, path: PathId, schema: &str) -> bool {
        let tokens = self.store.tokens();
        let Some(schema) = tokens.lookup(schema) else {
            return false;
        };
        match self.defined.iter().rev().find(|(p, _)| *p == path) {
            Some((_, defined)) => self
                .stage
                .schemas()
                .zip(tokens.lookup(defined))
                .is_some_and(|(registry, defined)| registry.is_a(defined, schema)),
            None => self
                .stage_prim(path)
                .and_then(layerstack::Prim::definition)
                .is_some_and(|definition| definition.is_a(schema)),
        }
    }

    /// The prim's type as this edit leaves it.
    fn type_name(&mut self, path: PathId) -> Option<TokenId> {
        match self.defined.iter().rev().find(|(p, _)| *p == path) {
            Some((_, defined)) => {
                let defined = defined.clone();
                Some(self.store.tokens().intern(defined))
            }
            None => self
                .stage_prim(path)
                .and_then(layerstack::Prim::type_name_token),
        }
    }

    /// The definition the prim's schemas, as this edit leaves them, give
    /// its property `name`.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (the prim definition).
    fn definition(&mut self, path: PathId, name: TokenId) -> Option<PropertyDefinition> {
        let registry = self.stage.schemas()?;
        let type_name = self.type_name(path);
        let applied = self.applied_names(path);
        registry.property_definition(type_name, &applied, name, self.store.tokens())
    }

    /// The prim's composed `apiSchemas` entries, then those this edit
    /// applies to it.
    ///
    /// Spec: AOUSD Core §13.2.1.2 (`apiSchemas`).
    fn applied_names(&self, path: PathId) -> Vec<TokenId> {
        let mut applied: Vec<TokenId> = match self
            .stage_prim(path)
            .and_then(|prim| prim.metadata("apiSchemas"))
            .map(|resolved| resolved.value)
        {
            Some(ResolvedValue::TokenList(applied)) => applied,
            _ => Vec::new(),
        };
        applied.extend(
            self.applied
                .iter()
                .filter(|(p, _)| *p == path)
                .map(|(_, name)| *name),
        );
        applied
    }

    /// Authors `value` for the attribute `name` of `path`, at `time` or as
    /// its default. An attribute no layer authors yet is created as the
    /// prim's schemas define it.
    ///
    /// OpenUSD: `UsdAttribute::Set`, and `UsdPrim::CreateAttribute` with
    /// the definition's type and variability for a schema attribute.
    pub(crate) fn set_value(&mut self, path: PathId, name: &str, time: Option<f64>, value: Value) {
        let token = self.store.tokens().intern(name);
        let at = self.target.property(PropertyPath::new(path, token));
        let authored = self
            .stage_property(PropertyPath::new(path, token))
            .is_some_and(layerstack::Property::is_authored)
            || self
                .created
                .iter()
                .any(|(p, n, _)| (*p, *n) == (path, token));
        let definition = if authored {
            None
        } else {
            self.definition(path, token)
        };
        match definition.and_then(|d| d.type_name.map(|ty| (ty, d.variability))) {
            Some((ty, variability)) => {
                let mut spec = PropertySpec::typed_attribute(ty.clone());
                spec.variability = variability;
                match time {
                    None => spec.default = Some(value),
                    Some(time) => {
                        #[cfg(feature = "usd-geom")]
                        self.samples.push(TimeSampleEdit {
                            property: PropertyPath::new(path, token),
                            time,
                            value: value.clone(),
                        });
                        let time = self.target.map_to_spec_time(time);
                        spec.time_samples = alloc::vec![(time, value)].into();
                    }
                }
                if time.is_none() {
                    self.defaults.push((path, token, value_of(&spec)));
                }
                self.transaction.create_property(at, spec);
                self.created.push((path, token, Some(ty)));
            }
            None => {
                // Track the local declaration before a value op can implicitly
                // create it, so a later metadata edit never queues a duplicate.
                self.ensure_property(PropertyPath::new(path, token));
                match time {
                    None => {
                        self.defaults.push((path, token, value.clone()));
                        self.transaction.set_default(at, value)
                    }
                    Some(time) => {
                        #[cfg(feature = "usd-geom")]
                        self.samples.push(TimeSampleEdit {
                            property: PropertyPath::new(path, token),
                            time,
                            value: value.clone(),
                        });
                        self.transaction.set_time_sample(at, time, value)
                    }
                };
            }
        }
    }

    /// The default of the attribute `name` of `path` as this edit leaves
    /// it: the last default this edit sets, else the stage's (fallback
    /// included).
    pub(crate) fn default_value(&mut self, path: PathId, name: &str) -> Option<Value> {
        let token = self.store.tokens().intern(name);
        if let Some((_, _, value)) = self
            .defaults
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return Some(value.clone());
        }
        if self.removed.contains(&(path, token)) {
            let property = PropertyPath::new(path, token);
            let mapped = self
                .target
                .map_property_to_spec_path(property, self.store.paths());
            if let Some(value) = self
                .stage_property(property)
                .map(layerstack::Property::opinions)
                .into_iter()
                .flatten()
                .filter(|op| {
                    !(op.layer_id() == self.target.layer()
                        && mapped.as_ref() == Some(&op.spec_path()))
                })
                .filter_map(|op| op.value().as_property()?.default.as_ref())
                .next()
            {
                return Some(value.materialized().clone());
            }
            return self.definition(path, token).and_then(|d| d.fallback);
        }
        let attribute = self
            .stage_property(PropertyPath::new(path, token))?
            .as_attribute()?;
        // A typed read leaves out an array that does not decode; the
        // authored value keeps it, for the caller's decode to report.
        let resolved = attribute
            .get(Time::Default)
            .or_else(|| attribute.authored_value(Time::Default))?;
        match resolved.value {
            Value::Dictionary(_) => None,
            value => Some(value),
        }
    }

    /// Whether a default value is authored for the attribute `name` of
    /// `path` as this edit leaves it: one this edit sets, else one a stage
    /// opinion authors. A block authors none. Time samples are not
    /// consulted, so use it for uniform attributes.
    ///
    /// OpenUSD: `UsdAttribute::HasAuthoredValue`, as
    /// `UsdSchemaBase::_CreateAttr` consults it to write sparsely.
    #[cfg(feature = "usd-shade")]
    pub(crate) fn authors_default(&mut self, path: PathId, name: &str) -> bool {
        let token = self.store.tokens().intern(name);
        if let Some((_, _, value)) = self
            .defaults
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return !matches!(value, Value::Blocked);
        }
        !self.removed.contains(&(path, token))
            && crate::PrimView::new(self.scene(), path).has_authored_value(name)
    }

    /// The value of the attribute `name` of `path` at `time` (its default for
    /// `None`) as this edit leaves it, with held interpolation: the samples
    /// this edit sets join the stage's composed sample times, a sample this
    /// edit sets wins at its own time, and before the first sample the first
    /// one answers. Without samples on either side, the default as this edit
    /// leaves it ([`Self::default_value`]) answers.
    ///
    /// OpenUSD: `UsdAttribute::Get` after earlier `Set` calls on the edit
    /// target; AOUSD Core §12.3.2 (time samples), §12.3.4 (held values).
    #[cfg(feature = "usd-geom")]
    pub(crate) fn value_at(
        &mut self,
        path: PathId,
        name: &str,
        time: Option<f64>,
    ) -> Option<Value> {
        let Some(time) = time else {
            return self.default_value(path, name);
        };
        let token = self.store.tokens().intern(name);
        let property = PropertyPath::new(path, token);
        let attribute = self.stage_property(property).and_then(|p| p.as_attribute());
        let stage_times = attribute.map(|a| a.sample_times()).unwrap_or_default();
        let pending: Vec<&TimeSampleEdit> = self
            .samples
            .iter()
            .filter(|sample| sample.property == property)
            .collect();
        if pending.is_empty() {
            if stage_times.is_empty() {
                return self.default_value(path, name);
            }
            return attribute
                .and_then(|a| a.get(Time::held(time)))
                .map(|r| r.value);
        }
        let mut times: Vec<f64> = stage_times;
        times.extend(pending.iter().map(|sample| sample.time));
        times.sort_by(f64::total_cmp);
        let at = times
            .iter()
            .copied()
            .rfind(|&t| t <= time)
            .unwrap_or(times[0]);
        if let Some(sample) = pending.iter().rev().find(|sample| sample.time == at) {
            return Some(sample.value.clone());
        }
        attribute
            .and_then(|a| a.get(Time::held(at)))
            .map(|r| r.value)
    }

    /// Whether the attribute `name` of `path` has an authored value, default
    /// or time samples, as this edit leaves it.
    ///
    /// OpenUSD: `UsdAttribute::HasAuthoredValue`.
    #[cfg(feature = "usd-geom")]
    pub(crate) fn has_authored_value(&mut self, path: PathId, name: &str) -> bool {
        let token = self.store.tokens().intern(name);
        let property = PropertyPath::new(path, token);
        if let Some((_, _, value)) = self
            .defaults
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return !matches!(value, Value::Blocked)
                || self.samples.iter().any(|s| s.property == property);
        }
        if self.samples.iter().any(|s| s.property == property) {
            return true;
        }
        !self.removed.contains(&(path, token))
            && crate::PrimView::new(self.scene(), path).has_authored_value(name)
    }

    // Resolve declarations after queued removals without hiding other source
    // specs, including weaker opinions of the same property.
    // Spec: AOUSD Core §12.2.2–§12.2.4 (declaration composition).
    fn composed_declaration(&mut self, path: PathId, name: TokenId) -> Option<PropertyDeclaration> {
        if !self.removed.contains(&(path, name)) {
            return self
                .stage_property(PropertyPath::new(path, name))?
                .declaration();
        }
        let property = PropertyPath::new(path, name);
        let mapped = self
            .target
            .map_property_to_spec_path(property, self.store.paths());
        let opinions = self.stage_property(property)?.opinions();
        let mut specs = opinions
            .iter()
            .filter(|op| {
                !(op.layer_id() == self.target.layer() && mapped.as_ref() == Some(&op.spec_path()))
            })
            .filter_map(|op| op.value().as_property());
        let strongest = specs.next()?;
        let mut declaration = PropertyDeclaration {
            kind: strongest.kind,
            type_name: strongest.type_name.clone(),
            variability: strongest.variability,
            custom: strongest.custom,
        };
        for spec in specs {
            if declaration.type_name.is_none() {
                declaration.type_name = spec.type_name.clone();
            }
            declaration.variability = spec.variability;
            declaration.custom |= spec.custom;
        }
        Some(declaration)
    }

    /// The declared type of the attribute `name` of `path` as this edit
    /// leaves it, if the prim has such an attribute: one this edit creates,
    /// else the stage's (an opinion's declaration, else the schema's).
    pub(crate) fn attribute_type(&mut self, path: PathId, name: &str) -> Option<PropertyType> {
        let token = self.store.tokens().intern(name);
        if let Some((_, _, ty)) = self
            .created
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return ty.clone();
        }
        if let Some(declared) = self.composed_declaration(path, token) {
            return (declared.kind == PropertyKind::Attribute)
                .then_some(declared.type_name)
                .flatten();
        }
        self.definition(path, token)
            .filter(|d| d.kind == PropertyKind::Attribute)
            .and_then(|d| d.type_name)
    }

    pub(crate) fn property_kind(&mut self, path: PathId, name: &str) -> Option<PropertyKind> {
        let token = self.store.tokens().intern(name);
        if let Some((_, _, ty)) = self
            .created
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return Some(if ty.is_some() {
                PropertyKind::Attribute
            } else {
                PropertyKind::Relationship
            });
        }
        self.composed_declaration(path, token)
            .map(|d| d.kind)
            .or_else(|| self.definition(path, token).map(|d| d.kind))
    }

    #[cfg(feature = "usd-shade")]
    pub(crate) fn set_connection_op(
        &mut self,
        path: PathId,
        name: &str,
        op: Option<ListOp<TargetPath>>,
    ) {
        let at = self.property(path, name);
        if let Some(op) = op {
            self.transaction.set_targets(at, op);
        } else {
            self.transaction.clear_targets(at);
        }
    }

    /// Adds `target` to the connections of the attribute `name` of `path`
    /// at `position` in the edit target's list op, which keeps the
    /// connections weaker layers author.
    ///
    /// OpenUSD: `UsdAttribute::AddConnection`.
    #[cfg(feature = "usd-shade")]
    pub(crate) fn add_connection(
        &mut self,
        path: PathId,
        name: &str,
        target: TargetPath,
        position: ListPosition,
    ) {
        let at = self.property(path, name);
        self.transaction.add_target(at, target, position);
    }

    /// Removes `target` from the connections of the attribute `name` of
    /// `path`: from the edit target's explicit list, else as a deletion that
    /// removes it from weaker layers' connections too.
    ///
    /// OpenUSD: `UsdAttribute::RemoveConnection`.
    #[cfg(feature = "usd-shade")]
    pub(crate) fn remove_connection(&mut self, path: PathId, name: &str, target: TargetPath) {
        let at = self.property(path, name);
        self.transaction.remove_target(at, target);
    }

    /// Creates the attribute `name` of `path`, not `custom`, of type `ty`,
    /// with no value.
    ///
    /// OpenUSD: `UsdPrim::CreateAttribute(name, typeName, custom = false)`.
    pub(crate) fn create_attribute(&mut self, path: PathId, name: &str, ty: PropertyType) {
        let token = self.store.tokens().intern(name);
        let at = self.target.property(PropertyPath::new(path, token));
        let mut spec = PropertySpec::typed_attribute(ty.clone());
        spec.custom = false;
        self.transaction.create_property(at, spec);
        self.created.push((path, token, Some(ty)));
    }

    #[cfg(feature = "usd-lux")]
    pub(crate) fn stage_up_axis(&self) -> &str {
        let scene = crate::Scene::new(self.stage, self.store);
        scene.metadata().up_axis().unwrap_or("Y")
    }

    #[cfg(any(feature = "usd-lux", feature = "usd-geom", feature = "usd-shade"))]
    pub(crate) fn create_uniform_attribute(&mut self, path: PathId, name: &str, ty: PropertyType) {
        let token = self.store.tokens().intern(name);
        let at = self.target.property(PropertyPath::new(path, token));
        let mut spec = PropertySpec::typed_attribute(ty.clone());
        spec.custom = false;
        spec.variability = layerstack::Variability::Uniform;
        self.transaction.create_property(at, spec);
        self.created.push((path, token, Some(ty)));
    }

    /// Authors the metadata `key` of the property `name` of `path`, with no
    /// check: the caller knows the field and its type.
    pub(crate) fn set_property_metadata(
        &mut self,
        path: PathId,
        name: &str,
        key: &str,
        value: impl Into<layerstack::FieldValue>,
    ) {
        let value = value.into();
        let token = self.tokens().intern(name);
        let property = PropertyPath::new(path, token);
        self.ensure_property(property);
        let key = self.tokens().intern(key);
        let at = self.target.property(property);
        self.transaction.set_metadata(at, key, value.clone());
        self.property_metadata.push(PropertyMetadataEdit {
            property,
            key,
            value: Some(value),
        });
    }

    /// The metadata `key` of the property `name` of `path` as this edit
    /// leaves it: the last value this edit authors or clears, else the
    /// stage's composed scalar value.
    #[cfg(feature = "usd-shade")]
    pub(crate) fn property_metadata_value(
        &mut self,
        path: PathId,
        name: &str,
        key: &str,
    ) -> Option<Value> {
        let property = PropertyPath::new(path, self.tokens().intern(name));
        let key = self.tokens().intern(key);
        if let Some(edit) = self
            .property_metadata
            .iter()
            .rev()
            .find(|edit| edit.property == property && edit.key == key)
        {
            return match &edit.value {
                Some(layerstack::FieldValue::Value(value)) => Some(value.clone()),
                _ => None,
            };
        }
        if self.removed.contains(&(path, property.property())) {
            return None;
        }
        match self.stage_property(property)?.metadata(key)?.value {
            ResolvedValue::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// The relationships of the prim at `path` named `name` or inside its
    /// namespace (`name:…`), as this edit leaves them: the stage's, then
    /// those this edit creates, in that order and without repeats.
    #[cfg(feature = "usd-shade")]
    pub(crate) fn relationships_in(&mut self, path: PathId, name: &str) -> Vec<String> {
        let inside = |candidate: &str| {
            candidate == name
                || candidate
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with(':'))
        };
        let mut names: Vec<String> = self
            .stage_prim(path)
            .map(layerstack::Prim::properties)
            .into_iter()
            .flatten()
            .filter(|property| {
                property
                    .declaration()
                    .map_or_else(|| property.kind(), |declaration| declaration.kind)
                    == PropertyKind::Relationship
            })
            .map(|property| String::from(property.name()))
            .filter(|candidate| inside(candidate))
            .collect();
        let tokens = self.store.tokens();
        for (p, n, ty) in &self.created {
            let created = tokens.resolve(*n);
            if *p == path && ty.is_none() && inside(created) && !names.iter().any(|n| n == created)
            {
                names.push(String::from(created));
            }
        }
        names
    }

    /// Checks and authors (`Some`) or clears (`None`) the metadata field
    /// `key` of the property `name` of `path`. See
    /// [`crate::PrimEdit::write_property_metadata`].
    pub(crate) fn author_property_metadata(
        &mut self,
        path: PathId,
        name: &str,
        key: &str,
        value: Option<layerstack::FieldValue>,
    ) -> Result<(), MetadataError> {
        let kind = self
            .property_kind(path, name)
            .ok_or(MetadataError::NoSuchProperty)?;
        let target = match kind {
            PropertyKind::Attribute => layerstack::MetadataTarget::Attribute,
            PropertyKind::Relationship => layerstack::MetadataTarget::Relationship,
        };
        self.check_metadata(key, target, value.as_ref())?;
        let field = self.tokens().intern(key);
        let property = PropertyPath::new(path, self.tokens().intern(name));
        self.replace_dictionary(TargetPath::Property(property), field, value.as_ref());
        match value {
            Some(value) => self.set_property_metadata(path, name, key, value),
            None => {
                let token = self.tokens().intern(name);
                let property = PropertyPath::new(path, token);
                let key = self.tokens().intern(key);
                // Only a spec this edit or the target layer holds has the
                // field to clear; no spec is created to clear nothing.
                let created = self
                    .created
                    .iter()
                    .any(|(p, n, _)| (*p, *n) == (path, token));
                if created || self.has_local_property(path, token) {
                    let at = self.target.property(property);
                    self.transaction.clear_metadata(at, key);
                }
                self.property_metadata.push(PropertyMetadataEdit {
                    property,
                    key,
                    value: None,
                });
            }
        }
        Ok(())
    }

    /// Checks and authors (`Some`) or clears (`None`) the metadata field
    /// `key` of the prim at `path`. See [`crate::PrimEdit::write_metadata`].
    pub(crate) fn author_prim_metadata(
        &mut self,
        path: PathId,
        key: &str,
        value: Option<layerstack::FieldValue>,
    ) -> Result<(), MetadataError> {
        if !self.exists(path) {
            return Err(MetadataError::NoSuchPrim);
        }
        self.can_edit(path).map_err(MetadataError::CannotEdit)?;
        self.check_metadata(key, layerstack::MetadataTarget::Prim, value.as_ref())?;
        let name = self.tokens().intern(key);
        self.replace_dictionary(TargetPath::Prim(path), name, value.as_ref());
        let at = self.target.prim(path);
        match value {
            Some(value) => {
                self.transaction.set_metadata(at, name, value.clone());
                #[cfg(feature = "usd-geom")]
                self.metadata.push((path, name, value));
            }
            None => {
                self.transaction.clear_metadata(at, name);
            }
        }
        Ok(())
    }

    /// Records that a whole-field edit sets the field `key` of `target` to
    /// `value` (`None` clears it), so a later by-key edit in this edit
    /// starts from it rather than from the layer.
    fn replace_dictionary(
        &mut self,
        target: TargetPath,
        key: TokenId,
        value: Option<&layerstack::FieldValue>,
    ) {
        self.dictionaries
            .retain(|(at, field, _)| (*at, *field) != (target, key));
        let dictionary = match value {
            Some(layerstack::FieldValue::Value(dictionary @ Value::Dictionary(_))) => {
                dictionary.clone()
            }
            Some(_) => return,
            None => Value::Dictionary(Vec::new()),
        };
        self.dictionaries.push((target, key, dictionary));
    }

    /// Sets (`Some`) or erases (`None`) the entry at `key_path` of the
    /// dictionary field `key` of `target` in the edit target, keeping the
    /// other entries the edit target authors; a weaker layer's dictionary
    /// keeps composing under it. Erasing the last entry clears the field.
    ///
    /// OpenUSD: `UsdObject::SetMetadataByDictKey` and
    /// `ClearMetadataByDictKey` (`SdfLayer::SetFieldDictValueByKey`,
    /// `EraseFieldDictValueByKey`). AOUSD Core §7.4, §12.2.5 (dictionary
    /// metadata composes per key).
    pub(crate) fn author_metadata_by_key(
        &mut self,
        target: TargetPath,
        key: &str,
        key_path: &str,
        value: Option<Value>,
    ) -> Result<(), MetadataError> {
        let keys: Vec<&str> = key_path.split(':').collect();
        if keys.iter().any(|part| part.is_empty()) {
            return Err(MetadataError::InvalidKeyPath(key_path.into()));
        }
        let spec_kind = match target {
            TargetPath::Prim(path) => {
                if !self.exists(path) {
                    return Err(MetadataError::NoSuchPrim);
                }
                self.can_edit(path).map_err(MetadataError::CannotEdit)?;
                layerstack::MetadataTarget::Prim
            }
            TargetPath::Property(path) => {
                let name = String::from(self.tokens().resolve(path.property()));
                match self
                    .property_kind(path.prim_path(), &name)
                    .ok_or(MetadataError::NoSuchProperty)?
                {
                    PropertyKind::Attribute => layerstack::MetadataTarget::Attribute,
                    PropertyKind::Relationship => layerstack::MetadataTarget::Relationship,
                }
            }
        };
        let field = self.tokens().intern(key);
        let mut dictionary = match self
            .dictionaries
            .iter()
            .rev()
            .find(|(at, f, _)| (*at, *f) == (target, field))
        {
            Some((_, _, dictionary)) => dictionary.clone(),
            None => self.local_dictionary(target, field),
        };
        let erase = value.is_none();
        if erase && crate::metadata::dictionary_entry(&dictionary, key_path).is_none() {
            // Nothing to erase: C++ authors nothing either.
            return Ok(());
        }
        set_dictionary_entry(&mut dictionary, &keys, value);
        // The field must take a dictionary: `customData`, `assetInfo`, or
        // one a plugin registers.
        self.check_metadata(
            key,
            spec_kind,
            Some(&layerstack::FieldValue::Value(dictionary.clone())),
        )?;
        let empty = matches!(&dictionary, Value::Dictionary(entries) if entries.is_empty());
        let at = match target {
            TargetPath::Prim(path) => self.target.prim(path),
            TargetPath::Property(path) => {
                self.ensure_property(path);
                self.target.property(path)
            }
        };
        if erase && empty {
            self.transaction.clear_metadata(at, field);
        } else {
            self.transaction.set_metadata(
                at,
                field,
                layerstack::FieldValue::Value(dictionary.clone()),
            );
        }
        if let TargetPath::Property(property) = target {
            self.property_metadata.push(PropertyMetadataEdit {
                property,
                key: field,
                value: (!(erase && empty))
                    .then(|| layerstack::FieldValue::Value(dictionary.clone())),
            });
        }
        self.dictionaries
            .retain(|(at, f, _)| (*at, *f) != (target, field));
        self.dictionaries.push((target, field, dictionary));
        Ok(())
    }

    /// The field `key` checked for a spec of `target` with the stage's
    /// schema registry ([`layerstack::MetadataField::check_edit`]).
    ///
    /// Spec: AOUSD Core §7.4 (metadata), §7.6 (the fields of each spec).
    fn check_metadata(
        &self,
        key: &str,
        target: layerstack::MetadataTarget,
        value: Option<&layerstack::FieldValue>,
    ) -> Result<(), MetadataError> {
        layerstack::MetadataField::check_edit(
            key,
            target,
            value,
            self.stage.schemas(),
            self.store.tokens(),
        )
        .map(|_| ())
        .map_err(MetadataError::Field)
    }

    // AOUSD Core §12.2.5: update only the mapped target layer's dictionary;
    // copying a composed dictionary would freeze weaker opinions into this layer.
    #[cfg(feature = "usd-ui")]
    pub(crate) fn update_ui_hint(&mut self, target: TargetPath, keys: &[&str], value: Value) {
        let mut dictionary = self
            .ui_hints
            .iter()
            .rev()
            .find(|(at, _)| *at == target)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| self.local_ui_hints(target));
        crate::ui_hints::set_dictionary_path(&mut dictionary, keys, value);
        if let Some((_, previous)) = self.ui_hints.iter_mut().find(|(at, _)| *at == target) {
            *previous = dictionary.clone();
        } else {
            self.ui_hints.push((target, dictionary.clone()));
        }
        self.set_ui_metadata(target, "uiHints", dictionary);
    }

    #[cfg(feature = "usd-ui")]
    fn local_ui_hints(&mut self, target: TargetPath) -> Value {
        let key = self.tokens().intern("uiHints");
        self.local_dictionary(target, key)
    }

    /// The dictionary the edit target's layer authors for the field `key`
    /// of `target` (an empty one when it authors none or another value),
    /// never a composed one.
    fn local_dictionary(&mut self, target: TargetPath, key: TokenId) -> Value {
        if let TargetPath::Property(path) = target
            && self.removed.contains(&(path.prim_path(), path.property()))
        {
            return Value::Dictionary(Vec::new());
        }
        let mapped = match target {
            TargetPath::Prim(path) => self.target.map_to_spec_path(path, self.store.paths()),
            TargetPath::Property(path) => self
                .target
                .map_property_to_spec_path(path, self.store.paths()),
        };
        let value = mapped.and_then(|mapped| {
            let layer = self.store.layer(self.target.layer())?;
            let fields: &[layerstack::FieldEntry] = if !mapped.variant_chain().is_empty() {
                let variant = layer.variant_spec_at(&mapped, self.store.paths())?;
                match mapped.property() {
                    Some(name) => variant
                        .properties
                        .iter()
                        .find(|p| p.name == name)?
                        .spec
                        .metadata
                        .as_slice(),
                    None => variant.fields.as_slice(),
                }
            } else {
                if self.store.paths().resolve(mapped.prim_path()).depth() == 0 {
                    return None;
                }
                // The branches enclosing the prim, hosted above it.
                let sites: Vec<_> = mapped
                    .variant_selection_sites()
                    .filter(|site| site.host_path != mapped.prim_path())
                    .collect();
                let prim = layer.prim_spec_in(mapped.prim_path(), &sites)?;
                match mapped.property() {
                    Some(name) => prim.property(name)?.metadata.as_slice(),
                    None => prim.fields.as_slice(),
                }
            };
            match layerstack::get_field(fields, &key)? {
                layerstack::FieldValue::Value(value @ Value::Dictionary(_)) => Some(value.clone()),
                _ => None,
            }
        });
        value.unwrap_or_else(|| Value::Dictionary(Vec::new()))
    }

    #[cfg(feature = "usd-ui")]
    pub(crate) fn ui_hint(&mut self, target: TargetPath, keys: &[&str]) -> Option<Value> {
        let key = self.store.tokens().lookup("uiHints")?;
        let resolved = match target {
            TargetPath::Prim(path) => self.stage_prim(path).and_then(|prim| prim.metadata(key)),
            TargetPath::Property(path)
                if self.removed.contains(&(path.prim_path(), path.property())) =>
            {
                let mapped = self
                    .target
                    .map_property_to_spec_path(path, self.store.paths());
                let mut values = self
                    .stage_property(path)
                    .map(layerstack::Property::opinions)
                    .into_iter()
                    .flatten()
                    .filter(|op| {
                        !(op.layer_id() == self.target.layer()
                            && mapped.as_ref() == Some(&op.spec_path()))
                    })
                    .filter_map(|op| op.value().as_property()?.metadata(key))
                    .peekable();
                // Spec: AOUSD Core §12.2.5: dictionaries combine through
                // weaker opinions, stopping at a block. Removed specs never
                // contribute labels to newly created properties.
                let dictionary = if matches!(
                    values.peek(),
                    Some(layerstack::FieldValue::Value(Value::Dictionary(_)))
                ) {
                    layerstack::combine_dictionary_chain(
                        values
                            .map_while(|field| {
                                (!matches!(field, layerstack::FieldValue::Value(Value::Blocked)))
                                    .then_some(field)
                            })
                            .filter_map(|field| match field {
                                layerstack::FieldValue::Value(Value::Dictionary(entries)) => {
                                    Some(entries.as_slice())
                                }
                                _ => None,
                            }),
                    )
                } else {
                    Vec::new()
                };
                let mut dictionary = dictionary;
                if let Some((_, Value::Dictionary(local))) =
                    self.ui_hints.iter().rev().find(|(at, _)| *at == target)
                {
                    dictionary = layerstack::combine_dictionaries(local, &dictionary);
                }
                return crate::ui_hints::entry(&Value::Dictionary(dictionary), keys).cloned();
            }
            TargetPath::Property(path) => self
                .stage_property(path)
                .and_then(|property| property.metadata(key)),
        };
        let mut dictionary = match resolved.map(|r| r.value) {
            Some(ResolvedValue::Dictionary(entries)) => entries,
            Some(ResolvedValue::Scalar(Value::Dictionary(entries))) => entries,
            _ => Vec::new(),
        };
        if let Some((_, Value::Dictionary(local))) =
            self.ui_hints.iter().rev().find(|(at, _)| *at == target)
        {
            dictionary = layerstack::combine_dictionaries(local, &dictionary);
        }
        crate::ui_hints::entry(&Value::Dictionary(dictionary), keys).cloned()
    }

    // A composed property may have no spec in the current target layer.
    // Metadata edits need that local declaration, preserving the composed type.
    fn ensure_property(&mut self, path: PropertyPath) {
        if self
            .created
            .iter()
            .any(|(p, n, _)| *p == path.prim_path() && *n == path.property())
        {
            return;
        }
        let mapped = self
            .target
            .map_property_to_spec_path(path, self.store.paths());
        let local = mapped.as_ref().is_some_and(|mapped| {
            self.store.layer(self.target.layer()).is_some_and(|layer| {
                if !mapped.variant_chain().is_empty() {
                    layer
                        .variant_spec_at(mapped, self.store.paths())
                        .is_some_and(|v| v.properties.iter().any(|p| p.name == path.property()))
                } else {
                    let sites: Vec<_> = mapped.variant_selection_sites().collect();
                    layer
                        .prim_spec_in(mapped.prim_path(), &sites)
                        .is_some_and(|p| p.property(path.property()).is_some())
                }
            })
        });
        if local && !self.removed.contains(&(path.prim_path(), path.property())) {
            return;
        }
        let declared = self.composed_declaration(path.prim_path(), path.property());
        let definition = self.definition(path.prim_path(), path.property());
        let definition = definition.as_ref();
        let kind = declared
            .as_ref()
            .map(|d| d.kind)
            .or_else(|| definition.map(|d| d.kind));
        let ty = declared
            .as_ref()
            .and_then(|d| d.type_name.clone())
            .or_else(|| definition.and_then(|d| d.type_name.clone()));
        let mut spec = match kind {
            Some(PropertyKind::Attribute) => match ty.clone() {
                Some(ty) => PropertySpec::typed_attribute(ty),
                None => return,
            },
            Some(PropertyKind::Relationship) => PropertySpec::relationship(),
            None => return,
        };
        spec.variability = declared
            .as_ref()
            .map(|d| d.variability)
            .or_else(|| definition.map(|d| d.variability))
            .unwrap_or(layerstack::Variability::Varying);
        spec.custom = declared.as_ref().is_some_and(|d| d.custom);
        self.transaction
            .create_property(self.target.property(path), spec);
        self.created.push((path.prim_path(), path.property(), ty));
    }

    #[cfg(feature = "usd-ui")]
    pub(crate) fn set_ui_metadata(&mut self, target: TargetPath, key: &str, value: Value) {
        if let TargetPath::Property(path) = target {
            self.ensure_property(path);
        }
        let key = self.tokens().intern(key);
        let at = match target {
            TargetPath::Prim(path) => self.target.prim(path),
            TargetPath::Property(path) => self.target.property(path),
        };
        self.transaction.set_metadata(at, key, value.into());
    }

    #[cfg(feature = "usd-vol")]
    pub(crate) fn valid_field_target(&self, target: TargetPath) -> bool {
        let path = match target {
            TargetPath::Prim(path) => path,
            TargetPath::Property(path) => path.prim_path(),
        };
        !self.store.paths().resolve(path).segments().is_empty()
    }

    /// Creates a local relationship declaration only when needed. Existing
    /// declarations keep their custom qualifier and metadata, including across
    /// reference and variant edit targets and repeated calls within this edit.
    /// OpenUSD: `UsdPrim::CreateRelationship`; AOUSD Core §12.2.4.
    #[cfg(any(feature = "usd-vol", feature = "usd-lux", feature = "usd-shade"))]
    pub(crate) fn ensure_relationship(&mut self, path: PathId, name: &str, custom: bool) {
        let token = self.store.tokens().intern(name);
        if self
            .created
            .iter()
            .any(|(p, n, _)| (*p, *n) == (path, token))
        {
            return;
        }
        if !self.has_local_property(path, token) {
            let mut spec = PropertySpec::relationship();
            spec.custom = self
                .stage_property(PropertyPath::new(path, token))
                .and_then(layerstack::Property::declaration)
                .map_or(custom, |declaration| declaration.custom);
            let at = self.target.property(PropertyPath::new(path, token));
            self.transaction.create_property(at, spec);
        }
        self.created.push((path, token, None));
    }

    #[cfg(feature = "usd-lux")]
    pub(crate) fn is_pseudo_root(&self, path: PathId) -> bool {
        self.store.paths().resolve(path).depth() == 0
    }

    #[cfg(feature = "usd-lux")]
    pub(crate) fn subtree_targets(&self, root: PathId, targets: &[TargetPath]) -> Vec<TargetPath> {
        let paths = self.store.paths();
        let mut targets: Vec<_> = targets
            .iter()
            .copied()
            .filter(|target| {
                paths
                    .resolve(root)
                    .is_prefix_of(paths.resolve(target.prim_path()))
            })
            .collect();
        targets.sort_by_cached_key(|target| target.display(paths, self.store.tokens()));
        targets.dedup();
        targets
    }

    #[cfg(feature = "usd-geom")]
    pub(crate) fn create_relationship(&mut self, path: PathId, name: &str) {
        let name = self.store.tokens().intern(name);
        let at = self.target.property(PropertyPath::new(path, name));
        self.transaction
            .create_property(at, PropertySpec::relationship());
        self.created.push((path, name, None));
    }

    #[cfg(feature = "usd-geom")]
    pub(crate) fn block_attribute(&mut self, path: PathId, name: &str) {
        let name = self.store.tokens().intern(name);
        self.ensure_property(PropertyPath::new(path, name));
        let at = self.target.property(PropertyPath::new(path, name));
        self.transaction.block_attribute(at);
        self.defaults.push((path, name, Value::Blocked));
        let property = PropertyPath::new(path, name);
        self.samples.retain(|sample| sample.property != property);
    }

    #[cfg(feature = "usd-geom")]
    pub(crate) fn remove_property(&mut self, path: PathId, name: &str) {
        let name = self.store.tokens().intern(name);
        let at = self.target.property(PropertyPath::new(path, name));
        self.transaction.remove_spec(at);
        self.removed.push((path, name));
        self.created.retain(|(p, n, _)| (*p, *n) != (path, name));
        self.defaults.retain(|(p, n, _)| (*p, *n) != (path, name));
        let property = PropertyPath::new(path, name);
        self.samples.retain(|sample| sample.property != property);
        #[cfg(feature = "usd-ui")]
        self.ui_hints
            .retain(|(at, _)| *at != TargetPath::Property(PropertyPath::new(path, name)));
    }

    /// Whether the edit target's layer holds a spec for the property
    /// `name` of the prim at `path`.
    fn has_local_property(&mut self, path: PathId, name: TokenId) -> bool {
        let Some(mapped) = self.target.map_to_spec_path(path, self.store.paths()) else {
            return false;
        };
        let Some(layer) = self.store.layer(self.target.layer()) else {
            return false;
        };
        let properties = if !mapped.variant_chain().is_empty() {
            layer
                .variant_spec_at(&mapped, self.store.paths())
                .map(|spec| &spec.properties)
        } else {
            layer
                .prim_specs(mapped.prim_path())
                .find(|spec| {
                    layerstack::SpecPath::from_variant_selection_sites(
                        mapped.prim_path(),
                        spec.outer_variant_sites(),
                        self.store.paths(),
                    ) == mapped
                })
                .map(|spec| &spec.properties)
        };
        properties.is_some_and(|properties| properties.iter().any(|entry| entry.name == name))
    }

    /// Creates the relationship `name` of `path` at the edit target, not
    /// `custom`, when the prim's schemas as this edit leaves them define it
    /// and neither the layer nor this edit has a spec for it: OpenUSD's
    /// `Create*Rel`, which stamps the schema definition. The stage knows
    /// no definition for a prim this edit defines or applies a schema to,
    /// and the transaction would create such a spec `custom`, as it creates
    /// a relationship nothing declares.
    ///
    /// OpenUSD: `UsdStage::_CreateNewPropertySpecFromSchema`.
    ///
    /// Spec: AOUSD Core §12.2.4 (properties), §13.3.2.3 (the prim
    /// definition).
    fn define_relationship(&mut self, path: PathId, name: TokenId) {
        if self
            .created
            .iter()
            .any(|(p, n, _)| (*p, *n) == (path, name))
            || self
                .definition(path, name)
                .is_none_or(|definition| definition.kind != PropertyKind::Relationship)
            || self.has_local_property(path, name)
        {
            return;
        }
        let at = self.target.property(PropertyPath::new(path, name));
        self.transaction
            .create_property(at, PropertySpec::relationship());
        self.created.push((path, name, None));
    }

    /// Authors `targets` as the explicit targets of the relationship `name`.
    pub(crate) fn set_targets(&mut self, path: PathId, name: &str, targets: &[TargetPath]) {
        let token = self.store.tokens().intern(name);
        self.define_relationship(path, token);
        let at = self.target.property(PropertyPath::new(path, token));
        self.transaction
            .set_targets(at, ListOp::explicit(targets.to_vec()));
    }

    /// Adds `target` to the targets of the relationship `name` at
    /// `position`, editing the edit target's list op.
    pub(crate) fn add_target(
        &mut self,
        path: PathId,
        name: &str,
        target: TargetPath,
        position: ListPosition,
    ) {
        let token = self.store.tokens().intern(name);
        self.define_relationship(path, token);
        let at = self.target.property(PropertyPath::new(path, token));
        self.transaction.add_target(at, target, position);
    }

    /// Removes `target` from the targets of the relationship `name`,
    /// deleting it in the edit target's list op.
    pub(crate) fn remove_target(&mut self, path: PathId, name: &str, target: TargetPath) {
        let token = self.store.tokens().intern(name);
        self.define_relationship(path, token);
        let at = self.target.property(PropertyPath::new(path, token));
        self.transaction.remove_target(at, target);
    }

    /// Applies the applied schema `schema` (with `instance` for a
    /// multiple-apply schema) to `path`, if a prim is there (see
    /// [`Self::exists`]) and the schema may be applied to it given its
    /// type (the type this edit defines it with, else its composed type) and
    /// the applied schemas it has (its composed ones and those this edit
    /// applies).
    ///
    /// OpenUSD: `UsdPrim::CanApplyAPI`, then `UsdPrim::ApplyAPI`, both of
    /// which fail for an invalid prim; `ApplyAPI` refuses an instance proxy
    /// or prototype prim (`AddAppliedSchema`, `_CreatePrimSpecForEditing`).
    pub(crate) fn apply(
        &mut self,
        path: PathId,
        schema: &str,
        instance: Option<&str>,
    ) -> Result<(), ApplyError> {
        self.can_edit(path)?;
        if !self.exists(path) {
            return Err(CannotApply::NoSuchPrim.into());
        }
        let registry = self
            .stage
            .schemas()
            .ok_or(CannotApply::NotAnAppliedSchema)?;
        let type_name = self.type_name(path);
        let names = self.applied_names(path);
        let tokens = self.store.tokens();
        let schema_token = tokens.intern(schema);
        registry.intern_instance_names(&names, tokens);
        let definition = registry.prim_definition(type_name, &names, tokens);
        registry.can_apply(&definition, schema_token, instance, tokens)?;
        let applied = match instance {
            None => tokens.intern(schema),
            Some(instance) => {
                let mut name = String::from(schema);
                name.push(':');
                name.push_str(instance);
                tokens.intern(name)
            }
        };
        let at = self.target.prim(path);
        self.transaction.add_applied_schema(at, applied);
        self.applied.push((path, applied));
        Ok(())
    }
}

/// The default value a created property spec holds.
fn value_of(spec: &PropertySpec) -> Value {
    spec.default.clone().unwrap_or(Value::Null)
}

/// Why a prim or property metadata field cannot be authored. Nothing is
/// collected.
///
/// OpenUSD: the errors `UsdObject::SetMetadata` raises.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MetadataError {
    /// No prim is at the path: none is on the stage or defined earlier in
    /// this edit.
    NoSuchPrim,
    /// The prim has no property of that name: none is authored, created
    /// earlier in this edit, or defined by the prim's schemas.
    NoSuchProperty,
    /// The prim is beneath an instance or in a prototype.
    CannotEdit(CannotEdit),
    /// The field cannot be authored on that kind of spec, or the value is
    /// not of its type: unregistered, registered for other kinds of spec,
    /// a field of the spec with its own edit, or a value of another type
    /// ([`layerstack::MetadataEditError`]). A by-key edit of a field that
    /// does not take a dictionary is refused this way.
    Field(layerstack::MetadataEditError),
    /// A dictionary key path with an empty part (`""`, `"a::b"`, `"a:"`).
    InvalidKeyPath(String),
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchPrim => f.write_str("no such prim"),
            Self::NoSuchProperty => f.write_str("no such property"),
            Self::CannotEdit(error) => error.fmt(f),
            Self::Field(error) => error.fmt(f),
            Self::InvalidKeyPath(path) => write!(f, "invalid dictionary key path {path:?}"),
        }
    }
}

impl core::error::Error for MetadataError {}

/// Sets (`Some`) or erases (`None`) the entry at `keys`, creating the
/// dictionaries on the way to a set and replacing a non-dictionary there;
/// an erase removes the dictionaries it leaves empty, as `VtDictionary::EraseValueAtPath` does.
fn set_dictionary_entry(value: &mut Value, keys: &[&str], replacement: Option<Value>) {
    let Some((key, rest)) = keys.split_first() else {
        return;
    };
    if !matches!(value, Value::Dictionary(_)) {
        if replacement.is_none() {
            return;
        }
        *value = Value::Dictionary(Vec::new());
    }
    let Value::Dictionary(entries) = value else {
        return;
    };
    let position = entries.iter().position(|(name, _)| name.as_ref() == *key);
    if rest.is_empty() {
        match (position, replacement) {
            (Some(i), Some(v)) => entries[i].1 = v,
            (Some(i), None) => {
                entries.remove(i);
            }
            (None, Some(v)) => entries.push((alloc::sync::Arc::from(*key), v)),
            (None, None) => {}
        }
        return;
    }
    let i = match position {
        Some(i) => i,
        None if replacement.is_none() => return,
        None => {
            entries.push((alloc::sync::Arc::from(*key), Value::Dictionary(Vec::new())));
            entries.len() - 1
        }
    };
    let erase = replacement.is_none();
    set_dictionary_entry(&mut entries[i].1, rest, replacement);
    if erase && matches!(&entries[i].1, Value::Dictionary(inner) if inner.is_empty()) {
        entries.remove(i);
    }
}
