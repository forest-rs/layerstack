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
//! text, ordered as OpenUSD's text writer orders a layer
//! (`pxr/usd/sdf/fileIO_Common.h`, `usdaFileFormat.cpp`), whatever order
//! the document holds it in:
//!
//! - a `comment` first in each metadata section, then the other fields by
//!   name: in a prim's or variant's section the metadata, arcs, variant
//!   selections and `variantSets` together, and in a property's, in
//!   `TfDictionaryLessThan` order ([`dictionary_cmp`]); in the layer's,
//!   `defaultPrim`, the metadata and `subLayers` in byte order, then
//!   `relocates`. A field is placed by its field name: `doc` as
//!   `documentation`, `inherits` as `inheritPaths`, `variants` as
//!   `variantSelection`, `variantSets` as `variantSetNames`;
//! - properties by name in [`dictionary_cmp`] order, an attribute before a
//!   relationship of the same name;
//! - variants by name, dictionary entries (variant selections included) by
//!   key, in byte order;
//! - and in document order everything else: root and child prims,
//!   variant sets, list-op items, sublayers, targets, connections and
//!   time samples.
//!
//! The document's own order is kept for the crate writer, which, like
//! OpenUSD's, stores a layer's fields and properties in their authored
//! order. Nothing is deduplicated behind the caller's back; invalid input
//! is rejected with a [`WriteError`] rather than silently repaired. Values are
//! spelled as OpenUSD's `SdfLayer::ExportToString` spells them: strings in
//! the quotes `Sdf_FileIOUtility::Quote` picks (single quotes around text
//! with a double quote and no single quote, triple quotes around text with
//! a line break, which is written as it is), dictionary keys bare when they
//! are ASCII identifiers, `bool` values as `1` and `0` (metadata fields as
//! `true` and `false`), `double` and `float` values in the fewest digits
//! that read back, with an exponent outside `[-6, 15)` (`3.42e-8`), `half`
//! and `timecode` values in six significant digits (`0.0999756`,
//! `1e-07`), a time code that needs more in the digits that keep it, and
//! a blank line ending the layer. Written in
//! OpenUSD's field order, a layer is the text OpenUSD writes for it.
//!
//! Scope: prims with their children and property order (`reorder
//! nameChildren`, `reorder properties`, `reorder rootPrims`); attributes with
//! default values (including a value block, `= None`), time samples
//! (including blocked samples) and connection lists; relationships with
//! target lists; explicit and list-edited (`delete`, `prepend`, `append`)
//! connections and targets; metadata, including list operations of tokens,
//! strings and integers such as `prepend apiSchemas = [...]`
//! ([`Value::TokenListOp`] and its siblings); values of every scalar,
//! vector, quaternion and matrix type, and arrays of them; and
//! composition arcs by their authored asset paths: sublayers with layer
//! offsets ([`Document::sublayers`]), and references, payloads, inherits
//! and specializes in any list-op form ([`Prim::references`] and its
//! siblings); and variant sets with their variants, nested variant sets
//! included, the `variantSets` list op and variant selections
//! ([`Prim::variant_sets`] and its siblings); and splines
//! ([`Attribute::spline`]), written as OpenUSD writes them.
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

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::{self, Write as _};
use core::ptr;

use layerstack::array_edit::{ArrayIndex, Instruction, Operand};
use layerstack::ident::dictionary_cmp;
use layerstack::spline::SplineData;
use layerstack::value_type::ValueType;
use smallvec::SmallVec;

use crate::float_text;

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
    /// written with `defaultPrim` and `subLayers` in byte order of their
    /// field names (see the [module docs](self)).
    pub metadata: Vec<Metadatum>,
    /// Root prim ordering (`reorder rootPrims = [...]`, the pseudo-root's
    /// `primOrder` field), or `None` when not authored. Written only when
    /// it names two prims or more, as OpenUSD writes it.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields), §16.2.18.
    pub prim_order: Option<Vec<String>>,
    /// Sublayers (the `subLayers` and `subLayerOffsets` fields), strongest
    /// first, written after the layer metadata; empty when not authored.
    ///
    /// Spec: AOUSD Core §10.3.1 (sublayers), §7.6.1 (layer spec fields).
    pub sublayers: Vec<SubLayer>,
    /// Ordered layer relocates, as absolute prim paths. An empty target removes
    /// the source prim. Spec: AOUSD Core §7.6.1.2.4, §16.2.18.5.
    pub relocates: Vec<(String, String)>,
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
        // Text cannot preserve every binary value description. Reject
        // those differences before output rather than silently rewrite them.
        for prim in &self.prims {
            validate_text_prim(prim, "")?;
        }
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
        validate_metadata(&self.metadata, &mut keys, "/")?;
        for sublayer in &self.sublayers {
            validate_asset_path(&sublayer.asset, "/")?;
            validate_layer_offset(sublayer.offset, "/")?;
        }
        for (source, target) in &self.relocates {
            for path in [source, target].into_iter().filter(|p| !p.is_empty()) {
                if validate_prim_path(path, "/#relocates").is_err() || path == "/" {
                    return Err(WriteError::InvalidArcPath {
                        path: "/#relocates".into(),
                        target: path.clone(),
                    });
                }
            }
            if source.is_empty() {
                return Err(WriteError::InvalidArcPath {
                    path: "/#relocates".into(),
                    target: source.clone(),
                });
            }
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
    /// Prim metadata (e.g. `kind`), in authored order; written sorted with
    /// the prim's arcs and variant fields (see the [module docs](self)).
    pub metadata: Vec<Metadatum>,
    /// Attributes and relationships, in one authored order (the
    /// `properties` children field); written sorted by name. Names must be unique across both
    /// kinds (§7.3.3).
    pub properties: Vec<Property>,
    /// Property ordering (`reorder properties = [...]`, the
    /// `propertyOrder` field), or `None` when not authored. Written only
    /// when it names two properties or more, as OpenUSD writes it.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    pub property_order: Option<Vec<String>>,
    /// Child ordering (`reorder nameChildren = [...]`, the `primOrder`
    /// field), or `None` when not authored. Written only when it names two
    /// children or more, as OpenUSD writes it.
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
    /// Variant selections (`variants = { string set = "variant" }`, the
    /// `variantSelection` field): set name and selected variant, each set
    /// once, written by set name in byte order. An empty variant name
    /// selects no variant.
    ///
    /// Spec: AOUSD Core §7.6.2.3.4 (`variantSelection`), §10.3.2.5
    /// (variants).
    pub variant_selections: Vec<(String, String)>,
    /// The variant sets the prim declares (`variantSets`, the
    /// `variantSetNames` field): identifiers, in strength order.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`).
    pub variant_set_names: Option<ListOp<String>>,
    /// Child prims, in order.
    pub children: Vec<Self>,
    /// Variant sets (`variantSet "name" = { ... }`), in order, written after
    /// the children.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant set specs), §7.6.6 (variant set
    /// spec fields).
    pub variant_sets: Vec<VariantSet>,
}

/// A variant set spec: a named set of variants.
///
/// Spec: AOUSD Core §7.3.6 (variant set specs), §16.2.17 (`variantSet`
/// statements).
#[derive(Clone, Debug, PartialEq)]
pub struct VariantSet {
    /// Variant set name; must be a valid identifier (§7.3.3).
    pub name: String,
    /// The variants; at least one, each named once. Written by name in
    /// byte order.
    ///
    /// Each variant is the prim spec it holds, as OpenUSD's
    /// `SdfVariantSpec::GetPrimSpec` presents it: named after the variant,
    /// `over` and untyped, with the opinions the variant contributes to
    /// the prim that hosts the set — metadata, arcs, variant selections,
    /// properties, child prims and nested variant sets. A variant name is
    /// made of identifier characters, `|` and `-`, after an optional
    /// leading `.` (OpenUSD's `VariantName` path grammar).
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain any spec a prim
    /// spec contains), §7.6.7 (variant spec fields).
    pub variants: Vec<Prim>,
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
            variant_selections: Vec::new(),
            variant_set_names: None,
            children: Vec::new(),
            variant_sets: Vec::new(),
        }
    }

    /// Appends an attribute or relationship to [`Self::properties`].
    pub fn push_property(&mut self, property: impl Into<Property>) {
        self.properties.push(property.into());
    }

    /// Whether the prim writes a metadata section: metadata, arcs or
    /// variant fields.
    fn has_metadata(&self) -> bool {
        !self.metadata.is_empty()
            || self.inherits.is_some()
            || self.payloads.is_some()
            || self.references.is_some()
            || self.specializes.is_some()
            || !self.variant_selections.is_empty()
            || self.variant_set_names.is_some()
    }

    fn validate<'a>(&'a self, parent: &str, siblings: &mut Vec<&'a str>) -> Result<(), WriteError> {
        // A prim in a variant (`/P{v=x}C`) follows its variant's selection
        // directly.
        let path = if parent.ends_with('}') {
            alloc::format!("{parent}{}", self.name)
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
        self.validate_body(&path)
    }

    /// Validates what a prim spec and a variant's prim spec both hold;
    /// `path` names the spec (`/P` or `/P{v=x}`).
    fn validate_body(&self, path: &str) -> Result<(), WriteError> {
        let path = String::from(path);
        validate_metadata(&self.metadata, &mut Vec::new(), &path)?;
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
                if key == "payload" && !arc.custom_data.is_empty() {
                    return Err(WriteError::PayloadCustomData { path: path.clone() });
                }
                validate_value(&Value::Dictionary(arc.custom_data.clone()), &path)?;
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
        self.validate_variant_fields(&path)?;
        let mut child_names: Vec<&str> = Vec::new();
        for child in &self.children {
            child.validate(&path, &mut child_names)?;
        }
        let mut set_names: Vec<&str> = Vec::new();
        for set in &self.variant_sets {
            set.validate(&path, &mut set_names)?;
        }
        Ok(())
    }

    /// Validates the variant selections and the `variantSets` list op:
    /// set names are identifiers, each selected once; a selection is a
    /// variant name or empty; the list op is well formed and names each
    /// set once per operation.
    ///
    /// Spec: AOUSD Core §7.6.2.3.4, §7.6.2.3.5.
    fn validate_variant_fields(&self, path: &str) -> Result<(), WriteError> {
        let key = alloc::format!("{path}#variants");
        let mut selected: Vec<&str> = Vec::new();
        for (set, variant) in &self.variant_selections {
            for (name, valid) in [
                (set, is_identifier(set)),
                (variant, variant.is_empty() || is_variant_name(variant)),
            ] {
                if !valid {
                    return Err(WriteError::InvalidName {
                        path: key,
                        name: name.clone(),
                    });
                }
            }
            if selected.contains(&set.as_str()) {
                return Err(WriteError::Duplicate {
                    path: alloc::format!("{key}/{set}"),
                });
            }
            selected.push(set);
        }
        if let Some(op) = &self.variant_set_names {
            validate_arc_list(op, path, "variantSets")?;
            if let Some(name) = op.items().find(|name| !is_identifier(name)) {
                return Err(WriteError::InvalidName {
                    path: alloc::format!("{path}#variantSets"),
                    name: name.clone(),
                });
            }
        }
        Ok(())
    }
}

impl VariantSet {
    /// Validates the set on the spec at `owner` (`/P` or `/P{v=x}`), among
    /// the sets `siblings` already named there.
    fn validate<'a>(&'a self, owner: &str, siblings: &mut Vec<&'a str>) -> Result<(), WriteError> {
        let path = alloc::format!("{owner}{{{}=}}", self.name);
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
        if self.variants.is_empty() {
            return Err(WriteError::EmptyVariantSet { path });
        }
        let mut names: Vec<&str> = Vec::new();
        for variant in &self.variants {
            let path = alloc::format!("{owner}{{{}={}}}", self.name, variant.name);
            if !is_variant_name(&variant.name) {
                return Err(WriteError::InvalidName {
                    path,
                    name: variant.name.clone(),
                });
            }
            if names.contains(&variant.name.as_str()) {
                return Err(WriteError::Duplicate { path });
            }
            names.push(&variant.name);
            if variant.specifier != Specifier::Over || variant.type_name.is_some() {
                return Err(WriteError::InvalidVariant { path });
            }
            variant.validate_body(&path)?;
        }
        Ok(())
    }
}

