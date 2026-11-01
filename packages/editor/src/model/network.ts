// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A shading network as the shading view draws it: the nodes of a
// material (or node graph), the ports on each, the connections between
// them, and a left-to-right layered layout.
//
// What a node is:
//
// - Each `Shader` is one node, its inputs on the left and its outputs on
//   the right.
// - A container (the `Material`, a `NodeGraph` within it) is two
//   boundary nodes: its inputs, the interface its shaders read, and its
//   outputs, the terminals it exposes (`outputs:surface`,
//   `outputs:mtlx:surface`). A boundary port is both a destination and a
//   source (a material's output reads a shader's output and is what a
//   renderer reads), so it has an anchor on each side.
//
// Connections are the ports' composed `connectionPaths`
// (`UsdShadeConnectableAPI::GetConnectedSources`); a source outside the
// container (a shared node graph) brings its prim into the network.
//
// The layout ranks each node by its longest path to a sink, so the
// material's outputs sit on the right and every node sits one column
// before the nearest of its consumers' columns that can hold it; nodes in
// a column are ordered and placed by where the ports they connect to are
// (a few barycentre sweeps), so edges run as straight as they can.
//
// Where the network's authors placed their nodes
// (`ui:nodegraph:node:pos`, `UsdUINodeGraphNodeAPI`), the layout follows
// them instead: columns in the order of the authored x, nodes in a column
// in the order of the authored y, stacked so none overlap. The authored
// units are the editing tool's own, so they order the drawing rather than
// scale it.
import type { Stage } from "@forest-rs/layerstack";

/** A port of a node: a shading input or output. */
export interface NetworkPort {
  /** Its property path (`/Looks/Stone/Surface.inputs:roughness`). */
  readonly path: string;
  /** Its name without namespace (`roughness`). */
  readonly name: string;
  readonly kind: "input" | "output";
  readonly typeName: string | null;
  /** The ports it reads from, in composed order. */
  readonly connections: readonly string[];
}

/** A node of the network. */
export interface NetworkNode {
  /** Unique within the network: the prim's path, with `#inputs` or `#outputs` for a boundary. */
  readonly key: string;
  /** The prim it draws. */
  readonly path: string;
  /** The prim's name. */
  readonly name: string;
  /** A shader, or one side of a container's boundary. */
  readonly role: "shader" | "inputs" | "outputs";
  /** A shader's `info:id`; for a boundary, its prim's type. */
  readonly detail: string | null;
  readonly ports: readonly NetworkPort[];
}

/** A connection: `to` reads `from`. */
export interface NetworkEdge {
  readonly from: string;
  readonly to: string;
}

/** A material's (or node graph's) network. */
export interface ShadingNetwork {
  /** The container's path. */
  readonly root: string;
  readonly nodes: readonly NetworkNode[];
  readonly edges: readonly NetworkEdge[];
  /** Each shader node's authored `ui:nodegraph:node:pos` (x right, y down), by node key; empty when none is authored. */
  readonly positions: ReadonlyMap<string, readonly [number, number]>;
}

/** The authored node position attribute (`UsdUINodeGraphNodeAPI`). */
const POSITION = "ui:nodegraph:node:pos";

/**
 * `network` with only the ports some connection touches; a node keeps its
 * header even with none. Returns how many ports were left out.
 */
export function connectedOnly(network: ShadingNetwork): { readonly network: ShadingNetwork; readonly hidden: number } {
  const used = new Set<string>();
  for (const edge of network.edges) {
    used.add(edge.from);
    used.add(edge.to);
  }
  let hidden = 0;
  const nodes = network.nodes.map((node) => {
    const ports = node.ports.filter((port) => used.has(port.path));
    hidden += node.ports.length - ports.length;
    return ports.length === node.ports.length ? node : { ...node, ports };
  });
  return { network: { ...network, nodes }, hidden };
}

/**
 * The prim whose network to show for a selection at `path`: the selection
 * when it is a material or node graph, else its nearest such ancestor; a
 * shader outside any is shown in its parent's network. `null` when the
 * selection is not shading.
 */
