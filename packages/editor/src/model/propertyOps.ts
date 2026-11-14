// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What authoring a property does where edits go, read from the stage with
// no DOM: which opinion of an attribute the edit target holds (its
// default, a block, its time samples, read from its layer), whether an edit
// there shows (a stronger opinion may still win), the names a new
// property may take, the value types it may declare and the value it
// starts with, and a relationship's target list edited one target at a
// time. The store's property operations and the inspector's commands
// decide from these what applies.
import type { Attribute, EditTarget, Explanation, Layer, LayerSpec, Opinion, ScalarInput, Stage, ValueInput, ValueTypeInfo } from "@forest-rs/layerstack";
import { formatNumber, isQuaternion, realFirst, releaseValue, scalarAt } from "./values.ts";

/** Where edits go, as the session holds it: a layer of the stage's stack, an edit target, or `undefined` for the stage's edit layer. */
export type Target = Layer | EditTarget | undefined;

/** The spec an edit of a property path writes: its layer, and its path there. */
export interface TargetSite {
  readonly layer: Layer;
  readonly specPath: string;
}

/**
 * Where an edit of `path` through `target` is written (OpenUSD's
 * `UsdEditTarget::MapToSpecPath`): the edit layer, or a stack layer, at
 * `path` itself; an edit target at the path it maps `path` to. `null`
 * when there is no edit layer, or the target does not map `path`.
 */
export function targetSite(stage: Stage, target: Target, path: string): TargetSite | null {
  if (target === undefined) {
    const layer = stage.editLayer;
    return layer === null ? null : { layer, specPath: path };
  }
  if (!("kind" in target)) return { layer: target, specPath: path };
  try {
    const specPath = target.mapPath(path);
    return specPath === null ? null : { layer: target.layer, specPath };
  } catch {
    // A stale target maps nothing.
    return null;
  }
}

/** Whether `opinion` is the one written at `site`. */
export function atSite(opinion: Opinion, site: TargetSite): boolean {
  return opinion.layer.id === site.layer.id && opinion.specPath === site.specPath;
}

/** `attribute`'s explanation at `time`, its value released: only the opinions are wanted. */
function explain(attribute: Attribute, time?: number): Explanation | null {
  const explanation = attribute.explain(time);
  if (explanation !== null) releaseValue(explanation.value);
  return explanation;
}

/**
 * The time in the target's layer a stage time is written at
 * (`UsdEditTarget`'s time mapping, `EditTarget.mapTime`): through an edit
 * target its own offset (none inside a variant on its prim, whatever
 * offset its layer is read at); in a layer of the stage's stack, or the
 * edit layer, the offset the stack reads the layer at.
 */
function layerTime(stage: Stage, target: Target, site: TargetSite, time: number): number {
  const through = target !== undefined && "kind" in target ? target : stage.editTargetForLayer(site.layer);
  return through.mapTime(time);
}

/** What the edit target's layer authors of an attribute, read from its spec there (`Stage.layerSpec`). */
export interface TargetOpinion {
  /** Where the target writes the attribute; `null` when it does not map it. */
  readonly site: TargetSite | null;
  /** The spec the target's layer holds where it writes it, of either kind of property (what removing it removes); `null` when none. */
  readonly spec: LayerSpec | null;
  /** Its default there: a value, a block (`UsdAttribute::Block`), or none. */
  readonly default: "value" | "block" | null;
  /**
   * The times of the samples the target's layer holds, in the layer's own
   * time ({@link layerTime} maps a stage time to it), whether the stage
   * reads them or a stronger opinion hides them: clearing the attribute
   * there clears them all, as `UsdAttribute::Clear` does.
   */
  readonly samples: readonly number[];
  /** Whether any layer has an opinion of its value (a default, a block or samples). */
  readonly any: boolean;
}

