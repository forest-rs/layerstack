// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Authoring composition, with no DOM: the edits that add and remove a
// prim's arcs and variant sets, the USDA each authors, which layer
// authors each arc the stage composes (from the layers' specs,
// `layerSpecs.ts`) and so whether it can be removed where edits go, where
// an edit target writes a prim, and which names USD takes. The inspector's
// Composition and Variants sections (`../app/CompositionSection.tsx`,
// `../app/VariantsSection.tsx`) and the store's actions read these.
// AOUSD Core §10 (composition arcs: LIVRPS), §12.4 (list ops), §7.3.6
// (variant sets), §7.6.2 (prim spec fields).
import type { ArcKind, CompositionArc, Edit, EditTarget, Layer, ListPosition } from "@forest-rs/layerstack";
import type { ArcField, AuthoredArc, LayerSpecs, ListOpKind } from "./layerSpecs.ts";

/** An arc the inspector adds: a reference, a payload, an inherit or a specialize. */
export type NewArcKind = "reference" | "payload" | "inherit" | "specialize";

/** An arc to add to a prim, where edits go. */
export interface NewArc {
  readonly kind: NewArcKind;
  /** The asset path to author (`./assets/robot.usda`); `null` for an internal reference or payload, and for class arcs. */
  readonly asset: string | null;
  /**
   * The prim path: in the asset for an external reference or payload
   * (`null` for its default prim), a stage path for an internal one and
   * for a class arc.
   */
  readonly primPath: string | null;
  /** Where in the list op it goes; `backOfPrepend` is OpenUSD's default. */
  readonly position: ListPosition;
  /** The layer offset across a reference or payload: stage time = `offset + scale × layer time`. */
  readonly offset: number;
  readonly scale: number;
}

/** The spec field a kind of arc is authored in. */
export function fieldOf(kind: NewArcKind): ArcField {
  return kind === "reference" ? "references" : kind === "payload" ? "payload" : kind === "inherit" ? "inherits" : "specializes";
}

/** The kind of arc a field authors. */
export function kindOf(field: ArcField): NewArcKind {
  return field === "references" ? "reference" : field === "payload" ? "payload" : field === "inherits" ? "inherit" : "specialize";
}

/** The field a composed arc kind is authored in; `null` for arcs no field authors so (`local`, `variants`, `relocates`). */
export function fieldOfArc(arc: ArcKind): ArcField | null {
  switch (arc) {
    case "references":
      return "references";
    case "payloads":
      return "payload";
    case "inherits":
      return "inherits";
    case "specializes":
      return "specializes";
    default:
      return null;
  }
}

/** A composed arc kind in words, singular: `reference`, `variant`, `local opinions`. */
export function arcNoun(arc: ArcKind): string {
  switch (arc) {
    case "local":
      return "local opinions";
    case "inherits":
      return "inherit";
    case "variants":
      return "variant";
    case "relocates":
      return "relocate";
    case "references":
      return "reference";
    case "payloads":
      return "payload";
    case "specializes":
      return "specialize";
    default:
      return arc;
  }
}

/** The list op keyword `position` authors: `prepend` or `append`. */
export function positionOp(position: ListPosition): "prepend" | "append" {
  return position.endsWith("Prepend") ? "prepend" : "append";
}

/** A number as USDA writes it. */
function number(value: number): string {
  return Number.isInteger(value) ? String(value) : String(Number(value.toPrecision(12)));
}

/** An arc item as USDA writes it: `@./robot.usda@</Robot> (offset = 10; scale = 2)`, `</Shape>`. */
export function arcItemText(asset: string | null, primPath: string | null, offset = 0, scale = 1): string {
  const parts: string[] = [];
  if (offset !== 0) parts.push(`offset = ${number(offset)}`);
  if (scale !== 1) parts.push(`scale = ${number(scale)}`);
  const at = asset === null ? "" : `@${asset}@`;
  const path = primPath === null ? "" : `<${primPath}>`;
  return `${at}${path}${parts.length === 0 ? "" : ` (${parts.join("; ")})`}`;
}

