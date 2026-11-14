// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What may connect to what in a shading network, as the shading view
// offers it, and what is wrong with the connections there are.
//
// Ends. A connection runs from a source to a destination
// (`UsdShadeConnectableAPI::ConnectToSource`, authored on the
// destination). A shader's outputs are sources and its inputs
// destinations; a container's (material's, node graph's) inputs are
// sources inside it, its outputs destinations inside it; a node graph
// nested in the network is also read from outside, so its inputs are
// destinations and its outputs sources there.
//
// Verdicts. USD is permissive: `ConnectToSource` authors any connection
// between shading attributes, whatever their types
// (`UsdShadeConnectableAPI::CanConnect` checks only which kinds of
// attribute meet, and encapsulation for containers). So the view refuses
// only what cannot mean anything (a port reading itself, a connection
// already there, one that closes a cycle) and warns of what USD takes but
// a renderer may not read as meant: types of another shape, and a
// connection across a container's boundary other than through its
// interface (`encapsulation`). The stage refuses what it refuses, and the
// view says why.
//
// Issues. The stage reports a connection target that names no shading
// attribute (`ShaderPort.invalidConnections`, and
// `Stage.connectionSourceInfo` says what is missing), and what it meets
// following connections through node graphs
// (`UsdShadeUtils::GetValueProducingAttributes`: a cycle, a source that
// is a shader's input). A cycle through shaders, which that walk stops
// short of, is found here from the network's edges.
import type { Stage } from "@forest-rs/layerstack";
import { type NetworkEdge, type NetworkNode, type NetworkPort, parentPath, primOf, type ShadingNetwork } from "./network.ts";

/** What connecting a source to a destination would be. */
export type Verdict =
  /** Types of one shape: the connection reads as meant. */
  | { readonly kind: "match" }
  /** USD connects it; the reason says what a renderer may make of it. */
  | { readonly kind: "warn"; readonly reason: string }
  /** The view does not offer it, and says why. */
  | { readonly kind: "refused"; readonly reason: string };

/** A port and its node. */
export interface PortRef {
  readonly node: NetworkNode;
  readonly port: NetworkPort;
}

/** The port at `path` and its node, or `null`. */
export function findPort(network: ShadingNetwork, path: string): PortRef | null {
  for (const node of network.nodes) {
    const port = node.ports.find((each) => each.path === path);
    if (port !== undefined) return { node, port };
  }
  return null;
}

/** Whether a connection may start at `ref`: something reads it. */
export function isSource(network: ShadingNetwork, { node, port }: PortRef): boolean {
  if (node.role === "shader") return port.kind === "output";
  if (node.role === "inputs") return true;
  // A container's outputs are what reads its inside; a nested node graph's feed the network.
  return node.path !== network.root;
}

/** Whether a connection may end at `ref`: it reads a source. */
export function isDestination(network: ShadingNetwork, { node, port }: PortRef): boolean {
  if (node.role === "shader") return port.kind === "input";
  if (node.role === "outputs") return true;
  // The network's own interface is read from outside it; a nested node graph's from the network.
  return node.path !== network.root;
}

/**
 * The shape of the values of a type: its role and precision dropped
 * (`color3f`, `normal3d`, `double3` are `float3`; `texCoord2f` is
 * `float2`); other types are themselves.
 */
export function valueShape(typeName: string): string {
  const role = /^(color|point|normal|vector|texCoord)([234])[fdh]$/.exec(typeName);
  if (role !== null) return `float${role[2]}`;
  const float = /^(float|double|half)([234]?)$/.exec(typeName);
  if (float !== null) return `float${float[2]}`;
  return typeName;
}

/** The kinds of value a port's type holds, which the drawing marks by shape as well as colour. */
export type TypeFamily = "color" | "number" | "vector" | "text" | "bool" | "token" | "other";

