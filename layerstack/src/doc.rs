//! Document model: layers, prim specs, and composition arcs.
//!
//! Spec: AOUSD Core §6–§7 (scene description data model and opinions), plus §10
//! for arc-related fields (variants/references).

use alloc::{string::String, sync::Arc, vec::Vec};
use core::fmt;

use hashbrown::HashMap;

use crate::{
    interner::TokenId,
    interner::TokenInterner,
    listop::ListOp,
    path::{PathId, PathInterner},
};

/// Identifies a layer by stable ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerId(pub u64);

/// Prim specifier: determines how a prim spec contributes to composition.
///
/// Spec: AOUSD Core §7.6 (specifier field), §12.2.1 (specifier resolution).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Specifier {
    /// Concretely defining (`def`). The prim is fully defined.
    Def,
    /// Non-defining (`over`). Provides opinions without defining a new prim.
    Over,
    /// Abstractly defining (`class`). Defines a prim template not meant for
    /// direct use.
    Class,
}

/// A plain value that can be resolved by the kernel.
///
/// Covers all scalar types from AOUSD Core §6.2. Domain-specific compound
/// types (vectors, matrices, quaternions) should typically be encoded as
/// `Opaque` values in a higher-level profile crate.
///
/// Spec: AOUSD Core §6.2 (scene description data types), §16.3.10 (value
/// type encoding).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// No value.
    Null,
    /// A boolean (`bool`). Spec: §6.2.
    Bool(bool),
    /// An unsigned 8-bit integer (`uchar`). Spec: §6.2.
    UChar(u8),
    /// A signed 32-bit integer (`int`). Spec: §6.2.
    Int(i32),
    /// An unsigned 32-bit integer (`uint`). Spec: §6.2.
    UInt(u32),
    /// A signed 64-bit integer (`int64`). Spec: §6.2.
    Int64(i64),
    /// An unsigned 64-bit integer (`uint64`). Spec: §6.2.
    UInt64(u64),
    /// An IEEE 754 half-precision float (`half`), stored as raw bits.
    ///
    /// Spec: §6.2, §16.3.10.8 (IEEE 754-2008).
    Half(u16),
    /// A 32-bit float (`float`). Spec: §6.2.
    Float(f32),
    /// A 64-bit float (`double`). Spec: §6.2.
    Double(f64),
    /// A UTF-8 string (`string`). Spec: §6.2.
    String(Arc<str>),
    /// A token (interned string, `token`). Spec: §6.2.
    Token(TokenId),
    /// An asset path, distinct from a plain string.
    ///
    /// Asset paths undergo variable substitution and resolution (§9).
    /// They are used for layer references, texture paths, and other
    /// external resource identifiers.
    ///
    /// Spec: §6.2 (asset type), §9 (asset resolution).
    Asset(Arc<str>),
    /// A time code value (`timecode`), semantically a time in frames.
    ///
    /// Spec: §6.2.
    TimeCode(f64),
    /// Opaque bytes tagged with a type name.
    Opaque {
        /// The (interned) type name for these bytes.
        type_name: TokenId,
        /// The opaque payload.
        bytes: Arc<[u8]>,
    },
    /// Value block sentinel — suppresses weaker opinions.
    ///
    /// When encountered during value resolution, all weaker opinions are
    /// skipped and the fallback value is returned instead.
    ///
    /// Spec: AOUSD Core §12.3 (value blocking), §16.3.10.16 (`ValueBlock` type).
    Blocked,
    /// An ordered sequence of values (tuples and arrays).
    ///
    /// Stores both fixed-size tuples (e.g. `(1.0, 2.0, 3.0)` from a
    /// `float3` attribute) and variable-length arrays (e.g. `[1, 2, 3]`
    /// from an `int[]` attribute). Type semantics are carried externally
    /// by the attribute's type name, not by the value itself.
    ///
    /// Spec: AOUSD Core §6.2 (scene description data types).
    Array(Vec<Self>),
    /// A dictionary of string-keyed values, maintaining insertion order.
    ///
    /// Dictionary-valued fields use combining semantics during value
    /// resolution: dictionaries from multiple opinions are recursively
    /// merged rather than using strongest-wins.
    ///
    /// Spec: AOUSD Core §6.2 (dictionary type), §6.6.2.1 (dictionary
    /// combining), §12.2.5 (dictionary-valued metadata combining).
    Dictionary(Vec<(Arc<str>, Self)>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::UChar(v) => write!(f, "{v}"),
            Self::Int(v) => write!(f, "{v}"),
            Self::UInt(v) => write!(f, "{v}"),
            Self::Int64(v) => write!(f, "{v}"),
            Self::UInt64(v) => write!(f, "{v}"),
            Self::Half(v) => write!(f, "half(0x{v:04x})"),
            Self::Float(v) => write!(f, "{v}"),
            Self::Double(v) => write!(f, "{v}"),
            Self::String(v) => write!(f, "{v}"),
            Self::Token(v) => write!(f, "token({v:?})"),
            Self::Asset(v) => write!(f, "@{v}@"),
            Self::TimeCode(v) => write!(f, "{v}"),
            Self::Opaque { type_name, bytes } => {
                write!(f, "opaque({type_name:?}, {} bytes)", bytes.len())
            }
            Self::Blocked => write!(f, "blocked"),
            Self::Array(items) => {
                write!(f, "[")?;
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Self::Dictionary(entries) => {
                write!(f, "{{")?;
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl Value {
    /// Creates a string value.
    pub fn string(s: impl Into<Arc<str>>) -> Self {
        Self::String(s.into())
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Self::String(Arc::from(s))
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Self::Int(v)
    }
}

impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Self::Float(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Double(v)
    }
}

/// Combines two dictionaries per §6.6.2.1 (dictionary combining).
///
/// Rules:
/// - Stronger non-dictionary values win per key.
/// - When both stronger and weaker have a dictionary value at the same key,
///   the values are combined recursively.
/// - Weaker-only keys are preserved (appended to maintain order).
///
/// Spec: AOUSD Core §6.6.2.1, §12.2.5.
pub fn combine_dictionaries(
    stronger: &[(Arc<str>, Value)],
    weaker: &[(Arc<str>, Value)],
) -> Vec<(Arc<str>, Value)> {
    let mut result: Vec<(Arc<str>, Value)> = stronger.to_vec();
    for (key, weak_val) in weaker {
        if let Some(pos) = result.iter().position(|(k, _)| k == key) {
            // Key exists in stronger. Recurse if both are dictionaries.
            let strong_val = &result[pos].1;
            if let (Value::Dictionary(strong_dict), Value::Dictionary(weak_dict)) =
                (strong_val, weak_val)
            {
                let combined = combine_dictionaries(strong_dict, weak_dict);
                result[pos].1 = Value::Dictionary(combined);
            }
            // Otherwise stronger non-dict value wins — no action needed.
        } else {
            // Key only in weaker: preserve it.
            result.push((key.clone(), weak_val.clone()));
        }
    }
    result
}

/// Combines a chain of dictionary opinions in strength order (strongest first).
///
/// Applies the combining algorithm pairwise from strongest to weakest.
///
/// Spec: AOUSD Core §6.6.2.1 (dictionary combining).
pub fn combine_dictionary_chain(
    opinions: impl IntoIterator<Item = Vec<(Arc<str>, Value)>>,
) -> Vec<(Arc<str>, Value)> {
    let mut result: Option<Vec<(Arc<str>, Value)>> = None;
    for dict in opinions {
        result = Some(match result {
            None => dict,
            Some(acc) => combine_dictionaries(&acc, &dict),
        });
    }
    result.unwrap_or_default()
}

/// A named field entry on a prim spec or variant spec.
///
/// This pairs a field name (interned token) with its value, and carries
/// per-field metadata such as whether the field was declared `custom`.
///
/// Spec: AOUSD Core §6 (scene description data model), §7 (opinions).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldEntry {
    /// The interned field name.
    pub name: TokenId,
    /// The field value.
    pub value: FieldValue,
}

/// A field value stored on a prim spec.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    /// A scalar (strongest wins).
    Value(Value),
    /// A list-op field over tokens (resolved by chaining).
    TokenListOp(ListOp<TokenId>),
    /// A list-op field over paths (resolved by chaining).
    ///
    /// This is used for relationship targets and other path-valued list fields.
    ///
    /// Spec: AOUSD Core §12.4 (`ListOps`), applied to path lists.
    PathListOp(ListOp<PathId>),
    /// Time-varying samples: sorted `(timeCode, value)` pairs.
    ///
    /// `TimeSamples` take priority over default values per §12.3.
    /// Interpolation between samples uses the layer's interpolation type
    /// (Held or Linear, §12.5).
    ///
    /// Spec: AOUSD Core §12.3.2.2 (timeSamples metadata).
    TimeSamples(Vec<(f64, Value)>),
}

