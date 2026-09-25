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

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{HashMap, HashSet};

use layerstack::doc::{
    FieldValue, Layer, LayerId, LayerOffset, PrimSpec, Reference, Specifier, SublayerEntry, Value,
    VariantSetSpec, VariantSpec, set_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner, PropertyPath, TargetPath};
use layerstack::property::{
    PropertyEntry, PropertyKind, PropertySpec, Variability, property_entry, set_property_vec,
};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};
use layerstack::{AssetResolver, PropertyType, ReferenceTarget, ResolvedAsset};

use crate::error::UsdcError;
use crate::section::CrateSections;
use crate::value_rep::{
    CrateArrayEdit, CrateArrayEditOp, CrateListOp, CrateValue, DecodeBudget, RawValueRep,
    decode_value_within,
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
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
    budget: &mut DecodeBudget,
) -> Result<AssembleResult, UsdcError> {
    let mut ctx = AssembleCtx {
        data,
        sections,
        tokens,
        paths,
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
    tokens: &'a mut TokenInterner,
    paths: &'a mut PathInterner,
    resolver: &'a mut dyn AssetResolver,
    layer_id: LayerId,
    resolved_layers: Vec<Layer>,
    diagnostics: Vec<AssembleDiagnostic>,
    /// What the read may still materialize.
    budget: &'a mut DecodeBudget,
}

/// A spec's fields: names borrowed from the token table, and their values.
type Fields<'a> = Vec<(&'a str, CrateValue)>;

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

    /// Assembles all specs into a [`Layer`].
    fn assemble_layer(&mut self) -> Result<Layer, UsdcError> {
        let mut layer = Layer::new(self.layer_id);

        // Build a map from prim path string → PrimSpec being constructed.
        // We need to process specs in a specific order:
        // 1. PseudoRoot first (layer metadata)
        // 2. Prim specs (create PrimSpecs)
        // 3. Attribute/Relationship specs (add fields to parent prims)
        // 4. VariantSet/Variant specs (handle variant structures)

        // First pass: collect all spec fields by spec index.
        let sections = self.sections;
        let spec_fields: Vec<Fields<'a>> = sections
            .specs
            .iter()
            .map(|spec| self.collect_fields(spec.fieldset_index))
            .collect::<Result<_, _>>()?;

        // Build a path-to-spec-index map grouped by prim path for child lookup.
        let mut prim_specs_map: HashMap<String, PrimSpec> = HashMap::new();
        // Authored property order (`propertyChildren`) per prim path.
        let mut property_children: HashMap<String, Vec<TokenId>> = HashMap::new();

        // Process PseudoRoot specs first.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::PseudoRoot {
                self.process_pseudo_root(&spec_fields[i], &mut layer)?;
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
                let (prim, children) = self.build_prim_spec(path_str, &spec_fields[i])?;
                if let Some(children) = children {
                    property_children.insert(String::from(path_str), children);
                }
                prim_specs_map.insert(String::from(path_str), prim);
            }
        }

        // Process Attribute and Relationship specs — add them to their
        // parent prim. Specs under variant paths are handled with the
        // variant specs below.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            let is_attribute = spec.form == SpecForm::Attribute;
            if !is_attribute && spec.form != SpecForm::Relationship {
                continue;
            }
            let path_str = self.lookup_path(spec.path_index)?;
            let (prim_path, name) = if path_str.contains('{') {
                // A property directly on a variant (`/A{v=x}.b`) belongs to
                // the variant spec and is handled with the variant specs;
                // one on a prim inside a branch (`/A{v=x}B.c`) is handled
                // here.
                match path_str.rsplit_once('.') {
                    Some((prim, _)) if prim.ends_with('}') => continue,
                    Some((prim, name)) => (String::from(prim), String::from(name)),
                    None => {
                        self.report(path_str, None, "property spec path could not be parsed")?;
                        continue;
                    }
                }
            } else {
                let Ok(property_path) = PropertyPath::parse(path_str, self.tokens, self.paths)
                else {
                    self.report(path_str, None, "property spec path could not be parsed")?;
                    continue;
                };
                (
                    self.paths.display(property_path.prim_path(), self.tokens),
                    String::from(self.tokens.resolve(property_path.property())),
                )
            };
            let Some(mut prim) = prim_specs_map.remove(&prim_path) else {
                self.report(path_str, None, "property spec has no owning prim spec")?;
                continue;
            };
            if is_attribute {
                self.apply_attribute_fields(path_str, &spec_fields[i], &name, &mut prim)?;
            } else {
                self.apply_relationship_fields(path_str, &spec_fields[i], &name, &mut prim)?;
            }
            prim_specs_map.insert(prim_path, prim);
        }

        // Process Connection specs — add connection paths to attribute on parent.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Connection {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Ok(property_path) = PropertyPath::parse(path_str, self.tokens, self.paths) {
                    let prim_path = self.paths.display(property_path.prim_path(), self.tokens);
                    let attr_name = String::from(self.tokens.resolve(property_path.property()));
                    if let Some(prim) = prim_specs_map.get_mut(&prim_path) {
                        self.apply_connection_fields(&spec_fields[i], &attr_name, prim)?;
                    }
                }
            }
        }

        // Process VariantSet and Variant specs.
        self.process_variant_specs(&spec_fields, &mut prim_specs_map)?;

        // Report specs of forms the layer model does not hold.
        for (i, spec) in self.sections.specs.iter().enumerate() {
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
            if !handled && !spec_fields[i].is_empty() {
                let path_str = self.lookup_path(spec.path_index)?;
                self.report(
                    path_str,
                    None,
                    alloc::format!("unsupported: {:?} spec fields are not read", spec.form),
                )?;
            }
        }

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
        let mut prim_specs: Vec<_> = prim_specs_map.into_iter().collect();
        prim_specs.sort_by(|a, b| a.0.cmp(&b.0));
        for (path_str, mut prim) in prim_specs {
            let namespace = match split_branch_path(&path_str) {
                Some((namespace, sites)) => {
                    // Each selection's host path is parsed and interned, so
                    // a path with many selections is charged for each.
                    let mut outer_sites = Vec::with_capacity(sites.len());
                    for (host_len, set, variant) in sites {
                        self.budget.charge(1)?;
                        self.budget.charge_text(host_len)?;
                        let Ok(host) = Path::parse_absolute(&namespace[..host_len], self.tokens)
                        else {
                            continue;
                        };
                        outer_sites.push(VariantSelectionSite {
                            host_path: self.paths.intern(host),
                            set: self.tokens.intern(set),
                            variant: self.tokens.intern(variant),
                        });
                    }
                    prim.outer_variant_sites = outer_sites;
                    namespace
                }
                None => path_str,
            };
            if let Ok(path) = Path::parse_absolute(&namespace, self.tokens) {
                let path_id = self.paths.intern(path);
                layer.insert_prim(path_id, prim);
            }
        }

        Ok(layer)
    }

    /// Collects decoded fields for a spec from the fieldsets/fields tables.
    ///
    /// Field names are borrowed from the token table: specs sharing a
    /// fieldset share its names. Each use is still charged by the name's
    /// length, since assembly matches, interns or reports it.
    fn collect_fields(&mut self, fieldset_index: u32) -> Result<Fields<'a>, UsdcError> {
        // A spec can become a field of its own (an attribute declared
        // without a value), so each spec is charged too.
        self.budget.charge(1)?;
        let mut result = Vec::new();
        let start = fieldset_index as usize;

        if start >= self.sections.fieldsets.len() {
            return Ok(result);
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
                let value = decode_value_within(&rep, self.data, self.sections, self.budget)?;
                result.push((field_name, value));
            }
            idx += 1;
        }

        Ok(result)
    }

    /// Processes a `PseudoRoot` spec: sublayers, root prim order and layer
    /// metadata.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields).
    fn process_pseudo_root(
        &mut self,
        fields: &[(&str, CrateValue)],
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
                    if let CrateValue::PathVector(paths) | CrateValue::StringVector(paths) = value {
                        sublayer_paths = paths;
                    }
                }
                "subLayerOffsets" => {
                    if let CrateValue::LayerOffsetVector(offsets) = value {
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
                    if let CrateValue::Token(name) = value {
                        layer.default_prim = Some(self.tokens.intern(name));
                    }
                }
                "layerRelocates" | "relocates" => {
                    // Relocates (AOUSD Core §10) are not modelled.
                    self.report("/", Some(name), "unsupported: relocates are not read")?;
                }
                _ => {
                    if let Some(field_value) = self.convert_metadata("/", name, value)? {
                        let key = self.tokens.intern(name);
                        set_field_vec(&mut layer.metadata, key, field_value);
                    }
                }
            }
        }

        // Spec: AOUSD Core §12.3.2.1 (sublayer offsets).
        for (i, asset_path) in sublayer_paths.iter().enumerate() {
            if let Some(resolved) = self.resolve_asset(asset_path) {
                let mut entry = SublayerEntry::new(resolved.layer_id);
                if let Some(&(offset, scale)) = sublayer_offsets.get(i) {
                    entry.offset = LayerOffset { offset, scale };
                }
                layer.sublayers.push(entry);
                if let Some(sub_layer) = resolved.layer {
                    self.resolved_layers.push(sub_layer);
                }
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
        fields: &[(&str, CrateValue)],
    ) -> Result<(PrimSpec, Option<Vec<TokenId>>), UsdcError> {
        let mut spec = PrimSpec::default();
        let mut property_children = None;

        for (name, value) in fields {
            match *name {
                "specifier" => {
                    if let CrateValue::Specifier(v) = value {
                        spec.specifier = Some(match v {
                            0 => Specifier::Def,
                            1 => Specifier::Over,
                            2 => Specifier::Class,
                            _ => Specifier::Def,
                        });
                    }
                }
                "typeName" => {
                    if let CrateValue::Token(t) = value
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
                    if let CrateValue::ListOp(listop) = value {
                        let converted = self.convert_ref_listop(listop)?;
                        merge_ref_listop(&mut spec.references, converted);
                    }
                }
                // Spec: AOUSD Core §7.6.2.3.2 (`payload`).
                "payload" => {
                    if let Some(converted) = self.convert_payload_value(value)? {
                        merge_ref_listop(&mut spec.payloads, converted);
                    }
                }
                // Spec: AOUSD Core §7.6.2.3.3 (`inheritPaths`).
                "inheritPaths" => {
                    if let CrateValue::ListOp(listop) = value {
                        let converted = self.convert_path_listop(listop)?;
                        merge_path_listop(&mut spec.inherits, converted);
                    }
                }
                "specializes" => {
                    if let CrateValue::ListOp(listop) = value {
                        let converted = self.convert_path_listop(listop)?;
                        merge_path_listop(&mut spec.specializes, converted);
                    }
                }
                "variantSelection" => {
                    if let CrateValue::VariantSelectionMap(pairs) = value {
                        for (set_name, branch_name) in pairs {
                            let set_tok = self.tokens.intern(set_name);
                            let branch_tok = self.tokens.intern(branch_name);
                            spec.variant_selections.insert(set_tok, branch_tok);
                        }
                    }
                }
                "variantSetNames" => {
                    // Ordered variant set names: a string list op (AOUSD
                    // Core §7.6.2.3.5) whose items, in order, give the
                    // variant sets' strength order.
                    let names = match value {
                        CrateValue::ListOp(listop) => self.list_op_names(listop),
                        _ => self.extract_token_names(value),
                    };
                    append_unique(&mut spec.variant_set_order, names);
                }
                "instanceable" if matches!(value, CrateValue::Bool(_)) => {
                    if let CrateValue::Bool(b) = value {
                        spec.instanceable = Some(*b);
                    }
                }
                "active" if matches!(value, CrateValue::Bool(_)) => {
                    if let CrateValue::Bool(b) = value {
                        spec.active = Some(*b);
                    }
                }
                "relocates" => {
                    // Relocates (AOUSD Core §10) are not modelled.
                    self.report(spec_path, Some(name), "unsupported: relocates are not read")?;
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
        fields: &[(&str, CrateValue)],
        attr_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(attr_name);
        let spec = self.build_attribute_spec(spec_path, fields)?;
        prim.set_property(name_tok, spec);
        Ok(())
    }

    /// Adds a relationship spec to its parent prim.
    fn apply_relationship_fields(
        &mut self,
        spec_path: &str,
        fields: &[(&str, CrateValue)],
        rel_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(rel_name);
        let spec = self.build_relationship_spec(spec_path, fields)?;
        prim.set_property(name_tok, spec);
        Ok(())
    }

    /// Applies a connection child spec's paths to its attribute, when the
    /// attribute itself authors no `connectionPaths`.
    fn apply_connection_fields(
        &mut self,
        fields: &[(&str, CrateValue)],
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
    fn process_variant_specs(
        &mut self,
        spec_fields: &[Fields<'_>],
        prim_specs: &mut HashMap<String, PrimSpec>,
    ) -> Result<(), UsdcError> {
        // Collect variant set info: path → variant set name → variant branches.
        // USDC stores variant set paths as `/Prim{varSet=}`
        // and variant paths as `/Prim{varSet=branchName}`.
        let mut variant_sets: HashMap<String, Vec<TokenId>> = HashMap::new();
        for spec in &self.sections.specs {
            if spec.form == SpecForm::VariantSet {
                let path_str = self.lookup_path(spec.path_index)?;
                // Path like: /Prim{varSetName=}
                if let Some((prim_path, vset_name)) = parse_variant_set_path(path_str)
                    && let Some(prim) = prim_specs.get_mut(&prim_path)
                {
                    let vset_tok = self.tokens.intern(&vset_name);
                    prim.variant_sets
                        .entry(vset_tok)
                        .or_insert_with(VariantSetSpec::default);
                    variant_sets.entry(prim_path).or_default().push(vset_tok);
                }
            }
        }
        for (prim_path, names) in variant_sets {
            if let Some(prim) = prim_specs.get_mut(&prim_path) {
                append_unique(&mut prim.variant_set_order, names);
            }
        }

        let mut variant_property_children = Vec::new();
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Variant {
                let path_str = self.lookup_path(spec.path_index)?;
                // Path like: /Prim{varSetName=branchName}
                if let Some((prim_path, vset_name, branch_name)) = parse_variant_path(path_str)
                    && let Some(prim) = prim_specs.get_mut(&prim_path)
                {
                    let vset_tok = self.tokens.intern(&vset_name);
                    let branch_tok = self.tokens.intern(&branch_name);
                    let mut variant = VariantSpec::default();
                    let mut property_children = None;

                    // Process variant fields.
                    for (name, value) in &spec_fields[i] {
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
                            "variantSetChildren" => {}
                            "variantSelection" => {
                                if let CrateValue::VariantSelectionMap(pairs) = value {
                                    for (sn, bn) in pairs {
                                        let st = self.tokens.intern(sn);
                                        let bt = self.tokens.intern(bn);
                                        variant.variant_selections.insert(st, bt);
                                    }
                                }
                            }
                            "references" => {
                                if let CrateValue::ListOp(listop) = value
                                    && let Ok(converted) = self.convert_ref_listop(listop)
                                {
                                    merge_ref_listop(&mut variant.references, converted);
                                }
                            }
                            "payload" => {
                                if let Ok(Some(converted)) = self.convert_payload_value(value) {
                                    merge_ref_listop(&mut variant.payloads, converted);
                                }
                            }
                            "inheritPaths" => {
                                if let CrateValue::ListOp(listop) = value
                                    && let Ok(converted) = self.convert_path_listop(listop)
                                {
                                    merge_path_listop(&mut variant.inherits, converted);
                                }
                            }
                            "specializes" => {
                                if let CrateValue::ListOp(listop) = value
                                    && let Ok(converted) = self.convert_path_listop(listop)
                                {
                                    merge_path_listop(&mut variant.specializes, converted);
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

                    let vset = prim
                        .variant_sets
                        .entry(vset_tok)
                        .or_insert_with(VariantSetSpec::default);
                    if let Some(order) = property_children {
                        variant_property_children.push((prim_path, vset_tok, branch_tok, order));
                    }
                    vset.variants.insert(branch_tok, variant);
                } else {
                    self.report(
                        path_str,
                        None,
                        "unsupported: variant specs below a variant branch are not read",
                    )?;
                }
            }
        }

        // Process attribute/relationship specs that live under variant paths.
        // These have paths like /Prim{varSet=branch}.attrName or
        // /Prim{varSet=branch}/Child.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Attribute || spec.form == SpecForm::Relationship {
                let path_str = self.lookup_path(spec.path_index)?;
                // Only properties directly on a variant (`/A{v=x}.b`); those of
                // prims inside a branch are attached with their prim.
                if !path_str
                    .rsplit_once('.')
                    .is_some_and(|(prim, _)| prim.ends_with('}'))
                {
                    continue;
                }
                // Check if this is under a variant context.
                if let Some((prim_path, vset_name, branch_name, prop_name)) =
                    parse_variant_property_path(path_str)
                    && let Some(prim) = prim_specs.get_mut(&prim_path)
                    && let Some(vset) = prim.variant_sets.get_mut(&self.tokens.intern(&vset_name))
                    && let Some(variant) = vset.variants.get_mut(&self.tokens.intern(&branch_name))
                {
                    let prop_tok = self.tokens.intern(&prop_name);
                    let property = if spec.form == SpecForm::Attribute {
                        self.build_attribute_spec(path_str, &spec_fields[i])?
                    } else {
                        self.build_relationship_spec(path_str, &spec_fields[i])?
                    };
                    set_property_vec(&mut variant.properties, prop_tok, property);
                } else {
                    self.report(
                        path_str,
                        None,
                        "unsupported: properties of nested variants are not read",
                    )?;
                }
            }
        }

        for (prim_path, vset_tok, branch_tok, order) in variant_property_children {
            if let Some(variant) = prim_specs
                .get_mut(&prim_path)
                .and_then(|prim| prim.variant_sets.get_mut(&vset_tok))
                .and_then(|vset| vset.variants.get_mut(&branch_tok))
            {
                sort_by_children(&mut variant.properties, &order);
            }
        }

        Ok(())
    }

    /// Builds parent-child relationships by examining prim paths.
    fn build_child_relationships(&mut self, prim_specs: &mut HashMap<String, PrimSpec>) {
        // Collect all prim paths, in a stable order.
        let mut prim_paths: Vec<String> = prim_specs.keys().cloned().collect();
        prim_paths.sort_unstable();

        let mut children: HashMap<String, Vec<TokenId>> = HashMap::new();
        for path in &prim_paths {
            if path == "/" {
                continue;
            }

            // Find parent path by stripping the last segment.
            if let Some(parent_path) = parent_prim_path(path) {
                let child_name = path.rsplit('/').next().unwrap_or("");
                if child_name.is_empty() {
                    continue;
                }
                let child_tok = self.tokens.intern(child_name);

                children.entry(parent_path).or_default().push(child_tok);
            }
        }

        // Only add to parent's authored_children if not already present
        // (the prim's own primChildren field is authoritative).
        for (parent_path, names) in children {
            if let Some(parent) = prim_specs.get_mut(&parent_path) {
                append_unique(&mut parent.authored_children, names);
            }
        }
    }

    // ── Value conversion helpers ──────────────────────────────────────

    /// Converts a [`CrateValue`] to a [`Value`].
    fn convert_crate_value(&mut self, cv: &CrateValue) -> Value {
        match cv {
            CrateValue::None => Value::Blocked,
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
            CrateValue::Array(items) => {
                let vals: Vec<Value> = items.iter().map(|v| self.convert_crate_value(v)).collect();
                Value::Array(vals)
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
                Value::Array(vals)
            }
            CrateValue::TokenVector(toks) => {
                let vals: Vec<Value> = toks
                    .iter()
                    .map(|t| Value::Token(self.tokens.intern(t)))
                    .collect();
                Value::Array(vals)
            }
            CrateValue::DoubleVector(ds) => {
                let vals: Vec<Value> = ds.iter().map(|d| Value::Double(*d)).collect();
                Value::Array(vals)
            }
            CrateValue::StringVector(ss) => {
                let vals: Vec<Value> = ss
                    .iter()
                    .map(|s| Value::String(Arc::from(s.as_str())))
                    .collect();
                Value::Array(vals)
            }
            CrateValue::LayerOffsetVector(offsets) => {
                let vals: Vec<Value> = offsets
                    .iter()
                    .map(|(o, s)| Value::Array(alloc::vec![Value::Double(*o), Value::Double(*s)]))
                    .collect();
                Value::Array(vals)
            }
            CrateValue::RelocatesMap(_) => Value::Null,
            CrateValue::Spline(_) => {
                // Splines are handled as FieldValue::Spline, not plain values.
                Value::Null
            }
            CrateValue::ArrayEdit(edit) => Value::ArrayEdit(self.convert_array_edit(edit)),
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
        cv: &CrateValue,
    ) -> Result<Option<FieldValue>, UsdcError> {
        Ok(match cv {
            CrateValue::ListOp(listop) => {
                let converted = match listop.op_type {
                    ValueType::TokenListOp => {
                        Some(FieldValue::TokenListOp(self.convert_token_listop(listop)))
                    }
                    ValueType::PathListOp => {
                        match self.convert_connection_value(&CrateValue::ListOp(listop.clone())) {
                            Ok(converted) => Some(FieldValue::PathListOp(converted)),
                            Err(error) => {
                                self.report(spec_path, Some(field), alloc::format!("{error}"))?;
                                return Ok(None);
                            }
                        }
                    }
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
            | CrateValue::Spline(_)
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
        fields: &[(&str, CrateValue)],
    ) -> Result<PropertySpec, UsdcError> {
        let mut spec = PropertySpec::typed_attribute(self.attribute_property_type(fields));
        for (name, value) in fields {
            match (*name, value) {
                ("default", value) => spec.default = Some(self.convert_crate_value(value)),
                ("timeSamples", CrateValue::TimeSamples(samples)) => {
                    let samples = samples
                        .iter()
                        .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                        .collect();
                    spec.time_samples = Some(samples);
                }
                ("spline", CrateValue::Spline(spline)) => spec.spline = Some(spline.clone()),
                ("connectionPaths", value) => {
                    spec.targets = Some(self.convert_connection_value(value)?);
                }
                // Read by `attribute_property_type`; `connectionChildren`
                // restates `connectionPaths`.
                ("typeName" | "connectionChildren", _) => {}
                _ => self.apply_property_field(spec_path, name, value, &mut spec)?,
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
        value: &CrateValue,
        spec: &mut PropertySpec,
    ) -> Result<(), UsdcError> {
        match (name, value) {
            ("custom", CrateValue::Bool(custom)) => spec.custom = *custom,
            ("variability", CrateValue::Variability(variability)) => {
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
                    set_field_vec(&mut spec.metadata, key, field_value);
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
        fields: &[(&str, CrateValue)],
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

    fn attribute_property_type(&mut self, fields: &[(&str, CrateValue)]) -> PropertyType {
        let mut type_name = String::new();
        for (name, value) in fields {
            if *name == "typeName"
                && let CrateValue::Token(token) = value
            {
                type_name = token.clone();
                break;
            }
        }

        let inferred_array = fields.iter().any(|(name, value)| {
            matches!(*name, "default" | "timeSamples") && crate_value_is_array(value)
        });

        // `typeName` spells arrays with `[]` (`point3f[]`); the declared type
        // keeps the element type name and the array flag apart, as USDA
        // ingestion does.
        let is_array = type_name.ends_with("[]") || inferred_array;
        let base_name = type_name.strip_suffix("[]").unwrap_or(type_name.as_str());
        PropertyType::new(
            base_name,
            is_array,
            default_scalar_for_type(base_name, self.tokens),
        )
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

        Ok(result)
    }

    /// Converts a USDC path list op to a layerstack `ListOp<PathId>`.
    fn convert_path_listop(&mut self, listop: &CrateListOp) -> Result<ListOp<PathId>, UsdcError> {
        let mut result = ListOp::default();

        let convert_items = |items: &[CrateValue],
                             tokens: &mut TokenInterner,
                             paths: &mut PathInterner|
         -> Vec<PathId> {
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

        Ok(result)
    }

    /// Converts a USDC token list op to a layerstack `ListOp<TokenId>`.
    fn convert_token_listop(&mut self, listop: &CrateListOp) -> ListOp<TokenId> {
        let mut result = ListOp::default();

        let convert_items = |items: &[CrateValue], tokens: &mut TokenInterner| -> Vec<TokenId> {
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

        result
    }

    /// Converts a connection or target paths value to `ListOp<TargetPath>`.
    fn convert_connection_value(
        &mut self,
        value: &CrateValue,
    ) -> Result<ListOp<TargetPath>, UsdcError> {
        match value {
            CrateValue::ListOp(listop) => self.convert_target_listop(listop),
            CrateValue::PathVector(paths) => {
                let target_paths: Vec<TargetPath> = paths
                    .iter()
                    .filter_map(|s| TargetPath::parse(s, self.tokens, self.paths).ok())
                    .collect();
                Ok(ListOp {
                    explicit: Some(target_paths),
                    ..ListOp::default()
                })
            }
            _ => Ok(ListOp::default()),
        }
    }

    fn convert_target_listop(
        &mut self,
        listop: &CrateListOp,
    ) -> Result<ListOp<TargetPath>, UsdcError> {
        let mut result = ListOp::default();

        let convert_items = |items: &[CrateValue],
                             tokens: &mut TokenInterner,
                             paths: &mut PathInterner|
         -> Vec<TargetPath> {
            items
                .iter()
                .filter_map(|v| {
                    if let CrateValue::String(s) = v {
                        TargetPath::parse(s, tokens, paths).ok()
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

        Ok(result)
    }

    /// Converts a `payload` field value to a list op.
    ///
    /// The field holds a payload list op from crate 0.8. Earlier files hold
    /// a single `SdfPayload`, which is an explicit list of that payload, or
    /// no payload when empty (`pxr/usd/sdf/crateFile.cpp:393`).
    ///
    /// Spec: AOUSD Core §7.6.2.3.2 (`payload`).
    fn convert_payload_value(
        &mut self,
        value: &CrateValue,
    ) -> Result<Option<ListOp<Reference>>, UsdcError> {
        match value {
            CrateValue::ListOp(listop) => self.convert_ref_listop(listop).map(Some),
            CrateValue::Dictionary(_) => Ok(Some(ListOp {
                explicit: Some(self.convert_crate_to_reference(value).into_iter().collect()),
                ..ListOp::default()
            })),
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
                    "layerScale" => {
                        if let CrateValue::Double(v) = val {
                            layer_scale_val = *v;
                        }
                    }
                    _ => {}
                }
            }

            // Resolve the asset path.
            if asset_path.is_empty() && prim_path.is_empty() {
                return None;
            }

            let (layer, target) = if !asset_path.is_empty() {
                let resolved = self.resolve_asset(&asset_path);
                let lid = resolved
                    .as_ref()
                    .map(|r| r.layer_id)
                    .unwrap_or(self.layer_id);
                if let Some(r) = resolved
                    && let Some(layer) = r.layer
                {
                    self.resolved_layers.push(layer);
                }
                let target = if !prim_path.is_empty() {
                    Path::parse_absolute(&prim_path, self.tokens)
                        .ok()
                        .map(|p| ReferenceTarget::Prim(self.paths.intern(p)))
                        .unwrap_or(ReferenceTarget::DefaultPrim)
                } else {
                    ReferenceTarget::DefaultPrim
                };
                (lid, target)
            } else {
                // Internal reference (same layer).
                let target = if !prim_path.is_empty() {
                    Path::parse_absolute(&prim_path, self.tokens)
                        .ok()
                        .map(|p| ReferenceTarget::Prim(self.paths.intern(p)))
                        .unwrap_or(ReferenceTarget::DefaultPrim)
                } else {
                    ReferenceTarget::DefaultPrim
                };
                (self.layer_id, target)
            };

            let asset = if asset_path.is_empty() {
                None
            } else {
                Some(asset_path)
            };

            Some(Reference {
                layer,
                target,
                asset,
                layer_offset: LayerOffset {
                    offset: layer_offset_val,
                    scale: layer_scale_val,
                },
            })
        } else {
            None
        }
    }

    // ── Token/path helpers ──────────────────────────────────────────

    /// Returns the names a token or string list op adds, in order: its
    /// explicit, prepended and appended items.
    fn list_op_names(&mut self, listop: &CrateListOp) -> Vec<TokenId> {
        let items = listop.explicit_items.iter().flatten();
        let listed: Vec<TokenId> = items
            .chain(&listop.prepended_items)
            .chain(&listop.appended_items)
            .filter_map(|item| match item {
                CrateValue::Token(name) | CrateValue::String(name) => {
                    Some(self.tokens.intern(name))
                }
                _ => None,
            })
            .collect();
        let mut names = Vec::new();
        append_unique(&mut names, listed);
        names
    }

    /// Extracts token names from a [`CrateValue`] (typically a `TokenVector`
    /// or `Array` of tokens).
    fn extract_token_names(&mut self, value: &CrateValue) -> Vec<TokenId> {
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
    fn resolve_asset(&mut self, asset_path: &str) -> Option<ResolvedAsset> {
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

/// Parses a variant set path like `/Prim{varSetName=}` → `("/Prim", "varSetName")`.
fn parse_variant_set_path(path: &str) -> Option<(String, String)> {
    let open = path.find('{')?;
    let close = path.find('}')?;
    if close <= open + 1 {
        return None;
    }
    let prim_path = &path[..open];
    let inner = &path[open + 1..close];
    // Inner is "varSetName=" for variant sets.
    let vset_name = inner.strip_suffix('=')?;
    if vset_name.is_empty() {
        return None;
    }
    Some((String::from(prim_path), String::from(vset_name)))
}

/// Parses a variant path like `/Prim{varSetName=branchName}` →
/// `("/Prim", "varSetName", "branchName")`.
///
/// Returns `None` for variants nested below another variant branch, such as
/// `/Prim{a=x}{b=y}` or `/Prim{a=x}Child{b=y}`.
fn parse_variant_path(path: &str) -> Option<(String, String, String)> {
    let open = path.find('{')?;
    let close = path.find('}')?;
    if close <= open + 1 || close + 1 != path.len() {
        return None;
    }
    let prim_path = &path[..open];
    let inner = &path[open + 1..close];
    let eq = inner.find('=')?;
    let vset_name = &inner[..eq];
    let branch_name = &inner[eq + 1..];
    if vset_name.is_empty() || branch_name.is_empty() {
        return None;
    }
    Some((
        String::from(prim_path),
        String::from(vset_name),
        String::from(branch_name),
    ))
}

/// Parses a variant property path like `/Prim{varSet=branch}.attrName` →
/// `("/Prim", "varSet", "branch", "attrName")`.
fn parse_variant_property_path(path: &str) -> Option<(String, String, String, String)> {
    let open = path.find('{')?;
    let close = path.find('}')?;
    if close <= open + 1 {
        return None;
    }

    // Only properties directly on the variant: `/Prim{set=branch}.name`,
    // not `/Prim{set=branch}Child.name`.
    let prop_name = path[close + 1..].strip_prefix('.')?;
    if prop_name.is_empty() || prop_name.contains('{') {
        return None;
    }

    let prim_path = &path[..open];
    let inner = &path[open + 1..close];
    let eq = inner.find('=')?;
    let vset_name = &inner[..eq];
    let branch_name = &inner[eq + 1..];
    if vset_name.is_empty() || branch_name.is_empty() {
        return None;
    }

    Some((
        String::from(prim_path),
        String::from(vset_name),
        String::from(branch_name),
        String::from(prop_name),
    ))
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
    })
}

/// Orders `properties` by their position in `children` (`propertyChildren`),
/// keeping properties not listed there after the listed ones, in order.
///
/// Positions are looked up in a map: both lists come from the file, so
/// searching `children` for each property would take quadratic time.
fn sort_by_children(properties: &mut [PropertyEntry], children: &[TokenId]) {
    let mut position: HashMap<TokenId, usize> = HashMap::new();
    for (i, child) in children.iter().enumerate() {
        position.entry(*child).or_insert(i);
    }
    properties.sort_by_key(|entry| position.get(&entry.name).copied().unwrap_or(usize::MAX));
}

fn crate_value_is_array(value: &CrateValue) -> bool {
    match value {
        CrateValue::Array(_) | CrateValue::ArrayEdit(_) => true,
        CrateValue::TimeSamples(samples) => samples
            .iter()
            .any(|(_, sample)| crate_value_is_array(sample)),
        _ => false,
    }
}

fn default_scalar_for_type(type_hint: &str, tokens: &mut TokenInterner) -> Value {
    match type_hint {
        "bool" => Value::Bool(false),
        "uchar" => Value::UChar(0),
        "int" => Value::Int(0),
        "uint" => Value::UInt(0),
        "int64" => Value::Int64(0),
        "uint64" => Value::UInt64(0),
        "half" => Value::Half(0),
        "float" => Value::Float(0.0),
        "double" => Value::Double(0.0),
        "string" => Value::String(Arc::from("")),
        "token" => Value::Token(tokens.intern("")),
        "asset" => Value::Asset(Arc::from("")),
        "pathExpression" => Value::PathExpression(Arc::from("")),
        "timecode" => Value::TimeCode(0.0),
        "double2" => Value::Vec2d([0.0; 2]),
        "double3" => Value::Vec3d([0.0; 3]),
        "double4" => Value::Vec4d([0.0; 4]),
        "float2" => Value::Vec2f([0.0; 2]),
        "float3" => Value::Vec3f([0.0; 3]),
        "float4" => Value::Vec4f([0.0; 4]),
        "half2" => Value::Vec2h([0; 2]),
        "half3" => Value::Vec3h([0; 3]),
        "half4" => Value::Vec4h([0; 4]),
        "int2" => Value::Vec2i([0; 2]),
        "int3" => Value::Vec3i([0; 3]),
        "int4" => Value::Vec4i([0; 4]),
        "matrix2d" => Value::Matrix2d(Box::new([0.0; 4])),
        "matrix3d" => Value::Matrix3d(Box::new([0.0; 9])),
        "matrix4d" => Value::Matrix4d(Box::new([0.0; 16])),
        "quatd" => Value::Quatd([0.0; 4]),
        "quatf" => Value::Quatf([0.0; 4]),
        "quath" => Value::Quath([0; 4]),
        type_name
            if type_name.ends_with('f')
                && (type_name.starts_with("color")
                    || type_name.starts_with("normal")
                    || type_name.starts_with("point")
                    || type_name.starts_with("vector")
                    || type_name.starts_with("texCoord")) =>
        {
            match semantic_component_count(type_name) {
                2 => Value::Vec2f([0.0; 2]),
                3 => Value::Vec3f([0.0; 3]),
                _ => Value::Vec4f([0.0; 4]),
            }
        }
        type_name
            if type_name.ends_with('d')
                && (type_name.starts_with("color")
                    || type_name.starts_with("normal")
                    || type_name.starts_with("point")
                    || type_name.starts_with("vector")
                    || type_name.starts_with("texCoord")) =>
        {
            match semantic_component_count(type_name) {
                2 => Value::Vec2d([0.0; 2]),
                3 => Value::Vec3d([0.0; 3]),
                _ => Value::Vec4d([0.0; 4]),
            }
        }
        type_name
            if type_name.ends_with('h')
                && (type_name.starts_with("color")
                    || type_name.starts_with("normal")
                    || type_name.starts_with("point")
                    || type_name.starts_with("vector")
                    || type_name.starts_with("texCoord")) =>
        {
            match semantic_component_count(type_name) {
                2 => Value::Vec2h([0; 2]),
                3 => Value::Vec3h([0; 3]),
                _ => Value::Vec4h([0; 4]),
            }
        }
        "dictionary" => Value::Dictionary(Vec::new()),
        _ => Value::Null,
    }
}

fn semantic_component_count(name: &str) -> usize {
    name.chars()
        .rev()
        .nth(1)
        .and_then(|c| c.to_digit(10))
        .unwrap_or(0) as usize
}

/// Returns the parent prim path for a path like `/A/B` → `/A`, `/A` → `/`.
fn parent_prim_path(path: &str) -> Option<String> {
    if path == "/" {
        return None;
    }
    // Don't compute parent for variant paths.
    if path.contains('{') {
        return None;
    }
    if let Some(last_slash) = path.rfind('/') {
        if last_slash == 0 {
            Some(String::from("/"))
        } else {
            Some(String::from(&path[..last_slash]))
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

fn read_f64x2(d: &[u8]) -> [f64; 2] {
    [f64_le(d, 0), f64_le(d, 1)]
}

fn read_f64x3(d: &[u8]) -> [f64; 3] {
    [f64_le(d, 0), f64_le(d, 1), f64_le(d, 2)]
}

fn read_f64x4(d: &[u8]) -> [f64; 4] {
    [f64_le(d, 0), f64_le(d, 1), f64_le(d, 2), f64_le(d, 3)]
}

fn read_f32x2(d: &[u8]) -> [f32; 2] {
    [f32_le(d, 0), f32_le(d, 1)]
}

fn read_f32x3(d: &[u8]) -> [f32; 3] {
    [f32_le(d, 0), f32_le(d, 1), f32_le(d, 2)]
}

fn read_f32x4(d: &[u8]) -> [f32; 4] {
    [f32_le(d, 0), f32_le(d, 1), f32_le(d, 2), f32_le(d, 3)]
}

fn read_u16x2(d: &[u8]) -> [u16; 2] {
    [u16_le(d, 0), u16_le(d, 1)]
}

fn read_u16x3(d: &[u8]) -> [u16; 3] {
    [u16_le(d, 0), u16_le(d, 1), u16_le(d, 2)]
}

fn read_u16x4(d: &[u8]) -> [u16; 4] {
    [u16_le(d, 0), u16_le(d, 1), u16_le(d, 2), u16_le(d, 3)]
}

fn read_i32x2(d: &[u8]) -> [i32; 2] {
    [i32_le(d, 0), i32_le(d, 1)]
}

fn read_i32x3(d: &[u8]) -> [i32; 3] {
    [i32_le(d, 0), i32_le(d, 1), i32_le(d, 2)]
}

fn read_i32x4(d: &[u8]) -> [i32; 4] {
    [i32_le(d, 0), i32_le(d, 1), i32_le(d, 2), i32_le(d, 3)]
}

fn read_f64_array<const N: usize>(d: &[u8]) -> [f64; N] {
    let mut out = [0.0_f64; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = f64_le(d, i);
    }
    out
}

// In the readers below, `idx` is a component index of a vector, quaternion
// or matrix (at most 15), so the offsets cannot overflow.

fn f64_le(d: &[u8], idx: usize) -> f64 {
    let off = idx * 8;
    f64::from_le_bytes(
        d.get(off..off + 8)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 8]),
    )
}

fn f32_le(d: &[u8], idx: usize) -> f32 {
    let off = idx * 4;
    f32::from_le_bytes(
        d.get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    )
}

fn u16_le(d: &[u8], idx: usize) -> u16 {
    let off = idx * 2;
    u16::from_le_bytes(
        d.get(off..off + 2)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 2]),
    )
}

fn i32_le(d: &[u8], idx: usize) -> i32 {
    let off = idx * 4;
    i32::from_le_bytes(
        d.get(off..off + 4)
            .and_then(|b| b.try_into().ok())
            .unwrap_or([0; 4]),
    )
}

// ---------------------------------------------------------------------------
// ListOp merge helpers
// ---------------------------------------------------------------------------

/// Merges a source reference list op into a target.
fn merge_ref_listop(target: &mut ListOp<Reference>, source: ListOp<Reference>) {
    if source.explicit.is_some() {
        target.explicit = source.explicit;
    }
    target.prepend.extend(source.prepend);
    target.append.extend(source.append);
    target.delete.extend(source.delete);
}

/// Merges a source path list op into a target.
fn merge_path_listop(target: &mut ListOp<PathId>, source: ListOp<PathId>) {
    if source.explicit.is_some() {
        target.explicit = source.explicit;
    }
    target.prepend.extend(source.prepend);
    target.append.extend(source.append);
    target.delete.extend(source.delete);
}

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
            _: &mut TokenInterner,
            _: &mut PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::NotFound)
        }

        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
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
            &mut TokenInterner::default(),
            &mut PathInterner::default(),
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
    fn split_property_simple() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let property = PropertyPath::parse("/Cube.size", &mut tokens, &mut paths).unwrap();
        assert_eq!(paths.display(property.prim_path(), &tokens), "/Cube");
        assert_eq!(tokens.resolve(property.property()), "size");
    }

    #[test]
    fn split_property_nested() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let property =
            PropertyPath::parse("/World/Cube.visibility", &mut tokens, &mut paths).unwrap();
        assert_eq!(paths.display(property.prim_path(), &tokens), "/World/Cube");
        assert_eq!(tokens.resolve(property.property()), "visibility");
    }

    #[test]
    fn split_property_none() {
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        assert!(PropertyPath::parse("/Cube", &mut tokens, &mut paths).is_err());
    }

    #[test]
    fn parse_variant_set_path_ok() {
        let (prim, vset) = parse_variant_set_path("/Prim{shadingVariant=}").unwrap();
        assert_eq!(prim, "/Prim");
        assert_eq!(vset, "shadingVariant");
    }

    #[test]
    fn parse_variant_path_ok() {
        let (prim, vset, branch) = parse_variant_path("/Prim{shadingVariant=red}").unwrap();
        assert_eq!(prim, "/Prim");
        assert_eq!(vset, "shadingVariant");
        assert_eq!(branch, "red");
    }

    #[test]
    fn parse_variant_property_path_ok() {
        let (prim, vset, branch, prop) =
            parse_variant_property_path("/Prim{shadingVariant=red}.color").unwrap();
        assert_eq!(prim, "/Prim");
        assert_eq!(vset, "shadingVariant");
        assert_eq!(branch, "red");
        assert_eq!(prop, "color");
    }

    #[test]
    fn variant_paths_below_variant_children_are_not_the_variant() {
        // A child prim of a variant, its property, and a nested selection.
        for path in [
            "/Prim{set=red}Child",
            "/Prim{set=red}Child.color",
            "/Prim{set=red}{inner=}",
            "/Prim{set=red}{inner=x}",
            "/Prim{set=red}{inner=x}.color",
        ] {
            assert_eq!(parse_variant_set_path(path), None, "{path}");
            assert_eq!(parse_variant_path(path), None, "{path}");
            assert_eq!(parse_variant_property_path(path), None, "{path}");
        }
    }

    #[test]
    fn parent_prim_path_root_child() {
        assert_eq!(parent_prim_path("/Cube"), Some(String::from("/")));
    }

    #[test]
    fn parent_prim_path_nested() {
        assert_eq!(
            parent_prim_path("/World/Cube"),
            Some(String::from("/World"))
        );
    }

    #[test]
    fn parent_prim_path_root() {
        assert!(parent_prim_path("/").is_none());
    }
}
