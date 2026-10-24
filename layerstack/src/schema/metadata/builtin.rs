// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The metadata fields usd-core 26.08 registers before any plugin of one's
//! own: Sdf's (`_RegisterStandardFields`, `pxr/usd/sdf/schema.cpp`) and those
//! the `SdfMetadata` of its own plugins declare (`pxr/usd/sdf`, `usd`,
//! `usdGeom`, `usdPhysics`, `usdRender`, `usdShade`, `usdSkel` and `usdUI`
//! `plugInfo.json`). `tests/registry_metadata.rs` checks the table against
//! the fields usd-core registers, kind of spec by kind of spec.

use core::fmt;

use super::{ListOpItem, MetadataTarget, MetadataType};
use crate::doc::{FieldValue, Value};
use crate::interner::TokenInterner;
use crate::value_type::ValueType;

/// Who registers a metadata field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetadataOrigin {
    /// Sdf itself; no plugin may declare the field.
    Sdf,
    /// One of the plugins usd-core is built with, by its name (`usdGeom`);
    /// a registry of its schemas may declare the field again, as that
    /// plugin does.
    Plugin(&'static str),
    /// A plugin's declaration a registry holds
    /// ([`SchemaRegistryBuilder::register_metadata`](crate::SchemaRegistryBuilder::register_metadata)).
    Registry,
}

impl fmt::Display for MetadataOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sdf => f.write_str("Sdf"),
            Self::Plugin(plugin) => write!(f, "the `{plugin}` plugin"),
            Self::Registry => f.write_str("the registry"),
        }
    }
}