impl From<Value> for FieldValue {
    fn from(v: Value) -> Self {
        Self::Value(v)
    }
}

impl From<&str> for FieldValue {
    fn from(s: &str) -> Self {
        Self::Value(Value::string(s))
    }
}

impl From<bool> for FieldValue {
    fn from(v: bool) -> Self {
        Self::Value(Value::Bool(v))
    }
}

impl From<i32> for FieldValue {
    fn from(v: i32) -> Self {
        Self::Value(Value::Int(v))
    }
}

impl From<i64> for FieldValue {
    fn from(v: i64) -> Self {
        Self::Value(Value::Int64(v))
    }
}

impl From<f32> for FieldValue {
    fn from(v: f32) -> Self {
        Self::Value(Value::Float(v))
    }
}

impl From<f64> for FieldValue {
    fn from(v: f64) -> Self {
        Self::Value(Value::Double(v))
    }
}

/// Interpolation method for time-varying attribute resolution.
///
/// Spec: AOUSD Core §12.5 (interpolation).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum InterpolationType {
    /// Step function — value holds until the next time sample.
    #[default]
    Held,
    /// Linearly interpolate between bracketing samples.
    /// Non-numeric types fall back to held.
    Linear,
}

/// A time offset/scale pair for retiming (§16.3.10.20, §12.3.2.1).
///
/// Remaps time via: `mappedTime = queryTime * scale + offset`.
/// The identity (no-op) is `{ offset: 0.0, scale: 1.0 }`.
///
/// `Eq` is implemented via bitwise comparison of the `f64` fields, which is
/// correct for list-op identity matching (two references with different
/// offsets are distinct list-op entries).
#[derive(Clone, Copy, Debug)]
pub struct LayerOffset {
    /// Time offset in frames.
    pub offset: f64,
    /// Time scale factor (must be positive and non-zero).
    pub scale: f64,
}

