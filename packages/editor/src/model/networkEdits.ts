// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the shading view's gestures author, as plain edits, and the menus
// that offer them, as data: the view renders a menu and hands what is
// chosen to the store, so what each gesture does is tested in Node.
//
// - Connecting authors the destination's `.connect`
//   (`UsdShadeConnectableAPI::ConnectToSource`, replacing what it read).
//   A port only the shader's definition has is created first, with the
//   definition's type: the destination by the edit's `typeName`, a source
//   output by `createAttribute` (as `UsdShadeShader::CreateOutput`, not
//   custom), so it is not given the destination's type.
// - Disconnecting removes one source (`DisconnectSource(source)`), or
//   every one.
// - Adding a shader defines a `Shader` child of the container with its
//   `info:id`, named after the definition and made unique, and, when the
//   network places its nodes (`ui:nodegraph:node:pos`), a position there.
import type { Edit, Stage } from "@forest-rs/layerstack";
import { type Candidate, destinationsFor, findPort, isDestination, isSource, nodeName, type PortRef, portName, sourcesFor } from "./connections.ts";
import { type NetworkLayout, type NetworkNode, primOf, type ShadingNetwork } from "./network.ts";
import { definedInput, definedOutput, SHADER_DEFINITIONS, type ShaderDefinition, shaderDefinition } from "./shaderDefinitions.ts";

/** The attribute where an authored node position is (`UsdUINodeGraphNodeAPI`'s `ui:nodegraph:node:pos`). */
export const NODE_POSITION = "ui:nodegraph:node:pos";

/** The type the port at `path` has or would be created with: its own, else its shader definition's; `null` when neither knows. */
export function portTypeName(stage: Stage, path: string): string | null {
  const port = stage.port(path);
  if (port !== null) return port.typeName;
  const prim = primOf(path);
  const definition = shaderDefinition(stage.shader(prim)?.id ?? null);
  const name = path.slice(path.lastIndexOf(".") + 1);
  if (name.startsWith("inputs:")) return definedInput(definition, name.slice("inputs:".length))?.typeName ?? null;
  if (name.startsWith("outputs:")) return definedOutput(definition, name.slice("outputs:".length))?.typeName ?? null;
  return null;
}

/**
 * The edits that connect `destination` to read `source` (property paths),
 * creating either when only its shader's definition has it, as the header
 * of this file describes: one `apply`, the source created before the
 * connection that names it.
 */
export function connectEdits(stage: Stage, destination: string, source: string): Edit[] {
  const edits: Edit[] = [];
  if (stage.port(source) === null) {
    const typeName = portTypeName(stage, source);
    if (typeName !== null) edits.push({ type: "createAttribute", path: source, typeName, custom: false });
  }
  const typeName = stage.port(destination) === null ? portTypeName(stage, destination) : null;
  edits.push(typeName === null ? { type: "connectToSource", path: destination, source } : { type: "connectToSource", path: destination, source, typeName });
  return edits;
}

/** The edit that stops `destination` reading `source`, or (`null`) every source. */
export function disconnectEdits(destination: string, source: string | null): Edit[] {
  return [source === null ? { type: "disconnectSource", path: destination } : { type: "disconnectSource", path: destination, source }];
}

/** The history's label of a connection. */
export function connectLabel(destination: string, source: string): string {
  return `connect ${destination} to ${source}`;
}

/** The history's label of a disconnection. */
export function disconnectLabel(destination: string, source: string | null): string {
  return source === null ? `disconnect ${destination}` : `disconnect ${destination} from ${source}`;
}

/**
 * Why the connection edit just made on `destination` does not show, or
 * `null` when it does: `connected` to `source` (which replaces what it
 * read), or disconnected from it (`null`: from every source). The edit
 * is in the layer edits go to, and a stronger opinion of the port's
 * connections still gives what it reads.
 */
export function unshownConnection(stage: Stage, destination: string, source: string | null, connected: boolean): string | null {
  const port = stage.port(destination);
  const reads = [...(port?.connections ?? []), ...(port?.invalidConnections ?? [])];
  const shows = connected ? reads.length === 1 && reads[0] === source : source === null ? reads.length === 0 : !reads.includes(source);
  if (shows) return null;
  return `${destination}.connect is authored where edits go, and a stronger opinion still gives what it reads: ${reads.length === 0 ? "nothing" : reads.join(", ")}`;
}