/// A property spec of a [`Prim`]: an attribute or a relationship.
///
/// Spec: AOUSD Core §7.3.7 (attribute specs and relationship specs are
/// collectively property specs).
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(
    target_pointer_width = "64",
    expect(
        clippy::large_enum_variant,
        reason = "most properties are attributes, which boxing would allocate one by one"
    )
)]
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
    /// A spline (the `spline` field), written as a `.spline = { ... }`
    /// statement. The attribute's type must be a `double`, `float` or
    /// `half` scalar, and the knot times finite and strictly increasing.
    ///
    /// Spec: AOUSD Core §12.3.3 (spline opinions).
    pub spline: Option<Box<SplineData>>,
    /// Authored knot dictionaries converted to this writer's value types.
    pub spline_custom_data: Vec<(f64, Vec<(String, Value)>)>,
    /// Connections (the `connectionPaths` field): a list op of absolute
    /// property paths such as `/Root/Materials/M/Tex.outputs:rgb`. `None`
    /// authors no connections; an explicit empty list blocks weaker ones
    /// (`.connect = None`).
    pub connections: Option<ListOp<String>>,
    /// Attribute metadata (e.g. `interpolation`, `elementSize`), in
    /// authored order; written sorted by name.
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
            spline: None,
            spline_custom_data: Vec::new(),
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
            spline: None,
            spline_custom_data: Vec::new(),
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
            && !matches!(value, Value::Block | Value::AnimationBlock)
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
        if let Some(spline) = self.spline.as_deref() {
            if self
                .spline_custom_data
                .iter()
                .enumerate()
                .any(|(i, (time, _))| {
                    !spline.knots.iter().any(|k| k.time == *time)
                        || self.spline_custom_data[..i].iter().any(|(t, _)| t == time)
                })
                || spline.knots.iter().any(|k| {
                    !k.custom_data.is_empty()
                        && !self.spline_custom_data.iter().any(|(t, _)| *t == k.time)
                })
            {
                return Err(WriteError::InvalidSplineCustomData { path: path.into() });
            }
            for (_, entries) in &self.spline_custom_data {
                validate_value(&Value::Dictionary(entries.clone()), path)?;
            }
            let scalar = matches!(
                self.type_name.as_str(),
                "double" | "float" | "half" | "timecode"
            );
            let increasing = spline
                .knots
                .windows(2)
                .all(|pair| pair[0].time < pair[1].time);
            if !scalar {
                return Err(WriteError::TypeMismatch {
                    path: path.into(),
                    type_name: self.type_name.clone(),
                });
            }
            if !increasing || spline.knots.iter().any(|knot| !knot.time.is_finite()) {
                return Err(WriteError::InvalidTimeSamples { path: path.into() });
            }
        }
        validate_targets(self.connections.as_ref(), path, true)?;
        validate_metadata(&self.metadata, &mut Vec::new(), path)
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
    /// Relationship metadata (e.g. `bindMaterialAs`), in authored order;
    /// written sorted by name.
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
        validate_metadata(&self.metadata, &mut Vec::new(), path)
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

/// A reference or payload arc, as authored (`SdfReference`, `SdfPayload`).
///
/// Written `@asset@<prim path> (offset = ...; scale = ...)`: an internal
/// arc (no asset) is `<prim path>`, and one that targets the `defaultPrim`
/// has no prim path, `<>` for an internal arc.
///
/// Spec: AOUSD Core §10.3.2.1 (references), §10.3.2.2 (payloads).
#[derive(Clone, Debug, PartialEq)]
pub struct Reference {
    /// Authored reference custom data. Payload arcs require an empty dictionary.
    pub custom_data: Vec<(String, Value)>,
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

impl Reference {
    /// Whether the arc is written in brackets even alone: a reference with
    /// custom data (`SingleItemRequiresBrackets`).
    fn bracketed(&self) -> bool {
        !self.custom_data.is_empty()
    }
}

/// A `key = value` metadata entry on a layer, prim or attribute.
///
/// Spec: AOUSD Core §7.4 (metadata fields), §16.2.15 (common metadata).
#[derive(Clone, Debug, PartialEq)]
pub struct Metadatum {
    /// Metadata field name; must be a plain identifier.
    pub key: String,
    /// Field value. Tokens and strings are both written quoted, as USDA
    /// metadata syntax requires. A list op ([`Value::TokenListOp`] and its
    /// string and integer siblings) is written as one statement per
    /// operation (`prepend key = [...]`); it is accepted on prims,
    /// attributes and relationships, not in layer metadata or inside
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
    /// `uchar`.
    UChar(u8),
    /// `int`.
    Int(i32),
    /// `uint`.
    UInt(u32),
    /// `int64`.
    Int64(i64),
    /// `uint64`.
    UInt64(u64),
    /// `half`, as its IEEE 754 binary16 bit pattern.
    Half(u16),
    /// `float`.
    Float(f32),
    /// `double` (also accepted for `timecode`).
    Double(f64),
    /// `timecode`: a time, written like a `double`. An attribute declared
    /// `timecode` may hold a [`Self::Double`] too; a dictionary entry needs
    /// this to be typed `timecode`.
    TimeCode(f64),
    /// `duration`: a length of time in time codes (`GfDuration`, OpenUSD
    /// 26.11), written like a [`Self::TimeCode`]. An attribute declared
    /// `duration` may hold a [`Self::Double`] too.
    Duration(f64),
    /// `string`.
    String(String),
    /// `token`.
    Token(String),
    /// `asset`; the path may not contain `@` or line breaks.
    Asset(String),
    /// Authored `SdfPathExpression` text, preserved without resolving references.
    PathExpression(String),
    /// Array of authored `SdfPathExpression` texts.
    PathExpressionArray(Vec<String>),
    /// A native sparse array edit. `element` is the scalar type descriptor;
    /// its value is ignored. Instructions retain their authored order and
    /// operate over weaker arrays rather than storing an evaluated result.
    /// OpenUSD: `VtArrayEdit<T>`; sparse-array-edits proposal.
    ArrayEdit {
        /// Scalar descriptor, such as `Int(0)` or `Float3([0.0; 3])`.
        element: Box<Self>,
        /// Portable edit program; every literal must match `element`.
        instructions: Vec<Instruction<Self>>,
    },
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
    /// `half2` and its semantic aliases, as binary16 bit patterns.
    Half2([u16; 2]),
    /// `half3` and its semantic aliases (e.g. `color3h`), as binary16 bit
    /// patterns.
    Half3([u16; 3]),
    /// `half4` and its semantic aliases, as binary16 bit patterns.
    Half4([u16; 4]),
    /// `quath`: one quaternion stored as [`Self::QuathArray`] stores each.
    Quath([u16; 4]),
    /// `quatf`: stored `[i, j, k, r]` and written `(r, i, j, k)`, like
    /// [`Self::QuathArray`].
    Quatf([f32; 4]),
    /// `quatd`: stored `[i, j, k, r]` and written `(r, i, j, k)`, like
    /// [`Self::QuathArray`].
    Quatd([f64; 4]),
    /// `matrix2d`, row-major.
    Matrix2d([[f64; 2]; 2]),
    /// `matrix3d`, row-major.
    Matrix3d([[f64; 3]; 3]),
    /// `matrix4d` (or `frame4d`), row-major with the translation in the
    /// last row, as USD authors it (row vectors, §6.3).
    Matrix4d([[f64; 4]; 4]),
    /// `bool[]`.
    BoolArray(Vec<bool>),
    /// `uchar[]`.
    UCharArray(Vec<u8>),
    /// `int[]`.
    IntArray(Vec<i32>),
    /// `uint[]`.
    UIntArray(Vec<u32>),
    /// `int64[]`.
    Int64Array(Vec<i64>),
    /// `uint64[]`.
    UInt64Array(Vec<u64>),
    /// `half[]`, as binary16 bit patterns.
    HalfArray(Vec<u16>),
    /// `float[]`.
    FloatArray(Vec<f32>),
    /// `double[]`.
    DoubleArray(Vec<f64>),
    /// `timecode[]`, as [`Self::TimeCode`] is to [`Self::Double`].
    TimeCodeArray(Vec<f64>),
    /// `duration[]`, as [`Self::Duration`] is to [`Self::Double`].
    DurationArray(Vec<f64>),
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
    /// `half2[]` and aliases.
    Half2Array(Vec<[u16; 2]>),
    /// `half3[]` and aliases.
    Half3Array(Vec<[u16; 3]>),
    /// `half4[]` and aliases.
    Half4Array(Vec<[u16; 4]>),
    /// `quath[]`: half-precision quaternions as IEEE 754 binary16 bits, each
    /// stored `[i, j, k, r]` (imaginary part first, real part last), the
    /// memory order of OpenUSD's `GfQuath`. USDA writes each one real part
    /// first, `(r, i, j, k)`, as OpenUSD does.
    ///
    /// Spec: AOUSD Core §6.3 (`quath`, dimensioned types).
    QuathArray(Vec<[u16; 4]>),
    /// `quatf[]`, each as [`Self::Quatf`] stores it.
    QuatfArray(Vec<[f32; 4]>),
    /// `quatd[]`, each as [`Self::Quatd`] stores it.
    QuatdArray(Vec<[f64; 4]>),
    /// `matrix2d[]`.
    Matrix2dArray(Vec<[[f64; 2]; 2]>),
    /// `matrix3d[]`.
    Matrix3dArray(Vec<[[f64; 3]; 3]>),
    /// `matrix4d[]` (or `frame4d[]`).
    Matrix4dArray(Vec<[[f64; 4]; 4]>),
    /// A dictionary: string keys to values, written by key in byte order
    /// with each entry's canonical type name (§6.6.2), as OpenUSD keeps a
    /// `VtDictionary`.
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
    /// A string list operation (`SdfStringListOp`), such as `clipSets`;
    /// written like [`Self::TokenListOp`].
    StringListOp(ListOp<String>),
    /// An `int` list operation (`SdfIntListOp`).
    IntListOp(ListOp<i32>),
    /// A `uint` list operation (`SdfUIntListOp`).
    UIntListOp(ListOp<u32>),
    /// An `int64` list operation (`SdfInt64ListOp`), such as
    /// `PointInstancer`'s `inactiveIds`.
    Int64ListOp(ListOp<i64>),
    /// A `uint64` list operation (`SdfUInt64ListOp`).
    UInt64ListOp(ListOp<u64>),
    /// A value block (`None`, `SdfValueBlock`): valid only as an
    /// [`Attribute`] default, of any declared type, where it blocks weaker
    /// opinions.
    ///
    /// Spec: AOUSD Core §12.3 (value blocking).
    Block,
    /// A default-time animation block (`SdfAnimationBlock`).
    AnimationBlock,
}

/// A list operation: either an explicit list, or edits (`delete`, `add`,
/// `prepend`, `append`, `reorder`) applied to weaker opinions.
///
/// Written in OpenUSD's order (`pxr/usd/sdf/fileIO_Common.cpp`,
/// `_WriteListOp`): the explicit list as `key = [...]`; otherwise one
/// statement per non-empty edit, `delete`, then `add`, then `prepend`,
/// then `append`, then `reorder`. An explicit list excludes edits, and a
/// list op must say something: both are checked
/// ([`WriteError::InvalidListOp`]).
///
/// Spec: AOUSD Core §6.6.3 (list operations), §16.2.14 (list-op syntax).
#[derive(Clone, Debug, PartialEq)]
pub struct ListOp<T> {
    /// The explicit list, replacing weaker opinions; `None` when the list
    /// op is made of edits.
    pub explicit: Option<Vec<T>>,
    /// Items removed from weaker opinions.
    pub deleted: Vec<T>,
    /// Items added to the back when absent: the legacy `add`.
    pub added: Vec<T>,
    /// Items added to the front.
    pub prepended: Vec<T>,
    /// Items added to the back.
    pub appended: Vec<T>,
    /// The order the items it names are moved into, after the other edits.
    pub reordered: Vec<T>,
}

/// An empty list op: no explicit list and no edits, which says nothing and
/// is only a starting point for building one.
impl<T> Default for ListOp<T> {
    fn default() -> Self {
        Self {
            explicit: None,
            deleted: Vec::new(),
            added: Vec::new(),
            prepended: Vec::new(),
            appended: Vec::new(),
            reordered: Vec::new(),
        }
    }
}

impl<T> ListOp<T> {
    /// An explicit list.
    pub fn explicit(items: Vec<T>) -> Self {
        Self {
            explicit: Some(items),
            deleted: Vec::new(),
            added: Vec::new(),
            prepended: Vec::new(),
            appended: Vec::new(),
            reordered: Vec::new(),
        }
    }

    /// A list op that prepends `items`.
    pub fn prepend(items: Vec<T>) -> Self {
        Self {
            explicit: None,
            deleted: Vec::new(),
            added: Vec::new(),
            prepended: items,
            appended: Vec::new(),
            reordered: Vec::new(),
        }
    }