export function networkRoot(stage: Stage, path: string): string | null {
  const prim = stage.prim(path);
  if (prim === null) return null;
  for (let at: string | null = path; at !== null && at !== "/"; at = parentPath(at)) {
    if (stage.prim(at)?.isA("NodeGraph")) return at;
  }
  if (prim.isA("Shader")) return parentPath(path);
  return null;
}

/** The material whose surface a selection at `path` previews: the selection or its nearest `Material` ancestor; `null` outside one. */
export function enclosingMaterial(stage: Stage, path: string): string | null {
  for (let at: string | null = path; at !== null && at !== "/"; at = parentPath(at)) {
    if (stage.material(at) !== null) return at;
  }
  return null;
}

/** The parent of a prim path; `null` for the root. */
export function parentPath(path: string): string | null {
  if (path === "/") return null;
  const slash = path.lastIndexOf("/");
  return slash <= 0 ? "/" : path.slice(0, slash);
}

/** The prim path of a property path. */
export function primOf(property: string): string {
  return property.slice(0, property.lastIndexOf("."));
}

/** Reads the network of the container at `root`: its shaders and node graphs, at any depth, and the sources they reach outside it. */
export function readNetwork(stage: Stage, root: string): ShadingNetwork {
  const nodes: NetworkNode[] = [];
  const seen = new Set<string>();
  const add = (path: string, container: boolean) => {
    if (seen.has(path)) return;
    seen.add(path);
    const prim = stage.prim(path);
    if (prim === null) return;
    const name = prim.name || path;
    const shader = container ? null : stage.shader(path);
    if (shader !== null) {
      const ports = [...shader.inputs(), ...shader.outputs()].map(port);
      nodes.push({ key: path, path, name, role: "shader", detail: shader.id, ports });
      return;
    }
    const material = stage.material(path);
    const inputs = (material?.inputs() ?? portsOf(stage, path, "input")).map(port);
    const outputs = (material?.outputs() ?? portsOf(stage, path, "output")).map(port);
    const detail = prim.typeName;
    if (inputs.length > 0) nodes.push({ key: `${path}#inputs`, path, name, role: "inputs", detail, ports: inputs });
    if (outputs.length > 0) nodes.push({ key: `${path}#outputs`, path, name, role: "outputs", detail, ports: outputs });
  };
  add(root, true);
  const walk = (path: string) => {
    for (const child of stage.children(path)) {
      const prim = stage.prim(child);
      if (prim === null) continue;
      if (prim.isA("Shader")) add(child, false);
      else if (prim.isA("NodeGraph")) add(child, true);
      walk(child);
    }
  };
  walk(root);
  // Sources outside the container join the network.
  for (let index = 0; index < nodes.length; index++) {
    for (const port of nodes[index]!.ports) {
      for (const source of port.connections) {
        const prim = primOf(source);
        if (seen.has(prim)) continue;
        const found = stage.prim(prim);
        if (found !== null) add(prim, !found.isA("Shader"));
      }
    }
  }
  const owner = portOwners(nodes);
  const edges: NetworkEdge[] = [];
  for (const node of nodes) {
    for (const port of node.ports) {
      for (const source of port.connections) if (owner.has(source)) edges.push({ from: source, to: port.path });
    }
  }
  const positions = new Map<string, readonly [number, number]>();
  for (const node of nodes) {
    if (node.role !== "shader") continue;
    const value = stage.attribute(`${node.path}.${POSITION}`)?.get() ?? null;
    if (value?.kind === "tuple" && value.value.length === 2) positions.set(node.key, [value.value[0]!, value.value[1]!]);
  }
  return { root, nodes, edges, positions };
}