impl PartialEq for LayerOffset {
    fn eq(&self, other: &Self) -> bool {
        self.offset.to_bits() == other.offset.to_bits()
            && self.scale.to_bits() == other.scale.to_bits()
    }
}

impl Eq for LayerOffset {}

impl Default for LayerOffset {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl LayerOffset {
    /// The identity (no-op) layer offset.
    pub const IDENTITY: Self = Self {
        offset: 0.0,
        scale: 1.0,
    };

    /// Returns `true` if this is the identity (no-op) offset.
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.offset == 0.0 && self.scale == 1.0
    }

    /// Maps a stage time to a layer-local time.
    ///
    /// Spec: §12.3.2.1 — `mappedTime = queryTime * scale + offset`.
    #[must_use]
    pub fn map_time(self, time: f64) -> f64 {
        time * self.scale + self.offset
    }

    /// Composes two offsets: `self` is the outer, `inner` is the inner.
    ///
    /// The result maps time as if `inner` were applied first, then `self`.
    #[must_use]
    pub fn compose(self, inner: Self) -> Self {
        Self {
            offset: self.offset + self.scale * inner.offset,
            scale: self.scale * inner.scale,
        }
    }
}

/// A sublayer entry with an optional time offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SublayerEntry {
    /// The sublayer's layer ID.
    pub layer: LayerId,
    /// Time offset applied to this sublayer (§12.3.2.1).
    pub offset: LayerOffset,
}

impl SublayerEntry {
    /// Creates a sublayer entry with no time offset.
    pub fn new(layer: LayerId) -> Self {
        Self {
            layer,
            offset: LayerOffset::IDENTITY,
        }
    }
}

impl From<LayerId> for SublayerEntry {
    fn from(layer: LayerId) -> Self {
        Self::new(layer)
    }
}

/// A composition reference arc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    /// The referenced layer/document.
    pub layer: LayerId,
    /// The referenced prim path within that layer/document.
    pub prim_path: PathId,
    /// Optional debug name / URI.
    pub asset: Option<String>,
    /// Time offset applied across this reference boundary (§12.3.2.1).
    pub layer_offset: LayerOffset,
}

impl Reference {
    /// Creates a reference with no asset path.
    pub fn new(layer: LayerId, prim_path: PathId) -> Self {
        Self {
            layer,
            prim_path,
            asset: None,
            layer_offset: LayerOffset::IDENTITY,
        }
    }

