// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Deterministic USDA serialization of an explicitly authored document.
//!
//! The composition model ([`layerstack::Layer`]) is built for *reading*: it
//! keeps what composition needs and drops authoring details such as
//! `custom`/`uniform` qualifiers, attribute metadata (e.g. primvar
//! `interpolation`) and most layer metadata. Formatting a composed `Value`
//! therefore cannot produce a faithful file. This module instead serializes
//! a small, owned *authored* representation — [`Document`], [`Prim`],
//! [`Attribute`], [`Relationship`], [`Metadatum`] and [`Value`] — that
//! states every one of those details explicitly.
//!
//! Output is deterministic: the same document always produces byte-identical
//! text. Items are written in the order they appear in the document (nothing
//! is sorted or deduplicated behind the caller's back); invalid input is
//! rejected with a [`WriteError`] rather than silently repaired.
//!
//! Scope: prims with their children and property order (`reorder
//! nameChildren`, `reorder properties`, `reorder rootPrims`); attributes with
//! default values (including a value block, `= None`), time samples
//! (including blocked samples) and connection lists; relationships with
//! target lists; explicit and list-edited (`delete`, `prepend`, `append`)
//! connections and targets; and metadata, including token list operations
//! such as `prepend apiSchemas = [...]` ([`Value::TokenListOp`]); and
//! composition arcs by their authored asset paths: sublayers with layer
//! offsets ([`Document::sublayers`]), and references, payloads, inherits
//! and specializes in any list-op form ([`Prim::references`] and its
//! siblings). Splines and variant sets are not representable.
//!
//! # Example
//!
//! ```
//! use layerstack_usda::writer::{Attribute, Document, Metadatum, Prim, Value};
//!
//! let mut root = Prim::def("Xform", "Root");
//! root.push_property(
//!     Attribute::new("xformOpOrder", "token[]", Value::TokenArray(vec![
//!         "xformOp:translate".into(),
//!     ]))
//!     .uniform(),
//! );
//! root.push_property(Attribute::new(
//!     "xformOp:translate",
//!     "double3",
//!     Value::Double3([0.0, 0.0, 1.5]),
//! ));
//!
//! let mut doc = Document::new();
//! doc.default_prim = Some("Root".into());
//! doc.metadata.push(Metadatum::new("upAxis", Value::Token("Z".into())));
//! doc.prims.push(root);
//!
//! let text = doc.to_usda()?;
//! assert!(text.starts_with("#usda 1.0\n(\n    defaultPrim = \"Root\"\n"));
//! assert!(text.contains("uniform token[] xformOpOrder = [\"xformOp:translate\"]"));
//! # Ok::<(), layerstack_usda::writer::WriteError>(())
//! ```
//!
//! Spec: AOUSD Core §16.2 (USDA grammar), §7.3.3 (names), §7.6 (core
//! metadata fields), §6.2–§6.5 (value types and semantic aliases).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

pub use crate::ast::Specifier;

const INDENT: &str = "    ";

// ── Document model ──────────────────────────────────────────────────────

/// A USDA layer to be written: layer metadata plus root prims.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Document {
    /// The `defaultPrim` layer metadata field, written as given. When set,
    /// it must name a prim of the document: a root prim by name, or a prim
    /// below the root by a relative (`Model/Geo`) or absolute (`/Model/Geo`)
    /// path, as OpenUSD reads it (`SdfLayer::GetDefaultPrimAsPath`).
    ///
    /// Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim`).
    pub default_prim: Option<String>,
    /// Further layer metadata (e.g. `upAxis`, `metersPerUnit`, `doc`),
    /// written after `defaultPrim` in this order.
    pub metadata: Vec<Metadatum>,
    /// Root prim ordering (`reorder rootPrims = [...]`, the pseudo-root's
    /// `primOrder` field), or `None` when not authored.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields), §16.2.18.
    pub prim_order: Option<Vec<String>>,
    /// Sublayers (the `subLayers` and `subLayerOffsets` fields), strongest
    /// first, written after the layer metadata; empty when not authored.
    ///
    /// Spec: AOUSD Core §10.3.1 (sublayers), §7.6.1 (layer spec fields).
    pub sublayers: Vec<SubLayer>,
    /// Root prims, in order.
    pub prims: Vec<Prim>,
}

impl Document {
    /// Creates an empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serializes the document as USDA text.
    ///
    /// # Errors
    ///
    /// Returns a [`WriteError`] when a name is not a valid identifier, a
    /// sibling name repeats, an attribute's type name is unknown or does not
    /// match its value, an asset path cannot be quoted, or `defaultPrim` does
    /// not name a root prim. Nothing is written in that case.
    pub fn to_usda(&self) -> Result<String, WriteError> {
        let mut out = String::new();
        self.write_usda(&mut out)?;
        Ok(out)
    }

    /// Serializes the document as USDA text into `out`.
    ///
    /// Validation runs before anything is written, so on error `out` is left
    /// unchanged.
    ///
    /// # Errors
    ///
    /// See [`Self::to_usda`].
    pub fn write_usda(&self, out: &mut String) -> Result<(), WriteError> {
        self.validate()?;
        let mut w = Writer { out };
        w.document(self);
        Ok(())
    }

    /// Whether `path` (a root prim name, or a relative or absolute prim
    /// path of names) names a prim of the document.
    ///
    /// The path is read as `layerstack::Layer::default_prim_path` reads it
    /// (OpenUSD's `SdfLayer::ConvertDefaultPrimTokenToPath`): an optional
    /// leading `/`, then `/`-separated prim names. Unlike composition, which
    /// accepts any such path and reports a missing prim when an arc uses
    /// it, the writer also requires the prim to be in the document.
    fn names_prim(&self, path: &str) -> bool {
        let mut segments = path.strip_prefix('/').unwrap_or(path).split('/');
        let Some(first) = segments.next() else {
            return false;
        };
        let Some(mut prim) = self.prims.iter().find(|p| p.name == first) else {
            return false;
        };
        for segment in segments {
            match prim.children.iter().find(|c| c.name == segment) {
                Some(child) => prim = child,
                None => return false,
            }
        }
        true
    }

    /// Checks everything [`Self::to_usda`] checks, without writing.
    ///
    /// Other serializations of a document (e.g. the binary crate writer in
    /// `layerstack_usdc`) run this first, so every format accepts and rejects
    /// the same documents for the same reasons.
    ///
    /// # Errors
    ///
    /// See [`Self::to_usda`].
    pub fn validate(&self) -> Result<(), WriteError> {
        let mut keys: Vec<&str> = Vec::new();
        if self.default_prim.is_some() {
            keys.push("defaultPrim");
        }
        validate_metadata(&self.metadata, &mut keys, "/", false)?;
        for sublayer in &self.sublayers {
            validate_asset_path(&sublayer.asset, "/")?;
            validate_layer_offset(sublayer.offset, "/")?;
        }
        validate_order(self.prim_order.as_deref(), "/", is_identifier)?;
        if let Some(name) = &self.default_prim
            && !self.names_prim(name)
        {
            return Err(WriteError::DefaultPrimNotFound { name: name.clone() });
        }
        let mut names: Vec<&str> = Vec::new();
        for prim in &self.prims {
            prim.validate("", &mut names)?;
        }
        Ok(())
    }
}

/// A prim spec with its metadata, properties and child prims.
///
/// Spec: AOUSD Core §7.3.5 (prim specs), §16.2.17 (prim spec grammar).
#[derive(Clone, Debug, PartialEq)]
pub struct Prim {
    /// `def`, `over` or `class`.
    pub specifier: Specifier,
    /// Schema type name (e.g. `Mesh`), or `None` for a typeless prim.
    pub type_name: Option<String>,
    /// Prim name; must be a valid identifier (§7.3.3).
    pub name: String,
    /// Prim metadata (e.g. `kind`), in order.
    pub metadata: Vec<Metadatum>,
    /// Attributes and relationships, in one authored order (the
    /// `properties` children field). Names must be unique across both
    /// kinds (§7.3.3).
    pub properties: Vec<Property>,
    /// Property ordering (`reorder properties = [...]`, the
    /// `propertyOrder` field), or `None` when not authored.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    pub property_order: Option<Vec<String>>,
    /// Child ordering (`reorder nameChildren = [...]`, the `primOrder`
    /// field), or `None` when not authored.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    pub prim_order: Option<Vec<String>>,
    /// Inherit arcs (the `inheritPaths` field): absolute prim paths.
    ///
    /// Spec: AOUSD Core §10.3.2.3 (inherits).
    pub inherits: Option<ListOp<String>>,
    /// Payload arcs (the `payload` field).
    ///
    /// Spec: AOUSD Core §10.3.2.2 (payloads).
    pub payloads: Option<ListOp<Reference>>,
    /// Reference arcs (the `references` field).
    ///
    /// Spec: AOUSD Core §10.3.2.1 (references).
    pub references: Option<ListOp<Reference>>,
    /// Specialize arcs (the `specializes` field): absolute prim paths.
    ///
    /// Spec: AOUSD Core §10.3.2.4 (specializes).
    pub specializes: Option<ListOp<String>>,
    /// Child prims, in order.
    pub children: Vec<Self>,
}

impl Prim {
    /// Creates a `def` prim of schema type `type_name`.
    pub fn def(type_name: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(Specifier::Def, Some(type_name.into()), name)
    }

    /// Creates a prim with an explicit specifier and optional type name.
    pub fn new(specifier: Specifier, type_name: Option<String>, name: impl Into<String>) -> Self {
        Self {
            specifier,
            type_name,
            name: name.into(),
            metadata: Vec::new(),
            properties: Vec::new(),
            property_order: None,
            prim_order: None,
            inherits: None,
            payloads: None,
            references: None,
            specializes: None,
            children: Vec::new(),
        }
    }

    /// Appends an attribute or relationship to [`Self::properties`].
    pub fn push_property(&mut self, property: impl Into<Property>) {
        self.properties.push(property.into());
    }

    /// Whether the prim authors any composition arc.
    fn has_arcs(&self) -> bool {
        self.inherits.is_some()
            || self.payloads.is_some()
            || self.references.is_some()
            || self.specializes.is_some()
    }

    fn validate<'a>(&'a self, parent: &str, siblings: &mut Vec<&'a str>) -> Result<(), WriteError> {
        let path = if parent.is_empty() {
            alloc::format!("/{}", self.name)
        } else {
            alloc::format!("{parent}/{}", self.name)
        };
        if !is_identifier(&self.name) {
            return Err(WriteError::InvalidName {
                path,
                name: self.name.clone(),
            });
        }
        if siblings.contains(&self.name.as_str()) {
            return Err(WriteError::Duplicate { path });
        }
        siblings.push(&self.name);
        if let Some(type_name) = &self.type_name
            && !is_identifier(type_name)
        {
            return Err(WriteError::InvalidName {
                path,
                name: type_name.clone(),
            });
        }
        validate_metadata(&self.metadata, &mut Vec::new(), &path, true)?;
        for (key, arcs) in [
            ("inherits", &self.inherits),
            ("specializes", &self.specializes),
        ] {
            let Some(op) = arcs else { continue };
            validate_arc_list(op, &path, key)?;
            for target in op.items() {
                validate_prim_path(target, &path)?;
            }
        }
        for (key, arcs) in [
            ("payload", &self.payloads),
            ("references", &self.references),
        ] {
            let Some(op) = arcs else { continue };
            validate_arc_list(op, &path, key)?;
            for arc in op.items() {
                if let Some(asset) = &arc.asset {
                    validate_asset_path(asset, &path)?;
                }
                if let Some(target) = &arc.prim_path {
                    validate_prim_path(target, &path)?;
                }
                validate_layer_offset(arc.offset, &path)?;
            }
        }
        validate_order(self.property_order.as_deref(), &path, is_property_name)?;
        validate_order(self.prim_order.as_deref(), &path, is_identifier)?;
        let mut property_names: Vec<&str> = Vec::new();
        for property in &self.properties {
            let name = property.name();
            let prop_path = alloc::format!("{path}.{name}");
            if !is_property_name(name) {
                return Err(WriteError::InvalidName {
                    path: prop_path,
                    name: name.into(),
                });
            }
            if property_names.contains(&name) {
                return Err(WriteError::Duplicate { path: prop_path });
            }
            property_names.push(name);
            match property {
                Property::Attribute(attribute) => attribute.validate(&prop_path)?,
                Property::Relationship(relationship) => relationship.validate(&prop_path)?,
            }
        }
        let mut child_names: Vec<&str> = Vec::new();
        for child in &self.children {
            child.validate(&path, &mut child_names)?;
        }
        Ok(())
    }
}

/// A property spec of a [`Prim`]: an attribute or a relationship.
///
/// Spec: AOUSD Core §7.3.7 (attribute specs and relationship specs are
/// collectively property specs).
#[derive(Clone, Debug, PartialEq)]
pub enum Property {
    /// An attribute spec.
    Attribute(Attribute),
    /// A relationship spec.
    Relationship(Relationship),
}

impl Property {
    /// The property name.
    pub fn name(&self) -> &str {
        match self {
            Self::Attribute(attribute) => &attribute.name,
            Self::Relationship(relationship) => &relationship.name,
        }
    }
}

impl From<Attribute> for Property {
    fn from(attribute: Attribute) -> Self {
        Self::Attribute(attribute)
    }
}

