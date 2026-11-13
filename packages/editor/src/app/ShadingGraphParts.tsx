// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What the shading view draws and opens: a node with its rows (each a
// port, its type marked by the shape of its anchor as well as its colour,
// an unconnected input's value editable in place), a connection's curve,
// the legend of the anchors' shapes, and a menu built from the menu data
// of `model/networkEdits.ts`. The view (`ShadingView.tsx`) handles the
// pointer and the keys on the whole drawing, by the `data-*` attributes
// these carry: `data-node`, `data-port`, `data-source` (a row a
// connection can start from), `data-edge`, `data-fold`, `data-control` (a
// value's field, which keeps its pointer and keys).
import { memo, useRef } from "react";
import { Header, Menu, MenuSection, Popover } from "react-aria-components";
import { DropdownMenuItem, DropdownMenuSub, DropdownMenuSubContent, DropdownMenuSubTrigger } from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { type PortIssue, type TypeFamily, typeFamily, type Verdict } from "../model/connections.ts";
import { GRAPH, type NetworkNode, type NetworkPort, type PlacedEdge, type PlacedNode, portOffset, showsValue } from "../model/network.ts";
import type { GraphAction, MenuEntry } from "../model/networkEdits.ts";
import { definedInput, shaderDefinition } from "../model/shaderDefinitions.ts";
import type { EditorStore } from "../store.ts";
import { useTime } from "./context.ts";
import { ShaderInputField } from "./ShaderInputField.tsx";

/** Each family's colour (a fill class) and word, for the anchors and the legend. */
const FAMILIES: Record<TypeFamily, { readonly tone: string; readonly word: string }> = {
  color: { tone: "fill-amber-400 stroke-amber-500", word: "colour" },
  number: { tone: "fill-sky-400 stroke-sky-500", word: "number" },
  vector: { tone: "fill-violet-400 stroke-violet-500", word: "vector" },
  text: { tone: "fill-emerald-400 stroke-emerald-500", word: "file or text" },
  bool: { tone: "fill-rose-400 stroke-rose-500", word: "switch" },
  token: { tone: "fill-muted-foreground stroke-muted-foreground", word: "token" },
  other: { tone: "fill-muted-foreground stroke-muted-foreground", word: "other" },
};

/**
 * A port's anchor at (`x`, `y`): its family's shape (a number a circle, a
 * vector a diamond, a colour a square, a file or text a triangle, a
 * switch a ring, a token a bar), filled when the prim authors the port
 * and hollow when only its definition has it.
 */
export function Anchor({ x, y, family, authored, large = false }: { x: number; y: number; family: TypeFamily; authored: boolean; large?: boolean }) {
  const r = large ? 5 : 3.5;
  const className = cn(FAMILIES[family].tone, !authored && "fill-card", "pointer-events-none");
  const stroke = { strokeWidth: 1.25, vectorEffect: "non-scaling-stroke" as const };
  switch (family) {
    case "vector":
      return <path d={`M ${x} ${y - r - 0.5} L ${x + r + 0.5} ${y} L ${x} ${y + r + 0.5} L ${x - r - 0.5} ${y} Z`} className={className} {...stroke} />;
    case "color":
      return <rect x={x - r} y={y - r} width={2 * r} height={2 * r} rx={1} className={className} {...stroke} />;
    case "text":
      return <path d={`M ${x} ${y - r - 0.5} L ${x + r + 0.5} ${y + r} L ${x - r - 0.5} ${y + r} Z`} className={className} {...stroke} />;
    case "bool":
      return <circle cx={x} cy={y} r={r} className={cn(className, "fill-card")} strokeWidth={2} vectorEffect="non-scaling-stroke" />;
    case "token":
      return <rect x={x - r - 1} y={y - r / 2} width={2 * r + 2} height={r} rx={r / 2} className={className} {...stroke} />;
    default:
      return <circle cx={x} cy={y} r={r} className={className} {...stroke} />;
  }
}

