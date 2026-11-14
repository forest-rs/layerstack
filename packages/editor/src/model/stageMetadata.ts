// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The stage's metadata as the Stage tab edits it: which fields there are
// and what each authors, the units `metersPerUnit` is usually set to, and
// which layer holds each opinion. The stage reads its metadata from the
// session layer over the root layer, and from no other: a sublayer's
// `upAxis` is not the stage's (AOUSD Core §12.2.7, layer metadata
// resolution; OpenUSD's `UsdStage::GetMetadata`). So an opinion is
// authored in one of those two, whatever layer other edits go to.
//
// The stage reads a field's composed value (`Stage.metadata`, its fallback
// when nothing authors it); which of the two layers holds an opinion is
// read from each one's pseudo-root spec (`Stage.layerSpec(layer, "/")`),
// whose `fields` name what it authors by their OpenUSD keys.

/** A field of the stage's metadata the Stage tab edits. */
export interface StageField {
  /** Its key (`setLayerMetadata`'s). */
  readonly key: string;
  /** What the tab calls it. */
  readonly label: string;
  /** The value type it is authored as. */
  readonly type: "token" | "double" | "string";
  /** What it does, in a sentence. */
  readonly about: string;
}

/** The fields the Stage tab edits, in its order. */
export const STAGE_FIELDS: readonly StageField[] = [
  { key: "upAxis", label: "Up axis", type: "token", about: "The axis that points up: Y or Z (UsdGeomGetStageUpAxis). The viewport orbits about it." },
  { key: "metersPerUnit", label: "Meters per unit", type: "double", about: "How long one unit of the scene is, in meters (UsdGeomGetStageMetersPerUnit)." },
  { key: "defaultPrim", label: "Default prim", type: "token", about: "The root prim a reference or payload to this layer without a prim path brings in." },
  { key: "startTimeCode", label: "Start time code", type: "double", about: "The first time code of the stage's animation: where the timeline starts." },
  { key: "endTimeCode", label: "End time code", type: "double", about: "The last time code of the stage's animation: where the timeline ends." },
  { key: "timeCodesPerSecond", label: "Time codes per second", type: "double", about: "How many time codes make a second: what playback runs at, and what scales time across a reference to a layer with another rate." },
  { key: "framesPerSecond", label: "Frames per second", type: "double", about: "The rate a client should play at, a hint (usdview's): the editor plays at time codes per second." },
  { key: "documentation", label: "Documentation", type: "string", about: "What the layer is, in words (`doc` in USDA)." },
] as const;

/** A unit `metersPerUnit` is commonly set to (`UsdGeomLinearUnits`). */
export interface UnitPreset {
  readonly name: string;
  /** Its abbreviation, as the picker shows it. */
  readonly short: string;
  readonly metersPerUnit: number;
}

/** The units of `UsdGeomLinearUnits`, smallest first. */
export const UNIT_PRESETS: readonly UnitPreset[] = [
  { name: "nanometers", short: "nm", metersPerUnit: 1e-9 },
  { name: "micrometers", short: "µm", metersPerUnit: 1e-6 },
  { name: "millimeters", short: "mm", metersPerUnit: 0.001 },
  { name: "centimeters", short: "cm", metersPerUnit: 0.01 },
  { name: "inches", short: "in", metersPerUnit: 0.0254 },
  { name: "feet", short: "ft", metersPerUnit: 0.3048 },
  { name: "yards", short: "yd", metersPerUnit: 0.9144 },
  { name: "meters", short: "m", metersPerUnit: 1 },
  { name: "kilometers", short: "km", metersPerUnit: 1000 },
  { name: "miles", short: "mi", metersPerUnit: 1609.344 },
];

/**
 * The preset `metersPerUnit` is, within the precision a layer keeps
 * (`UsdGeomLinearUnitsAre`'s relative tolerance of 1e-5); `null` for
 * another value.
 */
export function unitPreset(metersPerUnit: number): UnitPreset | null {
  return UNIT_PRESETS.find((preset) => Math.abs(preset.metersPerUnit - metersPerUnit) <= 1e-5 * Math.max(Math.abs(preset.metersPerUnit), Math.abs(metersPerUnit))) ?? null;
}

/** Where a stage metadata field's value comes from. */
export type FieldSource =
  /** The session layer authors it, over the root's. */
  | "session"
  /** The root layer authors it. */
  | "root"
  /** Neither does: the value is the field's fallback (`upAxis` Y, `metersPerUnit` 0.01, rates 24), or there is none. */
  | "fallback";

/** Which layer the stage reads `key` from, given the fields the session layer's and the root's pseudo-roots author. */
export function fieldSource(key: string, session: ReadonlySet<string> | null, root: ReadonlySet<string>): FieldSource {
  if (session?.has(key) === true) return "session";
  return root.has(key) ? "root" : "fallback";
}

/** Where a stage metadata edit goes ({@link metadataLayer}), and why there when it is not where other edits go. */
export interface MetadataPlacement<L> {
  readonly layer: L;
  /** Why it is not where edits go; `null` when it is. */
  readonly note: string | null;
}

/**
 * The layer a stage metadata edit is authored in: where edits go when
 * that is the root or the session layer, whose metadata the stage reads;
 * otherwise the root (the session layer when the root is read-only, a
 * package), and a note saying so. `target` is the layer edits go to
 * (`null` for an edit target through an arc).
 */
export function metadataLayer<L extends { readonly identifier: string | null; readonly readOnly: boolean }>(
  target: L | null,
  root: L,
  session: L | null,
  name: (layer: L) => string,
): MetadataPlacement<L> {
  if (target !== null && (target === root || target === session) && !target.readOnly) return { layer: target, note: null };
  const layer = root.readOnly && session !== null ? session : root;
  const where = target === null ? "an edit target through an arc" : name(target);
  return {
    layer,
    note: `Edits go to ${where}, but the stage reads its metadata only from the root and session layers: this is authored in ${name(layer)}.`,
  };
}