    /// Creates a reference with an asset path.
    pub fn with_asset(layer: LayerId, prim_path: PathId, asset: impl Into<String>) -> Self {
        Self {
            layer,
            prim_path,
            asset: Some(asset.into()),
            layer_offset: LayerOffset::IDENTITY,
        }
    }
}

/// Opinions for a variant branch.
#[derive(Clone, Debug, Default)]
pub struct VariantSpec {
    /// Authored fields within this variant.
    pub fields: Vec<FieldEntry>,
    /// Child prim names introduced by this variant branch.
    ///
    /// These children are only populated when this variant is selected.
    pub authored_children: Vec<TokenId>,
    /// For children that exist within nested variant branches, this records the
    /// additional outer variant selections required for the child to be visible.
    /// Only populated for children from deeply nested variant contexts.
    ///
    /// E.g., a child from `standin=render > shadingVariant=spooky` registered
    /// under `shadingVariant=spooky` would have `{child_tok: [(standin_tok, render_tok)]}`.
    pub required_outer_selections: HashMap<TokenId, Vec<(TokenId, TokenId)>>,
    /// Composition arcs for child prims within this variant branch.
    ///
    /// When a child prim (e.g. `over "Child" (add references = ...)`) appears
    /// inside a variant branch, its composition arcs are stored here keyed by
    /// the child prim name. These arcs apply only when this variant is selected.
    ///
    /// Spec: AOUSD Core §10.5 (variant arcs on child prims).
    pub child_references: HashMap<TokenId, ListOp<Reference>>,
    /// Inherits arcs for child prims within this variant branch.
    pub child_inherits: HashMap<TokenId, ListOp<PathId>>,
    /// Payloads for child prims within this variant branch.
    pub child_payloads: HashMap<TokenId, ListOp<Reference>>,
    /// Specializes arcs for child prims within this variant branch.
    pub child_specializes: HashMap<TokenId, ListOp<PathId>>,
    /// Authored children for child prims within this variant branch.
    ///
    /// When a child prim (e.g. `over "Child" { def "Grandchild" {} }`) inside
    /// a variant branch introduces grandchild prims, they are recorded here
    /// keyed by the child prim name. These grandchildren are only visible when
    /// this variant is selected.
    pub child_authored_children: HashMap<TokenId, Vec<TokenId>>,
    /// Field opinions for child prims within this variant branch.
    ///
    /// When a child prim (e.g. `class "Child" { bool attr = 0 }`) inside a
    /// variant branch defines field values, they are recorded here keyed by the
    /// child prim name. These opinions apply only when this variant is selected.
    pub child_fields: HashMap<TokenId, Vec<FieldEntry>>,
    /// References arcs on this variant branch itself.
    ///
    /// When a variant branch header includes composition arcs
    /// (e.g. `"full" (add references = @...@) { ... }`), those arcs apply
    /// to the prim owning the variant set when this variant is selected.
    ///
    /// Spec: AOUSD Core §10.5 (variant arcs).
    pub references: ListOp<Reference>,
    /// Inherits arcs on this variant branch itself.
    pub inherits: ListOp<PathId>,
    /// Specializes arcs on this variant branch itself.
    pub specializes: ListOp<PathId>,
    /// Payloads on this variant branch itself.
    pub payloads: ListOp<Reference>,
    /// Variant selections authored within this variant branch.
    ///
    /// When a variant branch header includes `variants = { string v2 = "b" }`,
    /// those selections apply to the owning prim when this variant is selected.
    pub variant_selections: HashMap<TokenId, TokenId>,
}

impl VariantSpec {
    /// Merges another [`VariantSpec`] into this one, combining authored
    /// children, required outer selections, and other fields.
    ///
    /// Used when the same variant branch name appears at multiple nesting
    /// levels (e.g., an outer `standin=anim` and a deeply nested
    /// `standin=anim` within `shadingVariant=spooky`).
    pub fn merge(&mut self, other: Self) {
        for child in other.authored_children {
            if !self.authored_children.contains(&child) {
                self.authored_children.push(child);
            }
        }
        for (child, reqs) in other.required_outer_selections {
            self.required_outer_selections.entry(child).or_insert(reqs);
        }
        for entry in other.fields {
            if !self.fields.iter().any(|e| e.name == entry.name) {
                self.fields.push(entry);
            }
        }
        for (k, v) in other.child_references {
            self.child_references.entry(k).or_insert(v);
        }
        for (k, v) in other.child_inherits {
            self.child_inherits.entry(k).or_insert(v);
        }
        for (k, v) in other.child_payloads {
            self.child_payloads.entry(k).or_insert(v);
        }
        for (k, v) in other.child_specializes {
            self.child_specializes.entry(k).or_insert(v);
        }
        for (k, v) in other.child_authored_children {
            let existing = self.child_authored_children.entry(k).or_default();
            for child in v {
                if !existing.contains(&child) {
                    existing.push(child);
                }
            }
        }
        for (k, v) in other.child_fields {
            let existing = self.child_fields.entry(k).or_default();
            for entry in v {
                if !existing.iter().any(|e| e.name == entry.name) {
                    existing.push(entry);
                }
            }
        }
        for (k, v) in other.variant_selections {
            self.variant_selections.entry(k).or_insert(v);
        }
    }
}

