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
//! A layer with:
//!
//! - layer metadata, including `defaultPrim` and bare-string comments, and
//!   sublayers with their layer offsets;
//! - prim specs with their specifier, `typeName`, metadata (list ops of
//!   tokens, strings and integers included, such as `apiSchemas`,
//!   `clipSets` and `inactiveIds`, and `active` and `instanceable`),
//!   composition arcs (references, payloads, inherits and specializes, in
//!   any list-op form), children in authored order, `reorder
//!   nameChildren`, `reorder properties` and `reorder rootPrims`;
//! - attribute specs with `custom`, `uniform`, the declared type, a default
//!   value (a value block included), time samples (blocked samples and an
//!   empty sample map included), explicit or list-edited connections and
//!   metadata;
//! - relationship specs with `custom`, explicit or list-edited targets and
//!   metadata;
//! - properties in one authored order, attributes and relationships
//!   interleaved;
//! - values of every scalar, vector, quaternion and matrix type (`half`
//!   and the other binary16 types kept as their bit patterns), and arrays
//!   of them;
//! - variant selections, the `variantSets` of each prim spec (written as
//!   `prepend`) and variant sets with their variants, each variant with
//!   everything a prim spec holds: metadata, arcs, selections, properties,
//!   `reorder properties`, child prims (the prim specs the layer keeps in
//!   that branch, [`Layer::branch_prim_specs`]) and nested variant sets,
//!   written inside the branch that encloses them.
//!
//! Variant sets are written in the prim spec's `variantSets` order, then by
//! name; variants and selections by name, as OpenUSD writes them.
//!
//! Arcs are written from what was authored, never from what they resolved
//! to: a sublayer, reference or payload by its authored asset path (an arc
//! whose asset did not resolve on import, `Reference::unresolved` or
//! `SublayerEntry::unresolved`, keeps its own), a reference or payload
//! with its prim path or none for the `defaultPrim`, and its layer offset.
//! A reference or payload without an asset path into the layer itself is
//! internal.
//!
//! # Rejected before any output
//!
//! [`layer_document`] checks the whole layer before a writer runs and
//! returns the first problem it finds, naming its source path:
//!
//! - [`SaveError::Unsupported`]: splines; sparse array edits (as a
//!   default or a time sample); list ops mixing an explicit list with
//!   edits; `varying` relationships; path list-op metadata; and values the
//!   writers have no representation for (`pathExpression`, `opaque`,
//!   `timecode` outside an attribute value, and arrays whose element type
//!   is not recorded, such as an empty array in a dictionary);
//! - [`SaveError::Invalid`]: a layer the file formats cannot hold as it
//!   stands, such as a prim spec that no parent or variant lists among its
//!   children, an attribute without a type, a relationship with time
//!   samples, or a sublayer or arc into another layer built from a layer id
//!   alone, with no authored asset path to write;
//! - [`SaveError::Document`]: what the writers' shared validation rejects
//!   (invalid identifiers, a `defaultPrim` that names no prim of the layer, keys
//!   whose USDA syntax the writer does not produce, sample times that are not
//!   finite and increasing, asset paths the formats cannot quote, arc
//!   paths that are not prim paths, an arc list that repeats an item within
//!   one operation, ...).
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

