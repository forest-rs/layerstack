// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Colours as the editor shows them. USD's colour values are linear unless
// their attribute says otherwise: `primvars:displayColor` is linear Rec.709
// (`UsdGeomPrimvarsAPI`, `displayColor`'s documentation), as are
// `UsdPreviewSurface`'s colour inputs and a light's `inputs:color`. A
// swatch is painted in the page's sRGB, so each channel goes through the
// sRGB transfer function (IEC 61966-2-1): `12.92 c` up to 0.0031308, else
// `1.055 c^(1/2.4) - 0.055`; a picked sRGB colour comes back through its
// inverse. Channels are clamped to [0, 1] for the swatch only: the value
// authored keeps what it had outside that range unless the picker
// replaces it.

/** A linear channel in sRGB, clamped to [0, 1]. */
export function linearToSrgb(linear: number): number {
  const clamped = Math.min(1, Math.max(0, Number.isFinite(linear) ? linear : 0));
  return clamped <= 0.0031308 ? 12.92 * clamped : 1.055 * clamped ** (1 / 2.4) - 0.055;
}

/** An sRGB channel in [0, 1] as linear. */
export function srgbToLinear(srgb: number): number {
  const clamped = Math.min(1, Math.max(0, srgb));
  return clamped <= 0.04045 ? clamped / 12.92 : ((clamped + 0.055) / 1.055) ** 2.4;
}

/** A linear RGB colour as the sRGB `#rrggbb` a swatch paints. */
export function linearToHex(rgb: ArrayLike<number>): string {
  let hex = "#";
  for (let channel = 0; channel < 3; channel++) {
    hex += Math.round(linearToSrgb(Number(rgb[channel] ?? 0)) * 255)
      .toString(16)
      .padStart(2, "0");
  }
  return hex;
}

/** An sRGB `#rrggbb` as linear RGB; `null` for text that is not one. */
export function hexToLinear(hex: string): [number, number, number] | null {
  const match = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (match === null) return null;
  return [srgbToLinear(Number.parseInt(match[1]!, 16) / 255), srgbToLinear(Number.parseInt(match[2]!, 16) / 255), srgbToLinear(Number.parseInt(match[3]!, 16) / 255)];
}

/** A CSS colour for a swatch of linear `rgb` at `alpha` (`displayOpacity`, or a `color4`'s fourth channel). */
export function swatchCss(rgb: ArrayLike<number>, alpha = 1): string {
  const [r, g, b] = [0, 1, 2].map((channel) => Math.round(linearToSrgb(Number(rgb[channel] ?? 0)) * 255));
  const a = Math.min(1, Math.max(0, Number.isFinite(alpha) ? alpha : 1));
  return a >= 1 ? `rgb(${r} ${g} ${b})` : `rgb(${r} ${g} ${b} / ${Number(a.toFixed(3))})`;
}

/**
 * The swatches an array of colours is shown as: each of the first `limit`
 * elements' linear RGB (and alpha from `opacities`, element by element,
 * or its one value for all), read from `values` with `components` numbers
 * per element (3 or 4; a fourth is alpha).
 */
export function swatches(
  values: ArrayLike<number>,
  components: number,
  limit: number,
  opacities: ArrayLike<number> | null = null,
): { readonly rgb: readonly [number, number, number]; readonly alpha: number }[] {
  const count = Math.min(limit, Math.floor(values.length / Math.max(1, components)));
  const out: { rgb: [number, number, number]; alpha: number }[] = [];
  for (let element = 0; element < count; element++) {
    const at = element * components;
    const rgb: [number, number, number] = [Number(values[at]), Number(values[at + 1]), Number(values[at + 2])];
    const own = components > 3 ? Number(values[at + 3]) : 1;
    const opacity = opacities === null || opacities.length === 0 ? 1 : Number(opacities[opacities.length === 1 ? 0 : Math.min(element, opacities.length - 1)]);
    out.push({ rgb, alpha: own * opacity });
  }
  return out;
}