/// A variant set: a named collection of variants.
#[derive(Clone, Debug, Default)]
pub struct VariantSetSpec {
    /// Variants keyed by variant name.
    pub variants: HashMap<TokenId, VariantSpec>,
}

/// Opinions for a prim at a path.
#[derive(Clone, Debug, Default)]
pub struct PrimSpec {
    /// The prim specifier (`def`, `over`, or `class`).
    ///
    /// Spec: AOUSD Core §7.6 (specifier field), §12.2.1 (specifier resolution).
    pub specifier: Option<Specifier>,
    /// The prim type name (e.g. `Xform`, `Mesh`, `Scope`).
    ///
    /// Type name resolution uses strongest-defining-opinion-wins: the first
    /// opinion (in strength order) with a non-`None` type name determines the
    /// composed type.
    ///
    /// Spec: AOUSD Core §7.6 (typeName field), §12.2.3 (type name resolution).
    pub type_name: Option<TokenId>,
    /// Authored fields.
    pub fields: Vec<FieldEntry>,
    /// Authored child prim names in this layer, in file order.
    ///
    /// This is used as a deterministic baseline for child ordering. Child
    /// ordering is then further refined by applying `prim_order` (`reorder
    /// nameChildren`) opinions across the prim stack.
    ///
    /// Spec: AOUSD Core §11 (stage population) and supplemental suite child
    /// ordering tests (e.g. `BasicListEditingWithInherits_root`).
    pub authored_children: Vec<TokenId>,
    /// Authored variant selections (set -> chosen variant).
    pub variant_selections: HashMap<TokenId, TokenId>,
    /// Authored variant sets available at this prim.
    pub variant_sets: HashMap<TokenId, VariantSetSpec>,
    /// Ordered list of variant set names (from `variantSets` metadata).
    ///
    /// This determines the evaluation order for variant children.
    /// Children from later variant sets appear before earlier ones.
    pub variant_set_order: Vec<TokenId>,
    /// References arcs (a `ListOp` chain across the layer stack).
    pub references: ListOp<Reference>,
    /// Inherits arcs (a `ListOp` chain across the layer stack).
    ///
    /// Spec: AOUSD Core §10 (inherits arc), with ordering via §12.4 (`ListOps`).
    pub inherits: ListOp<PathId>,
    /// Specializes arcs (a `ListOp` chain across the layer stack).
    ///
    /// Specializes is similar to inherits but sits at the weakest position in
    /// LIVERPS. Unlike references, specializes propagates through all levels of
    /// referencing.
    ///
    /// Spec: AOUSD Core §10 (specializes arc, §5.1.33), with ordering via §12.4 (`ListOps`).
    pub specializes: ListOp<PathId>,
    /// Payloads arcs (a `ListOp` chain across the layer stack).
    ///
    /// Payloads are structurally identical to references but support deferred
    /// loading. When loaded, they behave like references with the same namespace
    /// mapping. Their position in LIVERPS is between References and Specializes.
    ///
    /// Spec: AOUSD Core §10 (payloads arc, §5.1.22).
    pub payloads: ListOp<Reference>,
    /// Optional child ordering (aka `primOrder` in the supplemental suite).
    ///
    /// This is used during stage population to produce deterministic, authored
    /// child ordering (rather than purely lexicographic ordering).
    ///
    /// Spec: AOUSD Core §11 (stage population), plus the supplemental
    /// parser’s `primOrder` field (`reorder nameChildren = [...]`).
    pub prim_order: Option<Vec<TokenId>>,
    /// Whether this prim is instanceable.
    ///
    /// When `true` and the prim has composition arcs (references, payloads),
    /// descendant local opinions are stripped — only opinions from composition
    /// arc targets survive. This enables prototype sharing for identical
    /// composition structures.
    ///
    /// Spec: AOUSD Core §11 (instancing), §5.1.14 (instanceable).
    pub instanceable: Option<bool>,
    /// Whether this prim is active.
    ///
    /// When `false`, the prim and all its namespace descendants are excluded
    /// from the composed stage. The strongest opinion wins.
    ///
    /// Spec: AOUSD Core §7.6 (active metadata), §11 (stage population).
    pub active: Option<bool>,
}

