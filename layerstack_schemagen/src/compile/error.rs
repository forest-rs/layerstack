// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Why a schema did not compile: one diagnostic per usdGenSchema exception.

use core::fmt;

/// Why [`compile`](super::compile) produced nothing.
///
/// Each variant is one way usdGenSchema fails. [`CompileError::Schema`]
/// carries the class (or library prim) and the rule it breaks, so a caller
/// can match on the rule rather than on message text; `Display` writes
/// usdGenSchema's own message.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompileError {
    /// A layer of the schema stage could not be read or parsed: the schema
    /// file itself, a sublayer, or its `userDoc/schemaUserDoc.usda`.
    Read {
        /// The layer, as it was named or resolved.
        layer: String,
        /// What went wrong.
        message: String,
    },
    /// A sublayer resolved to no file in the anchoring layer's directory or
    /// any search path ([`CompileOptions`](super::CompileOptions)).
    /// usdGenSchema only warns and goes on without the layer, which then
    /// fails later or generates a different schema; this stops instead.
    UnresolvedSublayer {
        /// The layer whose `subLayers` names it.
        layer: String,
        /// The sublayer's asset path, as authored (`usdGeom/schema.usda`).
        asset: String,
    },
    /// The schema stage composed with errors that are not unresolved
    /// sublayers, such as an inherit of a prim no layer defines.
    Composition {
        /// The composition errors, as layerstack reports them.
        errors: Vec<String>,
    },
    /// A class, or the library's `GLOBAL` prim, breaks one of usdGenSchema's
    /// rules (`_GetSchemaDefException` and its other exceptions).
    Schema {
        /// The prim or property the rule is about (`/Mesh`, `/Mesh.points`).
        path: String,
        /// The rule it breaks.
        rule: Rule,
    },
    /// Fields that a schema cannot specify (`_ValidateFields`): those
    /// `UsdSchemaRegistry::IsDisallowedField` names, other than
    /// `inheritPaths`, `customData` and `specifier`.
    InvalidFields {
        /// The class whose prim or properties author them.
        class: String,
        /// Each field, where it is authored.
        fields: Vec<InvalidField>,
    },
    /// The existing `plugInfo.json` (see
    /// [`CompileOptions::with_existing_plug_info`](super::CompileOptions::with_existing_plug_info))
    /// is not JSON, or has a `Plugins` list without the library's plugin.
    PlugInfo {
        /// What is wrong with it.
        message: String,
    },
    /// The generated schema layer could not be written as USDA. This is a
    /// gap in layerstack, not in the schema.
    Write {
        /// What could not be written.
        message: String,
    },
    /// The library's Rust module could not be generated
    /// ([`CompiledSchema::rust_module`](super::CompiledSchema::rust_module)):
    /// an accessor's name, from a property's `apiName` or else its USD name,
    /// is a Rust keyword or collides with another; two schemas make the
    /// same Rust type; or a schema builds on a library that
    /// `layerstack_schemas` has no views of.
    Rust {
        /// What is wrong, naming the schema.
        message: String,
    },
}

/// A field a schema cannot specify, where it is authored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidField {
    /// The prim or property spec (`/Foo`, `/Foo.bar`).
    pub path: String,
    /// The field (`kind`, `targetPaths`, `timeSamples`).
    pub field: String,
}

