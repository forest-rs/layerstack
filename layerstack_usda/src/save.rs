// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Saving an authored [`Layer`]: the one lowering route from the layer
//! model to the writers.
//!
//! [`layer_document`] lowers a [`Layer`] to the authored [`Document`] that
//! both writers serialize: [`Document::to_usda`] here, and the crate writer
//! in `layerstack_usdc` (`layerstack_usdc::writer::save_layer`). Every source
//! field is read in this one place, so USDA and USDC cannot select
//! different slots of the same layer; the crate lowering of a [`Document`]
//! is itself defined as what OpenUSD's text parser stores for that
//! document's USDA.
//!
//! This saves the authored layer as it is, not a composed stage: nothing is
//! resolved, flattened or packaged. Asset paths are written as authored,
//! never as resolved locations, and path-looking strings stay strings.
//! Meaning is preserved, not formatting: whitespace, comments outside
//! `comment` fields and statement layout are the writer's own.
//!
//! # Supported subset
//!
//! An arc-free, non-variant layer:
//!
//! - layer metadata, including `defaultPrim` and bare-string comments;
//! - prim specs with their specifier, `typeName`, metadata (`apiSchemas`
//!   and other token list ops included, `active` and `instanceable`),
//!   children in authored order, `reorder nameChildren`, `reorder
//!   properties` and `reorder rootPrims`;
//! - attribute specs with `custom`, `uniform`, the declared type, a default
//!   value (a value block included), time samples (blocked samples and an
//!   empty sample map included), explicit or list-edited connections and
//!   metadata;
//! - relationship specs with `custom`, explicit or list-edited targets and
//!   metadata;
//! - properties in one authored order, attributes and relationships
//!   interleaved.
//!
//! # Rejected before any output
//!
//! [`layer_document`] checks the whole layer before a writer runs and
//! returns the first problem it finds, naming its source path:
//!
//! - [`SaveError::Unsupported`]: sublayers, references and payloads (an
//!   arc whose asset did not resolve on import is kept as
//!   `Reference::unresolved` and rejected the same way), inherits,
//!   specializes, variant sets, variant selections and specs authored inside
//!   variant branches; splines; sparse array edits (as a default or a time
//!   sample); list ops mixing an explicit list with edits;
//!   `varying` relationships; list-op metadata other than token list ops;
//!   and values the writers have no representation for (`half`, `uchar`,
//!   `uint64`, quaternions, `matrix2d`/`matrix3d`, `pathExpression`,
//!   `opaque`, `timecode` outside an attribute default, and arrays whose
//!   element type is not recorded, such as an empty array in a dictionary);
//! - [`SaveError::Invalid`]: a layer the file formats cannot hold as it
//!   stands, such as a prim spec that no parent lists among its children,
//!   an attribute without a type or a relationship with time samples;
//! - [`SaveError::Document`]: what the writers' shared validation rejects
//!   (invalid identifiers, a `defaultPrim` that names no prim of the layer, keys
//!   whose USDA syntax the writer does not produce, sample times that are not
//!   finite and increasing, ...).
//!
//! The crate writer additionally rejects metadata keys that OpenUSD does not
//! register, since it cannot store them as the text parser would.
//!
//! Spec: AOUSD Core §7 (scene description: layers, specs, fields and
//! metadata), §16.2 (USDA), §16.3 (crate format).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use layerstack::HashSet;
use layerstack::doc::{FieldEntry, FieldValue, Layer, PrimSpec, Specifier, Value as LayerValue};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp as LayerListOp;
use layerstack::path::{Path, PathId, PathInterner, TargetPath};
use layerstack::property::{PropertyEntry, PropertyKind, Variability};

use crate::writer::{
    Attribute, Document, ListOp, Metadatum, Prim, Property, Relationship,
    Specifier as WriterSpecifier, Value, Variability as WriterVariability, WriteError,
};

