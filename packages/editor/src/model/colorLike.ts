// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Float tuples that are colours by what they are for. A `float3` or
// `float4` (or `double`, `half`) shading input without the colour role is
// still a colour when the input it is, or the inputs it feeds, take one:
//
// - **By definition.** `UsdUVTexture`'s `inputs:scale`, `inputs:bias` and
//   `inputs:fallback` are `float4`s applied channel by channel to the
//   texture's RGBA (`outputs:rgba = texel * scale + bias`; `fallback` is
//   the RGBA read where there is no texture): the UsdPreviewSurface
//   specification, "UsdUVTexture" inputs. Its shader definition gives them
//   no colour type (Sdr reads them as plain `float4`), so the editor knows
//   them by name. Any other input is one when its `sdrMetadata` says so: a
//   `role` or `widget` of `color`, or a `renderType` that is a colour type
//   (`SdrPropertyMetadata` `Role`, `Widget`, `RenderType`). An input of a
//   colour type (`color3f`) is a colour already.
// - **By what it feeds.** A material's or node graph's interface input is
//   one when an input connected to it is one: a shader's, by definition,
//   or another node graph's interface input, by what that feeds in turn
//   (`UsdShadeConnectableAPI` connections followed downstream, from each
//   consumer to its source, through any depth of node graphs).
//
// The consumers are found by reading the connections of every input in
// the network the input belongs to (its enclosing material, else its
// prim's parent) once per revision of that prim (`revisions.ts`), not per
// render.
import type { Stage } from "@forest-rs/layerstack";
import { primRevision } from "./revisions.ts";
import type { Session } from "./session.ts";

/** Why a float tuple input is shown as a colour. */
export interface ColorLike {
  /** The shading input that takes a colour (`/Looks/Wood/Texture.inputs:scale`): the input itself, or the first colour input it feeds. */
  readonly input: string;
  /** Why that input takes a colour: it is a known one of the UsdPreviewSurface nodes, its `sdrMetadata` says so, or its type has the colour role. */
  readonly why: "previewSurface" | "sdrMetadata" | "colorRole";
  /** The interface inputs from the attribute to `input`, the attribute first; empty when the attribute is `input`. */
  readonly through: readonly string[];
}

/** The UsdPreviewSurface nodes' inputs that are RGBA, by shader id (the UsdPreviewSurface specification's `UsdUVTexture` inputs). */
const PREVIEW_COLOR_INPUTS: ReadonlyMap<string, ReadonlySet<string>> = new Map([["UsdUVTexture", new Set(["scale", "bias", "fallback"])]]);

/** Whether `sdrMetadata` marks an input as a colour (`role`, `widget`, `renderType`). */
function sdrColor(metadata: ReadonlyMap<string, string>): boolean {
  return metadata.get("role") === "color" || metadata.get("widget") === "color" || /^color[34]?$/.test(metadata.get("renderType") ?? "");
}

/** Whether a type name has the colour role (`color3f`, `color4d`). */
function colorRole(typeName: string | null): boolean {
  return typeName !== null && /^color[34][fdh]$/.test(typeName);
}

/** The property path's prim. */
function primOf(path: string): string {
  return path.slice(0, path.lastIndexOf("."));
}

/** The parent of a prim path; `null` for the root. */
function parentOf(path: string): string | null {
  if (path === "/") return null;
  const slash = path.lastIndexOf("/");
  return slash <= 0 ? "/" : path.slice(0, slash);
}

/** The prim whose subtree holds every consumer of an input on the prim at `prim`: its enclosing material, else its parent. */
export function networkScope(stage: Stage, prim: string): string {
  for (let at: string | null = prim; at !== null && at !== "/"; at = parentOf(at)) if (stage.material(at) !== null) return at;
  return parentOf(prim) ?? "/";
}

/** One shading input as the inference reads it. */
interface Port {
  readonly path: string;
  readonly typeName: string | null;
  /** The shader's id when it is a shader's input; `null` for an interface input. */
  readonly shaderId: string | null;
  readonly shader: boolean;
  readonly sdrMetadata: ReadonlyMap<string, string>;
}

/** A network's inputs by path, and each source's consumers. */
interface Network {
  readonly ports: ReadonlyMap<string, Port>;
  readonly consumers: ReadonlyMap<string, readonly string[]>;
}

/** Reads the inputs of every shader, node graph and material in the subtree at `scope`, and who reads whom. */
function readNetwork(stage: Stage, scope: string): Network {
  const ports = new Map<string, Port>();
  const consumers = new Map<string, string[]>();
  const visit = (path: string) => {
    const prim = stage.prim(path);
    if (prim === null) return;
    const shader = prim.isA("Shader") ? stage.shader(path) : null;
    if (shader !== null || prim.isA("NodeGraph")) {
      for (const port of prim.inputs()) {
        ports.set(port.path, { path: port.path, typeName: port.typeName, shaderId: shader?.id ?? null, shader: shader !== null, sdrMetadata: port.sdrMetadata });
        for (const source of port.connections) {
          const list = consumers.get(source);
          if (list === undefined) consumers.set(source, [port.path]);
          else list.push(port.path);
        }
      }
    }
    for (const child of stage.children(path)) visit(child);
  };
  visit(scope);
  return { ports, consumers };
}

