// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The schema classes of a schema stage and the rules they must follow:
//! usdGenSchema's `ParseUsd`, `_GenerateClassInfo`, `ClassInfo`, `PropInfo`
//! and `_ValidateFields`.

use std::collections::BTreeSet;
use std::sync::Arc;

use layerstack::{
    FieldValue, LayerId, LayerStore, ListOp, Path, PathId, PrimSpec, PropertyKind, PropertySpec,
    Specifier, TokenId, Value,
};

use super::names::{
    INSTANCE_NAME_PLACEHOLDER, camel_case, family_and_version, is_allowed_schema_identifier,
    make_valid_token, multiple_apply_name_instance, multiple_apply_name_template,
    multiple_apply_schema_name_template, proper_case,
};
use super::source::SchemaStage;
use super::{CompileError, InvalidField, Rule, SchemaKind, SchemaLibrary};

/// The `apiSchemaType` values (`API_SCHEMA_TYPE_TOKENS`).
pub(crate) const NON_APPLIED: &str = "nonApplied";
pub(crate) const SINGLE_APPLY: &str = "singleApply";
pub(crate) const MULTIPLE_APPLY: &str = "multipleApply";

/// A dictionary value: its entries.
pub(crate) type Dictionary = [(Arc<str>, FieldValue)];

/// What usdGenSchema knows of one schema class (`ClassInfo`).
#[derive(Debug)]
pub(crate) struct ClassInfo {
    /// The class prim's path.
    pub(crate) path: PathId,
    /// The schema's identifier, the prim's name (`usdPrimTypeName`).
    pub(crate) name: String,
    /// The C++ class name: the library prefix and `className`.
    pub(crate) cpp_class_name: String,
    /// The parent's C++ class name (`bases` in `plugInfo.json`).
    pub(crate) parent_cpp_class_name: String,
    /// The parent schema's identifier, unless the class is a base.
    pub(crate) parent_name: Option<String>,
    pub(crate) kind: SchemaKind,
    pub(crate) family: String,
    pub(crate) version: u32,
    /// The class's own customData.
    pub(crate) custom_data: Vec<(Arc<str>, FieldValue)>,
    pub(crate) is_applied: bool,
    pub(crate) is_multiple_apply: bool,
    pub(crate) property_namespace: Option<String>,
    pub(crate) auto_apply_to: Vec<String>,
    pub(crate) can_only_apply_to: Vec<String>,
    pub(crate) allowed_instance_names: Vec<String>,
    pub(crate) instance_can_only_apply_to: Vec<(String, Vec<String>)>,
    pub(crate) fallback_prim_types: Vec<String>,
    /// Every applied schema the class composes, templated for a
    /// multiple-apply schema (`allAppliedAPISchemas`).
    pub(crate) all_applied_api_schemas: Vec<String>,
    /// Its API schema override properties, inherited ones included,
    /// templated for a multiple-apply schema.
    pub(crate) api_schema_override_property_names: Vec<String>,
    /// Whether its library uses literal identifiers for tokens.
    pub(crate) use_literal_identifier: bool,
    /// Its attributes, in authored order.
    pub(crate) attributes: Vec<PropertyInfo>,
    /// Its relationships, in authored order.
    pub(crate) relationships: Vec<PropertyInfo>,
}

/// What usdGenSchema knows of a property (`PropInfo`, `AttrInfo`).
#[derive(Debug)]
pub(crate) struct PropertyInfo {
    /// The identifier its tokens and accessors are named for.
    pub(crate) name: String,
    /// Its name in the generated schema.
    pub(crate) generated_name: String,
    /// Its `apiName`, empty for an API schema override.
    pub(crate) api_name: String,
    /// The `apiName` its `customData` authors, if it does: what the
    /// generated Rust views name its accessors for.
    pub(crate) authored_api_name: Option<String>,
    /// Attribute or relationship.
    pub(crate) kind: PropertyKind,
    /// An attribute's value type, as its spec declares it.
    pub(crate) type_name: Option<layerstack::PropertyType>,
    /// Its variability.
    pub(crate) variability: layerstack::Variability,
    /// A `token` attribute's fallback.
    pub(crate) token_fallback: Option<String>,
    /// Its `allowedTokens`.
    pub(crate) allowed_tokens: Vec<String>,
}

/// What `ParseUsd` returns.
#[derive(Debug)]
pub(crate) struct Parsed {
    pub(crate) library: SchemaLibrary,
    /// `libraryTokens`: each token and its value.
    pub(crate) library_tokens: Vec<(String, String)>,
    /// The classes of the schema file, in its order.
    pub(crate) classes: Vec<ClassInfo>,
    /// Classes of other layers that a class reflects
    /// (`reflectedAPISchemas`), by name.
    pub(crate) reflected: Vec<ClassInfo>,
    /// The classes of the other layers of the stack, the libraries the
    /// schema file builds on, each with its library's name. usdGenSchema
    /// checks none of their rules; a layer or class that breaks one is
    /// left out.
    pub(crate) upstream: Vec<(String, ClassInfo)>,
}