/**
 * What the edit target authors of `attribute`: the default and the time
 * samples of its spec where the target writes it. They are read from the
 * layer, not from the opinions the stage consults or the sample times it
 * composes: samples (or a default) a stronger opinion hides are still the
 * target's to clear, key and remove, and the stage's sample times are the
 * layer's only through the target's own time mapping.
 */
export function targetOpinion(stage: Stage, attribute: Attribute, target: Target): TargetOpinion {
  const site = targetSite(stage, target, attribute.path);
  const spec = specAt(stage, site);
  const own = spec?.type === "attribute" ? spec : null;
  return {
    site,
    spec,
    default: own === null || own.default === null ? null : own.default === "block" ? "block" : "value",
    samples: own?.timeSamples ?? [],
    any: (explain(attribute)?.opinions.length ?? 0) > 0 || attribute.timeSamples().length > 0,
  };
}

/**
 * Which property operations would change something of a property where
 * edits go: what the inspector offers on it. Each one that would do
 * nothing is `false`.
 */
export interface PropertyActions {
  /** Clear its value: the target's layer holds its default (or block), or samples of it. */
  readonly clear: boolean;
  /** Block its value: some layer gives it one, and the target does not block it already. */
  readonly block: boolean;
  /** Remove it: the target's layer holds a spec of it (`Stage.layerSpec`), one left with nothing in it included. */
  readonly remove: boolean;
  /** Key it at the time: the stage has a time range, it may vary, it has a value there, and the target has no key there. */
  readonly key: boolean;
  /** Delete the key at the time: the target's layer holds a sample where it writes that time. */
  readonly deleteKey: boolean;
  /**
   * Deleting the key leaves the target's spec empty: that sample is all
   * it holds (no default, no connections, no other sample: what keying it
   * there made), so the spec stays with nothing in it, for Remove to take.
   */
  readonly lastKey: boolean;
}

const NONE: PropertyActions = { clear: false, block: false, remove: false, key: false, deleteKey: false, lastKey: false };

/** The fields a spec keying an attribute made holds besides its samples: its declaration. */
const DECLARATION = new Set(["typeName", "custom", "variability", "timeSamples"]);

/** The spec the target's layer holds at `site`; `null` when none, or the path names no spec. */
function specAt(stage: Stage, site: TargetSite | null): LayerSpec | null {
  if (site === null) return null;
  try {
    return stage.layerSpec(site.layer, site.specPath);
  } catch {
    return null;
  }
}

/**
 * Whether `spec` is a property spec, of either kind: what Remove takes
 * (`UsdPrim::RemoveProperty` removes the target's property spec, whatever
 * it is). A weaker layer may hold an attribute where the stage composes a
 * stronger layer's relationship of the same name, and the reverse.
 */
function holdsProperty(spec: LayerSpec | null): boolean {
  return spec !== null && (spec.type === "attribute" || spec.type === "relationship");
}

/**
 * What the property operations would do to the property at `path` now,
 * written through `target`, at `time` (`undefined` when the stage has no
 * time range: nothing is keyed). Nothing applies on an instance proxy,
 * whose properties come from its prototype and cannot be edited there.
 */
export function propertyActions(stage: Stage, path: string, target: Target, time: number | undefined): PropertyActions {
  const dot = path.indexOf(".", path.lastIndexOf("/"));
  if (dot < 0) return NONE;
  try {
    const prim = stage.prim(path.slice(0, dot));
    if (prim === null || prim.instanceProxy) return NONE;
    const attribute = stage.attribute(path);
    if (attribute === null) {
      if (stage.relationship(path) === null) return NONE;
      return { ...NONE, remove: holdsProperty(specAt(stage, targetSite(stage, target, path))) };
    }
    const own = targetOpinion(stage, attribute, target);
    const spec = own.spec;
    const keyed = time !== undefined && own.site !== null && own.samples.includes(layerTime(stage, target, own.site, time));
    let key = false;
    if (time !== undefined && attribute.variability === "varying" && !keyed) {
      const value = attribute.get(time);
      key = value !== null;
      releaseValue(value);
    }
    return {
      clear: own.default !== null || own.samples.length > 0,
      block: own.site !== null && own.any && !(own.default === "block" && own.samples.length === 0),
      remove: holdsProperty(spec),
      key: own.site !== null && key,
      deleteKey: keyed,
      lastKey:
        keyed &&
        spec?.type === "attribute" &&
        spec.default === null &&
        spec.connections === null &&
        spec.timeSamples?.length === 1 &&
        spec.fields.every((field) => DECLARATION.has(field)),
    };
  } catch {
    // Not a property path the stage takes: nothing to offer.
    return NONE;
  }
}