/// A built-in field's declared default.
#[derive(Clone, Copy, Debug)]
pub(super) enum Default {
    /// None: the type's fallback.
    None,
    Bool(bool),
    Int(i32),
    Float(f32),
    Double(f64),
    Token(&'static str),
    String(&'static str),
}

#[derive(Debug)]
pub(super) struct Builtin {
    pub(super) name: &'static str,
    pub(super) value_type: MetadataType,
    /// In [`MetadataTarget`] order; prims imply variants.
    pub(super) targets: &'static [MetadataTarget],
    pub(super) default: Default,
    pub(super) origin: MetadataOrigin,
}

impl Builtin {
    pub(super) fn fallback(&self, tokens: &TokenInterner) -> Option<FieldValue> {
        Some(FieldValue::Value(match self.default {
            Default::None => return self.value_type.fallback(),
            Default::Bool(value) => Value::Bool(value),
            Default::Int(value) => Value::Int(value),
            Default::Float(value) => Value::Float(value),
            Default::Double(value) => Value::Double(value),
            Default::Token(value) => Value::Token(tokens.intern(value)),
            Default::String(value) => Value::string(value),
        }))
    }

    /// Whether a declaration's `default` is this field's: what the
    /// registry's tables of the plugin hold when they declare it again.
    pub(super) fn default_matches(&self, default: Option<&Value>) -> bool {
        match (self.default, default) {
            (Default::None, None) => true,
            (Default::Bool(a), Some(Value::Bool(b))) => a == *b,
            (Default::Int(a), Some(Value::Int(b))) => a == *b,
            (Default::Float(a), Some(Value::Float(b))) => a.to_bits() == b.to_bits(),
            (Default::Double(a), Some(Value::Double(b))) => a.to_bits() == b.to_bits(),
            // The token is the registry's interner's, which the builder
            // does not hold; lookups find the built-in field first
            // (`MetadataField::lookup`), so its own default stands.
            (Default::Token(_), Some(Value::Token(_))) => true,
            (Default::String(a), Some(Value::String(b))) => a == &**b,
            _ => false,
        }
    }
}

use MetadataTarget::{Attribute as A, Layer as L, Prim as P, Relationship as R};
use MetadataType::{Array, Dictionary, ListOp, Value as V};

const LAYER: &[MetadataTarget] = &[L];
const PRIM: &[MetadataTarget] = &[P];
const ATTRIBUTE: &[MetadataTarget] = &[A];
const RELATIONSHIP: &[MetadataTarget] = &[R];
const PROPERTY: &[MetadataTarget] = &[A, R];
const OBJECT: &[MetadataTarget] = &[P, A, R];
const EVERY: &[MetadataTarget] = &[L, P, A, R];

const fn sdf(
    name: &'static str,
    value_type: MetadataType,
    targets: &'static [MetadataTarget],
    default: Default,
) -> Builtin {
    Builtin {
        name,
        value_type,
        targets,
        default,
        origin: MetadataOrigin::Sdf,
    }
}

const fn plugin(
    plugin: &'static str,
    name: &'static str,
    value_type: MetadataType,
    targets: &'static [MetadataTarget],
    default: Default,
) -> Builtin {
    Builtin {
        name,
        value_type,
        targets,
        default,
        origin: MetadataOrigin::Plugin(plugin),
    }
}

const BOOL: MetadataType = V(ValueType::Bool);
const INT: MetadataType = V(ValueType::Int);
const DOUBLE: MetadataType = V(ValueType::Double);
const STRING: MetadataType = V(ValueType::String);
const TOKEN: MetadataType = V(ValueType::Token);

/// In name order, for [`find`].
pub(super) static BUILTINS: &[Builtin] = &[
    sdf("active", BOOL, PRIM, Default::Bool(true)),
    sdf(
        "allowedTokens",
        Array(ValueType::Token),
        ATTRIBUTE,
        Default::None,
    ),
    plugin(
        "usd",
        "apiSchemas",
        ListOp(ListOpItem::Token),
        PRIM,
        Default::None,
    ),
    sdf(
        "arraySizeConstraint",
        V(ValueType::Int64),
        ATTRIBUTE,
        Default::None,
    ),
    sdf("assetInfo", Dictionary, OBJECT, Default::None),
    plugin(
        "usdShade",
        "bindMaterialAs",
        TOKEN,
        RELATIONSHIP,
        Default::None,
    ),
    sdf("clipSets", ListOp(ListOpItem::String), PRIM, Default::None),
    sdf("clips", Dictionary, PRIM, Default::None),
    sdf(
        "colorConfiguration",
        V(ValueType::Asset),
        LAYER,
        Default::None,
    ),
    sdf("colorManagementSystem", TOKEN, LAYER, Default::None),
    sdf("colorSpace", TOKEN, ATTRIBUTE, Default::None),
    plugin(
        "usdShade",
        "connectability",
        TOKEN,
        ATTRIBUTE,
        Default::Token("full"),
    ),
    plugin(
        "usdGeom",
        "constraintTargetIdentifier",
        TOKEN,
        ATTRIBUTE,
        Default::Token(""),
    ),
    sdf("customData", Dictionary, OBJECT, Default::None),
    sdf("customLayerData", Dictionary, LAYER, Default::None),
    sdf("defaultPrim", TOKEN, LAYER, Default::None),
    sdf("displayGroup", STRING, PROPERTY, Default::None),
    sdf(
        "displayGroupOrder",
        Array(ValueType::String),
        PRIM,
        Default::None,
    ),
    sdf("displayName", STRING, OBJECT, Default::None),
    sdf("documentation", STRING, EVERY, Default::None),
    plugin("usdGeom", "elementSize", INT, ATTRIBUTE, Default::Int(1)),
    sdf("endFrame", DOUBLE, LAYER, Default::None),
    sdf("endTimeCode", DOUBLE, LAYER, Default::None),
    sdf("expressionVariables", Dictionary, LAYER, Default::None),
    plugin("usd", "fallbackPrimTypes", Dictionary, LAYER, Default::None),
    sdf("framePrecision", INT, LAYER, Default::Int(3)),
    sdf("framesPerSecond", DOUBLE, LAYER, Default::Double(24.0)),
    sdf("hasOwnedSubLayers", BOOL, LAYER, Default::None),
    sdf("hidden", BOOL, OBJECT, Default::None),
    plugin(
        "usdGeom",
        "inactiveIds",
        ListOp(ListOpItem::Int64),
        PRIM,
        Default::None,
    ),
    sdf("instanceable", BOOL, PRIM, Default::None),
    plugin(
        "usdGeom",
        "interpolation",
        TOKEN,
        ATTRIBUTE,
        Default::Token("constant"),
    ),
    plugin(
        "usdPhysics",
        "kilogramsPerUnit",
        DOUBLE,
        LAYER,
        Default::Double(1.0),
    ),
    sdf("kind", TOKEN, PRIM, Default::None),
    sdf("limits", Dictionary, ATTRIBUTE, Default::None),
    plugin(
        "usdGeom",
        "metersPerUnit",
        DOUBLE,
        LAYER,
        Default::Double(0.01),
    ),
    sdf("noLoadHint", BOOL, RELATIONSHIP, Default::None),
    plugin("usdShade", "outputName", TOKEN, RELATIONSHIP, Default::None),
    sdf("owner", STRING, LAYER, Default::None),
    plugin(
        "sdf",
        "payloadAssetDependencies",
        Array(ValueType::Asset),
        PRIM,
        Default::None,
    ),
    sdf("prefix", STRING, OBJECT, Default::None),
    sdf("prefixSubstitutions", Dictionary, PRIM, Default::None),
    plugin(
        "usdRender",
        "renderSettingsPrimPath",
        STRING,
        LAYER,
        Default::String(""),
    ),
    plugin("usdShade", "renderType", TOKEN, PROPERTY, Default::None),
    plugin(
        "usdShade",
        "sdrMetadata",
        Dictionary,
        &[P, A],
        Default::None,
    ),
    sdf("sessionOwner", STRING, LAYER, Default::None),
    sdf("startFrame", DOUBLE, LAYER, Default::None),
    sdf("startTimeCode", DOUBLE, LAYER, Default::None),
    sdf("suffix", STRING, OBJECT, Default::None),
    sdf("suffixSubstitutions", Dictionary, PRIM, Default::None),
    sdf("symmetricPeer", STRING, OBJECT, Default::None),
    sdf("symmetryArguments", Dictionary, OBJECT, Default::None),
    sdf("symmetryFunction", TOKEN, OBJECT, Default::None),
    sdf("timeCodesPerSecond", DOUBLE, LAYER, Default::Double(24.0)),
    plugin("usdUI", "uiHints", Dictionary, OBJECT, Default::None),
    plugin(
        "usdGeom",
        "unauthoredValuesIndex",
        INT,
        ATTRIBUTE,
        Default::Int(-1),
    ),
    plugin("usdGeom", "upAxis", TOKEN, LAYER, Default::Token("Y")),
    plugin(
        "usdSkel",
        "weight",
        V(ValueType::Float),
        ATTRIBUTE,
        Default::Float(0.0),
    ),
];

pub(super) fn find(name: &str) -> Option<&'static Builtin> {
    BUILTINS
        .binary_search_by(|field| field.name.cmp(name))
        .ok()
        .map(|index| &BUILTINS[index])
}

/// The fields of each kind of spec that are not metadata, and the metadata
/// fields with routes of their own.
pub(super) fn is_spec_field(name: &str, target: MetadataTarget) -> bool {
    match target {
        MetadataTarget::Layer => matches!(
            name,
            "comment"
                | "primChildren"
                | "layerRelocates"
                | "primOrder"
                | "subLayers"
                | "subLayerOffsets"
        ),
        MetadataTarget::Prim | MetadataTarget::Variant => matches!(
            name,
            "specifier"
                | "comment"
                | "inheritPaths"
                | "specializes"
                | "primChildren"
                | "primOrder"
                | "properties"
                | "propertyOrder"
                | "references"
                | "relocates"
                | "variantSelection"
                | "variantSetChildren"
                | "variantSetNames"
                | "payload"
                | "typeName"
        ),
        MetadataTarget::Attribute => matches!(
            name,
            "custom"
                | "variability"
                | "comment"
                | "default"
                | "typeName"
                | "spline"
                | "connectionChildren"
                | "connectionPaths"
                | "displayUnit"
                | "timeSamples"
        ),
        MetadataTarget::Relationship => matches!(
            name,
            "custom" | "variability" | "comment" | "default" | "targetChildren" | "targetPaths"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_in_name_order_and_its_targets_in_target_order() {
        assert!(BUILTINS.windows(2).all(|w| w[0].name < w[1].name));
        assert!(
            BUILTINS
                .iter()
                .all(|field| field.targets.windows(2).all(|w| w[0] < w[1]))
        );
        assert!(
            BUILTINS
                .iter()
                .all(|field| !field.targets.contains(&MetadataTarget::Variant)),
            "prim fields are variant fields without saying so"
        );
    }
}
