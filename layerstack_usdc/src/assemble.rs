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

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::HashMap;

use layerstack::doc::{
    FieldValue, Layer, LayerId, PrimSpec, Reference, Specifier, Value, VariantSetSpec, VariantSpec,
    set_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner};
use layerstack::{AssetResolver, ResolvedAsset};

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
                if let Some((prim_path, attr_name)) = split_property_path(&path_str)
                    && let Some(prim) = prim_specs_map.get_mut(&prim_path)
                {
                    self.apply_attribute_fields(&spec_fields[i], &attr_name, prim)?;
                }
            }
        }

        // Process Relationship specs — add fields to their parent prim.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Relationship {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Some((prim_path, rel_name)) = split_property_path(&path_str)
                    && let Some(prim) = prim_specs_map.get_mut(&prim_path)
                {
                    self.apply_relationship_fields(&spec_fields[i], &rel_name, prim)?;
                }
            }
        }

        // Process Connection specs — add connection paths to attribute on parent.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Connection {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Some((prim_path, attr_name)) = split_property_path(&path_str)
                    && let Some(prim) = prim_specs_map.get_mut(&prim_path)
                {
                    self.apply_connection_fields(&spec_fields[i], &attr_name, prim)?;
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
                                layer.sublayers.push(resolved.layer_id);
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

        // Look for time samples, default value, or connection paths.
        let mut has_time_samples = false;
        let mut has_default = false;

        for (field_name, value) in fields {
            match field_name.as_str() {
                "timeSamples" => {
                    if let CrateValue::TimeSamples(samples) = value {
                        let ts: Vec<(f64, Value)> = samples
                            .iter()
                            .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                            .collect();
                        set_field_vec(&mut prim.fields, name_tok, FieldValue::TimeSamples(ts));
                        has_time_samples = true;
                    }
                }
                "default" => {
                    if !has_time_samples {
                        let converted = self.convert_crate_value(value);
                        set_field_vec(&mut prim.fields, name_tok, FieldValue::Value(converted));
                        has_default = true;
                    }
                }
                "connectionPaths" => {
                    let listop = self.convert_connection_value(value)?;
                    set_field_vec(&mut prim.fields, name_tok, FieldValue::PathListOp(listop));
                }
                // Skip metadata fields (variability, custom, etc.).
                "variability" | "custom" | "typeName" => {}
                _ => {
                    // Other attribute metadata gets stored as a sub-field.
                    // For now we skip these; they're rarely needed.
                }
            }
        }

        // If the attribute was declared with no default and no time samples,
        // register as Null (attribute declaration).
        if !has_time_samples && !has_default {
            let has_connection = fields.iter().any(|(n, _)| n == "connectionPaths");
            if !has_connection {
                layerstack::insert_field_if_absent(
                    &mut prim.fields,
                    name_tok,
                    FieldValue::Value(Value::Null),
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
                        set_field_vec(&mut variant.fields, prop_tok, fv);
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
            CrateValue::Opaque { value_type, data } => {
                let type_name = self.tokens.intern(value_type_name(*value_type));
                Value::Opaque {
                    type_name,
                    bytes: Arc::from(data.as_slice()),
                }
            }
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
                        if let Ok(converted) = self.convert_path_listop(listop) {
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

    /// Converts a connection or target paths value to `ListOp<PathId>`.
    fn convert_connection_value(
        &mut self,
        value: &CrateValue,
    ) -> Result<ListOp<PathId>, UsdcError> {
        match value {
            CrateValue::ListOp(listop) => self.convert_path_listop(listop),
            CrateValue::PathVector(paths) => {
                let path_ids: Vec<PathId> = paths
                    .iter()
                    .filter_map(|s| {
                        Path::parse_absolute(s, self.tokens)
                            .ok()
                            .map(|p| self.paths.intern(p))
                    })
                    .collect();
                Ok(ListOp {
                    explicit: Some(path_ids),
                    ..ListOp::default()
                })
            }
            _ => Ok(ListOp::default()),
        }
    }

    /// Converts a USDC reference-encoded dictionary to a [`Reference`].
    fn convert_crate_to_reference(&mut self, cv: &CrateValue) -> Option<Reference> {
        if let CrateValue::Dictionary(entries) = cv {
            let mut asset_path = String::new();
            let mut prim_path = String::new();
            // layer_offset and layer_scale not yet used by Reference.

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
                    _ => {}
                }
            }

            // Resolve the asset path.
            if asset_path.is_empty() && prim_path.is_empty() {
                return None;
            }

            let (layer, prim_path_id) = if !asset_path.is_empty() {
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
                let pid = if !prim_path.is_empty() {
                    Path::parse_absolute(&prim_path, self.tokens)
                        .ok()
                        .map(|p| self.paths.intern(p))
                        .unwrap_or_else(|| self.paths.intern(Path::root()))
                } else {
                    self.paths.intern(Path::root())
                };
                (lid, pid)
            } else {
                // Internal reference (same layer).
                let pid = Path::parse_absolute(&prim_path, self.tokens)
                    .ok()
                    .map(|p| self.paths.intern(p))
                    .unwrap_or_else(|| self.paths.intern(Path::root()));
                (self.layer_id, pid)
            };

            let asset = if asset_path.is_empty() {
                None
            } else {
                Some(asset_path)
            };

            Some(Reference {
                layer,
                prim_path: prim_path_id,
                asset,
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

/// Splits a property path like `/Prim.attrName` into `("/Prim", "attrName")`.
///
/// Returns `None` if the path doesn't contain a property separator.
fn split_property_path(path: &str) -> Option<(String, String)> {
    // Find the last `.` that is not inside variant braces.
    let brace_depth = path.chars().fold(0_i32, |depth, c| match c {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    });
    // Simple case: no braces, just find the last `.`.
    if brace_depth == 0 {
        if let Some(dot_pos) = path.rfind('.') {
            let prim_path = &path[..dot_pos];
            let prop_name = &path[dot_pos + 1..];
            if !prim_path.is_empty() && !prop_name.is_empty() {
                return Some((String::from(prim_path), String::from(prop_name)));
            }
        }
    } else {
        // Path contains braces (variant path). Find the `.` after the last `}`.
        if let Some(last_brace) = path.rfind('}') {
            let after = &path[last_brace + 1..];
            if let Some(dot_pos) = after.find('.') {
                let split_pos = last_brace + 1 + dot_pos;
                let prim_path = &path[..split_pos];
                let prop_name = &path[split_pos + 1..];
                if !prim_path.is_empty() && !prop_name.is_empty() {
                    return Some((String::from(prim_path), String::from(prop_name)));
                }
            }
        }
    }
    None
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

/// Returns a human-readable type name for a USDC [`ValueType`].
fn value_type_name(vtype: ValueType) -> &'static str {
    match vtype {
        ValueType::Vec2d => "GfVec2d",
        ValueType::Vec2f => "GfVec2f",
        ValueType::Vec2h => "GfVec2h",
        ValueType::Vec2i => "GfVec2i",
        ValueType::Vec3d => "GfVec3d",
        ValueType::Vec3f => "GfVec3f",
        ValueType::Vec3h => "GfVec3h",
        ValueType::Vec3i => "GfVec3i",
        ValueType::Vec4d => "GfVec4d",
        ValueType::Vec4f => "GfVec4f",
        ValueType::Vec4h => "GfVec4h",
        ValueType::Vec4i => "GfVec4i",
        ValueType::Quatd => "GfQuatd",
        ValueType::Quatf => "GfQuatf",
        ValueType::Quath => "GfQuath",
        ValueType::Matrix2d => "GfMatrix2d",
        ValueType::Matrix3d => "GfMatrix3d",
        ValueType::Matrix4d => "GfMatrix4d",
        _ => "unknown",
    }
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

    #[test]
    fn split_property_simple() {
        let (prim, prop) = split_property_path("/Cube.size").unwrap();
        assert_eq!(prim, "/Cube");
        assert_eq!(prop, "size");
    }

    #[test]
    fn split_property_nested() {
        let (prim, prop) = split_property_path("/World/Cube.visibility").unwrap();
        assert_eq!(prim, "/World/Cube");
        assert_eq!(prop, "visibility");
    }

    #[test]
    fn split_property_none() {
        assert!(split_property_path("/Cube").is_none());
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
}