/// Lowers an authored layer to the [`Document`] both writers serialize.
///
/// `tokens` and `paths` are the interners the layer's identifiers belong
/// to. The whole layer is checked first; on success the returned document
/// also passes [`Document::validate`]. See the [module docs](self) for the
/// supported subset.
///
/// # Errors
///
/// A [`SaveError`] naming the source path of the first unsupported or
/// invalid spec, field or value.
pub fn layer_document(
    layer: &Layer,
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Result<Document, SaveError> {
    let doc = Lowering { tokens, paths }.layer(layer)?;
    doc.validate().map_err(SaveError::Document)?;
    Ok(doc)
}

/// Saves an authored layer as USDA text, through [`layer_document`].
///
/// # Errors
///
/// See [`layer_document`]. Nothing is produced on error.
pub fn save_usda(
    layer: &Layer,
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Result<String, SaveError> {
    layer_document(layer, tokens, paths)?
        .to_usda()
        .map_err(SaveError::Document)
}

/// Why an authored layer could not be saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The layer authors something this save does not write yet.
    Unsupported {
        /// Source path: `/` for the layer, a prim or property path, with a
        /// metadata key after `#`.
        path: String,
        /// What is not supported.
        feature: Unsupported,
    },
    /// The layer cannot be written as it stands.
    Invalid {
        /// Source path, as for [`Self::Unsupported`].
        path: String,
        /// What is wrong.
        problem: Invalid,
    },
    /// The writers' shared validation rejects the lowered document.
    Document(WriteError),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { path, feature } => {
                write!(f, "{path}: saving {feature} is not supported")
            }
            Self::Invalid { path, problem } => write!(f, "{path}: {problem}"),
            Self::Document(e) => write!(f, "{e}"),
        }
    }
}

impl core::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Document(e) => Some(e),
            _ => None,
        }
    }
}

/// Authored content outside the supported subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Layer `subLayers`.
    Sublayers,
    /// A prim's `references`.
    References,
    /// A prim's `payload`.
    Payloads,
    /// A prim's `inheritPaths`.
    Inherits,
    /// A prim's `specializes`.
    Specializes,
    /// A prim's variant sets (`variantSetNames` or variant set specs).
    VariantSets,
    /// A prim's variant selections.
    VariantSelections,
    /// A prim spec authored inside a variant branch.
    VariantSpec,
    /// An attribute's `spline`.
    Spline,
    /// A sparse array edit.
    ArrayEdit,
    /// A list op that holds an explicit list and edits at once, which the
    /// file formats cannot hold.
    MixedListOp,
    /// A `varying` relationship.
    VaryingRelationship,
    /// List-op metadata of this kind (only token list ops are written).
    ListOpMetadata(&'static str),
    /// A value of this kind.
    Value(&'static str),
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sublayers => f.write_str("sublayers"),
            Self::References => f.write_str("references"),
            Self::Payloads => f.write_str("payloads"),
            Self::Inherits => f.write_str("inherits"),
            Self::Specializes => f.write_str("specializes"),
            Self::VariantSets => f.write_str("variant sets"),
            Self::VariantSelections => f.write_str("variant selections"),
            Self::VariantSpec => f.write_str("specs inside variant branches"),
            Self::Spline => f.write_str("splines"),
            Self::ArrayEdit => f.write_str("sparse array edits"),
            Self::MixedListOp => f.write_str("a list op with an explicit list and edits"),
            Self::VaryingRelationship => f.write_str("varying relationships"),
            Self::ListOpMetadata(kind) => write!(f, "{kind} metadata"),
            Self::Value(kind) => write!(f, "a {kind} value"),
        }
    }
}

/// A layer the file formats cannot hold as it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// A prim spec has no specifier.
    MissingSpecifier,
    /// An attribute spec has no declared type.
    MissingTypeName,
    /// A prim spec is not listed among its parent's authored children, so
    /// it has no place in the written namespace.
    UnlistedPrim,
    /// A prim lists a child that has no prim spec.
    MissingChildSpec,
    /// A relationship spec holds a type, default or time samples.
    RelationshipValue,
    /// The pseudo-root spec holds more than children and their order.
    PseudoRootOpinions,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingSpecifier => "prim spec has no specifier",
            Self::MissingTypeName => "attribute has no type name",
            Self::UnlistedPrim => "prim spec is not among its parent's children",
            Self::MissingChildSpec => "listed child has no prim spec",
            Self::RelationshipValue => "relationship holds a type or values",
            Self::PseudoRootOpinions => "pseudo-root spec holds prim opinions",
        })
    }
}

struct Lowering<'a> {
    tokens: &'a TokenInterner,
    paths: &'a PathInterner,
}

fn unsupported<T>(path: impl Into<String>, feature: Unsupported) -> Result<T, SaveError> {
    Err(SaveError::Unsupported {
        path: path.into(),
        feature,
    })
}

