// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit sampled camera math without render resources or clip-space adapters.
use crate::diagnostic::{Describe, Names, with_store};
use crate::{
    PrimView, Time, XformCache,
    bounds::BoundingBox,
    gf,
    usd_geom::{Camera, CameraProjection},
};
use alloc::vec::Vec;
use layerstack::{TokenInterner, Value};

mod frustum;
mod ray;

use frustum::{ConformedView, contains_planes, intersects_planes};
pub use frustum::{FieldOfViewAxis, FrustumProjection, Perspective, PreparedFrustum};
pub use ray::{PlaneHit, Ray, RayInterval, TriangleHit};
type Matrix = [[f64; 4]; 4];
/// Invalid camera parameters or transform; computations return no partial camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
        match self {
            Self::InvalidAttribute(name) => write!(
                f,
                "invalid camera: `{name}` has no value of its type, or one that makes no camera"
            ),
            Self::UnsupportedProjection => f.write_str(
                "invalid camera: its `projection` is neither `perspective` nor `orthographic`",
            ),
            Self::InvalidTransform => {
                f.write_str("invalid camera: its transform is missing, singular or not finite")
            }
            Self::InvalidTime => {
                f.write_str("invalid camera: a time, or a shutter end it gives, is not finite")
            }
        }
    }
}
impl core::error::Error for CameraError {}
fn read<'a, T>(
    prim: &PrimView<'a>,
    name: &str,
    time: Time,
    decode: impl Fn(&Value, &'a TokenInterner) -> Option<T>,
) -> Option<T> {
    prim.read_value_at(name, time, decode)
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
    exposure: CameraExposure,
}

/// A camera's exposure controls (`UsdGeomCamera`'s `exposure` and
/// `exposure:*` attributes), each the value read or, when none can be read,
/// the constant `UsdGeomCamera::ComputeLinearExposureScale` starts from (the
/// schema fallbacks).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraExposure {
    /// Exposure adjustment in stops (`exposure`, fallback 0).
    pub exposure: f32,
    /// Lens aperture for exposure (`exposure:fStop`, fallback 1).
    pub f_stop: f32,
    /// Sensor sensitivity (`exposure:iso`, fallback 100).
    pub iso: f32,
    /// Sensor responsivity scale (`exposure:responsivity`, fallback 1).
    pub responsivity: f32,
    /// Exposure time in seconds (`exposure:time`, fallback 1).
    pub time: f32,
}

impl Default for CameraExposure {
    fn default() -> Self {
        Self {
            exposure: 0.,
            f_stop: 1.,
            iso: 100.,
            responsivity: 1.,
            time: 1.,
        }
    }
}

impl CameraExposure {
    /// The scale applied to scene luminance before the image is written:
    /// `time * iso * 2^exposure * responsivity / (100 * fStop^2)`, in `f32`
    /// as OpenUSD computes it, 1 for the fallbacks.
    /// OpenUSD `UsdGeomCamera::ComputeLinearExposureScale`
    /// (`pxr/usd/usdGeom/camera.cpp`).
    #[must_use]
    #[doc(alias = "ComputeLinearExposureScale")]
    pub fn linear_scale(&self) -> f32 {
        (self.time * self.iso * libm::powf(2., self.exposure) * self.responsivity)
            / (100. * self.f_stop * self.f_stop)
    }
}

/// How [`ComputedCamera::conform_window`] fits a camera's aperture to a
/// target aspect ratio (width over height), as OpenUSD's
/// `CameraUtilConformWindowPolicy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConformWindowPolicy {
    /// Keep the height, change the width.
    MatchVertically,
    /// Keep the width, change the height.
    MatchHorizontally,
    /// Grow the width or the height: everything the camera saw stays in view.
    Fit,
    /// Shrink the width or the height: the view is filled, edges cut.
    Crop,
    /// Leave the aperture as it is; the image stretches to the target.
    DontConform,
}

