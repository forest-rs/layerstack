// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit sampled camera math without render resources or clip-space adapters.
use crate::{
    PrimView, Time, XformCache,
    bounds::BoundingBox,
    gf,
    usd_geom::{Camera, CameraProjection},
};
use alloc::vec::Vec;
use layerstack::{TokenInterner, Value};
type Matrix = [[f64; 4]; 4];
/// Invalid camera parameters or transform; computations return no partial camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraError {
    /// A required parameter has no compatible value or is geometrically invalid.
    InvalidAttribute(&'static str),
    /// Unknown projection tokens have no projection model in this API.
    UnsupportedProjection,
    /// The camera transform is absent, singular or nonfinite.
    InvalidTransform,
    /// Numeric camera/frame times or resulting shutter endpoints are nonfinite.
    InvalidTime,
}
impl core::fmt::Display for CameraError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid camera: {self:?}")
    }
}
impl core::error::Error for CameraError {}
fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    match time {
        Time::Default => prim.read_value(name, decode),
        Time::At {
            code,
            interpolation,
        } => prim.read_value_at(name, code, interpolation, decode),
    }
}
fn transform(point: [f64; 3], matrix: &Matrix) -> [f64; 3] {
    core::array::from_fn(|j| {
        point[0] * matrix[0][j] + point[1] * matrix[1][j] + point[2] * matrix[2][j] + matrix[3][j]
    })
}

#[inline]
fn contains_planes(planes: &[[f64; 4]; 6], point: [f64; 3]) -> bool {
    planes
        .iter()
        .all(|p| p[0] * point[0] + p[1] * point[1] + p[2] * point[2] + p[3] >= 0.)
}

#[inline]
fn intersects_planes(planes: &[[f64; 4]; 6], bound: &BoundingBox) -> bool {
    planes.iter().all(|plane| {
        (0..8).any(|corner| {
            let point = core::array::from_fn(|i| {
                if corner & (1 << i) == 0 {
                    bound.range.min[i]
                } else {
                    bound.range.max[i]
                }
            });
            let point = transform(point, &bound.matrix);
            plane[0] * point[0] + plane[1] * point[1] + plane[2] * point[2] + plane[3] >= 0.
        })
    })
}
/// Sampled camera parameters and primary viewing frustum, detached from the scene.
/// Matrices use USD row vectors, right-handed eye space looking down `-Z`, and
/// OpenGL NDC depth `[-1,1]`. Rebuild after camera/ancestor edits. The view conforms
/// reflection, scale and shear as `GfFrustum`; the authored world matrix remains
/// available separately. Resolution, aspect conforming, GPU conventions, depth
/// of field and additional clipping-plane execution belong to consumers.
#[derive(Clone, Debug)]
pub struct ComputedCamera {
    projection: CameraProjection,
    camera_to_world: Matrix,
    view: Matrix,
    view_inverse: Matrix,
    projection_matrix: Matrix,
    window: [f64; 4],
    clipping_range: [f64; 2],
    aperture: [f32; 2],
    aperture_offset: [f32; 2],
    focal_length: f32,
    clipping_planes: Vec<[f32; 4]>,
    f_stop: f32,
    focus_distance: f32,
    shutter_offsets: [f64; 2],
}

/// Immutable primary-frustum planes prepared for repeated world-space queries.
///
/// Owns its six normalized planes without scene borrows or allocations. Prepare
/// from a [`ComputedCamera`] with [`ComputedCamera::prepare_frustum`], then reuse
/// across points and bounds. It does not observe camera, time or scene changes;
/// prepare again from the recomputed camera after such changes. Additional
/// authored clipping planes are excluded, as in the one-shot camera helpers.
#[derive(Clone, Debug)]
pub struct PreparedFrustum {
    planes: [[f64; 4]; 6],
}

impl PreparedFrustum {
    /// Normalized inward world-space planes `[a,b,c,d]`: left, right,
    /// bottom, top, near, far. Nonnegative signed distance is inside.
    #[must_use]
    pub fn planes(&self) -> &[[f64; 4]; 6] {
        &self.planes
    }

    /// Whether a finite world point lies inside all six primary planes.
    #[must_use]
    pub fn contains_world_point(&self, point: [f64; 3]) -> bool {
        point.iter().all(|v| v.is_finite()) && contains_planes(&self.planes, point)
    }