/**
 * An edit that does not show: the edit target now holds an opinion, but
 * another one still gives the value (a stronger layer's, or, when the
 * target is not composed, whatever the stage reads instead).
 */
export interface Shadow {
  /** The property edited. */
  readonly path: string;
  /** The time edited at: a sample's time, or `null` for the default. */
  readonly time: number | null;
  /** The layer whose opinion gives the value. */
  readonly layer: Layer;
  /** That opinion's spec in its layer. */
  readonly specPath: string;
  /** Where an edit replaces that opinion (`Opinion.editTarget`); `null` when there is none. */
  readonly editTarget: EditTarget | null;
  /** Whether the stage composes the edit target: false for a variant not selected, or an arc no longer there. */
  readonly composed: boolean;
}

/**
 * Whether an edit of `attribute` at `time` (a sample's, or `null` for the
 * default) written through `target` shows: `null` when the target's
 * opinion is the one the stage reads (or nothing gives a value), else the
 * opinion that wins instead ({@link Shadow}). Read once, after the edit,
 * not as the inspector renders.
 */
export function shadowOf(stage: Stage, attribute: Attribute, target: Target, time: number | null): Shadow | null {
  const site = targetSite(stage, target, attribute.path);
  if (site === null) return null;
  const explanation = explain(attribute, time ?? undefined);
  const winner = explanation?.opinions.find((opinion) => opinion.role !== "ignored");
  if (winner === undefined || atSite(winner, site)) return null;
  let composed = true;
  if (target !== undefined && "kind" in target) {
    try {
      composed = target.composed();
    } catch {
      composed = false;
    }
  }
  return { path: attribute.path, time, layer: winner.layer, specPath: winner.specPath, editTarget: winner.editTarget, composed };
}

/** A property name as USD takes it: identifiers joined by `:` (`radius`, `primvars:displayColor`). */
const PROPERTY_NAME = /^[A-Za-z_][A-Za-z0-9_]*(:[A-Za-z_][A-Za-z0-9_]*)*$/;

/**
 * Why `name` cannot name a new property among `taken` (the prim's
 * properties, authored or defined by its schemas), or `null` when it can:
 * it must be a namespaced identifier, identifiers joined by the
 * namespace delimiter `:` (AOUSD Core §7.3.3 names, §8.5 path grammar).
 */
export function propertyNameProblem(name: string, taken: ReadonlySet<string>): string | null {
  if (name === "") return "Name the property.";
  if (!PROPERTY_NAME.test(name)) {
    return /::|^:|:$/.test(name)
      ? "Each part between colons is a name: a letter or _ then letters, digits or _."
      : "A property name is a letter or _ then letters, digits or _, in namespaces joined by colons (primvars:tint).";
  }
  if (taken.has(name)) return `The prim already has a property ${name}.`;
  return null;
}

/** A group of value types the Add property dialog offers: their scalar forms, each with its array form ({@link ValueTypeInfo.arrayType}). */
export interface ValueTypeGroup {
  readonly group: string;
  readonly types: readonly ValueTypeInfo[];
}

