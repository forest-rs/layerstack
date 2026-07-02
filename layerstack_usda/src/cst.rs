// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lossless concrete syntax tree for USDA files.
//!
//! The CST preserves every byte of the original source — whitespace, comments,
//! punctuation — enabling formatters, refactoring tools, and syntax
//! highlighting. It is a flat arena tree stored in pre-order traversal.
//!
//! The CST is produced by the parser via [`TreeBuilder`] and consumed by
//! [`crate::lower`] to create the typed AST.

use alloc::vec::Vec;

use smallvec::SmallVec;

use crate::Span;
use crate::lexer::TokenKind;

// ── SyntaxKind ─────────────────────────────────────────────────────────

/// Discriminant for both leaf tokens and interior CST nodes.
///
/// Token kinds (`Whitespace` … `Error`) mirror [`TokenKind`] 1:1.
/// Composite kinds (`SourceFile` … `Error_`) represent grammar productions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum SyntaxKind {
    // ── Tokens (mirror TokenKind exactly) ─────────────────────────
    /// Whitespace (spaces, tabs — not newlines).
    Whitespace = 0,
    /// A newline (`\n` or `\r\n`).
    Newline,
    /// `# ...` comment.
    PythonComment,
    /// `// ...` comment.
    CppComment,
    /// `/* ... */` comment.
    BlockComment,
    /// Numeric literal.
    Number,
    /// `'...'` string.
    SingleQuoteString,
    /// `"..."` string.
    DoubleQuoteString,
    /// `'''...'''` multiline string.
    MultilineSingleQuoteString,
    /// `"""..."""` multiline string.
    MultilineDoubleQuoteString,
    /// Identifier or keyword.
    Ident,
    /// `(`
    LeftParen,
    /// `)`
    RightParen,
    /// `[`
    LeftBracket,
    /// `]`
    RightBracket,
    /// `{`
    LeftBrace,
    /// `}`
    RightBrace,
    /// `<`
    LeftAngle,
    /// `>`
    RightAngle,
    /// `@`
    At,
    /// `&`
    Ampersand,
    /// `*`
    Asterisk,
    /// `:`
    Colon,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `=`
    Equals,
    /// `-`
    Minus,
    /// `+`
    Plus,
    /// `#`
    Pound,
    /// `;`
    Semicolon,
    /// Lexer error token.
    ErrorToken,

    // ── Composite nodes (grammar productions) ─────────────────────
    /// Root node — entire USDA file.
    SourceFile,
    /// `#usda X.Y` header.
    Header,
    /// Layer metadata block `( ... )`.
    LayerMetadata,
    /// A single layer metadata entry.
    LayerMetaEntry,
    /// `subLayers = [...]`
    SubLayersList,
    /// A single sublayer item with optional offset/scale.
    SubLayerItem,
    /// `relocates = { ... }`
    RelocatesMap,
    /// A single `<src> : <dst>` relocate entry.
    RelocateEntry,
    /// Prim definition (`def`/`over`/`class`).
    PrimDef,
    /// Prim metadata block `( ... )`.
    PrimMetadata,
    /// A single prim metadata entry.
    PrimMetaEntry,
    /// Prim body `{ ... }`.
    PrimBody,
    /// Attribute definition.
    AttributeDef,
    /// Relationship definition.
    RelationshipDef,
    /// Variant set definition.
    VariantSetDef,
    /// A single variant branch.
    VariantBranch,
    /// `reorder ...` statement.
    ReorderStmt,
    /// An asset reference `@...@`.
    AssetRef,
    /// A path reference `<...>`.
    PathRef,
    /// A namespaced name (`foo:bar:baz`).
    NamespacedName,
    /// A value expression (number, string, tuple, array, dict, etc.).
    ValueExpr,
    /// Tuple value `(...)`.
    TupleValue,
    /// Array value `[...]`.
    ArrayValue,
    /// Sparse array edit value `edit (...)`.
    ArrayEditValue,
    /// Sparse array edit instruction.
    ArrayEditInstruction,
    /// Sparse array edit index `[0]`, `[-1]`, or `[end]`.
    ArrayEditIndex,
    /// Dictionary value `{ ... }`.
    DictionaryValue,
    /// A single dictionary entry.
    DictionaryEntry,
    /// Time-sample map `{ time: value, ... }`.
    TimeSampleMap,
    /// A single time-sample entry.
    TimeSampleEntry,
    /// Composition arc reference.
    ArcRef,
    /// Composition arc list (references, payload, etc.).
    ArcList,
    /// Path list (inherits, specializes).
    PathList,
    /// Name list `[...]`.
    NameList,
    /// Variant selections `{ ... }`.
    VariantSelections,
    /// A single variant selection entry.
    VariantSelectionEntry,
    /// Generic metadata entry `key = value`.
    MetadataEntry,
    /// Connection suffix `.connect = ...`.
    ConnectionSuffix,
    /// `.timeSamples = { ... }` suffix.
    TimeSamplesSuffix,
    /// Layer offset parameters `( offset = N; scale = N )`.
    LayerOffsetParams,
    /// An error recovery node wrapping unexpected tokens.
    ErrorNode,
}

