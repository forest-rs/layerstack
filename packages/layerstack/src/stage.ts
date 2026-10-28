// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { LayerstackError } from "./errors.js";
import type { ErrorCode } from "./errors.js";
import type {
  ArcKind,
  ArrayData,
  ArrayElements,
  AssetFailure,
  AssetNeed,
  AssetSource,
  Explanation,
  Issue,
  LoadResult,
  MissingAsset,
  NeedKind,
  OpenOptions,
  Opinion,
  ResolveOptions,
  ResolveResult,
  Scalar,
  StageStatus,
  Transfer,
  Value,
  ValueSource,
  VariantSet,
  Version,
} from "./types.js";

/** This package's version. */
export const PACKAGE_VERSION = "0.1.0";

/**
 * The ABI this package speaks: the version of the records it exchanges
 * with the native module (`layerstack_wasm::ABI`).
 */
export const ABI = 2;

/** The native stage the generated glue exports (`layerstack_wasm::Stage`). Not for app use. */
export interface NativeStage {
  free(): void;
  wanted(): unknown;
  supply(identifier: string, bytes: Uint8Array): void;
  confirmAbsent(identifier: string): void;
  loadSupplied(): unknown;
  layers(): unknown;
  layerDirty(id: bigint): boolean;
  status(): unknown;
  prim(path: string): unknown;
  children(path: string): string[];
  attributeNames(path: string): string[];
  variantSets(path: string): unknown;
  attribute(path: string): unknown;
  get(path: string, time?: number | null): unknown;
  timeSamples(path: string): Float64Array;
  explain(path: string, time?: number | null): unknown;
  metadata(path: string, key: string): unknown;
  relationship(path: string): unknown;
}

/**
 * An initialized native module: the generated glue of `layerstack_wasm`,
 * or of a module that links it (such as a combined module of another
 * crate). Pass it to {@link loadLayerstack} to use that module.
 */
export interface NativeModule {
  readonly Stage: { open(identifier: string, bytes: Uint8Array, recover: boolean): NativeStage };
  nativeVersion(): unknown;
  /** The module's memory, for {@link Layerstack.transfer}; optional. */
  readonly memory?: WebAssembly.Memory;
}

type Row = readonly unknown[];

const owners = new WeakMap<object, Stage>();

/**
 * One instance of the native module: opens stages and counts the bytes
 * that cross the boundary. Get one with {@link loadLayerstack}.
 */
export class Layerstack {
  /** This package's and the module's versions. */
  readonly version: Version;
  readonly #module: NativeModule;
  #trapped: unknown = undefined;
  #bytesIn = 0;
  #bytesOut = 0;

  /** Wraps an initialized module, refusing one of another ABI (`module.abi`). */
  constructor(module: NativeModule) {
    this.#module = module;
    const [crate, revision, abi] = module.nativeVersion() as [string, string, number];
    if (abi !== ABI) {
      throw new LayerstackError(
        "module.abi",
        "loadLayerstack",
        `the module speaks ABI ${abi}, this package ABI ${ABI}; use a module built from a matching layerstack_wasm`,
      );
    }
    this.version = Object.freeze({ package: PACKAGE_VERSION, crate, revision, abi });
  }