/** Whether a shader's input takes a colour by its definition; `null` when not. */
function definedColor(port: Port): ColorLike["why"] | null {
  if (colorRole(port.typeName)) return "colorRole";
  if (port.shader && port.shaderId !== null && PREVIEW_COLOR_INPUTS.get(port.shaderId)?.has(port.path.slice(port.path.lastIndexOf(".inputs:") + ".inputs:".length))) {
    return "previewSurface";
  }
  if (sdrColor(port.sdrMetadata)) return "sdrMetadata";
  return null;
}

/** Whether the input at `path` in `network` takes a colour, by definition or by what it feeds (breadth first: the nearest colour input). */
function inferFrom(network: Network, path: string): ColorLike | null {
  const start = network.ports.get(path);
  if (start === undefined) return null;
  if (start.shader) {
    const why = definedColor(start);
    return why === null ? null : { input: path, why, through: [] };
  }
  // An interface input: follow its consumers downstream.
  const queue: { readonly path: string; readonly through: readonly string[] }[] = [{ path, through: [path] }];
  const seen = new Set([path]);
  for (let at = 0; at < queue.length; at++) {
    const { path: source, through } = queue[at]!;
    for (const consumer of network.consumers.get(source) ?? []) {
      if (seen.has(consumer)) continue;
      seen.add(consumer);
      const port = network.ports.get(consumer);
      if (port === undefined) continue;
      const why = definedColor(port);
      if (why !== null) return { input: consumer, why, through };
      if (!port.shader) queue.push({ path: consumer, through: [...through, consumer] });
    }
  }
  return null;
}

/**
 * Why the input at `path` is a colour, by its definition or by what it
 * feeds (the header of this file), read now; `null` when it is not one.
 * Does not look at the input's own type: the registry offers this only
 * for a three- or four-channel float tuple (its `colorLike` type).
 */
export function colorLikeNow(stage: Stage, path: string): ColorLike | null {
  if (!path.includes(".inputs:")) return null;
  return inferFrom(readNetwork(stage, networkScope(stage, primOf(path))), path);
}

/** A network read for a revision of its scope. */
interface Cached {
  readonly revision: number;
  readonly network: Network;
  readonly answers: Map<string, ColorLike | null>;
}

/** A session's networks by scope, and each input's scope as of a length of its change log. */
interface SessionCache {
  readonly networks: Map<string, Cached>;
  scopesAt: number;
  scopes: Map<string, string>;
}

const cache = new WeakMap<Session, SessionCache>();

/**
 * {@link colorLikeNow} for `session`'s stage, read once per revision of
 * the network's scope ({@link primRevision}): the network is read again
 * only after a recomposition reaches it, and each input's answer kept
 * until then. An input's scope is found again after any recomposition.
 */
export function colorLike(session: Session, path: string): ColorLike | null {
  if (!path.includes(".inputs:")) return null;
  const stage = session.stage;
  let held = cache.get(session);
  if (held === undefined) {
    held = { networks: new Map(), scopesAt: session.changeLog.length, scopes: new Map() };
    cache.set(session, held);
  }
  if (held.scopesAt !== session.changeLog.length) {
    held.scopesAt = session.changeLog.length;
    held.scopes = new Map();
  }
  let scope = held.scopes.get(path);
  if (scope === undefined) {
    scope = networkScope(stage, primOf(path));
    held.scopes.set(path, scope);
  }
  const revision = primRevision(session, scope);
  let cached = held.networks.get(scope);
  if (cached === undefined || cached.revision !== revision) {
    cached = { revision, network: readNetwork(stage, scope), answers: new Map() };
    held.networks.set(scope, cached);
  }
  let answer = cached.answers.get(path);
  if (answer === undefined) {
    answer = inferFrom(cached.network, path);
    cached.answers.set(path, answer);
  }
  return answer;
}

/** What a colour-like input's cue says: `as colour (feeds Texture.scale)`, or `as colour (UsdUVTexture's RGBA input)`. */
export function colorLikeCue(found: ColorLike): string {
  const prim = primOf(found.input);
  const name = `${prim.slice(prim.lastIndexOf("/") + 1)}.${found.input.slice(found.input.lastIndexOf(".inputs:") + ".inputs:".length)}`;
  if (found.through.length > 0) return `as colour (feeds ${name})`;
  return found.why === "previewSurface" ? "as colour (an RGBA input of UsdUVTexture)" : "as colour (its sdrMetadata marks it one)";
}
