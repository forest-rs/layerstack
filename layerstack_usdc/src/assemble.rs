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

use layerstack::HashMap;

use layerstack::doc::{
    FieldValue, Layer, LayerId, LayerOffset, PrimSpec, Reference, Specifier, SublayerEntry, Value,
    VariantSetSpec, VariantSpec, insert_property_field_if_absent, set_field_vec,
    set_property_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner, PropertyPath, TargetPath};
#[cfg(feature = "experimental_sparse_array_edits")]
use layerstack::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};
use layerstack::{AssetResolver, PropertyType, ReferenceTarget, ResolvedAsset};

use crate::error::UsdcError;
use crate::section::CrateSections;
use crate::value_rep::{CrateListOp, CrateValue, RawValueRep, decode_value};
use crate::value_type::{SpecForm, ValueType};

/// Result of assembling a USDC file into a layer.
#[derive(Debug)]
pub struct AssembleResult {
    /// The assembled layer.
    pub layer: Layer,
    /// Layers produced by resolving asset paths (sublayers, references,
    /// payloads). The caller should insert these into their store.
    pub resolved_layers: Vec<Layer>,
}

/// Assembles decoded USDC sections into a [`Layer`].
///
/// `data` is the full file byte slice (needed for offset-based value reads).
/// `sections` holds the decoded token/string/path/field/spec tables.
///
/// Spec: AOUSD Core §16.3.
pub fn assemble(
    data: &[u8],
    sections: &CrateSections,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> Result<AssembleResult, UsdcError> {
    let mut ctx = AssembleCtx {
        data,
        sections,
        tokens,
        paths,
        resolver,
        layer_id,
        resolved_layers: Vec::new(),
    };

    let layer = ctx.assemble_layer()?;

    Ok(AssembleResult {
        layer,
        resolved_layers: ctx.resolved_layers,
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
}

impl AssembleCtx<'_> {
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
        let spec_fields: Vec<Vec<(String, CrateValue)>> = self
            .sections
            .specs
            .iter()
            .map(|spec| self.collect_fields(spec.fieldset_index))
            .collect::<Result<_, _>>()?;

        // Build a path-to-spec-index map grouped by prim path for child lookup.
        let mut prim_specs_map: HashMap<String, PrimSpec> = HashMap::new();

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
                let prim = self.build_prim_spec(&spec_fields[i])?;
                prim_specs_map.insert(path_str, prim);
            }
        }

        // Process Attribute specs — add fields to their parent prim.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Attribute {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Ok(property_path) = PropertyPath::parse(&path_str, self.tokens, self.paths) {
                    let prim_path = self.paths.display(property_path.prim_path(), self.tokens);
                    let attr_name = String::from(self.tokens.resolve(property_path.property()));
                    if let Some(prim) = prim_specs_map.get_mut(&prim_path) {
                        self.apply_attribute_fields(&spec_fields[i], &attr_name, prim)?;
                    }
                }
            }
        }

        // Process Relationship specs — add fields to their parent prim.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Relationship {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Ok(property_path) = PropertyPath::parse(&path_str, self.tokens, self.paths) {
                    let prim_path = self.paths.display(property_path.prim_path(), self.tokens);
                    let rel_name = String::from(self.tokens.resolve(property_path.property()));
                    if let Some(prim) = prim_specs_map.get_mut(&prim_path) {
                        self.apply_relationship_fields(&spec_fields[i], &rel_name, prim)?;
                    }
                }
            }
        }

        // Process Connection specs — add connection paths to attribute on parent.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Connection {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Ok(property_path) = PropertyPath::parse(&path_str, self.tokens, self.paths) {
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

        // Establish parent-child relationships.
        self.build_child_relationships(&mut prim_specs_map);

        // Convert prim_specs_map into layer prims.
        for (path_str, prim) in prim_specs_map {
            if let Ok(path) = Path::parse_absolute(&path_str, self.tokens) {
                let path_id = self.paths.intern(path);
                layer.insert_prim(path_id, prim);
            }
        }

        Ok(layer)
    }

    /// Collects decoded fields for a spec from the fieldsets/fields tables.
    fn collect_fields(&self, fieldset_index: u32) -> Result<Vec<(String, CrateValue)>, UsdcError> {
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
                let rep = RawValueRep::new(field_def.value_rep);
                let value = decode_value(&rep, self.data, self.sections)?;
                result.push((field_name, value));
            }
            idx += 1;
        }

        Ok(result)
    }

    /// Processes a `PseudoRoot` spec to extract layer metadata.
    fn process_pseudo_root(
        &mut self,
        fields: &[(String, CrateValue)],
        layer: &mut Layer,
    ) -> Result<(), UsdcError> {
        let mut root_children = Vec::new();
        let mut prim_order: Option<Vec<TokenId>> = None;

        for (name, value) in fields {
            match name.as_str() {
                "subLayers" => {
                    if let CrateValue::PathVector(paths) = value {
                        for asset_path in paths {
                            if let Some(resolved) = self.resolve_asset(asset_path) {
                                layer.sublayers.push(SublayerEntry::new(resolved.layer_id));
                                if let Some(sub_layer) = resolved.layer {
                                    self.resolved_layers.push(sub_layer);
                                }
                            }
                        }
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
                _ => {
                    // Other pseudo-root fields (defaultPrim, doc, etc.)
                    // are handled via the root prim spec below.
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

    /// Builds a [`PrimSpec`] from decoded fields.
    fn build_prim_spec(&mut self, fields: &[(String, CrateValue)]) -> Result<PrimSpec, UsdcError> {
        let mut spec = PrimSpec::default();

        for (name, value) in fields {
            match name.as_str() {
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
                "references" => {
                    if let CrateValue::ListOp(listop) = value {
                        let converted = self.convert_ref_listop(listop)?;
                        merge_ref_listop(&mut spec.references, converted);
                    }
                }
                "payloads" => {
                    if let CrateValue::ListOp(listop) = value {
                        let converted = self.convert_ref_listop(listop)?;
                        merge_ref_listop(&mut spec.payloads, converted);
                    }
                }
                "inherits" => {
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
                    // Ordered variant set names.
                    let names = self.extract_token_names(value);
                    for name in names {
                        if !spec.variant_set_order.contains(&name) {
                            spec.variant_set_order.push(name);
                        }
                    }
                }
                "instanceable" => {
                    if let CrateValue::Bool(b) = value {
                        spec.instanceable = Some(*b);
                    }
                }
                "active" => {
                    if let CrateValue::Bool(b) = value {
                        spec.active = Some(*b);
                    }
                }
                "kind" => {
                    let key = self.tokens.intern("kind");
                    let val = self.convert_crate_value(value);
                    set_field_vec(&mut spec.fields, key, FieldValue::Value(val));
                }
                "documentation" => {
                    let key = self.tokens.intern("documentation");
                    let val = self.convert_crate_value(value);
                    set_field_vec(&mut spec.fields, key, FieldValue::Value(val));
                }
                _ => {
                    // Generic metadata field.
                    let key = self.tokens.intern(name);
                    let field_value = self.convert_field_value(value);
                    set_field_vec(&mut spec.fields, key, field_value);
                }
            }
        }

        Ok(spec)
    }

    /// Applies attribute fields to the parent prim.
    fn apply_attribute_fields(
        &mut self,
        fields: &[(String, CrateValue)],
        attr_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(attr_name);
        let property_type = self.attribute_property_type(fields);

        // Look for time samples, spline, default value, or connection paths.
        // Priority: timeSamples > spline > default (§12.3).
        let mut has_time_samples = false;
        let mut has_spline = false;
        let mut has_default = false;

        for (field_name, value) in fields {
            match field_name.as_str() {
                "timeSamples" => {
                    if let CrateValue::TimeSamples(samples) = value {
                        let ts: Vec<(f64, Value)> = samples
                            .iter()
                            .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                            .collect();
                        set_property_field_vec(
                            &mut prim.fields,
                            name_tok,
                            FieldValue::TimeSamples(ts),
                            property_type.clone(),
                        );
                        has_time_samples = true;
                    }
                }
                "spline" => {
                    if !has_time_samples && let CrateValue::Spline(spline) = value {
                        set_property_field_vec(
                            &mut prim.fields,
                            name_tok,
                            FieldValue::Spline(spline.clone()),
                            property_type.clone(),
                        );
                        has_spline = true;
                    }
                }
                "default" if !has_time_samples && !has_spline => {
                    let converted = self.convert_crate_value(value);
                    set_property_field_vec(
                        &mut prim.fields,
                        name_tok,
                        FieldValue::Value(converted),
                        property_type.clone(),
                    );
                    has_default = true;
                }
                "default" => {}
                "connectionPaths" => {
                    let listop = self.convert_connection_value(value)?;
                    set_property_field_vec(
                        &mut prim.fields,
                        name_tok,
                        FieldValue::PathListOp(listop),
                        property_type.clone(),
                    );
                }
                // Skip metadata fields (variability, custom, etc.).
                "variability" | "custom" | "typeName" => {}
                _ => {
                    // Other attribute metadata gets stored as a sub-field.
                    // For now we skip these; they're rarely needed.
                }
            }
        }

        // If the attribute was declared with no default, spline, or time samples,
        // register as Null (attribute declaration).
        if !has_time_samples && !has_spline && !has_default {
            let has_connection = fields.iter().any(|(n, _)| n == "connectionPaths");
            if !has_connection {
                insert_property_field_if_absent(
                    &mut prim.fields,
                    name_tok,
                    FieldValue::Value(Value::Null),
                    property_type,
                );
            }
        }

        Ok(())
    }

    /// Applies relationship fields to the parent prim.
    fn apply_relationship_fields(
        &mut self,
        fields: &[(String, CrateValue)],
        rel_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(rel_name);

        for (field_name, value) in fields {
            if field_name == "targetPaths" {
                let listop = self.convert_connection_value(value)?;
                set_field_vec(&mut prim.fields, name_tok, FieldValue::PathListOp(listop));
                return Ok(());
            }
        }

        // Relationship with no targets — register as empty PathListOp.
        layerstack::insert_field_if_absent(
            &mut prim.fields,
            name_tok,
            FieldValue::PathListOp(ListOp::default()),
        );

        Ok(())
    }

    /// Applies connection fields to an attribute on the parent prim.
    fn apply_connection_fields(
        &mut self,
        fields: &[(String, CrateValue)],
        attr_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(attr_name);

        for (field_name, value) in fields {
            if field_name == "connectionPaths" || field_name == "targetPaths" {
                let listop = self.convert_connection_value(value)?;
                set_field_vec(&mut prim.fields, name_tok, FieldValue::PathListOp(listop));
                return Ok(());
            }
        }

        Ok(())
    }

    /// Processes `VariantSet` and `Variant` specs.
    fn process_variant_specs(
        &mut self,
        spec_fields: &[Vec<(String, CrateValue)>],
        prim_specs: &mut HashMap<String, PrimSpec>,
    ) -> Result<(), UsdcError> {
        // Collect variant set info: path → variant set name → variant branches.
        // USDC stores variant set paths as `/Prim{varSet=}`
        // and variant paths as `/Prim{varSet=branchName}`.
        for spec in &self.sections.specs {
            if spec.form == SpecForm::VariantSet {
                let path_str = self.lookup_path(spec.path_index)?;
                // Path like: /Prim{varSetName=}
                if let Some((prim_path, vset_name)) = parse_variant_set_path(&path_str)
                    && let Some(prim) = prim_specs.get_mut(&prim_path)
                {
                    let vset_tok = self.tokens.intern(&vset_name);
                    if !prim.variant_set_order.contains(&vset_tok) {
                        prim.variant_set_order.push(vset_tok);
                    }
                    prim.variant_sets
                        .entry(vset_tok)
                        .or_insert_with(VariantSetSpec::default);
                }
            }
        }

        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Variant {
                let path_str = self.lookup_path(spec.path_index)?;
                // Path like: /Prim{varSetName=branchName}
                if let Some((prim_path, vset_name, branch_name)) = parse_variant_path(&path_str)
                    && let Some(prim) = prim_specs.get_mut(&prim_path)
                {
                    let vset_tok = self.tokens.intern(&vset_name);
                    let branch_tok = self.tokens.intern(&branch_name);
                    let vset = prim
                        .variant_sets
                        .entry(vset_tok)
                        .or_insert_with(VariantSetSpec::default);

                    let mut variant = VariantSpec::default();

                    // Process variant fields.
                    for (name, value) in &spec_fields[i] {
                        match name.as_str() {
                            "primChildren" => {
                                variant.authored_children = self.extract_token_names(value);
                            }
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
                            "payloads" => {
                                if let CrateValue::ListOp(listop) = value
                                    && let Ok(converted) = self.convert_ref_listop(listop)
                                {
                                    merge_ref_listop(&mut variant.payloads, converted);
                                }
                            }
                            "inherits" => {
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
                                let key = self.tokens.intern(name);
                                let fv = self.convert_field_value(value);
                                set_field_vec(&mut variant.fields, key, fv);
                            }
                        }
                    }

                    vset.variants.insert(branch_tok, variant);
                }
            }
        }

        // Process attribute/relationship specs that live under variant paths.
        // These have paths like /Prim{varSet=branch}.attrName or
        // /Prim{varSet=branch}/Child.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Attribute || spec.form == SpecForm::Relationship {
                let path_str = self.lookup_path(spec.path_index)?;
                // Check if this is under a variant context.
                if let Some((prim_path, vset_name, branch_name, prop_name)) =
                    parse_variant_property_path(&path_str)
                    && let Some(prim) = prim_specs.get_mut(&prim_path)
                {
                    let vset_tok = self.tokens.intern(&vset_name);
                    let branch_tok = self.tokens.intern(&branch_name);
                    if let Some(vset) = prim.variant_sets.get_mut(&vset_tok)
                        && let Some(variant) = vset.variants.get_mut(&branch_tok)
                    {
                        let prop_tok = self.tokens.intern(&prop_name);
                        let fv = if spec.form == SpecForm::Attribute {
                            self.build_attribute_field_value(&spec_fields[i])
                        } else {
                            self.build_relationship_field_value(&spec_fields[i])?
                        };
                        if spec.form == SpecForm::Attribute {
                            let property_type = self.attribute_property_type(&spec_fields[i]);
                            set_property_field_vec(
                                &mut variant.fields,
                                prop_tok,
                                fv,
                                property_type,
                            );
                        } else {
                            set_field_vec(&mut variant.fields, prop_tok, fv);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Builds parent-child relationships by examining prim paths.
    fn build_child_relationships(&mut self, prim_specs: &mut HashMap<String, PrimSpec>) {
        // Collect all prim paths.
        let prim_paths: Vec<String> = prim_specs.keys().cloned().collect();

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

                // Only add to parent's authored_children if not already present
                // (the prim's own primChildren field is authoritative).
                if let Some(parent) = prim_specs.get_mut(&parent_path)
                    && !parent.authored_children.contains(&child_tok)
                {
                    parent.authored_children.push(child_tok);
                }
            }
        }
    }

    // ── Value conversion helpers ──────────────────────────────────────

    /// Converts a [`CrateValue`] to a [`Value`].
    fn convert_crate_value(&mut self, cv: &CrateValue) -> Value {
        if let Some(edit) = self.try_convert_experimental_array_edit(cv) {
            return edit;
        }
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
            CrateValue::String(s) => Value::String(Arc::from(s.as_str())),
            CrateValue::Token(t) => Value::Token(self.tokens.intern(t)),
            CrateValue::AssetPath(p) => Value::Asset(Arc::from(p.as_str())),
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
        }
    }

    #[cfg(feature = "experimental_sparse_array_edits")]
    fn try_convert_experimental_array_edit(&mut self, cv: &CrateValue) -> Option<Value> {
        let CrateValue::Dictionary(entries) = cv else {
            return None;
        };

        let is_marker = entries.iter().any(|(key, value)| {
            key == "__layerstack_sparse_array_edit__" && matches!(value, CrateValue::Bool(true))
        });
        if !is_marker {
            return None;
        }

        let CrateValue::Array(op_values) = entries
            .iter()
            .find(|(key, _)| key == "ops")
            .map(|(_, value)| value)?
        else {
            return None;
        };

        let mut ops = Vec::with_capacity(op_values.len());
        for op_value in op_values {
            ops.push(self.convert_experimental_array_edit_op(op_value)?);
        }
        Some(Value::ArrayEdit(ArrayEdit { ops }))
    }

    #[cfg(not(feature = "experimental_sparse_array_edits"))]
    fn try_convert_experimental_array_edit(&mut self, _cv: &CrateValue) -> Option<Value> {
        None
    }

    #[cfg(feature = "experimental_sparse_array_edits")]
    fn convert_experimental_array_edit_op(&mut self, value: &CrateValue) -> Option<ArrayEditOp> {
        let CrateValue::Dictionary(entries) = value else {
            return None;
        };

        let op_name = entries.iter().find_map(|(key, value)| {
            (key == "op").then_some(match value {
                CrateValue::String(text) | CrateValue::Token(text) => Some(text.as_str()),
                _ => None,
            })?
        })?;

        match op_name {
            "write" => Some(ArrayEditOp::Write {
                src: self.convert_experimental_array_edit_operand(
                    entries
                        .iter()
                        .find(|(key, _)| key == "src")
                        .map(|(_, value)| value)?,
                )?,
                index: self.convert_experimental_array_edit_index(
                    entries
                        .iter()
                        .find(|(key, _)| key == "index")
                        .map(|(_, value)| value)?,
                )?,
            }),
            "insert" => Some(ArrayEditOp::Insert {
                src: self.convert_experimental_array_edit_operand(
                    entries
                        .iter()
                        .find(|(key, _)| key == "src")
                        .map(|(_, value)| value)?,
                )?,
                index: self.convert_experimental_array_edit_index(
                    entries
                        .iter()
                        .find(|(key, _)| key == "index")
                        .map(|(_, value)| value)?,
                )?,
            }),
            "prepend" => Some(ArrayEditOp::Insert {
                src: self.convert_experimental_array_edit_operand(
                    entries
                        .iter()
                        .find(|(key, _)| key == "src")
                        .map(|(_, value)| value)?,
                )?,
                index: ArrayIndex::Position(0),
            }),
            "append" => Some(ArrayEditOp::Insert {
                src: self.convert_experimental_array_edit_operand(
                    entries
                        .iter()
                        .find(|(key, _)| key == "src")
                        .map(|(_, value)| value)?,
                )?,
                index: ArrayIndex::End,
            }),
            "erase" => Some(ArrayEditOp::Erase {
                index: self.convert_experimental_array_edit_index(
                    entries
                        .iter()
                        .find(|(key, _)| key == "index")
                        .map(|(_, value)| value)?,
                )?,
            }),
            "minsize" => Some(ArrayEditOp::MinSize {
                len: self.convert_experimental_array_edit_len(
                    entries
                        .iter()
                        .find(|(key, _)| key == "len")
                        .map(|(_, value)| value)?,
                )?,
            }),
            "maxsize" => Some(ArrayEditOp::MaxSize {
                len: self.convert_experimental_array_edit_len(
                    entries
                        .iter()
                        .find(|(key, _)| key == "len")
                        .map(|(_, value)| value)?,
                )?,
            }),
            "resize" => Some(ArrayEditOp::Resize {
                len: self.convert_experimental_array_edit_len(
                    entries
                        .iter()
                        .find(|(key, _)| key == "len")
                        .map(|(_, value)| value)?,
                )?,
            }),
            _ => None,
        }
    }

    #[cfg(feature = "experimental_sparse_array_edits")]
    fn convert_experimental_array_edit_operand(
        &mut self,
        value: &CrateValue,
    ) -> Option<ArrayEditOperand> {
        let CrateValue::Dictionary(entries) = value else {
            return None;
        };

        if let Some(literal) = entries
            .iter()
            .find(|(key, _)| key == "literal")
            .map(|(_, value)| value)
        {
            return Some(ArrayEditOperand::Literal(self.convert_crate_value(literal)));
        }

        entries
            .iter()
            .find(|(key, _)| key == "copyFrom")
            .map(|(_, value)| value)
            .and_then(|value| self.convert_experimental_array_edit_index(value))
            .map(ArrayEditOperand::CopyFrom)
    }

    #[cfg(feature = "experimental_sparse_array_edits")]
    fn convert_experimental_array_edit_index(&self, value: &CrateValue) -> Option<ArrayIndex> {
        match value {
            CrateValue::Int(v) => Some(ArrayIndex::Position(i64::from(*v))),
            CrateValue::Int64(v) => Some(ArrayIndex::Position(*v)),
            CrateValue::UInt(v) => Some(ArrayIndex::Position(i64::from(*v))),
            CrateValue::UInt64(v) => i64::try_from(*v).ok().map(ArrayIndex::Position),
            CrateValue::String(text) | CrateValue::Token(text) if text == "end" => {
                Some(ArrayIndex::End)
            }
            _ => None,
        }
    }

    #[cfg(feature = "experimental_sparse_array_edits")]
    fn convert_experimental_array_edit_len(&self, value: &CrateValue) -> Option<usize> {
        match value {
            CrateValue::Int(v) if *v >= 0 => usize::try_from(*v).ok(),
            CrateValue::Int64(v) if *v >= 0 => usize::try_from(*v).ok(),
            CrateValue::UInt(v) => usize::try_from(*v).ok(),
            CrateValue::UInt64(v) => usize::try_from(*v).ok(),
            _ => None,
        }
    }

    /// Converts a [`CrateValue`] to a [`FieldValue`], handling list ops
    /// and time samples specially.
    fn convert_field_value(&mut self, cv: &CrateValue) -> FieldValue {
        match cv {
            CrateValue::ListOp(listop) => {
                match listop.op_type {
                    ValueType::TokenListOp => {
                        let converted = self.convert_token_listop(listop);
                        FieldValue::TokenListOp(converted)
                    }
                    ValueType::PathListOp => {
                        if let Ok(converted) =
                            self.convert_connection_value(&CrateValue::ListOp(listop.clone()))
                        {
                            FieldValue::PathListOp(converted)
                        } else {
                            FieldValue::Value(Value::Null)
                        }
                    }
                    _ => {
                        // Other list op types: convert items to array values.
                        FieldValue::Value(self.convert_crate_value(cv))
                    }
                }
            }
            CrateValue::TimeSamples(samples) => {
                let ts: Vec<(f64, Value)> = samples
                    .iter()
                    .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                    .collect();
                FieldValue::TimeSamples(ts)
            }
            CrateValue::Spline(spline) => FieldValue::Spline(spline.clone()),
            _ => FieldValue::Value(self.convert_crate_value(cv)),
        }
    }

    /// Builds a [`FieldValue`] from attribute spec fields.
    fn build_attribute_field_value(&mut self, fields: &[(String, CrateValue)]) -> FieldValue {
        // Check for time samples first.
        for (name, value) in fields {
            if name == "timeSamples"
                && let CrateValue::TimeSamples(samples) = value
            {
                let ts: Vec<(f64, Value)> = samples
                    .iter()
                    .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                    .collect();
                return FieldValue::TimeSamples(ts);
            }
        }

        // Check for connection paths.
        for (name, value) in fields {
            if name == "connectionPaths"
                && let Ok(listop) = self.convert_connection_value(value)
            {
                return FieldValue::PathListOp(listop);
            }
        }

        // Default value.
        for (name, value) in fields {
            if name == "default" {
                return FieldValue::Value(self.convert_crate_value(value));
            }
        }

        // No value — declaration only.
        FieldValue::Value(Value::Null)
    }

    /// Builds a [`FieldValue`] from relationship spec fields.
    fn build_relationship_field_value(
        &mut self,
        fields: &[(String, CrateValue)],
    ) -> Result<FieldValue, UsdcError> {
        for (name, value) in fields {
            if name == "targetPaths" {
                let listop = self.convert_connection_value(value)?;
                return Ok(FieldValue::PathListOp(listop));
            }
        }
        Ok(FieldValue::PathListOp(ListOp::default()))
    }

    fn attribute_property_type(&mut self, fields: &[(String, CrateValue)]) -> PropertyType {
        let mut type_name = String::new();
        for (name, value) in fields {
            if name == "typeName"
                && let CrateValue::Token(token) = value
            {
                type_name = token.clone();
                break;
            }
        }

        let inferred_array = fields.iter().any(|(name, value)| {
            matches!(name.as_str(), "default" | "timeSamples") && crate_value_is_array(value)
        });

        let is_array = type_name.ends_with("[]") || inferred_array;
        let base_name = type_name.strip_suffix("[]").unwrap_or(type_name.as_str());
        PropertyType::new(
            if type_name.is_empty() {
                base_name
            } else {
                type_name.as_str()
            },
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

    /// Looks up a path string from the paths table.
    fn lookup_path(&self, index: u32) -> Result<String, UsdcError> {
        let idx = index as usize;
        if idx < self.sections.paths.len() {
            Ok(self.sections.paths[idx].clone())
        } else {
            Err(UsdcError::Inconsistent {
                message: "path index out of range",
            })
        }
    }

    /// Looks up a token string from the tokens table.
    fn lookup_token(&self, index: u32) -> String {
        let idx = index as usize;
        if idx < self.sections.tokens.len() {
            self.sections.tokens[idx].clone()
        } else {
            String::new()
        }
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
fn parse_variant_path(path: &str) -> Option<(String, String, String)> {
    let open = path.find('{')?;
    let close = path.find('}')?;
    if close <= open + 1 {
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

    let after_brace = &path[close + 1..];
    let dot_pos = after_brace.find('.')?;
    let prop_name = &after_brace[dot_pos + 1..];
    if prop_name.is_empty() {
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

fn crate_value_is_array(value: &CrateValue) -> bool {
    match value {
        CrateValue::Array(_) => true,
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

fn f64_le(d: &[u8], idx: usize) -> f64 {
    let off = idx * 8;
    f64::from_le_bytes(d[off..off + 8].try_into().unwrap_or([0; 8]))
}

fn f32_le(d: &[u8], idx: usize) -> f32 {
    let off = idx * 4;
    f32::from_le_bytes(d[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn u16_le(d: &[u8], idx: usize) -> u16 {
    let off = idx * 2;
    u16::from_le_bytes(d[off..off + 2].try_into().unwrap_or([0; 2]))
}

fn i32_le(d: &[u8], idx: usize) -> i32 {
    let off = idx * 4;
    i32::from_le_bytes(d[off..off + 4].try_into().unwrap_or([0; 4]))
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
    #[cfg(feature = "experimental_sparse_array_edits")]
    use crate::section::CrateSections;
    #[cfg(feature = "experimental_sparse_array_edits")]
    use alloc::vec;

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

    #[cfg(feature = "experimental_sparse_array_edits")]
    #[test]
    fn converts_experimental_sparse_array_edit_dictionary() {
        struct NoopResolver;

        impl AssetResolver for NoopResolver {
            fn resolve(
                &mut self,
                _asset_path: &str,
                _anchor: Option<LayerId>,
                _tokens: &mut TokenInterner,
                _paths: &mut PathInterner,
            ) -> Result<ResolvedAsset, layerstack::AssetResolveError> {
                unreachable!("asset resolution is not used in this test")
            }

            fn resolved_path(&self, _id: LayerId) -> Option<&str> {
                None
            }
        }

        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let sections = CrateSections {
            tokens: Vec::new(),
            strings: Vec::new(),
            fields: Vec::new(),
            fieldsets: Vec::new(),
            paths: Vec::new(),
            specs: Vec::new(),
        };
        let mut resolver = NoopResolver;
        let mut ctx = AssembleCtx {
            data: &[],
            sections: &sections,
            tokens: &mut tokens,
            paths: &mut paths,
            resolver: &mut resolver,
            layer_id: LayerId(1),
            resolved_layers: Vec::new(),
        };

        let value = ctx.convert_crate_value(&CrateValue::Dictionary(vec![
            (
                String::from("__layerstack_sparse_array_edit__"),
                CrateValue::Bool(true),
            ),
            (
                String::from("ops"),
                CrateValue::Array(vec![
                    CrateValue::Dictionary(vec![
                        (
                            String::from("op"),
                            CrateValue::String(String::from("write")),
                        ),
                        (
                            String::from("src"),
                            CrateValue::Dictionary(vec![(
                                String::from("literal"),
                                CrateValue::Int(7),
                            )]),
                        ),
                        (String::from("index"), CrateValue::Int(0)),
                    ]),
                    CrateValue::Dictionary(vec![
                        (
                            String::from("op"),
                            CrateValue::String(String::from("append")),
                        ),
                        (
                            String::from("src"),
                            CrateValue::Dictionary(vec![(
                                String::from("literal"),
                                CrateValue::Int(9),
                            )]),
                        ),
                        (
                            String::from("index"),
                            CrateValue::String(String::from("end")),
                        ),
                    ]),
                ]),
            ),
        ]));

        match value {
            Value::ArrayEdit(edit) => {
                assert_eq!(edit.ops.len(), 2);
                assert!(matches!(
                    edit.ops[0],
                    ArrayEditOp::Write {
                        src: ArrayEditOperand::Literal(Value::Int(7)),
                        index: ArrayIndex::Position(0),
                    }
                ));
                assert!(matches!(
                    edit.ops[1],
                    ArrayEditOp::Insert {
                        src: ArrayEditOperand::Literal(Value::Int(9)),
                        index: ArrayIndex::End,
                    }
                ));
            }
            other => panic!("expected array edit, got {other:?}"),
        }
    }
}