impl From<Relationship> for Property {
    fn from(relationship: Relationship) -> Self {
        Self::Relationship(relationship)
    }
}

/// Attribute variability (the `uniform` qualifier).
///
/// Spec: AOUSD Core §16.2.16.1 (attribute declarations), §16.3.10.29.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variability {
    /// May vary over time (no qualifier).
    #[default]
    Varying,
    /// Written with the `uniform` qualifier.
    Uniform,
}

/// An attribute spec: declaration, optional default value, optional time
/// samples, optional connections and metadata.
///
/// Written as up to three kinds of statement, in the order OpenUSD writes
/// them (`pxr/usd/sdf/fileIO_Common.h`, `Sdf_WriteAttribute`):
///
/// - the declaration `[custom] [uniform] type name [= value] [( metadata )]`,
///   written unless the attribute only carries time samples or connections
///   (no default, no metadata, not `custom`);
/// - `[uniform] type name.timeSamples = { time: value, ... }` when
///   [`Self::time_samples`] is authored, with `None` for a blocked sample;
/// - `[uniform] type name.connect = <target>` (or `[<a>, <b>]`, or `None`
///   for an explicit empty list) when [`Self::connections`] is an explicit
///   list, otherwise one `delete`, `prepend` or `append` statement of that
///   form per non-empty edit.
///
/// So `token outputs:surface.connect = </M/S.outputs:surface>` is an
/// attribute with no value and one connection, and an input that has both
/// a default and a connection is written as two lines.
///
/// Spec: AOUSD Core §16.2.16 (attribute specs), §16.2.16.3 (time samples),
/// §16.2.16.4 (connections).
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    /// Namespaced property name (e.g. `primvars:st`).
    pub name: String,
    /// Declared value type name, including a trailing `[]` for arrays (e.g.
    /// `texCoord2f[]`). Semantic aliases such as `point3f`, `normal3f` and
    /// `color3f` are accepted and preserved.
    pub type_name: String,
    /// Whether the declaration carries the `custom` keyword.
    pub custom: bool,
    /// Whether the declaration carries the `uniform` keyword.
    pub variability: Variability,
    /// Default value; `None` writes a bare declaration, and
    /// [`Value::Block`] a value block (`= None`).
    pub value: Option<Value>,
    /// Time samples (the `timeSamples` field): `(time, value)` pairs with
    /// finite, strictly increasing times, each value of the declared type
    /// or [`Value::Block`] for a blocked sample. `None` authors no samples;
    /// an empty list authors an empty sample map.
    ///
    /// Spec: AOUSD Core §16.2.16.3 (time samples), §12.3.6 (blocked
    /// samples).
    pub time_samples: Option<Vec<(f64, Value)>>,
    /// Connections (the `connectionPaths` field): a list op of absolute
    /// property paths such as `/Root/Materials/M/Tex.outputs:rgb`. `None`
    /// authors no connections; an explicit empty list blocks weaker ones
    /// (`.connect = None`).
    pub connections: Option<ListOp<String>>,
    /// Attribute metadata (e.g. `interpolation`, `elementSize`), in order.
    pub metadata: Vec<Metadatum>,
}

impl Attribute {
    /// Creates a varying, non-custom attribute with a default value.
    pub fn new(name: impl Into<String>, type_name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            custom: false,
            variability: Variability::Varying,
            value: Some(value),
            time_samples: None,
            connections: None,
            metadata: Vec::new(),
        }
    }

    /// Creates a varying, non-custom attribute without a default value: a
    /// bare declaration such as `float3 outputs:rgb`, or, with
    /// [`Self::with_connection`], a connection-only attribute.
    pub fn declared(name: impl Into<String>, type_name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            type_name: type_name.into(),
            custom: false,
            variability: Variability::Varying,
            value: None,
            time_samples: None,
            connections: None,
            metadata: Vec::new(),
        }
    }

    /// Appends a connection target (an absolute property path) to the
    /// explicit connection list, starting one when none is authored.
    #[must_use]
    pub fn with_connection(mut self, target: impl Into<String>) -> Self {
        self.connections
            .get_or_insert_with(|| ListOp::explicit(Vec::new()))
            .explicit
            .get_or_insert_with(Vec::new)
            .push(target.into());
        self
    }

    /// Marks the attribute `uniform`.
    #[must_use]
    pub fn uniform(mut self) -> Self {
        self.variability = Variability::Uniform;
        self
    }

    /// Marks the attribute `custom`.
    #[must_use]
    pub fn custom(mut self) -> Self {
        self.custom = true;
        self
    }

    /// Appends an attribute metadata entry.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: Value) -> Self {
        self.metadata.push(Metadatum::new(key, value));
        self
    }

    fn validate(&self, path: &str) -> Result<(), WriteError> {
        let Some(declared) = parse_type_name(&self.type_name) else {
            return Err(WriteError::UnknownType {
                path: path.into(),
                type_name: self.type_name.clone(),
            });
        };
        if let Some(value) = &self.value
            && !matches!(value, Value::Block)
        {
            if value.shape() != Some(declared) {
                return Err(WriteError::TypeMismatch {
                    path: path.into(),
                    type_name: self.type_name.clone(),
                });
            }
            validate_value(value, path)?;
        }
        if let Some(samples) = &self.time_samples {
            let mut previous: Option<f64> = None;
            for (time, value) in samples {
                if !time.is_finite() || previous.is_some_and(|p| p >= *time) {
                    return Err(WriteError::InvalidTimeSamples { path: path.into() });
                }
                previous = Some(*time);
                if matches!(value, Value::Block) {
                    continue;
                }
                if value.shape() != Some(declared) {
                    return Err(WriteError::TypeMismatch {
                        path: path.into(),
                        type_name: self.type_name.clone(),
                    });
                }
                validate_value(value, path)?;
            }
        }
        validate_targets(self.connections.as_ref(), path, true)?;
        validate_metadata(&self.metadata, &mut Vec::new(), path, true)
    }
}

/// A relationship spec: declaration, optional targets and metadata.
///
/// Written as `[custom] rel name [= targets] [( metadata )]`, where
/// `targets` is an explicit list: `<path>`, `[<a>, <b>]` or `None`. A
/// list-edited target list is written as one `delete`, `prepend` or
/// `append` `rel name = targets` statement per non-empty edit, after the
/// declaration; the declaration itself is then written only when the
/// relationship is `custom` or has metadata, as OpenUSD writes it.
/// Relationships are always uniform; the `varying` qualifier is not
/// representable.
///
/// Spec: AOUSD Core §16.2.16.7–§16.2.16.9 (relationship specs); OpenUSD
/// `pxr/usd/sdf/fileIO_Common.h`, `Sdf_WriteRelationship`.
#[derive(Clone, Debug, PartialEq)]
pub struct Relationship {
    /// Namespaced property name (e.g. `material:binding`).
    pub name: String,
    /// Whether the declaration carries the `custom` keyword.
    pub custom: bool,
    /// Targets (the `targetPaths` field): a list op of absolute prim or
    /// property paths. `None` writes a bare declaration (`rel name`, no
    /// targets authored); an explicit empty list writes `rel name = None`,
    /// which blocks weaker opinions.
    pub targets: Option<ListOp<String>>,
    /// Relationship metadata (e.g. `bindMaterialAs`), in order.
    pub metadata: Vec<Metadatum>,
}

impl Relationship {
    /// Creates a non-custom relationship with one explicit target.
    pub fn new(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            custom: false,
            targets: Some(ListOp::explicit(alloc::vec![target.into()])),
            metadata: Vec::new(),
        }
    }

    /// Marks the relationship `custom`.
    #[must_use]
    pub fn custom(mut self) -> Self {
        self.custom = true;
        self
    }

    fn validate(&self, path: &str) -> Result<(), WriteError> {
        validate_targets(self.targets.as_ref(), path, false)?;
        validate_metadata(&self.metadata, &mut Vec::new(), path, true)
    }
}

/// A time offset and scale (`SdfLayerOffset`) on a sublayer, reference or
/// payload: time `t` in the included layer is time `t * scale + offset` in
/// the including one. Both must be finite.
///
/// Written as `(offset = 10; scale = 2)` after the asset, leaving out an
/// identity part, as OpenUSD writes it
/// (`Sdf_FileIOUtility::WriteLayerOffset`).
///
/// Spec: AOUSD Core §12.3.2.1 (layer offsets).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerOffset {
    /// Time offset, in time codes.
    pub offset: f64,
    /// Time scale.
    pub scale: f64,
}

impl LayerOffset {
    /// The identity: no offset, unit scale.
    pub const IDENTITY: Self = Self {
        offset: 0.0,
        scale: 1.0,
    };
}

impl Default for LayerOffset {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// A sublayer: the asset path as authored, not a resolved location, and
/// its layer offset.
///
/// Spec: AOUSD Core §10.3.1 (sublayers).
#[derive(Clone, Debug, PartialEq)]
pub struct SubLayer {
    /// The asset path, as authored; not empty, and without `@` or line
    /// breaks.
    pub asset: String,
    /// The sublayer's layer offset.
    pub offset: LayerOffset,
}

/// A reference or payload arc, as authored (`SdfReference` without
/// `customData`, `SdfPayload`).
///
/// Written `@asset@<prim path> (offset = ...; scale = ...)`: an internal
/// arc (no asset) is `<prim path>`, and one that targets the `defaultPrim`
/// has no prim path, `<>` for an internal arc.
///
/// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads).
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// The asset path, as authored (never a resolved location); `None` for
    /// an internal arc into this layer. When set it is not empty, and has
    /// no `@` or line breaks.
    pub asset: Option<String>,
    /// The absolute prim path the arc targets; `None` targets the
    /// `defaultPrim` of the asset (of this layer, for an internal arc).
    pub prim_path: Option<String>,
    /// The arc's layer offset.
    pub offset: LayerOffset,
}

/// A `key = value` metadata entry on a layer, prim or attribute.
///
/// Spec: AOUSD Core §7.4 (metadata fields), §16.2.15 (common metadata).
#[derive(Clone, Debug, PartialEq)]
pub struct Metadatum {
    /// Metadata field name; must be a plain identifier.
    pub key: String,
    /// Field value. Tokens and strings are both written quoted, as USDA
    /// metadata syntax requires. A [`Value::TokenListOp`] is written as one
    /// statement per operation (`prepend key = [...]`); it is accepted on
    /// prims, attributes and relationships, not in layer metadata or inside
    /// dictionaries. The `comment` field takes a string or token and is
    /// written as USDA spells it, a bare quoted string.
    pub value: Value,
}

impl Metadatum {
    /// Creates a metadata entry.
    pub fn new(key: impl Into<String>, value: Value) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }
}

/// An authored value.
///
/// Numeric element types are explicit so the declared attribute type can be
/// checked against the value (§6.5.1, type and alias agreement). Arrays of
/// small vectors are stored packed for large mesh buffers.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `bool`.
    Bool(bool),
    /// `int`.
    Int(i32),
    /// `uint`.
    UInt(u32),
    /// `int64`.
    Int64(i64),
    /// `float`.
    Float(f32),
    /// `double` (also accepted for `timecode`).
    Double(f64),
    /// `string`.
    String(String),
    /// `token`.
    Token(String),
    /// `asset`; the path may not contain `@` or line breaks.
    Asset(String),
    /// `float2` and its semantic aliases (e.g. `texCoord2f`).
    Float2([f32; 2]),
    /// `float3` and its semantic aliases (e.g. `point3f`, `color3f`).
    Float3([f32; 3]),
    /// `float4` and its semantic aliases (e.g. `color4f`).
    Float4([f32; 4]),
    /// `double2` and its semantic aliases.
    Double2([f64; 2]),
    /// `double3` and its semantic aliases.
    Double3([f64; 3]),
    /// `double4` and its semantic aliases.
    Double4([f64; 4]),
    /// `int2`.
    Int2([i32; 2]),
    /// `int3`.
    Int3([i32; 3]),
    /// `int4`.
    Int4([i32; 4]),
    /// `matrix4d` (or `frame4d`), row-major with the translation in the
    /// last row, as USD authors it (row vectors, §6.3).
    Matrix4d([[f64; 4]; 4]),
    /// `bool[]`.
    BoolArray(Vec<bool>),
    /// `int[]`.
    IntArray(Vec<i32>),
    /// `uint[]`.
    UIntArray(Vec<u32>),
    /// `int64[]`.
    Int64Array(Vec<i64>),
    /// `float[]`.
    FloatArray(Vec<f32>),
    /// `double[]`.
    DoubleArray(Vec<f64>),
    /// `string[]`.
    StringArray(Vec<String>),
    /// `token[]`.
    TokenArray(Vec<String>),
    /// `asset[]`.
    AssetArray(Vec<String>),
    /// `float2[]` and aliases (e.g. `texCoord2f[]`).
    Float2Array(Vec<[f32; 2]>),
    /// `float3[]` and aliases (e.g. `point3f[]`, `normal3f[]`).
    Float3Array(Vec<[f32; 3]>),
    /// `float4[]` and aliases.
    Float4Array(Vec<[f32; 4]>),
    /// `double2[]` and aliases.
    Double2Array(Vec<[f64; 2]>),
    /// `double3[]` and aliases.
    Double3Array(Vec<[f64; 3]>),
    /// `double4[]` and aliases.
    Double4Array(Vec<[f64; 4]>),
    /// `int2[]`.
    Int2Array(Vec<[i32; 2]>),
    /// `int3[]`.
    Int3Array(Vec<[i32; 3]>),
    /// `int4[]`.
    Int4Array(Vec<[i32; 4]>),
    /// `quath[]`: half-precision quaternions as IEEE 754 binary16 bits, each
    /// stored `[i, j, k, r]` (imaginary part first, real part last), the
    /// memory order of OpenUSD's `GfQuath`. USDA writes each one real part
    /// first, `(r, i, j, k)`, as OpenUSD does.
    ///
    /// Spec: AOUSD Core §6.3 (`quath`, dimensioned types).
    QuathArray(Vec<[u16; 4]>),
    /// A dictionary: string keys to values, written in order with each
    /// entry's canonical type name (§6.6.2).
    ///
    /// Dictionaries are *metadata* values (e.g. `customData`). They are not
    /// an attribute value type in USD (`Sdf` registers no `dictionary`
    /// attribute type), so an [`Attribute`] cannot hold or declare one.
    Dictionary(Vec<(String, Self)>),
    /// A token list operation (`SdfTokenListOp`), such as the `apiSchemas`
    /// prim metadata. Like [`Self::Dictionary`] it is a metadata value only;
    /// see [`ListOp`] for how it is written.
    ///
    /// Spec: AOUSD Core §6.6.3 (list operations), §16.2.14 (list-op
    /// syntax).
    TokenListOp(ListOp<String>),
    /// A value block (`None`, `SdfValueBlock`): valid only as an
    /// [`Attribute`] default, of any declared type, where it blocks weaker
    /// opinions.
    ///
    /// Spec: AOUSD Core §12.3 (value blocking).
    Block,
}