/** The ports of a prim that is neither a shader nor a material (a node graph): through the stage's port reader. */
function portsOf(stage: Stage, path: string, kind: "input" | "output") {
  const prim = stage.prim(path);
  if (prim === null) return [];
  const prefix = kind === "input" ? "inputs:" : "outputs:";
  return prim
    .attributeNames()
    .filter((name) => name.startsWith(prefix))
    .map((name) => stage.port(`${path}.${name}`))
    .filter((port) => port !== null);
}

function port(source: { path: string; name: string; kind: "input" | "output"; typeName: string | null; connections: readonly string[] }): NetworkPort {
  return { path: source.path, name: source.name, kind: source.kind, typeName: source.typeName, connections: source.connections };
}

/** Each port's node, by the port's path. */
function portOwners(nodes: readonly NetworkNode[]): Map<string, NetworkNode> {
  const owner = new Map<string, NetworkNode>();
  for (const node of nodes) for (const port of node.ports) owner.set(port.path, node);
  return owner;
}

/** Sizes of the drawing, in SVG units (CSS pixels at zoom 1). */
export const GRAPH = {
  /** A node's title: its name and its shader id. */
  header: 34,
  /** One port row. */
  row: 18,
  /** Space below the last row. */
  footer: 6,
  /** Between columns. */
  columnGap: 96,
  /** Between nodes in a column. */
  nodeGap: 18,
  minWidth: 150,
  maxWidth: 300,
  /** The width of a character of a port's or node's name, roughly, at the drawing's 11px. */
  character: 6.4,
} as const;

/** A node placed. */
export interface PlacedNode {
  readonly node: NetworkNode;
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  /** Its column, 0 on the left. */
  readonly column: number;
}

/** An edge placed: from the source port's right anchor to the destination port's left anchor. */
export interface PlacedEdge {
  readonly edge: NetworkEdge;
  readonly from: string;
  readonly to: string;
  readonly x1: number;
  readonly y1: number;
  readonly x2: number;
  readonly y2: number;
}

/** A network laid out. */
export interface NetworkLayout {
  readonly nodes: readonly PlacedNode[];
  readonly edges: readonly PlacedEdge[];
  readonly width: number;
  readonly height: number;
}

/** The y of the centre of a node's `index`th port row, from the node's top. */
export function portOffset(index: number): number {
  return GRAPH.header + index * GRAPH.row + GRAPH.row / 2;
}

/** How wide a node draws: its longest name, within the bounds. */
function nodeWidth(node: NetworkNode): number {
  const title = Math.max(node.name.length, (node.detail?.length ?? 0) + (node.role === "shader" ? 0 : 8));
  const ports = Math.max(0, ...node.ports.map((port) => port.name.length));
  const text = Math.max(title, ports + (node.role === "shader" ? 0 : 2));
  return Math.min(GRAPH.maxWidth, Math.max(GRAPH.minWidth, Math.round(text * GRAPH.character + 28)));
}

/**
 * Lays `network` out left to right, as the header of this file describes;
 * with `authored`, by its authors' node positions when every shader has
 * one.
 */
