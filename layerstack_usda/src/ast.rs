// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Abstract syntax tree for USDA files.
//!
//! These types represent what was *authored* in a `.usda` file, not what
//! composition produces. All string data borrows from the source text via
//! lifetime `'a`, making the AST zero-copy.
//!
//! The AST is produced by [`crate::parser::parse`] and consumed by
//! [`crate::emit`] to create layerstack [`Layer`](layerstack::Layer) /
//! [`PrimSpec`](layerstack::PrimSpec) values.

use alloc::string::String;
use alloc::vec::Vec;

use crate::Span;

// ── Top-level ──────────────────────────────────────────────────────────

/// A parsed USDA layer.
#[derive(Debug)]
pub struct Layer<'a> {
    /// Full source span.
    pub span: Span,
    /// Format version from the `#usda X.Y` header (e.g., `"1.0"`).
    pub version: &'a str,
    /// Layer-level metadata fields.
    pub metadata: Vec<LayerMeta<'a>>,
    /// Root prim definitions.
    pub prims: Vec<Prim<'a>>,
    /// Root prim ordering (`reorder rootPrims = [...]`).
    pub root_prim_order: Option<Vec<&'a str>>,
}

// ── Layer metadata ─────────────────────────────────────────────────────

/// A layer-level metadata entry.
#[derive(Debug)]
pub enum LayerMeta<'a> {
    /// `subLayers = [@...@, ...]`
    SubLayers(Vec<SubLayerItem<'a>>),
    /// `relocates = { <src>: <dst>, ... }`
    Relocates(Vec<RelocateEntry<'a>>),
    /// `doc = "..."`
    Doc(&'a str),
    /// Generic `key = value` metadata.
    Custom(MetadataEntry<'a>),
}

/// A sublayer item with optional layer offset.
#[derive(Debug)]
pub struct SubLayerItem<'a> {
    /// Asset path (without `@` delimiters).
    pub asset: &'a str,
    /// Optional layer offset.
    pub offset: Option<f64>,
    /// Optional layer scale.
    pub scale: Option<f64>,
}

/// A relocates entry: `<source> : <target>`.
#[derive(Debug)]
pub struct RelocateEntry<'a> {
    /// Source path (without `<>` delimiters).
    pub source: &'a str,
    /// Target path (without `<>` delimiters).
    pub target: &'a str,
}

// ── Prims ──────────────────────────────────────────────────────────────

/// A prim specifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Specifier {
    /// `def`
    Def,
    /// `over`
    Over,
    /// `class`
    Class,
}

/// A prim definition.
#[derive(Debug)]
pub struct Prim<'a> {
    /// Source span of the entire prim.
    pub span: Span,
    /// The specifier (`def`, `over`, `class`).
    pub specifier: Specifier,
    /// Optional type name (e.g., `Mesh`, `Scope`).
    pub type_name: Option<&'a str>,
    /// Prim name (from the quoted string).
    pub name: &'a str,
    /// Prim metadata block.
    pub metadata: Vec<PrimMeta<'a>>,
    /// Body contents: properties, child prims, variant sets.
    pub children: Vec<PrimChild<'a>>,
}