  /**
   * Opens a stage from the bytes of its root layer, copied once into the
   * module. Nothing is fetched: the stage composes what it has and wants
   * the rest ({@link Stage.wanted}, {@link Stage.resolve}).
   *
   * Throws `io.rejected` for a root that does not parse.
   */
  open(root: { readonly identifier: string; readonly bytes: Uint8Array }, options: OpenOptions = {}): Stage {
    if (typeof root?.identifier !== "string" || !(root.bytes instanceof Uint8Array)) {
      throw new LayerstackError("argument.invalid", "open", "expected { identifier: string, bytes: Uint8Array }");
    }
    const recover = options.importPolicy === "recover";
    const native = this.call("open", () => this.#module.Stage.open(root.identifier, root.bytes, recover));
    this.#bytesIn += root.bytes.byteLength;
    return new Stage(this, native);
  }

  /** The bytes that crossed the boundary through this instance so far. */
  transfer(): Transfer {
    return {
      bytesIn: this.#bytesIn,
      bytesOut: this.#bytesOut,
      memoryBytes: this.#module.memory?.buffer.byteLength ?? 0,
    };
  }

  /** @internal Counts bytes copied in or out. */
  count(direction: "in" | "out", bytes: number): void {
    if (direction === "in") this.#bytesIn += bytes;
    else this.#bytesOut += bytes;
  }

  /**
   * @internal Runs a native call, turning its thrown record into a
   * {@link LayerstackError} and a trap into `module.trapped`.
   */
  call<T>(operation: string, f: () => T): T {
    if (this.#trapped !== undefined) {
      throw new LayerstackError("module.trapped", operation, "the module trapped earlier; load a new instance", {
        cause: this.#trapped,
      });
    }
    try {
      return f();
    } catch (error) {
      throw this.#translate(operation, error);
    }
  }

  #translate(operation: string, error: unknown): unknown {
    if (Array.isArray(error) && typeof error[0] === "string" && typeof error[1] === "string") {
      const [code, message, identifier, path] = error as [ErrorCode, string, string | null, string | null];
      return new LayerstackError(code, operation, message, {
        identifier: identifier ?? undefined,
        path: path ?? undefined,
      });
    }
    if (error instanceof WebAssembly.RuntimeError) {
      this.#trapped = error;
      return new LayerstackError("module.trapped", operation, `the module trapped: ${error.message}`, {
        cause: error,
      });
    }
    return error;
  }
}

/** A layer of a stage: its identity. Ask its stage about its state ({@link Stage.isDirty}). */
export class Layer {
  /**
   * The layer's id in its stage. Ids may exceed 2^53; they are never
   * needed for normal work.
   */
  readonly id: bigint;
  /** The identifier it was read from (`set/scene.usda`, `scene.usdz[root.usdc]` for a package member); `null` for an anonymous layer. */
  readonly identifier: string | null;
  /** The identifier's extension (a package member's own): `usda`, `usdc`, `usdz`, or `""`. */
  readonly format: string;
  /** Whether it is read-only, as a package member is. */
  readonly readOnly: boolean;

  /** @internal */
  constructor(stage: Stage, id: bigint, identifier: string | null, format: string, readOnly: boolean) {
    this.id = id;
    this.identifier = identifier;
    this.format = format;
    this.readOnly = readOnly;
    owners.set(this, stage);
    Object.freeze(this);
  }

  /** Whether it has no identifier. */
  get anonymous(): boolean {
    return this.identifier === null;
  }
}

/**
 * An open stage. Reads are synchronous; only {@link Stage.resolve}, which
 * waits on the host's fetches, is asynchronous.
 *
 * Paths are text: prim paths (`/World/Rock`) and property paths
 * (`/World/Rock.size`). A stage owns native memory: dispose it with
 * {@link Stage.free} or `using`; every call after that fails with
 * `stage.disposed`.
 */
export class Stage implements Disposable {
  readonly #ls: Layerstack;
  #native: NativeStage | null;
  readonly #layers = new Map<bigint, Layer>();
  #root: Layer | null = null;

  /** @internal Use {@link Layerstack.open}. */
  constructor(layerstack: Layerstack, native: NativeStage) {
    this.#ls = layerstack;
    this.#native = native;
  }

  /** The instance that opened this stage. */
  get layerstack(): Layerstack {
    return this.#ls;
  }

  /** Whether the stage was disposed. */
  get disposed(): boolean {
    return this.#native === null;
  }

