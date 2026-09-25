// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Emits layerstack [`Layer`] / [`PrimSpec`] from a USDA AST.
//!
//! This module converts the typed AST (produced by [`crate::lower`]) into
//! the layerstack document model for composition. It is the final stage of
//! the USDA pipeline: `source → lexer → CST → AST → emit`.
//!
//! Asset path resolution (sublayer includes, references, payloads) is
//! delegated to the caller via the [`AssetResolver`] trait, keeping this
//! module `no_std` compatible.
//!
//! [`Layer`]: layerstack::Layer
//! [`PrimSpec`]: layerstack::PrimSpec

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::doc::{
    FieldEntry, FieldValue, Layer, LayerId, LayerOffset, PrimSpec, Reference, Specifier,
    SublayerEntry, Value, VariantSpec, get_field_mut, set_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner, TargetPath};
use layerstack::property::{
    PropertyEntry, PropertyKind, PropertySpec, Variability, property_entry,
};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, AssetResolver, PropertyType,
    ReferenceTarget, ResolvedAsset,
};

use crate::ast;
use crate::diagnostic::Diagnostic;

/// Result of emitting a single USDA layer.
#[derive(Debug)]
pub struct EmitResult {
    /// The emitted layer.
    pub layer: Layer,
    /// Any layers produced by resolving asset paths (sublayers, references,
    /// payloads). The caller should insert these into their store.
    pub resolved_layers: Vec<Layer>,
    /// Diagnostics from the emit pass.
    pub diagnostics: Vec<Diagnostic>,
}

/// Converts a parsed AST layer into a layerstack [`Layer`].
///
/// The caller provides:
/// - `ast`: the parsed AST layer
/// - `layer_id`: the [`LayerId`] to assign to the emitted layer
/// - `tokens`: shared token interner (mutated to intern new tokens)
/// - `paths`: shared path interner (mutated to intern new paths)
/// - `resolver`: an [`AssetResolver`] for resolving sublayer/reference/payload
///   asset paths into [`LayerId`]s
///
/// Returns an [`EmitResult`] with the emitted layer, any newly resolved layers,
/// and diagnostics.
pub fn emit(
    ast: &ast::Layer<'_>,
    layer_id: LayerId,
    tokens: &mut TokenInterner,
    paths: &mut PathInterner,
    resolver: &mut dyn AssetResolver,
) -> EmitResult {
    let mut ctx = EmitCtx {
        tokens,
        paths,
        resolver,
        layer_id,
        resolved_layers: Vec::new(),
        diagnostics: Vec::new(),
    };
    let layer = ctx.emit_layer(ast);
    EmitResult {
        layer,
        resolved_layers: ctx.resolved_layers,
        diagnostics: ctx.diagnostics,
    }
}

// ── Internal context ────────────────────────────────────────────────────

struct EmitCtx<'a> {
    tokens: &'a mut TokenInterner,
    paths: &'a mut PathInterner,
    resolver: &'a mut dyn AssetResolver,
    layer_id: LayerId,
    resolved_layers: Vec<Layer>,
    diagnostics: Vec<Diagnostic>,
}