/// A list operation: either an explicit list, or edits (`delete`,
/// `prepend`, `append`) applied to weaker opinions.
///
/// Written in OpenUSD's order (`pxr/usd/sdf/fileIO_Common.cpp`,
/// `_WriteListOp`): the explicit list as `key = [...]`; otherwise one
/// statement per non-empty edit, `delete`, then `prepend`, then `append`.
/// An explicit list excludes edits, and a list op must say something: both
/// are checked ([`WriteError::InvalidListOp`]). The legacy `add` and
/// `reorder` operations are not representable.
///
/// Spec: AOUSD Core §6.6.3 (list operations), §16.2.14 (list-op syntax).
#[derive(Clone, Debug, PartialEq)]
pub struct ListOp<T> {
    /// The explicit list, replacing weaker opinions; `None` when the list
    /// op is made of edits.
    pub explicit: Option<Vec<T>>,
    /// Items removed from weaker opinions.
    pub deleted: Vec<T>,
    /// Items added to the front.
    pub prepended: Vec<T>,
    /// Items added to the back.
    pub appended: Vec<T>,
}

/// An empty list op: no explicit list and no edits, which says nothing and
/// is only a starting point for building one.
impl<T> Default for ListOp<T> {
    fn default() -> Self {
        Self {
            explicit: None,
            deleted: Vec::new(),
            prepended: Vec::new(),
            appended: Vec::new(),
        }
    }
}

impl<T> ListOp<T> {
    /// An explicit list.
    pub fn explicit(items: Vec<T>) -> Self {
        Self {
            explicit: Some(items),
            deleted: Vec::new(),
            prepended: Vec::new(),
            appended: Vec::new(),
        }
    }

    /// A list op that prepends `items`.
    pub fn prepend(items: Vec<T>) -> Self {
        Self {
            explicit: None,
            deleted: Vec::new(),
            prepended: items,
            appended: Vec::new(),
        }
    }

    /// Every item of every list, explicit first, then deleted, prepended
    /// and appended.
    pub fn items(&self) -> impl Iterator<Item = &T> {
        self.explicit
            .iter()
            .flatten()
            .chain(&self.deleted)
            .chain(&self.prepended)
            .chain(&self.appended)
    }

    fn is_valid(&self) -> bool {
        let edits =
            !(self.deleted.is_empty() && self.prepended.is_empty() && self.appended.is_empty());
        if self.explicit.is_some() {
            !edits
        } else {
            edits
        }
    }
}

// ── Errors ──────────────────────────────────────────────────────────────

/// Why a [`Document`] could not be written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// A prim name, property name, type name or metadata key is not a valid
    /// identifier (§7.3.3).
    InvalidName {
        /// Path of the offending object.
        path: String,
        /// The rejected name.
        name: String,
    },
    /// Two siblings (prims, attributes or metadata keys on one owner) share
    /// a name.
    Duplicate {
        /// Path of the second occurrence (metadata keys appended after `#`).
        path: String,
    },
    /// An attribute's type name is not a USD attribute value type (this
    /// includes `dictionary`, which is only valid for metadata).
    UnknownType {
        /// Attribute path.
        path: String,
        /// The rejected type name.
        type_name: String,
    },
    /// An attribute's value does not have the element type, arity or
    /// array-ness its declared type requires.
    TypeMismatch {
        /// Attribute path.
        path: String,
        /// The declared type name.
        type_name: String,
    },
    /// An asset path contains `@` or a line break and cannot be quoted.
    InvalidAssetPath {
        /// Path of the owning object.
        path: String,
        /// The rejected asset path.
        asset: String,
    },
    /// A string, token or dictionary key contains NUL, which USD text
    /// cannot represent.
    NulInString {
        /// Path of the owning object.
        path: String,
    },
    /// `defaultPrim` does not name a prim of the document.
    DefaultPrimNotFound {
        /// The `defaultPrim` value.
        name: String,
    },
    /// A relationship target or connection is not an absolute path of
    /// identifiers (`/A/B` or `/A/B.ns:prop`); connections must name a
    /// property.
    InvalidTargetPath {
        /// Path of the owning property.
        path: String,
        /// The rejected target.
        target: String,
    },
    /// A list-op metadata value, connection list or target list is empty,
    /// mixes an explicit list with edits, or appears where USDA has no
    /// list-op syntax (layer metadata, dictionary entries); or an arc list
    /// repeats an item within one operation.
    InvalidListOp {
        /// Path of the owning object, with the metadata key after `#`.
        path: String,
    },
    /// A value block (`None`) appears outside an attribute default.
    MisplacedBlock {
        /// Path of the owning object, with the metadata key after `#`.
        path: String,
    },
    /// A metadata key USDA spells with dedicated syntax: composition arcs
    /// and sublayers, which are written from their own members
    /// ([`Prim::references`], [`Document::sublayers`], ...), and what this
    /// writer does not produce: variant fields, relocates, identifier-valued
    /// fields (`permission`, `symmetryFunction`) and the substitution maps.
    /// A quoted `key = value` statement would not parse.
    ReservedMetadata {
        /// Path of the owning object.
        path: String,
        /// The metadata key.
        key: String,
    },
    /// The `comment` metadata is not a string or token.
    CommentNotText {
        /// Path of the owning object.
        path: String,
    },
    /// An attribute's sample times are not finite and strictly increasing.
    InvalidTimeSamples {
        /// Attribute path.
        path: String,
    },
    /// An arc's prim path is not an absolute prim path of identifiers.
    InvalidArcPath {
        /// Path of the prim that authors the arc.
        path: String,
        /// The rejected prim path.
        target: String,
    },
    /// A layer offset is not finite.
    InvalidLayerOffset {
        /// Path of the owning object (`/` for a sublayer).
        path: String,
    },
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName { path, name } => write!(f, "{path}: invalid identifier {name:?}"),
            Self::Duplicate { path } => write!(f, "{path}: duplicate name"),
            Self::UnknownType { path, type_name } => {
                write!(f, "{path}: {type_name:?} is not a USD attribute value type")
            }
            Self::TypeMismatch { path, type_name } => {
                write!(
                    f,
                    "{path}: value does not match declared type {type_name:?}"
                )
            }
            Self::InvalidAssetPath { path, asset } => {
                write!(f, "{path}: asset path {asset:?} cannot be quoted")
            }
            Self::NulInString { path } => write!(f, "{path}: string contains NUL"),
            Self::DefaultPrimNotFound { name } => {
                write!(f, "defaultPrim {name:?} does not name a prim")
            }
            Self::InvalidTargetPath { path, target } => {
                write!(f, "{path}: invalid target path {target:?}")
            }
            Self::InvalidListOp { path } => write!(f, "{path}: invalid list op"),
            Self::MisplacedBlock { path } => {
                write!(f, "{path}: a value block is only an attribute default")
            }
            Self::ReservedMetadata { path, key } => {
                write!(
                    f,
                    "{path}: {key:?} needs USDA syntax this writer does not produce"
                )
            }
            Self::CommentNotText { path } => write!(f, "{path}: comment is not text"),
            Self::InvalidTimeSamples { path } => {
                write!(f, "{path}: sample times are not finite and increasing")
            }
            Self::InvalidArcPath { path, target } => {
                write!(
                    f,
                    "{path}: arc path {target:?} is not an absolute prim path"
                )
            }
            Self::InvalidLayerOffset { path } => write!(f, "{path}: layer offset is not finite"),
        }
    }
}

impl core::error::Error for WriteError {}

// ── Validation helpers ──────────────────────────────────────────────────

/// `([XID_Start] / '_') [XID_Continue]*` — §7.3.3 (`PrimName`), with the
/// Unicode XID tables the lexer also uses, so written names always re-parse
/// here and in OpenUSD (`pxr/usd/sdf/path.cpp`, `_IsValidIdentifier`).
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(crate::ident::is_start) && chars.all(crate::ident::is_continue)
}

/// Colon-joined identifiers — §7.3.3 (`PropertyName`).
fn is_property_name(name: &str) -> bool {
    name.split(':').all(is_identifier)
}

/// An absolute path to a prim (`/A/B`) or, after one `.`, to a property of
/// it (`/A/B.ns:prop`) — the target forms USDA relationships and
/// connections use (§16.2.9; §16.2.16.9 excludes variant selections from
/// targets). Relative paths, variant selections and
/// relational attribute paths are not produced by this writer.
fn validate_target(target: &str, path: &str, property: bool) -> Result<(), WriteError> {
    let (prim, prop) = match target.split_once('.') {
        Some((prim, prop)) => (prim, Some(prop)),
        None => (target, None),
    };
    let prim_ok = prim
        .strip_prefix('/')
        .is_some_and(|rest| rest.split('/').all(is_identifier));
    let prop_ok = match prop {
        Some(prop) => is_property_name(prop),
        None => !property,
    };
    if prim_ok && prop_ok {
        Ok(())
    } else {
        Err(WriteError::InvalidTargetPath {
            path: path.into(),
            target: target.into(),
        })
    }
}

/// An asset path the writers can quote: not empty, without `@` or line
/// breaks (`@@@`-quoting is not produced).
fn validate_asset_path(asset: &str, path: &str) -> Result<(), WriteError> {
    if asset.is_empty() || asset.contains(['@', '\n', '\r', '\0']) {
        return Err(WriteError::InvalidAssetPath {
            path: path.into(),
            asset: asset.into(),
        });
    }
    Ok(())
}

/// An arc's prim path: `/A/B`, absolute prim names only (no property,
/// variant selection or relative path).
fn validate_prim_path(target: &str, path: &str) -> Result<(), WriteError> {
    let ok = target
        .strip_prefix('/')
        .is_some_and(|rest| rest.split('/').all(is_identifier));
    if ok {
        Ok(())
    } else {
        Err(WriteError::InvalidArcPath {
            path: path.into(),
            target: target.into(),
        })
    }
}

fn validate_layer_offset(offset: LayerOffset, path: &str) -> Result<(), WriteError> {
    if offset.offset.is_finite() && offset.scale.is_finite() {
        Ok(())
    } else {
        Err(WriteError::InvalidLayerOffset { path: path.into() })
    }
}

/// An arc list op is well formed (see [`ListOp`]) and no list repeats an
/// item: OpenUSD refuses to open a layer whose arc field lists one item
/// twice in the same operation ("Duplicate items exist for field"). The
/// same item may appear in different operations, such as a `delete` and a
/// `prepend`. Arcs compare as a whole: asset path, prim path and layer
/// offset.
///
/// Spec: AOUSD Core §6.6.3 (list operations).
fn validate_arc_list<T: PartialEq>(
    op: &ListOp<T>,
    path: &str,
    key: &str,
) -> Result<(), WriteError> {
    let repeats = |items: &[T]| {
        items
            .iter()
            .enumerate()
            .any(|(i, item)| items[..i].contains(item))
    };
    let lists = [
        op.explicit.as_deref().unwrap_or(&[]),
        &op.deleted,
        &op.prepended,
        &op.appended,
    ];
    if op.is_valid() && !lists.into_iter().any(repeats) {
        Ok(())
    } else {
        Err(WriteError::InvalidListOp {
            path: alloc::format!("{path}#{key}"),
        })
    }
}

/// Validates a connection or target list op: well formed (see
/// [`ListOp`]), with every path a valid target.
fn validate_targets(
    targets: Option<&ListOp<String>>,
    path: &str,
    property: bool,
) -> Result<(), WriteError> {
    let Some(op) = targets else {
        return Ok(());
    };
    if !op.is_valid() {
        return Err(WriteError::InvalidListOp { path: path.into() });
    }
    for target in op.items() {
        validate_target(target, path, property)?;
    }
    Ok(())
}