  /** The root layer. */
  get root(): Layer {
    if (this.#root === null) this.layers();
    return this.#root as Layer;
  }

  /** The assets the stage wants now: storage has no answer for them yet. */
  wanted(): AssetNeed[] {
    const rows = this.#call("wanted", (native) => native.wanted()) as Row[];
    return rows.map(([identifier, asset, anchor, arc]) => ({
      identifier: identifier as string,
      asset: asset as string,
      anchor: this.#layer(anchor as bigint),
      arc: arc as NeedKind,
    }));
  }

  /**
   * Supplies the bytes of `identifier` (copied once into the module), or
   * `null` to confirm there is no such asset. A wanted asset is read at the
   * next {@link Stage.loadSupplied}; a layer already read stays as it was
   * read.
   */
  supply(identifier: string, bytes: Uint8Array | null): void {
    if (typeof identifier !== "string" || !(bytes === null || bytes instanceof Uint8Array)) {
      throw new LayerstackError("argument.invalid", "supply", "expected (identifier: string, bytes: Uint8Array | null)");
    }
    if (bytes === null) {
      this.#call("supply", (native) => native.confirmAbsent(identifier));
    } else {
      this.#call("supply", (native) => native.supply(identifier, bytes));
      this.#ls.count("in", bytes.byteLength);
    }
  }

  /**
   * Reads what was supplied for the assets the stage wants. It reads no
   * layer already read, so it works whatever layers are dirty. A supplied
   * layer may want more: call {@link Stage.wanted} again.
   *
   * Throws `io.rejected` when a supplied layer does not parse under the
   * strict import policy; the layers read before it stay.
   */
  loadSupplied(): LoadResult {
    const [loaded, failures] = this.#call("loadSupplied", (native) => native.loadSupplied()) as [bigint[], Row[]];
    return {
      loaded: loaded.map((id) => this.#layer(id)),
      failures: failures.map(([asset, anchor, code, message]) => this.#failure(asset, anchor, code, message)),
    };
  }

  /**
   * Runs the loop until nothing is wanted: {@link Stage.wanted}, fetch
   * every wanted identifier from `source` at once, {@link Stage.supply}
   * what arrived, {@link Stage.loadSupplied}.
   *
   * A fetch that throws fails the loop with `resolve.fetch` (naming the
   * identifier) after what the other fetches of its round returned is
   * loaded. `signal` aborts it with `resolve.aborted`.
   */
  async resolve(source: AssetSource, options: ResolveOptions = {}): Promise<ResolveResult> {
    const { signal, maxRounds = 100 } = options;
    const loaded: Layer[] = [];
    const failures: AssetFailure[] = [];
    const supplied = new Set<string>();
    let rounds = 0;
    let bytes = 0;
    for (;;) {
      const wanted = [...new Set(this.wanted().map((need) => need.identifier))];
      if (wanted.length === 0) break;
      const stuck = wanted.find((identifier) => supplied.has(identifier));
      if (stuck !== undefined) {
        throw new LayerstackError("resolve.stuck", "resolve", "an asset was supplied but the stage still wants it", {
          identifier: stuck,
        });
      }
      if (rounds === maxRounds) {
        throw new LayerstackError("resolve.limit", "resolve", `still wanting assets after ${maxRounds} rounds`);
      }
      rounds += 1;
      if (signal?.aborted) {
        throw new LayerstackError("resolve.aborted", "resolve", "the signal aborted", { cause: signal.reason });
      }
      const settled = await Promise.allSettled(wanted.map((identifier) => source.fetch(identifier, signal)));
      if (signal?.aborted) {
        throw new LayerstackError("resolve.aborted", "resolve", "the signal aborted", { cause: signal.reason });
      }
      let failed: LayerstackError | null = null;
      settled.forEach((outcome, index) => {
        const identifier = wanted[index] as string;
        if (outcome.status === "rejected") {
          failed ??= new LayerstackError("resolve.fetch", "resolve", "the asset source failed to fetch", {
            identifier,
            cause: outcome.reason,
          });
          return;
        }
        this.supply(identifier, outcome.value);
        supplied.add(identifier);
        bytes += outcome.value?.byteLength ?? 0;
      });
      const result = this.loadSupplied();
      loaded.push(...result.loaded);
      failures.push(...result.failures);
      if (failed !== null) throw failed;
    }
    return { loaded, failures, rounds, fetched: supplied.size, bytes };
  }

  /** Every layer the stage holds, read or anonymous. */
  layers(): Layer[] {
    const [root, , rows] = this.#call("layers", (native) => native.layers()) as [bigint, bigint | null, Row[]];
    const layers = rows.map(([id, identifier, format, readOnly]) =>
      this.#remember(id as bigint, identifier as string | null, format as string, readOnly as boolean),
    );
    this.#root = this.#layers.get(root) ?? null;
    return layers;
  }

  /** Whether `layer` changed since it was read or last saved. Throws `layer.affinity` for another stage's layer. */
  isDirty(layer: Layer): boolean {
    this.#own("isDirty", layer);
    return this.#call("isDirty", (native) => native.layerDirty(layer.id));
  }

  /** The stage's state now: prims, composition errors, wanted and missing assets. */
  status(): StageStatus {
    const [prims, errors, missing] = this.#call("status", (native) => native.status()) as [number, Row[], Row[]];
    const wanted = this.wanted();
    const missingAssets: MissingAsset[] = missing.map(([anchor, asset, arc]) => ({
      asset: asset as string,
      anchor: this.#layer(anchor as bigint),
      arc: arc as NeedKind,
    }));
    return {
      prims,
      compositionErrors: errors.map(
        ([code, message, prim]): Issue => ({
          code: code as string,
          message: message as string,
          prim: prim as string | null,
        }),
      ),
      wanted,
      missing: missingAssets,
      complete: wanted.length === 0 && missingAssets.length === 0,
    };
  }

  /** The composed prim at `path`, or `null`. Throws `path.invalid` for a path that is not an absolute prim path. */
  prim(path: string): Prim | null {
    const row = this.#call("prim", (native) => native.prim(path)) as Row | null;
    return row === null ? null : new Prim(this, path, row);
  }

  /** The paths of every composed child of the prim at `path`, in namespace order, inactive and abstract ones included. Throws `prim.not_found`. */
  children(path: string): string[] {
    return this.#call("children", (native) => native.children(path));
  }

  /** The attribute at the property path `path` (`/World/Rock.size`), or `null`. */
  attribute(path: string): Attribute | null {
    const row = this.#call("attribute", (native) => native.attribute(path)) as Row | null;
    return row === null ? null : new Attribute(this, path, row);
  }

  /** The relationship at the property path `path` (`/World.lookAt`), or `null`. */
  relationship(path: string): Relationship | null {
    const row = this.#call("relationship", (native) => native.relationship(path)) as Row | null;
    return row === null ? null : new Relationship(this, path, row);
  }

  /** Disposes the native stage. Idempotent. */
  free(): void {
    const native = this.#native;
    this.#native = null;
    native?.free();
  }

  /** Disposes the native stage, for `using`. */
  [Symbol.dispose](): void {
    this.free();
  }

  /** @internal */
  native<T>(operation: string, f: (native: NativeStage) => T): T {
    return this.#call(operation, f);
  }

  /** @internal The layer with `id`, from the cache or a fresh inventory. */
  layerOf(id: bigint): Layer {
    return this.#layer(id);
  }

  #call<T>(operation: string, f: (native: NativeStage) => T): T {
    const native = this.#native;
    if (native === null) {
      throw new LayerstackError("stage.disposed", operation, "the stage was disposed");
    }
    return this.#ls.call(operation, () => f(native));
  }

  #own(operation: string, layer: Layer): void {
    if (!(layer instanceof Layer) || owners.get(layer) !== this) {
      throw new LayerstackError("layer.affinity", operation, "the layer belongs to another stage");
    }
  }

  #layer(id: bigint): Layer {
    const known = this.#layers.get(id);
    if (known !== undefined) return known;
    this.layers();
    const found = this.#layers.get(id);
    if (found === undefined) {
      throw new LayerstackError("layer.unknown", "layers", `the stage has no layer ${id}`);
    }
    return found;
  }