    /// Conservative intersection with an oriented world bound. Rejects a box
    /// wholly outside any primary plane; crossing boxes can be false positives.
    /// Empty bounds do not intersect. Uses the same transformed-corner test as
    /// [`ComputedCamera::intersects_world_bound`], without rebuilding the planes.
    #[must_use]
    pub fn intersects_world_bound(&self, bound: &BoundingBox) -> bool {
        !bound.range.is_empty() && intersects_planes(&self.planes, bound)
    }
}

impl ComputedCamera {
    /// Perspective or orthographic projection model.
    #[must_use]
    pub fn projection(&self) -> &CameraProjection {
        &self.projection
    }
    /// Authored camera-local to world transform, including any scale/shear.
    #[must_use]
    pub fn camera_to_world(&self) -> &Matrix {
        &self.camera_to_world
    }
    /// Conformed world-to-eye transform, matching `GfFrustum::ComputeViewMatrix`.
    #[must_use]
    pub fn view_matrix(&self) -> &Matrix {
        &self.view
    }
    /// Eye-to-clip projection with USD/OpenGL `[-1,1]` depth. Backend depth-range,
    /// reversed-Z and Y conventions require explicit consumer conversion.
    #[must_use]
    pub fn projection_matrix(&self) -> &Matrix {
        &self.projection_matrix
    }
    /// Row-vector world-to-clip transform: view followed by projection.
    #[must_use]
    pub fn view_projection_matrix(&self) -> Matrix {
        gf::mul(&self.view, &self.projection_matrix)
    }
    /// Reference window `[left,bottom,right,top]`; perspective uses eye depth 1,
    /// orthographic uses scene units. Includes the authored aperture offsets.
    #[must_use]
    pub fn window(&self) -> [f64; 4] {
        self.window
    }
    /// Near/far eye-space distances in scene units.
    #[must_use]
    pub fn clipping_range(&self) -> [f64; 2] {
        self.clipping_range
    }
    /// Authored aperture width/height in tenths of a scene unit.
    #[must_use]
    pub fn aperture(&self) -> [f32; 2] {
        self.aperture
    }
    /// Authored horizontal/vertical aperture offsets in the same units.
    #[must_use]
    pub fn aperture_offset(&self) -> [f32; 2] {
        self.aperture_offset
    }
    /// Focal length in tenths of a scene unit; zero preserves `GfCamera`'s
    /// undivided reference window rather than performing division by zero.
    #[must_use]
    pub fn focal_length(&self) -> f32 {
        self.focal_length
    }
    /// Additional camera-space plane equations, retained for consumer execution.
    /// A point is clipped when `a*x+b*y+c*z+d < 0`. Primary frustum helpers below
    /// do not apply these arbitrary authored planes.
    #[must_use]
    pub fn clipping_planes(&self) -> &[[f32; 4]] {
        &self.clipping_planes
    }
    /// Authored lens aperture; zero disables depth of field.
    #[must_use]
    pub fn f_stop(&self) -> f32 {
        self.f_stop
    }
    /// Authored focus distance in scene units.
    #[must_use]
    pub fn focus_distance(&self) -> f32 {
        self.focus_distance
    }
    /// Frame-relative shutter open/close offsets, in time-code units.
    #[must_use]
    pub fn shutter_offsets(&self) -> [f64; 2] {
        self.shutter_offsets
    }
    /// Adds shutter offsets to a finite frame time. Reversed intervals remain
    /// reversed (USD defines them as no exposure); no sampling policy is chosen.
    pub fn shutter_interval(&self, frame: f64) -> Result<[f64; 2], CameraError> {
        let interval = self.shutter_offsets.map(|t| frame + t);
        if !frame.is_finite() || interval.iter().any(|t| !t.is_finite()) {
            return Err(CameraError::InvalidTime);
        }
        Ok(interval)
    }
    /// Eight world-space corners: near then far, each in lower-left,
    /// lower-right, upper-left, upper-right order, as `GfFrustum::ComputeCorners`.
    #[must_use]
    pub fn frustum_corners(&self) -> [[f64; 3]; 8] {
        core::array::from_fn(|i| {
            let distance = self.clipping_range[i / 4];
            let scale = if self.projection == CameraProjection::Perspective {
                distance
            } else {
                1.
            };
            transform(
                [
                    self.window[if i % 2 == 0 { 0 } else { 2 }] * scale,
                    self.window[if i % 4 < 2 { 1 } else { 3 }] * scale,
                    -distance,
                ],
                &self.view_inverse,
            )
        })
    }
    /// Six normalized inward world-space planes `[a,b,c,d]`: left, right,
    /// bottom, top, near, far. Nonnegative signed distance is inside.
    #[must_use]
    pub fn frustum_planes(&self) -> [[f64; 4]; 6] {
        let [l, b, r, t] = self.window;
        let [n, f] = self.clipping_range;
        // Derive from the window rather than subtracting projection columns:
        // large far/near ratios can round the far plane's normal to zero.
        let eye = if self.projection == CameraProjection::Perspective {
            [
                [1., 0., l, 0.],
                [-1., 0., -r, 0.],
                [0., 1., b, 0.],
                [0., -1., -t, 0.],
                [0., 0., -1., -n],
                [0., 0., 1., f],
            ]
        } else {
            [
                [1., 0., 0., -l],
                [-1., 0., 0., r],
                [0., 1., 0., -b],
                [0., -1., 0., t],
                [0., 0., -1., -n],
                [0., 0., 1., f],
            ]
        };
        eye.map(|p| {
            let length = libm::sqrt(p[0] * p[0] + p[1] * p[1] + p[2] * p[2]);
            let p = p.map(|v| v / length);
            // Row-vector world-to-eye transform acts on plane coefficients
            // as a matrix times a column vector.
            let world: [f64; 4] =
                core::array::from_fn(|i| (0..4).map(|j| self.view[i][j] * p[j]).sum());
            let length =
                libm::sqrt(world[0] * world[0] + world[1] * world[1] + world[2] * world[2]);
            world.map(|v| v / length)
        })
    }
    /// Prepare the primary world-space planes once for repeated point and bound
    /// queries. The owned result retains this camera snapshot's planes; it does
    /// not observe subsequent scene edits or apply additional clipping planes.
    #[must_use]
    #[inline]
    pub fn prepare_frustum(&self) -> PreparedFrustum {
        PreparedFrustum {
            planes: self.frustum_planes(),
        }
    }
    /// Whether a finite world point lies inside the six primary frustum planes.
    /// Use [`Self::prepare_frustum`] to reuse planes across multiple queries.
    #[must_use]
    pub fn contains_world_point(&self, point: [f64; 3]) -> bool {
        point.iter().all(|v| v.is_finite()) && contains_planes(&self.frustum_planes(), point)
    }
    /// Conservative intersection with an oriented world bound. Rejects a box
    /// wholly outside any primary plane; crossing boxes can be false positives.
    /// Empty bounds do not intersect. Additional clipping planes are excluded.
    /// Use [`Self::prepare_frustum`] to reuse planes across multiple queries.
    #[must_use]
    pub fn intersects_world_bound(&self, bound: &BoundingBox) -> bool {
        !bound.range.is_empty() && intersects_planes(&self.frustum_planes(), bound)
    }
}
impl Camera<'_> {
    /// Resolves a camera at `xforms.time()` using its shared ancestor transforms.
    /// Aperture/focal units and projection match `UsdGeomCamera::GetCamera` and
    /// `GfCamera::GetFrustum`; transforms conform as `GfFrustum`. Unknown projection,
    /// unreadable required parameters, nonfinite/singular transforms, nonpositive
    /// apertures, negative focal length and invalid near/far intervals error. Both caches/readers must
    /// use the same scene; pass scene edits to `xforms` before recomputing.
    /// AOUSD Core §6.3, §12.3–12.5; OpenUSD `usdGeom/camera.cpp`, `gf/camera.cpp`.
    #[doc(alias = "UsdGeomCamera::GetCamera")]
    #[doc(alias = "GetCamera")]
    pub fn compute_camera(&self, xforms: &mut XformCache) -> Result<ComputedCamera, CameraError> {
        let time = xforms.time();
        if matches!(time,Time::At {code,..} if !code.is_finite()) {
            return Err(CameraError::InvalidTime);
        }
        let projection = read(self, "projection", time, CameraProjection::read)
            .ok_or(CameraError::InvalidAttribute("projection"))?;
        if !matches!(
            projection,
            CameraProjection::Perspective | CameraProjection::Orthographic
        ) {
            return Err(CameraError::UnsupportedProjection);
        }
        let float = |name| {
            read(self, name, time, crate::value::read_float)
                .filter(|v| v.is_finite())
                .ok_or(CameraError::InvalidAttribute(name))
        };
        let aperture = [float("horizontalAperture")?, float("verticalAperture")?];
        if aperture.iter().any(|&v| v <= 0.) {
            return Err(CameraError::InvalidAttribute("aperture"));
        }
        let aperture_offset = [
            float("horizontalApertureOffset")?,
            float("verticalApertureOffset")?,
        ];
        let focal_length = float("focalLength")?;
        if focal_length < 0. {
            return Err(CameraError::InvalidAttribute("focalLength"));
        }
        let clipping = read(self, "clippingRange", time, crate::value::read_float2)
            .ok_or(CameraError::InvalidAttribute("clippingRange"))?;
        let clipping_range = clipping.map(f64::from);
        if clipping_range.iter().any(|v| !v.is_finite())
            || clipping_range[0] >= clipping_range[1]
            || (projection == CameraProjection::Perspective && clipping_range[0] <= 0.)
        {
            return Err(CameraError::InvalidAttribute("clippingRange"));
        }
        let camera_to_world = xforms
            .local_to_world(&self.scene(), self.path())
            .ok_or(CameraError::InvalidTransform)?;
        if camera_to_world.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CameraError::InvalidTransform);
        }
        let mut linear = gf::IDENTITY;
        for i in 0..3 {
            linear[i][..3].copy_from_slice(&camera_to_world[i][..3]);
        }
        let (_, det) = gf::inverse(&linear);
        if !det.is_finite() || det == 0. {
            return Err(CameraError::InvalidTransform);
        }
        if det < 0. {
            linear[0] = linear[0].map(|v| -v);
        }
        let rows = core::array::from_fn(|i| core::array::from_fn(|j| linear[i][j]));
        let q = gf::decomposition::quaternion(&gf::decomposition::orthonormalize(rows));
        let rotation = gf::Rotation::from_quat([q[1], q[2], q[3], q[0]]);
        let w = camera_to_world[3][3];
        let position = core::array::from_fn(|i| {
            if w != 1. && w.abs() > 1e-10 {
                camera_to_world[3][i] / w
            } else {
                camera_to_world[3][i]
            }
        });
        let view = gf::mul(
            &gf::translate(position.map(|v| -v)),
            &rotation.inverse().matrix(),
        );
        let (view_inverse, det) = gf::inverse(&view);
        if det == 0. || view.iter().flatten().any(|v| !v.is_finite()) {
            return Err(CameraError::InvalidTransform);
        }
        // GfCamera halves authored float apertures before widening to double.
        let half = aperture.map(|v| f64::from(v / 2.));
        let offset = aperture_offset.map(f64::from);
        let mut window = [
            -half[0] + offset[0],
            -half[1] + offset[1],
            half[0] + offset[0],
            half[1] + offset[1],
        ]
        .map(|v| v * 0.1);
        if projection == CameraProjection::Perspective && focal_length != 0. {
            window = window.map(|v| v / (f64::from(focal_length) * 0.1));
        }
        let [l, b, r, t] = window;
        let [n, f] = clipping_range;
        let rl = r - l;
        let tb = t - b;
        let fn_ = f - n;
        let mut projection_matrix = gf::IDENTITY;
        projection_matrix[0][0] = 2. / rl;
        projection_matrix[1][1] = 2. / tb;
        if projection == CameraProjection::Orthographic {
            projection_matrix[2][2] = -2. / fn_;
            projection_matrix[3][0] = -(r + l) / rl;
            projection_matrix[3][1] = -(t + b) / tb;
            projection_matrix[3][2] = -(f + n) / fn_;
        } else {
            projection_matrix[2][2] = -(f + n) / fn_;
            projection_matrix[2][0] = (r + l) / rl;
            projection_matrix[2][1] = (t + b) / tb;
            projection_matrix[3][2] = -2. * n * f / fn_;
            projection_matrix[2][3] = -1.;
            projection_matrix[3][3] = 0.;
        }
        if gf::mul(&view, &projection_matrix)
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
        {
            return Err(CameraError::InvalidTransform);
        }
        let double = |name| {
            read(self, name, time, crate::value::read_double)
                .filter(|v| v.is_finite())
                .ok_or(CameraError::InvalidAttribute(name))
        };
        let camera = ComputedCamera {
            projection,
            camera_to_world,
            view,
            view_inverse,
            projection_matrix,
            window,
            clipping_range,
            aperture,
            aperture_offset,
            focal_length,
            clipping_planes: read(
                self,
                "clippingPlanes",
                time,
                crate::value::read_float4_array,
            )
            .unwrap_or_default(),
            f_stop: float("fStop")?,
            focus_distance: float("focusDistance")?,
            shutter_offsets: [double("shutter:open")?, double("shutter:close")?],
        };
        if camera
            .frustum_corners()
            .iter()
            .flatten()
            .any(|v| !v.is_finite())
            || camera
                .frustum_planes()
                .iter()
                .flatten()
                .any(|v| !v.is_finite())
        {
            return Err(CameraError::InvalidTransform);
        }
        Ok(camera)
    }
}

