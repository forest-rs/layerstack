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
//! # Rejected layers
//!
//! Some text parses but is not scene description OpenUSD can hold: an
//! inherits, specializes, reference or payload path, a relocates path or a
//! relationship target path with a variant selection (`</A{v=x}B>`), and
//! an attribute connection path written with one; an arc path that does
//! not name a prim (`<>` but for a reference or payload, `</>`, `</A.b>`,
//! a relative reference path); and a target or connection that is `<>` or
//! the root. OpenUSD's text parser
//! (`Sdf_TextFileFormatParser`, validating with `SdfSchema::IsValid*`)
//! rejects the whole layer, so it never opens. Emitting reports each such
//! path as a [`Severity::Error`] diagnostic with OpenUSD's message, leaves
//! it out of the layer and sets [`EmitResult::rejected`]; a caller that
//! follows OpenUSD treats the layer as one that failed to load. A relative
//! relationship target inside a variant branch is anchored at the prim with
//! its variant selections, so it is rejected unless it climbs out of every
//! branch; a relative connection is anchored without them, as OpenUSD
//! strips them from connection paths.
//!
//! Spec: AOUSD Core §16.2.16.9 (target paths may not contain variant
//! selections; the text parser may reject them), §8 (path grammar),
//! §10.3.2 (arcs). The Core leaves the other paths to the parser too; this
//! follows OpenUSD, which rejects them all.
//!
//! [`Severity::Error`]: crate::diagnostic::Severity::Error
//! [`Layer`]: layerstack::Layer
//! [`PrimSpec`]: layerstack::PrimSpec

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::hash::Hash;

use hashbrown::HashSet;
use layerstack::doc::{
    FieldEntry, FieldValue, Layer, LayerId, LayerOffset, PrimSpec, Reference, ReferenceSource,
    Specifier, SublayerEntry, Value, VariantSets, VariantSpec, get_field_mut, set_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner, PropertyPath, TargetPath};
use layerstack::property::{
    PropertyEntry, PropertyKind, PropertySpec, Variability, property_entry,
};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::variable_expression::is_expression;
use layerstack::{
    ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex, AssetResolveError, AssetResolver,
    MetadataField, MetadataTarget, MetadataType, PropertyType, ReferenceTarget, ResolvedAsset,
    SchemaRegistry, SharedVec, StringArray, TypedArray,
};
use smallvec::SmallVec;

use crate::Span;
use crate::ast;
use crate::diagnostic::Diagnostic;
use crate::lower::{PropertyTail, anchor_bracketed, has_prim_variant_selection, split_property_tail};
use crate::metadata::{MetadataRejection, MetadataValueError};
use crate::numeric::{convert_scalar, dimensioned_value, element_type_hint};
use crate::writer::is_identifier;
use layerstack::value_type::ValueType;

/// Result of emitting a single USDA layer.
#[derive(Debug)]
#[non_exhaustive]
pub struct EmitResult {
    /// The emitted layer.
    pub layer: Layer,
    /// Any layers produced by resolving asset paths (sublayers, references,
    /// payloads). The caller should insert these into their store.
    pub resolved_layers: Vec<Layer>,
    /// Diagnostics from the emit pass.
    pub diagnostics: Vec<Diagnostic>,
    /// Whether OpenUSD's text parser rejects the layer as a whole (see
    /// [Rejected layers](self#rejected-layers)); `diagnostics` say why.
    pub rejected: bool,
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
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
) -> EmitResult {
    emit_with_arrays(
        ast,
        layer_id,
        tokens,
        paths,
        resolver,
        crate::read::PreparedValues::default(),
        None,
    )
}

/// Emits USDA with caller-registered metadata types and spec targets.
///
/// The registry must use `tokens`' interner. It types the fields its plugins
/// declare as the built-in fields are typed ([`crate::metadata`]): a statement
/// of a field registered for its kind of spec converts to the field's type,
/// and a literal the type cannot hold rejects the layer with a diagnostic
/// ([`crate::metadata::MetadataRejection`]); a field registered only for other
/// kinds of spec keeps its literal's own type. Registered defaults remain
/// registry data; importing never authors them. Asset resolution remains
/// caller-owned, including recursively emitted layers.
///
/// Spec: AOUSD Core §7.4 (metadata), §6.2–6.6 (typed values), §16.2 (USDA).
pub fn emit_with_registry(
    ast: &ast::Layer<'_>,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    registry: &SchemaRegistry,
) -> EmitResult {
    emit_with_arrays(
        ast,
        layer_id,
        tokens,
        paths,
        resolver,
        crate::read::PreparedValues::default(),
        Some(registry),
    )
}

/// The field `key` that reading `statements`, USDA metadata statements of
/// that field (`key = 1.5`, `prepend key = ["a", "b"]`), gives a layer;
/// `None` when they do not read cleanly.
///
/// OpenUSD keeps metadata no schema registers as an `SdfUnregisteredValue`
/// holding the text of its value as its text parser read it
/// (`textParserHelpers.cpp`), a list op as a list of such texts; a crate
/// file stores that text (`crateFile.cpp`), and the text writer writes it
/// back as it is. A crate reader reads the text with this, so that the
/// layer holds what reading OpenUSD's USDA of the crate file would give,
/// with the same conversions ([`emit`]).
///
/// Spec: AOUSD Core §7.4 (metadata fields), §16.2 (USDA).
#[must_use]
pub fn metadata_from_text(
    key: &str,
    statements: &str,
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Option<FieldValue> {
    /// Metadata statements name no assets.
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
    let source = format!("#usda 1.0\n(\n{statements}\n)\n");
    let parsed = crate::parser::parse(&source);
    if !parsed.diagnostics.is_empty() {
        return None;
    }
    let emitted = emit(&parsed.layer, LayerId(0), tokens, paths, &mut NoAssets);
    if !emitted.diagnostics.is_empty() || emitted.rejected {
        return None;
    }
    emitted.layer.metadata(tokens.intern(key)).cloned()
}

pub(crate) fn emit_with_arrays(
    ast: &ast::Layer<'_>,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    arrays: crate::read::PreparedValues,
    registry: Option<&SchemaRegistry>,
) -> EmitResult {
    let mut ctx = EmitCtx {
        tokens,
        paths,
        defer_variant_arcs: resolver.defers_variant_arcs(),
        resolver,
        layer_id,
        resolved_layers: Vec::new(),
        diagnostics: Vec::new(),
        rejections: Vec::new(),
        rejected: false,
        arrays,
        registry,
        emitted_arcs: false,
        property_types: Vec::new(),
    };
    let layer = ctx.emit_layer(ast);
    EmitResult {
        layer,
        resolved_layers: ctx.resolved_layers,
        diagnostics: ctx.diagnostics,
        rejected: ctx.rejected,
    }
}

// ── Internal context ────────────────────────────────────────────────────

struct EmitCtx<'a> {
    registry: Option<&'a SchemaRegistry>,
    arrays: crate::read::PreparedValues,
    tokens: &'a TokenInterner,
    paths: &'a PathInterner,
    resolver: &'a mut dyn AssetResolver,
    /// Whether arcs inside variant branches are left for composition to
    /// resolve ([`AssetResolver::defers_variant_arcs`]).
    defer_variant_arcs: bool,
    layer_id: LayerId,
    resolved_layers: Vec<Layer>,
    diagnostics: Vec<Diagnostic>,
    /// Why values being converted could not take their declared type; the
    /// caller that owns the value reports them (see [`EmitCtx::checked`]).
    rejections: Vec<String>,
    /// Whether the layer is rejected (see [`EmitResult::rejected`]).
    rejected: bool,
    /// Whether a prim or variant spec authors a reference or payload.
    emitted_arcs: bool,
    /// The attribute types the layer has declared so far, each built once
    /// and shared by the attributes declaring it (a text or matrix type
    /// keeps its zero in an allocation; see [`PropertyType`]); at most
    /// [`PROPERTY_TYPES_SHARED`].
    property_types: Vec<PropertyType>,
}

/// How many attribute types one layer's attributes share
/// ([`EmitCtx::property_types`]): layers declare a few dozen at most, and a
/// layer declaring more builds the rest per attribute.
const PROPERTY_TYPES_SHARED: usize = 64;