  #remember(id: bigint, identifier: string | null, format: string, readOnly: boolean): Layer {
    let layer = this.#layers.get(id);
    if (layer === undefined) {
      layer = new Layer(this, id, identifier, format, readOnly);
      this.#layers.set(id, layer);
    }
    return layer;
  }

  #failure(asset: unknown, anchor: unknown, code: unknown, message: unknown): AssetFailure {
    return {
      asset: asset as string,
      anchor: anchor === null ? null : this.#layer(anchor as bigint),
      code: code as string,
      message: message as string,
    };
  }
}

/**
 * A composed prim, as read when it was returned: its fields do not
 * follow later changes, so read it again after one. Its methods query the
 * stage now.
 */
export class Prim {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its path. */
  readonly path: string;
  /** Its name, the last element of its path (`""` for the pseudo-root). */
  readonly name: string;
  /** Its type name (`Sphere`), `null` when typeless. */
  readonly typeName: string | null;
  /** Its composed specifier. */
  readonly specifier: "def" | "over" | "class";
  /** Whether it is active. */
  readonly active: boolean;
  /** Whether it and its ancestors are defined (`def` or `class`). */
  readonly defined: boolean;
  /** Whether it or an ancestor is a `class`. */
  readonly abstract: boolean;
  /** Whether it is an instance. */
  readonly instance: boolean;
  /** Whether it is a proxy for a prim inside an instance's prototype. */
  readonly instanceProxy: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [typeName, specifier, active, defined, abstract, instance, instanceProxy] = row;
    this.stage = stage;
    this.path = path;
    this.name = path === "/" ? "" : path.slice(path.lastIndexOf("/") + 1);
    this.typeName = typeName as string | null;
    this.specifier = specifier as Prim["specifier"];
    this.active = active as boolean;
    this.defined = defined as boolean;
    this.abstract = abstract as boolean;
    this.instance = instance as boolean;
    this.instanceProxy = instanceProxy as boolean;
    Object.freeze(this);
  }

  /** The paths of its children ({@link Stage.children}). */
  children(): string[] {
    return this.stage.children(this.path);
  }

  /** The names of its attributes, schema-defined and authored, in property order. */
  attributeNames(): string[] {
    return this.stage.native("attributeNames", (native) => native.attributeNames(this.path));
  }

  /** Its attribute `name` (`size`, `inputs:diffuseColor`), or `null`. */
  attribute(name: string): Attribute | null {
    return this.stage.attribute(`${this.path}.${name}`);
  }

  /** Its variant sets, their selections and variant names. */
  variantSets(): VariantSet[] {
    const rows = this.stage.native("variantSets", (native) => native.variantSets(this.path)) as Row[];
    return rows.map(([name, selection, variants]) => ({
      name: name as string,
      selection: selection as string | null,
      variants: variants as string[],
    }));
  }

  /** Its composed metadata field `key` (`kind`, `documentation`, `apiSchemas`), or `null` when nothing authors it. */
  metadata(key: string): Value | null {
    return readMetadata(this.stage, this.path, key);
  }
}

