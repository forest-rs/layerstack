// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Renderer-ready `UsdLux` lights: the inputs a renderer reads, and the
//! emission, world placement and size they come to.
//!
//! The quantities follow the `UsdLux` schema documentation of OpenUSD 26.11
//! (`pxr/usd/usdLux/schema.usda`, `LightAPI`'s "Quantities and Units",
//! `inputs:intensity`, `inputs:exposure`, `inputs:color`,
//! `inputs:colorTemperature` and `inputs:normalize`):
//!
//! - **Emission** is `color * blackbody(colorTemperature) * intensity *
//!   2^exposure / sizeFactor`: luminance in nits (cd/m²) for a light seen
//!   directly, in the rendering color space. The blackbody color is
//!   `UsdLuxBlackbodyTemperatureAsRgb` (D65 white at 6500 K, unit
//!   luminance), applied only with `enableColorTemperature`.
//! - **`sizeFactor`** is 1 unless `normalize` is on. With it, an area
//!   light's is its world-space surface area, scale and shear of its
//!   transform included; a distant light's comes from its `angle`; a dome
//!   or portal light ignores `normalize`.
//!
//! `UsdLux` leaves the world-space area of a sphere or a cylinder under a
//! non-uniform transform to the renderer; the rule this module applies is
//! stated on [`LightEmission::size_factor`].

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cell::RefCell;

use layerstack::{PathId, TargetPath, Time, TokenInterner, Value};
use layerstack_schemas::light::blackbody_temperature_rgb;
use layerstack_schemas::shading::ValueSourceKind;
use layerstack_schemas::value::{read_asset, read_bool, read_float, read_float3, read_token};
use layerstack_schemas::{PrimView, Scene};

/// The built-in `UsdLux` light a [`LightItem`] is, by its typed schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LightKind {
    /// `DistantLight`: parallel light along the light's -Z axis.
    Distant,
    /// `SphereLight`: a sphere emitting outward.
    Sphere,
    /// `RectLight`: one side of a rectangle in the XY plane, emitting
    /// along -Z.
    Rect,
    /// `DiskLight`: one side of a disk in the XY plane, emitting along -Z.
    Disk,
    /// `CylinderLight`: a cylinder along X, emitting outward, not from its
    /// end caps.
    Cylinder,
    /// `DomeLight` or `DomeLight_1`: light from an environment at
    /// infinity.
    Dome,
    /// `PortalLight`: a rectangle in the XY plane guiding a dome's light.
    Portal,
}

impl LightKind {
    /// The kind of the prim at `path`, by `IsA`; `None` for anything else
    /// (mesh, volume, geometry and plugin lights are not listed yet).
    #[must_use]
    pub fn of(scene: &Scene<'_>, path: PathId) -> Option<Self> {
        [
            ("DistantLight", Self::Distant),
            ("SphereLight", Self::Sphere),
            ("RectLight", Self::Rect),
            ("DiskLight", Self::Disk),
            ("CylinderLight", Self::Cylinder),
            ("DomeLight", Self::Dome),
            ("DomeLight_1", Self::Dome),
            ("PortalLight", Self::Portal),
        ]
        .into_iter()
        .find(|(schema, _)| scene.is_a(path, schema))
        .map(|(_, kind)| kind)
    }
}

/// A light's emission: its photometric inputs and what they come to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightEmission {
    /// `inputs:color`, in the rendering color space.
    pub color: [f32; 3],
    /// `inputs:intensity`: luminance in nits before exposure.
    pub intensity: f32,
    /// `inputs:exposure`, in stops.
    pub exposure: f32,
    /// `inputs:normalize`.
    pub normalize: bool,
    /// `inputs:colorTemperature` in kelvin when `inputs:enableColorTemperature`
    /// is on; `None` when off.
    pub color_temperature: Option<f32>,
    /// The blackbody color of [`Self::color_temperature`]
    /// (`UsdLuxBlackbodyTemperatureAsRgb`), white when off or not finite.
    pub temperature_color: [f32; 3],
    /// `inputs:diffuse`: a non-physical multiplier of the diffuse response.
    pub diffuse: f32,
    /// `inputs:specular`: a non-physical multiplier of the specular
    /// response.
    pub specular: f32,
    /// What the luminance is divided by: 1 without `normalize`; with it,
    /// for a `RectLight` its world-space area (`width * height * |X × Y|`
    /// of the transform's axes), a `DiskLight` its world-space area
    /// (`π r² |X × Y|`), a `SphereLight` the area of the ellipsoid the
    /// transform makes of it (semi-axes `radius` times the singular values
    /// of the transform's linear part, area by Knud Thomsen's formula, exact
    /// for a sphere and within 1.1% otherwise), a `CylinderLight` its side's
    /// area (its world length times the perimeter of its world cross-section
    /// perpendicular to its axis, by Ramanujan's second formula, exact for a
    /// circle), a `DistantLight` `π sin²θ` (or `π (2 - sin²θ)` past a
    /// hemisphere) for `θ` half its `angle`, 1 at angle 0, and a dome or
    /// portal light 1. `treatAsPoint` and `treatAsLine` do not change it.
    pub size_factor: f64,
    /// `color * temperature_color * intensity * 2^exposure / size_factor`:
    /// the luminance in nits, per channel, that a renderer emits.
    pub radiance: [f64; 3],
}

