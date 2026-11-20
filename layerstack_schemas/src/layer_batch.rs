// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema-driven authoring of a fresh layer.

use alloc::string::String;
use core::fmt;
use layerstack::{
    CannotApply, Layer, LayerId, LayerPrims, LayerStore, PathId, PathInterner, PrimSpec,
    PropertyKind, PropertyType, SchemaKind, SchemaRegistry, TargetPath, TokenInterner,
};

mod prim;
pub use prim::{BatchApi, BatchPrim};

#[cfg(all(test, feature = "usd-geom"))]
mod tests;

/// Builds a new layer from schema-declared defaults, relationships and API applications.
///
/// Each [`define`](Self::define) writes one complete prim. The registry supplies
/// property names, types and variability. Only the opinions supplied by the caller
/// are authored. A caller can drop the batch to discard its records, or take its
/// layer with [`finish`](Self::finish) and insert it into the store. Composition,
/// application-specific validation and saving can then run on the finished layer.
/// Use [`crate::SchemaEdit`] for mapped edits to a composed stage with undo.
///
/// ```
/// # #[cfg(feature = "usd-geom")] {
/// use layerstack::{InMemoryStore, LayerId};
/// use layerstack_schemas::{LayerBatch, usd_geom::Cube};
///
/// let mut store = InMemoryStore::default();
/// let schemas = layerstack_schemas::openusd(store.tokens());
/// let mut batch = LayerBatch::new(LayerId(1), &store, &schemas)?;
/// batch.define(store.try_path("/World/Box")?, Cube::SCHEMA, |cube| {
///     cube.set(Cube::SIZE, 3.0)
/// })?;
/// let layer = batch.finish();
/// store.insert_layer(layer);
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct LayerBatch<'a> {
    id: LayerId,
    tokens: &'a TokenInterner,
    paths: &'a PathInterner,
    schemas: &'a SchemaRegistry,
    prims: LayerPrims,
}

impl<'a> LayerBatch<'a> {
    /// Starts an empty layer using the store's path and token interners.
    ///
    /// Returns [`BatchError::ForeignSchemaRegistry`] if the registry belongs to
    /// a different token interner. The caller chooses the destination layer ID.
    pub fn new(
        id: LayerId,
        store: &'a dyn LayerStore,
        schemas: &'a SchemaRegistry,
    ) -> Result<Self, BatchError> {
        let tokens = store.tokens();
        if !schemas.belongs_to(tokens) {
            return Err(BatchError::ForeignSchemaRegistry);
        }
        Ok(Self {
            id,
            tokens,
            paths: store.paths(),
            schemas,
            prims: LayerPrims::default(),
        })
    }