/// Reads the classes of the schema file, checking usdGenSchema's rules
/// (`ParseUsd`).
///
/// Spec: AOUSD Core §13 leaves how schemas are defined to the
/// implementation; this is OpenUSD's `usdGenSchema.py`.
pub(crate) fn parse(stage: &SchemaStage) -> Result<Parsed, CompileError> {
    let parser = Parser::new(stage);
    let root = stage.root;
    let metadata = parser.library_metadata(root)?;
    let library_name = parser.library_name(root)?;
    let skip_code_generation = stage
        .layer_stack()
        .iter()
        .any(|&layer| parser.skips_code_generation(layer));
    let library = SchemaLibrary {
        path: parser.library_path(root)?,
        prefix: parser.library_prefix(root)?,
        tokens_prefix: dict_str(&metadata, "tokensPrefix", parser.tokens())
            .map_or_else(|| parser.library_prefix(root), Ok)?,
        use_literal_identifier: dict_get(&metadata, "useLiteralIdentifier")
            .is_none_or(|v| is_truthy(v, parser.tokens())),
        skip_code_generation,
        name: library_name,
    };
    let library_tokens = match dict_get(&metadata, "libraryTokens") {
        Some(Value::Dictionary(tokens)) => tokens
            .iter()
            .map(|(token, info)| {
                let value = match info {
                    FieldValue::Value(Value::Dictionary(info)) => {
                        dict_str(info, "value", parser.tokens())
                    }
                    _ => None,
                };
                (
                    token.to_string(),
                    value.unwrap_or_else(|| token.to_string()),
                )
            })
            .collect(),
        _ => Vec::new(),
    };
    let mut classes = Vec::new();
    for path in parser.root_prims(root) {
        if let Some(class) = parser.class(root, path, library.use_literal_identifier, true)? {
            classes.push(class);
        }
    }
    let reflected = parser.reflected(&classes, library.use_literal_identifier)?;
    check_api_bases(&classes)?;
    let mut upstream = Vec::new();
    for layer in stage
        .layer_stack()
        .into_iter()
        .filter(|&layer| layer != root)
    {
        let Ok(name) = parser.library_name(layer) else {
            continue;
        };
        for path in parser.root_prims(layer) {
            if let Ok(Some(class)) = parser.class(layer, path, true, true) {
                upstream.push((name.clone(), class));
            }
        }
    }
    Ok(Parsed {
        library,
        library_tokens,
        classes,
        reflected,
        upstream,
    })
}

/// An applied API schema inheriting an API schema of the same file must
/// apply the same way (`ParseUsd`). The rules `ClassInfo` checks already
/// leave only non-applied API schemas with API schema bases; this is kept
/// as usdGenSchema keeps it.
fn check_api_bases(classes: &[ClassInfo]) -> Result<(), CompileError> {
    for class in classes {
        if !class.kind.is_api() || class.parent_cpp_class_name == "UsdAPISchemaBase" {
            continue;
        }
        if let Some(parent) = classes
            .iter()
            .find(|parent| parent.cpp_class_name == class.parent_cpp_class_name)
            && (parent.is_applied != class.is_applied
                || parent.is_multiple_apply != class.is_multiple_apply)
        {
            return Err(class.error(Rule::IncompatibleBaseApi {
                base: parent.cpp_class_name.clone(),
            }));
        }
    }
    Ok(())
}

impl ClassInfo {
    fn error(&self, rule: Rule) -> CompileError {
        CompileError::Schema {
            path: format!("/{}", self.name),
            rule,
        }
    }
}

/// Reads classes from the stage's layers.
struct Parser<'s> {
    stage: &'s SchemaStage,
    store: &'s dyn LayerStore,
    custom_data: TokenId,
    api_schemas: TokenId,
    allowed_tokens: TokenId,
    global: PathId,
}

impl<'s> Parser<'s> {
    fn new(stage: &'s SchemaStage) -> Self {
        let store = stage.store();
        let tokens = &store.tokens();
        let global = store
            .paths()
            .intern(Path::root().join(&[tokens.intern("GLOBAL")]));
        Self {
            stage,
            store,
            custom_data: tokens.intern("customData"),
            api_schemas: tokens.intern("apiSchemas"),
            allowed_tokens: tokens.intern("allowedTokens"),
            global,
        }
    }

    fn tokens(&self) -> &layerstack::TokenInterner {
        self.store.tokens()
    }

    fn text(&self, token: TokenId) -> &str {
        self.tokens().resolve(token)
    }

