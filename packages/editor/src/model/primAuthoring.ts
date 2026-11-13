// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the prim-authoring controls decide with, with no DOM: the kinds a
// prim may be, the concrete types grouped as the Add and Retype dialogs
// list them, the API schemas that can apply to a prim and their instance
// names, which applied schemas a layer authors (and so can remove), a
// free sibling name for a duplicate, a reparent's destination and why it
// cannot be made, a sibling order with one child moved, and where a
// prim's opinions are when the edit target has none to copy.
import type { ArcKind, EditTarget, Layer, Prim, SchemaInfo, Stage } from "@forest-rs/layerstack";
import { releaseValue } from "./values.ts";

/** A prim name as USD takes it: an identifier. */
export const PRIM_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Why `name` cannot name a new child among `taken`; `null` when it can. */
export function primNameProblem(name: string, parent: string, taken: ReadonlySet<string>): string | null {
  if (!PRIM_NAME.test(name)) return "A prim name is a letter or _ then letters, digits or _.";
  if (taken.has(name)) return `${parent} already has a child ${name}.`;
  return null;
}

/** A prim spec's specifier: defines it, overrides it, or defines it as a class (AOUSD Core §7.6). */
export type PrimSpecifier = "def" | "over" | "class";

/** What each specifier means, as the State section and the Add dialog say it. */
export const SPECIFIER_MEANINGS: Readonly<Record<PrimSpecifier, string>> = {
  def: "Defines the prim: it is on the stage, and drawn when it is geometry.",
  over: "Only overrides: opinions for a prim another layer or arc defines; alone, it defines nothing.",
  class: "Defines an abstract prim that others inherit or specialize; it is not drawn.",
};

/** The properties an applied API schema brought to its prim (`EditorStore.revealed`). */
export interface SchemaReveal {
  /** The prim's path. */
  readonly prim: string;
  /** The schema as applied (`CollectionAPI:parts`). */
  readonly schema: string;
  /** The property names it added, which the prim did not have before. */
  readonly names: readonly string[];
}

/** A `kind` the model hierarchy knows, and what it means. */
export interface KindInfo {
  readonly kind: string;
  readonly meaning: string;
}

/**
 * The kinds OpenUSD's `KindRegistry` registers, as the model hierarchy
 * places them (AOUSD Core §13.4, model hierarchy). The stage does not
 * expose a kind registry, so these are the standard ones; a kind a site
 * registers itself is offered when a prim already has it.
 */
export const STANDARD_KINDS: readonly KindInfo[] = [
  { kind: "component", meaning: "A leaf model: a published asset whose descendants are not models (a prop, a character)." },
  { kind: "assembly", meaning: "A group that is a published asset made of other models (a set, an environment)." },
  { kind: "group", meaning: "A model that groups models: its children may be models, and its parent must be a group too." },
  { kind: "subcomponent", meaning: "An important part inside a component, named for reference; not a model." },
  { kind: "model", meaning: "The base of the model kinds: rarely authored itself; component, group and assembly are models." },
];

/** The kinds to offer for a prim of kind `current` (`null` when it has none): the standard ones, and its own when it is not one of them. */
export function kindChoices(current: string | null): KindInfo[] {
  const choices = [...STANDARD_KINDS];
  if (current !== null && current !== "" && !choices.some((each) => each.kind === current)) {
    choices.push({ kind: current, meaning: "A kind this stage's site registers; the editor does not know its place in the model hierarchy." });
  }
  return choices;
}

/** The concrete typed schemas, grouped by what they are, by their bases: what a prim's `typeName` may name. */
export function typeGroups(schemas: readonly SchemaInfo[]): [string, SchemaInfo[]][] {
  const groups = new Map<string, SchemaInfo[]>();
  for (const schema of schemas) {
    if (schema.kind !== "concreteTyped") continue;
    const group =
      schema.bases.includes("Gprim") || schema.name === "PointInstancer"
        ? "Geometry"
        : schema.bases.some((base) => base.endsWith("LightBase")) || schema.name.endsWith("Light")
          ? "Lights"
          : schema.name === "Camera"
            ? "Cameras"
            : ["Material", "Shader", "NodeGraph"].includes(schema.name)
              ? "Shading"
              : schema.name.startsWith("Physics")
                ? "Physics"
                : schema.name === "Xform" || schema.name === "Scope" || schema.bases.includes("Xformable")
                  ? "Transforms and groups"
                  : "Other";
    groups.set(group, [...(groups.get(group) ?? []), schema]);
  }
  const order = ["Transforms and groups", "Geometry", "Lights", "Cameras", "Shading", "Physics", "Other"];
  return order.filter((group) => groups.has(group)).map((group) => [group, groups.get(group)!]);
}

