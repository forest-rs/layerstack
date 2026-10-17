// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Dynamic file formats: formats whose layers are read with arguments that
//! composition computes from the scene description around a payload.
//!
//! A payload to an asset of a dynamic format does not name its layer by
//! its asset path alone. When composition follows the payload, it asks the
//! format for the arguments of that payload ([`DynamicFileFormat::compose_args`]),
//! giving it a [`DynamicFormatContext`] that composes prim metadata fields
//! and attribute defaults as they stand at that point of indexing: the
//! opinions of the nodes already in the prim index, ancestors first, and
//! of the arcs to the prim stronger than the payload. The arguments join
//! those the asset path carries ([`FormatArgs`]), and the layer they name
//! is the payload's target: [`LayerStore::asset_availability`] of the
//! argument-carrying identifier, anchored to the layer authoring the
//! payload. One the store does not hold yet is asked for
//! ([`crate::Stage::asset_requests`]), for the host to read through the
//! format and compose again.
//!
//! Formats are registered explicitly in [`DynamicFormats`], passed through
//! [`crate::StageOptions::dynamic_formats`]; there is no global registry.
//! A format may compose only plugin metadata: fields a
//! [`crate::SchemaRegistry`] in [`crate::StageOptions::schemas`] registers
//! ([`crate::SchemaRegistryBuilder::register_metadata`]). The fields and
//! attributes a payload's arguments were composed from are recorded with
//! the prim index ([`DynamicDependency`], [`crate::Stage::dynamic_dependency`])
//! so that editing them recomposes the prims that read them, and only those.
//!
//! References never compose dynamic arguments, as in OpenUSD.
//!
//! OpenUSD: `PcpDynamicFileFormatInterface`
//! (`pxr/usd/pcp/dynamicFileFormatInterface.h`), `PcpDynamicFileFormatContext`
//! (`pxr/usd/pcp/dynamicFileFormatContext.cpp`),
//! `PcpDynamicFileFormatDependencyData`
//! (`pxr/usd/pcp/dynamicFileFormatDependencyData.cpp`), and
//! `_ComposeFieldsForFileFormatArguments` in `pxr/usd/pcp/primIndex.cpp`.
//! AOUSD Core §9 (asset resolution), §10.3.2.2 (payloads).
//!
//! [`LayerStore::asset_availability`]: crate::LayerStore::asset_availability