/** The kind of value `typeName` holds: a colour, a number, a vector (or matrix), a file or text, a switch, a token (a terminal's). */
export function typeFamily(typeName: string | null): TypeFamily {
  const type = typeName ?? "";
  if (type.startsWith("color")) return "color";
  if (/^(float|double|half|u?int|u?int64|uchar)$/.test(type)) return "number";
  if (/^(float|double|half|int)[234]$|^(vector|normal|point|texCoord)|^(matrix|quat)/.test(type)) return "vector";
  if (type === "asset" || type === "string") return "text";
  if (type === "bool") return "bool";
  if (type === "token") return "token";
  return "other";
}

/** How a node is named in a reason: its prim's name, and which side for a container. */
export function nodeName(node: NetworkNode): string {
  return node.role === "shader" ? node.name : `${node.name} ${node.role}`;
}

/** A port as a reason names it: `Texture.rgb`. */
export function portName({ node, port }: PortRef): string {
  return `${node.name}.${port.name}`;
}

/**
 * What a port's value is one with, following connections: a shader is one
 * vertex (each of its outputs may read each of its inputs), keyed by its
 * node; a container's boundary port is its own (it passes the one value
 * it reads through, whatever the container's other ports read), keyed by
 * its path.
 */
function vertexOf(node: NetworkNode, port: NetworkPort): string {
  return node.role === "shader" ? node.key : port.path;
}

/** The connections of a network between vertices ({@link vertexOf}): each port's vertex, each vertex's consumers and its name in a reason. */
interface Reads {
  readonly vertex: ReadonlyMap<string, string>;
  readonly consumers: ReadonlyMap<string, ReadonlySet<string>>;
  readonly names: ReadonlyMap<string, string>;
}

function readsOf(network: ShadingNetwork): Reads {
  const vertex = new Map<string, string>();
  const consumers = new Map<string, Set<string>>();
  const names = new Map<string, string>();
  for (const node of network.nodes) {
    for (const port of node.ports) {
      const key = vertexOf(node, port);
      vertex.set(port.path, key);
      consumers.set(key, consumers.get(key) ?? new Set());
      names.set(key, node.role === "shader" ? node.name : portName({ node, port }));
    }
  }
  for (const edge of network.edges) {
    const from = vertex.get(edge.from);
    const to = vertex.get(edge.to);
    if (from !== undefined && to !== undefined) consumers.get(from)!.add(to);
  }
  return { vertex, consumers, names };
}

/** The vertices on a path of connections from `from` to `to` (both included), or `null` when none leads there. */
function pathBetween(reads: Reads, from: string, to: string): string[] | null {
  const previous = new Map<string, string | null>([[from, null]]);
  const queue = [from];
  while (queue.length > 0) {
    const at = queue.shift()!;
    if (at === to) {
      const path: string[] = [];
      for (let step: string | null = at; step !== null; step = previous.get(step) ?? null) path.unshift(step);
      return path;
    }
    for (const next of reads.consumers.get(at) ?? []) {
      if (previous.has(next)) continue;
      previous.set(next, at);
      queue.push(next);
    }
  }
  return null;
}

/** The names of the vertices `keys`, joined by arrows. */
function chain(reads: Reads, keys: readonly string[]): string {
  return keys.map((key) => reads.names.get(key)!).join(" → ");
}

/**
 * Why USD's encapsulation does not take `destination` reading `source`,
 * or `null` when it does, as `UsdShadeConnectableAPIBehavior` decides it
 * for shaders, node graphs and materials (`_CanConnectInputToSource`,
 * `_CanConnectOutputToSource`):
 *
 * - an input reads an output of a prim beside its own (in the same
 *   container), or an input of the container its prim is directly in;
 * - a container's output reads an output of a prim directly inside it,
 *   or, a node graph's (not a material's), one of its own inputs.
 */