/** An authored arc's item as USDA writes it. */
export function authoredItemText(arc: AuthoredArc): string {
  return arcItemText(arc.asset, arc.primPath, arc.offset, arc.scale);
}

/** One arc list op entry as USDA writes it: `prepend references = @./robot.usda@`. */
export function arcUsda(op: ListOpKind, field: ArcField, item: string): string {
  return `${op === "explicit" ? "" : `${op} `}${field} = ${item}`;
}

/** The USDA line adding `arc` authors (`prepend references = @./robot.usda@</Robot>`). */
export function newArcUsda(arc: NewArc): string {
  const external = arc.kind === "reference" || arc.kind === "payload";
  const item = external ? arcItemText(arc.asset, arc.primPath, arc.offset, arc.scale) : arcItemText(null, arc.primPath);
  return arcUsda(positionOp(arc.position), fieldOf(arc.kind), item);
}

/** The edit that adds `arc` to the prim at `prim` (`UsdReferences::AddReference` and its siblings). */
export function addArcEdit(prim: string, arc: NewArc): Edit {
  if (arc.kind === "inherit" || arc.kind === "specialize") {
    if (arc.primPath === null) throw new Error(`an ${arc.kind} names the prim it ${arc.kind}s`);
    return { type: arc.kind === "inherit" ? "addInherit" : "addSpecialize", path: prim, primPath: arc.primPath, position: arc.position };
  }
  const offset = arc.offset !== 0 || arc.scale !== 1 ? { layerOffset: { offset: arc.offset, scale: arc.scale } } : {};
  return {
    type: arc.kind === "reference" ? "addReference" : "addPayload",
    path: prim,
    ...(arc.asset === null ? {} : { asset: arc.asset }),
    ...(arc.primPath === null ? {} : { primPath: arc.primPath }),
    position: arc.position,
    ...offset,
  };
}

/** The action's label, as Changes and History list it: `add reference @./robot.usda@ to /World/Robot`. */
export function newArcLabel(prim: string, arc: NewArc): string {
  const external = arc.kind === "reference" || arc.kind === "payload";
  return `add ${arc.kind} ${external ? arcItemText(arc.asset, arc.primPath) : arcItemText(null, arc.primPath)} to ${prim}`;
}

/** The label of removing `arc` from `prim`. */
export function removeArcLabel(prim: string, arc: AuthoredArc): string {
  return `remove ${kindOf(arc.field)} ${arcItemText(arc.asset, arc.primPath)} from ${prim}`;
}

/**
 * The stage path an edit through `target` writes at `specPath` of its
 * layer: the inverse of {@link EditTarget.mapPath} for a path beneath the
 * site it maps its prim to (`/Robot/Head` through `/World/Robot`'s
 * reference is `/World/Robot/Head`); any other path is itself, as a root
 * prim path is authored as given.
 */
export function stagePathOf(target: Pick<EditTarget, "prim" | "site"> | null, specPath: string): string {
  if (target === null || target.prim === null || target.site === null) return specPath;
  const site = target.site;
  if (specPath === site) return target.prim;
  if (specPath.startsWith(`${site}/`)) return `${target.prim}${specPath.slice(site.length)}`;
  return specPath;
}

/**
 * The edit that removes `arc`, which the target's layer authors on the
 * prim at `prim` (`UsdReferences::RemoveReference` and its siblings): its
 * item leaves the layer's lists and is added to the deleted items, which
 * removes it from weaker layers too. An internal path is given as the
 * stage path the target writes there ({@link stagePathOf}).
 */
export function removeArcEdit(prim: string, arc: AuthoredArc, target: Pick<EditTarget, "prim" | "site"> | null): Edit {
  const primPath = arc.primPath === null ? null : arc.asset === null ? stagePathOf(target, arc.primPath) : arc.primPath;
  if (arc.field === "inherits" || arc.field === "specializes") {
    return { type: arc.field === "inherits" ? "removeInherit" : "removeSpecialize", path: prim, primPath: primPath ?? "" };
  }
  return {
    type: arc.field === "references" ? "removeReference" : "removePayload",
    path: prim,
    ...(arc.asset === null ? {} : { asset: arc.asset }),
    ...(primPath === null ? {} : { primPath }),
  };
}

