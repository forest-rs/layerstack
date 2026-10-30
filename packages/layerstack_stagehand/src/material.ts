// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Renderer-ready `UsdPreviewSurface` materials, as `layerstack_stagehand`
// builds them: every input of the surface a constant, a primvar or a
// texture lookup, with the textures' files anchored and their sampling
// spelled out. The inputs, fallbacks and meaning are those of OpenUSD
// 26.11's `shaderDefs.usda` (the UsdPreviewSurface specification) and its
// reference GLSL (`previewSurface.glslfx`, `uvTexture.glslfx`,
// `transform2d.glslfx`); see `layerstack_stagehand::MaterialItem`.
import type { Stage, Value } from "@forest-rs/layerstack";

/** The output channel of a `UsdUVTexture` an input reads. */
export type TextureChannel = "r" | "g" | "b" | "a" | "rgb";
/** A texture's wrapping (`wrapS`, `wrapT`). */
export type TextureWrap = "useMetadata" | "black" | "clamp" | "repeat" | "mirror";
/**
 * How a texture's texels are encoded (`sourceColorSpace`): `sRGB` decodes
 * through the sRGB curve, `raw` is linear, `auto` leaves it to the file (an
 * 8-bit color texture read as sRGB, data as raw, as Hydra decides).
 */
export type TextureColorSpace = "auto" | "raw" | "sRGB";

/** Where a surface input's value comes from. */
export type MaterialValue<T> =
  /** One value for the whole surface: authored, through an interface input, or the definition's fallback. */
  | { readonly kind: "constant"; readonly value: T }
  /** A lookup of {@link MaterialItem.textures}`[texture]`, its output `channel`. */
  | { readonly kind: "texture"; readonly texture: number; readonly channel: TextureChannel }
  /** A primvar of the drawn prim, read by a `UsdPrimvarReader`; `fallback` where the prim has none. */
  | { readonly kind: "primvar"; readonly name: string; readonly fallback: T };

/**
 * How a surface's opacity draws: `opaque` (the constant opacity 1),
 * `mask` (a positive `opacityThreshold`: opacity below it is not drawn,
 * the rest is opaque, whatever `opacityMode` says), or `blend` (opacity
 * scales the diffuse response alone for `transparent`, the whole response
 * for `presence`).
 */
export type AlphaMode =
  | { readonly kind: "opaque" }
  | { readonly kind: "mask"; readonly threshold: number }
  | { readonly kind: "blend"; readonly presence: boolean };

/** A `UsdTransform2d` moving texture coordinates: `st * scale`, rotated `rotation` degrees counter-clockwise, plus `translation`. */
export interface TextureTransform {
  readonly shader: string;
  readonly rotation: number;
  readonly scale: readonly [number, number];
  readonly translation: readonly [number, number];
}

/** A texture's coordinates. */
export interface TextureCoordinates {
  /** The primvar read for them (`st`) by a `UsdPrimvarReader_float2`; `null` when `st` is a constant. */
  readonly primvar: string | null;
  /** The constant `st`, or the reader's fallback where the prim has no such primvar. */
  readonly fallback: readonly [number, number];
  /** The transform applied to them, if any. */
  readonly transform: TextureTransform | null;
}

/** A texture's file: the asset path as composed and its identifier, anchored to the layer that authors it. */
export interface TextureFile {
  /** The asset path as composed. */
  readonly assetPath: string;
  /** The identifier of the layer whose opinion it is; `null` for a fallback. */
  readonly layer: string | null;
  /**
   * The path anchored to that layer, as OpenUSD's
   * `SdfComputeAssetPathRelativeToLayer` anchors a `./` or `../` path; a
   * search path or an absolute one is kept, normalized. `null` when it
   * cannot be anchored (an empty path, an unevaluated expression).
   */
  readonly identifier: string | null;
  /** Whether it names a UDIM tile set (`<UDIM>`): its tiles are listed by {@link udimTiles}. */
  readonly udim: boolean;
}

/** A `UsdUVTexture` lookup, as the inputs reading it share it. */
export interface MaterialTexture {
  readonly shader: string;
  readonly file: TextureFile;
  readonly coordinates: TextureCoordinates;
  readonly wrapS: TextureWrap;
  readonly wrapT: TextureWrap;
  /** The value where the file cannot be read, before scale and bias. */
  readonly fallback: readonly [number, number, number, number];
  /** Multiplies each channel: a lookup is `texel * scale + bias`. */
  readonly scale: readonly [number, number, number, number];
  /** Added to each channel after the scale. */
  readonly bias: readonly [number, number, number, number];
  readonly colorSpace: TextureColorSpace;
}

