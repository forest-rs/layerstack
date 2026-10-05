// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Immutable upstream shading data and dependency-validated retention.
//!
//! Owns USD graph capture and component revisions; hosts own shader execution,
//! asset identifier creation, external resource versions, and rendering.
//! AOUSD Core §7.6.4 (typed attributes), §12.3–12.5 (values/connections);
//! OpenUSD `UsdShadeUtils::GetValueProducingAttributes`, `ComputeSurfaceSource`.

use super::{
    ConnectedSources, MaterialSource, MaterialTerminal, PortKind, ShadingDependency, ShadingIssue,
    ValueSourceKind, ValueSources,
};
use crate::{PrimView, Scene, Time, assets::AssetReference, usd_shade::Material};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use layerstack::{
    ArrayReadError, AttributeQuery, Changes, PathId, PrimSnapshot, PropertyPath, PropertyType,
    Provenance, StoreIdentity, Value,
};

mod cache;
mod defaults;
pub use cache::{
    MaterialCacheStats, MaterialNetworkCache, MaterialNetworkSample, MaterialRevisions,
};

/// Resolved USD storage, retaining decode failures instead of replacing them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaterialValue {
    /// Selected value; absence includes blocks and unavailable numeric samples.
    pub value: Option<Value>,
    /// Captured token/string/asset spelling, independent of future interners.
    pub text: Option<String>,
    /// Winning source, retained independently of optional stage provenance.
    pub provenance: Option<Provenance>,
    /// Deferred numeric data failed to decode; no weaker/default value is used.
    pub decode_error: Option<ArrayReadError>,
}
impl MaterialValue {
    /// An owned resource spelling and its authoring anchor, without loading it.
    #[must_use]
    pub fn asset_reference(&self) -> Option<AssetReference> {
        AssetReference::from_value(self.value.as_ref()?, self.provenance.as_ref())
    }
}
/// Why a constant was selected. USD fallbacks and shader defaults are distinct.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialValueOrigin {
    /// Authored value selected through the provider trace, including own input.
    Authored,
    /// A schema-registry fallback for an unconnected USD attribute.
    SchemaFallback,
    /// Default in a recognized shader definition, not an authored USD opinion.
    NodeDefault,
}
/// A captured constant with explicit fallback origin.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialConstant {
    /// Resolved storage and authoring evidence.
    pub value: MaterialValue,
    /// Source category, without conflating shader defaults and USD fallbacks.
    pub origin: MaterialValueOrigin,
}
/// An input or output; absent standard input names can carry definition defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialPort {
    /// Existing USD attribute; None for a solely shader-definition input.
    pub property: Option<PropertyPath>,
    /// Base name without `inputs:` or `outputs:`.
    pub name: String,
    /// Input or output namespace.
    pub kind: PortKind,
    /// Composed declaration, or known node-definition declaration when absent.
    pub property_type: Option<PropertyType>,
    /// This attribute's own USD value, independent of connections.
    pub own: MaterialValue,
    /// Shader-definition fallback, retained separately even when overridden.
    /// Definition tokens use owned string storage with their token declaration;
    /// capture never invents or interns token IDs for absent definition ports.
    pub node_default: Option<Value>,
    /// Direct connections and invalid targets, retaining authored order.
    pub connected: ConnectedSources,
    /// Forwarded providers and chains, following native interface semantics.
    pub providers: ValueSources,
    /// One captured value per provider; shader outputs remain unevaluated.
    pub provider_values: Vec<MaterialValue>,
    /// Constant selection. None means execution, multiple providers, blocked,
    /// unavailable or failed decoding; inspect all evidence before choosing policy.
    pub constant: Option<MaterialConstant>,
}
/// One upstream prim, captured once even when reached through shared branches.
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialNode {
    /// Store-local prim identity.
    pub path: PathId,
    /// Owned readable path for diagnostics without a live store.
    pub path_name: String,
    /// Composed prim type name, if present.
    pub type_name: Option<String>,
    /// Composed `info:id`, including unknown plugin identifiers.
    pub identifier: Option<String>,
    /// Composed `info:implementationSource`; unknown values remain inspectable.
    pub implementation_source: Option<String>,
    /// Whether the composed prim is a typed Shader, including derived schemas.
    pub is_shader: bool,
    /// Whether built-in connectability treats this prim as an interface container.
    pub is_container: bool,
    /// Composed shading ports, plus absent recognized definition inputs.
    /// Containers retain every input and reachable forwarding outputs; outputs
    /// from other material contexts are outside this selected upstream graph.
    pub ports: Vec<MaterialPort>,
}
impl MaterialNode {
    /// Captured input by base name, including an absent standard input's default.
    #[must_use]
    pub fn input(&self, name: &str) -> Option<&MaterialPort> {
        self.ports
            .iter()
            .find(|p| p.kind == PortKind::Input && p.name == name)
    }
}
/// A resource occurrence; this versions authored reference identity, not bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialResource {
    /// Attribute supplying the value, or None for a shader-definition default.
    pub property: Option<PropertyPath>,
    /// Original spelling and winning authoring layer.
    pub asset: AssetReference,
}
/// Recoverable graph evidence; unknown nodes remain in the capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaterialIssue {
    /// Native connection traversal rejected this branch.
    Connection(ShadingIssue),
    /// An executable node identifier or implementation is outside built-in nodes.
    UnknownNode(PathId),
    /// A branch of shader-to-shader inputs revisits a node.
    ShaderCycle(Vec<PathId>),
    /// A primvar reader has an unavailable or shader-driven name.
    DynamicPrimvarName(PathId),
    /// Selected numeric storage could not decode.
    Decode {
        /// Attribute holding the storage.
        property: PropertyPath,
        /// Retained source failure.
        error: ArrayReadError,
    },
}
/// Immutable material graph. IDs retain their captured store affinity; all
/// spellings needed by the projection are owned, so old captures need no store.
#[derive(Clone, Debug)]
pub struct MaterialNetwork {
    /// Originating token/path domain, not serialized identity or a content hash.
    pub store_identity: StoreIdentity,
    /// Material identity in that domain.
    pub material: PathId,
    /// Requested terminal category.
    pub terminal: MaterialTerminal,
    /// Explicit evaluation time; shaders themselves are not evaluated.
    pub time: Time,
    /// Selected context/terminal and candidate traversal evidence.
    pub source: MaterialSource,
    /// Reachable prims sorted by store-local path, with shared nodes deduplicated.
    pub nodes: Vec<MaterialNode>,
    /// Statically known primvar names required by upstream reader nodes.
    pub primvars: Vec<String>,
    /// Resources from own/provider constants, including nested arrays/dictionaries.
    pub resources: Vec<MaterialResource>,
    /// Recoverable connection, unknown-node, cycle and decode problems.
    pub issues: Vec<MaterialIssue>,
    /// Every inspected attribute/name, including absent contexts and targets.
    pub dependencies: Vec<ShadingDependency>,
}
/// A capture cannot establish a valid scene/material request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MaterialCaptureError {
    /// Stage and supplied store do not share token/path domains.
    SceneDomain,
    /// Cache belongs to another store domain; clear before rebinding it.
    CacheDomain,
    /// The requested prim is not a populated typed Material.
    MissingMaterial(PathId),
    /// Numeric time is NaN or infinite.
    InvalidTime,
}
impl core::fmt::Display for MaterialCaptureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "material capture: {self:?}")
    }
}
impl core::error::Error for MaterialCaptureError {}

