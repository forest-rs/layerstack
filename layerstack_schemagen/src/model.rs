// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The generator's model of OpenUSD's schemas, read from a usd-core wheel,
//! with each property's `apiName` read from the matching OpenUSD source.
//!
//! The model keeps everything the generated code needs, the registry tables
//! and the typed views built from the same schemas: each schema's kind,
//! parent, built-ins, auto-applies, documentation and properties, and each
//! property's declared type, variability, fallback, runtime metadata, `allowedTokens`,
//! documentation and `apiName`. Only `apiName` comes from the source
//! (`pxr/usd/*/schema.usda`), since usdGenSchema consumes it; everything
//! else comes from the wheel.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{
    AssetResolveError, AssetResolver, FieldValue, InMemoryStore, LayerId, PathInterner,
    PropertyKind, ResolvedAsset, SchemaKind, TokenInterner, Value, Variability,
};
use serde_json::Value as Json;

/// Where a domain's `generatedSchema.usda` and `plugInfo.json` are read
/// from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    /// The usd-core wheel: `pxr/pluginfo/<plugin>/resources`.
    Wheel,
    /// The OpenUSD source checkout, `pxr/usd/<plugin>`: a plugin the wheel
    /// is built without (`usdMtlx` needs `MaterialX`). Its release is the
    /// wheel's, which the version check ensures.
    Source,
}