    /// Defines a prim of a registered concrete schema at a non-root path in this store.
    ///
    /// Missing ancestors become untyped definitions. Children can be written before
    /// their parents and keep their authored order. Each path may be explicitly
    /// defined once. Returning an error from `write` discards the prim and leaves
    /// the batch's namespace intact.
    ///
    /// Spec: AOUSD Core §7.3.3 (prim names), §7.6 (prim specs and children).
    pub fn define(
        &mut self,
        path: PathId,
        schema: &str,
        write: impl FnOnce(&mut BatchPrim<'a>) -> Result<(), BatchError>,
    ) -> Result<(), BatchError> {
        let name = self.paths.resolve(path);
        if name.segments().is_empty()
            || !name
                .segments()
                .iter()
                .all(|&part| layerstack::ident::is_identifier(self.tokens.resolve(part)))
        {
            return Err(BatchError::InvalidPath(name.display(self.tokens)));
        }
        if self
            .prims
            .get(&path)
            .is_some_and(|prim| prim.type_name.is_some())
        {
            return Err(BatchError::AlreadyDefined(name.display(self.tokens)));
        }
        let ty = self
            .tokens
            .lookup(schema)
            .filter(|&ty| {
                self.tokens.resolve(ty) == schema
                    && self
                        .schemas
                        .schema(ty)
                        .is_some_and(|s| s.kind == SchemaKind::ConcreteTyped)
            })
            .ok_or_else(|| BatchError::NotConcreteSchema(schema.into()))?;
        let mut prim = BatchPrim::new(ty, self.schemas, self.tokens);
        write(&mut prim)?;
        let mut spec = prim.into_spec();
        if let Some(ancestor) = self.prims.get_mut(&path) {
            spec.authored_children = core::mem::take(&mut ancestor.authored_children);
            *ancestor = spec;
        } else {
            self.prims.insert(path, spec);
            self.link_ancestors(path);
        }
        Ok(())
    }

    // A path joins its parent exactly when it first enters the map. Thousands of
    // siblings can append in order without searching the growing child list.
    fn link_ancestors(&mut self, mut child: PathId) {
        while let Some(parent) = self.paths.parent(child) {
            let name = self
                .paths
                .resolve(child)
                .leaf()
                .expect("a child has a name");
            if let Some(spec) = self.prims.get_mut(&parent) {
                spec.authored_children.push(name);
                break;
            }
            let mut spec = if self.paths.parent(parent).is_some() {
                PrimSpec::def()
            } else {
                PrimSpec::default()
            };
            spec.authored_children.push(name);
            self.prims.insert(parent, spec);
            child = parent;
        }
    }

    /// Takes the authored records as a layer, transferring the prim map in place.
    #[must_use]
    pub fn finish(self) -> Layer {
        let mut layer = Layer::new(self.id);
        if !self.prims.is_empty() {
            *layer.prims_mut() = self.prims;
        }
        layer
    }
}

/// An invalid schema, path, declaration or value in a layer batch.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum BatchError {
    /// The schema registry's interned names belong to a different store.
    ForeignSchemaRegistry,
    /// The path is the pseudo-root or contains an invalid prim name.
    InvalidPath(String),
    /// A previous successful call explicitly defined this path.
    AlreadyDefined(String),
    /// The name does not identify a registered concrete prim type.
    NotConcreteSchema(String),
    /// The schema does not declare the requested property.
    UnknownProperty(String),
    /// The operation requires a different kind of property.
    WrongPropertyKind {
        /// The property requested.
        name: String,
        /// The kind the operation requires.
        expected: PropertyKind,
    },
    /// The attribute lacks a registered USD value type.
    UndeclaredType(String),
    /// The supplied value does not conform to the schema's declared type.
    ValueTypeMismatch {
        /// The property requested.
        name: String,
        /// The schema's declared type, including its role and array shape.
        expected: PropertyType,
    },
    /// An explicit relationship list contains a target more than once.
    DuplicateTarget {
        /// The relationship requested.
        name: String,
        /// The repeated target.
        target: TargetPath,
    },
    /// An API schema could not be applied to this definition.
    CannotApply {
        /// The schema requested.
        schema: String,
        /// Why application failed.
        reason: CannotApply,
    },
}

impl fmt::Display for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ForeignSchemaRegistry => f.write_str("schema registry belongs to another store"),
            Self::InvalidPath(path) => write!(f, "invalid prim path {path}"),
            Self::AlreadyDefined(path) => write!(f, "{path} is already defined"),
            Self::NotConcreteSchema(name) => {
                write!(f, "{name} is not a registered concrete schema")
            }
            Self::UnknownProperty(name) => write!(f, "no schema declaration for {name}"),
            Self::WrongPropertyKind { name, expected } => {
                write!(f, "{name} requires a property of kind {expected:?}")
            }
            Self::UndeclaredType(name) => write!(f, "{name} has no registered attribute type"),
            Self::ValueTypeMismatch { name, expected } => {
                write!(f, "{name} expects {}", expected.type_name())
            }
            Self::DuplicateTarget { name, .. } => {
                write!(f, "{name} has a duplicate relationship target")
            }
            Self::CannotApply { schema, reason } => write!(f, "cannot apply {schema}: {reason}"),
        }
    }
}

impl core::error::Error for BatchError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::CannotApply { reason, .. } => Some(reason),
            _ => None,
        }
    }
}
