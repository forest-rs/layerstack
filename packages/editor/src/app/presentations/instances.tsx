// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The built-in presentation of a point instancer's instance, and the
// commands on it. An instance is live wherever it is named (a row of its
// instancer's table, the chosen instance below it, which a viewport pick
// chooses): a click selects it (its row, and its outline in the viewport),
// and its context menu frames it, selects or draws alone its prototype,
// hides or deactivates it (`invisibleIds`, `inactiveIds`, one edit where
// edits go) and copies its id, index or position.
import { instanceId, type InstancerColumns } from "../../model/instancer.ts";
import { formatNumber } from "../../model/values.ts";
import { type InstanceObject, type PresentationContext, type Presented, type PresentProps, registerCommand, registerPresentation } from "../../presentations/registry.ts";
import { Live } from "./Present.tsx";

/** Instance `index` of the point instancer at `instancer`, as presented: its id and prototype read from `columns`. */
export function presentedInstance(instancer: string, columns: InstancerColumns, index: number): Presented<"instance"> {
  const prototype = columns.prototypes[columns.protoIndices[index]!] ?? null;
  return { kind: "instance", instancer, index, id: instanceId(columns, index), prototype };
}

/** A prototype's name: the last element of its path. */
const nameOf = (path: string | null) => (path === null ? "no prototype" : path.slice(path.lastIndexOf("/") + 1));

/** The instance's position (`positions`), when authored for it. */
function positionOf(object: InstanceObject, context: PresentationContext): readonly number[] | null {
  const positions = context.store.instancerColumns(object.instancer)?.positions ?? null;
  if (positions === null || positions.length < (object.index + 1) * 3) return null;
  return [0, 1, 2].map((c) => positions[object.index * 3 + c]!);
}

/** One line: its index, id and prototype (`#2 · id 3 · Stone`). */
function InstanceInline({ object, context }: PresentProps<"instance">) {
  return (
    <Live object={object} context={context} title={`instance ${object.index} of ${object.instancer}: click to select, right-click for more`}>
      #{object.index} · id {String(object.id)} · {nameOf(object.prototype)}
    </Live>
  );
}

/** Its tooltip: which instancer, index, id and prototype, where it is, whether it is hidden or inactive, and what a click does. */
function InstanceTooltip({ object, context }: PresentProps<"instance">) {
  const columns = context.store.instancerColumns(object.instancer);
  const position = positionOf(object, context);
  const facts = [
    `id ${object.id}`,
    object.prototype ?? "names no prototype",
    position === null ? null : `at (${position.map((v) => formatNumber(Number(v.toPrecision(4)))).join(", ")})`,
    columns?.invisibleIds.includes(object.id) === true ? "invisible" : null,
    columns?.inactiveIds.includes(object.id) === true ? "inactive" : null,
  ].filter((fact) => fact !== null);
  return (
    <div className="flex flex-col gap-0.5 text-left">
      <div className="font-mono break-all">
        {object.instancer} #{object.index}
      </div>
      <div className="opacity-70">{facts.join(" · ")}</div>
      <div className="opacity-70">Click to select it; right-click for more.</div>
    </div>
  );
}

const copy = (text: string) => void navigator.clipboard?.writeText(text);

/** Registers the built-in presentation of instances, and the commands on them. */
export function registerInstancePresentations(): void {
  registerPresentation({ id: "builtin.instance", kind: "instance", type: "instance", inline: InstanceInline, tooltip: InstanceTooltip });

  const columnsOf = (object: InstanceObject, context: PresentationContext) => context.store.instancerColumns(object.instancer);
  const exists = (path: string | null, context: PresentationContext) => {
    if (path === null) return false;
    try {
      return context.stage.prim(path) !== null;
    } catch {
      return false;
    }
  };
  registerCommand({
    id: "builtin.instance.select",
    kind: "instance",
    type: "instance",
    title: "Select (and outline it)",
    applies: (object, context) => columnsOf(object, context) !== null,
    run: (object, { store }) => store.selectInstance(object.instancer, object.index),
  });
  registerCommand({
    id: "builtin.instance.frame",
    kind: "instance",
    type: "instance",
    title: "Frame in the viewport",
    applies: (object, context) => columnsOf(object, context) !== null,
    run: (object, { store }) => store.selectInstance(object.instancer, object.index, true),
  });
  registerCommand({
    id: "builtin.instance.prototype",
    kind: "instance",
    type: "instance",
    title: "Select its prototype",
    applies: (object, context) => exists(object.prototype, context),
    run: (object, { store }) => store.select(object.prototype),
  });
  registerCommand({
    id: "builtin.instance.alone",
    kind: "instance",
    type: "instance",
    title: "Draw its prototype alone (asset view)",
    applies: (object, context) => exists(object.prototype, context),
    run: (object, { store }) => store.drawAlone(object.prototype!),
  });
  registerCommand({
    id: "builtin.instance.hide",
    kind: "instance",
    type: "instance",
    title: "Hide it (invisibleIds)",
    applies: (object, context) => columnsOf(object, context)?.invisibleIds.includes(object.id) === false,
    run: (object, { store }) => store.setInstanceVisible(object.instancer, object.id, false),
  });
  registerCommand({
    id: "builtin.instance.show",
    kind: "instance",
    type: "instance",
    title: "Show it (invisibleIds)",
    applies: (object, context) => columnsOf(object, context)?.invisibleIds.includes(object.id) === true,
    run: (object, { store }) => store.setInstanceVisible(object.instancer, object.id, true),
  });
  registerCommand({
    id: "builtin.instance.deactivate",
    kind: "instance",
    type: "instance",
    title: "Deactivate it (inactiveIds)",
    applies: (object, context) => columnsOf(object, context)?.inactiveIds.includes(object.id) === false,
    run: (object, { store }) => store.setInstanceActive(object.instancer, object.id, false),
  });
  registerCommand({
    id: "builtin.instance.activate",
    kind: "instance",
    type: "instance",
    title: "Activate it (inactiveIds)",
    applies: (object, context) => columnsOf(object, context)?.inactiveIds.includes(object.id) === true,
    run: (object, { store }) => store.setInstanceActive(object.instancer, object.id, true),
  });
  registerCommand({ id: "builtin.instance.copy-id", kind: "instance", type: "instance", title: "Copy its id", run: (object) => copy(String(object.id)) });
  registerCommand({ id: "builtin.instance.copy-index", kind: "instance", type: "instance", title: "Copy its index", run: (object) => copy(String(object.index)) });
  registerCommand({
    id: "builtin.instance.copy-position",
    kind: "instance",
    type: "instance",
    title: "Copy its position",
    applies: (object, context) => positionOf(object, context) !== null,
    run: (object, context) => copy(`(${positionOf(object, context)!.join(", ")})`),
  });
}
