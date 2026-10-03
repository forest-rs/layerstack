// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Composed shading connections and material terminal resolution.
//!
//! Connection lists follow AOUSD Core §12.4. Terminal traversal follows
//! OpenUSD `UsdShadeUtils::GetValueProducingAttributes(shaderOutputsOnly=true)`
//! and `UsdShadeMaterial::ComputeSurfaceSource`: node graphs pass connections
//! through; outputs on non-container prims terminate them. This module neither
//! evaluates shaders nor validates whether a renderer supports them. Container
//! behavior covers the built-in `UsdShade` and `UsdLux` policies; custom OpenUSD
//! connectability plugins are not loaded.

/// Typed views and authoring handles for OpenUSD's standard shader nodes.
/// Node defaults are separate from authored USD attribute values.
pub mod nodes {
    pub use crate::generated::shader_nodes::*;
}

mod behavior;
pub use behavior::{ConnectableBehavior, ConnectionError, ConnectionIssue};
mod ports;
pub use ports::{Port, PortEdit, PortError, PortKind};

use crate::{PrimView, Scene, usd_shade::Material};
use alloc::{format, string::String, vec::Vec};
use layerstack::{PathId, PropertyKind, PropertyPath, TargetPath};

/// Direct valid connection endpoints, in composed list order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectedSources {
    /// Existing attributes named `inputs:*` or `outputs:*`. The owning prim
    /// need not implement `ConnectableAPI`, matching OpenUSD.
    pub sources: Vec<PropertyPath>,
    /// Targets which are missing, are not attributes, or lack a shading prefix.
    pub invalid: Vec<TargetPath>,
}

/// A property read, including one whose name is not interned or authored yet.
///
/// Invalidate a retained result when this property or its owning prim changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShadingDependency {
    /// Owning prim; its type determines whether it is a container.
    pub prim: PathId,
    /// Full USD property name, including its namespace.
    pub property: String,
}

/// A recoverable problem encountered along a connection branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShadingIssue {
    /// A direct target does not name an existing shading attribute.
    InvalidTarget {
        /// Attribute holding the connection.
        from: PropertyPath,
        /// Invalid composed target.
        target: TargetPath,
    },
    /// A branch revisits an attribute already on that branch.
    Cycle(PropertyPath),
    /// A connection reaches an input on a non-container prim.
    NonContainerInput(PropertyPath),
}

/// A terminal endpoint with the branch that led to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderSource {
    /// An output on a non-container prim. Check the prim's schema if a
    /// particular shader schema is required by the consumer.
    pub output: PropertyPath,
    /// Requested attribute first, terminal output last.
    pub chain: Vec<PropertyPath>,
}

/// Ordered terminal candidates and the evidence required to inspect them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShaderSources {
    /// Depth-first connection order, retaining duplicates reached by different
    /// branches, as OpenUSD does. Multiple sources are valid, not an error.
    pub sources: Vec<ShaderSource>,
    /// Recoverable branch failures; another branch may still succeed.
    pub issues: Vec<ShadingIssue>,
    /// Every property inspected, including invalid or missing targets.
    pub dependencies: Vec<ShadingDependency>,
}

/// How a value-producing attribute supplies a shading input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueSourceKind {
    /// An output on a non-container prim. Check its prim schema before
    /// treating it as an executable shader result.
    ShaderOutput,
    /// An authored value on an input or output. Read it at the desired time
    /// through the stage's normal value resolver; it may be animated.
    AuthoredValue,
}

/// A value-producing attribute and the connection branch leading to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueSource {
    /// The contributing input or output attribute.
    pub attribute: PropertyPath,
    /// Whether to read an authored value or evaluate a shader output.
    pub kind: ValueSourceKind,
    /// Requested attribute first, contributing attribute last.
    pub chain: Vec<PropertyPath>,
}

impl ValueSource {
    /// Reads this authored provider as a primvar name, accepting `string`
    /// and legacy `token` values while retaining value provenance.
    ///
    /// Call `Scene::value_sources` on a primvar reader's `inputs:varname`
    /// first to follow node graph interfaces. Shader outputs require shader
    /// evaluation and return `None` here, as do blocks and other value types.
    /// The strongest authored default is read; an incompatible opinion does
    /// not retry weaker values. No node default is substituted. OpenUSD's imaging primvar-name
    /// consumer accepts both encodings (`_GetPrimvarNameAttributeValue`);
    /// the current Sdr declaration and strict validation remain `string`.
    #[must_use]
    pub fn primvar_name(
        &self,
        scene: &Scene<'_>,
    ) -> Option<layerstack::Resolved<alloc::sync::Arc<str>>> {
        if self.kind != ValueSourceKind::AuthoredValue {
            return None;
        }
        let resolved = scene.stage().resolve_field_path(self.attribute)?;
        let value = crate::value::read_primvar_name(&resolved.value, scene.store().tokens())?;
        Some(layerstack::Resolved {
            value,
            provenance: resolved.provenance,
        })
    }
}

