// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `sdrMetadata` a shader, or one of its inputs or outputs, carries for
//! its shader node definition (Sdr), read and authored as text.
//!
//! OpenUSD `UsdShadeShader`, `UsdShadeInput` and `UsdShadeOutput`
//! (`GetSdrMetadata`, `SetSdrMetadataByKey`, …; `shader.cpp`, `input.cpp`,
//! `output.cpp`): the field is a dictionary; each read stringifies an
//! entry's value as `TfStringify` does, and each write authors a `string`
//! entry, a key with `:` naming a nested dictionary
//! (`UsdObject::SetMetadataByDictKey`).

use alloc::{string::String, sync::Arc, vec::Vec};
use layerstack::{FieldValue, PropertyPath, Value};

use crate::shading::{Port, PortEdit};
use crate::usd_shade::{Shader, ShaderEdit};
use crate::{MetadataError, PrimView, SchemaEdit};

/// The metadata field.
const SDR_METADATA: &str = "sdrMetadata";

/// The entries of a composed `sdrMetadata` dictionary, each value as text.
fn entries(value: Option<Value>, store: &dyn layerstack::LayerStore) -> Vec<(Arc<str>, String)> {
    let Some(Value::Dictionary(entries)) = value else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|(key, value)| (key.clone(), super::stringify::field(value, store)))
        .collect()
}

/// The entry at `key` as text: the whole dictionary for an empty key, and
/// empty text when there is no such entry.
fn entry(value: Option<Value>, key: &str, store: &dyn layerstack::LayerStore) -> String {
    let Some(value) = value else {
        return String::new();
    };
    if key.is_empty() {
        return super::stringify::value(&value, store);
    }
    crate::metadata::dictionary_entry_at(&value, key)
        .map(|entry| super::stringify::field(entry, store))
        .unwrap_or_default()
}

/// Whether `value` has an entry at `key`; an empty key asks whether the
/// field is authored at all, as `UsdObject::HasMetadataDictKey` does.
fn has_entry(value: Option<&Value>, key: &str) -> bool {
    match value {
        None => false,
        Some(_) if key.is_empty() => true,
        Some(value) => crate::metadata::dictionary_entry_at(value, key).is_some(),
    }
}

/// Checks every key a setter authors before any is, so a refusal collects
/// nothing: the empty key is refused, since C++ replaces the whole
/// dictionary with the string there. A key with an empty part names an
/// entry named `""`, as in C++.
fn check_keys<'k>(keys: impl IntoIterator<Item = &'k str>) -> Result<(), MetadataError> {
    for key in keys {
        if key.is_empty() {
            return Err(MetadataError::InvalidKeyPath(String::from(key)));
        }
    }
    Ok(())
}

impl Shader<'_> {
    /// The shader's `sdrMetadata` entries, each value as text, in
    /// dictionary order; empty when none is authored. A nested dictionary
    /// is one entry, spelled `{'key': value}`.
    ///
    /// The generated [`PrimView::sdr_metadata`] returns the dictionary
    /// itself.
    ///
    /// OpenUSD: `UsdShadeShader::GetSdrMetadata`.
    #[doc(alias = "UsdShadeShader::GetSdrMetadata")]
    #[doc(alias = "GetSdrMetadata")]
    #[must_use]
    pub fn sdr_metadata_strings(&self) -> Vec<(Arc<str>, String)> {
        let prim: &PrimView<'_> = self;
        entries(prim.metadata().value(SDR_METADATA), prim.scene().store())
    }

    /// The `sdrMetadata` entry at `key` (`:` separating nested keys) as
    /// text, and empty text when there is none. An empty key spells the
    /// whole dictionary.
    ///
    /// OpenUSD: `UsdShadeShader::GetSdrMetadataByKey`.
    #[doc(alias = "UsdShadeShader::GetSdrMetadataByKey")]
    #[doc(alias = "GetSdrMetadataByKey")]
    #[must_use]
    pub fn sdr_metadata_by_key(&self, key: &str) -> String {
        let prim: &PrimView<'_> = self;
        entry(
            prim.metadata().value(SDR_METADATA),
            key,
            prim.scene().store(),
        )
    }

    /// Whether any layer authors `sdrMetadata` on the shader, even an empty
    /// dictionary.
    ///
    /// OpenUSD: `UsdShadeShader::HasSdrMetadata`.
    #[doc(alias = "UsdShadeShader::HasSdrMetadata")]
    #[doc(alias = "HasSdrMetadata")]
    #[must_use]
    pub fn has_sdr_metadata(&self) -> bool {
        let prim: &PrimView<'_> = self;
        prim.metadata().value(SDR_METADATA).is_some()
    }

    /// Whether the shader's `sdrMetadata` has an entry at `key`; an empty
    /// key asks [`Self::has_sdr_metadata`].
    ///
    /// OpenUSD: `UsdShadeShader::HasSdrMetadataByKey`.
    #[doc(alias = "UsdShadeShader::HasSdrMetadataByKey")]
    #[doc(alias = "HasSdrMetadataByKey")]
    #[must_use]
    pub fn has_sdr_metadata_by_key(&self, key: &str) -> bool {
        let prim: &PrimView<'_> = self;
        has_entry(prim.metadata().value(SDR_METADATA).as_ref(), key)
    }
}

