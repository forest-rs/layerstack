// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A material's or shader's inputs as the inspector lays them out, from
// the metadata shading authors give them:
//
// - `displayGroup` (attribute metadata) groups them; inputs without one
//   are "Inputs".
// - `displayName` labels one, else its `sdrMetadata` `uiname`, else its
//   name.
// - `sdrMetadata` `uiorder` orders them (as the shader registry's
//   `SdrShaderProperty` page order does), then property order; groups
//   come in the order of their first input.
// - `uimin`/`uimax` are hard bounds a typed value is clamped to;
//   `uisoftmin`/`uisoftmax` the range a slider covers where given, else
//   the hard bounds.
// - A shader whose `info:id` the editor knows (`shaderDefinitions.ts`)
//   has its definition's inputs too, those the prim does not author with
//   their fallback; without a `uiorder`, inputs come in the definition's
//   order, so one keeps its place once an edit authors it.
import type { ShaderPort, Stage, ValueInput } from "@forest-rs/layerstack";
import { type DefinedInput, definedInput, type ShaderDefinition } from "./shaderDefinitions.ts";
import { releaseValue } from "./values.ts";

/** The slider range and clamps of a number input; each `null` when not given. */
export interface InputBounds {
  readonly min: number | null;
  readonly max: number | null;
  readonly softMin: number | null;
  readonly softMax: number | null;
}

/** One input as the inspector shows it. */
export interface ShadingInput {
  /** Its property path. */
  readonly path: string;
  /** Its name without namespace. */
  readonly name: string;
  readonly typeName: string | null;
  /** The port the stage reads; `null` for an input only its shader's definition has. */
  readonly port: ShaderPort | null;
  /** Its shader definition's, when the editor knows the shader's id. */
  readonly definition: DefinedInput | null;
  /** Its label: `displayName`, `uiname` or its name. */
  readonly label: string;
  readonly group: string;
  /** `uiorder`, or `null`. */
  readonly order: number | null;
  readonly bounds: InputBounds;
  /**
   * Whether its value's strongest opinion is on this prim's own spec,
   * rather than one it inherits or references (a base material's): what
   * this prim changes.
   */
  readonly own: boolean;
}

/** A display group and its inputs, in order. */
export interface ShadingInputGroup {
  readonly name: string;
  readonly inputs: readonly ShadingInput[];
}

/** The group of inputs without a `displayGroup`. */
export const UNGROUPED = "Inputs";

/**
 * `ports` (of the prim at `prim`), with `definition`'s inputs the prim
 * does not author, grouped, labelled and ordered as the header of this
 * file describes.
 */
export function groupInputs(stage: Stage, prim: string, ports: readonly ShaderPort[], definition: ShaderDefinition | null = null): ShadingInputGroup[] {
  const defined = definition?.inputs ?? [];
  const position = new Map(defined.map((each, index) => [each.name, index]));
  const authored = new Set(ports.map((port) => port.name));
  const inputs = [
    ...ports.map((port, index) => ({ input: describe(stage, prim, port, definedInput(definition, port.name)), index: position.get(port.name) ?? defined.length + index })),
    ...defined.filter((each) => !authored.has(each.name)).map((each) => ({ input: unauthored(prim, each), index: position.get(each.name)! })),
  ];
  inputs.sort((a, b) => (a.input.order ?? Infinity) - (b.input.order ?? Infinity) || a.index - b.index);
  const groups = new Map<string, ShadingInput[]>();
  for (const { input } of inputs) {
    const group = groups.get(input.group) ?? [];
    group.push(input);
    groups.set(input.group, group);
  }
  return [...groups].map(([name, grouped]) => ({ name, inputs: grouped }));
}

/** An input only `definition` has: named by it, ungrouped, unbounded, not set here. */
function unauthored(prim: string, definition: DefinedInput): ShadingInput {
  return {
    path: `${prim}.inputs:${definition.name}`,
    name: definition.name,
    typeName: definition.typeName,
    port: null,
    definition,
    label: definition.name,
    group: UNGROUPED,
    order: null,
    bounds: { min: null, max: null, softMin: null, softMax: null },
    own: false,
  };
}

function describe(stage: Stage, prim: string, port: ShaderPort, definition: DefinedInput | null): ShadingInput {
  const attribute = stage.attribute(port.path);
  const text = (key: string) => {
    const value = attribute?.metadata(key) ?? null;
    return value?.kind === "scalar" && typeof value.value === "string" && value.value !== "" ? value.value : null;
  };
  const sdr = (key: string) => {
    const value = port.sdrMetadata.get(key);
    return value === undefined || value === "" ? null : value;
  };
  const number = (key: string) => sdrNumber(port.sdrMetadata, key);
  const explanation = attribute?.explain() ?? null;
  releaseValue(explanation?.value ?? null);
  const opinion = explanation?.opinions.find((found) => found.role !== "ignored") ?? null;
  return {
    path: port.path,
    name: port.name,
    typeName: port.typeName,
    port,
    definition,
    label: text("displayName") ?? sdr("uiname") ?? port.name,
    group: text("displayGroup") ?? UNGROUPED,
    order: number("uiorder"),
    bounds: inputBounds(port.sdrMetadata),
    own: opinion !== null && opinion.specPath.slice(0, opinion.specPath.lastIndexOf(".")) === prim,
  };
}

/** The number the `sdrMetadata` entry `key` gives; `null` for none, or one that is not a single number. */
function sdrNumber(sdrMetadata: ReadonlyMap<string, string>, key: string): number | null {
  const value = sdrMetadata.get(key);
  if (value === undefined || value === "" || value.includes(",")) return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** The bounds a port's `sdrMetadata` gives: `uimin`, `uimax`, `uisoftmin`, `uisoftmax`. */
export function inputBounds(sdrMetadata: ReadonlyMap<string, string>): InputBounds {
  return { min: sdrNumber(sdrMetadata, "uimin"), max: sdrNumber(sdrMetadata, "uimax"), softMin: sdrNumber(sdrMetadata, "uisoftmin"), softMax: sdrNumber(sdrMetadata, "uisoftmax") };
}

/** `value` as an edit authors it for an input with `bounds`: a number within the hard bounds, anything else as it is. */
export function withinBounds(value: ValueInput, bounds: InputBounds): ValueInput {
  if ("kind" in value || !("type" in value) || typeof value.value !== "number") return value;
  return { type: value.type, value: clampToBounds(value.value, bounds) };
}

/** The range a slider covers: the soft bounds where both are given, else the hard ones; `null` without both ends. */
export function sliderRange(bounds: InputBounds): readonly [number, number] | null {
  const min = bounds.softMin ?? bounds.min;
  const max = bounds.softMax ?? bounds.max;
  return min === null || max === null || !(max > min) ? null : [min, max];
}

/** `value` within the hard bounds. */
export function clampToBounds(value: number, bounds: InputBounds): number {
  return Math.min(bounds.max ?? Infinity, Math.max(bounds.min ?? -Infinity, value));
}