impl EmitCtx<'_> {
    // ── Layer ────────────────────────────────────────────────────────

    fn emit_layer(&mut self, ast: &ast::Layer<'_>) -> Layer {
        let mut layer = Layer::new(self.layer_id);

        // Process layer metadata.
        for meta in &ast.metadata {
            match meta {
                ast::LayerMeta::SubLayers(items) => {
                    for item in items {
                        if let Some(resolved) = self.resolve_asset(item.asset) {
                            let offset = LayerOffset {
                                offset: item.offset.unwrap_or(0.0),
                                scale: item.scale.unwrap_or(1.0),
                            };
                            layer.sublayers.push(SublayerEntry {
                                layer: resolved.layer_id,
                                offset,
                            });
                            if let Some(sub_layer) = resolved.layer {
                                self.resolved_layers.push(sub_layer);
                            }
                        }
                    }
                }
                ast::LayerMeta::Relocates(entries) => {
                    // Relocates (AOUSD Core §10, relocates arcs) are not
                    // modelled: the layer has nowhere to store them and
                    // composition does not apply them. Report each entry
                    // instead of dropping it silently, since the composed
                    // namespace will differ from the authored intent.
                    for entry in entries {
                        self.diagnostics.push(Diagnostic::error(
                            entry.span,
                            format!(
                                "unsupported: relocates `<{}>: <{}>` is ignored; \
                                 layerstack does not compose relocates",
                                entry.source, entry.target
                            ),
                        ));
                    }
                }
                ast::LayerMeta::Doc(doc) => {
                    // Spec: AOUSD Core §7.6.1.5.1 (`documentation`).
                    let key = self.tokens.intern("documentation");
                    set_field_vec(
                        &mut layer.metadata,
                        key,
                        FieldValue::Value(Value::String(Arc::from(*doc))),
                    );
                }
                ast::LayerMeta::Custom(entry) if entry.key == "defaultPrim" => {
                    // Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim: token`),
                    // authored as a quoted string in USDA layer metadata.
                    let name = match &entry.value {
                        ast::MetadataValue::Value(ast::Value::String(name)) => Some(*name),
                        ast::MetadataValue::String(name) => Some(name.as_str()),
                        _ => None,
                    };
                    if let Some(name) = name {
                        layer.default_prim = Some(self.tokens.intern(name));
                    }
                }
                ast::LayerMeta::Custom(entry) => {
                    // Spec: AOUSD Core §7.6.1 (layer spec fields), §12.2.7.
                    self.emit_metadata_entry(entry, &mut layer.metadata);
                }
            }
        }

        // Process root prims.
        let root_path = Path::root();
        let root_path_id = self.paths.intern(root_path);

        // Collect root children for a pseudo root prim if needed.
        let mut root_children = Vec::new();
        for prim in &ast.prims {
            let name_tok = self.tokens.intern(prim.name);
            root_children.push(name_tok);
            self.emit_prim(prim, &format!("/{}", prim.name), &[], &mut layer);
        }

        // If there are root prims, create a root prim spec to hold children
        // and any root ordering.
        if !root_children.is_empty() || ast.root_prim_order.is_some() {
            let root_spec = PrimSpec {
                authored_children: root_children,
                prim_order: ast
                    .root_prim_order
                    .as_ref()
                    .map(|order| order.iter().map(|n| self.tokens.intern(n)).collect()),
                ..PrimSpec::default()
            };
            layer.insert_prim(root_path_id, root_spec);
        }

        layer
    }

    // ── Prims ────────────────────────────────────────────────────────

    fn emit_prim(
        &mut self,
        prim: &ast::Prim<'_>,
        prim_path: &str,
        outer_variant_sites: &[VariantSelectionSite],
        layer: &mut Layer,
    ) {
        let path = Path::parse_absolute(prim_path, self.tokens).expect("valid prim path");
        let path_id = self.paths.intern(path);

        let mut spec = PrimSpec {
            specifier: Some(convert_specifier(prim.specifier)),
            type_name: prim.type_name.map(|t| self.tokens.intern(t)),
            outer_variant_sites: outer_variant_sites.to_vec(),
            ..PrimSpec::default()
        };

        // Process prim metadata.
        self.emit_prim_metadata(&prim.metadata, prim_path, &mut spec);

        // Process prim body children.
        for child in &prim.children {
            match child {
                ast::PrimChild::Attribute(attr) => {
                    self.emit_attribute(attr, &mut spec.properties, prim_path);
                }
                ast::PrimChild::Relationship(rel) => {
                    self.emit_relationship(rel, &mut spec.properties, prim_path);
                }
                ast::PrimChild::Prim(child_prim) => {
                    let child_name = self.tokens.intern(child_prim.name);
                    if !spec.authored_children.contains(&child_name) {
                        spec.authored_children.push(child_name);
                    }
                    let child_path = format!("{}/{}", prim_path, child_prim.name);
                    self.emit_prim(child_prim, &child_path, outer_variant_sites, layer);
                }
                ast::PrimChild::VariantSet(vs) => {
                    self.emit_variant_set(vs, path_id, prim_path, &mut spec, layer);
                }
                ast::PrimChild::ReorderNameChildren(names) => {
                    spec.prim_order = Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                }
                ast::PrimChild::ReorderProperties(names) => {
                    // Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`); the
                    // `reorder properties` statement authors `propertyOrder`.
                    spec.property_order =
                        Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                }
            }
        }

        layer.insert_prim(path_id, spec);
    }

    fn variant_site(
        &self,
        host_path: PathId,
        set: TokenId,
        variant: TokenId,
    ) -> VariantSelectionSite {
        VariantSelectionSite {
            host_path,
            set,
            variant,
        }
    }

    // ── Prim metadata ───────────────────────────────────────────────

    fn emit_prim_metadata(
        &mut self,
        metadata: &[ast::PrimMeta<'_>],
        prim_path: &str,
        spec: &mut PrimSpec,
    ) {
        for meta in metadata {
            match meta {
                ast::PrimMeta::References(arc) => {
                    merge_ref_listop(&mut spec.references, self.emit_arc_listop(arc, prim_path));
                }
                ast::PrimMeta::Payload(arc) => {
                    merge_ref_listop(&mut spec.payloads, self.emit_arc_listop(arc, prim_path));
                }
                ast::PrimMeta::Inherits(paths) => {
                    merge_path_listop(&mut spec.inherits, self.emit_path_listop(paths, prim_path));
                }
                ast::PrimMeta::Specializes(paths) => {
                    merge_path_listop(
                        &mut spec.specializes,
                        self.emit_path_listop(paths, prim_path),
                    );
                }
                ast::PrimMeta::Variants(selections) => {
                    for sel in selections {
                        let set_tok = self.tokens.intern(sel.set_name);
                        let branch_tok = self.tokens.intern(sel.branch_name);
                        spec.variant_selections.insert(set_tok, branch_tok);
                    }
                }
                ast::PrimMeta::VariantSets(listop) => {
                    // variantSets metadata declares the ordered set of variant
                    // set names. We store the union of all names in order.
                    if let Some(items) = &listop.items {
                        for name in items {
                            let tok = self.tokens.intern(name);
                            if !spec.variant_set_order.contains(&tok) {
                                spec.variant_set_order.push(tok);
                            }
                        }
                    }
                }
                ast::PrimMeta::Custom(entry) if entry.key == "instanceable" => {
                    if let ast::MetadataValue::Value(ast::Value::Bool(b)) = &entry.value {
                        spec.instanceable = Some(*b);
                    } else {
                        self.emit_metadata_entry(entry, &mut spec.fields);
                    }
                }
                ast::PrimMeta::Custom(entry) if entry.key == "active" => {
                    if let ast::MetadataValue::Value(ast::Value::Bool(b)) = &entry.value {
                        spec.active = Some(*b);
                    } else {
                        self.emit_metadata_entry(entry, &mut spec.fields);
                    }
                }
                ast::PrimMeta::Kind(_) | ast::PrimMeta::Doc(_) | ast::PrimMeta::Custom(_) => {
                    self.emit_plain_prim_meta(meta, &mut spec.fields);
                }
            }
        }
    }

    /// Emits a prim metadata entry that has no dedicated [`PrimSpec`]
    /// member (`kind`, `doc`, `comment`, `apiSchemas`, `customData`,
    /// `hidden`, …) into `fields`.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    fn emit_plain_prim_meta(&mut self, meta: &ast::PrimMeta<'_>, fields: &mut Vec<FieldEntry>) {
        match meta {
            ast::PrimMeta::Kind(kind) => {
                // Spec: AOUSD Core §7.6.2.4.4 (`kind: token`).
                let key = self.tokens.intern("kind");
                let val = self.tokens.intern(kind);
                set_field_vec(fields, key, FieldValue::Value(Value::Token(val)));
            }
            ast::PrimMeta::Doc(doc) => {
                // Spec: AOUSD Core §7.6.2.5.4 (`documentation`).
                let key = self.tokens.intern("documentation");
                set_field_vec(
                    fields,
                    key,
                    FieldValue::Value(Value::String(Arc::from(*doc))),
                );
            }
            ast::PrimMeta::Custom(entry) => self.emit_metadata_entry(entry, fields),
            ast::PrimMeta::References(_)
            | ast::PrimMeta::Inherits(_)
            | ast::PrimMeta::Specializes(_)
            | ast::PrimMeta::Payload(_)
            | ast::PrimMeta::Variants(_)
            | ast::PrimMeta::VariantSets(_) => {}
        }
    }

    /// Emits one generic metadata entry into `fields`.
    ///
    /// Values are converted with the field's registered type where it is
    /// known (see [`metadata_field_type`]), so `interpolation = "vertex"`
    /// becomes a token and `metersPerUnit = 1` a double, as OpenUSD's text
    /// parser does with its schema-registered field types. Repeated list-op
    /// statements for one field combine into a single list op.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields), §12.2.6 (list ops).
    fn emit_metadata_entry(
        &mut self,
        entry: &ast::MetadataEntry<'_>,
        fields: &mut Vec<FieldEntry>,
    ) {
        let key_name = match entry.key {
            // `doc` is the USDA spelling of the `documentation` field.
            "doc" => "documentation",
            key => key,
        };
        let key = self.tokens.intern(key_name);
        let field_type = metadata_field_type(key_name);
        let element = match field_type {
            MetadataFieldType::ListOp(element) => Some(element),
            _ if entry.op != ast::ListOpKind::Explicit => {
                // An unregistered list-op field: integers make an `int64`
                // list op, anything else a token list op.
                Some(if self.metadata_items_are_integers(&entry.value) {
                    ListElement::Int64
                } else {
                    ListElement::Token
                })
            }
            _ => None,
        };
        if let Some(element) = element {
            let Some(value) = self.metadata_list_op(entry, element) else {
                self.diagnostics.push(Diagnostic::error(
                    entry.span,
                    format!("`{key_name}` list op items do not match its element type; ignored"),
                ));
                return;
            };
            match get_field_mut(fields, &key) {
                Some(existing) => {
                    if !merge_field_list_ops(existing, value) {
                        self.diagnostics.push(Diagnostic::error(
                            entry.span,
                            format!("`{key_name}` list op statements disagree on type; ignored"),
                        ));
                    }
                }
                None => set_field_vec(fields, key, value),
            }
            return;
        }
        let value = self.convert_metadata_value(&entry.value, field_type.type_hint());
        set_field_vec(fields, key, FieldValue::Value(value));
    }

    /// Returns `true` when a metadata value is an array of integers.
    fn metadata_items_are_integers(&self, value: &ast::MetadataValue<'_>) -> bool {
        matches!(value, ast::MetadataValue::Value(ast::Value::Array(items))
            if !items.is_empty() && items.iter().all(|v| matches!(v, ast::Value::Int(_))))
    }

    /// Converts one list-op metadata statement, or returns `None` when an
    /// item does not fit `element`.
    ///
    /// Spec: AOUSD Core §12.2.6 (list ops).
    fn metadata_list_op(
        &mut self,
        entry: &ast::MetadataEntry<'_>,
        element: ListElement,
    ) -> Option<FieldValue> {
        let items: &[ast::Value<'_>] = match &entry.value {
            ast::MetadataValue::Value(ast::Value::Array(items)) => items,
            ast::MetadataValue::Value(single) => core::slice::from_ref(single),
            // `= None` clears the list.
            _ => &[],
        };
        fn list<T>(op: ast::ListOpKind, items: Vec<T>) -> ListOp<T> {
            let mut list = ListOp::default();
            match op {
                ast::ListOpKind::Explicit => list.explicit = Some(items),
                ast::ListOpKind::Prepend => list.prepend = items,
                ast::ListOpKind::Append => list.append = items,
                ast::ListOpKind::Delete => list.delete = items,
            }
            list
        }
        let int = |v: &ast::Value<'_>| match v {
            ast::Value::Int(n) => Some(*n),
            _ => None,
        };
        let op = entry.op;
        Some(match element {
            ListElement::Token => {
                let mut tokens = Vec::with_capacity(items.len());
                for item in items {
                    match item {
                        ast::Value::String(s) | ast::Value::Identifier(s) => {
                            tokens.push(self.tokens.intern(s));
                        }
                        _ => return None,
                    }
                }
                FieldValue::TokenListOp(list(op, tokens))
            }
            ListElement::String => {
                let strings = items
                    .iter()
                    .map(|v| match v {
                        ast::Value::String(s) => Some(Arc::from(*s)),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>()?;
                FieldValue::StringListOp(list(op, strings))
            }
            ListElement::Int64 => FieldValue::Int64ListOp(list(
                op,
                items.iter().map(int).collect::<Option<Vec<_>>>()?,
            )),
        })
    }

    // ── Properties ──────────────────────────────────────────────────

    /// Merges one attribute statement into the property list.
    ///
    /// USDA may split one attribute spec over several statements
    /// (`float a = 1`, `float a.timeSamples = {...}`, `float a.connect = ...`).
    /// Each statement fills its own slot of the same [`PropertySpec`]; no slot
    /// replaces another.
    ///
    /// Spec: AOUSD Core §7.6.4 (attribute spec fields), §16.2 (USDA grammar).
    fn emit_attribute(
        &mut self,
        attr: &ast::Attribute<'_>,
        properties: &mut Vec<PropertyEntry>,
        anchor: &str,
    ) {
        let name_tok = self.tokens.intern(attr.name);
        let property_type = self.declared_property_type(attr.type_name, attr.is_array);
        let connection = attr
            .connection
            .as_ref()
            .map(|conn| self.emit_connection_listop(conn, anchor));
        let time_samples = attr
            .time_samples
            .as_ref()
            .map(|samples| self.convert_time_samples(samples, attr.type_name));
        let default = attr
            .default
            .as_ref()
            .map(|value| self.convert_value(value, attr.type_name));
        let mut metadata = Vec::new();
        for entry in &attr.metadata {
            self.emit_metadata_entry(entry, &mut metadata);
        }

        let Some(spec) = self.property_of_kind(
            properties,
            name_tok,
            attr.name,
            attr.span,
            PropertyKind::Attribute,
        ) else {
            return;
        };
        spec.type_name = Some(property_type);
        // Qualifiers restated by any statement of the attribute hold for the
        // whole spec (Core §7.6.3.1.1 `custom`, §7.6.4.1.2 `variability`).
        spec.custom |= attr.custom;
        if attr.uniform {
            spec.variability = Variability::Uniform;
        }
        for entry in metadata {
            set_field_vec(&mut spec.metadata, entry.name, entry.value);
        }
        if let Some(listop) = connection {
            match spec.targets.as_mut() {
                Some(existing) => merge_path_listop(existing, listop),
                None => spec.targets = Some(listop),
            }
        }
        if let Some(samples) = time_samples {
            *spec = core::mem::take(spec).with_time_samples(samples);
        }
        if let Some(value) = default {
            spec.default = Some(value);
        }
    }

    /// Merges one relationship statement into the property list.
    ///
    /// Spec: AOUSD Core §7.6.5 (relationship spec fields).
    fn emit_relationship(
        &mut self,
        rel: &ast::Relationship<'_>,
        properties: &mut Vec<PropertyEntry>,
        anchor: &str,
    ) {
        let name_tok = self.tokens.intern(rel.name);
        let listop = rel.targets.as_ref().map(|targets| {
            let target_paths = self.target_paths(targets, anchor, rel.span);
            let mut listop = ListOp::default();
            match rel.op {
                ast::ListOpKind::Explicit => listop.explicit = Some(target_paths),
                ast::ListOpKind::Prepend => listop.prepend = target_paths,
                ast::ListOpKind::Append => listop.append = target_paths,
                ast::ListOpKind::Delete => listop.delete = target_paths,
            }
            listop
        });

        let mut metadata = Vec::new();
        for entry in &rel.metadata {
            self.emit_metadata_entry(entry, &mut metadata);
        }

        let Some(spec) = self.property_of_kind(
            properties,
            name_tok,
            rel.name,
            rel.span,
            PropertyKind::Relationship,
        ) else {
            return;
        };
        spec.custom |= rel.custom;
        for entry in metadata {
            set_field_vec(&mut spec.metadata, entry.name, entry.value);
        }
        if let Some(listop) = listop {
            match spec.targets.as_mut() {
                Some(existing) => merge_path_listop(existing, listop),
                None => spec.targets = Some(listop),
            }
        }
    }

    /// Returns the property `name` of `kind`, creating it when absent.
    ///
    /// Attributes and relationships of one prim share a name space (Core
    /// §7.3.3), so a statement of the other kind is reported and ignored
    /// instead of silently merging two properties.
    fn property_of_kind<'p>(
        &mut self,
        properties: &'p mut Vec<PropertyEntry>,
        name_tok: TokenId,
        name: &str,
        span: crate::Span,
        kind: PropertyKind,
    ) -> Option<&'p mut PropertySpec> {
        let spec = property_entry(properties, name_tok, kind);
        if spec.kind == kind {
            return Some(spec);
        }
        let (authored, ignored) = match kind {
            PropertyKind::Attribute => ("relationship", "attribute"),
            PropertyKind::Relationship => ("attribute", "relationship"),
        };
        self.diagnostics.push(Diagnostic::error(
            span,
            format!(
                "`{name}` is already authored as a {authored} on this prim; \
                 the {ignored} statement is ignored"
            ),
        ));
        None
    }

    // ── Variant sets ────────────────────────────────────────────────

    fn emit_variant_set(
        &mut self,
        vs: &ast::VariantSet<'_>,
        prim_path_id: PathId,
        prim_path: &str,
        spec: &mut PrimSpec,
        layer: &mut Layer,
    ) {
        let set_tok = self.tokens.intern(vs.name);

        // Add to variant_set_order if not already there.
        if !spec.variant_set_order.contains(&set_tok) {
            spec.variant_set_order.push(set_tok);
        }

        // Remove-then-reinsert: if a nested variant set with the same name
        // was built during recursive processing of inner branches, we merge
        // into it rather than overwriting.
        let mut set_spec = spec.variant_sets.remove(&set_tok).unwrap_or_default();

        for branch in &vs.branches {
            let branch_tok = self.tokens.intern(branch.name);
            let mut variant_spec = set_spec.variants.remove(&branch_tok).unwrap_or_default();
            if variant_spec.outer_variant_sites.is_empty() {
                variant_spec.outer_variant_sites = spec.outer_variant_sites.clone();
            }
            let branch_site = self.variant_site(prim_path_id, set_tok, branch_tok);
            let mut branch_context = spec.outer_variant_sites.clone();
            branch_context.push(branch_site);

            // Process branch metadata (arcs on the branch itself).
            self.emit_variant_branch_metadata(&branch.metadata, prim_path, &mut variant_spec);

            // Process branch children.
            for child in &branch.children {
                match child {
                    ast::PrimChild::Attribute(attr) => {
                        self.emit_attribute(attr, &mut variant_spec.properties, prim_path);
                    }
                    ast::PrimChild::Relationship(rel) => {
                        self.emit_relationship(rel, &mut variant_spec.properties, prim_path);
                    }
                    ast::PrimChild::Prim(child_prim) => {
                        let child_tok = self.tokens.intern(child_prim.name);
                        if !variant_spec.authored_children.contains(&child_tok) {
                            variant_spec.authored_children.push(child_tok);
                        }

                        // Check if this child prim also exists as a non-variant
                        // prim in the layer. If so, route arcs and fields to the
                        // variant spec's child_* maps.
                        let child_path = format!("{}/{}", prim_path, child_prim.name);
                        let child_path_parsed =
                            Path::parse_absolute(&child_path, self.tokens).expect("valid path");
                        let child_path_id = self.paths.intern(child_path_parsed);

                        // A child already authored outside any variant branch
                        // gets this branch's opinions through the child maps;
                        // otherwise the branch authors its own prim spec,
                        // which `Layer::insert_prim` keeps apart from other
                        // branches' specs at the same path.
                        if layer.prim_spec_in(child_path_id, &[]).is_some() {
                            // Route to variant child maps.
                            self.emit_variant_child_prim(
                                child_prim,
                                &child_path,
                                child_tok,
                                &branch_context,
                                &mut variant_spec,
                                layer,
                            );
                        } else {
                            // New child introduced by variant — create full PrimSpec.
                            self.emit_prim(child_prim, &child_path, &branch_context, layer);

                            // Also route arcs to variant spec for proper
                            // variant-selection-aware resolution.
                            self.emit_variant_child_arcs(
                                child_prim,
                                &child_path,
                                child_tok,
                                &mut variant_spec,
                            );

                            // Route grandchild names and fields to the
                            // variant spec so composition can find them
                            // under variant selection.
                            for gc in &child_prim.children {
                                if let ast::PrimChild::Prim(grandchild) = gc {
                                    let gc_tok = self.tokens.intern(grandchild.name);
                                    let gc_list = variant_spec
                                        .child_authored_children
                                        .entry(child_tok)
                                        .or_default();
                                    if !gc_list.contains(&gc_tok) {
                                        gc_list.push(gc_tok);
                                    }
                                }
                            }
                        }
                    }
                    ast::PrimChild::VariantSet(nested_vs) => {
                        // Process nested variant sets within variant branches.
                        // These variant sets belong to the same owning prim;
                        // children introduced by nested branches are gated by
                        // required_outer_variant_sites.
                        //
                        // Spec: AOUSD Core §10.5 (variant nesting).
                        self.emit_nested_variant_set(
                            nested_vs,
                            prim_path_id,
                            prim_path,
                            spec,
                            layer,
                            &branch_context,
                        );
                    }
                    ast::PrimChild::ReorderProperties(names) => {
                        variant_spec.property_order =
                            Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                    }
                    ast::PrimChild::ReorderNameChildren(_) => {}
                }
            }

            set_spec.variants.insert(branch_tok, variant_spec);
        }

        // Merge with any variant set spec that recursive nested calls may have
        // inserted under the same name during branch processing. This happens
        // when a nested variant branch contains a variant set with the same
        // name as the outer one (e.g., `standin → shadingVariant → standin`).
        if let Some(recursed) = spec.variant_sets.remove(&set_tok) {
            for (name, nested_variant) in recursed.variants {
                set_spec
                    .variants
                    .entry(name)
                    .or_default()
                    .merge(nested_variant);
            }
        }
        spec.variant_sets.insert(set_tok, set_spec);
    }

    /// Recursively emit a variant set nested inside a variant branch.
    ///
    /// Nested variant sets are syntactically defined inside an outer variant
    /// branch but semantically belong to the same owning prim. Children
    /// introduced by nested branches are registered with
    /// [`VariantSpec::required_outer_variant_sites`] so that
    /// `filter_variant_children` can gate them on the
    /// correct combination of outer variant selections.
    ///
    /// Spec: AOUSD Core §10.5 (variant nesting).
    fn emit_nested_variant_set(
        &mut self,
        vs: &ast::VariantSet<'_>,
        prim_path_id: PathId,
        prim_path: &str,
        spec: &mut PrimSpec,
        layer: &mut Layer,
        outer_context: &[VariantSelectionSite],
    ) {
        let nested_set_tok = self.tokens.intern(vs.name);

        // Add to variant_set_order if not already present.
        if !spec.variant_set_order.contains(&nested_set_tok) {
            spec.variant_set_order.push(nested_set_tok);
        }

        // Build the nested variant set spec in a local to avoid double
        // mutable borrows of `spec` when recursing.
        let mut local_set_spec = spec
            .variant_sets
            .remove(&nested_set_tok)
            .unwrap_or_default();

        for branch in &vs.branches {
            let nested_branch_tok = self.tokens.intern(branch.name);
            let variant_spec = local_set_spec
                .variants
                .entry(nested_branch_tok)
                .or_default();
            if variant_spec.outer_variant_sites.is_empty() {
                variant_spec.outer_variant_sites = outer_context.to_vec();
            }
            let branch_site = self.variant_site(prim_path_id, nested_set_tok, nested_branch_tok);
            let mut branch_context = outer_context.to_vec();
            branch_context.push(branch_site);

            // Process branch metadata.
            self.emit_variant_branch_metadata(&branch.metadata, prim_path, variant_spec);

            // Collect deeper nested variant sets to process after this
            // branch's variant_spec borrow is released.
            let mut deeper_variant_sets: Vec<(usize, Vec<VariantSelectionSite>)> = Vec::new();

            for (child_idx, child) in branch.children.iter().enumerate() {
                match child {
                    ast::PrimChild::Prim(child_prim) => {
                        let child_tok = self.tokens.intern(child_prim.name);

                        // Register child in the nested variant spec.
                        if !variant_spec.authored_children.contains(&child_tok) {
                            variant_spec.authored_children.push(child_tok);
                        }

                        // Record required outer selections for this child.
                        variant_spec
                            .required_outer_variant_sites
                            .entry(child_tok)
                            .or_insert_with(|| outer_context.to_vec());

                        // Emit the child prim as a full PrimSpec.
                        let child_path = format!("{}/{}", prim_path, child_prim.name);
                        // `Layer::insert_prim` keeps each branch's spec.
                        self.emit_prim(child_prim, &child_path, &branch_context, layer);

                        // Route composition arcs to the nested variant spec.
                        self.emit_variant_child_arcs(
                            child_prim,
                            &child_path,
                            child_tok,
                            variant_spec,
                        );

                        // Route grandchild names.
                        for gc in &child_prim.children {
                            if let ast::PrimChild::Prim(grandchild) = gc {
                                let gc_tok = self.tokens.intern(grandchild.name);
                                let gc_list = variant_spec
                                    .child_authored_children
                                    .entry(child_tok)
                                    .or_default();
                                if !gc_list.contains(&gc_tok) {
                                    gc_list.push(gc_tok);
                                }
                            }
                        }
                    }
                    ast::PrimChild::VariantSet(_) => {
                        // Defer recursive processing until after variant_spec
                        // borrow is released.
                        let mut deeper_ctx = outer_context.to_vec();
                        deeper_ctx.push(branch_site);
                        deeper_variant_sets.push((child_idx, deeper_ctx));
                    }
                    ast::PrimChild::Attribute(attr) => {
                        self.emit_attribute(attr, &mut variant_spec.properties, prim_path);
                    }
                    ast::PrimChild::Relationship(rel) => {
                        self.emit_relationship(rel, &mut variant_spec.properties, prim_path);
                    }
                    _ => {}
                }
            }

            // Now process deferred deeper nested variant sets.
            // Re-insert local_set_spec so recursive calls can access it.
            if !deeper_variant_sets.is_empty() {
                spec.variant_sets.insert(nested_set_tok, local_set_spec);
                for (child_idx, deeper_ctx) in deeper_variant_sets {
                    if let ast::PrimChild::VariantSet(deeper_vs) = &branch.children[child_idx] {
                        self.emit_nested_variant_set(
                            deeper_vs,
                            prim_path_id,
                            prim_path,
                            spec,
                            layer,
                            &deeper_ctx,
                        );
                    }
                }
                // Re-extract after recursion.
                local_set_spec = spec
                    .variant_sets
                    .remove(&nested_set_tok)
                    .unwrap_or_default();
            }
        }

        // Re-insert the completed set spec.
        spec.variant_sets.insert(nested_set_tok, local_set_spec);
    }

    /// Emit metadata arcs on a variant branch header into a [`VariantSpec`].
    fn emit_variant_branch_metadata(
        &mut self,
        metadata: &[ast::PrimMeta<'_>],
        prim_path: &str,
        variant_spec: &mut VariantSpec,
    ) {
        for meta in metadata {
            match meta {
                ast::PrimMeta::References(arc) => {
                    merge_ref_listop(
                        &mut variant_spec.references,
                        self.emit_arc_listop(arc, prim_path),
                    );
                }
                ast::PrimMeta::Payload(arc) => {
                    merge_ref_listop(
                        &mut variant_spec.payloads,
                        self.emit_arc_listop(arc, prim_path),
                    );
                }
                ast::PrimMeta::Inherits(paths) => {
                    merge_path_listop(
                        &mut variant_spec.inherits,
                        self.emit_path_listop(paths, prim_path),
                    );
                }
                ast::PrimMeta::Specializes(paths) => {
                    merge_path_listop(
                        &mut variant_spec.specializes,
                        self.emit_path_listop(paths, prim_path),
                    );
                }
                ast::PrimMeta::Variants(selections) => {
                    for sel in selections {
                        let set_tok = self.tokens.intern(sel.set_name);
                        let branch_tok = self.tokens.intern(sel.branch_name);
                        variant_spec.variant_selections.insert(set_tok, branch_tok);
                    }
                }
                ast::PrimMeta::Kind(_) | ast::PrimMeta::Doc(_) | ast::PrimMeta::Custom(_) => {
                    // Spec: AOUSD Core §7.6.7 (variant specs contribute prim
                    // spec fields).
                    self.emit_plain_prim_meta(meta, &mut variant_spec.fields);
                }
                ast::PrimMeta::VariantSets(_) => {}
            }
        }
    }

    /// Route a child prim's arcs and fields to the parent variant spec's
    /// child_* maps (when the child already has a non-variant definition).
    fn emit_variant_child_prim(
        &mut self,
        child_prim: &ast::Prim<'_>,
        child_path: &str,
        child_tok: TokenId,
        outer_variant_sites: &[VariantSelectionSite],
        variant_spec: &mut VariantSpec,
        layer: &mut Layer,
    ) {
        // Route composition arcs to variant child maps.
        self.emit_variant_child_arcs(child_prim, child_path, child_tok, variant_spec);

        // Record the child's presence in this branch even when it authors no
        // fields: composition adds the branch as a source of the child.
        let mut child_fields = variant_spec
            .child_fields
            .remove(&child_tok)
            .unwrap_or_default();
        for meta in &child_prim.metadata {
            self.emit_plain_prim_meta(meta, &mut child_fields);
        }
        variant_spec.child_fields.insert(child_tok, child_fields);

        // Route properties.
        for child_child in &child_prim.children {
            match child_child {
                ast::PrimChild::Attribute(attr) => {
                    let properties = variant_spec.child_properties.entry(child_tok).or_default();
                    self.emit_attribute(attr, properties, child_path);
                }
                ast::PrimChild::Relationship(rel) => {
                    let properties = variant_spec.child_properties.entry(child_tok).or_default();
                    self.emit_relationship(rel, properties, child_path);
                }
                ast::PrimChild::Prim(grandchild) => {
                    // Grandchild prims: record in child_authored_children.
                    let gc_tok = self.tokens.intern(grandchild.name);
                    let gc_list = variant_spec
                        .child_authored_children
                        .entry(child_tok)
                        .or_default();
                    if !gc_list.contains(&gc_tok) {
                        gc_list.push(gc_tok);
                    }
                    // Also emit the grandchild as a full prim.
                    let gc_path = format!("{}/{}", child_path, grandchild.name);
                    self.emit_prim(grandchild, &gc_path, outer_variant_sites, layer);
                }
                _ => {}
            }
        }
    }

    /// Route a child prim's composition arcs to the variant spec's child_* maps.
    fn emit_variant_child_arcs(
        &mut self,
        child_prim: &ast::Prim<'_>,
        child_path: &str,
        child_tok: TokenId,
        variant_spec: &mut VariantSpec,
    ) {
        for meta in &child_prim.metadata {
            match meta {
                ast::PrimMeta::References(arc) => {
                    let listop = self.emit_arc_listop(arc, child_path);
                    if has_ref_content(&listop) {
                        let entry = variant_spec.child_references.entry(child_tok).or_default();
                        merge_ref_listop(entry, listop);
                    }
                }
                ast::PrimMeta::Payload(arc) => {
                    let listop = self.emit_arc_listop(arc, child_path);
                    if has_ref_content(&listop) {
                        let entry = variant_spec.child_payloads.entry(child_tok).or_default();
                        merge_ref_listop(entry, listop);
                    }
                }
                ast::PrimMeta::Inherits(paths) => {
                    let listop = self.emit_path_listop(paths, child_path);
                    if has_path_content(&listop) {
                        let entry = variant_spec.child_inherits.entry(child_tok).or_default();
                        merge_path_listop(entry, listop);
                    }
                }
                ast::PrimMeta::Specializes(paths) => {
                    let listop = self.emit_path_listop(paths, child_path);
                    if has_path_content(&listop) {
                        let entry = variant_spec.child_specializes.entry(child_tok).or_default();
                        merge_path_listop(entry, listop);
                    }
                }
                ast::PrimMeta::Variants(selections) => {
                    let entry = variant_spec
                        .child_variant_selections
                        .entry(child_tok)
                        .or_default();
                    for selection in selections {
                        entry.insert(
                            self.tokens.intern(selection.set_name),
                            self.tokens.intern(selection.branch_name),
                        );
                    }
                }
                _ => {}
            }
        }
    }

    // ── Composition arcs ────────────────────────────────────────────

    fn emit_arc_listop(&mut self, arc: &ast::ListOpArc<'_>, _prim_path: &str) -> ListOp<Reference> {
        let Some(items) = &arc.items else {
            // `= None` clears the arc list.
            return ListOp {
                explicit: Some(Vec::new()),
                ..ListOp::default()
            };
        };

        let refs: Vec<Reference> = items.iter().filter_map(|r| self.emit_arc_ref(r)).collect();

        let mut listop = ListOp::default();
        match arc.kind {
            ast::ListOpKind::Explicit => listop.explicit = Some(refs),
            ast::ListOpKind::Prepend => listop.prepend = refs,
            ast::ListOpKind::Append => listop.append = refs,
            ast::ListOpKind::Delete => listop.delete = refs,
        }
        listop
    }

    fn emit_arc_ref(&mut self, arc_ref: &ast::ArcRef<'_>) -> Option<Reference> {
        let layer_id = if let Some(asset) = arc_ref.asset {
            let resolved = self.resolve_asset(asset)?;
            if let Some(layer) = resolved.layer {
                self.resolved_layers.push(layer);
            }
            resolved.layer_id
        } else {
            // Self-reference (same layer).
            self.layer_id
        };

        // An omitted prim path, and the empty path `<>`, target the layer's
        // `defaultPrim`. OpenUSD warns that `<>` is ill-formed, reads it as
        // the empty path and composes it as an omitted target.
        //
        // Spec: AOUSD Core §10.3.2.1 (references with no prim path).
        let target = match arc_ref.prim_path {
            Some(path_str) if !path_str.is_empty() => {
                let path = Path::parse_absolute(path_str, self.tokens).ok()?;
                ReferenceTarget::Prim(self.paths.intern(path))
            }
            _ => ReferenceTarget::DefaultPrim,
        };

        let asset_str = arc_ref.asset.map(String::from);

        let layer_offset = LayerOffset {
            offset: arc_ref.offset.unwrap_or(0.0),
            scale: arc_ref.scale.unwrap_or(1.0),
        };

        Some(Reference {
            layer: layer_id,
            target,
            asset: asset_str,
            layer_offset,
        })
    }

    /// Converts an inherits or specializes list; relative paths resolve
    /// against the prim path `anchor`.
    ///
    /// Spec: AOUSD Core §8 (paths; relative paths are anchored to the prim
    /// that authors them).
    fn emit_path_listop(&mut self, paths: &ast::ListOpPaths<'_>, anchor: &str) -> ListOp<PathId> {
        let Some(items) = &paths.items else {
            return ListOp {
                explicit: Some(Vec::new()),
                ..ListOp::default()
            };
        };

        let path_ids: Vec<PathId> = items
            .iter()
            .filter_map(|s| {
                let absolute = absolute_path(s, anchor);
                match Path::parse_absolute(&absolute, self.tokens) {
                    Ok(path) => Some(self.paths.intern(path)),
                    Err(_) => {
                        self.diagnostics.push(Diagnostic::error(
                            paths.span,
                            format!("unsupported: arc path `<{s}>` is not a prim path; ignored"),
                        ));
                        None
                    }
                }
            })
            .collect();

        let mut listop = ListOp::default();
        match paths.kind {
            ast::ListOpKind::Explicit => listop.explicit = Some(path_ids),
            ast::ListOpKind::Prepend => listop.prepend = path_ids,
            ast::ListOpKind::Append => listop.append = path_ids,
            ast::ListOpKind::Delete => listop.delete = path_ids,
        }
        listop
    }

    fn emit_connection_listop(
        &mut self,
        conn: &ast::Connection<'_>,
        anchor: &str,
    ) -> ListOp<TargetPath> {
        let target_paths = self.target_paths(&conn.targets, anchor, conn.span);

        let mut listop = ListOp::default();
        match conn.op {
            ast::ListOpKind::Explicit => listop.explicit = Some(target_paths),
            ast::ListOpKind::Prepend => listop.prepend = target_paths,
            ast::ListOpKind::Append => listop.append = target_paths,
            ast::ListOpKind::Delete => listop.delete = target_paths,
        }
        listop
    }

    /// Parses relationship or connection targets; relative paths resolve
    /// against the prim path `anchor`, and unparsable ones are reported.
    ///
    /// Spec: AOUSD Core §8 (paths), §12.4 (targets).
    fn target_paths(
        &mut self,
        targets: &[&str],
        anchor: &str,
        span: crate::Span,
    ) -> Vec<TargetPath> {
        targets
            .iter()
            .filter_map(|target| {
                let absolute = absolute_path(target, anchor);
                match TargetPath::parse(&absolute, self.tokens, self.paths) {
                    Ok(path) => Some(path),
                    Err(_) => {
                        self.diagnostics.push(Diagnostic::error(
                            span,
                            format!("unsupported: target path `<{target}>` is not read"),
                        ));
                        None
                    }
                }
            })
            .collect()
    }

    fn declared_property_type(&mut self, type_hint: &str, is_array: bool) -> PropertyType {
        let base = type_hint.strip_suffix("[]").unwrap_or(type_hint);
        PropertyType::new(
            type_hint,
            is_array,
            default_scalar_for_type(base, self.tokens),
        )
    }

    // ── Value conversion ────────────────────────────────────────────

    /// Converts authored time samples, keeping blocked samples.
    ///
    /// A `None` sample is a value block in effect from its time until the
    /// next sample (AOUSD Core §12.3.6: individual time samples can be
    /// blocked), so it stays in the series as [`Value::Blocked`].
    fn convert_time_samples(
        &mut self,
        samples: &[ast::TimeSample<'_>],
        type_hint: &str,
    ) -> Vec<(f64, Value)> {
        samples
            .iter()
            .map(|s| {
                let value = s
                    .value
                    .as_ref()
                    .map_or(Value::Blocked, |v| self.convert_value(v, type_hint));
                (s.time, value)
            })
            .collect()
    }

    fn convert_value(&mut self, val: &ast::Value<'_>, type_hint: &str) -> Value {
        match val {
            ast::Value::Bool(b) => Value::Bool(*b),
            ast::Value::Int(n) => convert_int(*n, type_hint),
            ast::Value::Number(n) => convert_float(*n, type_hint),
            ast::Value::String(s) => match type_hint {
                "token" => Value::Token(self.tokens.intern(s)),
                "asset" => Value::Asset(Arc::from(*s)),
                "pathExpression" => Value::PathExpression(Arc::from(*s)),
                _ => Value::String(Arc::from(*s)),
            },
            ast::Value::Identifier(s) => Value::Token(self.tokens.intern(s)),
            ast::Value::Asset(s) => Value::Asset(Arc::from(*s)),
            ast::Value::Path(s) => Value::String(Arc::from(*s)),
            ast::Value::Blocked => Value::Blocked,
            ast::Value::Dictionary(entries) => {
                let dict_entries: Vec<(Arc<str>, Value)> = entries
                    .iter()
                    .map(|e| {
                        let key = Arc::from(e.key);
                        let val = self.convert_value(&e.value, e.type_name.unwrap_or(""));
                        (key, val)
                    })
                    .collect();
                Value::Dictionary(dict_entries)
            }
            ast::Value::Tuple(items) => {
                let elem_hint = element_type_hint(type_hint);
                if let Some(v) = self.try_convert_dimensioned(items, type_hint, elem_hint) {
                    v
                } else {
                    let elements: Vec<Value> = items
                        .iter()
                        .map(|v| self.convert_value(v, elem_hint))
                        .collect();
                    Value::Array(elements)
                }
            }
            ast::Value::Array(items) => {
                // For array types like "float3[]", pass "float3" (not "float")
                // so inner tuples are recognized as dimensioned types.
                let arr_elem_hint = type_hint.strip_suffix("[]").unwrap_or(type_hint);
                let elements: Vec<Value> = items
                    .iter()
                    .map(|v| self.convert_value(v, arr_elem_hint))
                    .collect();
                Value::Array(elements)
            }
            ast::Value::ArrayEdit(edit) => {
                let element_hint = type_hint.strip_suffix("[]").unwrap_or(type_hint);
                Value::ArrayEdit(self.convert_array_edit(edit, element_hint))
            }
        }
    }

    fn convert_array_edit(&mut self, edit: &ast::ArrayEdit<'_>, element_hint: &str) -> ArrayEdit {
        let ops = edit
            .instructions
            .iter()
            .map(|instruction| self.convert_array_edit_instruction(instruction, element_hint))
            .collect();
        ArrayEdit { ops }
    }

    fn convert_array_edit_instruction(
        &mut self,
        instruction: &ast::ArrayEditInstruction<'_>,
        element_hint: &str,
    ) -> ArrayEditOp {
        match instruction {
            ast::ArrayEditInstruction::Write { src, index } => ArrayEditOp::Write {
                src: self.convert_array_edit_operand(src, element_hint),
                index: convert_array_edit_index(*index),
            },
            ast::ArrayEditInstruction::Insert { src, index } => ArrayEditOp::Insert {
                src: self.convert_array_edit_operand(src, element_hint),
                index: convert_array_edit_index(*index),
            },
            ast::ArrayEditInstruction::Erase { index } => ArrayEditOp::Erase {
                index: convert_array_edit_index(*index),
            },
            ast::ArrayEditInstruction::MinSize(len) => ArrayEditOp::MinSize { len: *len },
            ast::ArrayEditInstruction::MaxSize(len) => ArrayEditOp::MaxSize { len: *len },
            ast::ArrayEditInstruction::Resize(len) => ArrayEditOp::Resize { len: *len },
        }
    }

    fn convert_array_edit_operand(
        &mut self,
        operand: &ast::ArrayEditOperand<'_>,
        element_hint: &str,
    ) -> ArrayEditOperand {
        match operand {
            ast::ArrayEditOperand::Literal(value) => {
                ArrayEditOperand::Literal(self.convert_value(value, element_hint))
            }
            ast::ArrayEditOperand::CopyFrom(index) => {
                ArrayEditOperand::CopyFrom(convert_array_edit_index(*index))
            }
        }
    }

    fn convert_metadata_value(&mut self, val: &ast::MetadataValue<'_>, type_hint: &str) -> Value {
        match val {
            ast::MetadataValue::Value(v) => self.convert_value(v, type_hint),
            ast::MetadataValue::None => Value::Blocked,
            ast::MetadataValue::Dictionary(entries) => {
                let dict_entries: Vec<(Arc<str>, Value)> = entries
                    .iter()
                    .map(|e| {
                        let key = Arc::from(e.key);
                        let val = self.convert_value(&e.value, e.type_name.unwrap_or(""));
                        (key, val)
                    })
                    .collect();
                Value::Dictionary(dict_entries)
            }
            ast::MetadataValue::String(s) => Value::String(Arc::from(s.as_str())),
        }
    }

    // ── Asset resolution helper ─────────────────────────────────────

    fn resolve_asset(&mut self, asset_path: &str) -> Option<ResolvedAsset> {
        self.resolver
            .resolve(asset_path, Some(self.layer_id), self.tokens, self.paths)
            .ok()
    }

    /// Tries to convert a tuple into a typed dimensioned [`Value`] (§6.3).
    ///
    /// Returns `None` if `type_hint` is not a recognized dimensioned type,
    /// letting the caller fall through to the generic `Value::Array` path.
    fn try_convert_dimensioned(
        &mut self,
        items: &[ast::Value<'_>],
        type_hint: &str,
        elem_hint: &str,
    ) -> Option<Value> {
        // Strip array suffix for matching: "float3[]" → "float3".
        let base = type_hint.strip_suffix("[]").unwrap_or(type_hint);
        match base {
            // Vectors — f64
            "double2" => Some(Value::Vec2d(extract_f64s::<2>(items))),
            "double3" => Some(Value::Vec3d(extract_f64s::<3>(items))),
            "double4" => Some(Value::Vec4d(extract_f64s::<4>(items))),
            // Vectors — f32
            "float2" => Some(Value::Vec2f(extract_f32s::<2>(items))),
            "float3" => Some(Value::Vec3f(extract_f32s::<3>(items))),
            "float4" => Some(Value::Vec4f(extract_f32s::<4>(items))),
            // Vectors — half
            "half2" => Some(Value::Vec2h(extract_halves::<2>(items))),
            "half3" => Some(Value::Vec3h(extract_halves::<3>(items))),
            "half4" => Some(Value::Vec4h(extract_halves::<4>(items))),
            // Vectors — i32
            "int2" => Some(Value::Vec2i(extract_i32s::<2>(items))),
            "int3" => Some(Value::Vec3i(extract_i32s::<3>(items))),
            "int4" => Some(Value::Vec4i(extract_i32s::<4>(items))),
            // Matrices — f64
            "matrix2d" => Some(Value::Matrix2d(Box::new(extract_matrix_f64::<4>(items)))),
            "matrix3d" => Some(Value::Matrix3d(Box::new(extract_matrix_f64::<9>(items)))),
            "matrix4d" => Some(Value::Matrix4d(Box::new(extract_matrix_f64::<16>(items)))),
            // Quaternions — stored as (i, j, k, r) but authored as (r, i, j, k)
            // in USDA text per §16.3.10.22.
            "quatd" => {
                let v = extract_f64s::<4>(items);
                Some(Value::Quatd([v[1], v[2], v[3], v[0]]))
            }
            "quatf" => {
                let v = extract_f32s::<4>(items);
                Some(Value::Quatf([v[1], v[2], v[3], v[0]]))
            }
            "quath" => {
                let v = extract_halves::<4>(items);
                Some(Value::Quath([v[1], v[2], v[3], v[0]]))
            }
            // Semantic aliases (§6.5) — same element layout, different type name.
            _ if is_semantic_vec_alias(base, 'f') => {
                let n = semantic_component_count(base);
                match n {
                    2 => Some(Value::Vec2f(extract_f32s::<2>(items))),
                    3 => Some(Value::Vec3f(extract_f32s::<3>(items))),
                    4 => Some(Value::Vec4f(extract_f32s::<4>(items))),
                    _ => None,
                }
            }
            _ if is_semantic_vec_alias(base, 'd') => {
                let n = semantic_component_count(base);
                match n {
                    2 => Some(Value::Vec2d(extract_f64s::<2>(items))),
                    3 => Some(Value::Vec3d(extract_f64s::<3>(items))),
                    4 => Some(Value::Vec4d(extract_f64s::<4>(items))),
                    _ => None,
                }
            }
            _ if is_semantic_vec_alias(base, 'h') => {
                let n = semantic_component_count(base);
                match n {
                    2 => Some(Value::Vec2h(extract_halves::<2>(items))),
                    3 => Some(Value::Vec3h(extract_halves::<3>(items))),
                    4 => Some(Value::Vec4h(extract_halves::<4>(items))),
                    _ => None,
                }
            }
            _ => {
                // Not a recognized dimensioned type — fall through to
                // generic array handling. This also covers nested arrays of
                // tuples (e.g. `float3[]`), where the inner tuples will be
                // converted individually via recursive `convert_value` calls.
                let _ = elem_hint;
                None
            }
        }
    }
}

// ── Specifier conversion ────────────────────────────────────────────────

/// Makes a USDA path absolute against the prim path `anchor`: `../B`,
/// `Child`, `.attr` and `../B.attr` are relative, `/A/B` is not.
///
/// Spec: AOUSD Core §8 (paths).
fn absolute_path(path: &str, anchor: &str) -> String {
    if path.starts_with('/') {
        return String::from(path);
    }
    let mut segments: Vec<&str> = anchor.split('/').filter(|s| !s.is_empty()).collect();
    let mut rest = path;
    loop {
        if let Some(tail) = rest.strip_prefix("../") {
            segments.pop();
            rest = tail;
        } else if rest == ".." {
            segments.pop();
            rest = "";
        } else if let Some(tail) = rest.strip_prefix("./") {
            rest = tail;
        } else {
            break;
        }
    }
    let mut out = String::new();
    for segment in &segments {
        out.push('/');
        out.push_str(segment);
    }
    if rest.starts_with('.') {
        if out.is_empty() {
            out.push('/');
        }
        out.push_str(rest);
    } else if !rest.is_empty() {
        out.push('/');
        out.push_str(rest);
    } else if out.is_empty() {
        out.push('/');
    }
    out
}

/// The registered value type of a metadata field, as far as ingestion needs
/// it to convert USDA literals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MetadataFieldType {
    /// Convert with this USDA type name.
    Typed(&'static str),
    /// A list-op field: explicit assignments are list ops too.
    ListOp(ListElement),
    /// Unregistered here: convert from the literal alone.
    Unknown,
}

/// The element type of a list-op metadata field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListElement {
    Token,
    String,
    Int64,
}

impl MetadataFieldType {
    fn type_hint(self) -> &'static str {
        match self {
            Self::Typed(hint) => hint,
            Self::ListOp(_) | Self::Unknown => "",
        }
    }
}