impl ShaderEdit {
    /// Authors each of `entries` as [`Self::set_sdr_metadata_by_key`] does,
    /// keeping the other entries. Every key is checked first.
    ///
    /// OpenUSD: `UsdShadeShader::SetSdrMetadata`.
    ///
    /// # Errors
    ///
    /// [`MetadataError::InvalidKeyPath`] for the empty key, or
    /// why the prim cannot take the field; nothing is authored.
    #[doc(alias = "UsdShadeShader::SetSdrMetadata")]
    #[doc(alias = "SetSdrMetadata")]
    pub fn set_sdr_metadata(
        &self,
        edit: &mut SchemaEdit<'_>,
        entries: &[(&str, &str)],
    ) -> Result<&Self, MetadataError> {
        check_keys(entries.iter().map(|(key, _)| *key))?;
        edit.group(|edit| {
            for (key, value) in entries {
                self.set_sdr_metadata_by_key(edit, key, value)?;
            }
            Ok(())
        })?;
        Ok(self)
    }

    /// Authors `value` as the `string` entry `key` of the shader's
    /// `sdrMetadata` in the edit target, a key with `:` naming nested
    /// dictionaries, keeping the other entries
    /// ([`crate::PrimEdit::write_metadata_by_key`]). As in C++, an empty
    /// part names an entry named `""` (`"a:"` sets the entry `""` of the
    /// dictionary `a`), which USDA writes and reads back.
    ///
    /// The empty key is refused: C++ replaces the whole dictionary with
    /// the string there, leaving a `string` in a dictionary field.
    ///
    /// OpenUSD: `UsdShadeShader::SetSdrMetadataByKey`.
    ///
    /// # Errors
    ///
    /// [`MetadataError::InvalidKeyPath`] for the empty key, or why the prim
    /// cannot take the field; nothing is authored.
    #[doc(alias = "UsdShadeShader::SetSdrMetadataByKey")]
    #[doc(alias = "SetSdrMetadataByKey")]
    pub fn set_sdr_metadata_by_key(
        &self,
        edit: &mut SchemaEdit<'_>,
        key: &str,
        value: &str,
    ) -> Result<&Self, MetadataError> {
        check_keys([key])?;
        let value = Some(FieldValue::Value(Value::String(Arc::from(value))));
        edit.author_metadata_at_key(
            layerstack::TargetPath::Prim(self.path()),
            SDR_METADATA,
            key,
            value,
        )?;
        Ok(self)
    }

