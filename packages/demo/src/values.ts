// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Values as the editor shows them, and text typed into the inspector as a
// value to author.
import type { ScalarInput, Value, ValueInput } from "@forest-rs/layerstack";

/** The most elements an array's text shows. */
const SHOWN_ELEMENTS = 4;

/** A number as the inspector shows it: a `float` read as a double keeps 6 significant digits. */
export function formatNumber(value: number | bigint): string {
  if (typeof value === "bigint" || Number.isInteger(value)) return String(value);
  return String(Number(value.toPrecision(6)));
}

/**
 * A value in USDA-like text: `2`, `(0.8, 0.2, 0.1)`, `"text"`,
 * `[(-1, -1, -1), (1, 1, 1)] (2 elements)`. An array is read through a view of
 * the module's memory, with no copy; the caller still owns the value.
 */
export function formatValue(value: Value | null): string {
  if (value === null) return "none";
  switch (value.kind) {
    case "scalar":
      return typeof value.value === "string"
        ? JSON.stringify(value.value)
        : typeof value.value === "boolean"
          ? String(value.value)
          : formatNumber(value.value);
    case "tuple":
      return `(${value.value.map(formatNumber).join(", ")})`;
    case "array": {
      const { array } = value;
      const view = array.view();
      const shown: string[] = [];
      for (let element = 0; element < Math.min(array.length, SHOWN_ELEMENTS); element += 1) {
        const scalars: string[] = [];
        for (let component = 0; component < array.components; component += 1) {
          const scalar = view[element * array.components + component];
          scalars.push(typeof scalar === "string" ? JSON.stringify(scalar) : formatNumber(scalar as number | bigint));
        }
        shown.push(array.components === 1 ? (scalars[0] ?? "") : `(${scalars.join(", ")})`);
      }
      const more = array.length > SHOWN_ELEMENTS ? ", ..." : "";
      return `[${shown.join(", ")}${more}] (${array.length} element${array.length === 1 ? "" : "s"})`;
    }
    case "dictionary":
      return `{ ${value.value.size} entr${value.value.size === 1 ? "y" : "ies"} }`;
    case "list":
      return `[${value.value.map(formatValue).join(", ")}]`;
    case "opaque":
      return `<${value.type}>`;
  }
}

/** Releases what a value holds in the module: an array's pinned buffer, nested ones included. */
export function releaseValue(value: Value | null): void {
  if (value === null) return;
  if (value.kind === "array") value.array.release();
  else if (value.kind === "dictionary") for (const entry of value.value.values()) releaseValue(entry);
  else if (value.kind === "list") for (const item of value.value) releaseValue(item);
}

/**
 * Text typed into the inspector as a value of `type`, shaped like
 * `current` (the attribute's value now), if it has one: a tuple's numbers
 * from `(1, 0, 0)` or `1 0 0`, a number, `true`/`false`, or text for a
 * string-valued type. The module checks the result against the type
 * (`value.shape`, `value.kind`, `value.range`), so the editor does not
 * second-guess it.
 */
export function parseValue(type: string, text: string, current: Value | null): ValueInput {
  const trimmed = text.trim();
  if (current?.kind === "scalar" && typeof current.value === "string") {
    return { type, value: unquote(trimmed) };
  }
  const numbers = trimmed.replace(/^[[(]|[\])]$/g, "").split(/[\s,]+/).filter((part) => part !== "");
  if (current?.kind === "tuple" || numbers.length > 1) {
    return { type, value: numbers.map(scalar) };
  }
  return { type, value: scalar(unquote(trimmed)) };
}

function scalar(text: string): ScalarInput {
  if (text === "true") return true;
  if (text === "false") return false;
  const number = Number(text);
  return text !== "" && Number.isFinite(number) ? number : text;
}

function unquote(text: string): string {
  return text.length >= 2 && text.startsWith('"') && text.endsWith('"') ? text.slice(1, -1) : text;
}
