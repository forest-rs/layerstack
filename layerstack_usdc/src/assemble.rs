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

use alloc::borrow::Cow;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use layerstack::parallel::Threads;

use hashbrown::{HashMap, HashSet};

use layerstack::doc::{
    FieldValue, Layer, LayerId, LayerOffset, PrimSpec, RarePrimFields, Reference, Specifier,
    SublayerEntry, Value, VariantSets, VariantSpec, set_field_vec,
};
use layerstack::interner::{TokenId, TokenInterner};
use layerstack::listop::ListOp;
use layerstack::path::{Path, PathId, PathInterner, PropertyPath, TargetPath};
use layerstack::property::{
    PropertyEntry, PropertyKind, PropertySpec, Variability, property_entry, set_property_vec,
};
use layerstack::spec_path::VariantSelectionSite;
use layerstack::variable_expression::is_expression;
use layerstack::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};
use layerstack::{
    AssetResolveError, AssetResolver, PropertyType, ReferenceSource, ReferenceTarget, ResolvedAsset,
};

use crate::error::UsdcError;
use crate::numeric_array::*;
use crate::section::{CrateSections, OnceSlot, PathNode};
use crate::value_rep::{
    CrateArrayEdit, CrateArrayEditOp, CrateListOp, CrateValue, DecodeBudget, DecodedField,
    FloatArray, Items, ListOpParts, RawValueRep, TextKind, TokenIds, decode_field_within,
    decode_path_list_op_indices, decode_text_index, decode_text_indices,
    decode_token_vector_indices, scalar_text_kind, string_array,
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
/// Prim and property specs are converted on the calling thread and the
/// threads `threads` spares (with the `std` feature): one thread converts
/// them in file order, more in chunks of a fixed number of specs, when
/// the budget spares a thread as assembly starts and the file holds more
/// than a chunk. The layer, its diagnostics and the budget
/// charged, or the error, are the same on any number of threads; only the
/// order in which new tokens and paths are interned, and so their ids,
/// depends on them. Arcs are resolved on the calling thread, in file
/// order.
///
/// Spec: AOUSD Core §16.3.
#[expect(
    clippy::too_many_arguments,
    reason = "the inputs a read assembles from, each explicit"
)]
pub fn assemble(
    data: &[u8],
    sections: &CrateSections,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    budget: &mut DecodeBudget,
    threads: &Threads,
) -> Result<AssembleResult, UsdcError> {
    let chunk = SPECS_PER_CHUNK;
    assemble_in_chunks(
        data, sections, layer_id, tokens, paths, resolver, budget, threads, chunk,
    )
}

/// [`assemble`], converting chunks of `chunk` specs at a time.
#[expect(
    clippy::too_many_arguments,
    reason = "the inputs a read assembles from, each explicit"
)]
fn assemble_in_chunks(
    data: &[u8],
    sections: &CrateSections,
    layer_id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    budget: &mut DecodeBudget,
    threads: &Threads,
    chunk: usize,
) -> Result<AssembleResult, UsdcError> {
    let tables = ReadTables::for_read(sections);
    let inputs = ReadInputs {
        data,
        sections,
        tokens,
        paths,
        tables: &tables,
        layer_id,
        defer_variant_arcs: resolver.defers_variant_arcs(),
        chunk: chunk.max(1),
    };
    let mut ctx = inputs.ctx(budget, Some(resolver));

    let layer = ctx.assemble_layer(inputs, threads)?;

    Ok(AssembleResult {
        layer,
        resolved_layers: ctx.resolved_layers,
        diagnostics: ctx.diagnostics,
    })
}

// ---------------------------------------------------------------------------
// Internal context
// ---------------------------------------------------------------------------

struct AssembleCtx<'a, 'b> {
    data: &'a [u8],
    sections: &'a CrateSections,
    tokens: &'a TokenInterner,
    /// What the read finds once about the file's tables, for every pass.
    tables: &'a ReadTables,
    paths: &'a PathInterner,
    /// Relationship targets and connections parsed so far, by their text:
    /// many specs name the same targets (a material, a collection).
    target_paths: HashMap<String, Option<TargetPath>>,
    /// The entries [`Self::prim_path_at`] is finding, kept between calls.
    pending_paths: Vec<usize>,
    /// The asset resolver, for the passes that convert arcs; `None` for
    /// specs converted on other threads, which leave a spec asking for it
    /// to be converted with it ([`Self::wants_resolver`]).
    resolver: Option<&'b mut dyn AssetResolver>,
    /// Whether a conversion without a resolver asked for one.
    wants_resolver: bool,
    /// Whether arcs inside variant branches are left for composition to
    /// resolve ([`AssetResolver::defers_variant_arcs`]).
    defer_variant_arcs: bool,
    /// Whether the arcs being converted are authored inside a variant
    /// branch.
    in_branch: bool,
    /// Whether the arcs being converted are payloads.
    in_payloads: bool,
    /// Whether an arc was left for the resolver to resolve later
    /// ([`Reference::deferred`]).
    deferred_arcs: bool,
    layer_id: LayerId,
    resolved_layers: Vec<Layer>,
    diagnostics: Vec<AssembleDiagnostic>,
    /// What the read may still materialize.
    budget: &'b mut DecodeBudget,
    /// Whether each prim path text properties are authored on parses, as
    /// `PropertyPath::parse` requires; checked once per prim
    /// ([`Self::split_property_path`]).
    valid_prim_texts: HashMap<&'a str, bool>,
    /// The declared type of the attributes declaring each `typeName`, by
    /// its token index (`u32::MAX` for none) and whether an array value
    /// makes it an array ([`Self::attribute_property_type`]).
    property_types: HashMap<(u32, bool), PropertyType>,
}

/// A spec built from a fieldset other specs use too, for each of them to
/// take a copy of, and the budget its fields and their conversion took,
/// which each copy is charged again: a spec costs what it would if it were
/// built anew. Only a spec built without a diagnostic is shared, as a
/// diagnostic names its spec, and only one whose conversion asks nothing of
/// the asset resolver.
struct Shared<T> {
    spec: T,
    charged: u64,
}

/// A prim spec and its authored `properties` order.
type SharedPrim = (Box<PrimSpec>, Option<Vec<TokenId>>);

/// A prim spec's authored `properties` order: its own, or that of the spec
/// built for the fieldset it shares, read where that spec is kept.
enum Order<'a> {
    Own(Vec<TokenId>),
    Shared(&'a [TokenId]),
}

impl core::ops::Deref for Order<'_> {
    type Target = [TokenId];

    fn deref(&self) -> &[TokenId] {
        match self {
            Self::Own(order) => order,
            Self::Shared(order) => order,
        }
    }
}

/// Where a property spec belongs, by its path ([`AssembleCtx::property_owner`]).
enum PropertyOwner {
    /// On a variant spec: placed with the variant specs.
    Variant,
    /// A path whose prim path text does not parse.
    Invalid,
    /// On the prim spec at path table entry `owner`, named `name`.
    Prim { owner: u32, name: TokenId },
}

/// The path of the spec a diagnostic names: its text, or its entry of the
/// path table, whose text is spelled only when a diagnostic names it.
#[derive(Clone, Copy)]
enum SpecName<'a> {
    Text(&'a str),
    Entry(u32),
}

impl<'a> From<&'a str> for SpecName<'a> {
    fn from(text: &'a str) -> Self {
        Self::Text(text)
    }
}

/// A prim spec's key: the path its path table entry names outside variant
/// branches, when its text parses as one, otherwise its text. Two entries
/// spelling one text have one key, and two keys are equal only when the
/// texts are, as a text parses as only one path and a path is spelled by
/// only one text ([`AssembleCtx::prim_key_at`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum PrimKey<'a> {
    Path(PathId),
    Text(&'a str),
}

/// What one entry of the path table is as the prim path text of a property
/// path, once found.
#[derive(Clone, Copy)]
enum Owner {
    /// A variant spec's path (`/A{v=x}`).
    Variant,
    /// Text [`PropertyPath::split`] and [`Path::parse_absolute`] reject as
    /// a property's prim path.
    Invalid,
    /// A prim spec's path, perhaps inside a variant branch.
    Prim,
}

/// What every pass of a read converts specs from: the file, its tables,
/// the interners, and what the read finds once about the tables.
#[derive(Clone, Copy)]
struct ReadInputs<'a> {
    data: &'a [u8],
    sections: &'a CrateSections,
    tokens: &'a TokenInterner,
    paths: &'a PathInterner,
    tables: &'a ReadTables,
    layer_id: LayerId,
    defer_variant_arcs: bool,
    /// How many specs of a pass a chunk holds ([`SPECS_PER_CHUNK`]).
    chunk: usize,
}

impl<'a> ReadInputs<'a> {
    /// A context converting specs within `budget`, resolving arcs with
    /// `resolver`, or asking for it ([`AssembleCtx::wants_resolver`]).
    fn ctx<'b>(
        self,
        budget: &'b mut DecodeBudget,
        resolver: Option<&'b mut dyn AssetResolver>,
    ) -> AssembleCtx<'a, 'b> {
        AssembleCtx {
            data: self.data,
            sections: self.sections,
            tokens: self.tokens,
            tables: self.tables,
            paths: self.paths,
            target_paths: HashMap::new(),
            pending_paths: Vec::new(),
            defer_variant_arcs: self.defer_variant_arcs,
            in_branch: false,
            in_payloads: false,
            resolver,
            wants_resolver: false,
            layer_id: self.layer_id,
            resolved_layers: Vec::new(),
            diagnostics: Vec::new(),
            budget,
            valid_prim_texts: HashMap::new(),
            deferred_arcs: false,
            property_types: HashMap::new(),
        }
    }
}

/// Which specs a read has decoded the fields of, of the specs from
/// `first` on: the specs a pass leaves are validated at the end.
struct Visits<'v> {
    first: usize,
    seen: &'v mut [bool],
}

impl Visits<'_> {
    /// Marks spec `spec` decoded.
    fn mark(&mut self, spec: usize) {
        let seen = &mut self.seen[spec - self.first];
        debug_assert!(!*seen, "spec decoded twice");
        *seen = true;
    }

    /// Marks spec `spec` not decoded, for another pass to decode.
    fn unmark(&mut self, spec: usize) {
        self.seen[spec - self.first] = false;
    }

    fn is_marked(&self, spec: usize) -> bool {
        self.seen[spec - self.first]
    }
}

/// How many specs a pass converts at a time on one thread: a fixed number
/// of the specs the pass converts, so that a pass divides a file the same
/// way on any number of threads, and evenly however the file orders its
/// specs (a writer may put every prim spec first).
const SPECS_PER_CHUNK: usize = 512;

/// What converting one chunk of specs found ([`AssembleCtx::run_pass`]).
struct Chunk<T> {
    /// What its specs converted to, in spec order.
    items: Vec<T>,
    /// The diagnostics, each with the index of the spec reporting it, in
    /// spec order.
    diagnostics: Vec<(usize, AssembleDiagnostic)>,
    /// The specs that asked for the resolver, to convert with it.
    deferred: Vec<usize>,
    /// The budget units the chunk charged.
    used: u64,
    /// Whether an arc was left for the resolver to resolve later.
    deferred_arcs: bool,
    /// The error the chunk stopped at.
    error: Option<UsdcError>,
}

/// Converts one spec in a pass: what it converts to, or `None`.
#[cfg(feature = "std")]
type Convert<'c, 'a, T> = dyn for<'b, 'v> Fn(
        &mut AssembleCtx<'a, 'b>,
        usize,
        &mut Visits<'v>,
        &mut Fields<'a>,
    ) -> Result<Option<T>, UsdcError>
    + Sync
    + 'c;
/// Converts one spec in a pass: what it converts to, or `None`.
#[cfg(not(feature = "std"))]
type Convert<'c, 'a, T> = dyn for<'b, 'v> Fn(
        &mut AssembleCtx<'a, 'b>,
        usize,
        &mut Visits<'v>,
        &mut Fields<'a>,
    ) -> Result<Option<T>, UsdcError>
    + 'c;

/// What a pass converted one spec to, by its index.
trait SpecItem {
    fn spec(&self) -> usize;
}

/// A prim spec the prim pass converted, to place in the layer.
struct PrimItem<'a> {
    spec: usize,
    index: u32,
    key: PrimKey<'a>,
    prim: Box<PrimSpec>,
    children: Option<Order<'a>>,
}

impl SpecItem for PrimItem<'_> {
    fn spec(&self) -> usize {
        self.spec
    }
}

/// A property spec the property pass converted, for the prim spec at
/// `place`.
struct PropertyItem {
    spec: usize,
    place: usize,
    name: TokenId,
    property: Arc<PropertySpec>,
}

impl SpecItem for PropertyItem {
    fn spec(&self) -> usize {
        self.spec
    }
}

/// What a read finds once about each entry of the file's token and path
/// tables, when a spec first asks, for every pass, and with the `std`
/// feature for every thread a pass converts specs on: a value found twice
/// is the same, so a thread finding one another has set keeps either.
struct ReadTables {
    /// The token id of each entry of the token table.
    token_ids: Vec<OnceSlot<TokenId>>,
    /// The prim path each entry of the path table names outside variant
    /// branches ([`AssembleCtx::prim_path_at`]); `None` for a text holding
    /// a variant selection or not parsing as one.
    prim_paths: Vec<OnceSlot<Option<PathId>>>,
    /// Whether each entry of the token table, as a property path's
    /// element, names a property [`PropertyPath::split`] reads as one name.
    property_names: Vec<OnceSlot<bool>>,
    /// How many specs use each fieldset, by its index in the fieldset
    /// table, counting up to two: specs sharing a fieldset are built once.
    fieldset_uses: Vec<u8>,
    /// The place in [`Self::shared`] of each fieldset more than one spec
    /// uses, by its index in the fieldset table.
    shared_fieldsets: HashMap<u32, usize>,
    /// The specs built for each fieldset specs share.
    shared: Vec<SharedSpecs>,
}

/// The specs built for a fieldset specs share ([`Shared`]), when first
/// built: one for its prim specs outside variant branches, and one for its
/// attribute specs and one for its relationship specs.
#[derive(Default)]
struct SharedSpecs {
    prim: OnceSlot<Arc<Shared<SharedPrim>>>,
    attribute: OnceSlot<Shared<Arc<PropertySpec>>>,
    relationship: OnceSlot<Shared<Arc<PropertySpec>>>,
}

impl ReadTables {
    fn for_read(sections: &CrateSections) -> Self {
        fn slots<T>(n: usize) -> Vec<OnceSlot<T>> {
            (0..n).map(|_| OnceSlot::new()).collect()
        }
        let mut fieldset_uses = alloc::vec![0_u8; sections.fieldsets.len()];
        let mut shared_fieldsets = HashMap::new();
        for spec in &sections.specs {
            let at = spec.fieldset_index;
            if let Some(uses) = fieldset_uses.get_mut(at as usize) {
                *uses = uses.saturating_add(1).min(2);
                if *uses == 2 && !shared_fieldsets.contains_key(&at) {
                    let place = shared_fieldsets.len();
                    shared_fieldsets.insert(at, place);
                }
            }
        }
        let shared = (0..shared_fieldsets.len()).map(|_| SharedSpecs::default());
        let tree = sections.paths.has_tree();
        Self {
            token_ids: slots(sections.tokens.len()),
            prim_paths: slots(if tree { sections.paths.len() } else { 0 }),
            property_names: slots(if tree { sections.tokens.len() } else { 0 }),
            shared: shared.collect(),
            fieldset_uses,
            shared_fieldsets,
        }
    }
}

/// The value of `slot`: the one set, or `found()` set there.
fn found_once<T: Copy>(slot: &OnceSlot<T>, found: impl FnOnce() -> T) -> T {
    if let Some(&value) = slot.get() {
        return value;
    }
    let value = found();
    let _ = slot.set(value);
    value
}

/// The prim specs a read builds, by their key ([`PrimKey`]), and by the
/// path table index of each spec's path, in the order their paths were
/// first spelled. Each spec is boxed, so it stays where it was built, in
/// the layer too ([`Layer::insert_prims`]).
#[expect(
    clippy::vec_box,
    reason = "a boxed spec moves into the layer as a pointer, without a copy"
)]
struct PrimSpecs<'a> {
    by_key: HashMap<PrimKey<'a>, usize>,
    specs: Vec<Box<PrimSpec>>,
    /// The path table index each of `specs` was first placed with.
    placed: Vec<u32>,
    /// The place in `specs` of the spec at each path table index; `NONE`
    /// where no prim spec's path is.
    at_index: Vec<u32>,
}

impl<'a> PrimSpecs<'a> {
    const NONE: u32 = u32::MAX;

    fn new(prims: usize, paths: usize) -> Self {
        Self {
            by_key: HashMap::with_capacity(prims),
            specs: Vec::with_capacity(prims),
            placed: Vec::with_capacity(prims),
            at_index: alloc::vec![Self::NONE; paths],
        }
    }

    /// Sets the spec keyed `key`, the key of path table entry `index`,
    /// replacing any there, and returns its place: the number of paths
    /// spelled before it.
    fn insert(&mut self, index: u32, key: PrimKey<'a>, spec: Box<PrimSpec>) -> usize {
        let at = match self.by_key.get(&key) {
            Some(&at) => {
                self.specs[at] = spec;
                at
            }
            None => {
                let at = self.specs.len();
                self.by_key.insert(key, at);
                self.specs.push(spec);
                self.placed.push(index);
                at
            }
        };
        if let (Some(slot), Ok(place)) = (self.at_index.get_mut(index as usize), u32::try_from(at))
        {
            *slot = place;
        }
        at
    }

    fn contains_key(&self, key: PrimKey<'_>) -> bool {
        self.by_key.contains_key(&key)
    }

    /// The place of the spec keyed `key`.
    fn place(&self, key: PrimKey<'_>) -> Option<usize> {
        self.by_key.get(&key).copied()
    }

    /// The place of the spec at path table entry `index`, if a spec was
    /// placed with it.
    fn place_at(&self, index: u32) -> Option<usize> {
        match self.at_index.get(index as usize) {
            Some(&at) if at != Self::NONE => Some(at as usize),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: PrimKey<'_>) -> Option<&mut PrimSpec> {
        let at = self.place(key)?;
        self.spec_mut(at)
    }

    /// The spec at `place`.
    fn spec_mut(&mut self, place: usize) -> Option<&mut PrimSpec> {
        self.specs.get_mut(place).map(|spec| &mut **spec)
    }

    /// The path table index each spec was first placed with, in the order
    /// of [`PrimSpecs::into_specs`].
    fn placed(&self) -> &[u32] {
        &self.placed
    }

    /// The specs, in the order their paths were first spelled.
    #[expect(clippy::vec_box, reason = "the specs move into the layer boxed")]
    fn into_specs(self) -> Vec<Box<PrimSpec>> {
        self.specs
    }
}

/// A spec's fields: names borrowed from the token table, and their values.
type Fields<'a> = Vec<(&'a str, DecodedField<'a>)>;

impl<'a, 'b> AssembleCtx<'a, 'b> {
    /// Records content that could not be represented.
    ///
    /// A diagnostic copies the spec path, and a spec may report once per
    /// field, so each diagnostic is charged for its text.
    fn report<'p>(
        &mut self,
        spec_path: impl Into<SpecName<'p>>,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Result<(), UsdcError> {
        let spec_path = match spec_path.into() {
            SpecName::Text(text) => text,
            SpecName::Entry(index) => self.sections.path(index as usize).unwrap_or_default(),
        };
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

