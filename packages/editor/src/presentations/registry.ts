// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Presentations, after CLIM's: whatever the editor shows (a prim, a
// value, a path, a layer) is shown as an object of a presentation type,
// and what shows it is chosen by that type, not by the panel showing it.
// A colour swatch is the presentation of a `color3f` value wherever one
// appears; a Mesh preview the presentation of a `Mesh` prim.
//
// - **Types.** Each object has its presentation types, most specific
//   first ({@link typesOf}). A prim's are its schema type and that type's
//   ancestors (`Cube`, `Gprim`, `Boundable`, ..., `Typed`), then its
//   applied API schemas (`MaterialBindingAPI`, `CollectionAPI:parts`,
//   then `CollectionAPI`), then `Prim`. A value's are its value type
//   (`color3f[]`), its role (`color[]`), its shape (`array`) and `value`;
//   a token the schema restricts is `allowedToken` before `token`. Paths
//   are `primPath` or `propertyPath`, then `path`; a layer is `layer`.
// - **Views.** A registration presents one type in the views it can do:
//   `inline` (one line: property rows, tables, the explain and changes
//   panels), `accept` (an inline editor: a colour picker, a token menu),
//   `panel` (an inspector section: a mesh's preview, an instancer's
//   table) and `tooltip`. For a one-of view (`inline`, `accept`,
//   `tooltip`) the registration of the most specific type that has the
//   view wins ({@link PresentationRegistry.find}); every applicable
//   `panel` is shown, by priority ({@link PresentationRegistry.panels}).
// - **Accept.** An `accept` view edits through the callback it is given,
//   which the host routes to the store (`EditorStore.setValue`): the edit
//   is one transaction where edits go, undone, heard and logged as any.
// - **Commands.** What is presented stays live: a command registry keyed
//   by the same types offers, for any presented object, the commands that
//   apply to it (select, frame, explain, edit in a layer); the first one
//   is what a click does ({@link CommandRegistry.forObject}).
//
// Built-in presentations and commands register through this same API
// (`builtins.tsx`, called from `main.tsx`); a plugin registers its own the
// same way. Nothing here imports React or the DOM: the views are
// components only by type, so the dispatch is tested in Node.
import type { Attribute, Layer, Prim, SchemaInfo, Stage, ValueInput } from "@forest-rs/layerstack";
import type { ComponentType } from "react";
import { releaseValue, type Shown } from "../model/values.ts";
import type { EditorStore } from "../store.ts";

/** What presentations and commands are given besides their object: the editor, its stage and the time read at. */
export interface PresentationContext {
  readonly store: EditorStore;
  readonly stage: Stage;
  /** The time values are read at: a time code, or `undefined` for the default time. */
  readonly time: number | undefined;
}

/** A value as shown: its attribute (when it is one's), its type and its value read for display. */
export interface ValueObject {
  /** The attribute it is the value of; `null` for a value from elsewhere (an opinion, a record). */
  readonly attribute: Attribute | null;
  /** Its value type name (`color3f`, `float3[]`, `asset`). */
  readonly typeName: string;
  /** The value read for display ({@link Shown}): a scalar or tuple kept, anything else as text. */
  readonly shown: Shown;
  /** The time an edit authors a sample at (an animated attribute), or `null` for the default. */
  readonly at: number | null;
}

/** Each kind of object the editor presents, and what it holds. */
export interface PresentedObjects {
  /** A composed prim. */
  readonly prim: { readonly prim: Prim };
  /** A property's value. */
  readonly value: ValueObject;
  /** A prim's path (`/World/Ball`), which may name no prim. */
  readonly primPath: { readonly path: string };
  /** A property's path (`/World/Ball.radius`, a connection's source). */
  readonly propertyPath: { readonly path: string };
  /** A layer of the stage. */
  readonly layer: { readonly layer: Layer };
}

/** A kind of presented object. */
export type PresentedKind = keyof PresentedObjects;

/** A presented object: its kind and what it holds. */
export type Presented<K extends PresentedKind = PresentedKind> = { readonly [Kind in K]: { readonly kind: Kind } & PresentedObjects[Kind] }[K];

/** The views a type can be presented in. */
export type PresentationView = "inline" | "accept" | "panel" | "tooltip";

/** What a view is given. */
export interface PresentProps<K extends PresentedKind> {
  readonly object: Presented<K>;
  readonly context: PresentationContext;
}

/** What an `accept` view is given: its object, and the callback that authors what is accepted. */
export interface AcceptProps<K extends PresentedKind> extends PresentProps<K> {
  /** Authors `value` as one edit, where edits go (for a value, its attribute's default or its sample at `at`). */
  readonly accept: (value: ValueInput) => void;
}