impl PrimSpec {
    /// Creates a prim spec with `specifier = def`.
    pub fn def() -> Self {
        Self {
            specifier: Some(Specifier::Def),
            ..Self::default()
        }
    }

    /// Creates a prim spec with `specifier = over`.
    pub fn over() -> Self {
        Self {
            specifier: Some(Specifier::Over),
            ..Self::default()
        }
    }

    /// Creates a prim spec with `specifier = class`.
    pub fn class() -> Self {
        Self {
            specifier: Some(Specifier::Class),
            ..Self::default()
        }
    }

    /// Sets the prim type name (builder, consuming).
    ///
    /// Spec: AOUSD Core §7.6 (typeName field).
    pub fn with_type_name(mut self, type_name: TokenId) -> Self {
        self.type_name = Some(type_name);
        self
    }

    /// Inserts or replaces a field value, returning `&mut Self` for chaining.
    pub fn set_field(&mut self, token: TokenId, value: impl Into<FieldValue>) -> &mut Self {
        set_field_vec(&mut self.fields, token, value.into());
        self
    }

    /// Inserts or replaces a field value (builder, consuming).
    pub fn with_field(mut self, token: TokenId, value: impl Into<FieldValue>) -> Self {
        set_field_vec(&mut self.fields, token, value.into());
        self
    }

    /// Appends a reference arc.
    pub fn add_reference(&mut self, reference: Reference) -> &mut Self {
        self.references.append.push(reference);
        self
    }

    /// Appends a reference arc (builder, consuming).
    pub fn with_reference(mut self, reference: Reference) -> Self {
        self.references.append.push(reference);
        self
    }

    /// Appends an inherit arc.
    pub fn add_inherit(&mut self, path: PathId) -> &mut Self {
        self.inherits.append.push(path);
        self
    }

    /// Appends an inherit arc (builder, consuming).
    pub fn with_inherit(mut self, path: PathId) -> Self {
        self.inherits.append.push(path);
        self
    }

    /// Appends a payload arc.
    pub fn add_payload(&mut self, payload: Reference) -> &mut Self {
        self.payloads.append.push(payload);
        self
    }

    /// Appends a payload arc (builder, consuming).
    pub fn with_payload(mut self, payload: Reference) -> Self {
        self.payloads.append.push(payload);
        self
    }

    /// Appends a specialize arc.
    pub fn add_specialize(&mut self, path: PathId) -> &mut Self {
        self.specializes.append.push(path);
        self
    }

    /// Appends a specialize arc (builder, consuming).
    pub fn with_specialize(mut self, path: PathId) -> Self {
        self.specializes.append.push(path);
        self
    }

    /// Sets the authored children list (builder, consuming).
    pub fn with_children(mut self, children: Vec<TokenId>) -> Self {
        self.authored_children = children;
        self
    }

    /// Marks this prim as instanceable (or not).
    pub fn with_instanceable(mut self, instanceable: bool) -> Self {
        self.instanceable = Some(instanceable);
        self
    }

    /// Marks this prim as active (or not).
    ///
    /// When `false`, the prim and all its namespace descendants are excluded
    /// from the composed stage.
    ///
    /// Spec: AOUSD Core §7.6 (active metadata).
    pub fn with_active(mut self, active: bool) -> Self {
        self.active = Some(active);
        self
    }
}

/// Inserts or replaces a field in a `Vec<FieldEntry>` by name.
///
/// If a field with the given name already exists, its value is replaced.
/// Otherwise a new entry is appended.
pub fn set_field_vec(fields: &mut Vec<FieldEntry>, name: TokenId, value: FieldValue) {
    if let Some(entry) = fields.iter_mut().find(|e| e.name == name) {
        entry.value = value;
    } else {
        fields.push(FieldEntry { name, value });
    }
}