/** What a material's record could not represent, or found wrong; the input takes its fallback. */
export type MaterialNoteKind =
  | "noSurface"
  | "unsupportedSurface"
  | "unsupportedShader"
  | "unresolvedConnection"
  | "noValue"
  | "cycle"
  | "multipleSources"
  | "invalidValue"
  | "missingTexture"
  | "unsupported";

/** A note on a material: what, and where. */
export interface MaterialNote {
  readonly kind: MaterialNoteKind;
  /** The shader or prim it concerns, if one. */
  readonly prim: string | null;
  /** The input it concerns (`diffuseColor`), if one. */
  readonly input: string | null;
  /** The `info:id` of an unsupported surface or shader. */
  readonly id: string | null;
}

type Vec3 = readonly [number, number, number];

/** A material as a renderer draws it: the TypeScript mirror of `layerstack_stagehand::MaterialItem`. */
export interface MaterialItem {
  readonly path: string;
  /** The render context whose surface output was used (`""` universal); `null` when none resolved. */
  readonly context: string | null;
  /** The `UsdPreviewSurface` shader; `null` without one, every input then its fallback. */
  readonly surface: string | null;
  readonly diffuseColor: MaterialValue<Vec3>;
  readonly emissiveColor: MaterialValue<Vec3>;
  /** `useSpecularWorkflow` is 1: {@link specularColor} applies, {@link metallic} does not. */
  readonly useSpecularWorkflow: boolean;
  readonly specularColor: MaterialValue<Vec3>;
  readonly metallic: MaterialValue<number>;
  readonly roughness: MaterialValue<number>;
  readonly clearcoat: MaterialValue<number>;
  readonly clearcoatRoughness: MaterialValue<number>;
  readonly opacity: MaterialValue<number>;
  readonly opacityThreshold: number;
  /** `opacityMode` is `presence` rather than `transparent`. */
  readonly opacityPresence: boolean;
  readonly alpha: AlphaMode;
  readonly ior: MaterialValue<number>;
  /** A tangent-space normal in [-1, 1]. */
  readonly normal: MaterialValue<Vec3>;
  readonly displacement: MaterialValue<number>;
  readonly occlusion: MaterialValue<number>;
  /** The textures the inputs read, each once. */
  readonly textures: readonly MaterialTexture[];
  /** The primvars the inputs and textures read, sorted. */
  readonly primvars: readonly string[];
  readonly notes: readonly MaterialNote[];
}

/** One tile of a UDIM texture that exists. */
export interface UdimTileFile {
  readonly tile: number;
  readonly identifier: string;
}

/** The surface inputs and their `shaderDefs.usda` fallbacks. */
const SURFACE = {
  diffuseColor: [0.18, 0.18, 0.18],
  emissiveColor: [0, 0, 0],
  specularColor: [0, 0, 0],
  normal: [0, 0, 1],
  metallic: 0,
  roughness: 0.5,
  clearcoat: 0,
  clearcoatRoughness: 0.01,
  opacity: 1,
  opacityThreshold: 0,
  ior: 1.5,
  displacement: 0,
  occlusion: 1,
} as const;

/** `UsdUVTexture`'s fallbacks. */
const TEXTURE = {
  fallback: [0, 0, 0, 1],
  scale: [1, 1, 1, 1],
  bias: [0, 0, 0, 0],
  wrapS: "useMetadata",
  wrapT: "useMetadata",
  sourceColorSpace: "auto",
} as const;

/** The primvar readers an input of each shape reads. */
const READERS: Record<"float" | "float3", readonly string[]> = {
  float: ["UsdPrimvarReader_float"],
  float3: ["UsdPrimvarReader_float3", "UsdPrimvarReader_point", "UsdPrimvarReader_normal", "UsdPrimvarReader_vector"],
};

/** The standard nodes the record represents, through their right outputs. */
const KNOWN = new Set([
  "UsdUVTexture",
  "UsdTransform2d",
  "UsdPrimvarReader_float",
  "UsdPrimvarReader_float2",
  "UsdPrimvarReader_float3",
  "UsdPrimvarReader_float4",
  "UsdPrimvarReader_int",
  "UsdPrimvarReader_string",
  "UsdPrimvarReader_point",
  "UsdPrimvarReader_normal",
  "UsdPrimvarReader_vector",
  "UsdPrimvarReader_matrix",
]);

