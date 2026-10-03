// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Owned lighting inputs for engine evaluation and GPU upload preparation.
use super::ShaderIdSelection;
use crate::{
    PrimView, Scene, Time, XformCache,
    imageable::Visibility,
    shading::{PortKind, ShadingDependency, ValueSourceKind, ValueSources},
    usd_geom::Imageable,
    usd_lux::LightApi,
};
use alloc::{
    collections::BTreeSet,
    string::{String, ToString},
    vec::Vec,
};
use layerstack::{
    PathId, PropertyPath, PropertyType, Provenance, ResolvedValue, TargetPath, Value,
};

/// Classification of an emitter, independent of any rendering backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightKind {
    /// Local spherical emitter; `treatAsPoint` is captured as a non-port attribute.
    Sphere,
    /// Local disk in the XY plane, facing -Z.
    Disk,
    /// Local rectangle in the XY plane, facing -Z.
    Rectangle,
    /// Local cylinder along X; `treatAsLine` is captured as a non-port attribute.
    Cylinder,
    /// Infinitely distant directional emitter.
    Distant,
    /// Legacy environment dome, with orientation in its transform.
    Dome,
    /// Environment dome with an explicit pole-axis input.
    Dome1,
    /// Portal rectangle used by an environment light.
    Portal,
    /// Geometry referenced by `geometry`.
    Geometry,
    /// Mesh carrying `MeshLightAPI`.
    Mesh,
    /// Volume carrying `VolumeLightAPI`.
    Volume,
    /// Plugin or other prim carrying `LightAPI`; inspect type and shader ID.
    Custom,
}
impl LightKind {
    fn read(scene: &Scene<'_>, path: PathId) -> Self {
        for (name, kind) in [
            ("SphereLight", Self::Sphere),
            ("DiskLight", Self::Disk),
            ("RectLight", Self::Rectangle),
            ("CylinderLight", Self::Cylinder),
            ("DistantLight", Self::Distant),
            ("DomeLight_1", Self::Dome1),
            ("DomeLight", Self::Dome),
            ("PortalLight", Self::Portal),
            ("GeometryLight", Self::Geometry),
        ] {
            if scene.is_a(path, name) {
                return kind;
            }
        }
        if scene.has_api(path, "MeshLightAPI", None) {
            Self::Mesh
        } else if scene.has_api(path, "VolumeLightAPI", None) {
            Self::Volume
        } else {
            Self::Custom
        }
    }
}
/// Whether a captured parameter can be consumed as a constant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightInputStatus {
    /// One readable constant, or an unconnected schema fallback.
    Constant,
    /// At least one provider is a shader output; engine execution is required.
    ShaderRequired,
    /// Multiple providers require an engine policy; no arbitrary winner is chosen.
    MultipleProviders,
    /// Provider tracing encountered invalid targets or a cycle. Evidence is retained.
    InvalidConnections,
    /// No readable value or provider exists (including an authored value block).
    Unavailable,
}
/// Owned value with source-layer evidence for relative assets and diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct LightValue {
    /// Resolved USD value. Token identities belong to the originating store.
    pub value: Option<Value>,
    /// Resolved token, string or asset spelling; usable without the token interner.
    /// Asset spelling is not a resolved file path.
    pub text: Option<String>,
    /// Winning authored source, when stage provenance is enabled. Schema fallbacks
    /// have no source layer. Relative asset anchoring requires this evidence.
    pub provenance: Option<Provenance>,
}
fn default_is_blocked(scene: &Scene<'_>, path: PropertyPath) -> bool {
    scene
        .stage()
        .explain_property_path(path)
        .and_then(|opinions| {
            opinions
                .iter()
                .filter_map(|o| o.value.default_value())
                .next()
        })
        == Some(&Value::Blocked)
}
fn capture_value(scene: &Scene<'_>, path: PropertyPath, time: Time) -> LightValue {
    // At default time a strongest authored block suppresses Get() even when
    // the raw schema-aware resolver exposes its fallback. Numeric-time fallback
    // behavior is delegated to the stage. OpenUSD UsdAttribute::Get;
    // AOUSD Core §12.3.6 (value blocks).
    let default_blocked = time == Time::Default && default_is_blocked(scene, path);
    let resolved = if default_blocked {
        None
    } else {
        match time {
            Time::Default => scene
                .stage()
                .resolve_value_with_schema(path.prim_path(), path.property(), scene.store())
                .and_then(|r| match r.value {
                    ResolvedValue::Scalar(v) => Some((v, r.provenance)),
                    _ => None,
                }),
            Time::At {
                code,
                interpolation,
            } => scene
                .stage()
                .resolve_value_at_time_with_schema(
                    path.prim_path(),
                    path.property(),
                    code,
                    interpolation,
                    scene.store(),
                )
                .map(|r| (r.value, r.provenance)),
        }
    };
    let (value, provenance) = resolved.map_or((None, None), |(v, p)| (Some(v), p));
    let text = match &value {
        Some(Value::Token(t)) => Some(scene.store().tokens().resolve(*t).into()),
        Some(Value::String(s) | Value::Asset(s)) => Some(s.to_string()),
        _ => None,
    };
    LightValue {
        value,
        text,
        provenance,
    }
}
/// Captured shading input with constant values kept separate from shader execution.
#[derive(Clone, Debug, PartialEq)]
pub struct LightInput {
    /// Full property identity in the originating store.
    pub property: PropertyPath,
    /// Base name after `inputs:`, including any nested namespace.
    pub name: String,
    /// Composed declared value type.
    pub property_type: Option<PropertyType>,
    /// This input's own value, before following connections.
    pub own: LightValue,
    /// Providers, connection chains, failures and dependencies.
    pub providers: ValueSources,
    /// Value per provider, in the same order. Shader outputs are not read as constants.
    pub provider_values: Vec<LightValue>,
    /// Machine-readable readiness for constant consumption.
    pub status: LightInputStatus,
}
impl LightInput {
    /// Captures any existing shading input, including custom light-filter ports.
    /// Returns None for missing attributes or outputs; no shader execution occurs.
    #[must_use]
    pub fn read(scene: &Scene<'_>, property: PropertyPath, time: Time) -> Option<Self> {
        let port = crate::shading::Port::get(scene, property)?;
        if port.kind() != PortKind::Input {
            return None;
        }
        let providers = port.value_sources();
        let own = capture_value(scene, port.path(), time);
        let provider_values: Vec<_> = providers
            .sources
            .iter()
            .map(|p| {
                if p.kind == ValueSourceKind::AuthoredValue {
                    capture_value(scene, p.attribute, time)
                } else {
                    LightValue {
                        value: None,
                        text: None,
                        provenance: None,
                    }
                }
            })
            .collect();
        let connected = port.connected_sources();
        let status = if !providers.issues.is_empty() {
            LightInputStatus::InvalidConnections
        } else if providers
            .sources
            .iter()
            .any(|p| p.kind == ValueSourceKind::ShaderOutput)
        {
            LightInputStatus::ShaderRequired
        } else if providers.sources.len() > 1 {
            LightInputStatus::MultipleProviders
        } else if providers.sources.is_empty()
            && (!connected.sources.is_empty() || !connected.invalid.is_empty())
        {
            LightInputStatus::Unavailable
        } else if provider_values.first().unwrap_or(&own).value.is_some() {
            LightInputStatus::Constant
        } else {
            LightInputStatus::Unavailable
        };
        Some(Self {
            property,
            name: port.name().into(),
            property_type: port.property_type(),
            own,
            providers,
            provider_values,
            status,
        })
    }
    /// A usable constant. Failed/multiple/shader-driven branches never silently
    /// fall back to `own`; callers can inspect it to choose an explicit policy.
    #[must_use]
    pub fn constant(&self) -> Option<&LightValue> {
        (self.status == LightInputStatus::Constant)
            .then(|| self.provider_values.first().unwrap_or(&self.own))
    }
}
/// Common constant photometric inputs, without renderer-specific unit conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightPhotometry {
    /// USD color in the scene's working color space; temperature is not applied.
    pub color: [f32; 3],
    /// Authored intensity. Interpretation depends on light kind and normalization.
    pub intensity: f32,
    /// Exposure in stops; intensity multiplier is `2^exposure`.
    pub exposure: f32,
    /// Preserve total power rather than emitting-surface radiance when true.
    pub normalize: bool,
    /// Enabled color temperature in kelvin; engine color management remains explicit.
    pub temperature: Option<f32>,
    /// Diffuse contribution multiplier.
    pub diffuse: f32,
    /// Specular contribution multiplier.
    pub specular: f32,
}
/// Constant local shape parameters. Matrices and scene units are supplied separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LightShape {
    /// Sphere radius and point approximation flag.
    Sphere {
        /// Radius in stage length units.
        radius: f32,
        /// Whether to approximate a point source.
        treat_as_point: bool,
    },
    /// Disk radius in stage length units.
    Disk {
        /// Radius.
        radius: f32,
    },
    /// Rectangle dimensions in stage length units, also used by portals.
    Rectangle {
        /// Width along X.
        width: f32,
        /// Height along Y.
        height: f32,
    },
    /// Cylinder along X.
    Cylinder {
        /// Radius in stage length units.
        radius: f32,
        /// Length in stage length units.
        length: f32,
        /// Whether to approximate a line source.
        treat_as_line: bool,
    },
    /// Angular diameter of a distant light.
    Distant {
        /// Full angular diameter in degrees.
        angle: f32,
    },
    /// Shape requires environment, geometry, volume or custom engine interpretation.
    Other(LightKind),
}
/// A typed parameter could not be extracted; inspect its capture for recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightParameterError {
    /// Emitter path.
    pub light: PathId,
    /// Input base name.
    pub input: String,
    /// Capture status, or Unavailable if the input is absent.
    pub status: LightInputStatus,
    /// True for a constant with the wrong type or nonfinite numeric data.
    pub invalid_value: bool,
}
impl core::fmt::Display for LightParameterError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "light {:?} input {}: {:?}, invalid value {}",
            self.light, self.input, self.status, self.invalid_value
        )
    }
}
impl core::error::Error for LightParameterError {}
/// Failure to capture an emitter. Parameter failures are retained within a successful capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LightCaptureError {
    /// The prim no longer exists.
    MissingPrim(PathId),
    /// The prim does not carry `LightAPI`. Filters are discovered separately.
    NotLight(PathId),
}
impl core::fmt::Display for LightCaptureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "light capture failed: {self:?}")
    }
}
impl core::error::Error for LightCaptureError {}
/// A relationship branch that requires an engine's recovery policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightRelationshipIssue {
    /// Full source relationship name.
    pub relationship: PropertyPath,
    /// Invalid target or relationship revisited by forwarding.
    pub target: TargetPath,
    /// True when forwarding encountered a cycle; false for absent/unsupported targets.
    pub cycle: bool,
}
/// Owned, time-specific emitter inputs for CPU evaluation or an engine's GPU adapter.
///
/// No shader is executed, texture loaded, IES profile decoded, or normalization
/// baked into radiance. GPU layouts, precision conversion, resource handles and
/// sampling belong to the engine. IDs follow namespace paths within one store,
/// including deletion/recreation; they are not durable object/device identities.
/// AOUSD Core §12.3–12.4 (values/connections), §13.3 (schema fallbacks);
/// OpenUSD `UsdLux` light descriptions and `UsdShade` value-producing attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct LightInputs {
    /// Emitter identity in the originating store.
    pub light: PathId,
    /// Requested sample time and interpolation.
    pub time: Time,
    /// Concrete schema spelling, even for Custom emitters.
    pub type_name: String,
    /// Built-in emitter classification.
    pub kind: LightKind,
    /// Selected default-time renderer shader ID and fallback evidence.
    pub shader: ShaderIdSelection,
    /// Local-to-world row-vector matrix: translation in the last row.
    pub world_transform: [[f64; 4]; 4],
    /// Inherited USD visibility. This does not decide renderer purpose filtering.
    pub visibility: Visibility,
    /// Stage length units in meters, with the USD fallback 0.01.
    pub meters_per_unit: f64,
    /// Stage up axis, with USD fallback Y. Dome poleAxis remains distinct.
    pub up_axis: String,
    /// Authored scene color space, when specified. No conversion is performed.
    pub color_space: Option<String>,
    /// All composed inputs, including custom, shaping and shadow ports, sorted by name.
    pub inputs: Vec<LightInput>,
    /// Non-port light fields such as poleAxis and materialSyncMode.
    pub attributes: Vec<(String, LightValue)>,
    /// Composed forwarded filters, geometry and portal targets. Missing targets are
    /// retained so an engine can diagnose unsupported/incomplete scenes.
    pub relationships: Vec<(String, Vec<TargetPath>)>,
    /// Property evidence for input/provider/relationship/shader selection invalidation.
    pub dependencies: Vec<ShadingDependency>,
    /// Ancestors consulted for transforms and inherited visibility, emitter first.
    pub ancestors: Vec<PathId>,
    /// Transform problems on consulted ancestors; USD fallback matrices remain visible.
    pub transform_problems: Vec<(PathId, Vec<crate::xform::XformProblem>)>,
    /// Invalid or cyclic relationship branches, localized to their property.
    pub relationship_issues: Vec<LightRelationshipIssue>,
    /// Authored default blocks can differ between default and numeric time.
    /// Numeric-to-numeric changes need no recapture solely for this flag.
    pub default_time_sensitive: bool,
    /// Any consulted input may vary at numeric time; used by caller-owned retention.
    pub might_vary: bool,
}
impl LightInputs {
    /// Captures one light using only public scene state. Filters are not emitters.
    pub fn read(
        scene: &Scene<'_>,
        light: PathId,
        time: Time,
        contexts: &[&str],
    ) -> Result<Self, LightCaptureError> {
        Self::read_with_transforms(scene, light, time, contexts, &mut XformCache::new(time))
    }
    pub(super) fn read_with_transforms(
        scene: &Scene<'_>,
        light: PathId,
        time: Time,
        contexts: &[&str],
        transforms: &mut XformCache,
    ) -> Result<Self, LightCaptureError> {
        if !scene.stage().has_prim(light) {
            return Err(LightCaptureError::MissingPrim(light));
        }
        let Some(api) = LightApi::get(scene, light) else {
            return Err(LightCaptureError::NotLight(light));
        };
        let prim = PrimView::new(*scene, light);
        let shader = api.select_shader_id(contexts);
        let mut dependencies: Vec<_> = shader
            .consulted_properties
            .iter()
            .map(|p| ShadingDependency {
                prim: light,
                property: p.clone(),
            })
            .collect();
        let mut might_vary = false;
        let mut default_time_sensitive = false;
        let mut inputs = Vec::new();
        for port in prim.ports(PortKind::Input) {
            let input =
                LightInput::read(scene, port.path(), time).expect("existing immutable input");
            let property = scene.store().tokens().resolve(port.path().property());
            might_vary |= prim.property_might_vary(property);
            default_time_sensitive |= default_is_blocked(scene, port.path());
            for dep in &input.providers.dependencies {
                let upstream = PrimView::new(*scene, dep.prim);
                might_vary |= upstream.property_might_vary(&dep.property);
                default_time_sensitive |= upstream
                    .property_path(&dep.property)
                    .is_some_and(|p| default_is_blocked(scene, p));
            }
            dependencies.extend(input.providers.dependencies.iter().cloned());
            dependencies.push(ShadingDependency {
                prim: light,
                property: property.into(),
            });
            inputs.push(input);
        }
        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        let mut attributes = Vec::new();
        for name in [
            "poleAxis",
            "light:materialSyncMode",
            "treatAsPoint",
            "treatAsLine",
        ] {
            dependencies.push(ShadingDependency {
                prim: light,
                property: name.into(),
            });
            if let Some(path) = prim.property_path(name) {
                attributes.push((name.into(), capture_value(scene, path, time)));
            }
            might_vary |= prim.property_might_vary(name);
            default_time_sensitive |= prim
                .property_path(name)
                .is_some_and(|p| default_is_blocked(scene, p));
        }
        let mut relationships = Vec::new();
        let mut relationship_issues = Vec::new();
        for name in ["light:filters", "geometry", "portals"] {
            dependencies.push(ShadingDependency {
                prim: light,
                property: name.into(),
            });
            let mut targets = Vec::new();
            if let Some(property) = prim.property_path(name) {
                relationship_dependencies(
                    scene,
                    property,
                    &mut dependencies,
                    &mut relationship_issues,
                );
                targets = crate::view::forwarded_targets(scene, property);
                for &target in &targets {
                    let valid = if let TargetPath::Prim(path) = target {
                        scene.stage().has_prim(path)
                            && (name != "light:filters" || scene.is_a(path, "LightFilter"))
                    } else {
                        false
                    };
                    if !valid {
                        relationship_issues.push(LightRelationshipIssue {
                            relationship: property,
                            target,
                            cycle: false,
                        });
                    }
                }
            }
            relationships.push((name.into(), targets));
        }
        let mut ancestors = Vec::new();
        let mut transform_problems = Vec::new();
        let mut at = Some(light);
        while let Some(path) = at {
            ancestors.push(path);
            if let Some(local) = transforms.local_transform(scene, path)
                && !local.problems.is_empty()
            {
                transform_problems.push((path, local.problems.clone()));
            }
            let view = PrimView::new(*scene, path);
            for name in scene.stage().property_names(path, scene.store()) {
                let name = scene.store().tokens().resolve(name);
                if name.starts_with("xformOp:") || name == "visibility" {
                    might_vary |= view.property_might_vary(name);
                }
            }
            at = scene.parent(path);
        }
        if let Some(&root) = ancestors.last() {
            for name in ["metersPerUnit", "upAxis", "colorSpace"] {
                dependencies.push(ShadingDependency {
                    prim: root,
                    property: name.into(),
                });
            }
        }
        dependencies.sort_by(|a, b| (a.prim, &a.property).cmp(&(b.prim, &b.property)));
        dependencies.dedup();
        let type_name = scene
            .stage()
            .resolve_type_name(light, scene.store())
            .map_or(String::new(), |t| scene.store().tokens().resolve(t).into());
        Ok(Self {
            light,
            time,
            type_name,
            kind: LightKind::read(scene, light),
            shader,
            world_transform: transforms
                .local_to_world(scene, light)
                .unwrap_or(crate::gf::IDENTITY),
            visibility: Imageable::new(scene, light)
                .map_or(Visibility::Inherited, |v| v.compute_visibility(time)),
            meters_per_unit: scene.metadata().meters_per_unit().unwrap_or(0.01),
            up_axis: scene.metadata().up_axis().unwrap_or("Y").into(),
            color_space: scene.metadata().value("colorSpace").and_then(|v| match v {
                Value::Token(t) => Some(scene.store().tokens().resolve(t).into()),
                Value::String(s) => Some(s.to_string()),
                _ => None,
            }),
            inputs,
            attributes,
            relationships,
            dependencies,
            ancestors,
            transform_problems,
            relationship_issues,
            default_time_sensitive,
            might_vary,
        })
    }
    /// Named composed input, including custom engine parameters.
    #[must_use]
    pub fn input(&self, name: &str) -> Option<&LightInput> {
        self.inputs.iter().find(|p| p.name == name)
    }
    fn parameter<T>(
        &self,
        name: &str,
        read: impl FnOnce(&Value) -> Option<T>,
    ) -> Result<T, LightParameterError> {
        let input = self.input(name);
        let status = input.map_or(LightInputStatus::Unavailable, |i| i.status);
        let attribute = self
            .attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v);
        let status = if input.is_none() && attribute.is_some_and(|v| v.value.is_some()) {
            LightInputStatus::Constant
        } else {
            status
        };
        let value = if input.is_some() {
            input
                .and_then(LightInput::constant)
                .and_then(|v| v.value.as_ref())
        } else {
            attribute.and_then(|v| v.value.as_ref())
        };
        value.and_then(read).ok_or_else(|| LightParameterError {
            light: self.light,
            input: name.into(),
            status,
            invalid_value: value.is_some(),
        })
    }
    fn attribute_boolean(&self, name: &str) -> Result<bool, LightParameterError> {
        let value = self
            .attributes
            .iter()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.value.as_ref());
        match value {
            Some(Value::Bool(b)) => Ok(*b),
            _ => Err(LightParameterError {
                light: self.light,
                input: name.into(),
                status: if value.is_some() {
                    LightInputStatus::Constant
                } else {
                    LightInputStatus::Unavailable
                },
                invalid_value: value.is_some(),
            }),
        }
    }
    /// Extracts a finite scalar constant; shader-driven or malformed values are errors.
    pub fn float(&self, name: &str) -> Result<f32, LightParameterError> {
        self.parameter(name, |v| match v {
            Value::Float(f) if f.is_finite() => Some(*f),
            _ => None,
        })
    }
    /// Extracts a boolean constant without guessing a fallback.
    pub fn boolean(&self, name: &str) -> Result<bool, LightParameterError> {
        self.parameter(name, |v| match v {
            Value::Bool(b) => Some(*b),
            _ => None,
        })
    }
    /// Extracts common constants. Connected shaders remain explicit errors.
    pub fn photometry(&self) -> Result<LightPhotometry, LightParameterError> {
        Ok(LightPhotometry {
            color: self.parameter("color", |v| match v {
                Value::Vec3f(c) if c.iter().all(|v| v.is_finite()) => Some(*c),
                _ => None,
            })?,
            intensity: self.float("intensity")?,
            exposure: self.float("exposure")?,
            normalize: self.boolean("normalize")?,
            temperature: if self.boolean("enableColorTemperature")? {
                Some(self.float("colorTemperature")?)
            } else {
                None
            },
            diffuse: self.float("diffuse")?,
            specular: self.float("specular")?,
        })
    }
    /// Extracts finite built-in dimensions; other kinds stay explicit.
    pub fn shape(&self) -> Result<LightShape, LightParameterError> {
        Ok(match self.kind {
            LightKind::Sphere => LightShape::Sphere {
                radius: self.float("radius")?,
                treat_as_point: self.attribute_boolean("treatAsPoint")?,
            },
            LightKind::Disk => LightShape::Disk {
                radius: self.float("radius")?,
            },
            LightKind::Rectangle | LightKind::Portal => LightShape::Rectangle {
                width: self.float("width")?,
                height: self.float("height")?,
            },
            LightKind::Cylinder => LightShape::Cylinder {
                radius: self.float("radius")?,
                length: self.float("length")?,
                treat_as_line: self.attribute_boolean("treatAsLine")?,
            },
            LightKind::Distant => LightShape::Distant {
                angle: self.float("angle")?,
            },
            kind => LightShape::Other(kind),
        })
    }
}
fn relationship_dependencies(
    scene: &Scene<'_>,
    path: PropertyPath,
    deps: &mut Vec<ShadingDependency>,
    issues: &mut Vec<LightRelationshipIssue>,
) {
    enum Work {
        Visit(PropertyPath),
        Finish(PropertyPath),
    }
    let mut stack = alloc::vec![Work::Visit(path)];
    let mut active = BTreeSet::new();
    let mut completed = BTreeSet::new();
    while let Some(work) = stack.pop() {
        let path = match work {
            Work::Visit(p) => p,
            Work::Finish(p) => {
                active.remove(&p);
                completed.insert(p);
                continue;
            }
        };
        if active.contains(&path) {
            issues.push(LightRelationshipIssue {
                relationship: path,
                target: TargetPath::Property(path),
                cycle: true,
            });
            continue;
        }
        // Shared forwarding subgraphs are expanded once. Active ancestry still
        // detects back edges, without mistaking completed subgraphs for cycles.
        if completed.contains(&path) {
            continue;
        }
        deps.push(ShadingDependency {
            prim: path.prim_path(),
            property: scene.store().tokens().resolve(path.property()).into(),
        });
        active.insert(path);
        stack.push(Work::Finish(path));
        if let Some(targets) = scene.stage().resolve_target_list_path(path) {
            stack.extend(targets.value.into_iter().rev().filter_map(|target| {
                if let TargetPath::Property(p) = target {
                    Some(Work::Visit(p))
                } else {
                    None
                }
            }));
        }
    }
}