use alloc::{
    collections::BTreeSet,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::fmt;

use crate::{
    asset::FormatArgs,
    doc::{LayerId, Value},
    path::PathId,
    prim_indexing::dynamic_args::Walk,
};

/// A file format whose layers are read with arguments composed from the
/// scene description around each payload to them.
///
/// Composition calls [`Self::compose_args`] as it follows a payload whose
/// asset path has one of [`Self::extensions`]; the arguments name the layer
/// the payload targets. Change processing calls
/// [`Self::can_field_change_affect_args`] and
/// [`Self::can_attribute_default_change_affect_args`] with the data
/// `compose_args` returned, to skip recomposing for edits that cannot change
/// the arguments.
///
/// Calls may run on several threads at once, and must be deterministic.
///
/// OpenUSD: `PcpDynamicFileFormatInterface`.
pub trait DynamicFileFormat: fmt::Debug + Send + Sync {
    /// The format's name, for diagnostics.
    fn name(&self) -> &str;

    /// The file extensions it reads, without the dot, in lowercase.
    fn extensions(&self) -> &[&str];

    /// The arguments to read `asset_path` with for the payload being
    /// composed, from the metadata fields and attribute defaults `context`
    /// composes.
    ///
    /// `asset_path` is the payload's asset path as authored, with any
    /// format arguments it carries; those are kept, and the returned
    /// arguments replace them key by key.
    ///
    /// OpenUSD: `PcpDynamicFileFormatInterface::ComposeFieldsForFileFormatArguments`.
    fn compose_args(&self, asset_path: &str, context: &mut DynamicFormatContext<'_>)
    -> DynamicArgs;

    /// Whether changing the metadata field `field` from `old` to `new`
    /// (`None` where unauthored) may change the arguments a call to
    /// [`Self::compose_args`] that returned `dependency` as its
    /// [`DynamicArgs::dependency`] composed. The default, `true`, recomposes
    /// for every change of a field the call read.
    ///
    /// OpenUSD: `PcpDynamicFileFormatInterface::CanFieldChangeAffectFileFormatArguments`.
    fn can_field_change_affect_args(
        &self,
        field: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        let _ = (field, old, new, dependency);
        true
    }

    /// Whether changing the default of the attribute `attribute` from
    /// `old` to `new` may change the arguments, as
    /// [`Self::can_field_change_affect_args`] does for fields. The default
    /// is `true`.
    ///
    /// OpenUSD:
    /// `PcpDynamicFileFormatInterface::CanAttributeDefaultValueChangeAffectFileFormatArguments`.
    fn can_attribute_default_change_affect_args(
        &self,
        attribute: &str,
        old: Option<&Value>,
        new: Option<&Value>,
        dependency: &Value,
    ) -> bool {
        let _ = (attribute, old, new, dependency);
        true
    }
}

/// What [`DynamicFileFormat::compose_args`] composed for one payload.
#[derive(Clone, Debug, PartialEq)]
pub struct DynamicArgs {
    /// The arguments to read the payload's asset with.
    pub args: FormatArgs,
    /// Data the format wants back when change processing asks whether an
    /// edit can affect these arguments; [`Value::Null`] for none.
    ///
    /// OpenUSD: the `dependencyContextData` of
    /// `ComposeFieldsForFileFormatArguments`.
    pub dependency: Value,
}

impl Default for DynamicArgs {
    fn default() -> Self {
        FormatArgs::new().into()
    }
}

impl From<FormatArgs> for DynamicArgs {
    fn from(args: FormatArgs) -> Self {
        Self {
            args,
            dependency: Value::Null,
        }
    }
}

/// Composes metadata fields and attribute defaults for a
/// [`DynamicFileFormat`] at the point of prim indexing where a payload to
/// it is being added.
///
/// The opinions read are those of the prim index built so far, in strength
/// order: the nodes the payload's node will descend from (ancestors first,
/// across the indexes of the arcs whose target sites are being indexed
/// recursively), and their subtrees, except the arcs of the payload's parent
/// weaker than the payload. Every field and attribute composed is recorded
/// as an input of the payload's arguments ([`DynamicDependency`]).
///
/// OpenUSD: `PcpDynamicFileFormatContext`.
pub struct DynamicFormatContext<'a> {
    walk: Walk<'a>,
    fields: BTreeSet<Arc<str>>,
    attributes: BTreeSet<Arc<str>>,
    sites: BTreeSet<(LayerId, PathId)>,
}

impl fmt::Debug for DynamicFormatContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicFormatContext")
            .field("fields", &self.fields)
            .field("attributes", &self.attributes)
            .finish_non_exhaustive()
    }
}

/// A field a [`DynamicFormatContext`] cannot compose: one no
/// [`crate::SchemaRegistry`] registers as plugin metadata.
///
/// Only plugin metadata may supply dynamic arguments, so that change
/// processing of built-in fields (composition arcs, variant selections,
/// `active`, ...) need not consider them. OpenUSD reports the same as a
/// coding error (`_IsAllowedFieldForArguments`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnregisteredField {
    /// The field's name.
    pub field: String,
}

impl fmt::Display for UnregisteredField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "field {} is not registered plugin metadata and cannot compose dynamic file format arguments",
            self.field
        )
    }
}

impl core::error::Error for UnregisteredField {}

impl<'a> DynamicFormatContext<'a> {
    pub(crate) fn new(walk: Walk<'a>) -> Self {
        Self {
            walk,
            fields: BTreeSet::new(),
            attributes: BTreeSet::new(),
            sites: BTreeSet::new(),
        }
    }