// Asset paths. The stage names layers by identifiers relative to the
// source's root (`assets/robot.usda`); an arc authors an asset path
// relative to the layer that authors it (`./assets/robot.usda`).

/** Whether `path` is a URL or an absolute path: never made relative. */
function absolute(path: string): boolean {
  return path.startsWith("/") || /^[A-Za-z][A-Za-z0-9+.-]*:/.test(path);
}

/** The directory segments of an identifier: `assets` for `assets/robot.usda`. */
function directory(identifier: string): string[] {
  const parts = identifier.split("/");
  parts.pop();
  return parts.filter((part) => part !== "" && part !== ".");
}

/** `parts` with `.` dropped and `..` taken back where it can be. */
function normalize(parts: readonly string[]): string[] {
  const out: string[] = [];
  for (const part of parts) {
    if (part === "" || part === ".") continue;
    if (part === ".." && out.length > 0 && out.at(-1) !== "..") out.pop();
    else out.push(part);
  }
  return out;
}

/**
 * The identifier an asset path authored in the layer `anchor` names:
 * `./assets/robot.usda` in `scene.usda` is `assets/robot.usda`. A search
 * path (`robot.usda`, no `./`) is taken as beside the anchor, where
 * OpenUSD's resolver looks first.
 */
export function anchorAsset(anchor: string | null, asset: string): string {
  if (absolute(asset) || anchor === null || anchor.includes("[")) return asset;
  return normalize([...directory(anchor), ...asset.split("/")]).join("/");
}

/**
 * The asset path that names the layer `to` from the layer `from`, both
 * identifiers in the source: `./assets/robot.usda` from `scene.usda`,
 * `../library.usda` from `assets/robot.usda`. An absolute identifier, or
 * one in a package, is authored as it is.
 */
export function relativeAsset(from: string | null, to: string): string {
  if (absolute(to) || to.includes("[") || from === null || absolute(from)) return to;
  const base = directory(from);
  const target = normalize(to.split("/"));
  let common = 0;
  while (common < base.length && common < target.length - 1 && base[common] === target[common]) common++;
  const up = base.slice(common).map(() => "..");
  const rest = target.slice(common);
  return up.length === 0 ? `./${rest.join("/")}` : [...up, ...rest].join("/");
}

// Which layer authors a composed arc.

/** What {@link findAuthoring} reads of a composed arc. */
export type ArcRecord = Pick<CompositionArc, "arc" | "site" | "layerStack" | "layers" | "implied" | "parent">;

/** Where a composed arc is authored: the layer, the spec there, and the item. */
export interface ArcSource {
  readonly layer: Layer;
  /** The spec path in that layer (`/World/Robot`, `/World/Robot{finish=chrome}`, `/Robot`). */
  readonly specPath: string;
  readonly item: AuthoredArc;
  /** The arc it is authored beneath (the prim's own opinions, a variant, a reference), by index. */
  readonly parent: number;
}

/**
 * Whether `item`, authored in the layer `layer` beneath `parent`, is the
 * composed arc `arc`: the same field, the asset it names anchored at the
 * layer is the arc's layer stack (an internal one stays in the parent's),
 * and the prim it names is the arc's site (or, for a reference to a
 * default prim, any).
 */
export function itemComposes(item: AuthoredArc, layer: Layer, arc: ArcRecord, parent: ArcRecord): boolean {
  if (item.op === "delete" || item.op === "reorder") return false;
  if (fieldOfArc(arc.arc) !== item.field) return false;
  if (item.field === "references" || item.field === "payload") {
    if (item.asset === null || item.asset === "") {
      if (arc.layerStack !== parent.layerStack) return false;
    } else {
      const named = anchorAsset(layer.identifier, item.asset);
      const identifier = arc.layerStack.identifier ?? "";
      if (named !== identifier && !identifier.endsWith(`/${named}`) && !identifier.endsWith(`[${named}]`)) return false;
    }
    return item.primPath === null || item.primPath === arc.site;
  }
  return item.primPath === arc.site;
}