export function encapsulation(source: PortRef, destination: PortRef): string | null {
  const from = source.node.path;
  const to = destination.node.path;
  if (destination.port.kind === "input") {
    if (source.port.kind === "input") {
      return parentPath(to) === from ? null : `${destination.node.name} is not directly inside ${source.node.name}, whose input it would read`;
    }
    return parentPath(to) === parentPath(from) ? null : `${destination.node.name} and ${source.node.name} are not in the same container`;
  }
  if (source.port.kind === "input") {
    if (from !== to) return `${destination.node.name}'s output passes through an input of its own, not ${source.node.name}'s`;
    return destination.node.detail === "Material" ? "a material's output does not pass one of its inputs through" : null;
  }
  return parentPath(from) === to ? null : `${destination.node.name}'s outputs read what is directly inside it, and ${source.node.name} is not`;
}

/** What connecting `source` to `destination` would be, as the header of this file describes. */
export function judge(network: ShadingNetwork, source: PortRef, destination: PortRef): Verdict {
  if (!isSource(network, source)) return { kind: "refused", reason: `${portName(source)} is not read by anything: connect from an output` };
  if (!isDestination(network, destination)) return { kind: "refused", reason: `${portName(destination)} reads nothing: connect to an input` };
  if (source.port.path === destination.port.path) return { kind: "refused", reason: "a port cannot read itself" };
  if (destination.port.connections.includes(source.port.path)) return { kind: "refused", reason: `${portName(destination)} already reads ${portName(source)}` };
  if (source.node.role === "shader" && source.node.key === destination.node.key) {
    return { kind: "refused", reason: `${source.node.name} would read its own output: a cycle` };
  }
  // A path from what the destination feeds back to what the source reads closes a cycle: through a
  // shader by any of its ports, through a container's boundary by that port alone.
  const reads = readsOf(network);
  const tail = vertexOf(source.node, source.port);
  const head = vertexOf(destination.node, destination.port);
  const back = pathBetween(reads, head, tail);
  if (back !== null) return { kind: "refused", reason: `it would make a cycle: ${chain(reads, [...back, head])}` };
  const outside = encapsulation(source, destination);
  if (outside !== null) return { kind: "warn", reason: `${outside}: UsdShadeConnectableAPI::CanConnect does not take it, though ConnectToSource authors it` };
  const from = source.port.typeName;
  const to = destination.port.typeName;
  if (from !== null && to !== null && valueShape(from) !== valueShape(to)) {
    return { kind: "warn", reason: `a ${from} into a ${to}: USD connects it, and the renderer decides what it reads` };
  }
  return { kind: "match" };
}

/** A port a connection may be made with, and what it would be. */
export interface Candidate {
  readonly ref: PortRef;
  readonly verdict: Verdict;
}

/** Matches first, then warnings, then refusals; in network order within each. */
function ranked(candidates: Candidate[]): Candidate[] {
  const rank = (verdict: Verdict) => (verdict.kind === "match" ? 0 : verdict.kind === "warn" ? 1 : 2);
  return candidates.map((candidate, index) => ({ candidate, index })).sort((a, b) => rank(a.candidate.verdict) - rank(b.candidate.verdict) || a.index - b.index).map(({ candidate }) => candidate);
}

/** Every source in `network` that `destination` could read, with its verdict, the best first. */
export function sourcesFor(network: ShadingNetwork, destination: PortRef): Candidate[] {
  const found: Candidate[] = [];
  for (const node of network.nodes) {
    for (const port of node.ports) {
      const ref = { node, port };
      if (isSource(network, ref)) found.push({ ref, verdict: judge(network, ref, destination) });
    }
  }
  return ranked(found);
}

/** Every destination in `network` that could read `source`, with its verdict, the best first; with `node`, that node's only. */
export function destinationsFor(network: ShadingNetwork, source: PortRef, node?: string): Candidate[] {
  const found: Candidate[] = [];
  for (const each of network.nodes) {
    if (node !== undefined && each.key !== node) continue;
    for (const port of each.ports) {
      const ref = { node: each, port };
      if (isDestination(network, ref)) found.push({ ref, verdict: judge(network, source, ref) });
    }
  }
  return ranked(found);
}

/** An edge's key: its source and destination ports. */
export function edgeKey(edge: NetworkEdge): string {
  return `${edge.from} -> ${edge.to}`;
}

