// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Shading objects over the native module's schema reads: `Material`,
// `Shader` and `ShaderPort`, and the decoders of the binding, subset and
// coordinate-system records.
import { ArrayHandle } from "./arrays.js";
import type { NativeArrayHandle } from "./arrays.js";
import type { Layerstack, Stage } from "./stage.js";
import type {
  ConnectionSourceInfo,
  CoordSysBinding,
  GeomSubset,
  MaterialBinding,
  MaterialSubsets,
  MaterialTerminal,
  SubsetElementType,
  SubsetProblem,
  TerminalSource,
  Value,
  ValueSources,
} from "./types.js";

type Row = readonly unknown[];

/**
 * A shading input or output: an `inputs:` or `outputs:` attribute of a
 * shader, node graph or material, as read when it was returned.
 * {@link ShaderPort.get} and {@link ShaderPort.valueSources} query the
 * stage now.
 */
export class ShaderPort {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its property path (`/Looks/Stone/Surface.inputs:diffuseColor`). */
  readonly path: string;
  /** The path of the prim it is on. */
  readonly prim: string;
  /** Its name without its namespace (`diffuseColor`). */
  readonly name: string;
  /** Whether it is an input or an output. */
  readonly kind: "input" | "output";
  /** Its declared value type name (`color3f`); `null` when nothing declares one. */
  readonly typeName: string | null;
  /** Its `sdrMetadata` entries, as OpenUSD's `UsdShadeInput::GetSdrMetadata` reads them. */
  readonly sdrMetadata: ReadonlyMap<string, string>;
  /** The attributes it is directly connected to, in composed order (`/Looks/Stone/Texture.outputs:rgb`). */
  readonly connections: readonly string[];
  /** Its connection targets that name no shading attribute (missing, or not an input or output). */
  readonly invalidConnections: readonly string[];

  /** @internal */
  constructor(stage: Stage, row: Row) {
    const [path, kind, typeName, metadata, connections, invalid] = row;
    this.stage = stage;
    this.path = path as string;
    const dot = this.path.lastIndexOf(".");
    this.prim = this.path.slice(0, dot);
    this.kind = kind as ShaderPort["kind"];
    this.name = this.path.slice(dot + 1 + (this.kind === "input" ? "inputs:" : "outputs:").length);
    this.typeName = typeName as string | null;
    this.sdrMetadata = new Map(metadata as [string, string][]);
    this.connections = Object.freeze([...(connections as string[])]);
    this.invalidConnections = Object.freeze([...(invalid as string[])]);
    Object.freeze(this);
  }

  /** Its own composed value at `time`, without following connections; `null` when it resolves none. See {@link ShaderPort.valueSources}. */
  get(time?: number): Value | null {
    return this.stage.attribute(this.path)?.get(time) ?? null;
  }

  /**
   * Where its value comes from, following connections through node
   * graphs: shader outputs to evaluate, and attributes whose authored
   * values to read. An unconnected input with a value is its own source.
   *
   * Throws `prim.not_found` when it is gone.
   */
  valueSources(): ValueSources {
    const [sources, issues] = this.stage.native("valueSources", (native) => native.valueSources(this.path)) as [
      Row[],
      Row[],
    ];
    return {
      sources: sources.map(([attribute, kind, chain]) => ({
        attribute: attribute as string,
        kind: kind as "shaderOutput" | "authoredValue",
        chain: chain as string[],
      })),
      issues: issues.map(([kind, at]) => ({
        kind: kind as ValueSources["issues"][number]["kind"],
        at: at as string | null,
      })),
    };
  }
}

/** The shading ports of kind `kind` of the prim at `path`, in property order. */
export function readPorts(stage: Stage, path: string, kind: "input" | "output"): ShaderPort[] {
  const rows = stage.native(kind === "input" ? "inputs" : "outputs", (native) => native.ports(path, kind)) as Row[];
  return rows.map((row) => new ShaderPort(stage, row));
}

/** The shading port at the property path `path`, or `null`. */
export function readPort(stage: Stage, path: string): ShaderPort | null {
  const row = stage.native("port", (native) => native.port(path)) as Row | null;
  return row === null ? null : new ShaderPort(stage, row);
}

/** The connection source the path text `path` names, checked against the stage. */
export function readConnectionSourceInfo(stage: Stage, path: string): ConnectionSourceInfo {
  const [prim, name, kind, typeName, source] = stage.native("connectionSourceInfo", (native) =>
    native.connectionSourceInfo(path),
  ) as Row;
  return Object.freeze({
    prim: prim as string | null,
    name: name as string,
    kind: kind as ConnectionSourceInfo["kind"],
    typeName: typeName as string | null,
    source: source as string | null,
    valid: source !== null,
  });
}

