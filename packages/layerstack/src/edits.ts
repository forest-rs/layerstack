// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Lowering `Edit[]` into the native batch: shapes are checked here, with
// the edit's index; everything else (paths, types, values) natively.
import { nativeOf } from "./arrays.js";
import type { ArrayHost, NativeArrayHandle } from "./arrays.js";
import { LayerstackError } from "./errors.js";
import type { ErrorCode } from "./errors.js";
import type { Edit, ValueInput } from "./types.js";

/** The native batch the generated glue exports (`layerstack_wasm::EditBatch`). Not for app use. */
export interface NativeEditBatch extends Disposable {
  free(): void;
  readonly length: number;
  valueNumbers(type: string, data: ArrayLike<number>): number;
  valueFloats(type: string, data: Float32Array): number;
  valueInt64(type: string, data: ArrayLike<bigint>): number;
  valueUInt64(type: string, data: ArrayLike<bigint>): number;
  valueHalfBits(type: string, data: Uint16Array): number;
  valueText(type: string, data: string[]): number;
  valueArray(type: string, handle: NativeArrayHandle): number;
  valueDictionary(type: string, keys: string[], slots: Uint32Array): number;
  definePrim(path: string, specifier: string, typeName?: string | null): void;
  removePrim(path: string): void;
  removeProperty(path: string): void;
  createAttribute(path: string, typeName: string, uniform: boolean, custom: boolean, value?: number | null): void;
  setValue(path: string, time: number | null | undefined, value: number): void;
  clearValue(path: string, time?: number | null): void;
  blockValue(path: string): void;
  setTargets(path: string, targets: string[]): void;
  addArc(
    payload: boolean,
    path: string,
    asset: string | null | undefined,
    primPath: string | null | undefined,
    position: string,
    offset: number,
    scale: number,
  ): void;
  removeArc(payload: boolean, path: string, asset?: string | null, primPath?: string | null): void;
  setVariantSelection(path: string, set: string, variant?: string | null): void;
  setActive(path: string, active: boolean): void;
  setInstanceable(path: string, instanceable: boolean): void;
  setKind(path: string, kind: string): void;
  setMetadata(path: string, key: string, value: number): void;
  clearMetadata(path: string, key: string): void;
}

const I64_MIN = -(2n ** 63n);
const U64_MAX = 2n ** 64n - 1n;
/** Value type names whose scalars are half floats: a `Uint16Array` is their raw bits. */
const HALF = /^(half[234]?|quath|(point|vector|normal|color|texCoord)[234]h)(\[\])?$/;

class EditFailure {
  readonly code: ErrorCode;
  readonly message: string;

  constructor(code: ErrorCode, message: string) {
    this.code = code;
    this.message = message;
  }
}

/** Pushes `edits` into `batch`, throwing `argument.invalid` (or a `value.*` code) naming the first malformed edit. */
export function lowerEdits(batch: NativeEditBatch, edits: readonly Edit[], operation: string, host: ArrayHost): void {
  if (!Array.isArray(edits)) {
    throw new LayerstackError("argument.invalid", operation, "expected an array of edits");
  }
  edits.forEach((edit, index) => {
    try {
      lowerEdit(batch, edit, host);
    } catch (error) {
      if (!(error instanceof EditFailure)) throw error;
      const path = typeof edit?.path === "string" ? edit.path : undefined;
      throw new LayerstackError(error.code, operation, `edit ${index}: ${error.message}`, { edit: index, path });
    }
  });
}

