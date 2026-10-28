// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Section parsers for the six USDC sections.
//!
//! Each section is located via the TOC and parsed into in-memory structures
//! that the assembler can consume.
//!
//! Spec: AOUSD Core §16.3.5–§16.3.8.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::compression::{lz4_decompress, read_compressed_ints_as};
use crate::error::UsdcError;
use crate::toc::{SectionEntry, Toc};
use crate::value_rep::DecodeBudget;
use crate::value_type::SpecForm;
use crate::version::CrateVersion;
use layerstack::parallel::Threads;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A field definition: a token index and a raw 8-byte value representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldDef {
    /// Index into the tokens table for the field name.
    pub token_index: u32,
    /// Raw 8-byte value representation (decoded later by `value_rep`).
    pub value_rep: [u8; 8],
}

/// A spec definition: a path index, fieldset index, and spec form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecDef {
    /// Index into the paths table.
    pub path_index: u32,
    /// Start index into the fieldsets flat array.
    pub fieldset_index: u32,
    /// The spec form (Prim, Attribute, etc.).
    pub form: SpecForm,
}

/// Where one path of [`CrateSections::paths`] sits in the encoded path tree.
///
/// Spec: AOUSD Core §16.3.8 (PATHS section).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathNode {
    /// A table entry no encoded path fills; its text is empty.
    Unused,
    /// The pseudo-root, `/`.
    Root,
    /// The path whose text is the parent's followed by the element named by
    /// token `token`: a prim name or variant selection, or with `property`
    /// a property name.
    Child {
        /// The index of the parent path.
        parent: u32,
        /// The token table index of the element.
        token: u32,
        /// Whether the element is a property name.
        property: bool,
    },
}

/// The TOKENS section: every token's text in one buffer, and where each
/// ends, so that a token is read as a slice of the text the section
/// decompressed to rather than copied into a string of its own.
///
/// Spec: AOUSD Core §16.3.5.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenTable {
    /// The tokens' text, each followed by a separator byte.
    text: String,
    /// The offset in `text` where each token ends.
    ends: Vec<u32>,
}

impl TokenTable {
    /// The text of token `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        let end = *self.ends.get(index)? as usize;
        let start = match index.checked_sub(1) {
            Some(before) => self.ends[before] as usize + 1,
            None => 0,
        };
        self.text.get(start..end)
    }

    /// The length of the text of token `index`, found without reading it.
    #[must_use]
    pub fn text_len(&self, index: usize) -> Option<usize> {
        let end = *self.ends.get(index)? as usize;
        let start = match index.checked_sub(1) {
            Some(before) => self.ends[before] as usize + 1,
            None => 0,
        };
        Some(end - start)
    }

    /// The number of tokens.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether there are no tokens.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Every token's text, in table order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.len()).map(|index| self.get(index).unwrap_or_default())
    }

    /// The table of `text`, a section's decompressed tokens: each ended by
    /// a `\0` byte, the last perhaps not.
    fn of_section(text: String) -> Result<Self, UsdcError> {
        let too_long = UsdcError::Inconsistent {
            message: "TOKENS section exceeds 4 GiB",
        };
        let mut ends = Vec::new();
        for (at, byte) in text.bytes().enumerate() {
            if byte == 0 {
                ends.push(u32::try_from(at).map_err(|_| too_long.clone())?);
            }
        }
        let last = ends.last().map_or(0, |&end| end as usize + 1);
        if last < text.len() {
            ends.push(u32::try_from(text.len()).map_err(|_| too_long)?);
        }
        Ok(Self { text, ends })
    }
}

impl core::ops::Index<usize> for TokenTable {
    type Output = str;

    fn index(&self, index: usize) -> &str {
        self.get(index).expect("token index out of range")
    }
}

impl<S: AsRef<str>> FromIterator<S> for TokenTable {
    fn from_iter<I: IntoIterator<Item = S>>(tokens: I) -> Self {
        let mut table = Self::default();
        for token in tokens {
            table.text.push_str(token.as_ref());
            let end = u32::try_from(table.text.len()).expect("a token table under 4 GiB");
            table.ends.push(end);
            table.text.push('\0');
        }
        table
    }
}

/// A path's text once read ([`PathTable::text`]): shared by the threads a
/// retained read decodes values on, with the `std` feature.
type TextSlot = OnceSlot<Box<str>>;

/// A value found once, when first read: shared between threads with the
/// `std` feature.
#[cfg(feature = "std")]
pub(crate) type OnceSlot<T> = std::sync::OnceLock<T>;
/// A value found once, when first read.
#[cfg(not(feature = "std"))]
pub(crate) type OnceSlot<T> = core::cell::OnceCell<T>;

/// The PATHS section: the tree of paths it encodes, each path its parent's
/// followed by one element, and each path's text, found from the tree
/// when it is first read: a reader naming paths through the tree never
/// builds their text.
///
/// Spec: AOUSD Core §16.3.8 (PATHS section).
#[derive(Clone, Debug, Default)]
pub struct PathTable {
    /// Where each path sits in the tree; empty for a table of texts alone
    /// ([`PathTable::from_texts`]).
    nodes: Vec<PathNode>,
    /// Each path's text, once read.
    texts: Vec<TextSlot>,
    /// The length of each path's text.
    lens: Vec<usize>,
    /// What each path's text holds ([`Spelling`]).
    spellings: Vec<Spelling>,
}

/// What a path's text holds, found with its length, so that a reader can
/// tell without the text what it would find in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Spelling(u8);

impl Spelling {
    /// The text holds a `{`: it names a variant selection.
    const SELECTION: u8 = 1;
    /// The text holds a `.`.
    const DOT: u8 = 2;
    /// The text holds a `}`.
    const CLOSE: u8 = 4;
    /// The text ends with `}`.
    const CLOSE_LAST: u8 = 8;
    /// The text is `/`.
    const ROOT: u8 = 16;

    fn of(text: &str) -> Self {
        let mut bits = 0;
        for (flag, holds) in [
            (Self::SELECTION, text.contains('{')),
            (Self::DOT, text.contains('.')),
            (Self::CLOSE, text.contains('}')),
            (Self::CLOSE_LAST, text.ends_with('}')),
            (Self::ROOT, text == "/"),
        ] {
            if holds {
                bits |= flag;
            }
        }
        Self(bits)
    }

    fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    /// Whether the text holds a `{`.
    pub(crate) fn selection(self) -> bool {
        self.has(Self::SELECTION)
    }

    /// Whether the text holds a `.`.
    pub(crate) fn dot(self) -> bool {
        self.has(Self::DOT)
    }

    /// Whether the text holds a `}`.
    pub(crate) fn close(self) -> bool {
        self.has(Self::CLOSE)
    }

    /// Whether the text ends with `}`.
    pub(crate) fn ends_with_close(self) -> bool {
        self.has(Self::CLOSE_LAST)
    }

    /// Whether the text is `/`.
    pub(crate) fn root(self) -> bool {
        self.has(Self::ROOT)
    }
}

