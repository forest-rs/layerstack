// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws the store's draw list on a canvas (in its
// own chunk, loaded after the editor). A click picks the prim drawn
// there; a drag orbits.
import { useEffect, useRef, useState } from "react";
import { useStore } from "./context.ts";
import { updateSummary, viewportSummary } from "../model/draws.ts";
import { resolveAsset } from "../model/sources.ts";
import { SKIN_SLOTS } from "../model/skinning.ts";
import { Timeline } from "./Timeline.tsx";

export function ViewportPane() {
  const store = useStore();
  const canvas = useRef<HTMLCanvasElement>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let disposed = false;
    let dispose = () => {};
    void import("../viewport/viewport3d.ts").then(
      ({ Viewport }) => {
        if (disposed || canvas.current === null) return;
        try {
          const source = () => store.source;
          const viewport = new Viewport(
            canvas.current,
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? null : scene.fetch(await resolveAsset(scene, identifier, anchor));
            },
            async (identifier, anchor) => {
              const scene = source();
              return scene === null ? false : scene.exists(await resolveAsset(scene, identifier, anchor));
            },
          );
          store.attachViewport(viewport);
          dispose = () => {
            store.attachViewport(null);
            viewport.dispose();
          };
        } catch (error) {
          setFailed(error instanceof Error ? error.message : String(error));
        }
      },
      (error: unknown) => setFailed(String(error)),
    );
    return () => {
      disposed = true;
      dispose();
    };
  }, [store]);
  // A click picks; a drag (the orbit controls') does not.
  const down = useRef<{ x: number; y: number } | null>(null);
  const summary = viewportSummary(store.scene);
  const update = store.lastUpdate === null ? null : updateSummary(store.lastUpdate);
  const through = store.through;
  const skinning = store.scene.skinning;
  const skinned = skinning.gpu + skinning.cpu + skinning.failed.length > 0;
  return (
    <div className="flex h-full w-full flex-col">
      <div className="relative min-h-0 w-full flex-1 overflow-hidden bg-[#20201e]">
        <canvas
          ref={canvas}
          className="block h-full w-full outline-none"
          onPointerDown={(event) => {
            down.current = { x: event.clientX, y: event.clientY };
          }}
          onPointerUp={(event) => {
            const start = down.current;
            down.current = null;
            if (start === null || Math.hypot(event.clientX - start.x, event.clientY - start.y) > 4) return;
            const rect = event.currentTarget.getBoundingClientRect();
            store.select(store.viewport?.pick(event.clientX - rect.left, event.clientY - rect.top) ?? null);
          }}
        />
        {failed !== null && (
          <div className="absolute inset-0 flex items-center justify-center text-xs text-muted-foreground">
            The viewport cannot draw here: {failed}
          </div>
        )}
        <div className="pointer-events-none absolute top-2 left-2 rounded-md bg-black/40 px-2 py-1 font-mono text-[11px] text-white/80 backdrop-blur-sm">
          {through === null ? "Free camera" : `Looking through ${through}`}
        </div>
        <div className="pointer-events-none absolute right-2 bottom-2 rounded-md bg-black/40 px-2 py-1 text-right font-mono text-[11px] leading-relaxed text-white/80 tabular-nums backdrop-blur-sm">
          <div>
            {summary.drawn.length} prims drawn · {summary.triangles.toLocaleString()} triangles · {summary.lights.length} lights
          </div>
          {update !== null && (
            <div className="text-white/60">
              last update: +{update.added} −{update.removed} moved {update.moved} refilled {update.refilled} rebuilt {update.rebuilt} · read{" "}
              {update.itemsRead} draws
              {store.lastUpdate?.timeChanged === true && `, ${update.itemsTimeInvariant} left as they were`}
            </div>
          )}
          {skinned && store.lastUpdate !== null && (
            <div className="text-white/60" title={skinning.failed.join("\n")}>
              skinned: posed {store.lastUpdate.skinWork.gpuDraws} on the GPU ({store.lastUpdate.skinWork.joints.toLocaleString()} joints), deformed{" "}
              {store.lastUpdate.skinWork.cpuDraws} on the CPU ({store.lastUpdate.skinWork.cpuPoints.toLocaleString()} points)
              {skinning.cpu > 0 && ` · ${skinning.cpu} with blend shapes or dual quaternions`}
              {skinning.reducedBlocks > 0 && ` · ${skinning.reducedBlocks.toLocaleString()} points kept ${SKIN_SLOTS} joints (dropped ≤ ${skinning.droppedWeight.toFixed(3)})`}
              {skinning.failed.length > 0 && ` · ${skinning.failed.length} drawn at rest`}
            </div>
          )}
        </div>
      </div>
      <Timeline />
    </div>
  );
}
