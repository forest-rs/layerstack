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
    /// Whether a finite world point lies inside the six primary frustum planes.
    #[must_use]
    pub fn contains_world_point(&self, point: [f64; 3]) -> bool {
        point.iter().all(|v| v.is_finite())
            && self
                .frustum_planes()
                .iter()
                .all(|p| p[0] * point[0] + p[1] * point[1] + p[2] * point[2] + p[3] >= 0.)
    }
    /// Conservative intersection with an oriented world bound. Rejects a box
    /// wholly outside any primary plane; crossing boxes can be false positives.
    /// Empty bounds do not intersect. Additional clipping planes are excluded.
    #[must_use]
    pub fn intersects_world_bound(&self, bound: &BoundingBox) -> bool {
        if bound.range.is_empty() {
            return false;
        }
        self.frustum_planes().iter().all(|plane| {
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