impl PathTable {
    /// A table of `texts` without a tree, whose readers read the text.
    pub fn from_texts<S: Into<Box<str>>>(texts: impl IntoIterator<Item = S>) -> Self {
        let mut lens = Vec::new();
        let mut spellings = Vec::new();
        let texts = texts
            .into_iter()
            .map(|text| {
                let text = text.into();
                lens.push(text.len());
                spellings.push(Spelling::of(&text));
                let slot = TextSlot::new();
                let _ = slot.set(text);
                slot
            })
            .collect();
        Self {
            nodes: Vec::new(),
            texts,
            lens,
            spellings,
        }
    }

    /// The number of paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.texts.len()
    }

    /// Whether there are no paths.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    /// Whether the table holds the tree: where every path sits in it.
    #[must_use]
    pub fn has_tree(&self) -> bool {
        self.nodes.len() == self.texts.len()
    }

    /// Where each path sits in the tree, by index; empty for a table
    /// without one.
    #[must_use]
    pub fn nodes(&self) -> &[PathNode] {
        &self.nodes
    }

    /// The length of the text of path `index`, found without building it.
    #[must_use]
    pub fn text_len(&self, index: usize) -> Option<usize> {
        self.lens.get(index).copied()
    }

    /// What the text of path `index` holds, found without building it.
    pub(crate) fn spelling(&self, index: usize) -> Option<Spelling> {
        self.spellings.get(index).copied()
    }

    /// The text of path `index`, its element tokens read from `tokens`,
    /// the table the paths were read with: built from the tree when first
    /// read, as the PATHS section's paths are spelled. An unused entry's
    /// text is empty.
    pub fn text<'a>(&'a self, index: usize, tokens: &TokenTable) -> Option<&'a str> {
        let slot = self.texts.get(index)?;
        if let Some(text) = slot.get() {
            return Some(text);
        }
        // The entries up to the nearest whose text is known, nearest last.
        // A corrupt table may name a parent outside it, whose text is
        // empty, as it was when the text was built with the table.
        let mut pending = Vec::new();
        let mut at = index;
        while self.texts.get(at).is_some_and(|slot| slot.get().is_none()) {
            pending.push(at);
            match self.nodes.get(at) {
                Some(PathNode::Child { parent, .. }) => at = *parent as usize,
                _ => break,
            }
        }
        for &at in pending.iter().rev() {
            let text: Box<str> = match self.nodes.get(at) {
                Some(PathNode::Root) => "/".into(),
                Some(&PathNode::Child {
                    parent,
                    token,
                    property,
                }) => {
                    let parent = self.texts.get(parent as usize).and_then(TextSlot::get);
                    let parent = parent.map_or("", |text| text);
                    let element = tokens.get(token as usize).unwrap_or_default();
                    let sep = separator(parent, element, property);
                    let prefix = if parent == "/" { "" } else { parent };
                    let mut text = String::with_capacity(prefix.len() + sep.len() + element.len());
                    text.push_str(prefix);
                    text.push_str(sep);
                    text.push_str(element);
                    text.into()
                }
                Some(PathNode::Unused) | None => "".into(),
            };
            let _ = self.texts[at].set(text);
        }
        slot.get().map(|text| &**text)
    }
}

/// What joins a path's text to its child `element`'s: `.` before a
/// property, nothing before a variant selection or after one, nothing
/// before an element below a property (a target `[/C]`, a mapper
/// `.mapper[/C]` or a relational attribute `.d`, which carry their own
/// punctuation), `/` otherwise; OpenUSD rebuilds paths with
/// `SdfPath::AppendElementToken` (`pxr/usd/sdf/crateFile.cpp:4037`), so
/// `/A{v=x}B.attr` and `/A.b[/C].d`.
fn separator(parent: &str, element: &str, property: bool) -> &'static str {
    if property {
        "."
    } else if element.starts_with(['{', '[', '.']) || parent.ends_with('}') {
        ""
    } else {
        "/"
    }
}

/// All parsed section data from a USDC file.
///
/// Holds the decoded contents of all six sections ready for scene assembly.
#[derive(Clone, Debug)]
pub struct CrateSections {
    /// The tokens of the TOKENS section.
    pub tokens: TokenTable,
    /// String indices (into `tokens`) from the STRINGS section.
    pub strings: Vec<u32>,
    /// Field definitions from the FIELDS section.
    pub fields: Vec<FieldDef>,
    /// Flat fieldset array from the FIELDSETS section.
    ///
    /// Groups are delimited by negative values.
    pub fieldsets: Vec<i32>,
    /// The paths of the PATHS section, as the tree it encodes; read a
    /// path's text with [`Self::path`].
    pub paths: PathTable,
    /// Spec definitions from the SPECS section.
    pub specs: Vec<SpecDef>,
    /// The file's format version, from the header.
    ///
    /// Value decoding uses it to reject encodings the declared version
    /// cannot contain.
    pub version: CrateVersion,
}