/**
 * What the common applied API schemas are for, in a line. The registry
 * the stage exposes ({@link Stage.schemas}) carries no documentation, so
 * the editor keeps these, as it keeps its shader definitions.
 */
export const API_SCHEMA_NOTES: Readonly<Record<string, string>> = {
  MaterialBindingAPI: "Binds materials to the prim and its descendants (material:binding).",
  CollectionAPI: "A named collection of prims and properties, by include and exclude rules or an expression.",
  CoordSysAPI: "Binds a named coordinate system (a transform) for shaders to read.",
  GeomModelAPI: "A model's draw mode (origin, bounds, cards) and its card textures.",
  MotionAPI: "Motion blur controls: blur scale, velocity and nonlinear sample counts.",
  VisibilityAPI: "Visibility per purpose: guide, proxy and render visibility.",
  SkelBindingAPI: "Binds the prim to a skeleton and animation, with its joint influences.",
  LightAPI: "Makes the prim a light: intensity, exposure, colour, and its light-link collections.",
  MeshLightAPI: "Makes a mesh emit light as a light.",
  VolumeLightAPI: "Makes a volume emit light as a light.",
  LightListAPI: "Caches the lights beneath the prim for discovery.",
  ListAPI: "The older form of LightListAPI.",
  ShapingAPI: "Shapes a light's emission: focus, cone angle and softness, an IES profile.",
  ShadowAPI: "A light's shadows: enabled, colour, distance and falloff.",
  LODRootAPI: "Marks the root of a level-of-detail hierarchy.",
  LODOverrideAPI: "Overrides the level of detail chosen beneath it.",
  SemanticsLabelsAPI: "Semantic labels of a taxonomy, one instance per taxonomy.",
  ColorSpaceAPI: "The colour space values beneath the prim are authored in.",
  AssetPreviewsAPI: "Thumbnails and previews of an asset.",
  NodeGraphNodeAPI: "Where a node is placed in a node graph editor (ui:nodegraph:node).",
  PhysicsRigidBodyAPI: "Makes the prim a rigid body in a physics simulation.",
  PhysicsCollisionAPI: "Makes the prim's geometry collide in a physics simulation.",
  PhysicsMeshCollisionAPI: "How a mesh's collision shape is approximated.",
  PhysicsMassAPI: "A body's mass, density, centre of mass and inertia.",
  PhysicsMaterialAPI: "Makes a material a physics material: friction, restitution, density.",
  PhysicsArticulationRootAPI: "Marks the root of an articulation.",
  PhysicsDriveAPI: "A joint's drive, one instance per degree of freedom.",
  PhysicsLimitAPI: "A joint's limit, one instance per degree of freedom.",
  PhysicsFilteredPairsAPI: "Pairs of bodies that do not collide with each other.",
};

/** The group an applied API schema is listed under, by its name. */
export function apiSchemaGroup(name: string): string {
  if (name.startsWith("Physics")) return "Physics";
  if (/Light|Shadow|Shaping/.test(name)) return "Lights";
  if (["MaterialBindingAPI", "CoordSysAPI", "NodeGraphNodeAPI", "NodeDefAPI", "MaterialXConfigAPI", "ColorSpaceAPI", "ColorSpaceDefinitionAPI"].includes(name) || name.startsWith("Ri")) return "Shading";
  if (name === "CollectionAPI" || name === "SemanticsLabelsAPI") return "Collections and labels";
  if (["GeomModelAPI", "MotionAPI", "VisibilityAPI", "SkelBindingAPI", "LODRootAPI", "LODOverrideAPI"].includes(name) || name.startsWith("ParticleField")) return "Geometry";
  return "Other";
}

