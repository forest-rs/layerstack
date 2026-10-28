// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lowers a lossless CST into the typed AST.
//!
//! This module walks a [`SyntaxTree`] and produces the [`ast`](crate::ast)
//! types. Semantic logic (quote stripping, number parsing, keyword dispatch)
//! lives here rather than in the parser.

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use smallvec::SmallVec;

use crate::Span;
use crate::ast::*;
use crate::cst::{SyntaxKind, SyntaxNode, SyntaxTree};
use crate::diagnostic::Diagnostic;
use crate::writer::is_variant_selection;

/// Lowers a CST into an AST, producing diagnostics for semantic errors.
pub fn lower<'a>(tree: &SyntaxTree, source: &'a str) -> ParseResult<'a> {
    PrimLowering::new(source).finish(tree)
}

/// Lowers each prim as the parser closes it, so that the parser can drop
/// the prim's syntax and a reader holds the syntax of the prims it is in,
/// not the whole file's. The result, diagnostics and their order are those
/// of [`lower`] over the whole tree: lowering a prim reads its subtree
/// alone, and a prim's diagnostics join the enclosing prim's (or layer's)
/// where lowering the whole tree would report them.
pub(crate) struct PrimLowering<'a> {
    ctx: LowerCtx<'a>,
}

impl<'a> PrimLowering<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        Self {
            ctx: LowerCtx {
                source,
                diagnostics: Vec::new(),
                lowered: Vec::new(),
            },
        }
    }

    /// Where the prims closed inside one the parser opens now will start.
    pub(crate) fn mark(&self) -> usize {
        self.ctx.lowered.len()
    }

    /// Lowers the prim the parser has just closed at `node` of `tree`, the
    /// prims closed inside it starting at `mark`. The parser may then
    /// collapse `node` to a leaf: lowering finds the prim by its offset.
    pub(crate) fn close_prim(&mut self, tree: &SyntaxTree, node: u32, mark: usize) {
        let node = tree.node(crate::cst::NodeId(node));
        let start = node.span().start;
        let prim = self.ctx.lower_prim(node);
        let diagnostics = core::mem::take(&mut self.ctx.diagnostics);
        // The prims inside are part of this one now (or were not lowered by
        // it, as lowering the whole tree would not have lowered them).
        self.ctx.lowered.truncate(mark);
        self.ctx.lowered.push(LoweredPrim {
            start,
            prim: Some(prim),
            diagnostics,
        });
    }

    /// Lowers the rest of `tree`: the header, layer metadata and the root
    /// prims, whose lowering was done as they closed.
    pub(crate) fn finish(mut self, tree: &SyntaxTree) -> ParseResult<'a> {
        let layer = self.ctx.lower_source_file(tree.root());
        ParseResult {
            layer,
            diagnostics: self.ctx.diagnostics,
        }
    }
}

/// A prim lowered when the parser closed it, held with the diagnostics of
/// its lowering until the prim or layer it is in is lowered.
struct LoweredPrim<'a> {
    /// The prim's source offset, which its collapsed node keeps.
    start: u32,
    prim: Option<Prim<'a>>,
    diagnostics: Vec<Diagnostic>,
}

struct LowerCtx<'a> {
    source: &'a str,
    diagnostics: Vec<Diagnostic>,
    /// Prims lowered as they closed, in source order, that the prim or layer
    /// holding them has not taken yet.
    lowered: Vec<LoweredPrim<'a>>,
}

/// The kinds and ids of a node's non-trivia children, which lowering reads
/// by position: a statement's or entry's signature. Signatures are short, so
/// they stay inline.
type Signature = SmallVec<[(SyntaxKind, u32); 16]>;

fn signature(node: SyntaxNode<'_>) -> Signature {
    node.children_no_trivia()
        .map(|child| (child.kind(), child.id.0))
        .collect()
}

/// The children of `node` that are `kind` nodes, which is no trivia kind.
fn children_of(
    node: SyntaxNode<'_>,
    kind: SyntaxKind,
) -> impl Iterator<Item = SyntaxNode<'_>> + Clone {
    debug_assert!(!kind.is_trivia(), "trivia nodes are filtered as trivia");
    node.children().filter(move |child| child.kind() == kind)
}