/// Ordered value providers, branch diagnostics and inspected properties.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValueSources {
    /// Depth-first connection order, retaining duplicate providers reached
    /// through different branches. No value is evaluated by this query.
    pub sources: Vec<ValueSource>,
    /// Recoverable failures; successful sibling branches are retained.
    pub issues: Vec<ShadingIssue>,
    /// Every inspected property, including failed targets and fallback values.
    pub dependencies: Vec<ShadingDependency>,
}

/// A material output terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialTerminal {
    /// Surface shading.
    Surface,
    /// Displacement shading.
    Displacement,
    /// Volume shading.
    Volume,
}

impl MaterialTerminal {
    fn name(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Displacement => "displacement",
            Self::Volume => "volume",
        }
    }
}

/// Material terminal resolution across ordered render contexts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterialSource {
    /// Prim of the first candidate if it is a Shader, matching OpenUSD's
    /// typed return. A non-Shader first candidate does not trigger fallback.
    pub shader: Option<PathId>,
    /// Context that supplied the candidates; empty means universal. `None`
    /// means no context produced a terminal.
    pub context: Option<String>,
    /// Candidates from the winning context, plus diagnostics and dependencies
    /// from every context attempted before it.
    pub trace: ShaderSources,
}

impl MaterialSource {
    /// The first terminal endpoint selected by OpenUSD's traversal.
    /// `shader` separately reports whether its prim is a typed Shader.
    /// Inspect `trace.sources` to detect multiple valid terminals.
    #[must_use]
    pub fn selected(&self) -> Option<&ShaderSource> {
        self.trace.sources.first()
    }
}

// Build either public result directly: shader-only queries need no intermediate
// value-source array or conversion allocation.
struct SourceTrace<T> {
    sources: Vec<T>,
    issues: Vec<ShadingIssue>,
    dependencies: Vec<ShadingDependency>,
}

impl Scene<'_> {
    fn shading_attribute(&self, path: PropertyPath) -> bool {
        if !self.stage().has_prim(path.prim_path()) {
            return false;
        }
        self.stage()
            .resolve_property_declaration(path.prim_path(), path.property())
            .map(|d| d.kind)
            .or_else(|| {
                self.stage()
                    .property_definition(path.prim_path(), path.property(), self.store())
                    .map(|d| d.kind)
            })
            == Some(PropertyKind::Attribute)
    }

    /// Reads direct composed connections, retaining invalid targets separately.
    /// Does not follow node graphs or reject multiple valid connections.
    #[must_use]
    pub fn connected_sources(&self, attribute: PropertyPath) -> ConnectedSources {
        let mut result = ConnectedSources::default();
        if !self.shading_attribute(attribute) {
            return result;
        }
        if let Some(targets) = self.stage().resolve_target_list_path(attribute) {
            for target in targets.value {
                if let TargetPath::Property(path) = target
                    && self.shading_attribute(path)
                    && shading_kind(self.store().tokens().resolve(path.property())).is_some()
                {
                    result.sources.push(path);
                } else {
                    result.invalid.push(target);
                }
            }
        }
        result
    }

    /// Traces connections to non-container outputs through built-in
    /// containers (`NodeGraphs`, Materials, lights and light filters). Constants are intentionally excluded.
    /// Traversal uses an explicit stack and branch-local cycle detection.
    #[must_use]
    pub fn shader_sources(&self, attribute: PropertyPath) -> ShaderSources {
        let result = self.trace_sources(attribute, false, |output, _, chain| ShaderSource {
            output,
            chain,
        });
        ShaderSources {
            sources: result.sources,
            issues: result.issues,
            dependencies: result.dependencies,
        }
    }

    /// Finds shader outputs or authored values supplying a shading attribute.
    ///
    /// OpenUSD: `UsdShadeUtils::GetValueProducingAttributes` with
    /// `shaderOutputsOnly=false`. Node graphs pass connections through. If no
    /// connection yields a provider, the attribute's own authored value is used.
    /// A default block is not an authored value; time samples and splines are.
    /// This selects attributes, not values at a time (AOUSD Core §12.3–12.4).
    /// Cycles terminate only their branch; order and duplicate paths through
    /// separate branches are preserved. Custom connectability plugins are not
    /// loaded, as for [`Self::shader_sources`].
    #[must_use]
    pub fn value_sources(&self, attribute: PropertyPath) -> ValueSources {
        let result = self.trace_sources(attribute, true, |attribute, kind, chain| ValueSource {
            attribute,
            kind,
            chain,
        });
        ValueSources {
            sources: result.sources,
            issues: result.issues,
            dependencies: result.dependencies,
        }
    }

    fn trace_sources<T>(
        &self,
        attribute: PropertyPath,
        authored_values: bool,
        make_source: impl Fn(PropertyPath, ValueSourceKind, Vec<PropertyPath>) -> T,
    ) -> SourceTrace<T> {
        enum Work {
            Visit(PropertyPath, bool),
            Finish(PropertyPath, usize),
        }
        let mut result = SourceTrace {
            sources: Vec::new(),
            issues: Vec::new(),
            dependencies: Vec::new(),
        };
        let mut chain = Vec::new();
        let mut stack = alloc::vec![Work::Visit(attribute, true)];
        while let Some(work) = stack.pop() {
            let (path, initial) = match work {
                Work::Visit(path, initial) => (path, initial),
                Work::Finish(path, before) => {
                    // Postorder is essential: an invalid/cyclic connection may
                    // still fall back to this attribute, while one successful
                    // branch suppresses the fallback for the whole attribute.
                    if authored_values
                        && result.sources.len() == before
                        && PrimView::new(*self, path.prim_path())
                            .has_authored_value(self.store().tokens().resolve(path.property()))
                    {
                        result.sources.push(make_source(
                            path,
                            ValueSourceKind::AuthoredValue,
                            chain.clone(),
                        ));
                    }
                    chain.pop();
                    continue;
                }
            };
            result.dependencies.push(ShadingDependency {
                prim: path.prim_path(),
                property: self.store().tokens().resolve(path.property()).into(),
            });
            let kind = shading_kind(self.store().tokens().resolve(path.property()));
            if !self.shading_attribute(path) || kind.is_none() {
                continue;
            }
            if !initial
                && !self
                    .connectable_behavior(path.prim_path())
                    .is_some_and(|b| b.is_container)
            {
                if kind == Some(false) {
                    let mut branch = chain.clone();
                    branch.push(path);
                    result
                        .sources
                        .push(make_source(path, ValueSourceKind::ShaderOutput, branch));
                } else {
                    result.issues.push(ShadingIssue::NonContainerInput(path));
                }
                continue;
            }
            if chain.contains(&path) {
                result.issues.push(ShadingIssue::Cycle(path));
                continue;
            }
            let connected = self.connected_sources(path);
            for target in connected.invalid {
                if let TargetPath::Property(p) = target {
                    result.dependencies.push(ShadingDependency {
                        prim: p.prim_path(),
                        property: self.store().tokens().resolve(p.property()).into(),
                    });
                }
                result
                    .issues
                    .push(ShadingIssue::InvalidTarget { from: path, target });
            }
            chain.push(path);
            stack.push(Work::Finish(path, result.sources.len()));
            stack.extend(
                connected
                    .sources
                    .into_iter()
                    .rev()
                    .map(|p| Work::Visit(p, false)),
            );
        }
        result
    }
}