const API_GROUP_ORDER = ["Shading", "Geometry", "Lights", "Collections and labels", "Physics", "Other"];

/** What a prim is, for {@link applicableSchemas}: the schemas it is (`Prim.isA`) and has (`Prim.hasAPI`). */
export interface SchemaHolder {
  isA(schema: string): boolean;
  hasAPI(schema: string): boolean;
}

/**
 * The applied API schemas that can apply to `prim`, grouped: those whose
 * `canOnlyApplyTo` it is or has (any prim when empty), less the
 * single-apply ones it has already (`applied`, `Prim.appliedSchemas`).
 * A multiple-apply schema is always offered: each instance is another.
 */
export function applicableSchemas(schemas: readonly SchemaInfo[], prim: SchemaHolder, applied: readonly string[]): [string, SchemaInfo[]][] {
  const has = new Set(applied);
  const groups = new Map<string, SchemaInfo[]>();
  for (const schema of schemas) {
    if (schema.kind !== "singleApplyApi" && schema.kind !== "multipleApplyApi") continue;
    if (schema.kind === "singleApplyApi" && has.has(schema.name)) continue;
    if (schema.canOnlyApplyTo.length > 0 && !schema.canOnlyApplyTo.some((to) => prim.isA(to) || prim.hasAPI(to))) continue;
    const group = apiSchemaGroup(schema.name);
    groups.set(group, [...(groups.get(group) ?? []), schema]);
  }
  return API_GROUP_ORDER.filter((group) => groups.has(group)).map((group) => [group, groups.get(group)!]);
}

/** Why `name` cannot be an instance of the multiple-apply `schema` on a prim with `applied`; `null` when it can. */
export function instanceNameProblem(schema: SchemaInfo, name: string, applied: readonly string[]): string | null {
  if (name === "") return `${schema.name} is applied once per instance name: name this one.`;
  if (!PRIM_NAME.test(name)) return "An instance name is a letter or _ then letters, digits or _.";
  if (schema.allowedInstanceNames.length > 0 && !schema.allowedInstanceNames.includes(name)) {
    return `${schema.name} takes only the instance names ${schema.allowedInstanceNames.join(", ")}.`;
  }
  if (applied.includes(`${schema.name}:${name}`)) return `The prim has ${schema.name}:${name} already.`;
  return null;
}

/** An applied schema by its applied name (`CollectionAPI:parts`): its schema and instance. */
export interface AppliedSchema {
  /** As `Prim.appliedSchemas` lists it. */
  readonly applied: string;
  readonly schema: string;
  /** A multiple-apply instance's name; `null` for a single-apply schema. */
  readonly instance: string | null;
  /**
   * Whether layers author it in `apiSchemas`, so a `removeAPI` edit takes
   * it off; `false` for one its type builds in or auto-applies, which no
   * layer can remove.
   */
  readonly authored: boolean;
}

/**
 * `applied` (`Prim.appliedSchemas`) split into schema and instance, each
 * marked authored when the composed `apiSchemas` lists it (`authored`),
 * else built in by the prim's type.
 */
export function appliedSchemaRows(applied: readonly string[], authored: readonly string[]): AppliedSchema[] {
  const listed = new Set(authored);
  return applied.map((name) => {
    const colon = name.indexOf(":");
    return {
      applied: name,
      schema: colon < 0 ? name : name.slice(0, colon),
      instance: colon < 0 ? null : name.slice(colon + 1),
      authored: listed.has(name),
    };
  });
}

/** The composed `apiSchemas` of `prim` as names; empty when nothing authors it. Reads and releases the value. */
export function authoredApiSchemas(prim: Prim): string[] {
  const value = prim.metadata("apiSchemas");
  if (value === null) return [];
  try {
    if (value.kind === "array") return [...(value.array.view() as readonly string[])].map(String);
    if (value.kind === "list") return value.value.flatMap((item) => (item?.kind === "scalar" ? [String(item.value)] : []));
    return [];
  } finally {
    if (value.kind === "array") value.array.release();
  }
}

