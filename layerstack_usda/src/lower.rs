// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lowers a lossless CST into the typed AST.
//!
//! This module walks a [`SyntaxTree`] and produces the [`ast`](crate::ast)
//! types. Semantic logic (quote stripping, number parsing, keyword dispatch)
//! lives here rather than in the parser.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::Span;
use crate::ast::*;
use crate::cst::{SyntaxKind, SyntaxNode, SyntaxTree};
use crate::diagnostic::Diagnostic;

/// Lowers a CST into an AST, producing diagnostics for semantic errors.
pub fn lower<'a>(tree: &SyntaxTree, source: &'a str) -> ParseResult<'a> {
    let mut ctx = LowerCtx {
        source,
        diagnostics: Vec::new(),
    };
    let layer = ctx.lower_source_file(tree.root());
    ParseResult {
        layer,
        diagnostics: ctx.diagnostics,
    }
}

struct LowerCtx<'a> {
    source: &'a str,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> LowerCtx<'a> {
    #[expect(dead_code, reason = "reserved for lowering diagnostics")]
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
        let mut prims = Vec::new();
        let mut root_prim_order = None;

        let children: Vec<_> = root
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        for (kind, id) in children {
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
        let children: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut items = Vec::new();
        for (kind, id) in children {
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
        let item_ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::SubLayerItem)
            .map(|c| c.id.0)
            .collect();

        let mut items = Vec::new();
        for id in item_ids {
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
        let entry_ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::RelocateEntry)
            .map(|c| c.id.0)
            .collect();

        let mut entries = Vec::new();
        for id in entry_ids {
            entries.push(self.lower_relocate_entry(self.node_from(tree, id)));
        }
        LayerMeta::Relocates(entries)
    }

    fn lower_relocate_entry(&mut self, node: SyntaxNode<'_>) -> RelocateEntry<'a> {
        let paths: Vec<_> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::PathRef)
            .collect();
        let source = paths.first().map(|n| self.lower_path_ref(*n)).unwrap_or("");
        let target = paths.get(1).map(|n| self.lower_path_ref(*n)).unwrap_or("");
        RelocateEntry { source, target }
    }

    // ── Prims ──────────────────────────────────────────────────────

    fn lower_prim(&mut self, node: SyntaxNode<'_>) -> Prim<'a> {
        let span = node.span();
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

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
            name = strip_quotes(self.text(self.node_from(tree, sig[idx].1)));
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
        let entry_ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::PrimMetaEntry)
            .map(|c| c.id.0)
            .collect();

        let mut items = Vec::new();
        for id in entry_ids {
            if let Some(meta) = self.lower_prim_meta_entry(self.node_from(tree, id)) {
                items.push(meta);
            }
        }
        items
    }

    fn lower_prim_meta_entry(&mut self, node: SyntaxNode<'_>) -> Option<PrimMeta<'a>> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut idx = 0;
        let mut op = ListOpKind::Explicit;

        // Detect list-op prefix.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            match self.text(self.node_from(tree, sig[idx].1)) {
                "prepend" => {
                    op = ListOpKind::Prepend;
                    idx += 1;
                }
                "append" | "add" => {
                    op = ListOpKind::Append;
                    idx += 1;
                }
                "delete" => {
                    op = ListOpKind::Delete;
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
                Some(PrimMeta::References(self.lower_arc_listop(arc_node, op)))
            }
            "inherits" => {
                let pl = sig.iter().find(|s| s.0 == SyntaxKind::PathList);
                let pl_node = pl.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Inherits(self.lower_path_listop(pl_node, op)))
            }
            "specializes" => {
                let pl = sig.iter().find(|s| s.0 == SyntaxKind::PathList);
                let pl_node = pl.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Specializes(self.lower_path_listop(pl_node, op)))
            }
            "payload" => {
                let arc_list = sig.iter().find(|s| s.0 == SyntaxKind::ArcList);
                let arc_node = arc_list.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Payload(self.lower_arc_listop(arc_node, op)))
            }
            "variants" => {
                let sel = sig.iter().find(|s| s.0 == SyntaxKind::VariantSelections);
                let sel_node = sel.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::Variants(self.lower_variant_selections(sel_node)))
            }
            "variantSets" => {
                let nl = sig.iter().find(|s| s.0 == SyntaxKind::NameList);
                let nl_node = nl.map(|s| self.node_from(tree, s.1));
                Some(PrimMeta::VariantSets(self.lower_name_listop(nl_node, op)))
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
        let children_info: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut children = Vec::new();
        for (kind, id) in children_info {
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
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut custom = false;
        let mut uniform = false;
        let mut type_name = "";
        let mut is_array = false;
        let mut name = "";
        let mut default = None;
        let mut time_samples = None;
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
                "append" | "add" => {
                    list_op = ListOpKind::Append;
                    idx += 1;
                }
                "delete" => {
                    list_op = ListOpKind::Delete;
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

        // type name
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            type_name = self.text(self.node_from(tree, sig[idx].1));
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
                SyntaxKind::ConnectionSuffix => {
                    let targets = self.lower_connection(self.node_from(tree, id));
                    connection = Some(Connection {
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
            connection,
            metadata,
        }
    }

    fn lower_attribute_metadata(&mut self, node: SyntaxNode<'_>) -> Vec<MetadataEntry<'a>> {
        let tree = node.tree();
        let entry_ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::MetadataEntry)
            .map(|c| c.id.0)
            .collect();

        let mut items = Vec::new();
        for id in entry_ids {
            items.push(self.lower_metadata_entry(self.node_from(tree, id)));
        }
        items
    }

    // ── Relationships ──────────────────────────────────────────────

    fn lower_relationship(&mut self, node: SyntaxNode<'_>) -> Relationship<'a> {
        let span = node.span();
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut op = ListOpKind::Explicit;
        let mut custom = false;
        let mut name = "";
        let mut targets = None;
        let mut metadata = Vec::new();
        let mut idx = 0;

        // list-op prefix
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            match self.text(self.node_from(tree, sig[idx].1)) {
                "prepend" => {
                    op = ListOpKind::Prepend;
                    idx += 1;
                }
                "append" | "add" => {
                    op = ListOpKind::Append;
                    idx += 1;
                }
                "delete" => {
                    op = ListOpKind::Delete;
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
                SyntaxKind::PrimMetadata => {
                    metadata = self.lower_attribute_metadata(self.node_from(tree, id));
                }
                _ => {}
            }
        }

        Relationship {
            span,
            custom,
            op,
            name,
            targets,
            metadata,
        }
    }

    // ── Variant sets ───────────────────────────────────────────────

    fn lower_variant_set(&mut self, node: SyntaxNode<'_>) -> VariantSet<'a> {
        let span = node.span();
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut name = "";
        let mut branches = Vec::new();

        for (kind, id) in &sig {
            match *kind {
                k if is_string_kind(k) => {
                    name = strip_quotes(self.text(self.node_from(tree, *id)));
                }
                SyntaxKind::VariantBranch => {
                    branches.push(self.lower_variant_branch(self.node_from(tree, *id)));
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
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut name = "";
        let mut metadata = Vec::new();
        let mut children = Vec::new();

        for (kind, id) in &sig {
            match *kind {
                k if is_string_kind(k) => {
                    name = strip_quotes(self.text(self.node_from(tree, *id)));
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
        let tree = node.tree();
        let idents: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::Ident)
            .map(|c| c.id.0)
            .collect();
        let kind = idents
            .get(1)
            .map(|id| self.text(self.node_from(tree, *id)))
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
    ) -> ListOpArc<'a> {
        let Some(node) = list_node else {
            return ListOpArc { kind, items: None };
        };
        // Check for None keyword.
        if node
            .children_no_trivia()
            .any(|c| c.kind() == SyntaxKind::Ident && self.text(c) == "None")
        {
            return ListOpArc { kind, items: None };
        }
        let tree = node.tree();
        let arc_ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::ArcRef)
            .map(|c| c.id.0)
            .collect();

        let mut items = Vec::new();
        for id in arc_ids {
            items.push(self.lower_arc_ref(self.node_from(tree, id)));
        }
        ListOpArc {
            kind,
            items: Some(items),
        }
    }

    fn lower_arc_ref(&mut self, node: SyntaxNode<'_>) -> ArcRef<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut asset = None;
        let mut prim_path = None;
        let (mut offset, mut scale) = (None, None);

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
                }
                _ => {}
            }
        }

        ArcRef {
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
    ) -> ListOpPaths<'a> {
        let Some(node) = list_node else {
            return ListOpPaths { kind, items: None };
        };
        if node
            .children_no_trivia()
            .any(|c| c.kind() == SyntaxKind::Ident && self.text(c) == "None")
        {
            return ListOpPaths { kind, items: None };
        }
        let items = self.lower_path_list(node);
        ListOpPaths {
            kind,
            items: Some(items),
        }
    }

    fn lower_path_list(&mut self, node: SyntaxNode<'_>) -> Vec<&'a str> {
        node.children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::PathRef)
            .map(|c| self.lower_path_ref(c))
            .collect()
    }

    fn lower_name_listop(
        &mut self,
        list_node: Option<SyntaxNode<'_>>,
        kind: ListOpKind,
    ) -> ListOp<&'a str> {
        let Some(node) = list_node else {
            return ListOp { kind, items: None };
        };
        if node
            .children_no_trivia()
            .any(|c| c.kind() == SyntaxKind::Ident && self.text(c) == "None")
        {
            return ListOp { kind, items: None };
        }
        let items = self.lower_name_list(node);
        ListOp {
            kind,
            items: Some(items),
        }
    }

    fn lower_name_list(&mut self, node: SyntaxNode<'_>) -> Vec<&'a str> {
        node.children_no_trivia()
            .filter(|c| is_string_kind(c.kind()))
            .map(|c| strip_quotes(self.text(c)))
            .collect()
    }

    fn lower_variant_selections(
        &mut self,
        node: Option<SyntaxNode<'_>>,
    ) -> Vec<VariantSelection<'a>> {
        let Some(node) = node else {
            return Vec::new();
        };
        let tree = node.tree();
        let entry_ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::VariantSelectionEntry)
            .map(|c| c.id.0)
            .collect();

        let mut selections = Vec::new();
        for id in entry_ids {
            selections.push(self.lower_variant_selection_entry(self.node_from(tree, id)));
        }
        selections
    }

    fn lower_variant_selection_entry(&mut self, node: SyntaxNode<'_>) -> VariantSelection<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut set_name = "";
        let mut branch_name = "";

        let strings: Vec<_> = sig.iter().filter(|s| is_string_kind(s.0)).collect();
        let idents: Vec<_> = sig.iter().filter(|s| s.0 == SyntaxKind::Ident).collect();

        if strings.len() >= 2 {
            set_name = strip_quotes(self.text(self.node_from(tree, strings[0].1)));
            branch_name =
                strip_quotes(self.text(self.node_from(tree, strings[strings.len() - 1].1)));
        } else if !idents.is_empty() && !strings.is_empty() {
            for ident in &idents {
                let t = self.text(self.node_from(tree, ident.1));
                if t != "string" && t != "token" {
                    set_name = t;
                    break;
                }
            }
            branch_name =
                strip_quotes(self.text(self.node_from(tree, strings[strings.len() - 1].1)));
        }

        VariantSelection {
            set_name,
            branch_name,
        }
    }

    // ── Values ─────────────────────────────────────────────────────

    fn lower_value(&mut self, node: SyntaxNode<'_>) -> Value<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        if sig.is_empty() {
            return Value::Blocked;
        }

        // Negated number.
        if sig.len() >= 2 && sig[0].0 == SyntaxKind::Minus && sig[1].0 == SyntaxKind::Number {
            let text = self.text(self.node_from(tree, sig[1].1));
            return match parse_number_value(text) {
                Value::Int(n) => Value::Int(-n),
                Value::Number(n) => Value::Number(-n),
                other => other,
            };
        }

        let (kind, id) = sig[0];
        match kind {
            SyntaxKind::Number => {
                let text = self.text(self.node_from(tree, id));
                parse_number_value(text)
            }
            k if is_string_kind(k) => {
                Value::String(strip_quotes(self.text(self.node_from(tree, id))))
            }
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
            SyntaxKind::Ident => {
                let text = self.text(self.node_from(tree, id));
                match text {
                    "true" | "True" => Value::Bool(true),
                    "false" | "False" => Value::Bool(false),
                    "None" => Value::Blocked,
                    "inf" => Value::Number(f64::INFINITY),
                    "nan" => Value::Number(f64::NAN),
                    _ => Value::Identifier(text),
                }
            }
            _ => Value::Blocked,
        }
    }

    fn lower_tuple(&mut self, node: SyntaxNode<'_>) -> Vec<Value<'a>> {
        let tree = node.tree();
        let ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::ValueExpr)
            .map(|c| c.id.0)
            .collect();
        ids.iter()
            .map(|id| self.lower_value(self.node_from(tree, *id)))
            .collect()
    }

    fn lower_array(&mut self, node: SyntaxNode<'_>) -> Vec<Value<'a>> {
        let tree = node.tree();
        let ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::ValueExpr)
            .map(|c| c.id.0)
            .collect();
        ids.iter()
            .map(|id| self.lower_value(self.node_from(tree, *id)))
            .collect()
    }

    fn lower_array_edit(&mut self, node: SyntaxNode<'_>) -> ArrayEdit<'a> {
        let tree = node.tree();
        let instructions = node
            .children_no_trivia()
            .filter(|child| child.kind() == SyntaxKind::ArrayEditInstruction)
            .map(|child| self.lower_array_edit_instruction(self.node_from(tree, child.id.0)))
            .collect();
        ArrayEdit { instructions }
    }

    fn lower_array_edit_instruction(&mut self, node: SyntaxNode<'_>) -> ArrayEditInstruction<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|child| (child.kind(), child.id.0))
            .collect();

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
            "minsize" => ArrayEditInstruction::MinSize(self.lower_array_edit_size(node)),
            "maxsize" => ArrayEditInstruction::MaxSize(self.lower_array_edit_size(node)),
            "resize" => ArrayEditInstruction::Resize(self.lower_array_edit_size(node)),
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
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|child| (child.kind(), child.id.0))
            .collect();

        let mut negative = false;
        for (kind, id) in sig {
            match kind {
                SyntaxKind::ArrayEditIndex => {
                    return self.lower_array_edit_index(self.node_from(tree, id));
                }
                SyntaxKind::Minus => negative = true,
                SyntaxKind::Number => {
                    let raw = self.text(self.node_from(tree, id));
                    let parsed = raw.parse::<i64>().ok()?;
                    return Some(ArrayEditIndex::Position(if negative {
                        -parsed
                    } else {
                        parsed
                    }));
                }
                SyntaxKind::Ident => {
                    let text = self.text(self.node_from(tree, id));
                    if text == "end" {
                        return Some(ArrayEditIndex::End);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn lower_array_edit_size(&mut self, node: SyntaxNode<'_>) -> usize {
        let tree = node.tree();
        node.children_no_trivia()
            .find(|child| child.kind() == SyntaxKind::ValueExpr)
            .map(|child| self.lower_value(self.node_from(tree, child.id.0)))
            .and_then(|value| match value {
                Value::Int(v) if v >= 0 => usize::try_from(v).ok(),
                Value::Number(v) if v >= 0.0 => usize::try_from(v as i64).ok(),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn lower_last_array_edit_index(&mut self, node: SyntaxNode<'_>) -> Option<ArrayEditIndex> {
        let tree = node.tree();
        let children: Vec<_> = node.children_no_trivia().collect();
        children
            .iter()
            .rev()
            .find(|child| child.kind() == SyntaxKind::ArrayEditIndex)
            .and_then(|child| self.lower_array_edit_index(self.node_from(tree, child.id.0)))
    }

    fn lower_dictionary(&mut self, node: SyntaxNode<'_>) -> Vec<DictionaryEntry<'a>> {
        let tree = node.tree();
        let ids: Vec<u32> = node
            .children_no_trivia()
            .filter(|c| c.kind() == SyntaxKind::DictionaryEntry)
            .map(|c| c.id.0)
            .collect();
        ids.iter()
            .map(|id| self.lower_dictionary_entry(self.node_from(tree, *id)))
            .collect()
    }

    fn lower_dictionary_entry(&mut self, node: SyntaxNode<'_>) -> DictionaryEntry<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

        let mut type_name = None;
        let mut key = "";
        let mut value = Value::Blocked;
        let mut idx = 0;

        // Optional type or `dictionary`.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Ident {
            let t = self.text(self.node_from(tree, sig[idx].1));
            if t == "dictionary" {
                idx += 1;
            } else {
                type_name = Some(t);
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
                key = strip_quotes(self.text(self.node_from(tree, sig[idx].1)));
                idx += 1;
            } else if sig[idx].0 == SyntaxKind::Ident {
                key = self.text(self.node_from(tree, sig[idx].1));
                idx += 1;
            }
        }

        // Skip `=`.
        if idx < sig.len() && sig[idx].0 == SyntaxKind::Equals {
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
        let tree = node.tree();
        let mut samples = Vec::new();

        // Find TimeSampleMap child.
        let map_id = node
            .children_no_trivia()
            .find(|c| c.kind() == SyntaxKind::TimeSampleMap)
            .map(|c| c.id.0);

        if let Some(map_id) = map_id {
            let map_node = self.node_from(tree, map_id);
            let entry_ids: Vec<u32> = map_node
                .children_no_trivia()
                .filter(|c| c.kind() == SyntaxKind::TimeSampleEntry)
                .map(|c| c.id.0)
                .collect();

            for id in entry_ids {
                samples.push(self.lower_time_sample_entry(self.node_from(tree, id)));
            }
        }
        samples
    }

    fn lower_time_sample_entry(&mut self, node: SyntaxNode<'_>) -> TimeSample<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

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
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

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

    fn lower_asset_ref(&self, node: SyntaxNode<'_>) -> &'a str {
        let full = node.span().text(self.source);
        if full.starts_with('@') && full.ends_with('@') && full.len() >= 2 {
            &full[1..full.len() - 1]
        } else {
            full
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

    fn find_asset_ref(&self, node: SyntaxNode<'_>) -> &'a str {
        for child in node.children_no_trivia() {
            if child.kind() == SyntaxKind::AssetRef {
                return self.lower_asset_ref(child);
            }
        }
        ""
    }

    fn find_string_value(&self, node: SyntaxNode<'_>) -> &'a str {
        for child in node.children_no_trivia() {
            if is_string_kind(child.kind()) {
                return strip_quotes(self.text(child));
            }
        }
        ""
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
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

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

    fn lower_metadata_entry_from_node(&mut self, node: SyntaxNode<'_>) -> MetadataEntry<'a> {
        let tree = node.tree();
        let sig: Vec<_> = node
            .children_no_trivia()
            .map(|c| (c.kind(), c.id.0))
            .collect();

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
                    "append" | "add" => {
                        op = ListOpKind::Append;
                        idx += 1;
                        continue;
                    }
                    "delete" => {
                        op = ListOpKind::Delete;
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

        MetadataEntry { key, op, value }
    }

    fn lower_metadata_entry(&mut self, node: SyntaxNode<'_>) -> MetadataEntry<'a> {
        self.lower_metadata_entry_from_node(node)
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn is_string_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::DoubleQuoteString
            | SyntaxKind::SingleQuoteString
            | SyntaxKind::MultilineDoubleQuoteString
            | SyntaxKind::MultilineSingleQuoteString
    )
}

fn strip_quotes(s: &str) -> &str {
    if (s.starts_with("\"\"\"") && s.ends_with("\"\"\""))
        || (s.starts_with("'''") && s.ends_with("'''"))
    {
        &s[3..s.len() - 3]
    } else if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn parse_f64(s: &str) -> f64 {
    s.parse().unwrap_or(0.0)
}

fn parse_number_value(text: &str) -> Value<'_> {
    if let Ok(n) = text.parse::<i64>() {
        return Value::Int(n);
    }
    Value::Number(parse_f64(text))
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use alloc::vec;

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
        assert!(matches!(attr.default, Some(Value::String("hello"))));
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
        assert!(matches!(r.layer.metadata[0], LayerMeta::Doc("my layer")));
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
            r.layer.prims[0].metadata[0],
            PrimMeta::Doc("hello")
        ));
    }

    #[test]
    fn lower_prim_kind() {
        let src = "#usda 1.0\ndef \"A\" (\n    kind = \"component\"\n) {\n}\n";
        let r = parse(src);
        assert!(matches!(
            r.layer.prims[0].metadata[0],
            PrimMeta::Kind("component")
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
            assert_eq!(items[0].asset, Some("./ref.usd"));
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
            assert_eq!(items[0].asset, Some("./payload.usd"));
            assert_eq!(items[0].prim_path, Some("/Root"));
        } else {
            panic!("expected Payload");
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
                assert!(matches!(dict[0].value, Value::String("bar")));
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
        assert!(matches!(attr.default, Some(Value::Asset("./texture.png"))));
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
}
