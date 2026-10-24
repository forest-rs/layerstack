// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A compiled library's Rust module: typed views, edit handles and token
//! enums of its schemas, in the shape `layerstack_schemas` gives OpenUSD's,
//! and the static tables registering them
//! ([`CompiledSchema::rust_module`](super::CompiledSchema::rust_module)).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{InMemoryStore, LayerId, PropertyKind, PropertyType, Variability};

use super::classes::{ClassInfo, Parsed};
use super::{CompileError, SchemaClass};
use crate::emit::{Statics, ValueTypes, module, table_imports, tables};
use crate::model::{self, DOMAINS, Property, Schema};
use crate::views::{self, Context, Index, Target, TestTable};

/// What the Rust module needs beyond the generated files: the `apiName`s
/// the schema file authors, which `generatedSchema.usda` does not keep,
/// and the schemas of the libraries it builds on.
#[derive(Clone, Debug)]
pub(crate) struct Inputs {
    /// Each authored `apiName`, by `(class, generated property name)`.
    api_names: BTreeMap<(String, String), String>,
    /// The classes of the other libraries of the schema stage.
    upstream: Vec<Upstream>,
}

/// A class of a library the schema file builds on.
#[derive(Clone, Debug)]
struct Upstream {
    /// Its library's name (`usdGeom`).
    library: String,
    name: String,
    kind: layerstack::SchemaKind,
    parent: Option<String>,
    built_ins: Vec<String>,
    /// Its own properties, as its library's schema file declares them.
    properties: Vec<UpstreamProperty>,
}

#[derive(Clone, Debug)]
struct UpstreamProperty {
    name: String,
    kind: PropertyKind,
    type_name: Option<PropertyType>,
    variability: Variability,
    api_name: Option<String>,
}

impl Inputs {
    /// The inputs of `parsed`'s classes.
    pub(crate) fn new(parsed: &Parsed) -> Self {
        let mut api_names = BTreeMap::new();
        for class in &parsed.classes {
            for property in class.attributes.iter().chain(&class.relationships) {
                if let Some(api_name) = &property.authored_api_name {
                    api_names.insert(
                        (class.name.clone(), property.generated_name.clone()),
                        api_name.clone(),
                    );
                }
            }
        }
        let upstream = parsed
            .upstream
            .iter()
            .filter_map(|(library, class)| upstream(library, class))
            .collect();
        Self {
            api_names,
            upstream,
        }
    }
}

fn upstream(library: &str, class: &ClassInfo) -> Option<Upstream> {
    Some(Upstream {
        library: library.to_owned(),
        name: class.name.clone(),
        // OpenUSD's registry has `Typed`, the root of typed schemas, as
        // abstract typed, though it is an abstract base.
        kind: if class.name == "Typed" {
            layerstack::SchemaKind::AbstractTyped
        } else {
            class.kind.registry_kind()?
        },
        parent: class.parent_name.clone(),
        built_ins: class.all_applied_api_schemas.clone(),
        properties: class
            .attributes
            .iter()
            .chain(&class.relationships)
            .map(|property| UpstreamProperty {
                name: property.generated_name.clone(),
                kind: property.kind,
                type_name: property.type_name.clone(),
                variability: property.variability,
                api_name: property.authored_api_name.clone(),
            })
            .collect(),
    })
}

/// The type names a library module imports or the prelude has, which no
/// generated type may be.
const IMPORTED: &[&str] = &[
    "ApplyError",
    "Arc",
    "Box",
    "CannotApply",
    "CannotEdit",
    "Deref",
    "Err",
    "InstanceEdit",
    "InstanceView",
    "Metadata",
    "None",
    "Ok",
    "Option",
    "PathId",
    "PrimEdit",
    "PrimView",
    "Property",
    "PropertyKind",
    "Result",
    "Scene",
    "Schema",
    "SchemaEdit",
    "SchemaKind",
    "SchemaTables",
    "Some",
    "String",
    "Value",
    "ValueType",
    "Variability",
    "Vec",
];