fn invalid<T>(path: impl Into<String>, problem: Invalid) -> Result<T, SaveError> {
    Err(SaveError::Invalid {
        path: path.into(),
        problem,
    })
}

fn is_authored<T>(op: &LayerListOp<T>) -> bool {
    op.explicit.is_some()
        || !op.prepend.is_empty()
        || !op.append.is_empty()
        || !op.delete.is_empty()
}

/// Where a value is authored, which decides the types it may have.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Site<'t> {
    /// An attribute default or time sample, declared with this type name
    /// (`[]` included).
    Value(&'t str),
    /// A metadata field or dictionary entry.
    Metadata,
}

impl Lowering<'_> {
    fn name(&self, token: TokenId) -> String {
        String::from(self.tokens.resolve(token))
    }

    fn display(&self, path: PathId) -> String {
        self.paths.display(path, self.tokens)
    }

    // ── Layer ───────────────────────────────────────────────────────

    /// Spec: AOUSD Core §7.6.1 (layer spec fields).
    fn layer(&self, layer: &Layer) -> Result<Document, SaveError> {
        if !layer.sublayers.is_empty() {
            return unsupported("/", Unsupported::Sublayers);
        }
        let mut in_branches: Vec<String> = layer
            .variant_prims
            .iter()
            .filter(|(_, specs)| !specs.is_empty())
            .map(|(path, _)| self.display(*path))
            .chain(
                layer
                    .prims
                    .iter()
                    .filter(|(_, spec)| !spec.outer_variant_sites.is_empty())
                    .map(|(path, _)| self.display(*path)),
            )
            .collect();
        in_branches.sort();
        if let Some(path) = in_branches.into_iter().next() {
            return unsupported(path, Unsupported::VariantSpec);
        }

        let mut doc = Document::new();
        doc.default_prim = layer.default_prim.map(|t| self.name(t));
        doc.metadata = self.metadata(&layer.metadata, "/")?;

        let root = self.paths.lookup(&Path::root());
        let mut visited: HashSet<PathId> = HashSet::new();
        if let Some((root, spec)) = root.and_then(|id| Some((id, layer.prims.get(&id)?))) {
            visited.insert(root);
            if !is_children_only(spec) {
                return invalid("/", Invalid::PseudoRootOpinions);
            }
            doc.prim_order = spec.prim_order.as_ref().map(|o| self.names(o));
            for &child in &spec.authored_children {
                doc.prims
                    .push(self.prim(layer, &Path::root(), child, &mut visited)?);
            }
        }

        let mut unlisted: Vec<String> = layer
            .prims
            .keys()
            .filter(|path| !visited.contains(*path))
            .map(|path| self.display(*path))
            .collect();
        unlisted.sort();
        if let Some(path) = unlisted.into_iter().next() {
            return invalid(path, Invalid::UnlistedPrim);
        }
        Ok(doc)
    }

    fn names(&self, names: &[TokenId]) -> Vec<String> {
        names.iter().map(|&t| self.name(t)).collect()
    }

    // ── Prims ───────────────────────────────────────────────────────

    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    fn prim(
        &self,
        layer: &Layer,
        parent: &Path,
        name: TokenId,
        visited: &mut HashSet<PathId>,
    ) -> Result<Prim, SaveError> {
        let path = parent.join(&[name]);
        let shown = path.display(self.tokens);
        let Some((id, spec)) = self
            .paths
            .lookup(&path)
            .and_then(|id| Some((id, layer.prims.get(&id)?)))
        else {
            return invalid(shown, Invalid::MissingChildSpec);
        };
        visited.insert(id);

        for (op_authored, feature) in [
            (is_authored(&spec.references), Unsupported::References),
            (is_authored(&spec.payloads), Unsupported::Payloads),
            (is_authored(&spec.inherits), Unsupported::Inherits),
            (is_authored(&spec.specializes), Unsupported::Specializes),
            (
                !spec.variant_sets.is_empty() || !spec.variant_set_order.is_empty(),
                Unsupported::VariantSets,
            ),
            (
                !spec.variant_selections.is_empty(),
                Unsupported::VariantSelections,
            ),
        ] {
            if op_authored {
                return unsupported(shown, feature);
            }
        }
        let specifier = match spec.specifier {
            Some(Specifier::Def) => WriterSpecifier::Def,
            Some(Specifier::Over) => WriterSpecifier::Over,
            Some(Specifier::Class) => WriterSpecifier::Class,
            None => return invalid(shown, Invalid::MissingSpecifier),
        };

        let mut prim = Prim::new(
            specifier,
            spec.type_name.map(|t| self.name(t)),
            self.name(name),
        );
        prim.metadata = self.metadata(&spec.fields, &shown)?;
        // Dedicated members of the layer model, stored as the `active` and
        // `instanceable` fields (AOUSD Core §7.6.2).
        for (key, value) in [("active", spec.active), ("instanceable", spec.instanceable)] {
            if let Some(value) = value {
                prim.metadata.push(Metadatum::new(key, Value::Bool(value)));
            }
        }
        prim.property_order = spec.property_order.as_ref().map(|o| self.names(o));
        prim.prim_order = spec.prim_order.as_ref().map(|o| self.names(o));
        for entry in &spec.properties {
            prim.properties.push(self.property(entry, &shown)?);
        }
        for &child in &spec.authored_children {
            prim.children.push(self.prim(layer, &path, child, visited)?);
        }
        Ok(prim)
    }

    // ── Properties ──────────────────────────────────────────────────

    /// Spec: AOUSD Core §7.6.3–§7.6.5 (property, attribute and relationship
    /// spec fields).
    fn property(&self, entry: &PropertyEntry, prim: &str) -> Result<Property, SaveError> {
        let name = self.name(entry.name);
        let path = format!("{prim}.{name}");
        let spec = &entry.spec;
        if spec.spline.is_some() {
            return unsupported(path, Unsupported::Spline);
        }
        let targets = match &spec.targets {
            Some(op) => Some(self.target_list(op, &path)?),
            None => None,
        };
        let metadata = self.metadata(&spec.metadata, &path)?;
        Ok(match spec.kind {
            PropertyKind::Attribute => {
                let Some(declared) = &spec.type_name else {
                    return invalid(path, Invalid::MissingTypeName);
                };
                let base = declared
                    .type_name
                    .strip_suffix("[]")
                    .unwrap_or(&declared.type_name);
                let type_name = if declared.is_array {
                    format!("{base}[]")
                } else {
                    String::from(base)
                };
                let site = Site::Value(&type_name);
                let value = match &spec.default {
                    Some(value) => Some(self.value(value, site, &path)?),
                    None => None,
                };
                // A blocked sample is a value block (§12.3.6), which
                // `value` converts like any other value.
                let time_samples = match &spec.time_samples {
                    Some(samples) => Some(
                        samples
                            .iter()
                            .map(|(time, value)| Ok((*time, self.value(value, site, &path)?)))
                            .collect::<Result<Vec<_>, SaveError>>()?,
                    ),
                    None => None,
                };
                Property::Attribute(Attribute {
                    name,
                    type_name,
                    custom: spec.custom,
                    variability: match spec.variability {
                        Variability::Varying => WriterVariability::Varying,
                        Variability::Uniform => WriterVariability::Uniform,
                    },
                    value,
                    time_samples,
                    connections: targets,
                    metadata,
                })
            }
            PropertyKind::Relationship => {
                if spec.type_name.is_some() || spec.default.is_some() || spec.time_samples.is_some()
                {
                    return invalid(path, Invalid::RelationshipValue);
                }
                if spec.variability == Variability::Varying {
                    return unsupported(path, Unsupported::VaryingRelationship);
                }
                Property::Relationship(Relationship {
                    name,
                    custom: spec.custom,
                    targets,
                    metadata,
                })
            }
        })
    }

    fn target_list(
        &self,
        op: &LayerListOp<TargetPath>,
        path: &str,
    ) -> Result<ListOp<String>, SaveError> {
        self.list_op(op, path, |t| t.display(self.paths, self.tokens))
    }

    /// Converts a list op, keeping each list. OpenUSD's list ops hold an
    /// explicit list or edits, never both (AOUSD Core §6.6.3).
    fn list_op<T>(
        &self,
        op: &LayerListOp<T>,
        path: &str,
        item: impl Fn(&T) -> String,
    ) -> Result<ListOp<String>, SaveError> {
        let edits = !(op.prepend.is_empty() && op.append.is_empty() && op.delete.is_empty());
        if op.explicit.is_some() && edits {
            return unsupported(path, Unsupported::MixedListOp);
        }
        let list = |items: &[T]| items.iter().map(&item).collect::<Vec<_>>();
        Ok(ListOp {
            explicit: op.explicit.as_deref().map(list),
            deleted: list(&op.delete),
            prepended: list(&op.prepend),
            appended: list(&op.append),
        })
    }

    // ── Metadata ────────────────────────────────────────────────────

    /// Spec: AOUSD Core §7.4 (metadata fields). USDA spells the
    /// `documentation` field `doc`.
    fn metadata(&self, fields: &[FieldEntry], owner: &str) -> Result<Vec<Metadatum>, SaveError> {
        let mut out = Vec::with_capacity(fields.len());
        for field in fields {
            let key = self.tokens.resolve(field.name);
            let path = format!("{owner}#{key}");
            let value = match &field.value {
                FieldValue::Value(value) => self.metadata_value(value, key, &path)?,
                FieldValue::TokenListOp(op) => {
                    Value::TokenListOp(self.list_op(op, &path, |t| self.name(*t))?)
                }
                FieldValue::PathListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("path list op"));
                }
                FieldValue::StringListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("string list op"));
                }
                FieldValue::IntListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("int list op"));
                }
                FieldValue::UIntListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("uint list op"));
                }
                FieldValue::Int64ListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("int64 list op"));
                }
                FieldValue::UInt64ListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("uint64 list op"));
                }
            };
            let key = match key {
                "documentation" => "doc",
                key => key,
            };
            out.push(Metadatum::new(key, value));
        }
        Ok(out)
    }

    /// A metadata value. Registered array fields give an empty array its
    /// element type; elsewhere an empty array has none.
    fn metadata_value(
        &self,
        value: &LayerValue,
        key: &str,
        path: &str,
    ) -> Result<Value, SaveError> {
        if let LayerValue::Array(items) = value
            && items.is_empty()
        {
            let declared = match key {
                "allowedTokens" => "token[]",
                "displayGroupOrder" => "string[]",
                "payloadAssetDependencies" => "asset[]",
                _ => return unsupported(path, Unsupported::Value("untyped empty array")),
            };
            return Value::empty_array_of(declared)
                .map_or_else(|| unsupported(path, Unsupported::Value("array")), Ok);
        }
        self.value(value, Site::Metadata, path)
    }

    // ── Values ──────────────────────────────────────────────────────

    /// Converts a value, keeping its type. Spec: AOUSD Core §6.2–§6.3
    /// (value types), §12.3 (value blocks).
    fn value(&self, value: &LayerValue, site: Site<'_>, path: &str) -> Result<Value, SaveError> {
        use LayerValue as L;
        let no = |kind: &'static str| unsupported(path, Unsupported::Value(kind));
        Ok(match value {
            L::Bool(v) => Value::Bool(*v),
            L::Int(v) => Value::Int(*v),
            L::UInt(v) => Value::UInt(*v),
            L::Int64(v) => Value::Int64(*v),
            L::Float(v) => Value::Float(*v),
            L::Double(v) => Value::Double(*v),
            // The writers hold a `timecode` default as a double and store
            // it as `SdfTimeCode` from the declared type.
            L::TimeCode(v) if matches!(site, Site::Value(_)) => Value::Double(*v),
            L::TimeCode(_) => return no("timecode"),
            L::String(v) => Value::String(String::from(&**v)),
            L::Token(v) => Value::Token(self.name(*v)),
            L::Asset(v) => Value::Asset(String::from(&**v)),
            L::Vec2f(v) => Value::Float2(*v),
            L::Vec3f(v) => Value::Float3(*v),
            L::Vec4f(v) => Value::Float4(*v),
            L::Vec2d(v) => Value::Double2(*v),
            L::Vec3d(v) => Value::Double3(*v),
            L::Vec4d(v) => Value::Double4(*v),
            L::Vec2i(v) => Value::Int2(*v),
            L::Vec3i(v) => Value::Int3(*v),
            L::Vec4i(v) => Value::Int4(*v),
            L::Matrix4d(m) => Value::Matrix4d(core::array::from_fn(|r| {
                core::array::from_fn(|c| m[r * 4 + c])
            })),
            // Outside an attribute default the writers' validation rejects
            // a block, naming the owner.
            L::Blocked => Value::Block,
            L::Dictionary(entries) => Value::Dictionary(
                entries
                    .iter()
                    .map(|(k, v)| {
                        let path = format!("{path}/{k}");
                        Ok((String::from(&**k), self.metadata_value(v, "", &path)?))
                    })
                    .collect::<Result<_, SaveError>>()?,
            ),
            L::Array(items) => self.array(items, site, path)?,
            L::ArrayEdit(_) => return unsupported(path, Unsupported::ArrayEdit),
            L::Half(_) => return no("half"),
            L::UChar(_) => return no("uchar"),
            L::UInt64(_) => return no("uint64"),
            L::Vec2h(_) | L::Vec3h(_) | L::Vec4h(_) => return no("half vector"),
            L::Quatd(_) | L::Quatf(_) | L::Quath(_) => return no("quaternion"),
            L::Matrix2d(_) => return no("matrix2d"),
            L::Matrix3d(_) => return no("matrix3d"),
            L::PathExpression(_) => return no("pathExpression"),
            L::Opaque { .. } => return no("opaque"),
            L::Null => return no("null"),
        })
    }

    /// Converts an array. The element type comes from the declared type of
    /// an attribute default, otherwise from the elements themselves, which
    /// must all have the first one's type.
    fn array(&self, items: &[LayerValue], site: Site<'_>, path: &str) -> Result<Value, SaveError> {
        let mut out = match site {
            Site::Value(type_name) => Value::empty_array_of(type_name),
            Site::Metadata => None,
        };
        for item in items {
            let element = self.value(item, site, path)?;
            let array =
                out.get_or_insert_with(|| empty_array_like(&element).unwrap_or(Value::Block));
            if !push_element(array, element) {
                return unsupported(path, Unsupported::Value("mixed or nested array"));
            }
        }
        out.map_or_else(
            || unsupported(path, Unsupported::Value("untyped empty array")),
            Ok,
        )
    }
}

