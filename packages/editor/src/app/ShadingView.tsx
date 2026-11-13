// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The shading view: the network of the selected material (or of the
// material or node graph around a selected shader) as a node graph, left
// to right, ending in the material's outputs, and edited there. Ports are
// rows; a connection is a curve from its source's row to the row that
// reads it. Drag the background to pan, scroll to zoom; click a node to
// select its prim, a row to explain its value. Ports no connection
// touches fold away until asked for (all, or a node's), and a network
// whose authors placed its nodes is drawn in their arrangement unless the
// automatic layout is chosen.
//
// Editing, each one action where edits go:
// - drag from an output's row to an input's to connect them (the inputs
//   that could read it lit, those that could not dimmed with why); drop
//   on a node to choose which of its inputs;
// - select a curve and press Delete (⌘⌫), or use the context menu of the
//   curve or the input, to disconnect;
// - an unconnected input's row edits its value in place;
// - "Add shader" (toolbar, or the background's context menu) adds a
//   shader where it was asked for; a shader's menu deletes it.
// The keyboard reaches all of it: the arrow keys move between nodes and
// rows, Shift+F10 opens the focused row's or node's menu ("Connect to…",
// "Disconnect"), Enter selects or explains, Delete deletes or
// disconnects. The drawing keeps its nodes where they were across an
// edit (`model/networkPlaces.ts`); the gestures and keys are pure models
// (`model/graphGesture.ts`) this file only feeds events into.
import { useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { edgeKey, findPort, isDestination, isSource, nodeName, type PortIssue, portName, readIssues, type Verdict } from "../model/connections.ts";
import {
  dropTargetAt,
  type DropTarget,
  type Gesture,
  type GestureInput,
  type GestureOutcome,
  type GraphFocus,
  graphKey,
  initialFocus,
  judgeDrop,
  moveFocus,
  stepGesture,
} from "../model/graphGesture.ts";
import {
  connectedOnly,
  GRAPH,
  hasAuthoredLayout,
  layoutNetwork,
  type NetworkLayout,
  networkRoot,
  type PlacedNode,
  portOffset,
  primOf,
  readNetwork,
  type ShadingNetwork,
  showsValue,
} from "../model/network.ts";
import {
  addShaderEntries,
  authoredPlace,
  backgroundMenu,
  connectionsOf,
  dropMenu,
  dropTitle,
  edgeMenu,
  type GraphAction,
  newShaderPath,
  nodeMenu,
  type Point,
  portMenu,
} from "../model/networkEdits.ts";
import { keepPlaces, type Place } from "../model/networkPlaces.ts";
import { shaderDefinition } from "../model/shaderDefinitions.ts";
import type { EditorStore } from "../store.ts";
import { useEditorStore, useRevision } from "./context.ts";
import { EdgeView, GraphMenu, Legend, MenuEntries, type NodeState, NodeView, type OpenMenu, type RowDrag } from "./ShadingGraphParts.tsx";
import { targetText } from "./TargetPicker.tsx";

export function ShadingView() {
  const store = useEditorStore();
  useRevision(store, "selection", "composition");
  const stage = store.session?.stage ?? null;
  const root = stage === null || store.selection === null ? null : networkRoot(stage, store.selection);
  if (stage === null || root === null) {
    return <p className="p-3 text-xs text-muted-foreground">Select a material, a node graph or a shader to see its network.</p>;
  }
  return <Network key={root} store={store} root={root} />;
}

/** Where edits go, in words, for what an action says it authored. */
function editsGo(store: EditorStore): string {
  const session = store.session;
  const target = session?.target ?? session?.stage.editLayer ?? null;
  return target === null ? "the edit layer" : targetText(target);
}

/** The network at `root`: read after each recomposition, folded, laid out, and kept still across edits. */
function Network({ store, root }: { store: EditorStore; root: string }) {
  const stage = store.session!.stage;
  // The network is read again only after a recomposition.
  const recompositions = store.session?.changeLog.length ?? 0;
  const [allPorts, setAllPorts] = useState(false);
  const [automatic, setAutomatic] = useState(false);
  const [unfolded, setUnfolded] = useState<ReadonlySet<string>>(new Set());
  const network = useMemo(() => readNetwork(stage, root), [stage, root, recompositions]);
  const issues = useMemo(() => readIssues(stage, network), [stage, network]);
  const varying = useMemo(() => {
    const paths = network.nodes.flatMap((node) => node.ports.filter((port) => port.authored && showsValue(node, port)).map((port) => port.path));
    return new Set(stage.valuesMightBeTimeVarying(paths).flatMap((varies, index) => (varies ? [paths[index]!] : [])));
  }, [stage, network]);
  const authored = hasAuthoredLayout(network);
  const mode = `${allPorts} ${automatic}`;
  const { fresh, hidden, hiddenBy } = useMemo(() => {
    const shown = allPorts ? { network, hidden: 0 } : connectedOnly(network, unfolded);
    const counts = new Map(network.nodes.map((node, index) => [node.key, node.ports.length - shown.network.nodes[index]!.ports.length]));
    return { fresh: layoutNetwork(shown.network, authored && !automatic), hidden: shown.hidden, hiddenBy: counts };
  }, [network, allPorts, automatic, authored, unfolded]);
  // The drawing as last shown, and where new nodes were asked for: what keeps the nodes still across an edit.
  const previous = useRef<{ mode: string; layout: NetworkLayout } | null>(null);
  const wanted = useRef(new Map<string, Place>());
  const layout = useMemo(() => keepPlaces(previous.current?.mode === mode ? previous.current.layout : null, fresh, wanted.current), [fresh, mode]);
  useEffect(() => {
    previous.current = { mode, layout };
    for (const placed of layout.nodes) wanted.current.delete(placed.node.key);
  }, [layout, mode]);

  if (network.nodes.length === 0) {
    return (
      <div className="flex h-full flex-col items-start gap-2 p-3 text-xs text-muted-foreground">
        <p>{root} has no shaders, inputs or outputs.</p>
        <AddShaderButton onAdd={(id) => add(id, { x: 0, y: 0 })} />
      </div>
    );
  }

  /** Adds a shader of `id` with its top left at `at`; returns its path, or why the stage refused it. */
  function add(id: string, at: Point): { path: string } | { refusal: string } {
    const definition = shaderDefinition(id)!;
    const path = newShaderPath(stage, root, definition);
    // A network whose authors place its nodes gets the new one placed too, in their units.
    const position = authored ? authoredPlace(network, layout, { x: at.x + GRAPH.minWidth / 2, y: at.y + GRAPH.header }) : null;
    wanted.current.set(path, at);
    const refusal = store.addShader(path, id, position);
    if (refusal !== undefined) {
      wanted.current.delete(path);
      return { refusal };
    }
    setUnfolded((current) => new Set([...current, path]));
    return { path };
  }

  const fold = (key: string, unfold: boolean) =>
    setUnfolded((current) => {
      const next = new Set(current);
      if (unfold) next.add(key);
      else next.delete(key);
      return next;
    });

  const options = (
    <>
      <Button variant="ghost" size="xs" onPress={() => setAllPorts(!allPorts)}>
        {allPorts ? "Fold unconnected ports" : hidden > 0 ? `Show ${hidden} unconnected ports` : "Show all ports"}
      </Button>
      {authored && (
        <Button variant="ghost" size="xs" onPress={() => setAutomatic(!automatic)}>
          {automatic ? "Authored layout" : "Automatic layout"}
        </Button>
      )}
    </>
  );
  return (
    <Graph
      key={mode}
      store={store}
      root={root}
      network={network}
      layout={layout}
      issues={issues}
      varying={varying}
      hiddenBy={allPorts ? new Map() : hiddenBy}
      unfolded={unfolded}
      options={options}
      arrangement={authored && !automatic ? "authored" : "automatic"}
      onAdd={add}
      onFold={fold}
    />
  );
}

/** The "Add shader" menu of the toolbar: the shader ids the editor knows, by family. */
function AddShaderButton({ onAdd }: { onAdd: (id: string) => void }) {
  return (
    <DropdownMenuTrigger>
      <Button variant="outline" size="xs" aria-label="Add shader">
        Add shader
      </Button>
      <DropdownMenu aria-label="Add shader" className="w-auto min-w-56">
        <MenuEntries entries={addShaderEntries()} act={(action) => action.kind === "add" && onAdd(action.id)} />
      </DropdownMenu>
    </DropdownMenuTrigger>
  );
}

/** Where the drawing is: its translation and zoom. */
interface View {
  readonly x: number;
  readonly y: number;
  readonly scale: number;
}

const MIN_SCALE = 0.05;
const MAX_SCALE = 2.5;
/** The smallest zoom a first view uses: below it names cannot be read. */
const READABLE = 0.8;
const PADDING = 24;
/** How long a note about the last action stays, in milliseconds. */
const NOTE_MS = 5000;

/** A note under the toolbar: what the last action authored, or why it was refused. */
interface Note {
  readonly text: string;
  readonly tone: "info" | "error";
}

/** A port's short name for a note: `Texture.rgb`. */
function shortPort(network: ShadingNetwork, path: string): string {
  const ref = findPort(network, path);
  return ref === null ? path.slice(path.lastIndexOf("/") + 1) : portName(ref);
}

/** The destination's `.connect` an edit of it authors, as USD names it. */
function connectAttribute(path: string): string {
  return `${path.slice(path.lastIndexOf("/") + 1)}.connect`;
}

function Graph({
  store,
  root,
  network,
  layout,
  issues,
  varying,
  hiddenBy,
  unfolded,
  options,
  arrangement,
  onAdd,
  onFold,
}: {
  store: EditorStore;
  root: string;
  network: ShadingNetwork;
  layout: NetworkLayout;
  issues: ReadonlyMap<string, readonly PortIssue[]>;
  varying: ReadonlySet<string>;
  hiddenBy: ReadonlyMap<string, number>;
  unfolded: ReadonlySet<string>;
  options: React.ReactNode;
  arrangement: "authored" | "automatic";
  onAdd: (id: string, at: Point) => { path: string } | { refusal: string };
  onFold: (key: string, unfold: boolean) => void;
}) {
  const frame = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<View | null>(null);
  const pan = useRef<{ x: number; y: number; view: View; moved: boolean } | null>(null);
  const [gesture, setGesture] = useState<Gesture>({ kind: "idle" });
  const gestureRef = useRef<Gesture>(gesture);
  const [focus, setFocus] = useState<GraphFocus | null>(null);
  /** Whether a focus change should move the DOM focus (from the keyboard, or after an action). */
  const moveDomFocus = useRef(false);
  const [keyboard, setKeyboard] = useState(false);
  const [focusWithin, setFocusWithin] = useState(false);
  const [selectedEdge, setSelectedEdge] = useState<string | null>(null);
  const [menu, setMenu] = useState<(OpenMenu & { readonly at: Point }) | null>(null);
  const [note, setNote] = useState<Note | null>(null);
  const selection = store.selection;
  const selected = layout.nodes.find((placed) => placed.node.path === selection && placed.node.role === "shader") ?? null;
  const outputs = layout.nodes.find((placed) => placed.node.role === "outputs" && placed.node.path === root) ?? null;

  useEffect(() => {
    if (note === null) return;
    const timer = setTimeout(() => setNote(null), NOTE_MS);
    return () => clearTimeout(timer);
  }, [note]);

  /** The view that fits the whole drawing, or with `readable` the one that keeps names legible around `focus`. */
  const fit = (readable: boolean, focus: PlacedNode | null): View | null => {
    const box = frame.current?.getBoundingClientRect();
    if (box === undefined || box.width === 0) return null;
    const whole = Math.min((box.width - 2 * PADDING) / layout.width, (box.height - 2 * PADDING) / layout.height, 1);
    const scale = Math.max(MIN_SCALE, readable ? Math.max(whole, READABLE) : whole);
    if (scale === whole || focus === null) {
      return { scale, x: (box.width - layout.width * scale) / 2, y: Math.max(PADDING, (box.height - layout.height * scale) / 2) };
    }
    // Centred on the node, its title kept in the frame.
    const y = Math.max(box.height / 2 - (focus.y + focus.height / 2) * scale, PADDING - focus.y * scale);
    return { scale, x: box.width / 2 - (focus.x + focus.width / 2) * scale, y };
  };

  // The first view: legible, on the selected shader or the material's outputs.
  useEffect(() => {
    if (view === null) setView(fit(true, selected ?? outputs));
  });

  // A wheel zooms about the pointer; the listener is not passive, so the page does not scroll.
  useEffect(() => {
    const element = frame.current;
    if (element === null) return;
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const box = element.getBoundingClientRect();
      const px = event.clientX - box.left;
      const py = event.clientY - box.top;
      setView((current) => {
        if (current === null) return current;
        const scale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, current.scale * Math.exp(-event.deltaY * 0.0015)));
        const ratio = scale / current.scale;
        return { scale, x: px - (px - current.x) * ratio, y: py - (py - current.y) * ratio };
      });
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, []);

  // Escape cancels a drag before the editor's own Escape (which drops the selection) hears it.
  const dragging = gesture.kind !== "idle";
  useEffect(() => {
    if (!dragging) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      step({ kind: "cancel" });
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [dragging]);

  // The keyboard's focus, where it is drawn: the remembered one while its node is there, else the selection's, else the first.
  const current = focus !== null && layout.nodes.some((placed) => placed.node.key === focus.node && (focus.port === null || placed.node.ports.some((port) => port.path === focus.port))) ? focus : initialFocus(layout, selected?.node.key ?? outputs?.node.key ?? null);
  const tabStop = current.port ?? current.node;
  useEffect(() => {
    if (!moveDomFocus.current) return;
    const element = frame.current?.querySelector<SVGElement>(`[data-focus="${CSS.escape(tabStop)}"]`);
    if (element === null || element === undefined) return;
    moveDomFocus.current = false;
    element.focus({ preventScroll: true });
    reveal(element);
  });

  /** Pans the view so `element` is in the frame. */
  const reveal = (element: Element) => {
    const box = frame.current?.getBoundingClientRect();
    const at = element.getBoundingClientRect();
    if (box === undefined || view === null) return;
    const dx = at.left < box.left + PADDING ? box.left + PADDING - at.left : at.right > box.right - PADDING ? box.right - PADDING - at.right : 0;
    const dy = at.top < box.top + PADDING ? box.top + PADDING - at.top : at.bottom > box.bottom - PADDING ? box.bottom - PADDING - at.bottom : 0;
    if (dx !== 0 || dy !== 0) setView({ ...view, x: view.x + dx, y: view.y + dy });
  };

  /** A point of the frame (pixels from its top left) in the drawing. */
  const toDrawing = (x: number, y: number): Point => (view === null ? { x, y } : { x: (x - view.x) / view.scale, y: (y - view.y) / view.scale });
  /** A pointer event's point in the frame. */
  const inFrame = (event: { clientX: number; clientY: number }): Point => {
    const box = frame.current!.getBoundingClientRect();
    return { x: event.clientX - box.left, y: event.clientY - box.top };
  };

  const judge = (source: string, target: DropTarget): Verdict | null => judgeDrop(network, source, target);

  /** Feeds `input` to the drag, and carries out what it asks. */
  function step(input: GestureInput, at?: Point): void {
    const next = stepGesture(gestureRef.current, input, judge);
    gestureRef.current = next.state;
    setGesture(next.state);
    carry(next.outcome, at);
  }

  function carry(outcome: GestureOutcome, at?: Point): void {
    switch (outcome.kind) {
      case "none":
        return;
      case "click": {
        store.select(primOf(outcome.port));
        store.explain(outcome.port, false);
        setFocus(focusOf(outcome.port));
        return;
      }
      case "connect":
        connect(outcome.destination, outcome.source);
        return;
      case "choose":
        if (at !== undefined) setMenu({ ...at, at: toDrawing(at.x, at.y), title: `${dropTitle(network, outcome.source, outcome.node)}: which input? Edits go to ${editsGo(store)}`, entries: dropMenu(network, outcome.source, outcome.node) });
        return;
      case "refused":
        setNote({ tone: "error", text: `Not connected: ${outcome.reason}` });
        return;
    }
  }

  /** The focus on the row at `port`. */
  const focusOf = (port: string): GraphFocus | null => {
    const ref = findPort(network, port);
    return ref === null ? null : { node: layout.nodes.find((placed) => placed.node.ports.some((each) => each.path === port))?.node.key ?? ref.node.key, port };
  };

  function connect(destination: string, source: string): void {
    const refusal = store.connect(destination, source);
    setNote(
      refusal !== undefined
        ? { tone: "error", text: `Not connected: ${refusal}` }
        : { tone: "info", text: `${shortPort(network, destination)} reads ${shortPort(network, source)}: authored ${connectAttribute(destination)} in ${editsGo(store)}` },
    );
  }

  function disconnect(destination: string, source: string | null): void {
    const refusal = store.disconnect(destination, source);
    setSelectedEdge(null);
    setNote(
      refusal !== undefined
        ? { tone: "error", text: `Not disconnected: ${refusal}` }
        : {
            tone: "info",
            text: `${shortPort(network, destination)} ${source === null ? "reads nothing" : `no longer reads ${shortPort(network, source)}`}: authored ${connectAttribute(destination)} in ${editsGo(store)}`,
          },
    );
  }

  function remove(prim: string): void {
    const node = network.nodes.find((each) => each.path === prim && each.role === "shader");
    const count = node === undefined ? 0 : connectionsOf(network, node).length;
    // The network stays in view: a selection the delete would take goes to the container first.
    if (store.selection !== null && (store.selection === prim || store.selection.startsWith(`${prim}/`))) store.select(root);
    const refusal = store.delete(prim);
    setNote(
      refusal !== undefined
        ? { tone: "error", text: `Not deleted: ${refusal}` }
        : { tone: "info", text: `Deleted ${prim}${count > 0 ? ` and ${count} connection${count === 1 ? "" : "s"} to and from it` : ""}, in ${editsGo(store)}` },
    );
    moveDomFocus.current = true;
    setFocus(null);
  }

  function addAt(id: string, at: Point): void {
    const added = onAdd(id, at);
    if ("refusal" in added) {
      setNote({ tone: "error", text: `Not added: ${added.refusal}` });
      return;
    }
    setNote({ tone: "info", text: `Added ${id} at ${added.path}: a Shader with info:id, in ${editsGo(store)}` });
    moveDomFocus.current = true;
    setFocus({ node: added.path, port: null });
  }

  function act(action: GraphAction): void {
    switch (action.kind) {
      case "connect":
        connect(action.destination, action.source);
        return;
      case "disconnect":
        disconnect(action.destination, action.source);
        return;
      case "explain":
        store.select(primOf(action.port));
        store.explain(action.port, false);
        return;
      case "select":
        store.select(action.prim);
        return;
      case "delete":
        remove(action.prim);
        return;
      case "add":
        addAt(action.id, menu?.at ?? centre());
        return;
      case "fold":
        onFold(action.node, action.unfold);
        return;
    }
  }

  /** The drawing's point at the frame's centre, less half a node: where the toolbar's "Add shader" puts one. */
  const centre = (): Point => {
    const box = frame.current?.getBoundingClientRect();
    const point = toDrawing((box?.width ?? 0) / 2, (box?.height ?? 0) / 2);
    return { x: point.x - GRAPH.minWidth / 2, y: point.y - GRAPH.header };
  };

  /** The menu of what is at `target` (a row, a node, a curve, or the background), opened at `at` in the frame. */
  function openMenu(target: Element | null, at: Point): void {
    const edge = target?.closest<SVGElement>("[data-edge]")?.dataset.edge;
    const port = target?.closest<SVGElement>("[data-port]")?.dataset.port;
    const node = target?.closest<SVGElement>("[data-node]")?.dataset.node;
    const where = `Edits go to ${editsGo(store)}`;
    const drawing = toDrawing(at.x, at.y);
    if (edge !== undefined) {
      const [from, to] = edge.split(" -> ") as [string, string];
      setSelectedEdge(edge);
      setMenu({ ...at, at: drawing, title: `${shortPort(network, from)} → ${shortPort(network, to)}. ${where}`, entries: edgeMenu(network, from, to) });
    } else if (port !== undefined) {
      setMenu({ ...at, at: drawing, title: `${shortPort(network, port)}. ${where}`, entries: portMenu(network, port) });
    } else if (node !== undefined) {
      const found = network.nodes.find((each) => each.key === node)!;
      setMenu({ ...at, at: drawing, title: `${nodeName(found)}. ${where}`, entries: nodeMenu(network, node, unfolded.has(node), hiddenBy.get(node) ?? 0) });
    } else {
      setMenu({ ...at, at: drawing, title: `${root}. ${where}`, entries: backgroundMenu() });
    }
  }

  /** What a click (no drag) on `target` does: unfold, select a curve, or select the node and explain the row. */
  function pick(target: Element | null): void {
    const foldKey = target?.closest<SVGElement>("[data-fold]")?.dataset.fold;
    if (foldKey !== undefined) {
      onFold(foldKey, true);
      return;
    }
    const edge = target?.closest<SVGElement>("[data-edge]");
    if (edge !== null && edge !== undefined) {
      setSelectedEdge(edge.dataset.edge!);
      edge.focus({ preventScroll: true });
      return;
    }
    setSelectedEdge(null);
    const element = target?.closest<SVGElement>("[data-prim]");
    if (element === null || element === undefined) return;
    store.select(element.dataset.prim!);
    const property = element.dataset.port;
    // Explained without leaving this tab for the Explain one: the graph is what is being read.
    if (property !== undefined) store.explain(property, false);
    const node = element.closest<SVGElement>("[data-node]")?.dataset.node;
    if (node !== undefined) setFocus({ node, port: property ?? null });
  }

  /** The keys of the focused node, row or curve. */
  function onKey(event: React.KeyboardEvent<HTMLDivElement>): void {
    const target = event.target as Element;
    if (menu !== null) return;
    const key = graphKey(event);
    // Escape anywhere in the drawing, a row's value control too, lets go of a chosen connection and leaves the graph and the selection as they are.
    if (key?.kind === "escape") {
      event.preventDefault();
      setSelectedEdge(null);
      return;
    }
    if (target.closest("[data-control]") !== null || key === null) return;
    event.preventDefault();
    setKeyboard(true);
    const edge = target.closest<SVGElement>("[data-edge]")?.dataset.edge;
    const focusedRect = target.getBoundingClientRect();
    const box = frame.current!.getBoundingClientRect();
    const at = { x: focusedRect.left - box.left + 8, y: focusedRect.bottom - box.top };
    switch (key.kind) {
      case "move":
        setSelectedEdge(null);
        moveDomFocus.current = true;
        setFocus(moveFocus(layout, current, key.key));
        return;
      case "menu":
        openMenu(target, at);
        return;
      case "activate":
        if (edge !== undefined) return;
        if (current.port !== null) {
          store.select(primOf(current.port));
          store.explain(current.port, false);
        } else {
          const node = layout.nodes.find((placed) => placed.node.key === current.node);
          if (node !== undefined) store.select(node.node.path);
        }
        return;
      case "delete": {
        if (edge !== undefined) {
          const [from, to] = edge.split(" -> ") as [string, string];
          disconnect(to, from);
          moveDomFocus.current = true;
          return;
        }
        if (current.port !== null) {
          const ref = findPort(network, current.port);
          if (ref === null || !isDestination(network, ref) || ref.port.connections.length === 0) {
            setNote({ tone: "error", text: `${shortPort(network, current.port)} reads nothing to disconnect` });
          } else if (ref.port.connections.length === 1) {
            disconnect(ref.port.path, ref.port.connections[0]!);
          } else {
            openMenu(target, at);
          }
          return;
        }
        const node = network.nodes.find((each) => each.key === current.node);
        if (node?.role === "shader") remove(node.path);
        else setNote({ tone: "error", text: `${node === undefined ? current.node : nodeName(node)} is the container's boundary, not a node to delete` });
        return;
      }
    }
  }

  // Each node's part in a drag: computed when the drag starts and when what it is over changes, not as the pointer moves.
  const source = gesture.kind === "pressed" ? null : gesture.kind === "dragging" ? gesture.source : null;
  const over = gesture.kind === "dragging" ? gesture.over : null;
  const overKey = over === null ? null : over.kind === "port" ? `port ${over.port}` : `node ${over.node}`;
  const drags = useMemo(() => {
    if (source === null) return null;
    const rows = new Map<string, Map<string, RowDrag>>();
    const onNode = new Map<string, Verdict | null>();
    for (const placed of layout.nodes) {
      const map = new Map<string, RowDrag>();
      for (const port of placed.node.ports) {
        if (port.path === source) {
          map.set(port.path, { kind: "source" });
          continue;
        }
        const verdict = judge(source, { kind: "port", port: port.path });
        map.set(port.path, verdict === null ? { kind: "inert" } : { kind: "target", verdict, over: over?.kind === "port" && over.port === port.path });
      }
      rows.set(placed.node.key, map);
      onNode.set(placed.node.key, over?.kind === "node" && over.node === placed.node.key ? judge(source, over) : null);
    }
    return { rows, onNode };
  }, [source, overKey, layout, network]);

  const connected = useMemo(() => {
    const ports = new Set<string>();
    for (const { edge } of layout.edges) {
      ports.add(edge.from);
      ports.add(edge.to);
    }
    return ports;
  }, [layout]);
  const sources = useMemo(
    () => new Set(network.nodes.flatMap((node) => node.ports.filter((port) => isSource(network, { node, port })).map((port) => port.path))),
    [network],
  );
  const edgeProblems = useMemo(() => {
    const problems = new Map<string, string>();
    for (const [port, list] of issues) for (const issue of list) if (issue.source !== null) problems.set(`${issue.source} -> ${port}`, issue.reason);
    return problems;
  }, [issues]);

  const touching = (key: string) => selected !== null && key === selected.node.key;
  const ringShown = keyboard && focusWithin;
  const drag = gesture.kind === "dragging" ? gesture : null;
  const dragFrom = drag === null ? null : anchorOf(layout, drag.source);
  const pointer = drag === null || view === null ? null : toDrawing(drag.pointer.x, drag.pointer.y);
  const status: Note | null = drag === null ? note : dragNote(network, drag);
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-2 border-b px-3 py-1 text-[11px] text-muted-foreground">
        <span className="truncate font-mono text-foreground">{root}</span>
        <span className="shrink-0">
          {layout.nodes.length} nodes · {layout.edges.length} connections · {arrangement === "authored" ? "as its authors placed them" : "laid out automatically"}
        </span>
        <span className="ml-auto" />
        <AddShaderButton onAdd={(id) => addAt(id, centre())} />
        {options}
        <Button variant="ghost" size="xs" onPress={() => setView(fit(false, null))}>
          Fit
        </Button>
        <Button variant="ghost" size="xs" onPress={() => setView(fit(true, selected ?? outputs))}>
          {selected === null ? "Outputs" : "Selected"}
        </Button>
      </div>
      <div
        role={status?.tone === "error" ? "alert" : "status"}
        className={cn("min-h-5 truncate border-b px-3 py-0.5 text-[11px]", status === null ? "text-muted-foreground" : status.tone === "error" ? "text-destructive" : "text-foreground")}
        title={status?.text}
      >
        {status?.text ?? "Drag from an output to an input to connect; right-click (Shift+F10) for more; drag the background to pan, scroll to zoom."}
      </div>
      <div
        ref={frame}
        className={cn(
          "relative min-h-0 flex-1 touch-none overflow-hidden bg-[radial-gradient(var(--color-border)_1px,transparent_1px)] [background-size:16px_16px] select-none",
          drag !== null ? "cursor-crosshair" : "cursor-grab active:cursor-grabbing",
        )}
        onPointerDown={(event) => {
          setKeyboard(false);
          if (view === null || event.button !== 0) return;
          const target = event.target as Element;
          if (target.closest("[data-control]") !== null) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          const from = target.closest<SVGElement>("[data-source]")?.dataset.source;
          if (from !== undefined) {
            step({ kind: "press", source: from, at: inFrame(event) });
            return;
          }
          pan.current = { x: event.clientX, y: event.clientY, view, moved: false };
        }}
        onPointerMove={(event) => {
          if (gestureRef.current.kind !== "idle") {
            const hit = document.elementFromPoint(event.clientX, event.clientY);
            const port = hit?.closest<SVGElement | HTMLElement>("[data-port]")?.dataset.port ?? null;
            const node = hit?.closest<SVGElement>("[data-node]")?.dataset.node ?? null;
            step({ kind: "move", at: inFrame(event), over: dropTargetAt(network, { port, node }) });
            return;
          }
          const start = pan.current;
          if (start === null) return;
          const dx = event.clientX - start.x;
          const dy = event.clientY - start.y;
          if (!start.moved && Math.hypot(dx, dy) < 3) return;
          start.moved = true;
          setView({ ...start.view, x: start.view.x + dx, y: start.view.y + dy });
        }}
        onPointerUp={(event) => {
          if (gestureRef.current.kind !== "idle") {
            step({ kind: "release" }, inFrame(event));
            return;
          }
          const start = pan.current;
          pan.current = null;
          // A click (no drag) picks what is under the pointer; capture retargets the event, so find it by position.
          if (start !== null && !start.moved) pick(document.elementFromPoint(event.clientX, event.clientY));
        }}
        onPointerCancel={() => {
          pan.current = null;
          if (gestureRef.current.kind !== "idle") step({ kind: "cancel" });
        }}
        onContextMenu={(event) => {
          if ((event.target as Element).closest("[data-control]") !== null) return;
          event.preventDefault();
          openMenu(event.target as Element, inFrame(event));
        }}
        onDoubleClick={(event) => {
          if ((event.target as Element).closest("[data-control], [data-node]") === null) setView(fit(false, null));
        }}
        onKeyDown={onKey}
        onFocus={(event) => {
          setFocusWithin(true);
          const element = (event.target as Element).closest<SVGElement>("[data-focus]");
          const node = element?.closest<SVGElement>("[data-node]")?.dataset.node;
          if (element === null || element === undefined || node === undefined) return;
          const port = element.dataset.port ?? null;
          if (current.node !== node || current.port !== port) setFocus({ node, port });
        }}
        onBlur={(event) => {
          if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setFocusWithin(false);
        }}
      >
        {view !== null && (
          <svg className="absolute inset-0 size-full" role="group" aria-roledescription="node graph" aria-label={`Shading network of ${root}`} aria-describedby="shading-keys">
            <g transform={`translate(${view.x} ${view.y}) scale(${view.scale})`}>
              {layout.edges.map((edge) => {
                const id = edgeKey(edge.edge);
                return <EdgeView key={id} id={id} edge={edge} lit={touching(edge.from) || touching(edge.to)} selected={selectedEdge === id} problem={edgeProblems.get(id) ?? null} />;
              })}
              {layout.nodes.map((placed) => (
                <NodeView key={placed.node.key} store={store} placed={placed} state={nodeState(placed)} />
              ))}
              {dragFrom !== null && pointer !== null && (
                <path
                  d={`M ${dragFrom.x} ${dragFrom.y} C ${dragFrom.x + 60} ${dragFrom.y}, ${pointer.x - 60} ${pointer.y}, ${pointer.x} ${pointer.y}`}
                  className={cn("pointer-events-none fill-none", drag?.verdict?.kind === "refused" ? "stroke-destructive" : "stroke-primary")}
                  strokeWidth={2}
                  strokeDasharray="6 4"
                  vectorEffect="non-scaling-stroke"
                />
              )}
            </g>
          </svg>
        )}
        <p id="shading-keys" className="sr-only">
          Arrow keys move between nodes and their rows, following a row's connection left or right. Enter selects a node or explains a row. Shift+F10 opens the menu of the
          focused row, node or connection: Connect to, Disconnect, Delete shader. Delete deletes the focused shader or disconnects the focused row or connection. Escape cancels a drag or lets go of a chosen connection, and otherwise leaves the selection as it is.
        </p>
        <Legend />
        {menu !== null && (
          <GraphMenu
            menu={menu}
            act={act}
            onClose={() => {
              // A menu opened from the keyboard gives the focus back to the row or node it was opened on.
              if (keyboard) moveDomFocus.current = true;
              setMenu(null);
            }}
          />
        )}
      </div>
    </div>
  );

  /** What the node `placed` shows besides itself. */
  function nodeState(placed: PlacedNode): NodeState {
    const key = placed.node.key;
    const explained = store.property !== null && placed.node.ports.some((port) => port.path === store.property) ? store.property : null;
    return {
      selected: placed.node.path === selection,
      explained,
      ring: ringShown && current.node === key ? (current.port ?? "") : null,
      drag: drags?.rows.get(key) ?? null,
      dropOnNode: drags?.onNode.get(key) ?? null,
      connected,
      issues,
      hidden: hiddenBy.get(key) ?? 0,
      sources,
      varying,
      tabStop: current.node === key || placed.node.ports.some((port) => port.path === tabStop) ? tabStop : "",
    };
  }
}

