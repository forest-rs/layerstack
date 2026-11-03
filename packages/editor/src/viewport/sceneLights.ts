// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's `UsdLux` lights as Storm draws them, in the three.js light
// intensities that shade the same. Pure numbers, no three.js, so the
// mapping is testable on its own; `viewport3d.ts` builds the three.js
// lights from them.
//
// Storm draws every light but a dome as one `GlfSimpleLight`
// (`HdStLight::_ApproximateAreaLight`, `pxr/imaging/hdSt/light.cpp:119-251`
// in OpenUSD 26.11):
//
// - Its color `c` is `inputs:color` × the blackbody color of
//   `inputs:colorTemperature` (with `inputs:enableColorTemperature`) ×
//   `inputs:intensity` × 2^`inputs:exposure` (exposure clamped to ±50)
//   (lines 131-157), × an area unless `inputs:normalize` is set (lines
//   159-209): π r² for a disk or a sphere, width × height for a rect,
//   length × radius for a cylinder, and for a distant light the solid
//   angle of its cone, 2π (1 − cos(angle / 2)). The area is the authored
//   one: the light's transform does not scale it. A distant light of
//   `angle` 0 is so off unless normalized.
// - Its diffuse and specular are `c` (times `inputs:diffuse` and
//   `inputs:specular`, which three.js lights cannot split, so they are not
//   drawn) (lines 211-240).
// - A distant light shines along its -Z with no falloff; the others are
//   points at the light's origin falling off as 1/d² (lines 250-255, and
//   `Sync`, lines 455-472, for positions and directions;
//   `lightDistanceAttenuation`, `pxr/imaging/hdSt/shaders/
//   simpleLighting.glslfx:298-314`). A rect or a disk is a spot along its
//   -Z of `inputs:shaping:cone:angle` (90° when not authored), with
//   `pow(cos, inputs:shaping:focus)` inside it (lines 242-248;
//   `lightSpotAttenuation`, `simpleLighting.glslfx:317-327`); a sphere or
//   a cylinder lights all around.
// - A `UsdPreviewSurface` reflects `c` × attenuation × albedo / π × NdotL
//   (`previewSurface.glslfx`, lines 195, 286, 438): `c` is the irradiance.
//
// three.js (r155 on) shades a `MeshStandardMaterial` with irradiance ×
// albedo / π too: a `DirectionalLight`'s intensity is its irradiance, a
// `PointLight`'s or `SpotLight`'s (decay 2) its intensity / d². So each
// light is that three.js light with intensity `c`'s largest channel and
// the color `c` over it. A dome adds `c` without an area
// (`_PrepareDomeLight`, lines 261-337), drawn as a `HemisphereLight`
// (`domeLights.ts`). Not drawn: a spot's `focus` falloff (a hard cone, as
// at the default focus 0), `inputs:shaping:cone:softness` (Storm ignores
// it too), mesh and geometry lights (no three.js light of their shape).
import type { LightItem } from "@forest-rs/layerstack_stagehand";

/** A light as Storm draws it, in three.js terms. */
export type StormLight =
  /** A `DirectionalLight` along the light's direction. */
  | { readonly kind: "directional"; readonly color: Rgb; readonly intensity: number }
  /** A `PointLight` (decay 2) at the light's position. */
  | { readonly kind: "point"; readonly color: Rgb; readonly intensity: number }
  /** A `SpotLight` (decay 2) along the light's direction, of half-angle `angle` (radians). */
  | { readonly kind: "spot"; readonly color: Rgb; readonly intensity: number; readonly angle: number }
  /** A `HemisphereLight` adding `intensity` × π of irradiance to a surface facing the dome's pole (`domeLights.ts`). */
  | { readonly kind: "dome"; readonly color: Rgb; readonly intensity: number };

/** A color, its largest channel 1. */
export type Rgb = readonly [number, number, number];

/**
 * What Storm multiplies a light's color by, with `inputs:normalize` off:
 * its authored area, or a distant light's cone's solid angle
 * (`HdStLight::_ApproximateAreaLight`, `light.cpp:159-209`); 1 for a dome.
 */
export function stormArea(geometry: LightItem["geometry"]): number {
  switch (geometry.kind) {
    case "disk":
    case "sphere":
      return Math.PI * geometry.radius * geometry.radius;
    case "rect":
      return geometry.width * geometry.height;
    case "cylinder":
      return geometry.length * geometry.radius;
    case "distant": {
      const angle = (geometry.angle / 180) * Math.PI;
      return 2 * Math.PI * (1 - Math.cos(angle / 2));
    }
    default:
      return 1;
  }
}

/** `light` as Storm draws it; `null` when it draws nothing (hidden, black, a portal, a mesh or geometry light). */
export function stormLight(light: LightItem): StormLight | null {
  if (light.hidden) return null;
  const { color, temperatureColor, intensity, exposure, normalize } = light.emission;
  const geometry = light.geometry;
  if (geometry.kind === "portal" || geometry.kind === "mesh" || geometry.kind === "geometry") return null;
  const area = normalize || geometry.kind === "dome" ? 1 : stormArea(geometry);
  const scale = intensity * 2 ** Math.min(50, Math.max(-50, exposure)) * area;
  const c = [0, 1, 2].map((i) => color[i]! * temperatureColor[i]! * scale);
  const peak = Math.max(...c);
  if (!(peak > 0) || !Number.isFinite(peak)) return null;
  const rgb: Rgb = [c[0]! / peak, c[1]! / peak, c[2]! / peak];
  switch (geometry.kind) {
    case "distant":
      return { kind: "directional", color: rgb, intensity: peak };
    case "sphere":
    case "cylinder":
      return { kind: "point", color: rgb, intensity: peak };
    case "rect":
    case "disk": {
      const cone = light.shaping?.coneAngle ?? 90;
      return { kind: "spot", color: rgb, intensity: peak, angle: (Math.min(90, Math.max(0, cone)) * Math.PI) / 180 };
    }
    case "dome":
      return { kind: "dome", color: rgb, intensity: peak };
  }
}