function lowerEdit(batch: NativeEditBatch, edit: Edit, host: ArrayHost): void {
  if (typeof edit !== "object" || edit === null) fail("an edit is an object with a `type`");
  const path = text(edit.path, "path");
  switch (edit.type) {
    case "definePrim":
      batch.definePrim(
        path,
        oneOf(edit.specifier ?? "def", "specifier", ["def", "over", "class"]),
        optionalText(edit.typeName, "typeName"),
      );
      return;
    case "removePrim":
      batch.removePrim(path);
      return;
    case "removeProperty":
      batch.removeProperty(path);
      return;
    case "createAttribute": {
      const typeName = text(edit.typeName, "typeName");
      const variability = oneOf(edit.variability ?? "varying", "variability", ["varying", "uniform"]);
      const value = edit.value === undefined ? null : encodeValue(batch, edit.value, host);
      batch.createAttribute(path, typeName, variability === "uniform", flag(edit.custom ?? true, "custom"), value);
      return;
    }
    case "setValue":
      batch.setValue(path, time(edit.time), encodeValue(batch, edit.value, host));
      return;
    case "clearValue":
      batch.clearValue(path, time(edit.time));
      return;
    case "blockValue":
      batch.blockValue(path);
      return;
    case "setTargets": {
      if (!Array.isArray(edit.targets)) fail("`targets` must be an array of paths");
      batch.setTargets(
        path,
        edit.targets.map((target) => text(target, "targets")),
      );
      return;
    }
    case "addReference":
    case "addPayload": {
      const offset = edit.layerOffset?.offset ?? 0;
      const scale = edit.layerOffset?.scale ?? 1;
      if (typeof offset !== "number" || typeof scale !== "number") fail("`layerOffset` holds numbers");
      batch.addArc(
        edit.type === "addPayload",
        path,
        optionalText(edit.asset, "asset"),
        optionalText(edit.primPath, "primPath"),
        oneOf(edit.position ?? "backOfPrepend", "position", [
          "frontOfPrepend",
          "backOfPrepend",
          "frontOfAppend",
          "backOfAppend",
        ]),
        offset,
        scale,
      );
      return;
    }
    case "removeReference":
    case "removePayload":
      batch.removeArc(
        edit.type === "removePayload",
        path,
        optionalText(edit.asset, "asset"),
        optionalText(edit.primPath, "primPath"),
      );
      return;
    case "setVariantSelection":
      batch.setVariantSelection(
        path,
        text(edit.set, "set"),
        edit.variant === null ? null : text(edit.variant, "variant"),
      );
      return;
    case "setActive":
      batch.setActive(path, flag(edit.active, "active"));
      return;
    case "setInstanceable":
      batch.setInstanceable(path, flag(edit.instanceable, "instanceable"));
      return;
    case "setKind":
      batch.setKind(path, text(edit.kind, "kind"));
      return;
    case "setMetadata":
      batch.setMetadata(path, text(edit.key, "key"), encodeValue(batch, edit.value, host));
      return;
    case "clearMetadata":
      batch.clearMetadata(path, text(edit.key, "key"));
      return;
    default:
      fail(`unknown edit type \`${String((edit as { type?: unknown }).type)}\``);
  }
}

/** How deep dictionaries may nest in one value, as the module allows. */
const DICTIONARY_DEPTH = 32;

/** Puts `input` into a value slot of `batch` and returns the slot. */
function encodeValue(batch: NativeEditBatch, input: ValueInput, host: ArrayHost, depth = 0): number {
  if (typeof input !== "object" || input === null || typeof input.type !== "string") {
    fail("a value is { type, value }, such as { type: \"double\", value: 2 }");
  }
  const type = input.type;
  if (type === "dictionary") return encodeDictionary(batch, (input as { value?: unknown }).value, host, depth);
  if ("kind" in input) {
    switch (input.kind) {
      case "scalar":
      case "tuple":
        return encodeData(batch, type, input.value);
      case "array": {
        const native = nativeOf(input.array, host);
        if (native !== undefined) return batch.valueArray(type, native);
        return encodeData(batch, type, input.array.copy());
      }
      default:
        throw new EditFailure("value.unsupported", `a ${input.kind} value cannot be authored yet`);
    }
  }
  return encodeData(batch, type, input.value);
}

/**
 * A dictionary's entries, each a value of its own type, into slots before
 * the dictionary's own: a `Map` or a plain object of {@link ValueInput}s,
 * or the `value` of a dictionary read from a stage.
 */
