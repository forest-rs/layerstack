// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// An OpenPBR surface (MaterialX `ND_open_pbr_surface_surfaceshader`) read
// for a preview: each input the preview draws, as a constant or a texture
// lookup, and notes on what it cannot draw.
//
// Each input is followed to its value source
// (`UsdShadeConnectableAPI::GetValueProducingAttributes`, through the
// material's interface): an authored value is a constant; an image node
// (`ND_tiledimage_*`, `ND_image_*`) is a texture lookup, its `file` read
// with the layer that authored it (so a search path resolves beside it)
// and its `default` the constant where the file is empty. MaterialX's
// `tiledimage` samples `texcoord * uvtiling - uvoffset` (the
// `NG_tiledimage_*` graphs of its standard library); the lookup carries
// that as a 2D transform. Any other node is not evaluated: the input
// takes its fallback, and a note says so.
//
// The normal (`geometry_normal`) is the image node found upstream of it
// through `ND_normalmap` (whose `scale` is kept); a channel flip built of
// `ND_ifequal_*` nodes choosing between the image and its `ND_invert` is
// read by evaluating each switch's `value1 == value2`.
import type { Stage, Value } from "@forest-rs/layerstack";
import { anchorAssetPath, isUdim } from "@forest-rs/layerstack_stagehand";
import type { MaterialTexture, TextureColorSpace } from "@forest-rs/layerstack_stagehand";
import { primOf } from "./network.ts";
import { releaseValue } from "./values.ts";

/** The `info:id` of the surface this reads. */
export const OPEN_PBR = "ND_open_pbr_surface_surfaceshader";

/** An input: its constant, or a texture lookup (and the constant where the texture cannot be read). */
export interface PbrInput<T> {
  readonly value: T;
  readonly texture: MaterialTexture | null;
}

type Vec3 = readonly [number, number, number];

/** The inputs of an OpenPBR surface a preview draws, with OpenPBR's defaults where unauthored. */
export interface OpenPbrSurface {
  /** The surface shader's path. */
  readonly shader: string;
  readonly baseWeight: PbrInput<number>;
  readonly baseColor: PbrInput<Vec3>;
  readonly baseMetalness: PbrInput<number>;
  readonly specularWeight: PbrInput<number>;
  readonly specularColor: PbrInput<Vec3>;
  readonly specularRoughness: PbrInput<number>;
  readonly specularAnisotropy: number;
  readonly specularIor: number;
  readonly coatWeight: PbrInput<number>;
  readonly coatRoughness: PbrInput<number>;
  readonly coatIor: number;
  readonly fuzzWeight: number;
  readonly fuzzColor: PbrInput<Vec3>;
  readonly fuzzRoughness: number;
  readonly emissionLuminance: number;
  readonly emissionColor: PbrInput<Vec3>;
  readonly transmissionWeight: PbrInput<number>;
  readonly transmissionColor: Vec3;
  readonly transmissionDepth: number;
  readonly opacity: PbrInput<number>;
  readonly thinFilmWeight: number;
  /** In micrometres. */
  readonly thinFilmThickness: number;
  readonly thinFilmIor: number;
  /** The tangent-space normal map, its strength, and which of its channels are flipped. */
  readonly normal: { readonly texture: MaterialTexture; readonly scale: number; readonly flip: readonly [boolean, boolean, boolean] } | null;
  /** What the preview cannot draw as authored, in words. */
  readonly notes: readonly string[];
}