/** A `Material`, as read when it was returned: {@link Stage.material}. */
export class Material {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its path. */
  readonly path: string;
  /** The material it derives from through a specialize or inherit arc to another material; `null` without one (`UsdShadeMaterial::GetBaseMaterialPath`). */
  readonly baseMaterial: string | null;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [base] = row;
    this.stage = stage;
    this.path = path;
    this.baseMaterial = base as string | null;
    Object.freeze(this);
  }

  /**
   * The source of its `surface` output for the render `contexts`, tried
   * in order and then the universal one: OpenUSD's
   * `UsdShadeMaterial::ComputeSurfaceSource`.
   */
  surface(contexts: readonly string[] = []): TerminalSource {
    return this.terminal("surface", contexts);
  }

  /** The source of its `displacement` output, as {@link Material.surface} finds it. */
  displacement(contexts: readonly string[] = []): TerminalSource {
    return this.terminal("displacement", contexts);
  }

  /** The source of its `volume` output, as {@link Material.surface} finds it. */
  volume(contexts: readonly string[] = []): TerminalSource {
    return this.terminal("volume", contexts);
  }

  /** The source of its terminal output `terminal`. Throws `prim.not_found` when it is no longer a material. */
  terminal(terminal: MaterialTerminal, contexts: readonly string[] = []): TerminalSource {
    const [shader, context, sources] = this.stage.native("materialTerminal", (native) =>
      native.materialTerminal(this.path, terminal, [...contexts]),
    ) as [string | null, string | null, [string, string[]][]];
    return { shader, context, sources: sources.map(([output, chain]) => ({ output, chain })) };
  }

  /** Its interface inputs, in property order. */
  inputs(): ShaderPort[] {
    return readPorts(this.stage, this.path, "input");
  }

  /** Its outputs (`surface`, `mtlx:surface`), in property order. */
  outputs(): ShaderPort[] {
    return readPorts(this.stage, this.path, "output");
  }

  /** Its input `name` (`inputs:<name>`), or `null`. */
  input(name: string): ShaderPort | null {
    return readPort(this.stage, `${this.path}.inputs:${name}`);
  }

  /** Its output `name` (`outputs:<name>`), or `null`. */
  output(name: string): ShaderPort | null {
    return readPort(this.stage, `${this.path}.outputs:${name}`);
  }
}

/** A `Shader`, as read when it was returned: {@link Stage.shader}. */
export class Shader {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its path. */
  readonly path: string;
  /** Its `info:id` (`UsdPreviewSurface`); `null` unauthored. */
  readonly id: string | null;
  /** Its `info:implementationSource`: `id`, `sourceAsset` or `sourceCode`. */
  readonly implementationSource: string;
  /** Its `sdrMetadata` entries (`UsdShadeShader::GetSdrMetadata`). */
  readonly sdrMetadata: ReadonlyMap<string, string>;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [id, source, metadata] = row;
    this.stage = stage;
    this.path = path;
    this.id = id as string | null;
    this.implementationSource = source as string;
    this.sdrMetadata = new Map(metadata as [string, string][]);
    Object.freeze(this);
  }

  /** Its inputs, schema-defined and authored, in property order. */
  inputs(): ShaderPort[] {
    return readPorts(this.stage, this.path, "input");
  }

  /** Its outputs, in property order. */
  outputs(): ShaderPort[] {
    return readPorts(this.stage, this.path, "output");
  }

  /** Its input `name` (`inputs:<name>`), or `null`. */
  input(name: string): ShaderPort | null {
    return readPort(this.stage, `${this.path}.inputs:${name}`);
  }

  /** Its output `name` (`outputs:<name>`), or `null`. */
  output(name: string): ShaderPort | null {
    return readPort(this.stage, `${this.path}.outputs:${name}`);
  }
}

/** A binding record, or `null`. */
export function decodeBinding(record: unknown): MaterialBinding | null {
  if (record === null || record === undefined) return null;
  const [material, target, relationship, kind, collection, strength, purpose, withoutBindingApi] = record as Row;
  return {
    material: material as string | null,
    target: target as string,
    relationship: relationship as string,
    kind: kind as MaterialBinding["kind"],
    collection: collection as string | null,
    strength: strength as MaterialBinding["strength"],
    purpose: purpose as string,
    withoutBindingApi: withoutBindingApi as boolean,
  };
}

/** The problem records of a subset family. */
export function decodeProblems(rows: unknown): SubsetProblem[] {
  return (rows as Row[]).map(([prim, time, kind, at, message]) => ({
    prim: prim as string,
    time: time as number | null,
    kind: kind as SubsetProblem["kind"],
    at: at as SubsetProblem["at"],
    message: message as string,
  }));
}

/** A material subsets record. */
export function decodeMaterialSubsets(layerstack: Layerstack, record: unknown): MaterialSubsets {
  const [familyType, fallback, subsets, problems] = record as Row;
  return {
    familyType: familyType as string,
    fallback: decodeBinding(fallback),
    subsets: (subsets as Row[]).map(([path, indices, binding]) => ({
      path: path as string,
      indices: new ArrayHandle(layerstack, indices as NativeArrayHandle),
      binding: decodeBinding(binding),
    })),
    problems: decodeProblems(problems),
  };
}

/** Geometry subset records. */
export function decodeSubsets(layerstack: Layerstack, rows: unknown): GeomSubset[] {
  return (rows as Row[]).map(([path, elementType, familyName, indices]) => ({
    path: path as string,
    elementType: elementType as SubsetElementType | null,
    familyName: familyName as string | null,
    indices: indices === null ? null : new ArrayHandle(layerstack, indices as NativeArrayHandle),
  }));
}

/** Coordinate-system binding records. */
export function decodeCoordSys(rows: unknown): CoordSysBinding[] {
  return (rows as Row[]).map(([name, relationship, target]) => ({
    name: name as string,
    relationship: relationship as string,
    target: target as string,
  }));
}
