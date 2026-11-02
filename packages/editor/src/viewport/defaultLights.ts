// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The lights the viewport adds to a stage's own: usdview's and usdrecord's
// defaults, so a stage looks here as it does there. Pure numbers, no
// three.js, so the choice is testable on its own; `viewport3d.ts` builds
// the three.js lights from them.
//
// What usdview passes `UsdImagingGLEngine::SetLightingState` on every draw
// (`pxr/usdImaging/usdviewq/stageView.py`, `StageView.renderSinglePass`),
// and usdrecord the same (`pxr/usdImaging/usdAppUtils/frameRecorder.cpp`,
// `UsdAppUtilsFrameRecorder::Record`, "Make default material and lighting
// match usdview's defaults"):
//
// - A camera light: a `GlfSimpleLight` at the camera's position (`w` = 1, a
//   point light), moved with the camera on each draw, while View > Lights >
//   "Enable Default Camera Light" is checked, which it is by default
//   (`viewSettingsDataModel.py`, `cameraLightEnabled`, default `True`). Its
//   diffuse and specular are `GlfSimpleLight`'s defaults, white at 1, and
//   its attenuation (1, 0, 0) does not fall off with distance
//   (`pxr/imaging/glf/simpleLight.cpp`, the constructor). usdview zeroes
//   its ambient; usdrecord sets it to the scene ambient.
// - The camera light does not depend on the stage's lights: it is added
//   whether or not the stage has any. Storm's `HdxSimpleLightTask` gathers
//   every light sprim under its include paths, the absolute root by
//   default (`pxr/imaging/hdx/simpleLightTask.h`), the task controller's
//   own light prims (`HdxTaskController::_SetBuiltInLightingState`) among
//   the stage's, so the stage's lights add to it. Unchecking the menu item
//   removes it; scene lights are a separate toggle ("Enable Scene Lights").
// - A scene ambient of 0.01 and a `GlfSimpleMaterial` with ambient 0.2,
//   specular 0.1 and shininess 32 (`viewSettingsDataModel.py`,
//   `DEFAULT_AMBIENT` and `DEFAULT_SPECULAR`; `frameRecorder.cpp`,
//   `SCENE_AMBIENT`, `AMBIENT_DEFAULT`, `SPECULAR_DEFAULT`,
//   `SHININESS_DEFAULT`).
// - A default dome light, off by default ("Enable Default Dome Light",
//   `domeLightEnabled`, default `False`); not drawn here.
//
// How Storm shades with them, and the three.js lights that shade the same:
//
// - A `UsdPreviewSurface` (`pxr/usd/plugin/usdShaders/shaders/
//   previewSurface.glslfx`, `evaluateLight`) reflects a light's diffuse
//   irradiance × `NdotL` × diffuseColor / π, and no ambient at all. A prim
//   without a material is shaded by `simpleLightingMaterial`
//   (`pxr/imaging/glf/shaders/simpleLighting.glslfx`): diffuse × `NdotL` ×
//   light diffuse / π too, plus scene ambient × material ambient (0.002,
//   not scaled by its color) and a Blinn-Phong highlight of 0.1 × `NdotH`³²
//   / π. three.js (r155 on) shades a `MeshStandardMaterial` with
//   `BRDF_Lambert`, irradiance × albedo / π, a `PointLight`'s irradiance
//   being its intensity × `NdotL` (with `decay` 0, constant with
//   distance). So a `PointLight` of intensity 1 at the camera, decay 0,
//   reflects off a Lambertian surface what Storm's camera light does, for
//   every albedo and angle: {@link CAMERA_LIGHT_INTENSITY}.
// - An `AmbientLight` adds intensity × albedo / π. Storm's ambient term is
//   0.01 × 0.2 regardless of albedo, and only for prims without a
//   material; the closest one light does is that term on a white surface:
//   {@link AMBIENT_INTENSITY} = π × 0.01 × 0.2. It is a twentieth of a
//   percent of white: kept for fidelity, not visible.
// - The viewport's AgX tone mapping is left as it is (usdview's default is
//   sRGB without one). A gray of albedo 0.1 to 0.8 lit head on displays
//   within 0.045 (of 1) of Storm's sRGB value, a little brighter; saturated
//   colors desaturate a little.
// - The default material's highlight is not matched: a draw without a
//   material is a `MeshStandardMaterial` of roughness 0.7, as before.

/** usdview's and usdrecord's scene ambient, `(0.01, 0.01, 0.01, 1)`. */
export const SCENE_AMBIENT = 0.01;
/** `GlfSimpleMaterial`'s ambient as usdview and usdrecord set it. */
export const MATERIAL_AMBIENT = 0.2;
/** `GlfSimpleMaterial`'s specular as usdview and usdrecord set it. */
export const MATERIAL_SPECULAR = 0.1;
/** `GlfSimpleMaterial`'s shininess as usdview and usdrecord set it. */
export const MATERIAL_SHININESS = 32;

/**
 * The camera light's three.js `PointLight` intensity (candela, decay 0):
 * 1, as `GlfSimpleLight`'s diffuse of 1 shades a surface in Storm.
 */
export const CAMERA_LIGHT_INTENSITY = 1;

/**
 * The `AmbientLight` intensity that adds Storm's scene ambient term
 * (scene ambient × material ambient) to a white Lambertian surface.
 */
export const AMBIENT_INTENSITY = Math.PI * SCENE_AMBIENT * MATERIAL_AMBIENT;

/** The view settings the default lights follow, as usdview's View > Lights menu has them. */
export interface LightSettings {
  /** "Enable Default Camera Light": on by default. */
  readonly cameraLight: boolean;
}

/** usdview's defaults: the camera light on. */
export const DEFAULT_LIGHT_SETTINGS: LightSettings = { cameraLight: true };

/** The three.js intensities of the lights the viewport adds to the stage's. */
export interface DefaultLights {
  /** The camera light's `PointLight` intensity; 0 when it is off (the light stays, so materials keep their programs). */
  readonly cameraLight: number;
  /** The `AmbientLight` intensity. */
  readonly ambient: number;
}

/**
 * The lights added to the stage's own, as usdview adds them: the camera
 * light when the settings have it, and the scene ambient always. Neither
 * depends on the stage's lights: usdview passes the same ones whatever the
 * stage holds.
 */
export function defaultLights(settings: LightSettings): DefaultLights {
  return {
    cameraLight: settings.cameraLight ? CAMERA_LIGHT_INTENSITY : 0,
    ambient: AMBIENT_INTENSITY,
  };
}