/** The right-hand anchor of the row at `port`, where a dragged connection starts. */
function anchorOf(layout: NetworkLayout, port: string): Point | null {
  for (const placed of layout.nodes) {
    const index = placed.node.ports.findIndex((each) => each.path === port);
    if (index >= 0) return { x: placed.x + placed.width, y: placed.y + portOffset(index) };
  }
  return null;
}

/** What the note says while a connection is dragged: from where, and what a drop where the pointer is would do. */
function dragNote(network: ShadingNetwork, drag: Gesture & { readonly kind: "dragging" }): Note {
  const from = shortPort(network, drag.source);
  const { over, verdict } = drag;
  if (over === null || verdict === null) return { tone: "info", text: `Connecting from ${from}: drop on an input, or on a node to choose one; Escape cancels` };
  if (verdict.kind === "refused") return { tone: "error", text: `Cannot connect: ${verdict.reason}` };
  if (over.kind === "node") {
    const node = network.nodes.find((each) => each.key === over.node);
    return { tone: "info", text: `Drop to choose which input of ${node === undefined ? over.node : nodeName(node)} reads ${from}` };
  }
  return { tone: "info", text: `Drop to connect ${shortPort(network, over.port)} to ${from}${verdict.kind === "warn" ? `: ${verdict.reason}` : ""}` };
}
