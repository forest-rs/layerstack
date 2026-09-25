// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lowering of an authored USDA [`Document`] to crate specs.
//!
//! The result is what OpenUSD's text parser stores for the USDA the same
//! document writes (`Document::to_usda`), so both files describe one layer:
//! the same specs, fields, field types and children order. In particular:
//!
//! - the pseudo-root holds `defaultPrim`, then the layer metadata in order,
//!   then `primOrder` (`reorder rootPrims`) and `primChildren`;
//! - a prim holds `specifier`, `typeName` (for typed prims), its metadata
//!   in order (a token list op such as `prepend apiSchemas` included), then
//!   `propertyOrder` and `primOrder` (the `reorder` statements), then
//!   `primChildren` and `properties` (in the document's property order);
//! - an attribute holds `custom`, `typeName` (the declared name, role and
//!   `[]` included), `variability`, `default` when a value is authored (a
//!   value block included), its metadata in order, then `connectionPaths`
//!   (a path list op) when it has connections, which USDA writes as later
//!   `.connect` statements;
//! - a relationship holds `variability` (always uniform), `custom` only
//!   when custom, then `targetPaths` (a path list op, an empty explicit one
//!   for `rel r = None`) and its metadata: an explicit target list is part
//!   of the declaration, so it precedes the metadata, while list edits are
//!   later statements and follow it;
//! - metadata keys become field names (`doc` is stored as `documentation`)
//!   and values must already have the field's registered type (strings
//!   and tokens are interchangeable, since both are written as quoted
//!   text), so the USDA and the USDC carry the same value: a `float` for a
//!   `double` field would print as `0.1` in USDA, parse as the double
//!   `0.1`, but widen to `0.10000000149011612` here;
//! - a `timecode` / `timecode[]` default is an `SdfTimeCode` value (crate
//!   version 0.9.0); every other value keeps its own type.
//!
//! Metadata keys outside the table in [`metadata_field`] are rejected: the
//! text parser would store them as unregistered values, which this writer
//! does not produce, with one deliberate exception: prim `profilesInfo`
//! ([`FieldType::UnregisteredDictionary`]).
//!
//! Spec: AOUSD Core §7.4 (metadata), §7.6 (core fields), §16.3 (crate
//! format).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use layerstack::doc::Layer;
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack_usda::save::layer_document;
use layerstack_usda::writer::{
    Attribute, Document, ListOp as UsdaListOp, Metadatum, Prim, Property, Relationship,
    Specifier as UsdaSpecifier, Value as UsdaValue, Variability as UsdaVariability,
};

use super::error::UsdcWriteError;
use super::{ListOp, Spec, SpecForm, Specifier, Value, Variability, write_crate};

/// Serializes `doc` as a USDC file.
///
/// The document is validated exactly as [`Document::to_usda`] validates it,
/// then lowered to specs (see the [module docs](self)) and written with
/// [`write_crate`].
///
/// # Errors
///
/// [`UsdcWriteError::Document`] when the USDA writer would reject the
/// document; [`UsdcWriteError::UnknownMetadata`] and
/// [`UsdcWriteError::MetadataType`] for metadata the crate lowering cannot
/// type; otherwise the errors of [`write_crate`].
pub fn write_document(doc: &Document) -> Result<Vec<u8>, UsdcWriteError> {
    write_crate(&document_specs(doc)?)
}

/// Saves an authored layer as a USDC file.
///
/// The layer goes through the same lowering as its USDA
/// ([`layerstack_usda::save::layer_document`]), so both formats read the
/// same source fields; the resulting document is then written with
/// [`write_document`]. `tokens` and `paths` are the interners the layer's
/// identifiers belong to.
///
/// # Errors
///
/// [`UsdcWriteError::Save`] for what the lowering rejects, before any
/// output; otherwise the errors of [`write_document`] (notably
/// [`UsdcWriteError::UnknownMetadata`] for metadata OpenUSD does not
/// register).
pub fn save_layer(
    layer: &Layer,
    tokens: &TokenInterner,
    paths: &PathInterner,
) -> Result<Vec<u8>, UsdcWriteError> {
    let doc = layer_document(layer, tokens, paths).map_err(UsdcWriteError::Save)?;
    write_document(&doc)
}

/// Lowers `doc` to the crate specs [`write_document`] writes.
///
/// # Errors
///
/// See [`write_document`].
pub fn document_specs(doc: &Document) -> Result<Vec<Spec>, UsdcWriteError> {
    doc.validate().map_err(UsdcWriteError::Document)?;
    let mut root = Spec::new("/", SpecForm::PseudoRoot);
    if let Some(name) = &doc.default_prim {
        root = root.with_field("defaultPrim", Value::Token(name.clone()));
    }
    for entry in &doc.metadata {
        root.fields.push(metadatum(Owner::Layer, "/", entry)?);
    }
    if let Some(order) = &doc.prim_order {
        root = root.with_field("primOrder", Value::TokenVector(order.clone()));
    }
    if !doc.prims.is_empty() {
        root = root.with_field(
            "primChildren",
            Value::TokenVector(doc.prims.iter().map(|p| p.name.clone()).collect()),
        );
    }
    let mut specs = alloc::vec![root];
    for prim in &doc.prims {
        lower_prim(prim, "", &mut specs)?;
    }
    Ok(specs)
}

fn lower_prim(prim: &Prim, parent: &str, specs: &mut Vec<Spec>) -> Result<(), UsdcWriteError> {
    let path = alloc::format!("{parent}/{}", prim.name);
    let mut spec = Spec::new(path.clone(), SpecForm::Prim).with_field(
        "specifier",
        Value::Specifier(match prim.specifier {
            UsdaSpecifier::Def => Specifier::Def,
            UsdaSpecifier::Over => Specifier::Over,
            UsdaSpecifier::Class => Specifier::Class,
        }),
    );
    if let Some(type_name) = &prim.type_name {
        spec = spec.with_field("typeName", Value::Token(type_name.clone()));
    }
    for entry in &prim.metadata {
        spec.fields.push(metadatum(Owner::Prim, &path, entry)?);
    }
    if let Some(order) = &prim.property_order {
        spec = spec.with_field("propertyOrder", Value::TokenVector(order.clone()));
    }
    if let Some(order) = &prim.prim_order {
        spec = spec.with_field("primOrder", Value::TokenVector(order.clone()));
    }
    if !prim.children.is_empty() {
        spec = spec.with_field(
            "primChildren",
            Value::TokenVector(prim.children.iter().map(|c| c.name.clone()).collect()),
        );
    }
    if !prim.properties.is_empty() {
        let names = prim.properties.iter().map(|p| String::from(p.name()));
        spec = spec.with_field("properties", Value::TokenVector(names.collect()));
    }
    specs.push(spec);
    for property in &prim.properties {
        specs.push(match property {
            Property::Attribute(attribute) => lower_attribute(attribute, &path)?,
            Property::Relationship(relationship) => lower_relationship(relationship, &path)?,
        });
    }
    for child in &prim.children {
        lower_prim(child, &path, specs)?;
    }
    Ok(())
}

