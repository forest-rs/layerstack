// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `UsdShade` and `UsdLux` connectability policies: OpenUSD's built-in
//! behaviors, and the default behavior a schema's plugin metadata
//! configures.
use crate::{PrimView, Scene};
use layerstack::{
    PathId, PropertyKind, PropertyPath, SchemaDefinition, SchemaKind, SchemaRegistry, TokenId,
    TokenInterner, Value,
};

/// How a prim's inputs and outputs may connect: one of OpenUSD's built-in
/// behaviors (`Material`, `NodeGraph`, `Shader`, `LightFilter`,
/// `LightAPI`), or the default behavior a schema's plugin metadata
/// configures ([`Scene::connectable_behavior`]). Behaviors registered by
/// C++ code of other plugins are not loaded.
///
/// OpenUSD: `UsdShadeConnectableAPIBehavior`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectableBehavior {
    /// Interface inputs and outputs can forward to upstream providers.
    pub is_container: bool,
    /// Sources must obey the owner's container boundary.
    pub requires_encapsulation: bool,
    /// Outputs may be destinations. Shader and light-filter outputs may not.
    pub outputs_connectable: bool,
    /// Derived containers forbid output-to-input passthrough and use internal nodes.
    pub derived_container: bool,
}
/// Why a proposed shading connection violates built-in policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionIssue {
    /// The destination is not an existing shading attribute.
    InvalidDestination,
    /// The source is not an existing attribute.
    InvalidSource,
    /// No supported connectability behavior exists for this destination prim.
    UnsupportedBehavior,
    /// This prim's outputs may not receive connections.
    OutputNotConnectable,
    /// An interface-only input requires an interface-only input source.
    InterfaceOnly,
    /// An input declares a non-empty connectability token other than
    /// full/interfaceOnly (an empty one is `full`, as in C++).
    UnknownConnectability,
    /// Derived containers forbid output-to-input passthrough.
    PassthroughForbidden,
    /// The source violates the owner's container boundary.
    Encapsulation,
}
/// Precise rejection of a proposed connection; no scene edits occur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionError {
    /// Destination input/output.
    pub destination: PropertyPath,
    /// Proposed source attribute.
    pub source: PropertyPath,
    /// Machine-readable failed invariant.
    pub issue: ConnectionIssue,
}
impl core::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "connection {:?} <- {:?}: {:?}",
            self.destination, self.source, self.issue
        )
    }
}
impl core::error::Error for ConnectionError {}
/// The plugin metadata key that gives a schema a default connectable
/// behavior (`plugInfo.json`, `SchemaDefinition::plug_info`).
const PROVIDES_CONNECTABLE_BEHAVIOR: &str = "providesUsdShadeConnectableAPIBehavior";
/// The plugin metadata key that makes a schema's default connectable
/// behavior a container; `false` when absent or not a boolean.
const IS_CONTAINER: &str = "isUsdShadeContainer";
/// The plugin metadata key that sets whether a schema's default
/// connectable behavior requires encapsulation; `true` when absent or not
/// a boolean.
const REQUIRES_ENCAPSULATION: &str = "requiresUsdShadeEncapsulation";

impl ConnectableBehavior {
    const MATERIAL: Self = Self {
        is_container: true,
        requires_encapsulation: true,
        outputs_connectable: true,
        derived_container: true,
    };
    const NODE_GRAPH: Self = Self {
        is_container: true,
        requires_encapsulation: true,
        outputs_connectable: true,
        derived_container: false,
    };
    const SHADER: Self = Self {
        is_container: false,
        requires_encapsulation: true,
        outputs_connectable: false,
        derived_container: false,
    };
    #[cfg(feature = "usd-lux")]
    const LIGHT_FILTER: Self = Self {
        is_container: true,
        requires_encapsulation: true,
        outputs_connectable: false,
        derived_container: true,
    };
    #[cfg(feature = "usd-lux")]
    const LIGHT_API: Self = Self {
        is_container: true,
        requires_encapsulation: false,
        outputs_connectable: true,
        derived_container: true,
    };

