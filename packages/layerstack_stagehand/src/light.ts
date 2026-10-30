// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Renderer-ready `UsdLux` lights and `UsdGeomCamera`s, as a draw list lists
// them: the TypeScript mirror of `layerstack_stagehand::{LightItem,
// CameraItem}`, read through `@forest-rs/layerstack`.
//
// The quantities follow the `UsdLux` schema documentation of OpenUSD 26.11
// (`pxr/usd/usdLux/schema.usda`: `LightAPI`'s "Quantities and Units",
// `inputs:intensity`, `inputs:exposure`, `inputs:color`,
// `inputs:colorTemperature`, `inputs:normalize`); see {@link LightEmission}.
import type { Camera, Prim, Stage, Value } from "@forest-rs/layerstack";

/** The built-in `UsdLux` light a {@link LightItem} is, by its typed schema. */
export type LightKind = "distant" | "sphere" | "rect" | "disk" | "cylinder" | "dome" | "portal";

/** The typed schemas of each {@link LightKind}, in the order they are asked (`IsA`). */
export const LIGHT_SCHEMAS: readonly (readonly [string, LightKind])[] = [
  ["DistantLight", "distant"],
  ["SphereLight", "sphere"],
  ["RectLight", "rect"],
  ["DiskLight", "disk"],
  ["CylinderLight", "cylinder"],
  ["DomeLight", "dome"],
  ["DomeLight_1", "dome"],
  ["PortalLight", "portal"],
];

/** The kind of `prim`, by `IsA`; `null` for anything else (mesh, volume, geometry and plugin lights are not listed yet). */
export function lightKind(prim: Prim): LightKind | null {
  for (const [schema, kind] of LIGHT_SCHEMAS) {
    if (prim.typeName === schema || prim.isA(schema)) return kind;
  }
  return null;
}

/**
 * A light's emission: its photometric inputs and what they come to.
 *
 * `radiance` is `color * temperatureColor * intensity * 2^exposure /
 * sizeFactor`: luminance in nits (cd/m²), per channel, in the rendering
 * color space, as `UsdLux` defines it.
 */
export interface LightEmission {
  /** `inputs:color`. */
  readonly color: readonly [number, number, number];
  /** `inputs:intensity`: luminance in nits before exposure. */
  readonly intensity: number;
  /** `inputs:exposure`, in stops. */
  readonly exposure: number;
  /** `inputs:normalize`. */
  readonly normalize: boolean;
  /** `inputs:colorTemperature` in kelvin with `inputs:enableColorTemperature`; `null` without. */
  readonly colorTemperature: number | null;
  /** The blackbody color of {@link colorTemperature} (`UsdLuxBlackbodyTemperatureAsRgb`); white without. */
  readonly temperatureColor: readonly [number, number, number];
  /** `inputs:diffuse`: a non-physical multiplier of the diffuse response. */
  readonly diffuse: number;
  /** `inputs:specular`: a non-physical multiplier of the specular response. */
  readonly specular: number;
  /**
   * What the luminance is divided by: 1 without `normalize`; with it, a
   * rect's or disk's world-space area, a sphere's (Thomsen's ellipsoid
   * area over the singular values of the transform) or a cylinder's side
   * (its world length times Ramanujan's perimeter of its world
   * cross-section), a distant light's `π sin²θ` (`π (2 - sin²θ)` past a
   * hemisphere) for `θ` half its angle (1 at angle 0), 1 for a dome or a
   * portal. As `layerstack_stagehand::LightEmission::size_factor`.
   */
  readonly sizeFactor: number;
  /** `color * temperatureColor * intensity * 2^exposure / sizeFactor`. */
  readonly radiance: readonly [number, number, number];
}

