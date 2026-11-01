// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// An Xformable prim's transform as translation, rotation, scale and pivot,
// read and authored as OpenUSD's `UsdGeomXformCommonAPI` does: each field
// authors its one component (`setTranslate`, `setRotate`, ...), adding
// only that op. Ops that are not a common stack are shown as the stage
// factors them, read-only, with why.
import type { CommonTransform, Edit, Prim, Stage } from "@forest-rs/layerstack";
import { formatNumber } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { CommitNumber } from "./fields.tsx";
import { Section } from "./Section.tsx";

type Component = "translate" | "rotate" | "scale" | "pivot";

const ROWS: readonly [Component, string, keyof CommonTransform][] = [
  ["translate", "Translate", "translation"],
  ["rotate", "Rotate", "rotation"],
  ["scale", "Scale", "scale"],
  ["pivot", "Pivot", "pivot"],
];

export function TransformSection({ store, stage, prim }: { store: EditorStore; stage: Stage; prim: Prim }) {
  const xformable = stage.xformable(prim.path);
  if (xformable === null) return null;
  const read = xformable.commonTransform(store.time);
  // An animated transform is edited by a sample at the current frame.
  const at = xformable.transformTimeSamples.length > 0 && store.time !== undefined ? store.time : undefined;
  const editable = read.compatible && !prim.instanceProxy;
  const author = (component: Component, value: [number, number, number]) => {
    const time = at === undefined ? {} : { time: at };
    const edit: Edit =
      component === "rotate"
        ? { type: "setRotate", path: prim.path, value, rotationOrder: read.rotationOrder, ...time }
        : { type: component === "translate" ? "setTranslate" : component === "scale" ? "setScale" : "setPivot", path: prim.path, value, ...time };
    store.apply(`${component} ${prim.path}${at === undefined ? "" : ` at ${at}`}`, [edit]);
  };
  const note =
    read.source === "decomposed"
      ? "Its ops are not a common stack: these values are factored from its matrix, and editing them here would be refused."
      : read.source === "accumulated"
        ? "Its ops reduce to a common stack; editing them here would be refused until they are one."
        : prim.instanceProxy
          ? "An instance proxy is read-only."
          : null;
  return (
    <Section title="Transform" aside={at === undefined ? undefined : <span className="text-[10px] text-amber-500">sample at {at}</span>}>
      <div className="grid grid-cols-[minmax(0,2fr)_minmax(0,3fr)] items-center gap-x-2 gap-y-1">
        {ROWS.map(([component, label, field]) => {
          const values = read[field] as readonly [number, number, number];
          return (
            <div key={component} className="contents">
              <span className="px-1 text-xs">
                {label}
                {component === "rotate" && <span className="ml-1 text-[10px] text-muted-foreground">{read.rotationOrder}</span>}
              </span>
              <div className="flex min-w-0 gap-1">
                {values.map((value, axis) => (
                  <CommitNumber
                    key={axis}
                    label={`${label} ${"XYZ"[axis]}`}
                    value={formatNumber(value)}
                    disabled={!editable}
                    onCommit={(number) => author(component, values.map((old, index) => (index === axis ? number : old)) as [number, number, number])}
                  />
                ))}
              </div>
            </div>
          );
        })}
      </div>
      {note !== null && <p className="mt-1.5 text-[11px] text-muted-foreground">{note}</p>}
    </Section>
  );
}
