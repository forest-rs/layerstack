// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The generator's model of OpenUSD's schemas, read from a usd-core wheel.
//!
//! The model keeps everything the generated code needs, the registry tables
//! and the typed accessors built from the same schemas: each schema's kind,
//! parent, built-ins, auto-applies and properties, and each property's
//! declared type, variability and fallback.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use layerstack::schema::{SchemaDeclaration, read_generated_schema};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, LayerId, PathInterner, PropertyKind,
    ResolvedAsset, SchemaKind, TokenInterner, Value, Variability,
};
use serde_json::Value as Json;

/// A domain: one OpenUSD schema plugin.
#[derive(Debug)]
pub(crate) struct Domain {
    /// The plugin's name (`usdGeom`).
    pub(crate) plugin: &'static str,
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
}

/// What the generator read, and from where.
#[derive(Debug)]
pub(crate) struct Model {
    /// The usd-core release (`26.8`).
    pub(crate) version: String,
    /// The files read, relative to `site-packages`.
    pub(crate) files: Vec<String>,
    /// The domains, in generation order.
    pub(crate) domains: Vec<Domain>,
    /// The interner the fallback values' tokens are in.
    pub(crate) tokens: TokenInterner,
}

/// The domains generated: `(plugin, Rust name)`. Adding a domain is one
/// line here.
pub(crate) const DOMAINS: &[(&str, &str)] = &[
    ("usd", "Usd"),
    ("usdGeom", "UsdGeom"),
    ("usdShade", "UsdShade"),
    ("usdLux", "UsdLux"),
];

/// A type a plugin declares: its schema identifier, kind and bases.
struct TypeInfo {
    plugin: &'static str,
    identifier: Option<String>,
    kind: String,
    bases: Vec<String>,
}

/// Reads the model from `pxr`, the `pxr` package directory of a usd-core
/// wheel.
pub(crate) fn read(pxr: &Path) -> Result<Model, String> {
    let site_packages = pxr.parent().ok_or("the pxr directory has no parent")?;
    let version = wheel_version(site_packages)?;
    let mut files = Vec::new();
    let relative = |path: &Path| {
        path.strip_prefix(site_packages)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };

    // Every declared type of every generated domain, to resolve bases
    // across domains (`UsdLuxBoundableLightBase` derives from
    // `UsdGeomBoundable`).
    let mut types: BTreeMap<String, TypeInfo> = BTreeMap::new();
    let mut plugin_auto_applies: BTreeMap<&'static str, Vec<(String, String)>> = BTreeMap::new();
    for &(plugin, _) in DOMAINS {
        let path = resources(pxr, plugin).join("plugInfo.json");
        files.push(relative(&path));
        let info = plug_info(&path)?;
        for plugin_info in info["Plugins"].as_array().ok_or("no Plugins")? {
            let info = &plugin_info["Info"];
            if let Some(types_json) = info["Types"].as_object() {
                for (name, entry) in types_json {
                    types.insert(
                        name.clone(),
                        TypeInfo {
                            plugin,
                            identifier: entry["schemaIdentifier"].as_str().map(String::from),
                            kind: entry["schemaKind"].as_str().unwrap_or_default().into(),
                            bases: strings(&entry["bases"]),
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

    let mut store = InMemoryStore::default();
    let mut domains = Vec::new();
    for (index, &(plugin, variant)) in DOMAINS.iter().enumerate() {
        let path = resources(pxr, plugin).join("generatedSchema.usda");
        files.push(relative(&path));
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = layerstack_usda::parser::parse(&source);
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
            declared.push(SchemaDeclaration {
                name: store.tokens.intern(identifier),
                kind,
                parent: parent.map(|parent| store.tokens.intern(parent)),
            });
        }
        let definitions =
            read_generated_schema(&emitted.layer, &declared, &mut store.tokens, &store.paths)
                .map_err(|e| format!("{}: {e}", path.display()))?;

        let mut schemas = Vec::new();
        for definition in definitions {
            let name = String::from(store.tokens.resolve(definition.name));
            let convert = |p: &layerstack::PropertyDefinition, tokens: &TokenInterner| Property {
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
            };
            schemas.push(Schema {
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
        schemas.sort_by(|a, b| a.name.cmp(&b.name));
        left_out.sort();
        domains.push(Domain {
            plugin,
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
        dependencies.sort_by_key(|plugin| DOMAINS.iter().position(|(p, _)| p == plugin));
        domain.dependencies = dependencies;
    }

    Ok(Model {
        version,
        files,
        domains,
        tokens: store.tokens,
    })
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

fn resources(pxr: &Path, plugin: &str) -> PathBuf {
    pxr.join("pluginfo").join(plugin).join("resources")
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
struct NoAssets;

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
