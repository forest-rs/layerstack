// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene assembly: USDC sections → [`Layer`] / [`PrimSpec`].
//!
//! Converts the flat spec/field/path tables decoded from the six USDC sections
//! into a layerstack [`Layer`] with [`PrimSpec`]s, analogous to how
//! `layerstack_usda::emit` converts a USDA AST.
//!
//! Spec: AOUSD Core §16.3 (crate binary format), §6–§7 (scene description
//! data model and opinions).
//!
//! [`Layer`]: layerstack::doc::Layer
//! [`PrimSpec`]: layerstack::doc::PrimSpec

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{HashMap, HashSet};

use layerstack::doc::{
    FieldValue, Layer, LayerId, LayerOffset, PrimSpec, Reference, Specifier, SublayerEntry, Value,
    VariantSpec, set_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner, PropertyPath, TargetPath};
use layerstack::property::{
    PropertyEntry, PropertyKind, PropertySpec, Variability, property_entry, set_property_vec,
};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::variable_expression::is_expression;
use layerstack::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};
use layerstack::{AssetResolver, PropertyType, ReferenceTarget, ResolvedAsset};

use crate::error::UsdcError;
use crate::numeric_array::*;
use crate::section::{CrateSections, PathNode};
use crate::value_rep::{
    CrateArrayEdit, CrateArrayEditOp, CrateListOp, CrateValue, DecodeBudget, DecodedField,
    FloatArray, ListOpParts, RawValueRep, decode_field_within, decode_path_list_op_indices,
    decode_text_indices, decode_token_vector_indices,
};
use crate::value_type::{SpecForm, ValueType};

/// Result of assembling a USDC file into a layer.
#[derive(Debug)]
pub struct AssembleResult {
    /// The assembled layer.
    pub layer: Layer,
    /// Layers produced by resolving asset paths (sublayers, references,
    /// payloads). The caller should insert these into their store.
    pub resolved_layers: Vec<Layer>,
    /// Authored content the layer model could not represent.
    ///
    /// Assembly reports what it leaves out instead of dropping it silently;
    /// an empty list means every spec and field was represented.
    pub diagnostics: Vec<AssembleDiagnostic>,
}

/// Authored content that assembly could not represent in the layer model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembleDiagnostic {
    /// Path of the spec as written in the file (for example `/A.b` or
    /// `/A{v=x}B`).
    pub spec_path: String,
    /// The field concerned, when the problem is one field of the spec.
    pub field: Option<String>,
    /// What was not represented, and why.
    pub message: String,
}

/// Assembles decoded USDC sections into a [`Layer`].
///
/// `data` is the full file byte slice (needed for offset-based value reads).
/// `sections` holds the decoded token/string/path/field/spec tables.
/// Everything assembly materializes is charged to `budget` (see
/// [`DecodeBudget`]): every value decoded, including each repeat of a value
/// that several fields share, and every use of a field name or spec path,
/// which many specs may share.
///
/// Spec: AOUSD Core §16.3.
pub fn assemble(
    data: &[u8],
    sections: &CrateSections,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    budget: &mut DecodeBudget,
) -> Result<AssembleResult, UsdcError> {
    let token_ids = TokenIds::for_read(sections);
    let mut ctx = AssembleCtx {
        data,
        sections,
        tokens,
        token_ids,
        paths,
        target_paths: HashMap::new(),
        prim_paths: Vec::new(),
        owners: Vec::new(),
        targets: Vec::new(),
        defer_variant_arcs: resolver.defers_variant_arcs(),
        in_branch: false,
        in_payloads: false,
        resolver,
        layer_id,
        resolved_layers: Vec::new(),
        diagnostics: Vec::new(),
        budget,
    };

    let layer = ctx.assemble_layer()?;

    Ok(AssembleResult {
        layer,
        resolved_layers: ctx.resolved_layers,
        diagnostics: ctx.diagnostics,
    })
}

// ---------------------------------------------------------------------------
// Internal context
// ---------------------------------------------------------------------------

struct AssembleCtx<'a> {
    data: &'a [u8],
    sections: &'a CrateSections,
    tokens: &'a TokenInterner,
    /// The token ids of the file's token table, by index.
    token_ids: TokenIds,
    paths: &'a PathInterner,
    /// Relationship targets and connections parsed so far, by their text:
    /// many specs name the same targets (a material, a collection).
    target_paths: HashMap<String, Option<TargetPath>>,
    /// What each entry of the path table names as a prim path outside
    /// variant branches, found from the path tree when first asked
    /// ([`Self::prim_path_at`]).
    prim_paths: Vec<PrimPathAt>,
    /// Whether each entry of the path table, as the prim path text of a
    /// property path, is one [`PropertyPath::split`] and
    /// [`Path::parse_absolute`] accept: `0` unknown, `1` yes, `2` no.
    owners: Vec<u8>,
    /// The target path each entry of the path table names, found when
    /// first asked ([`Self::target_path_at`]).
    targets: Vec<TargetAt>,
    resolver: &'a mut dyn AssetResolver,
    /// Whether arcs inside variant branches are left for composition to
    /// resolve ([`AssetResolver::defers_variant_arcs`]).
    defer_variant_arcs: bool,
    /// Whether the arcs being converted are authored inside a variant
    /// branch.
    in_branch: bool,
    /// Whether the arcs being converted are payloads.
    in_payloads: bool,
    layer_id: LayerId,
    resolved_layers: Vec<Layer>,
    diagnostics: Vec<AssembleDiagnostic>,
    /// What the read may still materialize.
    budget: &'a mut DecodeBudget,
}

/// Where a property spec belongs, by its path ([`AssembleCtx::property_owner`]).
enum PropertyOwner {
    /// On a variant spec: placed with the variant specs.
    Variant,
    /// A path whose prim path text does not parse.
    Invalid,
    /// On the prim spec at path table entry `owner`, named `name`.
    Prim { owner: u32, name: TokenId },
}

/// What one entry of the path table names as a target path.
#[derive(Clone, Copy)]
enum TargetAt {
    Unknown,
    Found(Option<TargetPath>),
}

/// What one entry of the path table names as a prim path outside variant
/// branches.
#[derive(Clone, Copy)]
enum PrimPathAt {
    Unknown,
    /// Its text contains a variant selection, or does not parse.
    None,
    Some(PathId),
}

/// The prim specs a read builds, by their path's text, and by the path
/// table index of each spec's path, in the order their paths were first
/// spelled. Each spec is boxed, so it stays where it was built, in the
/// layer too ([`Layer::insert_prims`]).
#[expect(
    clippy::vec_box,
    reason = "a boxed spec moves into the layer as a pointer, without a copy"
)]
struct PrimSpecs<'a> {
    by_path: HashMap<&'a str, usize>,
    specs: Vec<Box<PrimSpec>>,
    /// The text and path table index each of `specs` was first placed
    /// with.
    placed: Vec<(&'a str, u32)>,
    /// The place in `specs` of the spec at each path table index; `NONE`
    /// where no prim spec's path is.
    at_index: Vec<u32>,
}

impl<'a> PrimSpecs<'a> {
    const NONE: u32 = u32::MAX;

    fn new(prims: usize, paths: usize) -> Self {
        Self {
            by_path: HashMap::with_capacity(prims),
            specs: Vec::with_capacity(prims),
            placed: Vec::with_capacity(prims),
            at_index: alloc::vec![Self::NONE; paths],
        }
    }

    /// Sets the spec at `path`, the text of path table entry `index`,
    /// replacing any there.
    fn insert(&mut self, index: u32, path: &'a str, spec: Box<PrimSpec>) {
        let at = match self.by_path.get(path) {
            Some(&at) => {
                self.specs[at] = spec;
                at
            }
            None => {
                let at = self.specs.len();
                self.by_path.insert(path, at);
                self.specs.push(spec);
                self.placed.push((path, index));
                at
            }
        };
        if let (Some(slot), Ok(at)) = (self.at_index.get_mut(index as usize), u32::try_from(at)) {
            *slot = at;
        }
    }