/// Returns a shared reference to the value of a field, if present.
pub fn get_field<'a>(fields: &'a [FieldEntry], name: &TokenId) -> Option<&'a FieldValue> {
    fields.iter().find(|e| &e.name == name).map(|e| &e.value)
}

/// Returns a mutable reference to the value of a field, if present.
pub fn get_field_mut<'a>(
    fields: &'a mut [FieldEntry],
    name: &TokenId,
) -> Option<&'a mut FieldValue> {
    fields
        .iter_mut()
        .find(|e| &e.name == name)
        .map(|e| &mut e.value)
}

/// Inserts a field only if no entry with the same name exists.
pub fn insert_field_if_absent(fields: &mut Vec<FieldEntry>, name: TokenId, value: FieldValue) {
    if !fields.iter().any(|e| e.name == name) {
        fields.push(FieldEntry { name, value });
    }
}

/// A document layer.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Stable identifier for this layer.
    pub id: LayerId,
    /// Ordered sublayer includes. The layer itself is always stronger than its sublayers.
    pub sublayers: Vec<SublayerEntry>,
    /// Prim specs keyed by prim path.
    pub prims: HashMap<PathId, PrimSpec>,
}

impl Layer {
    /// Creates an empty layer with no sublayers or prims.
    pub fn new(id: LayerId) -> Self {
        Self {
            id,
            sublayers: Vec::new(),
            prims: HashMap::new(),
        }
    }

    /// Inserts a prim spec at the given path.
    pub fn insert_prim(&mut self, path: PathId, spec: PrimSpec) {
        self.prims.insert(path, spec);
    }
}

/// A store for accessing layers and shared interners.
pub trait LayerStore {
    /// Returns a layer, if present.
    fn layer(&self, id: LayerId) -> Option<&Layer>;

    /// Returns the shared token interner.
    fn tokens(&self) -> &TokenInterner;

    /// Returns the shared token interner mutably, allowing interning of additional tokens.
    fn tokens_mut(&mut self) -> &mut TokenInterner;

    /// Returns the shared path interner.
    fn paths(&self) -> &PathInterner;

    /// Returns the shared path interner mutably, allowing interning of derived paths.
    fn paths_mut(&mut self) -> &mut PathInterner;
}

/// A simple in-memory [`LayerStore`] implementation.
#[derive(Debug, Default)]
pub struct InMemoryStore {
    /// Shared token interner for all layers in the store.
    pub tokens: TokenInterner,
    /// Shared path interner for all layers in the store.
    pub paths: PathInterner,
    /// Layers keyed by [`LayerId`].
    pub layers: HashMap<LayerId, Layer>,
}

impl InMemoryStore {
    /// Inserts (or replaces) a layer.
    pub fn insert_layer(&mut self, layer: Layer) {
        self.layers.insert(layer.id, layer);
    }

    /// Parses and interns an absolute path, returning its [`PathId`].
    ///
    /// # Panics
    ///
    /// Panics if `s` is not a valid absolute path (must start with `/`).
    pub fn path(&mut self, s: &str) -> PathId {
        let p =
            crate::path::Path::parse_absolute(s, &mut self.tokens).expect("valid absolute path");
        self.paths.intern(p)
    }
}