/** The group a scalar value type is offered in, by its shape and role (AOUSD Core §6.2, §6.3, §6.5). */
function valueTypeGroup(type: ValueTypeInfo): string {
  switch (type.role) {
    case "Color":
      return "Colors";
    case "Point":
    case "Vector":
    case "Normal":
      return "Points, vectors and normals";
    case "TextureCoordinate":
      return "Texture coordinates";
    case "Frame":
      return "Matrices";
    default:
      return type.dimensions.length === 0 ? "Single values" : type.dimensions.length === 1 ? "Tuples and quaternions" : "Matrices";
  }
}

/**
 * The value types a new attribute may declare, grouped as the inspector
 * offers them, from the runtime's registry (`Layerstack.valueTypes`,
 * OpenUSD's `Sdf.ValueTypeNames`), in its order: each scalar type, whose
 * array form the dialog's Array switch picks. Left out: `opaque` and
 * `group`, which hold no value and have no array form, and the legacy
 * types (`Transform`, `PointIndex`, ...), which OpenUSD still reads but
 * new scene description does not declare.
 */
export function valueTypeGroups(types: readonly ValueTypeInfo[]): ValueTypeGroup[] {
  const groups = new Map<string, ValueTypeInfo[]>();
  for (const type of types) {
    if (type.isArray || type.arrayType === null || type.legacy) continue;
    const group = valueTypeGroup(type);
    const members = groups.get(group);
    if (members === undefined) groups.set(group, [type]);
    else members.push(type);
  }
  return [...groups].map(([group, members]) => ({ group, types: members }));
}

/**
 * The value a new attribute of `typeName` starts with, so it is shown
 * ready to edit: an empty array, `false`, empty text, zeros, an identity
 * matrix or rotation. A quaternion is given as the bindings take one,
 * imaginary part first (`[i, j, k, r]`), so the identity is
 * `[0, 0, 0, 1]`, which USDA writes real part first, `(1, 0, 0, 0)`.
 * `null` for a type it does not know.
 */
export function initialValue(typeName: string): ValueInput | null {
  if (typeName.endsWith("[]")) {
    // An empty array of the type's scalars: a plain `[]` would read as text.
    const scalar = typeName.slice(0, -2);
    if (scalar === "token" || scalar === "string" || scalar === "asset" || scalar === "pathExpression") return { type: typeName, value: [] };
    if (scalar === "int64") return { type: typeName, value: new BigInt64Array(0) };
    if (scalar === "uint64") return { type: typeName, value: new BigUint64Array(0) };
    return { type: typeName, value: new Float64Array(0) };
  }
  if (typeName === "bool") return { type: typeName, value: false };
  if (typeName === "token" || typeName === "string" || typeName === "asset" || typeName === "pathExpression") return { type: typeName, value: "" };
  if (/^(u?int|u?int64|uchar|half|float|double|timecode|duration)$/.test(typeName)) return { type: typeName, value: 0 };
  const matrix = /^(?:matrix|frame)([234])d$/.exec(typeName);
  if (matrix !== null) {
    const size = Number(matrix[1]);
    return { type: typeName, value: Array.from({ length: size }, (_, row) => Array.from({ length: size }, (_, column) => (row === column ? 1 : 0))) };
  }
  if (isQuaternion(typeName)) return { type: typeName, value: [0, 0, 0, 1] };
  const tuple = /([234])[hfdi]?$/.exec(typeName);
  if (tuple !== null) return { type: typeName, value: new Array<number>(Number(tuple[1])).fill(0) };
  return null;
}

/** What adding a schema-defined attribute authors as its value ({@link schemaStart}). */
export interface SchemaStart {
  /** The value to author; `null` to declare the attribute with none (a fallback there is no input for). */
  readonly value: ValueInput | null;
  /** Whether it is the schema's fallback; false when the schema gives none and it is the type's starting value. */
  readonly fallback: boolean;
}