/** `base`, or with the first number after it that makes it a name `taken` does not have. */
export function uniqueName(base: string, taken: ReadonlySet<string>): string {
  if (!taken.has(base)) return base;
  for (let number = 1; ; number++) {
    const name = `${base}${number}`;
    if (!taken.has(name)) return name;
  }
}

/** The path a new shader of `definition` gets under `container` on `stage`: its name made unique among the container's children. */
export function newShaderPath(stage: Stage, container: string, definition: ShaderDefinition): string {
  const taken = new Set(stage.children(container).map((child) => child.slice(child.lastIndexOf("/") + 1)));
  const name = uniqueName(definition.primName, taken);
  return container === "/" ? `/${name}` : `${container}/${name}`;
}

/** The edits that define a shader of the id `id` (`info:id`) at `path`, placed at `position` (authored units) when given. */
export function addShaderEdits(path: string, id: string, position: readonly [number, number] | null): Edit[] {
  const edits: Edit[] = [
    { type: "definePrim", path, typeName: "Shader" },
    { type: "setValue", path: `${path}.info:id`, value: { type: "token", value: id } },
  ];
  if (position !== null) {
    edits.push({
      type: "createAttribute",
      path: `${path}.${NODE_POSITION}`,
      typeName: "float2",
      variability: "uniform",
      custom: false,
      value: { type: "float2", value: [position[0], position[1]] },
    });
  }
  return edits;
}

/** A point in the drawing (SVG units, x right, y down). */
export interface Point {
  readonly x: number;
  readonly y: number;
}

/** Drawing units per authored unit when the network has too few positions to measure it by. */
const DEFAULT_SCALE = 60;

/**
 * The authored position (`ui:nodegraph:node:pos`, x right, y down) that
 * puts a node's centre near `at` in a drawing that follows authored
 * positions: each axis fitted, by least squares, to where the placed
 * shaders' centres are drawn against where they are authored; with fewer
 * than two distinct places on an axis, offset from the nearest node at a
 * default scale. `null` when no node has a position.
 */
export function authoredPlace(network: ShadingNetwork, layout: NetworkLayout, at: Point): [number, number] | null {
  const pairs = layout.nodes.flatMap((placed) => {
    const authored = network.positions.get(placed.node.key);
    return authored === undefined ? [] : [{ drawn: [placed.x + placed.width / 2, placed.y + placed.height / 2] as const, authored }];
  });
  if (pairs.length === 0) return null;
  const axis = (index: 0 | 1, value: number): number => {
    const xs = pairs.map((pair) => pair.drawn[index]);
    const ys = pairs.map((pair) => pair.authored[index]!);
    const mx = xs.reduce((sum, x) => sum + x, 0) / xs.length;
    const my = ys.reduce((sum, y) => sum + y, 0) / ys.length;
    const sxx = xs.reduce((sum, x) => sum + (x - mx) ** 2, 0);
    const sxy = xs.reduce((sum, x, i) => sum + (x - mx) * (ys[i]! - my), 0);
    if (sxx > 1e-6 && Math.abs(sxy) > 1e-9) return my + (sxy / sxx) * (value - mx);
    const nearest = pairs.reduce((best, pair) => (Math.abs(pair.drawn[index] - value) < Math.abs(best.drawn[index] - value) ? pair : best));
    return nearest.authored[index]! + (value - nearest.drawn[index]) / DEFAULT_SCALE;
  };
  return [axis(0, at.x), axis(1, at.y)];
}

/** What a menu item, when chosen, asks the view to do. */
export type GraphAction =
  | { readonly kind: "connect"; readonly source: string; readonly destination: string }
  | { readonly kind: "disconnect"; readonly destination: string; readonly source: string | null }
  | { readonly kind: "explain"; readonly port: string }
  | { readonly kind: "select"; readonly prim: string }
  | { readonly kind: "delete"; readonly prim: string }
  | { readonly kind: "add"; readonly id: string }
  | { readonly kind: "fold"; readonly node: string; readonly unfold: boolean };

