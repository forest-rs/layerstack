// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Light and shadow linking over a draw list's items, as Hydra's
// light-link categories: the TypeScript mirror of `layerstack_stagehand`'s
// `links.rs`. Each light's `collection:lightLink` and
// `collection:shadowLink` (`UsdLuxLightAPI`) is a membership query
// (`Stage.collectionMembership`); equal queries share one category, and
// the fallback (`/` with `expandPrims` alone) has none and links
// everything, as `UsdImaging_CollectionCache` keeps them. A target is
// classified once against each distinct category, never every light
// against every item.

import type { Changes, Stage } from "@forest-rs/layerstack";

/** Which of a light's two linking collections: `lightLink` (what it illuminates) or `shadowLink` (what casts its shadows). */
export type LinkKind = "light" | "shadow";

/**
 * What a link decides for: an item, matched at one of its
 * {@link DrawItem.linkPaths}: a gprim's path, each instance proxy of a
 * native prototype's item (`UsdImagingInstanceAdapter::
 * GetInstanceCategories`), or a point instancer's path for its instances
 * (`UsdImagingDelegate::GetCategories` gives prototypes no categories).
 */
export interface LinkTarget {
  readonly item: number;
  readonly path: string;
}

/** One light-link category: the links sharing its membership, and the targets it includes. */
export interface LinkSet {
  readonly category: number;
  /** Each light (by key) and which of its links computes this membership. */
  readonly links: readonly { readonly light: string; readonly kind: LinkKind }[];
  /** The targets it includes (`IsPathIncluded`), by item then path. */
  readonly members: readonly LinkTarget[];
}

/**
 * How one light's link changed in a diff: the targets it links now and did
 * not before, and those it no longer links. Listed for lights and items the
 * diff neither adds nor removes; read a new light's or item's links from
 * {@link DrawList.isLinked}.
 */
export interface LinkChange {
  readonly light: string;
  readonly kind: LinkKind;
  readonly added: readonly LinkTarget[];
  readonly removed: readonly LinkTarget[];
}

/** The collection names of each kind. */
export const LINK_COLLECTIONS: Readonly<Record<LinkKind, string>> = { light: "lightLink", shadow: "shadowLink" };
const KINDS: readonly LinkKind[] = ["light", "shadow"];

interface Category {
  readonly category: number;
  readonly key: string;
  readonly reads: readonly string[];
  readonly readsAny: boolean;
  /** Link keys ({@link linkKey}). */
  readonly links: Set<string>;
  /** By {@link targetKey}. */
  readonly members: Map<string, LinkTarget>;
}

interface Link {
  readonly light: string;
  readonly path: string;
  readonly kind: LinkKind;
  category: number | null;
}

const linkKey = (light: string, kind: LinkKind) => `${kind} ${light}`;
const targetKey = (target: LinkTarget) => `${target.item} ${target.path}`;
const byTarget = (a: LinkTarget, b: LinkTarget) => a.item - b.item || (a.path < b.path ? -1 : a.path > b.path ? 1 : 0);

function under(path: string, root: string): boolean {
  return root === "/" || path === root || path.startsWith(`${root}/`);
}

type Records = Pick<Changes, "changedInfoOnly" | "properties" | "resyncedProperties">;

/** Whether `changes` reach the collection at `collection` (`/World.collection:robots`): a resync over its prim, one of its properties, or an unlisted change of its prim or above it. */
function collectionTouched(changes: Records, roots: readonly string[], collection: string): boolean {
  const dot = collection.lastIndexOf(".");
  const prim = collection.slice(0, dot);
  const name = collection.slice(dot + 1);
  const ofCollection = (property: string) => property.startsWith(`${name}:`);
  return (
    roots.some((root) => under(prim, root)) ||
    changes.changedInfoOnly.some((changed) => {
      if (changed === "/") return false;
      const inventory = changes.properties.find((properties) => properties.prim === changed);
      return inventory === undefined ? under(prim, changed) : changed === prim && inventory.fields.some((field) => ofCollection(field.property));
    }) ||
    changes.resyncedProperties.some((property) => {
      const at = property.lastIndexOf(".");
      return property.slice(0, at) === prim && ofCollection(property.slice(at + 1));
    })
  );
}