/** The anchors' shapes and what each means. */
export function Legend() {
  const families: TypeFamily[] = ["number", "vector", "color", "text", "bool", "token"];
  return (
    <div className="pointer-events-none absolute bottom-1.5 left-2 flex flex-wrap items-center gap-x-2.5 gap-y-0.5 rounded bg-background/80 px-1.5 py-0.5 text-[10px] text-muted-foreground">
      {families.map((family) => (
        <span key={family} className="flex items-center gap-1">
          <svg width={12} height={12} aria-hidden="true">
            <Anchor x={6} y={6} family={family} authored />
          </svg>
          {FAMILIES[family].word}
        </span>
      ))}
      <span className="flex items-center gap-1">
        <svg width={12} height={12} aria-hidden="true">
          <Anchor x={6} y={6} family="number" authored={false} />
        </svg>
        not authored
      </span>
    </div>
  );
}

/** How a row reads while a connection is dragged: the source, a destination and how a drop there would be, or neither. */
export type RowDrag = { readonly kind: "source" } | { readonly kind: "target"; readonly verdict: Verdict; readonly over: boolean } | { readonly kind: "inert" };

/** What a node shows besides its network: selection, the keyboard's focus, a drag, problems. */
export interface NodeState {
  readonly selected: boolean;
  /** The explained port, when it is one of this node's. */
  readonly explained: string | null;
  /** The focused row (or `""` for the header) when the keyboard's ring is shown on this node; `null` otherwise. */
  readonly ring: string | null;
  /** Each row's part in a drag, by port path; `null` when nothing is dragged. */
  readonly drag: ReadonlyMap<string, RowDrag> | null;
  /** Whether a drag is over the node itself, and how a drop there would be. */
  readonly dropOnNode: Verdict | null;
  /** The ports some connection touches. */
  readonly connected: ReadonlySet<string>;
  /** Problems with the rows' connections, by port path. */
  readonly issues: ReadonlyMap<string, readonly PortIssue[]>;
  /** How many of its ports are folded away. */
  readonly hidden: number;
  /** The ports that can start a connection. */
  readonly sources: ReadonlySet<string>;
  /** The inputs whose value may vary in time. */
  readonly varying: ReadonlySet<string>;
  /** The id of the focusable element that takes Tab into the drawing. */
  readonly tabStop: string;
}

/** A row's words for assistive technology: what it is, its type, what it reads and what is wrong. */
function portLabel(node: NetworkNode, port: NetworkPort, issues: readonly PortIssue[]): string {
  const parts = [`${port.kind} ${port.name}`, port.typeName ?? "untyped"];
  if (!port.authored) parts.push("not authored");
  if (port.connections.length > 0) parts.push(`reads ${port.connections.map((source) => source.slice(source.lastIndexOf("/") + 1)).join(", ")}`);
  for (const issue of issues) parts.push(`problem: ${issue.reason}`);
  return `${node.name} ${parts.join(", ")}`;
}