    fn display(&self, path: PathId) -> String {
        self.store.paths().display(path, self.tokens())
    }

    fn spec(&self, layer: LayerId, path: PathId) -> Option<&'s PrimSpec> {
        self.store.layer(layer)?.prim(path)
    }

    /// The root prims of `layer`, in its order.
    fn root_prims(&self, layer: LayerId) -> Vec<PathId> {
        let root = self.store.paths().intern(Path::root());
        self.spec(layer, root)
            .map(|pseudo_root| {
                pseudo_root
                    .authored_children
                    .iter()
                    .map(|&name| self.store.paths().intern(Path::root().join(&[name])))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The spec's own `customData`.
    fn custom_data(&self, spec: &PrimSpec) -> Vec<(Arc<str>, FieldValue)> {
        match spec.field(self.custom_data) {
            Some(FieldValue::Value(Value::Dictionary(entries))) => entries.clone(),
            _ => Vec::new(),
        }
    }

    /// `_GetLibMetadata`: the `customData` of the layer's `/GLOBAL`.
    fn library_metadata(
        &self,
        layer: LayerId,
    ) -> Result<Vec<(Arc<str>, FieldValue)>, CompileError> {
        let shown = self.stage.identifier(layer);
        let global = self.spec(layer, self.global).ok_or_else(|| {
            library_error(Rule::MissingGlobalPrim {
                layer: shown.clone(),
            })
        })?;
        let data = self.custom_data(global);
        if data.is_empty() {
            return Err(library_error(Rule::MissingLibraryData { layer: shown }));
        }
        Ok(data)
    }

    /// `_GetLibName`.
    fn library_name(&self, layer: LayerId) -> Result<String, CompileError> {
        dict_str(&self.library_metadata(layer)?, "libraryName", self.tokens()).ok_or_else(|| {
            library_error(Rule::MissingLibraryName {
                layer: self.stage.identifier(layer),
            })
        })
    }

    /// `_GetLibPrefix`: `libraryPrefix`, else the library's name in proper
    /// case.
    fn library_prefix(&self, layer: LayerId) -> Result<String, CompileError> {
        let fallback = proper_case(&self.library_name(layer)?);
        Ok(dict_str(
            &self.library_metadata(layer)?,
            "libraryPrefix",
            self.tokens(),
        )
        .unwrap_or(fallback))
    }

    /// `_GetLibPath`: empty for a codeless library.
    fn library_path(&self, layer: LayerId) -> Result<String, CompileError> {
        if self.skips_code_generation(layer) {
            return Ok(String::new());
        }
        dict_str(&self.library_metadata(layer)?, "libraryPath", self.tokens()).ok_or_else(|| {
            library_error(Rule::MissingLibraryPath {
                layer: self.stage.identifier(layer),
            })
        })
    }

    /// `_SkipCodeGenForLayer`.
    fn skips_code_generation(&self, layer: LayerId) -> bool {
        self.library_metadata(layer).is_ok_and(|data| {
            dict_get(&data, "skipCodeGeneration").is_some_and(|v| is_truthy(v, self.tokens()))
        })
    }

    /// The strongest `inherits` the layer stack authors for `path`, which
    /// is what `UsdPrim::GetMetadata('inheritPaths')` returns for a class:
    /// no inherited opinion can be stronger than a local one.
    fn inherits(&self, path: PathId) -> Option<&'s ListOp<PathId>> {
        self.stage.layer_stack().into_iter().find_map(|layer| {
            let inherits = self.spec(layer, path)?.inherits();
            is_authored(inherits).then_some(inherits)
        })
    }

    /// `_FindAllInherits`: what `path` inherits, directly or not.
    fn all_inherits(&self, path: PathId) -> Vec<PathId> {
        let mut all = Vec::new();
        let mut seen = BTreeSet::new();
        self.collect_inherits(path, &mut all, &mut seen);
        all
    }

    fn collect_inherits(&self, path: PathId, all: &mut Vec<PathId>, seen: &mut BTreeSet<PathId>) {
        if !seen.insert(path) {
            return;
        }
        let Some(inherits) = self.inherits(path) else {
            return;
        };
        let direct = inherits.apply_to(&[]);
        all.extend(direct.iter().copied());
        for base in direct {
            if self.stage.view().prim(base).is_some() {
                self.collect_inherits(base, all, seen);
            }
        }
    }

    /// `_GetDefiningLayerAndPrim`: the strongest layer with a root prim
    /// named `name`. `SchemaBase` is the weakest layer, without a prim.
    fn defining_layer(&self, name: &str) -> Result<(LayerId, Option<PathId>), CompileError> {
        let stack = self.stage.layer_stack();
        if name == "SchemaBase" {
            return Ok((*stack.last().expect("the root layer"), None));
        }
        for layer in stack {
            if let Some(path) = self
                .root_prims(layer)
                .into_iter()
                .find(|&path| self.display(path).strip_prefix('/') == Some(name))
            {
                return Ok((layer, Some(path)));
            }
        }
        Err(CompileError::Schema {
            path: format!("/{name}"),
            rule: Rule::UndefinedBase { base: name.into() },
        })
    }

    /// `_ExtractNames`: the schema's identifier and C++ class name.
    fn names(
        &self,
        layer: LayerId,
        path: PathId,
        custom_data: &Dictionary,
    ) -> Result<(String, String), CompileError> {
        let name = self.name(path);
        let class_name =
            dict_str(custom_data, "className", self.tokens()).unwrap_or_else(|| proper_case(&name));
        Ok((name, self.library_prefix(layer)? + &class_name))
    }

    fn name(&self, path: PathId) -> String {
        self.store
            .paths()
            .resolve(path)
            .leaf()
            .map_or_else(String::new, |name| self.text(name).to_owned())
    }

    /// Builds the class `path` of `layer`, if it is a class prim
    /// (`_GenerateClassInfo` and `ClassInfo`); `with_properties` reads
    /// its properties too, as the first pass over the schema file does.
    fn class(
        &self,
        layer: LayerId,
        path: PathId,
        literal: bool,
        with_properties: bool,
    ) -> Result<Option<ClassInfo>, CompileError> {
        let Some(spec) = self.spec(layer, path) else {
            return Ok(None);
        };
        if spec.specifier != Some(Specifier::Class) || self.stage.view().prim(path).is_none() {
            return Ok(None);
        }
        let shown = self.display(path);
        let schema_error = |rule| CompileError::Schema {
            path: shown.clone(),
            rule,
        };
        let mut invalid = Vec::new();
        self.invalid_prim_fields(spec, &shown, &mut invalid);

        let name = self.name(path);
        let type_name = spec
            .type_name
            .map_or_else(String::new, |t| self.text(t).to_owned());
        if !type_name.is_empty() && type_name != name {
            return Err(schema_error(Rule::TypeNameMismatch { type_name, name }));
        }
        let inherits = self.inherits(path);
        let inherits_list = inherits.map(|op| op.apply_to(&[])).unwrap_or_default();
        if inherits_list.len() > 1 {
            return Err(schema_error(Rule::MultipleInheritance {
                inherits: inherits_list.iter().map(|&p| self.display(p)).collect(),
            }));
        }
        let custom_data = self.custom_data(spec);
        let (name, cpp_class_name) = self.names(layer, path, &custom_data)?;

        let parent_class = inherits_list
            .first()
            .map_or_else(|| "SchemaBase".to_owned(), |&p| self.name(p));
        let (parent_layer, parent_prim) = self.defining_layer(&parent_class)?;
        self.library_path(parent_layer)?;
        let mut parent_custom_data = Vec::new();
        let mut parent_name = None;
        let mut parent_cpp_class_name = String::new();
        if let Some(parent) = parent_prim {
            let parent_spec = self.spec(parent_layer, parent).expect("a root prim");
            parent_custom_data = self.custom_data(parent_spec);
            let (name, cpp) = self.names(parent_layer, parent, &parent_custom_data)?;
            parent_name = Some(name);
            parent_cpp_class_name = cpp;
        } else if cpp_class_name == "UsdTyped" || cpp_class_name == "UsdAPISchemaBase" {
            parent_cpp_class_name = "UsdSchemaBase".into();
        }

        if let Some(FieldValue::TokenListOp(op)) = spec.field(self.api_schemas)
            && (op.explicit.is_some()
                || !op.add.is_empty()
                || !op.delete.is_empty()
                || !op.reorder.is_empty())
        {
            return Err(schema_error(Rule::ApiSchemasNotPrepended));
        }
        let mut all_applied_api_schemas: Vec<String> = match self
            .stage
            .view()
            .prim(path)
            .and_then(|prim| prim.metadata(self.api_schemas))
            .map(|resolved| resolved.value)
        {
            Some(layerstack::ResolvedValue::TokenList(applied)) => {
                applied.iter().map(|&t| self.text(t).to_owned()).collect()
            }
            _ => Vec::new(),
        };

        // A class does not inherit its parent's type name.
        let mut type_name = type_name;
        if let Some(inherits) = inherits {
            for &inherit in inherits.explicit.as_ref().unwrap_or(&inherits.add) {
                let parent = self.spec(parent_layer, inherit).ok_or_else(|| {
                    schema_error(Rule::InvalidInherit {
                        inherit: self.display(inherit),
                    })
                })?;
                let parent_type = parent.type_name.map_or("", |t| self.text(t));
                if parent_type == type_name {
                    type_name.clear();
                }
            }
        }

        let is_concrete = !type_name.is_empty();
        let all_inherits = self.all_inherits(path);
        let is_typed = all_inherits.iter().any(|&p| self.display(p) == "/Typed");
        let is_typed_base = cpp_class_name == "UsdTyped";
        let is_api_schema_base = cpp_class_name == "UsdAPISchemaBase";
        let tokens = self.tokens();
        let fallback_prim_types = dict_strings(&custom_data, "fallbackTypes", tokens);
        let is_api = !is_typed && !is_concrete && !is_api_schema_base && !is_typed_base;
        let api_schema_type = dict_str(&custom_data, "apiSchemaType", tokens)
            .or_else(|| is_api.then(|| SINGLE_APPLY.to_owned()));
        let property_namespace = dict_get(&custom_data, "propertyNamespacePrefix")
            .filter(|value| is_truthy(value, tokens))
            .and_then(|value| value_str(value, tokens));
        let truthy = |key| dict_get(&custom_data, key).is_some_and(|v| is_truthy(v, tokens));
        let has_auto_apply = truthy("apiSchemaAutoApplyTo");
        let has_can_only_apply = truthy("apiSchemaCanOnlyApplyTo");

        let (family, version) = family_and_version(&name);
        let own_family = family.to_owned();
        if all_inherits
            .iter()
            .any(|&p| family_and_version(&self.name(p)).0 == own_family)
        {
            return Err(schema_error(Rule::InheritsOwnFamily));
        }
        let api_type = api_schema_type.as_deref();
        if api_type != Some(MULTIPLE_APPLY) {
            for field in [
                "propertyNamespacePrefix",
                "apiSchemaAllowedInstanceNames",
                "apiSchemaInstances",
            ] {
                let present = if field == "propertyNamespacePrefix" {
                    property_namespace.is_some()
                } else {
                    truthy(field)
                };
                if present {
                    return Err(schema_error(Rule::MultipleApplyOnlyField { field }));
                }
            }
        }
        if is_api && !matches!(api_type, Some(NON_APPLIED | SINGLE_APPLY | MULTIPLE_APPLY)) {
            return Err(schema_error(Rule::InvalidApiSchemaType {
                value: api_type.unwrap_or("None").to_owned(),
            }));
        }
        if has_auto_apply && api_type != Some(SINGLE_APPLY) {
            return Err(schema_error(Rule::AutoApplyNotSingleApply));
        }
        let is_applied = matches!(api_type, Some(SINGLE_APPLY | MULTIPLE_APPLY));
        let is_multiple_apply = api_type == Some(MULTIPLE_APPLY);
        if has_can_only_apply && !is_applied {
            return Err(schema_error(Rule::CanOnlyApplyNotApplied));
        }
        let kind = if is_api {
            if !is_applied {
                SchemaKind::NonAppliedApi
            } else if is_multiple_apply {
                SchemaKind::MultipleApplyApi
            } else {
                SchemaKind::SingleApplyApi
            }
        } else if is_typed && !is_typed_base {
            if is_concrete {
                SchemaKind::ConcreteTyped
            } else {
                SchemaKind::AbstractTyped
            }
        } else {
            SchemaKind::AbstractBase
        };
        if is_concrete && !is_typed {
            return Err(schema_error(Rule::ConcreteNotTyped));
        }
        if !is_allowed_schema_identifier(&name) {
            return Err(schema_error(Rule::NotAllowedIdentifier));
        }
        if is_api && name != "APISchemaBase" && !family.ends_with("API") {
            return Err(schema_error(Rule::MissingApiSuffix));
        }
        if is_typed && family.ends_with("API") {
            return Err(schema_error(Rule::TypedApiSuffix));
        }
        if is_api && name != "APISchemaBase" && parent_cpp_class_name != "UsdAPISchemaBase" {
            if is_applied {
                return Err(schema_error(Rule::AppliedApiBase));
            }
            if dict_str(&parent_custom_data, "apiSchemaType", tokens).as_deref()
                != Some(NON_APPLIED)
            {
                return Err(schema_error(Rule::NonAppliedApiBase));
            }
        }
        if !is_api && is_applied {
            return Err(schema_error(Rule::ApiSchemaTypeNotApi));
        }
        if !fallback_prim_types.is_empty() && !is_concrete {
            return Err(schema_error(Rule::FallbackTypesNotConcrete));
        }

        if is_multiple_apply {
            if property_namespace.is_some() && spec.properties.is_empty() {
                return Err(schema_error(Rule::PrefixWithoutProperties));
            }
            if property_namespace.is_none() && !spec.properties.is_empty() {
                return Err(schema_error(Rule::PropertiesWithoutPrefix));
            }
            all_applied_api_schemas = all_applied_api_schemas
                .iter()
                .map(|s| multiple_apply_schema_name_template(s))
                .collect();
        }

        let mut class = ClassInfo {
            path,
            cpp_class_name,
            parent_cpp_class_name,
            parent_name,
            kind,
            family: own_family,
            version,
            is_applied,
            is_multiple_apply,
            auto_apply_to: dict_strings(&custom_data, "apiSchemaAutoApplyTo", tokens),
            can_only_apply_to: dict_strings(&custom_data, "apiSchemaCanOnlyApplyTo", tokens),
            allowed_instance_names: dict_strings(
                &custom_data,
                "apiSchemaAllowedInstanceNames",
                tokens,
            ),
            instance_can_only_apply_to: instance_can_only_apply_to(&custom_data, tokens),
            fallback_prim_types,
            all_applied_api_schemas,
            api_schema_override_property_names: Vec::new(),
            use_literal_identifier: literal,
            attributes: Vec::new(),
            relationships: Vec::new(),
            property_namespace,
            custom_data,
            name,
        };
        if with_properties {
            self.properties(&mut class, spec, &shown, &mut invalid)?;
            class.api_schema_override_property_names = self.override_names(&class)?;
        }
        if !invalid.is_empty() {
            return Err(CompileError::InvalidFields {
                class: shown,
                fields: invalid,
            });
        }
        Ok(Some(class))
    }

    /// Reads the class's own properties (the root layer spec's, not the
    /// composed ones), checking their names and types.
    fn properties(
        &self,
        class: &mut ClassInfo,
        spec: &PrimSpec,
        shown: &str,
        invalid: &mut Vec<InvalidField>,
    ) -> Result<(), CompileError> {
        let mut attribute_api_names = Vec::new();
        let mut relationship_api_names = Vec::new();
        for entry in &spec.properties {
            let raw = self.text(entry.name);
            let property_path = format!("{shown}.{raw}");
            self.invalid_property_fields(&entry.spec, &property_path, invalid);
            let property = self.property(class, raw, &entry.spec, &property_path)?;
            let relationship = entry.spec.kind == PropertyKind::Relationship;
            let (existing, api_names) = if relationship {
                (&class.relationships, &mut relationship_api_names)
            } else {
                (&class.attributes, &mut attribute_api_names)
            };
            if !property.api_name.is_empty() {
                if existing.iter().any(|p| p.name == property.name) {
                    return Err(class.error(Rule::DuplicateName {
                        name: format!("{}.{}", class.name, property.name),
                        relationship,
                    }));
                }
                if api_names.contains(&property.api_name) {
                    return Err(class.error(Rule::DuplicateApiName {
                        api_name: format!("{}.{}", class.name, property.api_name),
                        relationship,
                    }));
                }
                api_names.push(property.api_name.clone());
            }
            let list = if relationship {
                &mut class.relationships
            } else {
                &mut class.attributes
            };
            list.retain(|p| p.name != property.name);
            list.push(property);
        }
        Ok(())
    }

    /// `PropInfo` and `AttrInfo`.
    fn property(
        &self,
        class: &ClassInfo,
        raw: &str,
        spec: &PropertySpec,
        property_path: &str,
    ) -> Result<PropertyInfo, CompileError> {
        let tokens = self.tokens();
        let base = if raw == INSTANCE_NAME_PLACEHOLDER {
            ""
        } else {
            raw
        };
        let (name, generated_name) = property_names(base, class.property_namespace.as_deref());
        let custom_data = match spec.metadata(self.custom_data) {
            Some(FieldValue::Value(Value::Dictionary(entries))) => entries.clone(),
            _ => Vec::new(),
        };
        let is_override =
            dict_get(&custom_data, "apiSchemaOverride").is_some_and(|v| is_truthy(v, tokens));
        let authored_api_name = dict_str(&custom_data, "apiName", tokens);
        let api_name = if is_override {
            String::new()
        } else {
            authored_api_name
                .clone()
                .unwrap_or_else(|| camel_case(base))
        };
        let mut info = PropertyInfo {
            name,
            generated_name,
            api_name,
            authored_api_name,
            kind: spec.kind,
            type_name: spec.type_name.clone(),
            variability: spec.variability,
            token_fallback: None,
            allowed_tokens: Vec::new(),
        };
        if spec.kind == PropertyKind::Attribute {
            let type_name = spec
                .type_name
                .as_ref()
                .map_or_else(String::new, |t| t.type_name().to_string());
            if !spec
                .type_name
                .as_ref()
                .is_some_and(|t| t.value_type().is_some())
            {
                return Err(CompileError::Schema {
                    path: property_path.to_owned(),
                    rule: Rule::UnknownType { type_name },
                });
            }
            if let Some(FieldValue::Value(value)) = spec.metadata(self.allowed_tokens) {
                info.allowed_tokens = value_strings(value, tokens).unwrap_or_default();
            }
            if type_name == "token"
                && let Some(value) = spec.default.as_ref()
                && is_truthy(value, tokens)
            {
                info.token_fallback = value_str(value, tokens);
            }
        }
        Ok(info)
    }

    /// `_GetAPISchemaOverridePropertyNames`: the composed properties whose
    /// strongest spec is an API schema override, which every weaker spec
    /// must also be.
    fn override_names(&self, class: &ClassInfo) -> Result<Vec<String>, CompileError> {
        let mut names = Vec::new();
        let properties = self
            .stage
            .view()
            .prim(class.path)
            .map(layerstack::Prim::authored_properties)
            .unwrap_or_default();
        for handle in properties {
            let (path, property) = (handle.path(), handle.path().property());
            let mut specs = handle
                .opinions()
                .iter()
                .filter_map(|op| op.value().as_property());
            let is_override = |spec: &PropertySpec| match spec.metadata(self.custom_data) {
                Some(FieldValue::Value(Value::Dictionary(entries))) => {
                    dict_get(entries, "apiSchemaOverride")
                        .is_some_and(|v| is_truthy(v, self.tokens()))
                }
                _ => false,
            };
            if !specs.next().is_some_and(is_override) {
                continue;
            }
            if specs.any(|spec| !is_override(spec)) {
                return Err(CompileError::Schema {
                    path: path.display(self.store.paths(), self.tokens()),
                    rule: Rule::OverrideOfDefinedProperty {
                        property: path.display(self.store.paths(), self.tokens()),
                    },
                });
            }
            let name = self.text(property);
            names.push(match &class.property_namespace {
                Some(prefix) => multiple_apply_name_template(prefix, name),
                None => name.to_owned(),
            });
        }
        Ok(names)
    }

    /// The classes of other layers the schema file's classes reflect
    /// (`reflectedAPISchemas`), found by name on the stage as `ParseUsd`
    /// finds them.
    fn reflected(
        &self,
        classes: &[ClassInfo],
        literal: bool,
    ) -> Result<Vec<ClassInfo>, CompileError> {
        let wanted: BTreeSet<String> = classes
            .iter()
            .flat_map(|class| {
                dict_strings(&class.custom_data, "reflectedAPISchemas", self.tokens())
            })
            .filter(|name| classes.iter().all(|class| &class.name != name))
            .collect();
        let mut found = Vec::new();
        for name in wanted {
            let Ok((layer, Some(path))) = self.defining_layer(&name) else {
                continue;
            };
            if let Some(class) = self.class(layer, path, literal, true)? {
                found.push(class);
            }
        }
        Ok(found)
    }

    /// `_ValidateFields` for a prim spec: the fields it holds outside
    /// `fields` count as authored when set.
    fn invalid_prim_fields(&self, spec: &PrimSpec, shown: &str, invalid: &mut Vec<InvalidField>) {
        let mut found: Vec<String> = spec
            .fields
            .iter()
            .map(|field| self.text(field.name).to_owned())
            .filter(|key| is_disallowed_field(key))
            .collect();
        for (key, authored) in [
            ("references", is_authored(&spec.references)),
            ("payload", is_authored(&spec.payloads)),
            ("specializes", is_authored(spec.specializes())),
            ("variantSelection", spec.variant_selections().is_some()),
            ("variantSetNames", !spec.variant_set_names().is_empty()),
            ("active", spec.active.is_some()),
            ("instanceable", spec.instanceable.is_some()),
        ] {
            if authored {
                found.push(key.into());
            }
        }
        found.sort();
        invalid.extend(found.into_iter().map(|field| InvalidField {
            path: shown.to_owned(),
            field,
        }));
    }

    /// `_ValidateFields` for a property spec.
    fn invalid_property_fields(
        &self,
        spec: &PropertySpec,
        shown: &str,
        invalid: &mut Vec<InvalidField>,
    ) {
        let mut found: Vec<String> = spec
            .metadata
            .iter()
            .map(|field| self.text(field.name).to_owned())
            .filter(|key| is_disallowed_field(key))
            .collect();
        let targets = match spec.kind {
            PropertyKind::Attribute => "connectionPaths",
            PropertyKind::Relationship => "targetPaths",
        };
        for (key, authored) in [
            ("timeSamples", spec.time_samples.is_authored()),
            ("spline", spec.spline.is_some()),
            (targets, spec.targets.as_deref().is_some_and(is_authored)),
        ] {
            if authored {
                found.push(key.into());
            }
        }
        found.sort();
        invalid.extend(found.into_iter().map(|field| InvalidField {
            path: shown.to_owned(),
            field,
        }));
    }
}

fn library_error(rule: Rule) -> CompileError {
    CompileError::Schema {
        path: "/GLOBAL".into(),
        rule,
    }
}

/// `_GetNameAndGeneratedSchemaPropNameForPropInfo`: a property's
/// identifier and its name in the generated schema.
pub(crate) fn property_names(base: &str, namespace: Option<&str>) -> (String, String) {
    match namespace {
        Some(prefix) => {
            let generated = multiple_apply_name_template(prefix, base);
            let name = camel_case(&multiple_apply_name_instance(
                &generated,
                "_MultipleApplyTemplate_",
            ));
            (name, generated)
        }
        None => (make_valid_token(base, false), base.to_owned()),
    }
}

/// `UsdSchemaRegistry::IsDisallowedField`, less the fields `_ValidateFields`
/// allows (`inheritPaths`, `customData` and `specifier`), for the fields a
/// spec keeps in its field list. Composition arcs, variants, `active`,
/// `instanceable`, samples, splines and targets are checked where the
/// spec keeps them.
fn is_disallowed_field(key: &str) -> bool {
    matches!(
        key,
        "payload"
            | "references"
            | "specializes"
            | "variantSelection"
            | "variantSetNames"
            | "active"
            | "instanceable"
            | "timeSamples"
            | "connectionPaths"
            | "targetPaths"
            | "spline"
            | "kind"
            | "clips"
            | "clipSets"
            | "primChildren"
            | "properties"
            | "variantChildren"
            | "variantSetChildren"
            | "connectionChildren"
            | "targetChildren"
            | "mapperChildren"
            | "mapperArgChildren"
            | "expressionChildren"
    )
}

/// Whether a list op authors anything.
fn is_authored<T>(op: &ListOp<T>) -> bool {
    op.explicit.is_some()
        || !op.add.is_empty()
        || !op.prepend.is_empty()
        || !op.append.is_empty()
        || !op.delete.is_empty()
        || !op.reorder.is_empty()
}

/// The `apiSchemaCanOnlyApplyTo` of each `apiSchemaInstances` entry.
fn instance_can_only_apply_to(
    custom_data: &Dictionary,
    tokens: &layerstack::TokenInterner,
) -> Vec<(String, Vec<String>)> {
    let Some(Value::Dictionary(instances)) = dict_get(custom_data, "apiSchemaInstances") else {
        return Vec::new();
    };
    instances
        .iter()
        .filter_map(|(instance, info)| match info {
            FieldValue::Value(Value::Dictionary(info)) => Some((
                instance.to_string(),
                dict_strings(info, "apiSchemaCanOnlyApplyTo", tokens),
            )),
            _ => None,
        })
        .collect()
}

/// The plain value of `key` in a dictionary.
pub(crate) fn dict_get<'a>(entries: &'a Dictionary, key: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find(|(name, _)| &**name == key)
        .and_then(|(_, value)| value.as_value())
}

