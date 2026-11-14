// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the prim-authoring controls decide with, with no DOM: the kinds a
// prim may be, the concrete types grouped as the Add and Retype dialogs
// list them, the API schemas that can apply to a prim and their instance
// names, which applied schemas a layer authors and what removing one where
// edits go would do, which prim fields the edit target's own spec holds
// (what a Clear there takes away), a free sibling name for a duplicate, a reparent's destination and why it
// cannot be made, a sibling order with one child moved, and where a
// prim's opinions are when the edit target has none to copy.
import type { ArcKind, CompositionArc, EditTarget, Layer, LayerSpec, Prim, PrimSpec, SchemaInfo, Stage } from "@forest-rs/layerstack";

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

/** Whether a type named `name` is listed under `filter`: its name contains it, whatever the case. */
export function typeMatches(name: string, filter: string): boolean {
  return name.toLowerCase().includes(filter.toLowerCase());
}

/**
 * The type chosen once the type list is narrowed to `filter`: `chosen`
 * while the list still shows it ("no type", `null`, is always shown),
 * else the first type listed, so what a dialog's button names is a type
 * the person can see; `chosen` when the filter matches nothing.
 */
export function typeForFilter(groups: readonly (readonly [string, readonly Pick<SchemaInfo, "name">[]])[], filter: string, chosen: string | null): string | null {
  if (chosen === null || typeMatches(chosen, filter)) return chosen;
  for (const [, schemas] of groups) {
    const first = schemas.find((schema) => typeMatches(schema.name, filter));
    if (first !== undefined) return first.name;
  }
  return chosen;
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

/**
 * An instance name of a multiple-apply schema: a property name's
 * namespaced identifier (`parts`, `render:parts`), as
 * `UsdSchemaRegistry::IsAllowedAPISchemaInstanceName` takes it. The stage
 * refuses one that is a property of the schema itself (`includes`).
 */
const INSTANCE_NAME = /^[A-Za-z_][A-Za-z0-9_]*(:[A-Za-z_][A-Za-z0-9_]*)*$/;

/** Why `name` cannot be an instance of the multiple-apply `schema` on a prim with `applied`; `null` when it can. */
export function instanceNameProblem(schema: SchemaInfo, name: string, applied: readonly string[]): string | null {
  if (name === "") return `${schema.name} is applied once per instance name: name this one.`;
  if (!INSTANCE_NAME.test(name)) return "An instance name is identifiers (a letter or _ then letters, digits or _), joined by colons when it is namespaced.";
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

/** The prim fields a Clear takes away where edits go, by their OpenUSD keys (`SdfFieldKeys`). */
export type ClearableField = "active" | "instanceable" | "kind" | "typeName" | "documentation";

/**
 * Whether `spec`, the edit target's own spec of a prim
 * (`EditorStore.targetSpec`), holds `field`: what a Clear there would
 * take away. A value only another layer authors is not the target's to
 * clear, so Clear is not offered for it. AOUSD Core §7.6.2 (prim fields).
 */
export function holdsField(spec: LayerSpec | null, field: ClearableField): boolean {
  return spec !== null && (spec.type === "prim" || spec.type === "variant") && spec.fields.includes(field);
}

/** Whether `spec` applies `name` (`CollectionAPI:parts`): its `apiSchemas` list op adds it in a list other than `delete` (and `reorder`, which adds nothing). */
export function appliesSchema(spec: PrimSpec | null, name: string): boolean {
  const op = spec?.apiSchemas ?? null;
  if (op === null) return false;
  return [op.explicit ?? [], op.prepended, op.appended, op.added].some((list) => list.includes(name));
}

/** One spec a prim's `apiSchemas` compose from: a layer of an arc's stack at the arc's site, and the prim spec it holds there. */
export interface SchemaSite {
  readonly layer: Layer;
  readonly spec: PrimSpec | null;
}

/**
 * The specs a prim's `apiSchemas` compose from, strongest first (each
 * arc's layers at its site, `CompositionArc.layers`; a relocate reads
 * none), and which of them edits write: `target` is where an edit of the
 * prim is written (its layer and spec path), and the index returned is
 * its site's, -1 when it is none of them (a variant not selected), when
 * `own` is the spec it holds there: what it applies it can still remove,
 * though the prim does not compose it.
 */
export function schemaSites(
  arcs: readonly Pick<CompositionArc, "arc" | "site" | "layers">[],
  target: { readonly layer: Layer; readonly specPath: string } | null,
  read: (layer: Layer, path: string) => PrimSpec | null,
): { readonly sites: SchemaSite[]; readonly target: number; readonly own: PrimSpec | null } {
  const sites: SchemaSite[] = [];
  let at = -1;
  for (const arc of arcs) {
    if (arc.arc === "relocates") continue;
    for (const layer of arc.layers) {
      if (target !== null && layer === target.layer && arc.site === target.specPath) at = sites.length;
      sites.push({ layer, spec: read(layer, arc.site) });
    }
  }
  return { sites, target: at, own: at < 0 && target !== null ? read(target.layer, target.specPath) : null };
}

/**
 * What removing an applied schema does where edits go (`removeAPI`,
 * `UsdPrim::RemoveAPI`, which takes it from the target's lists and adds
 * it to the deleted items):
 *
 * - `builtIn`: its type applies it; no layer can remove it;
 * - `remove`: the target's own spec applies it, so Remove takes it away
 *   there (and deletes what weaker layers apply);
 * - `delete`: only a weaker spec applies it (`layer`; `null` when none is
 *   found), so the edit is a delete here, which removes what that layer
 *   applies;
 * - `stronger`: a stronger spec applies it (`layer`), which a delete
 *   where edits go does not take away: edit there to remove it.
 *
 * AOUSD Core §12.4 (list ops: each stronger opinion's lists apply over
 * the weaker's result, so a weaker `delete` does not take what a
 * stronger list adds).
 */
export type SchemaRemoval =
  | { readonly kind: "builtIn" }
  | { readonly kind: "remove" }
  | { readonly kind: "delete"; readonly layer: Layer | null }
  | { readonly kind: "stronger"; readonly layer: Layer };

/** What removing `row` where edits go does ({@link SchemaRemoval}), over the specs its prim composes from ({@link schemaSites}). */
export function schemaRemoval(row: Pick<AppliedSchema, "applied" | "authored">, sites: readonly SchemaSite[], target: number, own: PrimSpec | null = null): SchemaRemoval {
  if (!row.authored) return { kind: "builtIn" };
  if (appliesSchema(target >= 0 ? sites[target]!.spec : own, row.applied)) return { kind: "remove" };
  const at = sites.findIndex((site, index) => index !== target && appliesSchema(site.spec, row.applied));
  if (at < 0) return { kind: "delete", layer: null };
  // A target the prim does not compose (a variant not selected) takes nothing away: as weak as can be.
  if (target < 0 || at < target) return { kind: "stronger", layer: sites[at]!.layer };
  return { kind: "delete", layer: sites[at]!.layer };
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
  // Counted as integers of any size: a suffix past 2^53 does not count on as a number.
  for (let index = match === null ? 1n : BigInt(match[2]!) + 1n; ; index++) {
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
 * does, and `visible`. Read from the ancestors' computed visibility,
 * which the rows have, so telling them apart costs no read.
 */
export type RowVisibility = "visible" | "invisible" | "hidden";

/** Each of `paths` (a tree's rows, parents before children)'s {@link RowVisibility} from its computed visibility (`computed`, in order). */
export function rowVisibilities(paths: readonly string[], computed: readonly (string | null)[]): Map<string, RowVisibility> {
  const out = new Map<string, RowVisibility>();
  paths.forEach((path, index) => {
    const visibility = computed[index];
    if (visibility === null || visibility === undefined) return;
    if (visibility !== "invisible") out.set(path, "visible");
    else out.set(path, hiddenAbove(path, out) ? "hidden" : "invisible");
  });
  return out;
}

/**
 * Whether an ancestor of `path` in `rows` is not visible. Visibility is
 * inherited through prims that are not `Imageable` (a typeless group has
 * no row here), as `UsdGeomImageable::ComputeVisibility` walks every
 * ancestor, so the nearest ancestor with a row decides, not the parent
 * alone.
 */
function hiddenAbove(path: string, rows: ReadonlyMap<string, RowVisibility>): boolean {
  for (let at = parentOf(path); at !== "/"; at = parentOf(at)) {
    const above = rows.get(at);
    if (above !== undefined) return above !== "visible";
  }
  return false;
}

/** The nearest ancestor of `path` whose own visibility hides it (`invisible` in `rows`); `null` when none is shown. */
export function hiddenBy(path: string, rows: ReadonlyMap<string, RowVisibility>): string | null {
  for (let at = parentOf(path); at !== "/"; at = parentOf(at)) if (rows.get(at) === "invisible") return at;
  return null;
}

/** A layer or an edit target through an arc, where a prim has opinions. */
export interface OpinionSite {
  readonly layer: Layer;
  /** The target that writes there; `null` when none does (a relocation's source). */
  readonly editTarget: EditTarget | null;
  /** The arc that reaches it; `null` for the stage's own layer stack. */
  readonly arc: ArcKind | null;
}

/**
 * Where the prim at `path` has opinions, strongest first: each layer that
 * holds a spec of it, in its own layer stack and in the stack of every
 * arc it composes from (`Stage.layerSpec` of each arc's site in each of
 * the arc's layers), with the target that writes there. What says where
 * a copy could be made when the edit target has no spec of it: a copy
 * reads one layer's spec, so a layer is named only when it holds one.
 */
export function opinionSites(stage: Stage, path: string): OpinionSite[] {
  const prim = stage.prim(path);
  if (prim === null) return [];
  const sites: OpinionSite[] = [];
  const seen = new Set<string>();
  for (const arc of prim.compositionArcs()) {
    for (const layer of arc.layers) {
      const key = `${layer.id} ${arc.site}`;
      if (seen.has(key) || stage.layerSpec(layer, arc.site) === null) continue;
      seen.add(key);
      let target: EditTarget | null = null;
      try {
        target = arc.editTarget(layer);
      } catch {
        // A relocation's source, which no edit goes through.
      }
      sites.push({ layer, editTarget: target, arc: arc.arc === "local" ? null : arc.arc });
    }
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