    /// Clears the shader's `sdrMetadata` in the edit target.
    ///
    /// OpenUSD: `UsdShadeShader::ClearSdrMetadata`.
    ///
    /// # Errors
    ///
    /// Why the prim cannot take the field.
    #[doc(alias = "UsdShadeShader::ClearSdrMetadata")]
    #[doc(alias = "ClearSdrMetadata")]
    pub fn clear_sdr_metadata(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, MetadataError> {
        self.clear_metadata(edit, SDR_METADATA)?;
        Ok(self)
    }

    /// Erases the `sdrMetadata` entry at `key` in the edit target, removing
    /// the nested dictionaries the erase leaves empty and the field with its
    /// last entry ([`crate::PrimEdit::clear_metadata_by_key`]). Erasing an
    /// entry the edit target does not author authors nothing. Keys are split
    /// as [`Self::set_sdr_metadata_by_key`] splits them, and the empty key
    /// clears the whole field ([`Self::clear_sdr_metadata`]), as in C++.
    ///
    /// OpenUSD: `UsdShadeShader::ClearSdrMetadataByKey`.
    ///
    /// # Errors
    ///
    /// Why the prim cannot take the field.
    #[doc(alias = "UsdShadeShader::ClearSdrMetadataByKey")]
    #[doc(alias = "ClearSdrMetadataByKey")]
    pub fn clear_sdr_metadata_by_key(
        &self,
        edit: &mut SchemaEdit<'_>,
        key: &str,
    ) -> Result<&Self, MetadataError> {
        if key.is_empty() {
            return self.clear_sdr_metadata(edit);
        }
        edit.author_metadata_at_key(
            layerstack::TargetPath::Prim(self.path()),
            SDR_METADATA,
            key,
            None,
        )?;
        Ok(self)
    }
}

impl Port<'_> {
    /// The value `sdrMetadata` of this input or output, if any.
    fn sdr_metadata_value(&self) -> Option<Value> {
        let path = self.path();
        let prim = PrimView::new(self.scene(), path.prim_path());
        let name = self.scene().store().tokens().resolve(path.property());
        prim.property_metadata(name)?.value(SDR_METADATA)
    }

    /// The attribute's `sdrMetadata` entries, each value as text, as
    /// [`Shader::sdr_metadata_strings`] reads a shader's.
    ///
    /// OpenUSD: `UsdShadeInput::GetSdrMetadata`,
    /// `UsdShadeOutput::GetSdrMetadata`.
    #[doc(alias = "UsdShadeInput::GetSdrMetadata")]
    #[doc(alias = "UsdShadeOutput::GetSdrMetadata")]
    #[doc(alias = "GetSdrMetadata")]
    #[must_use]
    pub fn sdr_metadata_strings(&self) -> Vec<(Arc<str>, String)> {
        entries(self.sdr_metadata_value(), self.scene().store())
    }

    /// The attribute's `sdrMetadata` entry at `key` as text, as
    /// [`Shader::sdr_metadata_by_key`] reads a shader's.
    ///
    /// OpenUSD: `UsdShadeInput::GetSdrMetadataByKey`,
    /// `UsdShadeOutput::GetSdrMetadataByKey`.
    #[doc(alias = "UsdShadeInput::GetSdrMetadataByKey")]
    #[doc(alias = "UsdShadeOutput::GetSdrMetadataByKey")]
    #[doc(alias = "GetSdrMetadataByKey")]
    #[must_use]
    pub fn sdr_metadata_by_key(&self, key: &str) -> String {
        entry(self.sdr_metadata_value(), key, self.scene().store())
    }

    /// Whether any layer authors `sdrMetadata` on the attribute.
    ///
    /// OpenUSD: `UsdShadeInput::HasSdrMetadata`,
    /// `UsdShadeOutput::HasSdrMetadata`.
    #[doc(alias = "UsdShadeInput::HasSdrMetadata")]
    #[doc(alias = "UsdShadeOutput::HasSdrMetadata")]
    #[doc(alias = "HasSdrMetadata")]
    #[must_use]
    pub fn has_sdr_metadata(&self) -> bool {
        self.sdr_metadata_value().is_some()
    }

    /// Whether the attribute's `sdrMetadata` has an entry at `key`, as
    /// [`Shader::has_sdr_metadata_by_key`] asks a shader's.
    ///
    /// OpenUSD: `UsdShadeInput::HasSdrMetadataByKey`,
    /// `UsdShadeOutput::HasSdrMetadataByKey`.
    #[doc(alias = "UsdShadeInput::HasSdrMetadataByKey")]
    #[doc(alias = "UsdShadeOutput::HasSdrMetadataByKey")]
    #[doc(alias = "HasSdrMetadataByKey")]
    #[must_use]
    pub fn has_sdr_metadata_by_key(&self, key: &str) -> bool {
        has_entry(self.sdr_metadata_value().as_ref(), key)
    }
}

