// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Indexed access to immutable crate files without assembling a layer.
//!
//! Opening reads the structural tables. Looking up a spec or listing its
//! fields does not unpack their values. Decoding is explicit and fallible:
//! malformed or unsupported value encodings are reported when read, not when
//! opened. Use [`crate::read_usdc`] when a fully materialized layer is needed.
//!
//! Spec: AOUSD Core §16.3.5–§16.3.10 (tables and value representations).
//! OpenUSD's `Sdf_CrateData::Has` similarly detaches a packed value only when
//! the caller requests its contents.

use alloc::string::String;
use alloc::vec::Vec;

use hashbrown::HashMap;

use crate::{
    DecodeBudget, UsdcError, header, section,
    section::{CrateSections, FieldDef, OnceSlot, PathNode, SpecDef},
    toc,
    value_rep::{CrateValue, RawValueRep, decode_value_within},
    value_type::SpecForm,
    version::CrateVersion,
};

/// An immutable file-backed view of a USDC file.
///
/// Borrows the input bytes and owns the decoded structural tables and a sorted
/// spec index. It neither resolves external assets nor constructs a `Layer`.
/// Values are decoded only by [`CrateField::decode`], using the caller's budget;
/// there is no implicit value cache. Retain decoded values when reusing them.
/// The backing bytes must remain alive and unchanged while this view is used.
///
/// ```
/// use layerstack_usdc::{CrateFile, DecodeBudget};
/// use layerstack_usdc::writer::{Spec, SpecForm, Value, write_crate};
///
/// let bytes = write_crate(&[
///     Spec::new("/", SpecForm::PseudoRoot),
///     Spec::new("/Rock", SpecForm::Prim)
///         .with_field("typeName", Value::Token("Mesh".into())),
/// ]).unwrap();
/// let mut budget = DecodeBudget::for_input(bytes.len());
/// let file = CrateFile::open(&bytes, &mut budget).unwrap();
/// let prim = file.spec("/Rock").unwrap();
/// let field = prim.field("typeName").unwrap();
/// let value = field.decode(&mut budget).unwrap();
/// ```
#[derive(Debug)]
pub struct CrateFile<'a> {
    data: &'a [u8],
    sections: CrateSections,
    /// Each spec's index by the identity of its path in the tree
    /// ([`PathIdentities`]), when the tree spells every spec path one way;
    /// `None` for a table read by its text.
    by_identity: Option<(PathIdentities, HashMap<u32, usize>)>,
    /// Spec indexes sorted by full authored path, without copying the
    /// paths: found when first asked, or at open for a table read by text.
    order: OnceSlot<Vec<usize>>,
    /// The index of each token's text, for looking a path up through the
    /// tree, found when first asked.
    token_indexes: OnceSlot<HashMap<String, u32>>,
}

/// The identity of each path a file's specs name: one per text, as each
/// path's text is its parent's followed by its element, when every
/// element is spelled one way ([`PathIdentities::element_is_plain`]).
#[derive(Debug, Default)]
struct PathIdentities {
    /// Each path table entry's identity, `UNKNOWN` until found.
    of_entry: Vec<u32>,
    /// The identity of each parent's child element: by the parent's
    /// identity, the element's token and whether it is a property.
    children: HashMap<(u32, u32, bool), u32>,
}

impl PathIdentities {
    const UNKNOWN: u32 = u32::MAX;
    /// The identity of the root, `/`.
    const ROOT: u32 = 0;
    /// The identity of an unused entry, whose text is empty.
    const EMPTY: u32 = 1;

