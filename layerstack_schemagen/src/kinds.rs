// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plugin kind declarations; JSON and file access stay in this build-time crate.
use serde_json::Value as Json;
use std::{collections::BTreeMap, fmt::Write as _, path::Path};

const BUILT_INS: &[&str] = &["model", "group", "assembly", "component", "subcomponent"];

fn declarations(json: &Json, kinds: &mut BTreeMap<String, Option<String>>) -> Result<(), String> {
    let plugins = json["Plugins"]
        .as_array()
        .ok_or("plugInfo requires a Plugins array")?;
    for plugin in plugins {
        let Some(info) = plugin.get("Info") else {
            continue;
        };
        let info = info.as_object().ok_or("plugin Info must be an object")?;
        let Some(entries) = info.get("Kinds") else {
            continue;
        };
        for (name, entry) in entries.as_object().ok_or("Kinds must be an object")? {
            let valid = name
                .as_bytes()
                .first()
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
                && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
            if !valid {
                return Err(format!("invalid kind name `{name}`"));
            }
            let entry = entry
                .as_object()
                .ok_or_else(|| format!("kind `{name}` must be an object"))?;
            let base = entry
                .get("baseKind")
                .map(|base| {
                    base.as_str()
                        .ok_or_else(|| format!("kind `{name}` baseKind must be a string"))
                })
                .transpose()?
                .filter(|base| !base.is_empty())
                .map(String::from);
            if BUILT_INS.contains(&name.as_str()) || kinds.insert(name.clone(), base).is_some() {
                return Err(format!("kind `{name}` is declared more than once"));
            }
        }
    }
    Ok(())
}

fn render(kinds: &BTreeMap<String, Option<String>>) -> Result<String, String> {
    // Validate the entire explicit input set before producing output. AOUSD Core
    // §7.6.2.4.4 and §11.4; OpenUSD KindRegistry plugin Kinds/baseKind fields.
    for name in kinds.keys() {
        let mut current = name;
        for step in 0..=kinds.len() {
            let Some(Some(base)) = kinds.get(current) else {
                break;
            };
            if BUILT_INS.contains(&base.as_str()) {
                break;
            }
            if !kinds.contains_key(base) {
                return Err(format!("kind `{current}` has unknown base `{base}`"));
            }
            if step == kinds.len() {
                return Err(format!("kind `{name}` reaches an inheritance cycle"));
            }
            current = base;
        }
    }
    let mut out = String::from(
        "// Generated kind declarations. Do not edit.\n\n/// Plugin-declared kinds in stable name order.\npub const KINDS: &[(&str, Option<&str>)] = &[\n",
    );
    for (name, base) in kinds {
        let base = base
            .as_ref()
            .map_or_else(|| "None".into(), |base| format!("Some({base:?})"));
        let _ = writeln!(out, "    ({name:?}, {base}),");
    }
    out.push_str("] ;\n\n/// Registers the complete plugin hierarchy atomically.\n/// Errors leave the supplied registry unchanged.\npub fn register(registry: &mut ::layerstack_schemas::kind::KindRegistry) -> Result<(), ::layerstack_schemas::kind::KindValidationError> {\n    registry.register_all(KINDS.iter().copied())\n}\n");
    Ok(out)
}

pub(crate) fn generate(paths: &[impl AsRef<Path>]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("select at least one kind plugin".into());
    }
    let mut kinds = BTreeMap::new();
    for path in paths {
        let path = path.as_ref();
        declarations(&crate::model::plug_info(path)?, &mut kinds)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    render(&kinds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn generate(json: Json) -> Result<String, String> {
        let mut kinds = BTreeMap::new();
        declarations(&json, &mut kinds)?;
        render(&kinds)
    }
    #[test]
    fn checks_complete_hierarchies_before_rendering() {
        let source = generate(json!({"Plugins":[{"Info":{"Kinds":{"leaf":{"baseKind":"stem"},"stem":{"baseKind":"model"},"root":{"baseKind":""}}}}]})).unwrap();
        assert!(
            source.contains("Some(\"stem\")") && source.contains("(\"root\", None)"),
            "forward references and root kinds are preserved"
        );
        for kinds in [
            json!({"a":{"baseKind":"b"}}),
            json!({"a":{"baseKind":"b"},"b":{"baseKind":"a"}}),
            json!({"a":{"baseKind":true}}),
            json!({"a":null}),
            json!({"model":{}}),
            json!({"bad name":{}}),
            json!([]),
        ] {
            assert!(
                generate(json!({"Plugins":[{"Info":{"Kinds":kinds}}]})).is_err(),
                "invalid declaration: {kinds}"
            );
        }
        assert!(
            generate(json!({"Plugins":[{"Info":{"Kinds":{"a":{}}}},{"Info":{"Kinds":{"a":{}}}}]}))
                .is_err(),
            "cross-plugin collisions reject"
        );
    }
}