impl PortEdit {
    fn property(&self, edit: &mut SchemaEdit<'_>) -> PropertyPath {
        PropertyPath::new(self.prim(), edit.tokens().intern(self.name()))
    }

    /// Authors each of `entries` on the attribute, as
    /// [`ShaderEdit::set_sdr_metadata`] authors a shader's.
    ///
    /// OpenUSD: `UsdShadeInput::SetSdrMetadata`,
    /// `UsdShadeOutput::SetSdrMetadata`.
    ///
    /// # Errors
    ///
    /// As [`ShaderEdit::set_sdr_metadata`].
    #[doc(alias = "UsdShadeInput::SetSdrMetadata")]
    #[doc(alias = "UsdShadeOutput::SetSdrMetadata")]
    #[doc(alias = "SetSdrMetadata")]
    pub fn set_sdr_metadata(
        &self,
        edit: &mut SchemaEdit<'_>,
        entries: &[(&str, &str)],
    ) -> Result<&Self, MetadataError> {
        check_keys(entries.iter().map(|(key, _)| *key))?;
        edit.group(|edit| {
            for (key, value) in entries {
                self.set_sdr_metadata_by_key(edit, key, value)?;
            }
            Ok(())
        })?;
        Ok(self)
    }

    /// Authors `value` as the `string` entry `key` of the attribute's
    /// `sdrMetadata`, as [`ShaderEdit::set_sdr_metadata_by_key`] authors a
    /// shader's.
    ///
    /// OpenUSD: `UsdShadeInput::SetSdrMetadataByKey`,
    /// `UsdShadeOutput::SetSdrMetadataByKey`.
    ///
    /// # Errors
    ///
    /// As [`ShaderEdit::set_sdr_metadata_by_key`].
    #[doc(alias = "UsdShadeInput::SetSdrMetadataByKey")]
    #[doc(alias = "UsdShadeOutput::SetSdrMetadataByKey")]
    #[doc(alias = "SetSdrMetadataByKey")]
    pub fn set_sdr_metadata_by_key(
        &self,
        edit: &mut SchemaEdit<'_>,
        key: &str,
        value: &str,
    ) -> Result<&Self, MetadataError> {
        check_keys([key])?;
        let property = layerstack::TargetPath::Property(self.property(edit));
        let value = Some(FieldValue::Value(Value::String(Arc::from(value))));
        edit.author_metadata_at_key(property, SDR_METADATA, key, value)?;
        Ok(self)
    }

    /// Clears the attribute's `sdrMetadata` in the edit target.
    ///
    /// OpenUSD: `UsdShadeInput::ClearSdrMetadata`,
    /// `UsdShadeOutput::ClearSdrMetadata`.
    ///
    /// # Errors
    ///
    /// Why the attribute cannot take the field.
    #[doc(alias = "UsdShadeInput::ClearSdrMetadata")]
    #[doc(alias = "UsdShadeOutput::ClearSdrMetadata")]
    #[doc(alias = "ClearSdrMetadata")]
    pub fn clear_sdr_metadata(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, MetadataError> {
        edit.author_property_metadata(self.prim(), self.name(), SDR_METADATA, None)?;
        Ok(self)
    }

    /// Erases the attribute's `sdrMetadata` entry at `key`, as
    /// [`ShaderEdit::clear_sdr_metadata_by_key`] erases a shader's.
    ///
    /// OpenUSD: `UsdShadeInput::ClearSdrMetadataByKey`,
    /// `UsdShadeOutput::ClearSdrMetadataByKey`.
    ///
    /// # Errors
    ///
    /// Why the attribute cannot take the field.
    #[doc(alias = "UsdShadeInput::ClearSdrMetadataByKey")]
    #[doc(alias = "UsdShadeOutput::ClearSdrMetadataByKey")]
    #[doc(alias = "ClearSdrMetadataByKey")]
    pub fn clear_sdr_metadata_by_key(
        &self,
        edit: &mut SchemaEdit<'_>,
        key: &str,
    ) -> Result<&Self, MetadataError> {
        if key.is_empty() {
            return self.clear_sdr_metadata(edit);
        }
        let property = layerstack::TargetPath::Property(self.property(edit));
        edit.author_metadata_at_key(property, SDR_METADATA, key, None)?;
        Ok(self)
    }
}