/** A presentation: how objects of one type are shown, in the views it can do. */
export interface Presentation<K extends PresentedKind = PresentedKind> {
  /** Unique among presentations (`builtin.mesh`, `my-plugin.light-gel`): a registration with the same id replaces it. */
  readonly id: string;
  /** The kind of object it presents. */
  readonly kind: K;
  /** The presentation type it presents (`Mesh`, `MaterialBindingAPI`, `color`, `asset`, `primPath`). */
  readonly type: string;
  /** A panel's title. */
  readonly title?: string;
  /**
   * Among panels, larger first (default 0; the built-ins use 100 for the
   * prim's state, 50 for a type's own view, 0 for its properties, below 0
   * for what follows them). For a one-of view, it breaks a tie between
   * registrations of the same type.
   */
  readonly priority?: number;
  /** Whether it applies to this object of its type; every object of the type when absent. */
  readonly applies?: (object: Presented<K>, context: PresentationContext) => boolean;
  readonly inline?: ComponentType<PresentProps<K>>;
  readonly accept?: ComponentType<AcceptProps<K>>;
  readonly panel?: ComponentType<PresentProps<K>>;
  readonly tooltip?: ComponentType<PresentProps<K>>;
}

/** A command on presented objects of one type: what a click or a context menu on one offers. */
export interface Command<K extends PresentedKind = PresentedKind> {
  /** Unique among commands; a registration with the same id replaces it. */
  readonly id: string;
  readonly kind: K;
  /** The presentation type it applies to (`primPath`, `layer`, `asset`). */
  readonly type: string;
  /** The menu's label (`Select`, `Frame in the viewport`). */
  readonly title: string;
  /** Whether it applies to this object now; always when absent. */
  readonly applies?: (object: Presented<K>, context: PresentationContext) => boolean;
  /** Runs it. Edits go through the store, as any. */
  readonly run: (object: Presented<K>, context: PresentationContext) => void;
}

/** Typed schemas by name, read once per stage: what {@link typesOf} reads a prim's ancestry from. */
const schemaTables = new WeakMap<Stage, ReadonlyMap<string, SchemaInfo>>();

function schemaTable(stage: Stage): ReadonlyMap<string, SchemaInfo> {
  let table = schemaTables.get(stage);
  if (table === undefined) {
    table = new Map(stage.schemas().map((schema) => [schema.name, schema]));
    schemaTables.set(stage, table);
  }
  return table;
}

/**
 * A prim's presentation types, most specific first: its type and its
 * type's ancestors (`UsdPrim::IsA` order), its applied schemas
 * (`UsdPrim::GetAppliedSchemas` order; a multiple-apply instance
 * `CollectionAPI:parts` followed by its schema `CollectionAPI`, once),
 * then `Prim`.
 */
export function primTypes(prim: Prim): string[] {
  const types: string[] = [];
  const typeName = prim.typeName;
  if (typeName !== null && typeName !== "") {
    types.push(typeName);
    for (const base of schemaTable(prim.stage).get(typeName)?.bases ?? []) if (!types.includes(base)) types.push(base);
  }
  const applied = prim.appliedSchemas();
  for (const name of applied) {
    types.push(name);
    const colon = name.indexOf(":");
    if (colon > 0) {
      const schema = name.slice(0, colon);
      if (!types.includes(schema)) types.push(schema);
    }
  }
  types.push("Prim");
  return types;
}

/**
 * The role a value type has, by its name: `color` for `color3f`,
 * `color4d` and their arrays; `null` for a type without one. USD's roles
 * (`UsdValueTypeNames` `Color`, `Point`, `Normal`, `Vector`, `TexCoord`,
 * `Matrix`, `Frame`) are spelled in the type names.
 */
export function valueRole(typeName: string): string | null {
  const scalar = typeName.endsWith("[]") ? typeName.slice(0, -2) : typeName;
  const match = /^(color|point|normal|vector|texCoord)[234][fdh]$/.exec(scalar);
  return match?.[1] ?? null;
}

/**
 * A value's presentation types, most specific first: `allowedToken` for
 * a token the schema restricts, its value type (`color3f[]`), its role
 * (`color[]`, see {@link valueRole}), its kind of type (`token`, `asset`,
 * `bool`, `number`, `string`), its shape (`array`, `tuple`), then `value`.
 */
export function valueTypes(typeName: string, options: { readonly allowedTokens?: boolean } = {}): string[] {
  const array = typeName.endsWith("[]");
  const scalar = array ? typeName.slice(0, -2) : typeName;
  const suffix = array ? "[]" : "";
  // A token the schema restricts is more specific than its type.
  const types = scalar === "token" && options.allowedTokens === true && !array ? ["allowedToken", typeName] : [typeName];
  const role = valueRole(typeName);
  if (role !== null) types.push(`${role}${suffix}`);
  if (scalar === "token" || scalar === "asset" || scalar === "string" || scalar === "bool") {
    if (array) types.push(`${scalar}[]`);
    else if (!types.includes(scalar)) types.push(scalar);
  } else if (/^(u?int|u?int64|half|float|double|timecode|uchar)$/.test(scalar)) {
    if (!array) types.push("number");
  }
  if (array) types.push("array");
  else if (/[234][fdhi]?$/.test(scalar) || /^(matrix|quat)/.test(scalar)) types.push("tuple");
  types.push("value");
  return [...new Set(types)];
}

/** Whether the schema gives `attribute` `allowedTokens`. */
function restricted(attribute: Attribute): boolean {
  const value = attribute.metadata("allowedTokens");
  releaseValue(value);
  return value !== null;
}