/// Whether a prim spec holds only children and their order: the shape of
/// the pseudo-root spec the importers produce.
fn is_children_only(spec: &PrimSpec) -> bool {
    spec.specifier.is_none()
        && spec.type_name.is_none()
        && spec.fields.is_empty()
        && spec.properties.is_empty()
        && spec.property_order.is_none()
        && spec.variant_selections.is_empty()
        && spec.variant_sets.is_empty()
        && spec.variant_set_order.is_empty()
        && !is_authored(&spec.references)
        && !is_authored(&spec.payloads)
        && !is_authored(&spec.inherits)
        && !is_authored(&spec.specializes)
        && spec.active.is_none()
        && spec.instanceable.is_none()
        && spec.outer_variant_sites.is_empty()
}

/// The empty array of `element`'s type, if the writers have one.
fn empty_array_like(element: &Value) -> Option<Value> {
    Value::empty_array_of(&format!("{}[]", element.canonical_type_name()))
}

/// Appends `element` to `array` when it has the array's element type.
fn push_element(array: &mut Value, element: Value) -> bool {
    match (array, element) {
        (Value::BoolArray(a), Value::Bool(v)) => a.push(v),
        (Value::IntArray(a), Value::Int(v)) => a.push(v),
        (Value::UIntArray(a), Value::UInt(v)) => a.push(v),
        (Value::Int64Array(a), Value::Int64(v)) => a.push(v),
        (Value::FloatArray(a), Value::Float(v)) => a.push(v),
        (Value::DoubleArray(a), Value::Double(v)) => a.push(v),
        (Value::StringArray(a), Value::String(v)) => a.push(v),
        (Value::TokenArray(a), Value::Token(v)) => a.push(v),
        (Value::AssetArray(a), Value::Asset(v)) => a.push(v),
        (Value::Float2Array(a), Value::Float2(v)) => a.push(v),
        (Value::Float3Array(a), Value::Float3(v)) => a.push(v),
        (Value::Float4Array(a), Value::Float4(v)) => a.push(v),
        (Value::Double2Array(a), Value::Double2(v)) => a.push(v),
        (Value::Double3Array(a), Value::Double3(v)) => a.push(v),
        (Value::Double4Array(a), Value::Double4(v)) => a.push(v),
        (Value::Int2Array(a), Value::Int2(v)) => a.push(v),
        (Value::Int3Array(a), Value::Int3(v)) => a.push(v),
        (Value::Int4Array(a), Value::Int4(v)) => a.push(v),
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests;
