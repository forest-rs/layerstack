// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in `UsdShade` and `UsdLux` connectability policies.
use crate::{PrimView, Scene};
use layerstack::{PathId, PropertyKind, PropertyPath};

/// Built-in connection behavior. Custom C++ behavior plugins are not loaded.
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
    /// An input declares a connectability token other than full/interfaceOnly.
    UnknownConnectability,
    /// Derived containers forbid output-to-input passthrough.
    PassthroughForbidden,
    /// The source violates the owner's container boundary.
    Encapsulation,
}
/// Precise rejection of a proposed connection; no scene edits occur.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
impl Scene<'_> {
    /// Built-in `UsdShade`/`UsdLux` behavior for this prim, including applied `LightAPI`.
    /// `None` means unsupported behavior, not a promise that C++ would reject it.
    #[must_use]
    pub fn connectable_behavior(&self, path: PathId) -> Option<ConnectableBehavior> {
        // Explicit authored LightAPI behavior overrides a typed schema's
        // behavior; built-in/indirect APIs are consulted only without a typed
        // behavior. OpenUSD _BehaviorRegistry::_GetBehaviorForPrimTypeId.
        #[cfg(feature = "usd-lux")]
        if PrimView::new(*self,path).metadata_value("apiSchemas").and_then(|v|v.array_ref().map(|a|a.iter().any(|v|matches!(&*v,layerstack::Value::Token(t) if self.store().tokens().resolve(*t)=="LightAPI")))).unwrap_or(false) {
            return Some(ConnectableBehavior {is_container:true,requires_encapsulation:false,outputs_connectable:true,derived_container:true});
        }
        #[cfg(feature = "usd-lux")]
        if self.is_a(path, "LightFilter") {
            return Some(ConnectableBehavior {
                is_container: true,
                requires_encapsulation: true,
                outputs_connectable: false,
                derived_container: true,
            });
        }
        if self.is_a(path, "Material") {
            Some(ConnectableBehavior {
                is_container: true,
                requires_encapsulation: true,
                outputs_connectable: true,
                derived_container: true,
            })
        } else if self.is_a(path, "NodeGraph") {
            Some(ConnectableBehavior {
                is_container: true,
                requires_encapsulation: true,
                outputs_connectable: true,
                derived_container: false,
            })
        } else if self.is_a(path, "Shader") {
            Some(ConnectableBehavior {
                is_container: false,
                requires_encapsulation: true,
                outputs_connectable: false,
                derived_container: false,
            })
        } else {
            #[cfg(feature = "usd-lux")]
            if self.has_api(path, "LightAPI", None) {
                return Some(ConnectableBehavior {
                    is_container: true,
                    requires_encapsulation: false,
                    outputs_connectable: true,
                    derived_container: true,
                });
            }
            None
        }
    }
    /// Checks a proposed connection against C++'s built-in `CanConnect` policies.
    /// This is advisory: source-opinion authoring remains explicit and accepts
    /// invalid networks for interchange. It checks topology and connectability,
    /// not type compatibility or renderer shader support. AOUSD Core §12.4;
    /// OpenUSD `UsdShadeConnectableAPIBehavior`, `UsdLuxLightAPI/LightFilter`.
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
        let source_kind = self
            .stage()
            .resolve_property_declaration(source.prim_path(), source.property())
            .map(|d| d.kind)
            .or_else(|| {
                self.stage()
                    .property_definition(source.prim_path(), source.property(), self.store())
                    .map(|d| d.kind)
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
        let connectability = |path: PropertyPath| {
            PrimView::new(*self, path.prim_path())
                .property_metadata(self.store().tokens().resolve(path.property()))
                .and_then(|m| m.connectability())
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