    /// Every item of every list, explicit first, then deleted, added,
    /// prepended, appended and reordered.
    pub fn items(&self) -> impl Iterator<Item = &T> {
        self.explicit
            .iter()
            .flatten()
            .chain(&self.deleted)
            .chain(&self.added)
            .chain(&self.prepended)
            .chain(&self.appended)
            .chain(&self.reordered)
    }

    fn is_valid(&self) -> bool {
        let edits = !(self.deleted.is_empty()
            && self.added.is_empty()
            && self.prepended.is_empty()
            && self.appended.is_empty()
            && self.reordered.is_empty());
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
    /// USDA would regenerate an `AutoEase` tangent and change its stored data.
    StaleAutoEaseTangent {
        /// Attribute path.
        path: String,
    },
    /// Payloads cannot carry reference custom data.
    PayloadCustomData {
        /// Path of the owning prim.
        path: String,
    },
    /// Knot metadata has duplicate/orphan times or was not converted to writer values.
    InvalidSplineCustomData {
        /// Attribute path.
        path: String,
    },
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
    /// ([`Prim::references`], [`Document::sublayers`], ...) as are the
    /// variant fields ([`Prim::variant_selections`] and
    /// [`Prim::variant_set_names`]), and what this writer does not produce:
    /// relocates, the identifier-valued `symmetryFunction`, a `permission`
    /// other than `public` or `private`, and the substitution maps.
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
    /// A spline has a knot whose own curve type differs from the spline's,
    /// which USDA cannot write (USDC can). OpenUSD evaluates every segment
    /// with the spline's curve type.
    KnotCurveType {
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
    /// A variant set has no variants, which USDA cannot write.
    EmptyVariantSet {
        /// Variant set path (`/P{v=}`).
        path: String,
    },
    /// A variant's prim spec is not an untyped `over`, which a variant
    /// cannot hold.
    InvalidVariant {
        /// Variant path (`/P{v=x}`).
        path: String,
    },
    /// An empty explicit `variantSets` list, which USDA has no spelling
    /// for: OpenUSD's text parser takes only a name or a non-empty list
    /// there, and OpenUSD's own writer leaves the value out. The crate
    /// format holds it.
    EmptyExplicitVariantSets {
        /// Path of the prim or variant (`/P`, `/P{v=x}`).
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
            Self::KnotCurveType { path } => {
                write!(f, "{path}: a spline knot's own curve type has no USDA form")
            }
            Self::InvalidArcPath { path, target } => {
                write!(
                    f,
                    "{path}: arc path {target:?} is not an absolute prim path"
                )
            }
            Self::StaleAutoEaseTangent { path } => write!(
                f,
                "{path}: USDA would regenerate an authored AutoEase tangent"
            ),
            Self::PayloadCustomData { path } => {
                write!(f, "{path}: payloads cannot carry custom data")
            }
            Self::InvalidSplineCustomData { path } => {
                write!(f, "{path}: invalid or unconverted spline knot metadata")
            }
            Self::InvalidLayerOffset { path } => write!(f, "{path}: layer offset is not finite"),
            Self::EmptyVariantSet { path } => write!(f, "{path}: variant set has no variants"),
            Self::InvalidVariant { path } => {
                write!(f, "{path}: a variant's prim spec is not an untyped over")
            }
            Self::EmptyExplicitVariantSets { path } => {
                write!(f, "{path}: USDA cannot write an empty explicit variantSets")
            }
        }
    }
}

impl core::error::Error for WriteError {}

// ── Validation helpers ──────────────────────────────────────────────────

/// Whether `name` is a USD identifier, as prim names must be:
/// `([XID_Start] / '_') [XID_Continue]*` — §7.3.3 (`PrimName`), with the
/// Unicode XID tables the lexer also uses, so written names always re-parse
/// here and in OpenUSD (`pxr/usd/sdf/path.cpp`, `_IsValidIdentifier`).
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(crate::ident::is_start) && chars.all(crate::ident::is_continue)
}

/// Whether `name` is a variant name: identifier characters, `|` and `-`,
/// after an optional leading `.` — OpenUSD's `VariantName` path grammar
/// (`pxr/usd/sdf/pathParser.h`), less the empty name, which names a variant
/// set rather than a variant.
fn is_variant_name(name: &str) -> bool {
    let rest = name.strip_prefix('.').unwrap_or(name);
    !rest.is_empty()
        && rest
            .chars()
            .all(|c| crate::ident::is_continue(c) || c == '|' || c == '-')
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
        &op.added,
        &op.prepended,
        &op.appended,
        &op.reordered,
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
/// composition arcs, sublayers and variant fields (written from their own
/// members), relocates, the identifier-valued `symmetryFunction`, and the
/// string-to-string substitution maps. `permission` is written bare when
/// its value is `public` or `private` ([`PERMISSIONS`]).
const RESERVED_METADATA: &[&str] = &[
    "references",
    "payload",
    "inherits",
    "specializes",
    "variants",
    "variantSets",
    "subLayers",
    "relocates",
    "symmetryFunction",
    "prefixSubstitutions",
    "suffixSubstitutions",
];

/// The values of the `permission` field (`SdfPermission`), which USDA
/// writes as a bare identifier: `permission = private`.
///
/// Spec: AOUSD Core §7.6.2 and §7.6.3 (the prim and property `permission`
/// fields), §16.2.19 (`PermissionMetadata`, legacy content).
pub const PERMISSIONS: &[&str] = &["public", "private"];

/// Rejects binary descriptions USDA cannot preserve: per-knot curve types
/// and sparse-edit kinds distinct from their declaration. `parent` is the
/// path of the spec holding `prim`.
fn validate_text_prim(prim: &Prim, parent: &str) -> Result<(), WriteError> {
    // A prim in a variant (`/P{v=x}C`) follows its variant's selection
    // directly, as in `Prim::validate`.
    let path = if parent.ends_with('}') {
        alloc::format!("{parent}{}", prim.name)
    } else {
        alloc::format!("{parent}/{}", prim.name)
    };
    validate_text_body(prim, &path)
}

/// [`validate_text_prim`] for the contents of a prim spec or a variant's
/// prim spec at `path`: its properties, its children and its variant sets,
/// nested ones included.
fn validate_text_body(body: &Prim, path: &str) -> Result<(), WriteError> {
    if body
        .variant_set_names
        .as_ref()
        .and_then(|op| op.explicit.as_ref())
        .is_some_and(Vec::is_empty)
    {
        return Err(WriteError::EmptyExplicitVariantSets {
            path: String::from(path),
        });
    }
    for property in &body.properties {
        let Property::Attribute(attribute) = property else {
            continue;
        };
        let location = alloc::format!("{path}.{}", attribute.name);
        if let Some(spline) = attribute.spline.as_deref()
            && spline
                .knots
                .iter()
                .any(|knot| knot.curve_type != spline.default_curve_type)
        {
            return Err(WriteError::KnotCurveType { path: location });
        }
        if let Some(spline) = attribute.spline.as_deref() {
            let mut recomputed = spline.clone();
            crate::spline_text::apply_auto_ease(&mut recomputed);
            if spline.knots.iter().zip(&recomputed.knots).any(|(a, b)| {
                (a.pre_tan_algorithm == layerstack::spline::TangentAlgorithm::AutoEase
                    && (a.pre_tan_width.to_bits() != b.pre_tan_width.to_bits()
                        || a.pre_tan_slope.to_bits() != b.pre_tan_slope.to_bits()))
                    || (a.post_tan_algorithm == layerstack::spline::TangentAlgorithm::AutoEase
                        && (a.post_tan_width.to_bits() != b.post_tan_width.to_bits()
                            || a.post_tan_slope.to_bits() != b.post_tan_slope.to_bits()))
            }) {
                return Err(WriteError::StaleAutoEaseTangent { path: location });
            }
        }

        for value in attribute.value.iter().chain(
            attribute
                .time_samples
                .iter()
                .flatten()
                .map(|(_, value)| value),
        ) {
            // Native VtArrayEdit keeps its own storage kind. USDA derives
            // that kind from the declaration (textFileFormatParser.h), so
            // double/timecode shape compatibility alone would change retiming.
            if let Value::ArrayEdit { element, .. } = value
                && matches!(&**element, Value::Double(_) | Value::TimeCode(_))
                && (attribute.type_name == "timecode[]") != matches!(&**element, Value::TimeCode(_))
            {
                return Err(WriteError::TypeMismatch {
                    path: location,
                    type_name: attribute.type_name.clone(),
                });
            }
        }
    }
    for child in &body.children {
        validate_text_prim(child, path)?;
    }
    for set in &body.variant_sets {
        for variant in &set.variants {
            let branch = alloc::format!("{path}{{{}={}}}", set.name, variant.name);
            validate_text_body(variant, &branch)?;
        }
    }
    Ok(())
}

/// Validates metadata keys and values. A list op is a statement of the
/// metadata of a layer, prim or property (OpenUSD writes one for metadata
/// authored so, `Sdf_FileIOUtility::WriteListOp`), not a value.
fn validate_metadata<'a>(
    entries: &'a [Metadatum],
    seen: &mut Vec<&'a str>,
    path: &str,
) -> Result<(), WriteError> {
    for entry in entries {
        if matches!(entry.value, Value::Block | Value::AnimationBlock) {
            return Err(WriteError::MisplacedBlock {
                path: alloc::format!("{path}#{}", entry.key),
            });
        }
        if entry.value.list_op_is_valid() == Some(false) {
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
        let permission = entry.key == "permission"
            && matches!(&entry.value, Value::Token(v) | Value::String(v) if PERMISSIONS.contains(&v.as_str()));
        if RESERVED_METADATA.contains(&entry.key.as_str())
            || (entry.key == "permission" && !permission)
        {
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
        Value::String(text) | Value::Token(text) | Value::PathExpression(text) => {
            bad_text(text).map_or(Ok(()), Err)
        }
        Value::StringArray(texts)
        | Value::TokenArray(texts)
        | Value::PathExpressionArray(texts) => texts.iter().find_map(bad_text).map_or(Ok(()), Err),
        Value::ArrayEdit {
            element,
            instructions,
        } => {
            let shape = element
                .shape()
                .filter(|shape| {
                    !shape.array && !matches!(shape.elem, Elem::Dictionary | Elem::ListOp)
                })
                .ok_or_else(|| WriteError::TypeMismatch {
                    path: path.into(),
                    type_name: "array edit".into(),
                })?;
            let literal = |value: &Value| {
                if value.shape() != Some(shape) {
                    return Err(WriteError::TypeMismatch {
                        path: path.into(),
                        type_name: element.canonical_type_name().into(),
                    });
                }
                validate_value(value, path)
            };
            for op in instructions {
                match op {
                    Instruction::Write {
                        src: Operand::Literal(value),
                        ..
                    }
                    | Instruction::Insert {
                        src: Operand::Literal(value),
                        ..
                    }
                    | Instruction::MinSizeFill { fill: value, .. }
                    | Instruction::ResizeFill { fill: value, .. } => literal(value)?,
                    _ => {}
                }
            }
            Ok(())
        }
        Value::Asset(asset) => bad_asset(asset).map_or(Ok(()), Err),
        Value::AssetArray(assets) => assets.iter().find_map(bad_asset).map_or(Ok(()), Err),
        Value::Dictionary(entries) => {
            for (key, v) in entries {
                if let Some(error) = bad_text(key) {
                    return Err(error);
                }
                if v.list_op_is_valid().is_some() {
                    return Err(WriteError::InvalidListOp {
                        path: alloc::format!("{path}#{key}"),
                    });
                }
                if matches!(v, Value::Block | Value::AnimationBlock) {
                    return Err(WriteError::MisplacedBlock {
                        path: alloc::format!("{path}#{key}"),
                    });
                }
                validate_value(v, path)?;
            }
            Ok(())
        }
        Value::TokenListOp(op) | Value::StringListOp(op) => {
            op.items().find_map(bad_text).map_or(Ok(()), Err)
        }
        _ => Ok(()),
    }
}

// ── Type table ──────────────────────────────────────────────────────────

/// Element storage of a value type, for type/value agreement checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Elem {
    Bool,
    UChar,
    Int,
    UInt,
    Int64,
    UInt64,
    Half,
    Float,
    Double,
    String,
    Token,
    Asset,
    PathExpression,
    /// A half-precision quaternion (`quath`).
    Quath,
    /// A single-precision quaternion (`quatf`).
    Quatf,
    /// A double-precision quaternion (`quatd`).
    Quatd,
    /// A square `double` matrix; the arity is its dimension.
    Matrix,
    Dictionary,
    ListOp,
    /// `opaque` and `group`: types that hold no value, only a declaration.
    Valueless,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shape {
    elem: Elem,
    arity: u8,
    array: bool,
}

/// Maps a USD attribute value type name to its storage shape, through the
/// value type table ([`ValueType::parse`]): a registered type, optionally
/// with `[]`. A role shares its storage type's shape (`color3f` is a
/// `float3`). `opaque` and `group` hold no value (`SdfOpaqueValue`), so they
/// are declarations only and have no array form. `dictionary` is metadata
/// only, never an attribute type.
///
/// Spec: AOUSD Core §6.2 (scalar types), §6.3 (dimensioned types), §6.5
/// (semantic roles).
fn parse_type_name(type_name: &str) -> Option<Shape> {
    let (ty, array) = ValueType::parse(type_name)?;
    let (elem, arity) = match ty.storage() {
        ValueType::Bool => (Elem::Bool, 1),
        ValueType::UChar => (Elem::UChar, 1),
        ValueType::Int => (Elem::Int, 1),
        ValueType::UInt => (Elem::UInt, 1),
        ValueType::Int64 => (Elem::Int64, 1),
        ValueType::UInt64 => (Elem::UInt64, 1),
        ValueType::Half => (Elem::Half, 1),
        ValueType::Float => (Elem::Float, 1),
        ValueType::Double | ValueType::TimeCode | ValueType::Duration => (Elem::Double, 1),
        ValueType::String => (Elem::String, 1),
        ValueType::Token => (Elem::Token, 1),
        ValueType::Asset => (Elem::Asset, 1),
        ValueType::PathExpression => (Elem::PathExpression, 1),
        ValueType::Int2 => (Elem::Int, 2),
        ValueType::Int3 => (Elem::Int, 3),
        ValueType::Int4 => (Elem::Int, 4),
        ValueType::Half2 => (Elem::Half, 2),
        ValueType::Half3 => (Elem::Half, 3),
        ValueType::Half4 => (Elem::Half, 4),
        ValueType::Float2 => (Elem::Float, 2),
        ValueType::Float3 => (Elem::Float, 3),
        ValueType::Float4 => (Elem::Float, 4),
        ValueType::Double2 => (Elem::Double, 2),
        ValueType::Double3 => (Elem::Double, 3),
        ValueType::Double4 => (Elem::Double, 4),
        ValueType::Quath => (Elem::Quath, 1),
        ValueType::Quatf => (Elem::Quatf, 1),
        ValueType::Quatd => (Elem::Quatd, 1),
        ValueType::Matrix2d => (Elem::Matrix, 2),
        ValueType::Matrix3d => (Elem::Matrix, 3),
        ValueType::Matrix4d => (Elem::Matrix, 4),
        ValueType::Opaque => (Elem::Valueless, 1),
        // `storage` is a type without a role.
        _ => return None,
    };
    Some(Shape { elem, arity, array })
}

impl Value {
    /// The value as USDA writes it (`1.5`, `"text"`, `[1, 2]`), as OpenUSD's
    /// text parser records an unregistered metadata value: a `bool` as the
    /// `true` or `false` it read (`Sdf_ParserValueContext::AppendValue`),
    /// where an attribute value is written `1` or `0`.
    #[must_use]
    pub fn to_usda_text(&self) -> String {
        let spelling = |v: bool| if v { "true" } else { "false" };
        let mut out = String::new();
        let mut writer = Writer { out: &mut out };
        match self {
            Self::Bool(v) => writer.out.push_str(spelling(*v)),
            Self::BoolArray(items) => writer.array(items, |w, v| w.out.push_str(spelling(*v))),
            _ => writer.value(self, 0),
        }
        out
    }

    /// An empty array of the declared array type `type_name` (such as
    /// `point3f[]`), or `None` when the name is not an array type this
    /// writer has a [`Value`] for.
    pub(crate) fn empty_array_of(type_name: &str) -> Option<Self> {
        if type_name == "timecode[]" {
            return Some(Self::TimeCodeArray(Vec::new()));
        }
        if type_name == "duration[]" {
            return Some(Self::DurationArray(Vec::new()));
        }
        let shape = parse_type_name(type_name).filter(|shape| shape.array)?;
        Some(match (shape.elem, shape.arity) {
            (Elem::Bool, 1) => Self::BoolArray(Vec::new()),
            (Elem::UChar, 1) => Self::UCharArray(Vec::new()),
            (Elem::Int, 1) => Self::IntArray(Vec::new()),
            (Elem::UInt, 1) => Self::UIntArray(Vec::new()),
            (Elem::Int64, 1) => Self::Int64Array(Vec::new()),
            (Elem::UInt64, 1) => Self::UInt64Array(Vec::new()),
            (Elem::Half, 1) => Self::HalfArray(Vec::new()),
            (Elem::Half, 2) => Self::Half2Array(Vec::new()),
            (Elem::Half, 3) => Self::Half3Array(Vec::new()),
            (Elem::Half, 4) => Self::Half4Array(Vec::new()),
            (Elem::Quath, 1) => Self::QuathArray(Vec::new()),
            (Elem::Quatf, 1) => Self::QuatfArray(Vec::new()),
            (Elem::Quatd, 1) => Self::QuatdArray(Vec::new()),
            (Elem::Matrix, 2) => Self::Matrix2dArray(Vec::new()),
            (Elem::Matrix, 3) => Self::Matrix3dArray(Vec::new()),
            (Elem::Matrix, 4) => Self::Matrix4dArray(Vec::new()),
            (Elem::Float, 1) => Self::FloatArray(Vec::new()),
            (Elem::Double, 1) => Self::DoubleArray(Vec::new()),
            (Elem::String, 1) => Self::StringArray(Vec::new()),
            (Elem::Token, 1) => Self::TokenArray(Vec::new()),
            (Elem::Asset, 1) => Self::AssetArray(Vec::new()),
            (Elem::PathExpression, 1) => Self::PathExpressionArray(Vec::new()),
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

    /// Builds a homogeneous array from a scalar descriptor and elements.
    ///
    /// Returns `None` for unsupported descriptors or any mismatched element.
    /// The descriptor's value is ignored, so an empty array keeps its type.
    /// Semantic aliases belong to the attribute declaration.
    #[must_use]
    pub fn array_of(element: &Self, values: impl IntoIterator<Item = Self>) -> Option<Self> {
        let mut array =
            Self::empty_array_of(&alloc::format!("{}[]", element.canonical_type_name()))?;
        for value in values {
            if !crate::save::push_element(&mut array, value) {
                return None;
            }
        }
        Some(array)
    }

    /// For a list-op value, whether it is well formed (see [`ListOp`]);
    /// `None` for any other value.
    fn list_op_is_valid(&self) -> Option<bool> {
        Some(match self {
            Self::TokenListOp(op) | Self::StringListOp(op) => op.is_valid(),
            Self::IntListOp(op) => op.is_valid(),
            Self::UIntListOp(op) => op.is_valid(),
            Self::Int64ListOp(op) => op.is_valid(),
            Self::UInt64ListOp(op) => op.is_valid(),
            _ => return None,
        })
    }

    fn shape(&self) -> Option<Shape> {
        if let Self::ArrayEdit { element, .. } = self {
            let mut shape = element.shape().filter(|shape| !shape.array)?;
            shape.array = true;
            return Some(shape);
        }
        let (elem, arity, array) = match self {
            Self::Bool(_) => (Elem::Bool, 1, false),
            Self::UChar(_) => (Elem::UChar, 1, false),
            Self::Int(_) => (Elem::Int, 1, false),
            Self::UInt(_) => (Elem::UInt, 1, false),
            Self::Int64(_) => (Elem::Int64, 1, false),
            Self::UInt64(_) => (Elem::UInt64, 1, false),
            Self::Half(_) => (Elem::Half, 1, false),
            Self::Float(_) => (Elem::Float, 1, false),
            Self::Double(_) | Self::TimeCode(_) | Self::Duration(_) => (Elem::Double, 1, false),
            Self::String(_) => (Elem::String, 1, false),
            Self::Token(_) => (Elem::Token, 1, false),
            Self::Asset(_) => (Elem::Asset, 1, false),
            Self::PathExpression(_) => (Elem::PathExpression, 1, false),
            Self::PathExpressionArray(_) => (Elem::PathExpression, 1, true),
            Self::Float2(_) => (Elem::Float, 2, false),
            Self::Float3(_) => (Elem::Float, 3, false),
            Self::Float4(_) => (Elem::Float, 4, false),
            Self::Double2(_) => (Elem::Double, 2, false),
            Self::Double3(_) => (Elem::Double, 3, false),
            Self::Double4(_) => (Elem::Double, 4, false),
            Self::Int2(_) => (Elem::Int, 2, false),
            Self::Int3(_) => (Elem::Int, 3, false),
            Self::Int4(_) => (Elem::Int, 4, false),
            Self::Half2(_) => (Elem::Half, 2, false),
            Self::Half3(_) => (Elem::Half, 3, false),
            Self::Half4(_) => (Elem::Half, 4, false),
            Self::Quath(_) => (Elem::Quath, 1, false),
            Self::Quatf(_) => (Elem::Quatf, 1, false),
            Self::Quatd(_) => (Elem::Quatd, 1, false),
            Self::Matrix2d(_) => (Elem::Matrix, 2, false),
            Self::Matrix3d(_) => (Elem::Matrix, 3, false),
            Self::Matrix4d(_) => (Elem::Matrix, 4, false),
            Self::BoolArray(_) => (Elem::Bool, 1, true),
            Self::UCharArray(_) => (Elem::UChar, 1, true),
            Self::IntArray(_) => (Elem::Int, 1, true),
            Self::UIntArray(_) => (Elem::UInt, 1, true),
            Self::Int64Array(_) => (Elem::Int64, 1, true),
            Self::UInt64Array(_) => (Elem::UInt64, 1, true),
            Self::HalfArray(_) => (Elem::Half, 1, true),
            Self::FloatArray(_) => (Elem::Float, 1, true),
            Self::DoubleArray(_) | Self::TimeCodeArray(_) | Self::DurationArray(_) => {
                (Elem::Double, 1, true)
            }
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
            Self::Half2Array(_) => (Elem::Half, 2, true),
            Self::Half3Array(_) => (Elem::Half, 3, true),
            Self::Half4Array(_) => (Elem::Half, 4, true),
            Self::QuathArray(_) => (Elem::Quath, 1, true),
            Self::QuatfArray(_) => (Elem::Quatf, 1, true),
            Self::QuatdArray(_) => (Elem::Quatd, 1, true),
            Self::Matrix2dArray(_) => (Elem::Matrix, 2, true),
            Self::Matrix3dArray(_) => (Elem::Matrix, 3, true),
            Self::Matrix4dArray(_) => (Elem::Matrix, 4, true),
            Self::Dictionary(_) => (Elem::Dictionary, 1, false),
            Self::TokenListOp(_)
            | Self::StringListOp(_)
            | Self::IntListOp(_)
            | Self::UIntListOp(_)
            | Self::Int64ListOp(_)
            | Self::UInt64ListOp(_) => (Elem::ListOp, 1, false),
            Self::Block | Self::AnimationBlock | Self::ArrayEdit { .. } => return None,
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
            Self::UChar(_) => "uchar",
            Self::Int(_) => "int",
            Self::UInt(_) => "uint",
            Self::Int64(_) => "int64",
            Self::UInt64(_) => "uint64",
            Self::Half(_) => "half",
            Self::Float(_) => "float",
            Self::Double(_) => "double",
            Self::TimeCode(_) => "timecode",
            Self::Duration(_) => "duration",
            Self::String(_) => "string",
            Self::Token(_) => "token",
            Self::Asset(_) => "asset",
            Self::PathExpression(_) => "pathExpression",
            Self::PathExpressionArray(_) => "pathExpression[]",
            Self::Float2(_) => "float2",
            Self::Float3(_) => "float3",
            Self::Float4(_) => "float4",
            Self::Double2(_) => "double2",
            Self::Double3(_) => "double3",
            Self::Double4(_) => "double4",
            Self::Int2(_) => "int2",
            Self::Int3(_) => "int3",
            Self::Int4(_) => "int4",
            Self::Half2(_) => "half2",
            Self::Half3(_) => "half3",
            Self::Half4(_) => "half4",
            Self::Quath(_) => "quath",
            Self::Quatf(_) => "quatf",
            Self::Quatd(_) => "quatd",
            Self::Matrix2d(_) => "matrix2d",
            Self::Matrix3d(_) => "matrix3d",
            Self::Matrix4d(_) => "matrix4d",
            Self::BoolArray(_) => "bool[]",
            Self::UCharArray(_) => "uchar[]",
            Self::IntArray(_) => "int[]",
            Self::UIntArray(_) => "uint[]",
            Self::Int64Array(_) => "int64[]",
            Self::UInt64Array(_) => "uint64[]",
            Self::HalfArray(_) => "half[]",
            Self::FloatArray(_) => "float[]",
            Self::DoubleArray(_) => "double[]",
            Self::TimeCodeArray(_) => "timecode[]",
            Self::DurationArray(_) => "duration[]",
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
            Self::Half2Array(_) => "half2[]",
            Self::Half3Array(_) => "half3[]",
            Self::Half4Array(_) => "half4[]",
            Self::QuathArray(_) => "quath[]",
            Self::QuatfArray(_) => "quatf[]",
            Self::QuatdArray(_) => "quatd[]",
            Self::Matrix2dArray(_) => "matrix2d[]",
            Self::Matrix3dArray(_) => "matrix3d[]",
            Self::Matrix4dArray(_) => "matrix4d[]",
            Self::Dictionary(_) => "dictionary",
            Self::TokenListOp(_) => "tokenListOp",
            Self::StringListOp(_) => "stringListOp",
            Self::IntListOp(_) => "intListOp",
            Self::UIntListOp(_) => "uintListOp",
            Self::Int64ListOp(_) => "int64ListOp",
            Self::UInt64ListOp(_) => "uint64ListOp",
            Self::ArrayEdit { element, .. } => {
                Self::empty_array_of(&alloc::format!("{}[]", element.canonical_type_name()))
                    .map_or("opaque", |array| array.canonical_type_name())
            }
            Self::Block => "SdfValueBlock",
            Self::AnimationBlock => "SdfAnimationBlock",
        }
    }
}

// ── Text emission ───────────────────────────────────────────────────────

/// Minimum USDA version for authored spline features.
fn spline_text_version(prim: &Prim) -> u8 {
    let own = prim
        .properties
        .iter()
        .filter_map(|p| match p {
            Property::Attribute(a) => a.spline.as_deref(),
            _ => None,
        })
        .map(|s| {
            if s.pre_loop_boundary.is_some()
                || s.post_loop_boundary.is_some()
                || s.data_type == layerstack::spline::SplineDataType::TimeCode
            {
                3
            } else if s.knots.iter().any(|k| {
                k.pre_tan_algorithm != layerstack::spline::TangentAlgorithm::None
                    || k.post_tan_algorithm != layerstack::spline::TangentAlgorithm::None
            }) {
                1
            } else {
                0
            }
        })
        .max()
        .unwrap_or(0);
    prim.children
        .iter()
        .chain(prim.variant_sets.iter().flat_map(|v| &v.variants))
        .map(spline_text_version)
        .fold(own, u8::max)
}

/// USDA 1.4 for a document that holds a `duration` value or declares a
/// `duration` attribute, as OpenUSD 26.11 requests that version for a
/// `GfDuration`, `VtArray<GfDuration>` or `VtArrayEdit<GfDuration>`
/// (`_StringFromVtValueHelper`, `pxr/usd/sdf/fileIO_Common.cpp`); 0
/// otherwise.
fn duration_text_version(doc: &Document) -> u8 {
    fn in_value(value: &Value) -> bool {
        match value {
            Value::Duration(_) | Value::DurationArray(_) => true,
            Value::ArrayEdit { element, .. } => matches!(**element, Value::Duration(_)),
            Value::Dictionary(entries) => entries.iter().any(|(_, v)| in_value(v)),
            _ => false,
        }
    }
    fn metadata(metadata: &[Metadatum]) -> bool {
        metadata.iter().any(|m| in_value(&m.value))
    }
    fn references(list: Option<&ListOp<Reference>>) -> bool {
        list.is_some_and(|list| {
            [
                list.explicit.as_deref().unwrap_or_default(),
                &list.deleted,
                &list.added,
                &list.prepended,
                &list.appended,
                &list.reordered,
            ]
            .iter()
            .any(|items| {
                items
                    .iter()
                    .any(|r| r.custom_data.iter().any(|(_, v)| in_value(v)))
            })
        })
    }
    fn in_prim(prim: &Prim) -> bool {
        metadata(&prim.metadata)
            || references(prim.references.as_ref())
            || references(prim.payloads.as_ref())
            || prim.properties.iter().any(|p| match p {
                Property::Attribute(a) => {
                    matches!(a.type_name.as_str(), "duration" | "duration[]")
                        || a.value.as_ref().is_some_and(in_value)
                        || a.time_samples.iter().flatten().any(|(_, v)| in_value(v))
                        || a.spline_custom_data
                            .iter()
                            .any(|(_, entries)| entries.iter().any(|(_, v)| in_value(v)))
                        || metadata(&a.metadata)
                }
                Property::Relationship(r) => metadata(&r.metadata),
            })
            || prim
                .children
                .iter()
                .chain(prim.variant_sets.iter().flat_map(|v| &v.variants))
                .any(in_prim)
    }
    if metadata(&doc.metadata) || doc.prims.iter().any(in_prim) {
        4
    } else {
        0
    }
}

/// `items` in the order `cmp` gives, by reference. Items already in order,
/// as a layer read from OpenUSD's text holds them, cost one comparison
/// each and nothing else; others are sorted as references, on the heap so
/// that a recursive caller's frame stays small. Equal items keep their
/// order, as a stable sort keeps them.
fn sorted_by<'a, T>(items: &'a [T], mut cmp: impl FnMut(&T, &T) -> Ordering) -> InOrder<'a, T> {
    if items.is_sorted_by(|a, b| cmp(a, b) != Ordering::Greater) {
        return InOrder::Given(items.iter());
    }
    let mut refs: Vec<&T> = items.iter().collect();
    // Ties go by place in `items`: the unstable sort needs no buffer.
    refs.sort_unstable_by(|a, b| cmp(a, b).then_with(|| ptr::from_ref(*a).cmp(&ptr::from_ref(*b))));
    InOrder::Sorted(refs.into_iter())
}

/// What properties are sorted by: the name, then whether the property is
/// a relationship, as `_SortByNameThenType` puts an attribute before a
/// relationship of the same name (which validation does not let a
/// document hold).
type PropertyKey<'a> = (&'a str, bool);

fn property_key(property: &Property) -> PropertyKey<'_> {
    (
        property.name(),
        matches!(property, Property::Relationship(_)),
    )
}

/// The order of [`PropertyKey`]s: names in [`dictionary_cmp`] order.
fn property_order(a: &PropertyKey<'_>, b: &PropertyKey<'_>) -> Ordering {
    dictionary_cmp(a.0, b.0).then(a.1.cmp(&b.1))
}

/// The items [`sorted_by`] gives.
enum InOrder<'a, T> {
    Given(core::slice::Iter<'a, T>),
    Sorted(alloc::vec::IntoIter<&'a T>),
}

impl<'a, T> Iterator for InOrder<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        match self {
            Self::Given(items) => items.next(),
            Self::Sorted(items) => items.next(),
        }
    }
}