/// A rule of usdGenSchema's that a schema breaks.
///
/// The variants follow `usdGenSchema.py`; each names the function that
/// checks it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rule {
    /// `_GetLibMetadata`: a layer of the schema has no `/GLOBAL` prim.
    MissingGlobalPrim {
        /// The layer.
        layer: String,
    },
    /// `_GetLibMetadata`: `/GLOBAL` has no `customData`.
    MissingLibraryData {
        /// The layer.
        layer: String,
    },
    /// `_GetLibName`: `/GLOBAL`'s `customData` has no `libraryName`.
    MissingLibraryName {
        /// The layer.
        layer: String,
    },
    /// `_GetLibPath`: `/GLOBAL`'s `customData` has no `libraryPath`, and
    /// the library is not codeless (`skipCodeGeneration = true`).
    MissingLibraryPath {
        /// The layer.
        layer: String,
    },
    /// `_GetDefiningLayerAndPrim`: no layer of the schema stage defines
    /// the schema a class inherits from.
    UndefinedBase {
        /// The base's name.
        base: String,
    },
    /// `ClassInfo`: a class's type name is not its prim name.
    TypeNameMismatch {
        /// The declared type name.
        type_name: String,
        /// The class's name.
        name: String,
    },
    /// `ClassInfo`: a class inherits from more than one class.
    MultipleInheritance {
        /// What it inherits, in order.
        inherits: Vec<String>,
    },
    /// `ClassInfo`: `apiSchemas` does more than prepend.
    ApiSchemasNotPrepended,
    /// `ClassInfo`: an inherit names no prim of the base's layer.
    InvalidInherit {
        /// The inherit path.
        inherit: String,
    },
    /// `ClassInfo`: a class inherits, directly or not, from a schema of
    /// its own family.
    InheritsOwnFamily,
    /// `ClassInfo`: a customData field only multiple-apply schemas take
    /// (`propertyNamespacePrefix`, `apiSchemaAllowedInstanceNames`,
    /// `apiSchemaInstances`).
    MultipleApplyOnlyField {
        /// The field.
        field: &'static str,
    },
    /// `ClassInfo`: `apiSchemaType` is not `nonApplied`, `singleApply` or
    /// `multipleApply`.
    InvalidApiSchemaType {
        /// The authored value.
        value: String,
    },
    /// `ClassInfo`: `apiSchemaAutoApplyTo` on a schema that is not a
    /// single-apply API schema.
    AutoApplyNotSingleApply,
    /// `ClassInfo`: `apiSchemaCanOnlyApplyTo` on a schema that is not an
    /// applied API schema.
    CanOnlyApplyNotApplied,
    /// `ClassInfo`: a type name on a class that does not inherit `Typed`.
    ConcreteNotTyped,
    /// `ClassInfo`: the name is not one
    /// `UsdSchemaRegistry::IsAllowedSchemaIdentifier` allows.
    NotAllowedIdentifier,
    /// `ClassInfo`: an API schema whose family does not end in `API`.
    MissingApiSuffix,
    /// `ClassInfo`: a typed schema whose family ends in `API`.
    TypedApiSuffix,
    /// `ClassInfo`: an applied API schema that does not inherit
    /// `APISchemaBase` directly.
    AppliedApiBase,
    /// `ClassInfo`: a non-applied API schema that inherits neither
    /// `APISchemaBase` nor another non-applied API schema.
    NonAppliedApiBase,
    /// `ClassInfo`: an applied `apiSchemaType` on a schema that is not an
    /// API schema.
    ApiSchemaTypeNotApi,
    /// `ClassInfo`: `fallbackTypes` on a schema that is not concrete.
    FallbackTypesNotConcrete,
    /// `_GenerateClassInfo`: a multiple-apply schema with a
    /// `propertyNamespacePrefix` and no properties.
    PrefixWithoutProperties,
    /// `_GenerateClassInfo`: a multiple-apply schema with properties and no
    /// `propertyNamespacePrefix`.
    PropertiesWithoutPrefix,
    /// `AttrInfo`: an attribute type that is not an `Sdf.ValueTypeNames`
    /// member.
    UnknownType {
        /// The attribute's type name.
        type_name: String,
    },
    /// `_GenerateClassInfo`: two attributes, or two relationships, of a
    /// class whose names are the same identifier irrespective of
    /// namespacing (`a:b` and `aB`).
    DuplicateName {
        /// The identifier.
        name: String,
        /// Whether they are relationships.
        relationship: bool,
    },
    /// `_GenerateClassInfo`: two attributes, or two relationships, of a
    /// class with the same `apiName`.
    DuplicateApiName {
        /// The `apiName`.
        api_name: String,
        /// Whether they are relationships.
        relationship: bool,
    },
    /// `_GetAPISchemaOverridePropertyNames`: `apiSchemaOverride = true` on
    /// a property an inherited class defines as no override.
    OverrideOfDefinedProperty {
        /// The composed property.
        property: String,
    },
    /// `ParseUsd`: an API schema inherits an API schema of the file that
    /// is not applied the same way (applied or not; single- or
    /// multiple-apply).
    IncompatibleBaseApi {
        /// The base's C++ class name.
        base: String,
    },
    /// `GatherTokens`: two token values that make the same C++ identifier,
    /// in a library that generates code.
    TokenCollision {
        /// The identifier.
        identifier: String,
        /// The first value.
        first: String,
        /// The second value.
        second: String,
    },
    /// `GenerateCode`: `reflectedAPISchemas` names a schema no layer of the
    /// stage defines as a class.
    UnknownReflectedSchema {
        /// The reflected schema.
        schema: String,
    },
    /// `GenerateCode`: `reflectedAPISchemas` names a schema the class does
    /// not apply.
    ReflectedSchemaNotApplied {
        /// The reflected schema.
        schema: String,
    },
    /// `_RenamePropertiesWithInstanceablePrefix`: a property whose
    /// prefixed name is already a property of the schema.
    PrefixedPropertyExists {
        /// The prefixed name.
        name: String,
    },
    /// `GenerateRegistry`: prim metadata an API schema of this type
    /// cannot provide a fallback for.
    InvalidApiMetadata {
        /// The fields.
        fields: Vec<String>,
        /// The schema's `apiSchemaType`.
        api_schema_type: String,
        /// The fields it can provide.
        allowed: Vec<&'static str>,
    },
}