/// A string or token value.
pub(crate) fn value_str(value: &Value, tokens: &layerstack::TokenInterner) -> Option<String> {
    match value {
        Value::String(text) | Value::Asset(text) => Some(text.to_string()),
        Value::Token(token) => Some(tokens.resolve(*token).to_owned()),
        _ => None,
    }
}

/// The strings or tokens of an array value.
pub(crate) fn value_strings(
    value: &Value,
    tokens: &layerstack::TokenInterner,
) -> Option<Vec<String>> {
    value
        .array_ref()?
        .iter()
        .map(|item| value_str(&item, tokens))
        .collect()
}

/// The string or token value of `key`.
pub(crate) fn dict_str(
    entries: &Dictionary,
    key: &str,
    tokens: &layerstack::TokenInterner,
) -> Option<String> {
    dict_get(entries, key).and_then(|value| value_str(value, tokens))
}

/// The strings or tokens of `key`'s array.
pub(crate) fn dict_strings(
    entries: &Dictionary,
    key: &str,
    tokens: &layerstack::TokenInterner,
) -> Vec<String> {
    dict_get(entries, key)
        .and_then(|value| value_strings(value, tokens))
        .unwrap_or_default()
}

/// Python's truth value of a value: false for `false`, zero, empty text,
/// arrays and dictionaries.
pub(crate) fn is_truthy(value: &Value, tokens: &layerstack::TokenInterner) -> bool {
    match value {
        Value::Token(token) => !tokens.resolve(*token).is_empty(),
        Value::Bool(b) => *b,
        Value::Int(v) => *v != 0,
        Value::UInt(v) => *v != 0,
        Value::Int64(v) => *v != 0,
        Value::UInt64(v) => *v != 0,
        Value::UChar(v) => *v != 0,
        Value::Float(v) => *v != 0.0,
        Value::Double(v) => *v != 0.0,
        Value::String(text) | Value::Asset(text) => !text.is_empty(),
        Value::Array(_) | Value::TypedArray(_) => value.array_ref().is_some_and(|a| !a.is_empty()),
        Value::Dictionary(entries) => !entries.is_empty(),
        Value::Null | Value::Blocked => false,
        _ => true,
    }
}