/// A light's shape, in its own space (scene units), before its transform.
#[derive(Clone, Debug, PartialEq)]
pub enum LightGeometry {
    /// A `DistantLight`'s angular size.
    Distant {
        /// `inputs:angle`: the angular diameter, in degrees.
        angle: f32,
    },
    /// A `SphereLight`.
    Sphere {
        /// `inputs:radius`.
        radius: f32,
        /// `treatAsPoint`: a hint that it may be drawn as a point.
        treat_as_point: bool,
    },
    /// A `RectLight`.
    Rect {
        /// `inputs:width`, along X.
        width: f32,
        /// `inputs:height`, along Y.
        height: f32,
        /// `inputs:texture:file`, the asset path as authored, if any.
        texture: Option<Arc<str>>,
    },
    /// A `DiskLight`.
    Disk {
        /// `inputs:radius`.
        radius: f32,
    },
    /// A `CylinderLight`.
    Cylinder {
        /// `inputs:radius`.
        radius: f32,
        /// `inputs:length`, along X.
        length: f32,
        /// `treatAsLine`: a hint that it may be drawn as a line.
        treat_as_line: bool,
    },
    /// A `DomeLight` or `DomeLight_1`.
    Dome {
        /// `inputs:texture:file`, the asset path as authored, if any.
        texture: Option<Arc<str>>,
        /// `inputs:texture:format`: `automatic`, `latlong`,
        /// `mirroredBall`, `angular` or `cubeMapVerticalCross`.
        format: Arc<str>,
        /// `DomeLight_1`'s `poleAxis` (`scene`, `Y` or `Z`); `None` for a
        /// `DomeLight`, whose orientation is its transform's alone.
        pole_axis: Option<Arc<str>>,
    },
    /// A `PortalLight`.
    Portal {
        /// `inputs:width`, along X.
        width: f32,
        /// `inputs:height`, along Y.
        height: f32,
    },
}

/// `ShapingAPI`'s controls, as authored: a spot light's cone and an IES
/// profile.
#[derive(Clone, Debug, PartialEq)]
pub struct LightShaping {
    /// `inputs:shaping:cone:angle`: the cone's half-angle, in degrees.
    pub cone_angle: f32,
    /// `inputs:shaping:cone:softness`: the fraction of the cone over which
    /// emission fades.
    pub cone_softness: f32,
    /// `inputs:shaping:focus`: the exponent narrowing emission toward -Z.
    pub focus: f32,
    /// `inputs:shaping:focusTint`: the color off-axis emission fades to.
    pub focus_tint: [f32; 3],
    /// `inputs:shaping:ies:file`, the asset path as authored, if any.
    pub ies_file: Option<Arc<str>>,
    /// `inputs:shaping:ies:angleScale`.
    pub ies_angle_scale: f32,
    /// `inputs:shaping:ies:normalize`.
    pub ies_normalize: bool,
}

/// `ShadowAPI`'s controls, as authored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightShadow {
    /// `inputs:shadow:enable`.
    pub enable: bool,
    /// `inputs:shadow:color`.
    pub color: [f32; 3],
    /// `inputs:shadow:distance`; -1 for unlimited.
    pub distance: f32,
    /// `inputs:shadow:falloff`; nonpositive for none.
    pub falloff: f32,
    /// `inputs:shadow:falloffGamma`.
    pub falloff_gamma: f32,
}

/// A light-linking collection's rule as authored (`CollectionAPI`), data
/// only: which prims it lights or shadows is not evaluated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LightLink {
    /// `collection:<name>:includeRoot` (`true` by the `UsdLux` fallback:
    /// everything is lit).
    pub include_root: bool,
    /// `collection:<name>:includes` targets.
    pub includes: Vec<TargetPath>,
    /// `collection:<name>:excludes` targets.
    pub excludes: Vec<TargetPath>,
}

/// One light, read at the list's time: what a renderer needs to light the
/// scene with it.
#[derive(Clone, Debug, PartialEq)]
pub struct LightItem {
    /// The light prim.
    pub path: PathId,
    /// Its kind.
    pub kind: LightKind,
    /// Its local-to-world matrix, row-vector as OpenUSD's (`row[3]` is the
    /// translation): `UsdGeomXformable::ComputeLocalToWorldTransform`.
    pub matrix: [[f64; 4]; 4],
    /// Whether it is invisible (`ComputeVisibility`); a renderer leaves it
    /// out. It stays listed.
    pub hidden: bool,
    /// Its world position: the matrix's translation.
    pub position: [f64; 3],
    /// The world direction it emits along: its -Z axis, normalized (what a
    /// distant light shines along and a shaped light's cone points at);
    /// zero when the transform collapses Z.
    pub direction: [f64; 3],
    /// Its emission.
    pub emission: LightEmission,
    /// Its shape.
    pub geometry: LightGeometry,
    /// Its `ShapingAPI` controls, when the API is applied.
    pub shaping: Option<LightShaping>,
    /// Its `ShadowAPI` controls, when the API is applied.
    pub shadow: Option<LightShadow>,
    /// Its `lightLink` collection: the prims it lights.
    pub light_link: LightLink,
    /// Its `shadowLink` collection: the prims that cast its shadows.
    pub shadow_link: LightLink,
}

/// The attributes a light is read from, besides its transform and
/// visibility: what [`crate::DrawList::set_time`] asks to vary.
pub(crate) const LIGHT_INPUTS: &[&str] = &[
    "inputs:color",
    "inputs:intensity",
    "inputs:exposure",
    "inputs:normalize",
    "inputs:enableColorTemperature",
    "inputs:colorTemperature",
    "inputs:diffuse",
    "inputs:specular",
    "inputs:angle",
    "inputs:radius",
    "inputs:width",
    "inputs:height",
    "inputs:length",
    "inputs:texture:file",
    "inputs:texture:format",
    "inputs:shaping:cone:angle",
    "inputs:shaping:cone:softness",
    "inputs:shaping:focus",
    "inputs:shaping:focusTint",
    "inputs:shaping:ies:file",
    "inputs:shaping:ies:angleScale",
    "inputs:shaping:ies:normalize",
    "inputs:shadow:enable",
    "inputs:shadow:color",
    "inputs:shadow:distance",
    "inputs:shadow:falloff",
    "inputs:shadow:falloffGamma",
];