/**
 * The layer that authors the composed arc `arcs[index]`, strongest first
 * among the layers of the arc it is authored beneath, read through
 * `specsOf` (`null` for a layer that could not be read); `null` for the
 * prim's own opinions, a variant, a relocate, an implied arc (authored on
 * the class arc it is implied from), or an arc no readable layer authors.
 */
export function findAuthoring(arcs: readonly ArcRecord[], index: number, specsOf: (layer: Layer) => LayerSpecs | null): ArcSource | null {
  const arc = arcs[index];
  if (arc === undefined || arc.parent === null || arc.implied || fieldOfArc(arc.arc) === null) return null;
  const parent = arcs[arc.parent];
  if (parent === undefined) return null;
  for (const layer of parent.layers) {
    const spec = specsOf(layer)?.specs.get(parent.site);
    const item = spec?.arcs.find((each) => itemComposes(each, layer, arc, parent));
    if (item !== undefined) return { layer, specPath: parent.site, item, parent: arc.parent };
  }
  return null;
}

/**
 * The spec path `target` writes `path` at (`EditTarget.mapPath`); `null`
 * when it does not map it, or when the target is stale (the arc it went
 * through was removed, its prim renamed), which maps nothing.
 */
export function mapThrough(target: Pick<EditTarget, "mapPath">, path: string): string | null {
  try {
    return target.mapPath(path);
  } catch {
    return null;
  }
}

/** What {@link authoredHere} reads of where edits go. */
export type TargetLike = Pick<EditTarget, "layer" | "mapPath">;

/** Where edits of `prim` go through `target`: its layer and the spec path; `null` when it does not map the prim. */
export function targetSpec(target: Layer | TargetLike, prim: string): { readonly layer: Layer; readonly specPath: string } | null {
  if (!("mapPath" in target)) return { layer: target, specPath: prim };
  const specPath = mapThrough(target, prim);
  return specPath === null ? null : { layer: target.layer, specPath };
}

/** Whether edits of `prim` through `target` write the spec that authors `source`: whether it can be removed there. */
export function authoredHere(source: Pick<ArcSource, "layer" | "specPath">, target: Layer | TargetLike, prim: string): boolean {
  const spec = targetSpec(target, prim);
  return spec !== null && spec.layer === source.layer && spec.specPath === source.specPath;
}

/**
 * The arcs the spec edits of `prim` go to authors that none of `arcs`
 * composes: a reference to an asset that is missing or not fetched yet,
 * one in a variant not selected, one a stronger layer deletes. Deletes
 * are left out (they compose nothing by design).
 */
export function uncomposedHere(
  arcs: readonly ArcRecord[],
  target: Layer | TargetLike,
  prim: string,
  specsOf: (layer: Layer) => LayerSpecs | null,
): AuthoredArc[] {
  const spec = targetSpec(target, prim);
  if (spec === null) return [];
  const authored = specsOf(spec.layer)?.specs.get(spec.specPath);
  if (authored === undefined) return [];
  // By what the item says and where, not by object: each read of a layer is its own.
  const key = (item: AuthoredArc) => `${item.line}\n${item.field}\n${item.asset}\n${item.primPath}`;
  const composed = new Set<string>();
  arcs.forEach((_, index) => {
    const source = findAuthoring(arcs, index, specsOf);
    if (source !== null && source.layer === spec.layer && source.specPath === spec.specPath) composed.add(key(source.item));
  });
  return authored.arcs.filter((item) => item.op !== "delete" && item.op !== "reorder" && !composed.has(key(item)));
}

/** The arcs the spec edits of `prim` go to deletes (`delete references = ...`): composing nothing, by design. */
export function deletedHere(target: Layer | TargetLike, prim: string, specsOf: (layer: Layer) => LayerSpecs | null): AuthoredArc[] {
  const spec = targetSpec(target, prim);
  if (spec === null) return [];
  return specsOf(spec.layer)?.specs.get(spec.specPath)?.arcs.filter((item) => item.op === "delete") ?? [];
}

/**
 * The layer and spec that hold the variant set `set` of a prim, among
 * the specs its arcs read (its own layers, a reference's asset), strongest
 * first, and the arc that reads it (its edit target edits there); `null`
 * when no readable layer holds the set's spec.
 */