/// The reference window `[left, bottom, right, top]` of a lens, as
/// `GfCamera::GetFrustum` makes it: apertures halved in `f32`, offsets added,
/// tenths of a scene unit to scene units (`GfCamera::APERTURE_UNIT`), and,
/// for a perspective camera, divided by the focal length to depth 1.
fn lens_window(
    projection: &CameraProjection,
    aperture: [f32; 2],
    aperture_offset: [f32; 2],
    focal_length: f32,
) -> [f64; 4] {
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
    if *projection == CameraProjection::Perspective && focal_length != 0. {
        window = window.map(|v| v / (f64::from(focal_length) * 0.1));
    }
    window
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
    /// The exposure controls, and their linear scale
    /// ([`CameraExposure::linear_scale`]).
    #[must_use]
    pub fn exposure(&self) -> &CameraExposure {
        &self.exposure
    }
    /// The camera with its aperture fitted to `aspect` (width over height),
    /// as OpenUSD's `CameraUtilConformWindow` fits a `GfCamera`: the
    /// apertures change (computed in `f64`, stored as `f32`), the offsets
    /// and everything else are kept, and the window, projection matrix and
    /// frustum follow. `Fit` and `Crop` match vertically or horizontally by
    /// comparing the aperture's aspect (1 for a zero height) with `aspect`;
    /// a zero `aspect` keeps the width when matching horizontally.
    /// OpenUSD `pxr/imaging/cameraUtil/conformWindow.cpp`.
    #[must_use]
    #[doc(alias = "CameraUtilConformWindow")]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "GfCamera stores the conformed apertures as float"
    )]
    pub fn conform_window(&self, policy: ConformWindowPolicy, aspect: f64) -> Self {
        let [width, height] = self.aperture.map(f64::from);
        let resolved = match policy {
            ConformWindowPolicy::DontConform => return self.clone(),
            ConformWindowPolicy::MatchVertically | ConformWindowPolicy::MatchHorizontally => policy,
            ConformWindowPolicy::Fit | ConformWindowPolicy::Crop => {
                let own = if height == 0. { 1. } else { width / height };
                if (policy == ConformWindowPolicy::Fit) ^ (own > aspect) {
                    ConformWindowPolicy::MatchVertically
                } else {
                    ConformWindowPolicy::MatchHorizontally
                }
            }
        };
        let aperture = if resolved == ConformWindowPolicy::MatchHorizontally {
            [width, if aspect == 0. { width } else { width / aspect }]
        } else {
            [height * aspect, height]
        }
        .map(|v| v as f32);
        let window = lens_window(
            &self.projection,
            aperture,
            self.aperture_offset,
            self.focal_length,
        );
        Self {
            aperture,
            window,
            projection_matrix: frustum::projection_matrix(
                window,
                self.clipping_range,
                self.frustum_projection(),
            ),
            ..self.clone()
        }
    }
    /// Eight world-space corners: near then far, each in lower-left,
    /// lower-right, upper-left, upper-right order, as `GfFrustum::ComputeCorners`.
    #[must_use]
    pub fn frustum_corners(&self) -> [[f64; 3]; 8] {
        frustum::corners(
            &self.view_inverse,
            self.window,
            self.clipping_range,
            self.frustum_projection(),
        )
    }
    /// Six normalized inward world-space planes `[a,b,c,d]`: left, right,
    /// bottom, top, near, far. Nonnegative signed distance is inside.
    #[must_use]
    pub fn frustum_planes(&self) -> [[f64; 4]; 6] {
        frustum::planes(
            &self.view,
            self.window,
            self.clipping_range,
            self.frustum_projection(),
        )
    }
    /// Prepare the primary world-space planes once for repeated point and bound
    /// queries. The owned result retains this camera snapshot's planes; it does
    /// not observe subsequent scene edits or apply additional clipping planes.
    #[must_use]
    #[inline]
    pub fn prepare_frustum(&self) -> PreparedFrustum {
        PreparedFrustum::from_view(
            ConformedView {
                view: self.view,
                view_inverse: self.view_inverse,
            },
            self.window,
            self.clipping_range,
            self.frustum_projection(),
        )
    }
    /// The frustum projection; cameras only compute the two `GfFrustum` has.
    fn frustum_projection(&self) -> FrustumProjection {
        if self.projection == CameraProjection::Orthographic {
            FrustumProjection::Orthographic
        } else {
            FrustumProjection::Perspective
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
        let ConformedView { view, view_inverse } =
            frustum::conform(&camera_to_world).ok_or(CameraError::InvalidTransform)?;
        let window = lens_window(&projection, aperture, aperture_offset, focal_length);
        let projection_matrix = frustum::projection_matrix(
            window,
            clipping_range,
            if projection == CameraProjection::Orthographic {
                FrustumProjection::Orthographic
            } else {
                FrustumProjection::Perspective
            },
        );
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
            exposure: read_exposure(self, time),
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

/// The exposure controls at `time`; each one that cannot be read keeps the
/// constant `UsdGeomCamera::ComputeLinearExposureScale` starts from.
fn read_exposure(prim: &PrimView<'_>, time: Time) -> CameraExposure {
    let fallback = CameraExposure::default();
    let float =
        |name, otherwise| read(prim, name, time, crate::value::read_float).unwrap_or(otherwise);
    CameraExposure {
        exposure: float("exposure", fallback.exposure),
        f_stop: float("exposure:fStop", fallback.f_stop),
        iso: float("exposure:iso", fallback.iso),
        responsivity: float("exposure:responsivity", fallback.responsivity),
        time: float("exposure:time", fallback.time),
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
impl CameraWriteError {
    /// The error in words, a stronger source's spec and layer written with
    /// `store`, the store the stage was composed from; a failed inspection through
    /// [`layerstack::AuthoringInspectionError::display`]. `Display` writes the
    /// same with ids.
    #[must_use]
    pub fn display<'a>(
        &'a self,
        store: &'a dyn layerstack::LayerStore,
    ) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for CameraWriteError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        f.write_str("cannot author the whole camera: ")?;
        match self {
            Self::InvalidDestination => f.write_str(
                "the destination is not an existing composed `Camera` with schema definitions",
            ),
            Self::InvalidParentTransform => f.write_str(
                "the destination's parent transform is missing, invalid or not invertible",
            ),
            Self::InvalidClippingPlanes => {
                f.write_str("an additional clipping plane is not finite")
            }
            Self::Inspection { attribute, error } => {
                write!(f, "cannot inspect where `{attribute}` would be authored: ")?;
                match names.0 {
                    Some(store) => write!(f, "{}", error.display(store)),
                    None => write!(f, "{error}"),
                }
            }
            Self::InactiveTarget { attribute } => write!(
                f,
                "the edit target does not contribute to `{attribute}`; target one of the \
                 camera's sources"
            ),
            Self::StrongerSource { attribute, source } => write!(
                f,
                "`{attribute}` has a stronger opinion at {} in {}, which would alter the \
                 replacement",
                names.spec(&source.key.spec_path),
                names.layer(source.key.layer_id)
            ),
            Self::IncompatibleType { attribute } => write!(
                f,
                "`{attribute}` is declared with an incompatible type, or with none"
            ),
            Self::Transform(error) => {
                f.write_str("cannot replace its transform matrix: ")?;
                error.reason(f, names)
            }
        }
    }
}
impl core::fmt::Display for CameraWriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
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
                if Camera::get(&scene, self.path()).is_none() {
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
