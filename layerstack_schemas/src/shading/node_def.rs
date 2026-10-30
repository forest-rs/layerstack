// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How a shading node's `NodeDefAPI` attributes select its implementation.
//!
//! OpenUSD `UsdShadeNodeDefAPI` (`nodeDefAPI.cpp`): the generated getters
//! return what is authored; these read it the way C++ consults it, and the
//! setters author the attribute pairs C++ authors together.

use crate::diagnostic::{Describe, Names, with_store};
use crate::{
    SchemaEdit, Time,
    assets::AssetReference,
    usd_shade::{NodeDefApi, NodeDefApiEdit, NodeDefApiImplementationSource},
};
use alloc::{format, string::String, sync::Arc, vec::Vec};
use layerstack::{PathId, PropertyKind, PropertyType, Value};

const ID: &str = "info:id";
const IMPLEMENTATION_SOURCE: &str = "info:implementationSource";

/// Why a `NodeDefAPI` setter authored nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeDefError {
    /// The prim has no `info:id` attribute, authored or defined by its
    /// schemas, for [`NodeDefApiEdit::set_shader_id`] to set. C++
    /// `SetShaderId` fails there too (after authoring
    /// `info:implementationSource`; this edit authors nothing).
    MissingIdAttribute {
        /// The prim.
        prim: PathId,
    },
    /// The property is a relationship, an array or an attribute of another
    /// type than the setter authors.
    WrongPropertyType {
        /// The prim.
        prim: PathId,
        /// The property's USD name.
        property: String,
    },
}

impl NodeDefError {
    /// The error in words, the prim written as its path with `store`,
    /// the store the stage was composed from. `Display` writes the same with ids.
    #[must_use]
    pub fn display<'a>(
        &'a self,
        store: &'a dyn layerstack::LayerStore,
    ) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for NodeDefError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("cannot author the node definition: ")?;
        match self {
            Self::MissingIdAttribute { prim } => write!(
                f,
                "{} has no `info:id` attribute, authored or from its schemas",
                names.path(*prim)
            ),
            Self::WrongPropertyType { prim, property } => write!(
                f,
                "{}.{property} is a relationship, an array or of another type than the setter \
                 authors",
                names.path(*prim)
            ),
        }
    }
}
impl core::fmt::Display for NodeDefError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}

impl core::error::Error for NodeDefError {}

impl NodeDefApiImplementationSource {
    /// The source OpenUSD consults for an authored `info:implementationSource`:
    /// `authored` itself when the schema lists it, otherwise `Id`.
    ///
    /// C++ `UsdShadeNodeDefAPI::GetImplementationSource` warns about a token
    /// it does not know (or an unauthored one without a fallback) and falls
    /// back to `id`, so a shader with a misspelled source still resolves by
    /// its `info:id`. The `AllowedTokens` validation rule still reports the
    /// authored `Other` value.
    #[must_use]
    pub(crate) fn effective(authored: Option<&Self>) -> Self {
        match authored {
            Some(Self::SourceAsset) => Self::SourceAsset,
            Some(Self::SourceCode) => Self::SourceCode,
            Some(Self::Id | Self::Other(_)) | None => Self::Id,
        }
    }
}

/// The attribute naming a source of `kind` (`sourceAsset`, `sourceCode`,
/// `sourceAsset:subIdentifier`) for `source_type`: `info:<kind>` for the
/// universal (empty) type, `info:<type>:<kind>` otherwise.
///
/// OpenUSD: `_GetSourceAssetAttrName` and its siblings in `nodeDefAPI.cpp`.
fn source_attribute(source_type: &str, kind: &str) -> String {
    if source_type.is_empty() {
        format!("info:{kind}")
    } else {
        format!("info:{source_type}:{kind}")
    }
}

impl<'a> NodeDefApi<'a> {
    /// The source type whose attributes every other source type falls back
    /// to: `info:sourceAsset`, `info:sourceCode`, `info:sourceAsset:subIdentifier`.
    ///
    /// OpenUSD: `UsdShadeTokens->universalSourceType`.
    pub const UNIVERSAL_SOURCE_TYPE: &'static str = "";

