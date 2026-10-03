// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema-derived descriptions of intrinsic light shader nodes.
//! OpenUSD 26.8 `UsdLux_DiscoveryPlugin` / `UsdLux_LightDefParserPlugin`;
//! AOUSD Core §13.3 (schema definitions), §12.3.5 (fallback values).
use crate::{shading::PortKind, table::Schema};
use alloc::{collections::BTreeMap, vec::Vec};
use layerstack::{PropertyKind, PropertyType, SchemaKind, TokenInterner, Value, Variability};

/// One schema-defined shader port, preserving USD types and defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct LightNodePort {
    /// Port name without the `inputs:` or `outputs:` namespace.
    pub name: &'static str,
    /// Full USD property name for inspecting/authoring this port on a scene prim.
    pub property: &'static str,
    /// Input parameter or output result.
    pub kind: PortKind,
    /// Original USD declared type, before Sdr's bool/token/asset conversions.
    pub property_type: PropertyType,
    /// Schema-defined variability.
    pub variability: Variability,
    /// Schema default, distinct from any particular scene's authored value.
    /// Token identities use the interner passed to the catalogue constructor.
    pub default: Option<Value>,
    /// Schema whose local property won the parser's merge order.
    pub source_schema: &'static str,
    /// Whether the original USD type is an asset identifier.
    pub asset_identifier: bool,
}

/// Intrinsic light node description for inspectors and renderer adapters.
/// This describes USD parameters; it contains no shader implementation or GPU ABI.
#[derive(Clone, Debug, PartialEq)]
pub struct LightNodeDefinition {
    /// Shader node identifier. Mesh/volume identifiers map to API schema names.
    pub identifier: &'static str,
    /// Concrete schema or API schema supplying the light-specific parameters.
    pub schema: &'static str,
    /// OpenUSD parser source type (`USD`).
    pub source_type: &'static str,
    /// OpenUSD discovery type (`usd-schema-gen`).
    pub discovery_type: &'static str,
    /// OpenUSD node context (`light`).
    pub context: &'static str,
    /// OpenUSD node subdomain (`lighting`).
    pub subdomain: &'static str,
    /// Ports sorted by full USD property name, including optional shaping/shadow.
    pub ports: Vec<LightNodePort>,
}
fn tables() -> &'static [Schema] {
    crate::Domain::UsdLux.tables().schemas
}
fn schema(name: &str) -> &'static Schema {
    tables()
        .iter()
        .find(|schema| schema.name == name)
        .expect("generated Lux schema")
}
fn intrinsic(mut candidate: &Schema) -> bool {
    while let Some(parent) = candidate.parent {
        if matches!(parent, "BoundableLightBase" | "NonboundableLightBase") {
            return true;
        }
        let Some(next) = tables().iter().find(|schema| schema.name == parent) else {
            break;
        };
        candidate = next;
    }
    false
}
fn definition(
    identifier: &'static str,
    light: &'static Schema,
    tokens: &mut TokenInterner,
) -> LightNodeDefinition {
    let mut ports = BTreeMap::new();
    // C++ copies these four local property sets in this order. In particular,
    // do not include APIs auto-applied by unrelated/custom schema plugins.
    for source in [
        schema("LightAPI"),
        light,
        schema("ShadowAPI"),
        schema("ShapingAPI"),
    ] {
        for property in source.properties.iter().chain(source.overrides) {
            let (name, kind) = if let Some(name) = property.name.strip_prefix("inputs:") {
                (name, PortKind::Input)
            } else if let Some(name) = property.name.strip_prefix("outputs:") {
                (name, PortKind::Output)
            } else {
                continue;
            };
            let Some(ty) = property
                .value_type
                .filter(|_| property.kind == PropertyKind::Attribute)
            else {
                continue;
            };
            ports.insert(
                property.name,
                LightNodePort {
                    name,
                    property: property.name,
                    kind,
                    property_type: PropertyType::new(ty.name, ty.is_array, (ty.zero)(tokens)),
                    variability: property.variability,
                    default: property.fallback.map(|fallback| fallback(tokens)),
                    source_schema: source.name,
                    asset_identifier: ty.name == "asset",
                },
            );
        }
    }
    LightNodeDefinition {
        identifier,
        schema: light.name,
        source_type: "USD",
        discovery_type: "usd-schema-gen",
        context: "light",
        subdomain: "lighting",
        ports: ports.into_values().collect(),
    }
}

/// Builds the intrinsic Lux light-node catalogue in sorted identifier order.
///
/// Matches C++ discovery of concrete Lux schemas derived from its boundable or
/// nonboundable light bases, plus `MeshLight`/`VolumeLight` API definitions. It
/// includes common, light-specific, shadow and shaping ports using C++'s merge
/// order. Plugin lights/filters and custom renderer nodes require their own
/// catalogues; `PluginLight` does not derive from either intrinsic light base.
///
/// Descriptions retain original USD types/defaults: Sdr converts bool to int,
/// token/asset to string, while an engine generally needs the source types.
/// Token defaults and type zero-values use `tokens`; resolve them through that
/// same interner. Scene schema registries, authored values, plugin execution,
/// Sdr UI hints and renderer shader support do not affect this built-in catalogue.
#[must_use]
pub fn builtin_light_node_definitions(tokens: &mut TokenInterner) -> Vec<LightNodeDefinition> {
    let mut nodes: Vec<_> = tables()
        .iter()
        .filter(|schema| schema.kind == SchemaKind::ConcreteTyped && intrinsic(schema))
        .map(|light| definition(light.name, light, tokens))
        .collect();
    nodes.push(definition("MeshLight", schema("MeshLightAPI"), tokens));
    nodes.push(definition("VolumeLight", schema("VolumeLightAPI"), tokens));
    nodes.sort_by_key(|node| node.identifier);
    nodes
}
