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
// container's output reading a source outside it. The stage refuses what
// it refuses, and the view says why.
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

/** How a node is named in a reason: its prim's name, and which side for a container. */
export function nodeName(node: NetworkNode): string {
  return node.role === "shader" ? node.name : `${node.name} ${node.role}`;
}

/** A port as a reason names it: `Texture.rgb`. */
export function portName({ node, port }: PortRef): string {
  return `${node.name}.${port.name}`;
}

/** Node-level consumers: each node's key, the keys of the nodes its ports feed. */
function consumersOf(network: ShadingNetwork): Map<string, Set<string>> {
  const owner = new Map<string, string>();
  for (const node of network.nodes) for (const port of node.ports) owner.set(port.path, node.key);
  const consumers = new Map<string, Set<string>>(network.nodes.map((node) => [node.key, new Set<string>()]));
  for (const edge of network.edges) {
    const from = owner.get(edge.from);
    const to = owner.get(edge.to);
    if (from !== undefined && to !== undefined) consumers.get(from)!.add(to);
  }
  return consumers;
}

/** The keys of the nodes on a path of connections from `from` to `to` (both included), or `null` when none leads there. */
export function pathBetween(network: ShadingNetwork, from: string, to: string, consumers = consumersOf(network)): string[] | null {
  const previous = new Map<string, string | null>([[from, null]]);
  const queue = [from];
  while (queue.length > 0) {
    const at = queue.shift()!;
    if (at === to) {
      const path: string[] = [];
      for (let step: string | null = at; step !== null; step = previous.get(step) ?? null) path.unshift(step);
      return path;
    }
    for (const next of consumers.get(at) ?? []) {
      if (previous.has(next)) continue;
      previous.set(next, at);
      queue.push(next);
    }
  }
  return null;
}

/** The names of the nodes keyed `keys`, joined by arrows. */
function chain(network: ShadingNetwork, keys: readonly string[]): string {
  const byKey = new Map(network.nodes.map((node) => [node.key, node]));
  return keys.map((key) => nodeName(byKey.get(key)!)).join(" → ");
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
  // A path from the destination's node back to the source's closes a cycle.
  const back = pathBetween(network, destination.node.key, source.node.key);
  if (back !== null && source.node.key !== destination.node.key) {
    return { kind: "refused", reason: `it would make a cycle: ${chain(network, [...back, destination.node.key])}` };
  }
  const from = source.port.typeName;
  const to = destination.port.typeName;
  if (from !== null && to !== null && valueShape(from) !== valueShape(to)) {
    return { kind: "warn", reason: `a ${from} into a ${to}: USD connects any types, and what a renderer reads of it is up to the renderer` };
  }
  if (destination.node.role === "outputs") {
    // A container's output reads a node directly inside it, or passes one of its own inputs through.
    const prim = primOf(source.port.path);
    if (prim !== destination.node.path && parentPath(prim) !== destination.node.path) {
      return { kind: "warn", reason: `${destination.node.name}'s outputs read what is directly inside it (UsdShadeConnectableAPI::CanConnect), and ${source.node.name} is not` };
    }
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
 * The edges on a cycle through the network's nodes, each with the cycle
 * named (`it is on a cycle: Surface → Texture → Surface`), by
 * {@link edgeKey}: an edge from a node to itself, or one whose
 * destination's node leads back to its source's.
 */
export function cycleEdges(network: ShadingNetwork): Map<string, string> {
  const owner = new Map<string, string>();
  for (const node of network.nodes) for (const port of node.ports) owner.set(port.path, node.key);
  const consumers = consumersOf(network);
  const cycles = new Map<string, string>();
  for (const edge of network.edges) {
    const from = owner.get(edge.from);
    const to = owner.get(edge.to);
    if (from === undefined || to === undefined) continue;
    if (from === to) {
      cycles.set(edgeKey(edge), `it is on a cycle: ${chain(network, [from, from])}`);
      continue;
    }
    const back = pathBetween(network, to, from, consumers);
    if (back !== null) cycles.set(edgeKey(edge), `it is on a cycle: ${chain(network, [from, ...back])}`);
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
