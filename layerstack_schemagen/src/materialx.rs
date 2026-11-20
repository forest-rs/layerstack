// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Convert selected `MaterialX` XML interfaces to the shader code generator's model.

use crate::shader_nodes::{Node, NodePort, finite_default};
use layerstack::{PropertyType, Value, Variability};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;

type XmlPort = (String, String, BTreeMap<String, String>);
type XmlNode = (String, Vec<XmlPort>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Input,
    Output,
}

#[derive(Debug)]
enum Default {
    Literal(String),
    GeomProp(String),
    Input {
        name: String,
        fallback: Option<String>,
    },
}

#[derive(Debug)]
pub(crate) struct PortDefinition {
    name: String,
    direction: Direction,
    type_name: String,
    default: Option<Default>,
}

pub(crate) fn read(path: &Path, definitions: &[&str]) -> Result<Vec<Node>, String> {
    let output = Command::new("python3")
        .arg("-c")
        .arg(include_str!("materialx.py"))
        .arg(path)
        .args(definitions)
        .output()
        .map_err(|e| format!("`MaterialX` generation needs python3: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`MaterialX`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let xml: Vec<XmlNode> = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid extracted MaterialX interface: {error}"))?;
    let mut nodes = xml
        .into_iter()
        .map(|(id, ports)| node(id, &ports))
        .collect::<Result<Vec<_>, _>>()?;
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(nodes)
}

fn node(id: String, ports: &[XmlPort]) -> Result<Node, String> {
    let single_output = ports.iter().filter(|(tag, _, _)| tag == "output").count() == 1;
    let ports = ports
        .iter()
        .map(|port| {
            port_definition(port, ports, single_output)
                .map_err(|error| format!("{id}.{}: {error}", port.1))
        })
        .collect::<Result<_, _>>()?;
    Ok(Node { id, ports })
}

fn port_definition(
    (tag, name, attrs): &XmlPort,
    ports: &[XmlPort],
    single_output: bool,
) -> Result<NodePort, String> {
    let direction = match tag.as_str() {
        "input" => Direction::Input,
        "output" => Direction::Output,
        _ => return Err(format!("unknown port direction {tag:?}")),
    };
    // AOUSD Core §7.3.3 (property names) and §8.5 (path grammar).
    if !name.split(':').all(layerstack::ident::is_identifier) {
        return Err("invalid port name".into());
    }
    let attr = |key: &str| attrs.get(key).map(String::as_str);
    let type_name = attr("type")
        .filter(|name| !name.is_empty())
        .ok_or("missing port type")?;
    let ty = usd_type(type_name)?;
    let default = definition_default(direction, type_name, attr, ports)?;
    let literal = match &default {
        Some(Default::Literal(value)) => Some(value),
        Some(Default::Input { fallback, .. }) => fallback.as_ref(),
        _ => None,
    };
    let value = literal
        .map(|value| literal_value(&ty, value).map_err(|error| format!("{error}: {value:?}")))
        .transpose()?;
    // usdMtlx reader.cpp's _Context::_AddShaderOutput uses outputs:surface
    // for the material terminal. _NodeGraphBuilder::_AddOutput keeps port
    // names, so apply the terminal spelling only to the conventional sole
    // surface output. The descriptor keeps the original name in both cases.
    let usd_name = match (direction, single_output, type_name, name.as_str()) {
        (Direction::Input, _, _, _) => format!("inputs:{name}"),
        (Direction::Output, true, "surfaceshader", "out") => "outputs:surface".into(),
        (Direction::Output, _, _, _) => format!("outputs:{name}"),
    };
    Ok(NodePort {
        name: usd_name,
        ty,
        variability: Variability::Varying,
        default: value,
        doc: attr("doc").unwrap_or_default().into(),
        materialx: Some(PortDefinition {
            name: name.clone(),
            direction,
            type_name: type_name.into(),
            default,
        }),
    })
}