fn lower_attribute(attribute: &Attribute, prim: &str) -> Result<Spec, UsdcWriteError> {
    let path = alloc::format!("{prim}.{}", attribute.name);
    let mut spec = Spec::new(path.clone(), SpecForm::Attribute)
        .with_field("custom", Value::Bool(attribute.custom))
        .with_field("typeName", Value::Token(attribute.type_name.clone()))
        .with_field(
            "variability",
            Value::Variability(match attribute.variability {
                UsdaVariability::Varying => Variability::Varying,
                UsdaVariability::Uniform => Variability::Uniform,
            }),
        );
    if let Some(value) = &attribute.value {
        let is_timecode = attribute
            .type_name
            .strip_suffix("[]")
            .unwrap_or(&attribute.type_name)
            == "timecode";
        let value = match (is_timecode, value) {
            (true, UsdaValue::Double(v)) => Value::TimeCode(*v),
            (true, UsdaValue::DoubleArray(v)) => Value::TimeCodeArray(v.clone()),
            (_, v) => natural(v),
        };
        spec = spec.with_field("default", value);
    }
    for entry in &attribute.metadata {
        spec.fields.push(metadatum(Owner::Attribute, &path, entry)?);
    }
    if let Some(connections) = &attribute.connections {
        spec = spec.with_field("connectionPaths", Value::PathListOp(list_op(connections)));
    }
    Ok(spec)
}

fn lower_relationship(relationship: &Relationship, prim: &str) -> Result<Spec, UsdcWriteError> {
    let path = alloc::format!("{prim}.{}", relationship.name);
    let mut spec = Spec::new(path.clone(), SpecForm::Relationship)
        .with_field("variability", Value::Variability(Variability::Uniform));
    if relationship.custom {
        spec = spec.with_field("custom", Value::Bool(true));
    }
    let targets = relationship.targets.as_ref();
    if let Some(targets) = targets.filter(|op| op.explicit.is_some()) {
        spec = spec.with_field("targetPaths", Value::PathListOp(list_op(targets)));
    }
    for entry in &relationship.metadata {
        spec.fields
            .push(metadatum(Owner::Relationship, &path, entry)?);
    }
    if let Some(targets) = targets.filter(|op| op.explicit.is_none()) {
        spec = spec.with_field("targetPaths", Value::PathListOp(list_op(targets)));
    }
    Ok(spec)
}

fn list_op(op: &UsdaListOp<String>) -> ListOp<String> {
    ListOp {
        explicit: op.explicit.clone(),
        prepended: op.prepended.clone(),
        appended: op.appended.clone(),
        deleted: op.deleted.clone(),
    }
}

/// The value type each value keeps outside a typed field: the type its
/// USDA spelling (or typed dictionary entry) declares.
fn natural(value: &UsdaValue) -> Value {
    use UsdaValue as U;
    match value {
        U::Bool(v) => Value::Bool(*v),
        U::Int(v) => Value::Int(*v),
        U::UInt(v) => Value::UInt(*v),
        U::Int64(v) => Value::Int64(*v),
        U::Float(v) => Value::Float(*v),
        U::Double(v) => Value::Double(*v),
        U::String(v) => Value::String(v.clone()),
        U::Token(v) => Value::Token(v.clone()),
        U::Asset(v) => Value::Asset(v.clone()),
        U::Float2(v) => Value::Vec2f(*v),
        U::Float3(v) => Value::Vec3f(*v),
        U::Float4(v) => Value::Vec4f(*v),
        U::Double2(v) => Value::Vec2d(*v),
        U::Double3(v) => Value::Vec3d(*v),
        U::Double4(v) => Value::Vec4d(*v),
        U::Int2(v) => Value::Vec2i(*v),
        U::Int3(v) => Value::Vec3i(*v),
        U::Int4(v) => Value::Vec4i(*v),
        U::Matrix4d(v) => Value::Matrix4d(*v),
        U::BoolArray(v) => Value::BoolArray(v.clone()),
        U::IntArray(v) => Value::IntArray(v.clone()),
        U::UIntArray(v) => Value::UIntArray(v.clone()),
        U::Int64Array(v) => Value::Int64Array(v.clone()),
        U::FloatArray(v) => Value::FloatArray(v.clone()),
        U::DoubleArray(v) => Value::DoubleArray(v.clone()),
        U::StringArray(v) => Value::StringArray(v.clone()),
        U::TokenArray(v) => Value::TokenArray(v.clone()),
        U::AssetArray(v) => Value::AssetArray(v.clone()),
        U::Float2Array(v) => Value::Vec2fArray(v.clone()),
        U::Float3Array(v) => Value::Vec3fArray(v.clone()),
        U::Float4Array(v) => Value::Vec4fArray(v.clone()),
        U::Double2Array(v) => Value::Vec2dArray(v.clone()),
        U::Double3Array(v) => Value::Vec3dArray(v.clone()),
        U::Double4Array(v) => Value::Vec4dArray(v.clone()),
        U::Int2Array(v) => Value::Vec2iArray(v.clone()),
        U::Int3Array(v) => Value::Vec3iArray(v.clone()),
        U::Int4Array(v) => Value::Vec4iArray(v.clone()),
        U::QuathArray(v) => Value::QuathArray(v.clone()),
        U::TokenListOp(op) => Value::TokenListOp(list_op(op)),
        U::Block => Value::Block,
        U::Dictionary(entries) => Value::Dictionary(
            entries
                .iter()
                .map(|(k, v)| (k.clone(), natural(v)))
                .collect(),
        ),
    }
}

/// Which spec a metadatum belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Layer metadata (the pseudo-root).
    Layer,
    /// Prim metadata.
    Prim,
    /// Attribute metadata.
    Attribute,
    /// Relationship metadata.
    Relationship,
}