/// Validates the names of a `reorder` statement: each a valid name, none
/// repeated.
fn validate_order(
    order: Option<&[String]>,
    path: &str,
    valid: fn(&str) -> bool,
) -> Result<(), WriteError> {
    let mut seen: Vec<&str> = Vec::new();
    for name in order.into_iter().flatten() {
        if !valid(name) {
            return Err(WriteError::InvalidName {
                path: path.into(),
                name: name.clone(),
            });
        }
        if seen.contains(&name.as_str()) {
            return Err(WriteError::Duplicate {
                path: alloc::format!("{path}#{name}"),
            });
        }
        seen.push(name);
    }
    Ok(())
}

/// Metadata keys with dedicated USDA syntax that a quoted `key = value`
/// statement cannot express (OpenUSD `pxr/usd/sdf/textFileFormat.peg`):
/// composition arcs and sublayers (written from their own members),
/// variant fields and relocates, the identifier-valued `permission` and
/// `symmetryFunction`, and the string-to-string substitution maps.
const RESERVED_METADATA: &[&str] = &[
    "references",
    "payload",
    "inherits",
    "specializes",
    "variants",
    "variantSets",
    "subLayers",
    "relocates",
    "permission",
    "symmetryFunction",
    "prefixSubstitutions",
    "suffixSubstitutions",
];

/// Validates metadata keys and values; `list_ops` admits
/// [`Value::TokenListOp`] entries (prim and property metadata only).
fn validate_metadata<'a>(
    entries: &'a [Metadatum],
    seen: &mut Vec<&'a str>,
    path: &str,
    list_ops: bool,
) -> Result<(), WriteError> {
    for entry in entries {
        if matches!(entry.value, Value::Block) {
            return Err(WriteError::MisplacedBlock {
                path: alloc::format!("{path}#{}", entry.key),
            });
        }
        if let Value::TokenListOp(op) = &entry.value
            && !(list_ops && op.is_valid())
        {
            return Err(WriteError::InvalidListOp {
                path: alloc::format!("{path}#{}", entry.key),
            });
        }
        if !is_identifier(&entry.key) {
            return Err(WriteError::InvalidName {
                path: path.into(),
                name: entry.key.clone(),
            });
        }
        if RESERVED_METADATA.contains(&entry.key.as_str()) {
            return Err(WriteError::ReservedMetadata {
                path: path.into(),
                key: entry.key.clone(),
            });
        }
        if entry.key == "comment" && !matches!(entry.value, Value::String(_) | Value::Token(_)) {
            return Err(WriteError::CommentNotText { path: path.into() });
        }
        if seen.contains(&entry.key.as_str()) {
            return Err(WriteError::Duplicate {
                path: alloc::format!("{path}#{}", entry.key),
            });
        }
        seen.push(&entry.key);
        validate_value(&entry.value, path)?;
    }
    Ok(())
}

fn validate_value(value: &Value, path: &str) -> Result<(), WriteError> {
    let bad_asset = |asset: &String| {
        (asset.contains('@') || asset.contains('\n') || asset.contains('\r')).then(|| {
            WriteError::InvalidAssetPath {
                path: path.into(),
                asset: asset.clone(),
            }
        })
    };
    // USD strings cannot carry NUL: OpenUSD truncates at it, silently
    // dropping the rest of the value.
    let bad_text = |text: &String| {
        text.contains('\0')
            .then(|| WriteError::NulInString { path: path.into() })
    };
    match value {
        Value::String(text) | Value::Token(text) => bad_text(text).map_or(Ok(()), Err),
        Value::StringArray(texts) | Value::TokenArray(texts) => {
            texts.iter().find_map(bad_text).map_or(Ok(()), Err)
        }
        Value::Asset(asset) => bad_asset(asset).map_or(Ok(()), Err),
        Value::AssetArray(assets) => assets.iter().find_map(bad_asset).map_or(Ok(()), Err),
        Value::Dictionary(entries) => {
            for (key, v) in entries {
                if let Some(error) = bad_text(key) {
                    return Err(error);
                }
                if matches!(v, Value::TokenListOp(_)) {
                    return Err(WriteError::InvalidListOp {
                        path: alloc::format!("{path}#{key}"),
                    });
                }
                if matches!(v, Value::Block) {
                    return Err(WriteError::MisplacedBlock {
                        path: alloc::format!("{path}#{key}"),
                    });
                }
                validate_value(v, path)?;
            }
            Ok(())
        }
        Value::TokenListOp(op) => op.items().find_map(bad_text).map_or(Ok(()), Err),
        _ => Ok(()),
    }
}

// ── Type table ──────────────────────────────────────────────────────────

/// Element storage of a value type, for type/value agreement checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Elem {
    Bool,
    Int,
    UInt,
    Int64,
    Float,
    Double,
    String,
    Token,
    Asset,
    Dictionary,
    ListOp,
    /// A half-precision quaternion (`quath`).
    Quath,
    /// A known type this writer has no [`Value`] variant for (e.g. `half`,
    /// quaternions). It can be declared but not given a value.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    elem: Elem,
    arity: u8,
    array: bool,
}

/// Maps a USD attribute value type name to its storage shape.
///
/// The accepted names are a subset of the OpenUSD `Sdf` value type registry
/// (`pxr/usd/sdf/schema.cpp`, `_RegisterStandardTypes`; `pxr/usd/sdf/types.h`,
/// `SDF_VALUE_TYPES` plus the role aliases), each optionally with `[]`.
/// Deliberately excluded: `dictionary` (metadata only, never an attribute
/// type), `opaque`/`group`/`pathExpression`, and the legacy capitalized
/// aliases (`Vec3f`, `PointFloat`, ...). Registered types without a
/// [`Value`] variant (e.g. `half`, quaternions) may be declared without a
/// default.
///
/// Spec: AOUSD Core §6.2 (scalar types), §6.3 (dimensioned types), §6.5
/// (semantic aliases).
fn parse_type_name(type_name: &str) -> Option<Shape> {
    let (base, array) = match type_name.strip_suffix("[]") {
        Some(base) => (base, true),
        None => (type_name, false),
    };
    let (elem, arity) = match base {
        "bool" => (Elem::Bool, 1),
        "int" => (Elem::Int, 1),
        "uint" => (Elem::UInt, 1),
        "int64" => (Elem::Int64, 1),
        "float" => (Elem::Float, 1),
        "double" | "timecode" => (Elem::Double, 1),
        "string" => (Elem::String, 1),
        "token" => (Elem::Token, 1),
        "asset" => (Elem::Asset, 1),
        "int2" => (Elem::Int, 2),
        "int3" => (Elem::Int, 3),
        "int4" => (Elem::Int, 4),
        "float2" | "texCoord2f" => (Elem::Float, 2),
        "float3" | "point3f" | "normal3f" | "vector3f" | "color3f" | "texCoord3f" => {
            (Elem::Float, 3)
        }
        "float4" | "color4f" => (Elem::Float, 4),
        "double2" | "texCoord2d" => (Elem::Double, 2),
        "double3" | "point3d" | "normal3d" | "vector3d" | "color3d" | "texCoord3d" => {
            (Elem::Double, 3)
        }
        "double4" | "color4d" => (Elem::Double, 4),
        "matrix4d" | "frame4d" => (Elem::Double, 16),
        "quath" => (Elem::Quath, 1),
        "uchar" | "uint64" | "half" | "half2" | "half3" | "half4" | "texCoord2h" | "texCoord3h"
        | "point3h" | "normal3h" | "vector3h" | "color3h" | "color4h" | "matrix2d" | "matrix3d"
        | "quatf" | "quatd" => (Elem::Unsupported, 0),
        _ => return None,
    };
    Some(Shape { elem, arity, array })
}

impl Value {
    /// An empty array of the declared array type `type_name` (such as
    /// `point3f[]`), or `None` when the name is not an array type this
    /// writer has a [`Value`] for.
    pub(crate) fn empty_array_of(type_name: &str) -> Option<Self> {
        let shape = parse_type_name(type_name).filter(|shape| shape.array)?;
        Some(match (shape.elem, shape.arity) {
            (Elem::Bool, 1) => Self::BoolArray(Vec::new()),
            (Elem::Int, 1) => Self::IntArray(Vec::new()),
            (Elem::UInt, 1) => Self::UIntArray(Vec::new()),
            (Elem::Int64, 1) => Self::Int64Array(Vec::new()),
            (Elem::Float, 1) => Self::FloatArray(Vec::new()),
            (Elem::Double, 1) => Self::DoubleArray(Vec::new()),
            (Elem::String, 1) => Self::StringArray(Vec::new()),
            (Elem::Token, 1) => Self::TokenArray(Vec::new()),
            (Elem::Asset, 1) => Self::AssetArray(Vec::new()),
            (Elem::Float, 2) => Self::Float2Array(Vec::new()),
            (Elem::Float, 3) => Self::Float3Array(Vec::new()),
            (Elem::Float, 4) => Self::Float4Array(Vec::new()),
            (Elem::Double, 2) => Self::Double2Array(Vec::new()),
            (Elem::Double, 3) => Self::Double3Array(Vec::new()),
            (Elem::Double, 4) => Self::Double4Array(Vec::new()),
            (Elem::Int, 2) => Self::Int2Array(Vec::new()),
            (Elem::Int, 3) => Self::Int3Array(Vec::new()),
            (Elem::Int, 4) => Self::Int4Array(Vec::new()),
            _ => return None,
        })
    }

    fn shape(&self) -> Option<Shape> {
        let (elem, arity, array) = match self {
            Self::Bool(_) => (Elem::Bool, 1, false),
            Self::Int(_) => (Elem::Int, 1, false),
            Self::UInt(_) => (Elem::UInt, 1, false),
            Self::Int64(_) => (Elem::Int64, 1, false),
            Self::Float(_) => (Elem::Float, 1, false),
            Self::Double(_) => (Elem::Double, 1, false),
            Self::String(_) => (Elem::String, 1, false),
            Self::Token(_) => (Elem::Token, 1, false),
            Self::Asset(_) => (Elem::Asset, 1, false),
            Self::Float2(_) => (Elem::Float, 2, false),
            Self::Float3(_) => (Elem::Float, 3, false),
            Self::Float4(_) => (Elem::Float, 4, false),
            Self::Double2(_) => (Elem::Double, 2, false),
            Self::Double3(_) => (Elem::Double, 3, false),
            Self::Double4(_) => (Elem::Double, 4, false),
            Self::Int2(_) => (Elem::Int, 2, false),
            Self::Int3(_) => (Elem::Int, 3, false),
            Self::Int4(_) => (Elem::Int, 4, false),
            Self::Matrix4d(_) => (Elem::Double, 16, false),
            Self::BoolArray(_) => (Elem::Bool, 1, true),
            Self::IntArray(_) => (Elem::Int, 1, true),
            Self::UIntArray(_) => (Elem::UInt, 1, true),
            Self::Int64Array(_) => (Elem::Int64, 1, true),
            Self::FloatArray(_) => (Elem::Float, 1, true),
            Self::DoubleArray(_) => (Elem::Double, 1, true),
            Self::StringArray(_) => (Elem::String, 1, true),
            Self::TokenArray(_) => (Elem::Token, 1, true),
            Self::AssetArray(_) => (Elem::Asset, 1, true),
            Self::Float2Array(_) => (Elem::Float, 2, true),
            Self::Float3Array(_) => (Elem::Float, 3, true),
            Self::Float4Array(_) => (Elem::Float, 4, true),
            Self::Double2Array(_) => (Elem::Double, 2, true),
            Self::Double3Array(_) => (Elem::Double, 3, true),
            Self::Double4Array(_) => (Elem::Double, 4, true),
            Self::Int2Array(_) => (Elem::Int, 2, true),
            Self::Int3Array(_) => (Elem::Int, 3, true),
            Self::Int4Array(_) => (Elem::Int, 4, true),
            Self::QuathArray(_) => (Elem::Quath, 1, true),
            Self::Dictionary(_) => (Elem::Dictionary, 1, false),
            Self::TokenListOp(_) => (Elem::ListOp, 1, false),
            Self::Block => return None,
        };
        Some(Shape { elem, arity, array })
    }

    /// The canonical (alias-free) USD type name of this value, as used for
    /// typed dictionary entries. For the metadata-only values this is the
    /// `Sdf` value type name (`dictionary`, `tokenListOp`), which is not an
    /// attribute type; a [`Self::Block`] has the `SdfValueBlock` type.
    pub fn canonical_type_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::Int(_) => "int",
            Self::UInt(_) => "uint",
            Self::Int64(_) => "int64",
            Self::Float(_) => "float",
            Self::Double(_) => "double",
            Self::String(_) => "string",
            Self::Token(_) => "token",
            Self::Asset(_) => "asset",
            Self::Float2(_) => "float2",
            Self::Float3(_) => "float3",
            Self::Float4(_) => "float4",
            Self::Double2(_) => "double2",
            Self::Double3(_) => "double3",
            Self::Double4(_) => "double4",
            Self::Int2(_) => "int2",
            Self::Int3(_) => "int3",
            Self::Int4(_) => "int4",
            Self::Matrix4d(_) => "matrix4d",
            Self::BoolArray(_) => "bool[]",
            Self::IntArray(_) => "int[]",
            Self::UIntArray(_) => "uint[]",
            Self::Int64Array(_) => "int64[]",
            Self::FloatArray(_) => "float[]",
            Self::DoubleArray(_) => "double[]",
            Self::StringArray(_) => "string[]",
            Self::TokenArray(_) => "token[]",
            Self::AssetArray(_) => "asset[]",
            Self::Float2Array(_) => "float2[]",
            Self::Float3Array(_) => "float3[]",
            Self::Float4Array(_) => "float4[]",
            Self::Double2Array(_) => "double2[]",
            Self::Double3Array(_) => "double3[]",
            Self::Double4Array(_) => "double4[]",
            Self::Int2Array(_) => "int2[]",
            Self::Int3Array(_) => "int3[]",
            Self::Int4Array(_) => "int4[]",
            Self::QuathArray(_) => "quath[]",
            Self::Dictionary(_) => "dictionary",
            Self::TokenListOp(_) => "tokenListOp",
            Self::Block => "SdfValueBlock",
        }
    }
}

