// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Owned asset values and their authoring anchors; hosts own lookup and loading.
use crate::{Scene, Time};
use alloc::string::{String, ToString};
use layerstack::{AssetResolver, PropertyKind, PropertyPath, Provenance, Value};

/// A composed asset spelling with its winning authored source. IDs belong to
/// the originating store; retain the resolver's layer-location mapping too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetReference {
    /// Authored asset spelling, before host resolution or expression evaluation.
    pub authored_path: String,
    /// Winning authored source, or None for a schema fallback.
    pub source: Option<Provenance>,
}
/// An asset identifier anchored by the host's resolver, without loading it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchoredAsset {
    /// Original spelling, retained for diagnostics.
    pub authored_path: String,
    /// Host-created identifier. This does not prove that an asset exists or
    /// identify a decoded texture/profile or GPU resource.
    pub identifier: String,
    /// Winning authored source, when present.
    pub source: Option<Provenance>,
}
/// Why an attribute cannot be read as an asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetReadError {
    /// No authored or schema attribute exists at this identity.
    MissingAttribute(PropertyPath),
    /// A selected authored value is not an asset.
    InvalidValue(PropertyPath),
}
impl core::fmt::Display for AssetReadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "asset capture: {self:?}")
    }
}
impl core::error::Error for AssetReadError {}
/// Why a captured asset cannot be anchored without guessing host context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetAnchorError {
    /// An empty asset path does not request a resource.
    EmptyPath,
    /// Variable expressions require evaluation in their authoring context.
    ExpressionRequiresEvaluation,
    /// Relative identifiers require a winning authored layer.
    MissingSource,
    /// The resolver could not create an identifier (for example, an unknown
    /// authoring-layer location or unsupported package syntax).
    CannotAnchor,
}
impl core::fmt::Display for AssetAnchorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "asset anchoring: {self:?}")
    }
}
impl core::error::Error for AssetAnchorError {}
impl AssetReference {
    /// Captures an asset attribute and its winning source, independently of
    /// `StageOptions::with_provenance`. No connections are traced: pass a
    /// value-producing attribute, or use a light input's captured provider value.
    /// No value (including a default-time block) returns `None`; incompatible
    /// selected storage returns an explicit error. Default-time typed reads skip
    /// incompatible stronger opinions as `UsdAttribute::Get<SdfAssetPath>` does.
    /// AOUSD Core §9.4, §12.3, §13.3.2.4.
    pub fn read(
        scene: &Scene<'_>,
        property: PropertyPath,
        time: Time,
    ) -> Result<Option<Self>, AssetReadError> {
        let stage = scene.stage();
        let kind = stage
            .resolve_property_declaration(property.prim_path(), property.property())
            .map(|d| d.kind)
            .or_else(|| {
                stage
                    .property_definition_ref(property.prim_path(), property.property())
                    .map(|d| d.kind)
            });
        if !stage.has_prim(property.prim_path()) || kind != Some(PropertyKind::Attribute) {
            return Err(AssetReadError::MissingAttribute(property));
        }
        if time == Time::Default {
            for value in stage
                .explain_property_path(property)
                .unwrap_or_default()
                .iter()
                .filter_map(|o| o.value.default_value())
            {
                if matches!(value, Value::Blocked) {
                    return Ok(None);
                }
                if matches!(value, Value::Asset(_)) {
                    break;
                }
            }
        }
        let resolved = stage.read_property_with_provenance(property, time, |value| match value {
            Value::Asset(path) => Some(path.to_string()),
            _ => None,
        });
        if let Some(resolved) = resolved {
            return Ok(Some(Self {
                authored_path: resolved.value,
                source: resolved.provenance,
            }));
        }
        if stage
            .read_property(property, time, |v| Some(v.clone()))
            .is_some()
        {
            return Err(AssetReadError::InvalidValue(property));
        }
        Ok(None)
    }
    /// Captures already-resolved asset storage with source evidence. Returns
    /// None for other types; useful with owned input/provider values.
    #[must_use]
    pub fn from_value(value: &Value, source: Option<&Provenance>) -> Option<Self> {
        match value {
            Value::Asset(path) => Some(Self {
                authored_path: path.to_string(),
                source: source.cloned(),
            }),
            _ => None,
        }
    }
    /// Asks the host resolver to create an anchored identifier, retaining its
    /// authoring evidence. Search-path policy belongs to that resolver. Absolute
    /// paths and URIs without an authored source can be normalized directly.
    /// Expressions are reported explicitly; this operation performs no loading,
    /// decoding, color conversion or persistent resource allocation.
    /// AOUSD Core §9.2, §9.4–9.7.
    pub fn anchor(&self, resolver: &dyn AssetResolver) -> Result<AnchoredAsset, AssetAnchorError> {
        if self.authored_path.is_empty() {
            return Err(AssetAnchorError::EmptyPath);
        }
        if self.authored_path.starts_with('`') {
            return Err(AssetAnchorError::ExpressionRequiresEvaluation);
        }
        let identifier = if let Some(source) = &self.source {
            resolver.anchor_asset_path(&self.authored_path, source.layer)
        } else {
            // A source-free relative fallback cannot invent an authoring layer.
            let path = self.authored_path.replace('\\', "/");
            if !path.starts_with('/') && !path.contains(':') {
                return Err(AssetAnchorError::MissingSource);
            }
            layerstack::asset::anchor_asset_path(&path, "")
        }
        .ok_or(AssetAnchorError::CannotAnchor)?;
        Ok(AnchoredAsset {
            authored_path: self.authored_path.clone(),
            identifier,
            source: self.source.clone(),
        })
    }
}