    /// The default behavior `schema`'s plugin metadata configures, if its
    /// `providesUsdShadeConnectableAPIBehavior` is `true`: a container when
    /// `isUsdShadeContainer` is `true`, requiring encapsulation unless
    /// `requiresUsdShadeEncapsulation` is `false` (a key that is not a
    /// boolean counts as absent), whose outputs accept connections and
    /// whose inputs connect as a basic node's.
    ///
    /// ```
    /// use layerstack::{SchemaDefinition, TokenInterner};
    /// use layerstack_schemas::shading::ConnectableBehavior;
    ///
    /// let tokens = TokenInterner::default();
    /// let mixer = SchemaDefinition::typed(tokens.intern("Mixer"))
    ///     .with_plug_info(tokens.intern("providesUsdShadeConnectableAPIBehavior"), true)
    ///     .with_plug_info(tokens.intern("isUsdShadeContainer"), true);
    /// let behavior = ConnectableBehavior::from_plug_info(&mixer, &tokens).unwrap();
    /// assert!(behavior.is_container && behavior.requires_encapsulation);
    /// ```
    ///
    /// OpenUSD: `_BehaviorRegistry::RegisterPlugConfiguredBehaviorForType`
    /// and `_LoadPluginDefiningBehaviorForType` (`connectableAPIBehavior.cpp`),
    /// with the base `UsdShadeConnectableAPIBehavior` policies.
    #[must_use]
    pub fn from_plug_info(schema: &SchemaDefinition, tokens: &TokenInterner) -> Option<Self> {
        let flag = |key: &str| match tokens.lookup(key).and_then(|key| schema.plug_info(key)) {
            Some(Value::Bool(value)) => Some(*value),
            _ => None,
        };
        flag(PROVIDES_CONNECTABLE_BEHAVIOR)
            .filter(|provides| *provides)
            .map(|_| Self {
                is_container: flag(IS_CONTAINER).unwrap_or(false),
                requires_encapsulation: flag(REQUIRES_ENCAPSULATION).unwrap_or(true),
                outputs_connectable: true,
                derived_container: false,
            })
    }
}

/// The behavior registered for the schema `name` itself, not its
/// ancestors: a built-in behavior OpenUSD registers in code, which wins
/// over plugin metadata, or else the one its plugin metadata configures.
/// An applied schema counts only when it is single-apply, as only those
/// impart a behavior.
///
/// OpenUSD: `_BehaviorRegistry::_FindBehaviorForType`,
/// `_FindBehaviorForApiSchema`, and the `TF_REGISTRY_FUNCTION`s of
/// `UsdShadeMaterial`, `UsdShadeNodeGraph`, `UsdShadeShader`,
/// `UsdLuxLightFilter` and `UsdLuxLightAPI`.
fn schema_behavior(
    name: TokenId,
    applied: bool,
    registry: Option<&SchemaRegistry>,
    tokens: &TokenInterner,
) -> Option<ConnectableBehavior> {
    let built_in = match (applied, tokens.resolve(name)) {
        (false, "Material") => Some(ConnectableBehavior::MATERIAL),
        (false, "NodeGraph") => Some(ConnectableBehavior::NODE_GRAPH),
        (false, "Shader") => Some(ConnectableBehavior::SHADER),
        #[cfg(feature = "usd-lux")]
        (false, "LightFilter") => Some(ConnectableBehavior::LIGHT_FILTER),
        #[cfg(feature = "usd-lux")]
        (true, "LightAPI") => Some(ConnectableBehavior::LIGHT_API),
        _ => None,
    };
    built_in.or_else(|| {
        let schema = registry?.schema(name)?;
        if applied && schema.kind != SchemaKind::SingleApplyApi {
            return None;
        }
        ConnectableBehavior::from_plug_info(schema, tokens)
    })
}