/**
 * The value an attribute its prim's schema defines, and no layer authors,
 * is added with: the schema's fallback, so adding it changes nothing the
 * stage reads (a `Cube`'s `extent` stays its two corners, not an empty
 * array over them), or the type's starting value ({@link initialValue})
 * when the schema gives no fallback. The fallback is copied out of the
 * module, so it may be authored after this returns.
 */
export function schemaStart(attribute: Attribute): SchemaStart {
  const typeName = attribute.typeName;
  if (typeName === null) return { value: null, fallback: false };
  const value = attribute.get();
  try {
    if (value === null) return { value: initialValue(typeName), fallback: false };
    if (value.kind === "scalar" || value.kind === "tuple") return { value, fallback: true };
    if (value.kind === "list") {
      const elements: (ScalarInput | readonly ScalarInput[])[] = [];
      for (const item of value.value) {
        if (item?.kind !== "scalar" && item?.kind !== "tuple") return { value: null, fallback: true };
        elements.push(item.value);
      }
      return { value: { type: typeName, value: elements }, fallback: true };
    }
    if (value.kind === "array") {
      const { array } = value;
      const view = array.view();
      const elements: (ScalarInput | readonly ScalarInput[])[] = [];
      for (let element = 0; element < array.length; element += 1) {
        const scalars = Array.from({ length: array.components }, (_, component) => scalarAt(array, view, element * array.components + component));
        elements.push(array.components === 1 ? scalars[0]! : scalars);
      }
      return { value: { type: typeName, value: elements }, fallback: true };
    }
    return { value: null, fallback: true };
  } finally {
    releaseValue(value);
  }
}

/**
 * A value to author as the layer's USDA writes it after the attribute's
 * name (`0`, `(0, 0, 0)`, `""`, `@@`, `[]`, `( (1, 0), (0, 1) )`): what
 * the Add property dialog shows will be authored. A quaternion is written
 * real part first, a `bool` as `0` or `1`, an asset path between `@`.
 * Empty for a value it has no text for (a dictionary, an array read from
 * the stage).
 */
export function usdaValueText(input: ValueInput | null): string {
  if (input === null || !("value" in input)) return "";
  const { type } = input;
  const value = input.value as unknown;
  const one = (scalar: unknown): string =>
    typeof scalar === "string" ? (type.startsWith("asset") ? `@${scalar}@` : JSON.stringify(scalar)) : typeof scalar === "boolean" ? (scalar ? "1" : "0") : typeof scalar === "number" ? formatNumber(scalar) : String(scalar);
  const tuple = (items: readonly unknown[]): string => `(${items.map(one).join(", ")})`;
  const element = (items: readonly unknown[]): string => tuple(isQuaternion(type) ? realFirst(items) : items);
  // A typed array's elements are not told apart here: only an empty one has text.
  if (ArrayBuffer.isView(value)) return (value as unknown as ArrayLike<unknown>).length === 0 ? "[]" : "";
  if (!Array.isArray(value)) return one(value);
  if (type.endsWith("[]")) return `[${value.map((item: unknown) => (Array.isArray(item) ? element(item) : one(item))).join(", ")}]`;
  if (value.some(Array.isArray)) return `( ${value.map((row: unknown) => tuple(row as unknown[])).join(", ")} )`;
  return element(value);
}

/** `targets` with `path` added at the end; unchanged when it is there already. */
export function withTarget(targets: readonly string[], path: string): string[] {
  return targets.includes(path) ? [...targets] : [...targets, path];
}

/** `targets` without `path`. */
export function withoutTarget(targets: readonly string[], path: string): string[] {
  return targets.filter((target) => target !== path);
}

/** `targets` with the one at `from` moved by `by` places (−1 earlier, 1 later); unchanged past either end. */
export function movedTarget(targets: readonly string[], from: number, by: number): string[] {
  const to = from + by;
  if (from < 0 || from >= targets.length || to < 0 || to >= targets.length) return [...targets];
  const next = [...targets];
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved!);
  return next;
}