/** Whether `changes` touch any collection property: what a query with a missing reference reads. */
function anyCollectionTouched(changes: Records, roots: readonly string[]): boolean {
  return (
    roots.length > 0 ||
    changes.changedInfoOnly.some((changed) => {
      if (changed === "/") return false;
      const inventory = changes.properties.find((properties) => properties.prim === changed);
      return inventory === undefined || inventory.fields.some((field) => field.property.startsWith("collection:"));
    }) ||
    changes.resyncedProperties.some((property) => property.slice(property.lastIndexOf(".") + 1).startsWith("collection:"))
  );
}

/** What a diff hands the link pass. */
export interface LinkInputs {
  /** The change records, for the collections other links read; `null` for a time change. */
  readonly changes: Records | null;
  readonly roots: readonly string[];
  /** Link keys whose own collection properties a change reached. */
  readonly relink: ReadonlySet<string>;
  /** Items read again entirely, whose targets are classified again. */
  readonly reclassify: ReadonlySet<number>;
  readonly added: ReadonlySet<number>;
  readonly removed: ReadonlySet<number>;
  readonly lightsAdded: ReadonlySet<string>;
}

/** What the link pass did. */
export interface LinkOutcome {
  readonly changes: LinkChange[];
  /** Lights whose categories changed. */
  readonly relinked: Set<string>;
  evaluated: number;
  classified: number;
}

/** The link key of a light's link, for {@link LinkInputs.relink}. */
export { linkKey };

/** A draw list's light links: their categories, members and each listed light's categories. */
export class Links {
  readonly #categories = new Map<number, Category>();
  readonly #of = new Map<string, Link>();
  /** Each item's link paths, as of the last diff. */
  readonly #targets = new Map<number, readonly string[]>();
  #next = 0;

  /** The category of a light's link: `null` for a trivial one, `undefined` for a light not listed. */
  category(light: string, kind: LinkKind): number | null | undefined {
    return this.#of.get(linkKey(light, kind))?.category;
  }

  isLinked(light: string, kind: LinkKind, target: LinkTarget): boolean {
    const link = this.#of.get(linkKey(light, kind));
    if (link === undefined) return false;
    if (link.category === null) return this.#targets.get(target.item)?.includes(target.path) ?? false;
    return this.#categories.get(link.category)!.members.has(targetKey(target));
  }

  categoriesOf(target: LinkTarget): number[] {
    const key = targetKey(target);
    return [...this.#categories.values()].filter((set) => set.members.has(key)).map((set) => set.category);
  }

  sets(): LinkSet[] {
    return [...this.#categories.values()].map((set) => this.#record(set));
  }

  set(category: number): LinkSet | null {
    const set = this.#categories.get(category);
    return set === undefined ? null : this.#record(set);
  }

  #record(set: Category): LinkSet {
    const links = [...set.links].map((key) => {
      const link = this.#of.get(key)!;
      return Object.freeze({ light: link.light, kind: link.kind });
    });
    links.sort((a, b) => (a.light < b.light ? -1 : a.light > b.light ? 1 : a.kind < b.kind ? -1 : a.kind > b.kind ? 1 : 0));
    return Object.freeze({ category: set.category, links: Object.freeze(links), members: Object.freeze([...set.members.values()].sort(byTarget)) });
  }

  #every(): LinkTarget[] {
    const all: LinkTarget[] = [];
    for (const [item, paths] of this.#targets) for (const path of paths) all.push({ item, path });
    return all.sort(byTarget);
  }

  /** Classifies `targets` against `set`, by one of its links. */
  #classify(stage: Stage, set: Category, targets: readonly LinkTarget[]): boolean[] {
    if (targets.length === 0) return [];
    const link = this.#of.get(set.links.values().next().value!)!;
    return [...stage.collectionMembership(link.path, LINK_COLLECTIONS[link.kind], targets.map((target) => target.path)).included];
  }

