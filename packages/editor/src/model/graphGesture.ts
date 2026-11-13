// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The shading view's gestures and keys, apart from React and the DOM so
// they are tested in Node; the view only turns pointer and key events
// into these inputs and carries out what they return.
//
// Connecting by dragging: a press on a source's row, a move past a few
// pixels starts the drag (less is a click on the row), each move says
// what is under the pointer and how a drop there would be judged, and the
// release connects (on a destination port), asks which input (on a node,
// whose ports may be folded away), says why not (on a refused one) or
// does nothing; Escape cancels.
//
// Moving the keyboard's focus: Up and Down go through a node's header and
// its rows, on into the node above or below; Left and Right follow a
// row's connection (an input to what it reads, an output to what reads
// it) or go to the nearest node in that direction.
import { destinationsFor, findPort, isDestination, judge, nodeName, type Verdict } from "./connections.ts";
import { type NetworkLayout, type PlacedNode, portOffset, type ShadingNetwork } from "./network.ts";
import type { Point } from "./networkEdits.ts";

/** What a drag is over: a port that could read the source, or a node (which of its inputs is asked on the drop). */
export type DropTarget = { readonly kind: "port"; readonly port: string } | { readonly kind: "node"; readonly node: string };

/** Where a connecting drag is. */
export type Gesture =
  | { readonly kind: "idle" }
  /** Pressed on a source's row, not yet moved far enough to be a drag. */
  | { readonly kind: "pressed"; readonly source: string; readonly start: Point }
  /** Dragging a connection from `source`; `over` and its verdict, what a release would drop on. */
  | {
      readonly kind: "dragging";
      readonly source: string;
      readonly pointer: Point;
      readonly over: DropTarget | null;
      readonly verdict: Verdict | null;
    };

/** What the view tells the gesture. */
export type GestureInput =
  | { readonly kind: "press"; readonly source: string; readonly at: Point }
  | { readonly kind: "move"; readonly at: Point; readonly over: DropTarget | null }
  | { readonly kind: "release" }
  | { readonly kind: "cancel" };

/** What a step of the gesture asks the view to do. */
export type GestureOutcome =
  | { readonly kind: "none" }
  /** Pressed and released in place: what a click on the row does. */
  | { readonly kind: "click"; readonly port: string }
  | { readonly kind: "connect"; readonly source: string; readonly destination: string }
  /** Dropped on a node: ask which of its inputs reads the source. */
  | { readonly kind: "choose"; readonly source: string; readonly node: string }
  /** Dropped where the connection is refused. */
  | { readonly kind: "refused"; readonly reason: string };

/**
 * What a drag is over, from the row (`port`) and node (`node`) under the
 * pointer: the row when it could read a source, else the node, else
 * nothing.
 */
export function dropTargetAt(network: ShadingNetwork, hit: { readonly port: string | null; readonly node: string | null }): DropTarget | null {
  const ref = hit.port === null ? null : findPort(network, hit.port);
  if (ref !== null && isDestination(network, ref)) return { kind: "port", port: ref.port.path };
  return hit.node === null ? null : { kind: "node", node: hit.node };
}

/**
 * How a drop of `source` on `target` would be ({@link judge}); on a node,
 * a match when one of its inputs could read it, else refused saying so;
 * `null` when nothing there reads (a source's own node's outputs, a
 * container's interface).
 */
export function judgeDrop(network: ShadingNetwork, source: string, target: DropTarget): Verdict | null {
  const from = findPort(network, source);
  if (from === null) return null;
  if (target.kind === "port") {
    const to = findPort(network, target.port);
    return to === null || !isDestination(network, to) ? null : judge(network, from, to);
  }
  const candidates = destinationsFor(network, from, target.node);
  if (candidates.length === 0) return null;
  if (candidates.some((candidate) => candidate.verdict.kind !== "refused")) return { kind: "match" };
  const node = network.nodes.find((each) => each.key === target.node)!;
  const reasons = [...new Set(candidates.flatMap(({ verdict }) => (verdict.kind === "refused" ? [verdict.reason] : [])))];
  return { kind: "refused", reason: reasons.length === 1 ? reasons[0]! : `no input of ${nodeName(node)} can read it` };
}