// true = input, false = output. An empty base name is accepted by OpenUSD.
fn shading_kind(name: &str) -> Option<bool> {
    if name.starts_with("inputs:") {
        Some(true)
    } else if name.starts_with("outputs:") {
        Some(false)
    } else {
        None
    }
}

impl Material<'_> {
    /// Resolves the surface output using ordered render contexts, then the
    /// universal context if it was not already requested.
    #[must_use]
    pub fn compute_surface_source(&self, contexts: &[&str]) -> MaterialSource {
        self.compute_terminal_source(MaterialTerminal::Surface, contexts)
    }

    /// Resolves a material terminal. A disconnected, invalid or cyclic context
    /// permits the next context to be tried. All attempted output names are
    /// dependencies, including absent names that are not yet interned. An
    /// explicitly requested universal context with no authored output stops
    /// the search, matching OpenUSD even when later contexts were requested.
    #[must_use]
    pub fn compute_terminal_source(
        &self,
        terminal: MaterialTerminal,
        contexts: &[&str],
    ) -> MaterialSource {
        let mut result = MaterialSource::default();
        for context in contexts
            .iter()
            .copied()
            .chain((!contexts.contains(&"")).then_some(""))
        {
            let name = if context.is_empty() {
                format!("outputs:{}", terminal.name())
            } else {
                format!("outputs:{context}:{}", terminal.name())
            };
            result.trace.dependencies.push(ShadingDependency {
                prim: self.path(),
                property: name.clone(),
            });
            let Some(path) = PrimView::new(self.scene(), self.path()).property_path(&name) else {
                continue;
            };
            // OpenUSD Material::_ComputeNamedOutputSources stops on an
            // explicitly requested universal output that is only a schema
            // fallback. It does not advance to a later requested context.
            if context.is_empty()
                && self
                    .scene()
                    .stage()
                    .resolve_property_declaration(path.prim_path(), path.property())
                    .is_none()
            {
                break;
            }
            let mut trace = self.scene().shader_sources(path);
            result.trace.issues.append(&mut trace.issues);
            result.trace.dependencies.append(&mut trace.dependencies);
            if !trace.sources.is_empty() {
                result.shader = trace
                    .sources
                    .first()
                    .map(|s| s.output.prim_path())
                    .filter(|p| self.scene().is_a(*p, "Shader"));
                result.context = Some(context.into());
                result.trace.sources = trace.sources;
                break;
            }
        }
        result
    }
}