/** An entry of a menu: an item that acts, a submenu, or a heading. */
export interface MenuEntry {
  /** Unique within its menu. */
  readonly id: string;
  readonly label: string;
  /** A second line: what it authors, or why it is not offered. */
  readonly detail?: string;
  /** When set, the entry is shown but cannot be chosen, and `detail` says why. */
  readonly disabled?: boolean;
  readonly destructive?: boolean;
  readonly action?: GraphAction;
  /** A submenu's entries. */
  readonly entries?: readonly MenuEntry[];
}

/** A candidate as a menu item connecting it with `port`. */
function candidateItem(candidate: Candidate, connect: (other: PortRef) => GraphAction, authors: (other: PortRef) => string): MenuEntry {
  const { ref, verdict } = candidate;
  return {
    id: ref.port.path,
    label: `${portName(ref)}${ref.port.typeName === null ? "" : ` (${ref.port.typeName})`}`,
    detail: verdict.kind === "warn" ? verdict.reason : authors(ref),
    action: connect(ref),
  };
}

/** What connecting authors, in words: the destination's `.connect`. */
function connectsText(destination: PortRef): string {
  const namespace = destination.port.kind === "input" ? "inputs" : "outputs";
  return `authors ${destination.node.name}.${namespace}:${destination.port.name}.connect${destination.port.authored ? "" : `, creating the ${namespace === "inputs" ? "input" : "output"}`}`;
}

/** The "Connect to…" submenu of the port `ref`: the ports it could connect with, refused ones left out. */
function connectEntries(network: ShadingNetwork, ref: PortRef): MenuEntry[] {
  const entries: MenuEntry[] = [];
  if (isDestination(network, ref)) {
    for (const candidate of sourcesFor(network, ref)) {
      if (candidate.verdict.kind === "refused") continue;
      entries.push(candidateItem(candidate, (other) => ({ kind: "connect", source: other.port.path, destination: ref.port.path }), () => connectsText(ref)));
    }
  } else if (isSource(network, ref)) {
    for (const candidate of destinationsFor(network, ref)) {
      if (candidate.verdict.kind === "refused") continue;
      entries.push(candidateItem(candidate, (other) => ({ kind: "connect", source: ref.port.path, destination: other.port.path }), (other) => connectsText(other)));
    }
  }
  return entries;
}

/** The disconnect items of a destination port: one per source, or one for its only source. */
function disconnectEntries(ref: PortRef): MenuEntry[] {
  const sources = ref.port.connections;
  const namespace = ref.port.kind === "input" ? "inputs" : "outputs";
  const authors = `authors ${ref.node.name}.${namespace}:${ref.port.name}.connect without it`;
  if (sources.length === 1) return [{ id: "disconnect", label: "Disconnect", detail: authors, action: { kind: "disconnect", destination: ref.port.path, source: sources[0]! } }];
  return sources.map((source) => ({
    id: `disconnect ${source}`,
    label: `Disconnect from ${source.slice(source.lastIndexOf("/") + 1)}`,
    detail: authors,
    action: { kind: "disconnect", destination: ref.port.path, source },
  }));
}

/** The menu of the port at `path`: connect it, disconnect it, explain it. */
export function portMenu(network: ShadingNetwork, path: string): MenuEntry[] {
  const ref = findPort(network, path);
  if (ref === null) return [];
  const entries: MenuEntry[] = [];
  const candidates = connectEntries(network, ref);
  if (isSource(network, ref) || isDestination(network, ref)) {
    entries.push(
      candidates.length === 0
        ? { id: "connect", label: "Connect to…", disabled: true, detail: "nothing in the network can connect to it" }
        : { id: "connect", label: "Connect to…", entries: candidates },
    );
  }
  if (isDestination(network, ref)) entries.push(...disconnectEntries(ref));
  entries.push({ id: "explain", label: "Explain its value", action: { kind: "explain", port: path } });
  return entries;
}

