// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Cameras a draw list lists: `UsdGeomCamera`s computed as OpenUSD's
//! `UsdGeomCamera::GetCamera` and `GfCamera::GetFrustum` compute them.

use layerstack::PathId;
use layerstack_schemas::camera::{CameraError, ComputedCamera};

/// One `UsdGeomCamera`, computed at the list's time.
///
/// [`ComputedCamera`] carries what a renderer needs: the view matrix
/// (world to eye, conformed as `GfFrustum` conforms it), the projection
/// matrix (OpenGL depth `[-1, 1]`), the window and clipping range, the
/// apertures and their offsets and the focal length (tenths of a scene
/// unit), the depth-of-field inputs (`fStop`, `focusDistance`) and the
/// exposure controls. [`ComputedCamera::conform_window`] fits it to a
/// viewport's aspect ratio as `CameraUtilConformWindow` does.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraItem {
    /// The camera prim.
    pub path: PathId,
    /// The camera, or why it makes none (an unknown projection, an
    /// aperture or clipping range that makes no frustum, a singular
    /// transform).
    pub camera: Result<ComputedCamera, CameraError>,
}

/// The attributes a camera is read from, besides its transform: what
/// [`crate::DrawList::set_time`] asks to vary.
pub(crate) const CAMERA_INPUTS: &[&str] = &[
    "projection",
    "horizontalAperture",
    "verticalAperture",
    "horizontalApertureOffset",
    "verticalApertureOffset",
    "focalLength",
    "clippingRange",
    "clippingPlanes",
    "fStop",
    "focusDistance",
    "shutter:open",
    "shutter:close",
    "exposure",
    "exposure:fStop",
    "exposure:iso",
    "exposure:responsivity",
    "exposure:time",
];