function encodeDictionary(batch: NativeEditBatch, entries: unknown, host: ArrayHost, depth: number): number {
  if (depth === DICTIONARY_DEPTH) {
    throw new EditFailure("value.shape", `dictionaries nest deeper than ${DICTIONARY_DEPTH}`);
  }
  let pairs: [string, unknown][];
  if (entries instanceof Map) pairs = [...entries.entries()];
  else if (typeof entries === "object" && entries !== null && !Array.isArray(entries) && !ArrayBuffer.isView(entries)) {
    pairs = Object.entries(entries);
  } else {
    throw new EditFailure("value.kind", "a `dictionary` value is a Map or an object of { type, value } entries");
  }
  const keys: string[] = [];
  const slots = new Uint32Array(pairs.length);
  pairs.forEach(([key, value], index) => {
    if (typeof key !== "string") fail("dictionary keys are strings");
    if (value === null || value === undefined) {
      throw new EditFailure("value.kind", `dictionary key \`${key}\` has no value to author`);
    }
    keys.push(key);
    slots[index] = encodeValue(batch, value as ValueInput, host, depth + 1);
  });
  return batch.valueDictionary("dictionary", keys, slots);
}

function encodeData(batch: NativeEditBatch, type: string, data: unknown): number {
  if (data instanceof Float32Array) return batch.valueFloats(type, data);
  if (data instanceof Uint16Array && HALF.test(type)) return batch.valueHalfBits(type, data);
  if (data instanceof BigInt64Array || data instanceof BigUint64Array) return encodeBigints(batch, type, data);
  if (ArrayBuffer.isView(data) && !(data instanceof DataView)) {
    return batch.valueNumbers(type, data as unknown as ArrayLike<number>);
  }
  const items: unknown[] = Array.isArray(data) ? data.flat() : [data];
  if (items.every((item) => typeof item === "string")) return batch.valueText(type, items as string[]);
  if (items.every((item) => typeof item === "number" || typeof item === "boolean")) {
    return batch.valueNumbers(
      type,
      items.map((item) => (item === true ? 1 : item === false ? 0 : (item as number))),
    );
  }
  if (items.every((item) => typeof item === "bigint")) return encodeBigints(batch, type, items as bigint[]);
  throw new EditFailure(
    "value.kind",
    `the scalars of a \`${type}\` must all be numbers (or booleans), all bigints or all strings`,
  );
}

/** Bigints in the buffer that holds them without wrapping; the module checks the type's range. */
function encodeBigints(batch: NativeEditBatch, type: string, items: ArrayLike<bigint>): number {
  let negative = false;
  let large = false;
  for (let i = 0; i < items.length; i += 1) {
    const item = items[i] as bigint;
    if (item < I64_MIN || item > U64_MAX) {
      throw new EditFailure("value.range", `${item} is outside the 64-bit integers`);
    }
    negative ||= item < 0n;
    large ||= item > 2n ** 63n - 1n;
  }
  if (negative && large) {
    throw new EditFailure("value.range", `the scalars of a \`${type}\` span no one 64-bit integer type`);
  }
  return large ? batch.valueUInt64(type, items) : batch.valueInt64(type, items);
}

function fail(message: string): never {
  throw new EditFailure("argument.invalid", message);
}

function text(value: unknown, field: string): string {
  if (typeof value !== "string") fail(`\`${field}\` must be a string`);
  return value;
}

function optionalText(value: unknown, field: string): string | null {
  return value === undefined || value === null ? null : text(value, field);
}

function flag(value: unknown, field: string): boolean {
  if (typeof value !== "boolean") fail(`\`${field}\` must be a boolean`);
  return value;
}

function time(value: unknown): number | null {
  if (value === undefined || value === null) return null;
  if (typeof value !== "number" || !Number.isFinite(value)) fail("`time` must be a finite time code");
  return value;
}

function oneOf<const T extends string>(value: unknown, field: string, allowed: readonly T[]): T {
  if (!allowed.includes(value as T)) fail(`\`${field}\` must be one of ${allowed.join(", ")}`);
  return value as T;
}