    /// The implementation source OpenUSD consults for this node: the
    /// composed `info:implementationSource` when the schema lists it, and
    /// `Id` otherwise.
    /// [`Self::implementation_source`] keeps the authored value.
    #[doc(alias = "UsdShadeNodeDefAPI::GetImplementationSource")]
    #[doc(alias = "GetImplementationSource")]
    #[must_use]
    pub fn effective_implementation_source(&self) -> NodeDefApiImplementationSource {
        NodeDefApiImplementationSource::effective(self.implementation_source().as_ref())
    }

    /// The node's identifier when it is implemented by identifier: the
    /// composed `info:id` when [`Self::effective_implementation_source`] is
    /// `Id`, and `None` for a source-asset or source-code node, whose
    /// `info:id` C++ ignores. [`Self::id`] reads `info:id` regardless.
    #[doc(alias = "UsdShadeNodeDefAPI::GetShaderId")]
    #[doc(alias = "GetShaderId")]
    #[must_use]
    pub fn shader_id(&self) -> Option<&'a str> {
        (self.effective_implementation_source() == NodeDefApiImplementationSource::Id)
            .then(|| self.id())
            .flatten()
    }

    /// The node's source asset for `source_type` (such as `osl`; use
    /// [`Self::UNIVERSAL_SOURCE_TYPE`] for the universal one), with its
    /// authoring layer for anchoring ([`AssetReference::anchor`]): `None`
    /// unless the node is implemented by `sourceAsset`.
    ///
    /// `info:<type>:sourceAsset` answers when the prim has that attribute;
    /// otherwise the universal `info:sourceAsset` does. An attribute that
    /// exists without an asset value answers `None`, without falling back.
    #[doc(alias = "UsdShadeNodeDefAPI::GetSourceAsset")]
    #[doc(alias = "GetSourceAsset")]
    #[must_use]
    pub fn source_asset(&self, source_type: &str) -> Option<AssetReference> {
        if self.effective_implementation_source() != NodeDefApiImplementationSource::SourceAsset {
            return None;
        }
        let name = self.source_or_universal(source_type, "sourceAsset")?;
        let property = self.property_path(&name)?;
        AssetReference::read(&self.scene(), property, Time::Default)
            .ok()
            .flatten()
    }

    /// The sub-identifier naming one node of a source asset that defines
    /// several, for `source_type`, falling back to the universal
    /// `info:sourceAsset:subIdentifier` as [`Self::source_asset`] does:
    /// `None` unless the node is implemented by `sourceAsset`.
    #[doc(alias = "UsdShadeNodeDefAPI::GetSourceAssetSubIdentifier")]
    #[doc(alias = "GetSourceAssetSubIdentifier")]
    #[must_use]
    pub fn source_asset_subidentifier(&self, source_type: &str) -> Option<&'a str> {
        if self.effective_implementation_source() != NodeDefApiImplementationSource::SourceAsset {
            return None;
        }
        let name = self.source_or_universal(source_type, "sourceAsset:subIdentifier")?;
        self.read_value(&name, crate::value::read_token)
    }

    /// The node's inline source code for `source_type`, falling back to the
    /// universal `info:sourceCode` as [`Self::source_asset`] does: `None`
    /// unless the node is implemented by `sourceCode`.
    #[doc(alias = "UsdShadeNodeDefAPI::GetSourceCode")]
    #[doc(alias = "GetSourceCode")]
    #[must_use]
    pub fn source_code(&self, source_type: &str) -> Option<Arc<str>> {
        if self.effective_implementation_source() != NodeDefApiImplementationSource::SourceCode {
            return None;
        }
        let name = self.source_or_universal(source_type, "sourceCode")?;
        self.read_value(&name, crate::value::read_string)
    }

    /// The source types the node names a source for, in property-name
    /// order: `<type>` of each `info:<type>:sourceAsset` attribute when the
    /// node is implemented by `sourceAsset`, of each `info:<type>:sourceCode`
    /// when by `sourceCode`, and none when by identifier. The universal
    /// attributes name no type.
    #[doc(alias = "UsdShadeNodeDefAPI::GetSourceTypes")]
    #[doc(alias = "GetSourceTypes")]
    #[must_use]
    pub fn source_types(&self) -> Vec<&'a str> {
        let kind = match self.effective_implementation_source() {
            NodeDefApiImplementationSource::SourceAsset => "sourceAsset",
            NodeDefApiImplementationSource::SourceCode => "sourceCode",
            _ => return Vec::new(),
        };
        let tokens = self.scene().store().tokens();
        self.attributes_in_namespace("info")
            .into_iter()
            .filter_map(|property| {
                let name = tokens.resolve(property.property());
                // C++ tokenizes on `:`, dropping empty parts, and keeps a
                // three-part name ending in the kind.
                let mut parts = name.split(':').filter(|part| !part.is_empty());
                let (Some("info"), Some(source_type), Some(last), None) =
                    (parts.next(), parts.next(), parts.next(), parts.next())
                else {
                    return None;
                };
                (last == kind).then_some(source_type)
            })
            .collect()
    }

    /// The attribute for `kind` and `source_type` if the prim has it, else
    /// the universal one if it has that.
    fn source_or_universal(&self, source_type: &str, kind: &str) -> Option<String> {
        let name = source_attribute(source_type, kind);
        if self.has_attribute(&name) {
            return Some(name);
        }
        let universal = source_attribute(Self::UNIVERSAL_SOURCE_TYPE, kind);
        (!source_type.is_empty() && self.has_attribute(&universal)).then_some(universal)
    }
}