/// The OpenUSD domain `layerstack_schemas` generates for `library`, as
/// `model::DOMAINS` lists it: `(plugin, Rust name)`.
fn domain(library: &str) -> Option<(&'static str, &'static str)> {
    DOMAINS
        .iter()
        .find(|(plugin, _, _)| plugin.eq_ignore_ascii_case(library))
        .map(|&(plugin, variant, _)| (plugin, variant))
}

fn error(message: impl Into<String>) -> CompileError {
    CompileError::Rust {
        message: message.into(),
    }
}

/// The library's Rust module. See
/// [`CompiledSchema::rust_module`](super::CompiledSchema::rust_module).
pub(crate) fn module_text(
    library: &str,
    classes: &[SchemaClass],
    generated_schema: &str,
    plug_info: Option<&str>,
    inputs: &Inputs,
) -> Result<String, CompileError> {
    // The library's schemas, read from its generated schema layer as
    // OpenUSD's are read from theirs.
    let store = InMemoryStore::default();
    let parsed = layerstack_usda::parser::parse(generated_schema);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &store.tokens,
        &store.paths,
        &mut model::NoAssets,
    );
    if !parsed.diagnostics.is_empty() || !emitted.diagnostics.is_empty() {
        return Err(error(format!(
            "generatedSchema.usda does not read back: {:?} {:?}",
            parsed.diagnostics, emitted.diagnostics
        )));
    }
    let tokens = &store.tokens;
    let names = |names: &[String]| names.iter().map(|n| tokens.intern(n)).collect::<Vec<_>>();
    let mut declared = Vec::new();
    let mut left_out = Vec::new();
    for class in classes {
        let Some(kind) = class.kind.registry_kind() else {
            left_out.push((
                class.name.clone(),
                if class.kind.is_api() {
                    "a non-applied API schema"
                } else {
                    "an abstract base"
                },
            ));
            continue;
        };
        declared.push(SchemaDeclaration {
            parent: class
                .base
                .as_deref()
                .filter(|_| kind.is_typed())
                .map(|parent| tokens.intern(parent)),
            can_only_apply_to: names(&class.can_only_apply_to),
            allowed_instance_names: names(&class.allowed_instance_names),
            instance_can_only_apply_to: class
                .instance_can_only_apply_to
                .iter()
                .map(|(instance, types)| (tokens.intern(instance), names(types)))
                .collect(),
            ..SchemaDeclaration::new(tokens.intern(&class.name), kind)
        });
    }
    let definitions = read_generated_schema(&emitted.layer, &declared, &store.tokens, &store.paths)
        .map_err(|e| error(format!("generatedSchema.usda: {e}")))?;
    let mut schemas = Vec::new();
    for definition in definitions {
        schemas.push(
            model::schema_of(
                &emitted.layer,
                definition,
                &inputs.api_names,
                None,
                &store.tokens,
                &store.paths,
            )
            .map_err(error)?,
        );
    }
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    left_out.sort();

    // The libraries' schemas, with every property they inherit.
    let upstream: BTreeMap<&str, &Upstream> = inputs
        .upstream
        .iter()
        .map(|class| (class.name.as_str(), class))
        .collect();
    let upstream_schemas: Vec<(Schema, &Upstream)> = inputs
        .upstream
        .iter()
        .map(|class| (upstream_schema(class, &upstream), class))
        .collect();
    let mut index: Index<'_> = BTreeMap::new();
    for (schema, class) in &upstream_schemas {
        if let Some((plugin, _)) = domain(&class.library) {
            index.insert(&schema.name, (schema, module(plugin)));
        }
    }
    for schema in &schemas {
        index.insert(&schema.name, (schema, String::new()));
    }

    // Every schema of another library the library names must be one
    // `layerstack_schemas` has; those domains are its dependencies.
    let mut dependencies: Vec<&'static str> = Vec::new();
    for schema in &schemas {
        let named = schema
            .parent
            .iter()
            .chain(&schema.built_ins)
            .chain(&schema.can_only_apply_to)
            .chain(
                schema
                    .instance_can_only_apply_to
                    .iter()
                    .flat_map(|(_, t)| t),
            )
            .map(|name| name.split(':').next().unwrap_or(name));
        for name in named {
            if schemas.iter().any(|s| s.name == name) {
                continue;
            }
            let class = upstream.get(name).ok_or_else(|| {
                error(format!(
                    "{}: names `{name}`, which no layer of the schema stage defines",
                    schema.name
                ))
            })?;
            let (plugin, _) = domain(&class.library).ok_or_else(|| {
                error(format!(
                    "{}: builds on `{name}` of the `{}` library, which `layerstack_schemas` \
                     has no views of; only OpenUSD's domains can be built on",
                    schema.name, class.library
                ))
            })?;
            if !dependencies.contains(&plugin) {
                dependencies.push(plugin);
            }
        }
    }
    let plug_info_text = plug_info;
    let plug_info = plug_info
        .map(model::plug_info_text)
        .transpose()
        .map_err(|e| error(format!("plugInfo.json: {e}")))?;
    let mut auto_applies = Vec::new();
    for plugin in plug_info
        .as_ref()
        .and_then(|json| json["Plugins"].as_array())
        .into_iter()
        .flatten()
    {
        auto_applies.extend(model::auto_applies(&plugin["Info"]));
    }
    // The plugins' `SdfMetadata`, as OpenUSD's `SdfSchema` registers them
    // after its own and usd-core's fields; the module registers those it
    // accepts, so any it refuses is an error.
    let declared = match plug_info_text {
        Some(text) => crate::metadata::read([("plugInfo.json", text)], &store.tokens)
            .map_err(|e| error(format!("plugInfo.json: {}", e.message)))?,
        None => crate::metadata::PluginMetadata::default(),
    };
    if !declared.rejected.is_empty() {
        return Err(CompileError::Metadata {
            rejected: declared.rejected,
        });
    }
    let metadata: Vec<layerstack::MetadataDefinition> = declared
        .accepted
        .into_iter()
        .map(|declaration| declaration.definition)
        .collect();
    // The plugins' `Kinds`, as OpenUSD's `KindRegistry` reads them; the
    // module registers those it accepts, so any it refuses is an error.
    let kinds = match plug_info_text {
        Some(text) => crate::kinds::read([("plugInfo.json", text)])
            .map_err(|e| error(format!("plugInfo.json: {}", e.message)))?,
        None => crate::kinds::PluginKinds::default(),
    };
    if !kinds.rejected.is_empty() {
        return Err(CompileError::Kinds {
            rejected: kinds.rejected,
        });
    }
    for (_, target) in &auto_applies {
        let target = target.split(':').next().unwrap_or(target);
        if let Some((plugin, _)) = upstream.get(target).and_then(|c| domain(&c.library))
            && !dependencies.contains(&plugin)
        {
            dependencies.push(plugin);
        }
    }
    dependencies.sort_by_key(|plugin| DOMAINS.iter().position(|(p, _, _)| p == plugin));

    let cx = Context {
        tokens: &store.tokens,
        index: &index,
        module: String::new(),
        target: Target::Library,
        owner: format!("the `{library}` library's"),
    };
    let ordered: Vec<&Schema> = schemas.iter().collect();
    let mut items = views::module_items(&cx, &ordered, &mut TestTable::default())
        .map_err(|e| error(format!("{e} (a property's `apiName` names its accessors)")))?;
    // The typed views and edit handles of the library's metadata fields.
    items
        .body
        .push_str(&crate::metadata::library_items(&metadata, &store.tokens).map_err(error)?);
    check_type_names(&items.body)?;

    let mut types = ValueTypes::default();
    let mut failures = Vec::new();
    let table = tables(
        "static",
        &ordered,
        &auto_applies,
        &metadata,
        &store.tokens,
        Statics {
            types: &mut types,
            plugin: "",
            prefix: "",
        },
        &mut failures,
    );
    if !failures.is_empty() {
        return Err(error(format!(
            "values layerstack_schemas cannot express: {}",
            failures.join("; ")
        )));
    }
    let statics = types.statics("static");
    let (layerstack_items, table_items) = table_imports(&format!("{table}{statics}"));

    let features: Vec<String> = dependencies
        .iter()
        .map(|plugin| format!("`{}`", views::feature(plugin)))
        .collect();
    let variants: Vec<String> = dependencies
        .iter()
        .filter_map(|plugin| domain(plugin))
        .map(|(_, variant)| format!("::layerstack_schemas::Domain::{variant}"))
        .collect();
    let mut out = format!(
        "// Generated by `layerstack_schemagen` from the `{library}` schema library: typed\n\
         // views, edit handles and token enums of its schemas, and the tables that\n\
         // register them. Do not edit: regenerate with `layerstack_schemagen compile\n\
         // <schema.usda> --rust <file>`, or `CompiledSchema::rust_module` in a build script.\n\
         //\n\
         // It needs the crates `layerstack` and `layerstack_schemas`{features}, and is\n\
         // a module of its own: `pub mod name {{ include!(...); }}`.\n",
        features = if features.is_empty() {
            String::new()
        } else {
            format!(" (features {})", features.join(", "))
        }
    );
    if !left_out.is_empty() {
        out.push_str("//\n// Leaves out, having no prim definition:\n");
        for (name, why) in &left_out {
            let _ = writeln!(out, "// - `{name}`, {why}.");
        }
    }
    out.push_str("\nextern crate alloc;\n\n");
    let body = &items.body;
    if body.contains("Arc<") || body.contains("Arc::") || table.contains("Arc::from(") {
        out.push_str("use alloc::sync::Arc;\n");
    }
    // `register_kinds` returns a `Vec`.
    out.push_str("use alloc::vec::Vec;\n");
    let mut layerstack: Vec<&str> = items.layerstack.clone();
    layerstack.extend(layerstack_items);
    layerstack.sort_unstable();
    let _ = writeln!(
        out,
        "use core::ops::Deref;\n\nuse layerstack::{{{}}};\n\n\
         use ::layerstack_schemas::table::{{{}}};\nuse ::layerstack_schemas::{{{}}};",
        layerstack.join(", "),
        table_items.join(", "),
        items.schemas.join(", ")
    );
    out.push_str(body);
    let _ = write!(
        out,
        "\n/// The schema library's name, as its `/GLOBAL` prim declares it.\n\
         pub const LIBRARY: &str = {library:?};\n\n\
         /// The OpenUSD domains the library's schemas build on, which a\n\
         /// registry of them also registers ([`registry`]).\n\
         pub const DEPENDENCIES: &[::layerstack_schemas::Domain] = &[{variants}];\n\n\
         /// Registers the library's schemas, auto-applies and metadata\n\
         /// declarations with `builder`, interning their names in `tokens`, the\n\
         /// interner of the store whose stages use the registry. The OpenUSD\n\
         /// schemas they build on are registered separately\n\
         /// (`layerstack_schemas::register` with [`DEPENDENCIES`]).\n\
         ///\n\
         /// # Errors\n\
         ///\n\
         /// A metadata declaration that conflicts with one already registered.\n\
         pub fn register(\n    builder: &mut ::layerstack::SchemaRegistryBuilder,\n    \
         tokens: &::layerstack::TokenInterner,\n) -> Result<(), ::layerstack::MetadataConflict> {{\n    \
         TABLES.register(builder, tokens)\n}}\n\n\
         /// A registry of the library's schemas and of the OpenUSD schemas they\n\
         /// build on ([`DEPENDENCIES`]).\n\
         ///\n\
         /// # Errors\n\
         ///\n\
         /// A metadata declaration of the library that conflicts with one of\n\
         /// OpenUSD's.\n\
         pub fn registry(\n    tokens: &::layerstack::TokenInterner,\n\
         ) -> Result<::layerstack::SchemaRegistry, ::layerstack::MetadataConflict> {{\n    \
         let mut builder = ::layerstack::SchemaRegistry::builder();\n    \
         ::layerstack_schemas::register(&mut builder, DEPENDENCIES, tokens)?;\n    \
         register(&mut builder, tokens)?;\n    Ok(builder.build(tokens))\n}}\n\n\
         {kinds_table}\n\
         /// Registers [`KINDS`] with `kinds`, as OpenUSD registers a plugin's\n\
         /// `Kinds`: each that is not registered already. Returns those that are,\n\
         /// which keep their first registration; `KindRegistry::issues` reports\n\
         /// bases that are no kind and cycles.\n\
         #[must_use = \"the kinds that were not registered\"]\n\
         pub fn register_kinds(\n    kinds: &mut ::layerstack_schemas::kind::KindRegistry,\n\
         ) -> Vec<::layerstack_schemas::kind::KindError> {{\n    \
         kinds.register_all(KINDS.iter().copied())\n}}\n{statics}\n\
         #[allow(\n    clippy::unreadable_literal,\n    clippy::approx_constant,\n    \
         clippy::excessive_precision,\n    reason = \"values as the schema writes them\"\n)]\n\
         {table}",
        variants = variants.join(", "),
        kinds_table = crate::kinds::table(
            &kinds.accepted,
            "/// The kinds the library's `plugInfo.json` declares (`Kinds`), with their\n\
             /// base kinds: each plugin's in name order, as OpenUSD reads them.",
        ),
    );
    Ok(out)
}

