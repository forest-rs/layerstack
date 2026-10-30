// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Fitting a stage camera to a viewport, as OpenUSD's
// `CameraUtilConformWindow` fits a `GfCamera`
// (`pxr/imaging/cameraUtil/conformWindow.cpp`), and its projection as
// `GfFrustum::ComputeProjectionMatrix` makes it.
import type { Camera, ConformWindowPolicy } from "@forest-rs/layerstack";

/**
 * `camera` with its aperture fitted to `aspect` (width over height) by
 * `policy`, as `CameraUtilConformWindow` fits a `GfCamera` and
 * `layerstack_schemas`'s `ComputedCamera::conform_window` does: the
 * apertures change (stored as 32-bit floats), the offsets and everything
 * else are kept, and the window and projection matrix follow. `fit` and
 * `crop` match vertically or horizontally by comparing the aperture's
 * aspect with `aspect`. What `stage.camera(path, { conform })` returns,
 * for a camera already read.
 */
export function conformCamera(camera: Camera, policy: ConformWindowPolicy, aspect: number): Camera {
  if (policy === "dontConform") return camera;
  const [width, height] = [camera.aperture[0]!, camera.aperture[1]!];
  let resolved = policy;
  if (policy === "fit" || policy === "crop") {
    const own = height === 0 ? 1 : width / height;
    resolved = (policy === "fit") !== own > aspect ? "matchVertically" : "matchHorizontally";
  }
  const aperture = Float64Array.from(
    resolved === "matchHorizontally" ? [width, aspect === 0 ? width : width / aspect] : [height * aspect, height],
    Math.fround,
  );
  // `GfCamera::GetFrustum`: the float apertures halved as floats, the
  // offsets added, tenths of a scene unit to scene units, and for a
  // perspective camera divided by the focal length (depth 1).
  const [hx, hy] = [Math.fround(aperture[0]! / 2), Math.fround(aperture[1]! / 2)];
  const [ox, oy] = [camera.apertureOffset[0]!, camera.apertureOffset[1]!];
  let window = [-hx + ox, -hy + oy, hx + ox, hy + oy].map((v) => v * 0.1);
  const perspective = camera.projection === "perspective";
  if (perspective && camera.focalLength !== 0) window = window.map((v) => v / (camera.focalLength * 0.1));
  return Object.freeze({
    ...camera,
    aperture,
    window: Float64Array.from(window),
    projectionMatrix: projectionMatrix(window, camera.clippingRange, perspective),
  });
}

/** `GfFrustum::ComputeProjectionMatrix`: 16 numbers row by row, OpenGL `[-1, 1]` depth. */
function projectionMatrix(window: readonly number[], clipping: ArrayLike<number>, perspective: boolean): Float64Array {
  const [l, b, r, t] = window as [number, number, number, number];
  const [n, f] = [clipping[0]!, clipping[1]!];
  const m = new Float64Array([1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]);
  m[0] = 2 / (r - l);
  m[5] = 2 / (t - b);
  if (perspective) {
    m[10] = -(f + n) / (f - n);
    m[8] = (r + l) / (r - l);
    m[9] = (t + b) / (t - b);
    m[14] = (-2 * n * f) / (f - n);
    m[11] = -1;
    m[15] = 0;
  } else {
    m[10] = -2 / (f - n);
    m[12] = -(r + l) / (r - l);
    m[13] = -(t + b) / (t - b);
    m[14] = -(f + n) / (f - n);
  }
  return m;
}