export function variantSetSource(
  arcs: readonly ArcRecord[],
  set: string,
  specsOf: (layer: Layer) => LayerSpecs | null,
): { readonly layer: Layer; readonly specPath: string; readonly arc: number } | null {
  for (const [index, arc] of arcs.entries()) {
    if (arc.arc === "variants" || arc.arc === "relocates") continue;
    for (const layer of arc.layers) {
      if (specsOf(layer)?.specs.get(arc.site)?.variantSets.has(set) === true) return { layer, specPath: arc.site, arc: index };
    }
  }
  return null;
}

// Edit targets in words.

/** A layer as the editor names it. */
function layerName(layer: Layer): string {
  return layer.identifier ?? "session layer";
}

/** The variant set and variant a variant site ends in: `finish` and `chrome` for `/World/Robot{finish=chrome}`; `null` otherwise. */
export function variantOfSite(site: string | null): { readonly set: string; readonly variant: string } | null {
  const match = site === null ? null : /\{([^{}=]+)=([^{}]*)\}$/.exec(site);
  return match === null ? null : { set: match[1]!, variant: match[2]! };
}

/** What {@link targetGone} reads of the stage: a prim's variant sets. */
export interface VariantSetsReader {
  prim(path: string): { variantSets(): readonly { readonly name: string; readonly variants: readonly string[] }[] } | null;
}

/**
 * Why `target` writes where the stage has nothing any more, or `null`
 * while what it goes through is there: its prim is gone (deleted, renamed,
 * or an undo took it), the variant it writes inside is no longer one of
 * the prim's (an undo of the edit that added it, the set removed), or no
 * arc maps it (`target.stale`). A variant that is there and not selected
 * is not gone: editing inside it is what a variant target is for. Edits
 * through a target that is gone would author opinions nothing composes,
 * so the editor stops sending them there.
 */
export function targetGone(stage: VariantSetsReader, target: Layer | Pick<EditTarget, "kind" | "prim" | "site" | "mapPath"> | undefined): string | null {
  if (target === undefined || !("kind" in target) || target.kind === "layer" || target.prim === null) return null;
  const prim = stage.prim(target.prim);
  if (prim === null) return `${target.prim} is no longer there`;
  const variant = target.kind === "variant" ? variantOfSite(target.site) : null;
  if (variant !== null) {
    const set = prim.variantSets().find((each) => each.name === variant.set);
    return set?.variants.includes(variant.variant) === true ? null : `${target.prim} no longer has the variant ${variant.set}=${variant.variant}`;
  }
  try {
    target.mapPath(target.prim);
    return null;
  } catch {
    return `${target.prim} no longer has the arc they went through`;
  }
}

/**
 * Where a target writes, in words, naming its layer and how it is
 * reached: `scene.usda`; `scene.usda, inside finish=chrome of
 * /World/Robot`; `assets/robot.usda at /Robot, through /World/Robot's
 * reference`.
 */
export function describeTarget(target: Layer | Pick<EditTarget, "kind" | "layer" | "prim" | "site" | "arc">): string {
  if (!("kind" in target)) return layerName(target);
  const layer = layerName(target.layer);
  const route = targetRoute(target);
  if (route === null) return layer;
  return target.kind === "variant" ? `${layer}, ${route}` : `${layer} at ${target.site}, ${route}`;
}

/**
 * How a target reaches its layer, in words: `inside finish=chrome of
 * /World/Robot`, `through /World/Robot's reference`; `null` for a layer
 * of the stage's own stack.
 */
export function targetRoute(target: Pick<EditTarget, "kind" | "prim" | "site" | "arc">): string | null {
  if (target.kind === "layer" || target.prim === null) return null;
  const variant = variantOfSite(target.site);
  if (target.kind === "variant" && variant !== null) return `inside ${variant.set}=${variant.variant} of ${target.prim}`;
  return `through ${target.arc === null ? "an arc" : `${target.prim}'s ${arcNoun(target.arc)}`}`;
}