/// The value type a metadata field is registered with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// `TfToken`; a USDA string or token value.
    Token,
    /// `std::string`; a USDA string or token value.
    String,
    /// `double`; a USDA double only (a `float` or integer is rejected).
    Double,
    /// `int`; a USDA `int` only.
    Int,
    /// `int64`; a USDA `int64` only.
    Int64,
    /// `float`; a USDA `float` only.
    Float,
    /// `bool`.
    Bool,
    /// `VtDictionary`.
    Dictionary,
    /// `SdfAssetPath`.
    Asset,
    /// `VtTokenArray`; a USDA token or string array.
    TokenArray,
    /// `VtStringArray`; a USDA string or token array.
    StringArray,
    /// `SdfTokenListOp`; a USDA token list op.
    TokenListOp,
    /// No registered field: a USDA dictionary, stored as the text parser
    /// stores an unregistered dictionary literal, an `SdfUnregisteredValue`
    /// holding the `VtDictionary`.
    UnregisteredDictionary,
}

/// The crate field name and registered type of the USDA metadata `key` on
/// an `owner`, or `None` when the key is not registered for it.
///
/// The table covers the core fields `SdfSchema` registers for each spec
/// type (`pxr/usd/sdf/schema.cpp`, `_Define(SdfSpecTypePseudoRoot)`,
/// `SdfSpecTypePrim`, the property, `SdfSpecTypeAttribute` and
/// `SdfSpecTypeRelationship` definitions) and the plugin metadata of `usd`
/// (`apiSchemas`), `usdGeom` (`upAxis`, `metersPerUnit`, `interpolation`,
/// `elementSize`, `unauthoredValuesIndex`, `constraintTargetIdentifier`),
/// `usdPhysics` (`kilogramsPerUnit`) and `usdShade` (`bindMaterialAs`,
/// `connectability`, `renderType`, `sdrMetadata`) in their `plugInfo.json`
/// files, plus `usdSkel`'s `weight`, `usdRender`'s `renderSettingsPrimPath`,
/// `usdUI`'s `uiHints` and the core `limits`, `symmetryArguments`,
/// `fallbackPrimTypes` and `clips` dictionaries. USDA's `doc` is the
/// `documentation` field, and its bare string the `comment` field.
///
/// `profilesInfo` is the one unregistered key: `UsdProfilesClaimsAPI`
/// (`pxr/usd/usdProfiles/schema.usda`) documents it as prim metadata, but
/// OpenUSD v26.08 registers no such field, so its text parser keeps the
/// dictionary as an `SdfUnregisteredValue` and the crate carries the same.
/// `ClaimsAPI` itself reads and writes the dictionary under `customData`;
/// both spellings are transported as authored data, and nothing here adds,
/// checks or certifies a profile claim.
pub fn metadata_field(owner: Owner, key: &str) -> Option<(&'static str, FieldType)> {
    use FieldType as F;
    let shared = match key {
        "doc" => Some(("documentation", F::String)),
        "comment" => Some(("comment", F::String)),
        _ => None,
    };
    // `SdfSchema` registers these for prims and both property forms.
    let named = match (owner, key) {
        (Owner::Layer, _) => None,
        (_, "prefix") => Some(("prefix", F::String)),
        (_, "suffix") => Some(("suffix", F::String)),
        (_, "symmetricPeer") => Some(("symmetricPeer", F::String)),
        _ => None,
    };
    let specific = match owner {
        Owner::Layer => match key {
            "defaultPrim" => Some(("defaultPrim", F::Token)),
            "customLayerData" => Some(("customLayerData", F::Dictionary)),
            "fallbackPrimTypes" => Some(("fallbackPrimTypes", F::Dictionary)),
            "expressionVariables" => Some(("expressionVariables", F::Dictionary)),
            "startTimeCode" => Some(("startTimeCode", F::Double)),
            "endTimeCode" => Some(("endTimeCode", F::Double)),
            "timeCodesPerSecond" => Some(("timeCodesPerSecond", F::Double)),
            "framesPerSecond" => Some(("framesPerSecond", F::Double)),
            "framePrecision" => Some(("framePrecision", F::Int)),
            "startFrame" => Some(("startFrame", F::Double)),
            "endFrame" => Some(("endFrame", F::Double)),
            "renderSettingsPrimPath" => Some(("renderSettingsPrimPath", F::String)),
            "owner" => Some(("owner", F::String)),
            "sessionOwner" => Some(("sessionOwner", F::String)),
            "colorConfiguration" => Some(("colorConfiguration", F::Asset)),
            "colorManagementSystem" => Some(("colorManagementSystem", F::Token)),
            "upAxis" => Some(("upAxis", F::Token)),
            "metersPerUnit" => Some(("metersPerUnit", F::Double)),
            "kilogramsPerUnit" => Some(("kilogramsPerUnit", F::Double)),
            _ => None,
        },
        Owner::Prim => match key {
            "kind" => Some(("kind", F::Token)),
            "active" => Some(("active", F::Bool)),
            "hidden" => Some(("hidden", F::Bool)),
            "instanceable" => Some(("instanceable", F::Bool)),
            "customData" => Some(("customData", F::Dictionary)),
            "assetInfo" => Some(("assetInfo", F::Dictionary)),
            "displayName" => Some(("displayName", F::String)),
            "displayGroupOrder" => Some(("displayGroupOrder", F::StringArray)),
            "sdrMetadata" => Some(("sdrMetadata", F::Dictionary)),
            "uiHints" => Some(("uiHints", F::Dictionary)),
            "symmetryArguments" => Some(("symmetryArguments", F::Dictionary)),
            "clips" => Some(("clips", F::Dictionary)),
            "profilesInfo" => Some(("profilesInfo", F::UnregisteredDictionary)),
            "apiSchemas" => Some(("apiSchemas", F::TokenListOp)),
            _ => None,
        },
        Owner::Relationship => match key {
            "customData" => Some(("customData", F::Dictionary)),
            "assetInfo" => Some(("assetInfo", F::Dictionary)),
            "displayGroup" => Some(("displayGroup", F::String)),
            "displayName" => Some(("displayName", F::String)),
            "hidden" => Some(("hidden", F::Bool)),
            "noLoadHint" => Some(("noLoadHint", F::Bool)),
            "bindMaterialAs" => Some(("bindMaterialAs", F::Token)),
            "outputName" => Some(("outputName", F::Token)),
            "renderType" => Some(("renderType", F::Token)),
            "uiHints" => Some(("uiHints", F::Dictionary)),
            "symmetryArguments" => Some(("symmetryArguments", F::Dictionary)),
            _ => None,
        },
        Owner::Attribute => match key {
            "customData" => Some(("customData", F::Dictionary)),
            "assetInfo" => Some(("assetInfo", F::Dictionary)),
            "displayGroup" => Some(("displayGroup", F::String)),
            "displayName" => Some(("displayName", F::String)),
            "hidden" => Some(("hidden", F::Bool)),
            "colorSpace" => Some(("colorSpace", F::Token)),
            "allowedTokens" => Some(("allowedTokens", F::TokenArray)),
            "interpolation" => Some(("interpolation", F::Token)),
            "elementSize" => Some(("elementSize", F::Int)),
            "arraySizeConstraint" => Some(("arraySizeConstraint", F::Int64)),
            "weight" => Some(("weight", F::Float)),
            "unauthoredValuesIndex" => Some(("unauthoredValuesIndex", F::Int)),
            "constraintTargetIdentifier" => Some(("constraintTargetIdentifier", F::Token)),
            "connectability" => Some(("connectability", F::Token)),
            "renderType" => Some(("renderType", F::Token)),
            "sdrMetadata" => Some(("sdrMetadata", F::Dictionary)),
            "limits" => Some(("limits", F::Dictionary)),
            "uiHints" => Some(("uiHints", F::Dictionary)),
            "symmetryArguments" => Some(("symmetryArguments", F::Dictionary)),
            _ => None,
        },
    };
    shared.or(named).or(specific)
}