impl EmitCtx<'_> {
    /// Reports scene description OpenUSD's text parser rejects, which
    /// rejects the layer.
    fn reject(&mut self, span: Span, message: String) {
        self.diagnostics.push(Diagnostic::error(span, message));
        self.rejected = true;
    }

    // ── Layer ────────────────────────────────────────────────────────

    fn emit_layer(&mut self, ast: &ast::Layer<'_>) -> Layer {
        let mut layer = Layer::new(self.layer_id);

        // Process layer metadata.
        for meta in &ast.metadata {
            match meta {
                ast::LayerMeta::SubLayers(items) => {
                    for item in items {
                        let offset = LayerOffset {
                            offset: item.offset.unwrap_or(0.0),
                            scale: item.scale.unwrap_or(1.0),
                        };
                        // A sublayer that does not resolve keeps its place;
                        // composition reports it (AOUSD Core §10.3.1, §10.6;
                        // OpenUSD `PcpErrorInvalidSublayerPath`).
                        let Some(resolved) = self.resolve_asset(&item.asset) else {
                            layer
                                .sublayers_mut()
                                .push(SublayerEntry::unresolved(&*item.asset, offset));
                            continue;
                        };
                        layer.sublayers_mut().push(SublayerEntry::with_asset(
                            resolved.layer_id,
                            &*item.asset,
                            offset,
                        ));
                        if let Some(sub_layer) = resolved.layer {
                            self.resolved_layers.push(sub_layer);
                        }
                    }
                }
                ast::LayerMeta::Relocates(entries) => {
                    // The statement authors the field, an empty one for
                    // `relocates = {}` (`SdfLayer::HasRelocates`).
                    let _ = layer.relocates_mut();
                    for entry in entries {
                        // OpenUSD names the source path in both messages.
                        // Spec: `SdfSchema::IsValidRelocatesSourcePath`.
                        if has_variant_selection(entry.source) {
                            self.reject(
                                entry.span,
                                format!("'{}' is not a valid relocates source path", entry.source),
                            );
                            continue;
                        }
                        if has_variant_selection(entry.target) {
                            self.reject(
                                entry.span,
                                format!("'{}' is not a valid relocates target path", entry.source),
                            );
                            continue;
                        }
                        if let Some(relocate) = self.emit_relocate(entry) {
                            layer.relocates_mut().push(relocate);
                        }
                    }
                }
                ast::LayerMeta::Doc(doc) => {
                    // Spec: AOUSD Core §7.6.1.5.1 (`documentation`).
                    let key = self.tokens.intern("documentation");
                    set_field_vec(
                        layer.metadata_fields_mut(),
                        key,
                        FieldValue::Value(Value::String(Arc::from(&**doc))),
                    );
                }
                ast::LayerMeta::Custom(entry) if entry.key == "defaultPrim" => {
                    // Spec: AOUSD Core §7.6.1.2.3 (`defaultPrim: token`),
                    // authored as a quoted string in USDA layer metadata.
                    let name = match &entry.value {
                        ast::MetadataValue::Value(ast::Value::String(name)) => Some(&**name),
                        ast::MetadataValue::String(name) => Some(name.as_str()),
                        _ => None,
                    };
                    if let Some(name) = name {
                        layer.set_default_prim(Some(self.tokens.intern(name)));
                    }
                }
                ast::LayerMeta::Custom(entry) => {
                    // Spec: AOUSD Core §7.6.1 (layer spec fields), §12.2.7.
                    self.emit_metadata_entry(
                        entry,
                        layer.metadata_fields_mut(),
                        MetadataTarget::Layer,
                    );
                }
            }
        }

        // Process root prims.
        let root_path = Path::root();
        let root_path_id = self.paths.intern(root_path);

        // Collect root children for a pseudo root prim if needed.
        let mut root_children = Vec::with_capacity(ast.prims.len());
        // The text of the path of the prim being emitted, which anchors the
        // relative paths it authors: each prim's name is pushed while it is.
        let mut prim_path = String::new();
        for prim in &ast.prims {
            if !self.admits_prim_name(prim) {
                continue;
            }
            let name_tok = self.tokens.intern(&prim.name);
            root_children.push(name_tok);
            self.emit_prim(
                prim,
                PrimPlace {
                    parent: root_path_id,
                    name: name_tok,
                },
                &mut prim_path,
                &[],
                &mut layer,
            );
        }

        // If there are root prims, create a root prim spec to hold children
        // and any root ordering.
        if !root_children.is_empty() || ast.root_prim_order.is_some() {
            let mut root_spec = PrimSpec::default().with_children(root_children);
            if let Some(order) = &ast.root_prim_order {
                root_spec = root_spec
                    .with_prim_order(order.iter().map(|n| self.tokens.intern(n)).collect());
            }
            layer.insert_prim(root_path_id, root_spec);
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
        if !self.emitted_arcs {
            layer.note_no_reference_arcs();
        }
        layer
    }

    // ── Prims ────────────────────────────────────────────────────────

    /// Whether `prim`'s name is an identifier, which its spec can be placed
    /// at; any other name (empty, holding `/`, a space or a control
    /// character) rejects the layer, as OpenUSD's text parser rejects it
    /// (`'' is not a valid prim name`, `_CreatePrimSpec`,
    /// `pxr/usd/sdf/textParserHelpers.cpp`: `SdfPath::IsValidIdentifier`).
    ///
    /// Spec: AOUSD Core §7.3.3 (prim names are identifiers), §8 (paths).
    fn admits_prim_name(&mut self, prim: &ast::Prim<'_>) -> bool {
        // The name's escapes are evaluated (`def "\\x41"` is `A`) before it
        // is checked, as OpenUSD's parser checks the evaluated string.
        if !is_identifier(&prim.name) {
            let message = format!("'{}' is not a valid prim name", prim.name);
            self.reject(prim.span, message);
            return false;
        }
        true
    }

    /// Emits `prim`, the child `place` of its parent, whose path's text
    /// `prim_path` holds; `prim_path` holds the prim's while it is emitted.
    fn emit_prim(
        &mut self,
        prim: &ast::Prim<'_>,
        place: PrimPlace,
        prim_path: &mut String,
        outer_variant_sites: &[VariantSelectionSite],
        layer: &mut Layer,
    ) {
        let parent_len = prim_path.len();
        prim_path.push('/');
        prim_path.push_str(&prim.name);
        let path_id = self.paths.intern_child(place.parent, place.name);

        let mut spec = PrimSpec::default().with_outer_variant_sites(outer_variant_sites.to_vec());
        spec.specifier = Some(convert_specifier(prim.specifier));
        spec.type_name = prim.type_name.map(|t| self.tokens.intern(t));

        // Process prim metadata.
        self.emit_prim_metadata(&prim.metadata, prim_path, outer_variant_sites, &mut spec);

        // Process prim body children.
        for child in &prim.children {
            match child {
                ast::PrimChild::Attribute(attr) => {
                    self.emit_attribute(attr, &mut spec.properties, prim_path);
                }
                ast::PrimChild::Relationship(rel) => {
                    self.emit_relationship(
                        rel,
                        &mut spec.properties,
                        prim_path,
                        outer_variant_sites,
                    );
                }
                ast::PrimChild::Prim(child_prim) => {
                    if !self.admits_prim_name(child_prim) {
                        continue;
                    }
                    let child_name = self.tokens.intern(&child_prim.name);
                    if !spec.authored_children.contains(&child_name) {
                        spec.authored_children.push(child_name);
                    }
                    let place = PrimPlace {
                        parent: path_id,
                        name: child_name,
                    };
                    self.emit_prim(child_prim, place, prim_path, outer_variant_sites, layer);
                }
                ast::PrimChild::VariantSet(vs) => {
                    self.emit_variant_set(
                        vs,
                        path_id,
                        prim_path,
                        outer_variant_sites,
                        &mut spec.rare_mut().variant_sets,
                        layer,
                    );
                }
                ast::PrimChild::ReorderNameChildren(names) => {
                    spec.rare_mut().prim_order =
                        Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                }
                ast::PrimChild::ReorderProperties(names) => {
                    // Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`); the
                    // `reorder properties` statement authors `propertyOrder`.
                    spec.rare_mut().property_order =
                        Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                }
            }
        }

        layer.insert_prim(path_id, spec);
        prim_path.truncate(parent_len);
    }

    /// Interns the absolute prim path `text`, as interning its parsed
    /// [`Path`] does (its names' tokens, then the path and its ancestors),
    /// without building the `Path`: each name's path from its parent's
    /// ([`PathInterner::intern_child`]). `None` when `text` does not parse.
    fn intern_prim_path(&self, text: &str) -> Option<PathId> {
        let names = text.strip_prefix('/')?;
        let mut path = self.paths.intern(Path::root());
        if names.is_empty() {
            return Some(path);
        }
        let mut tokens = SmallVec::<[TokenId; 16]>::new();
        for name in names.split('/') {
            if name.is_empty() {
                return None;
            }
            tokens.push(self.tokens.intern(name));
        }
        for name in tokens {
            path = self.paths.intern_child(path, name);
        }
        Some(path)
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

    /// Emits `metadata` of the prim at `prim_path` into `spec`, the prim
    /// spec inside the variant branches `sites`.
    fn emit_prim_metadata(
        &mut self,
        metadata: &[ast::PrimMeta<'_>],
        prim_path: &str,
        sites: &[VariantSelectionSite],
        spec: &mut PrimSpec,
    ) {
        for meta in metadata {
            match meta {
                ast::PrimMeta::References(arc) => {
                    self.emitted_arcs = true;
                    let statement = self.emit_arc_listop(arc, "Reference", !sites.is_empty());
                    set_statement(spec.references.make_mut(), arc.kind, statement);
                }
                ast::PrimMeta::Payload(arc) => {
                    self.emitted_arcs = true;
                    let statement = self.emit_arc_listop(arc, "Payload", !sites.is_empty());
                    set_statement(spec.payloads.make_mut(), arc.kind, statement);
                }
                ast::PrimMeta::Inherits(paths) => {
                    let statement = self.emit_path_listop(paths, prim_path, sites, "Inherit");
                    set_statement(spec.rare_mut().inherits.make_mut(), paths.kind, statement);
                }
                ast::PrimMeta::Specializes(paths) => {
                    let statement = self.emit_path_listop(paths, prim_path, sites, "Specializes");
                    set_statement(
                        spec.rare_mut().specializes.make_mut(),
                        paths.kind,
                        statement,
                    );
                }
                ast::PrimMeta::Variants(selections) => {
                    for sel in selections {
                        let set_tok = self.tokens.intern(&sel.set_name);
                        let branch_tok = self.tokens.intern(&sel.branch_name);
                        spec.rare_mut()
                            .variant_selections
                            .insert(set_tok, branch_tok);
                    }
                }
                ast::PrimMeta::VariantSets(listop) => {
                    let names = self.emit_name_listop(listop);
                    declare_listed_sets(&names, &mut spec.rare_mut().variant_sets);
                    set_statement(
                        spec.rare_mut().variant_set_names.make_mut(),
                        listop.kind,
                        names,
                    );
                }
                // `instanceable` and `active` have members of their own,
                // and `typeName` is the spec's type; each reads as its
                // field's type (`instanceable = 0` is `false`).
                ast::PrimMeta::Custom(entry) if matches!(entry.key, "instanceable" | "active") => {
                    match self.metadata_entry(entry, MetadataTarget::Prim) {
                        Some((_, FieldValue::Value(Value::Bool(b)))) if entry.key == "active" => {
                            spec.active = Some(b);
                        }
                        Some((_, FieldValue::Value(Value::Bool(b)))) => spec.instanceable = Some(b),
                        Some((key, value)) => set_field_vec(&mut spec.fields, key, value),
                        None => {}
                    }
                }
                ast::PrimMeta::Custom(entry) if entry.key == "typeName" => {
                    let token = MetadataType::Value(ValueType::Token);
                    match crate::metadata::typed(
                        &entry.value,
                        entry.op,
                        token,
                        self.tokens,
                        &mut |_| Value::Null,
                    ) {
                        Ok(FieldValue::Value(Value::Token(name))) => spec.type_name = Some(name),
                        Ok(_) => {}
                        Err(error) => {
                            let rejection = MetadataRejection {
                                field: "typeName".into(),
                                target: MetadataTarget::Prim,
                                value_type: Some(token),
                                error,
                            };
                            self.diagnostics.push(
                                Diagnostic::error(entry.span, format!("{rejection}"))
                                    .with_metadata(rejection),
                            );
                            self.rejected = true;
                        }
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
                    FieldValue::Value(Value::String(Arc::from(&**doc))),
                );
            }
            ast::PrimMeta::Custom(entry) => {
                self.emit_metadata_entry(entry, fields, MetadataTarget::Prim);
            }
            ast::PrimMeta::References(_)
            | ast::PrimMeta::Inherits(_)
            | ast::PrimMeta::Specializes(_)
            | ast::PrimMeta::Payload(_)
            | ast::PrimMeta::Variants(_)
            | ast::PrimMeta::VariantSets(_) => {}
        }
    }

    /// Emits one generic metadata statement into `fields`, those of a
    /// spec of `target` (a variant's as a prim's, see
    /// [`crate::metadata`]).
    ///
    /// A field registered for `target` — built in or in the registry,
    /// alike ([`MetadataField::lookup`]) — reads its value as its type, so
    /// `interpolation = "vertex"` is a token and `metersPerUnit = 1` a
    /// double, and a literal it cannot hold rejects the layer
    /// ([`MetadataRejection`]); so does a key that is a field of the spec
    /// but not metadata. Any other key keeps its literal's own type, as
    /// OpenUSD keeps such a field's text. Each list-op statement of a
    /// field sets its own list of the field's list op ([`set_statement`]).
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields), §12.2.6 (list ops).
    /// OpenUSD: `_KeyValueMetadataStart` and `_KeyValueMetadataEnd`
    /// (`pxr/usd/sdf/textParserHelpers.cpp`).
    fn emit_metadata_entry(
        &mut self,
        entry: &ast::MetadataEntry<'_>,
        fields: &mut Vec<FieldEntry>,
        target: MetadataTarget,
    ) {
        let Some((key, value)) = self.metadata_entry(entry, target) else {
            return;
        };
        if value.is_list_op() {
            self.check_empty_edit(
                entry.span,
                entry.key,
                entry.op,
                field_list_op_is_empty(&value),
            );
        }
        match get_field_mut(fields, &key) {
            Some(existing) if value.is_list_op() => {
                if !set_field_list_op(existing, entry.op, value) {
                    self.diagnostics.push(Diagnostic::error(
                        entry.span,
                        format!(
                            "`{}` list op statements disagree on type; ignored",
                            entry.key
                        ),
                    ));
                }
            }
            Some(existing) => *existing = value,
            None => set_field_vec(fields, key, value),
        }
    }

    /// The field and value of one generic metadata statement on a spec of
    /// `target` (see [`Self::emit_metadata_entry`]); `None` when it gives
    /// none, with the reason reported.
    fn metadata_entry(
        &mut self,
        entry: &ast::MetadataEntry<'_>,
        target: MetadataTarget,
    ) -> Option<(TokenId, FieldValue)> {
        let key_name = match entry.key {
            // `doc` is the USDA spelling of the `documentation` field.
            "doc" => "documentation",
            key => key,
        };
        let key = self.tokens.intern(key_name);
        let reject = |ctx: &mut Self, value_type, error| {
            let rejection = MetadataRejection {
                field: key_name.into(),
                target,
                value_type,
                error,
            };
            ctx.diagnostics.push(
                Diagnostic::error(entry.span, format!("{rejection}")).with_metadata(rejection),
            );
            ctx.rejected = true;
        };
        // Two fields USDA spells with an identifier of their own, not a
        // value: `symmetryFunction`, any identifier or none at all, and an
        // attribute's `displayUnit`, the name of one of OpenUSD's units.
        // Both are kept as tokens. OpenUSD: `SymmetryFunctionMetadata` and
        // `DisplayUnitMetadata` (`pxr/usd/sdf/textFileFormatParser.h`),
        // `SdfGetUnitFromName` (`pxr/usd/sdf/types.cpp`).
        if key_name == "symmetryFunction"
            || (key_name == "displayUnit" && target == MetadataTarget::Attribute)
        {
            let name = match &entry.value {
                ast::MetadataValue::Value(ast::Value::Identifier(name))
                    if key_name == "symmetryFunction" || DISPLAY_UNITS.contains(name) =>
                {
                    *name
                }
                ast::MetadataValue::None if key_name == "symmetryFunction" => "",
                _ => {
                    let error = MetadataValueError::Unconvertible { element: None };
                    reject(self, Some(MetadataType::Value(ValueType::Token)), error);
                    return None;
                }
            };
            return Some((
                key,
                FieldValue::Value(Value::Token(self.tokens.intern(name))),
            ));
        }
        // A bare string is the `comment` field (`lower`).
        if key_name != "comment" && layerstack::is_spec_field(key_name, target) {
            reject(self, None, MetadataValueError::NotMetadata);
            return None;
        }
        let field = MetadataField::lookup(key_name, self.registry, self.tokens)
            .filter(|field| field.applies_to(target));
        if let Some(field) = field {
            let value_type = field.value_type();
            let tokens = self.tokens;
            let converted =
                crate::metadata::typed(&entry.value, entry.op, value_type, tokens, &mut |value| {
                    self.convert_metadata_value(value, "dictionary")
                });
            return match converted {
                // OpenUSD's parser sets no list of a registered field that
                // names an item twice, and reports nothing: the statement is
                // ignored (`_SetItemsIfListOpWithError`,
                // `pxr/usd/sdf/textParserHelpers.cpp`). It is ignored here
                // too, with a warning.
                Ok(value) if field_list_op_repeats(&value) => {
                    self.diagnostics.push(Diagnostic::warning(
                        entry.span,
                        format!("`{key_name}` list op statement names an item twice; ignored"),
                    ));
                    None
                }
                Ok(value) => Some((key, value)),
                Err(error) => {
                    reject(self, Some(value_type), error);
                    None
                }
            };
        }
        if entry.op != ast::ListOpKind::Explicit {
            // A list-op statement of a field no registration types: OpenUSD
            // keeps the text its parser records for the statement's list,
            // brackets dropped, as the one item of that list of an
            // `SdfUnregisteredValueListOp`; `None` sets no items
            // (`_KeyValueMetadataEnd`, `pxr/usd/sdf/textParserHelpers.cpp`).
            let items: Vec<Arc<str>> = match &entry.value {
                ast::MetadataValue::None => Vec::new(),
                ast::MetadataValue::Value(ast::Value::Array(items)) if items.is_empty() => {
                    // OpenUSD's grammar takes no `[]` here.
                    self.check_empty_edit(entry.span, entry.key, entry.op, true);
                    return None;
                }
                ast::MetadataValue::Value(ast::Value::Array(items)) => {
                    let Some(text) = recorded_items_text(items) else {
                        reject(
                            self,
                            None,
                            MetadataValueError::Unconvertible { element: None },
                        );
                        return None;
                    };
                    alloc::vec![Arc::from(text)]
                }
                ast::MetadataValue::Value(value) => {
                    let Some(text) = recorded_text(value) else {
                        reject(
                            self,
                            None,
                            MetadataValueError::Unconvertible { element: None },
                        );
                        return None;
                    };
                    alloc::vec![Arc::from(text)]
                }
                ast::MetadataValue::String(text) => {
                    let text = recorded_text(&ast::Value::String(text.as_str().into()))?;
                    alloc::vec![Arc::from(text)]
                }
                ast::MetadataValue::Dictionary(_) => {
                    reject(
                        self,
                        None,
                        MetadataValueError::Unconvertible { element: None },
                    );
                    return None;
                }
            };
            let mut list = ListOp::default();
            match entry.op {
                ast::ListOpKind::Explicit => list.explicit = Some(items),
                ast::ListOpKind::Prepend => list.prepend = items,
                ast::ListOpKind::Append => list.append = items,
                ast::ListOpKind::Add => list.add = items,
                ast::ListOpKind::Delete => list.delete = items,
                ast::ListOpKind::Reorder => list.reorder = items,
            }
            return Some((key, FieldValue::UnregisteredListOp(Box::new(list))));
        }
        // OpenUSD keeps such a field's text; a number array mixing
        // integers and reals is kept as reals, so it writes back as read.
        let numbers = |items: &[ast::Value<'_>]| {
            items.iter().all(|item| {
                matches!(
                    item,
                    ast::Value::Int(_) | ast::Value::UInt(_) | ast::Value::Number(_)
                )
            }) && items
                .iter()
                .any(|item| matches!(item, ast::Value::Number(_)))
        };
        let hint = match &entry.value {
            ast::MetadataValue::Value(ast::Value::Array(items)) if numbers(items) => "double[]",
            _ => "",
        };
        // `None` records the text `None` (`TextParserAction<KeywordNone>`,
        // `pxr/usd/sdf/textFileFormatParser.cpp`), written back as it is;
        // it blocks nothing.
        if matches!(entry.value, ast::MetadataValue::None) {
            return Some((
                key,
                FieldValue::Value(Value::Unregistered(Arc::from("None"))),
            ));
        }
        // Integers of which some only `uint64` holds and some only `int64`
        // (or `uint64` beside `int64`-only ones) have no one type here; the
        // text OpenUSD records is kept as it is (`[0, 18446744073709551615]`).
        if let ast::MetadataValue::Value(array @ ast::Value::Array(items)) = &entry.value
            && items.iter().any(|item| matches!(item, ast::Value::UInt(_)))
            && items.iter().any(|item| matches!(item, ast::Value::Int(_)))
            && let Some(text) = recorded_text(array)
        {
            return Some((key, FieldValue::Value(Value::Unregistered(Arc::from(text)))));
        }
        let value = self.checked(entry.span, |ctx| {
            ctx.convert_metadata_value(&entry.value, hint)
        })?;
        // Here only an identifier reads as a token (`foo`; a quoted string
        // is a string), and OpenUSD keeps its text, which it writes back
        // unquoted (`SdfUnregisteredValue`, `_KeyValueMetadataEnd`,
        // `pxr/usd/sdf/textParserHelpers.cpp`). `permission` is the field
        // OpenUSD registers with an identifier value of its own
        // (`PermissionMetadata`, `pxr/usd/sdf/textFileFormatParser.h`).
        let value = if key_name == "permission" {
            value
        } else {
            self.unregistered_identifiers(value)
        };
        Some((key, FieldValue::Value(value)))
    }

    /// `value` with each token, an identifier read in metadata no schema
    /// registers, kept as its text ([`Value::Unregistered`]).
    fn unregistered_identifiers(&self, value: Value) -> Value {
        match value {
            Value::Token(token) => Value::Unregistered(Arc::from(self.tokens.resolve(token))),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| self.unregistered_identifiers(item))
                    .collect(),
            ),
            Value::TypedArray(TypedArray::Token(tokens)) if !tokens.is_empty() => Value::Array(
                tokens
                    .iter()
                    .map(|token| Value::Unregistered(Arc::from(self.tokens.resolve(*token))))
                    .collect(),
            ),
            value => value,
        }
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
            .map(|conn| (conn.op, self.emit_connection_listop(conn, anchor)));
        let time_samples = attr
            .time_samples
            .as_ref()
            .map(|samples| self.convert_time_samples(samples, attr.type_name, attr.span));
        let default = attr.default.as_ref().and_then(|value| {
            self.checked(attr.span, |ctx| {
                ctx.convert_attribute_value(value, attr.type_name, (attr.span.start, None))
            })
        });
        let mut spline = attr.spline.clone();
        if let Some(data) = &mut spline {
            for (time, entries) in &attr.spline_custom_data {
                if let Some(knot) = data.knots.iter_mut().find(|k| k.time == *time) {
                    knot.custom_data = entries
                        .iter()
                        .map(|e| {
                            (
                                Arc::from(&*e.key),
                                self.convert_value(&e.value, e.type_name.unwrap_or(""))
                                    .into(),
                            )
                        })
                        .collect();
                }
            }
        }
        let mut metadata = Vec::with_capacity(attr.metadata.len());
        for entry in &attr.metadata {
            self.emit_metadata_entry(entry, &mut metadata, MetadataTarget::Attribute);
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
        // Every statement of an attribute declares its type and
        // variability; OpenUSD refuses a statement that changes either
        // ("attribute 'a' already has type ..., cannot change to ...",
        // `_PrimInitAttribute`, `pxr/usd/sdf/textParserHelpers.cpp`).
        let variability = if attr.uniform {
            Variability::Uniform
        } else {
            Variability::Varying
        };
        if let Some(existing) = &spec.type_name {
            if existing.type_name() != property_type.type_name() {
                self.reject(
                    attr.span,
                    format!(
                        "attribute '{}' already has type '{}', cannot change to '{}'",
                        attr.name,
                        existing.type_name(),
                        property_type.type_name()
                    ),
                );
                return;
            }
            if spec.variability != variability {
                self.reject(
                    attr.span,
                    format!(
                        "attribute '{}' already has variability '{:?}', cannot change to '{:?}'",
                        attr.name, spec.variability, variability
                    ),
                );
                return;
            }
        }
        spec.type_name = Some(property_type);
        // `custom` restated by any statement holds for the whole spec
        // (Core §7.6.3.1.1); the variability is the one every statement
        // declares (§7.6.4.1.2).
        spec.custom |= attr.custom;
        spec.variability = variability;
        set_fields(&mut spec.metadata, metadata);
        if let Some((kind, listop)) = connection {
            match spec.targets.as_mut() {
                Some(existing) => set_statement(existing, kind, listop),
                None => spec.targets = Some(Box::new(listop)),
            }
        }
        if let Some(samples) = time_samples {
            *spec = core::mem::take(spec).with_time_samples(samples);
        }
        if let Some(spline) = spline {
            spec.spline = Some(Box::new(*spline));
        }
        if let Some(value) = default {
            spec.default = Some(value);
        }
    }

    /// Merges one relationship statement into the property list.
    ///
    /// Spec: AOUSD Core §7.6.5 (relationship spec fields).
    /// Emits a relationship of the prim at `anchor`, whose spec sits in the
    /// variant branches `sites`.
    fn emit_relationship(
        &mut self,
        rel: &ast::Relationship<'_>,
        properties: &mut Vec<PropertyEntry>,
        anchor: &str,
        sites: &[VariantSelectionSite],
    ) {
        let name_tok = self.tokens.intern(rel.name);
        let listop = rel.targets.as_ref().map(|targets| {
            // Spec: `SdfSchema::IsValidRelationshipTargetPath`.
            let targets: Vec<Cow<'_, str>> = targets
                .iter()
                .copied()
                .filter_map(|target| {
                    // `<>` names no target; OpenUSD refuses it before it
                    // anchors a relative path.
                    if target.is_empty() {
                        self.reject(
                            rel.span,
                            String::from(
                                "Relationship target paths must be absolute prim, property or \
                                 mapper paths: <>",
                            ),
                        );
                        return None;
                    }
                    let absolute = (!has_prim_variant_selection(target))
                        .then(|| self.variant_anchored(target, anchor, sites))
                        .flatten()
                        .map(|absolute| anchor_relative_brackets(target, absolute));
                    if absolute.is_none() {
                        self.reject(
                            rel.span,
                            format!(
                                "Relationship target paths cannot contain variant \
                                 selections: <{target}>"
                            ),
                        );
                    }
                    absolute
                })
                .collect();
            let targets: Vec<&str> = targets.iter().map(|target| &**target).collect();
            let target_paths = self.target_paths(
                &targets,
                anchor,
                rel.span,
                "Relationship target paths must be absolute prim, property or mapper paths",
                true,
            );
            self.check_empty_edit(rel.span, "targetPaths", rel.op, target_paths.is_empty());
            self.check_duplicates(rel.span, "targetPaths", &target_paths);
            let mut listop = ListOp::default();
            match rel.op {
                ast::ListOpKind::Explicit => listop.explicit = Some(target_paths),
                ast::ListOpKind::Prepend => listop.prepend = target_paths,
                ast::ListOpKind::Append => listop.append = target_paths,
                ast::ListOpKind::Add => listop.add = target_paths,
                ast::ListOpKind::Delete => listop.delete = target_paths,
                ast::ListOpKind::Reorder => listop.reorder = target_paths,
            }
            listop
        });

        let mut metadata = Vec::with_capacity(rel.metadata.len());
        for entry in &rel.metadata {
            self.emit_metadata_entry(entry, &mut metadata, MetadataTarget::Relationship);
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
        // A relationship is uniform unless its statement says `varying`;
        // each statement sets it, so the last one's holds (AOUSD Core
        // §7.6.5; OpenUSD `TextParserAction<KeywordRel>`).
        spec.variability = if rel.varying {
            Variability::Varying
        } else {
            Variability::Uniform
        };
        set_fields(&mut spec.metadata, metadata);
        if let Some(listop) = listop {
            match spec.targets.as_mut() {
                Some(existing) => set_statement(existing, rel.op, listop),
                None => spec.targets = Some(Box::new(listop)),
            }
        }
        // `.default = <path>` sets the relationship's `default` to the path
        // as written, a relative one unanchored
        // (`TextParserAction<PathRef>`, `pxr/usd/sdf/textFileFormatParser.cpp`).
        if let Some(path) = rel.default {
            spec.default = Some(Value::PathText(Arc::from(path)));
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
        span: Span,
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

    /// Emits the variant set `vs` of the prim at `prim_path` (interned as
    /// `prim_path_id`) into `sets` and `order`: those of the prim spec, or
    /// for a set nested in a branch, those of that branch's variant spec.
    /// `enclosing` names the branches enclosing the set: the prim spec's
    /// own [`PrimSpec::outer_variant_sites`], then the branches of this
    /// prim it is nested in.
    ///
    /// Each branch is a variant spec of its own (`/P{a=x}`, `/P{a=x}{b=y}`),
    /// so a set nested in two branches, or reusing an enclosing set's name,
    /// is a set of its own under each; only a set authored twice in one
    /// place merges. The prim specs of the children a branch introduces
    /// record every enclosing branch.
    ///
    /// Spec: AOUSD Core §7.3.6 (prim and variant specs may contain variant
    /// set specs), §7.6.7 (variant specs). OpenUSD:
    /// `SdfVariantSetSpec` and `SdfVariantSpec` (`pxr/usd/sdf/variantSetSpec.h`,
    /// `pxr/usd/sdf/variantSpec.h`).
    #[allow(
        clippy::too_many_arguments,
        reason = "the destination sets, their order and the branch context are separate borrows"
    )]
    fn emit_variant_set(
        &mut self,
        vs: &ast::VariantSet<'_>,
        prim_path_id: PathId,
        prim_path: &mut String,
        enclosing: &[VariantSelectionSite],
        sets: &mut VariantSets,
        layer: &mut Layer,
    ) {
        // A variant named "" has the set's own path (`/P{v=}`): OpenUSD's
        // text parser creates that variant's spec there, then finds the
        // set's path taken and so neither creates nor lists the set
        // (`TextParserAction<VariantSetStatement>`,
        // `pxr/usd/sdf/textFileFormatParser.cpp`). The layer it reads holds
        // nothing of the statement, so it is left out here, with a warning.
        if vs.branches.iter().any(|branch| branch.name.is_empty()) {
            self.diagnostics.push(Diagnostic::warning(
                vs.span,
                format!(
                    "variantSet \"{}\" names a variant \"\", whose path is the set's; \
                     the statement is ignored",
                    vs.name
                ),
            ));
            return;
        }
        // The set spec alone: `variantSetNames` lists the sets that compose
        // (AOUSD Core §7.6.2.3.5), and a `variantSet` block does not list
        // its set, as OpenUSD's text parser does not.
        let set_tok = self.tokens.intern(&vs.name);
        // A set's variants keep the order their blocks first introduce them
        // (OpenUSD's `variantChildren`); a block naming a variant again
        // adds to it in place.
        let set_spec = sets.get_or_insert_default(set_tok);
        set_spec.variants.reserve(vs.branches.len());
        // The branch sites of each branch in turn: the enclosing ones, then
        // the branch's own.
        let mut branch_context = Vec::with_capacity(enclosing.len() + 1);

        for branch in &vs.branches {
            let branch_tok = self.tokens.intern(&branch.name);
            let variant_spec = set_spec.variants.get_or_insert_default(branch_tok);
            branch_context.clear();
            branch_context.extend_from_slice(enclosing);
            branch_context.push(self.variant_site(prim_path_id, set_tok, branch_tok));

            // Process branch metadata (arcs on the branch itself).
            self.emit_variant_branch_metadata(
                &branch.metadata,
                prim_path,
                &branch_context,
                variant_spec,
            );

            // Process branch children.
            for child in &branch.children {
                match child {
                    ast::PrimChild::Attribute(attr) => {
                        self.emit_attribute(attr, &mut variant_spec.properties, prim_path);
                    }
                    ast::PrimChild::Relationship(rel) => {
                        self.emit_relationship(
                            rel,
                            &mut variant_spec.properties,
                            prim_path,
                            &branch_context,
                        );
                    }
                    ast::PrimChild::Prim(child_prim) => {
                        if !self.admits_prim_name(child_prim) {
                            continue;
                        }
                        let child_tok = self.tokens.intern(&child_prim.name);
                        if !variant_spec.authored_children.contains(&child_tok) {
                            variant_spec.authored_children.push(child_tok);
                        }
                        // The branch owns the child's prim spec (`/P{v=x}C`),
                        // which `Layer::insert_prim` keeps apart from the
                        // specs at the same path outside this branch.
                        //
                        // Spec: AOUSD Core §7.3.6 (variant specs contain prim
                        // specs).
                        let place = PrimPlace {
                            parent: prim_path_id,
                            name: child_tok,
                        };
                        self.emit_prim(child_prim, place, prim_path, &branch_context, layer);
                    }
                    ast::PrimChild::VariantSet(nested_vs) => {
                        self.emit_variant_set(
                            nested_vs,
                            prim_path_id,
                            prim_path,
                            &branch_context,
                            &mut variant_spec.variant_sets,
                            layer,
                        );
                    }
                    ast::PrimChild::ReorderProperties(names) => {
                        variant_spec.property_order =
                            Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                    }
                    // OpenUSD keeps the order on the branch's prim spec,
                    // as on any prim spec (`primOrder`,
                    // `TextParserAction<ChildOrPropertyOrderStatement>`,
                    // `pxr/usd/sdf/textFileFormatParser.cpp`).
                    ast::PrimChild::ReorderNameChildren(names) => {
                        variant_spec.prim_order =
                            Some(names.iter().map(|n| self.tokens.intern(n)).collect());
                    }
                }
            }
        }
        // Blocks naming a variant twice leave room the set does not use.
        set_spec.variants.shrink_to_fit();
    }

    /// Emit metadata arcs on a variant branch header into a [`VariantSpec`].
    fn emit_variant_branch_metadata(
        &mut self,
        metadata: &[ast::PrimMeta<'_>],
        prim_path: &str,
        branch_sites: &[VariantSelectionSite],
        variant_spec: &mut VariantSpec,
    ) {
        for meta in metadata {
            match meta {
                ast::PrimMeta::References(arc) => {
                    self.emitted_arcs = true;
                    let statement = self.emit_arc_listop(arc, "Reference", true);
                    set_statement(variant_spec.references.make_mut(), arc.kind, statement);
                }
                ast::PrimMeta::Payload(arc) => {
                    self.emitted_arcs = true;
                    let statement = self.emit_arc_listop(arc, "Payload", true);
                    set_statement(variant_spec.payloads.make_mut(), arc.kind, statement);
                }
                ast::PrimMeta::Inherits(paths) => {
                    let statement =
                        self.emit_path_listop(paths, prim_path, branch_sites, "Inherit");
                    set_statement(variant_spec.inherits.make_mut(), paths.kind, statement);
                }
                ast::PrimMeta::Specializes(paths) => {
                    let statement =
                        self.emit_path_listop(paths, prim_path, branch_sites, "Specializes");
                    set_statement(variant_spec.specializes.make_mut(), paths.kind, statement);
                }
                ast::PrimMeta::Variants(selections) => {
                    for sel in selections {
                        let set_tok = self.tokens.intern(&sel.set_name);
                        let branch_tok = self.tokens.intern(&sel.branch_name);
                        variant_spec.variant_selections.insert(set_tok, branch_tok);
                    }
                }
                ast::PrimMeta::Kind(_) | ast::PrimMeta::Doc(_) | ast::PrimMeta::Custom(_) => {
                    // Spec: AOUSD Core §7.6.7 (variant specs contribute prim
                    // spec fields).
                    self.emit_plain_prim_meta(meta, &mut variant_spec.fields);
                }
                ast::PrimMeta::VariantSets(listop) => {
                    // The sets nested in this branch, as for a prim spec.
                    let names = self.emit_name_listop(listop);
                    declare_listed_sets(&names, &mut variant_spec.variant_sets);
                    set_statement(
                        variant_spec.variant_set_names.make_mut(),
                        listop.kind,
                        names,
                    );
                }
            }
        }
    }

    // ── Composition arcs ────────────────────────────────────────────

    /// Converts a reference or payload list; `arc` names the arc in
    /// OpenUSD's messages (`Reference` or `Payload`), and `in_branch` tells
    /// whether the list is authored inside a variant branch.
    fn emit_arc_listop(
        &mut self,
        arc_list: &ast::ListOpArc<'_>,
        arc: &str,
        in_branch: bool,
    ) -> ListOp<Reference> {
        let field = if arc == "Payload" {
            "payload"
        } else {
            "references"
        };
        let empty = arc_list.items.as_ref().is_none_or(Vec::is_empty);
        self.check_empty_edit(arc_list.span, field, arc_list.kind, empty);
        let Some(items) = &arc_list.items else {
            // `= None` clears the arc list.
            return ListOp::explicit(Vec::new());
        };

        let mut refs = Vec::with_capacity(items.len());
        refs.extend(
            items
                .iter()
                .filter_map(|r| self.emit_arc_ref(r, arc, in_branch)),
        );
        self.check_duplicates(arc_list.span, field, &refs);

        let mut listop = ListOp::default();
        match arc_list.kind {
            ast::ListOpKind::Explicit => listop.explicit = Some(refs),
            ast::ListOpKind::Prepend => listop.prepend = refs,
            ast::ListOpKind::Append => listop.append = refs,
            ast::ListOpKind::Add => listop.add = refs,
            ast::ListOpKind::Delete => listop.delete = refs,
            ast::ListOpKind::Reorder => listop.reorder = refs,
        }
        listop
    }

    fn emit_arc_ref(
        &mut self,
        arc_ref: &ast::ArcRef<'_>,
        arc: &str,
        in_branch: bool,
    ) -> Option<Reference> {
        let custom_data = arc_ref
            .custom_data
            .iter()
            .map(|e| {
                (
                    Arc::from(&*e.key),
                    FieldValue::Value(self.convert_value(&e.value, e.type_name.unwrap_or(""))),
                )
            })
            .collect::<Vec<_>>();
        // Spec: `SdfSchema::IsValidReference` and `IsValidPayload`.
        if let Some(path) = arc_ref.prim_path
            && has_variant_selection(path)
        {
            self.reject(
                arc_ref.span,
                format!("{arc} paths cannot contain variant selections: <{path}>"),
            );
            return None;
        }
        // An omitted prim path, and the empty path `<>`, target the layer's
        // `defaultPrim`. OpenUSD warns that `<>` is ill-formed, reads it as
        // the empty path and composes it as an omitted target.
        //
        // Spec: AOUSD Core §10.3.2.1 (references with no prim path).
        //
        // Any other path must be an absolute prim path: OpenUSD refuses a
        // relative one (`Reference prim path <../B> must be either empty or
        // an absolute prim path`) and the root or a property path
        // (`SdfSchema::IsValidReference`).
        let target = match arc_ref.prim_path {
            Some(path_str) if !path_str.is_empty() => {
                if !path_str.starts_with('/') {
                    self.reject(
                        arc_ref.span,
                        format!(
                            "{arc} prim path <{path_str}> must be either empty or an absolute \
                             prim path"
                        ),
                    );
                    return None;
                }
                if !is_absolute_prim_path(path_str) {
                    self.reject(
                        arc_ref.span,
                        format!("'{path_str}' is not a valid prim path"),
                    );
                    return None;
                }
                ReferenceTarget::Prim(self.intern_prim_path(path_str)?)
            }
            _ => ReferenceTarget::DefaultPrim,
        };

        let layer_offset = LayerOffset {
            offset: arc_ref.offset.unwrap_or(0.0),
            scale: arc_ref.scale.unwrap_or(1.0),
        };

        // Without an asset path the arc is internal. An asset path that
        // cannot be resolved keeps the arc unresolved, so composition
        // reports it; it never becomes an internal arc.
        //
        // Spec: AOUSD Core §10.3.2.1 (a reference whose layer stack cannot
        // be computed is a composition error and is ignored).
        let Some(asset) = arc_ref.asset.as_deref() else {
            let internal = ReferenceSource::Layer {
                layer: self.layer_id,
                asset: None,
            };
            return Some(
                Reference::from_source(internal, target)
                    .with_layer_offset(layer_offset)
                    .with_custom_data(custom_data),
            );
        };
        // An asset path that is a variable expression is evaluated during
        // composition, relative to this layer.
        if is_expression(asset) {
            return Some(
                Reference::expression(self.layer_id, asset, target, layer_offset)
                    .with_custom_data(custom_data),
            );
        }
        // A payload to a dynamic file format targets the layer its composed
        // arguments name, which composition asks for
        // (`AssetResolver::defers_payload`). An arc inside a variant branch
        // may never be composed; resolving the branches the layer selects
        // is left until the whole layer is read
        // (`resolve_selected_deferred_arcs`), the rest to composition.
        if (in_branch && self.defer_variant_arcs)
            || (arc == "Payload" && self.resolver.defers_payload(asset))
        {
            return Some(
                Reference::deferred(self.layer_id, asset, target, layer_offset)
                    .with_custom_data(custom_data),
            );
        }
        let resolved = match self.resolve_arc_asset(asset) {
            Ok(resolved) => resolved,
            // Not supplied yet: composition asks for it again.
            Err(AssetResolveError::Unavailable) => {
                return Some(
                    Reference::deferred(self.layer_id, asset, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            }
            Err(_) => {
                return Some(
                    Reference::unresolved(asset, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            }
        };
        if let Some(layer) = resolved.layer {
            self.resolved_layers.push(layer);
        }
        let source = ReferenceSource::Layer {
            layer: resolved.layer_id,
            asset: Some(asset),
        };
        Some(
            Reference::from_source(source, target)
                .with_layer_offset(layer_offset)
                .with_custom_data(custom_data),
        )
    }

    /// Converts a `variantSets` list op, as authored. The parser reports
    /// `None` and `[]`, which OpenUSD rejects; read past that report, they
    /// give an empty list.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
    fn emit_name_listop(&mut self, names: &ast::ListOp<Cow<'_, str>>) -> ListOp<TokenId> {
        let Some(items) = &names.items else {
            return ListOp::explicit(Vec::new());
        };
        let items: Vec<TokenId> = items.iter().map(|name| self.tokens.intern(name)).collect();
        self.check_duplicates(names.span, "variantSetNames", &items);
        let mut listop = ListOp::default();
        match names.kind {
            ast::ListOpKind::Explicit => listop.explicit = Some(items),
            ast::ListOpKind::Prepend => listop.prepend = items,
            ast::ListOpKind::Append => listop.append = items,
            ast::ListOpKind::Add => listop.add = items,
            ast::ListOpKind::Delete => listop.delete = items,
            ast::ListOpKind::Reorder => listop.reorder = items,
        }
        listop
    }

    /// Converts an inherits or specializes list; relative paths resolve
    /// against the prim path `anchor`, whose spec sits in the variant
    /// branches `sites`. `arc` names the arc in OpenUSD's messages
    /// (`Inherit` or `Specializes`).
    ///
    /// Spec: AOUSD Core §8 (paths; relative paths are anchored to the prim
    /// that authors them).
    fn emit_path_listop(
        &mut self,
        paths: &ast::ListOpPaths<'_>,
        anchor: &str,
        sites: &[VariantSelectionSite],
        arc: &str,
    ) -> ListOp<PathId> {
        let field = if arc == "Inherit" {
            "inheritPaths"
        } else {
            "specializes"
        };
        let empty = paths.items.as_ref().is_none_or(Vec::is_empty);
        self.check_empty_edit(paths.span, field, paths.kind, empty);
        let Some(items) = &paths.items else {
            return ListOp::explicit(Vec::new());
        };

        let mut path_ids = Vec::with_capacity(items.len());
        path_ids.extend(items.iter().filter_map(|s| {
            // A relative path may not climb above the root: OpenUSD's
            // `MakeAbsolutePath` gives no path for it.
            if self.climbs_above_root(s, anchor, sites) {
                self.reject(
                    paths.span,
                    format!("{arc} paths must be absolute prim paths: <{s}>"),
                );
                return None;
            }
            // Spec: `SdfSchema::IsValidInheritPath` and
            // `IsValidSpecializesPath`.
            let Some(absolute) = (!has_variant_selection(s))
                .then(|| self.variant_anchored(s, anchor, sites))
                .flatten()
            else {
                self.reject(
                    paths.span,
                    format!("{arc} paths cannot contain variant selections: <{s}>"),
                );
                return None;
            };
            // The path must name a prim: OpenUSD refuses the empty path, the
            // root and a property path as authored (`'/A.b' is not a valid
            // prim path`), and a relative path that anchors at the root.
            if s.is_empty() || !is_absolute_prim_path(&absolute) {
                let message = if s.starts_with('/') || s.is_empty() {
                    format!("'{s}' is not a valid prim path")
                } else {
                    format!("{arc} paths must be absolute prim paths: <{s}>")
                };
                self.reject(paths.span, message);
                return None;
            }
            self.intern_prim_path(&absolute)
        }));
        self.check_duplicates(paths.span, field, &path_ids);

        let mut listop = ListOp::default();
        match paths.kind {
            ast::ListOpKind::Explicit => listop.explicit = Some(path_ids),
            ast::ListOpKind::Prepend => listop.prepend = path_ids,
            ast::ListOpKind::Append => listop.append = path_ids,
            ast::ListOpKind::Add => listop.add = path_ids,
            ast::ListOpKind::Delete => listop.delete = path_ids,
            ast::ListOpKind::Reorder => listop.reorder = path_ids,
        }
        listop
    }

    /// Converts one `relocates` entry of the layer metadata. Paths are
    /// anchored at the pseudo-root; an empty target (`<>`) removes the
    /// source. An entry whose paths are not prim paths (a variant selection,
    /// a property) is reported and dropped, as OpenUSD rejects it when
    /// reading the layer.
    ///
    /// The entries are not validated against each other here: composition
    /// does that per layer stack.
    ///
    /// Spec: AOUSD Core §16.2.18.5 (layer relocates metadata), §7.6.1.2.4.
    fn emit_relocate(&mut self, entry: &ast::RelocateEntry<'_>) -> Option<layerstack::Relocate> {
        let prim_path = |text: &str| {
            let absolute = absolute_path(text, "/");
            let segments_are_names = absolute.split('/').skip(1).all(is_prim_name);
            match Path::parse_absolute(&absolute, self.tokens) {
                Ok(path) if segments_are_names && path.depth() > 0 => Some(self.paths.intern(path)),
                _ => None,
            }
        };
        let source = prim_path(entry.source);
        let target = if entry.target.is_empty() {
            Some(None)
        } else {
            prim_path(entry.target).map(Some)
        };
        let (Some(source), Some(target)) = (source, target) else {
            self.diagnostics.push(Diagnostic::error(
                entry.span,
                format!(
                    "relocates `<{}>: <{}>` is not a relocate between prim paths; ignored",
                    entry.source, entry.target
                ),
            ));
            return None;
        };
        Some(layerstack::Relocate { source, target })
    }

    fn emit_connection_listop(
        &mut self,
        conn: &ast::Connection<'_>,
        anchor: &str,
    ) -> ListOp<TargetPath> {
        // A relative connection is anchored without the variant selections
        // of the prim that authors it, as OpenUSD strips them; one written
        // with a variant selection is rejected (`Sdf_TextFileFormatParser`,
        // `PathRef`).
        let targets: Vec<&str> = conn
            .targets
            .iter()
            .copied()
            .filter(|target| {
                let invalid = has_prim_variant_selection(target);
                if invalid {
                    self.reject(
                        conn.span,
                        format!("'{target}' is not a valid prim or property scene path"),
                    );
                }
                !invalid
            })
            .collect();
        let target_paths = self.target_paths(
            &targets,
            anchor,
            conn.span,
            "Connection paths must be absolute prim or property paths",
            false,
        );
        self.check_empty_edit(
            conn.span,
            "connectionPaths",
            conn.op,
            conn.targets.is_empty(),
        );
        self.check_duplicates(conn.span, "connectionPaths", &target_paths);

        let mut listop = ListOp::default();
        match conn.op {
            ast::ListOpKind::Explicit => listop.explicit = Some(target_paths),
            ast::ListOpKind::Prepend => listop.prepend = target_paths,
            ast::ListOpKind::Append => listop.append = target_paths,
            ast::ListOpKind::Add => listop.add = target_paths,
            ast::ListOpKind::Delete => listop.delete = target_paths,
            ast::ListOpKind::Reorder => listop.reorder = target_paths,
        }
        listop
    }

    /// Reports a list-edit statement that sets no items, `None` or `[]`,
    /// which OpenUSD allows only for an explicit list ("Setting ... to None
    /// (or an empty list) is only allowed when setting explicit ...",
    /// `pxr/usd/sdf/textFileFormatParser.cpp`).
    ///
    /// Spec: AOUSD Core §12.4 (list ops).
    fn check_empty_edit(&mut self, span: Span, field: &str, kind: ast::ListOpKind, empty: bool) {
        if empty && kind != ast::ListOpKind::Explicit {
            self.reject(
                span,
                format!(
                    "Setting {field} to None (or an empty list) is only allowed when \
                     setting explicit {field}, not for list editing"
                ),
            );
        }
    }

    /// Reports a list-op statement that names an item twice, as OpenUSD's
    /// parser refuses it for targets, connections, arcs and `variantSets`
    /// (`_HasDuplicates` in `_SetListOpItemsWithError`,
    /// `pxr/usd/sdf/textParserHelpers.h`). List-op metadata statements are
    /// not checked: OpenUSD reads `prepend apiSchemas = ["X", "X"]`.
    fn check_duplicates<T: Eq + Hash>(&mut self, span: Span, field: &str, items: &[T]) {
        let duplicated = if items.len() <= 16 {
            items
                .iter()
                .enumerate()
                .any(|(i, item)| items[..i].contains(item))
        } else {
            let mut seen = HashSet::with_capacity(items.len());
            !items.iter().all(|item| seen.insert(item))
        };
        if duplicated {
            self.reject(span, format!("Duplicate items exist for field '{field}'"));
        }
    }

    /// Whether the relative `path`, authored on the prim at `anchor` in the
    /// variant branches `sites`, climbs above the root
    /// (`<../../B>` on `/A`).
    fn climbs_above_root(&self, path: &str, anchor: &str, sites: &[VariantSelectionSite]) -> bool {
        if path.starts_with('/') {
            return false;
        }
        let mut climbs = 0;
        let mut rest = path;
        while let Some(tail) = rest.strip_prefix("../") {
            climbs += 1;
            rest = tail;
        }
        if rest == ".." {
            climbs += 1;
        }
        climbs > self.anchor_elements(anchor, sites).len()
    }

    /// The elements of the prim path relative paths authored on the prim at
    /// `anchor`, in the variant branches `sites`, anchor at: each prim name,
    /// followed by the selections of the variant sets it hosts (`None`),
    /// without the selections the path ends with (`SdfPath::GetPrimPath`).
    fn anchor_elements<'p>(
        &self,
        anchor: &'p str,
        sites: &[VariantSelectionSite],
    ) -> Vec<Option<&'p str>> {
        let mut elements: Vec<Option<&str>> = Vec::new();
        for (level, name) in anchor.split('/').filter(|s| !s.is_empty()).enumerate() {
            elements.push(Some(name));
            let hosted = sites
                .iter()
                .filter(|site| self.paths.resolve(site.host_path).depth() == level + 1)
                .count();
            elements.extend(core::iter::repeat_n(None, hosted));
        }
        while elements.last() == Some(&None) {
            elements.pop();
        }
        elements
    }

    /// Makes `path`, authored on the prim at `anchor` whose spec sits in the
    /// variant branches `sites`, absolute, or returns `None` when the result
    /// names one of those variant selections.
    ///
    /// OpenUSD anchors a relative path at the prim path of the spec
    /// (`SdfPath::GetPrimPath`), which keeps the selections the path passes
    /// through (`/A{v=x}B`) but drops those it ends with, so a property of
    /// the variant `/A{v=x}` itself anchors at `/A`: `<C>` is `/A/C`. Each
    /// leading `..` then removes one element, a prim name or a variant
    /// selection (`SdfPath::MakeAbsolutePath`): `<../../D>` authored on
    /// `/A{v=x}C` is `/A/D`, while `<../D>` is `/A{v=x}D`.
    ///
    /// OpenUSD: `TextParserAction<PathRef>`
    /// (`pxr/usd/sdf/textFileFormatParser.cpp`).
    fn variant_anchored<'p>(
        &self,
        path: &'p str,
        anchor: &str,
        sites: &[VariantSelectionSite],
    ) -> Option<Cow<'p, str>> {
        if path.starts_with('/') || sites.is_empty() {
            return Some(absolute_path(path, anchor));
        }
        let mut elements = self.anchor_elements(anchor, sites);
        let mut rest = path;
        let mut climbs = 0;
        loop {
            if let Some(tail) = rest.strip_prefix("../") {
                climbs += 1;
                rest = tail;
            } else if rest == ".." {
                climbs += 1;
                rest = "";
            } else if let Some(tail) = rest.strip_prefix("./") {
                rest = tail;
            } else {
                break;
            }
        }
        elements.truncate(elements.len().saturating_sub(climbs));
        let names: Option<Vec<&str>> = elements.into_iter().collect();
        let mut base = String::new();
        for name in names? {
            base.push('/');
            base.push_str(name);
        }
        if base.is_empty() {
            base.push('/');
        }
        Some(Cow::Owned(absolute_path(rest, &base).into_owned()))
    }

    /// Parses relationship or connection targets; relative paths resolve
    /// against the prim path `anchor`, and unparsable ones are reported.
    ///
    /// Spec: AOUSD Core §8 (paths), §12.4 (targets).
    ///
    /// The empty path `<>` and a path that is or anchors at the root are
    /// refused with `refusal`, OpenUSD's message for the field
    /// (`SdfSchema::IsValidRelationshipTargetPath`,
    /// `IsValidAttributeConnectionPath`).
    fn target_paths(
        &mut self,
        targets: &[&str],
        anchor: &str,
        span: Span,
        refusal: &str,
        mappers: bool,
    ) -> Vec<TargetPath> {
        targets
            .iter()
            .filter_map(|target| {
                let absolute = absolute_path(target, anchor);
                if target.is_empty() || absolute == "/" {
                    self.reject(span, format!("{refusal}: <{target}>"));
                    return None;
                }
                let absolute = anchor_relative_brackets(target, absolute);
                // A relational attribute (`/A.b[/C].d`), and for a
                // relationship a mapper (`/A.b.mapper[/C]`), is a property
                // target whose property token is the whole property part
                // (`b[/C].d`): layerstack keeps and writes it, and
                // composes it as a property of its prim (`/A`), mapping
                // that prim's path and not the bracketed paths. The other
                // kinds are refused, as `IsValidRelationshipTargetPath` and
                // `IsValidAttributeConnectionPath` refuse them
                // (`pxr/usd/sdf/schema.cpp`).
                if let Some((prim, tail, ends)) = split_property_tail(&absolute) {
                    let allowed = match ends {
                        PropertyTail::RelationalAttribute => true,
                        PropertyTail::Mapper => mappers,
                        PropertyTail::Other => false,
                    };
                    let prim = Path::parse_absolute(prim, self.tokens)
                        .ok()
                        .filter(|_| allowed)
                        .map(|prim| self.paths.intern(prim));
                    let Some(prim) = prim else {
                        self.reject(span, format!("{refusal}: <{target}>"));
                        return None;
                    };
                    let property = self.tokens.intern(tail);
                    return Some(TargetPath::Property(PropertyPath::new(prim, property)));
                }
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

    /// The type an attribute declares, `type_hint` its element type name;
    /// a name the value type table lacks is kept, as OpenUSD's text reader
    /// keeps it. Attributes declaring one type share it
    /// ([`EmitCtx::property_types`]).
    fn declared_property_type(&mut self, type_hint: &str, is_array: bool) -> PropertyType {
        let base = type_hint.strip_suffix("[]").unwrap_or(type_hint);
        let declared = |name: &str| {
            if is_array {
                name.strip_suffix("[]") == Some(base)
            } else {
                name == base
            }
        };
        if let Some(shared) = self
            .property_types
            .iter()
            .find(|shared| declared(shared.type_name()))
        {
            return shared.clone();
        }
        let parsed = if is_array {
            // The array type's name, spelled where a type name fits.
            let mut name = SmallVec::<[u8; 64]>::new();
            name.extend_from_slice(base.as_bytes());
            name.extend_from_slice(b"[]");
            PropertyType::parse(core::str::from_utf8(&name).expect("a name and `[]` are UTF-8"))
        } else {
            PropertyType::parse(base)
        };
        let property_type = parsed.unwrap_or_else(PropertyType::unregistered);
        if self.property_types.len() < PROPERTY_TYPES_SHARED {
            self.property_types.push(property_type.clone());
        }
        property_type
    }

    // ── Value conversion ────────────────────────────────────────────

    /// Converts authored time samples, keeping blocked samples.
    ///
    /// A `None` sample is a value block in effect from its time until the
    /// next sample (AOUSD Core §12.3.6: individual time samples can be
    /// blocked), so it stays in the series as [`Value::Blocked`]. A sample
    /// whose value does not convert to the declared type is reported and
    /// left out.
    fn convert_time_samples(
        &mut self,
        samples: &[ast::TimeSample<'_>],
        type_hint: &str,
        span: Span,
    ) -> Vec<(f64, Value)> {
        // OpenUSD's parser reads one valued sample of a type no type
        // registers: its value context forgets the unrecognized type after
        // the first value (`Sdf_ParserValueContext`,
        // `pxr/usd/sdf/textFileFormatParser.cpp`), and a second valued
        // sample fails ("Unrecognized type name"), so the layer does not
        // open. `None` samples are not values and are read on.
        if ValueType::from_name(type_hint).is_none()
            && type_hint != "dictionary"
            && samples
                .iter()
                .filter(|s| s.value.is_some())
                .nth(1)
                .is_some()
        {
            self.diagnostics.push(Diagnostic::error(
                span,
                format!("Unrecognized type name '{type_hint}'"),
            ));
        }
        let mut converted = Vec::with_capacity(samples.len());
        converted.extend(samples.iter().enumerate().filter_map(|(index, s)| {
            let value = match &s.value {
                Some(v) => self.checked(span, |ctx| {
                    ctx.convert_attribute_value(v, type_hint, (span.start, Some(index)))
                })?,
                None => Value::Blocked,
            };
            Some((s.time, value))
        }));
        converted
    }

    /// Runs `convert`, returning its value only when every value it
    /// converted took its declared type; otherwise reports each rejection at
    /// `span` and returns `None`, so that no value of another type is
    /// imported.
    fn checked(&mut self, span: Span, convert: impl FnOnce(&mut Self) -> Value) -> Option<Value> {
        let before = self.rejections.len();
        let value = convert(self);
        if self.rejections.len() == before {
            return Some(value);
        }
        for message in self.rejections.drain(before..) {
            self.diagnostics.push(Diagnostic::error(span, message));
        }
        None
    }

    fn convert_attribute_value(
        &mut self,
        value: &ast::Value<'_>,
        type_hint: &str,
        key: (u32, Option<usize>),
    ) -> Value {
        // A value of a type no type registers is kept as the text OpenUSD's
        // parser records for it (`SdfUnregisteredValue`; the attribute's
        // `Sdf_ParserValueContext` records a string when its type name is not
        // valid, `pxr/usd/sdf/textFileFormatParser.cpp`). `dictionary` is no
        // attribute type, a keyword OpenUSD refuses there.
        if ValueType::from_name(type_hint).is_none()
            && type_hint != "dictionary"
            && let Some(text) = recorded_text(value)
        {
            return Value::Unregistered(Arc::from(text));
        }
        let Some((prepared, errors)) = self
            .arrays
            .take(crate::read::PreparedKey::new(key.0, key.1))
        else {
            return self.convert_value(value, type_hint);
        };
        self.rejections.extend(errors);
        prepared
    }

    fn convert_value(&mut self, val: &ast::Value<'_>, type_hint: &str) -> Value {
        if let Some(converted) = convert_scalar(val, type_hint) {
            return converted.unwrap_or_else(|message| {
                self.rejections.push(message);
                Value::Blocked
            });
        }
        match val {
            ast::Value::Bool(b) => Value::Bool(*b),
            // A number for a declared numeric or boolean type converted in
            // `convert_scalar`; for any other, it keeps its written type.
            ast::Value::Int(n) => Value::Int64(*n),
            ast::Value::UInt(n) => Value::UInt64(*n),
            ast::Value::Number(n) => Value::Double(*n),
            ast::Value::String(s) => match type_hint {
                "token" => Value::Token(self.tokens.intern(s)),
                "asset" => Value::Asset(Arc::from(&**s)),
                "pathExpression" => Value::PathExpression(Arc::from(&**s)),
                _ => Value::String(Arc::from(&**s)),
            },
            ast::Value::Identifier(s) => Value::Token(self.tokens.intern(s)),
            ast::Value::Asset(s) => Value::Asset(Arc::from(&**s)),
            ast::Value::Path(s) => Value::String(Arc::from(*s)),
            ast::Value::Blocked => Value::Blocked,
            ast::Value::AnimationBlock => Value::AnimationBlock,
            ast::Value::Dictionary(entries) => {
                let dict_entries: Vec<(Arc<str>, FieldValue)> = entries
                    .iter()
                    .map(|e| {
                        let key = Arc::from(&*e.key);
                        let val = self.convert_value(&e.value, e.type_name.unwrap_or(""));
                        (key, val.into())
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
                if let Some(text) = self.text_array(items, arr_elem_hint) {
                    return text;
                }
                let prototype = default_scalar_for_type(arr_elem_hint);
                let elements = items.iter().map(|v| self.convert_value(v, arr_elem_hint));
                Value::array_from_iter(elements, Some(&prototype))
            }
            ast::Value::ArrayEdit(edit) => {
                let element_hint = type_hint.strip_suffix("[]").unwrap_or(type_hint);
                let value_type = self.declared_property_type(element_hint, true);
                Value::typed_array_edit(self.convert_array_edit(edit, element_hint), value_type)
            }
        }
    }

    /// A `token[]` or `string[]` of quoted elements, built straight into
    /// its native buffer, without a value per element; `None` for another
    /// type or any other element, which converts element by element.
    ///
    /// Spec: AOUSD Core §6.2 (`token[]` and `string[]`), §16.2 (USDA).
    fn text_array(&mut self, items: &[ast::Value<'_>], element_hint: &str) -> Option<Value> {
        fn text<'v>(item: &'v ast::Value<'_>) -> Option<&'v str> {
            match item {
                ast::Value::String(text) => Some(text),
                _ => None,
            }
        }
        match element_hint {
            "token" => {
                if !items.iter().all(|item| text(item).is_some()) {
                    return None;
                }
                // Interned straight into the array's buffer, sized once.
                let ids = items
                    .iter()
                    .map(|item| self.tokens.intern(text(item).unwrap_or_default()))
                    .collect();
                Some(Value::TypedArray(TypedArray::Token(ids)))
            }
            "string" => {
                let bytes = items
                    .iter()
                    .map(|item| text(item).map_or(0, str::len))
                    .sum();
                let mut out = StringArray::with_capacity(items.len(), bytes);
                for item in items {
                    out.push(text(item)?);
                }
                Some(Value::from(out))
            }
            _ => None,
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
            ast::ArrayEditInstruction::MinSizeFill { len, fill } => ArrayEditOp::MinSizeFill {
                len: *len,
                fill: self.convert_value(fill, element_hint),
            },
            ast::ArrayEditInstruction::MaxSize(len) => ArrayEditOp::MaxSize { len: *len },
            ast::ArrayEditInstruction::Resize(len) => ArrayEditOp::Resize { len: *len },
            ast::ArrayEditInstruction::ResizeFill { len, fill } => ArrayEditOp::ResizeFill {
                len: *len,
                fill: self.convert_value(fill, element_hint),
            },
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
                let dict_entries: Vec<(Arc<str>, FieldValue)> = entries
                    .iter()
                    .map(|e| {
                        let key = Arc::from(&*e.key);
                        let val = self.convert_value(&e.value, e.type_name.unwrap_or(""));
                        (key, val.into())
                    })
                    .collect();
                Value::Dictionary(dict_entries)
            }
            ast::MetadataValue::String(s) => {
                self.convert_value(&ast::Value::String(s.as_str().into()), type_hint)
            }
        }
    }

    // ── Asset resolution helper ─────────────────────────────────────

    /// Resolves `asset_path` relative to this layer. A variable expression
    /// is not resolved here: composition evaluates it with the variables of
    /// the layer stack that authors it (see
    /// [`layerstack::variable_expression`]).
    fn resolve_asset(&mut self, asset_path: &str) -> Option<ResolvedAsset> {
        if is_expression(asset_path) {
            return None;
        }
        self.resolve_arc_asset(asset_path).ok()
    }

    /// Resolves the asset path of a reference or payload relative to this
    /// layer, keeping why it did not resolve: an asset the resolver cannot
    /// supply yet ([`AssetResolveError::Unavailable`]) leaves its arc
    /// deferred, for composition to ask for again.
    fn resolve_arc_asset(&mut self, asset_path: &str) -> Result<ResolvedAsset, AssetResolveError> {
        self.resolver
            .resolve(asset_path, Some(self.layer_id), self.tokens, self.paths)
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
        match dimensioned_value(items, type_hint, elem_hint) {
            Ok(value) => value,
            Err(message) => {
                self.rejections.push(message);
                Some(Value::Blocked)
            }
        }
    }
}

/// Sets `fields`, each named once, in `metadata`: the vector itself when it
/// has none yet.
fn set_fields(metadata: &mut SharedVec<FieldEntry>, fields: Vec<FieldEntry>) {
    if metadata.is_empty() {
        *metadata = SharedVec::from(fields);
        return;
    }
    for entry in fields {
        set_field_vec(metadata.make_mut(), entry.name, entry.value);
    }
}

/// Where a prim being emitted sits: the child `name` of the prim at
/// `parent`.
#[derive(Clone, Copy)]
struct PrimPlace {
    parent: PathId,
    name: TokenId,
}

// ── Specifier conversion ────────────────────────────────────────────────

/// The text OpenUSD's parser records for `value` when it has no type to
/// read it as: numbers as `TfStringify` writes them, strings quoted,
/// identifiers as they are, and lists and tuples item by item, `, `
/// between (`Sdf_ParserValueContext::AppendValue`, `BeginList`,
/// `BeginTuple`, `pxr/usd/sdf/parserValueContext.cpp`); `None` for a value
/// that records no text (a block, an array edit, a dictionary).
fn recorded_text(value: &ast::Value<'_>) -> Option<String> {
    let mut out = String::new();
    record_text(value, &mut out)?;
    Some(out)
}

/// [`recorded_text`] of the items of a list, without its brackets, as
/// OpenUSD keeps a list-op statement's items (`1, 2.5`).
fn recorded_items_text(items: &[ast::Value<'_>]) -> Option<String> {
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        record_text(item, &mut out)?;
    }
    Some(out)
}

/// Appends [`recorded_text`] of `value` to `out`.
fn record_text(value: &ast::Value<'_>, out: &mut String) -> Option<()> {
    use crate::writer::Value as Text;
    match value {
        ast::Value::Number(n) => out.push_str(&Text::Double(*n).to_usda_text()),
        ast::Value::Int(n) => out.push_str(&Text::Int64(*n).to_usda_text()),
        ast::Value::UInt(n) => out.push_str(&Text::UInt64(*n).to_usda_text()),
        ast::Value::String(s) => out.push_str(&Text::String(String::from(&**s)).to_usda_text()),
        ast::Value::Identifier(s) => out.push_str(s),
        ast::Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        ast::Value::Asset(s) => {
            out.push('@');
            out.push_str(s);
            out.push('@');
        }
        ast::Value::Path(s) => {
            out.push('<');
            out.push_str(s);
            out.push('>');
        }
        ast::Value::Array(items) | ast::Value::Tuple(items) => {
            let (open, close) = if matches!(value, ast::Value::Array(_)) {
                ('[', ']')
            } else {
                ('(', ')')
            };
            out.push(open);
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                record_text(item, out)?;
            }
            out.push(close);
        }
        ast::Value::ArrayEdit(_)
        | ast::Value::Dictionary(_)
        | ast::Value::AnimationBlock
        | ast::Value::Blocked => return None,
    }
    Some(())
}

/// Returns `true` when `name` is a prim name: an identifier, which may use
/// characters outside ASCII (AOUSD Core §7.3.3), and so not a variant
/// selection, property or relative path component.
fn is_prim_name(name: &str) -> bool {
    let mut chars = name.chars();
    let allowed =
        |c: char| c == '_' || c.is_ascii_alphanumeric() || (!c.is_ascii() && !c.is_whitespace());
    chars
        .next()
        .is_some_and(|first| !first.is_ascii_digit() && allowed(first))
        && chars.all(allowed)
}

/// Whether `text` is an absolute path to a prim, not the root: `/` and
/// identifiers joined by `/` (AOUSD Core §8, §16.2.9), as arcs name their
/// targets.
fn is_absolute_prim_path(text: &str) -> bool {
    text.strip_prefix('/')
        .is_some_and(|names| names.split('/').all(crate::ident::is_identifier))
}

/// Makes a USDA path absolute against the prim path `anchor`: `../B`,
/// `Child`, `.attr` and `../B.attr` are relative, `/A/B` is not.
///
/// Spec: AOUSD Core §8 (paths).
/// Whether `path` names a variant selection (`</A{v=x}B>`), which paths in
/// scene description cannot (see [Rejected layers](self#rejected-layers)).
fn has_variant_selection(path: &str) -> bool {
    path.contains('{')
}

/// `absolute`, the path `target` made absolute, with its bracketed paths
/// anchored as `SdfPath::MakeAbsolutePath` anchors them
/// ([`anchor_bracketed`]) when `target` is relative. OpenUSD's parser makes
/// only a relative path absolute (`TextParserAction<PathRef>`,
/// `pxr/usd/sdf/textFileFormatParser.cpp`): an absolute one keeps its
/// bracketed paths as written (`</A.b[C].d>`).
fn anchor_relative_brackets<'p>(target: &str, absolute: Cow<'p, str>) -> Cow<'p, str> {
    if target.starts_with('/') || !absolute.contains('[') {
        return absolute;
    }
    let anchored = anchor_bracketed(&absolute, &mut |inner, at| {
        absolute_path(inner, at).into_owned()
    });
    match anchored {
        Cow::Borrowed(_) => absolute,
        Cow::Owned(anchored) => Cow::Owned(anchored),
    }
}

fn absolute_path<'p>(path: &'p str, anchor: &str) -> Cow<'p, str> {
    if path.starts_with('/') {
        return Cow::Borrowed(path);
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
    Cow::Owned(out)
}

/// Whether a list-op field value names an item twice in one of its lists.
fn field_list_op_repeats(value: &FieldValue) -> bool {
    fn repeats<T: Eq + Hash>(op: &ListOp<T>) -> bool {
        let lists =
            op.explicit
                .iter()
                .chain([&op.prepend, &op.append, &op.delete, &op.add, &op.reorder]);
        lists.into_iter().any(|items| {
            let mut seen = HashSet::with_capacity(items.len());
            !items.iter().all(|item| seen.insert(item))
        })
    }
    match value {
        FieldValue::TokenListOp(op) => repeats(op),
        FieldValue::StringListOp(op) => repeats(op),
        FieldValue::IntListOp(op) => repeats(op),
        FieldValue::UIntListOp(op) => repeats(op),
        FieldValue::Int64ListOp(op) => repeats(op),
        FieldValue::UInt64ListOp(op) => repeats(op),
        FieldValue::PathListOp(op) => repeats(op),
        // Text OpenUSD keeps unchecked.
        FieldValue::UnregisteredListOp(_) | FieldValue::Value(_) => false,
        _ => false,
    }
}

/// Whether a list-op field value authors no items at all.
fn field_list_op_is_empty(value: &FieldValue) -> bool {
    match value {
        FieldValue::TokenListOp(op) => op.items().next().is_none(),
        FieldValue::StringListOp(op) => op.items().next().is_none(),
        FieldValue::IntListOp(op) => op.items().next().is_none(),
        FieldValue::UIntListOp(op) => op.items().next().is_none(),
        FieldValue::Int64ListOp(op) => op.items().next().is_none(),
        FieldValue::UInt64ListOp(op) => op.items().next().is_none(),
        FieldValue::PathListOp(op) => op.items().next().is_none(),
        // A `None` statement, which OpenUSD takes here (an empty list is
        // refused as it is read).
        FieldValue::UnregisteredListOp(_) | FieldValue::Value(_) => false,
        _ => false,
    }
}

/// The unit names an attribute's `displayUnit` may hold: those of
/// OpenUSD's length, angular and dimensionless units (`_SDF_UNITS`,
/// `pxr/usd/sdf/types.h`) that are identifiers.
const DISPLAY_UNITS: &[&str] = &[
    "mm", "cm", "dm", "m", "km", "in", "ft", "yd", "mi", "deg", "rad", "default",
];

/// Sets the list one list-op statement of a field authors, over the
/// field's earlier statements. Returns `false` when the two have different
/// element types.
fn set_field_list_op(existing: &mut FieldValue, kind: ast::ListOpKind, value: FieldValue) -> bool {
    match (existing, value) {
        (FieldValue::TokenListOp(a), FieldValue::TokenListOp(b)) => set_statement(a, kind, *b),
        (FieldValue::StringListOp(a), FieldValue::StringListOp(b)) => set_statement(a, kind, *b),
        (FieldValue::IntListOp(a), FieldValue::IntListOp(b)) => set_statement(a, kind, *b),
        (FieldValue::UIntListOp(a), FieldValue::UIntListOp(b)) => set_statement(a, kind, *b),
        (FieldValue::Int64ListOp(a), FieldValue::Int64ListOp(b)) => set_statement(a, kind, *b),
        (FieldValue::UInt64ListOp(a), FieldValue::UInt64ListOp(b)) => set_statement(a, kind, *b),
        (FieldValue::UnregisteredListOp(a), FieldValue::UnregisteredListOp(b)) => {
            set_statement(a, kind, *b);
        }
        // OpenUSD keeps an unregistered field's earlier value over a later
        // list-op statement (`_KeyValueMetadataEnd`).
        (FieldValue::Value(_), FieldValue::UnregisteredListOp(_)) => {}
        (existing @ FieldValue::Value(_), value) => *existing = value,
        _ => return false,
    }
    true
}

/// Sets, in `op`, the one list a list-op statement of `kind` authors:
/// `statement` is that statement alone. Each statement replaces its own
/// list, an explicit statement drops every edit, and an edit statement
/// drops an explicit list, so a later `prepend` replaces an earlier one and
/// `rel r = </A>` then `prepend rel r = </B>` leaves only the prepend.
///
/// Spec: AOUSD Core §12.4 (list ops hold an explicit list or edits).
/// OpenUSD: `_SetListOpItemsWithError` (`pxr/usd/sdf/textParserHelpers.h`)
/// calls `SdfListOp::SetItems` (`pxr/usd/sdf/listOp.cpp`) per statement.
fn set_statement<T>(op: &mut ListOp<T>, kind: ast::ListOpKind, statement: ListOp<T>) {
    let ListOp {
        explicit,
        prepend,
        append,
        delete,
        add,
        reorder,
        ..
    } = statement;
    if kind == ast::ListOpKind::Explicit || explicit.is_some() {
        *op = ListOp::explicit(explicit.unwrap_or_default());
        return;
    }
    if op.explicit.is_some() {
        *op = ListOp::new();
    }
    match kind {
        ast::ListOpKind::Prepend => op.prepend = prepend,
        ast::ListOpKind::Append => op.append = append,
        ast::ListOpKind::Add => op.add = add,
        ast::ListOpKind::Delete => op.delete = delete,
        ast::ListOpKind::Reorder => op.reorder = reorder,
        ast::ListOpKind::Explicit => {}
    }
}

fn convert_specifier(spec: ast::Specifier) -> Specifier {
    match spec {
        ast::Specifier::Def => Specifier::Def,
        ast::Specifier::Over => Specifier::Over,
        ast::Specifier::Class => Specifier::Class,
    }
}

fn default_scalar_for_type(type_hint: &str) -> Value {
    if type_hint == "dictionary" {
        return Value::Dictionary(Vec::new());
    }
    ValueType::from_name(type_hint).map_or(Value::Null, ValueType::zero)
}

// ── ListOp merge helpers ────────────────────────────────────────────────

fn convert_array_edit_index(index: ast::ArrayEditIndex) -> ArrayIndex {
    match index {
        ast::ArrayEditIndex::Position(value) => ArrayIndex::Position(value),
        ast::ArrayEditIndex::End => ArrayIndex::End,
    }
}

/// Creates an empty variant set spec for each name an explicit or `add`
/// `variantSets` list names that `sets` lacks, as OpenUSD's text parser
/// does for those two forms (and no other); it holds no variants, so it
/// changes no composition.
///
/// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`). OpenUSD: the
/// `TextParserAction<NameList>` in
/// `pxr/usd/sdf/textFileFormatParser.cpp`.
fn declare_listed_sets(names: &ListOp<TokenId>, sets: &mut VariantSets) {
    for name in names.explicit.iter().flatten().chain(&names.add) {
        sets.get_or_insert_default(*name);
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
            _tokens: &TokenInterner,
            _paths: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
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
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let mut resolver = StubResolver::new();
        let result = emit(
            &ast_result.layer,
            LayerId(1),
            &tokens,
            &paths,
            &mut resolver,
        );
        (result, tokens, paths)
    }

    /// An explicit or `add` `variantSets` creates an empty set spec for
    /// each name it lists, as OpenUSD's text parser does; the other forms
    /// create none, and every form is kept as authored.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`).
    #[test]
    fn explicit_and_added_variant_sets_declare_set_specs() {
        let src = "\
#usda 1.0
def \"P\" (
    variantSets = \"a\"
)
{
}

def \"Q\" (
    add variantSets = \"b\"
    prepend variantSets = \"c\"
    append variantSets = \"d\"
    delete variantSets = \"e\"
)
{
}
";
        let (result, tokens, paths) = emit_source(src);
        let spec = |path: &str| {
            let id = paths
                .lookup(&Path::parse_absolute(path, &tokens).unwrap())
                .unwrap();
            result.layer.prim(id).unwrap()
        };
        let [a, b, c, d, e] = ["a", "b", "c", "d", "e"].map(|name| tokens.intern(name));
        let (p, q) = (spec("/P"), spec("/Q"));
        assert_eq!(*p.variant_set_names(), ListOp::explicit(vec![a]));
        assert_eq!(
            *q.variant_set_names(),
            ListOp::added(vec![b])
                .with_prepended(vec![c])
                .with_appended(vec![d])
                .with_deleted(vec![e])
        );
        assert!(p.variant_sets()[&a].variants.is_empty());
        let mut declared: Vec<_> = q.variant_sets().keys().copied().collect();
        declared.sort();
        assert_eq!(declared, [b]);
    }

    #[test]
    fn emit_simple_prim() {
        let (result, tokens, paths) = emit_source("#usda 1.0\ndef Xform \"root\" {\n}\n");
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).expect("/A interned");
        let a_spec = result.layer.prim(a_id).expect("prim /A");
        let b_tok = tokens.intern("B");
        assert!(a_spec.authored_children.contains(&b_tok));

        let b_path = Path::parse_absolute("/A/B", &tokens).unwrap();
        let b_id = paths.lookup(&b_path).expect("/A/B interned");
        assert!(result.layer.prims().contains_key(&b_id));
    }

    #[test]
    fn emit_attribute_int() {
        let src = "#usda 1.0\ndef \"A\" {\n    int x = 42\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let x_tok = tokens.intern("x");
        assert_eq!(
            attr_default(&spec.properties, &x_tok),
            Some(&(Value::Int(42)))
        );
    }

    #[test]
    fn emit_attribute_double() {
        let src = "#usda 1.0\ndef \"A\" {\n    double y = 2.5\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
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
        assert_eq!(result.layer.sublayers().len(), 1);
        assert_eq!(
            result.layer.sublayers()[0],
            SublayerEntry::with_asset(LayerId(100), "./sub.usd", LayerOffset::IDENTITY),
            "the authored asset path is kept"
        );
        assert_eq!(result.resolved_layers.len(), 1);
    }

    #[test]
    fn emit_inherits() {
        let src = "#usda 1.0\ndef \"A\" (\n    inherits = </B>\n) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        assert!(spec.inherits().explicit.is_some());
        let b_path = Path::parse_absolute("/B", &tokens).unwrap();
        let b_id = paths.lookup(&b_path).expect("/B");
        assert_eq!(spec.inherits().explicit.as_ref().unwrap(), &[b_id]);
    }

    #[test]
    fn a_layer_without_arcs_is_noted_as_authoring_none() {
        let (result, ..) =
            emit_source("#usda 1.0\ndef \"A\" (inherits = </B>) {\n    def \"C\" {}\n}\n");
        assert!(result.layer.known_without_reference_arcs());
        // An arc in a nested variant branch, or on a prim, is one.
        for src in [
            "#usda 1.0\ndef \"A\" (prepend references = @./ref.usd@) {}\n",
            "#usda 1.0\ndef \"A\" (payload = @./p.usd@) {}\n",
            "#usda 1.0\ndef \"A\" (variantSets = \"v\") {\n    variantSet \"v\" = {\n        \"x\" (variantSets = \"w\") {\n            variantSet \"w\" = {\n                \"y\" (payload = @./p.usd@) {}\n            }\n        }\n    }\n}\n",
        ] {
            let (result, ..) = emit_source(src);
            assert!(!result.layer.known_without_reference_arcs(), "{src}");
        }
    }

    #[test]
    fn emit_references() {
        let src = "#usda 1.0\ndef \"A\" (\n    prepend references = @./ref.usd@\n) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        assert_eq!(spec.references.prepend.len(), 1);
        assert_eq!(
            spec.references.prepend[0].source(),
            ReferenceSource::Layer {
                layer: LayerId(100),
                asset: Some("./ref.usd"),
            }
        );
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        let shade_tok = tokens.intern("shade");
        let red_tok = tokens.intern("red");
        assert_eq!(spec.variant_selection(shade_tok), Some(red_tok));
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        let shade_tok = tokens.intern("shade");
        let red_tok = tokens.intern("red");
        assert_eq!(spec.variant_selection(shade_tok), Some(red_tok));
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        let color_tok = tokens.intern("color");
        assert!(spec.variant_sets().contains_key(&color_tok));
        let vs = spec.variant_sets().get(&color_tok).unwrap();
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
    fn empty_variant_names_drop_their_statement() {
        // OpenUSD reads the statement into a variant spec at the set's own
        // path and never lists the set, so its layer holds none of it.
        let src = "#usda 1.0\ndef \"A\"\n{\n    variantSet \"v\" = {\n        \"\" {\n        }\n        \"x\" {\n            int r = 1\n        }\n    }\n}\n";
        let (result, tokens, paths) = emit_source(src);
        assert!(!result.rejected);
        assert!(
            result
                .diagnostics
                .iter()
                .all(|d| d.severity == crate::diagnostic::Severity::Warning),
            "{:?}",
            result.diagnostics
        );
        assert_eq!(result.diagnostics.len(), 1);
        let a = paths
            .lookup(&Path::parse_absolute("/A", &tokens).unwrap())
            .unwrap();
        assert!(result.layer.prim(a).unwrap().variant_sets().is_empty());
    }

    #[test]
    fn emit_reorder_name_children() {
        let src = "\
#usda 1.0
def \"A\" {
    reorder nameChildren = [\"B\", \"C\"]
}
";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        let b = tokens.intern("B");
        let c = tokens.intern("C");
        assert_eq!(spec.prim_order(), Some(&[b, c][..]));
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        let x_tok = tokens.intern("x");
        if let Some(ts) = prop(&spec.properties, &x_tok).time_samples.get() {
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
        let x_tok = tokens.intern("x");
        let Some(ts) = prop(&spec.properties, &x_tok).time_samples.get() else {
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
    fn emit_relocates_reads_layer_metadata() {
        // Spec: AOUSD Core §16.2.18.5. Entries are kept in authored order,
        // relative paths are anchored at the pseudo-root, and `<>` removes
        // the source.
        let src = "\
#usda 1.0
(
    relocates = {
        </Rig/Anim>: </Anim>,
        <Rig/Other>: <>,
        </Rig/Deep>: </Rig/Anim/Deep>
    }
)
def \"Rig\" {
}
";
        let (result, tokens, paths) = emit_source(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let path = |text: &str| {
            paths
                .lookup(&Path::parse_absolute(text, &tokens).unwrap())
                .unwrap()
        };
        assert_eq!(
            result.layer.relocates(),
            [
                layerstack::Relocate {
                    source: path("/Rig/Anim"),
                    target: Some(path("/Anim")),
                },
                layerstack::Relocate {
                    source: path("/Rig/Other"),
                    target: None,
                },
                layerstack::Relocate {
                    source: path("/Rig/Deep"),
                    target: Some(path("/Rig/Anim/Deep")),
                },
            ]
        );
    }

    #[test]
    fn emit_relocates_rejects_non_prim_paths() {
        // OpenUSD rejects a layer whose relocates name a variant selection
        // or a property; the entry is reported and dropped.
        let src = "\
#usda 1.0
(
    relocates = {
        </Rig{v=a}Anim>: </Anim>,
        </Rig/Anim>: </Anim.attr>,
        </Rig/Keep>: </Keep>
    }
)
def \"Rig\" {
}
";
        let (result, _tokens, _paths) = emit_source(src);
        assert_eq!(result.diagnostics.len(), 2, "one diagnostic per bad entry");
        let first = &result.diagnostics[0];
        assert_eq!(first.severity, crate::diagnostic::Severity::Error);
        assert_eq!(first.span.text(src), "</Rig{v=a}Anim>: </Anim>");
        assert_eq!(result.layer.relocates().len(), 1);
        assert_eq!(result.layer.prims().len(), 2, "pseudo-root and /Rig");
    }

    /// Spec: OpenUSD's text parser rejects the layer (`SdfSchema::IsValid*`);
    /// see the module's "Rejected layers".
    #[test]
    fn emit_rejects_paths_and_names_openusd_refuses() {
        let cases = [
            (
                "def \"A\" (\n    inherits = </B{v=x}C>\n)\n{\n}\n",
                "Inherit paths cannot contain variant selections",
            ),
            (
                "def \"A\" (\n    specializes = </B{v=x}C>\n)\n{\n}\n",
                "Specializes paths cannot contain variant selections",
            ),
            (
                "def \"A\" (\n    references = </B{v=x}C>\n)\n{\n}\n",
                "Reference paths cannot contain variant selections",
            ),
            (
                "def \"A\" (\n    payload = </B{v=x}C>\n)\n{\n}\n",
                "Payload paths cannot contain variant selections",
            ),
            (
                "(\n    relocates = {\n        </B{v=x}C>: </B/D>\n    }\n)\n",
                "'/B{v=x}C' is not a valid relocates source path",
            ),
            (
                "def \"A\"\n{\n    rel r = </B{v=x}C>\n}\n",
                "Relationship target paths cannot contain variant selections",
            ),
            (
                "def \"A\" (\n    variantSets = \"v\"\n)\n{\n    variantSet \"v\" = {\n        \"x\" {\n            def \"C\"\n            {\n                rel r = <../D>\n            }\n        }\n    }\n}\n",
                "Relationship target paths cannot contain variant selections",
            ),
            (
                "def \"A\"\n{\n    int x.connect = </B{v=x}C.y>\n}\n",
                "'/B{v=x}C.y' is not a valid prim or property scene path",
            ),
            (
                "def \"A\" (\n    inherits = </>\n)\n{\n}\n",
                "'/' is not a valid prim path",
            ),
            (
                "def \"A\" (\n    inherits = <>\n)\n{\n}\n",
                "'' is not a valid prim path",
            ),
            (
                "def \"A\" (\n    inherits = <..>\n)\n{\n}\n",
                "Inherit paths must be absolute prim paths",
            ),
            (
                "def \"A\" (\n    specializes = </B.c>\n)\n{\n}\n",
                "'/B.c' is not a valid prim path",
            ),
            (
                "def \"A\" (\n    references = <../B>\n)\n{\n}\n",
                "Reference prim path <../B> must be either empty or an absolute prim path",
            ),
            (
                "def \"A\" (\n    payload = @a.usda@</>\n)\n{\n}\n",
                "'/' is not a valid prim path",
            ),
            (
                "def \"A\"\n{\n    rel r = <>\n}\n",
                "Relationship target paths must be absolute prim, property or mapper paths",
            ),
            (
                "def \"A\"\n{\n    rel r = </>\n}\n",
                "Relationship target paths must be absolute prim, property or mapper paths",
            ),
            (
                "def \"A\"\n{\n    int x.connect = <..>\n}\n",
                "Connection paths must be absolute prim or property paths",
            ),
            ("def \"A/B\"\n{\n}\n", "'A/B' is not a valid prim name"),
            ("def \"a b\"\n{\n}\n", "'a b' is not a valid prim name"),
            ("def \"1a\"\n{\n}\n", "'1a' is not a valid prim name"),
            ("def \"a\x10\"\n{\n}\n", "'a\x10' is not a valid prim name"),
            (
                "def \"A\"\n{\n    def \"\"\n    {\n    }\n}\n",
                "'' is not a valid prim name",
            ),
            (
                "def \"A\" (\n    variantSets = \"v\"\n)\n{\n    variantSet \"v\" = {\n        \"x\" {\n            def \"\"\n            {\n            }\n        }\n    }\n}\n",
                "'' is not a valid prim name",
            ),
        ];
        for (body, message) in cases {
            let src = format!("#usda 1.0\n{body}");
            let (result, _tokens, _paths) = emit_source(&src);
            assert!(result.rejected, "{src}");
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.message.starts_with(message)),
                "{src}: {:?}",
                result.diagnostics
            );
        }
    }

    /// A relative target that climbs out of every variant branch, and a
    /// relative connection inside one, are not rejected.
    #[test]
    fn emit_keeps_relative_paths_outside_variant_selections() {
        let src = "#usda 1.0
def \"A\" (
    variantSets = \"v\"
)
{
    variantSet \"v\" = {
        \"x\" {
            def \"C\"
            {
                rel r = <../../D>
                int x
                int x.connect = <.y>
            }
        }
    }
}
";
        let (result, tokens, paths) = emit_source(src);
        assert!(!result.rejected, "{:?}", result.diagnostics);
        // `<../../D>` climbs `C` and the selection `{v=x}` of `/A{v=x}C`:
        // `/A/D`. The connection is anchored at `/A/C`.
        let child = Path::parse_absolute("/A/C", &tokens).unwrap();
        let spec = result
            .layer
            .prim_specs(paths.lookup(&child).unwrap())
            .next()
            .expect("branch spec of /A/C");
        let (r, x) = (tokens.intern("r"), tokens.intern("x"));
        let targets = |name| {
            let op = spec.property(name).unwrap().targets.clone().unwrap();
            op.explicit
                .unwrap()
                .iter()
                .map(|target| target.display(&paths, &tokens))
                .collect::<Vec<_>>()
        };
        assert_eq!(targets(r), ["/A/D"]);
        assert_eq!(targets(x), ["/A/C.y"]);
    }

    #[test]
    fn emit_relationship() {
        let src = "#usda 1.0\ndef \"A\" {\n    rel target = </B>\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
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
        let (result, tokens, paths) = emit_source(src);
        // The `over "Rig"` inside the variant should produce a PrimSpec at /A/Rig.
        let rig_path = Path::parse_absolute("/A/Rig", &tokens).unwrap();
        let rig_id = paths.lookup(&rig_path).expect("/A/Rig interned");
        let spec = result.layer.prim(rig_id).expect("prim /A/Rig");
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
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).unwrap();
        let spec = result.layer.prim(a_id).unwrap();
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
        let d_path = Path::parse_absolute("/D", &tokens).unwrap();
        let d_id = paths.lookup(&d_path).unwrap();
        let d_spec = result.layer.prim(d_id).unwrap();

        let standin_tok = tokens.intern("standin");
        let shading_tok = tokens.intern("shadingVariant");
        let anim_tok = tokens.intern("anim");
        let default_tok = tokens.intern("default");
        let spooky_tok = tokens.intern("spooky");

        // The prim spec holds `standin`; `shadingVariant` is nested in the
        // `anim` branch's variant spec (`/D{standin=anim}{shadingVariant=}`).
        assert_eq!(d_spec.variant_set_names().add, [standin_tok]);
        assert!(!d_spec.variant_sets().contains_key(&shading_tok));
        let anim_variant = d_spec
            .variant_spec(&[(standin_tok, anim_tok)])
            .expect("anim variant");
        assert_eq!(anim_variant.variant_set_names.add, [shading_tok]);
        let shading_vs = anim_variant
            .variant_sets
            .get(&shading_tok)
            .expect("shadingVariant nested in standin=anim");
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

        // Each child's prim spec records both branches enclosing it:
        // standin=anim, then its own shadingVariant branch.
        let site = |set, variant| VariantSelectionSite {
            host_path: d_id,
            set,
            variant,
        };
        let branch_sites = |name: &str, tokens: &TokenInterner| {
            let path = Path::parse_absolute(&format!("/D/{name}"), tokens).unwrap();
            let specs: Vec<_> = result
                .layer
                .prim_specs(paths.lookup(&path).unwrap())
                .map(|spec| spec.outer_variant_sites().to_vec())
                .collect();
            specs
        };
        assert_eq!(
            branch_sites("anim_default_cone", &mut tokens),
            [vec![
                site(standin_tok, anim_tok),
                site(shading_tok, default_tok)
            ]]
        );
        assert_eq!(
            branch_sites("anim_spooky_sphere", &mut tokens),
            [vec![
                site(standin_tok, anim_tok),
                site(shading_tok, spooky_tok)
            ]]
        );

        // The outer branch lists only its own children.
        assert!(anim_variant.authored_children.is_empty());
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
        let d_path = Path::parse_absolute("/D", &tokens).unwrap();
        let d_id = paths.lookup(&d_path).unwrap();
        let d_spec = result.layer.prim(d_id).unwrap();

        let [standin, shading, anim, render, default, spooky] = [
            "standin",
            "shadingVariant",
            "anim",
            "render",
            "default",
            "spooky",
        ]
        .map(|name| tokens.intern(name));
        let children = |chain: &[(TokenId, TokenId)], tokens: &TokenInterner| {
            d_spec
                .variant_spec(chain)
                .unwrap_or_else(|| panic!("variant spec at {chain:?}"))
                .authored_children
                .iter()
                .map(|t| String::from(tokens.resolve(*t)))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            d_spec.variant_set_names().add,
            [standin],
            "prim spec's sets"
        );

        // Every branch is a variant spec of its own: the inner
        // `{standin=anim}` does not merge into the outer one, and the two
        // `shadingVariant` sets stay apart.
        let anim_spooky = [(standin, anim), (shading, spooky)];
        let anim_spooky_anim = [(standin, anim), (shading, spooky), (standin, anim)];
        assert!(children(&[(standin, anim)], &tokens).is_empty());
        assert_eq!(
            children(&[(standin, anim), (shading, default)], &tokens),
            ["anim_default_cone"]
        );
        assert_eq!(children(&anim_spooky, &tokens), ["anim_spooky_sphere"]);
        assert_eq!(
            children(&anim_spooky_anim, &tokens),
            ["anim_spooky_anim_sphere"]
        );
        assert_eq!(
            children(&[(standin, render), (shading, spooky)], &tokens),
            ["render_spooky_cylinder"]
        );
        assert_eq!(
            children(&[(standin, render), (shading, default)], &tokens),
            ["render_default_cube"]
        );
        assert_eq!(
            d_spec
                .variant_spec(&anim_spooky)
                .unwrap()
                .variant_set_names
                .add,
            [standin],
            "sets nested in the spooky branch"
        );
        assert_eq!(d_spec.variant_branches().count(), 7);

        // The prim spec of each child records every enclosing branch.
        let site = |set, variant| VariantSelectionSite {
            host_path: d_id,
            set,
            variant,
        };
        let branch_sites = |name: &str, tokens: &TokenInterner| {
            let path = Path::parse_absolute(&format!("/D/{name}"), tokens).unwrap();
            let specs: Vec<_> = result
                .layer
                .prim_specs(paths.lookup(&path).unwrap())
                .map(|spec| spec.outer_variant_sites().to_vec())
                .collect();
            specs
        };
        assert_eq!(
            branch_sites("anim_spooky_anim_sphere", &mut tokens),
            [vec![
                site(standin, anim),
                site(shading, spooky),
                site(standin, anim),
            ]],
            "branches enclosing anim_spooky_anim_sphere"
        );
        assert_eq!(
            branch_sites("anim_spooky_sphere", &mut tokens),
            [vec![site(standin, anim), site(shading, spooky)]],
            "branches enclosing anim_spooky_sphere"
        );
    }

    #[test]
    fn emit_type_name_stored() {
        let src = "#usda 1.0\ndef Xform \"root\" {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        let xform_tok = tokens.intern("Xform");
        assert_eq!(spec.type_name, Some(xform_tok));
    }

    #[test]
    fn emit_type_name_none_for_untyped() {
        let src = "#usda 1.0\ndef \"root\" {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        assert_eq!(spec.type_name, None);
    }

    #[test]
    fn emit_type_name_mesh() {
        let src = "#usda 1.0\ndef Mesh \"geo\" {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/geo", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("geo path interned");
        let spec = result.layer.prim(root_id).expect("geo prim");
        let mesh_tok = tokens.intern("Mesh");
        assert_eq!(spec.type_name, Some(mesh_tok));
    }

    #[test]
    fn emit_instanceable() {
        // Lowercase `true` (Rust-style).
        let src = "#usda 1.0\ndef Xform \"root\" (instanceable = true) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        assert_eq!(spec.instanceable, Some(true));
    }

    #[test]
    fn emit_instanceable_capitalized() {
        // Capitalized `True` (Python-style, common in USDA files).
        let src = "#usda 1.0\ndef Xform \"root\" (instanceable = True) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        assert_eq!(spec.instanceable, Some(true));
    }

    #[test]
    fn emit_active_false() {
        let src = "#usda 1.0\ndef Xform \"root\" (active = false) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        assert_eq!(spec.active, Some(false));
    }

    #[test]
    fn emit_active_true() {
        let src = "#usda 1.0\ndef Xform \"root\" (active = true) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        assert_eq!(spec.active, Some(true));
    }

    #[test]
    fn emit_active_capitalized() {
        let src = "#usda 1.0\ndef Xform \"root\" (active = False) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let root_path = Path::parse_absolute("/root", &tokens).unwrap();
        let root_id = paths.lookup(&root_path).expect("root path interned");
        let spec = result.layer.prim(root_id).expect("root prim");
        assert_eq!(spec.active, Some(false));
    }

    #[test]
    fn emit_prepend_api_schemas_produces_token_listop() {
        let src = "#usda 1.0\n\
                   def Mesh \"card\" (\n\
                       prepend apiSchemas = [\"MaterialBindingAPI\"]\n\
                   ) {\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let card_path = Path::parse_absolute("/card", &tokens).unwrap();
        let card_id = paths.lookup(&card_path).expect("card path interned");
        let spec = result.layer.prim(card_id).expect("card prim");

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
        let (result, tokens, _paths) = emit_source(src);
        assert_eq!(
            result.layer.default_prim(),
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let cd_tok = tokens.intern("customData");
        let field = get_field(&spec.fields, &cd_tok).expect("customData field");
        match field {
            FieldValue::Value(Value::Dictionary(entries)) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(&*entries[0].0, "foo");
                assert_eq!(entries[0].1, Value::String(Arc::from("bar")).into());
                assert_eq!(&*entries[1].0, "count");
                assert_eq!(entries[1].1, Value::Int(42).into());
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let cd_tok = tokens.intern("customData");
        let field = get_field(&spec.fields, &cd_tok).expect("customData field");
        match field {
            FieldValue::Value(Value::Dictionary(entries)) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(&*entries[0].0, "inner");
                match &entries[0].1 {
                    FieldValue::Value(Value::Dictionary(inner)) => {
                        assert_eq!(inner.len(), 1);
                        assert_eq!(&*inner[0].0, "val");
                        assert_eq!(inner[0].1, Value::Double(1.5).into());
                    }
                    other => panic!("expected nested Dictionary, got {:?}", other),
                }
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    /// OpenUSD 26.11 refuses an attribute typed `dictionary`, a keyword
    /// of its grammar (`Sdf.Layer.ImportFromString` fails with
    /// "Expected }"), so parsing reports it; recovery still emits the
    /// attribute as declared.
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
        let cst = parse_cst(src);
        assert_eq!(cst.diagnostics.len(), 1, "{:?}", cst.diagnostics);
        assert_eq!(
            cst.diagnostics[0].message,
            "expected an attribute type, not the keyword `dictionary`"
        );
        let ast_result = crate::lower::lower(&cst.tree, src);
        let tokens = TokenInterner::default();
        let _paths = PathInterner::default();
        let result = emit(
            &ast_result.layer,
            LayerId(1),
            &tokens,
            &_paths,
            &mut StubResolver::new(),
        );
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
                assert_eq!(entries[0].1, Value::String(Arc::from("value")).into());
            }
            other => panic!("expected Dictionary, got {:?}", other),
        }
    }

    #[test]
    fn emit_tuple_float3() {
        let src = "#usda 1.0\ndef \"A\" {\n    float3 pos = (1.0, 2.0, 3.0)\n}\n";
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let ids_tok = tokens.intern("ids");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &ids_tok)
                .expect("ids field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::TypedArray(TypedArray::Int(items))) => {
                assert_eq!(items.as_slice(), &[1, 2, 3]);
            }
            other => panic!("expected Array, got {:?}", other),
        }
    }

    #[test]
    fn emit_nested_array() {
        // Array of tuples: float3[] points = [(1, 2, 3), (4, 5, 6)]
        let src = "#usda 1.0\ndef \"A\" {\n    float3[] points = [(1, 2, 3), (4, 5, 6)]\n}\n";
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let pts_tok = tokens.intern("points");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &pts_tok)
                .expect("points field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::TypedArray(TypedArray::Vec3f(items))) => {
                assert_eq!(items.as_slice(), &[[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
            }
            other => panic!("expected Array of Vec3f, got {:?}", other),
        }
    }

    #[test]
    fn emit_empty_array() {
        let src = "#usda 1.0\ndef \"A\" {\n    int[] empty = []\n}\n";
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let empty_tok = tokens.intern("empty");
        let field = &FieldValue::Value(
            attr_default(&spec.properties, &empty_tok)
                .expect("empty field")
                .clone(),
        );
        match field {
            FieldValue::Value(Value::TypedArray(TypedArray::Int(items))) => {
                assert!(items.is_empty());
            }
            other => panic!("expected empty Array, got {:?}", other),
        }
    }

    #[test]
    fn emit_double3() {
        let src = "#usda 1.0\ndef \"A\" {\n    double3 pos = (1.5, 2.5, 3.5)\n}\n";
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
        let (result, tokens, _paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = _paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
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
    fn attributes_declaring_one_type_share_it() {
        let src = "#usda 1.0\ndef \"A\" {\n    notatype a = 1\n    notatype b\n    \
                   notatype[] c\n    float d\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let spec = result.layer.prim(paths.lookup(&a_path).unwrap()).unwrap();
        let declared = |name: &str| {
            let entry = prop(&spec.properties, &tokens.intern(name));
            entry.type_name.clone().expect("a declared type")
        };
        let (a, b, c) = (declared("a"), declared("b"), declared("c"));
        assert_eq!(a.type_name(), "notatype");
        assert_eq!(a.type_name().as_ptr(), b.type_name().as_ptr());
        assert_eq!(c.type_name(), "notatype[]");
        assert!(c.is_array() && !a.is_array());
        assert_eq!(declared("d"), PropertyType::parse("float").unwrap());
    }

    #[test]
    fn emit_array_edit_value() {
        let src =
            "#usda 1.0\ndef \"A\" {\n    int[] x = edit [write 3 to [0]; append 4; resize 3]\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let a_path = Path::parse_absolute("/A", &tokens).unwrap();
        let a_id = paths.lookup(&a_path).expect("/A");
        let spec = result.layer.prim(a_id).unwrap();
        let x_tok = tokens.intern("x");
        let entry = prop(&spec.properties, &x_tok);

        match entry.default.as_ref().and_then(Value::array_edit_ref) {
            Some(edit) => {
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
        assert!(property_type.is_array());
        assert_eq!(property_type.default_scalar(), &Value::Int(0));
        assert_eq!(
            entry.default.as_ref().unwrap().array_edit_type(),
            Some(property_type)
        );
    }

    #[test]
    fn literal_free_array_edits_retain_native_type_and_property_roles() {
        let src = "#usda 1.0\ndef \"A\" {\n\
            point3f[] points = edit []\n\
            timecode[] clock.timeSamples = {1: edit [resize 2]}\n\
            float[] scalar = edit [erase [0]]\n\
        }\n";
        let (result, tokens, paths) = emit_source(src);
        let a = prim(&result, &tokens, &paths, "/A");
        for (name, expected, storage, prototype) in [
            ("points", "point3f[]", "float3[]", Value::Vec3f([0.0; 3])),
            ("clock", "timecode[]", "timecode[]", Value::TimeCode(0.0)),
            ("scalar", "float[]", "float[]", Value::Float(0.0)),
        ] {
            let property = prop(&a.properties, &tokens.intern(name));
            let value = property
                .default
                .as_ref()
                .or_else(|| {
                    property
                        .time_samples
                        .get()
                        .and_then(|samples| samples.first().map(|(_, v)| v))
                })
                .unwrap();
            let actual = value.array_edit_type().unwrap();
            assert_eq!(actual.type_name(), storage);
            assert_eq!(property.type_name.as_ref().unwrap().type_name(), expected);
            assert_eq!(actual.default_scalar(), &prototype);
            assert!(actual.is_array());
            assert!(value.array_edit_ref().is_some());
        }
    }

    /// Returns the prim spec at `path` from an emit result.
    fn prim<'r>(
        result: &'r EmitResult,
        tokens: &TokenInterner,
        paths: &PathInterner,
        path: &str,
    ) -> &'r PrimSpec {
        let path = Path::parse_absolute(path, tokens).unwrap();
        let id = paths.lookup(&path).expect("path interned");
        result.layer.prim(id).expect("prim spec")
    }

    /// The default and time samples of each attribute of `/P`, by name, and
    /// the number of diagnostics.
    #[allow(clippy::type_complexity, reason = "test helper")]
    fn attribute_values(
        body: &str,
    ) -> (
        Vec<(String, Option<Value>, Option<Vec<(f64, Value)>>)>,
        usize,
    ) {
        let src = alloc::format!("#usda 1.0\ndef \"P\" {{\n{body}\n}}\n");
        let (result, tokens, paths) = emit_source(&src);
        let spec = prim(&result, &tokens, &paths, "/P");
        let values = spec
            .properties
            .iter()
            .map(|entry| {
                (
                    String::from(tokens.resolve(entry.name)),
                    entry.spec.default.clone(),
                    entry.spec.time_samples.get().map(<[_]>::to_vec),
                )
            })
            .collect();
        (values, result.diagnostics.len())
    }

    /// A number given for `bool` is true when nonzero, as OpenUSD 26.08
    /// reads it (`Sdf_ParserHelpers::_GetImpl<bool>`), in scalars, arrays
    /// and time samples.
    #[test]
    fn numbers_for_bool_are_true_when_nonzero() {
        let (values, diagnostics) = attribute_values(
            "    bool a = -0\n    bool b = 2\n    bool c = 1.5\n    bool d = -inf\n    bool e = \
             nan\n    bool f = 0\n    bool[] g = [-0, 2, 0.0, -1]\n    bool h.timeSamples = {\n        \
             1: -0,\n        2: 1.5,\n    }",
        );
        assert_eq!(diagnostics, 0);
        let expect = [
            ("a", Value::Bool(false)),
            ("b", Value::Bool(true)),
            ("c", Value::Bool(true)),
            ("d", Value::Bool(true)),
            ("e", Value::Bool(true)),
            ("f", Value::Bool(false)),
            (
                "g",
                Value::Array(vec![
                    Value::Bool(false),
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true),
                ]),
            ),
        ];
        for (name, value) in expect {
            let found = values.iter().find(|v| v.0 == name).unwrap();
            assert_eq!(found.1.as_ref(), Some(&value), "{name}");
        }
        let h = values.iter().find(|v| v.0 == "h").unwrap();
        assert_eq!(
            h.2.as_deref(),
            Some(&[(1.0, Value::Bool(false)), (2.0, Value::Bool(true))][..])
        );
    }

    /// Each number converts to the declared type as OpenUSD 26.08 converts
    /// it (`GfNumericCast`): toward zero for integers, signed zero kept for
    /// floating point.
    #[test]
    fn numbers_convert_to_the_declared_type() {
        let (values, diagnostics) = attribute_values(
            "    int a = 1.5\n    int b = -1.5\n    uint c = 1.5\n    int64 d = 4294967296\n    \
             float e = -0\n    double f = 2\n    timecode g = -0\n    half h = -0\n    uint64 i = \
             1e3\n    float[] j = [1, -0, 1.5]\n    int k.timeSamples = {\n        1: 2.5,\n    }",
        );
        assert_eq!(diagnostics, 0);
        let expect = [
            ("a", Value::Int(1)),
            ("b", Value::Int(-1)),
            ("c", Value::UInt(1)),
            ("d", Value::Int64(4_294_967_296)),
            ("f", Value::Double(2.0)),
            ("h", Value::Half(0x8000)),
            ("i", Value::UInt64(1000)),
        ];
        for (name, value) in expect {
            let found = values.iter().find(|v| v.0 == name).unwrap();
            assert_eq!(found.1.as_ref(), Some(&value), "{name}");
        }
        let signed = |name: &str| {
            values
                .iter()
                .find(|v| v.0 == name)
                .and_then(|v| v.1.clone())
                .unwrap()
        };
        assert!(matches!(signed("e"), Value::Float(z) if z == 0.0 && z.is_sign_negative()));
        assert!(matches!(signed("g"), Value::TimeCode(z) if z == 0.0 && z.is_sign_negative()));
        assert!(matches!(
            signed("j"),
            Value::TypedArray(TypedArray::Float(items)) if matches!(items[..],
                [a, b, c]
                    if a == 1.0 && b.is_sign_negative() && c == 1.5)
        ));
        let k = values.iter().find(|v| v.0 == "k").unwrap();
        assert_eq!(k.2.as_deref(), Some(&[(1.0, Value::Int(2))][..]));
    }

    /// A value OpenUSD 26.08 rejects for the declared type (out of range,
    /// not finite for an integer, or not a number) is reported and not
    /// imported; the other values and samples are kept.
    #[test]
    fn values_that_do_not_convert_are_rejected() {
        let (values, diagnostics) = attribute_values(
            "    uint a = -1\n    int b = inf\n    int c = nan\n    int d = 4294967296\n    int64 e \
             = 1e30\n    float f = true\n    int g = \"3\"\n    int[] h = [1, 1.5e10]\n    float3 i \
             = (1, true, 2)\n    int j.timeSamples = {\n        1: 1e20,\n        2: 3,\n    }\n    \
             double ok = 1",
        );
        assert_eq!(diagnostics, 10);
        for name in ["a", "b", "c", "d", "e", "f", "g", "h", "i"] {
            let found = values.iter().find(|v| v.0 == name).unwrap();
            assert_eq!(found.1, None, "{name}");
        }
        let j = values.iter().find(|v| v.0 == "j").unwrap();
        assert_eq!(j.2.as_deref(), Some(&[(2.0, Value::Int(3))][..]));
        let ok = values.iter().find(|v| v.0 == "ok").unwrap();
        assert_eq!(ok.1, Some(Value::Double(1.0)));
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
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/Root");
        let a = prop(&spec.properties, &tokens.intern("a"));
        assert!(a.custom);
        assert_eq!(a.default, Some(Value::Float(1.0)));
        assert_eq!(
            a.time_samples.get(),
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
        weight = 0.5
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
        let (result, tokens, paths) = emit_source(src);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let spec = prim(&result, &tokens, &paths, "/A");
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
        assert_eq!(
            color.metadata(tokens.intern("weight")),
            Some(&FieldValue::Value(Value::Float(0.5))),
            "usdSkel weight metadata retains its registered float type"
        );
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
                Value::Dictionary(vec![(Arc::from("minimum"), Value::Float(0.0).into())]).into(),
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
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/A");
        let api = tokens.intern("apiSchemas");
        let geom = tokens.intern("GeomModelAPI");
        let review = tokens.intern("StudioReviewAPI:main");
        assert_eq!(
            spec.field(api),
            Some(&FieldValue::TokenListOp(Box::new(
                ListOp::prepended(vec![geom]).with_appended(vec![review])
            ))),
            "a prepend and an append statement each set their own list"
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
            spec.property_order(),
            Some(&[tokens.intern("x"), tokens.intern("y")][..])
        );
    }

    /// A set's variants keep the order their blocks introduce them, as
    /// OpenUSD's text parser creates `variantChildren`; a block naming a
    /// variant again adds to it in place.
    #[test]
    fn emit_variants_in_block_order() {
        let src = "\
#usda 1.0
def \"A\"
{
    variantSet \"v\" = {
        \"zeta\" {
            int x = 1
        }
        \"alpha\" {
        }
    }
    variantSet \"v\" = {
        \"mid\" {
        }
        \"zeta\" {
            int y = 2
        }
    }
}
";
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/A");
        let variants = &spec.variant_sets()[&tokens.intern("v")].variants;
        let names: Vec<&str> = variants.keys().map(|name| tokens.resolve(*name)).collect();
        assert_eq!(names, ["zeta", "alpha", "mid"]);
        assert_eq!(variants[&tokens.intern("zeta")].properties.len(), 2);
        assert_eq!(
            variants.allocation_size(),
            3 * size_of::<(TokenId, VariantSpec)>()
        );
    }

    /// Each list-op statement sets its own list, as OpenUSD's parser calls
    /// `SdfListOp::SetItems` per statement: a repeated `prepend` replaces
    /// the earlier one, an explicit statement drops the edits and an edit
    /// statement the explicit list.
    #[test]
    fn each_list_op_statement_sets_its_own_list() {
        let src = "\
#usda 1.0
def \"A\" (
    apiSchemas = [\"X\"]
    prepend apiSchemas = [\"Y\"]
    prepend apiSchemas = [\"Z\"]
    append apiSchemas = [\"W\"]
)
{
    rel r = </B>
    prepend rel r = </C>
    prepend rel s = </B>
    rel s = None
}
";
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/A");
        assert_eq!(
            spec.field(tokens.intern("apiSchemas")),
            Some(&FieldValue::TokenListOp(Box::new(
                ListOp::prepended(vec![tokens.intern("Z")]).with_appended(vec![tokens.intern("W")])
            )))
        );
        let target = |path: &str| {
            let path = Path::parse_absolute(path, &tokens).unwrap();
            TargetPath::Prim(paths.intern(path))
        };
        let r = prop(&spec.properties, &tokens.intern("r"));
        assert_eq!(
            r.targets.as_deref(),
            Some(&ListOp::prepended(vec![target("/C")]))
        );
        let s = prop(&spec.properties, &tokens.intern("s"));
        assert_eq!(s.targets.as_deref(), Some(&ListOp::explicit(Vec::new())));
    }

    #[test]
    fn emit_explicit_api_schemas_is_a_list_op() {
        let src = "#usda 1.0\ndef \"A\" (\n    apiSchemas = [\"GeomModelAPI\"]\n)\n{\n}\n";
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/A");
        let geom = tokens.intern("GeomModelAPI");
        assert_eq!(
            spec.field(tokens.intern("apiSchemas")),
            Some(&FieldValue::TokenListOp(Box::new(ListOp::explicit(vec![
                geom
            ]))))
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
        let (result, tokens, _paths) = emit_source(src);
        let layer = &result.layer;
        assert_eq!(layer.default_prim(), Some(tokens.intern("A")));
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
        let (result, tokens, paths) = emit_source(src);
        assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
        assert_eq!(
            result.diagnostics[0].span.text(src).trim_end(),
            "rel x = </B>"
        );
        let spec = prim(&result, &tokens, &paths, "/A");
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
            reorder nameChildren = [\"C\", \"B\"]
        }
    }
}
";
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/A");
        let branch = &spec.variant_sets()[&tokens.intern("v")].variants[&tokens.intern("one")];
        // The branch's prim spec keeps its child order, as OpenUSD's does.
        assert_eq!(
            branch.prim_order,
            Some(vec![tokens.intern("C"), tokens.intern("B")])
        );
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
        let (result, tokens, paths) = emit_source(src);
        let spec = prim(&result, &tokens, &paths, "/A");
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
    fn prim_paths_intern_as_their_parsed_paths_do() {
        // The same tokens and paths, in the same order, as interning the
        // parsed `Path`, including for text that does not parse.
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let (parsed_tokens, parsed_paths) = (TokenInterner::default(), PathInterner::default());
        let mut resolver = StubResolver::new();
        let ctx = EmitCtx {
            registry: None,
            arrays: crate::read::PreparedValues::default(),
            tokens: &tokens,
            paths: &paths,
            resolver: &mut resolver,
            defer_variant_arcs: false,
            layer_id: LayerId(1),
            resolved_layers: Vec::new(),
            diagnostics: Vec::new(),
            rejections: Vec::new(),
            rejected: false,
            emitted_arcs: false,
            property_types: Vec::new(),
        };
        for text in [
            "/", "/A/B/C", "/A", "/A/B", "/A//B", "/A/X/", "//Y", "Z", "", "/A.b", "/D/E/F/G",
        ] {
            let parsed = Path::parse_absolute(text, &parsed_tokens)
                .ok()
                .map(|path| parsed_paths.intern(path));
            assert_eq!(ctx.intern_prim_path(text), parsed, "{text}");
            assert_eq!(
                tokens.intern("probe"),
                parsed_tokens.intern("probe"),
                "{text}"
            );
            let probe = |paths: &PathInterner, tokens: &TokenInterner| {
                paths.intern(Path::parse_absolute("/probe", tokens).expect("a path"))
            };
            assert_eq!(probe(&paths, &tokens), probe(&parsed_paths, &parsed_tokens));
        }
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

    /// A resolver that defers arcs inside variant branches and records
    /// the asset paths it resolves.
    #[derive(Default)]
    struct DeferringResolver {
        resolved: Vec<String>,
    }

    impl AssetResolver for DeferringResolver {
        fn resolve(
            &mut self,
            asset_path: &str,
            _anchor: Option<LayerId>,
            _tokens: &TokenInterner,
            _paths: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            self.resolved.push(asset_path.into());
            let id = LayerId(100 + self.resolved.len() as u64);
            Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: Arc::from(asset_path),
                layer: Some(Layer::new(id)),
            })
        }

        fn resolved_path(&self, _id: LayerId) -> Option<&str> {
            None
        }

        fn defers_variant_arcs(&self) -> bool {
            true
        }
    }

    /// A resolver that cannot supply any asset yet.
    struct UnavailableResolver;

    impl AssetResolver for UnavailableResolver {
        fn resolve(
            &mut self,
            _asset_path: &str,
            _anchor: Option<LayerId>,
            _tokens: &TokenInterner,
            _paths: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::Unavailable)
        }

        fn resolved_path(&self, _id: LayerId) -> Option<&str> {
            None
        }
    }

    /// An arc whose asset the resolver cannot supply yet stays deferred,
    /// anchored to the layer, for composition to ask for again; a sublayer
    /// keeps its asset path with no layer, which composition resolves
    /// through the store; an asset that is not found stays unresolved.
    #[test]
    fn unavailable_assets_keep_their_arcs_deferred() {
        let src = "#usda 1.0\n(subLayers = [@sub.usda@])\ndef \"P\" (prepend references = @later.usda@) {}\n";
        let cst = parse_cst(src);
        let ast = crate::lower::lower(&cst.tree, src);
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let result = emit(
            &ast.layer,
            LayerId(1),
            &tokens,
            &paths,
            &mut UnavailableResolver,
        );
        assert_eq!(
            result.layer.sublayers(),
            [SublayerEntry::unresolved("sub.usda", LayerOffset::IDENTITY)]
        );
        let p = paths
            .lookup(&Path::parse_absolute("/P", &tokens).unwrap())
            .unwrap();
        let arc = &result.layer.prim(p).unwrap().references.prepend[0];
        assert!(arc.is_unresolved());
        assert_eq!(
            arc.source(),
            ReferenceSource::Asset {
                anchor: LayerId(1),
                path: "later.usda"
            }
        );
    }

    /// Arcs inside variant branches the layer selects itself, at every
    /// enclosing set, resolve; the rest are deferred to composition.
    ///
    /// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes).
    #[test]
    fn deferring_resolvers_resolve_only_locally_selected_branch_arcs() {
        let src = r#"#usda 1.0
def "P" (
    variants = { string a = "x" }
    prepend variantSets = "a"
    prepend references = @outside.usda@
) {
    variantSet "a" = {
        "x" (
            variants = { string b = "u" }
            prepend variantSets = "b"
            prepend references = @ax.usda@
        ) {
            variantSet "b" = {
                "u" (prepend references = @bu.usda@) {}
                "v" (prepend references = @bv.usda@) {}
            }
            def "C" (prepend references = @child.usda@) {}
        }
        "y" (prepend references = @ay.usda@) {
            def "C" (prepend references = @other.usda@) {}
        }
    }
}
"#;
        let cst = parse_cst(src);
        let ast = crate::lower::lower(&cst.tree, src);
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let mut resolver = DeferringResolver::default();
        let result = emit(&ast.layer, LayerId(1), &tokens, &paths, &mut resolver);
        let mut resolved = resolver.resolved.clone();
        resolved.sort();
        assert_eq!(
            resolved,
            ["ax.usda", "bu.usda", "child.usda", "outside.usda"]
        );
        assert_eq!(result.resolved_layers.len(), 4);

        let p = paths
            .lookup(&Path::parse_absolute("/P", &tokens).unwrap())
            .unwrap();
        let (a, y) = (tokens.intern("a"), tokens.intern("y"));
        let spec = result.layer.prim(p).unwrap();
        let deferred = spec.variant_spec(&[(a, y)]).unwrap().references.prepend[0].clone();
        assert!(deferred.is_unresolved());
        assert_eq!(
            deferred.source(),
            ReferenceSource::Asset {
                anchor: LayerId(1),
                path: "ay.usda"
            }
        );
        let c = paths
            .lookup(&Path::parse_absolute("/P/C", &tokens).unwrap())
            .unwrap();
        let deferred_children: Vec<_> = result
            .layer
            .prim_specs(c)
            .flat_map(|spec| spec.references.prepend.iter())
            .filter_map(|arc| match arc.source() {
                ReferenceSource::Asset { path, .. } => Some(String::from(path)),
                _ => None,
            })
            .collect();
        assert_eq!(deferred_children, ["other.usda"]);
    }
}