    /// The identities of the spec paths of `sections`, or `None` when an
    /// element of one cannot be told apart from its neighbors in the text
    /// alone, so that two paths of one text could differ in the tree.
    fn of(sections: &CrateSections) -> Option<Self> {
        let nodes = sections.paths.nodes();
        if !sections.paths.has_tree() {
            return None;
        }
        let mut ids = Self {
            of_entry: alloc::vec![Self::UNKNOWN; nodes.len()],
            children: HashMap::new(),
        };
        let mut next = 2;
        let mut pending = Vec::new();
        for spec in &sections.specs {
            // The entries up to the nearest whose identity is known.
            let mut at = spec.path_index as usize;
            while ids.of_entry[at] == Self::UNKNOWN {
                pending.push(at);
                match nodes[at] {
                    PathNode::Child { parent, .. } => {
                        at = parent as usize;
                        if at >= nodes.len() {
                            return None;
                        }
                    }
                    PathNode::Root | PathNode::Unused => break,
                }
            }
            while let Some(at) = pending.pop() {
                let id = match nodes[at] {
                    PathNode::Root => Self::ROOT,
                    PathNode::Unused => Self::EMPTY,
                    PathNode::Child {
                        parent,
                        token,
                        property,
                    } => {
                        let parent_id = ids.of_entry[parent as usize];
                        let parent_node = nodes[parent as usize];
                        let element = sections.tokens.get(token as usize)?;
                        if !Self::element_is_plain(element, property, parent_node) {
                            return None;
                        }
                        *ids.children
                            .entry((parent_id, token, property))
                            .or_insert_with(|| {
                                next += 1;
                                next - 1
                            })
                    }
                };
                ids.of_entry[at] = id;
            }
        }
        Some(ids)
    }

    /// Whether `element`, a child of a path at `parent`, is spelled so
    /// that the text it joins tells it from its neighbors: a nonempty name
    /// holding no separator, or a variant selection (`{set=variant}`) on a
    /// prim below the root, and no element beneath a property.
    fn element_is_plain(element: &str, property: bool, parent: PathNode) -> bool {
        let name = !element.is_empty() && !element.contains(['/', '.', '{', '}']);
        let below_property = matches!(parent, PathNode::Child { property: true, .. });
        if below_property || parent == PathNode::Unused {
            return false;
        }
        if property {
            return name;
        }
        let selection = element
            .strip_prefix('{')
            .and_then(|inner| inner.strip_suffix('}'))
            .is_some_and(|inner| !inner.contains(['{', '}', '/', '.']));
        name || (selection && parent != PathNode::Root)
    }

    /// The identity of the path `text` spells, looking its elements up by
    /// `token_index`; `None` when no spec path is spelled so.
    fn of_text(&self, text: &str, token_index: impl Fn(&str) -> Option<u32>) -> Option<u32> {
        if text.is_empty() {
            return Some(Self::EMPTY);
        }
        let mut rest = text.strip_prefix('/')?;
        let mut id = Self::ROOT;
        if rest.is_empty() {
            return Some(id);
        }
        // Each element as the text joins it: a name after `/`, or after a
        // selection; a selection; a property after `.`, last.
        let mut after_selection = false;
        while !rest.is_empty() {
            let (element, property, tail) = if let Some(property) = rest.strip_prefix('.') {
                (property, true, "")
            } else if rest.starts_with('{') {
                let close = rest.find('}')?;
                (&rest[..=close], false, &rest[close + 1..])
            } else {
                let name = match rest.strip_prefix('/') {
                    Some(name) if !after_selection => name,
                    Some(_) => return None,
                    None if after_selection || id == Self::ROOT => rest,
                    None => return None,
                };
                let end = name.find(['/', '{', '.']).unwrap_or(name.len());
                (&name[..end], false, &name[end..])
            };
            after_selection = element.starts_with('{');
            let token = token_index(element)?;
            id = *self.children.get(&(id, token, property))?;
            rest = tail;
        }
        Some(id)
    }
}