/// A whole-camera edit could not be collected. Failed edits preserve the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CameraWriteError {
    /// Destination is not an existing composed Camera with schema definitions.
    InvalidDestination,
    /// The destination parent transform is missing, invalid or noninvertible.
    InvalidParentTransform,
    /// A retained camera contains nonfinite additional clipping planes.
    InvalidClippingPlanes,
    /// A field's prospective authoring site could not be inspected.
    Inspection {
        /// Camera attribute being inspected.
        attribute: &'static str,
        /// The underlying inspection failure.
        error: layerstack::AuthoringInspectionError,
    },
    /// The edit target does not contribute at the camera's site.
    InactiveTarget {
        /// Camera attribute being inspected.
        attribute: &'static str,
    },
    /// A stronger value-bearing source could alter the replacement.
    StrongerSource {
        /// Camera attribute being inspected.
        attribute: &'static str,
        /// The stronger source and its composition provenance.
        source: layerstack::AuthoringSource,
    },
    /// A destination property has an incompatible or missing type declaration.
    IncompatibleType {
        /// Camera attribute being inspected.
        attribute: &'static str,
    },
    /// The guarded matrix replacement failed.
    Transform(crate::MatrixXformError),
}
impl core::fmt::Display for CameraWriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "cannot author whole camera: {self:?}")
    }
}
impl core::error::Error for CameraWriteError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Inspection { error, .. } => Some(error),
            Self::Transform(error) => Some(error),
            _ => None,
        }
    }
}