    /// The strongest opinion of the prim metadata field `field`, or `None`
    /// when no node authors it. A dictionary-valued field composes key by
    /// key, each from its strongest opinion, recursively.
    ///
    /// An asset path that is a variable expression is evaluated with the
    /// expression variables of the layer stack that authors it.
    ///
    /// # Errors
    ///
    /// [`UnregisteredField`] when `field` is not registered plugin metadata;
    /// it is then not recorded as an input.
    ///
    /// OpenUSD: `PcpDynamicFileFormatContext::ComposeValue`.
    pub fn compose_value(&mut self, field: &str) -> Result<Option<Value>, UnregisteredField> {
        let dictionary = self.registered(field)?;
        self.fields.insert(Arc::from(field));
        if dictionary {
            let mut opinions = Vec::new();
            self.walk
                .compose(None, field, false, &mut self.sites, &mut |value| {
                    if let Value::Dictionary(entries) = value {
                        opinions.push(entries);
                    }
                });
            if opinions.is_empty() {
                return Ok(None);
            }
            return Ok(Some(Value::Dictionary(
                crate::doc::combine_dictionary_chain(opinions),
            )));
        }
        let mut strongest = None;
        self.walk
            .compose(None, field, true, &mut self.sites, &mut |value| {
                strongest = Some(value);
            });
        Ok(strongest)
    }

    /// Every opinion of the prim metadata field `field`, strongest first;
    /// dictionaries are returned as authored, not combined.
    ///
    /// # Errors
    ///
    /// As [`Self::compose_value`].
    ///
    /// OpenUSD: `PcpDynamicFileFormatContext::ComposeValueStack`.
    pub fn compose_value_stack(&mut self, field: &str) -> Result<Vec<Value>, UnregisteredField> {
        self.registered(field)?;
        self.fields.insert(Arc::from(field));
        let mut values = Vec::new();
        self.walk
            .compose(None, field, false, &mut self.sites, &mut |value| {
                values.push(value);
            });
        Ok(values)
    }

    /// The strongest authored default of the attribute `attribute`, or
    /// `None` when no node authors one.
    ///
    /// OpenUSD: `PcpDynamicFileFormatContext::ComposeAttributeDefaultValue`.
    pub fn compose_attribute_default(&mut self, attribute: &str) -> Option<Value> {
        self.attributes.insert(Arc::from(attribute));
        let mut strongest = None;
        self.walk
            .compose(Some(attribute), "", true, &mut self.sites, &mut |value| {
                strongest = Some(value);
            });
        strongest
    }

    /// Whether `field` is registered plugin metadata, and dictionary-valued.
    /// OpenUSD: `_IsAllowedFieldForArguments`.
    fn registered(&self, field: &str) -> Result<bool, UnregisteredField> {
        self.walk
            .registered_metadata(field)
            .map(|definition| &*definition.type_name == "dictionary")
            .ok_or_else(|| UnregisteredField {
                field: field.to_string(),
            })
    }

    /// The inputs this context composed, for `format`'s `composed` call.
    pub(crate) fn into_dependency(
        self,
        format: Arc<dyn DynamicFileFormat>,
        data: Value,
    ) -> DynamicDependency {
        DynamicDependency {
            contexts: alloc::vec![(format, data)],
            fields: self.fields,
            attributes: self.attributes,
            sites: self.sites,
            targets: BTreeSet::new(),
        }
    }
}

/// Why a [`DynamicFileFormat`] could not be registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicFormatRegistrationError {
    /// The format claims no extension.
    NoExtensions {
        /// The format's name.
        format: String,
    },
    /// A built-in format (`usda`, `usdc`, `usd`, `usdz`) reads the
    /// extension.
    BuiltinExtension {
        /// The format's name.
        format: String,
        /// The extension.
        extension: String,
    },
    /// Another registered format already claims the extension.
    DuplicateExtension {
        /// The format's name.
        format: String,
        /// The extension.
        extension: String,
        /// The format registered for it first.
        registered: String,
    },
    /// An extension is empty, has a dot, or has uppercase letters.
    InvalidExtension {
        /// The format's name.
        format: String,
        /// The extension.
        extension: String,
    },
}

impl fmt::Display for DynamicFormatRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoExtensions { format } => {
                write!(f, "dynamic format {format} claims no extension")
            }
            Self::BuiltinExtension { format, extension } => {
                write!(
                    f,
                    "dynamic format {format} claims built-in extension .{extension}"
                )
            }
            Self::DuplicateExtension {
                format,
                extension,
                registered,
            } => write!(
                f,
                "dynamic format {format} claims .{extension}, which {registered} claims"
            ),
            Self::InvalidExtension { format, extension } => write!(
                f,
                "dynamic format {format} claims `{extension}`, which is not a lowercase extension"
            ),
        }
    }
}

impl core::error::Error for DynamicFormatRegistrationError {}