fn definition_default<'a>(
    direction: Direction,
    type_name: &str,
    attr: impl Fn(&str) -> Option<&'a str>,
    ports: &[XmlPort],
) -> Result<Option<Default>, String> {
    // MaterialX 1.39, NodeDef Input Elements and NodeDef Output Elements.
    let value = attr("value");
    let fallback = attr("default");
    let geomprop = attr("defaultgeomprop");
    let input = attr("defaultinput");
    match direction {
        Direction::Input => {
            if fallback.is_some() || input.is_some() {
                return Err("default and defaultinput are output attributes".into());
            }
            match (value, geomprop) {
                (Some(_), Some(_)) => {
                    Err("value and defaultgeomprop are mutually exclusive".into())
                }
                (Some(text), None) => Ok(Some(Default::Literal(text.into()))),
                (None, Some(name)) => {
                    if name.trim().is_empty() || !matches!(type_name, "vector2" | "vector3") {
                        return Err("defaultgeomprop requires a named property on a vector2 or vector3 input".into());
                    }
                    Ok(Some(Default::GeomProp(name.into())))
                }
                (None, None) => Ok(None),
            }
        }
        Direction::Output => {
            if geomprop.is_some() {
                return Err("defaultgeomprop is an input attribute".into());
            }
            if value.is_some() && fallback.is_some() {
                return Err("value and default specify the same output literal".into());
            }
            let fallback = fallback.or(value).map(String::from);
            if let Some(name) = input {
                let typed_input = ports.iter().any(|(tag, port_name, attrs)| {
                    tag == "input"
                        && port_name == name
                        && attrs.get("type").is_some_and(|ty| ty == type_name)
                });
                if !typed_input {
                    return Err(format!(
                        "defaultinput {name:?} must name an input of type {type_name}"
                    ));
                }
                Ok(Some(Default::Input {
                    name: name.into(),
                    fallback,
                }))
            } else {
                Ok(fallback.map(Default::Literal))
            }
        }
    }
}

fn usd_type(type_name: &str) -> Result<PropertyType, String> {
    // usdMtlx utils.cpp's UsdMtlxGetUsdType defines these shared spellings.
    let usd = match type_name {
        "boolean" => "bool",
        "integer" => "int",
        "float" => "float",
        "color3" => "color3f",
        "color4" => "color4f",
        "vector2" => "float2",
        "vector3" => "float3",
        "vector4" => "float4",
        "matrix33" => "matrix3d",
        "matrix44" => "matrix4d",
        "string" => "string",
        "filename" => "asset",
        "surfaceshader" => "token",
        other => return Err(format!("unsupported MaterialX type {other:?}")),
    };
    PropertyType::parse(usd).map_err(|error| error.to_string())
}

fn literal_value(ty: &PropertyType, literal: &str) -> Result<Value, &'static str> {
    let value = match ty.default_scalar() {
        Value::Bool(_) => Value::Bool(parse(literal)?),
        Value::Int(_) => Value::Int(parse(literal)?),
        Value::Float(_) => Value::Float(parse(literal)?),
        Value::Vec2f(_) => Value::Vec2f(components(literal)?),
        Value::Vec3f(_) => Value::Vec3f(components(literal)?),
        Value::Vec4f(_) => Value::Vec4f(components(literal)?),
        Value::Matrix3d(_) => Value::Matrix3d(Box::new(components(literal)?)),
        Value::Matrix4d(_) => Value::Matrix4d(Box::new(components(literal)?)),
        Value::String(_) => Value::string(literal),
        Value::Asset(_) => Value::Asset(Arc::from(literal)),
        _ => return Err("MaterialX shader ports cannot have a literal default"),
    };
    if !finite_default(&value) {
        return Err("literal defaults must contain finite numbers");
    }
    Ok(value)
}

fn parse<T: FromStr>(text: &str) -> Result<T, &'static str> {
    text.trim().parse().map_err(|_| "invalid literal default")
}

fn components<T: FromStr, const N: usize>(text: &str) -> Result<[T; N], &'static str> {
    text.split(',')
        .map(parse)
        .collect::<Result<Vec<_>, _>>()?
        .try_into()
        .map_err(|_| "incorrect number of components in literal default")
}