/** The last element of a prim path (`Rock` of `/World/Rock`). */
export function nameOf(path: string): string {
  return path.slice(path.lastIndexOf("/") + 1);
}

/** The parent of a prim path (`/World` of `/World/Rock`, `/` of `/World`). */
export function parentOf(path: string): string {
  return path.slice(0, path.lastIndexOf("/")) || "/";
}

/** The path of `name` under `parent`. */
export function childPath(parent: string, name: string): string {
  return parent === "/" ? `/${name}` : `${parent}/${name}`;
}

/** A name for a copy of `name` not in `taken`: its digits counted on (`Rock` to `Rock1`, `Rock1` to `Rock2`). */
export function freeSiblingName(name: string, taken: ReadonlySet<string>): string {
  const match = /^(.*?)(\d+)$/.exec(name);
  const base = match === null ? name : match[1]!;
  for (let index = match === null ? 1 : Number(match[2]) + 1; ; index++) {
    const candidate = `${base}${index}`;
    if (!taken.has(candidate)) return candidate;
  }
}

/** Why the prim at `path` cannot move under `parent`, whose children are named `taken`; `null` when it can. */
export function moveProblem(path: string, parent: string, taken: ReadonlySet<string>): string | null {
  if (path === "/") return "The pseudo-root does not move.";
  if (parent === path || parent.startsWith(`${path}/`)) return `${nameOf(path)} cannot move beneath itself.`;
  if (parentOf(path) === parent) return `${nameOf(path)} is a child of ${parent} already.`;
  if (taken.has(nameOf(path))) return `${parent} already has a child named ${nameOf(path)}: rename one first.`;
  return null;
}

/**
 * The order `names` (a prim's children, in order) take with `name` moved
 * one place earlier (`-1`) or later (`1`): every name, as
 * `setChildrenReorder` authors it. `null` when it is at that end already.
 */
export function siblingOrder(names: readonly string[], name: string, direction: -1 | 1): string[] | null {
  const at = names.indexOf(name);
  const to = at + direction;
  if (at < 0 || to < 0 || to >= names.length) return null;
  const order = [...names];
  [order[at], order[to]] = [order[to]!, order[at]!];
  return order;
}

/** What dropping a dragged prim on the tree does: move it under another prim, or reorder it among its siblings. */
export type TreeDrop = { readonly kind: "move"; readonly parent: string } | { readonly kind: "reorder"; readonly parent: string; readonly order: readonly string[] };

/**
 * What dropping the prim at `path` `position` (`on`, `before`, `after`)
 * the row at `target` does: on a row, a move beneath it; between two
 * siblings of its own, the order its parent's children take
 * (`siblings`, the names of `target`'s parent's children in order).
 * `null` when the drop does nothing (on itself, its subtree or its own
 * parent; between rows of another parent, which would be a move and a
 * reorder at once).
 */
export function treeDrop(path: string, target: string, position: "on" | "before" | "after", siblings: readonly string[]): TreeDrop | null {
  if (path === "/" || target === path || target.startsWith(`${path}/`)) return null;
  if (position === "on") return parentOf(path) === target ? null : { kind: "move", parent: target };
  const parent = parentOf(target);
  if (parent !== parentOf(path)) return null;
  const name = nameOf(path);
  const order = siblings.filter((each) => each !== name);
  const at = order.indexOf(nameOf(target));
  if (at < 0) return null;
  order.splice(position === "before" ? at : at + 1, 0, name);
  return order.every((each, index) => each === siblings[index]) ? null : { kind: "reorder", parent, order };
}

/**
 * The computed visibility of each row shown (`Stage.visibilities`, `null`
 * when not `Imageable`), told apart: `invisible` where the prim's own
 * opinion (or a weaker layer's) hides it, `hidden` where an ancestor
 * does, and `visible`. Read from the parents' computed visibility, which
 * the rows have, so telling them apart costs no read.
 */
export type RowVisibility = "visible" | "invisible" | "hidden";