impl crate::usd_geom::CameraEdit {
    /// Collects a retained camera's world transform and lens fields atomically.
    ///
    /// Requires an existing composed Camera and its schema registry. Converts the
    /// supplied world transform through the destination's composed parent, then
    /// replaces its local op order with one matrix op, clearing any reset marker.
    /// Copies projection, apertures/offsets, focal length, clipping range/planes,
    /// f-stop, focus distance and shutter offsets. Other attributes are retained.
    /// Defaults change without removing numeric samples. Raw setters remain
    /// available for deliberately masked or partial edits.
    ///
    /// All destinations are checked for stronger sources and compatible types
    /// before collecting lens values. Parent transforms come from the composed
    /// snapshot; apply pending ancestor edits/definitions first. Parent evaluation
    /// may decode transform values. Rejection leaves prior batch edits intact.
    /// OpenUSD `UsdGeomCamera::SetFromCamera` uses the same world-to-parent matrix;
    /// this helper additionally guards every field and includes shutter offsets.
    /// AOUSD Core §12.3 (value strength), §13.3.1 (transformable schemas).
    ///
    /// # Errors
    /// Returns [`CameraWriteError`] for invalid destinations, stronger sources,
    /// incompatible declarations, invalid parent transforms or clipping planes.
    pub fn set_from_camera(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        camera: &ComputedCamera,
    ) -> Result<(), CameraWriteError> {
        self.set_from_camera_impl(edit, None, camera)
    }