export function layoutNetwork(network: ShadingNetwork, authored = false): NetworkLayout {
  const nodes = network.nodes;
  const owner = portOwners(nodes);
  const index = new Map(nodes.map((node, at) => [node.key, at]));
  const portIndex = new Map<string, number>();
  for (const node of nodes) node.ports.forEach((port, at) => portIndex.set(port.path, at));
  // Node-level edges, deduplicated, self-loops dropped.
  const consumers = nodes.map(() => new Set<number>());
  const producers = nodes.map(() => new Set<number>());
  for (const edge of network.edges) {
    const from = index.get(owner.get(edge.from)!.key)!;
    const to = index.get(owner.get(edge.to)!.key)!;
    if (from === to) continue;
    consumers[from]!.add(to);
    producers[to]!.add(from);
  }
  // Rank: longest path to a sink; an edge closing a cycle is ignored.
  const rank = new Array<number>(nodes.length).fill(-1);
  const visiting = new Set<number>();
  const rankOf = (at: number): number => {
    if (rank[at]! >= 0) return rank[at]!;
    if (visiting.has(at)) return -1;
    visiting.add(at);
    let best = 0;
    for (const next of consumers[at]!) {
      const r = rankOf(next);
      if (r >= 0) best = Math.max(best, r + 1);
    }
    visiting.delete(at);
    rank[at] = best;
    return best;
  };
  nodes.forEach((_, at) => rankOf(at));
  const follow = authored && hasAuthoredLayout(network);
  const top = Math.max(0, ...rank);
  const columnOf = follow ? authoredColumns(network, nodes) : rank.map((r) => top - r);
  const columns: number[][] = Array.from({ length: Math.max(0, ...columnOf) + 1 }, () => []);
  nodes.forEach((_, at) => columns[columnOf[at]!]!.push(at));
  const width = nodes.map(nodeWidth);
  const height = nodes.map((node) => GRAPH.header + node.ports.length * GRAPH.row + GRAPH.footer);
  const columnX: number[] = [];
  let x = 0;
  for (const column of columns) {
    columnX.push(x);
    x += Math.max(0, ...column.map((at) => width[at]!)) + GRAPH.columnGap;
  }
  // Initial placement: stacked in discovery order.
  const y = new Array<number>(nodes.length).fill(0);
  for (const column of columns) stack(column, y, height, (at) => y[at]!);
  if (follow) {
    // The authored y, scaled to the drawing: one scale for every column, so
    // what the authors lined up stays lined up, the one that keeps a typical
    // pair of neighbours apart (the median, so two nodes authored on one
    // line do not blow it up); then any overlap left is pushed down.
    const authoredY = (at: number) => network.positions.get(nodes[at]!.key)?.[1];
    const needed: number[] = [];
    for (const column of columns) {
      const placed = column.filter((at) => authoredY(at) !== undefined).sort((a, b) => authoredY(a)! - authoredY(b)!);
      for (let i = 1; i < placed.length; i++) {
        const gap = authoredY(placed[i]!)! - authoredY(placed[i - 1]!)!;
        if (gap > 1e-6) needed.push((height[placed[i - 1]!]! + GRAPH.nodeGap) / gap);
      }
    }
    needed.sort((a, b) => a - b);
    const scale = Math.min(400, Math.max(20, needed[Math.floor(needed.length / 2)] ?? 60));
    const ys = nodes.flatMap((_, at) => (authoredY(at) === undefined ? [] : [authoredY(at)!]));
    const top = Math.min(0, ...ys);
    for (const column of columns) {
      // A boundary has no authored place: level with the middle of what it connects to.
      for (const at of column) y[at] = authoredY(at) === undefined ? 0 : (authoredY(at)! - top) * scale;
      column.sort((a, b) => y[a]! - y[b]!);
      let bottom = -Infinity;
      for (const at of column) {
        y[at] = Math.max(y[at]!, bottom + GRAPH.nodeGap);
        bottom = y[at]! + height[at]!;
      }
    }
    // Boundaries (unplaced) sit level with the middle of the nodes they connect to.
    for (const column of columns) {
      for (const at of column) {
        if (authoredY(at) !== undefined) continue;
        const linked = [...consumers[at]!, ...producers[at]!];
        if (linked.length > 0) y[at] = linked.reduce((sum, other) => sum + y[other]! + height[other]! / 2, 0) / linked.length - height[at]! / 2;
      }
    }
  }
  // Sweeps: a node wants each of its ports level with the ports it connects to.
  const wanted = (at: number, toward: "consumers" | "producers"): number | null => {
    let sum = 0;
    let count = 0;
    for (const edge of network.edges) {
      const fromNode = index.get(owner.get(edge.from)!.key)!;
      const toNode = index.get(owner.get(edge.to)!.key)!;
      if (fromNode === toNode) continue;
      if (toward === "consumers" && fromNode === at) {
        sum += y[toNode]! + portOffset(portIndex.get(edge.to)!) - portOffset(portIndex.get(edge.from)!);
        count++;
      } else if (toward === "producers" && toNode === at) {
        sum += y[fromNode]! + portOffset(portIndex.get(edge.from)!) - portOffset(portIndex.get(edge.to)!);
        count++;
      }
    }
    return count === 0 ? null : sum / count;
  };
  for (let sweep = 0; sweep < (follow ? 0 : 4); sweep++) {
    for (let c = columns.length - 2; c >= 0; c--) {
      const column = columns[c]!;
      const want = new Map(column.map((at) => [at, wanted(at, "consumers") ?? y[at]!]));
      stack(column, y, height, (at) => want.get(at)!);
    }
    for (let c = 1; c < columns.length; c++) {
      const column = columns[c]!;
      const want = new Map(column.map((at) => [at, wanted(at, "producers") ?? y[at]!]));
      stack(column, y, height, (at) => want.get(at)!);
    }
  }
  // Shift everything so the drawing starts at 0.
  const minY = Math.min(0, ...y);
  const placed: PlacedNode[] = nodes.map((node, at) => ({
    node,
    x: columnX[columnOf[at]!]!,
    y: y[at]! - minY,
    width: width[at]!,
    height: height[at]!,
    column: columnOf[at]!,
  }));
  const byKey = new Map(placed.map((p) => [p.node.key, p]));
  const edges: PlacedEdge[] = network.edges.map((edge) => {
    const from = byKey.get(owner.get(edge.from)!.key)!;
    const to = byKey.get(owner.get(edge.to)!.key)!;
    return {
      edge,
      from: from.node.key,
      to: to.node.key,
      x1: from.x + from.width,
      y1: from.y + portOffset(portIndex.get(edge.from)!),
      x2: to.x,
      y2: to.y + portOffset(portIndex.get(edge.to)!),
    };
  });
  return {
    nodes: placed,
    edges,
    width: Math.max(0, x - GRAPH.columnGap),
    height: Math.max(0, ...placed.map((p) => p.y + p.height)),
  };
}