/**
 * The edges on a cycle of connections, each with the cycle named (`it is
 * on a cycle: Surface → Texture → Surface`), by {@link edgeKey}: an edge
 * from a shader to itself, or one whose destination leads back to its
 * source. A container's boundary is followed port by port: two of a node
 * graph's ports that read nothing of each other are not one node.
 */
export function cycleEdges(network: ShadingNetwork): Map<string, string> {
  const reads = readsOf(network);
  const cycles = new Map<string, string>();
  for (const edge of network.edges) {
    const from = reads.vertex.get(edge.from);
    const to = reads.vertex.get(edge.to);
    if (from === undefined || to === undefined) continue;
    if (from === to) {
      cycles.set(edgeKey(edge), `it is on a cycle: ${chain(reads, [from, from])}`);
      continue;
    }
    const back = pathBetween(reads, to, from);
    if (back !== null) cycles.set(edgeKey(edge), `it is on a cycle: ${chain(reads, [from, ...back])}`);
  }
  return cycles;
}

/** A problem with a port's connection. */
export interface PortIssue {
  /** The source the connection names; `null` for a problem the stage found further along (`at`). */
  readonly source: string | null;
  readonly kind: "invalidTarget" | "cycle" | "nonContainerInput" | "other";
  /** In words. */
  readonly reason: string;
}

/** Why the connection source `source` names nothing, by what `Stage.connectionSourceInfo` finds there. */
export function invalidReason(stage: Stage, source: string): string {
  const info = stage.connectionSourceInfo(source);
  if (info.prim === null) return `no prim is at ${primOf(source)} (deleted, renamed, or never there)`;
  if (info.kind === null) return `${source.slice(source.lastIndexOf(".") + 1)} is neither an input nor an output`;
  if (info.typeName === null) return `${info.prim} has no ${info.kind === "input" ? "inputs" : "outputs"}:${info.name}`;
  return `${source} is not a shading attribute`;
}

/**
 * The problems of the connections of `network`'s ports, by destination
 * port path: targets that name nothing, what the stage meets following
 * them (`ShaderPort.valueSources().issues`), and cycles through shaders
 * ({@link cycleEdges}).
 */
export function readIssues(stage: Stage, network: ShadingNetwork): Map<string, PortIssue[]> {
  const issues = new Map<string, PortIssue[]>();
  const add = (port: string, issue: PortIssue) => {
    const list = issues.get(port) ?? [];
    if (!list.some((each) => each.source === issue.source && each.reason === issue.reason)) list.push(issue);
    issues.set(port, list);
  };
  const owned = new Set(network.nodes.flatMap((node) => node.ports.map((port) => port.path)));
  for (const node of network.nodes) {
    for (const port of node.ports) {
      for (const source of [...port.invalid, ...port.connections.filter((each) => !owned.has(each))]) {
        const info = stage.connectionSourceInfo(source);
        if (info.valid && info.typeName !== null) continue;
        add(port.path, { source, kind: "invalidTarget", reason: `${source} names nothing: ${invalidReason(stage, source)}` });
      }
      if (port.connections.length === 0 || !port.authored) continue;
      for (const issue of stage.port(port.path)?.valueSources().issues ?? []) {
        const at = issue.at ?? port.path;
        const reason =
          issue.kind === "cycle"
            ? `reading its value goes round a cycle at ${at}`
            : issue.kind === "nonContainerInput"
              ? `a connection reads ${at}, a shader's input: a source is an output, or a container's input`
              : issue.kind === "invalidTarget"
                ? `a connection on the way names nothing at ${at}`
                : `the stage reports a problem at ${at}`;
        add(port.path, { source: null, kind: issue.kind, reason });
      }
    }
  }
  for (const [key, reason] of cycleEdges(network)) {
    const [from, to] = key.split(" -> ") as [string, string];
    add(to, { source: from, kind: "cycle", reason });
  }
  return issues;
}