/// Combines a list-op statement into an earlier one for the same field.
/// Returns `false` when the two have different element types.
fn merge_field_list_ops(existing: &mut FieldValue, value: FieldValue) -> bool {
    match (existing, value) {
        (FieldValue::TokenListOp(a), FieldValue::TokenListOp(b)) => merge_path_listop(a, b),
        (FieldValue::StringListOp(a), FieldValue::StringListOp(b)) => merge_path_listop(a, b),
        (FieldValue::IntListOp(a), FieldValue::IntListOp(b)) => merge_path_listop(a, b),
        (FieldValue::UIntListOp(a), FieldValue::UIntListOp(b)) => merge_path_listop(a, b),
        (FieldValue::Int64ListOp(a), FieldValue::Int64ListOp(b)) => merge_path_listop(a, b),
        (FieldValue::UInt64ListOp(a), FieldValue::UInt64ListOp(b)) => merge_path_listop(a, b),
        (existing @ FieldValue::Value(_), value) => *existing = value,
        _ => return false,
    }
    true
}

/// Returns the registered type of a well-known metadata field.
///
/// USDA writes tokens and strings alike as quoted literals and integers
/// without a type suffix, so the field's registered type decides the value
/// type. The Sdf fields come from `pxr/usd/sdf/schema.cpp`
/// (`_RegisterStandardFields`); the plugin fields from the `SdfMetadata`
/// sections of `pxr/usd/usd/plugInfo.json`, `usdGeom/plugInfo.json`,
/// `usdPhysics/plugInfo.json` and `usdShade/plugInfo.json`.
///
/// Spec: AOUSD Core §7.6 (core metadata fields and their types).
fn metadata_field_type(key: &str) -> MetadataFieldType {
    use MetadataFieldType::{ListOp, Typed, Unknown};
    match key {
        "kind"
        | "colorSpace"
        | "colorManagementSystem"
        | "symmetryFunction"
        | "upAxis"
        | "interpolation"
        | "connectability"
        | "renderType"
        | "bindMaterialAs"
        | "outputName"
        | "constraintTargetIdentifier" => Typed("token"),
        "documentation" | "comment" | "displayName" | "displayGroup" | "owner" | "sessionOwner"
        | "prefix" | "suffix" | "symmetricPeer" => Typed("string"),
        "metersPerUnit" | "kilogramsPerUnit" | "timeCodesPerSecond" | "framesPerSecond"
        | "startTimeCode" | "endTimeCode" | "startFrame" | "endFrame" => Typed("double"),
        "elementSize" | "framePrecision" | "unauthoredValuesIndex" => Typed("int"),
        "arraySizeConstraint" => Typed("int64"),
        "hidden" | "active" | "instanceable" | "noLoadHint" => Typed("bool"),
        "allowedTokens" => Typed("token[]"),
        "displayGroupOrder" => Typed("string[]"),
        "colorConfiguration" => Typed("asset"),
        "apiSchemas" => ListOp(ListElement::Token),
        "clipSets" => ListOp(ListElement::String),
        "inactiveIds" => ListOp(ListElement::Int64),
        _ => Unknown,
    }
}