/** The menu of a connection: disconnect it, explain what reads it. */
export function edgeMenu(network: ShadingNetwork, source: string, destination: string): MenuEntry[] {
  const ref = findPort(network, destination);
  const where = ref === null ? destination : `${ref.node.name}.${ref.port.kind === "input" ? "inputs" : "outputs"}:${ref.port.name}`;
  return [
    { id: "disconnect", label: "Disconnect", detail: `authors ${where}.connect without it`, action: { kind: "disconnect", destination, source } },
    { id: "explain", label: "Explain what reads it", action: { kind: "explain", port: destination } },
  ];
}

/** The connections into and out of `node`, as (destination, source) pairs of ports. */
export function connectionsOf(network: ShadingNetwork, node: NetworkNode): { readonly destination: string; readonly source: string }[] {
  const own = new Set(node.ports.map((port) => port.path));
  return network.edges.filter((edge) => own.has(edge.from) || own.has(edge.to)).map((edge) => ({ destination: edge.to, source: edge.from }));
}

/** The menu of the node keyed `key`: select it, show or fold its ports, delete a shader. */
export function nodeMenu(network: ShadingNetwork, key: string, unfolded: boolean, hidden: number): MenuEntry[] {
  const node = network.nodes.find((each) => each.key === key);
  if (node === undefined) return [];
  const entries: MenuEntry[] = [{ id: "select", label: `Select ${node.name}`, action: { kind: "select", prim: node.path } }];
  if (unfolded) entries.push({ id: "fold", label: "Fold its unconnected ports", action: { kind: "fold", node: key, unfold: false } });
  else if (hidden > 0) entries.push({ id: "fold", label: `Show its ${hidden} unconnected ports`, action: { kind: "fold", node: key, unfold: true } });
  if (node.role === "shader") {
    const count = connectionsOf(network, node).length;
    entries.push({
      id: "delete",
      label: "Delete shader",
      destructive: true,
      detail: `deletes ${node.path}${count === 0 ? "" : ` and the ${count} connection${count === 1 ? "" : "s"} to and from it`}`,
      action: { kind: "delete", prim: node.path },
    });
  }
  return entries;
}

/** The "Add shader" entries: the definitions the editor knows, by family. */
export function addShaderEntries(): MenuEntry[] {
  const families = new Map<string, ShaderDefinition[]>();
  for (const definition of SHADER_DEFINITIONS) families.set(definition.family, [...(families.get(definition.family) ?? []), definition]);
  const item = (definition: ShaderDefinition): MenuEntry => ({ id: definition.id, label: definition.id, detail: definition.summary, action: { kind: "add", id: definition.id } });
  return [...families].map(([family, definitions]) =>
    definitions.length === 1 ? item(definitions[0]!) : { id: family, label: `${family}s`, entries: definitions.map(item) },
  );
}

/** The menu of the drawing's background: add a shader there. */
export function backgroundMenu(): MenuEntry[] {
  return [{ id: "add", label: "Add shader", entries: addShaderEntries() }];
}

/**
 * The menu a drop of `source` on the node keyed `node` opens: which of the
 * node's inputs reads it, refused ones shown with why.
 */
export function dropMenu(network: ShadingNetwork, source: string, node: string): MenuEntry[] {
  const ref = findPort(network, source);
  if (ref === null) return [];
  return destinationsFor(network, ref, node).map((candidate) => ({
    id: candidate.ref.port.path,
    label: `${candidate.ref.port.name}${candidate.ref.port.typeName === null ? "" : ` (${candidate.ref.port.typeName})`}`,
    detail: candidate.verdict.kind === "match" ? connectsText(candidate.ref) : candidate.verdict.reason,
    disabled: candidate.verdict.kind === "refused",
    action: { kind: "connect", source, destination: candidate.ref.port.path },
  }));
}

/** The menu's title for a drop on `node`. */
export function dropTitle(network: ShadingNetwork, source: string, node: string): string {
  const ref = findPort(network, source);
  const target = network.nodes.find((each) => each.key === node);
  return `Connect ${ref === null ? source : portName(ref)} to ${target === undefined ? node : nodeName(target)}`;
}