    fn contains_key(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    fn get_mut(&mut self, path: &str) -> Option<&mut PrimSpec> {
        let at = *self.by_path.get(path)?;
        self.specs.get_mut(at).map(|spec| &mut **spec)
    }

    /// The spec at path table entry `index`, whose text is `path`.
    fn get_mut_at(&mut self, index: u32, path: &str) -> Option<&mut PrimSpec> {
        match self.at_index.get(index as usize) {
            Some(&at) if at != Self::NONE => {
                self.specs.get_mut(at as usize).map(|spec| &mut **spec)
            }
            // Another entry may spell the same path.
            _ => self.get_mut(path),
        }
    }

    /// The text and path table index each spec was first placed with, in
    /// the order of [`PrimSpecs::into_specs`].
    fn placed(&self) -> &[(&'a str, u32)] {
        &self.placed
    }

    /// The specs, in the order their paths were first spelled.
    #[expect(clippy::vec_box, reason = "the specs move into the layer boxed")]
    fn into_specs(self) -> Vec<Box<PrimSpec>> {
        self.specs
    }
}

/// A spec's fields: names borrowed from the token table, and their values.
type Fields<'a> = Vec<(&'a str, DecodedField<'a>)>;

/// The token id of each entry of a file's token table, so that a `token[]`
/// value converts by index, without copying or hashing its elements' text.
enum TokenIds {
    /// Each entry interned when a value first uses it.
    OnUse(Vec<Option<TokenId>>),
}

impl TokenIds {
    fn for_read(sections: &CrateSections) -> Self {
        Self::OnUse(alloc::vec![None; sections.tokens.len()])
    }
}

impl<'a> AssembleCtx<'a> {
    /// Records content that could not be represented.
    ///
    /// A diagnostic copies the spec path, and a spec may report once per
    /// field, so each diagnostic is charged for its text.
    fn report(
        &mut self,
        spec_path: &str,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Result<(), UsdcError> {
        let message = message.into();
        self.budget.charge(1)?;
        self.budget
            .charge_text(spec_path.len() + field.map_or(0, str::len) + message.len())?;
        self.diagnostics.push(AssembleDiagnostic {
            spec_path: String::from(spec_path),
            field: field.map(String::from),
            message,
        });
        Ok(())
    }

    /// Appends one `layerRelocates` entry to `layer`. An empty target path
    /// removes the source; an entry whose paths are not prim paths (a
    /// variant selection, a property, a relative path) is reported instead.
    ///
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`).
    fn push_relocate(
        &mut self,
        layer: &mut Layer,
        source: &str,
        target: &str,
    ) -> Result<(), UsdcError> {
        let prim_path = |ctx: &mut Self, text: &str| {
            let names = text.strip_prefix('/')?;
            let is_name = |name: &str| {
                let mut chars = name.chars();
                let allowed = |c: char| {
                    c == '_' || c.is_ascii_alphanumeric() || (!c.is_ascii() && !c.is_whitespace())
                };
                chars
                    .next()
                    .is_some_and(|first| !first.is_ascii_digit() && allowed(first))
                    && chars.all(allowed)
            };
            if !names.split('/').all(is_name) {
                return None;
            }
            let path = Path::parse_absolute(text, ctx.tokens).ok()?;
            Some(ctx.paths.intern(path))
        };
        let source_path = prim_path(self, source);
        let target_path = if target.is_empty() {
            Some(None)
        } else {
            prim_path(self, target).map(Some)
        };
        match (source_path, target_path) {
            (Some(source), Some(target)) => {
                layer
                    .relocates_mut()
                    .push(layerstack::Relocate { source, target });
                Ok(())
            }
            _ => self.report(
                "/",
                Some("layerRelocates"),
                alloc::format!("relocate `<{source}>: <{target}>` is not between prim paths"),
            ),
        }
    }

    /// Assembles all specs into a [`Layer`].
    fn assemble_layer(&mut self) -> Result<Layer, UsdcError> {
        let mut layer = Layer::new(self.layer_id);

        // Build a map from prim path string → PrimSpec being constructed.
        // We need to process specs in a specific order:
        // 1. PseudoRoot first (layer metadata)
        // 2. Prim specs (create PrimSpecs)
        // 3. Attribute/Relationship specs (add fields to parent prims)
        // 4. VariantSet/Variant specs (handle variant structures)

        // Reuse one field vector and decode each spec when its assembly
        // pass reaches it, so intermediate arrays do not remain alive alongside
        // the entire assembled layer.
        // Track visits to validate skipped/unsupported specs once at the end:
        // bulk import still checks every authored field (AOUSD Core §16.3).
        self.budget.charge_elements(self.sections.specs.len())?;
        let mut decoded = alloc::vec![false; self.sections.specs.len()];
        let mut fields = Vec::new();

        // Build a path-to-spec-index map grouped by prim path for child lookup.
        let prims = self.sections.specs.iter();
        let prims = prims.filter(|spec| spec.form == SpecForm::Prim).count();
        let mut prim_specs_map = PrimSpecs::new(prims, self.sections.paths.len());
        // Authored property order (`propertyChildren`) per prim path.
        let mut property_children: HashMap<&str, Vec<TokenId>> = HashMap::new();

        // Process PseudoRoot specs first.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::PseudoRoot {
                self.collect_spec_fields(i, &mut decoded, &mut fields)?;
                self.process_pseudo_root(&fields, &mut layer)?;
            }
        }

        // Process Prim specs.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Prim {
                let path_str = self.lookup_path(spec.path_index)?;
                // Prims inside variant branches (`/A{v=x}B`) keep their
                // variant-qualified key until they are placed in the layer.
                if path_str.contains('{') && split_branch_path(path_str).is_none() {
                    self.report(path_str, None, "prim spec path could not be parsed")?;
                    continue;
                }
                self.collect_spec_fields(i, &mut decoded, &mut fields)?;
                let (prim, children) = self.build_prim_spec(path_str, &fields)?;
                if let Some(children) = children {
                    property_children.insert(path_str, children);
                }
                prim_specs_map.insert(spec.path_index, path_str, Box::new(prim));
            }
        }

        // Process Attribute and Relationship specs — add them to their
        // parent prim. Specs under variant paths are handled with the
        // variant specs below.
        // Whether each prim path text properties are authored on parses, as
        // `PropertyPath::parse` requires; checked once per prim.
        let mut prim_paths_valid: HashMap<&str, bool> = HashMap::new();
        for (i, spec) in self.sections.specs.iter().enumerate() {
            let is_attribute = spec.form == SpecForm::Attribute;
            if !is_attribute && spec.form != SpecForm::Relationship {
                continue;
            }
            let path_str = self.lookup_path(spec.path_index)?;
            if let Some(owner) = self.property_owner(spec.path_index) {
                let (owner, name) = match owner {
                    PropertyOwner::Variant => continue,
                    PropertyOwner::Invalid => {
                        self.report(path_str, None, "property spec path could not be parsed")?;
                        continue;
                    }
                    PropertyOwner::Prim { owner, name } => (owner, name),
                };
                let owner_path = &self.sections.paths[owner as usize];
                let Some(prim) = prim_specs_map.get_mut_at(owner, owner_path) else {
                    self.report(path_str, None, "property spec has no owning prim spec")?;
                    continue;
                };
                self.collect_spec_fields(i, &mut decoded, &mut fields)?;
                if is_attribute {
                    self.apply_attribute_fields(path_str, &fields, name, prim)?;
                } else {
                    self.apply_relationship_fields(path_str, &fields, name, prim)?;
                }
                continue;
            }
            let (prim_path, name) = if path_str.contains('{') {
                // A property directly on a variant (`/A{v=x}.b`) belongs to
                // the variant spec and is handled with the variant specs;
                // one on a prim inside a branch (`/A{v=x}B.c`) is handled
                // here.
                match path_str.rsplit_once('.') {
                    Some((prim, _)) if prim.ends_with('}') => continue,
                    Some((prim, name)) => (prim, name),
                    None => {
                        self.report(path_str, None, "property spec path could not be parsed")?;
                        continue;
                    }
                }
            } else {
                // Validate without normalizing the spelling: borrow the
                // original text instead of formatting a parsed path back.
                let Some(parts) = self.split_property_path(path_str, &mut prim_paths_valid) else {
                    self.report(path_str, None, "property spec path could not be parsed")?;
                    continue;
                };
                parts
            };
            let Some(prim) = prim_specs_map.get_mut(prim_path) else {
                self.report(path_str, None, "property spec has no owning prim spec")?;
                continue;
            };
            self.collect_spec_fields(i, &mut decoded, &mut fields)?;
            let name = self.tokens.intern(name);
            if is_attribute {
                self.apply_attribute_fields(path_str, &fields, name, prim)?;
            } else {
                self.apply_relationship_fields(path_str, &fields, name, prim)?;
            }
        }

        // Process Connection specs — add connection paths to attribute on parent.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Connection {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Some((prim_path, attr_name)) =
                    self.split_property_path(path_str, &mut prim_paths_valid)
                    && let Some(prim) = prim_specs_map.get_mut(prim_path)
                {
                    self.collect_spec_fields(i, &mut decoded, &mut fields)?;
                    self.apply_connection_fields(&fields, attr_name, prim)?;
                }
            }
        }

        // Process VariantSet and Variant specs.
        self.process_variant_specs(&mut decoded, &mut fields, &mut prim_specs_map)?;

        // Validate fields not consumed above, including malformed paths and
        // specs the layer model does not hold, and report unsupported forms.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if decoded[i] {
                continue;
            }
            self.collect_spec_fields(i, &mut decoded, &mut fields)?;
            let handled = matches!(
                spec.form,
                SpecForm::PseudoRoot
                    | SpecForm::Prim
                    | SpecForm::Attribute
                    | SpecForm::Relationship
                    | SpecForm::Connection
                    | SpecForm::VariantSet
                    | SpecForm::Variant
            );
            if !handled && !fields.is_empty() {
                let path_str = self.lookup_path(spec.path_index)?;
                self.report(
                    path_str,
                    None,
                    alloc::format!("unsupported: {:?} spec fields are not read", spec.form),
                )?;
            }
        }

        drop(fields);

        // Keep the authored property order (`propertyChildren`).
        //
        // Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`).
        for (path, order) in &property_children {
            if let Some(prim) = prim_specs_map.get_mut(path) {
                sort_by_children(&mut prim.properties, order);
            }
        }

        // Establish parent-child relationships.
        self.build_child_relationships(&mut prim_specs_map);

        // Convert prim_specs_map into layer prims. A prim inside variant
        // branches goes to its namespace path with its branch context, so
        // each branch keeps its own spec (`Layer::insert_prim`).
        //
        // Spec: AOUSD Core §7.3.6 (variant specs contain prim specs).
        // In the file's spec order, so paths intern in the same order on
        // every read, and every spec outside variant branches first: a
        // branch spec then joins the namespace spec at its path, as
        // `Layer::insert_prim` keeps them. Inserted as one change, so the
        // layer's map grows once.
        // Each spec is taken by the first spec spelling its path, which
        // placed it, so the specs are in the file's order of those specs.
        let placed: Vec<(&str, Option<PathId>)> = prim_specs_map
            .placed()
            .iter()
            .map(|&(path_str, index)| {
                let path = if path_str.contains('{') {
                    None
                } else {
                    self.prim_path_at(index)
                };
                (path_str, path)
            })
            .collect();
        let mut branched: Vec<(&str, Box<PrimSpec>)> = Vec::new();
        let plain = prim_specs_map
            .into_specs()
            .into_iter()
            .zip(&placed)
            .filter_map(|(prim, &(path_str, path))| {
                if path_str.contains('{') {
                    branched.push((path_str, prim));
                    return None;
                }
                Some((path?, prim))
            });
        layer.insert_prims(plain);
        branched.sort_unstable_by_key(|(path, _)| *path);
        for (path_str, mut prim) in branched {
            let namespace = match split_branch_path(path_str) {
                Some((namespace, sites)) => {
                    prim.outer_variant_sites = self.branch_sites(&namespace, &sites)?;
                    Cow::Owned(namespace)
                }
                None => Cow::Borrowed(path_str),
            };
            if let Ok(path) = Path::parse_absolute(&namespace, self.tokens) {
                let path_id = self.paths.intern(path);
                layer.insert_prim(path_id, *prim);
            }
        }

        if self.defer_variant_arcs {
            let resolved = layerstack::resolve_selected_deferred_arcs(
                &mut layer,
                self.resolver,
                self.tokens,
                self.paths,
            );
            self.resolved_layers.extend(resolved);
        }
        Ok(layer)
    }

    /// Converts the variant selections of a crate path, as split by
    /// [`split_branch_path`], into sites whose host paths lie in `namespace`.
    ///
    /// Each selection's host path is parsed and interned, so a path with many
    /// selections is charged for each.
    fn branch_sites(
        &mut self,
        namespace: &str,
        sites: &[BranchSite<'_>],
    ) -> Result<Vec<VariantSelectionSite>, UsdcError> {
        let mut out = Vec::with_capacity(sites.len());
        for &(host_len, set, variant) in sites {
            self.budget.charge(1)?;
            self.budget.charge_text(host_len)?;
            let Ok(host) = Path::parse_absolute(&namespace[..host_len], self.tokens) else {
                continue;
            };
            out.push(VariantSelectionSite {
                host_path: self.paths.intern(host),
                set: self.tokens.intern(set),
                variant: self.tokens.intern(variant),
            });
        }
        Ok(out)
    }

    /// Decodes a spec once, retaining only a flag after its fields are consumed.
    /// Spec allocation is charged before the visit flags are allocated.
    fn collect_spec_fields(
        &mut self,
        index: usize,
        decoded: &mut [bool],
        fields: &mut Fields<'a>,
    ) -> Result<(), UsdcError> {
        debug_assert!(!decoded[index], "spec decoded twice");
        decoded[index] = true;
        let spec = &self.sections.specs[index];
        let retain_default = spec.form == SpecForm::Attribute;
        self.collect_fields(spec.fieldset_index, retain_default, fields)
    }

    /// Collects decoded fields for a spec from the fieldsets/fields tables.
    ///
    /// Field names are borrowed from the token table: specs sharing a
    /// fieldset share its names. Each use is still charged by the name's
    /// length, since assembly matches, interns or reports it.
    fn collect_fields(
        &mut self,
        fieldset_index: u32,
        retain_default: bool,
        result: &mut Fields<'a>,
    ) -> Result<(), UsdcError> {
        result.clear();
        let start = fieldset_index as usize;

        if start >= self.sections.fieldsets.len() {
            return Ok(());
        }

        // Walk the fieldset array from the start index until we hit a
        // negative delimiter or end of array.
        let mut idx = start;
        while idx < self.sections.fieldsets.len() {
            let field_idx = self.sections.fieldsets[idx];
            if field_idx < 0 {
                break; // End of group.
            }
            let fi = field_idx as usize;
            if fi < self.sections.fields.len() {
                let field_def = &self.sections.fields[fi];
                let field_name = self.lookup_token(field_def.token_index);
                self.budget.charge_text(field_name.len())?;
                let rep = RawValueRep::new(field_def.value_rep);
                #[cfg(feature = "std")]
                if retain_default
                    && field_name == "default"
                    && self.budget.lazy.is_some()
                    && crate::lazy::retains_text_array(&rep, self.data)
                {
                    result.push((field_name, DecodedField::Retained(rep)));
                    idx += 1;
                    continue;
                }
                let _ = retain_default;
                let value = if is_token_array(&rep) {
                    DecodedField::TokenArray(decode_text_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else if is_token_vector(&rep) {
                    DecodedField::TokenArray(decode_token_vector_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else if matches!(field_name, "targetPaths" | "connectionPaths")
                    && is_path_list_op(&rep)
                {
                    // Targets are named through the path tree
                    // (`Self::target_path_at`), not their text.
                    DecodedField::PathListOp(decode_path_list_op_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else {
                    decode_field_within(&rep, self.data, self.sections, self.budget)?
                };
                // This is the ownership boundary: assembly subsequently borrows
                // fields, so retain decoded numeric allocations before that borrow.
                // Raw math arrays already borrow the file bytes and are converted
                // once by assembly; the generic decoder remains unchanged.
                let value = match value {
                    DecodedField::FloatArray(array) => {
                        DecodedField::Value(CrateValue::TypedArray(float_array(array)))
                    }
                    other => other,
                };
                result.push((field_name, value));
            }
            idx += 1;
        }

        Ok(())
    }

    /// Processes a `PseudoRoot` spec: sublayers, root prim order and layer
    /// metadata.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields).
    fn process_pseudo_root(
        &mut self,
        fields: &[(&str, DecodedField<'_>)],
        layer: &mut Layer,
    ) -> Result<(), UsdcError> {
        let mut root_children = Vec::new();
        let mut prim_order: Option<Vec<TokenId>> = None;
        let mut sublayer_paths: &[String] = &[];
        let mut sublayer_offsets: &[(f64, f64)] = &[];

        for (name, value) in fields {
            match *name {
                // OpenUSD stores `subLayers` as a `std::vector<std::string>`
                // (`StringVector`) and `subLayerOffsets` as a
                // `LayerOffsetVector` parallel to it.
                "subLayers" => {
                    if let Some(CrateValue::PathVector(paths) | CrateValue::StringVector(paths)) =
                        value.value()
                    {
                        sublayer_paths = paths;
                    }
                }
                "subLayerOffsets" => {
                    if let Some(CrateValue::LayerOffsetVector(offsets)) = value.value() {
                        sublayer_offsets = offsets;
                    }
                }
                "primChildren" => {
                    root_children = self.extract_token_names(value);
                }
                "primOrder" => {
                    prim_order = Some(self.extract_token_names(value));
                }
                "defaultPrim" => {
                    if let Some(CrateValue::Token(name)) = value.value() {
                        layer.set_default_prim(Some(self.tokens.intern(name)));
                    }
                }
                "layerRelocates" => {
                    // Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`),
                    // §16.3.10.15 (crate relocates values).
                    if let Some(CrateValue::RelocatesMap(pairs)) = value.value() {
                        for (source, target) in pairs {
                            self.push_relocate(layer, source, target)?;
                        }
                    }
                }
                "relocates" => {
                    // Prim relocates are legacy scene description that
                    // OpenUSD ignores when composing a USD stage
                    // (`Pcp_ComputeRelocationsForLayerStack` in
                    // `pxr/usd/pcp/layerStack.cpp`).
                    self.report("/", Some(name), "unsupported: prim relocates are ignored")?;
                }
                _ => {
                    if let Some(field_value) = self.convert_metadata("/", name, value)? {
                        let key = self.tokens.intern(name);
                        set_field_vec(layer.metadata_fields_mut(), key, field_value);
                    }
                }
            }
        }

        // Spec: AOUSD Core §12.3.2.1 (sublayer offsets). A sublayer that
        // does not resolve keeps its place; composition reports it
        // (§10.3.1, §10.6; OpenUSD `PcpErrorInvalidSublayerPath`).
        for (i, asset_path) in sublayer_paths.iter().enumerate() {
            let offset =
                sublayer_offsets
                    .get(i)
                    .map_or(LayerOffset::IDENTITY, |&(offset, scale)| LayerOffset {
                        offset,
                        scale,
                    });
            let Some(resolved) = self.resolve_asset(asset_path) else {
                layer
                    .sublayers_mut()
                    .push(SublayerEntry::unresolved(asset_path.as_str(), offset));
                continue;
            };
            layer.sublayers_mut().push(SublayerEntry::with_asset(
                resolved.layer_id,
                asset_path.as_str(),
                offset,
            ));
            if let Some(sub_layer) = resolved.layer {
                self.resolved_layers.push(sub_layer);
            }
        }

        // Create a root prim spec for child ordering.
        if !root_children.is_empty() || prim_order.is_some() {
            let root_path = Path::root();
            let root_path_id = self.paths.intern(root_path);
            let root_spec = PrimSpec {
                authored_children: root_children,
                prim_order,
                ..PrimSpec::default()
            };
            layer.insert_prim(root_path_id, root_spec);
        }

        Ok(())
    }

    /// Builds a [`PrimSpec`] from decoded fields, also returning the
    /// authored property order (`propertyChildren`) when present.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    fn build_prim_spec(
        &mut self,
        spec_path: &str,
        fields: &[(&str, DecodedField<'_>)],
    ) -> Result<(PrimSpec, Option<Vec<TokenId>>), UsdcError> {
        let mut spec = PrimSpec::default();
        let mut property_children = None;
        self.in_branch = spec_path.contains('{');

        for (name, value) in fields {
            match *name {
                "specifier" => {
                    if let Some(CrateValue::Specifier(v)) = value.value() {
                        spec.specifier = Some(match v {
                            0 => Specifier::Def,
                            1 => Specifier::Over,
                            2 => Specifier::Class,
                            _ => Specifier::Def,
                        });
                    }
                }
                "typeName" => {
                    if let Some(CrateValue::Token(t)) = value.value()
                        && !t.is_empty()
                    {
                        spec.type_name = Some(self.tokens.intern(t));
                    }
                }
                "primChildren" => {
                    spec.authored_children = self.extract_token_names(value);
                }
                "primOrder" => {
                    spec.prim_order = Some(self.extract_token_names(value));
                }
                // `SdfChildrenKeys->PropertyChildren` is spelled `properties`
                // (`pxr/usd/sdf/schema.h`).
                "properties" => {
                    property_children = Some(self.extract_token_names(value));
                }
                "propertyOrder" => {
                    spec.property_order = Some(self.extract_token_names(value));
                }
                // Derived from the variant set specs themselves.
                "variantSetChildren" => {}
                "references" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value() {
                        let converted = self.convert_ref_listop(listop)?;
                        spec.references.merge(converted);
                    }
                }
                // Spec: AOUSD Core §7.6.2.3.2 (`payload`).
                "payload" => {
                    if let Some(converted) = self.convert_payload_value(value)? {
                        spec.payloads.merge(converted);
                    }
                }
                // Spec: AOUSD Core §7.6.2.3.3 (`inheritPaths`).
                "inheritPaths" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value() {
                        let converted = self.convert_path_listop(listop)?;
                        spec.inherits.merge(converted);
                    }
                }
                "specializes" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value() {
                        let converted = self.convert_path_listop(listop)?;
                        spec.specializes.merge(converted);
                    }
                }
                "variantSelection" => {
                    if let Some(CrateValue::VariantSelectionMap(pairs)) = value.value() {
                        for (set_name, branch_name) in pairs {
                            let set_tok = self.tokens.intern(set_name);
                            let branch_tok = self.tokens.intern(branch_name);
                            spec.variant_selections.insert(set_tok, branch_tok);
                        }
                    }
                }
                "variantSetNames" => {
                    spec.variant_set_names = self.variant_set_names(value);
                }
                "instanceable" if matches!(value.value(), Some(CrateValue::Bool(_))) => {
                    if let Some(CrateValue::Bool(b)) = value.value() {
                        spec.instanceable = Some(*b);
                    }
                }
                "active" if matches!(value.value(), Some(CrateValue::Bool(_))) => {
                    if let Some(CrateValue::Bool(b)) = value.value() {
                        spec.active = Some(*b);
                    }
                }
                "relocates" => {
                    // Prim relocates are legacy scene description that
                    // OpenUSD ignores when composing a USD stage; only
                    // `layerRelocates` is composed (AOUSD Core §10.3.2.6).
                    self.report(
                        spec_path,
                        Some(name),
                        "unsupported: prim relocates are ignored",
                    )?;
                }
                _ => {
                    // Generic metadata field (`kind`, `documentation`,
                    // `apiSchemas`, `customData`, `hidden`, …).
                    if let Some(field_value) = self.convert_metadata(spec_path, name, value)? {
                        let key = self.tokens.intern(name);
                        set_field_vec(&mut spec.fields, key, field_value);
                    }
                }
            }
        }

        Ok((spec, property_children))
    }

    /// Adds an attribute spec to its parent prim.
    fn apply_attribute_fields(
        &mut self,
        spec_path: &str,
        fields: &[(&str, DecodedField<'_>)],
        name_tok: TokenId,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let spec = self.build_attribute_spec(spec_path, fields)?;
        prim.set_property(name_tok, spec);
        Ok(())
    }

    /// Adds a relationship spec to its parent prim.
    fn apply_relationship_fields(
        &mut self,
        spec_path: &str,
        fields: &[(&str, DecodedField<'_>)],
        name_tok: TokenId,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let spec = self.build_relationship_spec(spec_path, fields)?;
        prim.set_property(name_tok, spec);
        Ok(())
    }

    /// Applies a connection child spec's paths to its attribute, when the
    /// attribute itself authors no `connectionPaths`.
    fn apply_connection_fields(
        &mut self,
        fields: &[(&str, DecodedField<'_>)],
        attr_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(attr_name);

        for (field_name, value) in fields {
            if *field_name == "connectionPaths" || *field_name == "targetPaths" {
                let listop = self.convert_connection_value(value)?;
                let spec = property_entry(&mut prim.properties, name_tok, PropertyKind::Attribute);
                if spec.targets.is_none() {
                    spec.targets = Some(listop);
                }
                return Ok(());
            }
        }

        Ok(())
    }

    /// Processes `VariantSet` and `Variant` specs.
    ///
    /// A variant set spec (`/P{v=}`) and a variant spec (`/P{v=x}`) belong to
    /// the spec holding the set: the prim spec owning it (`/P`, or
    /// `/P{a=y}C` for a set on a prim inside a branch, `/P{a=y}C{v=x}`), or
    /// for a set nested in another branch of the same prim
    /// (`/P{a=y}{v=x}`), that branch's variant spec. Each variant is a spec
    /// of its own, so a set nested under two branches, or reusing an
    /// enclosing set's name, stays two sets. Specs are placed outer first,
    /// so every holder exists before the sets it holds.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §7.6.7 (variant specs), §16.3 (crate paths).
    fn process_variant_specs(
        &mut self,
        decoded: &mut [bool],
        fields: &mut Fields<'a>,
        prim_specs: &mut PrimSpecs<'_>,
    ) -> Result<(), UsdcError> {
        // Properties directly on a variant (`/P{v=x}.b`), by variant path.
        let mut variant_properties: HashMap<&str, Vec<PropertyEntry>> = HashMap::new();
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form != SpecForm::Attribute && spec.form != SpecForm::Relationship {
                continue;
            }
            let path_str = self.lookup_path(spec.path_index)?;
            // Properties of prims inside a branch are attached with their
            // prim.
            let Some((variant_path, prop_name)) = path_str
                .rsplit_once('.')
                .filter(|(prim, _)| prim.ends_with('}'))
            else {
                continue;
            };
            self.collect_spec_fields(i, decoded, fields)?;
            let property = if spec.form == SpecForm::Attribute {
                self.build_attribute_spec(path_str, fields)?
            } else {
                self.build_relationship_spec(path_str, fields)?
            };
            let prop_tok = self.tokens.intern(prop_name);
            set_property_vec(
                variant_properties.entry(variant_path).or_default(),
                prop_tok,
                property,
            );
        }

        // Variant set and variant specs, outer before nested, and a set
        // before its variants.
        let mut specs: Vec<(usize, &str)> = Vec::new();
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if matches!(spec.form, SpecForm::VariantSet | SpecForm::Variant) {
                specs.push((i, self.lookup_path(spec.path_index)?));
            }
        }
        specs.sort_by(|(_, a), (_, b)| {
            let key = |path: &str| (path.matches('{').count(), !path.ends_with("=}"));
            key(a).cmp(&key(b)).then_with(|| a.cmp(b))
        });

        for (i, path_str) in specs {
            let is_set = self.sections.specs[i].form == SpecForm::VariantSet;
            let placed = split_trailing_selections(path_str)
                .filter(|(owner, _)| prim_specs.contains_key(owner))
                .filter(|(_, chain)| {
                    chain.split_last().is_some_and(|((_, last), enclosing)| {
                        last.is_empty() == is_set
                            && enclosing.iter().all(|(_, variant)| !variant.is_empty())
                    })
                });
            let Some((owner, chain)) = placed else {
                let what = if is_set { "variant set" } else { "variant" };
                self.report(
                    path_str,
                    None,
                    alloc::format!("{what} spec has no owning prim spec"),
                )?;
                continue;
            };
            let chain: Vec<(TokenId, TokenId)> = chain
                .iter()
                .map(|(set, variant)| (self.tokens.intern(set), self.tokens.intern(variant)))
                .collect();
            let ((set, variant), enclosing) = chain.split_last().expect("a selection");
            self.collect_spec_fields(i, decoded, fields)?;
            let variant_spec = if is_set {
                None
            } else {
                Some(self.build_variant_spec(path_str, fields, &mut variant_properties)?)
            };
            let prim = prim_specs.get_mut(owner).expect("owner checked above");
            let holder = if enclosing.is_empty() {
                Some(&mut prim.variant_sets)
            } else {
                prim.variant_spec_mut(enclosing)
                    .map(|spec| &mut spec.variant_sets)
            };
            let Some(sets) = holder else {
                self.report(path_str, None, "variant spec has no enclosing variant spec")?;
                continue;
            };
            // The set spec alone: only `variantSetNames` lists a set
            // (AOUSD Core §7.6.2.3.5).
            let set_spec = sets.entry(*set).or_default();
            if let Some(variant_spec) = variant_spec {
                set_spec.variants.insert(*variant, variant_spec);
            }
        }

        // Properties of variants that have no variant spec.
        let mut orphans: Vec<&str> = variant_properties.into_keys().collect();
        orphans.sort_unstable();
        for path in orphans {
            self.report(path, None, "variant property has no variant spec")?;
        }

        Ok(())
    }

    /// Builds the variant spec at `path_str` from its fields, with the
    /// properties read for it from `variant_properties`.
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs hold the fields a prim spec
    /// holds).
    fn build_variant_spec(
        &mut self,
        path_str: &str,
        fields: &Fields<'_>,
        variant_properties: &mut HashMap<&str, Vec<PropertyEntry>>,
    ) -> Result<VariantSpec, UsdcError> {
        let mut variant = VariantSpec::default();
        let mut property_children = None;
        self.in_branch = true;
        for (name, value) in fields {
            match *name {
                "primChildren" => {
                    variant.authored_children = self.extract_token_names(value);
                }
                "properties" => {
                    property_children = Some(self.extract_token_names(value));
                }
                "propertyOrder" => {
                    variant.property_order = Some(self.extract_token_names(value));
                }
                // Nested variant sets are read from their own specs.
                "variantSetChildren" => {}
                "variantSetNames" => {
                    // The nested sets, as for a prim spec.
                    variant.variant_set_names = self.variant_set_names(value);
                }
                "variantSelection" => {
                    if let Some(CrateValue::VariantSelectionMap(pairs)) = value.value() {
                        for (sn, bn) in pairs {
                            let st = self.tokens.intern(sn);
                            let bt = self.tokens.intern(bn);
                            variant.variant_selections.insert(st, bt);
                        }
                    }
                }
                "references" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value()
                        && let Ok(converted) = self.convert_ref_listop(listop)
                    {
                        variant.references.merge(converted);
                    }
                }
                "payload" => {
                    if let Ok(Some(converted)) = self.convert_payload_value(value) {
                        variant.payloads.merge(converted);
                    }
                }
                "inheritPaths" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value()
                        && let Ok(converted) = self.convert_path_listop(listop)
                    {
                        variant.inherits.merge(converted);
                    }
                }
                "specializes" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value()
                        && let Ok(converted) = self.convert_path_listop(listop)
                    {
                        variant.specializes.merge(converted);
                    }
                }
                _ => {
                    // Generic variant field.
                    if let Some(fv) = self.convert_metadata(path_str, name, value)? {
                        let key = self.tokens.intern(name);
                        set_field_vec(&mut variant.fields, key, fv);
                    }
                }
            }
        }
        if let Some(properties) = variant_properties.remove(path_str) {
            variant.properties = properties;
        }
        if let Some(order) = property_children {
            sort_by_children(&mut variant.properties, &order);
        }
        Ok(variant)
    }

    /// Builds parent-child relationships by examining prim paths.
    fn build_child_relationships(&mut self, prim_specs: &mut PrimSpecs<'_>) {
        // Each parent's child specs, parents and children in the file's
        // spec order, so names intern in the same order on every read.
        let mut parents: Vec<&str> = Vec::new();
        let mut children: HashMap<&str, Vec<(&str, TokenId)>> = HashMap::new();
        let sections = self.sections;
        for spec in &sections.specs {
            if spec.form != SpecForm::Prim {
                continue;
            }
            let Some(path) = sections
                .paths
                .get(spec.path_index as usize)
                .map(String::as_str)
                .filter(|path| prim_specs.contains_key(path))
            else {
                continue;
            };
            // Find parent path by stripping the last segment.
            let Some(parent_path) = parent_prim_path(path) else {
                continue;
            };
            let child_name = path.rsplit('/').next().unwrap_or("");
            if child_name.is_empty() {
                continue;
            }
            let child_tok = self.tokens.intern(child_name);
            children
                .entry(parent_path)
                .or_insert_with(|| {
                    parents.push(parent_path);
                    Vec::new()
                })
                .push((path, child_tok));
        }

        // Only add to parent's authored_children if not already present
        // (the prim's own primChildren field is authoritative), in path
        // order.
        for parent_path in parents {
            let Some(parent) = prim_specs.get_mut(parent_path) else {
                continue;
            };
            let mut named = children.remove(parent_path).unwrap_or_default();
            let listed: HashSet<TokenId> = parent.authored_children.iter().copied().collect();
            named.retain(|(_, name)| !listed.contains(name));
            named.sort_unstable();
            append_unique(
                &mut parent.authored_children,
                named.into_iter().map(|(_, name)| name),
            );
        }
    }

    // ── Value conversion helpers ──────────────────────────────────────

    /// Converts a validated field directly into its final layer value.
    fn convert_field_value(&mut self, field: &DecodedField<'_>) -> Value {
        match field {
            DecodedField::Value(value) => self.convert_crate_value(value),
            DecodedField::FloatArray(array) => Value::TypedArray(match array {
                FloatArray::Half(values) => layerstack::TypedArray::Half(Arc::new(values.clone())),
                FloatArray::Float(values) => {
                    layerstack::TypedArray::Float(Arc::new(values.clone()))
                }
                FloatArray::Double(values) => {
                    layerstack::TypedArray::Double(Arc::new(values.clone()))
                }
                FloatArray::TimeCode(values) => {
                    layerstack::TypedArray::TimeCode(Arc::new(values.clone()))
                }
            }),
            DecodedField::MathArray(array) => convert_math_array(array),
            // List ops are not plain values.
            DecodedField::PathListOp(_) => Value::Null,
            // What `Value::array` packs tokens into, without its copy.
            DecodedField::TokenArray(indices) => Value::Array(
                indices
                    .iter()
                    .map(|&index| Value::Token(self.token_id(index)))
                    .collect(),
            ),
            #[cfg(feature = "std")]
            DecodedField::Retained(rep) => {
                let pool = self
                    .budget
                    .lazy
                    .as_ref()
                    .expect("retained by a retained read");
                Value::Deferred(pool.register_value(*rep, self.tokens))
            }
        }
    }

    /// The token id of entry `index` of the token table, interning it on
    /// first use; the empty token outside the table, as the generic decoder
    /// reads it.
    fn token_id(&mut self, index: u32) -> TokenId {
        let Some(text) = self.sections.tokens.get(index as usize) else {
            return TokenId::EMPTY;
        };
        match &mut self.token_ids {
            TokenIds::OnUse(ids) => {
                *ids[index as usize].get_or_insert_with(|| self.tokens.intern(text))
            }
        }
    }

    /// Converts a [`CrateValue`] to a [`Value`].
    fn convert_crate_value(&mut self, cv: &CrateValue) -> Value {
        match cv {
            CrateValue::None => Value::Blocked,
            CrateValue::AnimationBlock => Value::AnimationBlock,
            CrateValue::Bool(b) => Value::Bool(*b),
            CrateValue::UChar(v) => Value::UChar(*v),
            CrateValue::Int(v) => Value::Int(*v),
            CrateValue::UInt(v) => Value::UInt(*v),
            CrateValue::Int64(v) => Value::Int64(*v),
            CrateValue::UInt64(v) => Value::UInt64(*v),
            CrateValue::Half(v) => Value::Half(*v),
            CrateValue::Float(v) => Value::Float(*v),
            CrateValue::Double(v) => Value::Double(*v),
            CrateValue::TimeCode(v) => Value::TimeCode(*v),
            CrateValue::String(s) => Value::String(Arc::from(s.as_str())),
            CrateValue::Token(t) => Value::Token(self.tokens.intern(t)),
            CrateValue::AssetPath(p) => Value::Asset(Arc::from(p.as_str())),
            CrateValue::PathExpression(p) => Value::PathExpression(Arc::from(p.as_str())),
            CrateValue::Specifier(v) => Value::Int(*v as i32),
            CrateValue::Variability(_) | CrateValue::Permission(_) => Value::Null,
            CrateValue::Opaque { value_type, data } => convert_math_value(*value_type, data),
            CrateValue::TypedArray(array) => Value::TypedArray(array.clone()),
            CrateValue::Array(items) => {
                let vals: Vec<Value> = items.iter().map(|v| self.convert_crate_value(v)).collect();
                Value::array(vals)
            }
            CrateValue::Dictionary(entries) => {
                let dict: Vec<(Arc<str>, Value)> = entries
                    .iter()
                    .map(|(k, v)| (Arc::from(k.as_str()), self.convert_crate_value(v)))
                    .collect();
                Value::Dictionary(dict)
            }
            CrateValue::ListOp(_) => {
                // ListOps are handled separately; shouldn't appear as plain values.
                Value::Null
            }
            CrateValue::TimeSamples(_) => {
                // TimeSamples are handled separately.
                Value::Null
            }
            CrateValue::VariantSelectionMap(_) => Value::Null,
            CrateValue::PathVector(paths) => {
                let vals: Vec<Value> = paths
                    .iter()
                    .map(|p| Value::String(Arc::from(p.as_str())))
                    .collect();
                Value::array(vals)
            }
            CrateValue::TokenVector(toks) => {
                let vals: Vec<Value> = toks
                    .iter()
                    .map(|t| Value::Token(self.tokens.intern(t)))
                    .collect();
                Value::array(vals)
            }
            CrateValue::DoubleVector(ds) => {
                let vals: Vec<Value> = ds.iter().map(|d| Value::Double(*d)).collect();
                Value::array(vals)
            }
            CrateValue::StringVector(ss) => {
                let vals: Vec<Value> = ss
                    .iter()
                    .map(|s| Value::String(Arc::from(s.as_str())))
                    .collect();
                Value::array(vals)
            }
            CrateValue::LayerOffsetVector(offsets) => {
                let vals: Vec<Value> = offsets
                    .iter()
                    .map(|(o, s)| Value::Array(alloc::vec![Value::Double(*o), Value::Double(*s)]))
                    .collect();
                Value::array(vals)
            }
            CrateValue::RelocatesMap(_) => Value::Null,
            CrateValue::Spline(..) => {
                // Splines are handled as FieldValue::Spline, not plain values.
                Value::Null
            }
            CrateValue::ArrayEdit(edit) => {
                // OpenUSD crateFile::_ValueHandler::PackArrayEdit keeps T in the
                // value representation even for an identity edit. The Sdf
                // attribute declaration is independent of that actual kind.
                let name = alloc::format!("{}[]", array_edit_element_name(edit.element_type));
                let value_type =
                    PropertyType::parse(&name).unwrap_or_else(PropertyType::unregistered);
                Value::typed_array_edit(self.convert_array_edit(edit), value_type)
            }
        }
    }

    /// Converts a native crate array edit to an [`ArrayEdit`].
    ///
    /// Instructions map one to one. OpenUSD's end index becomes
    /// [`ArrayIndex::End`]; other indices keep their value, and negative ones
    /// still count from the end. Out-of-range indices are kept and skipped
    /// when the edit is applied, as in `VtArrayEdit`
    /// (`pxr/base/vt/arrayEditOps.h`).
    ///
    /// Any number of instructions may use one literal, as OpenUSD stores
    /// each distinct literal once, so the literals are converted once and
    /// each instruction holds a clone of its converted literal. The
    /// literals are scalars, so a clone shares a string's allocation and
    /// copies anything else in constant size.
    fn convert_array_edit(&mut self, edit: &CrateArrayEdit) -> ArrayEdit {
        let literals: Vec<Value> = edit
            .literals
            .iter()
            .map(|literal| self.convert_crate_value(literal))
            .collect();
        let index = |i: i64| {
            if i == CrateArrayEdit::END {
                ArrayIndex::End
            } else {
                ArrayIndex::Position(i)
            }
        };
        let len = |len: u64| usize::try_from(len).unwrap_or(usize::MAX);
        let ops = edit
            .ops
            .iter()
            .map(|op| match *op {
                CrateArrayEditOp::WriteLiteral {
                    literal,
                    index: dst,
                } => ArrayEditOp::Write {
                    src: ArrayEditOperand::Literal(literals[literal].clone()),
                    index: index(dst),
                },
                CrateArrayEditOp::WriteRef { src, index: dst } => ArrayEditOp::Write {
                    src: ArrayEditOperand::CopyFrom(index(src)),
                    index: index(dst),
                },
                CrateArrayEditOp::InsertLiteral {
                    literal,
                    index: dst,
                } => ArrayEditOp::Insert {
                    src: ArrayEditOperand::Literal(literals[literal].clone()),
                    index: index(dst),
                },
                CrateArrayEditOp::InsertRef { src, index: dst } => ArrayEditOp::Insert {
                    src: ArrayEditOperand::CopyFrom(index(src)),
                    index: index(dst),
                },
                CrateArrayEditOp::Erase { index: dst } => ArrayEditOp::Erase { index: index(dst) },
                CrateArrayEditOp::MinSize { len: n } => ArrayEditOp::MinSize { len: len(n) },
                CrateArrayEditOp::MinSizeFill { len: n, literal } => ArrayEditOp::MinSizeFill {
                    len: len(n),
                    fill: literals[literal].clone(),
                },
                CrateArrayEditOp::SetSize { len: n } => ArrayEditOp::Resize { len: len(n) },
                CrateArrayEditOp::SetSizeFill { len: n, literal } => ArrayEditOp::ResizeFill {
                    len: len(n),
                    fill: literals[literal].clone(),
                },
                CrateArrayEditOp::MaxSize { len: n } => ArrayEditOp::MaxSize { len: len(n) },
            })
            .collect();
        ArrayEdit { ops }
    }

    /// Converts a metadata field's [`CrateValue`] to a [`FieldValue`].
    ///
    /// Returns `None`, after recording a diagnostic, for values the layer
    /// model cannot hold as metadata, rather than storing a placeholder.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields), §12.2.6 (list ops).
    fn convert_metadata(
        &mut self,
        spec_path: &str,
        field: &str,
        field_value: &DecodedField<'_>,
    ) -> Result<Option<FieldValue>, UsdcError> {
        if let DecodedField::PathListOp(parts) = field_value {
            return Ok(Some(FieldValue::PathListOp(self.target_list_op_at(parts))));
        }
        let Some(cv) = field_value.value() else {
            return Ok(Some(FieldValue::Value(
                self.convert_field_value(field_value),
            )));
        };
        Ok(match cv {
            CrateValue::ListOp(listop) => {
                let converted = match listop.op_type {
                    ValueType::TokenListOp => {
                        Some(FieldValue::TokenListOp(self.convert_token_listop(listop)))
                    }
                    ValueType::PathListOp => match self.convert_target_listop(listop) {
                        Ok(converted) => Some(FieldValue::PathListOp(converted)),
                        Err(error) => {
                            self.report(spec_path, Some(field), alloc::format!("{error}"))?;
                            return Ok(None);
                        }
                    },
                    ValueType::StringListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::String(s) => Some(Arc::from(s.as_str())),
                        _ => None,
                    })
                    .map(FieldValue::StringListOp),
                    ValueType::IntListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::Int(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::IntListOp),
                    ValueType::UIntListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::UInt(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::UIntListOp),
                    ValueType::Int64ListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::Int64(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::Int64ListOp),
                    ValueType::UInt64ListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::UInt64(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::UInt64ListOp),
                    // Reference and payload list ops are arcs, read by their
                    // dedicated fields; unregistered-value list ops have no
                    // element type.
                    _ => None,
                };
                if converted.is_none() {
                    self.report(
                        spec_path,
                        Some(field),
                        alloc::format!("unsupported: {:?} metadata is not read", listop.op_type),
                    )?;
                }
                converted
            }
            // `SdfPermission` (deprecated, Core §7.6.2.7): stored as the token
            // USDA spells it with.
            CrateValue::Permission(0) => Some(FieldValue::Value(Value::Token(
                self.tokens.intern("public"),
            ))),
            CrateValue::Permission(1) => Some(FieldValue::Value(Value::Token(
                self.tokens.intern("private"),
            ))),
            CrateValue::TimeSamples(_)
            | CrateValue::Spline(..)
            | CrateValue::VariantSelectionMap(_)
            | CrateValue::RelocatesMap(_)
            | CrateValue::Specifier(_)
            | CrateValue::Variability(_)
            | CrateValue::Permission(_) => {
                self.report(
                    spec_path,
                    Some(field),
                    "unsupported: this value type is not read as metadata",
                )?;
                None
            }
            _ => Some(FieldValue::Value(self.convert_crate_value(cv))),
        })
    }

    /// Builds an attribute [`PropertySpec`] from its crate fields, keeping
    /// every authored value slot.
    ///
    /// Spec: AOUSD Core §7.6.4 (attribute spec fields; §7.6.4.2.3: an
    /// attribute may have a value, a connection, or both).
    fn build_attribute_spec(
        &mut self,
        spec_path: &str,
        fields: &[(&str, DecodedField<'_>)],
    ) -> Result<PropertySpec, UsdcError> {
        let mut spec = PropertySpec::typed_attribute(self.attribute_property_type(fields));
        for (name, value) in fields {
            match (*name, value.value()) {
                ("default", _) => spec.default = Some(self.convert_field_value(value)),
                ("timeSamples", Some(CrateValue::TimeSamples(samples))) => {
                    let samples = samples
                        .iter()
                        .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                        .collect();
                    spec.time_samples = Some(samples);
                }
                ("spline", Some(CrateValue::Spline(spline, dictionaries))) => {
                    let mut spline = spline.clone();
                    for (time, entries) in dictionaries {
                        if let Some(knot) = spline.knots.iter_mut().find(|k| k.time == *time) {
                            knot.custom_data = entries
                                .iter()
                                .map(|(key, value)| {
                                    (Arc::from(key.as_str()), self.convert_crate_value(value))
                                })
                                .collect();
                        } else {
                            return Err(UsdcError::Inconsistent {
                                message: "custom data for a missing spline knot",
                            });
                        }
                    }
                    spec.spline = Some(spline);
                }
                ("connectionPaths", _) => {
                    spec.targets = Some(self.convert_connection_value(value)?);
                }
                // Read by `attribute_property_type`; `connectionChildren`
                // restates `connectionPaths`.
                ("typeName" | "connectionChildren", _) => {}
                _ => self.apply_property_field(spec_path, name, value, &mut spec)?,
            }
        }
        if let Some(property_type) = spec.type_name.as_ref() {
            let compact = |value: &mut Value| {
                if let Value::Array(items) = value {
                    // Text arrays keep their representation; skip the copy.
                    if matches!(
                        items.first(),
                        Some(Value::Token(_) | Value::String(_) | Value::Asset(_))
                    ) {
                        return;
                    }
                    let items = core::mem::take(items);
                    *value = Value::array_with_element(items, Some(property_type.default_scalar()));
                }
            };
            if let Some(value) = spec.default.as_mut() {
                compact(value);
            }
            if let Some(samples) = spec.time_samples.as_mut() {
                for (_, value) in samples.make_mut() {
                    compact(value);
                }
            }
        }
        Ok(spec)
    }

    /// Applies a field shared by attribute and relationship specs: the
    /// qualifiers, or a metadata entry.
    ///
    /// Spec: AOUSD Core §7.6.3 (property spec fields), §7.6.4.1.2
    /// (`variability`).
    fn apply_property_field(
        &mut self,
        spec_path: &str,
        name: &str,
        value: &DecodedField<'_>,
        spec: &mut PropertySpec,
    ) -> Result<(), UsdcError> {
        match (name, value.value()) {
            ("custom", Some(CrateValue::Bool(custom))) => spec.custom = *custom,
            ("variability", Some(CrateValue::Variability(variability))) => {
                spec.variability = match variability {
                    0 => Variability::Varying,
                    1 => Variability::Uniform,
                    other => {
                        self.report(
                            spec_path,
                            Some(name),
                            alloc::format!("unknown variability {other}; kept as varying"),
                        )?;
                        Variability::Varying
                    }
                };
            }
            _ => {
                if let Some(field_value) = self.convert_metadata(spec_path, name, value)? {
                    let key = self.tokens.intern(name);
                    set_field_vec(spec.metadata.make_mut(), key, field_value);
                }
            }
        }
        Ok(())
    }

    /// Builds a relationship [`PropertySpec`] from its crate fields.
    ///
    /// Spec: AOUSD Core §7.6.5 (relationship spec fields).
    fn build_relationship_spec(
        &mut self,
        spec_path: &str,
        fields: &[(&str, DecodedField<'_>)],
    ) -> Result<PropertySpec, UsdcError> {
        let mut spec = PropertySpec::relationship();
        for (name, value) in fields {
            match *name {
                "targetPaths" => spec.targets = Some(self.convert_connection_value(value)?),
                // `targetChildren` restates `targetPaths`.
                "targetChildren" => {}
                _ => self.apply_property_field(spec_path, name, value, &mut spec)?,
            }
        }
        Ok(spec)
    }

    fn attribute_property_type(&mut self, fields: &[(&str, DecodedField<'_>)]) -> PropertyType {
        let mut type_name = String::new();
        for (name, value) in fields {
            if *name == "typeName"
                && let Some(CrateValue::Token(token)) = value.value()
            {
                type_name = token.clone();
                break;
            }
        }

        let inferred_array = fields.iter().any(|(name, value)| {
            matches!(*name, "default" | "timeSamples")
                && value.value().is_none_or(crate_value_is_array)
        });

        // `typeName` spells arrays with `[]` (`point3f[]`); an array value
        // makes the declaration an array too, as USDA ingestion does. A name
        // the value type table lacks is kept, as OpenUSD keeps it.
        if inferred_array && !type_name.ends_with("[]") {
            type_name.push_str("[]");
        }
        PropertyType::parse(&type_name).unwrap_or_else(PropertyType::unregistered)
    }

    // ── List op conversion ──────────────────────────────────────────

    /// Converts a USDC reference/payload list op to a layerstack `ListOp<Reference>`.
    fn convert_ref_listop(&mut self, listop: &CrateListOp) -> Result<ListOp<Reference>, UsdcError> {
        let mut result = ListOp::default();

        if let Some(items) = &listop.explicit_items {
            result.explicit = Some(
                items
                    .iter()
                    .filter_map(|v| self.convert_crate_to_reference(v))
                    .collect(),
            );
        }
        result.prepend = listop
            .prepended_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();
        result.append = listop
            .appended_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();
        result.delete = listop
            .deleted_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();
        result.add = listop
            .added_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();
        result.reorder = listop
            .ordered_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();

        Ok(result)
    }

    /// Converts a USDC path list op to a layerstack `ListOp<PathId>`.
    fn convert_path_listop(&mut self, listop: &CrateListOp) -> Result<ListOp<PathId>, UsdcError> {
        let mut result = ListOp::default();

        let convert_items =
            |items: &[CrateValue], tokens: &TokenInterner, paths: &PathInterner| -> Vec<PathId> {
                items
                    .iter()
                    .filter_map(|v| {
                        if let CrateValue::String(s) = v {
                            Path::parse_absolute(s, tokens)
                                .ok()
                                .map(|p| paths.intern(p))
                        } else {
                            None
                        }
                    })
                    .collect()
            };

        if let Some(items) = &listop.explicit_items {
            result.explicit = Some(convert_items(items, self.tokens, self.paths));
        }
        result.prepend = convert_items(&listop.prepended_items, self.tokens, self.paths);
        result.append = convert_items(&listop.appended_items, self.tokens, self.paths);
        result.delete = convert_items(&listop.deleted_items, self.tokens, self.paths);
        result.add = convert_items(&listop.added_items, self.tokens, self.paths);
        result.reorder = convert_items(&listop.ordered_items, self.tokens, self.paths);

        Ok(result)
    }

    /// Converts a USDC token list op to a layerstack `ListOp<TokenId>`.
    fn convert_token_listop(&mut self, listop: &CrateListOp) -> ListOp<TokenId> {
        let mut result = ListOp::default();

        let convert_items = |items: &[CrateValue], tokens: &TokenInterner| -> Vec<TokenId> {
            items
                .iter()
                .filter_map(|v| match v {
                    CrateValue::Token(t) => Some(tokens.intern(t)),
                    CrateValue::String(s) => Some(tokens.intern(s)),
                    _ => None,
                })
                .collect()
        };

        if let Some(items) = &listop.explicit_items {
            result.explicit = Some(convert_items(items, self.tokens));
        }
        result.prepend = convert_items(&listop.prepended_items, self.tokens);
        result.append = convert_items(&listop.appended_items, self.tokens);
        result.delete = convert_items(&listop.deleted_items, self.tokens);
        result.add = convert_items(&listop.added_items, self.tokens);
        result.reorder = convert_items(&listop.ordered_items, self.tokens);

        result
    }

    /// Converts a connection or target paths value to `ListOp<TargetPath>`.
    fn convert_connection_value(
        &mut self,
        field: &DecodedField<'_>,
    ) -> Result<ListOp<TargetPath>, UsdcError> {
        if let DecodedField::PathListOp(parts) = field {
            return Ok(self.target_list_op_at(parts));
        }
        let Some(value) = field.value() else {
            return Ok(ListOp::default());
        };
        match value {
            CrateValue::ListOp(listop) => self.convert_target_listop(listop),
            CrateValue::PathVector(paths) => {
                let target_paths: Vec<TargetPath> =
                    paths.iter().filter_map(|s| self.target_path(s)).collect();
                Ok(ListOp::explicit(target_paths))
            }
            _ => Ok(ListOp::default()),
        }
    }

    fn convert_target_listop(
        &mut self,
        listop: &CrateListOp,
    ) -> Result<ListOp<TargetPath>, UsdcError> {
        let mut result = ListOp::default();

        let mut convert_items = |items: &[CrateValue]| -> Vec<TargetPath> {
            items
                .iter()
                .filter_map(|v| {
                    if let CrateValue::String(s) = v {
                        self.target_path(s)
                    } else {
                        None
                    }
                })
                .collect()
        };

        if let Some(items) = &listop.explicit_items {
            result.explicit = Some(convert_items(items));
        }
        result.prepend = convert_items(&listop.prepended_items);
        result.append = convert_items(&listop.appended_items);
        result.delete = convert_items(&listop.deleted_items);
        result.add = convert_items(&listop.added_items);
        result.reorder = convert_items(&listop.ordered_items);

        Ok(result)
    }

    /// [`Self::convert_target_listop`] for a list op of path table indices.
    fn target_list_op_at(&mut self, parts: &ListOpParts<u32>) -> ListOp<TargetPath> {
        let mut convert = |items: &[u32]| -> Vec<TargetPath> {
            items
                .iter()
                .filter_map(|&index| self.target_path_at(index))
                .collect()
        };
        let mut result = ListOp::default();
        if let Some(items) = &parts.explicit {
            result.explicit = Some(convert(items));
        }
        result.prepend = convert(&parts.prepended);
        result.append = convert(&parts.appended);
        result.delete = convert(&parts.deleted);
        result.add = convert(&parts.added);
        result.reorder = convert(&parts.ordered);
        result
    }

    /// The target path path table entry `index` names, as its text parses
    /// ([`TargetPath::parse`]), found once per entry from the path tree:
    /// a prim path's id ([`Self::prim_path_at`]), or a property's prim and
    /// name ([`Self::property_owner`]). An entry the tree cannot answer for
    /// certain (a text holding `{`, or a `.` in a prim name) is parsed.
    fn target_path_at(&mut self, index: u32) -> Option<TargetPath> {
        let sections = self.sections;
        let at = index as usize;
        let Some(text) = sections.paths.get(at) else {
            return self.target_path("");
        };
        if sections.path_nodes.len() != sections.paths.len() {
            return self.target_path(text);
        }
        if self.targets.is_empty() {
            self.targets = alloc::vec![TargetAt::Unknown; sections.paths.len()];
        }
        if let TargetAt::Found(found) = self.targets[at] {
            return found;
        }
        let found = match sections.path_nodes[at] {
            PathNode::Child { property: true, .. } => match self.property_owner(index) {
                Some(PropertyOwner::Prim { owner, name }) => {
                    // A prim inside a variant branch is no target's prim.
                    if sections.paths[owner as usize].contains('{') {
                        None
                    } else {
                        self.prim_path_at(owner)
                            .map(|prim| TargetPath::Property(PropertyPath::new(prim, name)))
                    }
                }
                Some(PropertyOwner::Variant | PropertyOwner::Invalid) => None,
                None => self.target_path(text),
            },
            PathNode::Root | PathNode::Child { .. } if !text.contains(['.', '{']) => {
                self.prim_path_at(index).map(TargetPath::Prim)
            }
            PathNode::Root | PathNode::Child { .. } | PathNode::Unused => self.target_path(text),
        };
        self.targets[at] = TargetAt::Found(found);
        found
    }

    /// Parses a target path's text ([`TargetPath::parse`]) once per read.
    fn target_path(&mut self, text: &str) -> Option<TargetPath> {
        if let Some(parsed) = self.target_paths.get(text) {
            return *parsed;
        }
        let parsed = TargetPath::parse(text, self.tokens, self.paths).ok();
        self.target_paths.insert(String::from(text), parsed);
        parsed
    }

    /// Converts a `payload` field value to a list op.
    ///
    /// The field holds a payload list op from crate 0.8. Earlier files hold
    /// a single `SdfPayload`, which is an explicit list of that payload, or
    /// no payload when empty (`pxr/usd/sdf/crateFile.cpp:393`); OpenUSD
    /// still writes one payload with an identity layer offset that way while
    /// the file needs no later version.
    ///
    /// A single payload with an empty asset path is an explicitly empty list,
    /// not an internal payload: internal payloads were introduced with
    /// payload list ops (`_ToPayloadListOpValue` in
    /// `pxr/usd/sdf/crateData.cpp`).
    ///
    /// Spec: AOUSD Core §7.6.2.3.2 (`payload`).
    fn convert_payload_value(
        &mut self,
        field: &DecodedField<'_>,
    ) -> Result<Option<ListOp<Reference>>, UsdcError> {
        let enclosing = core::mem::replace(&mut self.in_payloads, true);
        let converted = self.convert_payload_field(field);
        self.in_payloads = enclosing;
        converted
    }

    /// [`Self::convert_payload_value`], once the arcs read are marked
    /// payloads.
    fn convert_payload_field(
        &mut self,
        field: &DecodedField<'_>,
    ) -> Result<Option<ListOp<Reference>>, UsdcError> {
        let Some(value) = field.value() else {
            return Ok(None);
        };
        match value {
            CrateValue::ListOp(listop) => self.convert_ref_listop(listop).map(Some),
            CrateValue::Dictionary(entries) => {
                let has_asset = entries.iter().any(|(key, value)| {
                    key == "assetPath"
                        && matches!(value, CrateValue::AssetPath(asset) if !asset.is_empty())
                });
                let explicit = if has_asset {
                    self.convert_crate_to_reference(value).into_iter().collect()
                } else {
                    Vec::new()
                };
                Ok(Some(ListOp::explicit(explicit)))
            }
            _ => Ok(None),
        }
    }

    /// Converts a USDC reference-encoded dictionary to a [`Reference`].
    fn convert_crate_to_reference(&mut self, cv: &CrateValue) -> Option<Reference> {
        if let CrateValue::Dictionary(entries) = cv {
            let mut asset_path = String::new();
            let mut prim_path = String::new();
            let mut layer_offset_val = 0.0_f64;
            let mut layer_scale_val = 1.0_f64;
            let mut custom_data = Vec::new();

            for (key, val) in entries {
                match key.as_str() {
                    "assetPath" => {
                        if let CrateValue::AssetPath(p) = val {
                            asset_path = p.clone();
                        }
                    }
                    "primPath" => {
                        if let CrateValue::String(s) = val {
                            prim_path = s.clone();
                        }
                    }
                    "layerOffset" => {
                        if let CrateValue::Double(v) = val {
                            layer_offset_val = *v;
                        }
                    }
                    "customData" => {
                        if let Value::Dictionary(entries) = self.convert_crate_value(val) {
                            custom_data = entries;
                        }
                    }
                    "layerScale" => {
                        if let CrateValue::Double(v) = val {
                            layer_scale_val = *v;
                        }
                    }
                    _ => {}
                }
            }

            // An empty prim path targets the `defaultPrim`; a malformed one
            // is dropped like USDA's, never retargeted.
            let target = if prim_path.is_empty() {
                ReferenceTarget::DefaultPrim
            } else {
                let path = Path::parse_absolute(&prim_path, self.tokens).ok()?;
                ReferenceTarget::Prim(self.paths.intern(path))
            };
            let layer_offset = LayerOffset {
                offset: layer_offset_val,
                scale: layer_scale_val,
            };

            // With neither an asset path nor a prim path, the arc is internal
            // and targets this layer's `defaultPrim` (AOUSD Core §10.3.2.1),
            // as `<>` does in USDA. An asset path that cannot be resolved
            // keeps the arc unresolved; it never becomes an internal arc.
            if asset_path.is_empty() {
                return Some(Reference {
                    layer: self.layer_id,
                    target,
                    asset: None,
                    layer_offset,
                    custom_data,
                    deferred: false,
                });
            }
            // An asset path that is a variable expression is evaluated
            // during composition, relative to this layer.
            if is_expression(&asset_path) {
                return Some(
                    Reference::expression(self.layer_id, asset_path, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            }
            // A payload to a dynamic file format targets the layer its
            // composed arguments name, which composition asks for
            // (`AssetResolver::defers_payload`). An arc inside a variant
            // branch may never be composed; resolving the branches the
            // layer selects is left until the whole layer is read
            // (`resolve_selected_deferred_arcs`), the rest to composition.
            if (self.in_branch && self.defer_variant_arcs)
                || (self.in_payloads && self.resolver.defers_payload(&asset_path))
            {
                return Some(
                    Reference::deferred(self.layer_id, asset_path, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            }
            let Some(resolved) = self.resolve_asset(&asset_path) else {
                return Some(
                    Reference::unresolved(asset_path, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            };
            if let Some(layer) = resolved.layer {
                self.resolved_layers.push(layer);
            }
            Some(Reference {
                layer: resolved.layer_id,
                target,
                asset: Some(asset_path),
                layer_offset,
                custom_data,
                deferred: false,
            })
        } else {
            None
        }
    }

    // ── Token/path helpers ──────────────────────────────────────────

    /// Returns the names a token or string list op adds, in the order it
    /// gives them over no names: its explicit, prepended, added and
    /// appended items.
    /// The `variantSetNames` list op, as authored: a string list op, or
    /// for a name vector, an explicit list of its names.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
    fn variant_set_names(&mut self, field: &DecodedField<'_>) -> ListOp<TokenId> {
        match field.value() {
            Some(CrateValue::ListOp(listop)) => self.convert_token_listop(listop),
            Some(_) => ListOp::explicit(self.extract_token_names(field)),
            None if matches!(field, DecodedField::TokenArray(_)) => {
                ListOp::explicit(self.extract_token_names(field))
            }
            None => ListOp::default(),
        }
    }

    /// Extracts token names from a [`CrateValue`] (typically a `TokenVector`
    /// or `Array` of tokens).
    fn extract_token_names(&mut self, field: &DecodedField<'_>) -> Vec<TokenId> {
        if let DecodedField::TokenArray(indices) = field {
            return indices.iter().map(|&index| self.token_id(index)).collect();
        }
        let Some(value) = field.value() else {
            return Vec::new();
        };
        match value {
            CrateValue::TokenVector(tokens) => {
                tokens.iter().map(|t| self.tokens.intern(t)).collect()
            }
            CrateValue::Array(items) => items
                .iter()
                .filter_map(|v| match v {
                    CrateValue::Token(t) => Some(self.tokens.intern(t)),
                    CrateValue::String(s) => Some(self.tokens.intern(s)),
                    _ => None,
                })
                .collect(),
            CrateValue::ListOp(listop) => {
                // Some fields store ordered names as explicit list ops.
                if let Some(items) = &listop.explicit_items {
                    items
                        .iter()
                        .filter_map(|v| match v {
                            CrateValue::Token(t) => Some(self.tokens.intern(t)),
                            CrateValue::String(s) => Some(self.tokens.intern(s)),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    /// Looks up a spec path in the paths table.
    ///
    /// The path is borrowed, but each lookup is charged by its length: every
    /// pass that looks a spec's path up parses, splits or copies it.
    /// Splits the property path text `path` into its prim path text and
    /// property name when it is a valid property path
    /// ([`PropertyPath::parse`]), checking each prim path text once
    /// (`valid`).
    fn split_property_path(
        &mut self,
        path: &'a str,
        valid: &mut HashMap<&'a str, bool>,
    ) -> Option<(&'a str, &'a str)> {
        let (prim, name) = PropertyPath::split(path).ok()?;
        let tokens = self.tokens;
        let ok = *valid
            .entry(prim)
            .or_insert_with(|| Path::parse_absolute(prim, tokens).is_ok());
        ok.then_some((prim, name))
    }

    /// The prim path path table entry `index` names outside variant
    /// branches: the path its text parses as ([`Path::parse_absolute`]),
    /// `None` when its text holds a variant selection or does not parse.
    ///
    /// Found once per entry from its parent's and its element, as the
    /// text was built, rather than by parsing the text: the text is read
    /// only for an element that would not join its parent's segments as
    /// one segment (empty, or holding a `/`), an entry beneath one that
    /// names no prim path, or a table without its tree.
    fn prim_path_at(&mut self, index: u32) -> Option<PathId> {
        let sections = self.sections;
        let nodes = &sections.path_nodes;
        if nodes.len() != sections.paths.len() {
            return self.prim_path_of_text(index as usize);
        }
        if self.prim_paths.is_empty() {
            self.prim_paths = alloc::vec![PrimPathAt::Unknown; nodes.len()];
        }
        // The entries up to the nearest found ancestor, nearest last.
        let mut pending = Vec::new();
        let mut at = index as usize;
        while matches!(self.prim_paths.get(at), Some(PrimPathAt::Unknown)) {
            pending.push(at);
            match nodes[at] {
                PathNode::Child { parent, .. } => at = parent as usize,
                PathNode::Root | PathNode::Unused => break,
            }
        }
        for &at in pending.iter().rev() {
            let found = match nodes[at] {
                PathNode::Root => Some(self.paths.intern(Path::root())),
                PathNode::Unused => self.prim_path_of_text(at),
                PathNode::Child { property: true, .. } => None,
                PathNode::Child { parent, token, .. } => {
                    let element = self.lookup_token(token);
                    let parent = self.prim_paths.get(parent as usize).copied();
                    match parent {
                        _ if element.contains('{') => None,
                        Some(PrimPathAt::Some(parent))
                            if !element.is_empty() && !element.contains('/') =>
                        {
                            let name = self.token_id(token);
                            Some(self.paths.intern_child(parent, name))
                        }
                        _ => self.prim_path_of_text(at),
                    }
                }
            };
            self.prim_paths[at] = found.map_or(PrimPathAt::None, PrimPathAt::Some);
        }
        match self.prim_paths.get(index as usize) {
            Some(PrimPathAt::Some(path)) => Some(*path),
            _ => None,
        }
    }

    /// [`Self::prim_path_at`] from the text of path table entry `at`.
    fn prim_path_of_text(&self, at: usize) -> Option<PathId> {
        let text = self.sections.paths.get(at)?;
        if text.contains('{') {
            return None;
        }
        let path = Path::parse_absolute(text, self.tokens).ok()?;
        Some(self.paths.intern(path))
    }

    /// The prim spec path entry and name of the property path path table
    /// entry `index` names, read from the path tree as
    /// [`PropertyPath::split`] and [`Path::parse_absolute`] read its text;
    /// `None` when the tree cannot say so for certain (no tree, or a
    /// property element holding a separator), for the text to say.
    fn property_owner(&mut self, index: u32) -> Option<PropertyOwner> {
        let sections = self.sections;
        if sections.path_nodes.len() != sections.paths.len() {
            return None;
        }
        let PathNode::Child {
            parent,
            token,
            property: true,
        } = *sections.path_nodes.get(index as usize)?
        else {
            return None;
        };
        let name = self.lookup_token(token);
        if name.is_empty() || name.contains(['.', '/', '{', '}']) {
            return None;
        }
        let owner = sections.paths.get(parent as usize)?;
        if owner.contains('{') {
            // A property directly on a variant (`/A{v=x}.b`) belongs to
            // the variant spec, and is placed with the variant specs; one on
            // a prim inside a branch (`/A{v=x}B.c`) is placed here.
            if owner.ends_with('}') {
                return Some(PropertyOwner::Variant);
            }
        } else {
            if self.owners.is_empty() {
                self.owners = alloc::vec![0; sections.paths.len()];
            }
            if self.owners[parent as usize] == 0 {
                let valid = !owner.contains(['.', '}'])
                    && owner != "/"
                    && Path::parse_absolute(owner, self.tokens).is_ok();
                self.owners[parent as usize] = if valid { 1 } else { 2 };
            }
            if self.owners[parent as usize] != 1 {
                return Some(PropertyOwner::Invalid);
            }
        }
        Some(PropertyOwner::Prim {
            owner: parent,
            name: self.token_id(token),
        })
    }

    fn lookup_path(&mut self, index: u32) -> Result<&'a str, UsdcError> {
        let sections = self.sections;
        let path = sections
            .paths
            .get(index as usize)
            .ok_or(UsdcError::Inconsistent {
                message: "path index out of range",
            })?;
        self.budget.charge_text(path.len())?;
        Ok(path)
    }

    /// Looks up a token in the tokens table, or the empty string when out of
    /// range.
    fn lookup_token(&self, index: u32) -> &'a str {
        let sections = self.sections;
        sections
            .tokens
            .get(index as usize)
            .map_or("", String::as_str)
    }

    /// Resolves an asset path.
    /// Resolves `asset_path` relative to this layer. A variable expression
    /// is not resolved here: composition evaluates it with the variables of
    /// the layer stack that authors it (see
    /// [`layerstack::variable_expression`]).
    fn resolve_asset(&mut self, asset_path: &str) -> Option<ResolvedAsset> {
        if is_expression(asset_path) {
            return None;
        }
        self.resolver
            .resolve(asset_path, Some(self.layer_id), self.tokens, self.paths)
            .ok()
    }
}

// ---------------------------------------------------------------------------
// Path parsing helpers
// ---------------------------------------------------------------------------

/// Appends the `names` that `list` does not hold yet, in order.
///
/// The names come from the file, so membership is tested in a set: testing
/// the list would take time quadratic in its length.
fn append_unique(list: &mut Vec<TokenId>, names: impl IntoIterator<Item = TokenId>) {
    let mut present: HashSet<TokenId> = list.iter().copied().collect();
    list.extend(names.into_iter().filter(|name| present.insert(*name)));
}

/// A variant selection in a crate path: host prim path, set and variant.
///
/// The host is the first `host_len` bytes of the namespace path; the set and
/// variant are borrowed from the crate path.
type BranchSite<'p> = (usize, &'p str, &'p str);

/// Splits the crate path of a prim inside variant branches, such as
/// `/A{v=x}B/C`, into its namespace path (`/A/B/C`) and its variant
/// selections with their host prim paths, outer to inner.
///
/// Returns `None` for a path outside any variant branch or a malformed one.
fn split_branch_path(path: &str) -> Option<(String, Vec<BranchSite<'_>>)> {
    let mut namespace = String::new();
    let mut sites = Vec::new();
    let mut rest = path;
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('{') {
            let close = inner.find('}')?;
            let (set, variant) = inner[..close].split_once('=')?;
            if set.is_empty() || variant.is_empty() || namespace.is_empty() {
                return None;
            }
            sites.push((namespace.len(), set, variant));
            rest = &inner[close + 1..];
            if !rest.is_empty() && !rest.starts_with('/') && !rest.starts_with('{') {
                namespace.push('/');
            }
        } else {
            let end = rest.find('{').unwrap_or(rest.len());
            namespace.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }
    (namespace.starts_with('/') && !sites.is_empty() && !namespace.ends_with('/'))
        .then_some((namespace, sites))
}

/// Strips the variant selections ending a crate path, returning the path of
/// the prim spec they belong to: `/A{v=x}{w=y}` → `/A`, `/A{v=x}B` →
/// `/A{v=x}B`.
///
/// Returns `None` for a malformed path or one naming no prim.
fn owning_prim_path(path: &str) -> Option<&str> {
    let mut rest = path;
    while let Some(body) = rest.strip_suffix('}') {
        rest = &body[..body.rfind('{')?];
    }
    (rest.len() > 1 && rest.starts_with('/') && !rest.ends_with('/')).then_some(rest)
}

/// Splits the crate path of a variant set spec or a variant spec into the
/// path of the prim spec owning it (see [`owning_prim_path`]) and the
/// selections ending it, outermost first: `/A{v=x}{w=y}` →
/// `("/A", [("v", "x"), ("w", "y")])`, and `/A{v=x}{w=}` →
/// `("/A", [("v", "x"), ("w", "")])`.
///
/// Returns `None` for a malformed path or one ending in no selection.
fn split_trailing_selections(path: &str) -> Option<(&str, Vec<(&str, &str)>)> {
    let mut rest = path;
    let mut chain = Vec::new();
    while let Some(body) = rest.strip_suffix('}') {
        let open = body.rfind('{')?;
        let (set, variant) = body[open + 1..].split_once('=')?;
        if set.is_empty() {
            return None;
        }
        chain.push((set, variant));
        rest = &body[..open];
    }
    chain.reverse();
    let owner = owning_prim_path(rest)?;
    (owner == rest && !chain.is_empty()).then_some((owner, chain))
}

/// Converts a list op whose items are plain scalars, or returns `None` when
/// an item has an unexpected type.
///
/// Spec: AOUSD Core §16.3.10 (list op encoding).
fn convert_scalar_listop<T>(
    listop: &CrateListOp,
    item: impl Fn(&CrateValue) -> Option<T>,
) -> Option<ListOp<T>> {
    let items = |values: &[CrateValue]| values.iter().map(&item).collect::<Option<Vec<T>>>();
    Some(ListOp {
        explicit: match &listop.explicit_items {
            Some(values) => Some(items(values)?),
            None => None,
        },
        prepend: items(&listop.prepended_items)?,
        append: items(&listop.appended_items)?,
        delete: items(&listop.deleted_items)?,
        add: items(&listop.added_items)?,
        reorder: items(&listop.ordered_items)?,
    })
}

/// Orders `properties` by their position in `children` (`propertyChildren`),
/// keeping properties not listed there after the listed ones, in order.
///
/// Positions are looked up in a map: both lists come from the file, so
/// searching `children` for each property would take quadratic time.
fn sort_by_children(properties: &mut [PropertyEntry], children: &[TokenId]) {
    // A short list is searched faster than it is hashed, and most prims
    // list their properties in the order their specs came in.
    if children.len() <= 16 {
        let place = |entry: &PropertyEntry| {
            children
                .iter()
                .position(|child| *child == entry.name)
                .unwrap_or(usize::MAX)
        };
        if !properties.is_sorted_by_key(place) {
            properties.sort_by_key(place);
        }
        return;
    }
    let mut position: HashMap<TokenId, usize> = HashMap::new();
    for (i, child) in children.iter().enumerate() {
        position.entry(*child).or_insert(i);
    }
    let place = |entry: &PropertyEntry| position.get(&entry.name).copied().unwrap_or(usize::MAX);
    if !properties.is_sorted_by_key(place) {
        properties.sort_by_key(place);
    }
}

/// Whether `rep` is a `token[]` value, which assembly decodes to token-table
/// indices ([`decode_text_indices`]).
fn is_token_array(rep: &RawValueRep) -> bool {
    rep.is_array() && !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::Token)
}

/// Whether `rep` is a token vector, which decodes as one whatever its
/// array flag says.
fn is_token_vector(rep: &RawValueRep) -> bool {
    !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::TokenVector)
}

/// Whether `rep` is a path list op, which decodes as one whatever its array
/// flag says.
fn is_path_list_op(rep: &RawValueRep) -> bool {
    !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::PathListOp)
}

fn crate_value_is_array(value: &CrateValue) -> bool {
    match value {
        CrateValue::Array(_) | CrateValue::TypedArray(_) | CrateValue::ArrayEdit(_) => true,
        CrateValue::TimeSamples(samples) => samples
            .iter()
            .any(|(_, sample)| crate_value_is_array(sample)),
        _ => false,
    }
}

fn array_edit_element_name(value_type: ValueType) -> &'static str {
    match value_type {
        ValueType::Bool => "bool",
        ValueType::UChar => "uchar",
        ValueType::Int => "int",
        ValueType::UInt => "uint",
        ValueType::Int64 => "int64",
        ValueType::UInt64 => "uint64",
        ValueType::Half => "half",
        ValueType::Float => "float",
        ValueType::Double => "double",
        ValueType::String => "string",
        ValueType::Token => "token",
        ValueType::AssetPath => "asset",
        ValueType::Matrix2d => "matrix2d",
        ValueType::Matrix3d => "matrix3d",
        ValueType::Matrix4d => "matrix4d",
        ValueType::Quatd => "quatd",
        ValueType::Quatf => "quatf",
        ValueType::Quath => "quath",
        ValueType::Vec2d => "double2",
        ValueType::Vec2f => "float2",
        ValueType::Vec2h => "half2",
        ValueType::Vec2i => "int2",
        ValueType::Vec3d => "double3",
        ValueType::Vec3f => "float3",
        ValueType::Vec3h => "half3",
        ValueType::Vec3i => "int3",
        ValueType::Vec4d => "double4",
        ValueType::Vec4f => "float4",
        ValueType::Vec4h => "half4",
        ValueType::Vec4i => "int4",
        ValueType::TimeCode => "timecode",
        ValueType::PathExpression => "pathExpression",
        // decode_array_edit rejects these before a CrateArrayEdit is built.
        _ => unreachable!("decoded array edit has an array element type"),
    }
}

/// Returns the parent prim path for a path like `/A/B` → `/A`, `/A` → `/`.
fn parent_prim_path(path: &str) -> Option<&str> {
    if path == "/" {
        return None;
    }
    // Don't compute parent for variant paths.
    if path.contains('{') {
        return None;
    }
    if let Some(last_slash) = path.rfind('/') {
        if last_slash == 0 {
            Some("/")
        } else {
            Some(&path[..last_slash])
        }
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Value type name helper
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Math type → Value conversion
// ---------------------------------------------------------------------------

/// Converts USDC opaque math bytes into a typed [`Value`] variant.
///
/// The byte layout is little-endian and matches the USDC binary format
/// (§16.3.10). Vectors use the obvious element order; quaternions use
/// (i, j, k, r) storage order per §6.3.
fn convert_math_value(vtype: ValueType, data: &[u8]) -> Value {
    match vtype {
        // Vectors — f64
        ValueType::Vec2d => Value::Vec2d(read_f64x2(data)),
        ValueType::Vec3d => Value::Vec3d(read_f64x3(data)),
        ValueType::Vec4d => Value::Vec4d(read_f64x4(data)),
        // Vectors — f32
        ValueType::Vec2f => Value::Vec2f(read_f32x2(data)),
        ValueType::Vec3f => Value::Vec3f(read_f32x3(data)),
        ValueType::Vec4f => Value::Vec4f(read_f32x4(data)),
        // Vectors — half
        ValueType::Vec2h => Value::Vec2h(read_u16x2(data)),
        ValueType::Vec3h => Value::Vec3h(read_u16x3(data)),
        ValueType::Vec4h => Value::Vec4h(read_u16x4(data)),
        // Vectors — i32
        ValueType::Vec2i => Value::Vec2i(read_i32x2(data)),
        ValueType::Vec3i => Value::Vec3i(read_i32x3(data)),
        ValueType::Vec4i => Value::Vec4i(read_i32x4(data)),
        // Quaternions — (i, j, k, r) storage order
        ValueType::Quatd => Value::Quatd(read_f64x4(data)),
        ValueType::Quatf => Value::Quatf(read_f32x4(data)),
        ValueType::Quath => Value::Quath(read_u16x4(data)),
        // Matrices — row-major f64
        ValueType::Matrix2d => Value::Matrix2d(Box::new(read_f64_array::<4>(data))),
        ValueType::Matrix3d => Value::Matrix3d(Box::new(read_f64_array::<9>(data))),
        ValueType::Matrix4d => Value::Matrix4d(Box::new(read_f64_array::<16>(data))),
        _ => Value::Null,
    }
}

// --- Little-endian readers for math element arrays ---

// ---------------------------------------------------------------------------
// ListOp merge helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{FieldDef, SpecDef};
    use crate::version::CrateVersion;
    use layerstack::AssetResolveError;

    /// A resolver that resolves nothing.
    struct NoAssets;

    impl AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &TokenInterner,
            _: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::NotFound)
        }

        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }

    /// Naming paths from the path tree assembles the layer naming them from
    /// their text does: prims in and out of variant branches, properties of
    /// each, a variant's own property, relationship targets and
    /// connections in every list op part, and children listed by name; and
    /// both assemble the layer that was written.
    #[test]
    fn the_path_tree_names_what_the_text_does() {
        let source = r#"#usda 1.0
def "World" {
    def "A" (
        variants = { string look = "x" }
        prepend variantSets = "look"
    ) {
        int count = 1
        rel target = </World/B.size>
        prepend rel others = [</World/A/Child>, </Other>]
        delete rel others = </World/B>
        double linked.connect = </World/B.size>
        variantSet "look" = {
            "x" {
                double shade = 0.5
                def "Inner" { double d = 2 }
            }
            "y" { }
        }
        def "Child" { custom token kind = "leaf" }
    }
    over "B" { double size = 3 }
}
def "Other" {}
"#;
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
        assert!(
            read.parse_diagnostics.is_empty(),
            "{:?}",
            read.parse_diagnostics
        );
        let bytes = crate::writer::save_layer(&read.emitted.layer, &tokens, &paths).unwrap();
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let sections =
            crate::section::parse_sections(&bytes, &toc, header.crate_version(), &mut budget)
                .unwrap();
        assert_eq!(sections.path_nodes.len(), sections.paths.len());
        let assemble_from = |sections: &CrateSections| {
            let mut budget = DecodeBudget::with_limit(u64::MAX);
            assemble(
                &bytes,
                sections,
                LayerId(1),
                &tokens,
                &paths,
                &mut NoAssets,
                &mut budget,
            )
            .unwrap()
        };
        let tree = assemble_from(&sections);
        let mut text = sections.clone();
        text.path_nodes.clear();
        let text = assemble_from(&text);
        assert!(tree.layer == text.layer, "the same layer");
        assert_eq!(tree.diagnostics, text.diagnostics);
        assert!(tree.layer == read.emitted.layer, "the layer written");
    }

    /// Properties follow the authored `properties` order, those it does not
    /// name last in the order they came, whether the list is searched or
    /// hashed, and whether or not they came in order.
    #[test]
    fn properties_follow_their_authored_order() {
        let tokens = TokenInterner::default();
        let entry = |name: &str| PropertyEntry {
            name: tokens.intern(name),
            spec: Arc::new(PropertySpec::relationship()),
        };
        let names = |entries: &[PropertyEntry]| -> Vec<String> {
            entries
                .iter()
                .map(|entry| String::from(tokens.resolve(entry.name)))
                .collect()
        };
        for extra in [0, 20] {
            let mut children: Vec<TokenId> =
                ["c", "a", "b"].iter().map(|n| tokens.intern(n)).collect();
            // A long list names more properties than the prim has.
            children.extend((0..extra).map(|i| tokens.intern(alloc::format!("x{i}"))));
            let mut properties = alloc::vec![entry("a"), entry("z"), entry("b"), entry("c")];
            sort_by_children(&mut properties, &children);
            assert_eq!(names(&properties), ["c", "a", "b", "z"], "{extra}");
            sort_by_children(&mut properties, &children);
            assert_eq!(names(&properties), ["c", "a", "b", "z"], "in order already");
        }
    }

    #[test]
    fn identity_array_edits_retain_actual_kind_independently_of_declaration() {
        for byte in (1..=30).chain([56, 57]) {
            let kind = ValueType::try_from(byte).unwrap();
            let mut edit_rep = [0; 8];
            edit_rep[6] = byte;
            edit_rep[7] = 0x10; // identity edit, no payload or literals
            let mut declaration_rep = [0; 8];
            declaration_rep[0] = 2; // token index of "double[]"
            declaration_rep[6] = ValueType::Token as u8;
            declaration_rep[7] = 0x40;
            let sections = CrateSections {
                tokens: alloc::vec!["default".into(), "typeName".into(), "double[]".into()],
                strings: Vec::new(),
                fields: alloc::vec![
                    FieldDef {
                        token_index: 0,
                        value_rep: edit_rep
                    },
                    FieldDef {
                        token_index: 1,
                        value_rep: declaration_rep
                    },
                ],
                fieldsets: alloc::vec![-1, 0, 1, -1],
                paths: alloc::vec!["/A".into(), "/A.a".into()],
                path_nodes: Vec::new(),
                specs: alloc::vec![
                    SpecDef {
                        path_index: 0,
                        fieldset_index: 0,
                        form: SpecForm::Prim
                    },
                    SpecDef {
                        path_index: 1,
                        fieldset_index: 1,
                        form: SpecForm::Attribute
                    },
                ],
                version: CrateVersion::NEWEST_READABLE,
            };
            let tokens = TokenInterner::default();
            let read = assemble(
                &[],
                &sections,
                LayerId(1),
                &tokens,
                &PathInterner::default(),
                &mut NoAssets,
                &mut DecodeBudget::with_limit(10_000),
            )
            .unwrap();
            let property = &read.layer.prims().values().next().unwrap().properties[0].spec;
            assert_eq!(property.type_name.as_ref().unwrap().type_name(), "double[]");
            let value = property.default.as_ref().unwrap();
            assert!(value.array_edit_ref().unwrap().ops.is_empty());
            let actual = value.array_edit_type().unwrap();
            let element = array_edit_element_name(kind);
            assert_eq!(actual.type_name(), alloc::format!("{element}[]"));
            assert!(actual.is_array());
            assert_eq!(
                actual.default_scalar(),
                &layerstack::ValueType::from_name(element).unwrap().zero()
            );
        }
    }

    #[test]
    fn compact_integer_decode_preserves_width_and_signedness() {
        let values = [-1_i64, 0, 1, i64::MIN, i64::MAX];
        for (ty, expected) in [
            (
                ValueType::Bool,
                alloc::vec![
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true)
                ],
            ),
            (
                ValueType::UChar,
                alloc::vec![
                    Value::UChar(255),
                    Value::UChar(0),
                    Value::UChar(1),
                    Value::UChar(0),
                    Value::UChar(255)
                ],
            ),
            (
                ValueType::Int,
                alloc::vec![
                    Value::Int(-1),
                    Value::Int(0),
                    Value::Int(1),
                    Value::Int(0),
                    Value::Int(-1)
                ],
            ),
            (
                ValueType::UInt,
                alloc::vec![
                    Value::UInt(u32::MAX),
                    Value::UInt(0),
                    Value::UInt(1),
                    Value::UInt(0),
                    Value::UInt(u32::MAX)
                ],
            ),
            (
                ValueType::Int64,
                alloc::vec![
                    Value::Int64(-1),
                    Value::Int64(0),
                    Value::Int64(1),
                    Value::Int64(i64::MIN),
                    Value::Int64(i64::MAX)
                ],
            ),
            (
                ValueType::UInt64,
                alloc::vec![
                    Value::UInt64(u64::MAX),
                    Value::UInt64(0),
                    Value::UInt64(1),
                    Value::UInt64(1 << 63),
                    Value::UInt64((1 << 63) - 1)
                ],
            ),
        ] {
            let width = match ty {
                ValueType::Bool | ValueType::UChar => 1,
                ValueType::Int | ValueType::UInt => 4,
                _ => 8,
            };
            let mut data = alloc::vec![0; 8];
            data.extend_from_slice(&(values.len() as u64).to_le_bytes());
            for value in values {
                data.extend_from_slice(&value.to_le_bytes()[..width]);
            }
            let mut raw = [0; 8];
            raw[0] = 8;
            raw[6] = ty as u8;
            raw[7] = 0x80;
            let sections = CrateSections {
                tokens: Vec::new(),
                strings: Vec::new(),
                fields: Vec::new(),
                fieldsets: Vec::new(),
                paths: Vec::new(),
                path_nodes: Vec::new(),
                specs: Vec::new(),
                version: CrateVersion::NEWEST_READABLE,
            };
            let decoded = decode_field_within(
                &RawValueRep::new(raw),
                &data,
                &sections,
                &mut DecodeBudget::with_limit(100),
            )
            .unwrap();
            let DecodedField::Value(CrateValue::TypedArray(array)) = decoded else {
                panic!("native integer array")
            };
            assert_eq!(Value::TypedArray(array), Value::Array(expected));
        }
    }

    #[test]
    fn compact_array_fields_preserve_metadata_defaults_and_variants() {
        for tag in (1..=9).chain(13..=30).chain(core::iter::once(56)) {
            let ty = ValueType::try_from(tag).unwrap();
            let mut data = alloc::vec![0; 8];
            data.extend_from_slice(&1_u64.to_le_bytes());
            data.extend_from_slice(&[0x81; 128]);
            let mut raw = [0; 8];
            raw[0] = 8;
            raw[6] = tag;
            raw[7] = 0x80;
            let sections = CrateSections {
                tokens: alloc::vec!["default".into(), "customArray".into()],
                strings: Vec::new(),
                fields: alloc::vec![
                    FieldDef {
                        token_index: 0,
                        value_rep: raw
                    },
                    FieldDef {
                        token_index: 1,
                        value_rep: raw
                    },
                ],
                fieldsets: alloc::vec![0, -1, 1, -1],
                paths: alloc::vec![
                    "/P".into(),
                    "/P.a".into(),
                    "/P{v=x}".into(),
                    "/P{v=x}.a".into()
                ],
                path_nodes: Vec::new(),
                specs: alloc::vec![
                    SpecDef {
                        path_index: 0,
                        fieldset_index: 2,
                        form: SpecForm::Prim
                    },
                    SpecDef {
                        path_index: 1,
                        fieldset_index: 0,
                        form: SpecForm::Attribute
                    },
                    SpecDef {
                        path_index: 2,
                        fieldset_index: 2,
                        form: SpecForm::Variant
                    },
                    SpecDef {
                        path_index: 3,
                        fieldset_index: 0,
                        form: SpecForm::Attribute
                    },
                ],
                version: CrateVersion::NEWEST_READABLE,
            };
            let tokens = TokenInterner::default();
            let paths = PathInterner::default();
            let result = assemble(
                &data,
                &sections,
                LayerId(1),
                &tokens,
                &paths,
                &mut NoAssets,
                &mut DecodeBudget::with_limit(100),
            )
            .unwrap();
            assert!(result.diagnostics.is_empty());
            let prim = result.layer.prims().values().next().unwrap();
            let expected = if tag <= 6 {
                Value::Array(alloc::vec![match ty {
                    ValueType::Bool => Value::Bool(true),
                    ValueType::UChar => Value::UChar(0x81),
                    ValueType::Int => Value::Int(i32::from_le_bytes([0x81; 4])),
                    ValueType::UInt => Value::UInt(u32::from_le_bytes([0x81; 4])),
                    ValueType::Int64 => Value::Int64(i64::from_le_bytes([0x81; 8])),
                    ValueType::UInt64 => Value::UInt64(u64::from_le_bytes([0x81; 8])),
                    _ => unreachable!(),
                }])
            } else if matches!(tag, 7 | 8 | 9 | 56) {
                Value::Array(alloc::vec![match ty {
                    ValueType::Half => Value::Half(u16::from_le_bytes([0x81; 2])),
                    ValueType::Float => Value::Float(f32::from_le_bytes([0x81; 4])),
                    ValueType::Double => Value::Double(f64::from_le_bytes([0x81; 8])),
                    _ => Value::TimeCode(f64::from_le_bytes([0x81; 8])),
                }])
            } else {
                Value::Array(alloc::vec![convert_math_value(ty, &data[16..])])
            };
            let property = prim.property(tokens.intern("a")).unwrap();
            assert_eq!(property.default, Some(expected.clone()));
            assert!(property.type_name.as_ref().unwrap().is_array());
            assert_eq!(
                prim.field(tokens.intern("customArray")),
                Some(&FieldValue::Value(expected.clone()))
            );
            let variant = &prim.variant_sets[&tokens.intern("v")].variants[&tokens.intern("x")];
            assert_eq!(variant.properties[0].spec.default, Some(expected.clone()));
            assert_eq!(variant.fields[0].value, FieldValue::Value(expected));
        }
    }

    #[test]
    fn unused_math_fields_are_still_validated() {
        let mut sections = shared_fieldset_sections(1, 1, alloc::vec!["/Missing.a".into()], |_| 0);
        let mut raw = [0; 8];
        raw[0] = 8;
        raw[6] = ValueType::Vec3f as u8;
        raw[7] = 0x80;
        sections.fields[0].value_rep = raw;
        for form in [
            SpecForm::Attribute,
            SpecForm::Relationship,
            SpecForm::Connection,
            SpecForm::VariantSet,
            SpecForm::Variant,
            SpecForm::Mapper,
        ] {
            sections.specs[0].form = form;
            assert!(matches!(
                assemble_within(&sections, &mut DecodeBudget::with_limit(100)),
                Err(UsdcError::UnexpectedEof { .. })
            ));
        }
    }

    /// Sections of `n` prim specs sharing one fieldset whose single field,
    /// an inlined int, is named with `name_len` bytes; spec `i` has path
    /// `path_of(i)`.
    fn shared_fieldset_sections(
        n: u32,
        name_len: usize,
        paths: Vec<String>,
        path_of: impl Fn(u32) -> u32,
    ) -> CrateSections {
        let mut rep = [0_u8; 8];
        rep[6] = ValueType::Int as u8;
        rep[7] = 0x40; // inlined
        CrateSections {
            tokens: alloc::vec!["x".repeat(name_len)],
            strings: Vec::new(),
            fields: alloc::vec![FieldDef {
                token_index: 0,
                value_rep: rep,
            }],
            fieldsets: alloc::vec![0, -1],
            paths,
            path_nodes: Vec::new(),
            specs: (0..n)
                .map(|i| SpecDef {
                    path_index: path_of(i),
                    fieldset_index: 0,
                    form: SpecForm::Prim,
                })
                .collect(),
            version: CrateVersion::NEWEST_READABLE,
        }
    }

    fn assemble_within(
        sections: &CrateSections,
        budget: &mut DecodeBudget,
    ) -> Result<AssembleResult, UsdcError> {
        assemble(
            &[],
            sections,
            LayerId(1),
            &TokenInterner::default(),
            &PathInterner::default(),
            &mut NoAssets,
            budget,
        )
    }

    /// The re-review's probe: specs sharing a fieldset share its long field
    /// name, and each use is charged by the name's length, so a budget that
    /// covers only the specs and their values fails.
    #[test]
    fn shared_field_names_are_charged_per_use() {
        let n = 128;
        let name_len = 4096;
        let sections = shared_fieldset_sections(
            n,
            name_len,
            (0..n).map(|i| alloc::format!("/P{i}")).collect(),
            |i| i,
        );

        let mut budget = DecodeBudget::with_limit(2 * u64::from(n));
        assert_eq!(
            assemble_within(&sections, &mut budget).err(),
            Some(UsdcError::DecodeBudgetExceeded {
                limit: 2 * u64::from(n)
            })
        );

        let mut budget = DecodeBudget::with_limit(u64::MAX);
        assemble_within(&sections, &mut budget).unwrap();
        let names = u64::from(n) * (name_len / 16) as u64;
        assert!(budget.used() >= names, "{} < {names}", budget.used());
    }

    /// Specs sharing one long path are charged for each use of it.
    #[test]
    fn shared_spec_paths_are_charged_per_use() {
        let n = 128;
        let path = alloc::format!("/{}", "P".repeat(4095));
        let sections = shared_fieldset_sections(n, 1, alloc::vec![path], |_| 0);
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        assemble_within(&sections, &mut budget).unwrap();
        let paths = u64::from(n) * 4096 / 16;
        assert!(budget.used() >= paths, "{} < {paths}", budget.used());
    }

    #[test]
    fn validated_property_text_matches_interned_spelling() {
        // Borrowed keys must be identical to the previous display/resolve
        // keys, including namespace separators and text the parser permits.
        for text in [
            "/World/Mesh.primvars:st",
            "/World/Mesh.material:binding",
            "/世界/形状.displayColor",
            "/P[x].a[b]",
            "/P.a b",
        ] {
            let tokens = TokenInterner::default();
            let paths = PathInterner::default();
            let property = PropertyPath::parse(text, &tokens, &paths).unwrap();
            let (prim, name) = text.rsplit_once('.').unwrap();
            assert_eq!(prim, paths.display(property.prim_path(), &tokens));
            assert_eq!(name, tokens.resolve(property.property()));
        }
    }

    #[test]
    fn split_property_simple() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let property = PropertyPath::parse("/Cube.size", &tokens, &paths).unwrap();
        assert_eq!(paths.display(property.prim_path(), &tokens), "/Cube");
        assert_eq!(tokens.resolve(property.property()), "size");
    }

    #[test]
    fn split_property_nested() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let property = PropertyPath::parse("/World/Cube.visibility", &tokens, &paths).unwrap();
        assert_eq!(paths.display(property.prim_path(), &tokens), "/World/Cube");
        assert_eq!(tokens.resolve(property.property()), "visibility");
    }

    #[test]
    fn split_property_none() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert!(PropertyPath::parse("/Cube", &tokens, &paths).is_err());
    }

    #[test]
    fn split_trailing_selections_ok() {
        assert_eq!(
            split_trailing_selections("/Prim{shadingVariant=}"),
            Some(("/Prim", alloc::vec![("shadingVariant", "")]))
        );
        assert_eq!(
            split_trailing_selections("/Prim{shadingVariant=red}"),
            Some(("/Prim", alloc::vec![("shadingVariant", "red")]))
        );
        // Nested in another branch of the same prim: each enclosing
        // selection is kept, outermost first.
        assert_eq!(
            split_trailing_selections("/Prim{set=red}{inner=}"),
            Some(("/Prim", alloc::vec![("set", "red"), ("inner", "")]))
        );
        assert_eq!(
            split_trailing_selections("/Prim{set=red}{inner=x}{set=red}"),
            Some((
                "/Prim",
                alloc::vec![("set", "red"), ("inner", "x"), ("set", "red")]
            ))
        );
        // On a prim inside a branch.
        assert_eq!(
            split_trailing_selections("/A/Prim{set=red}Child{inner=x}"),
            Some(("/A/Prim{set=red}Child", alloc::vec![("inner", "x")]))
        );
    }

    #[test]
    fn variant_paths_below_variant_children_are_not_a_variant() {
        // A child prim of a variant, its property, and paths naming no
        // prim.
        for path in [
            "/Prim{set=red}Child",
            "/Prim{set=red}Child.color",
            "/Prim{set=red}.color",
            "/Prim",
            "/{set=red}",
            "{set=red}",
            "/Prim{=red}",
        ] {
            assert_eq!(split_trailing_selections(path), None, "{path}");
        }
    }

    #[test]
    fn parent_prim_path_root_child() {
        assert_eq!(parent_prim_path("/Cube"), Some("/"));
    }

    #[test]
    fn parent_prim_path_nested() {
        assert_eq!(parent_prim_path("/World/Cube"), Some("/World"));
    }

    #[test]
    fn parent_prim_path_root() {
        assert!(parent_prim_path("/").is_none());
    }
}
