// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Authoring schema properties and applied schemas as a [`Transaction`].

use core::fmt;

use alloc::{string::String, vec::Vec};

use layerstack::edit::{EditTarget, Transaction};
use layerstack::{
    CannotApply, LayerStore, ListOp, PathId, PropertyDefinition, PropertyKind, PropertyPath,
    PropertySpec, PropertyType, ResolvedValue, Specifier, Stage, TargetPath, TokenId,
    TokenInterner, Value,
};

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
///     schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
///     ..StageOptions::default()
/// };
/// let mut live = LiveStage::compose(&mut store, LayerId(1), options);
/// let path = store.path("/Key");
///
/// let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
/// let light = SphereLight::define(&mut edit, path);
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
    /// Applied schema names this edit adds, by prim.
    applied: Vec<(PathId, TokenId)>,
    /// Properties this edit creates, with an attribute's type name.
    created: Vec<(PathId, TokenId, Option<PropertyType>)>,
    /// The defaults this edit sets, latest last.
    defaults: Vec<(PathId, TokenId, Value)>,
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
            applied: Vec::new(),
            created: Vec::new(),
            defaults: Vec::new(),
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

    /// The scene interner used to encode values collected by this edit.
    pub fn tokens(&mut self) -> &mut TokenInterner {
        self.store.tokens_mut()
    }

    fn property(&mut self, path: PathId, name: &str) -> layerstack::edit::Address {
        let name = self.store.tokens_mut().intern(name);
        self.target.property(PropertyPath::new(path, name))
    }

    /// Whether a prim is at `path`: on the stage, or defined earlier in
    /// this edit. Edit handles and `apply` author only such prims, so no
    /// edit manufactures an `over` for a prim that does not exist.
    ///
    /// OpenUSD: authoring through an invalid `UsdPrim` fails.
    pub(crate) fn exists(&self, path: PathId) -> bool {
        self.stage.has_prim(path) || self.defined.iter().any(|(p, _)| *p == path)
    }

    /// The prim's type as this edit leaves it.
    fn type_name(&mut self, path: PathId) -> Option<TokenId> {
        match self.defined.iter().rev().find(|(p, _)| *p == path) {
            Some((_, defined)) => {
                let defined = defined.clone();
                Some(self.store.tokens_mut().intern(defined))
            }
            None => self.stage.resolve_type_name(path, &*self.store),
        }
    }

    /// The definition the prim's schemas, as this edit leaves them, give
    /// its property `name`.
    ///
    /// Spec: AOUSD Core §13.3.2.3 (the prim definition).
    fn definition(&mut self, path: PathId, name: TokenId) -> Option<PropertyDefinition> {
        let registry = self.stage.schemas()?;
        let type_name = self.type_name(path);
        let mut applied: Vec<TokenId> = self
            .store
            .tokens()
            .lookup("apiSchemas")
            .and_then(|field| self.stage.resolve_token_list(path, field))
            .map(|resolved| resolved.value)
            .unwrap_or_default();
        applied.extend(
            self.applied
                .iter()
                .filter(|(p, _)| *p == path)
                .map(|(_, name)| *name),
        );
        registry.property_definition(type_name, &applied, name, self.store.tokens())
    }

    /// Authors `value` for the attribute `name` of `path`, at `time` or as
    /// its default. An attribute no layer authors yet is created as the
    /// prim's schemas define it.
    ///
    /// OpenUSD: `UsdAttribute::Set`, and `UsdPrim::CreateAttribute` with
    /// the definition's type and variability for a schema attribute.
    pub(crate) fn set_value(&mut self, path: PathId, name: &str, time: Option<f64>, value: Value) {
        let token = self.store.tokens_mut().intern(name);
        let at = self.target.property(PropertyPath::new(path, token));
        let authored = self
            .stage
            .explain_property_path(PropertyPath::new(path, token))
            .is_some()
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
                        let time = self.target.map_to_spec_time(time);
                        spec.time_samples = Some(alloc::vec![(time, value)].into());
                    }
                }
                if time.is_none() {
                    self.defaults.push((path, token, value_of(&spec)));
                }
                self.transaction.create_property(at, spec);
                self.created.push((path, token, Some(ty)));
            }
            None => {
                match time {
                    None => {
                        self.defaults.push((path, token, value.clone()));
                        self.transaction.set_default(at, value)
                    }
                    Some(time) => self.transaction.set_time_sample(at, time, value),
                };
            }
        }
    }

    /// The default of the attribute `name` of `path` as this edit leaves
    /// it: the last default this edit sets, else the stage's (fallback
    /// included).
    pub(crate) fn default_value(&mut self, path: PathId, name: &str) -> Option<Value> {
        let token = self.store.tokens_mut().intern(name);
        if let Some((_, _, value)) = self
            .defaults
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return Some(value.clone());
        }
        match self
            .stage
            .resolve_value_with_schema(path, token, &*self.store)?
            .value
        {
            ResolvedValue::Scalar(value) => Some(value),
            _ => None,
        }
    }

    /// The declared type of the attribute `name` of `path` as this edit
    /// leaves it, if the prim has such an attribute: one this edit creates,
    /// else the stage's (an opinion's declaration, else the schema's).
    pub(crate) fn attribute_type(&mut self, path: PathId, name: &str) -> Option<PropertyType> {
        let token = self.store.tokens_mut().intern(name);
        if let Some((_, _, ty)) = self
            .created
            .iter()
            .rev()
            .find(|(p, n, _)| (*p, *n) == (path, token))
        {
            return ty.clone();
        }
        if let Some(declared) = self.stage.resolve_property_declaration(path, token) {
            return (declared.kind == PropertyKind::Attribute)
                .then_some(declared.type_name)
                .flatten();
        }
        self.definition(path, token)
            .filter(|d| d.kind == PropertyKind::Attribute)
            .and_then(|d| d.type_name)
    }

    #[cfg(any(feature = "usd-shade", feature = "usd-geom"))]
    pub(crate) fn property_kind(&mut self, path: PathId, name: &str) -> Option<PropertyKind> {
        let token = self.store.tokens_mut().intern(name);
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
        self.stage
            .resolve_property_declaration(path, token)
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

    /// Creates the attribute `name` of `path`, not `custom`, of type `ty`,
    /// with no value.
    ///
    /// OpenUSD: `UsdPrim::CreateAttribute(name, typeName, custom = false)`.
    pub(crate) fn create_attribute(&mut self, path: PathId, name: &str, ty: PropertyType) {
        let token = self.store.tokens_mut().intern(name);
        let at = self.target.property(PropertyPath::new(path, token));
        let mut spec = PropertySpec::typed_attribute(ty.clone());
        spec.custom = false;
        self.transaction.create_property(at, spec);
        self.created.push((path, token, Some(ty)));
    }

    #[cfg(feature = "usd-geom")]
    pub(crate) fn set_property_metadata(
        &mut self,
        path: PathId,
        name: &str,
        key: &str,
        value: Value,
    ) {
        let token = self.tokens().intern(name);
        let key = self.tokens().intern(key);
        let at = self.target.property(PropertyPath::new(path, token));
        self.transaction.set_metadata(at, key, value.into());
    }

    /// Authors `targets` as the explicit targets of the relationship `name`.
    pub(crate) fn set_targets(&mut self, path: PathId, name: &str, targets: &[TargetPath]) {
        let at = self.property(path, name);
        self.transaction
            .set_targets(at, ListOp::explicit(targets.to_vec()));
    }

    /// Defines `path` as a new `def` prim spec of the typed schema `schema`.
    ///
    /// OpenUSD: `UsdStage::DefinePrim`, for a prim the edit target does not
    /// author yet.
    pub(crate) fn define(&mut self, path: PathId, schema: &str) {
        let type_name = self.store.tokens_mut().intern(schema);
        let at = self.target.prim(path);
        self.transaction
            .create_prim(at, Specifier::Def, Some(type_name));
        self.defined.push((path, schema.into()));
    }

    /// Applies the applied schema `schema` (with `instance` for a
    /// multiple-apply schema) to `path`, if a prim is there (see
    /// [`Self::exists`]) and the schema may be applied to it given its
    /// type: the type this edit defines it with, else its composed type.
    ///
    /// OpenUSD: `UsdPrim::CanApplyAPI`, then `UsdPrim::ApplyAPI`, both of
    /// which fail for an invalid prim.
    pub(crate) fn apply(
        &mut self,
        path: PathId,
        schema: &str,
        instance: Option<&str>,
    ) -> Result<(), CannotApply> {
        if !self.exists(path) {
            return Err(CannotApply::NoSuchPrim);
        }
        let registry = self
            .stage
            .schemas()
            .ok_or(CannotApply::NotAnAppliedSchema)?;
        let tokens = self.store.tokens_mut();
        let schema_token = tokens.intern(schema);
        let type_name = match self.defined.iter().rev().find(|(p, _)| *p == path) {
            Some((_, defined)) => Some(tokens.intern(defined)),
            None => self.stage.resolve_type_name(path, &*self.store),
        };
        let tokens = self.store.tokens_mut();
        registry.can_apply(type_name, schema_token, instance, tokens)?;
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