fn convert_specifier(spec: ast::Specifier) -> Specifier {
    match spec {
        ast::Specifier::Def => Specifier::Def,
        ast::Specifier::Over => Specifier::Over,
        ast::Specifier::Class => Specifier::Class,
    }
}

// ── Type hint decomposition ─────────────────────────────────────────────

/// Extract the scalar element type from a compound USD type name.
///
/// Handles vector types (`float3` → `float`), array types (`int[]` → `int`),
/// combined forms (`float3[]` → `float`), and named compound types
/// (`color3f` → `float`, `matrix4d` → `double`, `quatf` → `float`).
///
/// Returns the original hint unchanged for already-scalar types.
///
/// Spec: AOUSD Core §6.2 (scene description data types).
fn element_type_hint(hint: &str) -> &str {
    // Strip array suffix first: "float3[]" → "float3", "int[]" → "int"
    let base = hint.strip_suffix("[]").unwrap_or(hint);

    // Named compound types with element-type suffixes.
    // color3f, color4f, normal3f, point3f, vector3f, texCoord2f, texCoord3f → float
    // color3d, color4d, normal3d, point3d, vector3d, texCoord2d, texCoord3d → double
    // color3h, normal3h, point3h, vector3h, texCoord2h, texCoord3h → half
    if base.ends_with('f')
        && (base.starts_with("color")
            || base.starts_with("normal")
            || base.starts_with("point")
            || base.starts_with("vector")
            || base.starts_with("texCoord"))
    {
        return "float";
    }
    if base.ends_with('d')
        && (base.starts_with("color")
            || base.starts_with("normal")
            || base.starts_with("point")
            || base.starts_with("vector")
            || base.starts_with("texCoord"))
    {
        return "double";
    }
    if base.ends_with('h')
        && (base.starts_with("color")
            || base.starts_with("normal")
            || base.starts_with("point")
            || base.starts_with("vector")
            || base.starts_with("texCoord"))
    {
        return "half";
    }

    // matrix2d, matrix3d, matrix4d → double
    if base.starts_with("matrix") && base.ends_with('d') {
        return "double";
    }

    // quatf → float, quatd → double, quath → half
    match base {
        "quatf" => return "float",
        "quatd" => return "double",
        "quath" => return "half",
        _ => {}
    }

    // Simple vector types: float2, float3, float4, double2, double3, double4,
    // int2, int3, int4, half2, half3, half4, etc.
    // Strip trailing digits to get the scalar type.
    let trimmed = base.trim_end_matches(|c: char| c.is_ascii_digit());
    if !trimmed.is_empty() && trimmed.len() < base.len() {
        return trimmed;
    }

    // Already scalar or unrecognised — return as-is.
    base
}

