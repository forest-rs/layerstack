// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Array values: native handles over Wasm memory, and text copies.
import { LayerstackError } from "./errors.js";
import type { ArrayData, NumericElements, Scalar } from "./types.js";

/** The native array handle the generated glue exports (`layerstack_wasm::ArrayHandle`). Not for app use. */
export interface NativeArrayHandle {
  free(): void;
  /** The address of the first byte of the scalars in the module's memory. */
  readonly address: number;
  /** The bytes of the scalars. */
  readonly byteLength: number;
  /** `[type, scalar, components, length]`. */
  layout(): unknown;
}

/** What an {@link ArrayHandle} needs of its instance: the memory and the transfer counters. */
export interface ArrayHost {
  /** The module's memory, `undefined` for a module that does not expose it. */
  memory(): WebAssembly.Memory | undefined;
  /** Counts bytes copied out, or exposed through a view. */
  count(direction: "out" | "view", bytes: number): void;
}

type TypedArrayConstructor =
  | Uint8ArrayConstructor
  | Uint16ArrayConstructor
  | Int32ArrayConstructor
  | Uint32ArrayConstructor
  | BigInt64ArrayConstructor
  | BigUint64ArrayConstructor
  | Float32ArrayConstructor
  | Float64ArrayConstructor;

const VIEWS: Record<Exclude<Scalar, "string">, TypedArrayConstructor> = {
  bool: Uint8Array,
  u8: Uint8Array,
  f16: Uint16Array,
  i32: Int32Array,
  u32: Uint32Array,
  i64: BigInt64Array,
  u64: BigUint64Array,
  f32: Float32Array,
  f64: Float64Array,
};

const natives = new WeakMap<ArrayHandle, NativeArrayHandle>();
const hosts = new WeakMap<ArrayHandle, ArrayHost>();

/**
 * @internal The native handle of a live {@link ArrayHandle} of `host`'s
 * module, for sending it back without a copy; `undefined` for any other
 * array, which is sent as a copy.
 */
export function nativeOf(array: ArrayData, host: ArrayHost): NativeArrayHandle | undefined {
  return array instanceof ArrayHandle && hosts.get(array) === host ? natives.get(array) : undefined;
}

/**
 * A numeric array still in the module: the native buffer the stage
 * composed, pinned by this handle. It is a snapshot: later edits copy on
 * write and never change it, and it outlives its stage.
 *
 * - {@link ArrayHandle.view} is a typed array over the native bytes, with
 *   no copy. **It is valid until the next call into the module**, which
 *   may grow the module's memory and so detach it (its `byteLength`
 *   becomes 0): call `view()` again after any other call rather than
 *   keeping one. Never write through a view; the bytes are the stage's.
 * - {@link ArrayHandle.copy} is one `slice` of the view: a JS-owned typed
 *   array that stays valid.
 * - {@link ArrayHandle.release} (or `using`) releases the pin; afterwards
 *   `view` and `copy` fail with `array.released`. A handle not released
 *   is released when it is garbage collected.
 */
export class ArrayHandle implements ArrayData {
  readonly type: string;
  readonly scalar: Scalar;
  readonly components: number;
  readonly length: number;
  readonly byteLength: number;
  readonly #host: ArrayHost;
  readonly #address: number;
  #buffer: ArrayBuffer | SharedArrayBuffer | null = null;
  #view: NumericElements | null = null;

  /** @internal Arrays come from values ({@link Attribute.array}). */
  constructor(host: ArrayHost, native: NativeArrayHandle) {
    const [type, scalar, components, length] = native.layout() as [string, Scalar, number, number];
    this.type = type;
    this.scalar = scalar;
    this.components = components;
    this.length = length;
    this.byteLength = native.byteLength;
    this.#address = native.address;
    this.#host = host;
    natives.set(this, native);
    hosts.set(this, host);
  }

  /** Whether it was released: it no longer pins the buffer. */
  get released(): boolean {
    return !natives.has(this);
  }

  /**
   * A typed array over the native bytes, no copy: `Float32Array` for
   * `f32` scalars, a `Uint16Array` of raw bits for `f16`, a `Uint8Array`
   * of 0 and 1 for `bool`. The same view is returned until the module's
   * memory grows. Valid until the next call into the module; never write
   * to it.
   *
   * Throws `array.released` after {@link ArrayHandle.release}, and
   * `array.no_memory` for a module that does not expose its memory.
   */
  view(): NumericElements {
    if (this.released) {
      throw new LayerstackError("array.released", "view", "the array handle was released");
    }
    const memory = this.#host.memory();
    if (memory === undefined) {
      throw new LayerstackError("array.no_memory", "view", "the module does not expose its memory; use copy()");
    }
    const buffer = memory.buffer;
    if (this.#view === null || this.#buffer !== buffer) {
      const View = VIEWS[this.scalar as Exclude<Scalar, "string">];
      this.#view = new View(buffer, this.#address, this.byteLength / View.BYTES_PER_ELEMENT);
      this.#buffer = buffer;
      this.#host.count("view", this.byteLength);
    }
    return this.#view;
  }

  /** A JS-owned copy of the elements (one `slice` of the view); each call copies. */
  copy(): NumericElements {
    if (this.released) {
      throw new LayerstackError("array.released", "copy", "the array handle was released");
    }
    if (this.#host.memory() === undefined) {
      throw new LayerstackError("array.no_memory", "copy", "the module does not expose its memory");
    }
    const copy = this.view().slice();
    this.#host.count("out", this.byteLength);
    return copy;
  }

  /** Releases the pin on the native buffer. Idempotent. */
  release(): void {
    const native = natives.get(this);
    natives.delete(this);
    this.#view = null;
    this.#buffer = null;
    native?.free();
  }

  /** Releases it, for `using`. */
  [Symbol.dispose](): void {
    this.release();
  }
}

/** A text array (`token[]`, `string[]`, `asset[]`), copied out of the module: the strings are JS-owned. */
export class ArrayCopy implements ArrayData {
  readonly type: string;
  readonly scalar: Scalar;
  readonly components: number;
  readonly length: number;
  readonly byteLength: number;
  readonly #elements: readonly string[];

  constructor(type: string, scalar: Scalar, components: number, length: number, elements: string[]) {
    this.type = type;
    this.scalar = scalar;
    this.components = components;
    this.length = length;
    this.#elements = Object.freeze(elements);
    this.byteLength = elements.reduce((sum, text) => sum + 2 * text.length, 0);
  }

  /** Always false: there is nothing native to release. */
  get released(): boolean {
    return false;
  }

  /** The strings themselves, frozen: they are already JS-owned. */
  view(): readonly string[] {
    return this.#elements;
  }

  /** A new array of the strings. */
  copy(): string[] {
    return [...this.#elements];
  }

  /** Nothing to release: the strings are JS-owned. */
  release(): void {}

  /** Nothing to release, for `using`. */
  [Symbol.dispose](): void {}
}