pub(crate) fn render_definition(out: &mut String, node: &Node, schema_crate: &str) {
    let default_ty = format!("{schema_crate}::materialx::MaterialXDefault");
    let _ = writeln!(
        out,
        "    /// The MaterialX NodeDef interface, independent of authored scene state.\n    pub const DEFINITION: MaterialXNode = MaterialXNode {{ id: Self::ID, ports: &["
    );
    for port in &node.ports {
        let metadata = port.materialx.as_ref().expect("MaterialX port definition");
        let default = match &metadata.default {
            Some(Default::Literal(value)) => format!("Some({default_ty}::Literal({value:?}))"),
            Some(Default::GeomProp(name)) => format!("Some({default_ty}::GeomProp({name:?}))"),
            Some(Default::Input { name, fallback }) => {
                format!("Some({default_ty}::Input {{ name: {name:?}, fallback: {fallback:?} }})")
            }
            None => "None".into(),
        };
        let _ = writeln!(
            out,
            "        MaterialXPort {{ name: {:?}, direction: PortKind::{:?}, type_name: {:?}, usd_name: {:?}, usd_type_name: {:?}, default: {default} }},",
            metadata.name,
            metadata.direction,
            metadata.type_name,
            port.name,
            port.ty.type_name()
        );
    }
    out.push_str("    ] };\n");
}

#[cfg(test)]
mod tests {
    use crate::generate_materialx_library;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Source(PathBuf);

    impl Source {
        fn new(xml: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "layerstack-materialx-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("nodes.mtlx"), xml).unwrap();
            Self(dir)
        }

        fn generate(&self, definitions: &[&str]) -> Result<String, String> {
            generate_materialx_library(self.0.join("nodes.mtlx"), definitions)
        }
    }