/// The URL usdGenSchema's messages end with.
const SEE: &str =
    "See https://openusd.org/release/api/_usd__page__generating_schemas.html for more information.";

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingGlobalPrim { layer } => write!(
                f,
                "Code generation requires a \"/GLOBAL\" prim with customData to define at least \
                 libraryName. GLOBAL prim not found (in {layer})."
            ),
            Self::MissingLibraryData { layer } => write!(
                f,
                "customData is either empty or not defined on /GLOBAL prim (in {layer}). At \
                 least \"libraryName\" entries in customData are required for code generation."
            ),
            Self::MissingLibraryName { layer } => write!(
                f,
                "Code generation requires that \"libraryName\" be defined in customData on \
                 /GLOBAL prim (in {layer})."
            ),
            Self::MissingLibraryPath { layer } => write!(
                f,
                "Code generation requires that \"libraryPath\" be defined in customData on \
                 /GLOBAL prim (in {layer}) or the schema must be declared codeless, by \
                 specifying skipCodeGeneration=true. The format for libraryPath is \
                 \"path/to/lib\"."
            ),
            Self::UndefinedBase { base } => {
                write!(f, "Could not find the defining layer for schema: {base}")
            }
            Self::TypeNameMismatch { type_name, name } => write!(
                f,
                "Code generation requires that every instantiable class's name must match its \
                 declared type ('{type_name}' and '{name}' do not match.)"
            ),
            Self::MultipleInheritance { inherits } => write!(
                f,
                "Schemas can only inherit from one other schema at most. This schema inherits \
                 from {} ({}).",
                inherits.len(),
                inherits.join(", ")
            ),
            Self::ApiSchemasNotPrepended => f.write_str(
                "The 'apiSchemas' metadata list operation is only allowed to prepend API schemas.",
            ),
            Self::InvalidInherit { inherit } => {
                write!(f, "The inherited prim <{inherit}> is not valid")
            }
            Self::InheritsOwnFamily => f.write_str(
                "Invalid inheritance: A schema cannot directly or indirectly inherit from another \
                 schema in the same schema family.",
            ),
            Self::MultipleApplyOnlyField { field } => write!(
                f,
                "{field} should only be used as a customData field on multiple-apply API schemas."
            ),
            Self::InvalidApiSchemaType { value } => write!(
                f,
                "CustomData 'apiSchemaType' is {value}. It must be one of ['nonApplied', \
                 'singleApply', 'multipleApply'] for an API schema."
            ),
            Self::AutoApplyNotSingleApply => f.write_str(
                "apiSchemaAutoApplyTo should only be used as a customData field on single-apply \
                 API schemas.",
            ),
            Self::CanOnlyApplyNotApplied => f.write_str(
                "apiSchemaCanOnlyApplyTo should only be used as a customData field on applied API \
                 schemas.",
            ),
            Self::ConcreteNotTyped => f.write_str(
                "Schema classes must either inherit Typed(IsA), or neither inherit typed nor \
                 provide a typename(API).",
            ),
            Self::NotAllowedIdentifier => {
                f.write_str("Schema name is not an allowed schema identifier.")
            }
            Self::MissingApiSuffix => f.write_str("API schemas must be named with an API suffix."),
            Self::TypedApiSuffix => {
                f.write_str("Typed schemas cannot be named with an API suffix.")
            }
            Self::AppliedApiBase => f.write_str(
                "Applied API schemas must explicitly inherit directly from APISchemaBase.",
            ),
            Self::NonAppliedApiBase => f.write_str(
                "Non-applied API schemas must inherit directly from APISchemaBase or another \
                 non-applied API schema.",
            ),
            Self::ApiSchemaTypeNotApi => {
                f.write_str("Non API schemas cannot have non-empty apiSchemaType value.")
            }
            Self::FallbackTypesNotConcrete => f.write_str(
                "fallbackPrimTypes can only be used as a customData field on concrete typed \
                 schema classes",
            ),
            Self::PrefixWithoutProperties => f.write_str(
                "Multiple-apply schemas that have the propertyNamespacePrefix metadata fields \
                 must have at least one property",
            ),
            Self::PropertiesWithoutPrefix => f.write_str(
                "Multiple-apply schemas that do not have a propertyNamespacePrefix metadata field \
                 must have zero properties",
            ),
            Self::UnknownType { type_name } => write!(
                f,
                "Code generation requires that all attributes have a known type (type \
                 '{type_name}' is not a member of Sdf.ValueTypeNames.)"
            ),
            Self::DuplicateName { name, relationship } => write!(
                f,
                "Schema {} names must be unique, irrespective of namespacing. Duplicate name \
                 encountered: {name}.",
                if *relationship {
                    "Relationship"
                } else {
                    "Attribute"
                }
            ),
            Self::DuplicateApiName {
                api_name,
                relationship,
            } => write!(
                f,
                "Schema {} API names must be unique. Duplicate apiName encountered: {api_name}.",
                if *relationship {
                    "Relationship"
                } else {
                    "Attribute"
                }
            ),
            Self::OverrideOfDefinedProperty { property } => write!(
                f,
                "Invalid schema property definition encountered while processing property \
                 {property}.\nProperty declarations in schema classes cannot set \
                 'apiSchemaOverride=true' on properties which exist in their inherited classes \
                 but are not already defined as API schemas overrides."
            ),
            Self::IncompatibleBaseApi { base } => write!(
                f,
                "API schema inherits from incompatible base API schema '{base}'. Both must be \
                 either applied API schemas or non-applied API schemas, and both single-apply or \
                 multiple-apply."
            ),
            Self::TokenCollision {
                identifier,
                first,
                second,
            } => write!(
                f,
                "Token identifiers must map to exactly one token value. One-to-Many mapping \
                 encountered: {identifier} maps to \"{first}\" and \"{second}\""
            ),
            Self::UnknownReflectedSchema { schema } => write!(
                f,
                "reflectedAPISchemas names {schema}, which no layer of the schema defines."
            ),
            Self::ReflectedSchemaNotApplied { schema } => write!(
                f,
                "Schemas listed in reflectedAPISchemas must be applied schemas. {schema} is not \
                 present in the apiSchemas metadata field."
            ),
            Self::PrefixedPropertyExists { name } => write!(
                f,
                "Prefixed property name '{name}' already exists as property base name in the \
                 schema."
            ),
            Self::InvalidApiMetadata {
                fields,
                api_schema_type,
                allowed,
            } => write!(
                f,
                "Found invalid metadata fields {fields:?} in API schema definition. API schemas \
                 of type {api_schema_type} can only provide prim metadata fallbacks for \
                 {allowed:?}"
            ),
        }
    }
}