/** Where the selected prim's edits land through a target: the mapping, or why there is none. */
export interface Mapping {
  /** `/World/Robot → /Robot in assets/robot.usda`, or why the target does not map the prim. */
  readonly text: string;
  /** The spec path; `null` when the target does not map the prim. */
  readonly specPath: string | null;
}

/** Where edits of `prim` go through `target` ({@link Mapping}). */
export function mapping(target: Layer | Pick<EditTarget, "kind" | "layer" | "prim" | "site" | "arc" | "mapPath" | "mapsOutsidePrim">, prim: string): Mapping {
  if (!("kind" in target)) return { text: `${prim} in ${layerName(target)}`, specPath: prim };
  let specPath: string | null;
  try {
    specPath = target.mapPath(prim);
  } catch {
    return { text: `${describeTarget(target)}, which no longer composes anything: choose where edits go again`, specPath: null };
  }
  if (specPath !== null) return { text: `${prim} → ${specPath} in ${layerName(target.layer)}`, specPath };
  const reach = target.mapsOutsidePrim ? "" : `: across a ${target.arc === null ? "reference" : arcNoun(target.arc)}, it maps only ${target.prim} and what is beneath it`;
  return { text: `${prim} is not written through ${describeTarget(target)}${reach}`, specPath: null };
}

/** A namespace action the prim tree and the inspector offer. */
export type NamespaceAction = "rename" | "delete" | "define";

/**
 * Why `action` on the prim at `path` cannot go through `target`, said
 * before it is tried; `null` when it can be tried. A rename or delete
 * edits every opinion of the stage's own layer stack (`Stage.move`,
 * `Stage.delete`, as `UsdNamespaceEditor`), so its target must be one of
 * its layers; a prim defined through an arc or variant must be one the
 * target maps.
 */
export function namespaceRefusal(target: Layer | Pick<EditTarget, "kind" | "layer" | "prim" | "site" | "arc" | "mapPath"> | undefined, action: NamespaceAction, path: string): string | null {
  if (target === undefined || !("kind" in target) || target.kind === "layer") return null;
  const where = describeTarget(target);
  if (action === "rename" || action === "delete") {
    return `Renaming and deleting edit the opinions of the stage's own layer stack, and edits go to ${where}: make a layer of the stack where edits go first.`;
  }
  if (mapThrough(target, path) === null) return `Edits go to ${where}, which does not map ${path}: define it where edits go to a layer of the stack, or inside ${target.prim}.`;
  return null;
}

// Names.

/** A variant set name (an identifier), or why not. */
export function variantSetNameProblem(name: string, taken: readonly string[] = []): string | null {
  if (name === "") return "A variant set needs a name.";
  if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) return "A variant set name is an identifier: a letter or _ then letters, digits or _.";
  if (taken.includes(name)) return `The prim has a variant set ${name} already.`;
  return null;
}

/** A variant name (letters, digits, `_`, `|` and `-`, after an optional `.`), or why not. */
export function variantNameProblem(name: string, taken: readonly string[] = []): string | null {
  if (name === "") return "A variant needs a name.";
  if (!/^\.?[A-Za-z0-9_|-]+$/.test(name)) return "A variant name is letters, digits, _, | and -, after an optional dot.";
  if (taken.includes(name)) return `The set has a variant ${name} already.`;
  return null;
}

/** An absolute prim path (`/Robot`, `/Library/Shapes/Rock`), or why not. */
export function primPathProblem(path: string): string | null {
  if (path === "") return "Type a prim path, such as /Robot.";
  if (!path.startsWith("/")) return "A prim path starts at the root: /Robot.";
  if (!/^(\/[A-Za-z_][A-Za-z0-9_]*)+$/.test(path)) return "A prim path is names (a letter or _ then letters, digits or _) after slashes, with no property or variant.";
  return null;
}

/** A layer offset's scale (finite and positive) and offset (finite), or why not. */
export function layerOffsetProblem(offset: number, scale: number): string | null {
  if (!Number.isFinite(offset)) return "The offset is a number of time codes.";
  if (!Number.isFinite(scale) || scale <= 0) return "The scale is a positive number.";
  return null;
}