const WRAPS = new Set(["useMetadata", "black", "clamp", "repeat", "mirror"]);
const SPACES = new Set(["auto", "raw", "sRGB"]);

/** `float` is a number; `float2`, `float3` and `float4` tuples of that length; `int` an integer; `text` a string or token. */
type Shape = "float" | "float2" | "float3" | "float4" | "int" | "text";

/** Converts a composed value to `shape`; `undefined` when it is another. */
function convert(value: Value | null, shape: Shape): unknown {
  if (value === null) return undefined;
  if (value.kind === "array") {
    value.array.release();
    return undefined;
  }
  switch (shape) {
    case "float":
    case "int":
      return value.kind === "scalar" && typeof value.value === "number" ? value.value : undefined;
    case "text":
      return value.kind === "scalar" && typeof value.value === "string" ? value.value : undefined;
    default: {
      const length = Number(shape.slice(5));
      return value.kind === "tuple" && value.value.length === length ? [...value.value] : undefined;
    }
  }
}

/** What an input resolved to before it becomes a {@link MaterialValue}. */
type Resolved =
  | { readonly kind: "value"; readonly value: Value; readonly attribute: string }
  | { readonly kind: "output"; readonly shader: string; readonly id: string | null; readonly output: string }
  | { readonly kind: "none"; readonly note: MaterialNoteKind | null };

/** What reading a material inspected: the prims (for invalidation) and the attributes (for time variability). */
export interface MaterialReading {
  readonly item: MaterialItem;
  /** Every prim inspected other than the material. */
  readonly sources: readonly string[];
  /** Every attribute whose value was read. */
  readonly attributes: readonly string[];
}

class Reader {
  readonly prims = new Set<string>();
  readonly attributes = new Set<string>();
  readonly textures: MaterialTexture[] = [];
  readonly primvars = new Set<string>();
  readonly notes: MaterialNote[] = [];
  readonly #stage: Stage;
  readonly #time: number | undefined;

  constructor(stage: Stage, time: number | undefined) {
    this.#stage = stage;
    this.#time = time;
  }

  note(kind: MaterialNoteKind, prim: string | null, input: string | null, id: string | null = null): void {
    if (!this.notes.some((n) => n.kind === kind && n.prim === prim && n.input === input && n.id === id)) {
      this.notes.push(Object.freeze({ kind, prim, input, id }));
    }
  }