/** A light's shape in its own space (scene units), before its transform. */
export type LightGeometry =
  /** `inputs:angle`: the angular diameter, in degrees. */
  | { readonly kind: "distant"; readonly angle: number }
  | { readonly kind: "sphere"; readonly radius: number; readonly treatAsPoint: boolean }
  /** `texture` is `inputs:texture:file` as authored. */
  | { readonly kind: "rect"; readonly width: number; readonly height: number; readonly texture: string | null }
  | { readonly kind: "disk"; readonly radius: number }
  | { readonly kind: "cylinder"; readonly radius: number; readonly length: number; readonly treatAsLine: boolean }
  /** `poleAxis` is `DomeLight_1`'s, `null` for a `DomeLight`. */
  | { readonly kind: "dome"; readonly texture: string | null; readonly format: string; readonly poleAxis: string | null }
  | { readonly kind: "portal"; readonly width: number; readonly height: number };

/** `ShapingAPI`'s controls, as authored. */
export interface LightShaping {
  /** The cone's half-angle, in degrees. */
  readonly coneAngle: number;
  readonly coneSoftness: number;
  readonly focus: number;
  readonly focusTint: readonly [number, number, number];
  /** `inputs:shaping:ies:file` as authored. */
  readonly iesFile: string | null;
  readonly iesAngleScale: number;
  readonly iesNormalize: boolean;
}

/** `ShadowAPI`'s controls, as authored. */
export interface LightShadow {
  readonly enable: boolean;
  readonly color: readonly [number, number, number];
  /** -1 for unlimited. */
  readonly distance: number;
  readonly falloff: number;
  readonly falloffGamma: number;
}

/** A light-linking collection's rule as authored, data only: not evaluated. */
export interface LightLink {
  /** `collection:<name>:includeRoot` (true by the `UsdLux` fallback). */
  readonly includeRoot: boolean;
  readonly includes: readonly string[];
  readonly excludes: readonly string[];
}

/** One light, read at the list's time. */
export interface LightItem {
  readonly path: string;
  readonly kind: LightKind;
  /** Its local-to-world matrix, 16 numbers row by row (`ComputeLocalToWorldTransform`). */
  readonly matrix: Float64Array;
  /** Whether it is invisible; it stays listed. */
  readonly hidden: boolean;
  /** Its world position. */
  readonly position: readonly [number, number, number];
  /** The world direction it emits along: its -Z axis, normalized. */
  readonly direction: readonly [number, number, number];
  readonly emission: LightEmission;
  readonly geometry: LightGeometry;
  /** `ShapingAPI`'s controls, when applied. */
  readonly shaping: LightShaping | null;
  /** `ShadowAPI`'s controls, when applied. */
  readonly shadow: LightShadow | null;
  readonly lightLink: LightLink;
  readonly shadowLink: LightLink;
}

/** One `UsdGeomCamera`, computed at the list's time ({@link Stage.camera}). */
export interface CameraItem {
  readonly path: string;
  /** The camera; `null` when its parameters or transform make none ({@link error} says why). */
  readonly camera: Camera | null;
  /** Why it makes no camera (`camera.invalid`'s message), or `null`. */
  readonly error: string | null;
}

/** The attributes a light is read from, besides its transform and visibility. */
export const LIGHT_INPUTS = [
  "inputs:color",
  "inputs:intensity",
  "inputs:exposure",
  "inputs:normalize",
  "inputs:enableColorTemperature",
  "inputs:colorTemperature",
  "inputs:diffuse",
  "inputs:specular",
  "inputs:angle",
  "inputs:radius",
  "inputs:width",
  "inputs:height",
  "inputs:length",
  "inputs:texture:file",
  "inputs:texture:format",
  "inputs:shaping:cone:angle",
  "inputs:shaping:cone:softness",
  "inputs:shaping:focus",
  "inputs:shaping:focusTint",
  "inputs:shaping:ies:file",
  "inputs:shaping:ies:angleScale",
  "inputs:shaping:ies:normalize",
  "inputs:shadow:enable",
  "inputs:shadow:color",
  "inputs:shadow:distance",
  "inputs:shadow:falloff",
  "inputs:shadow:falloffGamma",
];