/// Reads the light at `path` of `kind` at `time`, given its world matrix
/// and visibility, and the prims its connected inputs read. Inputs are read
/// with the schema fallbacks; a connected input reads its source
/// ([`input_at`]).
pub(crate) fn read_light(
    scene: &Scene<'_>,
    path: PathId,
    kind: LightKind,
    time: Time,
    matrix: [[f64; 4]; 4],
    hidden: bool,
) -> (LightItem, Vec<PathId>) {
    let prim = PrimView::new(*scene, path);
    let sources = RefCell::new(Vec::new());
    let float = |name: &str, fallback: f32| {
        input_at(
            scene,
            path,
            name,
            time,
            read_float,
            &mut sources.borrow_mut(),
        )
        .filter(|v| v.is_finite())
        .unwrap_or(fallback)
    };
    let boolean = |name: &str, fallback: bool| {
        input_at(
            scene,
            path,
            name,
            time,
            read_bool,
            &mut sources.borrow_mut(),
        )
        .unwrap_or(fallback)
    };
    let float3 = |name: &str, fallback: [f32; 3]| {
        input_at(
            scene,
            path,
            name,
            time,
            read_float3,
            &mut sources.borrow_mut(),
        )
        .unwrap_or(fallback)
    };
    let color = |name: &str| float3(name, [1., 1., 1.]);
    let asset = |name: &str| {
        input_at(
            scene,
            path,
            name,
            time,
            read_asset,
            &mut sources.borrow_mut(),
        )
        .filter(|asset| !asset.is_empty())
    };
    let token = |name: &str, fallback: &str| -> Arc<str> {
        input_at(
            scene,
            path,
            name,
            time,
            read_token,
            &mut sources.borrow_mut(),
        )
        .unwrap_or(fallback)
        .into()
    };
    let geometry = match kind {
        LightKind::Distant => LightGeometry::Distant {
            angle: float("inputs:angle", 0.53),
        },
        LightKind::Sphere => LightGeometry::Sphere {
            radius: float("inputs:radius", 0.5),
            treat_as_point: boolean("treatAsPoint", false),
        },
        LightKind::Rect => LightGeometry::Rect {
            width: float("inputs:width", 1.),
            height: float("inputs:height", 1.),
            texture: asset("inputs:texture:file"),
        },
        LightKind::Disk => LightGeometry::Disk {
            radius: float("inputs:radius", 0.5),
        },
        LightKind::Cylinder => LightGeometry::Cylinder {
            radius: float("inputs:radius", 0.5),
            length: float("inputs:length", 1.),
            treat_as_line: boolean("treatAsLine", false),
        },
        LightKind::Dome => LightGeometry::Dome {
            texture: asset("inputs:texture:file"),
            format: token("inputs:texture:format", "automatic"),
            pole_axis: scene
                .is_a(path, "DomeLight_1")
                .then(|| token("poleAxis", "scene")),
        },
        LightKind::Portal => LightGeometry::Portal {
            width: float("inputs:width", 1.),
            height: float("inputs:height", 1.),
        },
    };
    let color_temperature = boolean("inputs:enableColorTemperature", false)
        .then(|| float("inputs:colorTemperature", 6500.));
    let mut emission = LightEmission {
        color: color("inputs:color"),
        intensity: float(
            "inputs:intensity",
            if kind == LightKind::Distant {
                50000.
            } else {
                1.
            },
        ),
        exposure: float("inputs:exposure", 0.),
        normalize: boolean("inputs:normalize", false),
        color_temperature,
        temperature_color: color_temperature
            .and_then(|kelvin| blackbody_temperature_rgb(kelvin).ok())
            .unwrap_or([1., 1., 1.]),
        diffuse: float("inputs:diffuse", 1.),
        specular: float("inputs:specular", 1.),
        size_factor: 1.,
        radiance: [0.; 3],
    };
    emission.size_factor = if emission.normalize {
        size_factor(&geometry, &matrix)
    } else {
        1.
    };
    emission.radiance = radiance(&emission);
    let shaping = scene
        .has_api(path, "ShapingAPI", None)
        .then(|| LightShaping {
            cone_angle: float("inputs:shaping:cone:angle", 90.),
            cone_softness: float("inputs:shaping:cone:softness", 0.),
            focus: float("inputs:shaping:focus", 0.),
            focus_tint: float3("inputs:shaping:focusTint", [0., 0., 0.]),
            ies_file: asset("inputs:shaping:ies:file"),
            ies_angle_scale: float("inputs:shaping:ies:angleScale", 0.),
            ies_normalize: boolean("inputs:shaping:ies:normalize", false),
        });
    let shadow = scene.has_api(path, "ShadowAPI", None).then(|| LightShadow {
        enable: boolean("inputs:shadow:enable", true),
        color: float3("inputs:shadow:color", [0., 0., 0.]),
        distance: float("inputs:shadow:distance", -1.),
        falloff: float("inputs:shadow:falloff", -1.),
        falloff_gamma: float("inputs:shadow:falloffGamma", 1.),
    });
    let link = |name: &str| LightLink {
        include_root: prim
            .read_value_at(
                &alloc::format!("collection:{name}:includeRoot"),
                time,
                read_bool,
            )
            .unwrap_or(true),
        includes: prim.read_targets(&alloc::format!("collection:{name}:includes")),
        excludes: prim.read_targets(&alloc::format!("collection:{name}:excludes")),
    };
    let item = LightItem {
        path,
        kind,
        matrix,
        hidden,
        position: [matrix[3][0], matrix[3][1], matrix[3][2]],
        direction: normalized([-matrix[2][0], -matrix[2][1], -matrix[2][2]]),
        emission,
        geometry,
        shaping,
        shadow,
        light_link: link("lightLink"),
        shadow_link: link("shadowLink"),
    };
    (item, sources.into_inner())
}