/**
 * A composed attribute, as read when it was returned; {@link Attribute.get}
 * and {@link Attribute.explain} query the stage now.
 *
 * `time` is a time code, interpolated linearly between samples; without
 * one, queries read the default time.
 */
export class Attribute {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its property path (`/World/Rock.size`). */
  readonly path: string;
  /** Its name (`size`). */
  readonly name: string;
  /** Its value type name (`double`, `point3f[]`); `null` when nothing declares one. */
  readonly typeName: string | null;
  /** Whether its value may vary over time. */
  readonly variability: "varying" | "uniform";
  /** Whether no schema defines it. */
  readonly custom: boolean;
  /** Whether any layer authors an opinion for it. */
  readonly authored: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    const [typeName, variability, custom, authored] = row;
    this.stage = stage;
    this.path = path;
    this.name = path.slice(path.lastIndexOf(".") + 1);
    this.typeName = typeName as string | null;
    this.variability = variability as Attribute["variability"];
    this.custom = custom as boolean;
    this.authored = authored as boolean;
    Object.freeze(this);
  }

  /** Its composed value at `time`, or `null` when it resolves none. Arrays are copied out once. */
  get(time?: number): Value | null {
    const record = this.stage.native("get", (native) => native.get(this.path, time));
    return decodeValue(this.stage.layerstack, record);
  }

  /** Its value as array data, or `null` when it is not an array. */
  array(time?: number): ArrayData | null {
    const value = this.get(time);
    return value?.kind === "array" ? value.array : null;
  }

  /** Why it has its value at `time`, or `null` when it does not exist on the prim. */
  explain(time?: number): Explanation | null {
    const record = this.stage.native("explain", (native) => native.explain(this.path, time)) as Row | null;
    if (record === null) return null;
    const [value, source, lower, upper, opinions] = record;
    return {
      value: decodeValue(this.stage.layerstack, value),
      source: source as ValueSource,
      samples: lower === null ? null : { lower: lower as number, upper: upper as number },
      opinions: (opinions as Row[]).map(
        ([layer, specPath, arc, role, detail, offset, scale]): Opinion => ({
          layer: this.stage.layerOf(layer as bigint),
          specPath: specPath as string,
          arc: arc as ArcKind | null,
          role: role as Opinion["role"],
          contribution: role === "contributed" ? (detail as Opinion["contribution"]) : null,
          cause: role === "ignored" ? (detail as Opinion["cause"]) : null,
          layerOffset: { offset: offset as number, scale: scale as number },
        }),
      ),
    };
  }

  /** Its composed time sample times, ascending. */
  timeSamples(): number[] {
    return Array.from(this.stage.native("timeSamples", (native) => native.timeSamples(this.path)));
  }

  /** Its composed metadata field `key` (`hidden`, `documentation`), or `null` when nothing authors it. */
  metadata(key: string): Value | null {
    return readMetadata(this.stage, this.path, key);
  }
}