impl<'a> CrateFile<'a> {
    /// Opens the file's structural tables without decoding field values.
    ///
    /// Charges table materialization and the lookup index to `budget`. Share
    /// this budget with subsequent field reads to bound cumulative work.
    ///
    /// # Errors
    ///
    /// Reports invalid headers, table encodings, table indexes, duplicate spec
    /// paths, or exhausted budget. Success does not validate unread values:
    /// their encodings, offsets and version requirements are checked on decode.
    pub fn open(data: &'a [u8], budget: &mut DecodeBudget) -> Result<Self, UsdcError> {
        let header = header::parse_header(data)?;
        let toc = toc::parse_toc(data, header.toc_offset)?;
        let sections = section::parse_sections(data, &toc, header.crate_version(), budget)?;
        Self::from_sections(data, sections, budget)
    }

    fn from_sections(
        data: &'a [u8],
        sections: CrateSections,
        budget: &mut DecodeBudget,
    ) -> Result<Self, UsdcError> {
        let invalid = |message| UsdcError::Inconsistent { message };
        // Validate each shared table once, not every time a spec uses it.
        // A terminating delimiter guarantees every in-range fieldset lookup
        // ends safely, including suffixes shared by different specs.
        if sections.fieldsets.last().is_some_and(|last| *last >= 0) {
            return Err(invalid("unterminated fieldset"));
        }
        for &field in &sections.fieldsets {
            if field >= 0 && field as usize >= sections.fields.len() {
                return Err(invalid("fieldset field index out of range"));
            }
        }
        for field in &sections.fields {
            if field.token_index as usize >= sections.tokens.len() {
                return Err(invalid("field name index out of range"));
            }
        }
        for spec in &sections.specs {
            if spec.path_index as usize >= sections.paths.len() {
                return Err(invalid("spec path index out of range"));
            }
            if spec.fieldset_index as usize >= sections.fieldsets.len() {
                return Err(invalid("spec fieldset index out of range"));
            }
        }
        budget.charge(sections.specs.len() as u64)?;
        // Specs are found by the identity of their paths in the tree, which
        // two specs share exactly when their texts are equal; a table
        // spelling a path more than one way is sorted by its text.
        let order = OnceSlot::new();
        let by_identity = match PathIdentities::of(&sections) {
            Some(identities) => {
                let mut specs = HashMap::with_capacity(sections.specs.len());
                for (at, spec) in sections.specs.iter().enumerate() {
                    let id = identities.of_entry[spec.path_index as usize];
                    if specs.insert(id, at).is_some() {
                        return Err(invalid("duplicate spec path"));
                    }
                }
                Some((identities, specs))
            }
            None => {
                let sorted = sorted_by_path(&sections);
                let path = |i: usize| spec_path(&sections, i);
                if sorted.windows(2).any(|pair| path(pair[0]) == path(pair[1])) {
                    return Err(invalid("duplicate spec path"));
                }
                let _ = order.set(sorted);
                None
            }
        };
        Ok(Self {
            data,
            sections,
            by_identity,
            order,
            token_indexes: OnceSlot::new(),
        })
    }

    /// The file's declared crate version.
    #[must_use]
    pub fn version(&self) -> CrateVersion {
        self.sections.version
    }

    /// Looks up a full authored spec path, including any variant selections
    /// or property suffix. This does not resolve a composed scene path.
    #[must_use]
    pub fn spec(&self, path: &str) -> Option<CrateSpec<'_>> {
        let at = match &self.by_identity {
            Some((identities, specs)) => {
                let tokens = self.token_indexes.get_or_init(|| {
                    let mut indexes = HashMap::with_capacity(self.sections.tokens.len());
                    for (index, token) in (0_u32..).zip(self.sections.tokens.iter()) {
                        indexes.entry(String::from(token)).or_insert(index);
                    }
                    indexes
                });
                let id = identities.of_text(path, |element| tokens.get(element).copied())?;
                let at = *specs.get(&id)?;
                // The tree spells the path found as the text asked for.
                (spec_path(&self.sections, at) == path).then_some(at)?
            }
            None => {
                let order = self.order();
                let i = order
                    .binary_search_by(|&i| spec_path(&self.sections, i).cmp(path))
                    .ok()?;
                order[i]
            }
        };
        Some(self.view(at))
    }

    /// Enumerates authored specs in bytewise path order without decoding values.
    pub fn specs(&self) -> impl ExactSizeIterator<Item = CrateSpec<'_>> + '_ {
        self.order().iter().map(|&i| self.view(i))
    }

    /// The specs in bytewise path order, sorted when first asked.
    fn order(&self) -> &[usize] {
        self.order.get_or_init(|| sorted_by_path(&self.sections))
    }

    fn view(&self, index: usize) -> CrateSpec<'_> {
        CrateSpec {
            data: self.data,
            sections: &self.sections,
            spec: &self.sections.specs[index],
        }
    }
}