impl NodeDefApiEdit {
    /// Selects the node's implementation by identifier: authors `id` as
    /// `info:id` and makes `info:implementationSource` read `id`, writing it
    /// only when a layer authors another value (sparsely: an unauthored
    /// source already falls back to `id`). The generated [`Self::set_id`]
    /// authors `info:id` alone, as C++ `CreateIdAttr().Set` does, and leaves
    /// an authored `sourceAsset` in force.
    ///
    /// OpenUSD: `UsdShadeNodeDefAPI::SetShaderId`.
    ///
    /// # Errors
    ///
    /// [`NodeDefError::MissingIdAttribute`] for a prim with no `info:id`
    /// attribute, authored or defined, and
    /// [`NodeDefError::WrongPropertyType`] for one that is not a `token`;
    /// nothing is authored.
    #[doc(alias = "UsdShadeNodeDefAPI::SetShaderId")]
    #[doc(alias = "SetShaderId")]
    pub fn set_shader_id(
        &self,
        edit: &mut SchemaEdit<'_>,
        id: &str,
    ) -> Result<&Self, NodeDefError> {
        let prim = self.path();
        if edit.property_kind(prim, ID).is_none() {
            return Err(NodeDefError::MissingIdAttribute { prim });
        }
        check_uniform(edit, prim, ID, "token")?;
        check_uniform(edit, prim, IMPLEMENTATION_SOURCE, "token")?;
        let id_source = Value::Token(edit.tokens().intern("id"));
        let sparse = !edit.authors_default(prim, IMPLEMENTATION_SOURCE)
            && edit.default_value(prim, IMPLEMENTATION_SOURCE).as_ref() == Some(&id_source);
        if !sparse {
            author_uniform(edit, prim, IMPLEMENTATION_SOURCE, "token", id_source);
        }
        let id = Value::Token(edit.tokens().intern(id));
        author_uniform(edit, prim, ID, "token", id);
        Ok(self)
    }

    /// Selects the node's implementation by source asset: authors
    /// `info:implementationSource = "sourceAsset"` and `asset` as
    /// `info:<source_type>:sourceAsset` (`info:sourceAsset` for
    /// [`NodeDefApi::UNIVERSAL_SOURCE_TYPE`]), a new attribute being uniform
    /// and not `custom`.
    ///
    /// OpenUSD: `UsdShadeNodeDefAPI::SetSourceAsset`.
    ///
    /// # Errors
    ///
    /// [`NodeDefError::WrongPropertyType`] when either attribute exists with
    /// another type; nothing is authored.
    #[doc(alias = "UsdShadeNodeDefAPI::SetSourceAsset")]
    #[doc(alias = "SetSourceAsset")]
    pub fn set_source_asset(
        &self,
        edit: &mut SchemaEdit<'_>,
        asset: &str,
        source_type: &str,
    ) -> Result<&Self, NodeDefError> {
        let name = source_attribute(source_type, "sourceAsset");
        self.author_source(
            edit,
            "sourceAsset",
            &name,
            "asset",
            Value::Asset(Arc::from(asset)),
        )
    }