impl Scene<'_> {
    /// The connectability behavior of the prim at `path`, `None` when it
    /// has none, as for a prim no schema gives one (C++
    /// `UsdShadeConnectableAPI` is then not compatible with it).
    ///
    /// The behavior is the first found of:
    ///
    /// 1. the behavior of the prim's type, or else of the nearest of the
    ///    typed schemas it derives from that has one, overridden by the
    ///    first of the applied schemas the prim's `apiSchemas` authors, in
    ///    strength order, that has one (not their built-ins);
    /// 2. without a typed behavior, the first of all the prim's applied
    ///    schemas that has one, its type's built-ins and auto-applies
    ///    first ([`layerstack::PrimDefinition::applied_schemas`]).
    ///
    /// A schema has a behavior when OpenUSD registers one for it in code
    /// (`Material`, `NodeGraph`, `Shader`, `LightFilter`, `LightAPI`), or
    /// when its plugin metadata configures one
    /// ([`ConnectableBehavior::from_plug_info`]), as a custom schema's
    /// `plugInfo.json` does with `providesUsdShadeConnectableAPIBehavior`.
    /// Only single-apply schemas impart one. A prim typed with an abstract
    /// schema is typeless here, so only its applied schemas are consulted.
    ///
    /// Spec: AOUSD Core §12.4 (shading connections), §13.3 (schema types and
    /// applied schemas). OpenUSD: `_BehaviorRegistry::_GetBehaviorForPrimTypeId`
    /// (`pxr/usd/usdShade/connectableAPIBehavior.cpp`).
    #[must_use]
    pub fn connectable_behavior(&self, path: PathId) -> Option<ConnectableBehavior> {
        let definition = self.definition(path)?;
        let registry = self.stage().schemas();
        let tokens = self.store().tokens();
        let typed = definition
            .ancestors()
            .iter()
            .find_map(|&schema| schema_behavior(schema, false, registry, tokens));
        if let Some(typed) = typed {
            let authored = PrimView::new(*self, path)
                .metadata()
                .value("apiSchemas")
                .and_then(|value| {
                    value.array_ref().and_then(|names| {
                        names.iter().find_map(|name| match &*name {
                            Value::Token(name) => schema_behavior(*name, true, registry, tokens),
                            _ => None,
                        })
                    })
                });
            return Some(authored.unwrap_or(typed));
        }
        definition
            .applied_schemas()
            .iter()
            .filter(|applied| applied.instance.is_none())
            .find_map(|applied| schema_behavior(applied.schema, true, registry, tokens))
    }
    /// Checks a proposed connection against C++'s `CanConnect` policies, for
    /// the destination prim's [`Self::connectable_behavior`].
    /// This is advisory: source-opinion authoring remains explicit and accepts
    /// invalid networks for interchange. It checks topology and connectability,
    /// not type compatibility or renderer shader support. AOUSD Core §12.4;
    /// OpenUSD `UsdShadeConnectableAPIBehavior::CanConnectInputToSource` and
    /// `CanConnectOutputToSource`, and those of `UsdLuxLightAPI/LightFilter`.
    pub fn validate_shading_connection(
        &self,
        destination: PropertyPath,
        source: PropertyPath,
    ) -> Result<(), ConnectionError> {
        let reject = |issue| {
            Err(ConnectionError {
                destination,
                source,
                issue,
            })
        };
        if !self.shading_attribute(destination)
            || super::shading_kind(self.store().tokens().resolve(destination.property())).is_none()
        {
            return reject(ConnectionIssue::InvalidDestination);
        }
        let source_kind = self.property(source).map(|property| {
            property
                .declaration()
                .map_or_else(|| property.kind(), |declared| declared.kind)
        });
        if source_kind != Some(PropertyKind::Attribute) {
            return reject(ConnectionIssue::InvalidSource);
        }
        let Some(behavior) = self.connectable_behavior(destination.prim_path()) else {
            return reject(ConnectionIssue::UnsupportedBehavior);
        };
        let input = super::shading_kind(self.store().tokens().resolve(destination.property()))
            == Some(true);
        let source_input =
            super::shading_kind(self.store().tokens().resolve(source.property())) == Some(true);
        // C++ `UsdShadeInput::GetConnectability`: an unauthored or empty
        // `connectability` is `full`.
        let connectability = |path: PropertyPath| {
            PrimView::new(*self, path.prim_path())
                .property_metadata(self.store().tokens().resolve(path.property()))
                .and_then(|m| m.connectability())
                .filter(|token| !token.is_empty())
                .unwrap_or("full")
        };
        if input {
            match connectability(destination) {
                "full" => {}
                "interfaceOnly" => {
                    if !source_input || connectability(source) != "interfaceOnly" {
                        return reject(ConnectionIssue::InterfaceOnly);
                    }
                }
                _ => return reject(ConnectionIssue::UnknownConnectability),
            }
            if behavior.requires_encapsulation {
                let allowed = if source_input {
                    self.connectable_behavior(source.prim_path())
                        .is_some_and(|b| b.is_container)
                        && self.parent(destination.prim_path()) == Some(source.prim_path())
                } else if behavior.derived_container {
                    behavior.is_container
                        && self.parent(source.prim_path()) == Some(destination.prim_path())
                } else {
                    let parent = self.parent(destination.prim_path());
                    parent.is_some_and(|p| {
                        self.connectable_behavior(p).is_some_and(|b| b.is_container)
                    }) && parent == self.parent(source.prim_path())
                };
                if !allowed {
                    return reject(ConnectionIssue::Encapsulation);
                }
            }
        } else {
            if !behavior.outputs_connectable {
                return reject(ConnectionIssue::OutputNotConnectable);
            }
            if source_input {
                if behavior.derived_container {
                    return reject(ConnectionIssue::PassthroughForbidden);
                }
                if destination.prim_path() != source.prim_path() {
                    return reject(ConnectionIssue::Encapsulation);
                }
            } else if behavior.requires_encapsulation
                && self.parent(source.prim_path()) != Some(destination.prim_path())
            {
                return reject(ConnectionIssue::Encapsulation);
            }
        }
        Ok(())
    }
}
