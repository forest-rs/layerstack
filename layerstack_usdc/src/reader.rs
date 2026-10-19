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

use alloc::vec::Vec;

use crate::{
    DecodeBudget, UsdcError, header, section,
    section::{CrateSections, FieldDef, SpecDef},
    toc,
    value_rep::{CrateValue, RawValueRep, borrow_text, decode_text_indices, decode_value_within},
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
    /// Spec indexes sorted by full authored path, without copying the paths.
    order: Vec<usize>,
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
        let mut order: Vec<_> = (0..sections.specs.len()).collect();
        let path = |i: usize| sections.paths[sections.specs[i].path_index as usize].as_str();
        order.sort_unstable_by(|&a, &b| path(a).cmp(path(b)));
        if order.windows(2).any(|pair| path(pair[0]) == path(pair[1])) {
            return Err(invalid("duplicate spec path"));
        }
        Ok(Self {
            data,
            sections,
            order,
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
        let i = self
            .order
            .binary_search_by(|&i| {
                self.sections.paths[self.sections.specs[i].path_index as usize]
                    .as_str()
                    .cmp(path)
            })
            .ok()?;
        Some(self.view(self.order[i]))
    }

    /// Enumerates authored specs in bytewise path order without decoding values.
    pub fn specs(&self) -> impl ExactSizeIterator<Item = CrateSpec<'_>> + '_ {
        self.order.iter().map(|&i| self.view(i))
    }

    /// Materializes only authored specs accepted by `include` into a layer.
    ///
    /// The immutable file keeps its tables; unselected values are not decoded
    /// or validated. No full `Layer`, interner, or value cache is constructed
    /// for the catalog first. Each spec is visited once to select it, then
    /// normal assembly applies to the selected sequence in file order.
    ///
    /// This is an **explicit authored-data projection**, not a USD population
    /// mask or dependency-closure algorithm. The caller must include owning
    /// prims, wanted properties, ancestor/variant context and required layer
    /// metadata. Missing owners are assembly diagnostics. Arcs and relationship
    /// targets are preserved; this method does not discover omitted targets.
    /// A host can keep the complete file read-only and materialize its own
    /// working-set closure into a base layer beneath a separate edit layer.
    ///
    /// All accepted specs use the ordinary assembler and asset resolver;
    /// inspect returned diagnostics and insert resolved dependencies as usual.
    /// The shared budget covers selection storage and assembly, and is not
    /// reset. AOUSD Core §7.3.6–§7.3.7 (authored specs), §16.3 (crate tables).
    ///
    /// # Errors
    /// Selected encodings cannot decode, asset resolution fails, or the
    /// caller's cumulative decode budget is exhausted.
    #[allow(
        clippy::too_many_arguments,
        reason = "same explicit assembly context as assemble"
    )]
    pub fn materialize_where(
        &self,
        layer_id: layerstack::LayerId,
        tokens: &layerstack::TokenInterner,
        paths: &layerstack::PathInterner,
        resolver: &mut dyn layerstack::AssetResolver,
        budget: &mut DecodeBudget,
        mut include: impl FnMut(CrateSpec<'_>) -> bool,
    ) -> Result<crate::AssembleResult, UsdcError> {
        let mut selected = Vec::new();
        for index in 0..self.sections.specs.len() {
            if include(self.view(index)) {
                budget.charge(1)?;
                selected.push(self.sections.specs[index]);
            }
        }
        crate::assemble::assemble_specs(
            self.data,
            &self.sections,
            &selected,
            layer_id,
            tokens,
            paths,
            resolver,
            budget,
        )
    }

    fn view(&self, index: usize) -> CrateSpec<'_> {
        CrateSpec {
            data: self.data,
            sections: &self.sections,
            spec: &self.sections.specs[index],
        }
    }
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
        &self.sections.paths[self.spec.path_index as usize]
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

    /// Borrows a `string` or `token` scalar from this file's token table.
    ///
    /// Returns `None` for other types, arrays and array edits. No text is
    /// copied and no value cache is created. The returned borrow is tied to
    /// the file; the caller can retain ownership explicitly by copying it.
    /// This is authored data, not a composed or asset-resolved value.
    ///
    /// # Errors
    /// Invalid encodings or an exhausted caller budget. As in [`Self::decode`],
    /// a text index outside the tables reads as empty text.
    pub fn read_text(&self, budget: &mut DecodeBudget) -> Result<Option<&'a str>, UsdcError> {
        borrow_text(&self.representation(), self.data, self.sections, budget)
    }

    /// Reads a `string[]` or `token[]` as compact indices borrowing the text.
    ///
    /// Copies/decompresses only one `u32` per element, without allocating a
    /// string or type-erased value per element. The view keeps no cache and
    /// borrows this file's token table. Repeated calls decode again; retain
    /// the returned view for reuse. `None` means another type or an array edit.
    /// Counts, indices and conservative text work use the same budget as
    /// [`Self::decode`]. Invalid table indices read as empty text, as there.
    ///
    /// # Errors
    /// Malformed arrays, unknown types, or an exhausted caller budget.
    pub fn read_text_array(
        &self,
        budget: &mut DecodeBudget,
    ) -> Result<Option<CrateTextArray<'a>>, UsdcError> {
        let rep = self.representation();
        let kind = rep.value_type()?;
        if !rep.is_array()
            || rep.is_array_edit()
            || !matches!(
                kind,
                crate::value_type::ValueType::String | crate::value_type::ValueType::Token
            )
        {
            return Ok(None);
        }
        let indices = decode_text_indices(&rep, self.data, self.sections, budget)?;
        Ok(Some(CrateTextArray {
            indices,
            tokens: &self.sections.tokens,
        }))
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

/// A compact text-array read: one owned index per element and borrowed text.
///
/// Constructed by [`CrateField::read_text_array`]. No per-element strings,
/// generic value objects, interners, or editable property specs are created.
#[derive(Debug)]
pub struct CrateTextArray<'a> {
    indices: Vec<u32>,
    tokens: &'a [alloc::string::String],
}
impl<'a> CrateTextArray<'a> {
    /// Number of elements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.indices.len()
    }
    /// Whether the array has no elements.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
    /// Borrow one element. An out-of-range array position is absent.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&'a str> {
        self.indices.get(index).map(|&token| self.text(token))
    }
    /// Borrow every element in authored array order, without allocating.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &'a str> + DoubleEndedIterator + '_ {
        self.indices.iter().map(|&token| self.text(token))
    }
    fn text(&self, token: u32) -> &'a str {
        self.tokens
            .get(token as usize)
            .map_or("", alloc::string::String::as_str)
    }
}

#[cfg(test)]
mod tests;