/// A child item inside a prim body.
#[derive(Debug)]
pub enum PrimChild<'a> {
    /// An attribute definition.
    Attribute(Attribute<'a>),
    /// A relationship definition.
    Relationship(Relationship<'a>),
    /// A child prim definition.
    Prim(Prim<'a>),
    /// A variant set definition.
    VariantSet(VariantSet<'a>),
    /// `reorder nameChildren = [...]`
    ReorderNameChildren(Vec<&'a str>),
    /// `reorder properties = [...]`
    ReorderProperties(Vec<&'a str>),
}

// ── Prim metadata ──────────────────────────────────────────────────────

/// A prim-level metadata entry.
#[derive(Debug)]
pub enum PrimMeta<'a> {
    /// `references = ...` / `prepend references = ...`
    References(ListOpArc<'a>),
    /// `inherits = ...` / `prepend inherits = ...`
    Inherits(ListOpPaths<'a>),
    /// `specializes = ...` / `prepend specializes = ...`
    Specializes(ListOpPaths<'a>),
    /// `payload = ...` / `prepend payload = ...`
    Payload(ListOpArc<'a>),
    /// `variants = { ... }`
    Variants(Vec<VariantSelection<'a>>),
    /// `variantSets = [...]` / `prepend variantSets = [...]`
    VariantSets(ListOp<&'a str>),
    /// `kind = "..."`
    Kind(&'a str),
    /// `doc = "..."`
    Doc(&'a str),
    /// Generic key-value metadata.
    Custom(MetadataEntry<'a>),
}

/// A variant selection: `"setName" = "branchName"`.
#[derive(Debug)]
pub struct VariantSelection<'a> {
    /// Variant set name.
    pub set_name: &'a str,
    /// Selected variant branch name.
    pub branch_name: &'a str,
}

// ── Properties ─────────────────────────────────────────────────────────

/// An attribute definition.
#[derive(Debug)]
pub struct Attribute<'a> {
    /// Source span.
    pub span: Span,
    /// Whether `custom` was specified.
    pub custom: bool,
    /// Whether `uniform` was specified.
    pub uniform: bool,
    /// Type name (e.g., `int`, `float3`, `token`).
    pub type_name: &'a str,
    /// Whether this is an array type (`[]` suffix).
    pub is_array: bool,
    /// Attribute name (may be namespaced, e.g., `primvars:displayColor`).
    pub name: &'a str,
    /// Default value assignment.
    pub default: Option<Value<'a>>,
    /// `.timeSamples = { ... }`
    pub time_samples: Option<Vec<TimeSample<'a>>>,
    /// `.connect = ...` / `prepend ... .connect = ...`
    pub connection: Option<Connection<'a>>,
    /// Attribute metadata block.
    pub metadata: Vec<MetadataEntry<'a>>,
}

/// A relationship definition.
#[derive(Debug)]
pub struct Relationship<'a> {
    /// Source span.
    pub span: Span,
    /// Whether `custom` was specified.
    pub custom: bool,
    /// List-op kind for this relationship statement.
    pub op: ListOpKind,
    /// Relationship name (may be namespaced).
    pub name: &'a str,
    /// Target paths.
    pub targets: Option<Vec<&'a str>>,
    /// Relationship metadata block.
    pub metadata: Vec<MetadataEntry<'a>>,
}

/// An attribute connection.
#[derive(Debug)]
pub struct Connection<'a> {
    /// The list-op kind.
    pub op: ListOpKind,
    /// Connected paths.
    pub targets: Vec<&'a str>,
}

/// A time sample entry: `time : value`.
#[derive(Debug)]
pub struct TimeSample<'a> {
    /// The time code.
    pub time: f64,
    /// The value (or `None` for blocked).
    pub value: Option<Value<'a>>,
}

// ── Variant sets ───────────────────────────────────────────────────────

/// A variant set definition.
#[derive(Debug)]
pub struct VariantSet<'a> {
    /// Source span.
    pub span: Span,
    /// Variant set name.
    pub name: &'a str,
    /// Variant branches.
    pub branches: Vec<VariantBranch<'a>>,
}

/// A variant branch.
#[derive(Debug)]
pub struct VariantBranch<'a> {
    /// Branch name.
    pub name: &'a str,
    /// Branch metadata.
    pub metadata: Vec<PrimMeta<'a>>,
    /// Branch body (same contents as a prim body).
    pub children: Vec<PrimChild<'a>>,
}

// ── Values ─────────────────────────────────────────────────────────────

/// A typed value in a USDA file.
#[derive(Debug)]
pub enum Value<'a> {
    /// A numeric value (stored as text for exact roundtripping).
    Number(f64),
    /// An integer value.
    Int(i64),
    /// A string literal (contents only, quotes stripped).
    String(&'a str),
    /// An identifier used as a token value.
    Identifier(&'a str),
    /// An asset reference (contents only, `@` stripped).
    Asset(&'a str),
    /// A path reference (contents only, `<>` stripped).
    Path(&'a str),
    /// `true` or `false`.
    Bool(bool),
    /// A tuple value: `(1.0, 2.0, 3.0)`.
    Tuple(Vec<Self>),
    /// An array/list value: `[1, 2, 3]`.
    Array(Vec<Self>),
    /// A sparse array edit value: `edit (...)`.
    ArrayEdit(ArrayEdit<'a>),
    /// A dictionary: `{ "key": "value", ... }`.
    Dictionary(Vec<DictionaryEntry<'a>>),
    /// Blocked value (`None` keyword).
    Blocked,
}

/// A sparse array edit value.
#[derive(Debug)]
pub struct ArrayEdit<'a> {
    /// Instructions applied in order.
    pub instructions: Vec<ArrayEditInstruction<'a>>,
}

/// A sparse array edit instruction.
#[derive(Debug)]
pub enum ArrayEditInstruction<'a> {
    /// `write value to [index]`
    Write {
        /// Source operand.
        src: ArrayEditOperand<'a>,
        /// Destination index.
        index: ArrayEditIndex,
    },
    /// `insert value at [index]`
    Insert {
        /// Source operand.
        src: ArrayEditOperand<'a>,
        /// Insertion index.
        index: ArrayEditIndex,
    },
    /// `erase [index]`
    Erase {
        /// Erased index.
        index: ArrayEditIndex,
    },
    /// `minsize N`
    MinSize(usize),
    /// `maxsize N`
    MaxSize(usize),
    /// `resize N`
    Resize(usize),
}

/// A sparse array edit operand.
#[derive(Debug)]
pub enum ArrayEditOperand<'a> {
    /// A literal element value.
    Literal(Value<'a>),
    /// Copy the array element currently stored at `index`.
    CopyFrom(ArrayEditIndex),
}

/// An array edit index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrayEditIndex {
    /// Numeric position, including negative indexing.
    Position(i64),
    /// The index past the final element.
    End,
}

/// A dictionary entry.
#[derive(Debug)]
pub struct DictionaryEntry<'a> {
    /// Optional type annotation (e.g., `double`, `string`).
    pub type_name: Option<&'a str>,
    /// Key name.
    pub key: &'a str,
    /// Value.
    pub value: Value<'a>,
}

// ── List operations ────────────────────────────────────────────────────

/// The kind of list operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListOpKind {
    /// No list-op prefix — explicit assignment.
    Explicit,
    /// `prepend`
    Prepend,
    /// `append`
    Append,
    /// `delete`
    Delete,
}

/// A list-op applied to a generic list.
#[derive(Debug)]
pub struct ListOp<T> {
    /// The kind of list operation.
    pub kind: ListOpKind,
    /// The items (`None` for `= None`).
    pub items: Option<Vec<T>>,
}

/// A list-op applied to composition arc references.
#[derive(Debug)]
pub struct ListOpArc<'a> {
    /// The kind of list operation.
    pub kind: ListOpKind,
    /// The arc entries (`None` for `= None`).
    pub items: Option<Vec<ArcRef<'a>>>,
}

/// A composition arc reference (used by references and payloads).
#[derive(Debug)]
pub struct ArcRef<'a> {
    /// Optional asset path.
    pub asset: Option<&'a str>,
    /// Target prim path.
    pub prim_path: Option<&'a str>,
    /// Optional layer offset.
    pub offset: Option<f64>,
    /// Optional layer scale.
    pub scale: Option<f64>,
}

/// A list-op applied to path lists (inherits, specializes).
#[derive(Debug)]
pub struct ListOpPaths<'a> {
    /// The kind of list operation.
    pub kind: ListOpKind,
    /// The paths (`None` for `= None`).
    pub items: Option<Vec<&'a str>>,
}

// ── Generic metadata ───────────────────────────────────────────────────

/// A generic metadata key-value entry.
#[derive(Debug)]
pub struct MetadataEntry<'a> {
    /// Key name.
    pub key: &'a str,
    /// List-op kind (explicit, prepend, append, delete).
    pub op: ListOpKind,
    /// Value.
    pub value: MetadataValue<'a>,
}

/// A metadata value.
#[derive(Debug)]
pub enum MetadataValue<'a> {
    /// A typed value.
    Value(Value<'a>),
    /// `None` (blocked).
    None,
    /// A nested dictionary.
    Dictionary(Vec<DictionaryEntry<'a>>),
    /// A string value.
    String(String),
}

// ── Parse result ───────────────────────────────────────────────────────

/// The result of parsing a USDA file.
#[derive(Debug)]
pub struct ParseResult<'a> {
    /// The parsed layer AST.
    pub layer: Layer<'a>,
    /// Any diagnostics emitted during parsing.
    pub diagnostics: Vec<crate::diagnostic::Diagnostic>,
}