/** How far, in screen pixels, a press moves before it is a drag. */
export const DRAG_THRESHOLD = 4;

const IDLE: Gesture = { kind: "idle" };
const NONE: GestureOutcome = { kind: "none" };

/**
 * The gesture after `input`, and what it asks for. `judge` says how a
 * drop of `source` on a target would be (`null`: no drop there).
 */
export function stepGesture(
  state: Gesture,
  input: GestureInput,
  judge: (source: string, target: DropTarget) => Verdict | null,
): { readonly state: Gesture; readonly outcome: GestureOutcome } {
  switch (input.kind) {
    case "press":
      return { state: { kind: "pressed", source: input.source, start: input.at }, outcome: NONE };
    case "cancel":
      return { state: IDLE, outcome: NONE };
    case "move": {
      if (state.kind === "idle") return { state, outcome: NONE };
      if (state.kind === "pressed" && Math.hypot(input.at.x - state.start.x, input.at.y - state.start.y) < DRAG_THRESHOLD) return { state, outcome: NONE };
      const verdict = input.over === null ? null : judge(state.source, input.over);
      const over = verdict === null ? null : input.over;
      return { state: { kind: "dragging", source: state.source, pointer: input.at, over, verdict }, outcome: NONE };
    }
    case "release": {
      if (state.kind === "pressed") return { state: IDLE, outcome: { kind: "click", port: state.source } };
      if (state.kind !== "dragging" || state.over === null || state.verdict === null) return { state: IDLE, outcome: NONE };
      if (state.verdict.kind === "refused") return { state: IDLE, outcome: { kind: "refused", reason: state.verdict.reason } };
      if (state.over.kind === "node") return { state: IDLE, outcome: { kind: "choose", source: state.source, node: state.over.node } };
      return { state: IDLE, outcome: { kind: "connect", source: state.source, destination: state.over.port } };
    }
  }
}

/** Where the keyboard's focus is in the drawing: a node's header (`port` `null`) or one of its rows. */
export interface GraphFocus {
  readonly node: string;
  readonly port: string | null;
}

/** The keys that move the focus. */
export type FocusKey = "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" | "Home" | "End";

/** The node's centre, on an axis. */
const middle = (placed: PlacedNode, axis: "x" | "y") => (axis === "x" ? placed.x + placed.width / 2 : placed.y + placed.height / 2);

/** The y of the focus's row (or its node's header). */
function focusY(placed: PlacedNode, port: string | null): number {
  const index = port === null ? -1 : placed.node.ports.findIndex((each) => each.path === port);
  return index < 0 ? placed.y : placed.y + portOffset(index);
}

/** The node whose columns overlap `placed`'s and is nearest above (`-1`) or below (`1`) it; `null` when none is. */
function stacked(layout: NetworkLayout, placed: PlacedNode, direction: -1 | 1): PlacedNode | null {
  let best: PlacedNode | null = null;
  for (const other of layout.nodes) {
    if (other === placed || other.x >= placed.x + placed.width || placed.x >= other.x + other.width) continue;
    const ahead = direction === 1 ? other.y > placed.y : other.y < placed.y;
    if (!ahead) continue;
    if (best === null || (direction === 1 ? other.y < best.y : other.y > best.y)) best = other;
  }
  return best;
}

/** The node nearest the focus to the left (`-1`) or right (`1`): the nearest column that way, then the nearest in it by height. */
function beside(layout: NetworkLayout, placed: PlacedNode, y: number, direction: -1 | 1): PlacedNode | null {
  const x = middle(placed, "x");
  const that = layout.nodes.filter((other) => (direction === 1 ? middle(other, "x") > x + 1 : middle(other, "x") < x - 1));
  if (that.length === 0) return null;
  const column = direction === 1 ? Math.min(...that.map((other) => other.x)) : Math.max(...that.map((other) => other.x));
  const near = that.filter((other) => Math.abs(other.x - column) < 1);
  return near.reduce((best, other) => (Math.abs(middle(other, "y") - y) < Math.abs(middle(best, "y") - y) ? other : best));
}