impl LayerStore for InMemoryStore {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers.get(&id)
    }

    fn tokens(&self) -> &TokenInterner {
        &self.tokens
    }

    fn tokens_mut(&mut self) -> &mut TokenInterner {
        &mut self.tokens
    }

    fn paths(&self) -> &PathInterner {
        &self.paths
    }

    fn paths_mut(&mut self) -> &mut PathInterner {
        &mut self.paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec;

    fn entry(key: &str, val: Value) -> (Arc<str>, Value) {
        (Arc::from(key), val)
    }

    #[test]
    fn combine_stronger_scalar_wins() {
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker = vec![entry("a", Value::Int(2))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_weaker_only_keys_preserved() {
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker = vec![entry("b", Value::Int(2))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(
            result,
            vec![entry("a", Value::Int(1)), entry("b", Value::Int(2))]
        );
    }

    #[test]
    fn combine_nested_dictionaries_recurse() {
        let stronger = vec![entry(
            "sub",
            Value::Dictionary(vec![entry("x", Value::Int(10))]),
        )];
        let weaker = vec![entry(
            "sub",
            Value::Dictionary(vec![entry("x", Value::Int(20)), entry("y", Value::Int(30))]),
        )];
        let result = combine_dictionaries(&stronger, &weaker);
        // x=10 from stronger wins; y=30 from weaker preserved.
        assert_eq!(
            result,
            vec![entry(
                "sub",
                Value::Dictionary(vec![entry("x", Value::Int(10)), entry("y", Value::Int(30))])
            )]
        );
    }

    #[test]
    fn combine_stronger_dict_weaker_scalar_wins_stronger() {
        // When stronger has a dictionary and weaker has a scalar at the same key,
        // stronger wins (it's not a dict-dict merge).
        let stronger = vec![entry(
            "a",
            Value::Dictionary(vec![entry("x", Value::Int(1))]),
        )];
        let weaker = vec![entry("a", Value::Int(99))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(
            result,
            vec![entry(
                "a",
                Value::Dictionary(vec![entry("x", Value::Int(1))])
            )]
        );
    }

    #[test]
    fn combine_stronger_scalar_weaker_dict_wins_stronger() {
        // When stronger is scalar and weaker is dictionary, stronger wins.
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker = vec![entry(
            "a",
            Value::Dictionary(vec![entry("x", Value::Int(99))]),
        )];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_empty_stronger() {
        let stronger: Vec<(Arc<str>, Value)> = vec![];
        let weaker = vec![entry("a", Value::Int(1))];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_empty_weaker() {
        let stronger = vec![entry("a", Value::Int(1))];
        let weaker: Vec<(Arc<str>, Value)> = vec![];
        let result = combine_dictionaries(&stronger, &weaker);
        assert_eq!(result, vec![entry("a", Value::Int(1))]);
    }

    #[test]
    fn combine_chain_three_opinions() {
        let strongest = vec![entry("a", Value::Int(1))];
        let middle = vec![entry("b", Value::Int(2))];
        let weakest = vec![entry("c", Value::Int(3)), entry("a", Value::Int(99))];
        let result = combine_dictionary_chain([strongest, middle, weakest]);
        assert_eq!(
            result,
            vec![
                entry("a", Value::Int(1)),
                entry("b", Value::Int(2)),
                entry("c", Value::Int(3)),
            ]
        );
    }

    #[test]
    fn combine_chain_empty_yields_default() {
        let result = combine_dictionary_chain(Vec::<Vec<(Arc<str>, Value)>>::new());
        assert!(result.is_empty());
    }

    #[test]
    fn combine_chain_nested_across_three_layers() {
        // Three layers all contribute to a nested dictionary.
        let strongest = vec![entry(
            "d",
            Value::Dictionary(vec![entry("x", Value::Int(1))]),
        )];
        let middle = vec![entry(
            "d",
            Value::Dictionary(vec![entry("y", Value::Int(2))]),
        )];
        let weakest = vec![entry(
            "d",
            Value::Dictionary(vec![entry("x", Value::Int(99)), entry("z", Value::Int(3))]),
        )];
        let result = combine_dictionary_chain([strongest, middle, weakest]);
        assert_eq!(
            result,
            vec![entry(
                "d",
                Value::Dictionary(vec![
                    entry("x", Value::Int(1)),
                    entry("y", Value::Int(2)),
                    entry("z", Value::Int(3)),
                ])
            )]
        );
    }

    #[test]
    fn layer_offset_identity() {
        let id = LayerOffset::IDENTITY;
        assert!(id.is_identity());
        assert_eq!(id.map_time(42.0), 42.0);
    }

    #[test]
    fn layer_offset_map_time() {
        let lo = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        // mappedTime = time * scale + offset = 5 * 2 + 10 = 20
        assert_eq!(lo.map_time(5.0), 20.0);
    }

    #[test]
    fn layer_offset_compose() {
        // outer (offset=10, scale=2) composed with inner (offset=5, scale=3)
        // composed_offset = 10 + 2*5 = 20
        // composed_scale  = 2 * 3 = 6
        let outer = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        let inner = LayerOffset {
            offset: 5.0,
            scale: 3.0,
        };
        let composed = outer.compose(inner);
        assert_eq!(
            composed,
            LayerOffset {
                offset: 20.0,
                scale: 6.0
            }
        );
    }

    #[test]
    fn layer_offset_compose_identity_is_noop() {
        let lo = LayerOffset {
            offset: 10.0,
            scale: 2.0,
        };
        assert_eq!(lo.compose(LayerOffset::IDENTITY), lo);
        assert_eq!(LayerOffset::IDENTITY.compose(lo), lo);
    }
}