impl SyntaxKind {
    /// Returns `true` if this is a trivia token (whitespace or comment).
    #[inline]
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            Self::Whitespace
                | Self::Newline
                | Self::PythonComment
                | Self::CppComment
                | Self::BlockComment
        )
    }

    /// Returns `true` if this kind represents a leaf token (not a composite node).
    #[inline]
    pub fn is_token(self) -> bool {
        (self as u8) <= (Self::ErrorToken as u8)
    }
}

impl From<TokenKind> for SyntaxKind {
    fn from(kind: TokenKind) -> Self {
        match kind {
            TokenKind::Whitespace => Self::Whitespace,
            TokenKind::Newline => Self::Newline,
            TokenKind::PythonComment => Self::PythonComment,
            TokenKind::CppComment => Self::CppComment,
            TokenKind::BlockComment => Self::BlockComment,
            TokenKind::Number => Self::Number,
            TokenKind::SingleQuoteString => Self::SingleQuoteString,
            TokenKind::DoubleQuoteString => Self::DoubleQuoteString,
            TokenKind::MultilineSingleQuoteString => Self::MultilineSingleQuoteString,
            TokenKind::MultilineDoubleQuoteString => Self::MultilineDoubleQuoteString,
            TokenKind::Ident => Self::Ident,
            TokenKind::LeftParen => Self::LeftParen,
            TokenKind::RightParen => Self::RightParen,
            TokenKind::LeftBracket => Self::LeftBracket,
            TokenKind::RightBracket => Self::RightBracket,
            TokenKind::LeftBrace => Self::LeftBrace,
            TokenKind::RightBrace => Self::RightBrace,
            TokenKind::LeftAngle => Self::LeftAngle,
            TokenKind::RightAngle => Self::RightAngle,
            TokenKind::At => Self::At,
            TokenKind::Ampersand => Self::Ampersand,
            TokenKind::Asterisk => Self::Asterisk,
            TokenKind::Colon => Self::Colon,
            TokenKind::Comma => Self::Comma,
            TokenKind::Dot => Self::Dot,
            TokenKind::Equals => Self::Equals,
            TokenKind::Minus => Self::Minus,
            TokenKind::Plus => Self::Plus,
            TokenKind::Pound => Self::Pound,
            TokenKind::Semicolon => Self::Semicolon,
            TokenKind::Error => Self::ErrorToken,
            // TokenKind is `#[non_exhaustive]`; future variants map to ErrorToken.
            #[allow(unreachable_patterns, reason = "non_exhaustive future-proofing")]
            _ => Self::ErrorToken,
        }
    }
}

// ── Node storage ───────────────────────────────────────────────────────

/// Index into the [`SyntaxTree`] node array.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Sentinel value for "no parent" (the root node).
    pub const NONE: Self = Self(u32::MAX);
}