impl CrateSections {
    /// What decoding a retained value reads of these sections, once
    /// assembly has read the rest: the version, which bounds the encodings
    /// a value may use, and, when `text` (the read retained a `token[]` or
    /// `string[]` value), the token and string tables its elements index.
    /// A numeric array reads only its own payload.
    ///
    /// The fields, fieldsets, paths and specs only describe the layer's
    /// structure, which assembly has already built.
    #[cfg(feature = "std")]
    pub(crate) fn for_retained_values(self, text: bool) -> Self {
        let (tokens, strings) = if text {
            (self.tokens, self.strings)
        } else {
            (TokenTable::default(), Vec::new())
        };
        Self {
            tokens,
            strings,
            fields: Vec::new(),
            fieldsets: Vec::new(),
            paths: PathTable::default(),
            specs: Vec::new(),
            version: self.version,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level parse entry point
// ---------------------------------------------------------------------------

/// Parses all six sections from the given file data and TOC.
///
/// Sections that are absent from the TOC produce empty vectors. `version` is
/// the header's format version and is recorded in the result.
///
/// Every table entry, every 16 bytes of decompressed data and of each path
/// built from the tokens is charged to `budget` before it is allocated (see
/// [`DecodeBudget`]).
pub fn parse_sections(
    data: &[u8],
    toc: &Toc,
    version: CrateVersion,
    budget: &mut DecodeBudget,
) -> Result<CrateSections, UsdcError> {
    let tokens = if let Some(entry) = toc.tokens {
        parse_tokens(data, &entry, budget)?
    } else {
        TokenTable::default()
    };

    let strings = if let Some(entry) = toc.strings {
        parse_strings(data, &entry, budget)?
    } else {
        vec![]
    };

    let fields = if let Some(entry) = toc.fields {
        parse_fields(data, &entry, budget)?
    } else {
        vec![]
    };

    let fieldsets = if let Some(entry) = toc.fieldsets {
        parse_fieldsets(data, &entry, budget)?
    } else {
        vec![]
    };

    let paths = if let Some(entry) = toc.paths {
        let (path_indices, _) = parse_path_array(data, &entry, 0, budget)?;
        let (element_token_indices, _) = parse_path_array(data, &entry, 1, budget)?;
        let (jumps, count) = parse_path_array(data, &entry, 2, budget)?;
        let count = count.expect("the last array counts the paths");
        let mut nodes = vec![PathNode::Unused; count];
        let (lens, spellings) = build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut nodes,
            budget,
        )?;
        PathTable {
            nodes,
            texts: (0..count).map(|_| TextSlot::new()).collect(),
            lens,
            spellings,
        }
    } else {
        PathTable::default()
    };

    let specs = if let Some(entry) = toc.specs {
        let path_indices = parse_spec_array(data, &entry, 0, budget)?;
        let fieldset_indices = parse_spec_array(data, &entry, 1, budget)?;
        let forms = parse_spec_array(data, &entry, 2, budget)?;
        let mut specs = Vec::with_capacity(path_indices.len());
        for ((&path_index, &fieldset_index), &form) in
            path_indices.iter().zip(&fieldset_indices).zip(&forms)
        {
            specs.push(SpecDef {
                path_index,
                fieldset_index,
                form: SpecForm::try_from(form)?,
            });
        }
        specs
    } else {
        vec![]
    };

    Ok(CrateSections {
        tokens,
        strings,
        fields,
        fieldsets,
        paths,
        specs,
        version,
    })
}

/// The size, in bytes, from which [`parse_sections_on`] parses a file's
/// sections on more than the calling thread.
pub const THREADED_SECTIONS_MIN: usize = 64 * 1024;

/// Parses the sections as [`parse_sections`] does, decompressing and
/// decoding them on the calling thread and the threads `threads` spares
/// (with the `std` feature): each table, and each of the integer arrays of
/// the PATHS and SPECS sections, is a task of its own. The paths are then
/// placed in the tree, which needs the tokens, on the calling thread. A
/// file smaller than [`THREADED_SECTIONS_MIN`] bytes, or one parsed when
/// the budget spares no thread, is parsed on the calling thread alone:
/// the tasks of a small file take less time than starting a thread.
///
/// The result, the budget charged and the error, if any, are those of
/// parsing the sections one after another, in file order: each task is
/// charged within what is left of `budget`, and the tasks' charges are
/// added up in that order, so the first to fail, or to exceed the budget
/// with the tasks before it, is the one reported.
pub fn parse_sections_on(
    data: &[u8],
    toc: &Toc,
    version: CrateVersion,
    budget: &mut DecodeBudget,
    threads: &Threads,
) -> Result<CrateSections, UsdcError> {
    parse_sections_from(data, toc, version, budget, threads, THREADED_SECTIONS_MIN)
}

/// [`parse_sections_on`], parsing a file of at least `threaded_min` bytes
/// on more than the calling thread.
fn parse_sections_from(
    data: &[u8],
    toc: &Toc,
    version: CrateVersion,
    budget: &mut DecodeBudget,
    threads: &Threads,
    threaded_min: usize,
) -> Result<CrateSections, UsdcError> {
    if !crate::threads::pooled(threads, data.len() >= threaded_min) {
        return parse_sections(data, toc, version, budget);
    }
    let tasks: Vec<SectionTask> = [
        toc.tokens.map(SectionTask::Tokens),
        toc.strings.map(SectionTask::Strings),
        toc.fields.map(SectionTask::Fields),
        toc.fieldsets.map(SectionTask::Fieldsets),
        toc.paths.map(|entry| SectionTask::Paths(entry, 0)),
        toc.paths.map(|entry| SectionTask::Paths(entry, 1)),
        toc.paths.map(|entry| SectionTask::Paths(entry, 2)),
        toc.specs.map(|entry| SectionTask::Specs(entry, 0)),
        toc.specs.map(|entry| SectionTask::Specs(entry, 1)),
        toc.specs.map(|entry| SectionTask::Specs(entry, 2)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let limit = budget.remaining();
    let parsed = crate::threads::run_all(threads, tasks, &|task: SectionTask| {
        let mut local = DecodeBudget::with_limit(limit);
        let parsed = task.parse(data, &mut local);
        (task.order(), parsed, local.used())
    });

    let mut sections = CrateSections {
        tokens: TokenTable::default(),
        strings: Vec::new(),
        fields: Vec::new(),
        fieldsets: Vec::new(),
        paths: PathTable::default(),
        specs: Vec::new(),
        version,
    };
    let mut path_arrays: [Vec<i32>; 3] = Default::default();
    let mut spec_arrays: [Vec<u32>; 3] = Default::default();
    let mut path_count = None;
    let mut spec_count = 0;
    // The PATHS section is placed in the tree after its arrays, before the
    // SPECS section, in file order.
    let place_paths = |sections: &mut CrateSections,
                       arrays: &mut [Vec<i32>; 3],
                       count: Option<usize>,
                       budget: &mut DecodeBudget|
     -> Result<(), UsdcError> {
        let Some(count) = count else {
            return Ok(());
        };
        let mut nodes = vec![PathNode::Unused; count];
        let [path_indices, element_token_indices, jumps] = core::mem::take(arrays);
        let (lens, spellings) = build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &sections.tokens,
            &mut nodes,
            budget,
        )?;
        sections.paths = PathTable {
            nodes,
            texts: (0..count).map(|_| TextSlot::new()).collect(),
            lens,
            spellings,
        };
        Ok(())
    };
    for (order, result, used) in parsed {
        if order == SectionTask::SPECS_ORDER {
            place_paths(&mut sections, &mut path_arrays, path_count.take(), budget)?;
        }
        let parsed = match result {
            Err(UsdcError::DecodeBudgetExceeded { .. }) => return Err(budget.exceeded()),
            Err(error) => {
                budget.charge(used)?;
                return Err(error);
            }
            Ok(parsed) => parsed,
        };
        budget.charge(used)?;
        match parsed {
            Parsed::Tokens(tokens) => sections.tokens = tokens,
            Parsed::Strings(strings) => sections.strings = strings,
            Parsed::Fields(fields) => sections.fields = fields,
            Parsed::Fieldsets(fieldsets) => sections.fieldsets = fieldsets,
            Parsed::PathArray(at, array, count) => {
                path_arrays[at] = array;
                if let Some(count) = count {
                    path_count = Some(count);
                }
            }
            Parsed::SpecArray(at, array) => {
                spec_count = array.len();
                spec_arrays[at] = array;
            }
        }
    }
    place_paths(&mut sections, &mut path_arrays, path_count.take(), budget)?;
    if toc.specs.is_some() {
        let [path_indices, fieldset_indices, forms] = spec_arrays;
        let mut specs = Vec::with_capacity(spec_count);
        for ((&path_index, &fieldset_index), &form) in
            path_indices.iter().zip(&fieldset_indices).zip(&forms)
        {
            specs.push(SpecDef {
                path_index,
                fieldset_index,
                form: SpecForm::try_from(form)?,
            });
        }
        sections.specs = specs;
    }
    Ok(sections)
}

/// One task of [`parse_sections_on`]: a table, or one of the integer arrays
/// of the PATHS or SPECS sections, by its place among them.
#[derive(Clone, Copy)]
enum SectionTask {
    Tokens(SectionEntry),
    Strings(SectionEntry),
    Fields(SectionEntry),
    Fieldsets(SectionEntry),
    Paths(SectionEntry, usize),
    Specs(SectionEntry, usize),
}

/// What a [`SectionTask`] parsed.
enum Parsed {
    Tokens(TokenTable),
    Strings(Vec<u32>),
    Fields(Vec<FieldDef>),
    Fieldsets(Vec<i32>),
    /// One of the PATHS section's arrays, and with the last, the number of
    /// paths in the table.
    PathArray(usize, Vec<i32>, Option<usize>),
    SpecArray(usize, Vec<u32>),
}

impl SectionTask {
    /// The place of the first SPECS task in file order.
    const SPECS_ORDER: usize = 7;

    /// The task's place in file order.
    fn order(self) -> usize {
        match self {
            Self::Tokens(_) => 0,
            Self::Strings(_) => 1,
            Self::Fields(_) => 2,
            Self::Fieldsets(_) => 3,
            Self::Paths(_, at) => 4 + at,
            Self::Specs(_, at) => Self::SPECS_ORDER + at,
        }
    }

    fn parse(self, data: &[u8], budget: &mut DecodeBudget) -> Result<Parsed, UsdcError> {
        Ok(match self {
            Self::Tokens(entry) => Parsed::Tokens(parse_tokens(data, &entry, budget)?),
            Self::Strings(entry) => Parsed::Strings(parse_strings(data, &entry, budget)?),
            Self::Fields(entry) => Parsed::Fields(parse_fields(data, &entry, budget)?),
            Self::Fieldsets(entry) => Parsed::Fieldsets(parse_fieldsets(data, &entry, budget)?),
            Self::Paths(entry, at) => {
                let (array, count) = parse_path_array(data, &entry, at, budget)?;
                Parsed::PathArray(at, array, count)
            }
            Self::Specs(entry, at) => {
                Parsed::SpecArray(at, parse_spec_array(data, &entry, at, budget)?)
            }
        })
    }
}

impl CrateSections {
    /// The text of path `index` of the path table ([`PathTable::text`]).
    #[must_use]
    pub fn path(&self, index: usize) -> Option<&str> {
        self.paths.text(index, &self.tokens)
    }

    /// The text of every path of the path table, in table order.
    pub fn path_texts(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        (0..self.paths.len()).map(|index| self.path(index).unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// TOKENS section
// ---------------------------------------------------------------------------

/// Parses the TOKENS section.
///
/// Format: `num_tokens: u64`, `uncompressed_size: u64`,
/// `compressed_size: u64`, then `compressed_size` bytes of LZ4 data.
/// Decompressed data is a null-delimited, null-terminated list of UTF-8
/// strings.
///
/// Spec: AOUSD Core §16.3.5.
fn parse_tokens(
    data: &[u8],
    entry: &SectionEntry,
    budget: &mut DecodeBudget,
) -> Result<TokenTable, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 24 {
        return Err(UsdcError::UnexpectedEof {
            section: "TOKENS",
            offset: entry.offset,
            expected: 24,
        });
    }

    let num_tokens = read_u64(section, 0);
    let uncompressed_size = read_u64(section, 8);
    let compressed_size = read_u64(section, 16);

    let Some(compressed) = usize::try_from(compressed_size)
        .ok()
        .and_then(|csz| section[24..].get(..csz))
    else {
        return Err(UsdcError::UnexpectedEof {
            section: "TOKENS compressed data",
            offset: entry.offset + 24,
            expected: compressed_size,
        });
    };
    // `lz4_decompress` checks the size against the compressed data.
    let usz = usize::try_from(uncompressed_size).map_err(|_| UsdcError::DecompressionFailed {
        context: "TOKENS size exceeds the address space",
    })?;
    // The text is decompressed, and charged again for the table of where
    // each token ends, as when each token was copied into a string.
    budget.charge_text(usz)?;
    budget.charge_text(usz)?;
    budget.charge(num_tokens)?;
    let decompressed = lz4_decompress(compressed, usz)?;
    let text = String::from_utf8(decompressed).map_err(|_| UsdcError::Inconsistent {
        message: "TOKENS section contains invalid UTF-8",
    })?;
    // Each token ends at a null byte; the final null ends the last one.
    let tokens = TokenTable::of_section(text)?;

    #[allow(
        clippy::cast_possible_truncation,
        reason = "num_tokens bounded by file size"
    )]
    let expected = num_tokens as usize;
    if tokens.len() != expected {
        return Err(UsdcError::Inconsistent {
            message: "TOKENS count mismatch",
        });
    }

    Ok(tokens)
}

// ---------------------------------------------------------------------------
// STRINGS section
// ---------------------------------------------------------------------------

/// Parses the STRINGS section.
///
/// Format: `num_strings: u64`, then `num_strings × u32` token indices.
///
/// Spec: AOUSD Core §16.3.5.
fn parse_strings(
    data: &[u8],
    entry: &SectionEntry,
    budget: &mut DecodeBudget,
) -> Result<Vec<u32>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "STRINGS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num_strings = read_u64(section, 0);
    let indices = &section[8..];
    if num_strings > (indices.len() / 4) as u64 {
        return Err(UsdcError::UnexpectedEof {
            section: "STRINGS indices",
            offset: entry.offset + 8,
            expected: num_strings.saturating_mul(4),
        });
    }

    budget.charge(num_strings)?;
    Ok(indices
        .as_chunks::<4>()
        .0
        .iter()
        .take(usize::try_from(num_strings).unwrap_or(usize::MAX))
        .map(|idx| u32::from_le_bytes(*idx))
        .collect())
}

// ---------------------------------------------------------------------------
// FIELDS section
// ---------------------------------------------------------------------------

/// Parses the FIELDS section.
///
/// Format: `num_fields: u64`, compressed integer array of token indices
/// (`u32`), `reps_size: u64`, then `reps_size` bytes of LZ4-compressed
/// value reps (8 bytes each).
///
/// Spec: AOUSD Core §16.3.6.
fn parse_fields(
    data: &[u8],
    entry: &SectionEntry,
    budget: &mut DecodeBudget,
) -> Result<Vec<FieldDef>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num_fields = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "field count bounded by file size"
    )]
    let count = num_fields as usize;

    // Read compressed token indices. A unit per field covers its index, its
    // 8-byte value rep and its `FieldDef`.
    budget.charge(num_fields)?;
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "token indices are u32-range values"
    )]
    let (indices, indices_consumed) =
        read_compressed_ints_as(&section[8..], count, 4, |index| index as u32)?;

    // Read reps_size and LZ4-compressed value reps.
    let reps_start = 8 + indices_consumed;
    if section.len() < reps_start + 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDS reps_size",
            offset: entry.offset + reps_start as u64,
            expected: 8,
        });
    }

    let reps_size = read_u64(section, reps_start);
    let reps_data_start = reps_start + 8;
    let Some(reps) = usize::try_from(reps_size)
        .ok()
        .and_then(|rsz| section[reps_data_start..].get(..rsz))
    else {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDS reps data",
            offset: entry.offset + reps_data_start as u64,
            expected: reps_size,
        });
    };

    let reps_len = count.checked_mul(8).ok_or(UsdcError::Inconsistent {
        message: "FIELDS count exceeds the address space",
    })?;
    let decompressed_reps = lz4_decompress(reps, reps_len)?;
    if decompressed_reps.len() != reps_len {
        return Err(UsdcError::Inconsistent {
            message: "FIELDS value reps have the wrong size",
        });
    }

    let fields = indices
        .iter()
        .zip(decompressed_reps.as_chunks::<8>().0)
        .map(|(&token_index, rep)| FieldDef {
            token_index,
            value_rep: *rep,
        })
        .collect();

    Ok(fields)
}