impl<'a> LowerCtx<'a> {
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::error(span, message));
    }

    fn text(&self, node: SyntaxNode<'_>) -> &'a str {
        node.text(self.source)
    }

    /// Get a [`SyntaxNode`] from the tree by raw id.
    fn node_from<'t>(&self, tree: &'t SyntaxTree, id: u32) -> SyntaxNode<'t> {
        tree.node(crate::cst::NodeId(id))
    }

    // ── Source file ────────────────────────────────────────────────

    fn lower_source_file(&mut self, root: SyntaxNode<'_>) -> Layer<'a> {
        let span = root.span();
        let tree = root.tree();
        let mut version = "";
        let mut metadata = Vec::new();
        let mut prims = Vec::with_capacity(children_of(root, SyntaxKind::PrimDef).count());
        let mut root_prim_order = None;

        for child in root.children_no_trivia() {
            let (kind, id) = (child.kind(), child.id.0);
            match kind {
                SyntaxKind::Header => {
                    version = self.lower_header(self.node_from(tree, id));
                }
                SyntaxKind::LayerMetadata => {
                    metadata = self.lower_layer_metadata(self.node_from(tree, id));
                }
                SyntaxKind::PrimDef => {
                    prims.push(self.lower_prim(self.node_from(tree, id)));
                }
                SyntaxKind::ReorderStmt => {
                    root_prim_order =
                        Some(self.lower_name_list_from_reorder(self.node_from(tree, id)));
                }
                _ => {}
            }
        }

        Layer {
            span,
            version,
            metadata,
            prims,
            root_prim_order,
        }
    }

    // ── Header ─────────────────────────────────────────────────────

    fn lower_header(&mut self, node: SyntaxNode<'_>) -> &'a str {
        // The `#usda X.Y` header is lexed as a PythonComment (trivia), so we
        // must look at ALL children, not just non-trivia ones.
        for child in node.children() {
            if child.kind() == SyntaxKind::PythonComment {
                let text = self.text(child);
                if let Some(rest) = text.strip_prefix("#usda") {
                    return rest.trim();
                }
            }
        }
        ""
    }

    // ── Layer metadata ─────────────────────────────────────────────

    fn lower_layer_metadata(&mut self, node: SyntaxNode<'_>) -> Vec<LayerMeta<'a>> {
        let tree = node.tree();
        let mut items = Vec::with_capacity(
            node.children()
                .filter(|child| {
                    matches!(
                        child.kind(),
                        SyntaxKind::SubLayersList
                            | SyntaxKind::RelocatesMap
                            | SyntaxKind::LayerMetaEntry
                    )
                })
                .count(),
        );
        for child in node.children_no_trivia() {
            let (kind, id) = (child.kind(), child.id.0);
            match kind {
                SyntaxKind::SubLayersList => {
                    items.push(self.lower_sublayers(self.node_from(tree, id)));
                }
                SyntaxKind::RelocatesMap => {
                    items.push(self.lower_relocates(self.node_from(tree, id)));
                }
                SyntaxKind::LayerMetaEntry => {
                    if let Some(meta) = self.lower_layer_meta_entry(self.node_from(tree, id)) {
                        items.push(meta);
                    }
                }
                _ => {}
            }
        }
        items
    }

    fn lower_layer_meta_entry(&mut self, node: SyntaxNode<'_>) -> Option<LayerMeta<'a>> {
        if let Some(comment) = self.bare_comment(node) {
            return Some(LayerMeta::Custom(comment));
        }
        let key_node = node
            .children_no_trivia()
            .find(|c| c.kind() == SyntaxKind::Ident)?;
        let key = self.text(key_node);

        match key {
            "doc" => {
                let val = self.find_string_value(node);
                Some(LayerMeta::Doc(val))
            }
            _ => {
                let entry = self.lower_metadata_entry_from_node(node);
                Some(LayerMeta::Custom(entry))
            }
        }
    }

    fn lower_sublayers(&mut self, node: SyntaxNode<'_>) -> LayerMeta<'a> {
        let tree = node.tree();
        let item_ids = children_of(node, SyntaxKind::SubLayerItem);

        let mut items = Vec::with_capacity(item_ids.clone().count());
        for id in item_ids.map(|child| child.id.0) {
            items.push(self.lower_sublayer_item(self.node_from(tree, id)));
        }
        LayerMeta::SubLayers(items)
    }

    fn lower_sublayer_item(&mut self, node: SyntaxNode<'_>) -> SubLayerItem<'a> {
        let asset = self.find_asset_ref(node);
        let (offset, scale) = self.find_layer_offset_params(node);
        SubLayerItem {
            asset,
            offset,
            scale,
        }
    }

    fn lower_relocates(&mut self, node: SyntaxNode<'_>) -> LayerMeta<'a> {
        let tree = node.tree();
        let entry_ids = children_of(node, SyntaxKind::RelocateEntry);

        let mut entries = Vec::with_capacity(entry_ids.clone().count());
        for id in entry_ids.map(|child| child.id.0) {
            entries.push(self.lower_relocate_entry(self.node_from(tree, id)));
        }
        LayerMeta::Relocates(entries)
    }

    fn lower_relocate_entry(&mut self, node: SyntaxNode<'_>) -> RelocateEntry<'a> {
        let span = node.span();
        let mut paths = children_of(node, SyntaxKind::PathRef);
        let source = paths.next().map(|n| self.lower_path_ref(n)).unwrap_or("");
        let target = paths.next().map(|n| self.lower_path_ref(n)).unwrap_or("");
        RelocateEntry {
            span,
            source,
            target,
        }
    }

    // ── Prims ──────────────────────────────────────────────────────

    /// Takes the prim lowered when the parser closed it and collapsed its
    /// `node`, adding the diagnostics of its lowering where lowering it now
    /// would; `None` when `node` holds its syntax.
    fn take_lowered(&mut self, node: SyntaxNode<'_>) -> Option<Prim<'a>> {
        if node.children().len() != 0 {
            return None;
        }
        let start = node.span().start;
        let at = self
            .lowered
            .binary_search_by_key(&start, |lowered| lowered.start)
            .ok()?;
        let lowered = &mut self.lowered[at];
        let prim = lowered.prim.take()?;
        self.diagnostics.append(&mut lowered.diagnostics);
        Some(prim)
    }

    fn lower_prim(&mut self, node: SyntaxNode<'_>) -> Prim<'a> {
        if let Some(prim) = self.take_lowered(node) {
            return prim;
        }
        let span = node.span();
        let tree = node.tree();
        let sig = signature(node);

        let mut specifier = Specifier::Def;
        let mut type_name = None;
        let mut name = "";
        let mut metadata = Vec::new();
        let mut children = Vec::new();

        let mut idx = 0;

        // First ident = specifier.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            specifier = match self.text(self.node_from(tree, sig[idx].1)) {
                "def" => Specifier::Def,
                "over" => Specifier::Over,
                "class" => Specifier::Class,
                _ => Specifier::Def,
            };
            idx += 1;
        }

        // Optional type name (Ident followed by string).
        if idx + 1 < sig.len() && sig[idx].0 == SyntaxKind::Ident && is_string_kind(sig[idx + 1].0)
        {
            type_name = Some(self.text(self.node_from(tree, sig[idx].1)));
            idx += 1;
        }

        // Prim name (string).
        if idx < sig.len() && is_string_kind(sig[idx].0) {
            name = self.name_literal(self.node_from(tree, sig[idx].1));
            idx += 1;
        }

        // Remaining: metadata, body.
        for &(kind, id) in &sig[idx..] {
            match kind {
                SyntaxKind::PrimMetadata => {
                    metadata = self.lower_prim_metadata(self.node_from(tree, id));
                }
                SyntaxKind::PrimBody => {
                    children = self.lower_prim_body(self.node_from(tree, id));
                }
                _ => {}
            }
        }

        Prim {
            span,
            specifier,
            type_name,
            name,
            metadata,
            children,
        }
    }

    // ── Prim metadata ──────────────────────────────────────────────

    fn lower_prim_metadata(&mut self, node: SyntaxNode<'_>) -> Vec<PrimMeta<'a>> {
        let tree = node.tree();
        let entry_ids = children_of(node, SyntaxKind::PrimMetaEntry);

        let mut items = Vec::with_capacity(entry_ids.clone().count());
        for id in entry_ids.map(|child| child.id.0) {
            if let Some(meta) = self.lower_prim_meta_entry(self.node_from(tree, id)) {
                items.push(meta);
            }
        }
        items
    }

    fn lower_prim_meta_entry(&mut self, node: SyntaxNode<'_>) -> Option<PrimMeta<'a>> {
        if let Some(comment) = self.bare_comment(node) {
            return Some(PrimMeta::Custom(comment));
        }
        let tree = node.tree();
        let sig = signature(node);

        let mut idx = 0;
        let mut op = ListOpKind::Explicit;

        // Detect list-op prefix.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            match self.text(self.node_from(tree, sig[idx].1)) {
                "prepend" => {
                    op = ListOpKind::Prepend;
                    idx += 1;
                }
                "append" => {
                    op = ListOpKind::Append;
                    idx += 1;
                }
                "add" => {
                    op = ListOpKind::Add;
                    idx += 1;
                }
                "delete" => {
                    op = ListOpKind::Delete;
                    idx += 1;
                }
                "reorder" => {
                    op = ListOpKind::Reorder;
                    idx += 1;
                }
                _ => {}
            }
        }

        if idx >= sig.len() {
            return None;
        }

        let key = if sig[idx].0 == SyntaxKind::Ident {
            self.text(self.node_from(tree, sig[idx].1))
        } else {
            return None;
        };

        match key {
            "references" => {
                let arc_list = sig.iter().find(|s| s.0 == SyntaxKind::ArcList);
                let arc_node = arc_list.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::References(self.lower_arc_listop(
                    arc_node,
                    op,
                    node.span(),
                )))
            }
            "inherits" => {
                let pl = sig.iter().find(|s| s.0 == SyntaxKind::PathList);
                let pl_node = pl.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Inherits(self.lower_path_listop(
                    pl_node,
                    op,
                    node.span(),
                )))
            }
            "specializes" => {
                let pl = sig.iter().find(|s| s.0 == SyntaxKind::PathList);
                let pl_node = pl.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Specializes(self.lower_path_listop(
                    pl_node,
                    op,
                    node.span(),
                )))
            }
            "payload" => {
                let arc_list = sig.iter().find(|s| s.0 == SyntaxKind::ArcList);
                let arc_node = arc_list.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Payload(self.lower_arc_listop(
                    arc_node,
                    op,
                    node.span(),
                )))
            }
            "variants" => {
                let sel = sig.iter().find(|s| s.0 == SyntaxKind::VariantSelections);
                let sel_node = sel.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Variants(self.lower_variant_selections(sel_node)))
            }
            "variantSets" => {
                let nl = sig.iter().find(|s| s.0 == SyntaxKind::NameList);
                let nl_node = nl.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::VariantSets(self.lower_name_listop(
                    nl_node,
                    op,
                    node.span(),
                )))
            }
            "kind" => {
                let val = self.find_string_value(node);
                Some(PrimMeta::Kind(val))
            }
            "doc" => {
                let val = self.find_string_value(node);
                Some(PrimMeta::Doc(val))
            }
            _ => {
                let entry = self.lower_metadata_entry_from_node(node);
                Some(PrimMeta::Custom(entry))
            }
        }
    }

    // ── Prim body ──────────────────────────────────────────────────

    fn lower_prim_body(&mut self, node: SyntaxNode<'_>) -> Vec<PrimChild<'a>> {
        let tree = node.tree();
        let mut children = Vec::with_capacity(
            node.children()
                .filter(|child| {
                    matches!(
                        child.kind(),
                        SyntaxKind::PrimDef
                            | SyntaxKind::AttributeDef
                            | SyntaxKind::RelationshipDef
                            | SyntaxKind::VariantSetDef
                            | SyntaxKind::ReorderStmt
                    )
                })
                .count(),
        );
        for child in node.children_no_trivia() {
            let (kind, id) = (child.kind(), child.id.0);
            match kind {
                SyntaxKind::PrimDef => {
                    children.push(PrimChild::Prim(self.lower_prim(self.node_from(tree, id))));
                }
                SyntaxKind::AttributeDef => {
                    children.push(PrimChild::Attribute(
                        self.lower_attribute(self.node_from(tree, id)),
                    ));
                }
                SyntaxKind::RelationshipDef => {
                    children.push(PrimChild::Relationship(
                        self.lower_relationship(self.node_from(tree, id)),
                    ));
                }
                SyntaxKind::VariantSetDef => {
                    children.push(PrimChild::VariantSet(
                        self.lower_variant_set(self.node_from(tree, id)),
                    ));
                }
                SyntaxKind::ReorderStmt => {
                    children.push(self.lower_reorder_stmt(self.node_from(tree, id)));
                }
                _ => {}
            }
        }
        children
    }

    // ── Attributes ─────────────────────────────────────────────────

    fn lower_attribute(&mut self, node: SyntaxNode<'_>) -> Attribute<'a> {
        let span = node.span();
        let tree = node.tree();
        let sig = signature(node);

        let mut custom = false;
        let mut uniform = false;
        let mut type_name = "";
        let mut is_array = false;
        let mut name = "";
        let mut default = None;
        let mut time_samples = None;
        let mut spline = None;
        let mut spline_custom_data = Vec::new();
        let mut connection = None;
        let mut metadata = Vec::new();
        let mut list_op = ListOpKind::Explicit;

        let mut idx = 0;

        // Optional list-op prefix.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            match self.text(self.node_from(tree, sig[idx].1)) {
                "prepend" => {
                    list_op = ListOpKind::Prepend;
                    idx += 1;
                }
                "append" => {
                    list_op = ListOpKind::Append;
                    idx += 1;
                }
                "add" => {
                    list_op = ListOpKind::Add;
                    idx += 1;
                }
                "delete" => {
                    list_op = ListOpKind::Delete;
                    idx += 1;
                }
                "reorder" => {
                    list_op = ListOpKind::Reorder;
                    idx += 1;
                }
                _ => {}
            }
        }

        // `custom`
        if idx < sig.len()
            && sig[idx].0 == SyntaxKind::Ident
            && self.text(self.node_from(tree, sig[idx].1)) == "custom"
        {
            custom = true;
            idx += 1;
        }

        // `uniform`
        if idx < sig.len()
            && sig[idx].0 == SyntaxKind::Ident
            && self.text(self.node_from(tree, sig[idx].1)) == "uniform"
        {
            uniform = true;
            idx += 1;
        }

        // type name, a namespaced name when the name follows it (`Some::Type`)
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            type_name = current_type_name(self.text(self.node_from(tree, sig[idx].1)));
            idx += 1;
        } else if idx + 1 < sig.len()
            && sig[idx].0 == SyntaxKind::NamespacedName
            && matches!(
                sig[idx + 1].0,
                SyntaxKind::NamespacedName | SyntaxKind::Ident | SyntaxKind::LeftBracket
            )
        {
            type_name = self.node_from(tree, sig[idx].1).span().text(self.source);
            idx += 1;
        }

        // array suffix
        if idx < sig.len() && sig[idx].0 == SyntaxKind::LeftBracket {
            is_array = true;
            idx += 1;
            if idx < sig.len() && sig[idx].0 == SyntaxKind::RightBracket {
                idx += 1;
            }
        }

        // name (namespaced or simple ident)
        if idx < sig.len() && sig[idx].0 == SyntaxKind::NamespacedName {
            name = self.node_from(tree, sig[idx].1).span().text(self.source);
            idx += 1;
        } else if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            name = self.text(self.node_from(tree, sig[idx].1));
            idx += 1;
        }

        // Remaining children.
        for &(kind, id) in &sig[idx..] {
            match kind {
                SyntaxKind::ValueExpr => {
                    default = Some(self.lower_value(self.node_from(tree, id)));
                }
                SyntaxKind::TimeSamplesSuffix => {
                    time_samples = Some(self.lower_time_samples(self.node_from(tree, id)));
                }
                SyntaxKind::SplineSuffix => {
                    let node = self.node_from(tree, id);
                    if let Some((data, times)) = self.lower_spline(node, type_name, is_array) {
                        let dictionaries = children_of(node, SyntaxKind::DictionaryValue);
                        for (time, dictionary) in times.into_iter().zip(dictionaries) {
                            spline_custom_data.push((time, self.lower_dictionary(dictionary)));
                        }
                        spline = Some(Box::new(data));
                    }
                }
                SyntaxKind::ConnectionSuffix => {
                    let targets = self.lower_connection(self.node_from(tree, id));
                    connection = Some(Connection {
                        span,
                        op: list_op,
                        targets,
                    });
                }
                SyntaxKind::PrimMetadata => {
                    metadata = self.lower_attribute_metadata(self.node_from(tree, id));
                }
                _ => {}
            }
        }

        Attribute {
            span,
            custom,
            uniform,
            type_name,
            is_array,
            name,
            default,
            time_samples,
            spline_custom_data,
            spline,
            connection,
            metadata,
        }
    }

    /// Reads a `.spline = { ... }` value of an attribute of `type_name`.
    /// A spline holds `double`, `float` or `half` values.
    ///
    /// Spec: AOUSD Core §12.3.3 (spline opinions).
    fn lower_spline(
        &mut self,
        node: SyntaxNode<'_>,
        type_name: &str,
        is_array: bool,
    ) -> Option<(layerstack::spline::SplineData, Vec<f64>)> {
        use layerstack::spline::SplineDataType;
        let span = node.span();
        let data_type = match (type_name, is_array) {
            ("double", false) => SplineDataType::Double,
            ("float", false) => SplineDataType::Float,
            ("half", false) => SplineDataType::Half,
            ("timecode", false) => SplineDataType::TimeCode,
            // OpenUSD 26.11 `Ts_GetTypeFromTypeName` (`ts/typeHelpers.cpp`).
            ("duration", false) => SplineDataType::Duration,
            _ => {
                self.error(
                    span,
                    alloc::format!("unsupported: a spline of `{type_name}` values"),
                );
                return None;
            }
        };
        let text = self.text(node);
        let start = text.find('{')?;
        match crate::spline_text::parse(&text[start..], data_type) {
            Ok(spline) => Some(spline),
            Err(message) => {
                self.error(span, alloc::format!("invalid spline: {message}"));
                None
            }
        }
    }

    fn lower_attribute_metadata(&mut self, node: SyntaxNode<'_>) -> Vec<MetadataEntry<'a>> {
        let tree = node.tree();
        let entry_ids = children_of(node, SyntaxKind::MetadataEntry);

        let mut items = Vec::with_capacity(entry_ids.clone().count());
        for id in entry_ids.map(|child| child.id.0) {
            items.push(self.lower_metadata_entry(self.node_from(tree, id)));
        }
        items
    }

    // ── Relationships ──────────────────────────────────────────────

    fn lower_relationship(&mut self, node: SyntaxNode<'_>) -> Relationship<'a> {
        let span = node.span();
        let tree = node.tree();
        let sig = signature(node);

        let mut op = ListOpKind::Explicit;
        let mut custom = false;
        let mut varying = false;
        let mut name = "";
        let mut targets = None;
        let mut default = None;
        let mut metadata = Vec::new();
        let mut idx = 0;

        // list-op prefix
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            match self.text(self.node_from(tree, sig[idx].1)) {
                "prepend" => {
                    op = ListOpKind::Prepend;
                    idx += 1;
                }
                "append" => {
                    op = ListOpKind::Append;
                    idx += 1;
                }
                "add" => {
                    op = ListOpKind::Add;
                    idx += 1;
                }
                "delete" => {
                    op = ListOpKind::Delete;
                    idx += 1;
                }
                "reorder" => {
                    op = ListOpKind::Reorder;
                    idx += 1;
                }
                _ => {}
            }
        }

        // `custom`
        if idx < sig.len()
            && sig[idx].0 == SyntaxKind::Ident
            && self.text(self.node_from(tree, sig[idx].1)) == "custom"
        {
            custom = true;
            idx += 1;
        }

        // `varying`
        if idx < sig.len()
            && sig[idx].0 == SyntaxKind::Ident
            && self.text(self.node_from(tree, sig[idx].1)) == "varying"
        {
            varying = true;
            idx += 1;
        }

        // `rel`
        if idx < sig.len()
            && sig[idx].0 == SyntaxKind::Ident
            && self.text(self.node_from(tree, sig[idx].1)) == "rel"
        {
            idx += 1;
        }

        // name
        if idx < sig.len() && sig[idx].0 == SyntaxKind::NamespacedName {
            name = self.node_from(tree, sig[idx].1).span().text(self.source);
            idx += 1;
        } else if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            name = self.text(self.node_from(tree, sig[idx].1));
            idx += 1;
        }

        // Remaining children.
        for &(kind, id) in &sig[idx..] {
            match kind {
                SyntaxKind::PathList => {
                    targets = Some(self.lower_path_list(self.node_from(tree, id)));
                }
                SyntaxKind::PathRef => {
                    targets = Some(vec![self.lower_path_ref(self.node_from(tree, id))]);
                }
                // `rel r = None` authors an explicitly empty target list,
                // which blocks weaker targets; it is not a bare declaration
                // (AOUSD Core §7.6.5.1.1, §16.2.16.9).
                SyntaxKind::Ident if self.text(self.node_from(tree, id)) == "None" => {
                    targets = Some(Vec::new());
                }
                SyntaxKind::PrimMetadata => {
                    metadata = self.lower_attribute_metadata(self.node_from(tree, id));
                }
                SyntaxKind::DefaultSuffix => {
                    default = self
                        .node_from(tree, id)
                        .child_by_kind(SyntaxKind::PathRef)
                        .map(|path| self.lower_path_ref(path));
                }
                _ => {}
            }
        }

        Relationship {
            span,
            custom,
            varying,
            op,
            name,
            targets,
            default,
            metadata,
        }
    }

    // ── Variant sets ───────────────────────────────────────────────

    fn lower_variant_set(&mut self, node: SyntaxNode<'_>) -> VariantSet<'a> {
        let span = node.span();
        let mut name = "";
        let mut branches = Vec::with_capacity(children_of(node, SyntaxKind::VariantBranch).count());

        for child in node.children_no_trivia() {
            match child.kind() {
                k if is_string_kind(k) => {
                    name = self.name_literal(child);
                }
                SyntaxKind::VariantBranch => {
                    branches.push(self.lower_variant_branch(child));
                }
                _ => {}
            }
        }

        VariantSet {
            span,
            name,
            branches,
        }
    }

    fn lower_variant_branch(&mut self, node: SyntaxNode<'_>) -> VariantBranch<'a> {
        let tree = node.tree();
        let sig = signature(node);

        let mut name = "";
        let mut metadata = Vec::new();
        let mut children = Vec::new();

        for (kind, id) in &sig {
            match *kind {
                k if is_string_kind(k) => {
                    name = self.name_literal(self.node_from(tree, *id));
                }
                SyntaxKind::PrimMetadata => {
                    metadata = self.lower_prim_metadata(self.node_from(tree, *id));
                }
                SyntaxKind::PrimBody => {
                    children = self.lower_prim_body(self.node_from(tree, *id));
                }
                _ => {}
            }
        }

        VariantBranch {
            name,
            metadata,
            children,
        }
    }

    fn lower_reorder_stmt(&mut self, node: SyntaxNode<'_>) -> PrimChild<'a> {
        let kind = children_of(node, SyntaxKind::Ident)
            .nth(1)
            .map(|ident| self.text(ident))
            .unwrap_or("");
        let names = self.lower_name_list_from_reorder(node);
        match kind {
            "nameChildren" => PrimChild::ReorderNameChildren(names),
            "properties" => PrimChild::ReorderProperties(names),
            _ => PrimChild::ReorderNameChildren(names),
        }
    }

    fn lower_name_list_from_reorder(&mut self, node: SyntaxNode<'_>) -> Vec<&'a str> {
        let nl = node
            .children_no_trivia()
            .find(|c| c.kind() == SyntaxKind::NameList);
        if let Some(nl) = nl {
            return self.lower_name_list(nl);
        }
        Vec::new()
    }

    // ── Composition arcs ───────────────────────────────────────────

    fn lower_arc_listop(
        &mut self,
        list_node: Option<SyntaxNode<'_>>,
        kind: ListOpKind,
        span: Span,
    ) -> ListOpArc<'a> {
        let Some(node) = list_node else {
            return ListOpArc {
                span,
                kind,
                items: None,
            };
        };
        // Check for None keyword.
        if node
            .children_no_trivia()
            .any(|c| c.kind() == SyntaxKind::Ident && self.text(c) == "None")
        {
            return ListOpArc {
                span,
                kind,
                items: None,
            };
        }
        let tree = node.tree();
        let arc_ids = children_of(node, SyntaxKind::ArcRef);

        let mut items = Vec::with_capacity(arc_ids.clone().count());
        for id in arc_ids.map(|child| child.id.0) {
            items.push(self.lower_arc_ref(self.node_from(tree, id)));
        }
        ListOpArc {
            span,
            kind,
            items: Some(items),
        }
    }

    fn lower_arc_ref(&mut self, node: SyntaxNode<'_>) -> ArcRef<'a> {
        let tree = node.tree();
        let sig = signature(node);

        let mut asset = None;
        let mut prim_path = None;
        let (mut offset, mut scale) = (None, None);
        let mut custom_data = Vec::new();

        for (kind, id) in &sig {
            match *kind {
                SyntaxKind::AssetRef => {
                    asset = Some(self.lower_asset_ref(self.node_from(tree, *id)));
                }
                SyntaxKind::PathRef => {
                    prim_path = Some(self.lower_path_ref(self.node_from(tree, *id)));
                }
                SyntaxKind::LayerOffsetParams => {
                    let (o, s) = self.lower_layer_offset_params(self.node_from(tree, *id));
                    offset = o;
                    scale = s;
                    let params = self.node_from(tree, *id);
                    if let Some(dict) = params
                        .children_no_trivia()
                        .find(|c| c.kind() == SyntaxKind::DictionaryValue)
                    {
                        custom_data = self.lower_dictionary(dict);
                    }
                }
                _ => {}
            }
        }

        ArcRef {
            custom_data,
            span: node.span(),
            asset,
            prim_path,
            offset,
            scale,
        }
    }

    fn lower_path_listop(
        &mut self,
        list_node: Option<SyntaxNode<'_>>,
        kind: ListOpKind,
        span: Span,
    ) -> ListOpPaths<'a> {
        let Some(node) = list_node else {
            return ListOpPaths {
                span,
                kind,
                items: None,
            };
        };
        if node
            .children_no_trivia()
            .any(|c| c.kind() == SyntaxKind::Ident && self.text(c) == "None")
        {
            return ListOpPaths {
                span,
                kind,
                items: None,
            };
        }
        let items = self.lower_path_list(node);
        ListOpPaths {
            span,
            kind,
            items: Some(items),
        }
    }

    fn lower_path_list(&mut self, node: SyntaxNode<'_>) -> Vec<&'a str> {
        let refs = children_of(node, SyntaxKind::PathRef);
        let mut paths = Vec::with_capacity(refs.clone().count());
        paths.extend(refs.map(|c| self.lower_path_ref(c)));
        paths
    }

    fn lower_name_listop(
        &mut self,
        list_node: Option<SyntaxNode<'_>>,
        kind: ListOpKind,
        span: Span,
    ) -> ListOp<&'a str> {
        let Some(node) = list_node else {
            return ListOp {
                span,
                kind,
                items: None,
            };
        };
        if node
            .children_no_trivia()
            .any(|c| c.kind() == SyntaxKind::Ident && self.text(c) == "None")
        {
            return ListOp {
                span,
                kind,
                items: None,
            };
        }
        let items = self.lower_name_list(node);
        ListOp {
            span,
            kind,
            items: Some(items),
        }
    }

    fn lower_name_list(&mut self, node: SyntaxNode<'_>) -> Vec<&'a str> {
        let strings = node.children().filter(|c| is_string_kind(c.kind()));
        let mut names = Vec::with_capacity(strings.clone().count());
        names.extend(strings.map(|c| self.name_literal(c)));
        names
    }

    fn lower_variant_selections(
        &mut self,
        node: Option<SyntaxNode<'_>>,
    ) -> Vec<VariantSelection<'a>> {
        let Some(node) = node else {
            return Vec::new();
        };
        let tree = node.tree();
        let entry_ids = children_of(node, SyntaxKind::VariantSelectionEntry);

        let mut selections = Vec::with_capacity(entry_ids.clone().count());
        for id in entry_ids.map(|child| child.id.0) {
            selections.push(self.lower_variant_selection_entry(self.node_from(tree, id)));
        }
        selections
    }

    /// One `variants` entry, which OpenUSD reads as a dictionary entry
    /// (`TextParserAction<VariantsMetadata>`,
    /// `pxr/usd/sdf/textFileFormatParser.cpp`): a quoted set name and the
    /// selection have their escape sequences evaluated, as in any string; a
    /// typed entry must be a `string`; and the selection must be one
    /// `SdfSchemaBase::IsValidVariantSelection` accepts.
    ///
    /// Spec: AOUSD Core §16.2.17.3 (`variants`), §7.6.2.3.4
    /// (`variantSelection`).
    fn lower_variant_selection_entry(&mut self, node: SyntaxNode<'_>) -> VariantSelection<'a> {
        let tree = node.tree();
        let sig = signature(node);

        let mut set_name = Cow::Borrowed("");
        let mut branch_name = Cow::Borrowed("");

        let strings: Signature = sig
            .iter()
            .filter(|s| is_string_kind(s.0))
            .copied()
            .collect();
        let idents: Signature = sig
            .iter()
            .filter(|s| s.0 == SyntaxKind::Ident)
            .copied()
            .collect();

        // A type before an identifier or a quoted set name.
        let type_name =
            (idents.len() >= 2 || (!idents.is_empty() && strings.len() >= 2)).then(|| idents[0]);
        if let Some(type_name) = type_name {
            let type_node = self.node_from(tree, type_name.1);
            if self.text(type_node) != "string" {
                self.error(type_node.span(), "variant name must be a string");
            }
        }
        let Some(&value) = strings.last() else {
            return VariantSelection {
                set_name,
                branch_name,
            };
        };
        let value = self.node_from(tree, value.1);
        if strings.len() >= 2 {
            set_name = self.string_literal(self.node_from(tree, strings[0].1));
            branch_name = self.string_literal(value);
        } else if !idents.is_empty() {
            if let Some(key) = idents.get(usize::from(type_name.is_some())) {
                set_name = Cow::Borrowed(self.text(self.node_from(tree, key.1)));
            }
            branch_name = self.string_literal(value);
        }
        if !is_variant_selection(&branch_name) {
            self.error(
                value.span(),
                alloc::format!("{branch_name:?} is not a valid variant selection"),
            );
        }

        VariantSelection {
            set_name,
            branch_name,
        }
    }

    // ── Values ─────────────────────────────────────────────────────

    fn lower_value(&mut self, node: SyntaxNode<'_>) -> Value<'a> {
        let tree = node.tree();
        let mut children = node.children_no_trivia();
        let Some(first) = children.next() else {
            return Value::Blocked;
        };

        // Negated number. As in OpenUSD (`_GetNumericValueFromString`,
        // `pxr/usd/sdf/textParserHelpers.cpp`), `-0` is the double `-0.0`
        // rather than the integer zero, and `-inf` is negative infinity.
        if first.kind() == SyntaxKind::Minus
            && let Some(second) = children.next()
        {
            let text = self.text(second);
            match second.kind() {
                SyntaxKind::Number if text == "0" => return Value::Number(-0.0),
                SyntaxKind::Number => {
                    return match parse_number_value(text) {
                        Value::Int(n) => Value::Int(-n),
                        // The signed minimum has a positive magnitude one past
                        // i64::MAX, but is still an exact int64 literal. AOUSD
                        // Core §6.2; OpenUSD `_HelperGetNumericValueFromString`.
                        Value::UInt(n) if n == i64::MAX as u64 + 1 => Value::Int(i64::MIN),
                        // Below `i64::MIN`: a double, as in OpenUSD.
                        Value::UInt(n) => Value::Number(-(n as f64)),
                        Value::Number(n) => Value::Number(-n),
                        other => other,
                    };
                }
                SyntaxKind::Ident if text == "inf" => return Value::Number(f64::NEG_INFINITY),
                _ => {}
            }
        }

        let (kind, id) = (first.kind(), first.id.0);
        match kind {
            SyntaxKind::Number => {
                let text = self.text(self.node_from(tree, id));
                parse_number_value(text)
            }
            k if is_string_kind(k) => Value::String(self.string_literal(self.node_from(tree, id))),
            SyntaxKind::AssetRef => Value::Asset(self.lower_asset_ref(self.node_from(tree, id))),
            SyntaxKind::PathRef => Value::Path(self.lower_path_ref(self.node_from(tree, id))),
            SyntaxKind::TupleValue => Value::Tuple(self.lower_tuple(self.node_from(tree, id))),
            SyntaxKind::ArrayValue => Value::Array(self.lower_array(self.node_from(tree, id))),
            SyntaxKind::ArrayEditValue => {
                Value::ArrayEdit(self.lower_array_edit(self.node_from(tree, id)))
            }
            SyntaxKind::DictionaryValue => {
                Value::Dictionary(self.lower_dictionary(self.node_from(tree, id)))
            }
            // A C++ namespaced identifier, `Ns::fn` (`symmetryFunction`).
            SyntaxKind::NamespacedName => {
                Value::Identifier(self.node_from(tree, id).span().text(self.source))
            }
            SyntaxKind::Ident => {
                let text = self.text(self.node_from(tree, id));
                match text {
                    "true" | "True" => Value::Bool(true),
                    "false" | "False" => Value::Bool(false),
                    "None" => Value::Blocked,
                    "AnimationBlock" => Value::AnimationBlock,
                    "inf" => Value::Number(f64::INFINITY),
                    "nan" => Value::Number(f64::NAN),
                    _ => Value::Identifier(text),
                }
            }
            _ => Value::Blocked,
        }
    }

    fn lower_tuple(&mut self, node: SyntaxNode<'_>) -> Vec<Value<'a>> {
        let children = node
            .children()
            .filter(|child| child.kind() == SyntaxKind::ValueExpr);
        let mut values = Vec::with_capacity(children.clone().count());
        values.extend(children.map(|child| self.lower_value(child)));
        values
    }

    fn lower_array(&mut self, node: SyntaxNode<'_>) -> Vec<Value<'a>> {
        let children = node
            .children()
            .filter(|child| child.kind() == SyntaxKind::ValueExpr);
        let mut values = Vec::with_capacity(children.clone().count());
        values.extend(children.map(|child| self.lower_value(child)));
        values
    }

    fn lower_array_edit(&mut self, node: SyntaxNode<'_>) -> ArrayEdit<'a> {
        let nodes = children_of(node, SyntaxKind::ArrayEditInstruction);
        let mut instructions = Vec::with_capacity(nodes.clone().count());
        instructions.extend(nodes.map(|child| self.lower_array_edit_instruction(child)));
        ArrayEdit { instructions }
    }

    fn lower_array_edit_instruction(&mut self, node: SyntaxNode<'_>) -> ArrayEditInstruction<'a> {
        let tree = node.tree();
        let sig = signature(node);

        let keyword = sig
            .iter()
            .find_map(|(kind, id)| {
                (*kind == SyntaxKind::Ident).then(|| self.text(self.node_from(tree, *id)))
            })
            .unwrap_or("");

        match keyword {
            "write" => ArrayEditInstruction::Write {
                src: self.lower_array_edit_operand(node),
                index: self
                    .lower_last_array_edit_index(node)
                    .unwrap_or(ArrayEditIndex::Position(0)),
            },
            "insert" => ArrayEditInstruction::Insert {
                src: self.lower_array_edit_operand(node),
                index: self
                    .lower_last_array_edit_index(node)
                    .unwrap_or(ArrayEditIndex::Position(0)),
            },
            "prepend" => ArrayEditInstruction::Insert {
                src: self.lower_array_edit_operand(node),
                index: ArrayEditIndex::Position(0),
            },
            "append" => ArrayEditInstruction::Insert {
                src: self.lower_array_edit_operand(node),
                index: ArrayEditIndex::End,
            },
            "erase" => ArrayEditInstruction::Erase {
                index: self
                    .lower_array_edit_index(node)
                    .unwrap_or(ArrayEditIndex::Position(0)),
            },
            "minsize" => {
                let len = self.lower_array_edit_size(node);
                match self.lower_array_edit_fill(node) {
                    Some(fill) => ArrayEditInstruction::MinSizeFill { len, fill },
                    None => ArrayEditInstruction::MinSize(len),
                }
            }
            "maxsize" => ArrayEditInstruction::MaxSize(self.lower_array_edit_size(node)),
            "resize" => {
                let len = self.lower_array_edit_size(node);
                match self.lower_array_edit_fill(node) {
                    Some(fill) => ArrayEditInstruction::ResizeFill { len, fill },
                    None => ArrayEditInstruction::Resize(len),
                }
            }
            // The parser has reported the unknown instruction.
            _ => ArrayEditInstruction::Resize(0),
        }
    }

    fn lower_array_edit_operand(&mut self, node: SyntaxNode<'_>) -> ArrayEditOperand<'a> {
        let tree = node.tree();
        for child in node.children_no_trivia() {
            match child.kind() {
                SyntaxKind::ArrayEditIndex => {
                    if let Some(index) =
                        self.lower_array_edit_index(self.node_from(tree, child.id.0))
                    {
                        return ArrayEditOperand::CopyFrom(index);
                    }
                }
                SyntaxKind::ValueExpr => {
                    return ArrayEditOperand::Literal(
                        self.lower_value(self.node_from(tree, child.id.0)),
                    );
                }
                _ => {}
            }
        }
        ArrayEditOperand::Literal(Value::Blocked)
    }

    fn lower_array_edit_index(&mut self, node: SyntaxNode<'_>) -> Option<ArrayEditIndex> {
        let tree = node.tree();
        let sig = signature(node);

        let mut negative = false;
        for (kind, id) in sig {
            match kind {
                SyntaxKind::ArrayEditIndex => {
                    return self.lower_array_edit_index(self.node_from(tree, id));
                }
                SyntaxKind::Minus => negative = true,
                SyntaxKind::Number => {
                    let raw = self.text(self.node_from(tree, id));
                    // Parse the signed spelling together: negating a parsed
                    // magnitude cannot represent i64::MIN, Vt's EndIndex.
                    let signed = if negative {
                        alloc::format!("-{raw}")
                    } else {
                        String::from(raw)
                    };
                    let parsed = signed.parse::<i64>().ok()?;
                    return Some(if parsed == i64::MIN {
                        ArrayEditIndex::End
                    } else {
                        ArrayEditIndex::Position(parsed)
                    });
                }
                _ => {}
            }
        }
        None
    }

    /// The `fill` literal of `minsize N fill <literal>` or `resize N fill
    /// <literal>`: the value after the size.
    fn lower_array_edit_fill(&mut self, node: SyntaxNode<'_>) -> Option<Value<'a>> {
        let tree = node.tree();
        let fill = node
            .children_no_trivia()
            .filter(|child| child.kind() == SyntaxKind::ValueExpr)
            .nth(1)?;
        Some(self.lower_value(self.node_from(tree, fill.id.0)))
    }

    fn lower_array_edit_size(&mut self, node: SyntaxNode<'_>) -> usize {
        let tree = node.tree();
        node.children_no_trivia()
            .find(|child| child.kind() == SyntaxKind::ValueExpr)
            .map(|child| self.lower_value(self.node_from(tree, child.id.0)))
            .and_then(|value| match value {
                Value::Int(v) if v >= 0 => usize::try_from(v).ok(),
                Value::UInt(v) => usize::try_from(v).ok(),
                Value::Number(v) if v >= 0.0 => usize::try_from(v as i64).ok(),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn lower_last_array_edit_index(&mut self, node: SyntaxNode<'_>) -> Option<ArrayEditIndex> {
        children_of(node, SyntaxKind::ArrayEditIndex)
            .last()
            .and_then(|child| self.lower_array_edit_index(child))
    }

    fn lower_dictionary(&mut self, node: SyntaxNode<'_>) -> Vec<DictionaryEntry<'a>> {
        let nodes = children_of(node, SyntaxKind::DictionaryEntry);
        let mut entries = Vec::with_capacity(nodes.clone().count());
        entries.extend(nodes.map(|entry| self.lower_dictionary_entry(entry)));
        entries
    }

    fn lower_dictionary_entry(&mut self, node: SyntaxNode<'_>) -> DictionaryEntry<'a> {
        let tree = node.tree();
        let sig = signature(node);

        let mut type_name = None;
        let mut key = Cow::Borrowed("");
        let mut value = Value::Blocked;
        let mut idx = 0;

        // Optional type or `dictionary`.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            let t = self.text(self.node_from(tree, sig[idx].1));
            if t == "dictionary" {
                idx += 1;
            } else {
                type_name = Some(current_type_name(t));
                idx += 1;
                // Skip array suffix.
                if idx < sig.len() && sig[idx].0 == SyntaxKind::LeftBracket {
                    idx += 1;
                    if idx < sig.len() && sig[idx].0 == SyntaxKind::RightBracket {
                        idx += 1;
                    }
                }
            }
        }

        // Key.
        if idx < sig.len() {
            if is_string_kind(sig[idx].0) {
                key = self.string_literal(self.node_from(tree, sig[idx].1));
                idx += 1;
            } else if sig[idx].0 == SyntaxKind::Ident {
                key = Cow::Borrowed(self.text(self.node_from(tree, sig[idx].1)));
                idx += 1;
            }
        }

        // Skip `=`, or the `:` of a string dictionary item (`"key":
        // "value"`), whose value is a string.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Equals {
            idx += 1;
        } else if idx < sig.len() && sig[idx].0 == SyntaxKind::Colon {
            type_name = Some("string");
            idx += 1;
        }

        // Value.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::ValueExpr {
            value = self.lower_value(self.node_from(tree, sig[idx].1));
        }

        DictionaryEntry {
            type_name,
            key,
            value,
        }
    }

    fn lower_time_samples(&mut self, node: SyntaxNode<'_>) -> Vec<TimeSample<'a>> {
        // Find TimeSampleMap child.
        let Some(map_node) = node.child_by_kind(SyntaxKind::TimeSampleMap) else {
            return Vec::new();
        };
        let entries = children_of(map_node, SyntaxKind::TimeSampleEntry);
        let mut samples = Vec::with_capacity(entries.clone().count());
        samples.extend(entries.map(|entry| self.lower_time_sample_entry(entry)));
        samples
    }

    fn lower_time_sample_entry(&mut self, node: SyntaxNode<'_>) -> TimeSample<'a> {
        let tree = node.tree();
        let sig = signature(node);

        let mut time = 0.0;
        let mut value = None;
        let mut idx = 0;

        let negative = if idx < sig.len() && sig[idx].0 == SyntaxKind::Minus {
            idx += 1;
            true
        } else {
            false
        };

        if idx < sig.len() && sig[idx].0 == SyntaxKind::Number {
            time = parse_f64(self.text(self.node_from(tree, sig[idx].1)));
            if negative {
                time = -time;
            }
            idx += 1;
        }

        // Skip colon.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Colon {
            idx += 1;
        }

        if idx < sig.len() {
            if sig[idx].0 == SyntaxKind::Ident
                && self.text(self.node_from(tree, sig[idx].1)) == "None"
            {
                value = None;
            } else if sig[idx].0 == SyntaxKind::ValueExpr {
                value = Some(self.lower_value(self.node_from(tree, sig[idx].1)));
            }
        }

        TimeSample { time, value }
    }

    fn lower_connection(&mut self, node: SyntaxNode<'_>) -> Vec<&'a str> {
        let tree = node.tree();
        let sig = signature(node);

        // Check for None.
        if sig.iter().any(|(k, id)| {
            *k == SyntaxKind::Ident && self.text(self.node_from(tree, *id)) == "None"
        }) {
            return Vec::new();
        }

        // Check for PathList.
        for (kind, id) in &sig {
            if *kind == SyntaxKind::PathList {
                return self.lower_path_list(self.node_from(tree, *id));
            }
        }

        // Individual PathRefs.
        let mut paths = Vec::new();
        for (kind, id) in &sig {
            if *kind == SyntaxKind::PathRef {
                paths.push(self.lower_path_ref(self.node_from(tree, *id)));
            }
        }
        paths
    }

    // ── Primitive helpers ──────────────────────────────────────────

    /// The path of an asset reference, without its `@` or `@@@` delimiters.
    ///
    /// Spec: AOUSD Core §16.2.9; OpenUSD's `Sdf_EvalAssetPath`
    /// (`pxr/usd/sdf/parserHelpers.cpp`), which strips three `@` from each
    /// end of a triple-delimited path (so one or two further `@` before the
    /// closing three stay in the path) and replaces `\@@@` with `@@@`.
    fn lower_asset_ref(&mut self, node: SyntaxNode<'_>) -> Cow<'a, str> {
        let full = node.span().text(self.source);
        if full.len() >= 6 && full.starts_with("@@@") && full.ends_with("@@@") {
            let path = &full[3..full.len() - 3];
            if path.contains("\\@@@") {
                Cow::Owned(path.replace("\\@@@", "@@@"))
            } else {
                Cow::Borrowed(path)
            }
        } else if full.len() >= 2 && full.starts_with('@') && full.ends_with('@') {
            Cow::Borrowed(&full[1..full.len() - 1])
        } else {
            Cow::Borrowed(full)
        }
    }

    fn lower_path_ref(&self, node: SyntaxNode<'_>) -> &'a str {
        let full = node.span().text(self.source);
        if full.starts_with('<') && full.ends_with('>') && full.len() >= 2 {
            &full[1..full.len() - 1]
        } else {
            full
        }
    }

    fn find_asset_ref(&mut self, node: SyntaxNode<'_>) -> Cow<'a, str> {
        for child in node.children_no_trivia() {
            if child.kind() == SyntaxKind::AssetRef {
                return self.lower_asset_ref(child);
            }
        }
        Cow::Borrowed("")
    }

    fn find_string_value(&mut self, node: SyntaxNode<'_>) -> Cow<'a, str> {
        for child in node.children_no_trivia() {
            if is_string_kind(child.kind()) {
                return self.string_literal(child);
            }
        }
        Cow::Borrowed("")
    }

    /// The value of a string literal, with its escape sequences evaluated
    /// (see [`evaluate_escapes`]).
    fn string_literal(&mut self, node: SyntaxNode<'_>) -> Cow<'a, str> {
        match evaluate_escapes(strip_quotes(self.text(node))) {
            Ok(text) => text,
            Err(lossy) => {
                self.error(
                    node.span(),
                    "string escape sequences produce bytes that are not UTF-8",
                );
                Cow::Owned(lossy)
            }
        }
    }

    /// A quoted name: a prim, variant set or variant name.
    ///
    /// Names are identifiers or variant names (AOUSD Core §16.2.8), which
    /// OpenUSD's writer never escapes, so a name is taken from the source
    /// as written, and one spelled with an escape sequence is reported.
    fn name_literal(&mut self, node: SyntaxNode<'_>) -> &'a str {
        let name = strip_quotes(self.text(node));
        if name.contains('\\') {
            self.error(
                node.span(),
                "unsupported: escape sequences in a quoted name are not evaluated",
            );
        }
        name
    }

    fn find_layer_offset_params(&self, node: SyntaxNode<'_>) -> (Option<f64>, Option<f64>) {
        for child in node.children_no_trivia() {
            if child.kind() == SyntaxKind::LayerOffsetParams {
                return self.lower_layer_offset_params(child);
            }
        }
        (None, None)
    }

    fn lower_layer_offset_params(&self, node: SyntaxNode<'_>) -> (Option<f64>, Option<f64>) {
        let tree = node.tree();
        let sig = signature(node);

        let mut offset = None;
        let mut scale = None;
        let mut i = 0;
        while i < sig.len() {
            if sig[i].0 == SyntaxKind::Ident {
                let key = self.text(self.node_from(tree, sig[i].1));
                i += 1;
                if i < sig.len() && sig[i].0 == SyntaxKind::Equals {
                    i += 1;
                }
                let negative = if i < sig.len() && sig[i].0 == SyntaxKind::Minus {
                    i += 1;
                    true
                } else {
                    false
                };
                if i < sig.len() && sig[i].0 == SyntaxKind::Number {
                    let mut v = parse_f64(self.text(self.node_from(tree, sig[i].1)));
                    if negative {
                        v = -v;
                    }
                    match key {
                        "offset" => offset = Some(v),
                        "scale" => scale = Some(v),
                        _ => {}
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        }
        (offset, scale)
    }

    /// Lowers a metadata entry that is only a string (`( "text" )`) to the
    /// `comment` field it authors.
    ///
    /// Spec: AOUSD Core §7.6.1.6.1, §7.6.2.6.3, §7.6.3.3.3 (`comment`).
    /// OpenUSD's text parser stores a bare metadata string as
    /// `SdfFieldKeys->Comment` (`pxr/usd/sdf/textFileFormatParser.cpp`).
    fn bare_comment(&mut self, node: SyntaxNode<'_>) -> Option<MetadataEntry<'a>> {
        let mut children = node.children_no_trivia();
        let first = children.next()?;
        if !is_string_kind(first.kind()) || children.next().is_some() {
            return None;
        }
        Some(MetadataEntry {
            span: node.span(),
            key: "comment",
            op: ListOpKind::Explicit,
            value: MetadataValue::Value(Value::String(self.string_literal(first))),
        })
    }

    fn lower_metadata_entry_from_node(&mut self, node: SyntaxNode<'_>) -> MetadataEntry<'a> {
        if let Some(comment) = self.bare_comment(node) {
            return comment;
        }
        let tree = node.tree();
        let sig = signature(node);

        let mut key = "";
        let mut value = MetadataValue::None;
        let mut op = ListOpKind::Explicit;
        let mut idx = 0;

        // Detect list-op prefix, then find the actual key.
        while idx < sig.len() {
            if sig[idx].0 == SyntaxKind::Ident {
                let t = self.text(self.node_from(tree, sig[idx].1));
                match t {
                    "prepend" => {
                        op = ListOpKind::Prepend;
                        idx += 1;
                        continue;
                    }
                    "append" => {
                        op = ListOpKind::Append;
                        idx += 1;
                        continue;
                    }
                    "add" => {
                        op = ListOpKind::Add;
                        idx += 1;
                        continue;
                    }
                    "delete" => {
                        op = ListOpKind::Delete;
                        idx += 1;
                        continue;
                    }
                    "reorder" => {
                        op = ListOpKind::Reorder;
                        idx += 1;
                        continue;
                    }
                    _ => {
                        key = t;
                        idx += 1;
                        break;
                    }
                }
            }
            idx += 1;
        }

        // Skip `=`.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Equals {
            idx += 1;
        }

        // Value.
        if idx < sig.len() {
            if sig[idx].0 == SyntaxKind::Ident
                && self.text(self.node_from(tree, sig[idx].1)) == "None"
            {
                value = MetadataValue::None;
            } else if sig[idx].0 == SyntaxKind::DictionaryValue {
                value = MetadataValue::Dictionary(
                    self.lower_dictionary(self.node_from(tree, sig[idx].1)),
                );
            } else if sig[idx].0 == SyntaxKind::ValueExpr {
                value = MetadataValue::Value(self.lower_value(self.node_from(tree, sig[idx].1)));
            }
        }

        MetadataEntry {
            span: node.span(),
            key,
            op,
            value,
        }
    }

    fn lower_metadata_entry(&mut self, node: SyntaxNode<'_>) -> MetadataEntry<'a> {
        self.lower_metadata_entry_from_node(node)
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// The value type name OpenUSD reads a legacy one as (`Vec3f` is `float3`,
/// `NormalFloat` is `normal3f`), else `name` itself. OpenUSD's text parser
/// still accepts these spellings and stores the current type, which its
/// writer then spells.
///
/// OpenUSD: `_AddLegacyTypesToRegistry` (`pxr/usd/sdf/schema.cpp`) and the
/// legacy value factories of `Sdf_ParserHelpers` (`parserHelpers.cpp`).
/// `Transform`, `PointIndex`, `EdgeIndex`, `FaceIndex` and `Schema` keep
/// their names there, so they stay as they are here too.
pub(crate) fn current_type_name(name: &str) -> &str {
    match name {
        "Vec2i" => "int2",
        "Vec2h" => "half2",
        "Vec2f" => "float2",
        "Vec2d" => "double2",
        "Vec3i" => "int3",
        "Vec3h" => "half3",
        "Vec3f" => "float3",
        "Vec3d" => "double3",
        "Vec4i" => "int4",
        "Vec4h" => "half4",
        "Vec4f" => "float4",
        "Vec4d" => "double4",
        "Point" => "point3d",
        "PointFloat" => "point3f",
        "Normal" => "normal3d",
        "NormalFloat" => "normal3f",
        "Vector" => "vector3d",
        "VectorFloat" => "vector3f",
        "Color" => "color3d",
        "ColorFloat" => "color3f",
        "Quath" => "quath",
        "Quatf" => "quatf",
        "Quatd" => "quatd",
        "Matrix2d" => "matrix2d",
        "Matrix3d" => "matrix3d",
        "Matrix4d" => "matrix4d",
        "Frame" => "frame4d",
        name => name,
    }
}

fn is_string_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::DoubleQuoteString
            | SyntaxKind::SingleQuoteString
            | SyntaxKind::MultilineDoubleQuoteString
            | SyntaxKind::MultilineSingleQuoteString
    )
}

/// Evaluates the escape sequences in the contents of a string literal (its
/// text between the quotes).
///
/// Spec: AOUSD Core §16.2.5 (`Escaped`). The rules are those of OpenUSD's
/// text parser (`Sdf_EvalQuotedString`, `pxr/usd/sdf/parserHelpers.cpp`, and
/// `TfEscapeStringReplaceChar`, `pxr/base/tf/stringUtils.cpp`), a superset
/// of the grammar: `\\` is a backslash, `\a \b \f \n \r \t \v` are the C
/// control characters, `\x` takes up to two hex digits (with none it is a
/// NUL byte), a digit `0`-`7` starts up to three octal digits, and any other
/// escaped character stands for itself (so `\"` and `\'` are quotes). Hex
/// and octal escapes produce bytes; `Err` carries the lossy text when the
/// result is not UTF-8.
fn evaluate_escapes(content: &str) -> Result<Cow<'_, str>, String> {
    if !content.contains('\\') {
        return Ok(Cow::Borrowed(content));
    }
    let bytes = content.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        i += 1;
        if b != b'\\' {
            out.push(b);
            continue;
        }
        // A lexed literal never ends in a lone backslash: it escapes the
        // character after it, which is then part of the contents.
        let Some(&escaped) = bytes.get(i) else {
            out.push(b);
            break;
        };
        i += 1;
        match escaped {
            b'a' => out.push(0x07),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'v' => out.push(0x0b),
            b'x' => {
                let mut n: u8 = 0;
                for _ in 0..2 {
                    let Some(digit) = bytes.get(i).and_then(|d| char::from(*d).to_digit(16)) else {
                        break;
                    };
                    n = (n << 4) | digit as u8;
                    i += 1;
                }
                out.push(n);
            }
            b'0'..=b'7' => {
                let mut n = escaped - b'0';
                for _ in 0..2 {
                    match bytes.get(i) {
                        Some(d @ b'0'..=b'7') => {
                            // Three octal digits can exceed a byte; like
                            // OpenUSD's `unsigned char`, keep the low bits.
                            n = n.wrapping_mul(8).wrapping_add(d - b'0');
                            i += 1;
                        }
                        _ => break,
                    }
                }
                out.push(n);
            }
            other => out.push(other),
        }
    }
    String::from_utf8(out)
        .map(Cow::Owned)
        .map_err(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// The text between `s`'s quotes, when it is quoted, else `s`. A lone
/// quote, which recovery can lower as a string, opens no string and is
/// kept whole: its opening and closing quote would be one character.
fn strip_quotes(s: &str) -> &str {
    for quote in ["\"\"\"", "'''", "\"", "'"] {
        if s.len() >= 2 * quote.len()
            && let Some(inner) = s.strip_prefix(quote).and_then(|s| s.strip_suffix(quote))
        {
            return inner;
        }
    }
    s
}

fn parse_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

/// A number literal's value, typed as OpenUSD's text parser types it
/// (`_GetNumericValueFromString`, `pxr/usd/sdf/textParserHelpers.cpp`): with
/// `.`, `e` or `E` a double, otherwise an integer, which is unsigned 64-bit
/// when it does not fit `i64`, and a double when it fits neither.
pub(crate) fn parse_number_value(text: &str) -> Value<'_> {
    if !text.contains(['.', 'e', 'E']) {
        if let Ok(n) = text.parse::<i64>() {
            return Value::Int(n);
        }
        if let Ok(n) = text.parse::<u64>() {
            return Value::UInt(n);
        }
    }
    Value::Number(parse_f64(text))
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::vec;
    use alloc::vec::Vec;

    use crate::ast::*;
    use crate::parser::parse_cst;

    use super::lower;

    /// Helper: parse source through CST then lower to AST.
    fn parse(src: &str) -> ParseResult<'_> {
        let cst = parse_cst(src);
        assert!(
            cst.diagnostics.is_empty(),
            "CST errors: {:?}",
            cst.diagnostics
        );
        lower(&cst.tree, src)
    }

    // ── Header / version ──────────────────────────────────────────

    #[test]
    fn lower_version() {
        let r = parse("#usda 1.0\n");
        assert_eq!(r.layer.version, "1.0");
    }

    // ── Specifiers ────────────────────────────────────────────────

    #[test]
    fn lower_specifier_def() {
        let r = parse("#usda 1.0\ndef \"A\" {\n}\n");
        assert_eq!(r.layer.prims[0].specifier, Specifier::Def);
    }

    #[test]
    fn lower_specifier_over() {
        let r = parse("#usda 1.0\nover \"A\" {\n}\n");
        assert_eq!(r.layer.prims[0].specifier, Specifier::Over);
    }

    #[test]
    fn lower_specifier_class() {
        let r = parse("#usda 1.0\nclass \"A\" {\n}\n");
        assert_eq!(r.layer.prims[0].specifier, Specifier::Class);
    }

    // ── Prim type name ────────────────────────────────────────────

    #[test]
    fn lower_prim_no_type() {
        let r = parse("#usda 1.0\ndef \"A\" {\n}\n");
        assert_eq!(r.layer.prims[0].type_name, None);
    }

    #[test]
    fn lower_prim_with_type() {
        let r = parse("#usda 1.0\ndef Xform \"root\" {\n}\n");
        assert_eq!(r.layer.prims[0].type_name, Some("Xform"));
        assert_eq!(r.layer.prims[0].name, "root");
    }

    // ── Nested prims ──────────────────────────────────────────────

    #[test]
    fn lower_nested_prims() {
        let src = "\
#usda 1.0
def Scope \"root\" {
    def Mesh \"child\" {
    }
}
";
        let r = parse(src);
        assert_eq!(r.layer.prims.len(), 1);
        let root = &r.layer.prims[0];
        assert_eq!(root.name, "root");
        assert_eq!(root.children.len(), 1);
        if let PrimChild::Prim(child) = &root.children[0] {
            assert_eq!(child.name, "child");
            assert_eq!(child.type_name, Some("Mesh"));
        } else {
            panic!("expected child prim");
        }
    }

    // ── Attributes ────────────────────────────────────────────────

    #[test]
    fn lower_int_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    int x = 7\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert_eq!(attr.type_name, "int");
        assert_eq!(attr.name, "x");
        assert!(!attr.custom);
        assert!(!attr.uniform);
        assert!(!attr.is_array);
        assert!(matches!(attr.default, Some(Value::Int(7))));
    }

    #[test]
    fn lower_float_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    float y = 2.5\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert_eq!(attr.type_name, "float");
        assert!(matches!(attr.default, Some(Value::Number(n)) if (n - 2.5).abs() < 1e-10));
    }

    #[test]
    fn lower_negative_number() {
        let src = "#usda 1.0\ndef \"A\" {\n    int x = -5\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert!(matches!(attr.default, Some(Value::Int(-5))));
    }

    #[test]
    fn lower_string_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    string name = \"hello\"\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert_eq!(attr.type_name, "string");
        assert!(matches!(&attr.default, Some(Value::String(s)) if s == "hello"));
    }

    #[test]
    fn lower_bool_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    bool v = true\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert!(matches!(attr.default, Some(Value::Bool(true))));
    }

    #[test]
    fn lower_blocked_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    int x = None\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert!(matches!(attr.default, Some(Value::Blocked)));
    }

    #[test]
    fn lower_custom_uniform_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    custom uniform int x = 1\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert!(attr.custom);
        assert!(attr.uniform);
        assert_eq!(attr.type_name, "int");
    }

    #[test]
    fn lower_array_type_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    int[] ids = [1, 2, 3]\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert!(attr.is_array);
        assert_eq!(attr.type_name, "int");
        if let Some(Value::Array(items)) = &attr.default {
            assert_eq!(items.len(), 3);
            assert!(matches!(items[0], Value::Int(1)));
            assert!(matches!(items[1], Value::Int(2)));
            assert!(matches!(items[2], Value::Int(3)));
        } else {
            panic!("expected array value, got {:?}", attr.default);
        }
    }

    #[test]
    fn lower_tuple_value() {
        let src = "#usda 1.0\ndef \"A\" {\n    float3 pos = (1.0, 2.0, 3.0)\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        if let Some(Value::Tuple(items)) = &attr.default {
            assert_eq!(items.len(), 3);
        } else {
            panic!("expected tuple value");
        }
    }

    #[test]
    fn lower_namespaced_attribute() {
        let src = "#usda 1.0\ndef \"A\" {\n    color3f primvars:displayColor = (1, 0, 0)\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert_eq!(attr.name, "primvars:displayColor");
    }

    #[test]
    fn lower_multiple_attributes() {
        let src = "\
#usda 1.0
def \"A\" {
    int a = 1
    float b = 2.0
    string c = \"hi\"
}
";
        let r = parse(src);
        assert_eq!(r.layer.prims[0].children.len(), 3);
        for child in &r.layer.prims[0].children {
            assert!(matches!(child, PrimChild::Attribute(_)));
        }
    }

    // ── Relationships ─────────────────────────────────────────────

    #[test]
    fn lower_relationship_single_target() {
        let src = "#usda 1.0\ndef \"A\" {\n    rel myRel = </B>\n}\n";
        let r = parse(src);
        let rel = match &r.layer.prims[0].children[0] {
            PrimChild::Relationship(r) => r,
            other => panic!("expected Relationship, got {other:?}"),
        };
        assert_eq!(rel.name, "myRel");
        assert_eq!(rel.op, ListOpKind::Explicit);
        let targets = rel.targets.as_ref().unwrap();
        assert_eq!(targets, &["/B"]);
    }

    #[test]
    fn lower_relationship_array_targets() {
        let src = "#usda 1.0\ndef \"A\" {\n    rel myRel = [</B>, </C>]\n}\n";
        let r = parse(src);
        let rel = match &r.layer.prims[0].children[0] {
            PrimChild::Relationship(r) => r,
            other => panic!("expected Relationship, got {other:?}"),
        };
        let targets = rel.targets.as_ref().unwrap();
        assert_eq!(targets, &["/B", "/C"]);
    }

    /// A bare declaration authors no targets; `= None` and `= []` author an
    /// explicitly empty list, which blocks weaker targets.
    #[test]
    fn lower_relationship_explicit_empty_targets() {
        let src = "#usda 1.0\ndef \"A\" {\n    rel a\n    rel b = None\n    rel c = []\n    rel d = None (\n        doc = \"d\"\n    )\n}\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let targets: Vec<_> = r.layer.prims[0]
            .children
            .iter()
            .map(|c| match c {
                PrimChild::Relationship(r) => (r.name, r.targets.clone()),
                other => panic!("expected Relationship, got {other:?}"),
            })
            .collect();
        assert_eq!(
            targets,
            [
                ("a", None),
                ("b", Some(vec![])),
                ("c", Some(vec![])),
                ("d", Some(vec![])),
            ]
        );
    }

    #[test]
    fn lower_custom_relationship() {
        let src = "#usda 1.0\ndef \"A\" {\n    custom rel foo\n}\n";
        let r = parse(src);
        let rel = match &r.layer.prims[0].children[0] {
            PrimChild::Relationship(r) => r,
            other => panic!("expected Relationship, got {other:?}"),
        };
        assert!(rel.custom);
        assert_eq!(rel.name, "foo");
        assert!(rel.targets.is_none());
    }

    #[test]
    fn lower_prepend_relationship() {
        let src = "#usda 1.0\ndef \"A\" {\n    prepend rel proxyPrim = </B>\n}\n";
        let r = parse(src);
        let rel = match &r.layer.prims[0].children[0] {
            PrimChild::Relationship(r) => r,
            other => panic!("expected Relationship, got {other:?}"),
        };
        assert_eq!(rel.op, ListOpKind::Prepend);
    }

    // ── Layer metadata ────────────────────────────────────────────

    #[test]
    fn lower_layer_doc() {
        let src = "#usda 1.0\n(\n    doc = \"my layer\"\n)\n";
        let r = parse(src);
        assert_eq!(r.layer.metadata.len(), 1);
        assert!(matches!(&r.layer.metadata[0], LayerMeta::Doc(d) if d == "my layer"));
    }

    #[test]
    fn lower_sublayers_with_offset() {
        let src = "\
#usda 1.0
(
    subLayers = [
        @./a.usd@ (offset = 10; scale = 2)
    ]
)
";
        let r = parse(src);
        if let LayerMeta::SubLayers(items) = &r.layer.metadata[0] {
            assert_eq!(items[0].asset, "./a.usd");
            assert_eq!(items[0].offset, Some(10.0));
            assert_eq!(items[0].scale, Some(2.0));
        } else {
            panic!("expected SubLayers");
        }
    }

    #[test]
    fn lower_relocates() {
        let src = "\
#usda 1.0
(
    relocates = {
        </Old/Path>: </New/Path>
    }
)
";
        let r = parse(src);
        if let LayerMeta::Relocates(entries) = &r.layer.metadata[0] {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].source, "/Old/Path");
            assert_eq!(entries[0].target, "/New/Path");
        } else {
            panic!("expected Relocates");
        }
    }

    // ── Prim metadata ─────────────────────────────────────────────

    #[test]
    fn lower_prim_doc() {
        let src = "#usda 1.0\ndef \"A\" (\n    doc = \"hello\"\n) {\n}\n";
        let r = parse(src);
        assert!(matches!(
            &r.layer.prims[0].metadata[0],
            PrimMeta::Doc(d) if d == "hello"
        ));
    }

    #[test]
    fn lower_prim_kind() {
        let src = "#usda 1.0\ndef \"A\" (\n    kind = \"component\"\n) {\n}\n";
        let r = parse(src);
        assert!(matches!(
            &r.layer.prims[0].metadata[0],
            PrimMeta::Kind(k) if k == "component"
        ));
    }

    #[test]
    fn lower_inherits() {
        let src = "#usda 1.0\ndef \"A\" (\n    inherits = </B>\n) {\n}\n";
        let r = parse(src);
        if let PrimMeta::Inherits(inherits) = &r.layer.prims[0].metadata[0] {
            assert_eq!(inherits.kind, ListOpKind::Explicit);
            let items = inherits.items.as_ref().unwrap();
            assert_eq!(items, &["/B"]);
        } else {
            panic!("expected Inherits, got {:?}", r.layer.prims[0].metadata[0]);
        }
    }

    #[test]
    fn lower_prepend_references() {
        let src = "#usda 1.0\ndef \"A\" (\n    prepend references = @./ref.usd@\n) {\n}\n";
        let r = parse(src);
        if let PrimMeta::References(refs) = &r.layer.prims[0].metadata[0] {
            assert_eq!(refs.kind, ListOpKind::Prepend);
            let items = refs.items.as_ref().unwrap();
            assert_eq!(items[0].asset.as_deref(), Some("./ref.usd"));
        } else {
            panic!("expected References");
        }
    }

    #[test]
    fn lower_payload() {
        let src = "#usda 1.0\ndef \"A\" (\n    payload = @./payload.usd@</Root>\n) {\n}\n";
        let r = parse(src);
        if let PrimMeta::Payload(p) = &r.layer.prims[0].metadata[0] {
            let items = p.items.as_ref().unwrap();
            assert_eq!(items[0].asset.as_deref(), Some("./payload.usd"));
            assert_eq!(items[0].prim_path, Some("/Root"));
        } else {
            panic!("expected Payload");
        }
    }

    #[test]
    fn variant_selections_evaluate_escapes() {
        // OpenUSD's writer escapes a control character in a selection, as
        // in an expression; reading it back must evaluate the escape.
        let src = "#usda 1.0\ndef \"A\" (\n    variants = {\n        string \"s\\x10\" = \"`${V}\\x10`\"\n        string t = \"d\\x61rk\"\n    }\n) {\n}\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let PrimMeta::Variants(sels) = &r.layer.prims[0].metadata[0] else {
            panic!("expected Variants");
        };
        let sels: Vec<(&str, &str)> = sels
            .iter()
            .map(|s| (&*s.set_name, &*s.branch_name))
            .collect();
        assert_eq!(sels, [("s\x10", "`${V}\x10`"), ("t", "dark")]);
    }

    #[test]
    fn invalid_variant_selections_are_reported() {
        for entry in [
            "string v = \"a b\"",
            "string v = \"a\\x10b\"",
            "string v = \"`${V}\"",
            "token v = \"one\"",
        ] {
            let src = format!(
                "#usda 1.0\ndef \"A\" (\n    variants = {{\n        {entry}\n    }}\n) {{\n}}\n"
            );
            assert_eq!(parse(&src).diagnostics.len(), 1, "{entry}");
        }
        for entry in [
            "string v = \"\"",
            "string v = \".\"",
            "string v = \".a|b-c\"",
        ] {
            let src = format!(
                "#usda 1.0\ndef \"A\" (\n    variants = {{\n        {entry}\n    }}\n) {{\n}}\n"
            );
            assert!(parse(&src).diagnostics.is_empty(), "{entry}");
        }
    }

    #[test]
    fn lower_specializes() {
        let src = "#usda 1.0\ndef \"A\" (\n    specializes = </Base>\n) {\n}\n";
        let r = parse(src);
        if let PrimMeta::Specializes(sp) = &r.layer.prims[0].metadata[0] {
            let items = sp.items.as_ref().unwrap();
            assert_eq!(items, &["/Base"]);
        } else {
            panic!("expected Specializes");
        }
    }

    #[test]
    fn lower_variants_metadata() {
        let src = "\
#usda 1.0
def \"A\" (
    variants = {
        string shade = \"red\"
    }
) {
}
";
        let r = parse(src);
        if let PrimMeta::Variants(sels) = &r.layer.prims[0].metadata[0] {
            assert_eq!(sels.len(), 1);
            assert_eq!(sels[0].set_name, "shade");
            assert_eq!(sels[0].branch_name, "red");
        } else {
            panic!("expected Variants, got {:?}", r.layer.prims[0].metadata[0]);
        }
    }

    // ── Variant sets ──────────────────────────────────────────────

    #[test]
    fn lower_variant_set() {
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
        let r = parse(src);
        let vs = match &r.layer.prims[0].children[0] {
            PrimChild::VariantSet(v) => v,
            other => panic!("expected VariantSet, got {other:?}"),
        };
        assert_eq!(vs.name, "color");
        assert_eq!(vs.branches.len(), 2);
        assert_eq!(vs.branches[0].name, "red");
        assert_eq!(vs.branches[1].name, "blue");
        assert_eq!(vs.branches[0].children.len(), 1);
    }

    // ── Reorder statements ────────────────────────────────────────

    #[test]
    fn lower_reorder_properties() {
        let src = "\
#usda 1.0
def \"A\" {
    reorder properties = [\"b\", \"a\"]
}
";
        let r = parse(src);
        if let PrimChild::ReorderProperties(names) = &r.layer.prims[0].children[0] {
            assert_eq!(names, &["b", "a"]);
        } else {
            panic!("expected ReorderProperties");
        }
    }

    #[test]
    fn lower_reorder_root_prims() {
        let src = "\
#usda 1.0
reorder rootPrims = [\"B\", \"A\"]
";
        let r = parse(src);
        assert_eq!(r.layer.root_prim_order, Some(vec!["B", "A"]));
    }

    // ── Dictionary values ─────────────────────────────────────────

    #[test]
    fn lower_dictionary_metadata() {
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
        let r = parse(src);
        if let PrimMeta::Custom(entry) = &r.layer.prims[0].metadata[0] {
            assert_eq!(entry.key, "customData");
            if let MetadataValue::Dictionary(dict) = &entry.value {
                assert_eq!(dict.len(), 2);
                assert_eq!(dict[0].key, "foo");
                assert_eq!(dict[0].type_name, Some("string"));
                assert!(matches!(&dict[0].value, Value::String(s) if s == "bar"));
                assert_eq!(dict[1].key, "count");
                assert!(matches!(dict[1].value, Value::Int(42)));
            } else {
                panic!("expected Dictionary value");
            }
        } else {
            panic!("expected Custom metadata");
        }
    }

    // ── Asset and path values ─────────────────────────────────────

    #[test]
    fn lower_asset_value() {
        let src = "#usda 1.0\ndef \"A\" {\n    asset path = @./texture.png@\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert!(matches!(&attr.default, Some(Value::Asset(path)) if path == "./texture.png"));
    }

    #[test]
    fn lower_path_value() {
        let src = "#usda 1.0\ndef \"A\" {\n    token target = </B/C>\n}\n";
        // Note: path as value isn't always a direct parse — depends on grammar.
        // If the parser treats this as an identifier, that's also valid.
        let r = parse(src);
        assert_eq!(r.layer.prims[0].children.len(), 1);
    }

    // ── Connection ────────────────────────────────────────────────

    #[test]
    fn lower_connection() {
        let src = "#usda 1.0\ndef \"A\" {\n    float x.connect = </B.x>\n}\n";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        assert_eq!(attr.name, "x");
        let conn = attr.connection.as_ref().expect("expected connection");
        assert_eq!(conn.op, ListOpKind::Explicit);
        assert_eq!(conn.targets, &["/B.x"]);
    }

    // ── Time samples ──────────────────────────────────────────────

    #[test]
    fn lower_time_samples() {
        let src = "\
#usda 1.0
def \"A\" {
    float x.timeSamples = {
        1: 0.0,
        2: 1.0,
    }
}
";
        let r = parse(src);
        let attr = match &r.layer.prims[0].children[0] {
            PrimChild::Attribute(a) => a,
            other => panic!("expected Attribute, got {other:?}"),
        };
        let ts = attr.time_samples.as_ref().expect("expected timeSamples");
        assert_eq!(ts.len(), 2);
        assert!((ts[0].time - 1.0).abs() < 1e-10);
        assert!((ts[1].time - 2.0).abs() < 1e-10);
        assert!(matches!(ts[0].value, Some(Value::Number(n)) if n.abs() < 1e-10));
        assert!(matches!(ts[1].value, Some(Value::Number(n)) if (n - 1.0).abs() < 1e-10));
    }

    // ── No diagnostics from lowering ──────────────────────────────

    #[test]
    fn lower_produces_no_diagnostics_on_valid_input() {
        let src = "\
#usda 1.0
(
    doc = \"test layer\"
    subLayers = [
        @./sub.usd@
    ]
)

def Scope \"root\" {
    def Mesh \"child\" (
        kind = \"component\"
        prepend references = @./ref.usd@</Ref>
    ) {
        int x = 42
        float3 pos = (1.0, 2.0, 3.0)
        rel target = </root/other>
    }
    over \"other\" {
    }
}
";
        let r = parse(src);
        assert!(
            r.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            r.diagnostics
        );
    }

    // ── String literals ───────────────────────────────────────────

    fn string_default(literal: &str) -> (alloc::string::String, usize) {
        let src = alloc::format!("#usda 1.0\ndef \"P\" {{\n    string s = {literal}\n}}\n");
        let r = parse(&src);
        let PrimChild::Attribute(attr) = &r.layer.prims[0].children[0] else {
            panic!("expected an attribute");
        };
        let Some(Value::String(s)) = &attr.default else {
            panic!("expected a string default");
        };
        (alloc::string::String::from(&**s), r.diagnostics.len())
    }

    /// Each literal reads, in OpenUSD 26.08 (`Sdf.Layer.ImportFromString`),
    /// as the string beside it.
    #[test]
    fn string_escapes_evaluate_as_openusd_does() {
        let cases = [
            (r#""a\"b""#, "a\"b"),
            (r"'it\'s'", "it's"),
            (r#""\\""#, "\\"),
            (r#""\n\t\r\a\b\f\v""#, "\n\t\r\x07\x08\x0c\x0b"),
            (r#""\x5C\x41""#, "\\A"),
            (r#""\x""#, "\0"),
            (r#""\xZ""#, "\0Z"),
            (r#""\x4g""#, "\x04g"),
            (r#""\101\0""#, "A\0"),
            (r#""\1012""#, "A2"),
            (r#""\400""#, "\0"),
            (r#""\q\?\'""#, "q?'"),
            (r#""\8""#, "8"),
            (r#""\xc3\xa9""#, "\u{e9}"),
            ("\"\u{e9}\\t\"", "\u{e9}\t"),
            (
                "\"\"\"line1\nline2 \"q\" \\\"\"\"\"",
                "line1\nline2 \"q\" \"",
            ),
            (r"'''a\''''", "a'"),
        ];
        for (literal, expected) in cases {
            assert_eq!(string_default(literal), (expected.into(), 0), "{literal}");
        }
    }

    /// OpenUSD keeps the bytes of `"\777"` (0xFF); a Rust string cannot.
    #[test]
    fn string_escapes_that_are_not_utf8_are_reported() {
        assert_eq!(string_default(r#""\777""#), ("\u{fffd}".into(), 1));
    }

    #[test]
    fn escapes_evaluate_in_docs_and_dictionary_keys() {
        let src = "#usda 1.0\n(\n    doc = '''a\\'b'''\n    customLayerData = {\n        \
                   string \"k\\x41\" = \"v\\n\"\n    }\n)\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        assert!(matches!(&r.layer.metadata[0], LayerMeta::Doc(d) if d == "a'b"));
        let LayerMeta::Custom(entry) = &r.layer.metadata[1] else {
            panic!("expected customLayerData");
        };
        let MetadataValue::Dictionary(dict) = &entry.value else {
            panic!("expected a dictionary");
        };
        assert_eq!(dict[0].key, "kA");
        assert!(matches!(&dict[0].value, Value::String(s) if s == "v\n"));
    }

    /// `-0` is the double `-0.0`, and `-inf` negative infinity, as in
    /// OpenUSD's `_GetNumericValueFromString`.
    #[test]
    fn negative_zero_and_infinity() {
        let src = "#usda 1.0\ndef \"P\" {\n    double z = -0\n    double i = -inf\n    int n = -3\n    double \
                   y = -0.0\n}\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let values: Vec<_> = r.layer.prims[0]
            .children
            .iter()
            .map(|child| match child {
                PrimChild::Attribute(a) => a.default.as_ref().unwrap(),
                _ => panic!("expected attributes"),
            })
            .collect();
        assert!(matches!(values[0], Value::Number(z) if *z == 0.0 && z.is_sign_negative()));
        assert!(matches!(values[1], Value::Number(i) if *i == f64::NEG_INFINITY));
        assert!(matches!(values[2], Value::Int(-3)));
        assert!(matches!(values[3], Value::Number(y) if y.is_sign_negative()));
    }

    /// Integers beyond `i64` are unsigned, and beyond `u64` doubles, as in
    /// OpenUSD's `_GetNumericValueFromString`.
    #[test]
    fn large_integer_literals() {
        let src = "#usda 1.0\ndef \"P\" {\n    uint64 a = 18446744073709551615\n    double b = \
                   18446744073709551616\n    double c = -9223372036854775809\n}\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let values: Vec<_> = r.layer.prims[0]
            .children
            .iter()
            .map(|child| match child {
                PrimChild::Attribute(a) => a.default.as_ref().unwrap(),
                _ => panic!("expected attributes"),
            })
            .collect();
        assert!(matches!(values[0], Value::UInt(u64::MAX)));
        assert!(matches!(values[1], Value::Number(n) if *n == 18_446_744_073_709_551_616.0));
        assert!(matches!(values[2], Value::Number(n) if *n == -9_223_372_036_854_775_809.0));
    }

    #[test]
    fn a_minus_without_a_number_is_reported() {
        let cst = parse_cst("#usda 1.0\ndef \"P\" {\n    double x = -nan\n}\n");
        assert_eq!(cst.diagnostics.len(), 1, "{:?}", cst.diagnostics);
    }

    #[test]
    fn signed_minimum_integer_remains_exact_before_declared_type_conversion() {
        let parsed = parse(
            "#usda 1.0\ndef \"P\" {\n int64 low = -9223372036854775808\n double below = -9223372036854775809\n}\n",
        );
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let PrimChild::Attribute(low) = &parsed.layer.prims[0].children[0] else {
            panic!("attribute")
        };
        assert!(matches!(low.default, Some(Value::Int(i64::MIN))));
        let PrimChild::Attribute(below) = &parsed.layer.prims[0].children[1] else {
            panic!("attribute")
        };
        assert!(
            matches!(below.default, Some(Value::Number(_))),
            "an out-of-range integer must not become a valid signed minimum"
        );
    }

    #[test]
    fn triple_delimited_asset_paths() {
        let src = "#usda 1.0\ndef \"P\" {\n    asset a = @@@x@y.png@@@\n    asset b = \
                   @@@a@@@@@\n    asset c = @@\n}\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let assets: Vec<_> = r.layer.prims[0]
            .children
            .iter()
            .map(|child| match child {
                PrimChild::Attribute(a) => match &a.default {
                    Some(Value::Asset(path)) => &**path,
                    _ => panic!("expected an asset"),
                },
                _ => panic!("expected attributes"),
            })
            .collect();
        assert_eq!(assets, ["x@y.png", "a@@", ""]);

        // `\@@@` is `@@@` (`Sdf_EvalAssetPath`).
        let r = parse("#usda 1.0\ndef \"P\" {\n    asset a = @@@x\\@@@y@@@\n}\n");
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let PrimChild::Attribute(a) = &r.layer.prims[0].children[0] else {
            panic!("expected an attribute");
        };
        assert!(matches!(&a.default, Some(Value::Asset(path)) if path == "x@@@y"));
    }

    #[test]
    fn string_dictionaries() {
        let src = "#usda 1.0\ndef \"P\" (\n    prefixSubstitutions = {\n        \"/a\": \"/b\", \n        \
                   'k': \"v\"\n    }\n) {\n}\n";
        let r = parse(src);
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let PrimMeta::Custom(entry) = &r.layer.prims[0].metadata[0] else {
            panic!("expected prefixSubstitutions");
        };
        let MetadataValue::Dictionary(dict) = &entry.value else {
            panic!("expected a dictionary");
        };
        let items: Vec<_> = dict
            .iter()
            .map(|e| match &e.value {
                Value::String(v) => (e.type_name, &*e.key, &**v),
                _ => panic!("expected string values"),
            })
            .collect();
        assert_eq!(
            items,
            [(Some("string"), "/a", "/b"), (Some("string"), "k", "v")]
        );
    }

    #[test]
    fn escaped_names_are_reported() {
        let r = parse("#usda 1.0\ndef \"a\\x62\" {\n}\n");
        assert_eq!(r.diagnostics.len(), 1, "{:?}", r.diagnostics);
    }
}