  /**
   * Brings the links up to the lights (`lights`, key to path) and targets
   * now listed, as `layerstack_stagehand` does: the queries of new lights
   * and of those a change reaches computed, new targets (and those of items
   * read again entirely) classified against each category, and a new
   * category's every target.
   */
  apply(stage: Stage, lights: ReadonlyMap<string, string>, updates: ReadonlyMap<number, readonly string[]>, inputs: LinkInputs): LinkOutcome {
    const outcome: LinkOutcome = { changes: [], relinked: new Set(), evaluated: 0, classified: 0 };
    const persistent = (target: LinkTarget) => !inputs.added.has(target.item) && !inputs.removed.has(target.item);

    const relink = new Map<string, { light: string; kind: LinkKind }>();
    for (const light of [...lights.keys()].sort()) {
      for (const kind of KINDS) {
        const key = linkKey(light, kind);
        if (!this.#of.has(key) || inputs.relink.has(key)) relink.set(key, { light, kind });
      }
    }
    if (inputs.changes !== null) {
      for (const set of this.#categories.values()) {
        const touched =
          set.reads.some((collection) => collectionTouched(inputs.changes!, inputs.roots, collection)) ||
          (set.readsAny && anyCollectionTouched(inputs.changes!, inputs.roots));
        if (!touched) continue;
        for (const key of set.links) {
          const link = this.#of.get(key)!;
          relink.set(key, { light: link.light, kind: link.kind });
        }
      }
    }

    interface Switch {
      readonly key: string;
      readonly light: string;
      readonly kind: LinkKind;
      readonly old: number | null;
      readonly membership: { key: string; everything: boolean; reads: readonly string[]; readsAny: boolean };
      readonly before: Map<string, LinkTarget> | null;
    }
    const switches: Switch[] = [];
    for (const [key, { light, kind }] of [...relink].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
      const path = lights.get(light);
      if (path === undefined) continue;
      const membership = stage.collectionMembership(path, LINK_COLLECTIONS[kind], []);
      outcome.evaluated++;
      const link = this.#of.get(key);
      const same =
        link !== undefined && (link.category === null ? membership.everything : this.#categories.get(link.category)!.key === membership.key);
      if (same) continue;
      const before =
        link === undefined
          ? null
          : link.category === null
            ? new Map(this.#every().map((target) => [targetKey(target), target]))
            : new Map(this.#categories.get(link.category)!.members);
      switches.push({ key, light, kind, old: link?.category ?? null, membership, before });
    }

    // Lights gone, and those switching, leave their categories. A category
    // keeps its last link to classify by until the switches are done.
    for (const [key, link] of [...this.#of]) {
      if (lights.has(link.light)) continue;
      this.#of.delete(key);
      if (link.category !== null) this.#categories.get(link.category)?.links.delete(key);
    }
    for (const change of switches) if (change.old !== null) this.#categories.get(change.old)?.links.delete(change.key);
    for (const [category, set] of [...this.#categories]) if (set.links.size === 0) this.#categories.delete(category);

    // Targets gone leave every category; new ones, and those of items read
    // again entirely, are classified against each.
    const gained = new Map<number, LinkTarget[]>();
    const lost = new Map<number, LinkTarget[]>();
    const note = (into: Map<number, LinkTarget[]>, category: number, target: LinkTarget) => {
      const list = into.get(category);
      if (list === undefined) into.set(category, [target]);
      else list.push(target);
    };
    const goneTargets: LinkTarget[] = [];
    const newTargets: LinkTarget[] = [];
    for (const [item, paths] of updates) {
      const old = this.#targets.get(item) ?? [];
      for (const path of old) if (!paths.includes(path)) goneTargets.push({ item, path });
      for (const path of paths) if (!old.includes(path)) newTargets.push({ item, path });
    }
    for (const target of goneTargets) {
      for (const set of this.#categories.values()) {
        if (set.members.delete(targetKey(target)) && persistent(target)) note(lost, set.category, target);
      }
    }
    for (const [item, paths] of updates) {
      if (paths.length === 0) this.#targets.delete(item);
      else this.#targets.set(item, paths);
    }
    const classify = new Map(newTargets.map((target) => [targetKey(target), target]));
    for (const item of inputs.reclassify) {
      for (const path of this.#targets.get(item) ?? []) classify.set(targetKey({ item, path }), { item, path });
    }
    const asked = [...classify.values()].sort(byTarget);
    for (const set of this.#categories.values()) {
      // A set whose every link is leaving is classified by none: it was dropped above.
      const included = this.#classify(stage, set, asked);
      outcome.classified += asked.length;
      asked.forEach((target, at) => {
        const key = targetKey(target);
        const changed = included[at] ? !set.members.has(key) : set.members.has(key);
        if (included[at]) set.members.set(key, target);
        else set.members.delete(key);
        if (changed && persistent(target)) note(included[at] ? gained : lost, set.category, target);
      });
    }

    // Switched links join the category of an equal query, or make one,
    // classifying every target; a trivial query has none.
    const switched = new Map<string, Map<string, LinkTarget> | null>();
    for (const change of switches) {
      let category: number | null = null;
      if (!change.membership.everything) {
        const existing = [...this.#categories.values()].find((set) => set.key === change.membership.key);
        if (existing !== undefined) {
          existing.links.add(change.key);
          category = existing.category;
        } else {
          category = this.#next++;
          const set: Category = {
            category,
            key: change.membership.key,
            reads: change.membership.reads,
            readsAny: change.membership.readsAny,
            links: new Set([change.key]),
            members: new Map(),
          };
          this.#of.set(change.key, { light: change.light, path: lights.get(change.light)!, kind: change.kind, category });
          const every = this.#every();
          const included = this.#classify(stage, set, every);
          outcome.classified += every.length;
          every.forEach((target, at) => {
            if (included[at]) set.members.set(targetKey(target), target);
          });
          this.#categories.set(category, set);
        }
      }
      this.#of.set(change.key, { light: change.light, path: lights.get(change.light)!, kind: change.kind, category });
      outcome.relinked.add(change.light);
      switched.set(change.key, change.before);
    }

    // The listing, for the lights kept.
    const everything = new Map(this.#every().map((target) => [targetKey(target), target]));
    for (const [key, link] of [...this.#of].sort(([a], [b]) => {
      const [x, y] = [this.#of.get(a)!, this.#of.get(b)!];
      return x.light < y.light ? -1 : x.light > y.light ? 1 : x.kind < y.kind ? -1 : x.kind > y.kind ? 1 : 0;
    })) {
      if (inputs.lightsAdded.has(link.light)) continue;
      let added: LinkTarget[];
      let removed: LinkTarget[];
      if (switched.has(key)) {
        const before = switched.get(key)!;
        if (before === null) continue;
        const after = link.category === null ? everything : this.#categories.get(link.category)!.members;
        added = [...after].filter(([at, target]) => !before.has(at) && persistent(target)).map(([, target]) => target);
        removed = [...before].filter(([at, target]) => !after.has(at) && persistent(target)).map(([, target]) => target);
      } else if (link.category === null) {
        added = newTargets.filter(persistent);
        removed = goneTargets.filter(persistent);
      } else {
        added = gained.get(link.category) ?? [];
        removed = lost.get(link.category) ?? [];
      }
      if (added.length > 0 || removed.length > 0) {
        outcome.changes.push(
          Object.freeze({ light: link.light, kind: link.kind, added: Object.freeze([...added].sort(byTarget)), removed: Object.freeze([...removed].sort(byTarget)) }),
        );
      }
    }
    return outcome;
  }
}