// ── Numeric conversion with type hints ──────────────────────────────────

fn convert_int(n: i64, type_hint: &str) -> Value {
    match type_hint {
        // USDA writes `bool` values as `0` or `1` inside dictionaries.
        "bool" => Value::Bool(n != 0),
        "int" => Value::Int(n as i32),
        "uint" => Value::UInt(n as u32),
        "int64" => Value::Int64(n),
        "uint64" => Value::UInt64(n as u64),
        "float" => Value::Float(n as f32),
        "double" => Value::Double(n as f64),
        "half" => Value::Half(half_from_f64(n as f64)),
        "timecode" => Value::TimeCode(n as f64),
        _ => Value::Int64(n),
    }
}

fn convert_float(n: f64, type_hint: &str) -> Value {
    match type_hint {
        "float" => Value::Float(n as f32),
        "half" => Value::Half(half_from_f64(n)),
        "int" => Value::Int(n as i32),
        "int64" => Value::Int64(n as i64),
        "timecode" => Value::TimeCode(n),
        _ => Value::Double(n),
    }
}

/// Minimal IEEE 754 half-precision conversion (truncating).
fn half_from_f64(v: f64) -> u16 {
    let f = v as f32;
    let bits = f.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mantissa = bits & 0x007F_FFFF;

    if exp == 0 {
        // Zero / denorm → half zero.
        sign as u16
    } else if exp == 0xFF {
        // Inf/NaN.
        (sign | 0x7C00 | if mantissa != 0 { 0x0200 } else { 0 }) as u16
    } else {
        let new_exp = exp - 127 + 15;
        if new_exp >= 31 {
            (sign | 0x7C00) as u16 // overflow → inf
        } else if new_exp <= 0 {
            sign as u16 // underflow → zero
        } else {
            (sign | ((new_exp as u32) << 10) | (mantissa >> 13)) as u16
        }
    }
}

// ── Dimensioned type helpers (§6.3) ─────────────────────────────────────

/// Extracts `N` f64 values from AST value nodes.
fn extract_f64s<const N: usize>(items: &[ast::Value<'_>]) -> [f64; N] {
    let mut out = [0.0_f64; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0.0, ast_to_f64);
    }
    out
}

/// Extracts `N` f32 values from AST value nodes.
fn extract_f32s<const N: usize>(items: &[ast::Value<'_>]) -> [f32; N] {
    let mut out = [0.0_f32; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0.0, |v| ast_to_f64(v) as f32);
    }
    out
}

/// Extracts `N` half values (as raw u16 bits) from AST value nodes.
fn extract_halves<const N: usize>(items: &[ast::Value<'_>]) -> [u16; N] {
    let mut out = [0_u16; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0, |v| half_from_f64(ast_to_f64(v)));
    }
    out
}