/// The dynamic file formats a stage composes payloads with, by extension.
/// Cloning shares the formats.
#[derive(Clone, Debug, Default)]
pub struct DynamicFormats {
    formats: Vec<Arc<dyn DynamicFileFormat>>,
}

impl DynamicFormats {
    /// Registers `format` for its extensions. Registering a format already
    /// registered (the same `Arc`) does nothing.
    ///
    /// # Errors
    ///
    /// When it claims no extension, an invalid one, a built-in one, or one
    /// another registered format claims; nothing is registered then.
    pub fn add(
        &mut self,
        format: Arc<dyn DynamicFileFormat>,
    ) -> Result<(), DynamicFormatRegistrationError> {
        if self
            .formats
            .iter()
            .any(|registered| Arc::ptr_eq(registered, &format))
        {
            return Ok(());
        }
        let name = || format.name().to_string();
        if format.extensions().is_empty() {
            return Err(DynamicFormatRegistrationError::NoExtensions { format: name() });
        }
        for &extension in format.extensions() {
            if extension.is_empty()
                || extension.contains('.')
                || extension.chars().any(char::is_uppercase)
            {
                return Err(DynamicFormatRegistrationError::InvalidExtension {
                    format: name(),
                    extension: extension.into(),
                });
            }
            if matches!(extension, "usda" | "usdc" | "usd" | "usdz") {
                return Err(DynamicFormatRegistrationError::BuiltinExtension {
                    format: name(),
                    extension: extension.into(),
                });
            }
            if let Some(registered) = self.position(extension) {
                return Err(DynamicFormatRegistrationError::DuplicateExtension {
                    format: name(),
                    extension: extension.into(),
                    registered: self.formats[registered].name().into(),
                });
            }
        }
        self.formats.push(format);
        Ok(())
    }

    /// These formats and `format` (see [`Self::add`]).
    ///
    /// # Errors
    ///
    /// As [`Self::add`].
    pub fn with(
        mut self,
        format: Arc<dyn DynamicFileFormat>,
    ) -> Result<Self, DynamicFormatRegistrationError> {
        self.add(format)?;
        Ok(self)
    }

    /// The registered formats, in registration order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &dyn DynamicFileFormat> + '_ {
        self.formats.iter().map(|format| &**format)
    }

    /// Whether no format is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.formats.is_empty()
    }

    /// The format of the asset `asset_path` names, by the extension of its
    /// path without format arguments; `None` for a package member or an
    /// extension no format claims.
    ///
    /// OpenUSD: `_GetDynamicFileFormat` in `pxr/usd/pcp/primIndex.cpp`.
    #[must_use]
    pub fn for_asset(&self, asset_path: &str) -> Option<&dyn DynamicFileFormat> {
        self.shared_for_asset(asset_path).map(|format| &**format)
    }

    pub(crate) fn shared_for_asset(&self, asset_path: &str) -> Option<&Arc<dyn DynamicFileFormat>> {
        if self.formats.is_empty() {
            return None;
        }
        let path = FormatArgs::strip(asset_path);
        if path.ends_with(']') {
            return None;
        }
        let leaf = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let (_, extension) = leaf.rsplit_once('.')?;
        self.position(extension).map(|at| &self.formats[at])
    }

    fn position(&self, extension: &str) -> Option<usize> {
        self.formats.iter().position(|format| {
            format
                .extensions()
                .iter()
                .any(|claimed| claimed.eq_ignore_ascii_case(extension))
        })
    }
}

impl PartialEq for DynamicFormats {
    /// The same formats, by identity, in the same order.
    fn eq(&self, other: &Self) -> bool {
        self.formats.len() == other.formats.len()
            && self
                .formats
                .iter()
                .zip(&other.formats)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    }
}

/// What the dynamic payload arguments of one prim index were composed from:
/// the fields and attributes read, and each format's dependency data.
///
/// A prim index records one for the payloads to dynamic formats its own
/// indexing composed arguments for, including those of ancestral sites
/// indexed for its arcs; the payloads its namespace parent composed are the
/// parent's. Read it with [`crate::Stage::dynamic_dependency`].
///
/// OpenUSD: `PcpDynamicFileFormatDependencyData`, read through
/// `PcpCache::GetDynamicFileFormatArgumentDependencyData`.
#[derive(Clone, Default)]
pub struct DynamicDependency {
    contexts: Vec<(Arc<dyn DynamicFileFormat>, Value)>,
    fields: BTreeSet<Arc<str>>,
    attributes: BTreeSet<Arc<str>>,
    /// The `(layer, prim path)` of every spec the contexts read from.
    sites: BTreeSet<(LayerId, PathId)>,
    /// The layers the payloads target, as the store bound them.
    pub(crate) targets: BTreeSet<LayerId>,
}

