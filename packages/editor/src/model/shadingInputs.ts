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
import type { ShaderPort, Stage } from "@forest-rs/layerstack";
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
  readonly port: ShaderPort;
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

/** `ports` (of the prim at `prim`) grouped, labelled and ordered as the header of this file describes. */
export function groupInputs(stage: Stage, prim: string, ports: readonly ShaderPort[]): ShadingInputGroup[] {
  const inputs = ports.map((port, index) => ({ input: describe(stage, prim, port), index }));
  inputs.sort((a, b) => (a.input.order ?? Infinity) - (b.input.order ?? Infinity) || a.index - b.index);
  const groups = new Map<string, ShadingInput[]>();
  for (const { input } of inputs) {
    const group = groups.get(input.group) ?? [];
    group.push(input);
    groups.set(input.group, group);
  }
  return [...groups].map(([name, grouped]) => ({ name, inputs: grouped }));
}

function describe(stage: Stage, prim: string, port: ShaderPort): ShadingInput {
  const attribute = stage.attribute(port.path);
  const text = (key: string) => {
    const value = attribute?.metadata(key) ?? null;
    return value?.kind === "scalar" && typeof value.value === "string" && value.value !== "" ? value.value : null;
  };
  const sdr = (key: string) => {
    const value = port.sdrMetadata.get(key);
    return value === undefined || value === "" ? null : value;
  };
  const number = (key: string) => {
    const value = sdr(key);
    if (value === null || value.includes(",")) return null;
    const parsed = Number(value);
    return Number.isFinite(parsed) ? parsed : null;
  };
  const explanation = attribute?.explain() ?? null;
  releaseValue(explanation?.value ?? null);
  const opinion = explanation?.opinions.find((found) => found.role !== "ignored") ?? null;
  return {
    port,
    label: text("displayName") ?? sdr("uiname") ?? port.name,
    group: text("displayGroup") ?? UNGROUPED,
    order: number("uiorder"),
    bounds: { min: number("uimin"), max: number("uimax"), softMin: number("uisoftmin"), softMax: number("uisoftmax") },
    own: opinion !== null && opinion.specPath.slice(0, opinion.specPath.lastIndexOf(".")) === prim,
  };
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
