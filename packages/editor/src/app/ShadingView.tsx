// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The shading view: the network of the selected material (or of the
// material or node graph around a selected shader) as a node graph, left
// to right, ending in the material's outputs. Ports are rows; a
// connection is a curve from its source's row to the row that reads it.
// Drag to pan, scroll to zoom; click a node to select its prim, a port to
// explain its value.
import { useEffect, useMemo, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { GRAPH, layoutNetwork, type NetworkLayout, type NetworkNode, type NetworkPort, networkRoot, type PlacedNode, portOffset, readNetwork } from "../model/network.ts";
import type { EditorStore } from "../store.ts";
import { useStore } from "./context.ts";

export function ShadingView() {
  const store = useStore();
  const session = store.session;
  const stage = session?.stage ?? null;
  const root = stage === null || store.selection === null ? null : networkRoot(stage, store.selection);
  // The network is read again only after a recomposition.
  const recompositions = session?.changeLog.length ?? 0;
  const layout = useMemo(() => {
    if (stage === null || root === null) return null;
    return layoutNetwork(readNetwork(stage, root));
  }, [session, root, recompositions]);
  if (stage === null || root === null || layout === null) {
    return <p className="p-3 text-xs text-muted-foreground">Select a material, a node graph or a shader to see its network.</p>;
  }
  if (layout.nodes.length === 0) {
    return <p className="p-3 text-xs text-muted-foreground">{root} has no shaders, inputs or outputs.</p>;
  }
  return <Graph key={root} store={store} root={root} layout={layout} />;
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

function Graph({ store, root, layout }: { store: EditorStore; root: string; layout: NetworkLayout }) {
  const frame = useRef<HTMLDivElement>(null);
  const [view, setView] = useState<View | null>(null);
  const drag = useRef<{ x: number; y: number; view: View; moved: boolean } | null>(null);
  const selection = store.selection;
  const connected = useMemo(() => connectedPorts(layout), [layout]);
  const selected = layout.nodes.find((placed) => placed.node.path === selection && placed.node.role === "shader") ?? null;

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
  const outputs = layout.nodes.find((placed) => placed.node.role === "outputs" && placed.node.path === root) ?? null;
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

  const pick = (target: EventTarget | null) => {
    const element = (target as Element | null)?.closest<SVGElement>("[data-prim]");
    if (element === null || element === undefined) return;
    const prim = element.dataset.prim!;
    store.select(prim);
    const property = element.dataset.port;
    if (property !== undefined) store.explain(property);
  };

  const touching = (key: string) => selected !== null && key === selected.node.key;
  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-2 border-b px-3 py-1 text-[11px] text-muted-foreground">
        <span className="truncate font-mono text-foreground">{root}</span>
        <span>
          {layout.nodes.length} nodes · {layout.edges.length} connections
        </span>
        <span className="ml-auto">Drag to pan, scroll to zoom</span>
        <Button variant="ghost" size="xs" onPress={() => setView(fit(false, null))}>
          Fit
        </Button>
        <Button variant="ghost" size="xs" onPress={() => setView(fit(true, selected ?? outputs))}>
          {selected === null ? "Outputs" : "Selected"}
        </Button>
      </div>
      <div
        ref={frame}
        className="relative min-h-0 flex-1 cursor-grab touch-none overflow-hidden bg-[radial-gradient(var(--color-border)_1px,transparent_1px)] [background-size:16px_16px] select-none active:cursor-grabbing"
        onPointerDown={(event) => {
          if (view === null || event.button !== 0) return;
          event.currentTarget.setPointerCapture(event.pointerId);
          drag.current = { x: event.clientX, y: event.clientY, view, moved: false };
        }}
        onPointerMove={(event) => {
          const start = drag.current;
          if (start === null) return;
          const dx = event.clientX - start.x;
          const dy = event.clientY - start.y;
          if (!start.moved && Math.hypot(dx, dy) < 3) return;
          start.moved = true;
          setView({ ...start.view, x: start.view.x + dx, y: start.view.y + dy });
        }}
        onPointerUp={(event) => {
          const start = drag.current;
          drag.current = null;
          // A click (no drag) picks what is under the pointer; capture retargets the event, so find it by position.
          if (start !== null && !start.moved) pick(document.elementFromPoint(event.clientX, event.clientY));
        }}
        onDoubleClick={() => setView(fit(false, null))}
      >
        {view !== null && (
          <svg className="absolute inset-0 size-full">
            <g transform={`translate(${view.x} ${view.y}) scale(${view.scale})`}>
              {layout.edges.map((edge, index) => {
                const lit = touching(edge.from) || touching(edge.to);
                const bend = Math.max(40, Math.abs(edge.x2 - edge.x1) / 2);
                return (
                  <path
                    key={index}
                    d={`M ${edge.x1} ${edge.y1} C ${edge.x1 + bend} ${edge.y1}, ${edge.x2 - bend} ${edge.y2}, ${edge.x2} ${edge.y2}`}
                    className={cn("fill-none", lit ? "stroke-primary" : "stroke-muted-foreground/35")}
                    strokeWidth={lit ? 1.75 : 1.25}
                    vectorEffect="non-scaling-stroke"
                  />
                );
              })}
              {layout.nodes.map((placed) => (
                <Node key={placed.node.key} placed={placed} selected={placed.node.path === selection} property={store.property} connected={connected} />
              ))}
            </g>
          </svg>
        )}
      </div>
    </div>
  );
}

/** The ports some edge touches, by path. */
function connectedPorts(layout: NetworkLayout): ReadonlySet<string> {
  const ports = new Set<string>();
  for (const { edge } of layout.edges) {
    ports.add(edge.from);
    ports.add(edge.to);
  }
  return ports;
}

/** A port's colour by what its type holds: colours, numbers, vectors, files, switches, terminals. */
function portTone(port: NetworkPort): string {
  const type = port.typeName ?? "";
  if (type.startsWith("color")) return "fill-amber-400";
  if (/^(float|double|half|int|uint)\d?$/.test(type)) return "fill-sky-400";
  if (/^(float|double|half|int)[234]$|^(vector|normal|point|texCoord)/.test(type)) return "fill-violet-400";
  if (type === "asset" || type === "string") return "fill-emerald-400";
  if (type === "bool") return "fill-rose-400";
  return "fill-muted-foreground";
}

function Node({ placed, selected, property, connected }: { placed: PlacedNode; selected: boolean; property: string | null; connected: ReadonlySet<string> }) {
  const { node, x, y, width, height } = placed;
  const boundary = node.role !== "shader";
  return (
    <g transform={`translate(${x} ${y})`} data-prim={node.path} className="cursor-pointer">
      <title>{`${node.path}${node.detail === null ? "" : ` (${node.detail})`}`}</title>
      <rect
        width={width}
        height={height}
        rx={6}
        className={cn(boundary ? "fill-muted" : "fill-card", selected ? "stroke-primary" : "stroke-border")}
        strokeWidth={selected ? 2 : 1}
        vectorEffect="non-scaling-stroke"
      />
      <text x={10} y={15} className="fill-foreground text-[11px] font-medium">
        {clip(node.name, width - 16)}
      </text>
      <text x={10} y={28} className="fill-muted-foreground text-[10px]">
        {clip(subtitle(node), width - 16)}
      </text>
      <line x1={0} x2={width} y1={GRAPH.header - 2} y2={GRAPH.header - 2} className="stroke-border" vectorEffect="non-scaling-stroke" />
      {node.ports.map((port, index) => {
        const cy = portOffset(index);
        const used = connected.has(port.path);
        // A shader's inputs are on its left, outputs on its right; a boundary port reads on the left and is read on the right.
        const left = boundary ? node.role === "outputs" || port.connections.length > 0 : port.kind === "input";
        const right = boundary ? node.role === "inputs" || used : port.kind === "output";
        const focused = port.path === property;
        return (
          <g key={port.path} data-prim={node.path} data-port={port.path}>
            <title>{`${port.name}: ${port.typeName ?? "untyped"}${port.connections.length > 0 ? ` ← ${port.connections.join(", ")}` : ""}`}</title>
            <rect x={1} y={cy - GRAPH.row / 2} width={width - 2} height={GRAPH.row} className={cn(focused ? "fill-accent" : "fill-transparent hover:fill-muted/70")} />
            {left && <circle cx={0} cy={cy} r={3.5} className={portTone(port)} />}
            {right && <circle cx={width} cy={cy} r={3.5} className={portTone(port)} />}
            <text
              x={port.kind === "output" && !boundary ? width - 10 : 10}
              y={cy + 3.5}
              textAnchor={port.kind === "output" && !boundary ? "end" : "start"}
              className={cn("text-[11px]", used ? "fill-foreground" : "fill-muted-foreground")}
            >
              {clip(port.name, width - 20)}
            </text>
          </g>
        );
      })}
    </g>
  );
}

/** A node's second line: a shader's `info:id`, or which side of its container a boundary is. */
function subtitle(node: NetworkNode): string {
  if (node.role === "shader") return node.detail ?? "no info:id";
  return `${node.detail ?? "prim"} ${node.role}`;
}

/** `text` cut to fit `width` at the drawing's character width. */
function clip(text: string, width: number): string {
  const fits = Math.floor(width / GRAPH.character);
  return text.length <= fits ? text : `${text.slice(0, Math.max(1, fits - 1))}…`;
}