/** Whether every shader node of `network` has an authored position. */
export function hasAuthoredLayout(network: ShadingNetwork): boolean {
  const shaders = network.nodes.filter((node) => node.role === "shader");
  return shaders.length > 0 && shaders.every((node) => network.positions.has(node.key));
}

/**
 * Each node's column by its authored x: shaders whose x lie within a unit
 * of each other share a column, in x order; a container's inputs come
 * first and its outputs last.
 */
function authoredColumns(network: ShadingNetwork, nodes: readonly NetworkNode[]): number[] {
  const xs = [...new Set(nodes.flatMap((node) => {
    const at = network.positions.get(node.key);
    return at === undefined ? [] : [at[0]];
  }))].sort((a, b) => a - b);
  // Clusters: a new column wherever x jumps by a unit or more.
  const cluster = new Map<number, number>();
  let column = 0;
  xs.forEach((x, index) => {
    if (index > 0 && x - xs[index - 1]! >= 1) column++;
    cluster.set(x, column);
  });
  const last = column + 2;
  return nodes.map((node) => {
    if (node.role === "inputs") return 0;
    if (node.role === "outputs") return last;
    const at = network.positions.get(node.key);
    return at === undefined ? last - 1 : cluster.get(at[0])! + 1;
  });
}

/**
 * Places a column's nodes in the order of where they want to be, each at
 * its wanted top or just below the one above; then moves the column up by
 * the mean overshoot, so pushing down does not drift the column.
 */
function stack(column: number[], y: number[], height: readonly number[], want: (at: number) => number): void {
  column.sort((a, b) => want(a) + height[a]! / 2 - (want(b) + height[b]! / 2));
  let bottom = -Infinity;
  let overshoot = 0;
  for (const at of column) {
    const top = Math.max(want(at), bottom + GRAPH.nodeGap);
    overshoot += top - want(at);
    y[at] = top;
    bottom = top + height[at]!;
  }
  const shift = column.length === 0 ? 0 : overshoot / column.length;
  for (const at of column) y[at] = y[at]! - shift;
}