/** The OpenPBR surface the material at `path` outputs (any render context), or `null` when its surface is not OpenPBR. */
export function readOpenPbr(stage: Stage, path: string, time: number | undefined): OpenPbrSurface | null {
  const material = stage.material(path);
  if (material === null) return null;
  // The surface outputs, universal or of any context (`outputs:mtlx:surface`).
  const shader = material
    .outputs()
    .filter((output) => output.name === "surface" || output.name.endsWith(":surface"))
    .flatMap((output) => output.valueSources().sources)
    .map((source) => primOf(source.attribute))
    .find((prim) => stage.shader(prim)?.id === OPEN_PBR);
  if (shader === undefined) return null;
  const reader = new PbrReader(stage, shader, time);
  return {
    shader,
    baseWeight: reader.number("base_weight", 1),
    baseColor: reader.color("base_color", [0.8, 0.8, 0.8]),
    baseMetalness: reader.number("base_metalness", 0),
    specularWeight: reader.number("specular_weight", 1),
    specularColor: reader.color("specular_color", [1, 1, 1]),
    specularRoughness: reader.number("specular_roughness", 0.3),
    specularAnisotropy: reader.constant("specular_roughness_anisotropy", 0),
    specularIor: reader.constant("specular_ior", 1.5),
    coatWeight: reader.number("coat_weight", 0),
    coatRoughness: reader.number("coat_roughness", 0),
    coatIor: reader.constant("coat_ior", 1.6),
    fuzzWeight: reader.constant("fuzz_weight", 0),
    fuzzColor: reader.color("fuzz_color", [1, 1, 1]),
    fuzzRoughness: reader.constant("fuzz_roughness", 0.5),
    emissionLuminance: reader.constant("emission_luminance", 0),
    emissionColor: reader.color("emission_color", [1, 1, 1]),
    transmissionWeight: reader.number("transmission_weight", 0),
    transmissionColor: reader.color("transmission_color", [1, 1, 1]).value,
    transmissionDepth: reader.constant("transmission_depth", 0),
    opacity: reader.number("geometry_opacity", 1),
    thinFilmWeight: reader.constant("thin_film_weight", 0),
    thinFilmThickness: reader.constant("thin_film_thickness", 0.5),
    thinFilmIor: reader.constant("thin_film_ior", 1.4),
    normal: reader.normal("geometry_normal"),
    notes: reader.finish(),
  };
}

const IMAGE = /^ND_(tiled)?image_(float|color3|color4|vector2|vector3|vector4)$/;

class PbrReader {
  readonly #stage: Stage;
  readonly #shader: string;
  readonly #time: number | undefined;
  readonly #notes: string[] = [];

  constructor(stage: Stage, shader: string, time: number | undefined) {
    this.#stage = stage;
    this.#shader = shader;
    this.#time = time;
  }

  number(name: string, fallback: number): PbrInput<number> {
    return this.#input(name, fallback, (value) => (value.kind === "scalar" && typeof value.value === "number" ? value.value : null));
  }

  color(name: string, fallback: Vec3): PbrInput<Vec3> {
    return this.#input(name, fallback, (value) => (value.kind === "tuple" && value.value.length >= 3 ? (value.value.slice(0, 3) as unknown as Vec3) : null));
  }

  /** An input read as a constant: a texture lookup is noted, and its default taken. */
  constant(name: string, fallback: number): number {
    const input = this.number(name, fallback);
    if (input.texture !== null) this.#notes.push(`${name} is textured; the preview uses its default`);
    return input.value;
  }

  #input<T>(name: string, fallback: T, decode: (value: Value) => T | null): PbrInput<T> {
    const source = this.#source(`${this.#shader}.inputs:${name}`);
    if (source === null) return { value: fallback, texture: null };
    if (source.kind === "authoredValue") return { value: this.#read(source.attribute, decode) ?? fallback, texture: null };
    const node = primOf(source.attribute);
    const id = this.#stage.shader(node)?.id ?? null;
    if (id === null || !IMAGE.test(id)) {
      this.#notes.push(`${name} reads ${id ?? node}, which the preview does not evaluate`);
      return { value: fallback, texture: null };
    }
    const value = this.#readInput(node, "default", decode) ?? fallback;
    return { value, texture: this.#texture(node, name.endsWith("color")) };
  }

  /** The value source of the port at `path`: the first of `valueSources`, or `null` when it has none. */
  #source(path: string): { readonly attribute: string; readonly kind: "shaderOutput" | "authoredValue" } | null {
    const port = this.#stage.port(path);
    if (port === null) return null;
    return port.valueSources().sources[0] ?? null;
  }