/// The field a metadata key names, which decides where OpenUSD writes it:
/// USDA spells the `documentation` field `doc`.
fn field_name(key: &str) -> &str {
    match key {
        "doc" => "documentation",
        key => key,
    }
}

/// A field of a prim's or variant's metadata section, ordered as
/// `Sdf_WritePrimMetadata` sorts them: by field name, in
/// [`dictionary_cmp`] order.
#[derive(Clone, Copy)]
enum PrimField<'a> {
    Metadatum(&'a Metadatum),
    Inherits(&'a ListOp<String>),
    Payloads(&'a ListOp<Reference>),
    References(&'a ListOp<Reference>),
    Specializes(&'a ListOp<String>),
    VariantSelections,
    VariantSetNames(&'a ListOp<String>),
}

impl PrimField<'_> {
    /// The field name (`SdfFieldKeys`) the entry is sorted by.
    fn name(&self) -> &str {
        match self {
            Self::Metadatum(entry) => field_name(&entry.key),
            Self::Inherits(_) => "inheritPaths",
            Self::Payloads(_) => "payload",
            Self::References(_) => "references",
            Self::Specializes(_) => "specializes",
            Self::VariantSelections => "variantSelection",
            Self::VariantSetNames(_) => "variantSetNames",
        }
    }
}

/// A field of the layer's metadata section, ordered as `_WriteLayer`
/// (`pxr/usd/sdf/usdaFileFormat.cpp`) sorts them: by field name, in byte
/// order (`TfToken::operator<`).
#[derive(Clone, Copy)]
enum LayerField<'a> {
    DefaultPrim(&'a str),
    Metadatum(&'a Metadatum),
    SubLayers(&'a [SubLayer]),
}

impl LayerField<'_> {
    fn name(&self) -> &str {
        match self {
            Self::DefaultPrim(_) => "defaultPrim",
            Self::Metadatum(entry) => field_name(&entry.key),
            Self::SubLayers(_) => "subLayers",
        }
    }
}

/// The `comment` entry of a metadata list, which OpenUSD writes first, as
/// a bare string.
fn comment_of(entries: &[Metadatum]) -> Option<&str> {
    entries
        .iter()
        .find_map(|entry| match (&*entry.key, &entry.value) {
            ("comment", Value::String(text) | Value::Token(text)) => Some(text.as_str()),
            _ => None,
        })
}

/// Infallible text sink over a validated document.
struct Writer<'o> {
    out: &'o mut String,
}

impl Writer<'_> {
    /// The layer as `_WriteLayer` (`pxr/usd/sdf/usdaFileFormat.cpp`)
    /// writes it: the header with the comment first, the other fields
    /// sorted by name, then relocates; `reorder rootPrims`; the root prims
    /// in document order.
    fn document(&mut self, doc: &Document) {
        // §16.2.18.1: the layer header.
        let minor = doc
            .prims
            .iter()
            .map(spline_text_version)
            .max()
            .unwrap_or(0)
            .max(duration_text_version(doc));
        self.out.push_str(&alloc::format!("#usda 1.{minor}\n"));
        if doc.default_prim.is_some()
            || !doc.metadata.is_empty()
            || !doc.sublayers.is_empty()
            || !doc.relocates.is_empty()
        {
            self.out.push_str("(\n");
            if let Some(text) = comment_of(&doc.metadata) {
                self.comment(text, 1);
            }
            let mut fields: SmallVec<[LayerField<'_>; 16]> = doc
                .metadata
                .iter()
                .filter(|entry| entry.key != "comment")
                .map(LayerField::Metadatum)
                .collect();
            fields.extend(doc.default_prim.as_deref().map(LayerField::DefaultPrim));
            if !doc.sublayers.is_empty() {
                fields.push(LayerField::SubLayers(&doc.sublayers));
            }
            fields.sort_unstable_by(|a, b| a.name().cmp(b.name()));
            for field in fields {
                match field {
                    LayerField::DefaultPrim(name) => {
                        self.out.push_str(INDENT);
                        self.out.push_str("defaultPrim = ");
                        self.string(name);
                        self.out.push('\n');
                    }
                    LayerField::Metadatum(entry) => self.metadatum(entry, 1),
                    LayerField::SubLayers(sublayers) => {
                        // §16.2.18.3: `subLayers = [ @asset@ (offset = ...), ... ]`.
                        self.out.push_str(INDENT);
                        self.out.push_str("subLayers = [\n");
                        for (i, sublayer) in sublayers.iter().enumerate() {
                            self.indent(2);
                            self.asset(&sublayer.asset);
                            self.layer_offset(sublayer.offset);
                            if i + 1 < sublayers.len() {
                                self.out.push(',');
                            }
                            self.out.push('\n');
                        }
                        self.out.push_str(INDENT);
                        self.out.push_str("]\n");
                    }
                }
            }
            if !doc.relocates.is_empty() {
                self.out.push_str("    relocates = {\n");
                for (i, (source, target)) in doc.relocates.iter().enumerate() {
                    self.indent(2);
                    self.path(source);
                    self.out.push_str(": ");
                    self.path(target);
                    if i + 1 < doc.relocates.len() {
                        self.out.push(',');
                    }
                    self.out.push('\n');
                }
                self.out.push_str("    }\n");
            }
            self.out.push_str(")\n");
        }
        if let Some(order) = &doc.prim_order
            && order.len() > 1
        {
            self.out.push('\n');
            self.reorder("rootPrims", order, 0);
        }
        for prim in &doc.prims {
            self.out.push('\n');
            self.prim(prim, 0);
        }
        self.out.push('\n');
    }

    fn indent(&mut self, depth: usize) {
        for _ in 0..depth {
            self.out.push_str(INDENT);
        }
    }

    /// The metadata section of a property: the comment first, then the
    /// other entries sorted by field name in [`dictionary_cmp`] order, as
    /// `Sdf_WriteAttribute` and `Sdf_WriteRelationship` write them.
    fn metadata_entries(&mut self, entries: &[Metadatum], depth: usize) {
        if let Some(text) = comment_of(entries) {
            self.comment(text, depth);
        }
        let sorted = sorted_by(entries, |a, b| {
            dictionary_cmp(field_name(&a.key), field_name(&b.key))
        });
        for entry in sorted {
            if entry.key != "comment" {
                self.metadatum(entry, depth);
            }
        }
    }

    /// §16.2.15: a bare string is the `comment` field.
    fn comment(&mut self, text: &str, depth: usize) {
        self.indent(depth);
        self.string(text);
        self.out.push('\n');
    }

    /// One `key = value` entry, or one statement per operation of a list
    /// op.
    fn metadatum(&mut self, entry: &Metadatum, depth: usize) {
        let key = entry.key.as_str();
        match &entry.value {
            Value::TokenListOp(op) | Value::StringListOp(op) => {
                return self.list_op(key, op, depth, |w, s| w.string(s));
            }
            Value::IntListOp(op) => return self.list_op(key, op, depth, |w, x| w.display(x)),
            Value::UIntListOp(op) => return self.list_op(key, op, depth, |w, x| w.display(x)),
            Value::Int64ListOp(op) => return self.list_op(key, op, depth, |w, x| w.display(x)),
            Value::UInt64ListOp(op) => return self.list_op(key, op, depth, |w, x| w.display(x)),
            _ => {}
        }
        self.indent(depth);
        self.out.push_str(key);
        self.out.push_str(" = ");
        match (key, &entry.value) {
            ("permission", Value::Token(v) | Value::String(v)) => self.out.push_str(v),
            (_, Value::Bool(v)) => self.out.push_str(if *v { "true" } else { "false" }),
            _ => self.value(&entry.value, depth),
        }
        self.out.push('\n');
    }

    /// §16.2.14: one `[op] key = [items]` statement per operation, in
    /// OpenUSD's order; an explicit empty list is `key = None`, as
    /// OpenUSD writes it (`_WriteListOpList`, `pxr/usd/sdf/fileIO_Common.cpp`).
    /// Validation guarantees the op is non-empty and not mixed.
    fn list_op<T>(
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
            if items.is_empty() && keyword.is_empty() {
                w.out.push_str("None");
            } else {
                w.array(items, &mut item);
            }
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
        self.prim_metadata(prim, depth);
        self.out.push('\n');
        self.indent(depth);
        self.out.push_str("{\n");
        self.prim_body(prim, depth + 1);
        self.indent(depth);
        self.out.push_str("}\n");
    }

    /// ` ( metadata )` after a prim or variant name, when there is any, as
    /// `Sdf_WritePrimMetadata` writes it: the comment first, then every
    /// other field — metadata, arcs, variant selections and the
    /// `variantSets` list op — sorted by field name in [`dictionary_cmp`]
    /// order (`doc` as `documentation`, `inherits` as `inheritPaths`,
    /// `variants` as `variantSelection`, `variantSets` as
    /// `variantSetNames`).
    fn prim_metadata(&mut self, prim: &Prim, depth: usize) {
        if prim.has_metadata() {
            self.prim_fields(prim, depth);
        }
    }

    /// [`Self::prim_metadata`]'s section, out of line so that the fields
    /// it sorts do not enlarge the frame of [`Self::prim`], which recurses
    /// through child prims.
    #[inline(never)]
    fn prim_fields(&mut self, prim: &Prim, depth: usize) {
        self.out.push_str(" (\n");
        let depth = depth + 1;
        if let Some(text) = comment_of(&prim.metadata) {
            self.comment(text, depth);
        }
        let mut fields: SmallVec<[PrimField<'_>; 16]> = prim
            .metadata
            .iter()
            .filter(|entry| entry.key != "comment")
            .map(PrimField::Metadatum)
            .collect();
        fields.extend(prim.inherits.as_ref().map(PrimField::Inherits));
        fields.extend(prim.payloads.as_ref().map(PrimField::Payloads));
        fields.extend(prim.references.as_ref().map(PrimField::References));
        fields.extend(prim.specializes.as_ref().map(PrimField::Specializes));
        if !prim.variant_selections.is_empty() {
            fields.push(PrimField::VariantSelections);
        }
        fields.extend(
            prim.variant_set_names
                .as_ref()
                .map(PrimField::VariantSetNames),
        );
        fields.sort_unstable_by(|a, b| dictionary_cmp(a.name(), b.name()));
        for field in fields {
            match field {
                PrimField::Metadatum(entry) => self.metadatum(entry, depth),
                // §16.2.17.4, §16.2.17.5: arcs as list-op statements.
                PrimField::Inherits(op) => {
                    self.arc_statements(
                        "inherits",
                        op,
                        depth,
                        |_| false,
                        |w, path, _| {
                            w.path(path);
                        },
                    );
                }
                PrimField::Payloads(op) => {
                    self.arc_statements(
                        "payload",
                        op,
                        depth,
                        Reference::bracketed,
                        Self::reference,
                    );
                }
                PrimField::References(op) => {
                    self.arc_statements(
                        "references",
                        op,
                        depth,
                        Reference::bracketed,
                        Self::reference,
                    );
                }
                PrimField::Specializes(op) => {
                    self.arc_statements(
                        "specializes",
                        op,
                        depth,
                        |_| false,
                        |w, path, _| {
                            w.path(path);
                        },
                    );
                }
                PrimField::VariantSelections => {
                    // §16.2.17.3: `variants = { string set = "variant" }`, a
                    // dictionary of strings.
                    let selections = prim
                        .variant_selections
                        .iter()
                        .map(|(set, variant)| (set.clone(), Value::String(variant.clone())))
                        .collect::<Vec<_>>();
                    self.indent(depth);
                    self.out.push_str("variants = ");
                    self.dictionary(&selections, depth);
                    self.out.push('\n');
                }
                PrimField::VariantSetNames(op) => {
                    // `WriteNameVector`: one name without brackets.
                    self.list_op_statements(op, depth, |w, keyword, names| {
                        w.out.push_str(keyword);
                        w.out.push_str("variantSets = ");
                        w.names(names);
                    });
                }
            }
        }
        self.indent(depth - 1);
        self.out.push(')');
    }

    /// The statements of a prim or variant body at `depth`, as
    /// `Sdf_WritePrimBody` orders and spaces them: `reorder` statements;
    /// properties sorted by name in [`dictionary_cmp`] order, an attribute
    /// before a relationship of the same name (`_SortByNameThenType`);
    /// child prims in document order (the `primChildren` field); then
    /// variant sets in document order (`variantSetChildren`).
    fn prim_body(&mut self, prim: &Prim, depth: usize) {
        if let Some(order) = &prim.property_order {
            self.reorder("properties", order, depth);
        }
        if let Some(order) = &prim.prim_order {
            self.reorder("nameChildren", order, depth);
        }
        let in_order = prim.properties.is_sorted_by(|a, b| {
            property_order(&property_key(a), &property_key(b)) != Ordering::Greater
        });
        if in_order {
            for property in &prim.properties {
                self.property(property, depth);
            }
        } else {
            self.sorted_properties(&prim.properties, depth);
        }
        // A blank line between the properties and the children and
        // between children; none before or between variant sets.
        if !prim.properties.is_empty() && !prim.children.is_empty() {
            self.out.push('\n');
        }
        for (i, child) in prim.children.iter().enumerate() {
            if i > 0 {
                self.out.push('\n');
            }
            self.prim(child, depth);
        }
        for set in &prim.variant_sets {
            self.variant_set(set, depth);
        }
    }

    /// Writes `properties`, which are out of order, sorted. Out of line,
    /// so that the keys it sorts do not enlarge the frame of
    /// [`Self::prim_body`], which recurses through child prims.
    #[inline(never)]
    fn sorted_properties(&mut self, properties: &[Property], depth: usize) {
        let mut keyed: SmallVec<[(PropertyKey<'_>, &Property); 16]> =
            properties.iter().map(|p| (property_key(p), p)).collect();
        keyed.sort_unstable_by(|a, b| property_order(&a.0, &b.0));
        for (_, property) in keyed {
            self.property(property, depth);
        }
    }

    fn property(&mut self, property: &Property, depth: usize) {
        match property {
            Property::Attribute(attribute) => self.attribute(attribute, depth),
            Property::Relationship(relationship) => self.relationship(relationship, depth),
        }
    }

    /// §16.2.17: `variantSet "name" = { "variant" [( metadata )] { body }
    /// ... }`, each variant with its prim spec's metadata and body, then a
    /// blank line (`Sdf_WriteVariant`), the variants sorted by name in byte
    /// order, as `Sdf_WriteVariantSet` sorts them.
    fn variant_set(&mut self, set: &VariantSet, depth: usize) {
        self.indent(depth);
        self.out.push_str("variantSet ");
        self.string(&set.name);
        self.out.push_str(" = {\n");
        for variant in sorted_by(&set.variants, |a, b| a.name.cmp(&b.name)) {
            self.indent(depth + 1);
            self.string(&variant.name);
            self.prim_metadata(variant, depth + 1);
            self.out.push_str(" {\n");
            self.prim_body(variant, depth + 2);
            self.out.push('\n');
            self.indent(depth + 1);
            self.out.push_str("}\n");
        }
        self.indent(depth);
        self.out.push_str("}\n");
    }

    /// `[op] key = item`, a list or `None` per list-op statement, as
    /// `_WriteListOpList` (`pxr/usd/sdf/fileIO_Common.cpp`) writes a list of
    /// paths, references or payloads: one item bare unless `bracketed`
    /// says it needs brackets, otherwise one item per line. `item` writes
    /// an item at the depth it is given.
    fn arc_statements<T>(
        &mut self,
        key: &str,
        op: &ListOp<T>,
        depth: usize,
        bracketed: impl Fn(&T) -> bool,
        mut item: impl FnMut(&mut Self, &T, usize),
    ) {
        self.list_op_statements(op, depth, |w, keyword, items| {
            w.out.push_str(keyword);
            w.out.push_str(key);
            w.out.push_str(" = ");
            match items {
                [] => w.out.push_str("None"),
                [one] if !bracketed(one) => item(w, one, depth),
                _ => {
                    w.out.push_str("[\n");
                    for (i, one) in items.iter().enumerate() {
                        w.indent(depth + 1);
                        item(w, one, depth + 1);
                        w.out
                            .push_str(if i + 1 < items.len() { ",\n" } else { "\n" });
                    }
                    w.indent(depth);
                    w.out.push(']');
                }
            }
        });
    }

    /// `@asset@<path> (offset = ...; scale = ...)`, or with custom data the
    /// offset, scale and `customData` on lines of their own, as
    /// `_ListOpWriter<SdfReference>` writes them; `depth` is the item's.
    fn reference(&mut self, arc: &Reference, depth: usize) {
        if let Some(asset) = &arc.asset {
            self.asset(asset);
        }
        match (&arc.asset, &arc.prim_path) {
            (_, Some(path)) => self.path(path),
            (None, None) => self.out.push_str("<>"),
            (Some(_), None) => {}
        }
        if arc.custom_data.is_empty() {
            self.layer_offset(arc.offset);
            return;
        }
        self.out.push_str(" (\n");
        if arc.offset.offset != 0.0 {
            self.indent(depth + 1);
            self.out.push_str("offset = ");
            self.f64(arc.offset.offset);
            self.out.push('\n');
        }
        if arc.offset.scale != 1.0 {
            self.indent(depth + 1);
            self.out.push_str("scale = ");
            self.f64(arc.offset.scale);
            self.out.push('\n');
        }
        self.indent(depth + 1);
        self.out.push_str("customData = ");
        self.dictionary(&arc.custom_data, depth + 1);
        self.out.push('\n');
        self.indent(depth);
        self.out.push(')');
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
    /// `properties`, §16.2.18 for `rootPrims`). An order of fewer than two
    /// names writes nothing, as `Sdf_WritePrimNamespaceReorders` and
    /// `_WriteLayer` leave it out: it reorders nothing.
    fn reorder(&mut self, key: &str, names: &[String], depth: usize) {
        if names.len() < 2 {
            return;
        }
        self.indent(depth);
        self.out.push_str("reorder ");
        self.out.push_str(key);
        self.out.push_str(" = ");
        self.names(names);
        self.out.push('\n');
    }

    /// Names as `Sdf_FileIOUtility::WriteNameVector` writes them: one
    /// quoted name alone, several in brackets.
    fn names(&mut self, names: &[String]) {
        match names {
            [one] => self.string(one),
            _ => self.array(names, |w, s| w.string(s)),
        }
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
            || (attribute.connections.is_none()
                && attribute.time_samples.is_none()
                && attribute.spline.is_none());
        if declare {
            self.indent(depth);
            if attribute.custom {
                self.out.push_str("custom ");
            }
            self.attribute_head(attribute);
            if let Some(value) = &attribute.value {
                self.out.push_str(" = ");
                self.attribute_value(attribute, value, depth);
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
                self.attribute_value(attribute, value, depth + 1);
                self.out.push_str(",\n");
            }
            self.indent(depth);
            self.out.push_str("}\n");
        }
        if let Some(spline) = attribute.spline.as_deref() {
            self.indent(depth);
            self.attribute_head(attribute);
            self.out.push_str(".spline = {\n");
            let indent = "    ".repeat(depth + 1);
            let time = |out: &mut String, v: f64| Writer { out }.f64(v);
            let value = |out: &mut String, v: f64| {
                #[allow(clippy::cast_possible_truncation, reason = "a float spline's values")]
                match attribute.type_name.as_str() {
                    "float" => Writer { out }.f32(v as f32),
                    "half" => Writer { out }.half(layerstack::half::from_f64(v)),
                    _ => Writer { out }.f64(v),
                }
            };
            let custom = |out: &mut String, time: f64| {
                if let Some((_, entries)) = attribute
                    .spline_custom_data
                    .iter()
                    .find(|(t, _)| *t == time)
                {
                    out.push_str("; ");
                    Writer { out }.dictionary(entries, depth + 1);
                }
            };
            crate::spline_text::write(self.out, &indent, spline, &time, &value, &custom);
            self.indent(depth);
            self.out.push_str("}\n");
        }
        if let Some(connections) = &attribute.connections {
            self.list_op_statements(connections, depth, |w, keyword, targets| {
                w.out.push_str(keyword);
                w.attribute_head(attribute);
                w.out.push_str(".connect = ");
                w.targets(targets, depth);
            });
        }
    }

    /// A default or time sample of `attribute`. A document holds a
    /// `timecode` attribute's values as doubles, as the crate writer reads
    /// them; they are written as the time codes they are (§6.3).
    fn attribute_value(&mut self, attribute: &Attribute, value: &Value, depth: usize) {
        match (attribute.type_name.as_str(), value) {
            ("timecode", Value::Double(v)) => self.time_code(*v),
            ("timecode[]", Value::DoubleArray(v)) => self.array(v, |w, x| w.time_code(*x)),
            ("duration", Value::Double(v)) => self.time_code(*v),
            ("duration[]", Value::DoubleArray(v)) => self.array(v, |w, x| w.time_code(*x)),
            _ => self.value(value, depth),
        }
    }

    /// One statement per list-op operation, in OpenUSD's order: the
    /// explicit list, otherwise `delete`, `add`, `prepend`, `append` and
    /// `reorder` for each non-empty edit. `statement` writes the line after the indentation,
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
            ("add ", &op.added),
            ("prepend ", &op.prepended),
            ("append ", &op.appended),
            ("reorder ", &op.reordered),
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
                self.targets(targets, depth);
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
                w.targets(targets, depth);
            });
        }
    }

    /// `<path>` for one target, `None` for none, and for several one per
    /// line, each followed by a comma, as `Sdf_WriteRelationshipTargetList`
    /// and `Sdf_WriteConnectionStatement` write them; `depth` is the
    /// statement's. Validation guarantees each path is plain identifiers,
    /// so it needs no escaping.
    fn targets(&mut self, targets: &[String], depth: usize) {
        match targets {
            [] => self.out.push_str("None"),
            [one] => self.path(one),
            _ => {
                self.out.push_str("[\n");
                for target in targets {
                    self.indent(depth + 1);
                    self.path(target);
                    self.out.push_str(",\n");
                }
                self.indent(depth);
                self.out.push(']');
            }
        }
    }

    fn value(&mut self, value: &Value, depth: usize) {
        match value {
            Value::ArrayEdit { instructions, .. } => {
                self.out.push_str("edit [");
                for (i, op) in instructions.iter().enumerate() {
                    if i != 0 {
                        self.out.push_str("; ");
                    }
                    match op {
                        Instruction::Write { src, index } => {
                            self.out.push_str("write ");
                            self.edit_operand(src, depth);
                            self.out.push_str(" to ");
                            self.edit_index(*index);
                        }
                        Instruction::Insert {
                            src,
                            index: ArrayIndex::End,
                        } => {
                            self.out.push_str("append ");
                            self.edit_operand(src, depth);
                        }
                        Instruction::Insert { src, index } => {
                            self.out.push_str("insert ");
                            self.edit_operand(src, depth);
                            self.out.push_str(" at ");
                            self.edit_index(*index);
                        }
                        Instruction::Erase { index } => {
                            self.out.push_str("erase ");
                            self.edit_index(*index);
                        }
                        Instruction::MinSize { len } => {
                            self.out.push_str("minsize ");
                            self.display(len);
                        }
                        Instruction::MaxSize { len } => {
                            self.out.push_str("maxsize ");
                            self.display(len);
                        }
                        Instruction::Resize { len } => {
                            self.out.push_str("resize ");
                            self.display(len);
                        }
                        Instruction::MinSizeFill { len, fill }
                        | Instruction::ResizeFill { len, fill } => {
                            self.out
                                .push_str(if matches!(op, Instruction::MinSizeFill { .. }) {
                                    "minsize "
                                } else {
                                    "resize "
                                });
                            self.display(len);
                            self.out.push_str(" fill ");
                            self.value(fill, depth);
                        }
                    }
                }
                self.out.push(']');
            }
            Value::Bool(v) => self.out.push_str(if *v { "1" } else { "0" }),
            Value::UChar(v) => self.display(v),
            Value::Int(v) => self.display(v),
            Value::UInt(v) => self.display(v),
            Value::Int64(v) => self.display(v),
            Value::UInt64(v) => self.display(v),
            Value::Half(v) => self.half(*v),
            Value::Float(v) => self.f32(*v),
            Value::Double(v) => self.f64(*v),
            // OpenUSD 26.11 streams a `GfDuration` as its double
            // (`operator<<`, `pxr/base/gf/duration.cpp`), as a time code.
            Value::TimeCode(v) | Value::Duration(v) => self.time_code(*v),
            Value::String(v) | Value::Token(v) | Value::PathExpression(v) => self.string(v),
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
            Value::Half2(v) => self.tuple(v, Self::half),
            Value::Half3(v) => self.tuple(v, Self::half),
            Value::Half4(v) => self.tuple(v, Self::half),
            Value::Quath(q) => self.quat(q, Self::half),
            Value::Quatf(q) => self.quat(q, Self::f32),
            Value::Quatd(q) => self.quat(q, Self::f64),
            Value::Matrix2d(rows) => self.matrix(rows),
            Value::Matrix3d(rows) => self.matrix(rows),
            Value::Matrix4d(rows) => self.matrix(rows),
            Value::BoolArray(v) => self.array(v, |w, b| {
                w.out.push_str(if *b { "1" } else { "0" });
            }),
            Value::UCharArray(v) => self.array(v, |w, x| w.display(x)),
            Value::IntArray(v) => self.array(v, |w, x| w.display(x)),
            Value::UIntArray(v) => self.array(v, |w, x| w.display(x)),
            Value::Int64Array(v) => self.array(v, |w, x| w.display(x)),
            Value::UInt64Array(v) => self.array(v, |w, x| w.display(x)),
            Value::HalfArray(v) => self.array(v, |w, x| w.half(*x)),
            Value::FloatArray(v) => self.array(v, |w, x| w.f32(*x)),
            Value::DoubleArray(v) => self.array(v, |w, x| w.f64(*x)),
            Value::TimeCodeArray(v) | Value::DurationArray(v) => {
                self.array(v, |w, x| w.time_code(*x));
            }
            Value::StringArray(v) | Value::TokenArray(v) | Value::PathExpressionArray(v) => {
                self.array(v, |w, s| w.string(s));
            }
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
            Value::Half2Array(v) => self.array(v, |w, t| w.tuple(t, Self::half)),
            Value::Half3Array(v) => self.array(v, |w, t| w.tuple(t, Self::half)),
            Value::Half4Array(v) => self.array(v, |w, t| w.tuple(t, Self::half)),
            Value::QuathArray(v) => self.array(v, |w, &[i, j, k, r]| {
                w.tuple(&[r, i, j, k], Self::half);
            }),
            Value::QuatfArray(v) => self.array(v, |w, q| w.quat(q, Self::f32)),
            Value::QuatdArray(v) => self.array(v, |w, q| w.quat(q, Self::f64)),
            Value::Matrix2dArray(v) => self.array(v, |w, m| w.matrix(m)),
            Value::Matrix3dArray(v) => self.array(v, |w, m| w.matrix(m)),
            Value::Matrix4dArray(v) => self.array(v, |w, m| w.matrix(m)),
            // Written as statements by `metadata_entries`; validation keeps
            // list ops out of attribute values and dictionaries.
            Value::TokenListOp(_)
            | Value::StringListOp(_)
            | Value::IntListOp(_)
            | Value::UIntListOp(_)
            | Value::Int64ListOp(_)
            | Value::UInt64ListOp(_) => unreachable!("list ops are written as statements"),
            Value::Block => self.out.push_str("None"),
            Value::AnimationBlock => self.out.push_str("AnimationBlock"),
            Value::Dictionary(entries) => self.dictionary(entries, depth),
        }
    }

    /// §6.6.2, §16.2.15: `{ type key = value ... }`, typed entries with
    /// keys quoted unless they are identifiers, sorted by key in byte
    /// order, as `Sdf_FileIOUtility::WriteDictionary` writes the
    /// `std::map` it builds.
    fn dictionary(&mut self, entries: &[(String, Value)], depth: usize) {
        self.out.push_str("{\n");
        for (key, v) in sorted_by(entries, |a, b| a.0.cmp(&b.0)) {
            self.indent(depth + 1);
            self.out.push_str(v.canonical_type_name());
            self.out.push(' ');
            self.key(key);
            self.out.push_str(" = ");
            self.value(v, depth + 1);
            self.out.push('\n');
        }
        self.indent(depth);
        self.out.push('}');
    }

    fn edit_index(&mut self, index: ArrayIndex) {
        self.out.push('[');
        self.display(&match index {
            ArrayIndex::Position(i) => i,
            ArrayIndex::End => i64::MIN,
        });
        self.out.push(']');
    }

    fn edit_operand(&mut self, src: &Operand<Value>, depth: usize) {
        match src {
            Operand::Literal(value) => self.value(value, depth),
            Operand::CopyFrom(index) => self.edit_index(*index),
        }
    }

    fn display(&mut self, v: &impl fmt::Display) {
        let _ = write!(self.out, "{v}");
    }

    fn i32(&mut self, v: i32) {
        self.display(&v);
    }

    /// A `float` as OpenUSD writes it ([`float_text`]).
    fn f32(&mut self, v: f32) {
        float_text::push_float(self.out, v);
    }

    /// A `half` as OpenUSD writes it: six significant digits, which read
    /// back as the same half ([`float_text`]).
    fn half(&mut self, bits: u16) {
        float_text::push_half(self.out, bits);
    }

    /// A `double` as OpenUSD writes it ([`float_text`]).
    fn f64(&mut self, v: f64) {
        float_text::push_double(self.out, v);
    }

    /// A `timecode` as OpenUSD writes it where that reads back as the same
    /// time ([`float_text`]).
    fn time_code(&mut self, v: f64) {
        float_text::push_time_code(self.out, v);
    }

    /// `(r, i, j, k)`, the USDA order of a quaternion stored as `i, j, k,
    /// r` (§16.3.10.22).
    fn quat<T: Copy>(&mut self, &[i, j, k, r]: &[T; 4], item: impl FnMut(&mut Self, T)) {
        self.tuple(&[r, i, j, k], item);
    }

    /// `( (a, b), (c, d) )`: one tuple per row.
    fn matrix<const N: usize>(&mut self, rows: &[[f64; N]; N]) {
        self.out.push_str("( ");
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                self.out.push_str(", ");
            }
            self.tuple(row, Self::f64);
        }
        self.out.push_str(" )");
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

    /// A quoted string with the escapes of §16.2.5, quoted as OpenUSD
    /// quotes it (`Sdf_FileIOUtility::Quote`): double quotes unless the
    /// text holds a double quote and no single quote, and triple quotes
    /// around text with a line break, which is then written as it is.
    fn string(&mut self, s: &str) {
        let quote = if s.contains('"') && !s.contains('\'') {
            '\''
        } else {
            '"'
        };
        let triple = s.contains('\n');
        let open = |out: &mut String| {
            for _ in 0..if triple { 3 } else { 1 } {
                out.push(quote);
            }
        };
        open(self.out);
        for c in s.chars() {
            match c {
                '\n' if triple => self.out.push('\n'),
                '\n' => self.out.push_str("\\n"),
                '\r' => self.out.push_str("\\r"),
                '\t' => self.out.push_str("\\t"),
                '\\' => self.out.push_str("\\\\"),
                c if c == quote => {
                    self.out.push('\\');
                    self.out.push(c);
                }
                c if u32::from(c) < 0x20 || c == '\u{7F}' => {
                    let _ = write!(self.out, "\\x{:02x}", u32::from(c));
                }
                c => self.out.push(c),
            }
        }
        open(self.out);
    }

    /// A dictionary key: bare when it is an ASCII identifier, as OpenUSD
    /// writes it, otherwise quoted.
    fn key(&mut self, key: &str) {
        let mut chars = key.chars();
        if chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.out.push_str(key);
        } else {
            self.string(key);
        }
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
        custom string exedra:label = 'say "hi"'
        int[] faceVertexCounts = [3]
        point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 0.1, -0.0025)]
        texCoord2f[] primvars:st = [(0, 0), (1, 0), (0, 1)] (
            interpolation = "vertex"
        )
        uniform token subdivisionScheme = "none"
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
        dictionary nested = {
            string s = "x\\y"
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
            text.contains("quath[] orientations = [(0.707031, 0, 0, 0.707031), (1, 0, 0, 0)]"),
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
        for type_name in ["Vec3f", "PointFloat", "opaque[]", "dictionary[]"] {
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
            text.contains("        dictionary nested = {\n            int n = 1\n        }"),
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
        rel exedra:blocked = None
        custom rel exedra:declared
        rel exedra:many = [
            </Root/Mat>,
            </Root/Body.points>,
        ]
        rel material:binding = </Root/Mat>
        uniform token subsetFamily:materialBind:familyType = "partition"
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
                ("exedra:blocked", false, Some(vec![])),
                ("exedra:declared", true, None),
                (
                    "exedra:many",
                    false,
                    Some(vec!["/Root/Mat", "/Root/Body.points"])
                ),
                ("material:binding", false, Some(vec!["/Root/Mat"])),
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
        doc.metadata.push(Metadatum::new("tags", op.clone()));
        assert!(
            doc.to_usda()
                .unwrap()
                .contains("\n    prepend tags = [\"A\"]\n"),
            "a list-op statement in layer metadata"
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
                added: vec!["N".into()],
                prepended: vec!["P".into()],
                appended: vec!["A1".into(), "A2".into()],
                reordered: vec!["A2".into(), "P".into()],
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
                "    add apiSchemas = [\"N\"]\n",
                "    prepend apiSchemas = [\"P\"]\n",
                "    append apiSchemas = [\"A1\", \"A2\"]\n",
                "    reorder apiSchemas = [\"A2\", \"P\"]\n",
                // An explicit empty list is `None`, as OpenUSD writes it.
                "    exedraTags = None\n",
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

    /// An order of fewer than two names writes no `reorder` statement, as
    /// OpenUSD's writer leaves it out (`Sdf_WritePrimNamespaceReorders`,
    /// `_WriteLayer`): it reorders nothing, and a name list takes at least
    /// one name, so `reorder nameChildren = []` would not read back.
    #[test]
    fn short_orders_write_nothing() {
        let mut prim = Prim::def("Scope", "A");
        prim.prim_order = Some(Vec::new());
        prim.property_order = Some(vec!["x".into()]);
        let doc = Document {
            prim_order: Some(vec!["A".into()]),
            prims: vec![prim],
            ..Document::new()
        };
        let text = doc.to_usda().unwrap();
        assert_eq!(text, "#usda 1.0\n\ndef Scope \"A\"\n{\n}\n\n");
        assert!(parse(&text).diagnostics.is_empty(), "{text}");
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
    prepend rel r = [
        </A/C>,
        </A.x>,
    ]
    custom rel s (
        doc = "s"
    )
    prepend rel s = </A/C>
    float x = None
    float y.connect = None
    uniform float z = 1
    append uniform float z.connect = </A.x>

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
    'say "hi"'
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
                custom_data: Vec::new(),
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
    inherits = </C>
    kind = "group"
    prepend payload = @./p.usda@ (offset = 24; scale = 0.5)
    delete references = @./gone.usda@</G>
    append references = [
        </C> (offset = 1),
        <> (scale = 2)
    ]
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
                    custom_data: Vec::new(),
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

    /// Variant sets are written after the children, each variant with its
    /// metadata and body; nested sets inside variants; selections and the
    /// `variantSets` list op after the arcs.
    ///
    /// Spec: AOUSD Core §7.3.6, §16.2.17.
    #[test]
    fn writes_variant_sets() {
        let variant = |name: &str| Prim::new(Specifier::Over, None, name);
        let mut summer = variant("summer");
        summer
            .metadata
            .push(Metadatum::new("kind", Value::Token("component".into())));
        summer.references = Some(ListOp::prepend(vec![Reference {
            asset: Some("./leaves.usda".into()),
            prim_path: None,
            offset: LayerOffset::IDENTITY,
            custom_data: Vec::new(),
        }]));
        summer.variant_selections = vec![("size".into(), "tall".into())];
        summer.variant_set_names = Some(ListOp::prepend(vec!["size".into()]));
        summer.push_property(Attribute::new("height", "double", Value::Double(4.0)));
        summer.children.push(Prim::def("Mesh", "Canopy"));
        let mut tall = variant("tall");
        tall.push_property(Attribute::new("height", "double", Value::Double(9.0)));
        summer.variant_sets.push(VariantSet {
            name: "size".into(),
            variants: vec![tall],
        });
        let winter = variant("winter");

        let mut tree = Prim::def("Xform", "Tree");
        tree.variant_selections = vec![("season".into(), "summer".into())];
        tree.variant_set_names = Some(ListOp::prepend(vec!["season".into()]));
        tree.children.push(Prim::def("Scope", "Trunk"));
        tree.variant_sets.push(VariantSet {
            name: "season".into(),
            variants: vec![summer, winter],
        });
        let doc = Document {
            prims: vec![tree],
            ..Document::new()
        };
        let text = doc.to_usda().unwrap();
        let expected = r#"#usda 1.0

def Xform "Tree" (
    variants = {
        string season = "summer"
    }
    prepend variantSets = "season"
)
{
    def Scope "Trunk"
    {
    }
    variantSet "season" = {
        "summer" (
            kind = "component"
            prepend references = @./leaves.usda@
            variants = {
                string size = "tall"
            }
            prepend variantSets = "size"
        ) {
            double height = 4

            def Mesh "Canopy"
            {
            }
            variantSet "size" = {
                "tall" {
                    double height = 9

                }
            }

        }
        "winter" {

        }
    }
}

"#;
        assert_eq!(text, expected, "variant sets");
        assert!(parse(&text).diagnostics.is_empty(), "re-parses");

        let bad = |edit: fn(&mut Prim)| {
            let mut bad = doc.clone();
            edit(&mut bad.prims[0]);
            bad.to_usda().unwrap_err()
        };
        assert_eq!(
            bad(|p| p.variant_sets[0].variants[1].specifier = Specifier::Def),
            WriteError::InvalidVariant {
                path: "/Tree{season=winter}".into()
            },
            "a variant is an untyped over"
        );
        assert_eq!(
            bad(|p| p.variant_sets[0].variants.clear()),
            WriteError::EmptyVariantSet {
                path: "/Tree{season=}".into()
            },
            "a variant set has variants"
        );
        assert_eq!(
            bad(|p| {
                let again = p.variant_sets[0].variants[1].clone();
                p.variant_sets[0].variants.push(again);
            }),
            WriteError::Duplicate {
                path: "/Tree{season=winter}".into()
            },
            "variant names are unique in their set"
        );
        assert_eq!(
            bad(|p| p.variant_sets[0].variants[1].name = "a b".into()),
            WriteError::InvalidName {
                path: "/Tree{season=a b}".into(),
                name: "a b".into()
            },
            "variant names"
        );
        assert_eq!(
            bad(|p| p
                .variant_selections
                .push(("season".into(), "winter".into()))),
            WriteError::Duplicate {
                path: "/Tree#variants/season".into()
            },
            "one selection per set"
        );
        assert_eq!(
            bad(|p| p.variant_set_names = Some(ListOp::default())),
            WriteError::InvalidListOp {
                path: "/Tree#variantSets".into()
            },
            "a variantSets list op says something"
        );
        assert_eq!(
            bad(|p| p.variant_set_names = Some(ListOp::explicit(Vec::new()))),
            WriteError::EmptyExplicitVariantSets {
                path: "/Tree".into()
            },
            "USDA cannot spell an empty explicit variantSets"
        );
        let mut empty_explicit = doc.clone();
        empty_explicit.prims[0].variant_set_names = Some(ListOp::explicit(Vec::new()));
        assert_eq!(
            empty_explicit.validate(),
            Ok(()),
            "the crate format holds an empty explicit variantSets"
        );
        assert_eq!(
            bad(|p| p.variant_sets[0].variants[0].children[0].name = "1".into()),
            WriteError::InvalidName {
                path: "/Tree{season=summer}1".into(),
                name: "1".into()
            },
            "a prim in a variant follows its selection"
        );
        let mut empty_selection = doc.clone();
        empty_selection.prims[0].variant_selections[0].1 = String::new();
        assert!(
            empty_selection
                .to_usda()
                .unwrap()
                .contains("string season = \"\"\n"),
            "an empty selection selects no variant"
        );
    }

    /// The text OpenUSD 26.8's `SdfLayer::ExportToString` writes for the
    /// same strings, dictionary keys and values.
    #[test]
    fn values_are_spelled_as_openusd_spells_them() {
        let entries = [
            ("b", Value::Bool(true)),
            ("bs", Value::String("back\\slash".into())),
            ("ctl", Value::String("a\u{1}b\u{7f}".into())),
            ("m", Value::String("a\nb".into())),
            ("m2", Value::String("a\n\"b\"".into())),
            ("m3", Value::String("a\nb'\"".into())),
            ("s", Value::String("it's \"q\"".into())),
            ("q", Value::String("say \"hi\"".into())),
            ("tab", Value::String("a\tb".into())),
            ("u", Value::String("\u{e9}t\u{e9}".into())),
            ("\u{e9}t\u{e9}", Value::Int(4)),
            ("class", Value::Int(1)),
        ];
        let mut prim = Prim::new(Specifier::Class, None, "P");
        prim.metadata.push(Metadatum::new(
            "customData",
            Value::Dictionary(entries.map(|(k, v)| (k.to_string(), v)).to_vec()),
        ));
        prim.metadata
            .push(Metadatum::new("hidden", Value::Bool(true)));
        prim.push_property(Attribute::new("a", "bool", Value::Bool(false)));
        prim.push_property(Attribute::new(
            "flags",
            "bool[]",
            Value::BoolArray(vec![true, false]),
        ));
        prim.push_property(Attribute::declared("o", "opaque"));
        let text = Document {
            prims: vec![prim],
            ..Document::new()
        }
        .to_usda()
        .unwrap();
        assert_eq!(
            text,
            r#"#usda 1.0

class "P" (
    customData = {
        bool b = 1
        string bs = "back\\slash"
        int class = 1
        string ctl = "a\x01b\x7f"
        string m = """a
b"""
        string m2 = '''a
"b"'''
        string m3 = """a
b'\""""
        string q = 'say "hi"'
        string s = "it's \"q\""
        string tab = "a\tb"
        string u = "été"
        int "été" = 4
    }
    hidden = true
)
{
    bool a = 0
    bool[] flags = [1, 0]
    opaque o
}

"#
        );
        assert!(
            parse(&text).diagnostics.is_empty(),
            "OpenUSD's spelling reads back"
        );
        assert!(
            matches!(
                doc_with(Attribute::new("o", "opaque", Value::Int(1))).to_usda(),
                Err(WriteError::TypeMismatch { .. })
            ),
            "an opaque attribute holds no value"
        );
    }
}