// ---------------------------------------------------------------------------
// FIELDSETS section
// ---------------------------------------------------------------------------

/// Parses the FIELDSETS section.
///
/// Format: `num_fieldsets: u64`, compressed integer array of `i32` values.
/// Negative values act as group delimiters.
///
/// Spec: AOUSD Core §16.3.7.
fn parse_fieldsets(
    data: &[u8],
    entry: &SectionEntry,
    budget: &mut DecodeBudget,
) -> Result<Vec<i32>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "FIELDSETS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "fieldset count bounded by file size"
    )]
    let count = num as usize;

    budget.charge(num)?;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "fieldset values are i32-range"
    )]
    let (fieldsets, _consumed) = read_compressed_ints_as(&section[8..], count, 4, |v| v as i32)?;
    Ok(fieldsets)
}

// ---------------------------------------------------------------------------
// PATHS section
// ---------------------------------------------------------------------------

/// Decodes array `at` of the PATHS section, and with the last, checks and
/// returns the number of paths its table holds, for [`build_paths`] to
/// place them in the tree.
///
/// Format: `num_paths: u64`, `num_encoded: u64`, then three compressed
/// integer arrays of `num_encoded` entries: `path_indices`,
/// `element_token_indices`, `jumps`. Each array starts with its compressed
/// size, so array `at` is found without decoding those before it.
///
/// Spec: AOUSD Core §16.3.7.
fn parse_path_array(
    data: &[u8],
    entry: &SectionEntry,
    at: usize,
    budget: &mut DecodeBudget,
) -> Result<(Vec<i32>, Option<usize>), UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 16 {
        return Err(UsdcError::UnexpectedEof {
            section: "PATHS",
            offset: entry.offset,
            expected: 16,
        });
    }

    let num_paths = read_u64(section, 0);
    let num_encoded = read_u64(section, 8);

    #[allow(
        clippy::cast_possible_truncation,
        reason = "path counts bounded by file size"
    )]
    let n_paths = num_paths as usize;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "path counts bounded by file size"
    )]
    let n_encoded = num_encoded as usize;

    // Three index arrays of `n_encoded` entries.
    if at == 0 {
        budget.charge(num_encoded.saturating_mul(3))?;
    }
    let cursor = skip_compressed_ints(&section[16..], at)?;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "path arrays are summed in int32 arithmetic"
    )]
    let (array, _) = read_compressed_ints_as(cursor, n_encoded, 4, |v| v as i32)?;
    if at < 2 {
        return Ok((array, None));
    }

    // The table holds the encoded paths, and OpenUSD tolerates unused
    // entries. The count comes from the file, so unused entries are limited
    // to one per byte of the section before the table is allocated.
    if n_paths > n_encoded.saturating_add(section.len()) {
        return Err(UsdcError::Inconsistent {
            message: "PATHS count exceeds the encoded paths",
        });
    }
    budget.charge(num_paths)?;
    Ok((array, Some(n_paths)))
}

