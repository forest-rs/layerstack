// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// How the viewport and the previews turn the linear light they draw into
// the display's colours: usdview's default, or AgX for scenes whose
// highlights a plain encoding clips.
//
// usdview's default (`sRGB`) is what Storm presents unless asked for
// OpenColorIO, in OpenUSD 26.11:
//
// - usdview's `colorCorrectionMode` defaults to `ColorCorrectionModes.SRGB`
//   (`pxr/usdImaging/usdviewq/viewSettingsDataModel.py:144`, the modes at
//   `common.py:156-161`), passed to `UsdImagingGLEngine` on every draw
//   (`stageView.py:1174-1175`, `engine.cpp:2405-2431`). OpenColorIO is used
//   only when chosen and `$OCIO` is set (`HdxColorCorrectionTask::_GetUseOcio`,
//   `pxr/imaging/hdx/colorCorrectionTask.cpp:103-119`).
// - In `sRGB` mode `HdxColorCorrectionTask` runs `FloatToSRGB` on each
//   pixel's colour, not its alpha: the piecewise sRGB transfer function
//   (× 12.92 below 0.0031308, else 1.055 x^(1/2.4) − 0.055) and nothing else:
//   no tone curve, no clamp, no dither
//   (`pxr/imaging/hdx/shaders/colorCorrection.glslfx:36-54`).
// - Storm draws into a `Float16Vec4` colour AOV
//   (`HdStRenderDelegate::GetDefaultAovDescriptor`,
//   `pxr/imaging/hdSt/renderDelegate.cpp:382-384`), so light past 1 lives
//   until the corrected image is presented to the window's framebuffer
//   (8 bits a channel, as usdview's), which clips it to white.
// - The camera's exposure scales each surface's shaded colour before it is
//   written (`shadingColor.rgb *= GetLinearExposure()`,
//   `pxr/imaging/hdSt/shaders/terminals.glslfx:104-105`): its
//   `linearExposureScale` (`UsdGeomCamera::ComputeLinearExposureScale`),
//   unless the render delegate's `enableExposureCompensation` setting (on by
//   default) is off (`pxr/imaging/hdSt/renderPassState.cpp:362-365`); 1 for
//   a camera that authors none, as usdview's free camera
//   (`pxr/imaging/hd/camera.cpp:48`).
//
// three.js (r187) draws into a half-float target too and converts it to the
// canvas in an output pass of its own: un-premultiplies, tone-maps, encodes
// the output colour space (sRGB's same piecewise function), premultiplies
// again (`RenderOutputNode`), so a transparent canvas still composes over
// the page. Its `toneMappingExposure` applies only inside a tone-mapping
// operator (`NoToneMapping` would drop the camera's exposure), so usdview's
// default is `LinearToneMapping`: colour × exposure, clamped to [0, 1], then
// sRGB-encoded. The clamp is the 8-bit framebuffer's clip moved before the
// encoding, which leaves every value the same (the encoding is increasing
// and maps 0 to 0 and 1 to 1); the exposure applied to the blended colour
// rather than to each surface's is the same in linear light. AgX is
// `AgXToneMapping` at the same exposure.
//
// Pure, with no three.js (the store reads the choice before three.js
// loads); `gpu.ts`'s `applyToneMap` sets a renderer's output from it.

/** How the viewport and the previews map light to the display: usdview's sRGB encoding, or AgX. */
export type ToneMapChoice = "srgb" | "agx";

/** Each choice, in order, with its label. */
export const TONE_MAP_CHOICES: readonly (readonly [ToneMapChoice, string])[] = [
  ["srgb", "sRGB (usdview)"],
  ["agx", "AgX"],
];

/** usdview's default: sRGB encoding, no tone curve. */
export const DEFAULT_TONE_MAP: ToneMapChoice = "srgb";

/** The renderer's output for a choice: its tone-mapping operator and exposure. */
export interface ToneMapOutput {
  /** `linear` for sRGB (three.js's `LinearToneMapping`: exposure, clamp; then the sRGB encoding), `agx` for AgX (`AgXToneMapping`). */
  readonly operator: "linear" | "agx";
  /** The renderer's `toneMappingExposure`: the camera's linear exposure scale. */
  readonly exposure: number;
}

/**
 * The output `choice` draws with at a camera's `linearExposureScale`
 * (`UsdGeomCamera::ComputeLinearExposureScale`; 1 without a stage camera).
 * A scale that is not a finite number of zero or more is taken as 1, as a
 * camera that authors no exposure.
 */
export function toneMapOutput(choice: ToneMapChoice, linearExposureScale = 1): ToneMapOutput {
  const exposure = Number.isFinite(linearExposureScale) && linearExposureScale >= 0 ? linearExposureScale : 1;
  return { operator: choice === "agx" ? "agx" : "linear", exposure };
}

/** Where a viewer's tone-mapping choice is remembered. */
const TONE_MAP_KEY = "layerstack-editor:tone-map";

/** The tone-mapping choice this viewer made last; the default when none is remembered or storage is unavailable. */
export function rememberedToneMap(): ToneMapChoice {
  try {
    const value = globalThis.localStorage?.getItem(TONE_MAP_KEY) ?? null;
    return TONE_MAP_CHOICES.some(([choice]) => choice === value) ? (value as ToneMapChoice) : DEFAULT_TONE_MAP;
  } catch {
    return DEFAULT_TONE_MAP;
  }
}

/** Remembers `choice` for this viewer, when storage is available. */
export function rememberToneMap(choice: ToneMapChoice): void {
  try {
    globalThis.localStorage?.setItem(TONE_MAP_KEY, choice);
  } catch {
    // Storage blocked: the choice lasts this page.
  }
}