/// The text of the path of spec `at`.
fn spec_path(sections: &CrateSections, at: usize) -> &str {
    sections
        .path(sections.specs[at].path_index as usize)
        .unwrap_or_default()
}

/// The indexes of the specs of `sections`, sorted by their paths' text.
fn sorted_by_path(sections: &CrateSections) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sections.specs.len()).collect();
    order.sort_unstable_by(|&a, &b| spec_path(sections, a).cmp(spec_path(sections, b)));
    order
}

/// A borrowed authored spec in a [`CrateFile`].
#[derive(Clone, Copy, Debug)]
pub struct CrateSpec<'a> {
    data: &'a [u8],
    sections: &'a CrateSections,
    spec: &'a SpecDef,
}

impl<'a> CrateSpec<'a> {
    /// The full authored spec path, including variant context and property name.
    #[must_use]
    pub fn path(self) -> &'a str {
        self.sections
            .path(self.spec.path_index as usize)
            .unwrap_or_default()
    }

    /// The authored spec kind (prim, attribute, variant, and so on).
    #[must_use]
    pub fn form(self) -> SpecForm {
        self.spec.form
    }

    /// Lists fields in file order without decoding their values or allocating.
    pub fn fields(self) -> impl Iterator<Item = CrateField<'a>> + 'a {
        self.sections.fieldsets[self.spec.fieldset_index as usize..]
            .iter()
            .take_while(|&&index| index >= 0)
            .map(move |&index| CrateField {
                data: self.data,
                sections: self.sections,
                field: &self.sections.fields[index as usize],
            })
    }

    /// Looks up an authored field without decoding its value.
    #[must_use]
    pub fn field(self, name: &str) -> Option<CrateField<'a>> {
        self.fields().find(|field| field.name() == name)
    }
}

/// A borrowed packed field in a [`CrateFile`].
#[derive(Clone, Copy, Debug)]
pub struct CrateField<'a> {
    data: &'a [u8],
    sections: &'a CrateSections,
    field: &'a FieldDef,
}

impl<'a> CrateField<'a> {
    /// The authored field name.
    #[must_use]
    pub fn name(&self) -> &'a str {
        &self.sections.tokens[self.field.token_index as usize]
    }

    /// The packed representation, whose type and flags can be inspected
    /// without decoding the value payload.
    #[must_use]
    pub fn representation(&self) -> RawValueRep {
        RawValueRep::new(self.field.value_rep)
    }

    /// Decodes this field into an owned crate value using the existing decoder.
    ///
    /// Each call decodes again and charges `budget`, including repeated reads
    /// of a shared representation. Retaining the returned value is the caller's
    /// responsibility. Large arrays expand to the generic [`CrateValue`] model;
    /// bulk [`crate::read_usdc`] uses a more compact intermediate representation
    /// and can be faster when most values are needed.
    ///
    /// The result preserves crate types; it is not a composed
    /// value and does not resolve asset paths or apply layer offsets.
    ///
    /// # Errors
    ///
    /// Reports malformed or unsupported values, version violations, or exhausted
    /// decode budget. A failed read may have consumed some of the budget.
    pub fn decode(&self, budget: &mut DecodeBudget) -> Result<CrateValue, UsdcError> {
        decode_value_within(&self.representation(), self.data, self.sections, budget)
    }
}

#[cfg(test)]
mod tests;