impl fmt::Debug for DynamicDependency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicDependency")
            .field(
                "formats",
                &self
                    .contexts
                    .iter()
                    .map(|(format, data)| (format.name(), data))
                    .collect::<Vec<_>>(),
            )
            .field("fields", &self.fields)
            .field("attributes", &self.attributes)
            .finish_non_exhaustive()
    }
}

impl DynamicDependency {
    /// Whether no payload argument was composed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }

    /// The names of the metadata fields the arguments were composed from,
    /// sorted.
    ///
    /// OpenUSD: `PcpDynamicFileFormatDependencyData::GetRelevantFieldNames`.
    pub fn relevant_fields(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.fields.iter().map(|name| &**name)
    }

    /// The names of the attributes whose defaults the arguments were
    /// composed from, sorted.
    ///
    /// OpenUSD: `PcpDynamicFileFormatDependencyData::GetRelevantAttributeNames`.
    pub fn relevant_attributes(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.attributes.iter().map(|name| &**name)
    }

    /// Each argument composition: its format and the dependency data the
    /// format returned, in the order indexing composed them.
    pub fn compositions(&self) -> impl ExactSizeIterator<Item = (&dyn DynamicFileFormat, &Value)> {
        self.contexts.iter().map(|(format, data)| (&**format, data))
    }

    /// The `(layer, prim path)` of every spec the arguments' fields and
    /// attribute defaults were read from (whether or not it authored them),
    /// sorted: the sites whose edits may change them.
    pub fn sites(&self) -> impl ExactSizeIterator<Item = (LayerId, PathId)> + '_ {
        self.sites.iter().copied()
    }

    /// The root layers of the layer stacks the payloads target, as the
    /// store binds their argument-carrying identifiers, sorted; a payload
    /// whose layer the store does not hold yet targets none.
    pub fn target_layers(&self) -> impl ExactSizeIterator<Item = LayerId> + '_ {
        self.targets.iter().copied()
    }

    /// Whether changing the field `field` from `old` to `new` may change
    /// any of the arguments: the field is relevant and a composition's
    /// format says it may ([`DynamicFileFormat::can_field_change_affect_args`]).
    ///
    /// OpenUSD: `PcpDynamicFileFormatDependencyData::CanFieldChangeAffectFileFormatArguments`.
    #[must_use]
    pub fn can_field_change_affect_args(
        &self,
        field: &str,
        old: Option<&Value>,
        new: Option<&Value>,
    ) -> bool {
        self.fields.contains(field)
            && self
                .contexts
                .iter()
                .any(|(format, data)| format.can_field_change_affect_args(field, old, new, data))
    }

    /// Whether changing the default of `attribute` from `old` to `new` may
    /// change any of the arguments, as
    /// [`Self::can_field_change_affect_args`] does for fields.
    ///
    /// OpenUSD:
    /// `PcpDynamicFileFormatDependencyData::CanAttributeDefaultValueChangeAffectFileFormatArguments`.
    #[must_use]
    pub fn can_attribute_default_change_affect_args(
        &self,
        attribute: &str,
        old: Option<&Value>,
        new: Option<&Value>,
    ) -> bool {
        self.attributes.contains(attribute)
            && self.contexts.iter().any(|(format, data)| {
                format.can_attribute_default_change_affect_args(attribute, old, new, data)
            })
    }

    /// Adds `other`'s compositions and inputs to these.
    /// OpenUSD: `PcpDynamicFileFormatDependencyData::AppendDependencyData`.
    pub(crate) fn append(&mut self, other: Self) {
        self.contexts.extend(other.contexts);
        self.fields.extend(other.fields);
        self.attributes.extend(other.attributes);
        self.sites.extend(other.sites);
        self.targets.extend(other.targets);
    }
}

#[cfg(test)]
mod tests;