    /// Appends one `layerRelocates` entry to `layer`. An empty target path
    /// removes the source; an entry whose paths are not prim paths (a
    /// variant selection, a property, a relative path) is reported instead.
    ///
    /// Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`).
    fn push_relocate(
        &mut self,
        layer: &mut Layer,
        source: &str,
        target: &str,
    ) -> Result<(), UsdcError> {
        let prim_path = |ctx: &mut Self, text: &str| {
            let names = text.strip_prefix('/')?;
            let is_name = |name: &str| {
                let mut chars = name.chars();
                let allowed = |c: char| {
                    c == '_' || c.is_ascii_alphanumeric() || (!c.is_ascii() && !c.is_whitespace())
                };
                chars
                    .next()
                    .is_some_and(|first| !first.is_ascii_digit() && allowed(first))
                    && chars.all(allowed)
            };
            if !names.split('/').all(is_name) {
                return None;
            }
            let path = Path::parse_absolute(text, ctx.tokens).ok()?;
            Some(ctx.paths.intern(path))
        };
        let source_path = prim_path(self, source);
        let target_path = if target.is_empty() {
            Some(None)
        } else {
            prim_path(self, target).map(Some)
        };
        match (source_path, target_path) {
            (Some(source), Some(target)) => {
                layer
                    .relocates_mut()
                    .push(layerstack::Relocate { source, target });
                Ok(())
            }
            _ => self.report(
                "/",
                Some("layerRelocates"),
                alloc::format!("relocate `<{source}>: <{target}>` is not between prim paths"),
            ),
        }
    }

    /// Assembles all specs into a [`Layer`], converting prim and property
    /// specs on the threads of `threads` ([`Self::run_pass`]).
    fn assemble_layer(
        &mut self,
        inputs: ReadInputs<'a>,
        threads: &Threads,
    ) -> Result<Layer, UsdcError> {
        let mut layer = Layer::new(self.layer_id);

        // Build a map from prim path string → PrimSpec being constructed.
        // We need to process specs in a specific order:
        // 1. PseudoRoot first (layer metadata)
        // 2. Prim specs (create PrimSpecs)
        // 3. Attribute/Relationship specs (add fields to parent prims)
        // 4. VariantSet/Variant specs (handle variant structures)

        // Reuse one field vector and decode each spec when its assembly
        // pass reaches it, so intermediate arrays do not remain alive alongside
        // the entire assembled layer.
        // Track visits to validate skipped/unsupported specs once at the end:
        // bulk import still checks every authored field (AOUSD Core §16.3).
        self.budget.charge_elements(self.sections.specs.len())?;
        let mut seen = alloc::vec![false; self.sections.specs.len()];
        let mut fields = Vec::new();

        // Build a path-to-spec-index map grouped by prim path for child lookup.
        let prims = self.sections.specs.iter();
        let prims = prims.filter(|spec| spec.form == SpecForm::Prim).count();
        let mut prim_specs_map = PrimSpecs::new(prims, self.sections.paths.len());
        // Authored property order (`propertyChildren`) per prim spec, by
        // its place in `prim_specs_map`.
        let mut property_children: Vec<Option<Order<'a>>> = Vec::with_capacity(prims);

        // Process PseudoRoot specs first.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::PseudoRoot {
                let mut decoded = Visits {
                    first: 0,
                    seen: &mut seen,
                };
                self.collect_spec_fields(i, &mut decoded, &mut fields)?;
                self.process_pseudo_root(&fields, &mut layer)?;
            }
        }