    /// Names the node of a source asset that defines several: authors
    /// `info:implementationSource = "sourceAsset"` and `sub_identifier` as
    /// `info:<source_type>:sourceAsset:subIdentifier`, as
    /// [`Self::set_source_asset`] authors its pair.
    ///
    /// OpenUSD: `UsdShadeNodeDefAPI::SetSourceAssetSubIdentifier`.
    ///
    /// # Errors
    ///
    /// As [`Self::set_source_asset`].
    #[doc(alias = "UsdShadeNodeDefAPI::SetSourceAssetSubIdentifier")]
    #[doc(alias = "SetSourceAssetSubIdentifier")]
    pub fn set_source_asset_subidentifier(
        &self,
        edit: &mut SchemaEdit<'_>,
        sub_identifier: &str,
        source_type: &str,
    ) -> Result<&Self, NodeDefError> {
        let name = source_attribute(source_type, "sourceAsset:subIdentifier");
        let value = Value::Token(edit.tokens().intern(sub_identifier));
        self.author_source(edit, "sourceAsset", &name, "token", value)
    }

    /// Selects the node's implementation by inline source code: authors
    /// `info:implementationSource = "sourceCode"` and `code` as
    /// `info:<source_type>:sourceCode`, as [`Self::set_source_asset`]
    /// authors its pair.
    ///
    /// OpenUSD: `UsdShadeNodeDefAPI::SetSourceCode`.
    ///
    /// # Errors
    ///
    /// As [`Self::set_source_asset`].
    #[doc(alias = "UsdShadeNodeDefAPI::SetSourceCode")]
    #[doc(alias = "SetSourceCode")]
    pub fn set_source_code(
        &self,
        edit: &mut SchemaEdit<'_>,
        code: &str,
        source_type: &str,
    ) -> Result<&Self, NodeDefError> {
        let name = source_attribute(source_type, "sourceCode");
        self.author_source(
            edit,
            "sourceCode",
            &name,
            "string",
            Value::String(Arc::from(code)),
        )
    }

    /// Authors `info:implementationSource = source` and `value` as `name`,
    /// both written whatever the composed values (C++ writes them densely).
    fn author_source(
        &self,
        edit: &mut SchemaEdit<'_>,
        source: &str,
        name: &str,
        ty: &str,
        value: Value,
    ) -> Result<&Self, NodeDefError> {
        let prim = self.path();
        check_uniform(edit, prim, IMPLEMENTATION_SOURCE, "token")?;
        check_uniform(edit, prim, name, ty)?;
        let source = Value::Token(edit.tokens().intern(source));
        author_uniform(edit, prim, IMPLEMENTATION_SOURCE, "token", source);
        author_uniform(edit, prim, name, ty, value);
        Ok(self)
    }
}

/// Refuses a property `name` that exists as anything but a scalar attribute
/// of type `ty`.
fn check_uniform(
    edit: &mut SchemaEdit<'_>,
    prim: PathId,
    name: &str,
    ty: &str,
) -> Result<(), NodeDefError> {
    let kind = edit.property_kind(prim, name);
    let declared = edit.attribute_type(prim, name);
    if kind.is_some_and(|kind| kind != PropertyKind::Attribute)
        || declared
            .as_ref()
            .is_some_and(|declared| declared.is_array() || declared.type_name() != ty)
    {
        return Err(NodeDefError::WrongPropertyType {
            prim,
            property: String::from(name),
        });
    }
    Ok(())
}

/// Authors `value` as the default of `name`, first declaring it a uniform,
/// not `custom` attribute of type `ty` when neither a layer nor a schema
/// declares it, as C++ `UsdSchemaBase::_CreateAttr` creates it.
fn author_uniform(edit: &mut SchemaEdit<'_>, prim: PathId, name: &str, ty: &str, value: Value) {
    if edit.attribute_type(prim, name).is_none() {
        let ty = PropertyType::parse(ty).expect("a registered value type");
        edit.create_uniform_attribute(prim, name, ty);
    }
    edit.set_value(prim, name, None, value);
}