    impl Drop for Source {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_are_preserved_across_inheritance() {
        let source = Source::new(
            r#"<materialx xmlns:xi="http://www.w3.org/2001/XInclude">
                <xi:include href="base.mtlx"/>
                <nodedef name="ND_child" inherit="ND_base">
                    <input name="uv" value="0.25, 0.75"/>
                    <input name="normal" defaultgeomprop="Nworld"/>
                    <output name="out" value="0, 1"/>
                </nodedef>
            </materialx>"#,
        );
        fs::write(
            source.0.join("base.mtlx"),
            r#"<materialx>
                <nodedef name="ND_base">
                    <input name="uv" type="vector2" defaultgeomprop="UV0"/>
                    <input name="normal" type="vector3" value="0, 0, 1"/>
                    <output name="out" type="vector2" defaultinput="uv" default="1, 0"/>
                </nodedef>
            </materialx>"#,
        )
        .unwrap();
        let child = source.generate(&["ND_child"]).unwrap();
        assert!(child.contains("pub const DEFINITION: MaterialXNode"));
        assert!(child.contains(r#"MaterialXDefault::Literal("0.25, 0.75")"#));
        assert!(child.contains(r#"MaterialXDefault::GeomProp("Nworld")"#));
        assert!(
            child.contains(r#"MaterialXDefault::Input { name: "uv", fallback: Some("0, 1") }"#)
        );
        assert!(child.contains("pub fn uv_default() -> [f32; 2] { [0.25, 0.75] }"));
        assert!(!child.contains("pub fn normal_default("));
        let base = source.generate(&["ND_base"]).unwrap();
        assert!(base.contains(r#"MaterialXDefault::GeomProp("UV0")"#));
        assert!(base.contains(r#"MaterialXDefault::Input { name: "uv", fallback: Some("1, 0") }"#));
    }

    #[test]
    fn surface_terminal_names_are_normalized_only_for_a_conventional_single_output() {
        let source = Source::new(
            r#"<materialx>
                <nodedef name="ND_explicit">
                    <output name="out" type="surfaceshader"/>
                </nodedef>
                <nodedef name="ND_legacy" type="surfaceshader"/>
                <nodedef name="ND_named">
                    <output name="beauty" type="surfaceshader"/>
                </nodedef>
                <nodedef name="ND_pair">
                    <output name="out" type="surfaceshader"/>
                    <output name="surface" type="float"/>
                </nodedef>
            </materialx>"#,
        );
        let conventional = source.generate(&["ND_legacy", "ND_explicit"]).unwrap();
        let descriptor = r#"name: "out", direction: PortKind::Output, type_name: "surfaceshader", usd_name: "outputs:surface", usd_type_name: "token", default: None"#;
        assert_eq!(conventional.matches(descriptor).count(), 2);
        assert_eq!(
            conventional
                .matches("pub fn create_surface_output(")
                .count(),
            2
        );
        let other = source.generate(&["ND_named", "ND_pair"]).unwrap();
        assert!(other.contains(r#"name: "beauty", direction: PortKind::Output, type_name: "surfaceshader", usd_name: "outputs:beauty", usd_type_name: "token""#));
        assert!(other.contains(r#"name: "out", direction: PortKind::Output, type_name: "surfaceshader", usd_name: "outputs:out", usd_type_name: "token""#));
        assert!(other.contains(r#"name: "surface", direction: PortKind::Output, type_name: "float", usd_name: "outputs:surface", usd_type_name: "float""#));
    }

    #[test]
    fn rejects_malformed_or_colliding_interfaces() {
        for (xml, message) in [
            (
                r#"<nodedef name="ND_test" type="float"><input name="uv" type="vector2" value="0,0" defaultgeomprop="UV0"/></nodedef>"#,
                "value and defaultgeomprop are mutually exclusive",
            ),
            (
                r#"<nodedef name="ND_test" type="float"><input name="uv" type="float" defaultgeomprop="UV0"/></nodedef>"#,
                "defaultgeomprop requires",
            ),
            (
                r#"<nodedef name="ND_test"><input name="uv" type="color3"/><output name="out" type="vector3" defaultinput="uv"/></nodedef>"#,
                "defaultinput \"uv\" must name an input of type vector3",
            ),
            (
                r#"<nodedef name="ND_test"><output name="out" type="float" default="1e40"/></nodedef>"#,
                "literal defaults must contain finite numbers",
            ),
            (
                r#"<nodedef name="ND_test" type="float"><input name="uv" type="vector2" value="1,2,3"/></nodedef>"#,
                "incorrect number of components",
            ),
            (
                r#"<nodedef name="ND_test" type="float"><input name="bad:9name" type="float"/></nodedef>"#,
                "invalid port name",
            ),
            (
                r#"<nodedef name="ND_test" type="float"><input name="x" type="float"/><input name="x" type="float"/></nodedef>"#,
                "duplicate or unnamed port",
            ),
            (
                r#"<nodedef name="ND_test" type="multioutput"/>"#,
                "multioutput NodeDef has no outputs",
            ),
            (
                r#"<nodedef name="ND_test" inherit="ND_base"/><nodedef name="ND_base" inherit="ND_test"/>"#,
                "cyclic NodeDef inheritance",
            ),
            (
                r#"<nodedef name="ND_base" type="float"><input name="x" type="float"/></nodedef><nodedef name="ND_test" inherit="ND_base"><input name="x" type="integer"/></nodedef>"#,
                "incompatible inherited port",
            ),
            (
                r#"<nodedef name="ND_test" type="surfaceshader"><input name="surfaceOutput" type="float"/></nodedef>"#,
                "generated method collision `surface_output`",
            ),
        ] {
            let source = Source::new(&format!("<materialx>{xml}</materialx>"));
            let error = source.generate(&["ND_test"]).unwrap_err();
            assert!(error.contains(message), "{xml}: {error}");
        }
        let source = Source::new(
            r#"<materialx>
                <nodedef name="ND_example" type="float"/>
                <nodedef name="NDExample" type="float"/>
            </materialx>"#,
        );
        let error = source.generate(&["ND_example", "NDExample"]).unwrap_err();
        assert!(
            error.contains("generated shader node type collision"),
            "{error}"
        );
    }
}