/// A single node in the flat CST arena.
///
/// Interior nodes have children; leaf nodes (tokens) have `child_count == 0`.
/// Nodes are stored in pre-order so children of node `i` start at
/// `i + 1` and span `i.subtree_len - 1` entries total.
#[derive(Clone, Debug)]
pub struct NodeData {
    /// What kind of syntax element this node represents.
    pub kind: SyntaxKind,
    /// Byte range in the source text.
    pub span: Span,
    /// Parent node index, or [`NodeId::NONE`] for the root.
    pub parent: NodeId,
    /// Number of direct children.
    pub child_count: u32,
    /// This node's index among its parent's children (0-based).
    pub index_in_parent: u32,
    /// Total number of nodes in this subtree (self + all descendants).
    /// Leaf tokens have `subtree_len == 1`.
    pub subtree_len: u32,
}

// ── SyntaxTree ─────────────────────────────────────────────────────────

/// A lossless concrete syntax tree for a USDA file.
///
/// All nodes are stored in a flat `Vec` in pre-order traversal order.
/// Use [`SyntaxNode`] handles to navigate the tree.
#[derive(Clone, Debug)]
pub struct SyntaxTree {
    nodes: Vec<NodeData>,
    #[expect(dead_code, reason = "useful for future range validation")]
    source_len: u32,
}