/// The schema `class` with the properties it inherits from the classes of
/// `upstream`, as a generated schema holds them.
fn upstream_schema(class: &Upstream, upstream: &BTreeMap<&str, &Upstream>) -> Schema {
    let mut properties: Vec<Property> = Vec::new();
    let mut seen = BTreeSet::new();
    let mut current = Some(class);
    while let Some(at) = current {
        for property in &at.properties {
            if seen.insert(property.name.clone()) {
                properties.push(Property {
                    name: property.name.clone(),
                    kind: property.kind,
                    value_type: property.type_name.as_ref().map(model::value_type_of),
                    variability: property.variability,
                    fallback: None,
                    metadata: Vec::new(),
                    allowed_tokens: Vec::new(),
                    doc: String::new(),
                    api_name: property.api_name.clone(),
                });
            }
        }
        current = at
            .parent
            .as_deref()
            .and_then(|parent| upstream.get(parent))
            .copied();
    }
    Schema {
        cpp: None,
        name: class.name.clone(),
        kind: class.kind,
        parent: class.parent.clone(),
        built_ins: class.built_ins.clone(),
        properties,
        overrides: Vec::new(),
        can_only_apply_to: Vec::new(),
        allowed_instance_names: Vec::new(),
        instance_can_only_apply_to: Vec::new(),
        doc: String::new(),
    }
}

/// Fails when two generated types share a name, or one is a name the
/// module imports.
fn check_type_names(items: &str) -> Result<(), CompileError> {
    let mut seen = BTreeSet::new();
    for line in items.lines() {
        let Some(rest) = line
            .strip_prefix("pub struct ")
            .or_else(|| line.strip_prefix("pub enum "))
        else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if IMPORTED.contains(&name.as_str()) {
            return Err(error(format!(
                "the Rust type `{name}` is a name the module imports; rename the schema"
            )));
        }
        if !seen.insert(name.clone()) {
            return Err(error(format!(
                "two generated Rust types are named `{name}`: a view is named for its \
                 schema, its edit handle `<view>Edit` and a token enum `<view><Accessor>`; \
                 rename a schema or give the property an `apiName`"
            )));
        }
    }
    Ok(())
}