/// Reads the light's attribute `name` at `time` by `read`. An input with
/// connections reads what they lead to, through node graphs and interface
/// inputs (`UsdShadeInput::GetValueProducingAttributes`, as
/// `layerstack_schemas::light::LightInput` reads it): one attribute with an
/// authored value is read (the input's own value when every connection
/// fails, as OpenUSD falls back to it); a shader output or several sources
/// read nothing, so the input takes its fallback. The prims the connections inspected are added to
/// `sources`. AOUSD Core §12.4 (connections).
fn input_at<'a, T>(
    scene: &Scene<'a>,
    path: PathId,
    name: &str,
    time: Time,
    read: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
    sources: &mut Vec<PathId>,
) -> Option<T> {
    let prim = PrimView::new(*scene, path);
    if name.starts_with("inputs:")
        && let Some(property) = prim.property_path(name)
    {
        let connected = scene.connected_sources(property);
        if !connected.sources.is_empty() || !connected.invalid.is_empty() {
            let found = scene.value_sources(property);
            for dependency in &found.dependencies {
                if dependency.prim != path && !sources.contains(&dependency.prim) {
                    sources.push(dependency.prim);
                }
            }
            return match found.sources.as_slice() {
                [one] if one.kind == ValueSourceKind::AuthoredValue => {
                    let property = scene.store().tokens().resolve(one.attribute.property());
                    PrimView::new(*scene, one.attribute.prim_path())
                        .read_value_at(property, time, read)
                }
                _ => None,
            };
        }
    }
    prim.read_value_at(name, time, read)
}

/// `color * temperature_color * intensity * 2^exposure / size_factor`.
fn radiance(emission: &LightEmission) -> [f64; 3] {
    let scalar = f64::from(emission.intensity) * libm::exp2(f64::from(emission.exposure))
        / emission.size_factor;
    core::array::from_fn(|i| {
        f64::from(emission.color[i]) * f64::from(emission.temperature_color[i]) * scalar
    })
}

