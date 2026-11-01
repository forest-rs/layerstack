// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The material preview in the inspector: the material on a sphere, read
// again after each recomposition (so an edited input shows), with what
// kind of surface it is and what the drawing approximates. three.js loads
// in its own chunk, as the viewport's does.
import { useEffect, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/utils";
import type { Stage } from "@forest-rs/layerstack";
import { readMaterial } from "@forest-rs/layerstack_stagehand";
import { readOpenPbr } from "../model/openpbr.ts";
import { resolveAsset } from "../model/sources.ts";
import type { EditorStore } from "../store.ts";
import type { MaterialPreview as Preview, PreviewSource } from "../viewport/materialPreview.ts";

/** What the preview draws for a material, and what it says about it. */
interface Reading {
  readonly source: PreviewSource | null;
  readonly summary: string;
  readonly notes: readonly string[];
}

function read(stage: Stage, material: string, time: number | undefined): Reading {
  const record = readMaterial(stage, material, time, []);
  if (record !== null && record.item.surface !== null) {
    return { source: { kind: "previewSurface", item: record.item }, summary: "UsdPreviewSurface, as the viewport draws it", notes: [] };
  }
  const openPbr = readOpenPbr(stage, material, time);
  if (openPbr !== null) {
    return {
      source: { kind: "openPbr", surface: openPbr },
      summary: "OpenPBR, approximated by three.js's physical material",
      notes: openPbr.notes,
    };
  }
  const shader = stage.material(material)?.surface().shader ?? null;
  const id = shader === null ? null : stage.shader(shader)?.id;
  return { source: null, summary: id ? `${id} is not a surface the preview draws` : "No surface the preview draws", notes: [] };
}

/**
 * `material` drawn on a sphere (the inspector's, with what it
 * approximates) or, with `shape` `shaderBall`, on a shader ball filling
 * its container (the viewport's material view).
 */
export function MaterialPreview({
  store,
  stage,
  material,
  shape = "sphere",
}: {
  store: EditorStore;
  stage: Stage;
  material: string;
  shape?: "sphere" | "shaderBall";
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const recompositions = store.session?.changeLog.length ?? 0;
  const time = store.time;
  const reading = useMemo(() => read(stage, material, time), [stage, material, time, recompositions]);

  useEffect(() => {
    let disposed = false;
    let made: Preview | null = null;
    void import("../viewport/materialPreview.ts").then(
      ({ MaterialPreview: Made }) => {
        if (disposed || canvas.current === null) return;
        try {
          const source = () => store.source;
          made = new Made(
            canvas.current,
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? null : scene.fetch(await resolveAsset(scene, identifier, anchor));
            },
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? false : scene.exists(await resolveAsset(scene, identifier, anchor));
            },
            shape,
          );
          setPreview(made);
        } catch (error) {
          setFailed(error instanceof Error ? error.message : String(error));
        }
      },
      (error: unknown) => setFailed(String(error)),
    );
    return () => {
      disposed = true;
      made?.dispose();
    };
  }, [store, shape]);

  useEffect(() => preview?.show(reading.source), [preview, reading]);

  const drag = useRef<{ x: number; y: number } | null>(null);
  const ball = shape === "shaderBall";
  return (
    <div className={ball ? "relative h-full w-full" : "mb-2"}>
      <canvas
        ref={canvas}
        aria-label={`${material} on a ${ball ? "shader ball" : "sphere"}`}
        className={cn(
          "w-full cursor-grab touch-none active:cursor-grabbing",
          ball ? "h-full bg-[radial-gradient(circle_at_50%_35%,#3a3a36,#20201e_70%)]" : "h-40 rounded-md bg-[radial-gradient(circle_at_50%_40%,var(--color-muted),transparent_70%)]",
        )}
        onPointerDown={(event) => {
          event.currentTarget.setPointerCapture(event.pointerId);
          drag.current = { x: event.clientX, y: event.clientY };
        }}
        onPointerMove={(event) => {
          if (drag.current === null) return;
          preview?.turn(event.clientX - drag.current.x, event.clientY - drag.current.y);
          drag.current = { x: event.clientX, y: event.clientY };
        }}
        onPointerUp={() => (drag.current = null)}
      />
      {ball ? (
        <p className="pointer-events-none absolute bottom-2 left-2 max-w-[70%] truncate rounded-md bg-black/40 px-2 py-1 text-[11px] text-white/80 backdrop-blur-sm">
          {failed ?? `${material} · ${reading.summary}`}
        </p>
      ) : (
        <p className="mt-1 text-[11px] text-muted-foreground">{failed ?? reading.summary}</p>
      )}
      {!ball && reading.source?.kind === "openPbr" && (
        <details className="text-[11px] text-muted-foreground">
          <summary className="cursor-pointer hover:text-foreground">What the preview approximates</summary>
          <ul className="mt-1 ml-3 list-disc space-y-0.5">
            <li>Base, specular, coat, fuzz, emission, transmission, opacity and thin film map to their nearest three.js physical inputs; float images are read as grey.</li>
            <li>Specular weight is its default value when textured; coat colour, coat normal and subsurface are not drawn.</li>
            <li>Emission luminance is divided by 1000 (the preview's lights are not in nits).</li>
            {reading.notes.map((note) => (
              <li key={note}>{note}.</li>
            ))}
          </ul>
        </details>
      )}
    </div>
  );
}