impl SyntaxTree {
    /// Returns the root node handle.
    #[inline]
    pub fn root(&self) -> SyntaxNode<'_> {
        SyntaxNode {
            tree: self,
            id: NodeId(0),
        }
    }

    /// Returns a node handle by ID.
    #[inline]
    pub fn node(&self, id: NodeId) -> SyntaxNode<'_> {
        SyntaxNode { tree: self, id }
    }

    /// Returns the number of nodes in the tree.
    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` if the tree is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the raw node data slice.
    #[inline]
    pub fn nodes(&self) -> &[NodeData] {
        &self.nodes
    }

    /// Reconstructs the full source text from leaf spans.
    ///
    /// For a correctly built tree, this returns the original source verbatim.
    pub fn to_text<'a>(&self, source: &'a str) -> &'a str {
        if self.nodes.is_empty() {
            return "";
        }
        let root = &self.nodes[0];
        root.span.text(source)
    }
}

// ── SyntaxNode (cursor) ───────────────────────────────────────────────

/// A lightweight handle into a [`SyntaxTree`].
#[derive(Clone, Copy, Debug)]
pub struct SyntaxNode<'a> {
    tree: &'a SyntaxTree,
    /// The node's index in the arena.
    pub id: NodeId,
}

impl<'a> SyntaxNode<'a> {
    /// Returns a reference to the owning [`SyntaxTree`].
    #[inline]
    pub fn tree(self) -> &'a SyntaxTree {
        self.tree
    }

    /// Returns the underlying [`NodeData`].
    #[inline]
    pub fn data(self) -> &'a NodeData {
        &self.tree.nodes[self.id.0 as usize]
    }

    /// Returns the syntax kind.
    #[inline]
    pub fn kind(self) -> SyntaxKind {
        self.data().kind
    }

    /// Returns the source span.
    #[inline]
    pub fn span(self) -> Span {
        self.data().span
    }

    /// Extracts this node's text from the source.
    #[inline]
    pub fn text(self, source: &str) -> &str {
        self.data().span.text(source)
    }

    /// Returns the parent node, or `None` for the root.
    pub fn parent(self) -> Option<Self> {
        let p = self.data().parent;
        if p == NodeId::NONE {
            None
        } else {
            Some(self.tree.node(p))
        }
    }

    /// Returns an iterator over direct children.
    pub fn children(self) -> Children<'a> {
        let d = self.data();
        // First child is at self.id + 1 in pre-order.
        let first_child = if d.child_count > 0 { self.id.0 + 1 } else { 0 };
        Children {
            tree: self.tree,
            index: first_child,
            remaining: d.child_count,
        }
    }

    /// Returns an iterator over direct children, skipping trivia.
    pub fn children_no_trivia(self) -> impl Iterator<Item = Self> {
        self.children().filter(|n| !n.kind().is_trivia())
    }

    /// Finds the first child with the given kind.
    pub fn child_by_kind(self, kind: SyntaxKind) -> Option<Self> {
        self.children().find(|n| n.kind() == kind)
    }

    /// Returns the next sibling, or `None`.
    pub fn next_sibling(self) -> Option<Self> {
        let d = self.data();
        let parent = self.parent()?;
        let pd = parent.data();
        if d.index_in_parent + 1 >= pd.child_count {
            return None;
        }
        // Next sibling is right after this node's subtree.
        let next_id = self.id.0 + d.subtree_len;
        Some(self.tree.node(NodeId(next_id)))
    }

    /// Returns the previous sibling, or `None`.
    pub fn prev_sibling(self) -> Option<Self> {
        let d = self.data();
        if d.index_in_parent == 0 {
            return None;
        }
        // Walk from parent's first child to find the sibling before us.
        let parent = self.parent()?;
        let mut child_id = parent.id.0 + 1; // first child
        for _ in 0..d.index_in_parent - 1 {
            let child_data = &self.tree.nodes[child_id as usize];
            child_id += child_data.subtree_len;
        }
        Some(self.tree.node(NodeId(child_id)))
    }

    /// Returns an iterator over ancestors (parent, grandparent, …).
    pub fn ancestors(self) -> Ancestors<'a> {
        Ancestors {
            current: Some(self),
        }
    }

    /// Finds the deepest leaf token covering the given byte offset.
    pub fn token_at_offset(self, offset: u32) -> Option<Self> {
        let d = self.data();
        if offset < d.span.start || offset >= d.span.end {
            return None;
        }
        // If this is a leaf, return self.
        if d.child_count == 0 {
            return Some(self);
        }
        // Binary search children by span start.
        for child in self.children() {
            if let Some(found) = child.token_at_offset(offset) {
                return Some(found);
            }
        }
        None
    }
}

/// Iterator over the direct children of a node.
#[derive(Clone, Debug)]
pub struct Children<'a> {
    tree: &'a SyntaxTree,
    index: u32,
    remaining: u32,
}

impl<'a> Iterator for Children<'a> {
    type Item = SyntaxNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let node = self.tree.node(NodeId(self.index));
        // Skip over the entire subtree of this child.
        self.index += node.data().subtree_len;
        self.remaining -= 1;
        Some(node)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let n = self.remaining as usize;
        (n, Some(n))
    }
}

impl ExactSizeIterator for Children<'_> {}

/// Iterator over ancestors of a node.
#[derive(Clone, Debug)]
pub struct Ancestors<'a> {
    current: Option<SyntaxNode<'a>>,
}

impl<'a> Iterator for Ancestors<'a> {
    type Item = SyntaxNode<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let node = self.current?;
        self.current = node.parent();
        Some(node)
    }
}

// ── TreeBuilder ────────────────────────────────────────────────────────

/// Builds a [`SyntaxTree`] via `start_node` / `token` / `finish_node` calls.
///
/// The parser drives this builder. Interior nodes are opened with
/// [`start_node`](Self::start_node) and closed with
/// [`finish_node`](Self::finish_node). Leaf tokens are added with
/// [`token`](Self::token).
#[derive(Debug)]
pub struct TreeBuilder {
    nodes: Vec<NodeData>,
    /// Stack of open interior nodes (indices into `nodes`).
    parent_stack: SmallVec<[u32; 16]>,
    /// For each open node, tracks how many direct children have been added.
    child_counts: SmallVec<[u32; 16]>,
    source_len: u32,
}

impl TreeBuilder {
    /// Creates a new builder for a source of the given length.
    pub fn new(source_len: u32) -> Self {
        Self {
            nodes: Vec::new(),
            parent_stack: SmallVec::new(),
            child_counts: SmallVec::new(),
            source_len,
        }
    }

    /// Opens a new interior node. Must be paired with [`finish_node`](Self::finish_node).
    pub fn start_node(&mut self, kind: SyntaxKind, start: u32) {
        let parent = self
            .parent_stack
            .last()
            .copied()
            .map_or(NodeId::NONE, NodeId);
        let index_in_parent = self.child_counts.last().copied().unwrap_or(0);

        let id = self.nodes.len() as u32;
        self.nodes.push(NodeData {
            kind,
            span: Span::new(start, start), // end filled in by finish_node
            parent,
            child_count: 0, // updated by finish_node
            index_in_parent,
            subtree_len: 1, // updated by finish_node
        });

        // Register as child of parent.
        if let Some(count) = self.child_counts.last_mut() {
            *count += 1;
        }

        self.parent_stack.push(id);
        self.child_counts.push(0);
    }

    /// Adds a leaf token to the current interior node.
    pub fn token(&mut self, kind: SyntaxKind, span: Span) {
        let parent = self
            .parent_stack
            .last()
            .copied()
            .map_or(NodeId::NONE, NodeId);
        let index_in_parent = self.child_counts.last().copied().unwrap_or(0);

        self.nodes.push(NodeData {
            kind,
            span,
            parent,
            child_count: 0,
            index_in_parent,
            subtree_len: 1, // leaf token
        });

        if let Some(count) = self.child_counts.last_mut() {
            *count += 1;
        }
    }

    /// Closes the current interior node, setting its span end and child layout.
    pub fn finish_node(&mut self, end: u32) {
        let child_count = self.child_counts.pop().expect("unbalanced finish_node");
        let id = self.parent_stack.pop().expect("unbalanced finish_node") as usize;

        self.nodes[id].span.end = end;
        self.nodes[id].child_count = child_count;

        // subtree_len = total nodes from this node's index to current end.
        self.nodes[id].subtree_len = (self.nodes.len() - id) as u32;
    }

    /// Returns the current number of nodes (for rollback support).
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Truncates back to `count` nodes (for rollback support).
    pub fn truncate(&mut self, count: usize) {
        self.nodes.truncate(count);
    }

    /// Consumes the builder and produces a [`SyntaxTree`].
    ///
    /// # Panics
    ///
    /// Panics if there are unclosed nodes.
    pub fn finish(self) -> SyntaxTree {
        assert!(
            self.parent_stack.is_empty(),
            "TreeBuilder has {} unclosed nodes",
            self.parent_stack.len()
        );
        SyntaxTree {
            nodes: self.nodes,
            source_len: self.source_len,
        }
    }
}

// ── Parse result ───────────────────────────────────────────────────────

/// Result of parsing USDA source into a CST.
#[derive(Clone, Debug)]
pub struct CstParseResult {
    /// The lossless concrete syntax tree.
    pub tree: SyntaxTree,
    /// Diagnostics emitted during parsing.
    pub diagnostics: Vec<crate::diagnostic::Diagnostic>,
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    #[test]
    fn builder_single_token() {
        let mut b = TreeBuilder::new(5);
        b.start_node(SyntaxKind::SourceFile, 0);
        b.token(SyntaxKind::Ident, Span::new(0, 5));
        b.finish_node(5);
        let tree = b.finish();

        assert_eq!(tree.len(), 2);
        let root = tree.root();
        assert_eq!(root.kind(), SyntaxKind::SourceFile);
        assert_eq!(root.span(), Span::new(0, 5));
        assert_eq!(root.children().len(), 1);

        let child = root.children().next().unwrap();
        assert_eq!(child.kind(), SyntaxKind::Ident);
        assert_eq!(child.parent().unwrap().kind(), SyntaxKind::SourceFile);
    }

    #[test]
    fn builder_nested_nodes() {
        let mut b = TreeBuilder::new(10);
        b.start_node(SyntaxKind::SourceFile, 0);
        {
            b.start_node(SyntaxKind::PrimDef, 0);
            b.token(SyntaxKind::Ident, Span::new(0, 3));
            b.token(SyntaxKind::Whitespace, Span::new(3, 4));
            b.token(SyntaxKind::DoubleQuoteString, Span::new(4, 9));
            b.finish_node(9);
        }
        b.finish_node(10);
        let tree = b.finish();

        assert_eq!(tree.len(), 5);
        let root = tree.root();
        assert_eq!(root.children().len(), 1);

        let prim = root.children().next().unwrap();
        assert_eq!(prim.kind(), SyntaxKind::PrimDef);
        assert_eq!(prim.children().len(), 3);
        assert_eq!(prim.children_no_trivia().count(), 2);

        // Sibling navigation
        let first = prim.children().next().unwrap();
        assert!(first.prev_sibling().is_none());
        let second = first.next_sibling().unwrap();
        assert_eq!(second.kind(), SyntaxKind::Whitespace);
        let third = second.next_sibling().unwrap();
        assert_eq!(third.kind(), SyntaxKind::DoubleQuoteString);
        assert!(third.next_sibling().is_none());
    }

    #[test]
    fn builder_child_by_kind() {
        let mut b = TreeBuilder::new(5);
        b.start_node(SyntaxKind::SourceFile, 0);
        b.token(SyntaxKind::Whitespace, Span::new(0, 1));
        b.token(SyntaxKind::Ident, Span::new(1, 4));
        b.token(SyntaxKind::Newline, Span::new(4, 5));
        b.finish_node(5);
        let tree = b.finish();

        let root = tree.root();
        let ident = root.child_by_kind(SyntaxKind::Ident).unwrap();
        assert_eq!(ident.span(), Span::new(1, 4));
    }

    #[test]
    fn ancestors_traversal() {
        let mut b = TreeBuilder::new(5);
        b.start_node(SyntaxKind::SourceFile, 0);
        b.start_node(SyntaxKind::PrimDef, 0);
        b.start_node(SyntaxKind::AttributeDef, 0);
        b.token(SyntaxKind::Ident, Span::new(0, 5));
        b.finish_node(5);
        b.finish_node(5);
        b.finish_node(5);
        let tree = b.finish();

        let leaf = tree.node(NodeId(3)); // the Ident token
        let kinds: Vec<_> = leaf.ancestors().map(|n| n.kind()).collect();
        assert_eq!(
            kinds,
            vec![
                SyntaxKind::Ident,
                SyntaxKind::AttributeDef,
                SyntaxKind::PrimDef,
                SyntaxKind::SourceFile,
            ]
        );
    }

    #[test]
    fn token_at_offset_finds_leaf() {
        let mut b = TreeBuilder::new(10);
        b.start_node(SyntaxKind::SourceFile, 0);
        b.token(SyntaxKind::Ident, Span::new(0, 3));
        b.token(SyntaxKind::Whitespace, Span::new(3, 4));
        b.token(SyntaxKind::Number, Span::new(4, 6));
        b.finish_node(6);
        let tree = b.finish();

        let found = tree.root().token_at_offset(4).unwrap();
        assert_eq!(found.kind(), SyntaxKind::Number);

        let found = tree.root().token_at_offset(0).unwrap();
        assert_eq!(found.kind(), SyntaxKind::Ident);

        assert!(tree.root().token_at_offset(10).is_none());
    }

    #[test]
    fn to_text_roundtrip() {
        let source = "hello world";
        let mut b = TreeBuilder::new(source.len() as u32);
        b.start_node(SyntaxKind::SourceFile, 0);
        b.token(SyntaxKind::Ident, Span::new(0, 5));
        b.token(SyntaxKind::Whitespace, Span::new(5, 6));
        b.token(SyntaxKind::Ident, Span::new(6, 11));
        b.finish_node(11);
        let tree = b.finish();

        assert_eq!(tree.to_text(source), source);
    }
}