impl fmt::Display for InvalidField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { path, field } = self;
        match field.as_str() {
            "targetPaths" => write!(
                f,
                "Relationship targets on <{path}> cannot be specified in a schema."
            ),
            "connectionPaths" => write!(
                f,
                "Attribute connections on <{path}> cannot be specified in a schema."
            ),
            _ => write!(
                f,
                "Fallback values for '{field}' on <{path}> cannot be specified in a schema."
            ),
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { layer, message } => write!(f, "{layer}: {message}"),
            Self::UnresolvedSublayer { layer, asset } => write!(
                f,
                "could not resolve sublayer @{asset}@ of {layer} in its directory or any search \
                 path"
            ),
            Self::Composition { errors } => {
                write!(
                    f,
                    "the schema stage composed with errors: {}",
                    errors.join("; ")
                )
            }
            Self::Schema { path, rule } => {
                write!(f, "Invalid schema definition at <{path}>\n{rule}\n{SEE}")
            }
            Self::InvalidFields { fields, .. } => {
                for field in fields {
                    writeln!(f, "{field}")?;
                }
                f.write_str("Invalid fields specified in schema.")
            }
            Self::PlugInfo { message } => write!(f, "plugInfo.json: {message}"),
            Self::Write { message } => write!(f, "writing generatedSchema.usda: {message}"),
            Self::Rust { message } => write!(f, "generating the Rust module: {message}"),
        }
    }
}

impl core::error::Error for CompileError {}