  #get(attribute: string): Value | null {
    this.attributes.add(attribute);
    return this.#stage.attribute(attribute)?.get(this.#time) ?? null;
  }

  /**
   * Where the input `name` of `prim` takes its value from, following
   * connections through node graphs and interface inputs
   * (`GetValueProducingAttributes`): an attribute's value, a shader's
   * output, or nothing (with why).
   */
  resolve(prim: string, name: string): Resolved {
    this.prims.add(prim);
    const path = `${prim}.inputs:${name}`;
    const port = this.#stage.port(path);
    if (port === null) return { kind: "none", note: null };
    if (port.connections.length === 0 && port.invalidConnections.length === 0) {
      const value = this.#get(path);
      if (value !== null) return { kind: "value", value, attribute: path };
      // Unauthored: the fallback; authored without a value at this time
      // (samples read at the default time, a block): noted.
      return { kind: "none", note: this.#stage.attribute(path)?.authored === true ? "noValue" : null };
    }
    const found = port.valueSources();
    for (const source of found.sources) for (const step of source.chain) this.prims.add(step.slice(0, step.lastIndexOf(".")));
    if (found.sources.length > 1) return { kind: "none", note: "multipleSources" };
    const [source] = found.sources;
    if (source === undefined) {
      const note = found.issues.some((issue) => issue.kind === "cycle")
        ? "cycle"
        : found.issues.length > 0 || port.invalidConnections.length > 0
          ? "unresolvedConnection"
          : "noValue";
      return { kind: "none", note };
    }
    const at = source.attribute.lastIndexOf(".");
    const owner = source.attribute.slice(0, at);
    this.prims.add(owner);
    if (source.kind === "authoredValue") {
      const value = this.#get(source.attribute);
      return value === null ? { kind: "none", note: "noValue" } : { kind: "value", value, attribute: source.attribute };
    }
    const shader = this.#stage.shader(owner);
    return { kind: "output", shader: owner, id: shader?.id ?? null, output: source.attribute.slice(at + 1 + "outputs:".length) };
  }

  /** A constant-only input (`opacityThreshold`, a texture's `wrapS`): its value, else `fallback`, noting why. */
  constant<T>(prim: string, name: string, shape: Shape, fallback: T, input: string, allowed?: ReadonlySet<string>): T {
    const resolved = this.resolve(prim, name);
    if (resolved.kind === "value") {
      const value = convert(resolved.value, shape);
      if (value !== undefined && (allowed === undefined || allowed.has(value as string))) return value as T;
      this.note("invalidValue", prim, input);
      return fallback;
    }
    if (resolved.kind === "output") this.note("unsupported", resolved.shader, input);
    else if (resolved.note !== null) this.note(resolved.note, prim, input);
    return fallback;
  }

  /** A surface input of `shape`: a constant, a primvar or a texture lookup. */
  value<T>(surface: string, name: string, shape: "float" | "float3", fallback: T): MaterialValue<T> {
    const resolved = this.resolve(surface, name);
    switch (resolved.kind) {
      case "value": {
        const value = convert(resolved.value, shape);
        if (value === undefined) {
          this.note("invalidValue", surface, name);
          return { kind: "constant", value: fallback };
        }
        return { kind: "constant", value: value as T };
      }
      case "none":
        if (resolved.note !== null) this.note(resolved.note, surface, name);
        return { kind: "constant", value: fallback };
      case "output":
        break;
    }
    const { shader, id, output } = resolved;
    if (id === "UsdUVTexture") {
      const channel = output === "rgb" && shape === "float3" ? "rgb" : ["r", "g", "b", "a"].includes(output) && shape === "float" ? (output as TextureChannel) : null;
      if (channel === null) {
        this.note("unsupported", shader, name);
        return { kind: "constant", value: fallback };
      }
      const texture = this.texture(shader, name);
      return texture === null ? { kind: "constant", value: fallback } : { kind: "texture", texture, channel };
    }
    if (READERS[shape].includes(id ?? "") && output === "result") {
      const primvar = this.constant<string>(shader, "varname", "text", "", name);
      if (primvar === "") {
        this.note("noValue", shader, name);
        return { kind: "constant", value: fallback };
      }
      this.primvars.add(primvar);
      const readerFallback = this.constant<T>(shader, "fallback", shape, (shape === "float" ? 0 : [0, 0, 0]) as T, name);
      return { kind: "primvar", name: primvar, fallback: readerFallback };
    }
    // A standard node read through the wrong output or shape is not
    // representable; any other node is unsupported.
    if (KNOWN.has(id ?? "")) this.note("unsupported", shader, name);
    else this.note("unsupportedShader", shader, name, id);
    return { kind: "constant", value: fallback };
  }

  /** The index of the texture `shader` in the record, added when new; `null` when its coordinates cannot be represented. */
  texture(shader: string, input: string): number | null {
    const known = this.textures.findIndex((texture) => texture.shader === shader);
    if (known >= 0) return known;
    const coordinates = this.coordinates(shader, input);
    if (coordinates === null) return null;
    const file = this.file(shader, input);
    const vec4 = (name: "fallback" | "scale" | "bias") =>
      this.constant<[number, number, number, number]>(shader, name, "float4", [...TEXTURE[name]], input);
    this.textures.push(
      Object.freeze({
        shader,
        file,
        coordinates,
        wrapS: this.constant<TextureWrap>(shader, "wrapS", "text", TEXTURE.wrapS, input, WRAPS),
        wrapT: this.constant<TextureWrap>(shader, "wrapT", "text", TEXTURE.wrapT, input, WRAPS),
        fallback: vec4("fallback"),
        scale: vec4("scale"),
        bias: vec4("bias"),
        colorSpace: this.constant<TextureColorSpace>(shader, "sourceColorSpace", "text", TEXTURE.sourceColorSpace, input, SPACES),
      }),
    );
    return this.textures.length - 1;
  }

  file(shader: string, input: string): TextureFile {
    const resolved = this.resolve(shader, "file");
    let assetPath = "";
    let layer: string | null = null;
    if (resolved.kind === "value") {
      const value = resolved.value;
      if (value.kind === "scalar" && value.type === "asset" && typeof value.value === "string") {
        assetPath = value.value;
        // The layer of the opinion: the file input's own, or the
        // interface input's it connects to.
        const explanation = this.#stage.attribute(resolved.attribute)?.explain(this.#time);
        layer = explanation?.opinions.find((opinion) => opinion.role === "contributed")?.layer.identifier ?? null;
      } else {
        this.note("invalidValue", shader, input);
      }
    } else if (resolved.kind === "output") {
      this.note("unsupported", resolved.shader, input);
    }
    const identifier = anchorAssetPath(assetPath, layer);
    if (identifier === null) this.note("missingTexture", shader, input);
    return Object.freeze({ assetPath, layer, identifier, udim: isUdim(assetPath) });
  }

  coordinates(shader: string, input: string): TextureCoordinates | null {
    let at = this.resolve(shader, "st");
    let transform: TextureTransform | null = null;
    if (at.kind === "output" && at.id === "UsdTransform2d" && at.output === "result") {
      const place = at.shader;
      const vec2 = (name: string, fallback: [number, number]) => this.constant<[number, number]>(place, name, "float2", fallback, input);
      transform = Object.freeze({
        shader: place,
        rotation: this.constant<number>(place, "rotation", "float", 0, input),
        scale: vec2("scale", [1, 1]),
        translation: vec2("translation", [0, 0]),
      });
      at = this.resolve(place, "in");
    }
    switch (at.kind) {
      case "value": {
        const value = convert(at.value, "float2") as [number, number] | undefined;
        const fallback: [number, number] = value ?? [0, 0];
        return Object.freeze({ primvar: null, fallback, transform });
      }
      case "none": {
        const fallback: [number, number] = [0, 0];
        return Object.freeze({ primvar: null, fallback, transform });
      }
      case "output":
        if (at.id === "UsdPrimvarReader_float2" && at.output === "result") {
          const primvar = this.constant<string>(at.shader, "varname", "text", "", input);
          if (primvar !== "") this.primvars.add(primvar);
          const fallback = this.constant<[number, number]>(at.shader, "fallback", "float2", [0, 0], input);
          return Object.freeze({ primvar: primvar === "" ? null : primvar, fallback, transform });
        }
        this.note("unsupported", at.shader, input);
        return null;
    }
  }
}