/** Each of `paths` (a tree's rows, parents before children)'s {@link RowVisibility} from its computed visibility (`computed`, in order). */
export function rowVisibilities(paths: readonly string[], computed: readonly (string | null)[]): Map<string, RowVisibility> {
  const out = new Map<string, RowVisibility>();
  paths.forEach((path, index) => {
    const visibility = computed[index];
    if (visibility === null || visibility === undefined) return;
    if (visibility !== "invisible") out.set(path, "visible");
    else out.set(path, out.get(parentOf(path)) === "invisible" || out.get(parentOf(path)) === "hidden" ? "hidden" : "invisible");
  });
  return out;
}

/** The nearest ancestor of `path` whose own visibility hides it (`invisible` in `rows`); `null` when none is shown. */
export function hiddenBy(path: string, rows: ReadonlyMap<string, RowVisibility>): string | null {
  for (let at = parentOf(path); at !== "/"; at = parentOf(at)) if (rows.get(at) === "invisible") return at;
  return null;
}

/** A layer or an edit target through an arc, where a prim has opinions. */
export interface OpinionSite {
  readonly layer: Layer;
  /** The target that writes there; `null` when none does (a value clip's layer). */
  readonly editTarget: EditTarget | null;
  /** The arc that reaches it; `null` for the stage's own layer stack. */
  readonly arc: ArcKind | null;
}

/** How many of a prim's authored attributes {@link opinionSites} explains. */
const SITE_SAMPLES = 12;

/**
 * Where the prim at `path` has opinions, strongest first: the layers its
 * authored attributes' opinions are in (`Attribute.explain`, up to a
 * dozen attributes), else, with none authored, the root layer of each arc
 * it composes from. What says where a copy could be made when the edit
 * target has no spec of it.
 */
export function opinionSites(stage: Stage, path: string): OpinionSite[] {
  const prim = stage.prim(path);
  if (prim === null) return [];
  const sites: OpinionSite[] = [];
  const add = (site: OpinionSite) => {
    if (!sites.some((each) => each.layer === site.layer && each.editTarget === site.editTarget)) sites.push(site);
  };
  let explained = 0;
  for (const name of prim.attributeNames()) {
    if (explained >= SITE_SAMPLES) break;
    const attribute = prim.attribute(name);
    if (attribute === null || !attribute.authored) continue;
    explained++;
    const explanation = attribute.explain();
    if (explanation === null) continue;
    for (const opinion of explanation.opinions) add({ layer: opinion.layer, editTarget: opinion.editTarget, arc: opinion.arc === "local" ? null : opinion.arc });
    releaseValue(explanation.value);
  }
  if (sites.length > 0) return sites;
  for (const arc of prim.compositionArcs()) {
    let target: EditTarget | null = null;
    try {
      target = arc.editTarget();
    } catch {
      // A relocation's source, which no edit goes through.
    }
    add({ layer: arc.layerStack, editTarget: target, arc: arc.arc === "local" ? null : arc.arc });
  }
  return sites;
}

/** The sentence a copy refused for want of a spec says (`edit.rejected.no_such_spec`). */
export function noSpecToCopy(path: string, where: string, sites: readonly OpinionSite[]): string {
  const names = [...new Set(sites.map((site) => site.layer.identifier ?? "the session layer"))];
  const there = names.length === 0 ? "" : `: its opinions are in ${names.join(", ")}`;
  return `${where} has no spec of ${path} to copy${there}. A copy is made within one layer, so make one of those where edits go and duplicate it there.`;
}

/**
 * A stage path for an unrepaired path's spec (`/World/Robot{finish=chrome}.mirrors`
 * to `/World/Robot.mirrors`): the variant selections dropped, so it can
 * be selected or explained.
 */
export function specStagePath(spec: string): string {
  return spec.replace(/\{[^}]*\}/g, "");
}

/** The notice a move or delete gives about the `count` paths it left naming the old place, which the Health tab lists. */
export function unrepairedMessage(action: "move" | "delete", count: number): string {
  const what = action === "move" ? "moved" : "deleted";
  return `${count} path${count === 1 ? "" : "s"} still name${count === 1 ? "s" : ""} what was ${what}, in specs no composed prim uses: see Health.`;
}