/// `data` after the first `count` of the compressed integer arrays it
/// starts with, each its compressed size and its bytes.
fn skip_compressed_ints(mut data: &[u8], count: usize) -> Result<&[u8], UsdcError> {
    for _ in 0..count {
        let truncated = UsdcError::UnexpectedEof {
            section: "compressed int array",
            offset: 0,
            expected: 8,
        };
        let size = data.get(..8).ok_or(truncated)?;
        let size = u64::from_le_bytes(size.try_into().expect("eight bytes"));
        let end = usize::try_from(size)
            .ok()
            .and_then(|size| size.checked_add(8))
            .filter(|end| *end <= data.len())
            .ok_or(UsdcError::UnexpectedEof {
                section: "compressed int array data",
                offset: 8,
                expected: size,
            })?;
        data = &data[end..];
    }
    Ok(data)
}

/// What a path element's text holds, for [`build_paths`] to spell a path
/// from its parent's without the text.
#[derive(Clone, Copy, Debug, Default)]
struct Element {
    len: usize,
    flags: u8,
}

impl Element {
    /// The element has been read.
    const KNOWN: u8 = 1;
    /// The text starts with `{`.
    const OPENS: u8 = 2;
    /// The text ends with `}`.
    const CLOSES: u8 = 4;
    /// The text holds a `{`.
    const OPEN: u8 = 8;
    /// The text holds a `.`.
    const DOT: u8 = 16;
    /// The text holds a `}`.
    const CLOSE: u8 = 32;
    /// The text starts with `[` or `.`: an element below a property.
    const JOINS: u8 = 64;

    fn of(text: &str) -> Self {
        let mut flags = Self::KNOWN;
        for (flag, holds) in [
            (Self::OPENS, text.starts_with('{')),
            (Self::CLOSES, text.ends_with('}')),
            (Self::OPEN, text.contains('{')),
            (Self::DOT, text.contains('.')),
            (Self::CLOSE, text.contains('}')),
            (Self::JOINS, text.starts_with(['[', '.'])),
        ] {
            if holds {
                flags |= flag;
            }
        }
        Self {
            len: text.len(),
            flags,
        }
    }

