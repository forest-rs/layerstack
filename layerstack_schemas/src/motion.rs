// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Motion settings inherited through any prim type.

use layerstack::{PathId, TokenInterner, Value};

use crate::{PrimView, Scene, Time, usd_geom::MotionApi};

impl Scene<'_> {
    fn inherited_motion<T: Copy>(
        &self,
        path: PathId,
        name: &str,
        time: Time,
        fallback: T,
        read: impl Fn(&Value, &TokenInterner) -> Option<T>,
    ) -> T {
        // OpenUSD: usdGeom/motionAPI.cpp, _ComputeInheritedMotionAttr.
        // AOUSD Core §12.3 and §12.3.6 govern authored values and blocks;
        // the schema's inheritance policy walks past unreadable opinions.
        if !self.stage().has_prim(path) {
            return fallback;
        }
        let mut at = Some(path);
        while let Some(path) = at {
            at = self.parent(path);
            if at.is_none() {
                break; // The pseudo-root never contributes.
            }
            if !self.has_api(path, MotionApi::SCHEMA, None) {
                continue;
            }
            let prim = PrimView::new(*self, path);
            if !prim.has_authored_value(name) {
                continue;
            }
            let value = match time {
                Time::Default => prim.read_value(name, &read),
                Time::At {
                    code,
                    interpolation,
                } => prim.read_value_at(name, code, interpolation, &read),
            };
            if let Some(value) = value {
                return value;
            }
        }
        fallback
    }

    /// The nearest readable authored `motion:blurScale` on `path` or an
    /// ancestor with `MotionAPI` applied, or `1.0` if none contributes.
    ///
    /// Any existing prim type may inherit motion settings; a missing prim
    /// yields the fallback. Default blocks and
    /// unreadable values continue the ancestor search; sampled-only
    /// attributes can read their schema fallback at [`Time::Default`] and
    /// stop it. A sampled block also stops at the schema fallback under
    /// AOUSD Core §12.3.6 and §16.2.16.3. This deliberately differs from
    /// OpenUSD26.8, which continues the ancestor search after a sampled
    /// block (`sampled-block-drops-fallback`).
    ///
    /// OpenUSD: `UsdGeomMotionAPI::ComputeMotionBlurScale`.
    #[must_use]
    pub fn compute_motion_blur_scale(&self, path: PathId, time: Time) -> f32 {
        self.inherited_motion(
            path,
            MotionApi::MOTION_BLUR_SCALE,
            time,
            1.0,
            crate::value::read_float,
        )
    }

    /// The nearest readable authored `motion:nonlinearSampleCount` on
    /// `path` or an ancestor with `MotionAPI` applied, or `3`.
    ///
    /// Values are not clamped; negative counts remain authored values.
    /// OpenUSD: `UsdGeomMotionAPI::ComputeNonlinearSampleCount`.
    #[must_use]
    pub fn compute_nonlinear_sample_count(&self, path: PathId, time: Time) -> i32 {
        self.inherited_motion(
            path,
            MotionApi::NONLINEAR_SAMPLE_COUNT,
            time,
            3,
            crate::value::read_int,
        )
    }

    /// The nearest readable authored `motion:velocityScale` on `path` or
    /// an ancestor with `MotionAPI` applied, or `1.0`.
    ///
    /// This preserves OpenUSD's deprecated velocity-scale setting for
    /// existing assets; it does not evaluate positions or motion blur.
    /// OpenUSD: `UsdGeomMotionAPI::ComputeVelocityScale`.
    #[must_use]
    pub fn compute_velocity_scale(&self, path: PathId, time: Time) -> f32 {
        self.inherited_motion(
            path,
            MotionApi::VELOCITY_SCALE,
            time,
            1.0,
            crate::value::read_float,
        )
    }
}

impl MotionApi<'_> {
    /// The inherited motion blur scale; see [`Scene::compute_motion_blur_scale`].
    #[must_use]
    pub fn compute_motion_blur_scale(&self, time: Time) -> f32 {
        self.scene().compute_motion_blur_scale(self.path(), time)
    }

    /// The inherited nonlinear sample count; see [`Scene::compute_nonlinear_sample_count`].
    #[must_use]
    pub fn compute_nonlinear_sample_count(&self, time: Time) -> i32 {
        self.scene()
            .compute_nonlinear_sample_count(self.path(), time)
    }

    /// The inherited deprecated velocity scale; see [`Scene::compute_velocity_scale`].
    #[must_use]
    pub fn compute_velocity_scale(&self, time: Time) -> f32 {
        self.scene().compute_velocity_scale(self.path(), time)
    }
}
