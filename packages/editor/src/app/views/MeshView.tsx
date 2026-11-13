// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The mesh's view: the mesh alone in a preview (orbit by dragging, zoom by
// scrolling), what stagehand made of it (faces, points, triangles,
// concave and degenerate faces, holes, normals authored or computed), its
// subdivision scheme, orientation and `doubleSided`, its primvars with
// interpolation and element size, and its face subsets with their face
// counts, a hovered or focused subset highlighted in the preview.
//
// Nothing is triangulated again for a mesh the viewport draws: the facts
// are its draw list buffers' report, and the preview draws the geometry the
// viewport uploaded (`Viewport.sharedGeometry`), the same GPU buffers. A
// mesh the viewport does not draw (under the budget, outside the draw
// scope, or with Nothing drawn), and wireframe and normals (which read
// vertices on the CPU, which the viewport released once uploaded), build
// this mesh alone with stagehand, once per change. The preview stands the
// mesh as the scene does: about the stage's up axis, taken into the mesh's
// own space (`usePreviewOrientation`).
import { useEffect, useMemo, useRef, useState } from "react";
import type { Prim } from "@forest-rs/layerstack";
import { type DrawRange, type MeshReport, readMesh } from "@forest-rs/layerstack_stagehand";
import { cn } from "@/lib/utils";
import { releaseValue } from "../../model/values.ts";
import type { PresentationContext, PresentProps } from "../../presentations/registry.ts";
import { localView, type ViewOrientation } from "../../viewport/framing.ts";
import type { CpuMesh, MeshPreview, MeshPreviewStyle } from "../../viewport/meshPreview.ts";
import type { ToneMapChoice } from "../../viewport/toneMap.ts";
import { Live, Present, usePrimRevision } from "../presentations/Present.tsx";
import { Section } from "../Section.tsx";
import { useTimeOf } from "../context.ts";
import { countRender } from "../renders.ts";

/** This mesh built alone: its buffers on the CPU, its draw ranges and subsets, and its report. */
interface Built {
  readonly cpu: CpuMesh;
  readonly ranges: readonly DrawRange[];
  readonly subsets: readonly string[];
  readonly report: MeshReport | null;
}

/** Builds the mesh at `path` alone with stagehand, as the draw list would; `null` when it cannot be read. */
function build(context: PresentationContext, path: string): Built | null {
  const input = readMesh(context.stage, path, context.time === undefined ? {} : { time: context.time });
  if (input === null) return null;
  const mesh = context.store.stagehand.buildMesh(input);
  try {
    return {
      cpu: { positions: mesh.positions, normals: mesh.normals, st: mesh.st, colors: mesh.colors, indices: mesh.indices },
      ranges: mesh.ranges,
      subsets: (input.subsets ?? []).map((subset) => subset.path ?? ""),
      report: mesh.report,
    };
  } finally {
    mesh.dispose();
  }
}

/**
 * The stage's up and the viewport's view direction in the prim at
 * `path`'s own space ({@link localView}): what a preview of its
 * untransformed geometry orbits about and looks from, so a Z-up asset
 * turned upright in a Y-up stage stands upright in it too. Read again when
 * a change reaches the prim, the time moves or another stage opens.
 */
export function usePreviewOrientation(context: PresentationContext, path: string): ViewOrientation {
  const revision = usePrimRevision(context.store, path);
  const up = context.store.upAxis;
  const { stage } = context;
  const time = useTimeOf(context.store, stage, path, { attributes: [], worldTransform: true });
  return useMemo(() => {
    let world: Float64Array | null = null;
    try {
      world = stage.worldTransforms([path], time === undefined ? {} : { time });
    } catch {
      // No prim there any more: the stage's own orientation.
    }
    return localView(up, world);
  }, [stage, path, revision, time, up]);
}

/** A face subset of the mesh: its path, family and face count. */
interface Subset {
  readonly path: string;
  readonly family: string | null;
  readonly faces: number;
}

function subsetsOf(prim: Prim, time: number | undefined): Subset[] {
  return prim.geomSubsets({ elementType: "face", ...(time === undefined ? {} : { time }) }).map((subset) => {
    const faces = subset.indices?.length ?? 0;
    subset.indices?.release();
    return { path: subset.path, family: subset.familyName, faces };
  });
}

/** A uniform token or bool of the mesh, as text. */
function scalar(prim: Prim, name: string, time: number | undefined): string | null {
  const value = prim.attribute(name)?.get(time) ?? null;
  const text = value?.kind === "scalar" ? String(value.value) : null;
  releaseValue(value);
  return text;
}