    fn has(self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

/// The [`Element`] of each token of a table, read when first asked.
struct Elements(Vec<Element>);

impl Elements {
    /// The element token `index` of `tokens` spells; `None` outside the
    /// table.
    fn of(&mut self, index: usize, tokens: &TokenTable) -> Option<Element> {
        let slot = self.0.get_mut(index)?;
        if !slot.has(Element::KNOWN) {
            *slot = Element::of(tokens.get(index)?);
        }
        Some(*slot)
    }
}

/// Iterative path reconstruction using an explicit stack.
///
/// Mirrors the Python reference `build_decompressed_paths` but avoids
/// recursion by using a work stack of `(start_index, parent)` frames.
///
/// The Python algorithm is a `do-while` loop that continues as long as
/// `has_child or has_sibling`. When there are siblings but no children
/// (`jump >= 0` and `jump != -1`, specifically `jump == 0`), the loop
/// still advances `start_index += 1` to process the next entry as a
/// sibling under the same parent.
///
/// Each path is placed in the tree, its parent and element, without its
/// text. Paths share their prefixes and tokens, so a small table can
/// describe paths far longer than the file: each path's text is charged to
/// `budget` by its length, found from its parent's, as building it would
/// be.
fn build_paths<I: Copy + Into<i64>>(
    path_indices: &[I],
    element_token_indices: &[I],
    jumps: &[I],
    tokens: &TokenTable,
    nodes: &mut [PathNode],
    budget: &mut DecodeBudget,
) -> Result<(Vec<usize>, Vec<Spelling>), UsdcError> {
    let mut spellings = vec![Spelling::default(); nodes.len()];
    if path_indices.is_empty() {
        return Ok((vec![0; nodes.len()], spellings));
    }

    let corrupt = UsdcError::PathReconstruction;
    // Like OpenUSD (`_ReadCompressedPaths`, `pxr/usd/sdf/crateFile.cpp:3935`),
    // the path indexes must be in range and distinct, and the jumps must stay
    // in the table. A valid encoding visits each entry once; a corrupt one
    // whose jumps overlap could revisit entries exponentially often.
    let mut targets = vec![false; nodes.len()];
    for &target in path_indices {
        let target = usize::try_from(target.into()).map_err(|_| UsdcError::PathReconstruction)?;
        match targets.get_mut(target) {
            Some(seen @ false) => *seen = true,
            _ => return Err(corrupt),
        }
    }
    let mut visited = vec![false; path_indices.len()];
    // What each element's text holds, found once per token: many paths
    // share an element (a property name).
    let mut elements = Elements(vec![Element::default(); tokens.len()]);
    // Each path's text length, whether it ends with `}`, and whether it is
    // `/`, as the root's is.
    let mut texts: Vec<(usize, bool, bool)> = vec![(0, false, false); nodes.len()];

    // Stack frames: (start_index, parent's index, is_first_iteration);
    // `NO_PARENT` before the root.
    const NO_PARENT: u32 = u32::MAX;
    let mut stack: Vec<(usize, u32, bool)> = vec![(0, NO_PARENT, true)];

    while let Some((mut idx, mut parent_at, mut first)) = stack.pop() {
        // do-while: always execute at least once per stack frame,
        // then continue while has_child || has_sibling.
        loop {
            if idx >= path_indices.len() {
                break;
            }
            if core::mem::replace(&mut visited[idx], true) {
                return Err(UsdcError::PathReconstruction);
            }

            #[allow(
                clippy::cast_possible_truncation,
                reason = "path indexes were checked against the table"
            )]
            let target = path_indices[idx].into() as usize;

            if first && parent_at == NO_PARENT {
                // Root node.
                texts[target] = (1, false, true);
                spellings[target] = Spelling(Spelling::ROOT);
                nodes[target] = PathNode::Root;
            } else {
                let token_idx_raw: i64 = element_token_indices[idx].into();
                let is_property = token_idx_raw < 0;
                let Some(element) = usize::try_from(token_idx_raw.unsigned_abs())
                    .ok()
                    .and_then(|token_idx| elements.of(token_idx, tokens))
                else {
                    return Err(UsdcError::PathReconstruction);
                };
                // A sibling of the root follows no parent: a corrupt
                // encoding, whose paths join the empty text.
                let (parent_len, parent_brace, parent_slash) = texts
                    .get(parent_at as usize)
                    .copied()
                    .unwrap_or((0, false, false));
                let sep = if is_property {
                    "."
                } else if element.has(Element::OPENS | Element::JOINS) || parent_brace {
                    ""
                } else {
                    "/"
                };
                budget.charge_text(parent_len + sep.len() + element.len)?;
                // As the text is spelled: a path below `/` starts with its
                // separator, and one with an empty element ends as its
                // parent's text does.
                let prefix = if parent_slash { 0 } else { parent_len };
                let len = prefix + sep.len() + element.len;
                let brace = if element.len == 0 {
                    sep.is_empty() && !parent_slash && parent_brace
                } else {
                    element.has(Element::CLOSES)
                };
                texts[target] = (len, brace, len == 1 && sep == "/");
                // Separators hold none of these but `.`, and the text
                // holds its parent's, or `/` alone.
                let parent = spellings
                    .get(parent_at as usize)
                    .copied()
                    .unwrap_or_default();
                let mut bits = parent.0 & !(Spelling::CLOSE_LAST | Spelling::ROOT);
                for (flag, holds) in [
                    (Spelling::SELECTION, element.has(Element::OPEN)),
                    (Spelling::DOT, sep == "." || element.has(Element::DOT)),
                    (Spelling::CLOSE, element.has(Element::CLOSE)),
                    (Spelling::CLOSE_LAST, brace),
                    (Spelling::ROOT, len == 1 && sep == "/"),
                ] {
                    if holds {
                        bits |= flag;
                    }
                }
                spellings[target] = Spelling(bits);
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "token indices are u32-range"
                )]
                let token = token_idx_raw.unsigned_abs() as u32;
                nodes[target] = PathNode::Child {
                    parent: parent_at,
                    token,
                    property: is_property,
                };
            }
            first = false;

            let jump: i64 = jumps[idx].into();
            let has_child = jump > 0 || jump == -1;
            let has_sibling = jump >= 0;

            if has_child {
                if has_sibling {
                    // Push sibling for later processing with the same parent.
                    let Some(sibling_idx) = usize::try_from(jump)
                        .ok()
                        .and_then(|jump| idx.checked_add(jump))
                        .filter(|sibling| *sibling < path_indices.len())
                    else {
                        return Err(UsdcError::PathReconstruction);
                    };
                    stack.push((sibling_idx, parent_at, false));
                }

                // Descend into child: advance to next index with current path
                // as new parent.
                parent_at = u32::try_from(target).map_err(|_| UsdcError::PathReconstruction)?;
                idx += 1;
            } else if has_sibling {
                // No children but has sibling: advance to the next entry
                // which is processed as a sibling under the same parent.
                idx += 1;
            } else {
                // Leaf with no siblings — done with this branch.
                break;
            }
        }
    }

    Ok((texts.into_iter().map(|(len, ..)| len).collect(), spellings))
}

// ---------------------------------------------------------------------------
// SPECS section
// ---------------------------------------------------------------------------

/// Decodes array `at` of the SPECS section.
///
/// Format: `num_specs: u64`, then three compressed integer arrays:
/// `path_indices`, `fieldset_indices`, `forms`, found as
/// [`parse_path_array`] finds the PATHS section's.
///
/// Spec: AOUSD Core §16.3.8.
fn parse_spec_array(
    data: &[u8],
    entry: &SectionEntry,
    at: usize,
    budget: &mut DecodeBudget,
) -> Result<Vec<u32>, UsdcError> {
    let section = section_slice(data, entry)?;
    if section.len() < 8 {
        return Err(UsdcError::UnexpectedEof {
            section: "SPECS",
            offset: entry.offset,
            expected: 8,
        });
    }

    let num_specs = read_u64(section, 0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "spec count bounded by file size"
    )]
    let count = num_specs as usize;

    // Three index arrays, and the specs.
    if at == 0 {
        budget.charge(num_specs.saturating_mul(4))?;
    }
    let cursor = skip_compressed_ints(&section[8..], at)?;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "spec indices and forms are u32-range values"
    )]
    let (array, _) = read_compressed_ints_as(cursor, count, 4, |v| v as u32)?;
    Ok(array)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the slice of `data` corresponding to the given section entry.