/// A domain: one OpenUSD schema plugin.
#[derive(Debug)]
pub(crate) struct Domain {
    /// The plugin's directory under `pxr/pluginfo` (`usdGeom`).
    pub(crate) plugin: &'static str,
    /// Plugin-defined fields independent of prim schemas.
    pub(crate) metadata: Vec<layerstack::MetadataDefinition>,
    /// Where its definitions were read from.
    pub(crate) origin: Origin,
    /// The plugin's name, as its `plugInfo.json` gives it (`usdGeom`,
    /// `UsdProfiles`).
    pub(crate) name: String,
    /// The Rust name of the domain (`UsdGeom`).
    pub(crate) variant: &'static str,
    /// The schemas it registers, in `generatedSchema.usda` order.
    pub(crate) schemas: Vec<Schema>,
    /// The schemas it defines that no prim definition has, with why.
    pub(crate) left_out: Vec<(String, &'static str)>,
    /// Auto-applies it declares: `(applied schema, target)`.
    pub(crate) auto_applies: Vec<(String, String)>,
    /// The other domains whose schemas its schemas name.
    pub(crate) dependencies: Vec<&'static str>,
}

/// A schema, as its plugin defines it.
#[derive(Debug)]
pub(crate) struct Schema {
    /// Its identifier (`Mesh`, `CollectionAPI`).
    pub(crate) name: String,
    /// Its kind.
    pub(crate) kind: SchemaKind,
    /// The typed schema it inherits from.
    pub(crate) parent: Option<String>,
    /// Its built-ins, in the by-type and by-named-instance form.
    pub(crate) built_ins: Vec<String>,
    /// Its properties.
    pub(crate) properties: Vec<Property>,
    /// Its override properties.
    pub(crate) overrides: Vec<Property>,
    /// `apiSchemaCanOnlyApplyTo`.
    pub(crate) can_only_apply_to: Vec<String>,
    /// `apiSchemaAllowedInstanceNames`.
    pub(crate) allowed_instance_names: Vec<String>,
    /// `apiSchemaInstances`' `apiSchemaCanOnlyApplyTo`, by instance name.
    pub(crate) instance_can_only_apply_to: Vec<(String, Vec<String>)>,
    /// Its `userDocBrief`.
    pub(crate) doc: String,
}

/// A property a schema defines or overrides.
#[derive(Debug)]
pub(crate) struct Property {
    /// Its name, with `__INSTANCE_NAME__` in a multiple-apply schema.
    pub(crate) name: String,
    /// Attribute or relationship.
    pub(crate) kind: PropertyKind,
    /// The declared value type: its name, whether it is an array, and the
    /// value of one element of that type.
    pub(crate) value_type: Option<(String, bool, Value)>,
    /// The declared variability.
    pub(crate) variability: Variability,
    /// The fallback value.
    pub(crate) fallback: Option<Value>,
    /// Runtime property metadata, excluding generator-only and arc fields.
    pub(crate) metadata: Vec<layerstack::FieldEntry>,
    /// The tokens `allowedTokens` lists.
    pub(crate) allowed_tokens: Vec<String>,
    /// Its `userDocBrief`.
    pub(crate) doc: String,
    /// Its `apiName` in the OpenUSD source, if it has one.
    pub(crate) api_name: Option<String>,
}

/// What the generator read, and from where.
#[derive(Debug)]
pub(crate) struct Model {
    /// Composed shader-node definitions from the matching wheel.
    pub(crate) nodes: Vec<crate::shader_nodes::Node>,
    /// The usd-core release (`26.8`).
    pub(crate) version: String,
    /// The files read, relative to `site-packages`.
    pub(crate) files: Vec<String>,
    /// The domains, in generation order.
    pub(crate) domains: Vec<Domain>,
    /// The interner the fallback values' tokens are in.
    pub(crate) tokens: TokenInterner,
}

/// The domains generated: `(plugin directory, Rust name, where its
/// definitions are)`. Adding a domain is one line here.
pub(crate) const DOMAINS: &[(&str, &str, Origin)] = &[
    ("usd", "Usd", Origin::Wheel),
    ("usdGeom", "UsdGeom", Origin::Wheel),
    ("usdShade", "UsdShade", Origin::Wheel),
    ("usdLux", "UsdLux", Origin::Wheel),
    ("usdSkel", "UsdSkel", Origin::Wheel),
    ("usdPhysics", "UsdPhysics", Origin::Wheel),
    ("usdVol", "UsdVol", Origin::Wheel),
    ("usdRender", "UsdRender", Origin::Wheel),
    ("usdLod", "UsdLod", Origin::Wheel),
    ("usdUI", "UsdUI", Origin::Wheel),
    ("usdRi", "UsdRi", Origin::Wheel),
    ("usdHydra", "UsdHydra", Origin::Wheel),
    ("usdMedia", "UsdMedia", Origin::Wheel),
    ("usdProc", "UsdProc", Origin::Wheel),
    ("usdSemantics", "UsdSemantics", Origin::Wheel),
    ("usdProfiles", "UsdProfiles", Origin::Wheel),
    ("usdMtlx", "UsdMtlx", Origin::Source),
];

/// A type a plugin declares: its schema identifier, kind and bases.
struct TypeInfo {
    plugin: &'static str,
    identifier: Option<String>,
    kind: String,
    bases: Vec<String>,
    can_only_apply_to: Vec<String>,
    allowed_instance_names: Vec<String>,
    instance_can_only_apply_to: Vec<(String, Vec<String>)>,
}

/// Reads the model from `pxr`, the `pxr` package directory of a usd-core
/// wheel, and `source`, an OpenUSD checkout of the same release.
pub(crate) fn read(pxr: &Path, source: &Path) -> Result<Model, String> {
    let site_packages = pxr.parent().ok_or("the pxr directory has no parent")?;
    let version = wheel_version(site_packages)?;
    let source_version = source_version(source)?;
    if source_version != version {
        return Err(format!(
            "the OpenUSD source at {} is {source_version}, the wheel is {version}; \
             check out the matching tag",
            source.display()
        ));
    }
    let mut files = Vec::new();
    let mut store = InMemoryStore::default();
    let mut plugin_metadata = BTreeMap::new();
    let relative = |path: &Path| {
        path.strip_prefix(site_packages)
            .or_else(|_| path.strip_prefix(source))
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };

    // Every declared type of every generated domain, to resolve bases
    // across domains (`UsdLuxBoundableLightBase` derives from
    // `UsdGeomBoundable`).
    let mut types: BTreeMap<String, TypeInfo> = BTreeMap::new();
    let mut plugin_auto_applies: BTreeMap<&'static str, Vec<(String, String)>> = BTreeMap::new();
    let mut plugin_names: BTreeMap<&'static str, String> = BTreeMap::new();
    for &(plugin, _, origin) in DOMAINS {
        let path = definitions(pxr, source, plugin, origin).join("plugInfo.json");
        files.push(relative(&path));
        let info = plug_info(&path)?;
        for plugin_info in info["Plugins"].as_array().ok_or("no Plugins")? {
            if let Some(name) = plugin_info["Name"].as_str() {
                plugin_names.insert(plugin, name.into());
            }
            let info = &plugin_info["Info"];
            plugin_metadata
                .entry(plugin)
                .or_insert_with(Vec::new)
                .extend(crate::metadata::read(
                    &info["SdfMetadata"],
                    &mut store.tokens,
                )?);
            if let Some(types_json) = info["Types"].as_object() {
                for (name, entry) in types_json {
                    types.insert(
                        name.clone(),
                        TypeInfo {
                            plugin,
                            identifier: entry["schemaIdentifier"].as_str().map(String::from),
                            kind: entry["schemaKind"].as_str().unwrap_or_default().into(),
                            bases: strings(&entry["bases"]),
                            can_only_apply_to: strings(&entry["apiSchemaCanOnlyApplyTo"]),
                            allowed_instance_names: strings(
                                &entry["apiSchemaAllowedInstanceNames"],
                            ),
                            instance_can_only_apply_to: entry["apiSchemaInstances"]
                                .as_object()
                                .map(|instances| {
                                    instances
                                        .iter()
                                        .map(|(instance, info)| {
                                            (
                                                instance.clone(),
                                                strings(&info["apiSchemaCanOnlyApplyTo"]),
                                            )
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        },
                    );
                    if let Some(identifier) = entry["schemaIdentifier"].as_str() {
                        for target in strings(&entry["apiSchemaAutoApplyTo"]) {
                            plugin_auto_applies
                                .entry(plugin)
                                .or_default()
                                .push((identifier.into(), target));
                        }
                    }
                }
            }
            if let Some(auto) = info["AutoApplyAPISchemas"].as_object() {
                for (schema, entry) in auto {
                    for target in strings(&entry["apiSchemaAutoApplyTo"]) {
                        plugin_auto_applies
                            .entry(plugin)
                            .or_default()
                            .push((schema.clone(), target));
                    }
                }
            }
        }
    }

    let custom_data = store.tokens.intern("customData");
    let allowed_tokens = store.tokens.intern("allowedTokens");
    let mut domains = Vec::new();
    for (index, &(plugin, variant, origin)) in DOMAINS.iter().enumerate() {
        let path = definitions(pxr, source, plugin, origin).join("generatedSchema.usda");
        files.push(relative(&path));
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = layerstack_usda::parser::parse(&text);
        let emitted = layerstack_usda::emit::emit(
            &parsed.layer,
            LayerId(u64::try_from(index).expect("few domains") + 1),
            &mut store.tokens,
            &mut store.paths,
            &mut NoAssets,
        );
        let diagnostics: Vec<String> = parsed
            .diagnostics
            .iter()
            .chain(&emitted.diagnostics)
            .map(|d| d.message.clone())
            .collect();
        if !diagnostics.is_empty() {
            return Err(format!("{}: {diagnostics:?}", path.display()));
        }

        let mut declared = Vec::new();
        let mut left_out = Vec::new();
        for info in types.values().filter(|info| info.plugin == plugin) {
            let Some(identifier) = &info.identifier else {
                continue;
            };
            let kind = match (info.kind.as_str(), identifier.as_str()) {
                ("concreteTyped", _) => SchemaKind::ConcreteTyped,
                ("abstractTyped", _) | ("abstractBase", "Typed") => SchemaKind::AbstractTyped,
                ("singleApplyAPI", _) => SchemaKind::SingleApplyApi,
                ("multipleApplyAPI", _) => SchemaKind::MultipleApplyApi,
                ("nonAppliedAPI", _) => {
                    left_out.push((identifier.clone(), "a non-applied API schema"));
                    continue;
                }
                ("abstractBase", _) => {
                    left_out.push((identifier.clone(), "an abstract base of API schemas"));
                    continue;
                }
                (other, _) => return Err(format!("{identifier}: unknown schema kind {other}")),
            };
            let parent = if kind.is_typed() {
                parent_of(info, &types)?
            } else {
                None
            };
            let tokens = &mut store.tokens;
            let can_only_apply_to = intern_all(tokens, &info.can_only_apply_to);
            let allowed_instance_names = intern_all(tokens, &info.allowed_instance_names);
            let instance_can_only_apply_to = info
                .instance_can_only_apply_to
                .iter()
                .map(|(instance, types)| (tokens.intern(instance), intern_all(tokens, types)))
                .collect();
            declared.push(SchemaDeclaration {
                parent: parent.map(|parent| store.tokens.intern(parent)),
                can_only_apply_to,
                allowed_instance_names,
                instance_can_only_apply_to,
                ..SchemaDeclaration::new(store.tokens.intern(identifier), kind)
            });
        }
        let definitions =
            read_generated_schema(&emitted.layer, &declared, &mut store.tokens, &store.paths)
                .map_err(|e| format!("{}: {e}", path.display()))?;

        let api_names = source_api_names(source, plugin, &mut files)?;
        let mut used_api_names: Vec<(String, String)> = Vec::new();
        let mut schemas = Vec::new();
        for definition in definitions {
            let name = String::from(store.tokens.resolve(definition.name));
            let spec = store
                .paths
                .lookup(&layerstack::Path::root().join(&[definition.name]))
                .and_then(|path| emitted.layer.prims.get(&path))
                .ok_or_else(|| format!("{name}: no prim spec"))?;
            let class_doc = doc_of(spec.field(custom_data));
            let schema_name = name.clone();
            let property_spec = |property: layerstack::TokenId| {
                spec.properties
                    .iter()
                    .find(|entry| entry.name == property)
                    .map(|entry| &entry.spec)
            };
            let convert = |p: &layerstack::PropertyDefinition, tokens: &TokenInterner| Property {
                doc: property_spec(p.name)
                    .map(|s| doc_of(s.metadata(custom_data)))
                    .unwrap_or_default(),
                allowed_tokens: property_spec(p.name)
                    .and_then(|s| s.metadata(allowed_tokens))
                    .map(|allowed| match allowed {
                        FieldValue::Value(Value::Array(items)) => items
                            .iter()
                            .filter_map(|item| match item {
                                Value::Token(token) => Some(tokens.resolve(*token).to_string()),
                                Value::String(text) => Some(text.to_string()),
                                _ => None,
                            })
                            .collect(),
                        _ => Vec::new(),
                    })
                    .unwrap_or_default(),
                api_name: api_names
                    .get(&(schema_name.clone(), tokens.resolve(p.name).to_string()))
                    .cloned(),
                name: tokens.resolve(p.name).to_string(),
                kind: p.kind,
                value_type: p.type_name.as_ref().map(|t| {
                    (
                        t.type_name.to_string(),
                        t.is_array,
                        t.default_scalar.clone(),
                    )
                }),
                variability: p.variability,
                fallback: p.fallback.clone(),
                metadata: p.metadata.clone(),
            };
            let names = |ids: &[layerstack::TokenId], tokens: &TokenInterner| -> Vec<String> {
                ids.iter()
                    .map(|id| tokens.resolve(*id).to_string())
                    .collect()
            };
            for property in definition.properties.iter().chain(&definition.overrides) {
                used_api_names.push((
                    name.clone(),
                    store.tokens.resolve(property.name).to_string(),
                ));
            }
            schemas.push(Schema {
                doc: class_doc,
                can_only_apply_to: names(&definition.can_only_apply_to, &store.tokens),
                allowed_instance_names: names(&definition.allowed_instance_names, &store.tokens),
                instance_can_only_apply_to: definition
                    .instance_can_only_apply_to
                    .iter()
                    .map(|(instance, types)| {
                        (
                            store.tokens.resolve(*instance).to_string(),
                            names(types, &store.tokens),
                        )
                    })
                    .collect(),
                name,
                kind: definition.kind,
                parent: definition
                    .parent
                    .map(|parent| store.tokens.resolve(parent).to_string()),
                built_ins: definition
                    .built_ins
                    .iter()
                    .map(|built_in| store.tokens.resolve(*built_in).to_string())
                    .collect(),
                properties: definition
                    .properties
                    .iter()
                    .map(|p| convert(p, &store.tokens))
                    .collect(),
                overrides: definition
                    .overrides
                    .iter()
                    .map(|p| convert(p, &store.tokens))
                    .collect(),
            });
        }
        // Every `apiName` of a generated schema names one of its properties;
        // one that does not means the source and the wheel differ.
        for (schema, property) in api_names.keys() {
            let generated = schemas.iter().any(|s: &Schema| &s.name == schema);
            if generated && !used_api_names.contains(&(schema.clone(), property.clone())) {
                return Err(format!(
                    "{plugin}: the source gives {schema}.{property} an apiName, but the wheel \
                     defines no such property"
                ));
            }
        }
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        left_out.sort();
        domains.push(Domain {
            metadata: plugin_metadata.remove(plugin).unwrap_or_default(),
            plugin,
            origin,
            name: plugin_names
                .get(plugin)
                .cloned()
                .unwrap_or_else(|| plugin.into()),
            variant,
            schemas,
            left_out,
            auto_applies: plugin_auto_applies.remove(plugin).unwrap_or_default(),
            dependencies: Vec::new(),
        });
    }

    // The domains whose schemas each domain's schemas name.
    let mut owner: BTreeMap<String, &'static str> = BTreeMap::new();
    for domain in &domains {
        for schema in &domain.schemas {
            owner.insert(schema.name.clone(), domain.plugin);
        }
    }
    for domain in &mut domains {
        let mut named: Vec<&str> = Vec::new();
        for schema in &domain.schemas {
            named.extend(schema.parent.as_deref());
            named.extend(schema.built_ins.iter().map(|b| schema_part(b)));
            named.extend(schema.can_only_apply_to.iter().map(String::as_str));
            for (_, types) in &schema.instance_can_only_apply_to {
                named.extend(types.iter().map(String::as_str));
            }
        }
        for (schema, target) in &domain.auto_applies {
            named.push(schema_part(schema));
            named.push(schema_part(target));
        }
        let mut dependencies: Vec<&'static str> = Vec::new();
        for name in named {
            let Some(&plugin) = owner.get(name) else {
                return Err(format!(
                    "{}: names {name}, which no domain defines",
                    domain.plugin
                ));
            };
            if plugin != domain.plugin && !dependencies.contains(&plugin) {
                dependencies.push(plugin);
            }
        }
        // The render computation also reads Camera schema fallbacks and
        // traces UsdShade output providers. Those behavioral dependencies
        // are not expressed by RenderSettings' schema inheritance.
        if domain.plugin == "usdRender" {
            for plugin in ["usdGeom", "usdShade"] {
                if !dependencies.contains(&plugin) {
                    dependencies.push(plugin);
                }
            }
        }
        dependencies.sort_by_key(|plugin| DOMAINS.iter().position(|(p, _, _)| p == plugin));
        domain.dependencies = dependencies;
    }

    let nodes = crate::shader_nodes::read(pxr, &mut store, &mut files)?;
    Ok(Model {
        nodes,
        version,
        files,
        domains,
        tokens: store.tokens,
    })
}

fn intern_all(tokens: &mut TokenInterner, names: &[String]) -> Vec<layerstack::TokenId> {
    names.iter().map(|name| tokens.intern(name)).collect()
}

/// The first paragraph of the `userDocBrief` in `custom_data`, on one line.
fn doc_of(custom_data: Option<&FieldValue>) -> String {
    let Some(FieldValue::Value(Value::Dictionary(entries))) = custom_data else {
        return String::new();
    };
    let Some((_, Value::String(text))) = entries.iter().find(|(key, _)| &**key == "userDocBrief")
    else {
        return String::new();
    };
    let paragraph = text.split("\n\n").next().unwrap_or_default();
    paragraph.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The OpenUSD release of the source checkout at `source`
/// (`cmake/defaults/Version.cmake`), as the wheel names it (`26.8`).
fn source_version(source: &Path) -> Result<String, String> {
    let path = source.join("cmake/defaults/Version.cmake");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let part = |name: &str| {
        text.lines()
            .find_map(|line| {
                let rest = line.trim().strip_prefix(&format!("set({name} "))?;
                Some(rest.split('"').nth(1)?.to_string())
            })
            .ok_or_else(|| format!("{}: no {name}", path.display()))
    };
    let (minor, patch) = (part("PXR_MINOR_VERSION")?, part("PXR_PATCH_VERSION")?);
    Ok(format!("{minor}.{patch}"))
}

/// The `apiName` of each property of the domain's source `schema.usda`, by
/// `(schema, generated property name)`: a multiple-apply schema's property
/// `p` is generated as `prefix:__INSTANCE_NAME__:p` (`prefix:__INSTANCE_NAME__`
/// for the property named `__INSTANCE_NAME__`), as usdGenSchema names it.
fn source_api_names(
    source: &Path,
    plugin: &str,
    files: &mut Vec<String>,
) -> Result<BTreeMap<(String, String), String>, String> {
    let relative = format!("pxr/usd/{plugin}/schema.usda");
    let path = source.join(&relative);
    files.push(relative);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut store = InMemoryStore::default();
    let parsed = layerstack_usda::parser::parse(&text);
    // Its sublayers name other schema files, which need not resolve.
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    let custom_data = store.tokens.intern("customData");
    let placeholder = layerstack::schema::INSTANCE_NAME_PLACEHOLDER;
    let mut names = BTreeMap::new();
    for (path, spec) in &emitted.layer.prims {
        let schema_path = store.paths.resolve(*path);
        if schema_path.depth() != 1 {
            continue;
        }
        let Some(schema) = schema_path
            .leaf()
            .map(|t| store.tokens.resolve(t).to_string())
        else {
            continue;
        };
        let entry = |data: Option<&FieldValue>, key: &str| match data {
            Some(FieldValue::Value(Value::Dictionary(entries))) => {
                entries.iter().find_map(|(k, v)| match v {
                    Value::String(text) if &**k == key => Some(text.to_string()),
                    Value::Token(token) if &**k == key => {
                        Some(store.tokens.resolve(*token).to_string())
                    }
                    _ => None,
                })
            }
            _ => None,
        };
        let prefix = entry(spec.field(custom_data), "propertyNamespacePrefix");
        for property in &spec.properties {
            let Some(api_name) = entry(property.spec.metadata(custom_data), "apiName") else {
                continue;
            };
            let raw = store.tokens.resolve(property.name);
            let generated = match &prefix {
                None => raw.to_string(),
                Some(prefix) if raw == placeholder => format!("{prefix}:{placeholder}"),
                Some(prefix) => format!("{prefix}:{placeholder}:{raw}"),
            };
            names.insert((schema.clone(), generated), api_name);
        }
    }
    Ok(names)
}

/// The schema name an applied name or inclusion starts with.
fn schema_part(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

/// The identifier of the typed schema `info` derives from: its first base
/// that is a typed schema, or none at `UsdTyped`'s base.
fn parent_of<'t>(
    info: &TypeInfo,
    types: &'t BTreeMap<String, TypeInfo>,
) -> Result<Option<&'t str>, String> {
    for base in &info.bases {
        if base == "UsdSchemaBase" {
            return Ok(None);
        }
        let Some(base_info) = types.get(base) else {
            return Err(format!(
                "base {base} is in no generated domain; add its domain to DOMAINS"
            ));
        };
        if let Some(identifier) = &base_info.identifier {
            return Ok(Some(identifier));
        }
    }
    Ok(None)
}

fn strings(json: &Json) -> Vec<String> {
    json.as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// The directory holding a domain's `generatedSchema.usda` and
/// `plugInfo.json`.
fn definitions(pxr: &Path, source: &Path, plugin: &str, origin: Origin) -> PathBuf {
    match origin {
        Origin::Wheel => pxr.join("pluginfo").join(plugin).join("resources"),
        Origin::Source => source.join("pxr").join("usd").join(plugin),
    }
}

/// A `plugInfo.json`, without its `#` comment lines, which JSON has not.
fn plug_info(path: &Path) -> Result<Json, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let json: String = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str(&json).map_err(|e| format!("{}: {e}", path.display()))
}

/// The release of the usd-core wheel installed in `site_packages`.
fn wheel_version(site_packages: &Path) -> Result<String, String> {
    let entries =
        fs::read_dir(site_packages).map_err(|e| format!("{}: {e}", site_packages.display()))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("usd_core-") && name.ends_with(".dist-info") {
            let metadata = fs::read_to_string(entry.path().join("METADATA"))
                .map_err(|e| format!("{name}: {e}"))?;
            if let Some(version) = metadata
                .lines()
                .find_map(|line| line.strip_prefix("Version: "))
            {
                return Ok(version.trim().to_string());
            }
        }
    }
    Err(format!(
        "no usd_core dist-info in {}; point --pxr at a usd-core wheel's pxr package",
        site_packages.display()
    ))
}

/// Rejects every asset path: generated schema layers have none.
pub(crate) struct NoAssets;

impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