/** A composed relationship, as read when it was returned; {@link Relationship.targets} queries the stage now. */
export class Relationship {
  /** The stage it was read from. */
  readonly stage: Stage;
  /** Its property path (`/World.lookAt`). */
  readonly path: string;
  /** Its name (`lookAt`). */
  readonly name: string;
  /** Whether an opinion declares it `custom`: one no schema of its prim is expected to define. */
  readonly custom: boolean;

  /** @internal */
  constructor(stage: Stage, path: string, row: Row) {
    this.stage = stage;
    this.path = path;
    this.name = path.slice(path.lastIndexOf(".") + 1);
    this.custom = row[0] as boolean;
    Object.freeze(this);
  }

  /** Its composed targets, in order: prim and property paths. */
  targets(): string[] {
    const row = this.stage.native("relationship", (native) => native.relationship(this.path)) as Row | null;
    return row === null ? [] : [...(row[1] as string[])];
  }

  /** Its composed metadata field `key`, or `null`. */
  metadata(key: string): Value | null {
    return readMetadata(this.stage, this.path, key);
  }
}

/** Array data copied out of the module: the elements live in JS memory. */
class ArrayCopy implements ArrayData {
  readonly type: string;
  readonly scalar: Scalar;
  readonly components: number;
  readonly length: number;
  readonly byteLength: number;
  readonly #elements: ArrayElements;

  constructor(type: string, scalar: Scalar, components: number, length: number, elements: ArrayElements) {
    this.type = type;
    this.scalar = scalar;
    this.components = components;
    this.length = length;
    this.#elements = elements;
    this.byteLength = Array.isArray(elements)
      ? elements.reduce((sum, text) => sum + 2 * text.length, 0)
      : elements.byteLength;
  }

  copy(): ArrayElements {
    return Array.isArray(this.#elements) ? [...this.#elements] : this.#elements.slice();
  }

  /** Nothing to release: the elements are JS-owned. */
  [Symbol.dispose](): void {}
}

function readMetadata(stage: Stage, path: string, key: string): Value | null {
  const record = stage.native("metadata", (native) => native.metadata(path, key));
  return decodeValue(stage.layerstack, record);
}

function decodeValue(layerstack: Layerstack, record: unknown): Value | null {
  if (record === null || record === undefined) return null;
  const [type, tag, ...rest] = record as [string, string, ...unknown[]];
  switch (tag) {
    case "s":
      return { kind: "scalar", type, value: rest[0] as boolean | number | bigint | string };
    case "t":
      return { kind: "tuple", type, value: rest[0] as number[] };
    case "a": {
      const [scalar, components, length, elements] = rest as [Scalar, number, number, ArrayElements];
      const array = new ArrayCopy(type, scalar, components, length, elements);
      layerstack.count("out", array.byteLength);
      return { kind: "array", type, array };
    }
    case "d":
      return {
        kind: "dictionary",
        type,
        value: new Map((rest[0] as [string, unknown][]).map(([key, value]) => [key, decodeValue(layerstack, value)])),
      };
    case "l":
      return { kind: "list", type, value: (rest[0] as unknown[]).map((item) => decodeValue(layerstack, item)) };
    default:
      return { kind: "opaque", type };
  }
}