        // Process Prim specs, placing them in the order the file holds
        // them, then Attribute and Relationship specs, adding them to their
        // parent prim: on one thread each in turn, on more in chunks
        // (`Self::run_pass`). Specs under variant paths are handled with
        // the variant specs below.
        if !crate::threads::pooled(threads, self.sections.specs.len() > inputs.chunk) {
            let mut decoded = Visits {
                first: 0,
                seen: &mut seen,
            };
            for spec in 0..self.sections.specs.len() {
                if let Some(item) = self.prim_item(spec, &mut decoded, &mut fields)? {
                    place_prim(&mut prim_specs_map, &mut property_children, item);
                }
            }
            for spec in 0..self.sections.specs.len() {
                let item = self.property_item(spec, &prim_specs_map, &mut decoded, &mut fields)?;
                if let Some(item) = item {
                    place_property(&mut prim_specs_map, item);
                }
            }
        } else {
            let convert =
                |ctx: &mut AssembleCtx<'a, '_>,
                 spec: usize,
                 decoded: &mut Visits<'_>,
                 fields: &mut Fields<'a>| { ctx.prim_item(spec, decoded, fields) };
            let prim_form = [SpecForm::Prim];
            let items = self.run_pass(inputs, threads, &mut seen, &prim_form, &convert)?;
            for item in items.into_iter().flatten() {
                place_prim(&mut prim_specs_map, &mut property_children, item);
            }
            let prims = &prim_specs_map;
            let convert = |ctx: &mut AssembleCtx<'a, '_>,
                           spec: usize,
                           decoded: &mut Visits<'_>,
                           fields: &mut Fields<'a>| {
                ctx.property_item(spec, prims, decoded, fields)
            };
            let property_forms = [SpecForm::Attribute, SpecForm::Relationship];
            let items = self.run_pass(inputs, threads, &mut seen, &property_forms, &convert)?;
            for item in items.into_iter().flatten() {
                place_property(&mut prim_specs_map, item);
            }
        }
        let mut decoded = Visits {
            first: 0,
            seen: &mut seen,
        };

        // Process Connection specs — add connection paths to attribute on parent.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if spec.form == SpecForm::Connection {
                let path_str = self.lookup_path(spec.path_index)?;
                if let Some((prim_path, attr_name)) = self.split_property_path(path_str)
                    && let Some(prim) = prim_specs_map.get_mut(self.prim_key_of_text(prim_path))
                {
                    self.collect_spec_fields(i, &mut decoded, &mut fields)?;
                    self.apply_connection_fields(&fields, attr_name, prim)?;
                }
            }
        }

        // Process VariantSet and Variant specs.
        self.process_variant_specs(&mut decoded, &mut fields, &mut prim_specs_map)?;

        // Validate fields not consumed above, including malformed paths and
        // specs the layer model does not hold, and report unsupported forms.
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if decoded.is_marked(i) {
                continue;
            }
            self.collect_spec_fields(i, &mut decoded, &mut fields)?;
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
            if !handled && !fields.is_empty() {
                let path_str = self.lookup_path(spec.path_index)?;
                self.report(
                    path_str,
                    None,
                    alloc::format!("unsupported: {:?} spec fields are not read", spec.form),
                )?;
            }
        }

        drop(fields);

        // Keep the authored property order (`propertyChildren`).
        //
        // Spec: AOUSD Core §7.6.2.2.2 (`propertyChildren`).
        // Each prim's properties sort on their own: in chunks, on threads.
        let orders = prim_specs_map.specs.chunks_mut(SPECS_PER_CHUNK);
        let orders = orders
            .zip(property_children.chunks(SPECS_PER_CHUNK))
            .collect();
        let sort = |(prims, orders): (&mut [Box<PrimSpec>], &[Option<Order<'a>>])| {
            for (prim, order) in prims.iter_mut().zip(orders) {
                if let Some(order) = order {
                    sort_by_children(&mut prim.properties, order);
                }
            }
        };
        crate::threads::run_all(threads, orders, &sort);

        // Establish parent-child relationships.
        self.build_child_relationships(&mut prim_specs_map);

        // Convert prim_specs_map into layer prims. A prim inside variant
        // branches goes to its namespace path with its branch context, so
        // each branch keeps its own spec (`Layer::insert_prim`).
        //
        // Spec: AOUSD Core §7.3.6 (variant specs contain prim specs).
        // In the file's spec order, so paths intern in the same order on
        // every read, and every spec outside variant branches first: a
        // branch spec then joins the namespace spec at its path, as
        // `Layer::insert_prim` keeps them. Inserted as one change, so the
        // layer's map grows once.
        // Each spec is taken by the first spec spelling its path, which
        // placed it, so the specs are in the file's order of those specs.
        let placed: Vec<(u32, bool, Option<PathId>)> = prim_specs_map
            .placed()
            .iter()
            .map(|&index| {
                let branch = self.in_branch_of(SpecName::Entry(index));
                let path = if branch {
                    None
                } else {
                    self.prim_path_at(index)
                };
                (index, branch, path)
            })
            .collect();
        let mut branched: Vec<(&str, Box<PrimSpec>)> = Vec::new();
        let sections = self.sections;
        let plain = prim_specs_map
            .into_specs()
            .into_iter()
            .zip(&placed)
            .filter_map(|(prim, &(index, branch, path))| {
                if branch {
                    let path_str = sections.path(index as usize).unwrap_or_default();
                    branched.push((path_str, prim));
                    return None;
                }
                Some((path?, prim))
            });
        layer.insert_prims(plain);
        branched.sort_unstable_by_key(|(path, _)| *path);
        for (path_str, mut prim) in branched {
            let namespace = match split_branch_path(path_str) {
                Some((namespace, sites)) => {
                    prim.rare_mut().outer_variant_sites = self.branch_sites(&namespace, &sites)?;
                    Cow::Owned(namespace)
                }
                None => Cow::Borrowed(path_str),
            };
            if let Ok(path) = Path::parse_absolute(&namespace, self.tokens) {
                let path_id = self.paths.intern(path);
                layer.insert_prim(path_id, *prim);
            }
        }

        // Only a layer with a deferred arc has one to resolve, and most
        // have none: finding them visits every spec.
        if self.defer_variant_arcs
            && self.deferred_arcs
            && let Some(resolver) = self.resolver.as_deref_mut()
        {
            let resolved = layerstack::resolve_selected_deferred_arcs(
                &mut layer,
                resolver,
                self.tokens,
                self.paths,
            );
            self.resolved_layers.extend(resolved);
        }
        // No field of the file is a reference or payload list: no spec,
        // in a variant branch or not, authors one.
        let sections = self.sections;
        if !sections.fields.iter().any(|field| {
            matches!(
                self.lookup_token(field.token_index),
                "references" | "payload"
            )
        }) {
            layer.note_no_reference_arcs();
        }
        Ok(layer)
    }

    /// Converts the specs of `forms` in the file with `convert`, in chunks
    /// of `inputs.chunk` specs of `forms` ([`SPECS_PER_CHUNK`]) on the
    /// threads of `threads`, each chunk within what is left of this context's
    /// budget and without the resolver. A spec asking for the resolver is
    /// converted again here, with it, after the chunks. Returns what the
    /// specs converted to, by chunk, in spec order; charges their budget
    /// and keeps their diagnostics in spec order as converting them one by
    /// one would: the first chunk in spec order to fail, or to exceed the
    /// budget with the chunks before it, fails the pass.
    fn run_pass<T: SpecItem + Send>(
        &mut self,
        inputs: ReadInputs<'a>,
        threads: &Threads,
        seen: &mut [bool],
        forms: &[SpecForm],
        convert: &Convert<'_, 'a, T>,
    ) -> Result<Vec<Vec<T>>, UsdcError> {
        let limit = self.budget.remaining();
        #[cfg(feature = "std")]
        let lazy = self.budget.lazy.clone();
        let work = |(first, seen): (usize, &mut [bool])| {
            let mut budget = DecodeBudget::with_limit(limit);
            #[cfg(feature = "std")]
            {
                budget.lazy.clone_from(&lazy);
            }
            let mut ctx = inputs.ctx(&mut budget, None);
            let mut chunk = Chunk {
                items: Vec::new(),
                diagnostics: Vec::new(),
                deferred: Vec::new(),
                used: 0,
                deferred_arcs: false,
                error: None,
            };
            let specs = first..first + seen.len();
            let converted = inputs.sections.specs[specs.clone()].iter();
            let converted = converted.filter(|spec| forms.contains(&spec.form)).count();
            chunk.items.reserve_exact(converted);
            let mut decoded = Visits { first, seen };
            let mut fields = Vec::new();
            for spec in specs {
                if !forms.contains(&inputs.sections.specs[spec].form) {
                    continue;
                }
                let used = ctx.budget.used();
                let item = match convert(&mut ctx, spec, &mut decoded, &mut fields) {
                    Ok(item) => item,
                    Err(error) => {
                        chunk.error = Some(error);
                        break;
                    }
                };
                if core::mem::take(&mut ctx.wants_resolver) {
                    // Converted again with the resolver: as if never here.
                    ctx.budget.rewind_to(used);
                    ctx.diagnostics.clear();
                    if decoded.is_marked(spec) {
                        decoded.unmark(spec);
                    }
                    chunk.deferred.push(spec);
                    continue;
                }
                chunk.items.extend(item);
                if !ctx.diagnostics.is_empty() {
                    let reported = ctx.diagnostics.drain(..).map(|reported| (spec, reported));
                    chunk.diagnostics.extend(reported);
                }
            }
            chunk.deferred_arcs = ctx.deferred_arcs;
            chunk.used = budget.used();
            chunk
        };
        // Each chunk runs from its first spec to just before the spec that
        // would be its `inputs.chunk + 1`th of `forms`.
        let mut tasks = Vec::new();
        let (mut rest, mut first, mut count) = (&mut *seen, 0, 0);
        for (at, spec) in self.sections.specs.iter().enumerate() {
            if forms.contains(&spec.form) {
                if count == inputs.chunk {
                    let (chunk, tail) = rest.split_at_mut(at - first);
                    tasks.push((first, chunk));
                    (rest, first, count) = (tail, at, 0);
                }
                count += 1;
            }
        }
        tasks.push((first, rest));
        let chunks = crate::threads::run_all(threads, tasks, &work);

        let mut items = Vec::with_capacity(chunks.len());
        let mut diagnostics = Vec::new();
        let mut deferred = Vec::new();
        for chunk in chunks {
            if let Some(error) = chunk.error {
                if matches!(error, UsdcError::DecodeBudgetExceeded { .. }) {
                    return Err(self.budget.exceeded());
                }
                self.budget.charge(chunk.used)?;
                return Err(error);
            }
            self.budget.charge(chunk.used)?;
            self.deferred_arcs |= chunk.deferred_arcs;
            items.push(chunk.items);
            diagnostics.extend(chunk.diagnostics);
            deferred.extend(chunk.deferred);
        }
        if !deferred.is_empty() {
            let mut decoded = Visits { first: 0, seen };
            let mut fields = Vec::new();
            let mut all: Vec<T> = items.into_iter().flatten().collect();
            for spec in deferred {
                let before = self.diagnostics.len();
                all.extend(convert(self, spec, &mut decoded, &mut fields)?);
                let reported = self.diagnostics.drain(before..);
                diagnostics.extend(reported.map(|reported| (spec, reported)));
            }
            // Stable: a spec's diagnostics keep their order.
            all.sort_by_key(SpecItem::spec);
            sort_by_spec(&mut diagnostics);
            items = alloc::vec![all];
        }
        self.diagnostics
            .extend(diagnostics.into_iter().map(|(_, reported)| reported));
        Ok(items)
    }

    /// The prim spec `spec` of the spec table converts to, with its key
    /// and authored `properties` order; `None` for a spec of another form
    /// or whose path does not parse, which is reported.
    fn prim_item(
        &mut self,
        spec: usize,
        decoded: &mut Visits<'_>,
        fields: &mut Fields<'a>,
    ) -> Result<Option<PrimItem<'a>>, UsdcError> {
        let def = &self.sections.specs[spec];
        if def.form != SpecForm::Prim {
            return Ok(None);
        }
        // A spec with arcs asks the resolver: converted with it, not here.
        if self.resolver.is_none() && self.authors_arcs(def.fieldset_index) {
            self.wants_resolver = true;
            return Ok(None);
        }
        let index = def.path_index;
        self.charge_path(index)?;
        let name = SpecName::Entry(index);
        // Prims inside variant branches (`/A{v=x}B`) keep their
        // variant-qualified key until they are placed in the layer.
        if self.in_branch_of(name) && split_branch_path(self.path_text(index)).is_none() {
            self.report(name, None, "prim spec path could not be parsed")?;
            return Ok(None);
        }
        let (mut prim, children) = self.prim_spec(spec, name, decoded, fields)?;
        // Its property specs come next: as many as it lists, mostly.
        if let Some(children) = &children {
            prim.properties.reserve_exact(children.len());
        }
        let key = self.prim_key_at(index);
        Ok(Some(PrimItem {
            spec,
            index,
            key,
            prim,
            children,
        }))
    }

    /// The attribute or relationship spec `spec` of the spec table converts
    /// to, with the place in `prims` of the prim spec it belongs to; `None`
    /// for a spec of another form, one on a variant, which the variant
    /// pass places, or one that has no owning prim spec or whose path does
    /// not parse, which is reported.
    fn property_item(
        &mut self,
        spec: usize,
        prims: &PrimSpecs<'_>,
        decoded: &mut Visits<'_>,
        fields: &mut Fields<'a>,
    ) -> Result<Option<PropertyItem>, UsdcError> {
        let def = &self.sections.specs[spec];
        let is_attribute = def.form == SpecForm::Attribute;
        if !is_attribute && def.form != SpecForm::Relationship {
            return Ok(None);
        }
        let index = def.path_index;
        self.charge_path(index)?;
        let spec_name = SpecName::Entry(index);
        if let Some(owner) = self.property_owner(index) {
            let (owner, name) = match owner {
                PropertyOwner::Variant => return Ok(None),
                PropertyOwner::Invalid => {
                    self.report(spec_name, None, "property spec path could not be parsed")?;
                    return Ok(None);
                }
                PropertyOwner::Prim { owner, name } => (owner, name),
            };
            // Another entry may spell the owner's path.
            let place = match prims.place_at(owner) {
                Some(place) => Some(place),
                None => prims.place(self.prim_key_at(owner)),
            };
            let Some(place) = place else {
                self.report(spec_name, None, "property spec has no owning prim spec")?;
                return Ok(None);
            };
            let property = self.property_spec(spec, spec_name, is_attribute, decoded, fields)?;
            return Ok(Some(PropertyItem {
                spec,
                place,
                name,
                property,
            }));
        }
        let path_str = self.path_text(index);
        let (prim_path, name) = if path_str.contains('{') {
            // A property directly on a variant (`/A{v=x}.b`) belongs to
            // the variant spec and is handled with the variant specs;
            // one on a prim inside a branch (`/A{v=x}B.c`) is handled
            // here.
            match path_str.rsplit_once('.') {
                Some((prim, _)) if prim.ends_with('}') => return Ok(None),
                Some((prim, name)) => (prim, name),
                None => {
                    self.report(path_str, None, "property spec path could not be parsed")?;
                    return Ok(None);
                }
            }
        } else {
            // Validate without normalizing the spelling: borrow the
            // original text instead of formatting a parsed path back.
            let Some(parts) = self.split_property_path(path_str) else {
                self.report(path_str, None, "property spec path could not be parsed")?;
                return Ok(None);
            };
            parts
        };
        let Some(place) = prims.place(self.prim_key_of_text(prim_path)) else {
            self.report(path_str, None, "property spec has no owning prim spec")?;
            return Ok(None);
        };
        let property = self.property_spec(spec, path_str.into(), is_attribute, decoded, fields)?;
        let name = self.tokens.intern(name);
        Ok(Some(PropertyItem {
            spec,
            place,
            name,
            property,
        }))
    }

    /// Converts the variant selections of a crate path, as split by
    /// [`split_branch_path`], into sites whose host paths lie in `namespace`.
    ///
    /// Each selection's host path is parsed and interned, so a path with many
    /// selections is charged for each.
    fn branch_sites(
        &mut self,
        namespace: &str,
        sites: &[BranchSite<'_>],
    ) -> Result<Vec<VariantSelectionSite>, UsdcError> {
        let mut out = Vec::with_capacity(sites.len());
        for &(host_len, set, variant) in sites {
            self.budget.charge(1)?;
            self.budget.charge_text(host_len)?;
            let Ok(host) = Path::parse_absolute(&namespace[..host_len], self.tokens) else {
                continue;
            };
            out.push(VariantSelectionSite {
                host_path: self.paths.intern(host),
                set: self.tokens.intern(set),
                variant: self.tokens.intern(variant),
            });
        }
        Ok(out)
    }

    /// Decodes a spec once, retaining only a flag after its fields are consumed.
    /// Spec allocation is charged before the visit flags are allocated.
    fn collect_spec_fields(
        &mut self,
        index: usize,
        decoded: &mut Visits<'_>,
        fields: &mut Fields<'a>,
    ) -> Result<(), UsdcError> {
        decoded.mark(index);
        let spec = &self.sections.specs[index];
        let retain_default = spec.form == SpecForm::Attribute;
        self.collect_fields(spec.fieldset_index, retain_default, fields)
    }

    /// Collects decoded fields for a spec from the fieldsets/fields tables.
    ///
    /// Field names are borrowed from the token table: specs sharing a
    /// fieldset share its names. Each use is still charged by the name's
    /// length, since assembly matches, interns or reports it.
    fn collect_fields(
        &mut self,
        fieldset_index: u32,
        retain_default: bool,
        result: &mut Fields<'a>,
    ) -> Result<(), UsdcError> {
        result.clear();
        let start = fieldset_index as usize;

        if start >= self.sections.fieldsets.len() {
            return Ok(());
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
                #[cfg(feature = "std")]
                if retain_default
                    && field_name == "default"
                    && self.budget.lazy.is_some()
                    && crate::lazy::retains_text_array(&rep, self.data)
                {
                    result.push((field_name, DecodedField::Retained(rep)));
                    idx += 1;
                    continue;
                }
                let _ = retain_default;
                let value = if is_token_array(&rep) {
                    DecodedField::TokenArray(decode_text_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else if is_string_array(&rep) {
                    DecodedField::StringArray(decode_text_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else if is_token_vector(&rep) {
                    DecodedField::TokenArray(decode_token_vector_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else if matches!(field_name, "targetPaths" | "connectionPaths")
                    && is_path_list_op(&rep)
                {
                    // Targets are named through the path tree
                    // (`Self::target_path_at`), not their text.
                    DecodedField::PathListOp(decode_path_list_op_indices(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                    )?)
                } else if let Some(kind) = scalar_text_kind(&rep) {
                    // Converted from the token table's text, not a copy.
                    let index =
                        decode_text_index(&rep, self.data, self.sections, kind, self.budget)?;
                    DecodedField::Text(kind, index)
                } else {
                    let tokens = self.token_ids();
                    let value = decode_field_within(
                        &rep,
                        self.data,
                        self.sections,
                        self.budget,
                        Some(tokens),
                    )?;
                    match value {
                        // Text, not a string value: read as USDA when the
                        // field is converted (`Self::convert_metadata`).
                        DecodedField::Value(CrateValue::String(text))
                            if rep.value_type().ok() == Some(ValueType::UnregisteredValue) =>
                        {
                            DecodedField::UnregisteredText(text)
                        }
                        value => value,
                    }
                };
                // This is the ownership boundary: assembly subsequently borrows
                // fields, so retain decoded numeric allocations before that borrow.
                // Raw math arrays already borrow the file bytes and are converted
                // once by assembly; the generic decoder remains unchanged.
                let value = match value {
                    DecodedField::FloatArray(array) => {
                        DecodedField::Value(CrateValue::TypedArray(float_array(array)))
                    }
                    other => other,
                };
                result.push((field_name, value));
            }
            idx += 1;
        }

        Ok(())
    }

    /// Processes a `PseudoRoot` spec: sublayers, root prim order and layer
    /// metadata.
    ///
    /// Spec: AOUSD Core §7.6.1 (layer spec fields).
    fn process_pseudo_root(
        &mut self,
        fields: &[(&str, DecodedField<'_>)],
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
                    if let Some(CrateValue::PathVector(paths) | CrateValue::StringVector(paths)) =
                        value.value()
                    {
                        sublayer_paths = paths;
                    }
                }
                "subLayerOffsets" => {
                    if let Some(CrateValue::LayerOffsetVector(offsets)) = value.value() {
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
                    if let DecodedField::Text(TextKind::Token, index) = *value {
                        layer.set_default_prim(Some(self.token_id(index)));
                    }
                }
                "layerRelocates" => {
                    // Spec: AOUSD Core §7.6.1.2.4 (`layerRelocates`),
                    // §16.3.10.15 (crate relocates values).
                    if let Some(CrateValue::RelocatesMap(pairs)) = value.value() {
                        // The field is authored even with no entries.
                        let _ = layer.relocates_mut();
                        for (source, target) in pairs {
                            self.push_relocate(layer, source, target)?;
                        }
                    }
                }
                "relocates" => {
                    // Prim relocates are legacy scene description that
                    // OpenUSD ignores when composing a USD stage
                    // (`Pcp_ComputeRelocationsForLayerStack` in
                    // `pxr/usd/pcp/layerStack.cpp`).
                    self.report("/", Some(name), "unsupported: prim relocates are ignored")?;
                }
                _ => {
                    if let Some(field_value) = self.convert_metadata("/".into(), name, value)? {
                        let key = self.tokens.intern(name);
                        set_field_vec(layer.metadata_fields_mut(), key, field_value);
                    }
                }
            }
        }

        // Spec: AOUSD Core §12.3.2.1 (sublayer offsets). A sublayer that
        // does not resolve keeps its place; composition reports it
        // (§10.3.1, §10.6; OpenUSD `PcpErrorInvalidSublayerPath`).
        // Offsets that do not pair with the sublayers (§7.6.1.2.2) give
        // every sublayer the identity offset, and composition reports the
        // counts, as OpenUSD 26.11 does (`PcpErrorInvalidSublayerAndOffsetCount`
        // in `PcpLayerStack::_BuildLayerStack`, `pxr/usd/pcp/layerStack.cpp`).
        let paired = sublayer_offsets.len() == sublayer_paths.len();
        for (i, asset_path) in sublayer_paths.iter().enumerate() {
            let offset = sublayer_offsets.get(i).filter(|_| paired).map_or(
                LayerOffset::IDENTITY,
                |&(offset, scale)| LayerOffset { offset, scale },
            );
            let Some(resolved) = self.resolve_asset(asset_path) else {
                layer
                    .sublayers_mut()
                    .push(SublayerEntry::unresolved(asset_path.as_str(), offset));
                continue;
            };
            layer.sublayers_mut().push(SublayerEntry::with_asset(
                resolved.layer_id,
                asset_path.as_str(),
                offset,
            ));
            if let Some(sub_layer) = resolved.layer {
                self.resolved_layers.push(sub_layer);
            }
        }
        if !paired {
            layer.set_unpaired_sublayer_offsets(Some(sublayer_offsets.len()));
        }

        // Create a root prim spec for child ordering.
        if !root_children.is_empty() || prim_order.is_some() {
            let root_path = Path::root();
            let root_path_id = self.paths.intern(root_path);
            let mut root_spec = PrimSpec::default().with_children(root_children);
            if let Some(order) = prim_order {
                root_spec = root_spec.with_prim_order(order);
            }
            layer.insert_prim(root_path_id, root_spec);
        }

        Ok(())
    }

    /// Fills `spec`, a new [`PrimSpec`], from decoded fields, returning the
    /// authored property order (`propertyChildren`) when present.
    ///
    /// Spec: AOUSD Core §7.6.2 (prim spec fields).
    fn build_prim_spec(
        &mut self,
        spec_path: SpecName<'_>,
        fields: &[(&str, DecodedField<'_>)],
        spec: &mut PrimSpec,
    ) -> Result<Option<Vec<TokenId>>, UsdcError> {
        let mut property_children = None;
        self.in_branch = self.in_branch_of(spec_path);

        for (name, value) in fields {
            match *name {
                "specifier" => {
                    if let Some(CrateValue::Specifier(v)) = value.value() {
                        spec.specifier = Some(match v {
                            0 => Specifier::Def,
                            1 => Specifier::Over,
                            2 => Specifier::Class,
                            _ => Specifier::Def,
                        });
                    }
                }
                "typeName" => {
                    if let DecodedField::Text(TextKind::Token, index) = *value
                        && !self.lookup_token(index).is_empty()
                    {
                        spec.type_name = Some(self.token_id(index));
                    }
                }
                "primChildren" => {
                    spec.authored_children = self.extract_token_names(value);
                }
                "primOrder" => {
                    spec.rare_mut().prim_order = Some(self.extract_token_names(value));
                }
                // `SdfChildrenKeys->PropertyChildren` is spelled `properties`
                // (`pxr/usd/sdf/schema.h`).
                "properties" => {
                    property_children = Some(self.extract_token_names(value));
                }
                "propertyOrder" => {
                    spec.rare_mut().property_order = Some(self.extract_token_names(value));
                }
                // The set specs, read from their own specs, in this order
                // (see `declare_variant_sets`).
                "variantSetChildren" => {
                    let names = self.extract_token_names(value);
                    declare_variant_sets(&mut spec.rare_mut().variant_sets, &names);
                }
                "references" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value() {
                        let converted = self.convert_ref_listop(listop)?;
                        spec.references.make_mut().merge(converted);
                    }
                }
                // Spec: AOUSD Core §7.6.2.3.2 (`payload`).
                "payload" => {
                    if let Some(converted) = self.convert_payload_value(value)? {
                        spec.payloads.make_mut().merge(converted);
                    }
                }
                // Spec: AOUSD Core §7.6.2.3.3 (`inheritPaths`).
                "inheritPaths" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value() {
                        let converted = self.convert_path_listop(listop)?;
                        spec.rare_mut().inherits.make_mut().merge(converted);
                    }
                }
                "specializes" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value() {
                        let converted = self.convert_path_listop(listop)?;
                        spec.rare_mut().specializes.make_mut().merge(converted);
                    }
                }
                "variantSelection" => {
                    if let Some(CrateValue::VariantSelectionMap(pairs)) = value.value() {
                        for (set_name, branch_name) in pairs {
                            let set_tok = self.tokens.intern(set_name);
                            let branch_tok = self.tokens.intern(branch_name);
                            spec.rare_mut()
                                .variant_selections
                                .insert(set_tok, branch_tok);
                        }
                    }
                }
                "variantSetNames" => {
                    spec.rare_mut().variant_set_names = self.variant_set_names(value).into();
                }
                "instanceable" if matches!(value.value(), Some(CrateValue::Bool(_))) => {
                    if let Some(CrateValue::Bool(b)) = value.value() {
                        spec.instanceable = Some(*b);
                    }
                }
                "active" if matches!(value.value(), Some(CrateValue::Bool(_))) => {
                    if let Some(CrateValue::Bool(b)) = value.value() {
                        spec.active = Some(*b);
                    }
                }
                "relocates" => {
                    // Prim relocates are legacy scene description; only
                    // `layerRelocates` is composed (AOUSD Core §10.3.2.6).
                    // OpenUSD 26.11 removed the field from Sdf's schema, yet
                    // its crate reader keeps it as an unregistered field and
                    // composes without it, as this reader does.
                    self.report(
                        spec_path,
                        Some(name),
                        "unsupported: prim relocates are ignored",
                    )?;
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

        Ok(property_children)
    }

    /// Builds the prim spec `index` of the spec table, at `spec_path`,
    /// with its authored `properties` order: from its fields, or as a copy
    /// of the spec built for a spec sharing its fieldset ([`Shared`]).
    fn prim_spec(
        &mut self,
        index: usize,
        spec_path: SpecName<'_>,
        decoded: &mut Visits<'_>,
        fields: &mut Fields<'a>,
    ) -> Result<(Box<PrimSpec>, Option<Order<'a>>), UsdcError> {
        let fieldset = self.sections.specs[index].fieldset_index;
        // A spec inside a variant branch converts its arcs as one.
        let shared = self
            .shared_specs(fieldset)
            .filter(|_| !self.in_branch_of(spec_path));
        if let Some(shared) = shared.and_then(|shared| shared.prim.get()) {
            self.budget.charge(shared.charged)?;
            decoded.mark(index);
            let (spec, children) = &shared.spec;
            let children = children.as_deref().map(Order::Shared);
            return Ok((Box::new(copy_prim(spec)), children));
        }
        let (used, diagnostics) = (self.budget.used(), self.diagnostics.len());
        self.collect_spec_fields(index, decoded, fields)?;
        // Built where it is kept: a spec is too large to move cheaply.
        let mut prim = Box::new(PrimSpec::default());
        let children = self.build_prim_spec(spec_path, fields, &mut prim)?;
        let resolves = fields
            .iter()
            .any(|(name, _)| matches!(*name, "references" | "payload"));
        if let Some(shared) = shared
            && !resolves
            && !self.wants_resolver
            && self.diagnostics.len() == diagnostics
        {
            let charged = self.budget.used() - used;
            let _ = shared.prim.set(Arc::new(Shared {
                spec: (Box::new(copy_prim(&prim)), children.clone()),
                charged,
            }));
        }
        Ok((prim, children.map(Order::Own)))
    }

    /// Builds the attribute or relationship spec `index` of the spec table,
    /// at `spec_path`: from its fields, or shared with a spec sharing its
    /// fieldset ([`Shared`]).
    fn property_spec(
        &mut self,
        index: usize,
        spec_path: SpecName<'_>,
        is_attribute: bool,
        decoded: &mut Visits<'_>,
        fields: &mut Fields<'a>,
    ) -> Result<Arc<PropertySpec>, UsdcError> {
        let fieldset = self.sections.specs[index].fieldset_index;
        let shared = self.shared_specs(fieldset).map(|shared| {
            if is_attribute {
                &shared.attribute
            } else {
                &shared.relationship
            }
        });
        if let Some(shared) = shared.and_then(OnceSlot::get) {
            self.budget.charge(shared.charged)?;
            decoded.mark(index);
            return Ok(Arc::clone(&shared.spec));
        }
        let (used, diagnostics) = (self.budget.used(), self.diagnostics.len());
        self.collect_spec_fields(index, decoded, fields)?;
        // Built where it is kept: a spec is too large to move cheaply.
        let mut spec = Arc::new(if is_attribute {
            PropertySpec::attribute()
        } else {
            PropertySpec::relationship()
        });
        let built = Arc::get_mut(&mut spec).expect("a new spec");
        if is_attribute {
            self.build_attribute_spec(spec_path, fields, built)?;
        } else {
            self.build_relationship_spec(spec_path, fields, built)?;
        }
        if let Some(shared) = shared
            && self.diagnostics.len() == diagnostics
        {
            let charged = self.budget.used() - used;
            let _ = shared.set(Shared {
                spec: Arc::clone(&spec),
                charged,
            });
        }
        Ok(spec)
    }

    /// Whether the fieldset at `fieldset` holds a `references` or `payload`
    /// field, whose arcs the resolver resolves.
    fn authors_arcs(&self, fieldset: u32) -> bool {
        let sections = self.sections;
        let fields = sections
            .fieldsets
            .get(fieldset as usize..)
            .unwrap_or_default();
        fields
            .iter()
            .take_while(|&&field| field >= 0)
            .filter_map(|&field| sections.fields.get(field as usize))
            .any(|field| {
                matches!(
                    self.lookup_token(field.token_index),
                    "references" | "payload"
                )
            })
    }

    /// The specs built for the fieldset at `fieldset`, when more than one
    /// spec uses it.
    fn shared_specs(&self, fieldset: u32) -> Option<&'a SharedSpecs> {
        let tables = self.tables;
        if tables
            .fieldset_uses
            .get(fieldset as usize)
            .is_none_or(|&uses| uses < 2)
        {
            return None;
        }
        let place = *tables.shared_fieldsets.get(&fieldset)?;
        tables.shared.get(place)
    }

    /// Applies a connection child spec's paths to its attribute, when the
    /// attribute itself authors no `connectionPaths`.
    fn apply_connection_fields(
        &mut self,
        fields: &[(&str, DecodedField<'_>)],
        attr_name: &str,
        prim: &mut PrimSpec,
    ) -> Result<(), UsdcError> {
        let name_tok = self.tokens.intern(attr_name);

        for (field_name, value) in fields {
            if *field_name == "connectionPaths" || *field_name == "targetPaths" {
                let listop = self.convert_connection_value(value)?;
                let spec = property_entry(&mut prim.properties, name_tok, PropertyKind::Attribute);
                if spec.targets.is_none() {
                    spec.targets = Some(Box::new(listop));
                }
                return Ok(());
            }
        }

        Ok(())
    }

    /// Processes `VariantSet` and `Variant` specs.
    ///
    /// A variant set spec (`/P{v=}`) and a variant spec (`/P{v=x}`) belong to
    /// the spec holding the set: the prim spec owning it (`/P`, or
    /// `/P{a=y}C` for a set on a prim inside a branch, `/P{a=y}C{v=x}`), or
    /// for a set nested in another branch of the same prim
    /// (`/P{a=y}{v=x}`), that branch's variant spec. Each variant is a spec
    /// of its own, so a set nested under two branches, or reusing an
    /// enclosing set's name, stays two sets. Specs are placed outer first,
    /// so every holder exists before the sets it holds.
    ///
    /// Spec: AOUSD Core §7.3.6 (variant specs may contain variant set
    /// specs), §7.6.7 (variant specs), §16.3 (crate paths).
    fn process_variant_specs(
        &mut self,
        decoded: &mut Visits<'_>,
        fields: &mut Fields<'a>,
        prim_specs: &mut PrimSpecs<'_>,
    ) -> Result<(), UsdcError> {
        // Properties directly on a variant (`/P{v=x}.b`), by variant path.
        let mut variant_properties: HashMap<&str, Vec<PropertyEntry>> = HashMap::new();
        // Only a path holding a `}` names a variant: in a file without one,
        // each property's path is charged, all at once, and passed over.
        let paths = &self.sections.paths;
        let any_close = (0..paths.len()).any(|at| paths.spelling(at).is_some_and(|s| s.close()));
        if !any_close {
            let mut units = 0_usize;
            for spec in &self.sections.specs {
                if matches!(spec.form, SpecForm::Attribute | SpecForm::Relationship) {
                    let len = paths.text_len(spec.path_index as usize).ok_or(
                        UsdcError::Inconsistent {
                            message: "path index out of range",
                        },
                    )?;
                    units = units.saturating_add(len / 16);
                }
            }
            self.budget.charge_elements(units)?;
        }
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if !any_close
                || (spec.form != SpecForm::Attribute && spec.form != SpecForm::Relationship)
            {
                continue;
            }
            let index = spec.path_index;
            self.charge_path(index)?;
            // Properties of prims inside a branch are attached with their
            // prim. A property element holding no `.` is split from its
            // prim at its own `.`: on a variant when that prim's text ends
            // with `}`.
            let sections = self.sections;
            if let Some(&PathNode::Child {
                parent,
                token,
                property: true,
            }) = sections.paths.nodes().get(index as usize)
                && !self.lookup_token(token).contains('.')
                && !sections
                    .paths
                    .spelling(parent as usize)
                    .is_some_and(|spelling| spelling.ends_with_close())
            {
                continue;
            }
            let path_str = self.path_text(index);
            let Some((variant_path, prop_name)) = path_str
                .rsplit_once('.')
                .filter(|(prim, _)| prim.ends_with('}'))
            else {
                continue;
            };
            self.collect_spec_fields(i, decoded, fields)?;
            let property = if spec.form == SpecForm::Attribute {
                let mut property = PropertySpec::attribute();
                self.build_attribute_spec(path_str.into(), fields, &mut property)?;
                property
            } else {
                let mut property = PropertySpec::relationship();
                self.build_relationship_spec(path_str.into(), fields, &mut property)?;
                property
            };
            let prop_tok = self.tokens.intern(prop_name);
            set_property_vec(
                variant_properties.entry(variant_path).or_default(),
                prop_tok,
                property,
            );
        }

        // Variant set and variant specs, outer before nested, and a set
        // before its variants.
        let mut specs: Vec<(usize, &str)> = Vec::new();
        for (i, spec) in self.sections.specs.iter().enumerate() {
            if matches!(spec.form, SpecForm::VariantSet | SpecForm::Variant) {
                specs.push((i, self.lookup_path(spec.path_index)?));
            }
        }
        specs.sort_by(|(_, a), (_, b)| {
            let key = |path: &str| (path.matches('{').count(), !path.ends_with("=}"));
            key(a).cmp(&key(b)).then_with(|| a.cmp(b))
        });

        // Each set's `variantChildren`, by its path up to the `=` its
        // variants' paths share (`/P{v=`): its variants keep that order,
        // as `SdfVariantSetSpec::GetVariants` lists them, and it sizes them.
        let mut variant_children: HashMap<&str, Vec<TokenId>> = HashMap::new();
        for (i, path_str) in specs {
            let is_set = self.sections.specs[i].form == SpecForm::VariantSet;
            let placed = split_trailing_selections(path_str)
                .filter(|(owner, _)| prim_specs.contains_key(self.prim_key_of_text(owner)))
                .filter(|(_, chain)| {
                    chain.split_last().is_some_and(|((_, last), enclosing)| {
                        last.is_empty() == is_set
                            && enclosing.iter().all(|(_, variant)| !variant.is_empty())
                    })
                });
            let Some((owner, chain)) = placed else {
                let what = if is_set { "variant set" } else { "variant" };
                self.report(
                    path_str,
                    None,
                    alloc::format!("{what} spec has no owning prim spec"),
                )?;
                continue;
            };
            let chain: Vec<(TokenId, TokenId)> = chain
                .iter()
                .map(|(set, variant)| (self.tokens.intern(set), self.tokens.intern(variant)))
                .collect();
            let ((set, variant), enclosing) = chain.split_last().expect("a selection");
            self.collect_spec_fields(i, decoded, fields)?;
            let set_key = path_str.rfind('=').map_or(path_str, |at| &path_str[..=at]);
            let variant_spec = if is_set {
                if let Some((_, children)) =
                    fields.iter().find(|(name, _)| *name == "variantChildren")
                {
                    let names = self.extract_token_names(children);
                    variant_children.insert(set_key, names);
                }
                None
            } else {
                Some(self.build_variant_spec(path_str, fields, &mut variant_properties)?)
            };
            let prim = prim_specs
                .get_mut(self.prim_key_of_text(owner))
                .expect("owner checked above");
            let holder = if enclosing.is_empty() {
                Some(&mut prim.rare_mut().variant_sets)
            } else {
                prim.variant_spec_mut(enclosing)
                    .map(|spec| &mut spec.variant_sets)
            };
            let Some(sets) = holder else {
                self.report(path_str, None, "variant spec has no enclosing variant spec")?;
                continue;
            };
            // The set spec alone: only `variantSetNames` lists a set
            // (AOUSD Core §7.6.2.3.5).
            let set_spec = sets.get_or_insert_default(*set);
            let children = variant_children.get(set_key).map_or(&[][..], Vec::as_slice);
            match variant_spec {
                None => set_spec.variants.reserve(children.len()),
                Some(variant_spec) if !set_spec.variants.contains_key(variant) => {
                    // A variant `variantChildren` does not list goes last.
                    let rank = |name: &TokenId| {
                        children
                            .iter()
                            .position(|child| child == name)
                            .unwrap_or(usize::MAX)
                    };
                    let at = set_spec
                        .variants
                        .keys()
                        .take_while(|name| rank(name) <= rank(variant))
                        .count();
                    set_spec.variants.insert_at(at, *variant, variant_spec);
                }
                Some(variant_spec) => {
                    set_spec.variants.insert(*variant, variant_spec);
                }
            }
        }

        // Properties of variants that have no variant spec.
        let mut orphans: Vec<&str> = variant_properties.into_keys().collect();
        orphans.sort_unstable();
        for path in orphans {
            self.report(path, None, "variant property has no variant spec")?;
        }

        Ok(())
    }

    /// Builds the variant spec at `path_str` from its fields, with the
    /// properties read for it from `variant_properties`.
    ///
    /// Spec: AOUSD Core §7.6.7 (variant specs hold the fields a prim spec
    /// holds).
    fn build_variant_spec(
        &mut self,
        path_str: &str,
        fields: &Fields<'_>,
        variant_properties: &mut HashMap<&str, Vec<PropertyEntry>>,
    ) -> Result<VariantSpec, UsdcError> {
        let mut variant = VariantSpec::default();
        let mut property_children = None;
        self.in_branch = true;
        for (name, value) in fields {
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
                // Nested variant sets are read from their own specs, in
                // this order.
                "variantSetChildren" => {
                    let names = self.extract_token_names(value);
                    declare_variant_sets(&mut variant.variant_sets, &names);
                }
                "variantSetNames" => {
                    // The nested sets, as for a prim spec.
                    variant.variant_set_names = self.variant_set_names(value).into();
                }
                "variantSelection" => {
                    if let Some(CrateValue::VariantSelectionMap(pairs)) = value.value() {
                        for (sn, bn) in pairs {
                            let st = self.tokens.intern(sn);
                            let bt = self.tokens.intern(bn);
                            variant.variant_selections.insert(st, bt);
                        }
                    }
                }
                "references" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value()
                        && let Ok(converted) = self.convert_ref_listop(listop)
                    {
                        variant.references.make_mut().merge(converted);
                    }
                }
                "payload" => {
                    if let Ok(Some(converted)) = self.convert_payload_value(value) {
                        variant.payloads.make_mut().merge(converted);
                    }
                }
                "inheritPaths" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value()
                        && let Ok(converted) = self.convert_path_listop(listop)
                    {
                        variant.inherits.make_mut().merge(converted);
                    }
                }
                "specializes" => {
                    if let Some(CrateValue::ListOp(listop)) = value.value()
                        && let Ok(converted) = self.convert_path_listop(listop)
                    {
                        variant.specializes.make_mut().merge(converted);
                    }
                }
                _ => {
                    // Generic variant field.
                    if let Some(fv) = self.convert_metadata(path_str.into(), name, value)? {
                        let key = self.tokens.intern(name);
                        set_field_vec(&mut variant.fields, key, fv);
                    }
                }
            }
        }
        if let Some(properties) = variant_properties.remove(path_str) {
            variant.properties = properties;
        }
        if let Some(order) = property_children {
            sort_by_children(&mut variant.properties, &order);
        }
        Ok(variant)
    }

    /// Builds parent-child relationships by examining prim paths.
    fn build_child_relationships(&mut self, prim_specs: &mut PrimSpecs<'a>) {
        // Each parent's child specs, parents and children in the file's
        // spec order, so names intern in the same order on every read. A
        // child is its path table entry and name.
        let mut parents: Vec<PrimKey<'a>> = Vec::new();
        let mut children: HashMap<PrimKey<'a>, Vec<(u32, TokenId)>> = HashMap::new();
        let sections = self.sections;
        for spec in &sections.specs {
            if spec.form != SpecForm::Prim {
                continue;
            }
            let index = spec.path_index;
            // Specs inside variant branches have no parent prim spec here.
            if index as usize >= sections.paths.len() || self.in_branch_of(SpecName::Entry(index)) {
                continue;
            }
            let key = self.prim_key_at(index);
            if !prim_specs.contains_key(key) {
                continue;
            }
            // The parent strips the path's last name: from the path, or as
            // the text is split.
            let (parent, child) = match key {
                PrimKey::Path(path) => {
                    let Some(parent) = self.paths.parent(path) else {
                        continue;
                    };
                    let Some(name) = self.paths.resolve(path).leaf() else {
                        continue;
                    };
                    (PrimKey::Path(parent), name)
                }
                PrimKey::Text(path) => {
                    let Some(parent_path) = parent_prim_path(path) else {
                        continue;
                    };
                    let child_name = path.rsplit('/').next().unwrap_or("");
                    if child_name.is_empty() {
                        continue;
                    }
                    (
                        self.prim_key_of_text(parent_path),
                        self.tokens.intern(child_name),
                    )
                }
            };
            children
                .entry(parent)
                .or_insert_with(|| {
                    parents.push(parent);
                    Vec::new()
                })
                .push((index, child));
        }

        // Only add to parent's authored_children if not already present
        // (the prim's own primChildren field is authoritative), in path
        // order.
        for parent_key in parents {
            let Some(parent) = prim_specs.get_mut(parent_key) else {
                continue;
            };
            let mut named = children.remove(&parent_key).unwrap_or_default();
            let listed: HashSet<TokenId> = parent.authored_children.iter().copied().collect();
            named.retain(|(_, name)| !listed.contains(name));
            if named.is_empty() {
                continue;
            }
            // The few a parent does not list are ordered by their texts.
            let mut named: Vec<(&str, TokenId)> = named
                .into_iter()
                .map(|(index, name)| (self.path_text(index), name))
                .collect();
            named.sort_unstable();
            append_unique(
                &mut parent.authored_children,
                named.into_iter().map(|(_, name)| name),
            );
        }
    }

    // ── Value conversion helpers ──────────────────────────────────────

    /// Converts a validated field directly into its final layer value.
    fn convert_field_value(&mut self, field: &DecodedField<'_>) -> Value {
        match field {
            DecodedField::Value(value) => self.convert_crate_value(value),
            DecodedField::FloatArray(array) => Value::TypedArray(match array {
                FloatArray::Half(values) => layerstack::TypedArray::Half(Arc::new(values.clone())),
                FloatArray::Float(values) => {
                    layerstack::TypedArray::Float(Arc::new(values.clone()))
                }
                FloatArray::Double(values) => {
                    layerstack::TypedArray::Double(Arc::new(values.clone()))
                }
                FloatArray::TimeCode(values) => {
                    layerstack::TypedArray::TimeCode(Arc::new(values.clone()))
                }
                FloatArray::Duration(values) => {
                    layerstack::TypedArray::Duration(Arc::new(values.clone()))
                }
            }),
            DecodedField::MathArray(array) => convert_math_array(array),
            // List ops are not plain values.
            DecodedField::PathListOp(_) => Value::Null,
            // Read as USDA only as metadata; elsewhere (the value of an
            // attribute of a type no type registers), as the text it is.
            DecodedField::UnregisteredText(text) => Value::Unregistered(Arc::from(text.as_str())),
            DecodedField::Text(kind, index) => {
                let text = self.lookup_token(*index);
                match kind {
                    TextKind::Token => Value::Token(self.token_id(*index)),
                    TextKind::String => Value::String(Arc::from(text)),
                    TextKind::AssetPath => Value::Asset(Arc::from(text)),
                    TextKind::PathExpression => Value::PathExpression(Arc::from(text)),
                }
            }
            // Spec: AOUSD Core §6.2 (`token[]` and `string[]`); the ids
            // are collected straight into their shared allocation.
            DecodedField::TokenArray(indices) => Value::TypedArray(layerstack::TypedArray::Token(
                indices.iter().map(|&index| self.token_id(index)).collect(),
            )),
            DecodedField::StringArray(indices) => {
                Value::from(string_array(&self.sections.tokens, indices))
            }
            #[cfg(feature = "std")]
            DecodedField::Retained(rep) => {
                let pool = self
                    .budget
                    .lazy
                    .as_ref()
                    .expect("retained by a retained read");
                Value::Deferred(pool.register_value(*rep))
            }
        }
    }

    /// The token id of entry `index` of the token table, interning it on
    /// first use; the empty token outside the table, as the generic decoder
    /// reads it.
    fn token_id(&mut self, index: u32) -> TokenId {
        self.token_ids().id(index)
    }

    /// What converts token-table indices to token ids, for this read.
    fn token_ids(&self) -> TokenIds<'a> {
        TokenIds {
            table: &self.sections.tokens,
            ids: &self.tables.token_ids,
            interner: self.tokens,
        }
    }

    /// Converts a [`CrateValue`] to a [`Value`].
    fn convert_crate_value(&mut self, cv: &CrateValue) -> Value {
        match cv {
            CrateValue::None => Value::Blocked,
            CrateValue::AnimationBlock => Value::AnimationBlock,
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
            CrateValue::Duration(v) => Value::Duration(*v),
            CrateValue::String(s) => Value::String(Arc::from(s.as_str())),
            CrateValue::Token(t) => Value::Token(self.tokens.intern(t)),
            CrateValue::AssetPath(p) => Value::Asset(Arc::from(p.as_str())),
            CrateValue::PathExpression(p) => Value::PathExpression(Arc::from(p.as_str())),
            CrateValue::Specifier(v) => Value::Int(*v as i32),
            CrateValue::Variability(_) | CrateValue::Permission(_) => Value::Null,
            CrateValue::Opaque { value_type, data } => convert_math_value(*value_type, data),
            CrateValue::TypedArray(array) => Value::TypedArray(array.clone()),
            CrateValue::Array(items) => {
                let vals: Vec<Value> = items.iter().map(|v| self.convert_crate_value(v)).collect();
                Value::array(vals)
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
            // A sample of an attribute of a type no type registers, as the
            // text it is (as `DecodedField::UnregisteredText` for a default).
            CrateValue::UnregisteredText(text) => Value::Unregistered(Arc::from(text.as_str())),
            CrateValue::VariantSelectionMap(_) => Value::Null,
            CrateValue::PathVector(paths) => {
                let vals: Vec<Value> = paths
                    .iter()
                    .map(|p| Value::String(Arc::from(p.as_str())))
                    .collect();
                Value::array(vals)
            }
            CrateValue::TokenVector(toks) => {
                let vals: Vec<Value> = toks
                    .iter()
                    .map(|t| Value::Token(self.tokens.intern(t)))
                    .collect();
                Value::array(vals)
            }
            CrateValue::DoubleVector(ds) => {
                let vals: Vec<Value> = ds.iter().map(|d| Value::Double(*d)).collect();
                Value::array(vals)
            }
            CrateValue::StringVector(ss) => {
                let vals: Vec<Value> = ss
                    .iter()
                    .map(|s| Value::String(Arc::from(s.as_str())))
                    .collect();
                Value::array(vals)
            }
            CrateValue::LayerOffsetVector(offsets) => {
                let vals: Vec<Value> = offsets
                    .iter()
                    .map(|(o, s)| Value::Array(alloc::vec![Value::Double(*o), Value::Double(*s)]))
                    .collect();
                Value::array(vals)
            }
            CrateValue::RelocatesMap(_) => Value::Null,
            CrateValue::Spline(..) => {
                // Splines are handled as FieldValue::Spline, not plain values.
                Value::Null
            }
            CrateValue::ArrayEdit(edit) => {
                // OpenUSD crateFile::_ValueHandler::PackArrayEdit keeps T in the
                // value representation even for an identity edit. The Sdf
                // attribute declaration is independent of that actual kind.
                let name = alloc::format!("{}[]", array_edit_element_name(edit.element_type));
                let value_type =
                    PropertyType::parse(&name).unwrap_or_else(PropertyType::unregistered);
                Value::typed_array_edit(self.convert_array_edit(edit), value_type)
            }
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
        spec_path: SpecName<'_>,
        field: &str,
        field_value: &DecodedField<'_>,
    ) -> Result<Option<FieldValue>, UsdcError> {
        if let DecodedField::PathListOp(parts) = field_value {
            return Ok(Some(FieldValue::PathListOp(Box::new(
                self.target_list_op_at(parts),
            ))));
        }
        if let DecodedField::UnregisteredText(text) = field_value {
            let statement = alloc::format!("{field} = {text}");
            return self.unregistered_metadata(spec_path, field, &statement);
        }
        let Some(cv) = field_value.value() else {
            return Ok(Some(FieldValue::Value(
                self.convert_field_value(field_value),
            )));
        };
        Ok(match cv {
            CrateValue::ListOp(listop) => {
                let converted = match listop.op_type {
                    ValueType::TokenListOp => Some(FieldValue::TokenListOp(Box::new(
                        self.convert_token_listop(listop),
                    ))),
                    ValueType::PathListOp => match self.convert_target_listop(listop) {
                        Ok(converted) => Some(FieldValue::PathListOp(Box::new(converted))),
                        Err(error) => {
                            self.report(spec_path, Some(field), alloc::format!("{error}"))?;
                            return Ok(None);
                        }
                    },
                    ValueType::StringListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::String(s) => Some(Arc::from(s.as_str())),
                        _ => None,
                    })
                    .map(FieldValue::from),
                    ValueType::IntListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::Int(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::from),
                    ValueType::UIntListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::UInt(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::from),
                    ValueType::Int64ListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::Int64(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::from),
                    ValueType::UInt64ListOp => convert_scalar_listop(listop, |v| match v {
                        CrateValue::UInt64(v) => Some(*v),
                        _ => None,
                    })
                    .map(FieldValue::from),
                    ValueType::UnregisteredValueListOp => {
                        let Some(statements) = unregistered_list_op_text(field, listop) else {
                            self.report(
                                spec_path,
                                Some(field),
                                "unregistered list op holds a value that is not text",
                            )?;
                            return Ok(None);
                        };
                        return self.unregistered_metadata(spec_path, field, &statements);
                    }
                    // Reference and payload list ops are arcs, read by their
                    // dedicated fields.
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
            | CrateValue::Spline(..)
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

    /// Reads the metadata `field` from `statements`, the USDA text a crate
    /// holds for metadata no schema registers, as reading OpenUSD's text of
    /// the crate file would ([`layerstack_usda::emit::metadata_from_text`]);
    /// `None`, after a diagnostic, when the text does not read.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields).
    fn unregistered_metadata(
        &mut self,
        spec_path: SpecName<'_>,
        field: &str,
        statements: &str,
    ) -> Result<Option<FieldValue>, UsdcError> {
        let read =
            layerstack_usda::emit::metadata_from_text(field, statements, self.tokens, self.paths);
        if read.is_none() {
            self.report(
                spec_path,
                Some(field),
                alloc::format!("unregistered metadata `{statements}` does not read as USDA"),
            )?;
        }
        Ok(read)
    }

    /// Fills `spec`, a new attribute [`PropertySpec`], from its crate fields, keeping
    /// every authored value slot.
    ///
    /// Spec: AOUSD Core §7.6.4 (attribute spec fields; §7.6.4.2.3: an
    /// attribute may have a value, a connection, or both).
    fn build_attribute_spec(
        &mut self,
        spec_path: SpecName<'_>,
        fields: &[(&str, DecodedField<'_>)],
        spec: &mut PropertySpec,
    ) -> Result<(), UsdcError> {
        spec.type_name = Some(self.attribute_property_type(fields));
        for (name, value) in fields {
            match (*name, value.value()) {
                ("default", _) => spec.default = Some(self.convert_field_value(value)),
                ("timeSamples", Some(CrateValue::TimeSamples(samples))) => {
                    let samples: Vec<_> = samples
                        .iter()
                        .map(|(tc, v)| (*tc, self.convert_crate_value(v)))
                        .collect();
                    spec.time_samples = samples.into();
                }
                ("spline", Some(CrateValue::Spline(spline, dictionaries))) => {
                    let mut spline = spline.clone();
                    for (time, entries) in dictionaries {
                        if let Some(knot) = spline.knots.iter_mut().find(|k| k.time == *time) {
                            knot.custom_data = entries
                                .iter()
                                .map(|(key, value)| {
                                    (Arc::from(key.as_str()), self.convert_crate_value(value))
                                })
                                .collect();
                        } else {
                            return Err(UsdcError::Inconsistent {
                                message: "custom data for a missing spline knot",
                            });
                        }
                    }
                    spec.spline = Some(Box::new(spline));
                }
                ("connectionPaths", _) => {
                    spec.targets = Some(Box::new(self.convert_connection_value(value)?));
                }
                // Read by `attribute_property_type`; `connectionChildren`
                // restates `connectionPaths`.
                ("typeName" | "connectionChildren", _) => {}
                _ => self.apply_property_field(spec_path, name, value, spec)?,
            }
        }
        if let Some(property_type) = spec.type_name.as_ref() {
            let compact = |value: &mut Value| {
                if let Value::Array(items) = value {
                    // Asset path arrays keep their representation; skip the copy.
                    if matches!(items.first(), Some(Value::Asset(_))) {
                        return;
                    }
                    let items = core::mem::take(items);
                    *value = Value::array_with_element(items, Some(property_type.default_scalar()));
                }
            };
            if let Some(value) = spec.default.as_mut() {
                compact(value);
            }
            if let Some(samples) = spec.time_samples.get_mut() {
                for (_, value) in samples {
                    compact(value);
                }
            }
        }
        Ok(())
    }

    /// Applies a field shared by attribute and relationship specs: the
    /// qualifiers, or a metadata entry.
    ///
    /// Spec: AOUSD Core §7.6.3 (property spec fields), §7.6.4.1.2
    /// (`variability`).
    fn apply_property_field(
        &mut self,
        spec_path: SpecName<'_>,
        name: &str,
        value: &DecodedField<'_>,
        spec: &mut PropertySpec,
    ) -> Result<(), UsdcError> {
        match (name, value.value()) {
            ("custom", Some(CrateValue::Bool(custom))) => spec.custom = *custom,
            ("variability", Some(CrateValue::Variability(variability))) => {
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
                    set_field_vec(spec.metadata.make_mut(), key, field_value);
                }
            }
        }
        Ok(())
    }

    /// Fills `spec`, a new relationship [`PropertySpec`], from its crate fields.
    ///
    /// Spec: AOUSD Core §7.6.5 (relationship spec fields).
    fn build_relationship_spec(
        &mut self,
        spec_path: SpecName<'_>,
        fields: &[(&str, DecodedField<'_>)],
        spec: &mut PropertySpec,
    ) -> Result<(), UsdcError> {
        for (name, value) in fields {
            match *name {
                "targetPaths" => {
                    spec.targets = Some(Box::new(self.convert_connection_value(value)?));
                }
                // `targetChildren` restates `targetPaths`.
                "targetChildren" => {}
                _ => self.apply_property_field(spec_path, name, value, spec)?,
            }
        }
        Ok(())
    }

    /// The declared type of an attribute spec with `fields`: its
    /// `typeName`, made an array type by an array value. Many attributes
    /// declare one type, so each is parsed once per read and shared.
    fn attribute_property_type(&mut self, fields: &[(&str, DecodedField<'_>)]) -> PropertyType {
        let type_name = fields.iter().find_map(|(name, value)| match *value {
            DecodedField::Text(TextKind::Token, index) if *name == "typeName" => Some(index),
            _ => None,
        });
        let inferred_array = fields.iter().any(|(name, value)| {
            matches!(*name, "default" | "timeSamples")
                && !matches!(
                    value,
                    DecodedField::Text(..) | DecodedField::UnregisteredText(_)
                )
                && value.value().is_none_or(crate_value_is_array)
        });
        let key = (type_name.unwrap_or(u32::MAX), inferred_array);
        if let Some(found) = self.property_types.get(&key) {
            return found.clone();
        }
        let mut type_name = String::from(type_name.map_or("", |index| self.lookup_token(index)));
        // `typeName` spells arrays with `[]` (`point3f[]`); an array value
        // makes the declaration an array too, as USDA ingestion does. A name
        // the value type table lacks is kept, as OpenUSD keeps it.
        if inferred_array && !type_name.ends_with("[]") {
            type_name.push_str("[]");
        }
        let found = PropertyType::parse(&type_name).unwrap_or_else(PropertyType::unregistered);
        self.property_types.insert(key, found.clone());
        found
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
        result.add = listop
            .added_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();
        result.reorder = listop
            .ordered_items
            .iter()
            .filter_map(|v| self.convert_crate_to_reference(v))
            .collect();

        Ok(result)
    }

    /// Converts a USDC path list op to a layerstack `ListOp<PathId>`.
    fn convert_path_listop(&mut self, listop: &CrateListOp) -> Result<ListOp<PathId>, UsdcError> {
        let mut result = ListOp::default();

        let convert_items =
            |items: &[CrateValue], tokens: &TokenInterner, paths: &PathInterner| -> Vec<PathId> {
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
        result.add = convert_items(&listop.added_items, self.tokens, self.paths);
        result.reorder = convert_items(&listop.ordered_items, self.tokens, self.paths);

        Ok(result)
    }

    /// Converts a USDC token list op to a layerstack `ListOp<TokenId>`.
    fn convert_token_listop(&mut self, listop: &CrateListOp) -> ListOp<TokenId> {
        let mut result = ListOp::default();

        let convert_items = |items: &[CrateValue], tokens: &TokenInterner| -> Vec<TokenId> {
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
        result.add = convert_items(&listop.added_items, self.tokens);
        result.reorder = convert_items(&listop.ordered_items, self.tokens);

        result
    }

    /// Converts a connection or target paths value to `ListOp<TargetPath>`.
    fn convert_connection_value(
        &mut self,
        field: &DecodedField<'_>,
    ) -> Result<ListOp<TargetPath>, UsdcError> {
        if let DecodedField::PathListOp(parts) = field {
            return Ok(self.target_list_op_at(parts));
        }
        let Some(value) = field.value() else {
            return Ok(ListOp::default());
        };
        match value {
            CrateValue::ListOp(listop) => self.convert_target_listop(listop),
            CrateValue::PathVector(paths) => {
                let target_paths: Vec<TargetPath> =
                    paths.iter().filter_map(|s| self.target_path(s)).collect();
                Ok(ListOp::explicit(target_paths))
            }
            _ => Ok(ListOp::default()),
        }
    }

    fn convert_target_listop(
        &mut self,
        listop: &CrateListOp,
    ) -> Result<ListOp<TargetPath>, UsdcError> {
        let mut result = ListOp::default();

        let mut convert_items = |items: &[CrateValue]| -> Vec<TargetPath> {
            items
                .iter()
                .filter_map(|v| {
                    if let CrateValue::String(s) = v {
                        self.target_path(s)
                    } else {
                        None
                    }
                })
                .collect()
        };

        if let Some(items) = &listop.explicit_items {
            result.explicit = Some(convert_items(items));
        }
        result.prepend = convert_items(&listop.prepended_items);
        result.append = convert_items(&listop.appended_items);
        result.delete = convert_items(&listop.deleted_items);
        result.add = convert_items(&listop.added_items);
        result.reorder = convert_items(&listop.ordered_items);

        Ok(result)
    }

    /// [`Self::convert_target_listop`] for a list op of path table indices.
    fn target_list_op_at(&mut self, parts: &ListOpParts<Items<u32>>) -> ListOp<TargetPath> {
        let mut convert = |items: &[u32]| -> Vec<TargetPath> {
            items
                .iter()
                .filter_map(|&index| self.target_path_at(index))
                .collect()
        };
        let mut result = ListOp::default();
        if let Some(items) = &parts.explicit {
            result.explicit = Some(convert(items));
        }
        result.prepend = convert(&parts.prepended);
        result.append = convert(&parts.appended);
        result.delete = convert(&parts.deleted);
        result.add = convert(&parts.added);
        result.reorder = convert(&parts.ordered);
        result
    }

    /// The target path path table entry `index` names, as its text parses
    /// ([`TargetPath::parse`]), found from the path tree: a prim path's id
    /// ([`Self::prim_path_at`]), or a property's prim and name
    /// ([`Self::property_owner`]), each found once per entry. An entry the tree cannot answer for
    /// certain (a text holding `{`, or a `.` in a prim name) is parsed.
    fn target_path_at(&mut self, index: u32) -> Option<TargetPath> {
        let sections = self.sections;
        let at = index as usize;
        if at >= sections.paths.len() {
            return self.target_path("");
        }
        if !sections.paths.has_tree() {
            return self.target_path(self.path_text(index));
        }
        let spelling = |at: usize| sections.paths.spelling(at).unwrap_or_default();
        match sections.paths.nodes()[at] {
            PathNode::Child { property: true, .. } => match self.property_owner(index) {
                Some(PropertyOwner::Prim { owner, name }) => {
                    // A prim inside a variant branch is no target's prim.
                    if spelling(owner as usize).selection() {
                        None
                    } else {
                        self.prim_path_at(owner)
                            .map(|prim| TargetPath::Property(PropertyPath::new(prim, name)))
                    }
                }
                Some(PropertyOwner::Variant | PropertyOwner::Invalid) => None,
                None => self.target_path(self.path_text(index)),
            },
            PathNode::Root | PathNode::Child { .. }
                if !spelling(at).dot() && !spelling(at).selection() =>
            {
                self.prim_path_at(index).map(TargetPath::Prim)
            }
            PathNode::Root | PathNode::Child { .. } | PathNode::Unused => {
                self.target_path(self.path_text(index))
            }
        }
    }

    /// Parses a target path's text ([`TargetPath::parse`]) once per read.
    fn target_path(&mut self, text: &str) -> Option<TargetPath> {
        if let Some(parsed) = self.target_paths.get(text) {
            return *parsed;
        }
        let parsed = TargetPath::parse(text, self.tokens, self.paths).ok();
        self.target_paths.insert(String::from(text), parsed);
        parsed
    }

    /// Converts a `payload` field value to a list op.
    ///
    /// The field holds a payload list op from crate 0.8. Earlier files hold
    /// a single `SdfPayload`, which is an explicit list of that payload, or
    /// no payload when empty (`pxr/usd/sdf/crateFile.cpp:393`); OpenUSD
    /// still writes one payload with an identity layer offset that way while
    /// the file needs no later version.
    ///
    /// A single payload with an empty asset path is an explicitly empty list,
    /// not an internal payload: internal payloads were introduced with
    /// payload list ops (`_ToPayloadListOpValue` in
    /// `pxr/usd/sdf/crateData.cpp`).
    ///
    /// Spec: AOUSD Core §7.6.2.3.2 (`payload`).
    fn convert_payload_value(
        &mut self,
        field: &DecodedField<'_>,
    ) -> Result<Option<ListOp<Reference>>, UsdcError> {
        let enclosing = core::mem::replace(&mut self.in_payloads, true);
        let converted = self.convert_payload_field(field);
        self.in_payloads = enclosing;
        converted
    }

    /// [`Self::convert_payload_value`], once the arcs read are marked
    /// payloads.
    fn convert_payload_field(
        &mut self,
        field: &DecodedField<'_>,
    ) -> Result<Option<ListOp<Reference>>, UsdcError> {
        let Some(value) = field.value() else {
            return Ok(None);
        };
        match value {
            CrateValue::ListOp(listop) => self.convert_ref_listop(listop).map(Some),
            CrateValue::Dictionary(entries) => {
                let has_asset = entries.iter().any(|(key, value)| {
                    key == "assetPath"
                        && matches!(value, CrateValue::AssetPath(asset) if !asset.is_empty())
                });
                let explicit = if has_asset {
                    self.convert_crate_to_reference(value).into_iter().collect()
                } else {
                    Vec::new()
                };
                Ok(Some(ListOp::explicit(explicit)))
            }
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
            let mut custom_data = Vec::new();

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
                    "customData" => {
                        if let Value::Dictionary(entries) = self.convert_crate_value(val) {
                            custom_data = entries;
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

            // An empty prim path targets the `defaultPrim`; a malformed one
            // is dropped like USDA's, never retargeted.
            let target = if prim_path.is_empty() {
                ReferenceTarget::DefaultPrim
            } else {
                let path = Path::parse_absolute(&prim_path, self.tokens).ok()?;
                ReferenceTarget::Prim(self.paths.intern(path))
            };
            let layer_offset = LayerOffset {
                offset: layer_offset_val,
                scale: layer_scale_val,
            };

            // With neither an asset path nor a prim path, the arc is internal
            // and targets this layer's `defaultPrim` (AOUSD Core §10.3.2.1),
            // as `<>` does in USDA. An asset path that cannot be resolved
            // keeps the arc unresolved; it never becomes an internal arc.
            if asset_path.is_empty() {
                let internal = ReferenceSource::Layer {
                    layer: self.layer_id,
                    asset: None,
                };
                return Some(
                    Reference::from_source(internal, target)
                        .with_layer_offset(layer_offset)
                        .with_custom_data(custom_data),
                );
            }
            // An asset path that is a variable expression is evaluated
            // during composition, relative to this layer.
            if is_expression(&asset_path) {
                return Some(
                    Reference::expression(self.layer_id, asset_path, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            }
            // A payload to a dynamic file format targets the layer its
            // composed arguments name, which composition asks for
            // (`AssetResolver::defers_payload`). An arc inside a variant
            // branch may never be composed; resolving the branches the
            // layer selects is left until the whole layer is read
            // (`resolve_selected_deferred_arcs`), the rest to composition.
            if (self.in_branch && self.defer_variant_arcs)
                || (self.in_payloads && self.defers_payload(&asset_path))
            {
                self.deferred_arcs = true;
                return Some(
                    Reference::deferred(self.layer_id, asset_path, target, layer_offset)
                        .with_custom_data(custom_data),
                );
            }
            let resolved = match self.resolve_arc_asset(&asset_path) {
                Ok(resolved) => resolved,
                // Not supplied yet: composition asks for it again.
                Err(AssetResolveError::Unavailable) => {
                    return Some(
                        Reference::deferred(self.layer_id, asset_path, target, layer_offset)
                            .with_custom_data(custom_data),
                    );
                }
                Err(_) => {
                    return Some(
                        Reference::unresolved(asset_path, target, layer_offset)
                            .with_custom_data(custom_data),
                    );
                }
            };
            if let Some(layer) = resolved.layer {
                self.resolved_layers.push(layer);
            }
            // The asset path moves into the arc, not copied.
            let arc = match target {
                ReferenceTarget::Prim(prim) => {
                    Reference::with_asset(resolved.layer_id, prim, asset_path)
                }
                ReferenceTarget::DefaultPrim => {
                    Reference::with_asset_default_prim(resolved.layer_id, asset_path)
                }
            };
            Some(
                arc.with_layer_offset(layer_offset)
                    .with_custom_data(custom_data),
            )
        } else {
            None
        }
    }

    // ── Token/path helpers ──────────────────────────────────────────

    /// Returns the names a token or string list op adds, in the order it
    /// gives them over no names: its explicit, prepended, added and
    /// appended items.
    /// The `variantSetNames` list op, as authored: a string list op, or
    /// for a name vector, an explicit list of its names.
    ///
    /// Spec: AOUSD Core §7.6.2.3.5 (`variantSetNames`), §12.4 (list ops).
    fn variant_set_names(&mut self, field: &DecodedField<'_>) -> ListOp<TokenId> {
        match field.value() {
            Some(CrateValue::ListOp(listop)) => self.convert_token_listop(listop),
            Some(_) => ListOp::explicit(self.extract_token_names(field)),
            None if matches!(field, DecodedField::Text(..)) => ListOp::explicit(Vec::new()),
            None if matches!(
                field,
                DecodedField::TokenArray(_) | DecodedField::StringArray(_)
            ) =>
            {
                ListOp::explicit(self.extract_token_names(field))
            }
            None => ListOp::default(),
        }
    }

    /// Extracts token names from a [`CrateValue`] (typically a `TokenVector`
    /// or `Array` of tokens).
    fn extract_token_names(&mut self, field: &DecodedField<'_>) -> Vec<TokenId> {
        if let DecodedField::TokenArray(indices) | DecodedField::StringArray(indices) = field {
            return indices.iter().map(|&index| self.token_id(index)).collect();
        }
        let Some(value) = field.value() else {
            return Vec::new();
        };
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
    /// Splits the property path text `path` into its prim path text and
    /// property name when it is a valid property path
    /// ([`PropertyPath::parse`]), checking each prim path text once
    /// ([`Self::valid_prim_texts`]).
    fn split_property_path(&mut self, path: &'a str) -> Option<(&'a str, &'a str)> {
        let (prim, name) = PropertyPath::split(path).ok()?;
        let tokens = self.tokens;
        let ok = *self
            .valid_prim_texts
            .entry(prim)
            .or_insert_with(|| Path::parse_absolute(prim, tokens).is_ok());
        ok.then_some((prim, name))
    }

    /// The prim path path table entry `index` names outside variant
    /// branches: the path its text parses as ([`Path::parse_absolute`]),
    /// `None` when its text holds a variant selection or does not parse.
    ///
    /// Found once per entry from its parent's and its element, as the
    /// text was built, rather than by parsing the text: the text is read
    /// only for an element that would not join its parent's segments as
    /// one segment (empty, or holding a `/`), a property's entry, an
    /// entry beneath one that names no prim path, or a table without its
    /// tree.
    fn prim_path_at(&mut self, index: u32) -> Option<PathId> {
        let sections = self.sections;
        let tables = self.tables;
        let nodes = sections.paths.nodes();
        if !sections.paths.has_tree() {
            return self.prim_path_of_text(index as usize);
        }
        let known = |at: usize| tables.prim_paths.get(at).map(OnceSlot::get);
        if let Some(Some(&found)) = known(index as usize) {
            return found;
        }
        // The entries up to the nearest found ancestor, nearest last.
        let mut pending = core::mem::take(&mut self.pending_paths);
        pending.clear();
        let mut at = index as usize;
        while matches!(known(at), Some(None)) {
            pending.push(at);
            match nodes[at] {
                PathNode::Child { parent, .. } => at = parent as usize,
                PathNode::Root | PathNode::Unused => break,
            }
        }
        for &at in pending.iter().rev() {
            let found = match nodes[at] {
                PathNode::Root => Some(self.paths.intern(Path::root())),
                PathNode::Unused => self.prim_path_of_text(at),
                // A property's text may still parse as a prim path (`/A.b`).
                PathNode::Child { property: true, .. } => self.prim_path_of_text(at),
                PathNode::Child { parent, token, .. } => {
                    let element = self.lookup_token(token);
                    let parent = known(parent as usize).flatten().copied();
                    match parent {
                        _ if element.contains('{') => None,
                        Some(Some(parent)) if !element.is_empty() && !element.contains('/') => {
                            let name = self.token_id(token);
                            Some(self.paths.intern_child(parent, name))
                        }
                        _ => self.prim_path_of_text(at),
                    }
                }
            };
            let _ = tables.prim_paths[at].set(found);
        }
        self.pending_paths = pending;
        known(index as usize).flatten().copied().flatten()
    }

    /// [`Self::prim_path_at`] from the text of path table entry `at`.
    fn prim_path_of_text(&self, at: usize) -> Option<PathId> {
        let text = self.sections.path(at)?;
        if text.contains('{') {
            return None;
        }
        let path = Path::parse_absolute(text, self.tokens).ok()?;
        Some(self.paths.intern(path))
    }

    /// The prim spec path entry and name of the property path path table
    /// entry `index` names, read from the path tree as
    /// [`PropertyPath::split`] and [`Path::parse_absolute`] read its text;
    /// `None` when the tree cannot say so for certain (no tree, or a
    /// property element holding a separator), for the text to say.
    fn property_owner(&mut self, index: u32) -> Option<PropertyOwner> {
        let sections = self.sections;
        if !sections.paths.has_tree() {
            return None;
        }
        let PathNode::Child {
            parent,
            token,
            property: true,
        } = *sections.paths.nodes().get(index as usize)?
        else {
            return None;
        };
        // Each name and each owner is checked once.
        let tables = self.tables;
        let named = found_once(tables.property_names.get(token as usize)?, || {
            let name = &sections.tokens[token as usize];
            !name.is_empty() && !name.contains(['.', '/', '{', '}'])
        });
        if !named {
            return None;
        }
        // Found from the parent's spelling and prim path, each found once.
        let spelling = sections.paths.spelling(parent as usize)?;
        let found = if spelling.selection() {
            // A property directly on a variant (`/A{v=x}.b`) belongs to the
            // variant spec, and is placed with the variant specs; one on a
            // prim inside a branch (`/A{v=x}B.c`) is placed here.
            if spelling.ends_with_close() {
                Owner::Variant
            } else {
                Owner::Prim
            }
        } else if self.is_property_owner_at(parent) {
            Owner::Prim
        } else {
            Owner::Invalid
        };
        Some(match found {
            Owner::Variant => PropertyOwner::Variant,
            Owner::Invalid => PropertyOwner::Invalid,
            Owner::Prim => PropertyOwner::Prim {
                owner: parent,
                name: self.token_id(token),
            },
        })
    }

    /// [`is_property_owner`] of the text of path table entry `index`,
    /// holding no `{`: a path the entry names as a prim path holds no
    /// empty name and starts with `/`, so its text is a property's prim
    /// path when it holds no `.` or `}` and is not the root's; the text is
    /// read only for an entry naming no prim path.
    fn is_property_owner_at(&mut self, index: u32) -> bool {
        let spelling = self
            .sections
            .paths
            .spelling(index as usize)
            .unwrap_or_default();
        if self.prim_path_at(index).is_some() {
            return !spelling.dot() && !spelling.close() && !spelling.root();
        }
        is_property_owner(self.path_text(index))
    }

    /// Charges a look at the path at path table entry `index` by its
    /// text's length, as [`Self::lookup_path`] does, without the text.
    fn charge_path(&mut self, index: u32) -> Result<(), UsdcError> {
        let len = self
            .sections
            .paths
            .text_len(index as usize)
            .ok_or(UsdcError::Inconsistent {
                message: "path index out of range",
            })?;
        self.budget.charge_text(len)
    }

    /// The text of the path at path table entry `index`, spelled when
    /// first asked; empty outside the table.
    fn path_text(&self, index: u32) -> &'a str {
        let sections = self.sections;
        sections.path(index as usize).unwrap_or_default()
    }

    /// Whether the spec `name` names lies inside a variant branch: whether
    /// its path's text holds a `{`.
    fn in_branch_of(&self, name: SpecName<'_>) -> bool {
        match name {
            SpecName::Text(text) => text.contains('{'),
            SpecName::Entry(index) => self
                .sections
                .paths
                .spelling(index as usize)
                .is_some_and(|spelling| spelling.selection()),
        }
    }

    /// The key of the prim spec at path table entry `index`: the path the
    /// tree names ([`Self::prim_path_at`]) for an entry whose text holds no
    /// `{` or `}`, which the tree's segments spell as the text does;
    /// [`Self::prim_key_of_text`] of its text otherwise.
    fn prim_key_at(&mut self, index: u32) -> PrimKey<'a> {
        let plain = self
            .sections
            .paths
            .spelling(index as usize)
            .is_some_and(|spelling| !spelling.selection() && !spelling.close());
        if plain && let Some(path) = self.prim_path_at(index) {
            return PrimKey::Path(path);
        }
        self.prim_key_of_text(self.path_text(index))
    }

    /// The key of a prim spec at the path `text`: the path it parses as
    /// outside variant branches ([`Path::parse_absolute`]), otherwise the
    /// text itself.
    fn prim_key_of_text(&self, text: &'a str) -> PrimKey<'a> {
        if !text.contains('{')
            && let Ok(path) = Path::parse_absolute(text, self.tokens)
        {
            return PrimKey::Path(self.paths.intern(path));
        }
        PrimKey::Text(text)
    }

    fn lookup_path(&mut self, index: u32) -> Result<&'a str, UsdcError> {
        let sections = self.sections;
        let path = sections
            .path(index as usize)
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
        sections.tokens.get(index as usize).unwrap_or_default()
    }

    /// Resolves an asset path.
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
        let Some(resolver) = self.resolver.as_deref_mut() else {
            self.wants_resolver = true;
            return Err(AssetResolveError::NotFound);
        };
        resolver.resolve(asset_path, Some(self.layer_id), self.tokens, self.paths)
    }

    /// Whether the resolver leaves a payload to `asset_path` for
    /// composition to resolve ([`AssetResolver::defers_payload`]).
    fn defers_payload(&mut self, asset_path: &str) -> bool {
        let Some(resolver) = self.resolver.as_deref_mut() else {
            self.wants_resolver = true;
            return false;
        };
        resolver.defers_payload(asset_path)
    }
}

// ---------------------------------------------------------------------------
// Path parsing helpers
// ---------------------------------------------------------------------------

/// Places the prim spec `item` among `prims`, and its authored property
/// order among `orders`, by its place.
fn place_prim<'a>(
    prims: &mut PrimSpecs<'a>,
    orders: &mut Vec<Option<Order<'a>>>,
    item: PrimItem<'a>,
) {
    let at = prims.insert(item.index, item.key, item.prim);
    if at == orders.len() {
        orders.push(None);
    }
    if let Some(children) = item.children {
        orders[at] = Some(children);
    }
}

/// Adds the property spec `item` to its prim spec among `prims`.
fn place_property(prims: &mut PrimSpecs<'_>, item: PropertyItem) {
    let prim = prims.spec_mut(item.place).expect("a placed spec");
    put_property(&mut prim.properties, item.name, item.property);
}

/// A copy of `spec`, a spec built for a fieldset specs share, cloning only
/// what it authors: most of a prim spec's lists and maps are empty, and
/// cloning each costs as much as building the spec.
fn copy_prim(spec: &PrimSpec) -> PrimSpec {
    fn list<T: Clone>(list: &ListOp<T>) -> ListOp<T> {
        if list.is_empty() {
            ListOp::new()
        } else {
            list.clone()
        }
    }
    fn map<K: Clone, V: Clone>(map: &HashMap<K, V>) -> HashMap<K, V> {
        if map.is_empty() {
            HashMap::new()
        } else {
            map.clone()
        }
    }
    fn vec<T: Clone>(vec: &[T]) -> Vec<T> {
        vec.to_vec()
    }
    let mut copy = PrimSpec::default();
    copy.specifier = spec.specifier;
    copy.type_name = spec.type_name;
    copy.fields = vec(&spec.fields);
    copy.properties = vec(&spec.properties);
    copy.authored_children = vec(&spec.authored_children);
    copy.references = list(&spec.references).into();
    copy.payloads = list(&spec.payloads).into();
    copy.instanceable = spec.instanceable;
    copy.active = spec.active;
    if let Some(rare) = spec.rare() {
        *copy.rare_mut() = RarePrimFields {
            inherits: list(&rare.inherits).into(),
            specializes: list(&rare.specializes).into(),
            variant_selections: map(&rare.variant_selections),
            variant_sets: rare.variant_sets.clone(),
            variant_set_names: list(&rare.variant_set_names).into(),
            property_order: rare.property_order.clone(),
            prim_order: rare.prim_order.clone(),
            outer_variant_sites: vec(&rare.outer_variant_sites),
        };
    }
    copy
}

/// Declares the variant sets a spec's `variantSetChildren` names, in its
/// order, before their own specs fill them: OpenUSD keeps a spec's set
/// specs in that order (`SdfPrimSpec::GetVariantSets`), the order its text
/// writer writes them in, while the crate's specs come in an order of
/// their own.
///
/// Spec: AOUSD Core §7.3.6 (prim and variant specs contain variant set
/// specs).
fn declare_variant_sets(sets: &mut VariantSets, names: &[TokenId]) {
    for &name in names {
        sets.get_or_insert_default(name);
    }
}

/// The USDA statements OpenUSD writes for the unregistered list op `field`
/// (`Sdf_FileIOUtility::WriteListOp`, `pxr/usd/sdf/fileIO_Common.cpp`):
/// the explicit items, or one statement per edit, each list in brackets
/// with its items' text as it is, `None` for no items. Its text parser kept
/// a list's text, brackets stripped, as one item (`textParserHelpers.cpp`).
/// `None` when an item is not text.
fn unregistered_list_op_text(field: &str, listop: &CrateListOp) -> Option<String> {
    use core::fmt::Write as _;
    let list = |items: &[CrateValue]| -> Option<String> {
        if items.is_empty() {
            return Some(String::from("None"));
        }
        let mut list = String::from("[");
        for (i, item) in items.iter().enumerate() {
            let CrateValue::String(text) = item else {
                return None;
            };
            if i > 0 {
                list.push_str(", ");
            }
            list.push_str(text);
        }
        list.push(']');
        Some(list)
    };
    let mut out = String::new();
    if let Some(items) = &listop.explicit_items {
        let _ = writeln!(out, "{field} = {}", list(items)?);
        return Some(out);
    }
    for (op, items) in [
        ("delete", &listop.deleted_items),
        ("add", &listop.added_items),
        ("prepend", &listop.prepended_items),
        ("append", &listop.appended_items),
        ("reorder", &listop.ordered_items),
    ] {
        if !items.is_empty() {
            let _ = writeln!(out, "{op} {field} = {}", list(items)?);
        }
    }
    Some(out)
}

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

/// Strips the variant selections ending a crate path, returning the path of
/// the prim spec they belong to: `/A{v=x}{w=y}` → `/A`, `/A{v=x}B` →
/// `/A{v=x}B`.
///
/// Returns `None` for a malformed path or one naming no prim.
fn owning_prim_path(path: &str) -> Option<&str> {
    let mut rest = path;
    while let Some(body) = rest.strip_suffix('}') {
        rest = &body[..body.rfind('{')?];
    }
    (rest.len() > 1 && rest.starts_with('/') && !rest.ends_with('/')).then_some(rest)
}

/// Splits the crate path of a variant set spec or a variant spec into the
/// path of the prim spec owning it (see [`owning_prim_path`]) and the
/// selections ending it, outermost first: `/A{v=x}{w=y}` →
/// `("/A", [("v", "x"), ("w", "y")])`, and `/A{v=x}{w=}` →
/// `("/A", [("v", "x"), ("w", "")])`.
///
/// Returns `None` for a malformed path or one ending in no selection.
fn split_trailing_selections(path: &str) -> Option<(&str, Vec<(&str, &str)>)> {
    let mut rest = path;
    let mut chain = Vec::new();
    while let Some(body) = rest.strip_suffix('}') {
        let open = body.rfind('{')?;
        let (set, variant) = body[open + 1..].split_once('=')?;
        if set.is_empty() {
            return None;
        }
        chain.push((set, variant));
        rest = &body[..open];
    }
    chain.reverse();
    let owner = owning_prim_path(rest)?;
    (owner == rest && !chain.is_empty()).then_some((owner, chain))
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
        add: items(&listop.added_items)?,
        reorder: items(&listop.ordered_items)?,
    })
}

/// Whether `text`, holding no variant selection, is the prim path text of a
/// property path that [`PropertyPath::split`] and [`Path::parse_absolute`]
/// accept: absolute, below the root, with no empty name, and no `.` or `}`
/// [`PropertyPath::split`] would read as part of a property.
fn is_property_owner(text: &str) -> bool {
    let Some((b'/', names)) = text.as_bytes().split_first() else {
        return false;
    };
    let mut previous = b'/';
    for &byte in names {
        if matches!(byte, b'.' | b'}') || (byte == b'/' && previous == b'/') {
            return false;
        }
        previous = byte;
    }
    previous != b'/'
}

/// Sets the property `name` of a spec's `properties` to `spec`, keeping the
/// place of one already there, as [`set_property_vec`] does.
fn put_property(properties: &mut Vec<PropertyEntry>, name: TokenId, spec: Arc<PropertySpec>) {
    match properties.iter_mut().find(|entry| entry.name == name) {
        Some(entry) => entry.spec = spec,
        None => properties.push(PropertyEntry { name, spec }),
    }
}

/// Sorts diagnostics by the spec they are about, stably. Outside the
/// generic pass, so the prim and property passes share one sort.
fn sort_by_spec(diagnostics: &mut [(usize, AssembleDiagnostic)]) {
    diagnostics.sort_by_key(|(spec, _)| *spec);
}

/// Orders `properties` by their position in `children` (`propertyChildren`),
/// keeping properties not listed there after the listed ones, in order.
///
/// Positions are looked up in a map: both lists come from the file, so
/// searching `children` for each property would take quadratic time.
fn sort_by_children(properties: &mut [PropertyEntry], children: &[TokenId]) {
    // A short list is searched faster than it is hashed, and most prims
    // list their properties in the order their specs came in. Both
    // lookups go through one closure, so one sort serves them.
    let positions = if children.len() <= 16 {
        Positions::Listed(children)
    } else {
        let mut position: HashMap<TokenId, usize> = HashMap::new();
        for (i, child) in children.iter().enumerate() {
            position.entry(*child).or_insert(i);
        }
        Positions::Indexed(position)
    };
    let place = |entry: &PropertyEntry| positions.of(entry.name);
    if !properties.is_sorted_by_key(place) {
        properties.sort_by_key(place);
    }
}

/// Where each name is in a prim's `propertyChildren`.
enum Positions<'a> {
    /// A short list, searched.
    Listed(&'a [TokenId]),
    /// A long list's first positions, by name.
    Indexed(HashMap<TokenId, usize>),
}

impl Positions<'_> {
    /// The position of `name`, `usize::MAX` for a name not listed.
    fn of(&self, name: TokenId) -> usize {
        match self {
            Self::Listed(children) => children
                .iter()
                .position(|child| *child == name)
                .unwrap_or(usize::MAX),
            Self::Indexed(position) => position.get(&name).copied().unwrap_or(usize::MAX),
        }
    }
}

/// Whether `rep` is a `token[]` value, which assembly decodes to token-table
/// indices ([`decode_text_indices`]).
fn is_token_array(rep: &RawValueRep) -> bool {
    rep.is_array() && !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::Token)
}

/// Whether `rep` is a `string[]` value, which assembly decodes to the
/// token-table indices of its text ([`decode_text_indices`]).
fn is_string_array(rep: &RawValueRep) -> bool {
    rep.is_array() && !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::String)
}

/// Whether `rep` is a token vector, which decodes as one whatever its
/// array flag says.
fn is_token_vector(rep: &RawValueRep) -> bool {
    !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::TokenVector)
}

/// Whether `rep` is a path list op, which decodes as one whatever its array
/// flag says.
fn is_path_list_op(rep: &RawValueRep) -> bool {
    !rep.is_array_edit() && rep.value_type().ok() == Some(ValueType::PathListOp)
}

fn crate_value_is_array(value: &CrateValue) -> bool {
    match value {
        CrateValue::Array(_) | CrateValue::TypedArray(_) | CrateValue::ArrayEdit(_) => true,
        CrateValue::TimeSamples(samples) => samples
            .iter()
            .any(|(_, sample)| crate_value_is_array(sample)),
        _ => false,
    }
}

fn array_edit_element_name(value_type: ValueType) -> &'static str {
    match value_type {
        ValueType::Bool => "bool",
        ValueType::UChar => "uchar",
        ValueType::Int => "int",
        ValueType::UInt => "uint",
        ValueType::Int64 => "int64",
        ValueType::UInt64 => "uint64",
        ValueType::Half => "half",
        ValueType::Float => "float",
        ValueType::Double => "double",
        ValueType::String => "string",
        ValueType::Token => "token",
        ValueType::AssetPath => "asset",
        ValueType::Matrix2d => "matrix2d",
        ValueType::Matrix3d => "matrix3d",
        ValueType::Matrix4d => "matrix4d",
        ValueType::Quatd => "quatd",
        ValueType::Quatf => "quatf",
        ValueType::Quath => "quath",
        ValueType::Vec2d => "double2",
        ValueType::Vec2f => "float2",
        ValueType::Vec2h => "half2",
        ValueType::Vec2i => "int2",
        ValueType::Vec3d => "double3",
        ValueType::Vec3f => "float3",
        ValueType::Vec3h => "half3",
        ValueType::Vec3i => "int3",
        ValueType::Vec4d => "double4",
        ValueType::Vec4f => "float4",
        ValueType::Vec4h => "half4",
        ValueType::Vec4i => "int4",
        ValueType::TimeCode => "timecode",
        ValueType::Duration => "duration",
        ValueType::PathExpression => "pathExpression",
        // decode_array_edit rejects these before a CrateArrayEdit is built.
        _ => unreachable!("decoded array edit has an array element type"),
    }
}

/// Returns the parent prim path for a path like `/A/B` → `/A`, `/A` → `/`.
fn parent_prim_path(path: &str) -> Option<&str> {
    if path == "/" {
        return None;
    }
    // Don't compute parent for variant paths.
    if path.contains('{') {
        return None;
    }
    if let Some(last_slash) = path.rfind('/') {
        if last_slash == 0 {
            Some("/")
        } else {
            Some(&path[..last_slash])
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

// ---------------------------------------------------------------------------
// ListOp merge helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section::{FieldDef, SpecDef};
    use crate::section::{PathTable, TokenTable};
    use crate::version::CrateVersion;
    use layerstack::AssetResolveError;

    /// A resolver that resolves nothing.
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

    /// Naming paths from the path tree assembles the layer naming them from
    /// their text does: prims in and out of variant branches, properties of
    /// each, a variant's own property, relationship targets and
    /// connections in every list op part, and children listed by name; and
    /// both assemble the layer that was written.
    #[test]
    fn the_path_tree_names_what_the_text_does() {
        let source = r#"#usda 1.0
def "World" {
    def "A" (
        variants = { string look = "x" }
        prepend variantSets = "look"
    ) {
        int count = 1
        rel target = </World/B.size>
        prepend rel others = [</World/A/Child>, </Other>]
        delete rel others = </World/B>
        double linked.connect = </World/B.size>
        variantSet "look" = {
            "x" {
                double shade = 0.5
                def "Inner" { double d = 2; }
            }
            "y" { }
        }
        def "Child" { custom token kind = "leaf"; }
    }
    over "B" { double size = 3; }
}
def "Other" {}
"#;
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
        assert!(
            read.parse_diagnostics.is_empty(),
            "{:?}",
            read.parse_diagnostics
        );
        let bytes = crate::writer::save_layer(&read.emitted.layer, &tokens, &paths).unwrap();
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let sections =
            crate::section::parse_sections(&bytes, &toc, header.crate_version(), &mut budget)
                .unwrap();
        assert!(sections.paths.has_tree());
        let assemble_from = |sections: &CrateSections| {
            let mut budget = DecodeBudget::with_limit(u64::MAX);
            assemble(
                &bytes,
                sections,
                LayerId(1),
                &tokens,
                &paths,
                &mut NoAssets,
                &mut budget,
                &Threads::new(1),
            )
            .unwrap()
        };
        let tree = assemble_from(&sections);
        let mut text = sections.clone();
        text.paths = PathTable::from_texts(sections.path_texts());
        let text = assemble_from(&text);
        assert!(tree.layer == text.layer, "the same layer");
        assert_eq!(tree.diagnostics, text.diagnostics);
        assert!(tree.layer == read.emitted.layer, "the layer written");
    }

    /// An `SdfUnregisteredValue` holding text is the USDA text of the
    /// metadata value, as OpenUSD's text parser recorded it: it reads as
    /// that USDA, not as a string of it, on the layer and on a prim; one
    /// holding a dictionary is the dictionary.
    ///
    /// Spec: AOUSD Core §7.4 (metadata fields).
    #[test]
    fn unregistered_metadata_reads_as_its_usda_text() {
        use crate::writer::{Spec, SpecForm as Form, Value as V, write_crate};
        let text = |text: &str| V::UnregisteredValue(Box::new(V::String(text.into())));
        let bytes = write_crate(&[
            Spec::new("/", Form::PseudoRoot)
                .with_field("primChildren", V::TokenVector(alloc::vec!["A".into()]))
                .with_field("label", text("\"1\""))
                .with_field("scale", text("1.5")),
            Spec::new("/A", Form::Prim)
                .with_field("specifier", V::Specifier(Specifier::Def))
                .with_field("words", text("[\"a\", \"b\"]"))
                .with_field(
                    "info",
                    V::UnregisteredValue(Box::new(V::Dictionary(alloc::vec![(
                        "k".into(),
                        V::Int(1)
                    )]))),
                ),
        ])
        .unwrap();
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = crate::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
        assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
        let layer = &read.layer;
        let value = |value: Value| Some(FieldValue::Value(value));
        assert_eq!(
            layer.metadata(tokens.intern("label")).cloned(),
            value(Value::String("1".into()))
        );
        assert_eq!(
            layer.metadata(tokens.intern("scale")).cloned(),
            value(Value::Double(1.5))
        );
        let a = paths.intern(Path::parse_absolute("/A", &tokens).unwrap());
        let prim = layer.prims().get(&a).unwrap();
        let field = |name: &str| {
            let name = tokens.intern(name);
            prim.fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.value.clone())
        };
        let words = alloc::vec![Value::String("a".into()), Value::String("b".into())];
        assert_eq!(field("words"), value(Value::array(words)));
        assert_eq!(
            field("info"),
            value(Value::Dictionary(alloc::vec![("k".into(), Value::Int(1))]))
        );
    }

    /// A time sample stored as an `SdfUnregisteredValue`, as OpenUSD's text
    /// parser records a sample of a type no type registers, reads as the
    /// text it is, as a default does; it read as a string, which the USDA
    /// writer refuses for such a type. OpenUSD 26.11 reads the sample of
    /// `widget g.timeSamples = { 1: 200 }` from its own crate file as
    /// `Sdf.UnregisteredValue('200')`.
    #[test]
    fn unregistered_time_samples_read_as_text() {
        use crate::writer::{Spec, SpecForm as Form, Value as V, write_crate};
        let text = |text: &str| V::UnregisteredValue(Box::new(V::String(text.into())));
        let bytes = write_crate(&[
            Spec::new("/", Form::PseudoRoot)
                .with_field("primChildren", V::TokenVector(alloc::vec!["P".into()])),
            Spec::new("/P", Form::Prim)
                .with_field("specifier", V::Specifier(Specifier::Def))
                .with_field("propertyChildren", V::TokenVector(alloc::vec!["g".into()])),
            Spec::new("/P.g", Form::Attribute)
                .with_field("typeName", V::Token("widget".into()))
                .with_field("custom", V::Bool(false))
                .with_field(
                    "timeSamples",
                    V::TimeSamples(alloc::vec![(1.0, text("200")), (2.0, text("alpha"))]),
                ),
        ])
        .unwrap();
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = crate::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
        let p = paths.intern(Path::parse_absolute("/P", &tokens).unwrap());
        let prim = read.layer.prims().get(&p).unwrap();
        let g = tokens.intern("g");
        let attribute = prim
            .properties
            .iter()
            .find(|entry| entry.name == g)
            .expect("/P.g");
        assert_eq!(
            attribute.spec.time_samples.get().unwrap(),
            [
                (1.0, Value::Unregistered("200".into())),
                (2.0, Value::Unregistered("alpha".into()))
            ]
        );
    }

    /// Text values convert from the token table's text: strings, tokens,
    /// asset paths and path expressions, as attribute defaults, prim
    /// `typeName`s and metadata, and in a dictionary, through the generic
    /// decoder. Attributes declaring one `typeName` share its type, except
    /// where an array value makes it an array.
    #[test]
    fn text_values_convert_from_the_token_table() {
        use crate::writer::{Spec, SpecForm as Form, Value as V, write_crate};
        let attribute = |path: &str, type_name: &str, value: V| {
            Spec::new(path, Form::Attribute)
                .with_field("typeName", V::Token(type_name.into()))
                .with_field("default", value)
        };
        let bytes = write_crate(&[
            Spec::new("/", Form::PseudoRoot)
                .with_field("primChildren", V::TokenVector(alloc::vec!["A".into()]))
                .with_field("defaultPrim", V::Token("A".into())),
            Spec::new("/A", Form::Prim)
                .with_field("specifier", V::Specifier(Specifier::Def))
                .with_field("typeName", V::Token("Scope".into()))
                .with_field("kind", V::Token("component".into()))
                .with_field("documentation", V::String("a note".into())),
            attribute("/A.s", "string", V::String("hello".into())),
            attribute("/A.t", "token", V::Token("there".into())),
            attribute("/A.u", "asset", V::Asset("./u.usda".into())),
            attribute("/A.e", "pathExpression", V::PathExpression("/A/*".into())),
            attribute("/A.one", "int", V::Int(1)),
            attribute("/A.many", "int", V::IntArray(alloc::vec![1, 2])),
            Spec::new("/A.d", Form::Attribute)
                .with_field("typeName", V::Token("int".into()))
                .with_field(
                    "customData",
                    V::Dictionary(alloc::vec![("k".into(), V::String("v".into()))]),
                ),
        ])
        .unwrap();
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = crate::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
        assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
        let layer = &read.layer;
        assert_eq!(layer.default_prim(), Some(tokens.intern("A")));
        let a = paths.intern(Path::parse_absolute("/A", &tokens).unwrap());
        let prim = layer.prims().get(&a).unwrap();
        assert_eq!(prim.type_name, Some(tokens.intern("Scope")));
        let field = |name: &str| {
            let name = tokens.intern(name);
            prim.fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.value.clone())
        };
        assert_eq!(
            field("kind"),
            Some(FieldValue::Value(Value::Token(tokens.intern("component"))))
        );
        assert_eq!(
            field("documentation"),
            Some(FieldValue::Value(Value::String("a note".into())))
        );
        let property = |name: &str| {
            let name = tokens.intern(name);
            let entry = prim.properties.iter().find(|e| e.name == name).unwrap();
            Arc::clone(&entry.spec)
        };
        let default = |name: &str| property(name).default.clone().unwrap();
        assert_eq!(default("s"), Value::String("hello".into()));
        assert_eq!(default("t"), Value::Token(tokens.intern("there")));
        assert_eq!(default("u"), Value::Asset("./u.usda".into()));
        assert_eq!(default("e"), Value::PathExpression("/A/*".into()));
        let type_name =
            |name: &str| String::from(property(name).type_name.as_ref().unwrap().type_name());
        assert_eq!(type_name("s"), "string");
        assert_eq!(type_name("one"), "int");
        assert_eq!(type_name("many"), "int[]");
        assert_eq!(type_name("d"), "int");
        let custom = tokens.intern("customData");
        let data = property("d");
        let data = data.metadata.iter().find(|f| f.name == custom).unwrap();
        assert_eq!(
            data.value,
            FieldValue::Value(Value::Dictionary(alloc::vec![(
                "k".into(),
                Value::String("v".into())
            )]))
        );
    }

    /// A resolver that resolves nothing and records what it is asked.
    #[derive(Default)]
    struct Recording(Vec<String>);

    impl AssetResolver for Recording {
        fn resolve(
            &mut self,
            asset: &str,
            _: Option<LayerId>,
            _: &TokenInterner,
            _: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            self.0.push(String::from(asset));
            Err(AssetResolveError::NotFound)
        }

        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }

    /// A resolver that cannot supply any asset yet.
    struct Unavailable;

    impl AssetResolver for Unavailable {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &TokenInterner,
            _: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            Err(AssetResolveError::Unavailable)
        }

        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
    }

    /// An arc whose asset the resolver cannot supply yet stays deferred,
    /// anchored to the layer, for composition to ask for again.
    #[test]
    fn unavailable_assets_keep_their_arcs_deferred() {
        let bytes = chunked_file(8);
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = crate::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut Unavailable).unwrap();
        let p3 = paths
            .lookup(&Path::parse_absolute("/P3", &tokens).unwrap())
            .unwrap();
        let arc = &read.layer.prim(p3).unwrap().references.prepend[0];
        assert!(arc.is_unresolved());
        assert_eq!(
            arc.source(),
            ReferenceSource::Asset {
                anchor: LayerId(1),
                path: "./asset_3.usda"
            }
        );
    }

    /// A file of `n` prims and their properties, with what a read reports
    /// or resolves spread among them: prims sharing fieldsets, references
    /// and payloads the resolver is asked for, prim relocates, which are
    /// reported, and a variant set.
    fn chunked_file(n: usize) -> Vec<u8> {
        use crate::writer::{
            ListOp as Op, Reference as Arc_, Spec, SpecForm as Form, Value as V, write_crate,
        };
        let arc = |asset: &str| Arc_ {
            custom_data: Vec::new(),
            asset: String::from(asset),
            prim_path: String::new(),
            offset: 0.0,
            scale: 1.0,
        };
        let names: Vec<String> = (0..n).map(|i| alloc::format!("P{i}")).collect();
        let mut specs = alloc::vec![
            Spec::new("/", Form::PseudoRoot)
                .with_field("primChildren", V::TokenVector(names.clone()))
        ];
        for (i, name) in names.iter().enumerate() {
            let path = alloc::format!("/{name}");
            let mut prim = Spec::new(&path, Form::Prim)
                .with_field("specifier", V::Specifier(Specifier::Def))
                .with_field("typeName", V::Token("Scope".into()));
            if i % 7 == 3 {
                let asset = alloc::format!("./asset_{i}.usda");
                prim = prim.with_field(
                    "references",
                    V::ReferenceListOp(Op::prepend(alloc::vec![arc(&asset)])),
                );
            }
            if i % 11 == 5 {
                let asset = alloc::format!("./payload_{i}.usda");
                prim = prim.with_field(
                    "payload",
                    V::PayloadListOp(Op::prepend(alloc::vec![arc(&asset)])),
                );
            }
            if i % 13 == 9 {
                prim = prim.with_field(
                    "relocates",
                    V::Relocates(alloc::vec![("/a".into(), "/b".into())]),
                );
            }
            if i == n / 2 {
                prim = prim.with_field(
                    "variantSetNames",
                    V::StringListOp(Op::prepend(alloc::vec!["look".into()])),
                );
            }
            specs.push(prim);
            specs.push(
                Spec::new(alloc::format!("{path}.x"), Form::Attribute)
                    .with_field("typeName", V::Token("int".into()))
                    .with_field("default", V::Int(i32::try_from(i % 3).unwrap())),
            );
            specs.push(
                Spec::new(alloc::format!("{path}.label"), Form::Attribute)
                    .with_field("typeName", V::Token("string".into()))
                    .with_field("default", V::String(alloc::format!("label {i}"))),
            );
            specs.push(
                Spec::new(alloc::format!("{path}.next"), Form::Relationship).with_field(
                    "targetPaths",
                    V::PathListOp(Op::explicit(alloc::vec![alloc::format!(
                        "/P{}",
                        (i + 1) % n
                    )])),
                ),
            );
        }
        let host = alloc::format!("/P{}", n / 2);
        specs.push(Spec::new(
            alloc::format!("{host}{{look=}}"),
            Form::VariantSet,
        ));
        specs.push(Spec::new(
            alloc::format!("{host}{{look=red}}"),
            Form::Variant,
        ));
        write_crate(&specs).unwrap()
    }

    /// Assembling in chunks, on any number of threads, reads the layer a
    /// single pass does: the same specs, diagnostics in the same order, the
    /// resolver asked the same things in the same order, the same budget
    /// charged, and with too small a budget, the same error.
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn chunks_on_any_number_of_threads_read_what_one_pass_does() {
        let bytes = chunked_file(60);
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let sections =
            crate::section::parse_sections(&bytes, &toc, header.crate_version(), &mut budget)
                .unwrap();
        // Shared interners: each read finds the ids the first interned.
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = |limit: u64, threads: usize, chunk: usize| {
            let mut resolver = Recording::default();
            let mut budget = DecodeBudget::with_limit(limit);
            let result = assemble_in_chunks(
                &bytes,
                &sections,
                LayerId(1),
                &tokens,
                &paths,
                &mut resolver,
                &mut budget,
                &Threads::new(threads),
                chunk,
            );
            (result, resolver.0, budget.used())
        };
        let (whole, asked, used) = read(u64::MAX, 1, usize::MAX);
        let whole = whole.unwrap();
        assert!(!whole.diagnostics.is_empty());
        assert!(!asked.is_empty());
        for threads in [1, 2, 4] {
            for chunk in [1, 2, 3, 7, 64] {
                let (chunked, chunked_asked, chunked_used) = read(u64::MAX, threads, chunk);
                let chunked = chunked.unwrap();
                assert!(chunked.layer == whole.layer, "{threads} threads, {chunk}");
                assert_eq!(chunked.diagnostics, whole.diagnostics, "{threads} {chunk}");
                assert_eq!(chunked_asked, asked, "{threads} {chunk}");
                assert_eq!(chunked_used, used, "{threads} {chunk}");
            }
        }
        for limit in [used - 1, used / 2, used / 5] {
            let (expected, ..) = read(limit, 1, usize::MAX);
            let expected = expected.unwrap_err();
            for (threads, chunk) in [(1, 3), (4, 3), (4, 1)] {
                let (chunked, ..) = read(limit, threads, chunk);
                assert_eq!(chunked.unwrap_err(), expected, "{limit} {threads} {chunk}");
            }
        }
    }

    /// A file with no reference or payload field reads as a layer known
    /// to author none; one with either, in a variant branch or not, does
    /// not, and neither does the layer once it is edited.
    #[test]
    fn a_layer_without_arc_fields_is_noted_as_authoring_none() {
        let read = |source: &str| {
            let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
            let text =
                layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
            let bytes = crate::writer::save_layer(&text.emitted.layer, &tokens, &paths).unwrap();
            let read =
                crate::read_usdc(&bytes, LayerId(1), &tokens, &paths, &mut NoAssets).unwrap();
            (read.layer, paths, tokens)
        };
        let (mut layer, paths, tokens) =
            read("#usda 1.0\ndef \"A\" {\n    int x = 1\n    def \"B\" {}\n}\n");
        assert!(layer.known_without_reference_arcs());
        let a = paths.intern(Path::parse_absolute("/A", &tokens).unwrap());
        layer.prim_mut(a).unwrap();
        assert!(
            !layer.known_without_reference_arcs(),
            "an edit ends the note"
        );
        for source in [
            "#usda 1.0\ndef \"A\" (references = </B>) {}\ndef \"B\" {}\n",
            "#usda 1.0\ndef \"A\" (payload = </B>) {}\ndef \"B\" {}\n",
            "#usda 1.0\ndef \"A\" (variantSets = \"v\") {\n    variantSet \"v\" = {\n        \"x\" (references = </B>) {}\n    }\n}\ndef \"B\" {}\n",
        ] {
            assert!(!read(source).0.known_without_reference_arcs(), "{source}");
        }
    }

    /// A property's prim path text is checked as parsing it would check it.
    #[test]
    fn property_owners_are_checked_as_parsed() {
        let tokens = TokenInterner::default();
        for text in [
            "/", "", "A", "/A", "/A/B", "/A//B", "/A/", "//A", "/A.b", "/A}", "/A:b", "/A/B C",
        ] {
            let parsed = !text.contains(['.', '}'])
                && text != "/"
                && Path::parse_absolute(text, &tokens).is_ok();
            assert_eq!(is_property_owner(text), parsed, "{text:?}");
        }
    }

    /// Specs sharing a fieldset are built once: prims are copies of one
    /// spec, properties share one, and each is what reading it alone
    /// gives; editing one leaves the others as they are.
    #[test]
    fn specs_sharing_a_fieldset_read_as_if_alone() {
        let source = r#"#usda 1.0
def Xform "A" (kind = "component") {
    rel r = </T>
    uniform token u = "x"
}
def Xform "B" (kind = "component") {
    rel r = </T>
    uniform token u = "x"
}
def "T" {}
"#;
        let (tokens, paths) = (TokenInterner::default(), PathInterner::default());
        let read = layerstack_usda::read_usda(source, LayerId(1), &tokens, &paths, &mut NoAssets);
        let bytes = crate::writer::save_layer(&read.emitted.layer, &tokens, &paths).unwrap();
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let sections =
            crate::section::parse_sections(&bytes, &toc, header.crate_version(), &mut budget)
                .unwrap();
        let fieldset = |path: &str| {
            let spec = sections
                .specs
                .iter()
                .find(|spec| sections.path(spec.path_index as usize) == Some(path));
            spec.unwrap().fieldset_index
        };
        assert_eq!(fieldset("/A"), fieldset("/B"), "the prims share a fieldset");
        assert_eq!(fieldset("/A.r"), fieldset("/B.r"), "the relationships do");
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let assembled = assemble(
            &bytes,
            &sections,
            LayerId(1),
            &tokens,
            &paths,
            &mut NoAssets,
            &mut budget,
            &Threads::new(1),
        )
        .unwrap();
        assert!(
            assembled.diagnostics.is_empty(),
            "{:?}",
            assembled.diagnostics
        );
        let mut layer = assembled.layer;
        assert!(layer == read.emitted.layer, "the layer written");
        let [a, b] =
            ["/A", "/B"].map(|path| paths.intern(Path::parse_absolute(path, &tokens).unwrap()));
        let r = tokens.intern("r");
        let shared = |layer: &Layer| {
            let [a, b] = [a, b].map(|prim| {
                let spec = layer.prim(prim).unwrap();
                spec.properties
                    .iter()
                    .find(|entry| entry.name == r)
                    .unwrap()
                    .spec
                    .clone()
            });
            Arc::ptr_eq(&a, &b)
        };
        assert!(shared(&layer), "one relationship spec");
        let edited = layer.prim_mut(b).unwrap().property_mut(r).unwrap();
        edited.custom = true;
        assert!(!shared(&layer));
        assert!(
            !layer.prim(a).unwrap().property(r).unwrap().custom,
            "/A.r as read"
        );
    }

    /// Each spec sharing a fieldset reports what reading it reports, and
    /// is charged what reading it costs.
    #[test]
    fn specs_sharing_a_fieldset_each_report_and_are_charged() {
        let n = 3;
        let mut sections = shared_fieldset_sections(
            n,
            1,
            (0..n).map(|i| alloc::format!("/P{i}")).collect(),
            |i| i,
        );
        sections.tokens = [String::from("relocates")]
            .into_iter()
            .collect::<TokenTable>();
        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let read = assemble_within(&sections, &mut budget).unwrap();
        let reported: Vec<&str> = read
            .diagnostics
            .iter()
            .map(|d| d.spec_path.as_str())
            .collect();
        assert_eq!(reported, ["/P0", "/P1", "/P2"]);

        // Every spec sharing the fieldset costs what the first did.
        let used = |specs: u32| {
            let paths = (0..specs).map(|i| alloc::format!("/P{i}")).collect();
            let sections = shared_fieldset_sections(specs, 64, paths, |i| i);
            let mut budget = DecodeBudget::with_limit(u64::MAX);
            assemble_within(&sections, &mut budget).unwrap();
            budget.used()
        };
        let (one, two, three) = (used(1), used(2), used(3));
        assert_eq!(three - two, two - one);
        assert!(two - one >= 64 / 16, "charged for the shared name");
    }

    /// Properties follow the authored `properties` order, those it does not
    /// name last in the order they came, whether the list is searched or
    /// hashed, and whether or not they came in order.
    #[test]
    fn properties_follow_their_authored_order() {
        let tokens = TokenInterner::default();
        let entry = |name: &str| PropertyEntry {
            name: tokens.intern(name),
            spec: Arc::new(PropertySpec::relationship()),
        };
        let names = |entries: &[PropertyEntry]| -> Vec<String> {
            entries
                .iter()
                .map(|entry| String::from(tokens.resolve(entry.name)))
                .collect()
        };
        for extra in [0, 20] {
            let mut children: Vec<TokenId> =
                ["c", "a", "b"].iter().map(|n| tokens.intern(n)).collect();
            // A long list names more properties than the prim has.
            children.extend((0..extra).map(|i| tokens.intern(alloc::format!("x{i}"))));
            let mut properties = alloc::vec![entry("a"), entry("z"), entry("b"), entry("c")];
            sort_by_children(&mut properties, &children);
            assert_eq!(names(&properties), ["c", "a", "b", "z"], "{extra}");
            sort_by_children(&mut properties, &children);
            assert_eq!(names(&properties), ["c", "a", "b", "z"], "in order already");
        }
    }

    #[test]
    fn identity_array_edits_retain_actual_kind_independently_of_declaration() {
        for byte in (1..=30).chain([56, 57]) {
            let kind = ValueType::try_from(byte).unwrap();
            let mut edit_rep = [0; 8];
            edit_rep[6] = byte;
            edit_rep[7] = 0x10; // identity edit, no payload or literals
            let mut declaration_rep = [0; 8];
            declaration_rep[0] = 2; // token index of "double[]"
            declaration_rep[6] = ValueType::Token as u8;
            declaration_rep[7] = 0x40;
            let sections = CrateSections {
                tokens: ["default", "typeName", "double[]"]
                    .into_iter()
                    .collect::<TokenTable>(),
                strings: Vec::new(),
                fields: alloc::vec![
                    FieldDef {
                        token_index: 0,
                        value_rep: edit_rep
                    },
                    FieldDef {
                        token_index: 1,
                        value_rep: declaration_rep
                    },
                ],
                fieldsets: alloc::vec![-1, 0, 1, -1],
                paths: PathTable::from_texts(["/A", "/A.a"]),
                specs: alloc::vec![
                    SpecDef {
                        path_index: 0,
                        fieldset_index: 0,
                        form: SpecForm::Prim
                    },
                    SpecDef {
                        path_index: 1,
                        fieldset_index: 1,
                        form: SpecForm::Attribute
                    },
                ],
                version: CrateVersion::NEWEST_READABLE,
            };
            let tokens = TokenInterner::default();
            let read = assemble(
                &[],
                &sections,
                LayerId(1),
                &tokens,
                &PathInterner::default(),
                &mut NoAssets,
                &mut DecodeBudget::with_limit(10_000),
                &Threads::new(1),
            )
            .unwrap();
            let property = &read.layer.prims().values().next().unwrap().properties[0].spec;
            assert_eq!(property.type_name.as_ref().unwrap().type_name(), "double[]");
            let value = property.default.as_ref().unwrap();
            assert!(value.array_edit_ref().unwrap().ops.is_empty());
            let actual = value.array_edit_type().unwrap();
            let element = array_edit_element_name(kind);
            assert_eq!(actual.type_name(), alloc::format!("{element}[]"));
            assert!(actual.is_array());
            assert_eq!(
                actual.default_scalar(),
                &layerstack::ValueType::from_name(element).unwrap().zero()
            );
        }
    }

    #[test]
    fn compact_integer_decode_preserves_width_and_signedness() {
        let values = [-1_i64, 0, 1, i64::MIN, i64::MAX];
        for (ty, expected) in [
            (
                ValueType::Bool,
                alloc::vec![
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::Bool(true)
                ],
            ),
            (
                ValueType::UChar,
                alloc::vec![
                    Value::UChar(255),
                    Value::UChar(0),
                    Value::UChar(1),
                    Value::UChar(0),
                    Value::UChar(255)
                ],
            ),
            (
                ValueType::Int,
                alloc::vec![
                    Value::Int(-1),
                    Value::Int(0),
                    Value::Int(1),
                    Value::Int(0),
                    Value::Int(-1)
                ],
            ),
            (
                ValueType::UInt,
                alloc::vec![
                    Value::UInt(u32::MAX),
                    Value::UInt(0),
                    Value::UInt(1),
                    Value::UInt(0),
                    Value::UInt(u32::MAX)
                ],
            ),
            (
                ValueType::Int64,
                alloc::vec![
                    Value::Int64(-1),
                    Value::Int64(0),
                    Value::Int64(1),
                    Value::Int64(i64::MIN),
                    Value::Int64(i64::MAX)
                ],
            ),
            (
                ValueType::UInt64,
                alloc::vec![
                    Value::UInt64(u64::MAX),
                    Value::UInt64(0),
                    Value::UInt64(1),
                    Value::UInt64(1 << 63),
                    Value::UInt64((1 << 63) - 1)
                ],
            ),
        ] {
            let width = match ty {
                ValueType::Bool | ValueType::UChar => 1,
                ValueType::Int | ValueType::UInt => 4,
                _ => 8,
            };
            let mut data = alloc::vec![0; 8];
            data.extend_from_slice(&(values.len() as u64).to_le_bytes());
            for value in values {
                data.extend_from_slice(&value.to_le_bytes()[..width]);
            }
            let mut raw = [0; 8];
            raw[0] = 8;
            raw[6] = ty as u8;
            raw[7] = 0x80;
            let sections = CrateSections {
                tokens: TokenTable::default(),
                strings: Vec::new(),
                fields: Vec::new(),
                fieldsets: Vec::new(),
                paths: PathTable::default(),
                specs: Vec::new(),
                version: CrateVersion::NEWEST_READABLE,
            };
            let decoded = decode_field_within(
                &RawValueRep::new(raw),
                &data,
                &sections,
                &mut DecodeBudget::with_limit(100),
                None,
            )
            .unwrap();
            let DecodedField::Value(CrateValue::TypedArray(array)) = decoded else {
                panic!("native integer array")
            };
            assert_eq!(Value::TypedArray(array), Value::Array(expected));
        }
    }

    #[test]
    fn compact_array_fields_preserve_metadata_defaults_and_variants() {
        for tag in (1..=9).chain(13..=30).chain(core::iter::once(56)) {
            let ty = ValueType::try_from(tag).unwrap();
            let mut data = alloc::vec![0; 8];
            data.extend_from_slice(&1_u64.to_le_bytes());
            data.extend_from_slice(&[0x81; 128]);
            let mut raw = [0; 8];
            raw[0] = 8;
            raw[6] = tag;
            raw[7] = 0x80;
            let sections = CrateSections {
                tokens: ["default", "customArray"]
                    .into_iter()
                    .collect::<TokenTable>(),
                strings: Vec::new(),
                fields: alloc::vec![
                    FieldDef {
                        token_index: 0,
                        value_rep: raw
                    },
                    FieldDef {
                        token_index: 1,
                        value_rep: raw
                    },
                ],
                fieldsets: alloc::vec![0, -1, 1, -1],
                paths: PathTable::from_texts(["/P", "/P.a", "/P{v=x}", "/P{v=x}.a"]),
                specs: alloc::vec![
                    SpecDef {
                        path_index: 0,
                        fieldset_index: 2,
                        form: SpecForm::Prim
                    },
                    SpecDef {
                        path_index: 1,
                        fieldset_index: 0,
                        form: SpecForm::Attribute
                    },
                    SpecDef {
                        path_index: 2,
                        fieldset_index: 2,
                        form: SpecForm::Variant
                    },
                    SpecDef {
                        path_index: 3,
                        fieldset_index: 0,
                        form: SpecForm::Attribute
                    },
                ],
                version: CrateVersion::NEWEST_READABLE,
            };
            let tokens = TokenInterner::default();
            let paths = PathInterner::default();
            let result = assemble(
                &data,
                &sections,
                LayerId(1),
                &tokens,
                &paths,
                &mut NoAssets,
                &mut DecodeBudget::with_limit(100),
                &Threads::new(1),
            )
            .unwrap();
            assert!(result.diagnostics.is_empty());
            let prim = result.layer.prims().values().next().unwrap();
            let expected = if tag <= 6 {
                Value::Array(alloc::vec![match ty {
                    ValueType::Bool => Value::Bool(true),
                    ValueType::UChar => Value::UChar(0x81),
                    ValueType::Int => Value::Int(i32::from_le_bytes([0x81; 4])),
                    ValueType::UInt => Value::UInt(u32::from_le_bytes([0x81; 4])),
                    ValueType::Int64 => Value::Int64(i64::from_le_bytes([0x81; 8])),
                    ValueType::UInt64 => Value::UInt64(u64::from_le_bytes([0x81; 8])),
                    _ => unreachable!(),
                }])
            } else if matches!(tag, 7 | 8 | 9 | 56) {
                Value::Array(alloc::vec![match ty {
                    ValueType::Half => Value::Half(u16::from_le_bytes([0x81; 2])),
                    ValueType::Float => Value::Float(f32::from_le_bytes([0x81; 4])),
                    ValueType::Double => Value::Double(f64::from_le_bytes([0x81; 8])),
                    _ => Value::TimeCode(f64::from_le_bytes([0x81; 8])),
                }])
            } else {
                Value::Array(alloc::vec![convert_math_value(ty, &data[16..])])
            };
            let property = prim.property(tokens.intern("a")).unwrap();
            assert_eq!(property.default, Some(expected.clone()));
            assert!(property.type_name.as_ref().unwrap().is_array());
            assert_eq!(
                prim.field(tokens.intern("customArray")),
                Some(&FieldValue::Value(expected.clone()))
            );
            let variant = &prim.variant_sets()[&tokens.intern("v")].variants[&tokens.intern("x")];
            assert_eq!(variant.properties[0].spec.default, Some(expected.clone()));
            assert_eq!(variant.fields[0].value, FieldValue::Value(expected));
        }
    }

    #[test]
    fn unused_math_fields_are_still_validated() {
        let mut sections = shared_fieldset_sections(1, 1, alloc::vec!["/Missing.a".into()], |_| 0);
        let mut raw = [0; 8];
        raw[0] = 8;
        raw[6] = ValueType::Vec3f as u8;
        raw[7] = 0x80;
        sections.fields[0].value_rep = raw;
        for form in [
            SpecForm::Attribute,
            SpecForm::Relationship,
            SpecForm::Connection,
            SpecForm::VariantSet,
            SpecForm::Variant,
            SpecForm::Mapper,
        ] {
            sections.specs[0].form = form;
            assert!(matches!(
                assemble_within(&sections, &mut DecodeBudget::with_limit(100)),
                Err(UsdcError::UnexpectedEof { .. })
            ));
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
            tokens: ["x".repeat(name_len)].into_iter().collect::<TokenTable>(),
            strings: Vec::new(),
            fields: alloc::vec![FieldDef {
                token_index: 0,
                value_rep: rep,
            }],
            fieldsets: alloc::vec![0, -1],
            paths: PathTable::from_texts(paths),
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
            &TokenInterner::default(),
            &PathInterner::default(),
            &mut NoAssets,
            budget,
            &Threads::new(1),
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
    fn validated_property_text_matches_interned_spelling() {
        // Borrowed keys must be identical to the previous display/resolve
        // keys, including namespace separators and text the parser permits.
        for text in [
            "/World/Mesh.primvars:st",
            "/World/Mesh.material:binding",
            "/世界/形状.displayColor",
            "/P[x].a[b]",
            "/P.a b",
        ] {
            let tokens = TokenInterner::default();
            let paths = PathInterner::default();
            let property = PropertyPath::parse(text, &tokens, &paths).unwrap();
            let (prim, name) = text.rsplit_once('.').unwrap();
            assert_eq!(prim, paths.display(property.prim_path(), &tokens));
            assert_eq!(name, tokens.resolve(property.property()));
        }
    }

    #[test]
    fn split_property_simple() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let property = PropertyPath::parse("/Cube.size", &tokens, &paths).unwrap();
        assert_eq!(paths.display(property.prim_path(), &tokens), "/Cube");
        assert_eq!(tokens.resolve(property.property()), "size");
    }

    #[test]
    fn split_property_nested() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        let property = PropertyPath::parse("/World/Cube.visibility", &tokens, &paths).unwrap();
        assert_eq!(paths.display(property.prim_path(), &tokens), "/World/Cube");
        assert_eq!(tokens.resolve(property.property()), "visibility");
    }

    #[test]
    fn split_property_none() {
        let tokens = TokenInterner::default();
        let paths = PathInterner::default();
        assert!(PropertyPath::parse("/Cube", &tokens, &paths).is_err());
    }

    #[test]
    fn split_trailing_selections_ok() {
        assert_eq!(
            split_trailing_selections("/Prim{shadingVariant=}"),
            Some(("/Prim", alloc::vec![("shadingVariant", "")]))
        );
        assert_eq!(
            split_trailing_selections("/Prim{shadingVariant=red}"),
            Some(("/Prim", alloc::vec![("shadingVariant", "red")]))
        );
        // Nested in another branch of the same prim: each enclosing
        // selection is kept, outermost first.
        assert_eq!(
            split_trailing_selections("/Prim{set=red}{inner=}"),
            Some(("/Prim", alloc::vec![("set", "red"), ("inner", "")]))
        );
        assert_eq!(
            split_trailing_selections("/Prim{set=red}{inner=x}{set=red}"),
            Some((
                "/Prim",
                alloc::vec![("set", "red"), ("inner", "x"), ("set", "red")]
            ))
        );
        // On a prim inside a branch.
        assert_eq!(
            split_trailing_selections("/A/Prim{set=red}Child{inner=x}"),
            Some(("/A/Prim{set=red}Child", alloc::vec![("inner", "x")]))
        );
    }

    #[test]
    fn variant_paths_below_variant_children_are_not_a_variant() {
        // A child prim of a variant, its property, and paths naming no
        // prim.
        for path in [
            "/Prim{set=red}Child",
            "/Prim{set=red}Child.color",
            "/Prim{set=red}.color",
            "/Prim",
            "/{set=red}",
            "{set=red}",
            "/Prim{=red}",
        ] {
            assert_eq!(split_trailing_selections(path), None, "{path}");
        }
    }

    #[test]
    fn parent_prim_path_root_child() {
        assert_eq!(parent_prim_path("/Cube"), Some("/"));
    }

    #[test]
    fn parent_prim_path_nested() {
        assert_eq!(parent_prim_path("/World/Cube"), Some("/World"));
    }

    #[test]
    fn parent_prim_path_root() {
        assert!(parent_prim_path("/").is_none());
    }
}