/** A node's words for assistive technology. */
function nodeLabel(node: NetworkNode, hidden: number): string {
  const what = node.role === "shader" ? `${node.detail ?? "no info:id"} shader` : `${node.detail ?? "prim"} ${node.role}`;
  return `${node.name}, ${what}, ${node.ports.length} rows${hidden > 0 ? `, ${hidden} more folded` : ""}`;
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

/** A row's background while a connection is dragged. */
function dragFill(drag: RowDrag | undefined): string | null {
  if (drag === undefined) return null;
  if (drag.kind === "source") return "fill-primary/25";
  if (drag.kind === "inert") return null;
  if (drag.verdict.kind === "refused") return drag.over ? "fill-destructive/15" : null;
  if (drag.verdict.kind === "warn") return drag.over ? "fill-amber-400/35" : "fill-amber-400/15";
  return drag.over ? "fill-primary/35" : "fill-primary/15";
}

/** Whether two node states hold the same things: a node draws again only when its own state changes, not as the pointer moves. */
function sameState(a: NodeState, b: NodeState): boolean {
  return (Object.keys(a) as (keyof NodeState)[]).every((key) => Object.is(a[key], b[key]));
}

/** A node and its rows, placed. */
export const NodeView = memo(function NodeView({ store, placed, state }: { store: EditorStore; placed: PlacedNode; state: NodeState }) {
  const { node, x, y, width, height } = placed;
  const boundary = node.role !== "shader";
  const dragging = state.drag !== null;
  const drop = state.dropOnNode;
  const definition = node.role === "shader" ? shaderDefinition(node.detail) : null;
  const values = node.ports.some((each) => showsValue(node, each));
  const nameWidth = values ? width - GRAPH.value - 24 : width - 20;
  return (
    <g
      transform={`translate(${x} ${y})`}
      data-node={node.key}
      data-prim={node.path}
      data-focus={node.key}
      role="group"
      aria-roledescription="node"
      aria-label={nodeLabel(node, state.hidden)}
      tabIndex={state.tabStop === node.key ? 0 : -1}
      className="cursor-pointer outline-none"
    >
      <title>{`${node.path}${node.detail === null ? "" : ` (${node.detail})`}`}</title>
      <rect
        width={width}
        height={height}
        rx={6}
        className={cn(
          boundary ? "fill-muted" : "fill-card",
          drop !== null ? (drop.kind === "refused" ? "stroke-destructive" : "stroke-primary") : state.selected ? "stroke-primary" : "stroke-border",
        )}
        strokeWidth={state.selected || drop !== null ? 2 : 1}
        vectorEffect="non-scaling-stroke"
      />
      {state.ring === "" && <rect x={-3} y={-3} width={width + 6} height={GRAPH.header + 2} rx={8} className="pointer-events-none fill-none stroke-ring" strokeWidth={2} vectorEffect="non-scaling-stroke" />}
      <text x={10} y={15} className="fill-foreground text-[11px] font-medium">
        {clip(node.name, width - 16 - (state.hidden > 0 ? 28 : 0))}
      </text>
      <text x={10} y={28} className="fill-muted-foreground text-[10px]">
        {clip(subtitle(node), width - 16)}
      </text>
      {state.hidden > 0 && (
        <g data-fold={node.key} className="cursor-pointer">
          <title>{`Show its ${state.hidden} unconnected ports`}</title>
          <rect x={width - 34} y={6} width={28} height={14} rx={7} className="fill-muted hover:fill-accent" />
          <text x={width - 20} y={16.5} textAnchor="middle" className="fill-muted-foreground text-[9.5px]">
            +{state.hidden}
          </text>
        </g>
      )}
      <line x1={0} x2={width} y1={GRAPH.header - 2} y2={GRAPH.header - 2} className="stroke-border" vectorEffect="non-scaling-stroke" />
      {node.ports.map((port, index) => {
        const cy = portOffset(index);
        const used = state.connected.has(port.path);
        // A shader's inputs are on its left, outputs on its right; a boundary port reads on the left and is read on the right.
        const left = boundary ? node.role === "outputs" || port.connections.length > 0 : port.kind === "input";
        const right = boundary ? node.role === "inputs" || used : port.kind === "output";
        const issues = state.issues.get(port.path) ?? [];
        const drag = state.drag?.get(port.path);
        const fill = dragFill(drag);
        const output = port.kind === "output" && !boundary;
        const family = typeFamily(port.typeName);
        const reason = drag?.kind === "target" && drag.verdict.kind !== "match" ? drag.verdict.reason : null;
        const value = showsValue(node, port);
        // Dangling: connections that name nothing, drawn as a stub out of the row.
        const dangling = issues.filter((issue) => issue.kind === "invalidTarget" && issue.source !== null);
        return (
          <g key={port.path} opacity={dragging && (drag === undefined || drag.kind === "inert" || (drag.kind === "target" && drag.verdict.kind === "refused")) ? 0.4 : 1}>
            <g
              data-port={port.path}
              data-prim={node.path}
              data-focus={port.path}
              data-source={state.sources.has(port.path) ? port.path : undefined}
              role="button"
              aria-haspopup="menu"
              aria-label={portLabel(node, port, issues)}
              tabIndex={state.tabStop === port.path ? 0 : -1}
              className={cn("outline-none", state.sources.has(port.path) && !dragging && "cursor-crosshair")}
            >
              <title>
                {reason ??
                  `${port.name}: ${port.typeName ?? "untyped"}${port.authored ? "" : " (not authored)"}${port.connections.length > 0 ? ` ← ${port.connections.join(", ")}` : ""}${
                    issues.length > 0 ? `\n${issues.map((issue) => issue.reason).join("\n")}` : ""
                  }${state.sources.has(port.path) ? "\nDrag to an input to connect" : ""}`}
              </title>
              <rect
                x={1}
                y={cy - GRAPH.row / 2}
                width={width - 2}
                height={GRAPH.row}
                className={fill ?? (port.path === state.explained ? "fill-accent" : "fill-transparent hover:fill-muted/70")}
              />
              {state.ring === port.path && (
                <rect x={-2} y={cy - GRAPH.row / 2} width={width + 4} height={GRAPH.row} rx={4} className="pointer-events-none fill-none stroke-ring" strokeWidth={2} vectorEffect="non-scaling-stroke" />
              )}
              {left && <Anchor x={0} y={cy} family={family} authored={port.authored} large={drag?.kind === "target" && drag.over} />}
              {right && <Anchor x={width} y={cy} family={family} authored={port.authored} large={drag?.kind === "source"} />}
              {issues.length > 0 && (
                <g className="pointer-events-none">
                  <circle cx={output ? width - 14 : 14} cy={cy} r={5.5} className="fill-destructive" />
                  <text x={output ? width - 14 : 14} y={cy + 3.5} textAnchor="middle" className="fill-white text-[9px] font-bold">
                    !
                  </text>
                </g>
              )}
              <text
                x={output ? width - (issues.length > 0 ? 24 : 10) : issues.length > 0 ? 24 : 10}
                y={cy + 3.5}
                textAnchor={output ? "end" : "start"}
                className={cn("text-[11px]", !port.authored && "italic", used || port.connections.length > 0 ? "fill-foreground" : "fill-muted-foreground")}
              >
                {clip(port.name, nameWidth - (issues.length > 0 ? 14 : 0))}
              </text>
            </g>
            {dangling.map((issue, at) => (
              <g key={issue.source} className="pointer-events-none">
                <line x1={-34} x2={0} y1={cy + at * 3} y2={cy} className="stroke-destructive" strokeDasharray="3 3" strokeWidth={1.25} vectorEffect="non-scaling-stroke" />
                <text x={-40} y={cy + 3.5 + at * 3} textAnchor="middle" className="fill-destructive text-[11px] font-bold">
                  ×
                </text>
              </g>
            ))}
            {value && (
              <foreignObject x={width - GRAPH.value - 8} y={cy - GRAPH.row / 2 + 1} width={GRAPH.value} height={GRAPH.row - 2} data-control data-port={port.path}>
                <div className="flex h-full w-full items-center text-[10px] [&_button]:h-5 [&_input]:h-5 [&_input]:text-[10px] [&_[data-slot=select-trigger]]:h-5">
                  <PortValue store={store} path={port.path} name={port.name} definition={definedInput(definition, port.name)} animated={state.varying.has(port.path)} />
                </div>
              </foreignObject>
            )}
          </g>
        );
      })}
    </g>
  );
}, (a, b) => a.store === b.store && a.placed === b.placed && sameState(a.state, b.state));

/** An unconnected input's value on its row: read again as time moves only when it may vary. */
function PortValue({ store, path, name, definition, animated }: { store: EditorStore; path: string; name: string; definition: ReturnType<typeof definedInput>; animated: boolean }) {
  const time = useTime(store, animated);
  return <ShaderInputField store={store} path={path} name={name} definition={definition} time={time} at={animated ? (time ?? null) : null} />;
}

/** A connection's curve, from its source's row to its destination's; selected, lit or wrong. */
export function EdgeView({ edge, id, lit, selected, problem }: { edge: PlacedEdge; id: string; lit: boolean; selected: boolean; problem: string | null }) {
  const bend = Math.max(40, Math.abs(edge.x2 - edge.x1) / 2);
  const d = `M ${edge.x1} ${edge.y1} C ${edge.x1 + bend} ${edge.y1}, ${edge.x2 - bend} ${edge.y2}, ${edge.x2} ${edge.y2}`;
  const from = edge.edge.from.slice(edge.edge.from.lastIndexOf("/") + 1);
  const to = edge.edge.to.slice(edge.edge.to.lastIndexOf("/") + 1);
  return (
    <g data-edge={id} data-focus={id} tabIndex={-1} role="button" aria-label={`connection ${from} to ${to}${problem === null ? "" : `, problem: ${problem}`}`} className="cursor-pointer outline-none">
      <title>{`${edge.edge.from} → ${edge.edge.to}${problem === null ? "" : `\n${problem}`}\nClick to select; Delete or the context menu disconnects`}</title>
      <path d={d} className="fill-none stroke-transparent" strokeWidth={12} vectorEffect="non-scaling-stroke" pointerEvents="stroke" />
      <path
        d={d}
        className={cn("pointer-events-none fill-none", problem !== null ? "stroke-destructive" : selected ? "stroke-primary" : lit ? "stroke-primary" : "stroke-muted-foreground/35")}
        strokeDasharray={problem !== null ? "5 4" : undefined}
        strokeWidth={selected ? 3 : lit || problem !== null ? 1.75 : 1.25}
        vectorEffect="non-scaling-stroke"
      />
    </g>
  );
}

/** A menu open at a point of the drawing's frame. */
export interface OpenMenu {
  /** Where, in the frame's pixels. */
  readonly x: number;
  readonly y: number;
  /** Its heading: what the items act on, and where edits go. */
  readonly title: string;
  readonly entries: readonly MenuEntry[];
}

/** One entry of a menu: an item that acts (with its detail as a second line), or a submenu. */
function Entry({ entry, act }: { entry: MenuEntry; act: (action: GraphAction) => void }) {
  if (entry.entries !== undefined) {
    return (
      <DropdownMenuSub>
        <DropdownMenuSubTrigger id={entry.id}>{entry.label}</DropdownMenuSubTrigger>
        <DropdownMenuSubContent aria-label={entry.label} className="max-h-80 max-w-96">
          {entry.entries.map((each) => (
            <Entry key={each.id} entry={each} act={act} />
          ))}
        </DropdownMenuSubContent>
      </DropdownMenuSub>
    );
  }
  return (
    <DropdownMenuItem
      id={entry.id}
      textValue={entry.detail === undefined ? entry.label : `${entry.label}: ${entry.detail}`}
      isDisabled={entry.disabled === true}
      variant={entry.destructive === true ? "destructive" : "default"}
      onAction={() => entry.action !== undefined && act(entry.action)}
      className="flex-col items-start gap-0"
    >
      <span>{entry.label}</span>
      {entry.detail !== undefined && <span className="max-w-80 text-[10px] leading-tight whitespace-normal text-muted-foreground">{entry.detail}</span>}
    </DropdownMenuItem>
  );
}

/** The entries of a menu, as items and submenus. */
export function MenuEntries({ entries, act }: { entries: readonly MenuEntry[]; act: (action: GraphAction) => void }) {
  return (
    <>
      {entries.map((entry) => (
        <Entry key={entry.id} entry={entry} act={act} />
      ))}
    </>
  );
}

/** `menu` open at its point; choosing an item acts and closes it. */
export function GraphMenu({ menu, act, onClose }: { menu: OpenMenu; act: (action: GraphAction) => void; onClose: () => void }) {
  const anchor = useRef<HTMLSpanElement>(null);
  return (
    <>
      <span ref={anchor} className="pointer-events-none absolute size-px" style={{ left: menu.x, top: menu.y }} />
      <Popover
        triggerRef={anchor}
        isOpen
        onOpenChange={(open) => !open && onClose()}
        placement="bottom start"
        offset={2}
        className="z-50 max-w-96 min-w-48 overflow-y-auto rounded-lg bg-popover p-1 text-popover-foreground shadow-md ring-1 ring-foreground/10 outline-none **:data-[slot$=-item]:data-focused:bg-foreground/10"
      >
        <Menu aria-label={menu.title} autoFocus="first" className="outline-hidden">
          <MenuSection>
            <Header className="max-w-80 px-2 py-1 text-[11px] text-muted-foreground">{menu.title}</Header>
            {menu.entries.map((entry) => (
              <Entry
                key={entry.id}
                entry={entry}
                act={(action) => {
                  onClose();
                  act(action);
                }}
              />
            ))}
          </MenuSection>
        </Menu>
      </Popover>
    </>
  );
}