/**
 * Reads the material at `path` at `time` through the render `contexts`
 * (then the universal one), as `layerstack_stagehand` does; `null` when it
 * is not a `Material`.
 */
export function readMaterial(stage: Stage, path: string, time: number | undefined, contexts: readonly string[]): MaterialReading | null {
  const material = stage.material(path);
  if (material === null) return null;
  const terminal = material.surface(contexts);
  const reader = new Reader(stage, time);
  for (const source of terminal.sources) for (const step of [...source.chain, source.output]) reader.prims.add(step.slice(0, step.lastIndexOf(".")));
  const selected = terminal.sources[0];
  let surface: string | null = null;
  if (selected === undefined) {
    reader.note("noSurface", null, null);
  } else {
    const prim = selected.output.slice(0, selected.output.lastIndexOf("."));
    const id = stage.shader(prim)?.id ?? null;
    if (id === "UsdPreviewSurface") surface = prim;
    else reader.note("unsupportedSurface", prim, null, id);
  }
  const color = (name: "diffuseColor" | "emissiveColor" | "specularColor" | "normal") =>
    surface === null ? { kind: "constant" as const, value: SURFACE[name] as Vec3 } : reader.value<Vec3>(surface, name, "float3", SURFACE[name] as Vec3);
  const float = (name: "metallic" | "roughness" | "clearcoat" | "clearcoatRoughness" | "opacity" | "ior" | "displacement" | "occlusion") =>
    surface === null ? { kind: "constant" as const, value: SURFACE[name] as number } : reader.value<number>(surface, name, "float", SURFACE[name]);
  const constant = <T>(name: string, shape: Shape, fallback: T, allowed?: ReadonlySet<string>): T =>
    surface === null ? fallback : reader.constant<T>(surface, name, shape, fallback, name, allowed);
  const diffuseColor = color("diffuseColor");
  const emissiveColor = color("emissiveColor");
  const useSpecularWorkflow = constant<number>("useSpecularWorkflow", "int", 0) === 1;
  const specularColor = color("specularColor");
  const metallic = float("metallic");
  const roughness = float("roughness");
  const clearcoat = float("clearcoat");
  const clearcoatRoughness = float("clearcoatRoughness");
  const opacity = float("opacity");
  const opacityThreshold = constant<number>("opacityThreshold", "float", 0);
  const opacityPresence = constant<string>("opacityMode", "text", "transparent", new Set(["transparent", "presence"])) === "presence";
  const ior = float("ior");
  const normal = color("normal");
  const displacement = float("displacement");
  const occlusion = float("occlusion");
  // `previewSurface.glslfx`: a threshold above 0 masks whatever
  // `opacityMode` says; otherwise any opacity but the constant 1 blends.
  const alpha: AlphaMode =
    opacityThreshold > 0
      ? { kind: "mask", threshold: opacityThreshold }
      : opacity.kind === "constant" && opacity.value === 1
        ? { kind: "opaque" }
        : { kind: "blend", presence: opacityPresence };
  reader.prims.delete(path);
  const item: MaterialItem = Object.freeze({
    path,
    context: terminal.context,
    surface,
    diffuseColor,
    emissiveColor,
    useSpecularWorkflow,
    specularColor,
    metallic,
    roughness,
    clearcoat,
    clearcoatRoughness,
    opacity,
    opacityThreshold,
    opacityPresence,
    alpha,
    ior,
    normal,
    displacement,
    occlusion,
    textures: Object.freeze([...reader.textures]),
    primvars: Object.freeze([...reader.primvars].sort()),
    notes: Object.freeze([...reader.notes]),
  });
  return { item, sources: [...reader.prims], attributes: [...reader.attributes] };
}