/** An object's presentation types, most specific first. */
export function typesOf(object: Presented): readonly string[] {
  switch (object.kind) {
    case "prim":
      return primTypes(object.prim);
    case "value":
      return valueTypes(object.typeName, { allowedTokens: object.typeName === "token" && object.attribute !== null && restricted(object.attribute) });
    case "primPath":
      return ["primPath", "path"];
    case "propertyPath":
      return ["propertyPath", "path"];
    case "layer":
      return ["layer"];
  }
}

/** The presentations an editor knows, by type and view. */
export class PresentationRegistry {
  readonly #entries = new Map<string, Presentation>();

  /** Registers `presentation`, replacing one of the same id; returns the call that removes it. */
  register<K extends PresentedKind>(presentation: Presentation<K>): () => void {
    this.#entries.set(presentation.id, presentation as unknown as Presentation);
    return () => {
      if (this.#entries.get(presentation.id) === (presentation as unknown as Presentation)) this.#entries.delete(presentation.id);
    };
  }

  /** Every registration, in registration order. */
  all(): readonly Presentation[] {
    return [...this.#entries.values()];
  }

  /**
   * The presentation that shows `object` in `view`: of the registrations
   * that have the view and apply, the one whose type comes first in
   * `object`'s types (`types`, read with {@link typesOf} when not given);
   * on a tie, the larger priority, then the later registration. `null`
   * when none does.
   */
  find<K extends PresentedKind>(
    object: Presented<K>,
    view: Exclude<PresentationView, "panel">,
    context: PresentationContext,
    types: readonly string[] = typesOf(object as Presented),
  ): Presentation<K> | null {
    let best: Presentation | null = null;
    let bestRank = Infinity;
    for (const entry of this.#entries.values()) {
      if (entry.kind !== object.kind || entry[view] === undefined) continue;
      const rank = types.indexOf(entry.type);
      if (rank < 0 || rank > bestRank) continue;
      if (rank === bestRank && (entry.priority ?? 0) < (best?.priority ?? 0)) continue;
      if (entry.applies !== undefined && !entry.applies(object as Presented, context)) continue;
      best = entry;
      bestRank = rank;
    }
    return best as unknown as Presentation<K> | null;
  }

  /**
   * Every panel that applies to `object`, larger priority first, then the
   * more specific type, then registration order.
   */
  panels<K extends PresentedKind>(object: Presented<K>, context: PresentationContext, types: readonly string[] = typesOf(object as Presented)): Presentation<K>[] {
    const found: { entry: Presentation; rank: number; order: number }[] = [];
    let order = 0;
    for (const entry of this.#entries.values()) {
      order += 1;
      if (entry.kind !== object.kind || entry.panel === undefined) continue;
      const rank = types.indexOf(entry.type);
      if (rank < 0) continue;
      if (entry.applies !== undefined && !entry.applies(object as Presented, context)) continue;
      found.push({ entry, rank, order });
    }
    found.sort((a, b) => (b.entry.priority ?? 0) - (a.entry.priority ?? 0) || a.rank - b.rank || a.order - b.order);
    return found.map(({ entry }) => entry as unknown as Presentation<K>);
  }
}

/** The commands an editor offers on presented objects, by type. */
export class CommandRegistry {
  readonly #entries = new Map<string, Command>();

  /** Registers `command`, replacing one of the same id; returns the call that removes it. */
  register<K extends PresentedKind>(command: Command<K>): () => void {
    this.#entries.set(command.id, command as unknown as Command);
    return () => {
      if (this.#entries.get(command.id) === (command as unknown as Command)) this.#entries.delete(command.id);
    };
  }

  /**
   * The commands that apply to `object`: those of its more specific types
   * first, each type's in registration order. The first is what a click on
   * it does.
   */
  forObject<K extends PresentedKind>(object: Presented<K>, context: PresentationContext, types: readonly string[] = typesOf(object as Presented)): Command<K>[] {
    const found: { entry: Command; rank: number; order: number }[] = [];
    let order = 0;
    for (const entry of this.#entries.values()) {
      order += 1;
      if (entry.kind !== object.kind) continue;
      const rank = types.indexOf(entry.type);
      if (rank < 0) continue;
      if (entry.applies !== undefined && !entry.applies(object as Presented, context)) continue;
      found.push({ entry, rank, order });
    }
    found.sort((a, b) => a.rank - b.rank || a.order - b.order);
    return found.map(({ entry }) => entry as unknown as Command<K>);
  }
}

/** The editor's presentations: the built-ins and any a plugin registers. */
export const presentations = new PresentationRegistry();

/** The editor's commands on presented objects. */
export const commands = new CommandRegistry();

/** Registers a presentation with the editor ({@link PresentationRegistry.register}); returns the call that removes it. */
export function registerPresentation<K extends PresentedKind>(presentation: Presentation<K>): () => void {
  return presentations.register(presentation);
}

/** Registers a command with the editor ({@link CommandRegistry.register}); returns the call that removes it. */
export function registerCommand<K extends PresentedKind>(command: Command<K>): () => void {
  return commands.register(command);
}