  #read<T>(attribute: string, decode: (value: Value) => T | null): T | null {
    const value = this.#stage.attribute(attribute)?.get(this.#time) ?? null;
    try {
      return value === null ? null : decode(value);
    } finally {
      releaseValue(value);
    }
  }

  /** A node's input value, through its connections to an authored value. */
  #readInput<T>(node: string, name: string, decode: (value: Value) => T | null): T | null {
    const source = this.#source(`${node}.inputs:${name}`);
    return source?.kind === "authoredValue" ? this.#read(source.attribute, decode) : null;
  }

  /** The image node at `node` as a texture lookup; `null` when its file is empty. */
  #texture(node: string, color: boolean): MaterialTexture | null {
    const source = this.#source(`${node}.inputs:file`);
    if (source === null || source.kind !== "authoredValue") return null;
    const attribute = this.#stage.attribute(source.attribute);
    const value = attribute?.get(this.#time) ?? null;
    const assetPath = value?.kind === "scalar" && typeof value.value === "string" ? value.value : "";
    releaseValue(value);
    if (assetPath === "") return null;
    const explanation = attribute?.explain(this.#time) ?? null;
    releaseValue(explanation?.value ?? null);
    const layer = explanation?.opinions.find((opinion) => opinion.role === "contributed")?.layer.identifier ?? null;
    const space = attribute?.metadata("colorSpace") ?? null;
    const named = space?.kind === "scalar" ? String(space.value) : "";
    const colorSpace: TextureColorSpace = /srgb/i.test(named) ? "sRGB" : named === "raw" || /linear/i.test(named) ? "raw" : color ? "sRGB" : "raw";
    const vec2 = (value: Value) => (value.kind === "tuple" && value.value.length === 2 ? (value.value as unknown as readonly [number, number]) : null);
    const tiling = this.#readInput(node, "uvtiling", vec2) ?? [1, 1];
    const offset = this.#readInput(node, "uvoffset", vec2) ?? [0, 0];
    return {
      shader: node,
      file: { assetPath, layer, identifier: anchorAssetPath(assetPath, layer), udim: isUdim(assetPath) },
      coordinates: { primvar: null, fallback: [0, 0], transform: { shader: node, rotation: 0, scale: tiling, translation: [-offset[0], -offset[1]] } },
      wrapS: "repeat",
      wrapT: "repeat",
      fallback: [0, 0, 0, 1],
      scale: [1, 1, 1, 1],
      bias: [0, 0, 0, 0],
      colorSpace,
    };
  }

  /** The normal map upstream of the input `name`, as the header of this file describes. */
  normal(name: string): OpenPbrSurface["normal"] {
    const nodes = this.#upstream(`${this.#shader}.inputs:${name}`);
    const image = nodes.find((node) => IMAGE.test(this.#stage.shader(node)?.id ?? ""));
    if (image === undefined) return null;
    const texture = this.#texture(image, false);
    if (texture === null) return null;
    const normalmap = nodes.find((node) => this.#stage.shader(node)?.id === "ND_normalmap");
    const scale = normalmap === undefined ? 1 : (this.#readInput(normalmap, "scale", (value) => (value.kind === "scalar" ? Number(value.value) : null)) ?? 1);
    const combine = nodes.find((node) => /^ND_combine3_/.test(this.#stage.shader(node)?.id ?? ""));
    const flip: [boolean, boolean, boolean] = [false, false, false];
    if (combine !== undefined) {
      (["in1", "in2", "in3"] as const).forEach((channel, index) => {
        const source = this.#source(`${combine}.inputs:${channel}`);
        if (source?.kind !== "shaderOutput") return;
        const node = primOf(source.attribute);
        if (!/^ND_ifequal_/.test(this.#stage.shader(node)?.id ?? "")) return;
        const flag = (value: Value) => (value.kind === "scalar" ? Number(value.value) : null);
        const equal = (this.#readInput(node, "value1", flag) ?? 0) === (this.#readInput(node, "value2", flag) ?? 0);
        const chosen = this.#upstream(`${node}.inputs:${equal ? "in1" : "in2"}`);
        flip[index] = chosen.some((upstream) => /^ND_invert_/.test(this.#stage.shader(upstream)?.id ?? ""));
      });
    }
    return { texture, scale, flip };
  }

  /** The shader nodes the port at `path` reads, nearest first. */
  #upstream(path: string): string[] {
    const found: string[] = [];
    const queue = [path];
    while (queue.length > 0) {
      const port = this.#stage.port(queue.shift()!);
      for (const connection of port?.connections ?? []) {
        const node = primOf(connection);
        if (found.includes(node) || this.#stage.shader(node) === null) continue;
        found.push(node);
        for (const input of this.#stage.shader(node)!.inputs()) queue.push(input.path);
      }
    }
    return found;
  }

  finish(): string[] {
    const ignored = (name: string, fallback: number, words: string) => {
      const input = this.number(name, fallback);
      if (input.texture !== null || input.value !== fallback) this.#notes.push(words);
    };
    ignored("subsurface_weight", 0, "subsurface scattering is not drawn");
    ignored("base_diffuse_roughness", 0, "diffuse roughness is not drawn (Lambert diffuse)");
    ignored("coat_darkening", 1, "coat darkening is not drawn");
    ignored("transmission_dispersion_scale", 0, "dispersion is not drawn");
    return this.#notes;
  }
}