    /// As [`Self::set_from_camera`], replacing one sample at stage `time`.
    /// The transform order is authored at default time; lens fields, matrix and
    /// shutter offsets use the numeric slot. Existing other samples are retained.
    ///
    /// # Errors
    /// As [`Self::set_from_camera`]; nonfinite numeric times are rejected too.
    pub fn set_from_camera_at(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        time: f64,
        camera: &ComputedCamera,
    ) -> Result<(), CameraWriteError> {
        self.set_from_camera_impl(edit, Some(time), camera)
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "computed clipping range is widened from the original authored float2"
    )]
    fn set_from_camera_impl(
        &self,
        edit: &mut crate::SchemaEdit<'_>,
        time: Option<f64>,
        camera: &ComputedCamera,
    ) -> Result<(), CameraWriteError> {
        edit.group(|edit| {
            let query = time.map_or(Time::Default, Time::at);
            for (attribute, expected) in [
                ("projection", "token"),
                ("horizontalAperture", "float"),
                ("verticalAperture", "float"),
                ("horizontalApertureOffset", "float"),
                ("verticalApertureOffset", "float"),
                ("focalLength", "float"),
                ("clippingRange", "float2"),
                ("clippingPlanes", "float4[]"),
                ("fStop", "float"),
                ("focusDistance", "float"),
                ("shutter:open", "double"),
                ("shutter:close", "double"),
            ] {
                let inspection = edit
                    .inspect_authoring(self.path(), attribute, query)
                    .map_err(|error| CameraWriteError::Inspection { attribute, error })?;
                if inspection.target.is_none() {
                    return Err(CameraWriteError::InactiveTarget { attribute });
                }
                if let Some(source) = inspection.stronger_sources.into_iter().next() {
                    return Err(CameraWriteError::StrongerSource { attribute, source });
                }
                if !edit
                    .attribute_type(self.path(), attribute)
                    .is_some_and(|ty| ty.type_name() == expected)
                {
                    return Err(CameraWriteError::IncompatibleType { attribute });
                }
            }
            if camera
                .clipping_planes
                .iter()
                .flatten()
                .any(|v| !v.is_finite())
            {
                return Err(CameraWriteError::InvalidClippingPlanes);
            }
            let local = {
                let scene = edit.scene();
                if Camera::new(&scene, self.path()).is_none() {
                    return Err(CameraWriteError::InvalidDestination);
                }
                let mut cache = XformCache::new(query);
                let mut ancestor = scene.parent(self.path());
                while let Some(path) = ancestor {
                    let local = cache
                        .local_transform(&scene, path)
                        .ok_or(CameraWriteError::InvalidParentTransform)?;
                    if !local.problems.is_empty() {
                        return Err(CameraWriteError::InvalidParentTransform);
                    }
                    if local.resets_xform_stack {
                        break;
                    }
                    ancestor = scene.parent(path);
                }
                let parent = cache
                    .parent_to_world(&scene, self.path())
                    .ok_or(CameraWriteError::InvalidParentTransform)?;
                let (inverse, determinant) = gf::inverse(&parent);
                if !determinant.is_finite()
                    || determinant == 0.
                    || inverse.iter().flatten().any(|v| !v.is_finite())
                {
                    return Err(CameraWriteError::InvalidParentTransform);
                }
                gf::mul(&camera.camera_to_world, &inverse)
            };
            match time {
                Some(time) => self.set_matrix_transform_at(edit, time, local),
                None => self.set_matrix_transform(edit, local),
            }
            .map_err(CameraWriteError::Transform)?;
            let projection = camera.projection.clone();
            let projection = CameraProjection::write(projection, edit.tokens());
            let planes = crate::value::write_float4_array(&camera.clipping_planes, edit.tokens());
            for (name, value) in [
                ("projection", projection),
                ("horizontalAperture", Value::Float(camera.aperture[0])),
                ("verticalAperture", Value::Float(camera.aperture[1])),
                (
                    "horizontalApertureOffset",
                    Value::Float(camera.aperture_offset[0]),
                ),
                (
                    "verticalApertureOffset",
                    Value::Float(camera.aperture_offset[1]),
                ),
                ("focalLength", Value::Float(camera.focal_length)),
                (
                    "clippingRange",
                    Value::Vec2f(camera.clipping_range.map(|v| v as f32)),
                ),
                ("clippingPlanes", planes),
                ("fStop", Value::Float(camera.f_stop)),
                ("focusDistance", Value::Float(camera.focus_distance)),
                ("shutter:open", Value::Double(camera.shutter_offsets[0])),
                ("shutter:close", Value::Double(camera.shutter_offsets[1])),
            ] {
                edit.set_value(self.path(), name, time, value);
            }
            Ok(())
        })
    }
}