// ── Text emission ───────────────────────────────────────────────────────

/// Infallible text sink over a validated document. `fmt::Write` for
/// `String` never fails, so results are discarded deliberately.
struct Writer<'o> {
    out: &'o mut String,
}

impl Writer<'_> {
    fn document(&mut self, doc: &Document) {
        // §16.2.18.1: the layer header.
        self.out.push_str("#usda 1.0\n");
        if doc.default_prim.is_some() || !doc.metadata.is_empty() || !doc.sublayers.is_empty() {
            self.out.push_str("(\n");
            if let Some(name) = &doc.default_prim {
                self.out.push_str(INDENT);
                self.out.push_str("defaultPrim = ");
                self.string(name);
                self.out.push('\n');
            }
            self.metadata_entries(&doc.metadata, 1);
            if !doc.sublayers.is_empty() {
                // §16.2.18.3: `subLayers = [ @asset@ (offset = ...), ... ]`.
                self.out.push_str(INDENT);
                self.out.push_str("subLayers = [\n");
                for (i, sublayer) in doc.sublayers.iter().enumerate() {
                    self.indent(2);
                    self.asset(&sublayer.asset);
                    self.layer_offset(sublayer.offset);
                    if i + 1 < doc.sublayers.len() {
                        self.out.push(',');
                    }
                    self.out.push('\n');
                }
                self.out.push_str(INDENT);
                self.out.push_str("]\n");
            }
            self.out.push_str(")\n");
        }
        if let Some(order) = &doc.prim_order {
            self.out.push('\n');
            self.reorder("rootPrims", order, 0);
        }
        for prim in &doc.prims {
            self.out.push('\n');
            self.prim(prim, 0);
        }
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push_str(INDENT);
        }
    }

    fn metadata_entries(&mut self, entries: &[Metadatum], depth: usize) {
        for entry in entries {
            if let Value::TokenListOp(op) = &entry.value {
                self.list_op(&entry.key, op, depth);
                continue;
            }
            self.indent(depth);
            // §16.2.15: a bare string is the `comment` field.
            if let ("comment", Value::String(text) | Value::Token(text)) =
                (entry.key.as_str(), &entry.value)
            {
                self.string(text);
                self.out.push('\n');
                continue;
            }
            self.out.push_str(&entry.key);
            self.out.push_str(" = ");
            self.value(&entry.value, depth);
            self.out.push('\n');
        }
    }

    /// §16.2.14: one `[op] key = [items]` statement per operation, in
    /// OpenUSD's order. Validation guarantees the op is non-empty and not
    /// mixed.
    fn list_op(&mut self, key: &str, op: &ListOp<String>, depth: usize) {
        self.list_op_statements(op, depth, |w, keyword, items| {
            w.out.push_str(keyword);
            w.out.push_str(key);
            w.out.push_str(" = ");
            w.array(items, |w, s| w.string(s));
        });
    }

    /// §16.2.17: `specifier [type] "name" [( metadata )] { body }`.
    fn prim(&mut self, prim: &Prim, depth: usize) {
        self.indent(depth);
        self.out.push_str(match prim.specifier {
            Specifier::Def => "def ",
            Specifier::Over => "over ",
            Specifier::Class => "class ",
        });
        if let Some(type_name) = &prim.type_name {
            self.out.push_str(type_name);
            self.out.push(' ');
        }
        self.string(&prim.name);
        if !prim.metadata.is_empty() || prim.has_arcs() {
            self.out.push_str(" (\n");
            self.metadata_entries(&prim.metadata, depth + 1);
            self.arcs(prim, depth + 1);
            self.indent(depth);
            self.out.push(')');
        }
        self.out.push('\n');
        self.indent(depth);
        self.out.push_str("{\n");
        if let Some(order) = &prim.property_order {
            self.reorder("properties", order, depth + 1);
        }
        if let Some(order) = &prim.prim_order {
            self.reorder("nameChildren", order, depth + 1);
        }
        for property in &prim.properties {
            match property {
                Property::Attribute(attribute) => self.attribute(attribute, depth + 1),
                Property::Relationship(relationship) => self.relationship(relationship, depth + 1),
            }
        }
        let has_body = !prim.properties.is_empty()
            || prim.property_order.is_some()
            || prim.prim_order.is_some();
        for (i, child) in prim.children.iter().enumerate() {
            if i > 0 || has_body {
                self.out.push('\n');
            }
            self.prim(child, depth + 1);
        }
        self.indent(depth);
        self.out.push_str("}\n");
    }

    /// The prim's composition arcs, in key order as OpenUSD writes them:
    /// `inherits`, `payload`, `references`, `specializes` (§16.2.17.4,
    /// §16.2.17.5), each
    /// as list-op statements.
    fn arcs(&mut self, prim: &Prim, depth: usize) {
        if let Some(op) = &prim.inherits {
            self.arc_statements("inherits", op, depth, |w, path| w.path(path));
        }
        if let Some(op) = &prim.payloads {
            self.arc_statements("payload", op, depth, Self::reference);
        }
        if let Some(op) = &prim.references {
            self.arc_statements("references", op, depth, Self::reference);
        }
        if let Some(op) = &prim.specializes {
            self.arc_statements("specializes", op, depth, |w, path| w.path(path));
        }
    }

    /// `[op] key = item`, `[a, b]` or `None` per list-op statement.
    fn arc_statements<T>(
        &mut self,
        key: &str,
        op: &ListOp<T>,
        depth: usize,
        mut item: impl FnMut(&mut Self, &T),
    ) {
        self.list_op_statements(op, depth, |w, keyword, items| {
            w.out.push_str(keyword);
            w.out.push_str(key);
            w.out.push_str(" = ");
            match items {
                [] => w.out.push_str("None"),
                [one] => item(w, one),
                _ => w.array(items, &mut item),
            }
        });
    }

    /// `@asset@<path> (offset = ...; scale = ...)`.
    fn reference(&mut self, arc: &Reference) {
        if let Some(asset) = &arc.asset {
            self.asset(asset);
        }
        match (&arc.asset, &arc.prim_path) {
            (_, Some(path)) => self.path(path),
            (None, None) => self.out.push_str("<>"),
            (Some(_), None) => {}
        }
        self.layer_offset(arc.offset);
    }

    /// ` (offset = 10; scale = 2)`, leaving out identity parts; nothing for
    /// the identity.
    fn layer_offset(&mut self, offset: LayerOffset) {
        let has_offset = offset.offset != 0.0;
        let has_scale = offset.scale != 1.0;
        if !(has_offset || has_scale) {
            return;
        }
        self.out.push_str(" (");
        if has_offset {
            self.out.push_str("offset = ");
            self.f64(offset.offset);
        }
        if has_scale {
            if has_offset {
                self.out.push_str("; ");
            }
            self.out.push_str("scale = ");
            self.f64(offset.scale);
        }
        self.out.push(')');
    }

    /// `<path>`; validation guarantees plain identifiers.
    fn path(&mut self, path: &str) {
        self.out.push('<');
        self.out.push_str(path);
        self.out.push('>');
    }

    /// `reorder key = ["a", "b"]` (§16.2.17 for `nameChildren` and
    /// `properties`, §16.2.18 for `rootPrims`).
    fn reorder(&mut self, key: &str, names: &[String], depth: usize) {
        self.indent(depth);
        self.out.push_str("reorder ");
        self.out.push_str(key);
        self.out.push_str(" = ");
        self.array(names, |w, s| w.string(s));
        self.out.push('\n');
    }

    /// §16.2.16.1: `[custom] [uniform] type name [= value] [( metadata )]`,
    /// then `[uniform] type name.timeSamples = { ... }` (§16.2.16.3), then
    /// `[op] [uniform] type name.connect = targets`. The declaration is
    /// skipped for an attribute that only has samples or connections, as
    /// `Sdf_WriteAttribute` does.
    fn attribute(&mut self, attribute: &Attribute, depth: usize) {
        let declare = attribute.value.is_some()
            || !attribute.metadata.is_empty()
            || attribute.custom
            || (attribute.connections.is_none() && attribute.time_samples.is_none());
        if declare {
            self.indent(depth);
            if attribute.custom {
                self.out.push_str("custom ");
            }
            self.attribute_head(attribute);
            if let Some(value) = &attribute.value {
                self.out.push_str(" = ");
                self.value(value, depth);
            }
            if !attribute.metadata.is_empty() {
                self.out.push_str(" (\n");
                self.metadata_entries(&attribute.metadata, depth + 1);
                self.indent(depth);
                self.out.push(')');
            }
            self.out.push('\n');
        }
        if let Some(samples) = &attribute.time_samples {
            self.indent(depth);
            self.attribute_head(attribute);
            self.out.push_str(".timeSamples = {\n");
            for (time, value) in samples {
                self.indent(depth + 1);
                self.f64(*time);
                self.out.push_str(": ");
                self.value(value, depth + 1);
                self.out.push_str(",\n");
            }
            self.indent(depth);
            self.out.push_str("}\n");
        }
        if let Some(connections) = &attribute.connections {
            self.list_op_statements(connections, depth, |w, keyword, targets| {
                w.out.push_str(keyword);
                w.attribute_head(attribute);
                w.out.push_str(".connect = ");
                w.targets(targets);
            });
        }
    }

    /// One statement per list-op operation, in OpenUSD's order: the
    /// explicit list, otherwise `delete`, `prepend` and `append` for each
    /// non-empty edit. `statement` writes the line after the indentation,
    /// given the operation keyword (empty for the explicit list).
    fn list_op_statements<T>(
        &mut self,
        op: &ListOp<T>,
        depth: usize,
        mut statement: impl FnMut(&mut Self, &str, &[T]),
    ) {
        if let Some(items) = &op.explicit {
            self.indent(depth);
            statement(self, "", items);
            self.out.push('\n');
            return;
        }
        for (keyword, items) in [
            ("delete ", &op.deleted),
            ("prepend ", &op.prepended),
            ("append ", &op.appended),
        ] {
            if !items.is_empty() {
                self.indent(depth);
                statement(self, keyword, items);
                self.out.push('\n');
            }
        }
    }

    /// `[uniform] type name`.
    fn attribute_head(&mut self, attribute: &Attribute) {
        if attribute.variability == Variability::Uniform {
            self.out.push_str("uniform ");
        }
        self.out.push_str(&attribute.type_name);
        self.out.push(' ');
        self.out.push_str(&attribute.name);
    }

    /// §16.2.16.7: `[custom] rel name [= targets] [( metadata )]`, then
    /// `op rel name = targets` per edit of a list-edited target list.
    fn relationship(&mut self, relationship: &Relationship, depth: usize) {
        let explicit = relationship
            .targets
            .as_ref()
            .and_then(|op| op.explicit.as_deref());
        let edits = relationship
            .targets
            .as_ref()
            .filter(|op| op.explicit.is_none());
        if edits.is_none() || relationship.custom || !relationship.metadata.is_empty() {
            self.indent(depth);
            if relationship.custom {
                self.out.push_str("custom ");
            }
            self.out.push_str("rel ");
            self.out.push_str(&relationship.name);
            if let Some(targets) = explicit {
                self.out.push_str(" = ");
                self.targets(targets);
            }
            if !relationship.metadata.is_empty() {
                self.out.push_str(" (\n");
                self.metadata_entries(&relationship.metadata, depth + 1);
                self.indent(depth);
                self.out.push(')');
            }
            self.out.push('\n');
        }
        if let Some(op) = edits {
            self.list_op_statements(op, depth, |w, keyword, targets| {
                w.out.push_str(keyword);
                w.out.push_str("rel ");
                w.out.push_str(&relationship.name);
                w.out.push_str(" = ");
                w.targets(targets);
            });
        }
    }

    /// `<path>` for one target, `[<a>, <b>]` for several and `None` for
    /// none. Validation guarantees each path is plain identifiers, so it
    /// needs no escaping.
    fn targets(&mut self, targets: &[String]) {
        let path = |w: &mut Self, target: &String| {
            w.out.push('<');
            w.out.push_str(target);
            w.out.push('>');
        };
        match targets {
            [] => self.out.push_str("None"),
            [one] => path(self, one),
            _ => self.array(targets, path),
        }
    }

    fn value(&mut self, value: &Value, depth: usize) {
        match value {
            Value::Bool(v) => self.out.push_str(if *v { "true" } else { "false" }),
            Value::Int(v) => self.display(v),
            Value::UInt(v) => self.display(v),
            Value::Int64(v) => self.display(v),
            Value::Float(v) => self.f32(*v),
            Value::Double(v) => self.f64(*v),
            Value::String(v) | Value::Token(v) => self.string(v),
            Value::Asset(v) => self.asset(v),
            Value::Float2(v) => self.tuple(v, Self::f32),
            Value::Float3(v) => self.tuple(v, Self::f32),
            Value::Float4(v) => self.tuple(v, Self::f32),
            Value::Double2(v) => self.tuple(v, Self::f64),
            Value::Double3(v) => self.tuple(v, Self::f64),
            Value::Double4(v) => self.tuple(v, Self::f64),
            Value::Int2(v) => self.tuple(v, Self::i32),
            Value::Int3(v) => self.tuple(v, Self::i32),
            Value::Int4(v) => self.tuple(v, Self::i32),
            Value::Matrix4d(rows) => {
                self.out.push_str("( ");
                for (i, row) in rows.iter().enumerate() {
                    if i > 0 {
                        self.out.push_str(", ");
                    }
                    self.tuple(row, Self::f64);
                }
                self.out.push_str(" )");
            }
            Value::BoolArray(v) => self.array(v, |w, b| {
                w.out.push_str(if *b { "true" } else { "false" });
            }),
            Value::IntArray(v) => self.array(v, |w, x| w.display(x)),
            Value::UIntArray(v) => self.array(v, |w, x| w.display(x)),
            Value::Int64Array(v) => self.array(v, |w, x| w.display(x)),
            Value::FloatArray(v) => self.array(v, |w, x| w.f32(*x)),
            Value::DoubleArray(v) => self.array(v, |w, x| w.f64(*x)),
            Value::StringArray(v) | Value::TokenArray(v) => self.array(v, |w, s| w.string(s)),
            Value::AssetArray(v) => self.array(v, |w, s| w.asset(s)),
            Value::Float2Array(v) => self.array(v, |w, t| w.tuple(t, Self::f32)),
            Value::Float3Array(v) => self.array(v, |w, t| w.tuple(t, Self::f32)),
            Value::Float4Array(v) => self.array(v, |w, t| w.tuple(t, Self::f32)),
            Value::Double2Array(v) => self.array(v, |w, t| w.tuple(t, Self::f64)),
            Value::Double3Array(v) => self.array(v, |w, t| w.tuple(t, Self::f64)),
            Value::Double4Array(v) => self.array(v, |w, t| w.tuple(t, Self::f64)),
            Value::Int2Array(v) => self.array(v, |w, t| w.tuple(t, Self::i32)),
            Value::Int3Array(v) => self.array(v, |w, t| w.tuple(t, Self::i32)),
            Value::Int4Array(v) => self.array(v, |w, t| w.tuple(t, Self::i32)),
            Value::QuathArray(v) => self.array(v, |w, &[i, j, k, r]| {
                w.tuple(&[r, i, j, k], Self::half);
            }),
            // Written as statements by `metadata_entries`; validation keeps
            // list ops out of attribute values and dictionaries.
            Value::TokenListOp(_) => unreachable!("list ops are written as statements"),
            Value::Block => self.out.push_str("None"),
            Value::Dictionary(entries) => {
                // §6.6.2, §16.2.15: typed entries with quoted keys.
                self.out.push_str("{\n");
                for (key, v) in entries {
                    self.indent(depth + 1);
                    self.out.push_str(v.canonical_type_name());
                    self.out.push(' ');
                    self.string(key);
                    self.out.push_str(" = ");
                    self.value(v, depth + 1);
                    self.out.push('\n');
                }
                self.indent(depth);
                self.out.push('}');
            }
        }
    }

    fn display(&mut self, v: &impl fmt::Display) {
        let _ = write!(self.out, "{v}");
    }

    fn i32(&mut self, v: i32) {
        self.display(&v);
    }

    /// Shortest representation that parses back to the same `f32` (Rust's
    /// `Display`), with the USDA spellings for non-finite values (§16.2.5).
    fn f32(&mut self, v: f32) {
        if v.is_finite() {
            self.display(&v);
        } else {
            self.non_finite(v.is_nan(), v.is_sign_negative());
        }
    }

    /// A `half`, widened exactly to `f32` and written as that.
    fn half(&mut self, bits: u16) {
        self.f32(layerstack::half::to_f32(bits));
    }

    fn f64(&mut self, v: f64) {
        if v.is_finite() {
            self.display(&v);
        } else {
            self.non_finite(v.is_nan(), v.is_sign_negative());
        }
    }

    fn non_finite(&mut self, nan: bool, negative: bool) {
        self.out.push_str(match (nan, negative) {
            (true, _) => "nan",
            (false, false) => "inf",
            (false, true) => "-inf",
        });
    }

    fn tuple<T>(&mut self, items: &[T], mut item: impl FnMut(&mut Self, T))
    where
        T: Copy,
    {
        self.out.push('(');
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            item(self, *x);
        }
        self.out.push(')');
    }

    fn array<T>(&mut self, items: &[T], mut item: impl FnMut(&mut Self, &T)) {
        self.out.push('[');
        for (i, x) in items.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            item(self, x);
        }
        self.out.push(']');
    }

    /// Double-quoted string with the escapes of §16.2.5. The grammar has no
    /// single-character escape for backslash, so it uses the hex form.
    fn string(&mut self, s: &str) {
        self.out.push('"');
        for c in s.chars() {
            match c {
                '"' => self.out.push_str("\\\""),
                '\\' => self.out.push_str("\\x5C"),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                c if u32::from(c) < 0x20 || c == '\u{7F}' => {
                    let _ = write!(self.out, "\\x{:02X}", u32::from(c));
                }
                c => self.out.push(c),
            }
        }
        self.out.push('"');
    }

    /// `@path@`; validation guarantees the path has no `@` or line break.
    fn asset(&mut self, path: &str) {
        self.out.push('@');
        self.out.push_str(path);
        self.out.push('@');
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use super::*;
    use crate::ast;
    use crate::parser::parse;

    fn first_attribute(prim: &mut Prim) -> &mut Attribute {
        match &mut prim.properties[0] {
            Property::Attribute(attribute) => attribute,
            Property::Relationship(_) => unreachable!("the first property is an attribute"),
        }
    }

    fn mesh_doc() -> Document {
        let mut mesh = Prim::def("Mesh", "Tri");
        mesh.push_property(Attribute::new(
            "points",
            "point3f[]",
            Value::Float3Array(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.1, -2.5e-3]]),
        ));
        mesh.push_property(Attribute::new(
            "faceVertexCounts",
            "int[]",
            Value::IntArray(vec![3]),
        ));
        mesh.push_property(
            Attribute::new(
                "primvars:st",
                "texCoord2f[]",
                Value::Float2Array(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            )
            .with_metadata("interpolation", Value::Token("vertex".into())),
        );
        mesh.push_property(
            Attribute::new("subdivisionScheme", "token", Value::Token("none".into())).uniform(),
        );
        mesh.push_property(
            Attribute::new("exedra:label", "string", Value::String("say \"hi\"".into())).custom(),
        );
        let mut root = Prim::def("Xform", "Root");
        root.metadata
            .push(Metadatum::new("kind", Value::Token("component".into())));
        root.push_property(Attribute::new(
            "xformOp:transform",
            "matrix4d",
            Value::Matrix4d([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [2.0, 3.0, 4.0, 1.0],
            ]),
        ));
        root.children.push(mesh);
        Document {
            default_prim: Some("Root".into()),
            metadata: vec![
                Metadatum::new("metersPerUnit", Value::Double(1.0)),
                Metadatum::new("upAxis", Value::Token("Z".into())),
            ],
            prims: vec![root],
            ..Document::new()
        }
    }

    #[test]
    fn golden_text() {
        let text = mesh_doc().to_usda().unwrap();
        let expected = r#"#usda 1.0
(
    defaultPrim = "Root"
    metersPerUnit = 1
    upAxis = "Z"
)

def Xform "Root" (
    kind = "component"
)
{
    matrix4d xformOp:transform = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (2, 3, 4, 1) )

    def Mesh "Tri"
    {
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 0.1, -0.0025)]
        int[] faceVertexCounts = [3]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "vertex"
        )
        uniform token subdivisionScheme = "none"
        custom string exedra:label = "say \"hi\""
    }
}
"#;
        assert_eq!(text, expected, "writer output");
        assert_eq!(text, mesh_doc().to_usda().unwrap(), "deterministic");
    }

    #[test]
    fn output_reparses_with_authored_details() {
        let text = mesh_doc().to_usda().unwrap();
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let layer = &parsed.layer;
        let meta: Vec<(&str, String)> = layer
            .metadata
            .iter()
            .filter_map(|m| match m {
                ast::LayerMeta::Custom(entry) => {
                    Some((entry.key, alloc::format!("{:?}", entry.value)))
                }
                _ => None,
            })
            .collect();
        assert_eq!(meta.len(), 3, "layer metadata entries: {meta:?}");
        assert_eq!(meta[0].0, "defaultPrim", "first layer metadata key");
        assert_eq!(meta[2].0, "upAxis", "last layer metadata key");

        let root = &layer.prims[0];
        assert_eq!(root.type_name, Some("Xform"), "root type");
        let ast::PrimChild::Prim(mesh) = &root.children[1] else {
            panic!("expected child prim");
        };
        let attrs: Vec<&ast::Attribute<'_>> = mesh
            .children
            .iter()
            .filter_map(|c| match c {
                ast::PrimChild::Attribute(a) => Some(a),
                _ => None,
            })
            .collect();
        let st = attrs.iter().find(|a| a.name == "primvars:st").unwrap();
        assert_eq!(st.type_name, "texCoord2f", "st type");
        assert!(st.is_array, "st is an array");
        assert_eq!(st.metadata.len(), 1, "st metadata");
        assert_eq!(st.metadata[0].key, "interpolation", "st metadata key");
        let scheme = attrs
            .iter()
            .find(|a| a.name == "subdivisionScheme")
            .unwrap();
        assert!(
            scheme.uniform && !scheme.custom,
            "subdivisionScheme qualifiers"
        );
        let label = attrs.iter().find(|a| a.name == "exedra:label").unwrap();
        assert!(
            label.custom && !label.uniform,
            "custom attribute qualifiers"
        );
    }

    #[test]
    fn rejects_invalid_documents() {
        let mut doc = mesh_doc();
        doc.default_prim = Some("Missing".into());
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::DefaultPrimNotFound {
                name: "Missing".into()
            }),
            "defaultPrim must name a root prim"
        );
        for path in ["Root/Tri", "/Root/Tri", "/Root"] {
            let mut doc = mesh_doc();
            doc.default_prim = Some(path.into());
            let text = doc.to_usda().expect("a prim path names a prim");
            assert!(
                text.contains(&alloc::format!("defaultPrim = \"{path}\"")),
                "written as authored"
            );
        }
        for path in ["Root/Missing", "/Tri", "Root//Tri", "/"] {
            let mut doc = mesh_doc();
            doc.default_prim = Some(path.into());
            assert!(
                matches!(doc.to_usda(), Err(WriteError::DefaultPrimNotFound { .. })),
                "{path} names no prim"
            );
        }

        let mut doc = mesh_doc();
        first_attribute(&mut doc.prims[0].children[0]).type_name = "normal3f".into();
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::TypeMismatch {
                path: "/Root/Tri.points".into(),
                type_name: "normal3f".into()
            }),
            "array value needs an array type"
        );

        let mut doc = mesh_doc();
        first_attribute(&mut doc.prims[0].children[0]).type_name = "vec3f[]".into();
        assert!(
            matches!(doc.to_usda(), Err(WriteError::UnknownType { .. })),
            "unknown type"
        );

        let mut doc = mesh_doc();
        doc.prims[0].children[0].name = "1bad".into();
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidName { .. })),
            "prim names are identifiers"
        );

        let mut doc = mesh_doc();
        let dup = doc.prims[0].children[0].properties[0].clone();
        doc.prims[0].children[0].push_property(dup);
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::Duplicate {
                path: "/Root/Tri.points".into()
            }),
            "duplicate attribute"
        );

        let mut doc = mesh_doc();
        doc.metadata
            .push(Metadatum::new("defaultPrim", Value::Token("Root".into())));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::Duplicate { .. })),
            "defaultPrim cannot be repeated as generic metadata"
        );

        let mut doc = mesh_doc();
        doc.prims[0].children[0].push_property(Attribute::new(
            "tex",
            "asset",
            Value::Asset("a@b.png".into()),
        ));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidAssetPath { .. })),
            "unquotable asset path"
        );
    }

    #[test]
    fn scalar_spellings() {
        let mut prim = Prim::new(Specifier::Over, None, "P");
        prim.metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![
                ("a:b".to_string(), Value::Int(1)),
                (
                    "nested".to_string(),
                    Value::Dictionary(vec![("s".to_string(), Value::String("x\\y".into()))]),
                ),
            ]),
        ));
        prim.push_property(Attribute::new(
            "f",
            "float[]",
            Value::FloatArray(vec![f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 0.1, -0.0]),
        ));
        prim.push_property(Attribute::new(
            "tex",
            "asset",
            Value::Asset("textures/a.png".into()),
        ));
        let doc = Document {
            prims: vec![prim],
            ..Document::new()
        };
        let text = doc.to_usda().unwrap();
        let expected = r#"#usda 1.0