/// Converts a metadatum to its typed field. The value must have the
/// field's registered type, so that the USDA text of the same document
/// parses to the same value; only text types convert (a USDA string or
/// token is quoted text either way).
fn metadatum(owner: Owner, path: &str, entry: &Metadatum) -> Result<super::Field, UsdcWriteError> {
    let Some((name, ty)) = metadata_field(owner, &entry.key) else {
        return Err(UsdcWriteError::UnknownMetadata {
            path: path.into(),
            key: entry.key.clone(),
        });
    };
    use UsdaValue as U;
    let value = match (ty, &entry.value) {
        (FieldType::Token, U::Token(v) | U::String(v)) => Some(Value::Token(v.clone())),
        (FieldType::String, U::Token(v) | U::String(v)) => Some(Value::String(v.clone())),
        (FieldType::Double, U::Double(v)) => Some(Value::Double(*v)),
        (FieldType::Int, U::Int(v)) => Some(Value::Int(*v)),
        (FieldType::Int64, U::Int64(v)) => Some(Value::Int64(*v)),
        (FieldType::Float, U::Float(v)) => Some(Value::Float(*v)),
        (FieldType::Bool, U::Bool(v)) => Some(Value::Bool(*v)),
        (FieldType::Dictionary, v @ U::Dictionary(_)) => Some(natural(v)),
        (FieldType::Asset, U::Asset(v)) => Some(Value::Asset(v.clone())),
        (FieldType::TokenArray, U::TokenArray(v) | U::StringArray(v)) => {
            Some(Value::TokenArray(v.clone()))
        }
        (FieldType::StringArray, U::StringArray(v) | U::TokenArray(v)) => {
            Some(Value::StringArray(v.clone()))
        }
        (FieldType::TokenListOp, U::TokenListOp(op)) => Some(Value::TokenListOp(list_op(op))),
        (FieldType::UnregisteredDictionary, v @ U::Dictionary(_)) => {
            Some(Value::UnregisteredValue(Box::new(natural(v))))
        }
        _ => None,
    };
    let value = value.ok_or_else(|| UsdcWriteError::MetadataType {
        path: path.into(),
        key: entry.key.clone(),
        expected: ty,
    })?;
    Ok(super::Field {
        name: String::from(name),
        value,
    })
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use layerstack_usda::writer::WriteError;

    use super::*;
    use crate::writer::Field;

    fn mesh_doc() -> Document {
        let mut mesh = Prim::def("Mesh", "Tri");
        mesh.push_property(Attribute::new(
            "points",
            "point3f[]",
            UsdaValue::Float3Array(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
        ));
        mesh.push_property(
            Attribute::new(
                "primvars:st",
                "texCoord2f[]",
                UsdaValue::Float2Array(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            )
            .with_metadata("interpolation", UsdaValue::String("vertex".into()))
            .with_metadata("elementSize", UsdaValue::Int(1)),
        );
        mesh.push_property(
            Attribute::new(
                "subdivisionScheme",
                "token",
                UsdaValue::Token("none".into()),
            )
            .uniform(),
        );
        mesh.push_property(Attribute {
            value: None,
            ..Attribute::new("h", "half", UsdaValue::Int(0)).custom()
        });
        mesh.push_property(Attribute::new("t", "timecode", UsdaValue::Double(24.0)).custom());
        let mut root = Prim::def("Xform", "Root");
        root.metadata
            .push(Metadatum::new("kind", UsdaValue::Token("component".into())));
        root.metadata
            .push(Metadatum::new("doc", UsdaValue::Token("A root.".into())));
        root.children.push(mesh);
        root.children
            .push(Prim::new(UsdaSpecifier::Over, None, "Untyped"));
        Document {
            default_prim: Some("Root".into()),
            metadata: vec![
                Metadatum::new("metersPerUnit", UsdaValue::Double(1.0)),
                Metadatum::new("upAxis", UsdaValue::Token("Z".into())),
                Metadatum::new(
                    "customLayerData",
                    UsdaValue::Dictionary(vec![("n".into(), UsdaValue::Int(3))]),
                ),
            ],
            prims: vec![root],
            ..Document::new()
        }
    }

    fn fields(specs: &[Spec], path: &str) -> Vec<Field> {
        specs
            .iter()
            .find(|s| s.path == path)
            .unwrap()
            .fields
            .clone()
    }

    #[test]
    fn lowers_as_the_text_parser_stores() {
        let specs = document_specs(&mesh_doc()).unwrap();
        let paths: Vec<&str> = specs.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/",
                "/Root",
                "/Root/Tri",
                "/Root/Tri.points",
                "/Root/Tri.primvars:st",
                "/Root/Tri.subdivisionScheme",
                "/Root/Tri.h",
                "/Root/Tri.t",
                "/Root/Untyped",
            ],
            "specs"
        );
        assert_eq!(
            fields(&specs, "/"),
            [
                Field::new("defaultPrim", Value::Token("Root".into())),
                Field::new("metersPerUnit", Value::Double(1.0)),
                Field::new("upAxis", Value::Token("Z".into())),
                Field::new(
                    "customLayerData",
                    Value::Dictionary(vec![("n".into(), Value::Int(3))])
                ),
                Field::new("primChildren", Value::TokenVector(vec!["Root".into()])),
            ],
            "pseudo-root"
        );
        assert_eq!(
            fields(&specs, "/Root"),
            [
                Field::new("specifier", Value::Specifier(Specifier::Def)),
                Field::new("typeName", Value::Token("Xform".into())),
                Field::new("kind", Value::Token("component".into())),
                Field::new("documentation", Value::String("A root.".into())),
                Field::new(
                    "primChildren",
                    Value::TokenVector(vec!["Tri".into(), "Untyped".into()])
                ),
            ],
            "prim"
        );
        assert_eq!(
            fields(&specs, "/Root/Untyped"),
            [Field::new("specifier", Value::Specifier(Specifier::Over))],
            "untyped over"
        );
        assert_eq!(
            fields(&specs, "/Root/Tri.primvars:st"),
            [
                Field::new("custom", Value::Bool(false)),
                Field::new("typeName", Value::Token("texCoord2f[]".into())),
                Field::new("variability", Value::Variability(Variability::Varying)),
                Field::new(
                    "default",
                    Value::Vec2fArray(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]])
                ),
                Field::new("interpolation", Value::Token("vertex".into())),
                Field::new("elementSize", Value::Int(1)),
            ],
            "primvar"
        );
        assert_eq!(
            fields(&specs, "/Root/Tri.h"),
            [
                Field::new("custom", Value::Bool(true)),
                Field::new("typeName", Value::Token("half".into())),
                Field::new("variability", Value::Variability(Variability::Varying)),
            ],
            "declaration only"
        );
        assert_eq!(
            fields(&specs, "/Root/Tri.t")[3],
            Field::new("default", Value::TimeCode(24.0)),
            "timecode"
        );
        let uniform = &fields(&specs, "/Root/Tri.subdivisionScheme")[2];
        assert_eq!(
            uniform.value,
            Value::Variability(Variability::Uniform),
            "uniform"
        );
    }

    #[test]
    fn writes_and_reads_back() {
        let bytes = write_document(&mesh_doc()).unwrap();
        assert_eq!(&bytes[..8], b"PXR-USDC", "magic");
        assert_eq!(&bytes[8..11], &[0, 9, 0], "timecode needs 0.9.0");
        assert_eq!(bytes, write_document(&mesh_doc()).unwrap(), "deterministic");
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let sections = crate::section::parse_sections(
            &bytes,
            &toc,
            header.crate_version(),
            &mut crate::DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        assert_eq!(sections.specs.len(), 9, "spec count");
    }

    /// Double-typed layer metadata reads back as the same value from the
    /// USDA (through the text parser) and from the USDC (through the crate
    /// reader), including values that are not exactly a `float`.
    #[test]
    fn double_metadata_agrees_between_usda_and_usdc() {
        use layerstack_usda::ast::{LayerMeta, MetadataValue, Value as AstValue};

        let values = [
            ("metersPerUnit", 0.1),
            ("kilogramsPerUnit", 0.001),
            ("timeCodesPerSecond", 23.976),
            ("startTimeCode", 1e-300),
        ];
        let mut doc = mesh_doc();
        doc.metadata.retain(|m| m.key != "metersPerUnit");
        for (key, v) in values {
            doc.metadata.push(Metadatum::new(key, UsdaValue::Double(v)));
        }

        let text = doc.to_usda().unwrap();
        let parsed = layerstack_usda::parser::parse(&text);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let bytes = write_document(&doc).unwrap();
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let sections = crate::section::parse_sections(
            &bytes,
            &toc,
            header.crate_version(),
            &mut crate::DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        let root = sections
            .specs
            .iter()
            .find(|s| sections.paths[s.path_index as usize] == "/")
            .unwrap();

        for (key, v) in values {
            let from_usda = parsed
                .layer
                .metadata
                .iter()
                .find_map(|m| match m {
                    LayerMeta::Custom(e) if e.key == key => match &e.value {
                        MetadataValue::Value(AstValue::Number(n)) => Some(*n),
                        _ => None,
                    },
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{key} in the USDA"));
            let mut i = root.fieldset_index as usize;
            let from_usdc = loop {
                let index = sections.fieldsets[i];
                assert!(index >= 0, "{key} in the USDC");
                let field = sections.fields[index as usize];
                if sections.tokens[field.token_index as usize] == key {
                    let rep = crate::value_rep::RawValueRep::new(field.value_rep);
                    match crate::value_rep::decode_value(&rep, &bytes, &sections) {
                        Ok(crate::value_rep::CrateValue::Double(d)) => break d,
                        other => panic!("{key}: expected a double, got {other:?}"),
                    }
                }
                i += 1;
            };
            assert_eq!(from_usda.to_bits(), v.to_bits(), "{key} from USDA");
            assert_eq!(from_usdc.to_bits(), v.to_bits(), "{key} from USDC");
        }
    }

    #[test]
    fn lowers_relationships_connections_and_api_schemas() {
        let mut shader = Prim::def("Shader", "S");
        shader.push_property(Attribute {
            value: None,
            ..Attribute::new("outputs:surface", "token", UsdaValue::Int(0))
        });
        let mut surface = Attribute::new("outputs:surface", "token", UsdaValue::Int(0));
        surface.value = None;
        surface.connections = Some(UsdaListOp::explicit(vec![
            "/Root/M/S.outputs:surface".into(),
        ]));
        let mut input = Attribute::new("inputs:x", "float", UsdaValue::Float(0.5))
            .with_metadata("connectability", UsdaValue::Token("interfaceOnly".into()));
        input.connections = Some(UsdaListOp::explicit(vec![
            "/Root/M/S.outputs:surface".into(),
        ]));
        let mut material = Prim::def("Material", "M");
        material.push_property(surface);
        material.push_property(input);
        material.children.push(shader);
        let mut root = Prim::def("Mesh", "Root");
        root.metadata.push(Metadatum::new(
            "apiSchemas",
            UsdaValue::TokenListOp(UsdaListOp::prepend(vec!["MaterialBindingAPI".into()])),
        ));
        root.push_property(Attribute::new("a", "int", UsdaValue::Int(1)));
        let mut binding = Relationship::new("material:binding", "/Root/M");
        binding.metadata.push(Metadatum::new(
            "bindMaterialAs",
            UsdaValue::Token("strongerThanDescendants".into()),
        ));
        root.push_property(binding);
        root.push_property(Relationship {
            targets: Some(UsdaListOp::explicit(vec![])),
            ..Relationship::new("blocked", "/Root").custom()
        });
        root.push_property(Relationship {
            targets: None,
            ..Relationship::new("bare", "/Root")
        });
        root.children.push(material);
        let doc = Document {
            prims: vec![root],
            ..Document::new()
        };
        let specs = document_specs(&doc).unwrap();
        let explicit = |paths: &[&str]| {
            Value::PathListOp(ListOp::explicit(
                paths.iter().map(|p| (*p).into()).collect(),
            ))
        };
        assert_eq!(
            fields(&specs, "/Root"),
            [
                Field::new("specifier", Value::Specifier(Specifier::Def)),
                Field::new("typeName", Value::Token("Mesh".into())),
                Field::new(
                    "apiSchemas",
                    Value::TokenListOp(ListOp::prepend(vec!["MaterialBindingAPI".into()]))
                ),
                Field::new("primChildren", Value::TokenVector(vec!["M".into()])),
                Field::new(
                    "properties",
                    Value::TokenVector(vec![
                        "a".into(),
                        "material:binding".into(),
                        "blocked".into(),
                        "bare".into()
                    ])
                ),
            ],
            "prim"
        );
        assert_eq!(
            fields(&specs, "/Root.material:binding"),
            [
                Field::new("variability", Value::Variability(Variability::Uniform)),
                Field::new("targetPaths", explicit(&["/Root/M"])),
                Field::new(
                    "bindMaterialAs",
                    Value::Token("strongerThanDescendants".into())
                ),
            ],
            "relationship"
        );
        assert_eq!(
            fields(&specs, "/Root.blocked"),
            [
                Field::new("variability", Value::Variability(Variability::Uniform)),
                Field::new("custom", Value::Bool(true)),
                Field::new("targetPaths", explicit(&[])),
            ],
            "custom relationship with an explicit empty target list"
        );
        assert_eq!(
            fields(&specs, "/Root.bare"),
            [Field::new(
                "variability",
                Value::Variability(Variability::Uniform)
            )],
            "bare relationship"
        );
        assert_eq!(
            fields(&specs, "/Root/M.inputs:x"),
            [
                Field::new("custom", Value::Bool(false)),
                Field::new("typeName", Value::Token("float".into())),
                Field::new("variability", Value::Variability(Variability::Varying)),
                Field::new("default", Value::Float(0.5)),
                Field::new("connectability", Value::Token("interfaceOnly".into())),
                Field::new("connectionPaths", explicit(&["/Root/M/S.outputs:surface"])),
            ],
            "value, metadata and connection"
        );
        assert_eq!(
            fields(&specs, "/Root/M.outputs:surface")[3],
            Field::new("connectionPaths", explicit(&["/Root/M/S.outputs:surface"])),
            "connection only"
        );
        let file = write_document(&doc).unwrap();
        assert_eq!(&file[..8], b"PXR-USDC", "writes");
    }

    /// Dictionary metadata: the registered UI hint, limit and symmetry
    /// dictionaries on every owner, and prim `profilesInfo`, which OpenUSD
    /// registers nowhere and so stores as an `SdfUnregisteredValue`. The
    /// profile data is carried exactly as authored; nothing is added.
    #[test]
    fn transports_dictionary_metadata_and_profiles_info() {
        let dict = |entries: Vec<(&str, UsdaValue)>| {
            UsdaValue::Dictionary(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
        };
        let profiles = dict(vec![
            (
                "capabilityUsages",
                dict(vec![("usd.geom.mesh", UsdaValue::String("hard".into()))]),
            ),
            (
                "profileCompatibility",
                dict(vec![(
                    "vnd.apple.visionos_v1",
                    UsdaValue::StringArray(vec!["usd.geom.hairAndFur".into()]),
                )]),
            ),
        ]);
        let limits = dict(vec![
            ("soft", dict(vec![("min", UsdaValue::Float(0.0))])),
            ("hard", dict(vec![("max", UsdaValue::Float(10.0))])),
        ]);
        let hints = dict(vec![("displayGroup", UsdaValue::String("Shape".into()))]);
        let mut prim = Prim::def("Xform", "Root");
        prim.metadata.push(Metadatum::new(
            "apiSchemas",
            UsdaValue::TokenListOp(UsdaListOp::prepend(vec!["ClaimsAPI".into()])),
        ));
        prim.metadata
            .push(Metadatum::new("profilesInfo", profiles.clone()));
        prim.metadata.push(Metadatum::new(
            "customData",
            dict(vec![("profilesInfo", profiles.clone())]),
        ));
        prim.metadata.push(Metadatum::new("uiHints", hints.clone()));
        prim.metadata.push(Metadatum::new(
            "symmetryArguments",
            dict(vec![("axis", UsdaValue::Token("x".into()))]),
        ));
        prim.push_property(
            Attribute::new("size", "float", UsdaValue::Float(1.0))
                .with_metadata("limits", limits.clone())
                .with_metadata("uiHints", hints.clone()),
        );
        let mut rel = Relationship::new("target", "/Root");
        rel.metadata.push(Metadatum::new("uiHints", hints.clone()));
        prim.push_property(rel);
        let doc = Document {
            metadata: vec![Metadatum::new(
                "fallbackPrimTypes",
                dict(vec![(
                    "MyType",
                    UsdaValue::TokenArray(vec!["Xform".into()]),
                )]),
            )],
            prims: vec![prim],
            ..Document::new()
        };
        let specs = document_specs(&doc).unwrap();
        assert_eq!(
            fields(&specs, "/")[0],
            Field::new("fallbackPrimTypes", natural(&doc.metadata[0].value)),
            "layer fallbackPrimTypes"
        );
        let root = fields(&specs, "/Root");
        assert_eq!(
            root[3],
            Field::new(
                "profilesInfo",
                Value::UnregisteredValue(Box::new(natural(&profiles)))
            ),
            "bare profilesInfo is an unregistered dictionary"
        );
        assert_eq!(
            root[4].value,
            Value::Dictionary(vec![("profilesInfo".into(), natural(&profiles))]),
            "ClaimsAPI's customData.profilesInfo is ordinary customData"
        );
        assert_eq!(root[5], Field::new("uiHints", natural(&hints)), "uiHints");
        assert_eq!(
            fields(&specs, "/Root.size")[4],
            Field::new("limits", natural(&limits)),
            "limits"
        );
        assert_eq!(
            fields(&specs, "/Root.target")[2],
            Field::new("uiHints", natural(&hints)),
            "relationship uiHints"
        );
        let unregistered = specs
            .iter()
            .flat_map(|s| &s.fields)
            .filter(|f| matches!(f.value, Value::UnregisteredValue(_)))
            .count();
        assert_eq!(unregistered, 1, "only profilesInfo is unregistered");

        // The crate reader sees the dictionary itself.
        let bytes = write_document(&doc).unwrap();
        let header = crate::header::parse_header(&bytes).unwrap();
        let toc = crate::toc::parse_toc(&bytes, header.toc_offset).unwrap();
        let sections = crate::section::parse_sections(
            &bytes,
            &toc,
            header.crate_version(),
            &mut crate::DecodeBudget::with_limit(u64::MAX),
        )
        .unwrap();
        let profiles_field = sections
            .fields
            .iter()
            .find(|f| sections.tokens[f.token_index as usize] == "profilesInfo")
            .unwrap();
        let rep = crate::value_rep::RawValueRep::new(profiles_field.value_rep);
        assert_eq!(
            rep.value_type().unwrap(),
            crate::value_type::ValueType::UnregisteredValue,
            "stored as an unregistered value"
        );
        let crate::value_rep::CrateValue::Dictionary(entries) =
            crate::value_rep::decode_value(&rep, &bytes, &sections).unwrap()
        else {
            panic!("profilesInfo holds a dictionary");
        };
        let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["capabilityUsages", "profileCompatibility"], "keys");

        // Other unregistered keys are still rejected, dictionary or not.
        let mut doc = doc;
        doc.prims[0]
            .metadata
            .push(Metadatum::new("exedraInfo", hints));
        assert!(
            matches!(
                document_specs(&doc),
                Err(UsdcWriteError::UnknownMetadata { .. })
            ),
            "only profilesInfo is transported unregistered"
        );
    }

    /// Reorder statements, list-edited targets and connections and a value
    /// block land where the text parser stores them.
    #[test]
    fn lowers_reorders_list_edits_and_blocks() {
        let mut prim = Prim::def("Xform", "A");
        prim.property_order = Some(vec!["x".into(), "r".into()]);
        prim.prim_order = Some(vec!["C".into()]);
        let mut edited = Relationship {
            targets: Some(UsdaListOp::prepend(vec!["/A/C".into()])),
            ..Relationship::new("r", "/A").custom()
        };
        edited
            .metadata
            .push(Metadatum::new("doc", UsdaValue::String("r".into())));
        prim.push_property(edited);
        let mut explicit = Relationship::new("s", "/A/C");
        explicit
            .metadata
            .push(Metadatum::new("doc", UsdaValue::String("s".into())));
        prim.push_property(explicit);
        let mut x = Attribute::new("x", "float", UsdaValue::Block);
        x.connections = Some(UsdaListOp {
            deleted: vec!["/A.y".into()],
            ..UsdaListOp::default()
        });
        prim.push_property(x);
        prim.children.push(Prim::def("Scope", "C"));
        let doc = Document {
            prim_order: Some(vec!["A".into()]),
            prims: vec![prim],
            ..Document::new()
        };
        let specs = document_specs(&doc).unwrap();
        let tokens =
            |names: &[&str]| Value::TokenVector(names.iter().map(|n| String::from(*n)).collect());
        assert_eq!(
            fields(&specs, "/"),
            [
                Field::new("primOrder", tokens(&["A"])),
                Field::new("primChildren", tokens(&["A"])),
            ],
            "pseudo-root"
        );
        assert_eq!(
            fields(&specs, "/A"),
            [
                Field::new("specifier", Value::Specifier(Specifier::Def)),
                Field::new("typeName", Value::Token("Xform".into())),
                Field::new("propertyOrder", tokens(&["x", "r"])),
                Field::new("primOrder", tokens(&["C"])),
                Field::new("primChildren", tokens(&["C"])),
                Field::new("properties", tokens(&["r", "s", "x"])),
            ],
            "prim"
        );
        let paths = |items: &[&str]| items.iter().map(|p| String::from(*p)).collect();
        assert_eq!(
            fields(&specs, "/A.r"),
            [
                Field::new("variability", Value::Variability(Variability::Uniform)),
                Field::new("custom", Value::Bool(true)),
                Field::new("documentation", Value::String("r".into())),
                Field::new(
                    "targetPaths",
                    Value::PathListOp(ListOp::prepend(paths(&["/A/C"])))
                ),
            ],
            "list edits follow the declaration's metadata"
        );
        assert_eq!(
            fields(&specs, "/A.s")[1..],
            [
                Field::new(
                    "targetPaths",
                    Value::PathListOp(ListOp::explicit(paths(&["/A/C"])))
                ),
                Field::new("documentation", Value::String("s".into())),
            ],
            "an explicit list is part of the declaration"
        );
        assert_eq!(
            fields(&specs, "/A.x")[3..],
            [
                Field::new("default", Value::Block),
                Field::new(
                    "connectionPaths",
                    Value::PathListOp(ListOp {
                        deleted: paths(&["/A.y"]),
                        ..ListOp::default()
                    })
                ),
            ],
            "blocked default and deleted connection"
        );
    }

    /// Imports USDA text into a layer with its own interners.
    fn import_usda(source: &str) -> (Layer, TokenInterner, PathInterner) {
        let parsed = layerstack_usda::parser::parse(source);
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let result = layerstack_usda::emit::emit(
            &parsed.layer,
            layerstack::LayerId(1),
            &mut tokens,
            &mut paths,
            &mut NoAssets,
        );
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        (result.layer, tokens, paths)
    }

    struct NoAssets;

    impl layerstack::AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<layerstack::LayerId>,
            _: &mut TokenInterner,
            _: &mut PathInterner,
        ) -> Result<layerstack::ResolvedAsset, layerstack::AssetResolveError> {
            Err(layerstack::AssetResolveError::NotFound)
        }

        fn resolved_path(&self, _: layerstack::LayerId) -> Option<&str> {
            None
        }
    }

    /// A layer saved as USDC reads back as the layer it was saved from:
    /// saving the read-back layer as USDA gives the same text as saving
    /// the original, so both formats carried the same authored slots.
    #[test]
    fn saved_layer_reads_back_as_the_same_layer() {
        let source = r#"#usda 1.0
(
    "A layer."
    defaultPrim = "A"
    upAxis = "Z"
)

reorder rootPrims = ["A"]

def Xform "A" (
    prepend apiSchemas = ["ClaimsAPI", "exedra:Tagged:one"]
    profilesInfo = {
        dictionary capabilityUsages = {
            string "usd.geom.mesh" = "hard"
        }
    }
)
{
    reorder properties = ["y", "x"]
    uniform token exedra:mode = "a"
    rel r = </A.x>
    float x = 1 (
        limits = {
            dictionary soft = {
                float min = 0
            }
        }
    )
    delete float y.connect = </A.x>
    custom double z = None
    timecode t = 24
    asset[] files = [@./a.png@, @b/c.exr@]

    def Scope "C"
    {
    }
}
"#;
        let (layer, tokens, paths) = import_usda(source);
        let usda = layerstack_usda::save::save_usda(&layer, &tokens, &paths).unwrap();
        let bytes = save_layer(&layer, &tokens, &paths).unwrap();

        let mut tokens = TokenInterner::default();
        let mut paths = PathInterner::default();
        let read = crate::read_usdc(
            &bytes,
            layerstack::LayerId(1),
            &mut tokens,
            &mut paths,
            &mut NoAssets,
        )
        .unwrap();
        assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
        let again = layerstack_usda::save::save_usda(&read.layer, &tokens, &paths).unwrap();
        assert_eq!(again, usda, "USDC and USDA carry the same layer");
    }

    /// A `quath[]` default reads back as the same quaternions from the USDA
    /// text and from the USDC: real part first in text, `[i, j, k, r]` in
    /// both layers.
    #[test]
    fn quath_arrays_agree_between_usda_and_usdc() {
        let quats = vec![[0, 0, 0x39a8, 0x39a8], [0x3c00, 0, 0, 0], [0, 0, 0, 0x3c00]];
        let mut root = Prim::def("PointInstancer", "Root");
        root.push_property(Attribute::new(
            "orientations",
            "quath[]",
            UsdaValue::QuathArray(quats.clone()),
        ));
        let doc = Document {
            prims: vec![root],
            ..Document::new()
        };
        let (layer, mut tokens, mut paths) = import_usda(&doc.to_usda().unwrap());
        let mut usdc_tokens = TokenInterner::default();
        let mut usdc_paths = PathInterner::default();
        let read = crate::read_usdc(
            &write_document(&doc).unwrap(),
            layerstack::LayerId(1),
            &mut usdc_tokens,
            &mut usdc_paths,
            &mut NoAssets,
        )
        .unwrap();
        assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
        let expected = Some(layerstack::Value::Array(
            quats.iter().map(|&q| layerstack::Value::Quath(q)).collect(),
        ));
        let path =
            layerstack::PropertyPath::parse("/Root.orientations", &mut tokens, &mut paths).unwrap();
        assert_eq!(layer.property(path).unwrap().default, expected, "USDA");
        let path = layerstack::PropertyPath::parse(
            "/Root.orientations",
            &mut usdc_tokens,
            &mut usdc_paths,
        )
        .unwrap();
        assert_eq!(read.layer.property(path).unwrap().default, expected, "USDC");
    }

    #[test]
    fn save_layer_rejects_before_writing() {
        let (layer, tokens, paths) = import_usda(
            "#usda 1.0\ndef \"A\"\n{\n    float a.timeSamples = {\n        0: 1,\n    }\n}\n",
        );
        assert_eq!(
            save_layer(&layer, &tokens, &paths),
            Err(UsdcWriteError::Save(
                layerstack_usda::save::SaveError::Unsupported {
                    path: "/A.a".into(),
                    feature: layerstack_usda::save::Unsupported::TimeSamples,
                }
            )),
            "the USDA save's error, before any output"
        );
        // Unregistered metadata is saved as USDA but has no crate form.
        let (layer, tokens, paths) =
            import_usda("#usda 1.0\ndef \"A\" (\n    exedraNote = \"n\"\n)\n{\n}\n");
        assert!(layerstack_usda::save::save_usda(&layer, &tokens, &paths).is_ok());
        assert_eq!(
            save_layer(&layer, &tokens, &paths),
            Err(UsdcWriteError::UnknownMetadata {
                path: "/A".into(),
                key: "exedraNote".into()
            }),
            "unregistered metadata"
        );
    }

    #[test]
    fn rejects_what_usda_rejects_and_untypable_metadata() {
        let mut doc = mesh_doc();
        doc.default_prim = Some("Missing".into());
        assert_eq!(
            document_specs(&doc),
            Err(UsdcWriteError::Document(WriteError::DefaultPrimNotFound {
                name: "Missing".into()
            })),
            "document validation"
        );
        let mut doc = mesh_doc();
        doc.metadata
            .push(Metadatum::new("fooBar", UsdaValue::String("x".into())));
        assert_eq!(
            document_specs(&doc),
            Err(UsdcWriteError::UnknownMetadata {
                path: "/".into(),
                key: "fooBar".into()
            }),
            "unregistered layer metadata"
        );
        let mut doc = mesh_doc();
        doc.prims[0]
            .metadata
            .push(Metadatum::new("elementSize", UsdaValue::Int(2)));
        assert!(
            matches!(
                document_specs(&doc),
                Err(UsdcWriteError::UnknownMetadata { .. })
            ),
            "elementSize is attribute metadata"
        );
        // A registered type is required, not converted to: USDA would print
        // a `float` 0.1 as `0.1`, which parses as the double 0.1, while
        // widening the `float` gives 0.10000000149011612.
        for value in [
            UsdaValue::String("1".into()),
            UsdaValue::Float(0.1),
            UsdaValue::Int(1),
        ] {
            let mut doc = mesh_doc();
            doc.metadata[0].value = value.clone();
            assert_eq!(
                document_specs(&doc),
                Err(UsdcWriteError::MetadataType {
                    path: "/".into(),
                    key: "metersPerUnit".into(),
                    expected: FieldType::Double
                }),
                "metersPerUnit is a double, not {value:?}"
            );
        }
        let mut doc = mesh_doc();
        let Property::Attribute(points) = &mut doc.prims[0].children[0].properties[0] else {
            unreachable!("points is an attribute");
        };
        points
            .metadata
            .push(Metadatum::new("elementSize", UsdaValue::UInt(2)));
        assert!(
            matches!(
                document_specs(&doc),
                Err(UsdcWriteError::MetadataType {
                    expected: FieldType::Int,
                    ..
                })
            ),
            "elementSize is an int"
        );
    }
}