pub(super) struct Evidence {
    pub prims: Vec<PrimSnapshot>,
    pub queries: Vec<AttributeQuery>,
}
impl Evidence {
    fn current(&self, scene: &Scene<'_>, time: Time) -> bool {
        self.prims.iter().all(|p| p.is_current(scene.stage()))
            && self
                .queries
                .iter()
                .all(|q| q.is_current(scene.stage(), time))
    }
}
struct Capture<'a, 'b> {
    scene: &'a Scene<'b>,
    time: Time,
    queries: BTreeMap<PropertyPath, (MaterialValue, AttributeQuery)>,
    dependencies: Vec<ShadingDependency>,
    issues: Vec<MaterialIssue>,
}
impl Capture<'_, '_> {
    fn value(&mut self, property: PropertyPath) -> MaterialValue {
        if let Some((value, _)) = self.queries.get(&property) {
            return value.clone();
        }
        let mut query = AttributeQuery::new(property);
        let mut result = MaterialValue::default();
        match query.try_get(self.scene.stage(), self.time) {
            Ok(Some(value)) => {
                result.text = text(&value.value, self.scene);
                result.provenance = self
                    .scene
                    .stage()
                    .read_property_with_provenance(property, self.time, |_| Some(()))
                    .and_then(|r| r.provenance);
                result.value = Some(value.value);
            }
            Ok(None) => {}
            Err(error) => {
                self.issues.push(MaterialIssue::Decode {
                    property,
                    error: error.clone(),
                });
                result.decode_error = Some(error);
            }
        }
        // Native default-time blocks suppress reads even if raw schema-aware
        // fallback resolution supplies a value. AOUSD Core §12.3.6.
        if self.time == Time::Default
            && self
                .scene
                .stage()
                .explain_property_path(property)
                .and_then(|o| o.iter().find_map(|o| o.value.default_value()))
                == Some(&Value::Blocked)
        {
            result = MaterialValue::default();
        }
        self.queries.insert(property, (result.clone(), query));
        result
    }
    fn port(&mut self, port: super::Port<'_>, default: Option<Value>) -> MaterialPort {
        let property = port.path();
        let own = self.value(property);
        let connected = port.connected_sources();
        let providers = port.value_sources();
        self.dependencies
            .extend(providers.dependencies.iter().cloned());
        self.issues.extend(
            providers
                .issues
                .iter()
                .cloned()
                .map(MaterialIssue::Connection),
        );
        let provider_values = providers
            .sources
            .iter()
            .map(|p| {
                if p.kind == ValueSourceKind::AuthoredValue {
                    self.value(p.attribute)
                } else {
                    MaterialValue::default()
                }
            })
            .collect::<Vec<_>>();
        // Successful native fallback remains usable despite a failed sibling
        // connection. Multiple providers are preserved without choosing a winner.
        let constant = if providers.sources.len() == 1
            && providers.sources[0].kind == ValueSourceKind::AuthoredValue
        {
            provider_values[0].value.as_ref().map(|_| MaterialConstant {
                value: provider_values[0].clone(),
                origin: MaterialValueOrigin::Authored,
            })
        } else if providers.sources.is_empty()
            && connected.sources.is_empty()
            && connected.invalid.is_empty()
        {
            own.value
                .as_ref()
                .map(|_| MaterialConstant {
                    value: own.clone(),
                    origin: MaterialValueOrigin::SchemaFallback,
                })
                .or_else(|| {
                    if own.decode_error.is_none() && !blocked(self.scene, property, self.time) {
                        default.as_ref().map(|v| node_constant(v.clone()))
                    } else {
                        None
                    }
                })
        } else {
            None
        };
        MaterialPort {
            property: Some(property),
            name: port.name().into(),
            kind: port.kind(),
            property_type: port.property_type(),
            own,
            node_default: default,
            connected,
            providers,
            provider_values,
            constant,
        }
    }
}
fn blocked(scene: &Scene<'_>, property: PropertyPath, time: Time) -> bool {
    let opinions = scene
        .stage()
        .explain_property_path(property)
        .unwrap_or_default();
    let default_blocked =
        opinions.iter().find_map(|o| o.value.default_value()) == Some(&Value::Blocked);
    if time == Time::Default {
        return default_blocked;
    }
    // Missing selected values after an authored value/animation opinion are
    // unavailable, not permission to substitute a shader-definition default.
    // HasAuthoredValue excludes value blocks. Retain explicit default-block
    // evidence as well; sample/spline selection still belongs to the ordinary
    // numeric resolver, which already supplies `own` before this fallback gate.
    default_blocked
        || PrimView::new(*scene, property.prim_path())
            .has_authored_value(scene.store().tokens().resolve(property.property()))
}
fn text(value: &Value, scene: &Scene<'_>) -> Option<String> {
    match value {
        Value::Token(t) => Some(scene.store().tokens().resolve(*t).into()),
        Value::String(t) | Value::Asset(t) => Some(t.to_string()),
        _ => None,
    }
}
fn node_constant(value: Value) -> MaterialConstant {
    let text = match &value {
        Value::String(s) | Value::Asset(s) => Some(s.to_string()),
        _ => None,
    };
    MaterialConstant {
        value: MaterialValue {
            value: Some(value),
            text,
            ..MaterialValue::default()
        },
        origin: MaterialValueOrigin::NodeDefault,
    }
}
impl MaterialNetwork {
    /// Captures upstream USD storage and connections at explicit time. Unknown
    /// nodes, shared nodes, cycles, multi-source inputs and decoding failures
    /// remain evidence. Standard definition defaults never become authored values.
    /// No shader execution, texture loading or identifier resolution occurs.
    pub fn capture(
        scene: &Scene<'_>,
        material: PathId,
        terminal: MaterialTerminal,
        contexts: &[&str],
        time: Time,
    ) -> Result<Self, MaterialCaptureError> {
        Self::capture_evidence(scene, material, terminal, contexts, time)
            .map(|(network, _)| network)
    }
    pub(super) fn capture_evidence(
        scene: &Scene<'_>,
        material: PathId,
        terminal: MaterialTerminal,
        contexts: &[&str],
        time: Time,
    ) -> Result<(Self, Evidence), MaterialCaptureError> {
        if scene.stage().store_identity() != Some(&scene.store().identity()) {
            return Err(MaterialCaptureError::SceneDomain);
        }
        if matches!(time,Time::At {code,..} if !code.is_finite()) {
            return Err(MaterialCaptureError::InvalidTime);
        }
        let material_view = Material::new(scene, material)
            .ok_or(MaterialCaptureError::MissingMaterial(material))?;
        let source = material_view.compute_terminal_source(terminal, contexts);
        let mut capture = Capture {
            scene,
            time,
            queries: BTreeMap::new(),
            dependencies: source.trace.dependencies.clone(),
            issues: source
                .trace
                .issues
                .iter()
                .cloned()
                .map(MaterialIssue::Connection)
                .collect(),
        };
        let mut pending = alloc::vec![material];
        pending.extend(
            source
                .trace
                .sources
                .iter()
                .flat_map(|s| s.chain.iter().map(|p| p.prim_path())),
        );
        let mut active_ports: BTreeSet<_> = source
            .trace
            .sources
            .iter()
            .flat_map(|s| s.chain.iter().copied())
            .collect();
        let mut visited = BTreeSet::new();
        let mut nodes = Vec::new();
        while let Some(path) = pending.pop() {
            if !visited.insert(path) || !scene.stage().has_prim(path) {
                continue;
            }
            let view = PrimView::new(*scene, path);
            let read_text = |name: &str, capture: &mut Capture<'_, '_>| {
                view.property_path(name).and_then(|p| capture.value(p).text)
            };
            let identifier = read_text("info:id", &mut capture);
            let implementation_source = read_text("info:implementationSource", &mut capture);
            let is_container = scene
                .connectable_behavior(path)
                .is_some_and(|b| b.is_container);
            let is_shader = scene.is_a(path, "Shader");
            let defaults = if is_shader && implementation_source.as_deref() == Some("id") {
                defaults::inputs(identifier.as_deref().unwrap_or(""))
            } else {
                Vec::new()
            };
            if !is_container && defaults.is_empty() {
                capture.issues.push(MaterialIssue::UnknownNode(path));
            }
            let mut ports = Vec::new();
            for kind in [PortKind::Input, PortKind::Output] {
                for port in view.ports(kind) {
                    if is_container
                        && kind == PortKind::Output
                        && !active_ports.contains(&port.path())
                    {
                        continue;
                    }
                    let default = defaults
                        .iter()
                        .find(|d| d.0 == port.name())
                        .map(|d| d.2.clone());
                    let captured = capture.port(port, default);
                    if !is_container || active_ports.contains(&port.path()) {
                        active_ports.extend(captured.connected.sources.iter().copied());
                        active_ports.extend(captured.providers.dependencies.iter().filter_map(
                            |d| {
                                scene
                                    .store()
                                    .tokens()
                                    .lookup(&d.property)
                                    .map(|t| PropertyPath::new(d.prim, t))
                            },
                        ));
                        active_ports.extend(
                            captured
                                .providers
                                .sources
                                .iter()
                                .flat_map(|p| p.chain.iter().copied()),
                        );
                        pending.extend(captured.connected.sources.iter().map(|p| p.prim_path()));
                        pending.extend(captured.providers.dependencies.iter().map(|d| d.prim));
                        pending.extend(
                            captured
                                .providers
                                .sources
                                .iter()
                                .flat_map(|p| p.chain.iter().map(|p| p.prim_path())),
                        );
                    }
                    ports.push(captured);
                }
            }
            for (name, ty, value) in defaults {
                if ports
                    .iter()
                    .any(|p| p.kind == PortKind::Input && p.name == name)
                {
                    continue;
                }
                capture.dependencies.push(ShadingDependency {
                    prim: path,
                    property: alloc::format!("inputs:{name}"),
                });
                ports.push(MaterialPort {
                    property: None,
                    name: name.into(),
                    kind: PortKind::Input,
                    property_type: Some(ty),
                    own: MaterialValue::default(),
                    node_default: Some(value.clone()),
                    connected: ConnectedSources::default(),
                    providers: ValueSources::default(),
                    provider_values: Vec::new(),
                    constant: Some(node_constant(value)),
                });
            }
            ports.sort_by(|a, b| {
                (a.kind == PortKind::Output, &a.name).cmp(&(b.kind == PortKind::Output, &b.name))
            });
            let type_name = scene
                .stage()
                .prim(path, scene.store())
                .and_then(|p| p.type_name())
                .map(|t| scene.store().tokens().resolve(t).into());
            nodes.push(MaterialNode {
                path,
                path_name: scene.store().paths().display(path, scene.store().tokens()),
                type_name,
                identifier,
                implementation_source,
                is_shader,
                is_container,
                ports,
            });
        }
        // A shared container can be visited before a later shader branch makes
        // another output reachable. Provider traces already discover every
        // upstream prim; finish its port inventory using the final endpoint set.
        for node in nodes.iter_mut().filter(|node| node.is_container) {
            for port in PrimView::new(*scene, node.path).ports(PortKind::Output) {
                if active_ports.contains(&port.path())
                    && !node.ports.iter().any(|p| p.property == Some(port.path()))
                {
                    node.ports.push(capture.port(port, None));
                }
            }
            node.ports.sort_by(|a, b| {
                (a.kind == PortKind::Output, &a.name).cmp(&(b.kind == PortKind::Output, &b.name))
            });
        }
        nodes.sort_by_key(|n| n.path);
        let mut primvars = BTreeSet::new();
        let mut resources = Vec::new();
        for node in &nodes {
            if node
                .identifier
                .as_deref()
                .is_some_and(|id| id.starts_with("UsdPrimvarReader_"))
            {
                if let Some(name) = node
                    .input("varname")
                    .and_then(|p| p.constant.as_ref())
                    .and_then(|v| v.value.text.as_ref())
                {
                    if !name.is_empty() {
                        primvars.insert(name.clone());
                    }
                } else {
                    capture
                        .issues
                        .push(MaterialIssue::DynamicPrimvarName(node.path));
                }
            }
            for port in &node.ports {
                collect_assets(&port.own, port.property, &mut resources);
                for (provider, value) in port.providers.sources.iter().zip(&port.provider_values) {
                    collect_assets(value, Some(provider.attribute), &mut resources);
                }
            }
        }
        detect_cycles(&nodes, &mut capture.issues);
        capture
            .dependencies
            .sort_by(|a, b| (a.prim, &a.property).cmp(&(b.prim, &b.property)));
        capture.dependencies.dedup();
        let mut dependency_prims = BTreeSet::from([material]);
        dependency_prims.extend(nodes.iter().map(|n| n.path));
        dependency_prims.extend(capture.dependencies.iter().map(|d| d.prim));
        let evidence = Evidence {
            prims: dependency_prims
                .into_iter()
                .map(|p| scene.stage().prim_snapshot(p))
                .collect(),
            queries: capture.queries.into_values().map(|(_, q)| q).collect(),
        };
        Ok((
            Self {
                store_identity: scene.store().identity(),
                material,
                terminal,
                time,
                source,
                nodes,
                primvars: primvars.into_iter().collect(),
                resources,
                issues: capture.issues,
                dependencies: capture.dependencies,
            },
            evidence,
        ))
    }
    /// Finds a captured node using its original domain's ID. No live store lookup.
    #[must_use]
    pub fn node(&self, path: PathId) -> Option<&MaterialNode> {
        self.nodes.iter().find(|n| n.path == path)
    }
}
fn collect_assets(
    value: &MaterialValue,
    property: Option<PropertyPath>,
    out: &mut Vec<MaterialResource>,
) {
    fn visit(
        value: &Value,
        property: Option<PropertyPath>,
        source: Option<&Provenance>,
        out: &mut Vec<MaterialResource>,
    ) {
        match value {
            Value::Asset(_) => {
                if let Some(asset) = AssetReference::from_value(value, source) {
                    let resource = MaterialResource { property, asset };
                    if !out.contains(&resource) {
                        out.push(resource);
                    }
                }
            }
            Value::Array(v) => {
                for v in v {
                    visit(v, property, source, out);
                }
            }
            Value::Dictionary(v) => {
                for (_, v) in v {
                    visit(v, property, source, out);
                }
            }
            _ => {}
        }
    }
    if let Some(v) = &value.value {
        visit(v, property, value.provenance.as_ref(), out);
    }
}
fn detect_cycles(nodes: &[MaterialNode], issues: &mut Vec<MaterialIssue>) {
    // Explicit DFS work avoids process-stack dependence on shader graph depth.
    for start in nodes {
        let mut stack = alloc::vec![(start.path, Vec::new())];
        let mut done = BTreeSet::new();
        while let Some((path, mut chain)) = stack.pop() {
            if chain.contains(&path) {
                chain.push(path);
                let issue = MaterialIssue::ShaderCycle(chain);
                if !issues.contains(&issue) {
                    issues.push(issue);
                }
                continue;
            }
            if !done.insert(path) {
                continue;
            }
            chain.push(path);
            if let Some(node) = nodes.iter().find(|n| n.path == path) {
                for port in node.ports.iter().filter(|p| p.kind == PortKind::Input) {
                    for source in port
                        .providers
                        .sources
                        .iter()
                        .filter(|s| s.kind == ValueSourceKind::ShaderOutput)
                    {
                        stack.push((source.attribute.prim_path(), chain.clone()));
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests;