use layerstack::doc::{
    FieldEntry, FieldValue, Layer, LayerOffset as LayerLayerOffset, PrimSpec,
    Reference as LayerReference, ReferenceTarget, Specifier, Value as LayerValue, VariantSetSpec,
    VariantSpec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp as LayerListOp;
use layerstack::path::{Path, PathId, PathInterner, TargetPath};
use layerstack::property::{PropertyEntry, PropertyKind, Variability};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::{HashMap, HashSet};

use crate::writer::{
    Attribute, Document, LayerOffset, ListOp, Metadatum, Prim, Property, Reference, Relationship,
    Specifier as WriterSpecifier, SubLayer, Value, Variability as WriterVariability, VariantSet,
    WriteError,
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
    /// An attribute's `spline`.
    Spline,
    /// A sparse array edit.
    ArrayEdit,
    /// A list op that holds an explicit list and edits at once, which the
    /// file formats cannot hold.
    MixedListOp,
    /// A `varying` relationship.
    VaryingRelationship,
    /// List-op metadata of this kind (path list ops, which USDA metadata
    /// has no syntax for).
    ListOpMetadata(&'static str),
    /// A value of this kind.
    Value(&'static str),
    /// The layer's `layerRelocates` metadata.
    Relocates,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spline => f.write_str("splines"),
            Self::ArrayEdit => f.write_str("sparse array edits"),
            Self::MixedListOp => f.write_str("a list op with an explicit list and edits"),
            Self::VaryingRelationship => f.write_str("varying relationships"),
            Self::ListOpMetadata(kind) => write!(f, "{kind} metadata"),
            Self::Value(kind) => write!(f, "a {kind} value"),
            Self::Relocates => f.write_str("layer relocates"),
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
    /// A prim spec is not listed among the authored children of its parent
    /// (of the variant that holds it, for a prim spec in a variant branch),
    /// so it has no place in the written namespace.
    UnlistedPrim,
    /// A prim lists a child that has no prim spec.
    MissingChildSpec,
    /// A relationship spec holds a type, default or time samples.
    RelationshipValue,
    /// The pseudo-root spec holds more than children and their order.
    PseudoRootOpinions,
    /// A sublayer, or a reference or payload into another layer, records no
    /// authored asset path to write (it was built from a layer id alone).
    ArcWithoutAsset,
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
            Self::ArcWithoutAsset => "arc to another layer has no authored asset path",
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
        // Spec: AOUSD Core §7.6.1.2.4. The writers have no relocates
        // syntax yet, and dropping them would change the composed namespace.
        if !layer.relocates.is_empty() {
            return unsupported("/", Unsupported::Relocates);
        }

        let mut doc = Document::new();
        doc.default_prim = layer.default_prim.map(|t| self.name(t));
        doc.metadata = self.metadata(&layer.metadata, "/")?;
        // Spec: AOUSD Core §10.3.1 (sublayers), written by their authored
        // asset paths; an unresolved sublayer keeps its own.
        for sublayer in &layer.sublayers {
            let Some(asset) = &sublayer.asset else {
                return invalid("/", Invalid::ArcWithoutAsset);
            };
            doc.sublayers.push(SubLayer {
                asset: asset.clone(),
                offset: layer_offset(sublayer.offset),
            });
        }

        let root = self.paths.lookup(&Path::root());
        let mut visited = Visited::new();
        if let Some((root, spec)) = root.and_then(|id| Some((id, layer.prims.get(&id)?))) {
            visited.insert((root, Vec::new()));
            if !is_children_only(spec) {
                return invalid("/", Invalid::PseudoRootOpinions);
            }
            doc.prim_order = spec.prim_order.as_ref().map(|o| self.names(o));
            for &child in &spec.authored_children {
                let parent = Parent {
                    path: &Path::root(),
                    shown: "/",
                    sites: &[],
                };
                doc.prims
                    .push(self.prim(layer, parent, child, &mut visited)?);
            }
        }

        // Every prim spec, outside variants or inside one, must have been
        // reached from the pseudo-root through the children lists.
        let mut unlisted: Vec<String> = layer
            .prims
            .iter()
            .chain(
                layer
                    .variant_prims
                    .iter()
                    .flat_map(|(path, specs)| specs.iter().map(move |spec| (path, spec))),
            )
            .filter(|(path, spec)| !visited.contains(&(**path, spec.outer_variant_sites.clone())))
            .map(|(path, spec)| self.spec_display(*path, &spec.outer_variant_sites))
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

    /// The variant-qualified path of the prim spec at `path` inside the
    /// branches `sites` (`/A{v=x}B`), as OpenUSD names it.
    fn spec_display(&self, path: PathId, sites: &[VariantSelectionSite]) -> String {
        let segments = self.paths.resolve(path).segments();
        let mut out = String::new();
        for (i, &segment) in segments.iter().enumerate() {
            out = child_display(&out, self.tokens.resolve(segment));
            for site in sites {
                if self.paths.resolve(site.host_path).segments() == &segments[..=i] {
                    out = branch_display(
                        &out,
                        self.tokens.resolve(site.set),
                        self.tokens.resolve(site.variant),
                    );
                }
            }
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    }

    // ── Prims ───────────────────────────────────────────────────────

    /// Lowers the prim spec named `name` below `parent`, in the variant
    /// branches `parent.sites` enclose it in (none outside variants).
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields), §7.3.6 (variant specs
    /// contain prim specs).
    fn prim(
        &self,
        layer: &Layer,
        parent: Parent<'_>,
        name: TokenId,
        visited: &mut Visited,
    ) -> Result<Prim, SaveError> {
        let path = parent.path.join(&[name]);
        let shown = child_display(parent.shown, self.tokens.resolve(name));
        let Some((id, spec)) = self
            .paths
            .lookup(&path)
            .and_then(|id| Some((id, layer.prim_spec_in(id, parent.sites)?)))
        else {
            return invalid(shown, Invalid::MissingChildSpec);
        };
        visited.insert((id, parent.sites.to_vec()));

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
        let arcs = Arcs {
            references: &spec.references,
            payloads: &spec.payloads,
            inherits: &spec.inherits,
            specializes: &spec.specializes,
        };
        self.arcs(layer, arcs, &mut prim, &shown)?;
        prim.variant_selections = self.selections(&spec.variant_selections);
        prim.property_order = spec.property_order.as_ref().map(|o| self.names(o));
        prim.prim_order = spec.prim_order.as_ref().map(|o| self.names(o));
        for entry in &spec.properties {
            prim.properties.push(self.property(entry, &shown)?);
        }
        let here = Parent {
            path: &path,
            shown: &shown,
            sites: parent.sites,
        };
        for &child in &spec.authored_children {
            prim.children.push(self.prim(layer, here, child, visited)?);
        }

        (prim.variant_set_names, prim.variant_sets) = self.variant_sets(
            layer,
            id,
            &spec.variant_sets,
            &spec.variant_set_order,
            &spec.deleted_variant_sets,
            here,
            visited,
        )?;
        Ok(prim)
    }

    /// Sets a prim's or variant's composition arcs.
    ///
    /// Spec: AOUSD Core §7.6.2.3 (prim composition fields), §10.3.2.1–
    /// §10.3.2.4 (references, payloads, inherits, specializes).
    fn arcs(
        &self,
        layer: &Layer,
        arcs: Arcs<'_>,
        prim: &mut Prim,
        shown: &str,
    ) -> Result<(), SaveError> {
        let references = |op: &LayerListOp<LayerReference>, key: &str| {
            if !is_authored(op) {
                return Ok(None);
            }
            let path = format!("{shown}#{key}");
            self.list_op(op, &path, |r| self.arc(layer, r, shown))
                .map(Some)
        };
        prim.payloads = references(arcs.payloads, "payload")?;
        prim.references = references(arcs.references, "references")?;
        let paths = |op: &LayerListOp<PathId>, key: &str| {
            if !is_authored(op) {
                return Ok(None);
            }
            let path = format!("{shown}#{key}");
            self.list_op(op, &path, |p| Ok(self.display(*p))).map(Some)
        };
        prim.inherits = paths(arcs.inherits, "inheritPaths")?;
        prim.specializes = paths(arcs.specializes, "specializes")?;
        Ok(())
    }

    /// Variant selections, by set name as OpenUSD writes them.
    ///
    /// Spec: AOUSD Core §7.6.2.3.4 (`variantSelection`).
    fn selections(&self, selections: &HashMap<TokenId, TokenId>) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = selections
            .iter()
            .map(|(&set, &variant)| (self.name(set), self.name(variant)))
            .collect();
        out.sort();
        out
    }

    // ── Variants ────────────────────────────────────────────────────

    /// The variant sets `sets`, declared in `order`, of the prim at `host`,
    /// held by `owner`: the prim spec, or one of its variant specs for the
    /// sets nested there. Returns the `variantSets` list op naming every
    /// declared set, those without variants included, and the sets.
    ///
    /// Sets are written in `variantSets` order, then by name, and variants
    /// by name, as `Sdf_WriteVariantSet` sorts them. The list op is written
    /// as `prepend`, which adds the sets to weaker opinions as composition
    /// of the layer model does, with the set names `deleted` removes (a
    /// prim spec's [`PrimSpec::deleted_variant_sets`]).
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §7.6.2.3.5 (`variantSetNames`), §7.6.6–§7.6.7.
    fn variant_sets(
        &self,
        layer: &Layer,
        host: PathId,
        sets: &HashMap<TokenId, VariantSetSpec>,
        order: &[TokenId],
        deleted: &[TokenId],
        owner: Parent<'_>,
        visited: &mut Visited,
    ) -> Result<(Option<ListOp<String>>, Vec<VariantSet>), SaveError> {
        let mut order: Vec<TokenId> = order.to_vec();
        let mut unordered: Vec<TokenId> = sets
            .keys()
            .copied()
            .filter(|set| !order.contains(set))
            .collect();
        unordered.sort_by(|a, b| self.tokens.resolve(*a).cmp(self.tokens.resolve(*b)));
        order.extend(unordered);

        let mut names = Vec::new();
        let mut written = Vec::new();
        for set in order {
            names.push(self.name(set));
            let mut here: Vec<(TokenId, &VariantSpec)> = sets
                .get(&set)
                .into_iter()
                .flat_map(|set| &set.variants)
                .map(|(&name, variant)| (name, variant))
                .collect();
            if here.is_empty() {
                continue;
            }
            here.sort_by(|a, b| self.tokens.resolve(a.0).cmp(self.tokens.resolve(b.0)));
            let mut variants = Vec::with_capacity(here.len());
            for (name, variant) in here {
                variants.push(self.variant(layer, host, owner, set, name, variant, visited)?);
            }
            written.push(VariantSet {
                name: self.name(set),
                variants,
            });
        }
        let deleted: Vec<String> = deleted.iter().map(|set| self.name(*set)).collect();
        let names = (!names.is_empty() || !deleted.is_empty()).then(|| ListOp {
            deleted,
            ..ListOp::prepend(names)
        });
        Ok((names, written))
    }

    /// Lowers one variant of the prim at `host`, held by `owner`, to the
    /// prim spec it holds: its metadata, arcs, selections, properties,
    /// child prims (the prim specs the layer keeps in this branch) and the
    /// variant sets nested in it.
    ///
    /// Spec: AOUSD Core §7.3.6, §7.6.7 (variant specs), §10.3.2.5.
    #[allow(
        clippy::too_many_arguments,
        reason = "the variant, its set and where it is written"
    )]
    fn variant(
        &self,
        layer: &Layer,
        host: PathId,
        owner: Parent<'_>,
        set: TokenId,
        name: TokenId,
        variant: &VariantSpec,
        visited: &mut Visited,
    ) -> Result<Prim, SaveError> {
        let shown = branch_display(
            owner.shown,
            self.tokens.resolve(set),
            self.tokens.resolve(name),
        );
        let mut sites = owner.sites.to_vec();
        sites.push(VariantSelectionSite {
            host_path: host,
            set,
            variant: name,
        });
        let mut prim = Prim::new(WriterSpecifier::Over, None, self.name(name));
        prim.metadata = self.metadata(&variant.fields, &shown)?;
        let arcs = Arcs {
            references: &variant.references,
            payloads: &variant.payloads,
            inherits: &variant.inherits,
            specializes: &variant.specializes,
        };
        self.arcs(layer, arcs, &mut prim, &shown)?;
        prim.variant_selections = self.selections(&variant.variant_selections);
        prim.property_order = variant.property_order.as_ref().map(|o| self.names(o));
        for entry in &variant.properties {
            prim.properties.push(self.property(entry, &shown)?);
        }
        let branch = Parent {
            path: owner.path,
            shown: &shown,
            sites: &sites,
        };
        for &child in &variant.authored_children {
            prim.children
                .push(self.prim(layer, branch, child, visited)?);
        }
        (prim.variant_set_names, prim.variant_sets) = self.variant_sets(
            layer,
            host,
            &variant.variant_sets,
            &variant.variant_set_order,
            &[],
            branch,
            visited,
        )?;
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
        self.list_op(op, path, |t| Ok(t.display(self.paths, self.tokens)))
    }

    /// A reference or payload as authored: its asset path, never the layer
    /// it resolved to (an unresolved arc keeps its asset path too), its
    /// prim path or none for the `defaultPrim`, and its layer offset. An
    /// arc into `layer` itself without an asset path is internal.
    fn arc(&self, layer: &Layer, arc: &LayerReference, prim: &str) -> Result<Reference, SaveError> {
        let asset = match &arc.asset {
            Some(asset) => Some(asset.clone()),
            None if arc.layer == layer.id => None,
            None => return invalid(prim, Invalid::ArcWithoutAsset),
        };
        let prim_path = match arc.target {
            ReferenceTarget::Prim(path) => Some(self.display(path)),
            ReferenceTarget::DefaultPrim => None,
        };
        Ok(Reference {
            asset,
            prim_path,
            offset: layer_offset(arc.layer_offset),
        })
    }

    /// Converts a list op, keeping each list. OpenUSD's list ops hold an
    /// explicit list or edits, never both (AOUSD Core §6.6.3).
    fn list_op<T, U>(
        &self,
        op: &LayerListOp<T>,
        path: &str,
        item: impl Fn(&T) -> Result<U, SaveError>,
    ) -> Result<ListOp<U>, SaveError> {
        let edits = !(op.prepend.is_empty() && op.append.is_empty() && op.delete.is_empty());
        if op.explicit.is_some() && edits {
            return unsupported(path, Unsupported::MixedListOp);
        }
        let list = |items: &[T]| items.iter().map(&item).collect::<Result<Vec<_>, _>>();
        Ok(ListOp {
            explicit: op.explicit.as_deref().map(list).transpose()?,
            deleted: list(&op.delete)?,
            prepended: list(&op.prepend)?,
            appended: list(&op.append)?,
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
                    Value::TokenListOp(self.list_op(op, &path, |t| Ok(self.name(*t)))?)
                }
                FieldValue::StringListOp(op) => {
                    Value::StringListOp(self.list_op(op, &path, |s| Ok(String::from(&**s)))?)
                }
                FieldValue::IntListOp(op) => Value::IntListOp(self.list_op(op, &path, |x| Ok(*x))?),
                FieldValue::UIntListOp(op) => {
                    Value::UIntListOp(self.list_op(op, &path, |x| Ok(*x))?)
                }
                FieldValue::Int64ListOp(op) => {
                    Value::Int64ListOp(self.list_op(op, &path, |x| Ok(*x))?)
                }
                FieldValue::UInt64ListOp(op) => {
                    Value::UInt64ListOp(self.list_op(op, &path, |x| Ok(*x))?)
                }
                FieldValue::PathListOp(_) => {
                    return unsupported(path, Unsupported::ListOpMetadata("path list op"));
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
            L::UChar(v) => Value::UChar(*v),
            L::Int(v) => Value::Int(*v),
            L::UInt(v) => Value::UInt(*v),
            L::Int64(v) => Value::Int64(*v),
            L::UInt64(v) => Value::UInt64(*v),
            L::Half(v) => Value::Half(*v),
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
            L::Vec2h(v) => Value::Half2(*v),
            L::Vec3h(v) => Value::Half3(*v),
            L::Vec4h(v) => Value::Half4(*v),
            // Both hold a quaternion as `i, j, k, r`.
            L::Quath(q) => Value::Quath(*q),
            L::Quatf(q) => Value::Quatf(*q),
            L::Quatd(q) => Value::Quatd(*q),
            L::Matrix2d(m) => Value::Matrix2d(rows(m)),
            L::Matrix3d(m) => Value::Matrix3d(rows(m)),
            L::Matrix4d(m) => Value::Matrix4d(rows(m)),
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
        && spec.deleted_variant_sets.is_empty()
        && !is_authored(&spec.references)
        && !is_authored(&spec.payloads)
        && !is_authored(&spec.inherits)
        && !is_authored(&spec.specializes)
        && spec.active.is_none()
        && spec.instanceable.is_none()
        && spec.outer_variant_sites.is_empty()
}

/// The prim specs reached so far, by path and branch context.
type Visited = HashSet<(PathId, Vec<VariantSelectionSite>)>;

/// Where a prim spec or variant is written: the namespace path of the prim
/// spec it belongs to, its variant-qualified display path, and the variant
/// branches enclosing it.
#[derive(Clone, Copy)]
struct Parent<'a> {
    path: &'a Path,
    shown: &'a str,
    sites: &'a [VariantSelectionSite],
}

/// The arc list ops of a prim spec or variant.
#[derive(Clone, Copy)]
struct Arcs<'a> {
    references: &'a LayerListOp<LayerReference>,
    payloads: &'a LayerListOp<LayerReference>,
    inherits: &'a LayerListOp<PathId>,
    specializes: &'a LayerListOp<PathId>,
}

/// The display path of child prim `name` of `parent` (`/`, `/A` or
/// `/A{v=x}`): a prim inside a variant follows its selection directly.
fn child_display(parent: &str, name: &str) -> String {
    if parent.ends_with('}') {
        format!("{parent}{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

/// The display path of variant `variant` of set `set` on `owner`.
fn branch_display(owner: &str, set: &str, variant: &str) -> String {
    format!("{owner}{{{set}={variant}}}")
}

fn layer_offset(offset: LayerLayerOffset) -> LayerOffset {
    LayerOffset {
        offset: offset.offset,
        scale: offset.scale,
    }
}

/// The empty array of `element`'s type, if the writers have one.
fn empty_array_like(element: &Value) -> Option<Value> {
    Value::empty_array_of(&format!("{}[]", element.canonical_type_name()))
}

/// A row-major `N`×`N` matrix from its flat entries.
fn rows<const N: usize, const M: usize>(m: &[f64; M]) -> [[f64; N]; N] {
    core::array::from_fn(|r| core::array::from_fn(|c| m[r * N + c]))
}

/// Appends `element` to `array` when it has the array's element type.
fn push_element(array: &mut Value, element: Value) -> bool {
    match (array, element) {
        (Value::BoolArray(a), Value::Bool(v)) => a.push(v),
        (Value::UCharArray(a), Value::UChar(v)) => a.push(v),
        (Value::IntArray(a), Value::Int(v)) => a.push(v),
        (Value::UIntArray(a), Value::UInt(v)) => a.push(v),
        (Value::Int64Array(a), Value::Int64(v)) => a.push(v),
        (Value::UInt64Array(a), Value::UInt64(v)) => a.push(v),
        (Value::HalfArray(a), Value::Half(v)) => a.push(v),
        (Value::Half2Array(a), Value::Half2(v)) => a.push(v),
        (Value::Half3Array(a), Value::Half3(v)) => a.push(v),
        (Value::Half4Array(a), Value::Half4(v)) => a.push(v),
        (Value::QuathArray(a), Value::Quath(v)) => a.push(v),
        (Value::QuatfArray(a), Value::Quatf(v)) => a.push(v),
        (Value::QuatdArray(a), Value::Quatd(v)) => a.push(v),
        (Value::Matrix2dArray(a), Value::Matrix2d(v)) => a.push(v),
        (Value::Matrix3dArray(a), Value::Matrix3d(v)) => a.push(v),
        (Value::Matrix4dArray(a), Value::Matrix4d(v)) => a.push(v),
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