/** Whether `path` names a UDIM tile set: it has `<UDIM>` and something beside it (`UsdShadeUdimUtils::IsUdimIdentifier`). */
export function isUdim(path: string): boolean {
  return path.includes("<UDIM>") && path.length > "<UDIM>".length;
}

/**
 * `assetPath` anchored to the layer `layer` identifies, as
 * `layerstack::asset::anchor_asset_path` and OpenUSD's
 * `SdfComputeAssetPathRelativeToLayer` anchor it: a `./` or `../` path is
 * joined to the layer's directory (a member's, inside a package); any
 * other path is normalized. `null` for an empty path, an expression, or a
 * relative path without a layer. AOUSD Core §9.4.
 */
export function anchorAssetPath(assetPath: string, layer: string | null): string | null {
  if (assetPath === "" || assetPath.startsWith("`")) return null;
  const path = assetPath.replaceAll("\\", "/");
  if (!path.startsWith("./") && !path.startsWith("../")) return normalize(path);
  if (layer === null || layer === "") return null;
  const location = layer.replaceAll("\\", "/");
  const open = location.indexOf("[");
  if (open >= 0 && location.endsWith("]")) {
    const member = location.slice(open + 1, -1);
    const slash = member.lastIndexOf("/");
    const anchored = normalize(slash >= 0 ? `${member.slice(0, slash)}/${path}` : path).replace(/^\.\//, "");
    return `${location.slice(0, open)}[${anchored}]`;
  }
  const slash = location.lastIndexOf("/");
  if (slash < 0) return normalize(path);
  const directory = location.slice(0, slash);
  const marked = directory.startsWith("/") || directory.startsWith(".") || /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(directory) ? directory : `./${directory}`;
  return normalize(`${marked}/${path}`);
}

/** Removes `.` and `..` segments, keeping a leading `./` or `../` marker and a root. */
function normalize(path: string): string {
  const absolute = path.startsWith("/");
  const marked = path.startsWith("./");
  const out: string[] = [];
  for (const part of path.split("/")) {
    if (part === "" || part === ".") continue;
    if (part === ".." && out.length > 0 && out[out.length - 1] !== "..") out.pop();
    else if (part !== ".." || !absolute) out.push(part);
  }
  const joined = out.join("/");
  if (absolute) return `/${joined}`;
  return marked && !joined.startsWith("..") ? `./${joined}` : joined;
}

/**
 * The tiles of a UDIM texture that exist, in tile order: each of 1001
 * through 1100 spelled into the anchored pattern and kept when `exists`
 * (the host's storage or fetch) has it, as
 * `UsdShadeUdimUtils::ResolveUdimTilePaths` resolves each tile. Empty for
 * a file that is not a UDIM set or cannot be anchored.
 */
export async function udimTiles(file: TextureFile, exists: (identifier: string) => boolean | Promise<boolean>): Promise<UdimTileFile[]> {
  const pattern = file.identifier;
  if (pattern === null || !isUdim(pattern)) return [];
  const at = pattern.indexOf("<UDIM>");
  const tiles = Array.from({ length: 100 }, (_, i) => 1001 + i).map((tile) => ({
    tile,
    identifier: `${pattern.slice(0, at)}${tile}${pattern.slice(at + "<UDIM>".length)}`,
  }));
  const found = await Promise.all(tiles.map((tile) => exists(tile.identifier)));
  return tiles.filter((_, i) => found[i]);
}