over "P" (
    customData = {
        int "a:b" = 1
        dictionary "nested" = {
            string "s" = "x\x5Cy"
        }
    }
)
{
    float[] f = [inf, -inf, nan, 0.1, -0]
    asset tex = @textures/a.png@
}
"#;
        assert_eq!(text, expected, "scalar spellings");
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    }

    #[test]
    fn quath_arrays_are_written_real_part_first() {
        // Bits stored `[i, j, k, r]`: a 90° turn about Z and the identity.
        let value = Value::QuathArray(vec![[0, 0, 0x39a8, 0x39a8], [0, 0, 0, 0x3c00]]);
        let text = doc_with(Attribute::new("orientations", "quath[]", value.clone()))
            .to_usda()
            .unwrap();
        assert!(
            text.contains("quath[] orientations = [(0.70703125, 0, 0, 0.70703125), (1, 0, 0, 0)]"),
            "{text}"
        );
        assert!(parse(&text).diagnostics.is_empty(), "reparses");
        assert_eq!(value.canonical_type_name(), "quath[]", "canonical type");
        for wrong in ["half4[]", "float4[]", "quatf[]", "quath"] {
            let err = doc_with(Attribute::new("q", wrong, value.clone()))
                .to_usda()
                .unwrap_err();
            assert!(
                matches!(err, WriteError::TypeMismatch { .. }),
                "{wrong}: {err:?}"
            );
        }
    }

    fn doc_with(attribute: Attribute) -> Document {
        let mut prim = Prim::def("Xform", "Root");
        prim.push_property(attribute);
        Document {
            prims: vec![prim],
            ..Document::new()
        }
    }

    #[test]
    fn dictionaries_are_metadata_not_attribute_types() {
        let dict = Value::Dictionary(vec![("k".to_string(), Value::Int(1))]);
        let with_default = Attribute::new("data", "dictionary", dict.clone()).custom();
        assert_eq!(
            doc_with(with_default).to_usda(),
            Err(WriteError::UnknownType {
                path: "/Root.data".into(),
                type_name: "dictionary".into()
            }),
            "dictionary attribute with a default"
        );
        let declaration = Attribute {
            value: None,
            ..Attribute::new("data", "dictionary", Value::Int(0))
        };
        assert!(
            matches!(
                doc_with(declaration).to_usda(),
                Err(WriteError::UnknownType { .. })
            ),
            "dictionary attribute declaration"
        );
        let smuggled = Attribute::new("data", "int", dict);
        assert!(
            matches!(
                doc_with(smuggled).to_usda(),
                Err(WriteError::TypeMismatch { .. })
            ),
            "dictionary value under another type"
        );
        // Legacy capitalized aliases and non-attribute registry entries are
        // not admitted either.
        for type_name in [
            "Vec3f",
            "PointFloat",
            "opaque",
            "pathExpression",
            "dictionary[]",
        ] {
            let declaration = Attribute {
                value: None,
                ..Attribute::new("x", type_name, Value::Int(0))
            };
            assert!(
                matches!(
                    doc_with(declaration).to_usda(),
                    Err(WriteError::UnknownType { .. })
                ),
                "{type_name} rejected"
            );
        }
    }

    #[test]
    fn identifiers_follow_xid_tables() {
        // U+00B2 is alphanumeric but not XID_Continue; OpenUSD rejects it.
        let superscript = Attribute::new("x\u{b2}", "int", Value::Int(1)).custom();
        assert!(
            matches!(
                doc_with(superscript).to_usda(),
                Err(WriteError::InvalidName { .. })
            ),
            "x\u{b2} rejected"
        );
        let mut doc = doc_with(Attribute::new("a", "int", Value::Int(1)));
        doc.prims[0].name = "x\u{b2}".into();
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidName { .. })),
            "prim name x\u{b2} rejected"
        );
        // U+0301 (combining acute) is XID_Continue.
        let combining = "cafe\u{301}";
        let mut doc = doc_with(Attribute::new(
            alloc::format!("ns:{combining}"),
            "int",
            Value::Int(1),
        ));
        doc.prims[0].name = combining.into();
        let text = doc.to_usda().expect("combining marks are valid");
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.layer.prims[0].name, combining, "prim name re-parses");
    }

    /// Every character the writer escapes reads back as itself.
    #[test]
    fn escaped_strings_read_back() {
        let tricky = "say \"hi\" it's \\ \n\t\r\x01\x1f\x7f \u{e9} \u{65e5}";
        let mut prim = Prim::def("Xform", "Root");
        prim.metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![(tricky.to_string(), Value::String(tricky.into()))]),
        ));
        prim.push_property(Attribute::new("s", "string", Value::String(tricky.into())));
        let text = Document {
            prims: vec![prim],
            ..Document::new()
        }
        .to_usda()
        .unwrap();
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let root = &parsed.layer.prims[0];
        let ast::PrimChild::Attribute(attribute) = &root.children[0] else {
            panic!("expected an attribute");
        };
        assert!(matches!(&attribute.default, Some(ast::Value::String(s)) if s == tricky));
        let ast::PrimMeta::Custom(entry) = &root.metadata[0] else {
            panic!("expected customData");
        };
        let ast::MetadataValue::Dictionary(entries) = &entry.value else {
            panic!("expected a dictionary");
        };
        assert_eq!(entries[0].key, tricky);
        assert!(matches!(&entries[0].value, ast::Value::String(s) if s == tricky));
    }

    #[test]
    fn rejects_nul_in_strings() {
        let nul = Attribute::new("s", "string", Value::String("a\0b".into()));
        assert_eq!(
            doc_with(nul).to_usda(),
            Err(WriteError::NulInString {
                path: "/Root.s".into()
            }),
            "NUL would be truncated by readers"
        );
    }

    #[test]
    fn nested_dictionary_metadata_is_written() {
        let mut prim = Prim::def("Xform", "Root");
        prim.metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![
                ("exedra:path".to_string(), Value::String("a/b".into())),
                (
                    "nested".to_string(),
                    Value::Dictionary(vec![("n".to_string(), Value::Int(1))]),
                ),
            ]),
        ));
        let text = Document {
            prims: vec![prim],
            ..Document::new()
        }
        .to_usda()
        .unwrap();
        assert!(
            text.contains(
                "        dictionary \"nested\" = {\n            int \"n\" = 1\n        }"
            ),
            "{text}"
        );
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");
    }

    /// A minimal `UsdShade` network: list-op metadata, relationships and
    /// connections with and without a default value.
    fn shading_doc() -> Document {
        let mut shader = Prim::def("Shader", "Surface");
        shader.push_property(
            Attribute::new("info:id", "token", Value::Token("UsdPreviewSurface".into())).uniform(),
        );
        shader.push_property(
            Attribute::new(
                "inputs:diffuseColor",
                "color3f",
                Value::Float3([0.5, 0.5, 0.5]),
            )
            .with_connection("/Root/Mat/Tex.outputs:rgb"),
        );
        shader.push_property(
            Attribute::declared("inputs:roughness", "float")
                .with_connection("/Root/Mat/Tex.outputs:g"),
        );
        shader.push_property(Attribute::declared("outputs:surface", "token"));
        let mut material = Prim::def("Material", "Mat");
        material.push_property(
            Attribute::declared("outputs:surface", "token")
                .with_connection("/Root/Mat/Surface.outputs:surface"),
        );
        material.children.push(shader);
        let mut mesh = Prim::def("Mesh", "Body");
        mesh.metadata.push(Metadatum::new(
            "apiSchemas",
            Value::TokenListOp(ListOp::prepend(vec!["MaterialBindingAPI".into()])),
        ));
        mesh.push_property(
            Attribute::new(
                "subsetFamily:materialBind:familyType",
                "token",
                Value::Token("partition".into()),
            )
            .uniform(),
        );
        mesh.push_property(Relationship::new("material:binding", "/Root/Mat"));
        mesh.push_property(Relationship {
            targets: None,
            ..Relationship::new("exedra:declared", "/Root").custom()
        });
        mesh.push_property(Relationship {
            targets: Some(ListOp::explicit(vec![
                "/Root/Mat".into(),
                "/Root/Body.points".into(),
            ])),
            ..Relationship::new("exedra:many", "/Root")
        });
        mesh.push_property(Relationship {
            targets: Some(ListOp::explicit(Vec::new())),
            ..Relationship::new("exedra:blocked", "/Root")
        });
        let mut root = Prim::def("Xform", "Root");
        root.children.push(material);
        root.children.push(mesh);
        Document {
            default_prim: Some("Root".into()),
            prims: vec![root],
            ..Document::new()
        }
    }

    #[test]
    fn shading_network_golden_text() {
        let text = shading_doc().to_usda().unwrap();
        let expected = r#"#usda 1.0
(
    defaultPrim = "Root"
)

def Xform "Root"
{
    def Material "Mat"
    {
        token outputs:surface.connect = </Root/Mat/Surface.outputs:surface>

        def Shader "Surface"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.5, 0.5, 0.5)
            color3f inputs:diffuseColor.connect = </Root/Mat/Tex.outputs:rgb>
            float inputs:roughness.connect = </Root/Mat/Tex.outputs:g>
            token outputs:surface
        }
    }

    def Mesh "Body" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {
        uniform token subsetFamily:materialBind:familyType = "partition"
        rel material:binding = </Root/Mat>
        custom rel exedra:declared
        rel exedra:many = [</Root/Mat>, </Root/Body.points>]
        rel exedra:blocked = None
    }
}
"#;
        assert_eq!(text, expected, "writer output");
    }

    #[test]
    fn shading_network_reparses() {
        let text = shading_doc().to_usda().unwrap();
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let root = &parsed.layer.prims[0];
        let child = |prim: &ast::Prim<'_>, name: &str| -> usize {
            prim.children
                .iter()
                .position(|c| matches!(c, ast::PrimChild::Prim(p) if p.name == name))
                .unwrap_or_else(|| panic!("{name} child"))
        };
        let ast::PrimChild::Prim(mesh) = &root.children[child(root, "Body")] else {
            unreachable!()
        };
        let ast::PrimMeta::Custom(api) = &mesh.metadata[0] else {
            panic!("apiSchemas metadata: {:?}", mesh.metadata);
        };
        assert_eq!(
            (api.key, api.op),
            ("apiSchemas", ast::ListOpKind::Prepend),
            "prepend apiSchemas"
        );
        let rels: Vec<(&str, bool, Option<Vec<&str>>)> = mesh
            .children
            .iter()
            .filter_map(|c| match c {
                ast::PrimChild::Relationship(r) => Some((r.name, r.custom, r.targets.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            rels,
            [
                ("material:binding", false, Some(vec!["/Root/Mat"])),
                ("exedra:declared", true, None),
                (
                    "exedra:many",
                    false,
                    Some(vec!["/Root/Mat", "/Root/Body.points"])
                ),
                ("exedra:blocked", false, Some(vec![])),
            ],
            "relationships"
        );

        let ast::PrimChild::Prim(material) = &root.children[child(root, "Mat")] else {
            unreachable!()
        };
        let ast::PrimChild::Prim(shader) = &material.children[child(material, "Surface")] else {
            unreachable!()
        };
        let statements: Vec<(&str, bool, Option<Vec<&str>>)> = shader
            .children
            .iter()
            .filter_map(|c| match c {
                ast::PrimChild::Attribute(a) => Some((
                    a.name,
                    a.default.is_some(),
                    a.connection.as_ref().map(|c| c.targets.clone()),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            statements,
            [
                ("info:id", true, None),
                ("inputs:diffuseColor", true, None),
                (
                    "inputs:diffuseColor",
                    false,
                    Some(vec!["/Root/Mat/Tex.outputs:rgb"])
                ),
                (
                    "inputs:roughness",
                    false,
                    Some(vec!["/Root/Mat/Tex.outputs:g"])
                ),
                ("outputs:surface", false, None),
            ],
            "declarations and connection statements"
        );
    }

    #[test]
    fn rejects_invalid_targets_and_list_ops() {
        for target in [
            "Root/Mat",
            "/",
            "/Root//Mat",
            "/Root/Mat.",
            "/Root{v=a}/Mat",
        ] {
            let mut doc = shading_doc();
            let Property::Relationship(binding) = &mut doc.prims[0].children[1].properties[1]
            else {
                unreachable!("material:binding is a relationship");
            };
            binding.targets = Some(ListOp::explicit(vec![target.into()]));
            assert_eq!(
                doc.to_usda(),
                Err(WriteError::InvalidTargetPath {
                    path: "/Root/Body.material:binding".into(),
                    target: target.into()
                }),
                "relationship target {target:?}"
            );
        }
        // Connections name properties, not prims.
        let mut doc = shading_doc();
        first_attribute(&mut doc.prims[0].children[0]).connections =
            Some(ListOp::explicit(vec!["/Root/Mat/Surface".into()]));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidTargetPath { .. })),
            "connection to a prim path"
        );
        // A relationship and an attribute cannot share a name.
        let mut doc = shading_doc();
        doc.prims[0].children[1].push_property(Relationship::new(
            "subsetFamily:materialBind:familyType",
            "/Root",
        ));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::Duplicate { .. })),
            "property names are shared by attributes and relationships"
        );

        let invalid = [
            ListOp::default(),
            ListOp {
                explicit: Some(vec!["A".into()]),
                prepended: vec!["B".into()],
                ..ListOp::default()
            },
        ];
        for op in invalid {
            let mut doc = shading_doc();
            doc.prims[0].children[1].metadata[0].value = Value::TokenListOp(op.clone());
            assert_eq!(
                doc.to_usda(),
                Err(WriteError::InvalidListOp {
                    path: "/Root/Body#apiSchemas".into()
                }),
                "{op:?}"
            );
        }
        let op = Value::TokenListOp(ListOp::prepend(vec!["A".into()]));
        let mut doc = shading_doc();
        doc.metadata.push(Metadatum::new("apiSchemas", op.clone()));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidListOp { .. })),
            "no list ops in layer metadata"
        );
        let mut doc = shading_doc();
        doc.prims[0].metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![("k".into(), op.clone())]),
        ));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidListOp { .. })),
            "no list ops in dictionaries"
        );
        let mut doc = shading_doc();
        doc.prims[0].push_property(Attribute::new("x", "token[]", op).custom());
        assert!(
            matches!(doc.to_usda(), Err(WriteError::TypeMismatch { .. })),
            "a list op is not an attribute value"
        );
    }

    #[test]
    fn list_op_statements_follow_openusd_order() {
        let mut prim = Prim::new(Specifier::Over, None, "P");
        prim.metadata.push(Metadatum::new(
            "apiSchemas",
            Value::TokenListOp(ListOp {
                explicit: None,
                deleted: vec!["D".into()],
                prepended: vec!["P".into()],
                appended: vec!["A1".into(), "A2".into()],
            }),
        ));
        prim.metadata.push(Metadatum::new(
            "exedraTags",
            Value::TokenListOp(ListOp::explicit(Vec::new())),
        ));
        let text = Document {
            prims: vec![prim],
            ..Document::new()
        }
        .to_usda()
        .unwrap();
        assert!(
            text.contains(concat!(
                "    delete apiSchemas = [\"D\"]\n",
                "    prepend apiSchemas = [\"P\"]\n",
                "    append apiSchemas = [\"A1\", \"A2\"]\n",
                "    exedraTags = []\n",
            )),
            "{text}"
        );
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");
    }

    /// Reorder statements, list-edited targets and connections, explicit
    /// empty lists and a value block.
    fn edits_doc() -> Document {
        let mut prim = Prim::def("Xform", "A");
        prim.property_order = Some(vec!["y".into(), "x".into()]);
        prim.prim_order = Some(vec!["D".into(), "C".into()]);
        prim.push_property(Relationship {
            targets: Some(ListOp {
                deleted: vec!["/A/D".into()],
                prepended: vec!["/A/C".into(), "/A.x".into()],
                ..ListOp::default()
            }),
            ..Relationship::new("r", "/A")
        });
        prim.push_property(Attribute::new("x", "float", Value::Block));
        let mut y = Attribute::declared("y", "float");
        y.connections = Some(ListOp::explicit(Vec::new()));
        prim.push_property(y);
        let mut z = Attribute::new("z", "float", Value::Float(1.0)).uniform();
        z.connections = Some(ListOp {
            appended: vec!["/A.x".into()],
            ..ListOp::default()
        });
        prim.push_property(z);
        let mut s = Relationship {
            targets: Some(ListOp::prepend(vec!["/A/C".into()])),
            ..Relationship::new("s", "/A").custom()
        };
        s.metadata
            .push(Metadatum::new("doc", Value::String("s".into())));
        prim.push_property(s);
        prim.children.push(Prim::def("Scope", "C"));
        prim.children.push(Prim::def("Scope", "D"));
        Document {
            prim_order: Some(vec!["B".into(), "A".into()]),
            prims: vec![prim, Prim::new(Specifier::Over, None, "B")],
            ..Document::new()
        }
    }

    #[test]
    fn edits_golden_text() {
        let text = edits_doc().to_usda().unwrap();
        let expected = r#"#usda 1.0

reorder rootPrims = ["B", "A"]

def Xform "A"
{
    reorder properties = ["y", "x"]
    reorder nameChildren = ["D", "C"]
    delete rel r = </A/D>
    prepend rel r = [</A/C>, </A.x>]
    float x = None
    float y.connect = None
    uniform float z = 1
    append uniform float z.connect = </A.x>
    custom rel s (
        doc = "s"
    )
    prepend rel s = </A/C>

    def Scope "C"
    {
    }

    def Scope "D"
    {
    }
}

over "B"
{
}
"#;
        assert_eq!(text, expected, "writer output");
        let parsed = parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(
            parsed.layer.root_prim_order,
            Some(vec!["B", "A"]),
            "rootPrims"
        );
    }

    #[test]
    fn rejects_misplaced_blocks_and_invalid_edits() {
        let mut doc = edits_doc();
        doc.prims[0]
            .metadata
            .push(Metadatum::new("hidden", Value::Block));
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::MisplacedBlock {
                path: "/A#hidden".into()
            }),
            "a block is not metadata"
        );
        let mut doc = edits_doc();
        doc.prims[0].metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(vec![("k".into(), Value::Block)]),
        ));
        assert!(
            matches!(doc.to_usda(), Err(WriteError::MisplacedBlock { .. })),
            "a block is not a dictionary value"
        );
        let mut doc = edits_doc();
        let Property::Relationship(r) = &mut doc.prims[0].properties[0] else {
            unreachable!("r is a relationship");
        };
        r.targets = Some(ListOp::default());
        assert_eq!(
            doc.to_usda(),
            Err(WriteError::InvalidListOp {
                path: "/A.r".into()
            }),
            "an empty edit list says nothing"
        );
        let mut doc = edits_doc();
        let Property::Attribute(z) = &mut doc.prims[0].properties[3] else {
            unreachable!("z is an attribute");
        };
        z.connections = Some(ListOp {
            explicit: Some(vec!["/A.x".into()]),
            prepended: vec!["/A.y".into()],
            ..ListOp::default()
        });
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidListOp { .. })),
            "explicit connections cannot also edit"
        );
        let mut doc = edits_doc();
        doc.prims[0].prim_order = Some(vec!["C".into(), "C".into()]);
        assert!(
            matches!(doc.to_usda(), Err(WriteError::Duplicate { .. })),
            "a reorder names each child once"
        );
        let mut doc = edits_doc();
        doc.prims[0].property_order = Some(vec!["a.b".into()]);
        assert!(
            matches!(doc.to_usda(), Err(WriteError::InvalidName { .. })),
            "reordered properties are property names"
        );
    }

    #[test]
    fn comments_are_bare_strings_and_reserved_keys_are_rejected() {
        let mut prim = Prim::new(Specifier::Over, None, "P");
        prim.metadata.push(Metadatum::new(
            "comment",
            Value::String("say \"hi\"".into()),
        ));
        prim.push_property(
            Attribute::declared("a", "int").with_metadata("comment", Value::String("attr".into())),
        );
        let doc = Document {
            metadata: vec![Metadatum::new("comment", Value::String("layer".into()))],
            prims: vec![prim],
            ..Document::new()
        };
        let text = doc.to_usda().unwrap();
        let expected = r#"#usda 1.0
(
    "layer"
)

over "P" (
    "say \"hi\""
)
{
    int a (
        "attr"
    )
}
"#;
        assert_eq!(text, expected, "comments");
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");

        let mut bad = doc.clone();
        bad.metadata[0].value = Value::Int(1);
        assert_eq!(
            bad.to_usda(),
            Err(WriteError::CommentNotText { path: "/".into() }),
            "a comment is text"
        );
        for key in ["references", "permission", "symmetryFunction", "variants"] {
            let mut bad = doc.clone();
            bad.prims[0]
                .metadata
                .push(Metadatum::new(key, Value::Token("x".into())));
            assert_eq!(
                bad.to_usda(),
                Err(WriteError::ReservedMetadata {
                    path: "/P".into(),
                    key: key.into()
                }),
                "{key} has dedicated syntax"
            );
        }
    }

    /// Arcs are written in OpenUSD's syntax and re-parse; a malformed arc
    /// is rejected with its owner's path.
    ///
    /// Spec: AOUSD Core §16.2.17.5 (arc syntax), §16.2.18.3 (sublayers).
    #[test]
    fn writes_composition_arcs() {
        fn arc(asset: Option<&str>, prim_path: Option<&str>, offset: f64, scale: f64) -> Reference {
            Reference {
                asset: asset.map(Into::into),
                prim_path: prim_path.map(Into::into),
                offset: LayerOffset { offset, scale },
            }
        }
        let mut prim = Prim::def("Xform", "P");
        prim.metadata
            .push(Metadatum::new("kind", Value::Token("group".into())));
        prim.inherits = Some(ListOp::explicit(vec!["/C".into()]));
        prim.payloads = Some(ListOp::prepend(vec![arc(
            Some("./p.usda"),
            None,
            24.0,
            0.5,
        )]));
        prim.references = Some(ListOp {
            deleted: vec![arc(Some("./gone.usda"), Some("/G"), 0.0, 1.0)],
            appended: vec![arc(None, Some("/C"), 1.0, 1.0), arc(None, None, 0.0, 2.0)],
            ..ListOp::default()
        });
        prim.specializes = Some(ListOp::explicit(vec![]));
        let doc = Document {
            sublayers: vec![
                SubLayer {
                    asset: "./a.usda".into(),
                    offset: LayerOffset::IDENTITY,
                },
                SubLayer {
                    asset: "./b.usdc".into(),
                    offset: LayerOffset {
                        offset: -1.5,
                        scale: 1.0,
                    },
                },
            ],
            prims: vec![prim, Prim::new(Specifier::Class, None, "C")],
            ..Document::new()
        };
        let text = doc.to_usda().unwrap();
        let expected = r#"#usda 1.0
(
    subLayers = [
        @./a.usda@,
        @./b.usdc@ (offset = -1.5)
    ]
)

def Xform "P" (
    kind = "group"
    inherits = </C>
    prepend payload = @./p.usda@ (offset = 24; scale = 0.5)
    delete references = @./gone.usda@</G>
    append references = [</C> (offset = 1), <> (scale = 2)]
    specializes = None
)
{
}

class "C"
{
}
"#;
        assert_eq!(text, expected, "arcs");
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");

        let bad_arc = |edit: fn(&mut Document)| {
            let mut bad = doc.clone();
            edit(&mut bad);
            bad.to_usda().unwrap_err()
        };
        assert_eq!(
            bad_arc(|d| d.prims[0].inherits = Some(ListOp::explicit(vec!["/C.x".into()]))),
            WriteError::InvalidArcPath {
                path: "/P".into(),
                target: "/C.x".into()
            },
            "an arc names a prim"
        );
        assert_eq!(
            bad_arc(|d| d.prims[0].specializes = Some(ListOp::default())),
            WriteError::InvalidListOp {
                path: "/P#specializes".into()
            },
            "an arc list op says something"
        );
        let twice = arc(Some("./r.usda"), Some("/R"), 1.0, 1.0);
        let other = arc(Some("./r.usda"), Some("/R"), 2.0, 1.0);
        for (key, edit) in [
            (
                "inherits",
                (|d: &mut Document| {
                    d.prims[0].inherits = Some(ListOp::explicit(vec!["/C".into(), "/C".into()]));
                }) as fn(&mut Document),
            ),
            ("specializes", |d| {
                d.prims[0].specializes = Some(ListOp::prepend(vec!["/C".into(), "/C".into()]));
            }),
            ("references", |d| {
                let r = arc(None, Some("/C"), 0.0, 1.0);
                d.prims[0].references = Some(ListOp {
                    appended: vec![r.clone(), r],
                    ..ListOp::default()
                });
            }),
            ("payload", |d| {
                let p = arc(Some("./p.usda"), None, 0.0, 1.0);
                d.prims[0].payloads = Some(ListOp {
                    deleted: vec![p.clone(), p],
                    ..ListOp::default()
                });
            }),
        ] {
            assert_eq!(
                bad_arc(edit),
                WriteError::InvalidListOp {
                    path: alloc::format!("/P#{key}")
                },
                "{key} repeats an item within one operation"
            );
        }
        // The same item in different operations, and arcs that differ only
        // in their layer offset, are distinct and valid.
        let mut ok = doc.clone();
        ok.prims[0].references = Some(ListOp {
            deleted: vec![twice.clone()],
            prepended: vec![twice, other],
            ..ListOp::default()
        });
        ok.prims[0].inherits = Some(ListOp {
            deleted: vec!["/C".into()],
            prepended: vec!["/C".into()],
            ..ListOp::default()
        });
        let text = ok.to_usda().unwrap();
        assert!(
            text.contains("    delete inherits = </C>\n    prepend inherits = </C>\n"),
            "{text}"
        );
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");
        assert_eq!(
            bad_arc(|d| d.sublayers[0].asset = "a@b".into()),
            WriteError::InvalidAssetPath {
                path: "/".into(),
                asset: "a@b".into()
            },
            "a sublayer asset path is quotable"
        );
        assert_eq!(
            bad_arc(|d| {
                d.prims[0].payloads = Some(ListOp::explicit(vec![Reference {
                    asset: Some(String::new()),
                    prim_path: None,
                    offset: LayerOffset::IDENTITY,
                }]));
            }),
            WriteError::InvalidAssetPath {
                path: "/P".into(),
                asset: String::new()
            },
            "an internal arc has no asset path, not an empty one"
        );
        assert_eq!(
            bad_arc(|d| d.sublayers[1].offset.scale = f64::INFINITY),
            WriteError::InvalidLayerOffset { path: "/".into() },
            "a layer offset is finite"
        );
    }
}