/// Extracts `N` i32 values from AST value nodes.
fn extract_i32s<const N: usize>(items: &[ast::Value<'_>]) -> [i32; N] {
    let mut out = [0_i32; N];
    for (i, val) in out.iter_mut().enumerate() {
        *val = items.get(i).map_or(0, ast_to_i32);
    }
    out
}

/// Extracts `N` f64 values from a matrix tuple-of-tuples or flat tuple.
///
/// USDA matrices are authored as nested tuples:
///   `((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))`
/// Each element is either a `Tuple` (nested row) or a scalar (flat).
fn extract_matrix_f64<const N: usize>(items: &[ast::Value<'_>]) -> [f64; N] {
    let mut out = [0.0_f64; N];
    let mut idx = 0;
    for item in items {
        match item {
            ast::Value::Tuple(row) => {
                for elem in row {
                    if idx < N {
                        out[idx] = ast_to_f64(elem);
                        idx += 1;
                    }
                }
            }
            _ => {
                if idx < N {
                    out[idx] = ast_to_f64(item);
                    idx += 1;
                }
            }
        }
    }
    out
}

/// Converts an AST value node to f64 (best-effort).
fn ast_to_f64(v: &ast::Value<'_>) -> f64 {
    match v {
        ast::Value::Number(n) => *n,
        ast::Value::Int(n) => *n as f64,
        _ => 0.0,
    }
}

/// Converts an AST value node to i32 (best-effort).
fn ast_to_i32(v: &ast::Value<'_>) -> i32 {
    match v {
        ast::Value::Int(n) => *n as i32,
        ast::Value::Number(n) => *n as i32,
        _ => 0,
    }
}

/// Returns `true` if `name` is a semantic type alias (§6.5) ending with
/// precision suffix `p` ('f', 'd', or 'h').
///
/// Semantic aliases: `color3f`, `color4f`, `normal3f`, `point3f`,
/// `vector3f`, `texCoord2f`, `texCoord3f`, `frame4d`, etc.
fn is_semantic_vec_alias(name: &str, precision: char) -> bool {
    if !name.ends_with(precision) {
        return false;
    }
    name.starts_with("color")
        || name.starts_with("normal")
        || name.starts_with("point")
        || name.starts_with("vector")
        || name.starts_with("texCoord")
        || name.starts_with("frame")
}

/// Extracts the component count from a semantic alias name.
///
/// E.g., `"color3f"` → 3, `"texCoord2f"` → 2, `"frame4d"` → 4.
fn semantic_component_count(name: &str) -> usize {
    // The digit is always the second-to-last character.
    name.chars()
        .rev()
        .nth(1)
        .and_then(|c| c.to_digit(10))
        .unwrap_or(0) as usize
}

// ── ListOp merge helpers ────────────────────────────────────────────────

fn merge_ref_listop(target: &mut ListOp<Reference>, source: ListOp<Reference>) {
    if source.explicit.is_some() {
        target.explicit = source.explicit;
    }
    target.prepend.extend(source.prepend);
    target.append.extend(source.append);
    target.delete.extend(source.delete);
}

fn merge_path_listop<T>(target: &mut ListOp<T>, source: ListOp<T>) {
    if source.explicit.is_some() {
        target.explicit = source.explicit;
    }
    target.prepend.extend(source.prepend);
    target.append.extend(source.append);
    target.delete.extend(source.delete);
}

fn has_ref_content(listop: &ListOp<Reference>) -> bool {
    listop.explicit.is_some() || !listop.prepend.is_empty() || !listop.append.is_empty()
}

fn has_path_content<T>(listop: &ListOp<T>) -> bool {
    listop.explicit.is_some() || !listop.prepend.is_empty() || !listop.append.is_empty()
}

fn convert_array_edit_index(index: ast::ArrayEditIndex) -> ArrayIndex {
    match index {
        ast::ArrayEditIndex::Position(value) => ArrayIndex::Position(value),
        ast::ArrayEditIndex::End => ArrayIndex::End,
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
        "dictionary" => Value::Dictionary(Vec::new()),
        base if is_semantic_vec_alias(base, 'f') => match semantic_component_count(base) {
            2 => Value::Vec2f([0.0; 2]),
            3 => Value::Vec3f([0.0; 3]),
            _ => Value::Vec4f([0.0; 4]),
        },
        base if is_semantic_vec_alias(base, 'd') => match semantic_component_count(base) {
            2 => Value::Vec2d([0.0; 2]),
            3 => Value::Vec3d([0.0; 3]),
            _ => Value::Vec4d([0.0; 4]),
        },
        base if is_semantic_vec_alias(base, 'h') => match semantic_component_count(base) {
            2 => Value::Vec2h([0; 2]),
            3 => Value::Vec3h([0; 3]),
            _ => Value::Vec4h([0; 4]),
        },
        _ => Value::Null,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_cst;

    use alloc::vec;

    use layerstack::doc::get_field;
    use layerstack::property::{PropertyEntry, PropertySpec, Variability, get_property};

    /// The authored attribute default of `name`.
    fn attr_default<'a>(properties: &'a [PropertyEntry], name: &TokenId) -> Option<&'a Value> {
        get_property(properties, *name)?.default.as_ref()
    }

    /// The authored property `name`.
    fn prop<'a>(properties: &'a [PropertyEntry], name: &TokenId) -> &'a PropertySpec {
        get_property(properties, *name).expect("property authored")
    }
    use layerstack::interner::TokenInterner;
    use layerstack::path::PathInterner;

    /// Stub resolver that assigns incrementing layer IDs.
    struct StubResolver {
        next_id: u64,
    }

    impl StubResolver {
        fn new() -> Self {
            Self { next_id: 100 }
        }
    }

    impl AssetResolver for StubResolver {
        fn resolve(
            &mut self,
            _asset_path: &str,
            _anchor: Option<LayerId>,
            _tokens: &mut TokenInterner,
            _paths: &mut PathInterner,
        ) -> Result<ResolvedAsset, layerstack::AssetResolveError> {
            let id = LayerId(self.next_id);
            self.next_id += 1;
            Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: Arc::from("stub"),
                layer: Some(Layer::new(id)),
            })
        }

        fn resolved_path(&self, _id: LayerId) -> Option<&str> {
            None
        }
    }

    /// Helper: parse source through CST → AST → emit.
    fn emit_source(src: &str) -> (EmitResult, TokenInterner, PathInterner) {
        let cst = parse_cst(src);
        assert!(
            cst.diagnostics.is_empty(),
            "CST errors: {:?}",
            cst.diagnostics
        );
        let ast_result = crate::lower::lower(&cst.tree, src);
        assert!(
            ast_result.diagnostics.is_empty(),
            "AST errors: {:?}",
            ast_result.diagnostics
        );
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let mut resolver = StubResolver::new();
        let result = emit(
            &ast_result.layer,
            LayerId(1),
            &mut tokens,
            &mut paths,
            &mut resolver,
        );
        (result, tokens, paths)
    }

    #[test]
    fn emit_simple_prim() {
        let (result, mut tokens, paths) = emit_source("#usda 1.0\ndef Xform \"root\" {\n}\n");
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.specifier, Some(Specifier::Def));
    }

    #[test]
    fn emit_nested_prims() {
        let src = "\
#usda 1.0
def \"A\" {
    def \"B\" {
    }
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).expect("/A interned");
        let a_spec = result.layer.prims.get(&a_id).expect("prim /A");
        let b_tok = tokens.intern("B");
        assert!(a_spec.authored_children.contains(&b_tok));

        let b_path = Path::parse_absolute("/A/B", &mut tokens).unwrap();
        let b_id = paths.lookup(&b_path).expect("/A/B interned");
        assert!(result.layer.prims.contains_key(&b_id));
    }

    #[test]
    fn emit_attribute_int() {
        let src = "#usda 1.0\ndef \"A\" {\n    int x = 42\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let x_tok = tokens.intern("x");
        assert_eq!(
            attr_default(&spec.properties, &x_tok),
            Some(&(Value::Int(42)))
        );
    }

    #[test]
    fn emit_attribute_double() {
        let src = "#usda 1.0\ndef \"A\" {\n    double y = 2.5\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let y_tok = tokens.intern("y");
        assert_eq!(
            attr_default(&spec.properties, &y_tok),
            Some(&(Value::Double(2.5)))
        );
    }

    #[test]
    fn emit_sublayers() {
        let src = "\
#usda 1.0
(
    subLayers = [
        @./sub.usd@
    ]
)
";
        let (result, _, _) = emit_source(src);
        assert_eq!(result.layer.sublayers.len(), 1);
        assert_eq!(result.layer.sublayers[0], SublayerEntry::new(LayerId(100)));
        assert_eq!(result.resolved_layers.len(), 1);
    }

    #[test]
    fn emit_inherits() {
        let src = "#usda 1.0\ndef \"A\" (\n    inherits = </B>\n) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        assert!(spec.inherits.explicit.is_some());
        let b_path = Path::parse_absolute("/B", &mut tokens).unwrap();
        let b_id = paths.lookup(&b_path).expect("/B");
        assert_eq!(spec.inherits.explicit.as_ref().unwrap(), &[b_id]);
    }

    #[test]
    fn emit_references() {
        let src = "#usda 1.0\ndef \"A\" (\n    prepend references = @./ref.usd@\n) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        assert_eq!(spec.references.prepend.len(), 1);
        assert_eq!(spec.references.prepend[0].layer, LayerId(100));
    }

    #[test]
    fn emit_variant_selections() {
        let src = "\
#usda 1.0
def \"A\" (
    variants = {
        string shade = \"red\"
    }
) {
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let shade_tok = tokens.intern("shade");
        let red_tok = tokens.intern("red");
        assert_eq!(spec.variant_selections.get(&shade_tok), Some(&red_tok));
    }

    #[test]
    fn emit_variant_selections_with_quoted_key() {
        let src = "\
#usda 1.0
def \"A\" (
    variants = {
        string \"shade\" = \"red\"
    }
) {
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let shade_tok = tokens.intern("shade");
        let red_tok = tokens.intern("red");
        assert_eq!(spec.variant_selections.get(&shade_tok), Some(&red_tok));
    }

    #[test]
    fn emit_variant_set_with_fields() {
        let src = "\
#usda 1.0
def \"A\" {
    variantSet \"color\" = {
        \"red\" {
            int r = 255
        }
        \"blue\" {
            int r = 0
        }
    }
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let color_tok = tokens.intern("color");
        assert!(spec.variant_sets.contains_key(&color_tok));
        let vs = spec.variant_sets.get(&color_tok).unwrap();
        let red_tok = tokens.intern("red");
        let blue_tok = tokens.intern("blue");
        assert!(vs.variants.contains_key(&red_tok));
        assert!(vs.variants.contains_key(&blue_tok));
        let r_tok = tokens.intern("r");
        assert_eq!(
            attr_default(&vs.variants.get(&red_tok).unwrap().properties, &r_tok),
            Some(&(Value::Int(255)))
        );
        assert_eq!(
            attr_default(&vs.variants.get(&blue_tok).unwrap().properties, &r_tok),
            Some(&(Value::Int(0)))
        );
    }

    #[test]
    fn emit_reorder_name_children() {
        let src = "\
#usda 1.0
def \"A\" {
    reorder nameChildren = [\"B\", \"C\"]
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let b = tokens.intern("B");
        let c = tokens.intern("C");
        assert_eq!(spec.prim_order, Some(vec![b, c]));
    }

    #[test]
    fn emit_time_samples() {
        let src = "\
#usda 1.0
def \"A\" {
    float x.timeSamples = {
        1: 10.0,
        2: 20.0,
    }
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let x_tok = tokens.intern("x");
        if let Some(ts) = prop(&spec.properties, &x_tok).time_samples.as_ref() {
            assert_eq!(ts.len(), 2);
            assert!((ts[0].0 - 1.0).abs() < 1e-10);
            assert_eq!(ts[0].1, Value::Float(10.0));
            assert!((ts[1].0 - 2.0).abs() < 1e-10);
            assert_eq!(ts[1].1, Value::Float(20.0));
        } else {
            panic!("expected TimeSamples");
        }
    }

    #[test]
    fn emit_keeps_blocked_time_samples() {
        let src = "\
#usda 1.0
def \"A\" {
    float x.timeSamples = {
        1: 10.0,
        2: None,
        3: 30.0,
    }
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let x_tok = tokens.intern("x");
        let Some(ts) = prop(&spec.properties, &x_tok).time_samples.as_ref() else {
            panic!("expected TimeSamples");
        };
        assert_eq!(
            ts,
            &vec![
                (1.0, Value::Float(10.0)),
                (2.0, Value::Blocked),
                (3.0, Value::Float(30.0)),
            ],
            "a `None` sample blocks from its time on, so it must stay in the series"
        );
    }

    #[test]
    fn emit_relocates_reports_unsupported() {
        let src = "\
#usda 1.0
(
    relocates = {
        </Rig/Anim>: </Anim>
        </Rig/Other>: </Other>
    }
)
def \"Rig\" {
}
";
        let (result, _tokens, _paths) = emit_source(src);
        assert_eq!(result.diagnostics.len(), 2, "one diagnostic per entry");
        let first = &result.diagnostics[0];
        assert_eq!(first.severity, crate::diagnostic::Severity::Error);
        assert!(
            first.message.contains("relocates") && first.message.contains("</Rig/Anim>: </Anim>"),
            "message names the ignored entry: {}",
            first.message
        );
        assert_eq!(first.span.text(src), "</Rig/Anim>: </Anim>");
        // The rest of the layer is still emitted.
        assert_eq!(result.layer.prims.len(), 2, "pseudo-root and /Rig");
    }

    #[test]
    fn emit_relationship() {
        let src = "#usda 1.0\ndef \"A\" {\n    rel target = </B>\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let target_tok = tokens.intern("target");
        let rel = prop(&spec.properties, &target_tok);
        assert!(rel.is_relationship());
        assert!(rel.targets.is_some());
    }

    #[test]
    fn emit_connection_in_variant_child() {
        let src = "\
#usda 1.0
def \"A\" (
    add variantSets = [\"v\"]
    variants = {
        string v = \"on\"
    }
)
{
    variantSet \"v\" = {
        \"on\" {
            over \"Rig\"
            {
                add double focalLength.connect = </A/Lens.focalLength>
            }
        }
    }
}
";
        let (result, mut tokens, paths) = emit_source(src);
        // The `over "Rig"` inside the variant should produce a PrimSpec at /A/Rig.
        let rig_path = Path::parse_absolute("/A/Rig", &mut tokens).unwrap();
        let rig_id = paths.lookup(&rig_path).expect("/A/Rig interned");
        let spec = result.layer.prims.get(&rig_id).expect("prim /A/Rig");
        let connect_key = tokens.intern("focalLength");
        let attr = prop(&spec.properties, &connect_key);
        assert!(
            attr.targets.is_some(),
            "expected connection paths, got {attr:?}"
        );
    }

    #[test]
    fn emit_blocked_value() {
        let src = "#usda 1.0\ndef \"A\" {\n    int x = None\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prims.get(&a_id).unwrap();
        let x_tok = tokens.intern("x");
        assert_eq!(
            attr_default(&spec.properties, &x_tok),
            Some(&(Value::Blocked))
        );
    }

    #[test]
    fn emit_directly_nested_variant_sets() {
        // Simplified version of the DirectlyNestedVariants case from
        // the BasicNestedVariants conformance fixture.
        let src = r##"#usda 1.0
def Scope "D" (
    add variantSets = ['standin']
    variants = {
        string shadingVariant = "spooky"
        string standin = "anim"
    }
)
{
    variantSet "standin" = {
        "anim" (
            add variantSets = ['shadingVariant']
        ) {
            variantSet "shadingVariant" = {
                "default" {
                    def Cone "anim_default_cone"
                    {
                    }
                }
                "spooky" {
                    def Sphere "anim_spooky_sphere"
                    {
                    }
                }
            }
        }
    }
}
"##;
        let (result, mut tokens, paths) = emit_source(src);
        let d_path = Path::parse_absolute("/D", &mut tokens).unwrap();
        let d_id = paths.lookup(&d_path).unwrap();
        let d_spec = result.layer.prims.get(&d_id).unwrap();

        // The "standin" variant set should exist on the PrimSpec.
        let standin_tok = tokens.intern("standin");
        let standin_vs = d_spec
            .variant_sets
            .get(&standin_tok)
            .expect("standin variant set");

        // The "anim" variant branch should exist.
        let anim_tok = tokens.intern("anim");
        let anim_variant = standin_vs.variants.get(&anim_tok).expect("anim variant");

        // The "shadingVariant" variant set should also exist on the PrimSpec
        // (not inside VariantSpec — nested variant sets are hoisted).
        let shading_tok = tokens.intern("shadingVariant");
        assert!(
            d_spec.variant_sets.contains_key(&shading_tok),
            "shadingVariant should be on PrimSpec.variant_sets: {:?}",
            d_spec
                .variant_sets
                .keys()
                .map(|k| tokens.resolve(*k))
                .collect::<Vec<_>>()
        );
        let shading_vs = d_spec.variant_sets.get(&shading_tok).unwrap();

        // The "default" and "spooky" branches should exist.
        let default_tok = tokens.intern("default");
        let spooky_tok = tokens.intern("spooky");
        assert!(shading_vs.variants.contains_key(&default_tok));
        assert!(shading_vs.variants.contains_key(&spooky_tok));

        // anim_default_cone is in shadingVariant=default
        let cone_tok = tokens.intern("anim_default_cone");
        let default_variant = shading_vs.variants.get(&default_tok).unwrap();
        assert!(default_variant.authored_children.contains(&cone_tok));

        // anim_spooky_sphere is in shadingVariant=spooky
        let sphere_tok = tokens.intern("anim_spooky_sphere");
        let spooky_variant = shading_vs.variants.get(&spooky_tok).unwrap();
        assert!(spooky_variant.authored_children.contains(&sphere_tok));

        // Both children should have required outer variant sites
        // pointing to standin=anim (since they're nested inside it).
        let cone_reqs = default_variant.required_outer_variant_sites.get(&cone_tok);
        assert!(
            cone_reqs.is_some(),
            "anim_default_cone should have required outer variant sites"
        );
        assert_eq!(
            cone_reqs.unwrap(),
            &[VariantSelectionSite {
                host_path: d_id,
                set: standin_tok,
                variant: anim_tok,
            }]
        );

        let sphere_reqs = spooky_variant.required_outer_variant_sites.get(&sphere_tok);
        assert!(
            sphere_reqs.is_some(),
            "anim_spooky_sphere should have required outer variant sites"
        );
        assert_eq!(
            sphere_reqs.unwrap(),
            &[VariantSelectionSite {
                host_path: d_id,
                set: standin_tok,
                variant: anim_tok,
            }]
        );

        // Nested children should NOT be in the outer variant's
        // authored_children — they are gated by the inner variant set.
        // The composition engine discovers them through the inner variant
        // set's VariantSpec and filters via required outer variant sites.
        assert!(
            !anim_variant.authored_children.contains(&cone_tok),
            "anim variant should NOT list nested child anim_default_cone"
        );
        assert!(
            !anim_variant.authored_children.contains(&sphere_tok),
            "anim variant should NOT list nested child anim_spooky_sphere"
        );

        // variant_set_order should be [standin, shadingVariant].
        assert_eq!(d_spec.variant_set_order.len(), 2);
        assert_eq!(d_spec.variant_set_order[0], standin_tok);
        assert_eq!(d_spec.variant_set_order[1], shading_tok);
    }

    #[test]
    fn emit_triple_nested_variant_sets() {
        // Full DirectlyNestedVariants case: standin → shadingVariant → standin (reused name).
        let src = r##"#usda 1.0
def Scope "D" (
    add variantSets = ['standin']
    variants = {
        string shadingVariant = "spooky"
        string standin = "anim"
    }
)
{
    variantSet "standin" = {
        "anim" (
            add variantSets = ['shadingVariant']
        ) {
            variantSet "shadingVariant" = {
                "default" {
                    def Cone "anim_default_cone"
                    {
                    }
                }
                "spooky" (
                    add variantSets = ['standin']
                ) {
                    def Sphere "anim_spooky_sphere"
                    {
                    }
                    variantSet "standin" = {
                        "anim" {
                            def Sphere "anim_spooky_anim_sphere"
                            {
                            }
                        }
                    }
                }
            }
        }
        "render" (
            add variantSets = ['shadingVariant']
        ) {
            variantSet "shadingVariant" = {
                "default" {
                    def Cube "render_default_cube"
                    {
                    }
                }
                "spooky" {
                    def Cylinder "render_spooky_cylinder"
                    {
                    }
                }
            }
        }
    }
}
"##;
        let (result, mut tokens, paths) = emit_source(src);
        let d_path = Path::parse_absolute("/D", &mut tokens).unwrap();
        let d_id = paths.lookup(&d_path).unwrap();
        let d_spec = result.layer.prims.get(&d_id).unwrap();

        let standin_tok = tokens.intern("standin");
        let shading_tok = tokens.intern("shadingVariant");
        let anim_tok = tokens.intern("anim");
        let spooky_tok = tokens.intern("spooky");

        // variant_set_order should be [standin, shadingVariant].
        assert_eq!(
            d_spec
                .variant_set_order
                .iter()
                .map(|t| tokens.resolve(*t))
                .collect::<Vec<_>>(),
            vec!["standin", "shadingVariant"],
            "variant_set_order"
        );

        // standin variant set should have "anim" and "render" branches.
        let standin_vs = d_spec.variant_sets.get(&standin_tok).expect("standin VS");
        assert!(standin_vs.variants.contains_key(&anim_tok));
        let render_tok = tokens.intern("render");
        assert!(standin_vs.variants.contains_key(&render_tok));

        // shadingVariant variant set should have "default" and "spooky" branches.
        let shading_vs = d_spec
            .variant_sets
            .get(&shading_tok)
            .expect("shadingVariant VS");
        let default_tok = tokens.intern("default");
        assert!(shading_vs.variants.contains_key(&default_tok));
        assert!(shading_vs.variants.contains_key(&spooky_tok));

        // anim_spooky_anim_sphere: lives in standin=anim branch, with two
        // required outer variant sites on the same host.
        let sphere3_tok = tokens.intern("anim_spooky_anim_sphere");
        let anim_branch = standin_vs.variants.get(&anim_tok).unwrap();
        assert!(
            anim_branch.authored_children.contains(&sphere3_tok),
            "anim_spooky_anim_sphere should be in standin=anim: {:?}",
            anim_branch
                .authored_children
                .iter()
                .map(|t| tokens.resolve(*t))
                .collect::<Vec<_>>()
        );
        let sphere3_reqs = anim_branch.required_outer_variant_sites.get(&sphere3_tok);
        assert!(
            sphere3_reqs.is_some(),
            "anim_spooky_anim_sphere should have required outer variant sites"
        );
        assert_eq!(
            sphere3_reqs.unwrap(),
            &[
                VariantSelectionSite {
                    host_path: d_id,
                    set: standin_tok,
                    variant: anim_tok,
                },
                VariantSelectionSite {
                    host_path: d_id,
                    set: shading_tok,
                    variant: spooky_tok,
                }
            ],
            "required outer variant sites for anim_spooky_anim_sphere"
        );

        // anim_spooky_sphere: lives in shadingVariant=spooky branch, with one
        // required outer variant site.
        let sphere2_tok = tokens.intern("anim_spooky_sphere");
        let spooky_branch = shading_vs.variants.get(&spooky_tok).unwrap();
        assert!(
            spooky_branch.authored_children.contains(&sphere2_tok),
            "anim_spooky_sphere should be in shadingVariant=spooky: {:?}",
            spooky_branch
                .authored_children
                .iter()
                .map(|t| tokens.resolve(*t))
                .collect::<Vec<_>>()
        );
        let sphere2_reqs = spooky_branch.required_outer_variant_sites.get(&sphere2_tok);
        assert!(
            sphere2_reqs.is_some(),
            "anim_spooky_sphere should have required outer variant sites"
        );
        assert_eq!(
            sphere2_reqs.unwrap(),
            &[VariantSelectionSite {
                host_path: d_id,
                set: standin_tok,
                variant: anim_tok,
            }],
            "required outer variant sites for anim_spooky_sphere"
        );
    }

    #[test]
    fn emit_type_name_stored() {
        let src = "#usda 1.0\ndef Xform \"root\" {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        let xform_tok = tokens.intern("Xform");
        assert_eq!(spec.type_name, Some(xform_tok));
    }

    #[test]
    fn emit_type_name_none_for_untyped() {
        let src = "#usda 1.0\ndef \"root\" {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.type_name, None);
    }

    #[test]
    fn emit_type_name_mesh() {
        let src = "#usda 1.0\ndef Mesh \"geo\" {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/geo", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("geo path interned");
        let spec = result.layer.prims.get(&root_id).expect("geo prim");
        let mesh_tok = tokens.intern("Mesh");
        assert_eq!(spec.type_name, Some(mesh_tok));
    }

    #[test]
    fn emit_instanceable() {
        // Lowercase `true` (Rust-style).
        let src = "#usda 1.0\ndef Xform \"root\" (instanceable = true) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.instanceable, Some(true));
    }

    #[test]
    fn emit_instanceable_capitalized() {
        // Capitalized `True` (Python-style, common in USDA files).
        let src = "#usda 1.0\ndef Xform \"root\" (instanceable = True) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.instanceable, Some(true));
    }

    #[test]
    fn emit_active_false() {
        let src = "#usda 1.0\ndef Xform \"root\" (active = false) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.active, Some(false));
    }

    #[test]
    fn emit_active_true() {
        let src = "#usda 1.0\ndef Xform \"root\" (active = true) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.active, Some(true));
    }

    #[test]
    fn emit_active_capitalized() {
        let src = "#usda 1.0\ndef Xform \"root\" (active = False) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &mut tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prims.get(&root_id).expect("root prim");
        assert_eq!(spec.active, Some(false));
    }

    #[test]
    fn emit_prepend_api_schemas_produces_token_listop() {
        let src = "#usda 1.0\n\
                   def Mesh \"card\" (\n\
                       prepend apiSchemas = [\"MaterialBindingAPI\"]\n\
                   ) {\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let card_path = Path::parse_absolute("/card", &mut tokens).unwrap();
        let card_id = paths.lookup(&card_path).expect("card path interned");
        let spec = result.layer.prims.get(&card_id).expect("card prim");

        let api_tok = tokens.intern("apiSchemas");
        let field = get_field(&spec.fields, &api_tok).expect("apiSchemas field");
        match field {
            FieldValue::TokenListOp(listop) => {
                assert!(listop.explicit.is_none(), "should not be explicit");
                assert_eq!(listop.prepend.len(), 1);
                let mat_tok = tokens.intern("MaterialBindingAPI");
                assert_eq!(listop.prepend[0], mat_tok);
                assert!(listop.append.is_empty());
                assert!(listop.delete.is_empty());
            }
            other => panic!("expected TokenListOp, got {:?}", other),
        }
    }

    #[test]
    fn emit_default_prim() {
        let src = "#usda 1.0\n(\n    defaultPrim = \"Root\"\n)\n\ndef Xform \"Root\"\n{\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        assert_eq!(
            result.layer.default_prim,
            Some(tokens.intern("Root")),
            "defaultPrim is read from layer metadata"
        );
    }

    #[test]
    fn emit_dictionary_metadata() {
        let src = "\
#usda 1.0
def \"A\" (
    customData = {
        string foo = \"bar\"
        int count = 42
    }
) {
}
";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let cd_tok = tokens.intern("customData");
        let field = get_field(&spec.fields, &cd_tok).expect("customData field");
        match field {
            FieldValue::Value(Value::Dictionary(entries)) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(&*entries[0].0, "foo");
                assert_eq!(entries[0].1, Value::String(Arc::from("bar")));
                assert_eq!(&*entries[1].0, "count");
                assert_eq!(entries[1].1, Value::Int(42));
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    #[test]
    fn emit_nested_dictionary_metadata() {
        let src = "\
#usda 1.0
def \"A\" (
    customData = {
        dictionary inner = {
            double val = 1.5
        }
    }
) {
}
";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let cd_tok = tokens.intern("customData");
        let field = get_field(&spec.fields, &cd_tok).expect("customData field");
        match field {
            FieldValue::Value(Value::Dictionary(entries)) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(&*entries[0].0, "inner");
                match &entries[0].1 {
                    Value::Dictionary(inner) => {
                        assert_eq!(inner.len(), 1);
                        assert_eq!(&*inner[0].0, "val");
                        assert_eq!(inner[0].1, Value::Double(1.5));
                    }
                    other => panic!("expected nested Dictionary, got {:?}", other),
                }
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    #[test]
    fn emit_dictionary_attribute() {
        let src = "\
#usda 1.0
def \"A\" {
    dictionary d = {
        string key = \"value\"
    }
}
";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let d_tok = tokens.intern("d");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &d_tok)
                .expect("d field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::Dictionary(entries)) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(&*entries[0].0, "key");
                assert_eq!(entries[0].1, Value::String(Arc::from("value")));
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    #[test]
    fn emit_tuple_float3() {
        let src = "#usda 1.0\ndef \"A\" {\n    float3 pos = (1.0, 2.0, 3.0)\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let pos_tok = tokens.intern("pos");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &pos_tok)
                .expect("pos field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::Vec3f(v)) => {
                assert_eq!(*v, [1.0_f32, 2.0, 3.0]);
            }
            other => panic!("expected Vec3f, got {:?}", other),
        }
    }

    #[test]
    fn emit_array_int() {
        let src = "#usda 1.0\ndef \"A\" {\n    int[] ids = [1, 2, 3]\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let ids_tok = tokens.intern("ids");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &ids_tok)
                .expect("ids field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::Array(items)) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Value::Int(1));
                assert_eq!(items[1], Value::Int(2));
                assert_eq!(items[2], Value::Int(3));
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn emit_nested_array() {
        // Array of tuples: float3[] points = [(1, 2, 3), (4, 5, 6)]
        let src = "#usda 1.0\ndef \"A\" {\n    float3[] points = [(1, 2, 3), (4, 5, 6)]\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let pts_tok = tokens.intern("points");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &pts_tok)
                .expect("points field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::Array(items)) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], Value::Vec3f([1.0, 2.0, 3.0]));
                assert_eq!(items[1], Value::Vec3f([4.0, 5.0, 6.0]));
            }
            other => panic!("expected Array of Vec3f, got {:?}", other),
        }
    }

    #[test]
    fn emit_empty_array() {
        let src = "#usda 1.0\ndef \"A\" {\n    int[] empty = []\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let empty_tok = tokens.intern("empty");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &empty_tok)
                .expect("empty field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::Array(items)) => {
                assert!(items.is_empty());
            }
            other => panic!("expected empty Array, got {:?}", other),
        }
    }

    #[test]
    fn emit_double3() {
        let src = "#usda 1.0\ndef \"A\" {\n    double3 pos = (1.5, 2.5, 3.5)\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let pos_tok = tokens.intern("pos");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &pos_tok)
                .expect("pos field")
                .clone(),
        );
        assert_eq!(*field, FieldValue::Value(Value::Vec3d([1.5, 2.5, 3.5])));
    }

    #[test]
    fn emit_int2() {
        let src = "#usda 1.0\ndef \"A\" {\n    int2 v = (10, 20)\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let v_tok = tokens.intern("v");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &v_tok)
                .expect("v field")
                .clone(),
        );
        assert_eq!(*field, FieldValue::Value(Value::Vec2i([10, 20])));
    }

    #[test]
    fn emit_matrix4d() {
        let src = "#usda 1.0\ndef \"A\" {\n    matrix4d xform = ((1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 0, 0, 1))\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let xf_tok = tokens.intern("xform");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &xf_tok)
                .expect("xform field")
                .clone(),
        );
        let mut expected = [0.0_f64; 16];
        expected[0] = 1.0;
        expected[5] = 1.0;
        expected[10] = 1.0;
        expected[15] = 1.0;
        assert_eq!(
            *field,
            FieldValue::Value(Value::Matrix4d(Box::new(expected)))
        );
    }

    #[test]
    fn emit_color3f_semantic_alias() {
        let src = "#usda 1.0\ndef \"A\" {\n    color3f primvars:displayColor = (1, 0, 0)\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let c_tok = tokens.intern("primvars:displayColor");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &c_tok)
                .expect("displayColor field")
                .clone(),
        );
        assert_eq!(*field, FieldValue::Value(Value::Vec3f([1.0, 0.0, 0.0])));
    }

    #[test]
    fn emit_quatf() {
        // USDA text order: (r, i, j, k) = (1.0, 0.0, 0.0, 0.0)
        // Storage order: [i, j, k, r] = [0.0, 0.0, 0.0, 1.0]
        let src = "#usda 1.0\ndef \"A\" {\n    quatf rot = (1.0, 0.0, 0.0, 0.0)\n}\n";
        let (result, mut tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let rot_tok = tokens.intern("rot");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &rot_tok)
                .expect("rot field")
                .clone(),
        );
        // Storage is [i, j, k, r]; from (r=1, i=0, j=0, k=0) → [0, 0, 0, 1].
        assert_eq!(
            *field,
            FieldValue::Value(Value::Quatf([0.0, 0.0, 0.0, 1.0]))
        );
    }

    #[test]
    fn emit_array_edit_value() {
        let src =
            "#usda 1.0\ndef \"A\" {\n    int[] x = edit (write 3 to [0], append 4, resize 3)\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &mut tokens).unwrap();
        let a_id = paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prims.get(&a_id).unwrap();
        let x_tok = tokens.intern("x");
        let entry = prop(&spec.properties, &x_tok);

        match &entry.default {
            Some(Value::ArrayEdit(edit)) => {
                assert_eq!(edit.ops.len(), 3);
                assert!(matches!(edit.ops[0], ArrayEditOp::Write { .. }));
                assert!(matches!(
                    edit.ops[1],
                    ArrayEditOp::Insert {
                        index: ArrayIndex::End,
                        ..
                    }
                ));
                assert!(matches!(edit.ops[2], ArrayEditOp::Resize { len: 3 }));
            }
            other => panic!("expected array edit value, got {other:?}"),
        }

        let property_type = entry.type_name.as_ref().expect("property type");
        assert!(property_type.is_array);
        assert_eq!(property_type.default_scalar, Value::Int(0));
    }

    /// Returns the prim spec at `path` from an emit result.
    fn prim<'r>(
        result: &'r EmitResult,
        tokens: &mut TokenInterner,
        paths: &PathInterner,
        path: &str,
    ) -> &'r PrimSpec {
        let path = Path::parse_absolute(path, tokens).unwrap();
        let id = paths.lookup(&path).expect("path interned");
        result.layer.prims.get(&id).expect("prim spec")
    }

    #[test]
    fn emit_keeps_default_samples_and_connections_together() {
        // Spec: AOUSD Core §7.6.4.2.3 (a value, a connection, or both).
        let src = "\
#usda 1.0
def \"Root\" {
    custom float a = 1
    float a.timeSamples = {
        0: 2,
        1: None,
    }
    float b = 4
    float b.connect = </Root.a>
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let spec = prim(&result, &mut tokens, &paths, "/Root");
        let a = prop(&spec.properties, &tokens.intern("a"));
        assert!(a.custom);
        assert_eq!(a.default, Some(Value::Float(1.0)));
        assert_eq!(
            a.time_samples.as_deref(),
            Some(&[(0.0, Value::Float(2.0)), (1.0, Value::Blocked)][..])
        );
        let b = prop(&spec.properties, &tokens.intern("b"));
        assert!(!b.custom);
        assert_eq!(b.default, Some(Value::Float(4.0)));
        assert!(
            b.targets.is_some(),
            "the connection survives next to the value"
        );
    }

    #[test]
    fn emit_property_qualifiers_and_metadata() {
        let src = "\
#usda 1.0
def \"A\" {
    custom uniform token authorship:main:tool = \"painter\" (
        doc = \"Which tool\"
    )
    color3f[] primvars:displayColor = [(1, 0, 0)] (
        \"a comment\"
        colorSpace = \"srgb_texture\"
        customData = {
            string source = \"scan\"
        }
        elementSize = 1
        interpolation = \"constant\"
        limits = {
            dictionary soft = {
                float minimum = 0
            }
        }
    )
    custom rel look:material = </A/Mat> (
        displayName = \"Look\"
    )
}
";
        let (result, mut tokens, paths) = emit_source(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let spec = prim(&result, &mut tokens, &paths, "/A");
        let names: Vec<_> = spec.properties.iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            vec![
                tokens.intern("authorship:main:tool"),
                tokens.intern("primvars:displayColor"),
                tokens.intern("look:material"),
            ],
            "properties keep their authored order"
        );

        let tool = prop(&spec.properties, &tokens.intern("authorship:main:tool"));
        assert!(tool.custom);
        assert_eq!(tool.variability, Variability::Uniform);
        assert_eq!(
            tool.metadata(tokens.intern("documentation")),
            Some(&FieldValue::Value(Value::string("Which tool")))
        );

        let color = prop(&spec.properties, &tokens.intern("primvars:displayColor"));
        assert!(!color.custom);
        assert_eq!(color.variability, Variability::Varying);
        let interpolation = tokens.intern("constant");
        let srgb = tokens.intern("srgb_texture");
        assert_eq!(
            color.metadata(tokens.intern("interpolation")),
            Some(&FieldValue::Value(Value::Token(interpolation))),
            "`interpolation` is a token field"
        );
        assert_eq!(
            color.metadata(tokens.intern("colorSpace")),
            Some(&FieldValue::Value(Value::Token(srgb)))
        );
        assert_eq!(
            color.metadata(tokens.intern("elementSize")),
            Some(&FieldValue::Value(Value::Int(1))),
            "`elementSize` is an int field"
        );
        assert_eq!(
            color.metadata(tokens.intern("comment")),
            Some(&FieldValue::Value(Value::string("a comment")))
        );
        assert!(matches!(
            color.metadata(tokens.intern("customData")),
            Some(FieldValue::Value(Value::Dictionary(_)))
        ));
        assert_eq!(
            color.metadata(tokens.intern("limits")),
            Some(&FieldValue::Value(Value::Dictionary(vec![(
                Arc::from("soft"),
                Value::Dictionary(vec![(Arc::from("minimum"), Value::Float(0.0))]),
            )])))
        );

        let rel = prop(&spec.properties, &tokens.intern("look:material"));
        assert!(rel.is_relationship());
        assert!(rel.custom);
        assert_eq!(
            rel.metadata(tokens.intern("displayName")),
            Some(&FieldValue::Value(Value::string("Look")))
        );
    }

    #[test]
    fn emit_prim_metadata_and_property_order() {
        let src = "\
#usda 1.0
def Xform \"A\" (
    \"prim comment\"
    prepend apiSchemas = [\"GeomModelAPI\"]
    append apiSchemas = [\"StudioReviewAPI:main\"]
    displayName = \"The A\"
    doc = \"Documented\"
    hidden = true
    kind = \"component\"
)
{
    float y = 1
    float x = 2
    reorder properties = [\"x\", \"y\"]
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let spec = prim(&result, &mut tokens, &paths, "/A");
        let api = tokens.intern("apiSchemas");
        let geom = tokens.intern("GeomModelAPI");
        let review = tokens.intern("StudioReviewAPI:main");
        assert_eq!(
            spec.field(api),
            Some(&FieldValue::TokenListOp(ListOp {
                prepend: vec![geom],
                append: vec![review],
                ..ListOp::default()
            })),
            "list-op statements for one field combine"
        );
        assert_eq!(
            spec.field(tokens.intern("comment")),
            Some(&FieldValue::Value(Value::string("prim comment")))
        );
        assert_eq!(
            spec.field(tokens.intern("documentation")),
            Some(&FieldValue::Value(Value::string("Documented")))
        );
        assert_eq!(
            spec.field(tokens.intern("hidden")),
            Some(&FieldValue::Value(Value::Bool(true)))
        );
        assert_eq!(
            spec.field(tokens.intern("displayName")),
            Some(&FieldValue::Value(Value::string("The A")))
        );
        assert_eq!(
            spec.property_order,
            Some(vec![tokens.intern("x"), tokens.intern("y")])
        );
    }

    #[test]
    fn emit_explicit_api_schemas_is_a_list_op() {
        let src = "#usda 1.0\ndef \"A\" (\n    apiSchemas = [\"GeomModelAPI\"]\n)\n{\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let spec = prim(&result, &mut tokens, &paths, "/A");
        let geom = tokens.intern("GeomModelAPI");
        assert_eq!(
            spec.field(tokens.intern("apiSchemas")),
            Some(&FieldValue::TokenListOp(ListOp {
                explicit: Some(vec![geom]),
                ..ListOp::default()
            }))
        );
    }

    #[test]
    fn emit_layer_metadata() {
        let src = "\
#usda 1.0
(
    \"layer comment\"
    customLayerData = {
        string author = \"me\"
    }
    defaultPrim = \"A\"
    doc = \"About\"
    metersPerUnit = 1
    timeCodesPerSecond = 24
    upAxis = \"Z\"
)
def \"A\" {
}
";
        let (result, mut tokens, _paths) = emit_source(src);
        let layer = &result.layer;
        assert_eq!(layer.default_prim, Some(tokens.intern("A")));
        let z = tokens.intern("Z");
        assert_eq!(
            layer.metadata(tokens.intern("upAxis")),
            Some(&FieldValue::Value(Value::Token(z)))
        );
        assert_eq!(
            layer.metadata(tokens.intern("metersPerUnit")),
            Some(&FieldValue::Value(Value::Double(1.0))),
            "`metersPerUnit` is a double field even when written as an integer"
        );
        assert_eq!(
            layer.metadata(tokens.intern("timeCodesPerSecond")),
            Some(&FieldValue::Value(Value::Double(24.0)))
        );
        assert_eq!(
            layer.metadata(tokens.intern("documentation")),
            Some(&FieldValue::Value(Value::string("About")))
        );
        assert_eq!(
            layer.metadata(tokens.intern("comment")),
            Some(&FieldValue::Value(Value::string("layer comment")))
        );
        assert!(matches!(
            layer.metadata(tokens.intern("customLayerData")),
            Some(FieldValue::Value(Value::Dictionary(_)))
        ));
        assert!(
            layer.metadata(tokens.intern("defaultPrim")).is_none(),
            "`defaultPrim` is kept in its dedicated member only"
        );
    }

    #[test]
    fn emit_reports_attribute_relationship_name_clash() {
        // Spec: AOUSD Core §7.3.3 (properties of one prim share a name space).
        let src = "#usda 1.0\ndef \"A\" {\n    float x = 1\n    rel x = </B>\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
        assert_eq!(
            result.diagnostics[0].span.text(src).trim_end(),
            "rel x = </B>"
        );
        let spec = prim(&result, &mut tokens, &paths, "/A");
        let x = prop(&spec.properties, &tokens.intern("x"));
        assert!(x.is_attribute());
        assert!(x.targets.is_none());
    }

    #[test]
    fn emit_variant_branch_metadata_and_properties() {
        let src = "\
#usda 1.0
def \"A\" (
    variantSets = \"v\"
)
{
    variantSet \"v\" = {
        \"one\" (
            kind = \"group\"
        ) {
            uniform token mode = \"fast\" (
                doc = \"branch doc\"
            )
            reorder properties = [\"mode\"]
        }
    }
}
";
        let (result, mut tokens, paths) = emit_source(src);
        let spec = prim(&result, &mut tokens, &paths, "/A");
        let branch = &spec.variant_sets[&tokens.intern("v")].variants[&tokens.intern("one")];
        let group = tokens.intern("group");
        assert_eq!(
            get_field(&branch.fields, &tokens.intern("kind")),
            Some(&FieldValue::Value(Value::Token(group)))
        );
        let mode = prop(&branch.properties, &tokens.intern("mode"));
        assert_eq!(mode.variability, Variability::Uniform);
        assert!(mode.metadata(tokens.intern("documentation")).is_some());
        assert_eq!(branch.property_order, Some(vec![tokens.intern("mode")]));
    }

    #[test]
    fn emit_path_expression_is_typed() {
        let src = "#usda 1.0\ndef \"A\" {\n    pathExpression e = \"/A/B //C\"\n    pathExpression[] l = [\"/X\"]\n}\n";
        let (result, mut tokens, paths) = emit_source(src);
        let spec = prim(&result, &mut tokens, &paths, "/A");
        assert_eq!(
            attr_default(&spec.properties, &tokens.intern("e")),
            Some(&Value::PathExpression(Arc::from("/A/B //C")))
        );
        assert_eq!(
            attr_default(&spec.properties, &tokens.intern("l")),
            Some(&Value::Array(vec![Value::PathExpression(Arc::from("/X"))]))
        );
    }

    #[test]
    fn absolute_path_anchors_relative_paths() {
        assert_eq!(absolute_path("/A/B", "/X"), "/A/B");
        assert_eq!(absolute_path("../Sym", "/Root/Rig/Left"), "/Root/Rig/Sym");
        assert_eq!(absolute_path("Child", "/Root/Left"), "/Root/Left/Child");
        assert_eq!(absolute_path(".weight", "/Root/Left"), "/Root/Left.weight");
        assert_eq!(absolute_path("../B.attr", "/A/C"), "/A/B.attr");
        assert_eq!(absolute_path("../..", "/A/B"), "/");
        assert_eq!(absolute_path("./C", "/A"), "/A/C");
    }
}