export function MeshView({ object, context }: PresentProps<"prim">) {
  countRender("MeshView");
  const { store } = context;
  const prim = object.prim;
  const path = prim.path;
  const revision = usePrimRevision(store, path);
  // Its attributes and its subsets'; the preview's orientation asks about the transforms itself.
  const time = useTimeOf(store, context.stage, path, { attributes: null, descendants: true });
  // The draw list's buffers for this mesh, when the viewport draws it.
  const view = store.scene.items.find((candidate) => candidate.item.prim === path && candidate.kind === "mesh");
  const buffers = view?.buffers ?? null;
  const shared = buffers === null ? null : (store.viewport?.sharedGeometry(buffers.key) ?? null);
  const [style, setStyle] = useState<Omit<MeshPreviewStyle, "highlight" | "doubleSided">>({ checker: false, wireframe: false, normals: false });
  const [hovered, setHovered] = useState<string | null>(null);
  const needsCpu = shared === null || style.wireframe || style.normals;
  const built = useMemo(() => (needsCpu ? build(context, path) : null), [needsCpu, path, revision, time, buffers?.revision]);
  const subsets = useMemo(() => subsetsOf(prim, time), [path, revision, time]);
  const primvars = useMemo(() => prim.primvars(), [path, revision]);
  const hasSt = primvars.some((primvar) => primvar.name === "st");
  const bounds = useMemo(() => {
    const box = context.stage.untransformedBounds([path], { purposes: ["default", "proxy", "render"], ...(time === undefined ? {} : { time }) }).alignedRanges;
    return box[0]! <= box[3]! ? Array.from(box.subarray(0, 6)) : [-1, -1, -1, 1, 1, 1];
  }, [path, revision, time]);
  const previewOrientation = usePreviewOrientation(context, path);
  const report = buffers?.mesh.report ?? built?.report ?? null;
  const ranges = shared !== null ? buffers!.mesh.ranges : (built?.ranges ?? []);
  const rangeSubsets = shared !== null ? buffers!.subsets : (built?.subsets ?? []);
  const highlight = useMemo(() => {
    if (hovered === null) return null;
    const position = rangeSubsets.indexOf(hovered);
    const range = ranges.find((candidate) => candidate.subset === position);
    return position < 0 || range === undefined ? null : { first: range.first, count: range.count };
  }, [hovered, ranges, rangeSubsets]);
  const doubleSided = scalar(prim, "doubleSided", time) === "true";
  const orientation = scalar(prim, "orientation", time) ?? "rightHanded";
  const scheme = scalar(prim, "subdivisionScheme", time) ?? "catmullClark";

  return (
    <Section title="Mesh" aside={<span>{shared !== null ? "the viewport's buffers" : built !== null ? "built for this view" : "not readable"}</span>}>
      <Preview source={{ shared, cpu: built?.cpu ?? null, bounds, orientation: previewOrientation }} style={{ ...style, doubleSided, highlight }} toneMap={store.toneMap} />
      <div role="group" aria-label="Preview" className="mt-1 mb-2 flex gap-1">
        {(
          [
            ["checker", "UV checker", "A checker on st"],
            ["wireframe", "Wireframe", "Triangle edges (builds the mesh alone, on the CPU)"],
            ["normals", "Normals", "Vertex normals (builds the mesh alone, on the CPU)"],
          ] as const
        ).map(([key, label, title]) => (
          <button
            key={key}
            type="button"
            title={key === "checker" && !hasSt ? "It has no st primvar to draw a checker on" : title}
            disabled={key === "checker" && !hasSt}
            aria-pressed={style[key]}
            onClick={() => setStyle({ ...style, [key]: !style[key] })}
            className={cn(
              "rounded-md border px-2 py-0.5 text-[11px] text-muted-foreground hover:text-foreground disabled:opacity-50 disabled:hover:text-muted-foreground",
              style[key] && "border-primary/60 bg-primary/10 text-foreground",
            )}
          >
            {label}
          </button>
        ))}
      </div>
      <dl className="mb-2 grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)] gap-x-2 gap-y-0.5 text-[11px]">
        {report !== null && (
          <>
            <Fact label="faces" value={`${report.faces.toLocaleString()}${report.drawnFaces !== report.faces ? ` (${report.drawnFaces.toLocaleString()} drawn)` : ""}`} />
            <Fact label="points" value={report.points.toLocaleString()} />
            <Fact label="triangles" value={report.triangles.toLocaleString()} />
            <Fact label="vertices" value={report.vertices.toLocaleString()} title="Distinct corners: a point with its normal, st and colour" />
            <Fact label="concave faces" value={report.concaveFaces === 0 ? "none" : `${report.concaveFaces.toLocaleString()} (${report.clippedFaces.toLocaleString()} ear-clipped)`} />
            {report.holeFaces > 0 && <Fact label="holes" value={report.holeFaces.toLocaleString()} />}
            {report.degenerateFaces > 0 && <Fact label="degenerate faces" value={report.degenerateFaces.toLocaleString()} />}
            <Fact label="normals" value={report.normals === "authored" ? "authored" : "computed (smooth)"} />
            {report.topologyProblem !== null && <Fact label="not drawn" value={report.topologyProblem.reason} />}
          </>
        )}
        <Fact label="subdivision" value={scheme === "none" ? "none (polygonal)" : `${scheme}${report?.refined === false ? ", drawn as its cage" : ""}`} />
        <Fact label="orientation" value={orientation} />
        <Fact label="double-sided" value={doubleSided ? "yes" : "no"} />
      </dl>
      {primvars.length > 0 && (
        <table className="mb-2 w-full table-fixed text-[11px]">
          <thead className="text-left text-muted-foreground">
            <tr>
              <th className="w-[38%] font-normal">primvar</th>
              <th className="w-[26%] font-normal">type</th>
              <th className="font-normal">interpolation</th>
              <th className="w-[10%] text-right font-normal" title="Element size">size</th>
            </tr>
          </thead>
          <tbody>
            {primvars.map((primvar) => (
              <tr key={primvar.path}>
                <td className="max-w-0 truncate">
                  <Live object={{ kind: "propertyPath", path: primvar.path }} context={context} className="block max-w-full" title={`${primvar.path}: click to explain it`}>
                    {primvar.name}
                  </Live>
                </td>
                <td className="truncate font-mono text-muted-foreground" title={`${primvar.typeName}${primvar.indexed ? ", indexed" : ""}`}>
                  {primvar.typeName}
                  {primvar.indexed && " · indexed"}
                </td>
                <td className="truncate">{primvar.interpolation}</td>
                <td className="text-right tabular-nums">{primvar.elementSize}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {subsets.length > 0 && (
        <ul className="flex flex-col text-[11px]" aria-label="Face subsets" onMouseLeave={() => setHovered(null)}>
          {subsets.map((subset) => {
            const drawn = rangeSubsets.includes(subset.path);
            return (
              <li
                key={subset.path}
                onMouseEnter={() => setHovered(subset.path)}
                onFocus={() => setHovered(subset.path)}
                className={cn("grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-2 rounded px-1 hover:bg-muted", hovered === subset.path && "bg-muted")}
                title={drawn ? "Hover to highlight it in the preview" : "Not a materialBind subset: the draw has no range of its own to highlight"}
              >
                <Present object={{ kind: "primPath", path: subset.path }} context={context} />
                <span className="text-muted-foreground">{subset.family ?? "no family"}</span>
                <span className="text-right tabular-nums">{subset.faces.toLocaleString()} faces</span>
              </li>
            );
          })}
        </ul>
      )}
    </Section>
  );
}

export function Fact({ label, value, title }: { label: string; value: string; title?: string }) {
  return (
    <>
      <dt className="text-muted-foreground" title={title}>
        {label}
      </dt>
      <dd className="truncate font-mono" title={value}>
        {value}
      </dd>
    </>
  );
}

/** The preview canvas: a {@link MeshPreview} on the shared renderer, loaded with three.js after the editor. */
export function Preview({ source, style, toneMap }: { source: Parameters<MeshPreview["show"]>[0]; style: MeshPreviewStyle; toneMap: ToneMapChoice }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [preview, setPreview] = useState<MeshPreview | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    let made: MeshPreview | null = null;
    void Promise.all([import("../../viewport/meshPreview.ts"), import("../../viewport/gpu.ts").then(({ sharedRenderer }) => sharedRenderer())]).then(
      ([{ MeshPreview: Made }, renderer]) => {
        if (disposed || canvas.current === null) return;
        made = new Made(canvas.current, renderer);
        setPreview(made);
      },
      (error: unknown) => setFailed(String(error)),
    );
    return () => {
      disposed = true;
      made?.dispose();
    };
  }, []);
  useEffect(() => preview?.setToneMap(toneMap), [preview, toneMap]);
  const { shared, cpu, bounds, orientation } = source;
  useEffect(() => preview?.show({ shared, cpu, bounds, orientation }, style), [preview, shared?.geometry, cpu, bounds, orientation, style.checker, style.wireframe, style.normals, style.doubleSided, style.highlight?.first, style.highlight?.count]);
  return (
    <>
      <canvas
        ref={canvas}
        aria-label="The mesh alone: drag to orbit, scroll to zoom"
        className="h-48 w-full cursor-grab touch-none rounded-md bg-[radial-gradient(circle_at_50%_40%,var(--color-muted),transparent_75%)] active:cursor-grabbing"
      />
      {failed !== null && <p className="text-[11px] text-destructive">{failed}</p>}
    </>
  );
}