/// The `sizeFactor` of a normalized light: see
/// [`LightEmission::size_factor`].
pub(crate) fn size_factor(geometry: &LightGeometry, matrix: &[[f64; 4]; 4]) -> f64 {
    let axis = |row: usize| [matrix[row][0], matrix[row][1], matrix[row][2]];
    let (x, y, z) = (axis(0), axis(1), axis(2));
    let factor = match *geometry {
        LightGeometry::Rect { width, height, .. } => {
            f64::from(width) * f64::from(height) * length(cross(x, y))
        }
        LightGeometry::Disk { radius } => {
            core::f64::consts::PI * f64::from(radius) * f64::from(radius) * length(cross(x, y))
        }
        LightGeometry::Sphere { radius, .. } => {
            let [a, b, c] = singular_values([x, y, z]).map(|s| s * f64::from(radius));
            ellipsoid_area(a, b, c)
        }
        LightGeometry::Cylinder {
            radius, length: l, ..
        } => {
            let along = length(x);
            if along == 0. {
                0.
            } else {
                // The cross-section perpendicular to the axis: the images
                // of Y and Z with their components along X removed.
                let unit = x.map(|v| v / along);
                let y = sub(y, scale(unit, dot(y, unit)));
                let z = sub(z, scale(unit, dot(z, unit)));
                let [a, b] = singular_values_2(y, z).map(|s| s * f64::from(radius));
                f64::from(l) * along * ellipse_perimeter(a, b)
            }
        }
        LightGeometry::Distant { angle } => {
            let theta = (f64::from(angle).to_radians() / 2.).clamp(0., core::f64::consts::PI);
            let sin2 = libm::sin(theta) * libm::sin(theta);
            if theta == 0. {
                1.
            } else if theta <= core::f64::consts::FRAC_PI_2 {
                sin2 * core::f64::consts::PI
            } else {
                (2. - sin2) * core::f64::consts::PI
            }
        }
        LightGeometry::Dome { .. } | LightGeometry::Portal { .. } => 1.,
    };
    // A light the transform flattens has no area to spread over: emit as
    // if not normalized rather than divide by zero.
    if factor > 0. && factor.is_finite() {
        factor
    } else {
        1.
    }
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn length(a: [f64; 3]) -> f64 {
    libm::sqrt(dot(a, a))
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    a.map(|v| v * s)
}

fn normalized(a: [f64; 3]) -> [f64; 3] {
    let l = length(a);
    if l > 0. { a.map(|v| v / l) } else { [0.; 3] }
}

/// The singular values of the 3x3 matrix whose rows are `rows`: the square
/// roots of the eigenvalues of its Gram matrix, by the trigonometric
/// solution of the symmetric eigenproblem, largest first.
fn singular_values(rows: [[f64; 3]; 3]) -> [f64; 3] {
    let g = |i: usize, j: usize| dot(rows[i], rows[j]);
    let (a, b, c) = (g(0, 0), g(1, 1), g(2, 2));
    let (d, e, f) = (g(0, 1), g(1, 2), g(0, 2));
    let off = d * d + e * e + f * f;
    let eigen = if off == 0. {
        let mut diagonal = [a, b, c];
        diagonal.sort_by(|x, y| y.total_cmp(x));
        diagonal
    } else {
        let q = (a + b + c) / 3.;
        let p =
            libm::sqrt(((a - q) * (a - q) + (b - q) * (b - q) + (c - q) * (c - q) + 2. * off) / 6.);
        // det((G - qI) / p) / 2, clamped to acos's domain.
        let (a, b, c) = ((a - q) / p, (b - q) / p, (c - q) / p);
        let (d, e, f) = (d / p, e / p, f / p);
        let r =
            ((a * (b * c - e * e) - d * (d * c - e * f) + f * (d * e - b * f)) / 2.).clamp(-1., 1.);
        let phi = libm::acos(r) / 3.;
        let first = q + 2. * p * libm::cos(phi);
        let third = q + 2. * p * libm::cos(phi + 2. * core::f64::consts::PI / 3.);
        [first, 3. * q - first - third, third]
    };
    eigen.map(|v| libm::sqrt(v.max(0.)))
}

/// The singular values of the 3x2 map taking the unit circle's axes to `u`
/// and `v`: the ellipse's semi-axes.
fn singular_values_2(u: [f64; 3], v: [f64; 3]) -> [f64; 2] {
    let (a, b, c) = (dot(u, u), dot(v, v), dot(u, v));
    let mean = (a + b) / 2.;
    let spread = libm::sqrt((a - b) * (a - b) / 4. + c * c);
    [mean + spread, mean - spread].map(|v| libm::sqrt(v.max(0.)))
}

/// The surface area of an ellipsoid of semi-axes `a`, `b`, `c` by Knud
/// Thomsen's formula (`p = 1.6075`): exact for a sphere, within 1.061%.
fn ellipsoid_area(a: f64, b: f64, c: f64) -> f64 {
    const P: f64 = 1.6075;
    let (a, b, c) = (libm::pow(a, P), libm::pow(b, P), libm::pow(c, P));
    4. * core::f64::consts::PI * libm::pow((a * b + a * c + b * c) / 3., 1. / P)
}

/// The perimeter of an ellipse of semi-axes `a`, `b` by Ramanujan's second
/// formula: exact for a circle.
fn ellipse_perimeter(a: f64, b: f64) -> f64 {
    if a + b == 0. {
        return 0.;
    }
    let h = (a - b) * (a - b) / ((a + b) * (a + b));
    core::f64::consts::PI * (a + b) * (1. + 3. * h / (10. + libm::sqrt(4. - 3. * h)))
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use alloc::vec::Vec;
    use core::f64::consts::PI;

    use layerstack::{
        EditTarget, InMemoryStore, LayerId, LiveStage, NoAssets, PropertyPath, StageOptions, Time,
        Transaction, Value,
    };
    use layerstack_schemas::usd_lux::LightApiEdit;
    use layerstack_schemas::{Scene, SchemaEdit};

    use super::{LightGeometry, LightKind, TargetPath, singular_values, size_factor};
    use crate::{DrawDiff, DrawList, DrawOptions};

    const SCENE: &str = r#"#usda 1.0
def Xform "World" {
    def Camera "Eye" {
        float focalLength = 35
        float2 clippingRange = (0.1, 100)
        double3 xformOp:translate.timeSamples = {0: (0, 1, 8), 10: (0, 1, 4)}
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
    def Xform "Rig" {
        double3 xformOp:scale = (2, 2, 2)
        uniform token[] xformOpOrder = ["xformOp:scale"]
        def SphereLight "Bulb" {
            float inputs:radius = 0.5
            float inputs:intensity = 8
            float inputs:exposure = 1
            bool inputs:normalize = true
            double3 xformOp:translate = (0, 1, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
        }
    }
    def RectLight "Panel" {
        float inputs:width = 2
        float inputs:height = 0.5
        bool inputs:normalize = true
        asset inputs:texture:file = @panel.png@
        double3 xformOp:scale = (3, 2, 1)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }
    def DistantLight "Sun" (
        prepend apiSchemas = ["ShadowAPI"]
    ) {
        bool inputs:enableColorTemperature = true
        float inputs:colorTemperature = 4000
        float inputs:intensity.timeSamples = {0: 1000, 10: 3000}
        float3 xformOp:rotateXYZ = (-90, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:rotateXYZ"]
    }
    def DiskLight "Spot" (
        prepend apiSchemas = ["ShapingAPI"]
    ) {
        float inputs:shaping:cone:angle = 30
        float inputs:shaping:cone:softness = 0.25
        rel collection:lightLink:includes = </World/Rig>
        uniform bool collection:lightLink:includeRoot = false
    }
    def DomeLight_1 "Sky" {
        asset inputs:texture:file = @sky.exr@
        token inputs:texture:format = "latlong"
    }
    def CylinderLight "Tube" {
        float inputs:length = 2
        float inputs:radius = 0.25
        bool inputs:normalize = true
        bool treatAsLine = true
        double3 xformOp:scale = (2, 3, 3)
        uniform token[] xformOpOrder = ["xformOp:scale"]
    }
}
"#;

    struct Fixture {
        store: InMemoryStore,
        live: LiveStage,
        list: DrawList,
    }

    impl Fixture {
        fn new() -> (Self, DrawDiff) {
            let mut store = InMemoryStore::default();
            let read = layerstack_usda::read_usda(
                SCENE,
                LayerId(1),
                store.tokens(),
                store.paths(),
                &mut NoAssets,
            );
            assert!(
                !read.has_errors(),
                "{:?}",
                read.diagnostics().collect::<Vec<_>>()
            );
            store.insert_layer(read.emitted.layer);
            let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
            let live = LiveStage::compose(
                &mut store,
                LayerId(1),
                StageOptions {
                    schemas: Some(schemas),
                    ..StageOptions::default()
                },
            );
            let mut list = DrawList::new(DrawOptions {
                time: Time::at(0.0),
                ..DrawOptions::default()
            });
            let diff = list.refresh(&Scene::new(live.stage(), &store));
            (Self { store, live, list }, diff)
        }

        fn edit(&mut self, author: impl FnOnce(&mut SchemaEdit<'_>)) -> DrawDiff {
            let mut edit = SchemaEdit::new(
                self.live.stage(),
                &mut self.store,
                EditTarget::for_layer(LayerId(1)),
            );
            author(&mut edit);
            let transaction = edit.finish();
            let applied = self.live.apply(&mut self.store, &transaction).unwrap();
            let scene = Scene::new(self.live.stage(), &self.store);
            self.list.apply_changes(&scene, &applied.changes)
        }

        fn path(&self, text: &str) -> layerstack::PathId {
            self.store.path(text)
        }
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-12 * b.abs().max(1.), "{a} != {b}");
    }

    #[test]
    fn a_refresh_lists_lights_with_renderer_ready_values() {
        let (fixture, diff) = Fixture::new();
        let list = &fixture.list;
        assert_eq!(diff.lights_added.len(), 6);
        assert_eq!(diff.cameras_added, [fixture.path("/World/Eye")]);
        assert_eq!(diff.work.lights_read, 6);
        assert_eq!(diff.work.cameras_read, 1);

        // A sphere of radius 0.5 scaled by 2: area 4π, normalized.
        let bulb = list.light(fixture.path("/World/Rig/Bulb")).unwrap();
        assert_eq!(bulb.kind, LightKind::Sphere);
        assert_eq!(bulb.position, [0., 2., 0.]);
        close(bulb.emission.size_factor, 4. * PI);
        close(bulb.emission.radiance[0], 8. * 2. / (4. * PI));

        // A 2 x 0.5 rectangle scaled by 3 x 2: area 6.
        let panel = list.light(fixture.path("/World/Panel")).unwrap();
        close(panel.emission.size_factor, 6.);
        assert_eq!(panel.direction, [0., 0., -1.]);
        let LightGeometry::Rect { texture, .. } = &panel.geometry else {
            panic!("{:?}", panel.geometry);
        };
        assert_eq!(texture.as_deref(), Some("panel.png"));

        // The sun: rotated to shine down, blackbody tinted, its fallback
        // angle unused without `normalize`.
        let sun = list.light(fixture.path("/World/Sun")).unwrap();
        assert!(sun.direction[1] < -0.999_999, "{:?}", sun.direction);
        assert_eq!(sun.emission.color_temperature, Some(4000.));
        let rgb = layerstack_schemas::light::blackbody_temperature_rgb(4000.).unwrap();
        assert_eq!(sun.emission.temperature_color, rgb);
        close(sun.emission.radiance[2], f64::from(rgb[2]) * 1000.);
        assert_eq!(sun.emission.size_factor, 1.);
        assert!(
            sun.shadow
                .is_some_and(|shadow| shadow.enable && shadow.distance == -1.)
        );
        assert!(sun.shaping.is_none());

        let spot = list.light(fixture.path("/World/Spot")).unwrap();
        let shaping = spot.shaping.as_ref().unwrap();
        assert_eq!((shaping.cone_angle, shaping.cone_softness), (30., 0.25));
        assert!(!spot.light_link.include_root);
        assert_eq!(spot.light_link.includes.len(), 1);
        assert!(spot.shadow_link.include_root);

        let sky = list.light(fixture.path("/World/Sky")).unwrap();
        let LightGeometry::Dome {
            texture,
            format,
            pole_axis,
        } = &sky.geometry
        else {
            panic!("{:?}", sky.geometry);
        };
        assert_eq!(texture.as_deref(), Some("sky.exr"));
        assert_eq!(&**format, "latlong");
        assert_eq!(pole_axis.as_deref(), Some("scene"));

        // A tube of radius 0.25, length 2, scaled 2 along its axis and 3
        // across: side area 2π (0.75) (4).
        let tube = list.light(fixture.path("/World/Tube")).unwrap();
        close(tube.emission.size_factor, 2. * PI * 0.75 * 4.);
        assert!(matches!(
            tube.geometry,
            LightGeometry::Cylinder {
                treat_as_line: true,
                ..
            }
        ));

        let eye = list.camera(fixture.path("/World/Eye")).unwrap();
        let camera = eye.camera.as_ref().unwrap();
        assert_eq!(camera.view_matrix()[3][2], -8.);
    }

    #[test]
    fn a_light_edit_reads_only_that_light() {
        let (mut fixture, _) = Fixture::new();
        let bulb = fixture.path("/World/Rig/Bulb");
        let diff = fixture.edit(|edit| {
            LightApiEdit::new(edit, bulb)
                .unwrap()
                .unwrap()
                .set_intensity(edit, 16.);
        });
        assert_eq!(diff.lights_changed, [bulb]);
        assert_eq!(diff.work.lights_read, 1);
        assert_eq!(diff.work.cameras_read, 0);
        assert_eq!(diff.work.items_read, 0);
        close(
            fixture.list.light(bulb).unwrap().emission.radiance[0],
            16. * 2. / (4. * PI),
        );
        // Moving the rig moves the bulb, and nothing else.
        let rig = fixture.path("/World/Rig");
        let name = fixture.store.tokens().intern("xformOp:scale");
        let mut scale = Transaction::new();
        scale.set_default(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(rig, name)),
            Value::Vec3d([3., 3., 3.]),
        );
        let applied = fixture.live.apply(&mut fixture.store, &scale).unwrap();
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let diff = fixture.list.apply_changes(&scene, &applied.changes);
        assert_eq!(diff.lights_changed, [bulb]);
        assert_eq!(diff.work.lights_read, 1);
        // A property the light does not read: nothing read.
        let aim = fixture.store.tokens().intern("aim");
        let mut relationship = Transaction::new();
        relationship.create_property(
            EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(bulb, aim)),
            layerstack::PropertySpec::relationship(),
        );
        let applied = fixture
            .live
            .apply(&mut fixture.store, &relationship)
            .unwrap();
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let diff = fixture.list.apply_changes(&scene, &applied.changes);
        assert_eq!(diff.work.lights_read, 0);
        assert!(diff.is_empty());
    }

    #[test]
    fn a_time_change_reads_only_what_might_vary() {
        let (mut fixture, _) = Fixture::new();
        let scene = Scene::new(fixture.live.stage(), &fixture.store);
        let diff = fixture.list.set_time(&scene, Time::at(10.0));
        let sun = fixture.path("/World/Sun");
        let eye = fixture.path("/World/Eye");
        assert_eq!(diff.lights_changed, [sun]);
        assert_eq!(diff.cameras_changed, [eye]);
        assert_eq!((diff.work.lights_read, diff.work.cameras_read), (1, 1));
        close(
            fixture.list.light(sun).unwrap().emission.intensity.into(),
            3000.,
        );
        let camera = fixture.list.camera(eye).unwrap().camera.as_ref().unwrap();
        assert_eq!(camera.view_matrix()[3][2], -4.);
    }

    #[test]
    fn a_new_light_is_added_by_its_resync() {
        let (mut fixture, _) = Fixture::new();
        let lamp = fixture.path("/World/Lamp");
        let diff = fixture.edit(|edit| {
            edit.define_prim(lamp, "SphereLight").unwrap();
        });
        assert_eq!(diff.lights_added, [lamp]);
        assert_eq!(diff.work.lights_read, 1);
    }

    #[test]
    fn sizes_follow_the_transform() {
        // A shear keeps a sphere's volume but not its area: the singular
        // values of [[1, 1, 0], [0, 1, 0], [0, 0, 1]] are φ, 1/φ and 1.
        let phi = f64::midpoint(1., libm::sqrt(5.));
        let values = singular_values([[1., 1., 0.], [0., 1., 0.], [0., 0., 1.]]);
        close(values[0], phi);
        close(values[1], 1.);
        close(values[2], 1. / phi);
        let identity = [
            [1., 0., 0., 0.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
            [0., 0., 0., 1.],
        ];
        let sphere = LightGeometry::Sphere {
            radius: 2.,
            treat_as_point: true,
        };
        close(size_factor(&sphere, &identity), 16. * PI);
        let distant = |angle| size_factor(&LightGeometry::Distant { angle }, &identity);
        assert_eq!(distant(0.), 1.);
        close(distant(90.), PI / 2.);
        close(distant(270.), (2. - 0.5) * PI);
        let mut flat = identity;
        flat[1] = [0.; 4];
        let disk = LightGeometry::Disk { radius: 1. };
        assert_eq!(size_factor(&disk, &flat), 1.);
        close(size_factor(&disk, &identity), PI);
    }

    /// Every light of `stagehand_lights/scene.usda` at four times against
    /// what usd-core answers: inputs through the `UsdLux` schema API
    /// (authored or fallback), blackbody colors,
    /// `ComputeLocalToWorldTransform`, `ComputeVisibility`, `ShapingAPI`
    /// and `ShadowAPI` inputs and link collections.
    #[test]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the oracle writes `float` inputs widened to double"
    )]
    fn lights_match_openusd() {
        use serde_json::Value as Json;
        let oracle: Json = serde_json::from_str(include_str!(
            "../../layerstack_conformance/fixtures/stagehand_lights/oracle.json"
        ))
        .unwrap();
        assert_eq!(
            oracle["version"].as_str(),
            Some(layerstack_schemas::OPENUSD_VERSION)
        );
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            include_str!("../../layerstack_conformance/fixtures/stagehand_lights/scene.usda"),
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        store.insert_layer(read.emitted.layer);
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        let scene = Scene::new(live.stage(), &store);
        let mut list = DrawList::new(DrawOptions::default());
        list.refresh(&scene);
        let f32_of = |value: &Json| value.as_f64().unwrap() as f32;
        let color_of = |value: &Json| -> [f32; 3] {
            core::array::from_fn(|i| value[i].as_f64().unwrap() as f32)
        };
        let text_of = |value: &Json| value.as_str().map(Arc::<str>::from);
        let rows = oracle["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 36);
        for row in rows {
            let time = row["time"].as_f64().map_or(Time::Default, Time::at);
            if list.options().time != time {
                list.set_time(&scene, time);
            }
            let at = alloc::format!("{} {time:?}", row["path"]);
            let light = list
                .light(store.path(row["path"].as_str().unwrap()))
                .unwrap_or_else(|| panic!("{at} is not listed"));
            let inputs = &row["inputs"];
            for (i, matrix_row) in light.matrix.iter().enumerate() {
                for (j, value) in matrix_row.iter().enumerate() {
                    let expected = row["world"][i][j].as_f64().unwrap();
                    assert!((value - expected).abs() <= 1e-12, "{at}: world {i} {j}");
                }
            }
            assert_eq!(light.hidden, row["visibility"] == "invisible", "{at}");
            let emission = &light.emission;
            assert_eq!(emission.color, color_of(&inputs["color"]), "{at}");
            assert_eq!(emission.intensity, f32_of(&inputs["intensity"]), "{at}");
            assert_eq!(emission.exposure, f32_of(&inputs["exposure"]), "{at}");
            assert_eq!(emission.normalize, inputs["normalize"] == true, "{at}");
            assert_eq!(emission.diffuse, f32_of(&inputs["diffuse"]), "{at}");
            assert_eq!(emission.specular, f32_of(&inputs["specular"]), "{at}");
            if inputs["enableColorTemperature"] == true {
                assert_eq!(
                    emission.color_temperature,
                    Some(f32_of(&inputs["colorTemperature"])),
                    "{at}"
                );
                assert_eq!(
                    emission.temperature_color,
                    color_of(&row["blackbody"]),
                    "{at}"
                );
            } else {
                assert_eq!(emission.color_temperature, None, "{at}");
            }
            let attribute = &row["attributes"];
            match &light.geometry {
                LightGeometry::Distant { angle } => {
                    assert_eq!(*angle, f32_of(&inputs["angle"]), "{at}");
                }
                LightGeometry::Sphere {
                    radius,
                    treat_as_point,
                } => {
                    assert_eq!(*radius, f32_of(&inputs["radius"]), "{at}");
                    assert_eq!(*treat_as_point, attribute["treatAsPoint"] == true, "{at}");
                }
                LightGeometry::Rect {
                    width,
                    height,
                    texture,
                } => {
                    assert_eq!(*width, f32_of(&inputs["width"]), "{at}");
                    assert_eq!(*height, f32_of(&inputs["height"]), "{at}");
                    assert_eq!(*texture, text_of(&inputs["texture:file"]), "{at}");
                }
                LightGeometry::Disk { radius } => {
                    assert_eq!(*radius, f32_of(&inputs["radius"]), "{at}");
                }
                LightGeometry::Cylinder {
                    radius,
                    length,
                    treat_as_line,
                } => {
                    assert_eq!(*radius, f32_of(&inputs["radius"]), "{at}");
                    assert_eq!(*length, f32_of(&inputs["length"]), "{at}");
                    assert_eq!(*treat_as_line, attribute["treatAsLine"] == true, "{at}");
                }
                LightGeometry::Dome {
                    texture,
                    format,
                    pole_axis,
                } => {
                    assert_eq!(*texture, text_of(&inputs["texture:file"]), "{at}");
                    assert_eq!(
                        Some(format.clone()),
                        text_of(&inputs["texture:format"]),
                        "{at}"
                    );
                    assert_eq!(*pole_axis, text_of(&attribute["poleAxis"]), "{at}");
                }
                LightGeometry::Portal { width, height } => {
                    assert_eq!(*width, f32_of(&inputs["width"]), "{at}");
                    assert_eq!(*height, f32_of(&inputs["height"]), "{at}");
                }
            }
            assert_eq!(light.shaping.is_some(), row["shaping"] == true, "{at}");
            if let Some(shaping) = &light.shaping {
                assert_eq!(shaping.cone_angle, f32_of(&inputs["shaping:cone:angle"]));
                assert_eq!(
                    shaping.cone_softness,
                    f32_of(&inputs["shaping:cone:softness"])
                );
                assert_eq!(shaping.focus, f32_of(&inputs["shaping:focus"]));
                assert_eq!(shaping.focus_tint, color_of(&inputs["shaping:focusTint"]));
                assert_eq!(shaping.ies_file, text_of(&inputs["shaping:ies:file"]));
                assert_eq!(
                    shaping.ies_angle_scale,
                    f32_of(&inputs["shaping:ies:angleScale"])
                );
                assert_eq!(
                    shaping.ies_normalize,
                    inputs["shaping:ies:normalize"] == true
                );
            }
            assert_eq!(light.shadow.is_some(), row["shadow"] == true, "{at}");
            if let Some(shadow) = &light.shadow {
                assert_eq!(shadow.enable, inputs["shadow:enable"] == true);
                assert_eq!(shadow.color, color_of(&inputs["shadow:color"]));
                assert_eq!(shadow.distance, f32_of(&inputs["shadow:distance"]));
                assert_eq!(shadow.falloff, f32_of(&inputs["shadow:falloff"]));
                assert_eq!(shadow.falloff_gamma, f32_of(&inputs["shadow:falloffGamma"]));
            }
            for (link, name) in [
                (&light.light_link, "lightLink"),
                (&light.shadow_link, "shadowLink"),
            ] {
                let expected = &row["links"][name];
                assert_eq!(link.include_root, expected["includeRoot"] == true, "{at}");
                let targets = |list: &Json| -> Vec<TargetPath> {
                    list.as_array()
                        .unwrap()
                        .iter()
                        .map(|path| TargetPath::Prim(store.path(path.as_str().unwrap())))
                        .collect()
                };
                assert_eq!(link.includes, targets(&expected["includes"]), "{at}");
                assert_eq!(link.excludes, targets(&expected["excludes"]), "{at}");
            }
        }
    }
}