fn section_slice<'a>(data: &'a [u8], entry: &SectionEntry) -> Result<&'a [u8], UsdcError> {
    // Checked here as well as by the TOC parser, since `SectionEntry` is
    // public. Once the section lies within `data`, offsets within it plus
    // small constants cannot overflow, even on 32-bit targets.
    let range = usize::try_from(entry.offset)
        .ok()
        .zip(usize::try_from(entry.size).ok())
        .and_then(|(start, size)| Some(start..start.checked_add(size)?));
    range
        .and_then(|range| data.get(range))
        .ok_or(UsdcError::SectionOutOfBounds {
            name: String::from("section"),
            offset: entry.offset,
            size: entry.size,
        })
}

/// Reads a `u64` little-endian from `data` at `offset`. Callers check that
/// the eight bytes are present.
fn read_u64(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    /// A section's tokens are its text split at each null byte, the null
    /// ending the last token dropped, as splitting the text into strings
    /// read them.
    /// Places the paths in the tree, as parsing does, and reads every
    /// path's text into `paths`, checking it has the length the tree
    /// found for it.
    fn build_paths(
        path_indices: &[i64],
        element_token_indices: &[i64],
        jumps: &[i64],
        tokens: &TokenTable,
        paths: &mut [String],
        nodes: &mut [PathNode],
        budget: &mut DecodeBudget,
    ) -> Result<(), UsdcError> {
        let (lens, spellings) = super::build_paths(
            path_indices,
            element_token_indices,
            jumps,
            tokens,
            nodes,
            budget,
        )?;
        let table = PathTable {
            nodes: nodes.to_vec(),
            texts: (0..nodes.len()).map(|_| TextSlot::new()).collect(),
            lens,
            spellings,
        };
        for (at, path) in paths.iter_mut().enumerate() {
            *path = String::from(table.text(at, tokens).unwrap());
            assert_eq!(table.text_len(at), Some(path.len()), "{path}");
            assert_eq!(table.spelling(at), Some(Spelling::of(path)), "{path}");
        }
        Ok(())
    }

    /// Parsing the sections on threads parses what one thread does: the
    /// same tables, the same budget charged, and with too small a budget,
    /// the same error.
    #[test]
    #[cfg_attr(target_os = "wasi", ignore = "WASI runs tests without threads")]
    fn sections_on_threads_parse_what_one_thread_does() {
        use crate::writer::{Spec, SpecForm as Form, Value as V, write_crate};
        let mut specs = vec![Spec::new("/", Form::PseudoRoot)];
        for i in 0..200 {
            let path = alloc::format!("/P{i}");
            specs.push(
                Spec::new(&path, Form::Prim).with_field("typeName", V::Token("Scope".into())),
            );
            specs.push(
                Spec::new(alloc::format!("{path}.label"), Form::Attribute)
                    .with_field("default", V::String(alloc::format!("label {i}"))),
            );
        }
        let bytes = write_crate(&specs).unwrap();
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let version = header.crate_version();
        let parse = |limit: u64, threads: usize| {
            let mut budget = DecodeBudget::with_limit(limit);
            let threads = Threads::new(threads);
            let parsed = parse_sections_from(&bytes, &toc, version, &mut budget, &threads, 0);
            (parsed, budget.used())
        };
        let (one, used) = parse(u64::MAX, 1);
        let one = one.unwrap();
        for threads in [2, 4] {
            let (many, many_used) = parse(u64::MAX, threads);
            let many = many.unwrap();
            assert_eq!(many_used, used);
            assert_eq!(many.tokens, one.tokens);
            assert_eq!(many.strings, one.strings);
            assert_eq!(many.fields, one.fields);
            assert_eq!(many.fieldsets, one.fieldsets);
            assert_eq!(many.specs, one.specs);
            assert_eq!(many.paths.nodes(), one.paths.nodes());
            for at in 0..one.paths.len() {
                assert_eq!(many.path(at), one.path(at));
                assert_eq!(many.paths.spelling(at), one.paths.spelling(at));
            }
        }
        for limit in [used - 1, used / 2, used / 4, 3] {
            let (expected, _) = parse(limit, 1);
            let (parsed, _) = parse(limit, 4);
            assert_eq!(parsed.unwrap_err(), expected.unwrap_err(), "{limit}");
        }
    }

    #[test]
    fn token_tables_split_the_text_at_nulls() {
        for text in ["", "a", "a\0", "a\0b", "a\0b\0", "a\0\0", "\0", "\0\0x"] {
            let mut split: Vec<&str> = text.split('\0').collect();
            if split.last().is_some_and(|s| s.is_empty()) {
                split.pop();
            }
            let table = TokenTable::of_section(String::from(text)).unwrap();
            assert_eq!(table.iter().collect::<Vec<_>>(), split, "{text:?}");
            assert_eq!(table.len(), split.len());
            assert_eq!(table.get(split.len()), None);
        }
        let built: TokenTable = ["x", "", "y\0z"].into_iter().collect();
        assert_eq!(built.iter().collect::<Vec<_>>(), ["x", "", "y\0z"]);
    }

    #[test]
    fn build_paths_single_root() {
        // One path: the pseudo-root "/"
        let tokens: TokenTable = ["Root".to_string()].into_iter().collect();
        let path_indices = vec![0_i64];
        let element_token_indices = vec![0_i64];
        let jumps = vec![0_i64]; // leaf

        let mut paths = vec![String::new(); 1];
        let mut nodes = vec![PathNode::Unused; 1];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
            &mut nodes,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();

        assert_eq!(paths[0], "/");
    }

    #[test]
    fn build_paths_root_with_child() {
        // Two paths: "/" at index 0, "/Cube" at index 1
        // Encoded entries:
        //   [0]: path_index=0, token=0("Cube"), jump=-1 (has child, no sibling)
        //   [1]: path_index=1, token=0("Cube"), jump=0 (leaf)
        let tokens: TokenTable = ["Cube".to_string()].into_iter().collect();
        let path_indices = vec![0_i64, 1];
        let element_token_indices = vec![0_i64, 0];
        let jumps = vec![-1_i64, 0];

        let mut paths = vec![String::new(); 2];
        let mut nodes = vec![PathNode::Unused; 2];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
            &mut nodes,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();

        assert_eq!(paths[0], "/");
        assert_eq!(paths[1], "/Cube");
    }

    #[test]
    fn build_paths_property() {
        // Three paths: "/" at 0, "/Sphere" at 1, "/Sphere.radius" at 2
        let tokens: TokenTable = ["Sphere".to_string(), "radius".to_string()]
            .into_iter()
            .collect();
        let path_indices = vec![0_i64, 1, 2];
        // Entry 0: root
        // Entry 1: prim Sphere (positive token index)
        // Entry 2: property radius (negative token index = -1)
        let element_token_indices = vec![0_i64, 0, -1];
        // Entry 0: has child, no sibling → jump=-1
        // Entry 1: has child, no sibling → jump=-1
        // Entry 2: leaf → jump=0
        let jumps = vec![-1_i64, -1, 0];

        let mut paths = vec![String::new(); 3];
        let mut nodes = vec![PathNode::Unused; 3];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
            &mut nodes,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();

        assert_eq!(paths[0], "/");
        assert_eq!(paths[1], "/Sphere");
        assert_eq!(paths[2], "/Sphere.radius");
    }

    #[test]
    fn build_paths_variant_selections_attach_to_their_prim() {
        // "/", "/root", "/root{foo=}", "/root{foo=eggs}",
        // "/root{foo=eggs}Child", "/root{foo=eggs}Child.attr".
        let tokens: TokenTable = [
            "root".to_string(),
            "{foo=}".to_string(),
            "{foo=eggs}".to_string(),
            "Child".to_string(),
            "attr".to_string(),
        ]
        .into_iter()
        .collect();
        let path_indices = vec![0, 1, 2, 3, 4, 5_i64];
        let element_token_indices = vec![0, 0, 1, 2, 3, -4_i64];
        // The variant set is a leaf with a sibling (the variant); the
        // others each have one child.
        let jumps = vec![-1, -1, 0, -1, -1, -2_i64];

        let mut paths = vec![String::new(); 6];
        let mut nodes = vec![PathNode::Unused; 6];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
            &mut nodes,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();

        assert_eq!(
            paths,
            [
                "/",
                "/root",
                "/root{foo=}",
                "/root{foo=eggs}",
                "/root{foo=eggs}Child",
                "/root{foo=eggs}Child.attr",
            ]
        );
        let child = |parent, token, property| PathNode::Child {
            parent,
            token,
            property,
        };
        assert_eq!(
            nodes,
            [
                PathNode::Root,
                child(0, 0, false),
                child(1, 1, false),
                child(1, 2, false),
                child(3, 3, false),
                child(4, 4, true),
            ],
            "each path names its parent and element"
        );
    }

    /// A chain of prims named by one long token describes paths whose
    /// total length grows with the square of the depth; each path's length
    /// is charged before it is built.
    #[test]
    fn build_paths_charges_each_path_by_length() {
        let depth = 64_usize;
        let tokens: TokenTable = ["n".repeat(1024)].into_iter().collect();
        let path_indices: Vec<i64> = (0..=depth as i64).collect();
        let element_token_indices = vec![0_i64; depth + 1];
        let mut jumps = vec![-1_i64; depth + 1];
        jumps[depth] = -2; // the last prim is a leaf
        let build = |budget: &mut DecodeBudget| {
            let mut paths = vec![String::new(); depth + 1];
            let mut nodes = vec![PathNode::Unused; depth + 1];
            build_paths(
                &path_indices,
                &element_token_indices,
                &jumps,
                &tokens,
                &mut paths,
                &mut nodes,
                budget,
            )
            .map(|()| paths)
        };

        let mut budget = DecodeBudget::with_limit(u64::MAX);
        let paths = build(&mut budget).unwrap();
        let total: usize = paths.iter().map(String::len).sum();
        assert!(total > depth * depth * 1024 / 2, "{total}");
        assert!(
            budget.used() >= (total / 16 - depth) as u64,
            "{}",
            budget.used()
        );

        let limit = (total / 32) as u64;
        assert_eq!(
            build(&mut DecodeBudget::with_limit(limit)).err(),
            Some(UsdcError::DecodeBudgetExceeded { limit })
        );
    }

    /// Paths a corrupt encoding places beside the root follow no parent
    /// and join the empty text, and are charged by the lengths of the
    /// texts they spell.
    #[test]
    fn build_paths_beside_the_root_spell_what_they_did() {
        let tokens: TokenTable = ["", "a", "{v=x}"].into_iter().collect();
        let mut paths = vec![String::new(); 5];
        let mut nodes = vec![PathNode::Unused; 5];
        build_paths(
            &[0, 1, 2, 3, 4],
            &[0, 0, 1, 2, 0],
            &[0, -1, -1, -1, -2],
            &tokens,
            &mut paths,
            &mut nodes,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        assert_eq!(paths, ["/", "/", "/a", "/a{v=x}", "/a{v=x}"]);
    }

    #[test]
    fn build_paths_rejects_corrupt_encodings() {
        let tokens: TokenTable = ["a".to_string(), "b".to_string()].into_iter().collect();
        let build = |indices: &[i64], elements: &[i64], jumps: &[i64]| {
            let mut paths = vec![String::new(); 3];
            let mut nodes = vec![PathNode::Unused; 3];
            build_paths(
                indices,
                elements,
                jumps,
                &tokens,
                &mut paths,
                &mut nodes,
                &mut DecodeBudget::with_limit(u64::MAX),
            )
        };
        // Path index out of range, and repeated.
        assert!(build(&[0, 3], &[0, 0], &[-1, -2]).is_err());
        assert!(build(&[0, 0], &[0, 0], &[-1, -2]).is_err());
        // Token index out of range.
        assert!(build(&[0, 1], &[0, 2], &[-1, -2]).is_err());
        // A sibling jump past the end, or overflowing.
        assert!(build(&[0, 1], &[0, 0], &[-1, 5]).is_err());
        assert!(build(&[0, 1], &[0, 0], &[-1, i64::MAX]).is_err());
        // Jumps that revisit entries.
        assert!(build(&[0, 1, 2], &[0, 0, 1], &[-1, 1, 1]).is_err());
    }

    #[test]
    fn build_paths_multiple_property_siblings() {
        // Typical gen_bool.usdc layout:
        //   /  (path 0)
        //   /root  (path 1)
        //   /root.array  (path 2)
        //   /root.array:unset  (path 3)
        //   /root.single  (path 4)
        //   /root.unset  (path 5)
        //
        // Encoded entries:
        //   [0]: idx=0, token=0(""),      jump=-1  → root "/" (has child, no sibling)
        //   [1]: idx=1, token=1("root"),   jump=-1  → /root (has child, no sibling)
        //   [2]: idx=2, token=-2("array"), jump=0   → /root.array (no child, has sibling → advance)
        //   [3]: idx=3, token=-3("array:unset"), jump=0 → /root.array:unset (sibling → advance)
        //   [4]: idx=4, token=-4("single"),jump=0   → /root.single (sibling → advance)
        //   [5]: idx=5, token=-5("unset"), jump=-2  → /root.unset (no child, no sibling → stop)
        let tokens: TokenTable = [
            "".to_string(),
            "root".to_string(),
            "array".to_string(),
            "array:unset".to_string(),
            "single".to_string(),
            "unset".to_string(),
        ]
        .into_iter()
        .collect();
        let path_indices = vec![0, 1, 2, 3, 4, 5_i64];
        let element_token_indices = vec![0, 1, -2, -3, -4, -5_i64];
        let jumps = vec![-1, -1, 0, 0, 0, -2_i64];

        let mut paths = vec![String::new(); 6];
        let mut nodes = vec![PathNode::Unused; 6];
        build_paths(
            &path_indices,
            &element_token_indices,
            &jumps,
            &tokens,
            &mut paths,
            &mut nodes,
            &mut DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();

        assert_eq!(paths[0], "/");
        assert_eq!(paths[1], "/root");
        assert_eq!(paths[2], "/root.array");
        assert_eq!(paths[3], "/root.array:unset");
        assert_eq!(paths[4], "/root.single");
        assert_eq!(paths[5], "/root.unset");
    }
}
