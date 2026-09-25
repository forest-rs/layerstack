// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Lowering of an authored USDA [`Document`] to crate specs.
//!
//! The result is what OpenUSD's text parser stores for the USDA the same
//! document writes (`Document::to_usda`), so both files describe one layer:
//! the same specs, fields, field types and children order. In particular:
//!
//! - the pseudo-root holds `defaultPrim`, then the layer metadata in order,
//!   then `primChildren`;
//! - a prim holds `specifier`, `typeName` (for typed prims), its metadata
//!   in order (a token list op such as `prepend apiSchemas` included), then
//!   `primChildren` and `properties` (attributes, then relationships, as
//!   the USDA writer orders them);
//! - an attribute holds `custom`, `typeName` (the declared name, role and
//!   `[]` included), `variability`, `default` when a value is authored, its
//!   metadata in order, then `connectionPaths` (an explicit path list op)
//!   when it has connections, which USDA writes as a later `.connect`
//!   statement;
//! - a relationship holds `variability` (always uniform), `custom` only
//!   when custom, `targetPaths` (an explicit path list op, empty for
//!   `rel r = None`) when targets are authored, then its metadata;
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
//! does not produce.
//!
//! Spec: AOUSD Core §7.4 (metadata), §7.6 (core fields), §16.3 (crate
//! format).

use alloc::string::String;
use alloc::vec::Vec;

use layerstack_usda::writer::{
    Attribute, Document, ListOp as UsdaListOp, Metadatum, Prim, Relationship,
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
    if !prim.children.is_empty() {
        spec = spec.with_field(
            "primChildren",
            Value::TokenVector(prim.children.iter().map(|c| c.name.clone()).collect()),
        );
    }
    if !prim.attributes.is_empty() || !prim.relationships.is_empty() {
        let names = prim.attributes.iter().map(|a| a.name.clone());
        let names = names.chain(prim.relationships.iter().map(|r| r.name.clone()));
        spec = spec.with_field("properties", Value::TokenVector(names.collect()));
    }
    specs.push(spec);
    for attribute in &prim.attributes {
        specs.push(lower_attribute(attribute, &path)?);
    }
    for relationship in &prim.relationships {
        specs.push(lower_relationship(relationship, &path)?);
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
    if !attribute.connections.is_empty() {
        spec = spec.with_field(
            "connectionPaths",
            Value::PathListOp(ListOp::explicit(attribute.connections.clone())),
        );
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
    if let Some(targets) = &relationship.targets {
        spec = spec.with_field(
            "targetPaths",
            Value::PathListOp(ListOp::explicit(targets.clone())),
        );
    }
    for entry in &relationship.metadata {
        spec.fields
            .push(metadatum(Owner::Relationship, &path, entry)?);
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
        U::TokenListOp(op) => Value::TokenListOp(list_op(op)),
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
    /// `bool`.
    Bool,
    /// `VtDictionary`.
    Dictionary,
    /// `SdfAssetPath`.
    Asset,
    /// `VtTokenArray`; a USDA token or string array.
    TokenArray,
    /// `SdfTokenListOp`; a USDA token list op.
    TokenListOp,
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
/// files. USDA's `doc` is the `documentation` field.
pub fn metadata_field(owner: Owner, key: &str) -> Option<(&'static str, FieldType)> {
    use FieldType as F;
    let shared = match key {
        "doc" => Some(("documentation", F::String)),
        _ => None,
    };
    let specific = match owner {
        Owner::Layer => match key {
            "defaultPrim" => Some(("defaultPrim", F::Token)),
            "customLayerData" => Some(("customLayerData", F::Dictionary)),
            "expressionVariables" => Some(("expressionVariables", F::Dictionary)),
            "startTimeCode" => Some(("startTimeCode", F::Double)),
            "endTimeCode" => Some(("endTimeCode", F::Double)),
            "timeCodesPerSecond" => Some(("timeCodesPerSecond", F::Double)),
            "framesPerSecond" => Some(("framesPerSecond", F::Double)),
            "framePrecision" => Some(("framePrecision", F::Int)),
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
            "sdrMetadata" => Some(("sdrMetadata", F::Dictionary)),
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
            "renderType" => Some(("renderType", F::Token)),
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
            "unauthoredValuesIndex" => Some(("unauthoredValuesIndex", F::Int)),
            "constraintTargetIdentifier" => Some(("constraintTargetIdentifier", F::Token)),
            "connectability" => Some(("connectability", F::Token)),
            "renderType" => Some(("renderType", F::Token)),
            "sdrMetadata" => Some(("sdrMetadata", F::Dictionary)),
            _ => None,
        },
    };
    shared.or(specific)
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
        (FieldType::Bool, U::Bool(v)) => Some(Value::Bool(*v)),
        (FieldType::Dictionary, v @ U::Dictionary(_)) => Some(natural(v)),
        (FieldType::Asset, U::Asset(v)) => Some(Value::Asset(v.clone())),
        (FieldType::TokenArray, U::TokenArray(v) | U::StringArray(v)) => {
            Some(Value::TokenArray(v.clone()))
        }
        (FieldType::TokenListOp, U::TokenListOp(op)) => Some(Value::TokenListOp(list_op(op))),
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
        mesh.attributes.push(Attribute::new(
            "points",
            "point3f[]",
            UsdaValue::Float3Array(vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]),
        ));
        mesh.attributes.push(
            Attribute::new(
                "primvars:st",
                "texCoord2f[]",
                UsdaValue::Float2Array(vec![[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]]),
            )
            .with_metadata("interpolation", UsdaValue::String("vertex".into()))
            .with_metadata("elementSize", UsdaValue::Int(1)),
        );
        mesh.attributes.push(
            Attribute::new(
                "subdivisionScheme",
                "token",
                UsdaValue::Token("none".into()),
            )
            .uniform(),
        );
        mesh.attributes.push(Attribute {
            value: None,
            ..Attribute::new("h", "half", UsdaValue::Int(0)).custom()
        });
        mesh.attributes
            .push(Attribute::new("t", "timecode", UsdaValue::Double(24.0)).custom());
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
        shader.attributes.push(Attribute {
            value: None,
            ..Attribute::new("outputs:surface", "token", UsdaValue::Int(0))
        });
        let mut surface = Attribute::new("outputs:surface", "token", UsdaValue::Int(0));
        surface.value = None;
        surface.connections.push("/Root/M/S.outputs:surface".into());
        let mut input = Attribute::new("inputs:x", "float", UsdaValue::Float(0.5))
            .with_metadata("connectability", UsdaValue::Token("interfaceOnly".into()));
        input.connections.push("/Root/M/S.outputs:surface".into());
        let mut material = Prim::def("Material", "M");
        material.attributes.push(surface);
        material.attributes.push(input);
        material.children.push(shader);
        let mut root = Prim::def("Mesh", "Root");
        root.metadata.push(Metadatum::new(
            "apiSchemas",
            UsdaValue::TokenListOp(UsdaListOp::prepend(vec!["MaterialBindingAPI".into()])),
        ));
        root.attributes
            .push(Attribute::new("a", "int", UsdaValue::Int(1)));
        let mut binding = Relationship::new("material:binding", "/Root/M");
        binding.metadata.push(Metadatum::new(
            "bindMaterialAs",
            UsdaValue::Token("strongerThanDescendants".into()),
        ));
        root.relationships.push(binding);
        root.relationships.push(Relationship {
            targets: Some(vec![]),
            ..Relationship::new("blocked", "/Root").custom()
        });
        root.relationships.push(Relationship {
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
        doc.prims[0].children[0].attributes[0]
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
