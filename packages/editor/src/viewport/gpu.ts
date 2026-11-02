// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's one WebGPU renderer, shared by every canvas that draws: the
// viewport and the material previews. A WebGPU device is costly (its own
// queue, pipeline cache and memory), so there is one, and each canvas is a
// `CanvasTarget` of it, set before each of its renders
// (`WebGPURenderer.setCanvasTarget`). The editor requires WebGPU: there is
// no WebGL backend to fall back to (`support.ts` says why a page has none,
// before three.js loads).
//
// A device can be lost (a GPU reset, a driver update, the system reclaiming
// it). A lost device's renderer cannot draw again, and everything uploaded
// with it is gone: the next `sharedRenderer` makes a new one, and each
// user, told by `onRendererLost`, makes again what it drew from the CPU
// copies it keeps (the viewport fills its released buffers again first).
import { WebGPURenderer } from "three/webgpu";

let current: Promise<WebGPURenderer> | null = null;
const lostListeners = new Set<() => void>();

/**
 * The shared renderer, made and initialized at the first call (and again
 * after a lost device). Rejects when WebGPU cannot start: three.js would
 * fall back to WebGL 2, which the editor does not draw with, so a renderer
 * that did is disposed and refused.
 */
export function sharedRenderer(): Promise<WebGPURenderer> {
  current ??= start().catch((error: unknown) => {
    current = null;
    throw error;
  });
  return current;
}

/** Calls `listener` when the shared renderer's device is lost; returns the call that stops it. */
export function onRendererLost(listener: () => void): () => void {
  lostListeners.add(listener);
  return () => lostListeners.delete(listener);
}

async function start(): Promise<WebGPURenderer> {
  // Multisampled (4×) as the WebGL renderer's default framebuffer was.
  const renderer = new WebGPURenderer({ antialias: true });
  await renderer.init();
  if ((renderer.backend as { isWebGPUBackend?: boolean }).isWebGPUBackend !== true) {
    renderer.dispose();
    throw new Error("WebGPU did not start: this browser offered only WebGL");
  }
  renderer.onDeviceLost = (info) => {
    console.warn(`WebGPU device lost (${info.reason ?? "unknown reason"}): ${info.message}`);
    const lost = current;
    current = null;
    void lost?.then((renderer) => renderer.dispose());
    for (const listener of lostListeners) listener();
  };
  return renderer;
}