/** The attributes a camera is read from, besides its transform. */
export const CAMERA_INPUTS = [
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

type Vec3 = readonly [number, number, number];

/**
 * Reads the light at `path` of `kind` at `time`, given its world matrix
 * and visibility, as `layerstack_stagehand` reads it: inputs with the
 * schema fallbacks, a connected input through its connections
 * (`GetValueProducingAttributes`): one attribute with a value is read (the
 * input's own when every connection fails, as OpenUSD falls back to it); a
 * shader output or several sources read nothing, so the input takes its
 * fallback. The prims the connections inspected are added to `sources`.
 */
export function readLight(
  stage: Stage,
  path: string,
  kind: LightKind,
  time: number | undefined,
  matrix: Float64Array,
  hidden: boolean,
  sources: string[] = [],
): LightItem {
  const source = (name: string): string | null => {
    const own = `${path}.${name}`;
    if (!name.startsWith("inputs:")) return own;
    const port = stage.port(own);
    if (port === null || (port.connections.length === 0 && port.invalidConnections.length === 0)) return own;
    const found = port.valueSources();
    const seen = new Set(sources);
    for (const producer of found.sources) {
      for (const step of [...producer.chain, producer.attribute]) {
        const prim = step.slice(0, step.lastIndexOf("."));
        if (prim !== path && !seen.has(prim)) {
          seen.add(prim);
          sources.push(prim);
        }
      }
    }
    for (const target of [...port.connections, ...port.invalidConnections]) {
      const prim = target.slice(0, target.lastIndexOf("."));
      if (prim !== path && !seen.has(prim)) {
        seen.add(prim);
        sources.push(prim);
      }
    }
    const [one, ...more] = found.sources;
    return one !== undefined && more.length === 0 && one.kind === "authoredValue" ? one.attribute : null;
  };
  const get = (name: string): Value | null => {
    const at = source(name);
    const value = at === null ? null : (stage.attribute(at)?.get(time) ?? null);
    if (value?.kind === "array") {
      value.array.release();
      return null;
    }
    return value;
  };
  const float = (name: string, fallback: number): number => {
    const value = get(name);
    return value?.kind === "scalar" && typeof value.value === "number" && Number.isFinite(value.value) ? value.value : fallback;
  };
  const boolean = (name: string, fallback: boolean): boolean => {
    const value = get(name);
    return value?.kind === "scalar" && typeof value.value === "boolean" ? value.value : fallback;
  };
  const color = (name: string, fallback: Vec3): Vec3 => {
    const value = get(name);
    return value?.kind === "tuple" && value.value.length === 3 ? [value.value[0]!, value.value[1]!, value.value[2]!] : fallback;
  };
  const text = (name: string, type: string): string | null => {
    const value = get(name);
    return value?.kind === "scalar" && value.type === type && typeof value.value === "string" ? value.value : null;
  };
  const asset = (name: string): string | null => {
    const value = text(name, "asset");
    return value === "" ? null : value;
  };
  const prim = stage.prim(path)!;
  let geometry: LightGeometry;
  switch (kind) {
    case "distant":
      geometry = { kind, angle: float("inputs:angle", 0.53) };
      break;
    case "sphere":
      geometry = { kind, radius: float("inputs:radius", 0.5), treatAsPoint: boolean("treatAsPoint", false) };
      break;
    case "rect":
      geometry = { kind, width: float("inputs:width", 1), height: float("inputs:height", 1), texture: asset("inputs:texture:file") };
      break;
    case "disk":
      geometry = { kind, radius: float("inputs:radius", 0.5) };
      break;
    case "cylinder":
      geometry = {
        kind,
        radius: float("inputs:radius", 0.5),
        length: float("inputs:length", 1),
        treatAsLine: boolean("treatAsLine", false),
      };
      break;
    case "dome":
      geometry = {
        kind,
        texture: asset("inputs:texture:file"),
        format: text("inputs:texture:format", "token") ?? "automatic",
        poleAxis: prim.typeName === "DomeLight_1" || prim.isA("DomeLight_1") ? (text("poleAxis", "token") ?? "scene") : null,
      };
      break;
    case "portal":
      geometry = { kind, width: float("inputs:width", 1), height: float("inputs:height", 1) };
      break;
  }
  Object.freeze(geometry);
  const colorTemperature = boolean("inputs:enableColorTemperature", false) ? float("inputs:colorTemperature", 6500) : null;
  let temperatureColor: Vec3 = [1, 1, 1];
  if (colorTemperature !== null && Number.isFinite(Math.fround(colorTemperature))) {
    const [r, g, b] = stage.layerstack.blackbodyTemperatureRgb(colorTemperature);
    temperatureColor = [r!, g!, b!];
  }
  const normalize = boolean("inputs:normalize", false);
  const intensity = float("inputs:intensity", kind === "distant" ? 50000 : 1);
  const exposure = float("inputs:exposure", 0);
  const emissionColor = color("inputs:color", [1, 1, 1]);
  const sizeFactor = normalize ? lightSizeFactor(geometry, matrix) : 1;
  const scalar = (intensity * 2 ** exposure) / sizeFactor;
  const emission: LightEmission = Object.freeze({
    color: emissionColor,
    intensity,
    exposure,
    normalize,
    colorTemperature,
    temperatureColor,
    diffuse: float("inputs:diffuse", 1),
    specular: float("inputs:specular", 1),
    sizeFactor,
    radiance: [0, 1, 2].map((i) => emissionColor[i]! * temperatureColor[i]! * scalar) as unknown as Vec3,
  });
  const shaping: LightShaping | null = prim.hasAPI("ShapingAPI")
    ? Object.freeze({
        coneAngle: float("inputs:shaping:cone:angle", 90),
        coneSoftness: float("inputs:shaping:cone:softness", 0),
        focus: float("inputs:shaping:focus", 0),
        focusTint: color("inputs:shaping:focusTint", [0, 0, 0]),
        iesFile: asset("inputs:shaping:ies:file"),
        iesAngleScale: float("inputs:shaping:ies:angleScale", 0),
        iesNormalize: boolean("inputs:shaping:ies:normalize", false),
      })
    : null;
  const shadow: LightShadow | null = prim.hasAPI("ShadowAPI")
    ? Object.freeze({
        enable: boolean("inputs:shadow:enable", true),
        color: color("inputs:shadow:color", [0, 0, 0]),
        distance: float("inputs:shadow:distance", -1),
        falloff: float("inputs:shadow:falloff", -1),
        falloffGamma: float("inputs:shadow:falloffGamma", 1),
      })
    : null;
  const link = (name: string): LightLink =>
    Object.freeze({
      includeRoot: boolean(`collection:${name}:includeRoot`, true),
      includes: stage.relationship(`${path}.collection:${name}:includes`)?.targets() ?? [],
      excludes: stage.relationship(`${path}.collection:${name}:excludes`)?.targets() ?? [],
    });
  return Object.freeze({
    path,
    kind,
    matrix,
    hidden,
    position: [matrix[12]!, matrix[13]!, matrix[14]!] as const,
    direction: normalized([-matrix[8]!, -matrix[9]!, -matrix[10]!]),
    emission,
    geometry,
    shaping,
    shadow,
    lightLink: link("lightLink"),
    shadowLink: link("shadowLink"),
  });
}

const dot = (a: Vec3, b: Vec3) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
const cross = (a: Vec3, b: Vec3): Vec3 => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
const length = (a: Vec3) => Math.sqrt(dot(a, a));
const sub = (a: Vec3, b: Vec3): Vec3 => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
const scale = (a: Vec3, s: number): Vec3 => [a[0] * s, a[1] * s, a[2] * s];

function normalized(a: Vec3): Vec3 {
  const l = length(a);
  return l > 0 ? [a[0] / l, a[1] / l, a[2] / l] : [0, 0, 0];
}

/** The `sizeFactor` of a normalized light ({@link LightEmission.sizeFactor}), as `layerstack_stagehand` computes it. */
export function lightSizeFactor(geometry: LightGeometry, matrix: ArrayLike<number>): number {
  const axis = (row: number): Vec3 => [matrix[4 * row]!, matrix[4 * row + 1]!, matrix[4 * row + 2]!];
  const [x, y, z] = [axis(0), axis(1), axis(2)];
  let factor: number;
  switch (geometry.kind) {
    case "rect":
      factor = geometry.width * geometry.height * length(cross(x, y));
      break;
    case "disk":
      factor = Math.PI * geometry.radius * geometry.radius * length(cross(x, y));
      break;
    case "sphere": {
      const [a, b, c] = singularValues([x, y, z]).map((s) => s * geometry.radius) as [number, number, number];
      factor = ellipsoidArea(a, b, c);
      break;
    }
    case "cylinder": {
      const along = length(x);
      if (along === 0) {
        factor = 0;
        break;
      }
      const unit = scale(x, 1 / along);
      const [a, b] = singularValues2(sub(y, scale(unit, dot(y, unit))), sub(z, scale(unit, dot(z, unit)))).map(
        (s) => s * geometry.radius,
      ) as [number, number];
      factor = geometry.length * along * ellipsePerimeter(a, b);
      break;
    }
    case "distant": {
      const theta = Math.min(Math.max(((geometry.angle * Math.PI) / 180) / 2, 0), Math.PI);
      const sin2 = Math.sin(theta) ** 2;
      factor = theta === 0 ? 1 : theta <= Math.PI / 2 ? sin2 * Math.PI : (2 - sin2) * Math.PI;
      break;
    }
    case "dome":
    case "portal":
      factor = 1;
      break;
  }
  return factor > 0 && Number.isFinite(factor) ? factor : 1;
}

/** The singular values of the 3x3 matrix of `rows`, largest first (the symmetric eigenproblem of its Gram matrix, solved trigonometrically). */
function singularValues(rows: readonly [Vec3, Vec3, Vec3]): number[] {
  const g = (i: number, j: number) => dot(rows[i]!, rows[j]!);
  let [a, b, c] = [g(0, 0), g(1, 1), g(2, 2)];
  let [d, e, f] = [g(0, 1), g(1, 2), g(0, 2)];
  const off = d * d + e * e + f * f;
  let eigen: number[];
  if (off === 0) {
    eigen = [a, b, c].sort((p, q) => q - p);
  } else {
    const q = (a + b + c) / 3;
    const p = Math.sqrt(((a - q) * (a - q) + (b - q) * (b - q) + (c - q) * (c - q) + 2 * off) / 6);
    [a, b, c] = [(a - q) / p, (b - q) / p, (c - q) / p];
    [d, e, f] = [d / p, e / p, f / p];
    const r = Math.min(Math.max((a * (b * c - e * e) - d * (d * c - e * f) + f * (d * e - b * f)) / 2, -1), 1);
    const phi = Math.acos(r) / 3;
    const first = q + 2 * p * Math.cos(phi);
    const third = q + 2 * p * Math.cos(phi + (2 * Math.PI) / 3);
    eigen = [first, 3 * q - first - third, third];
  }
  return eigen.map((v) => Math.sqrt(Math.max(v, 0)));
}

function singularValues2(u: Vec3, v: Vec3): number[] {
  const [a, b, c] = [dot(u, u), dot(v, v), dot(u, v)];
  const mean = (a + b) / 2;
  const spread = Math.sqrt(((a - b) * (a - b)) / 4 + c * c);
  return [mean + spread, mean - spread].map((value) => Math.sqrt(Math.max(value, 0)));
}

function ellipsoidArea(a: number, b: number, c: number): number {
  const p = 1.6075;
  const [x, y, z] = [a ** p, b ** p, c ** p];
  return 4 * Math.PI * ((x * y + x * z + y * z) / 3) ** (1 / p);
}

function ellipsePerimeter(a: number, b: number): number {
  if (a + b === 0) return 0;
  const h = ((a - b) * (a - b)) / ((a + b) * (a + b));
  return Math.PI * (a + b) * (1 + (3 * h) / (10 + Math.sqrt(4 - 3 * h)));
}