/** The focus `key` moves `focus` to in `layout`, as the header of this file describes; the same focus when it cannot move. */
export function moveFocus(layout: NetworkLayout, focus: GraphFocus, key: FocusKey): GraphFocus {
  const byKey = new Map(layout.nodes.map((placed) => [placed.node.key, placed]));
  const placed = byKey.get(focus.node);
  if (placed === undefined) return initialFocus(layout, null);
  const ports = placed.node.ports;
  const index = focus.port === null ? -1 : ports.findIndex((each) => each.path === focus.port);
  switch (key) {
    case "Home":
      return { node: placed.node.key, port: null };
    case "End":
      return { node: placed.node.key, port: ports.at(-1)?.path ?? null };
    case "ArrowDown": {
      if (index + 1 < ports.length) return { node: placed.node.key, port: ports[index + 1]!.path };
      const below = stacked(layout, placed, 1);
      return below === null ? focus : { node: below.node.key, port: null };
    }
    case "ArrowUp": {
      if (index > 0) return { node: placed.node.key, port: ports[index - 1]!.path };
      if (index === 0) return { node: placed.node.key, port: null };
      const above = stacked(layout, placed, -1);
      return above === null ? focus : { node: above.node.key, port: above.node.ports.at(-1)?.path ?? null };
    }
    case "ArrowLeft":
    case "ArrowRight": {
      const direction = key === "ArrowRight" ? 1 : -1;
      // A row's connection, followed.
      if (focus.port !== null) {
        const edge = layout.edges.find((each) => (direction === 1 ? each.edge.from === focus.port : each.edge.to === focus.port));
        if (edge !== undefined) return direction === 1 ? { node: edge.to, port: edge.edge.to } : { node: edge.from, port: edge.edge.from };
      }
      const next = beside(layout, placed, focusY(placed, focus.port), direction);
      return next === null ? focus : { node: next.node.key, port: null };
    }
  }
}

/** Where the focus starts: on the node keyed `preferred` when it is drawn, else the leftmost, topmost node; `null` with no nodes. */
export function initialFocus(layout: NetworkLayout, preferred: string | null): GraphFocus {
  const found = layout.nodes.find((placed) => placed.node.key === preferred);
  if (found !== undefined) return { node: found.node.key, port: null };
  const first = [...layout.nodes].sort((a, b) => a.x - b.x || a.y - b.y)[0];
  return { node: first?.node.key ?? "", port: null };
}

/** The modifiers and key of a keyboard event. */
export interface GraphKeyPress {
  readonly key: string;
  readonly metaKey: boolean;
  readonly ctrlKey: boolean;
  readonly altKey: boolean;
  readonly shiftKey: boolean;
}

/** What a key on the focused node, row or connection does. */
export type GraphKey =
  | { readonly kind: "move"; readonly key: FocusKey }
  /** Delete the focused shader, or disconnect the focused row or connection. */
  | { readonly kind: "delete" }
  /** Its menu (the context menu key, Shift+F10). */
  | { readonly kind: "menu" }
  /** Enter or Space: select the node, explain the row. */
  | { readonly kind: "activate" };

/** What `press` does on the drawing, or `null` to leave it to the page. */
export function graphKey(press: GraphKeyPress): GraphKey | null {
  // A Mac keyboard has no Delete key: ⌘⌫ deletes there, as in the prim tree.
  if (press.key === "Backspace" && press.metaKey && !press.ctrlKey && !press.altKey && !press.shiftKey) return { kind: "delete" };
  if ((press.key === "F10" && press.shiftKey) || press.key === "ContextMenu") return { kind: "menu" };
  if (press.metaKey || press.ctrlKey || press.altKey || press.shiftKey) return null;
  switch (press.key) {
    case "ArrowUp":
    case "ArrowDown":
    case "ArrowLeft":
    case "ArrowRight":
    case "Home":
    case "End":
      return { kind: "move", key: press.key };
    case "Delete":
      return { kind: "delete" };
    case "Enter":
    case " ":
      return { kind: "activate" };
    default:
      return null;
  }
}
