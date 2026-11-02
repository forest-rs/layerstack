// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Whether this page can draw with WebGPU, asked before three.js loads, so a
// browser without it is told why (`NoWebGPU.tsx`) instead of shown an
// editor whose viewport stays empty.

/** Why WebGPU is not available, or `null` when it is. */
export type WebGpuMissing = "api" | "adapter" | null;

/**
 * `"api"` when the browser has no `navigator.gpu` (no WebGPU, or not in a
 * secure context), `"adapter"` when it has the API but offers no adapter
 * (blocklisted GPU, disabled hardware acceleration), `null` otherwise.
 */
export async function webGpuMissing(): Promise<WebGpuMissing> {
  const gpu = (navigator as { gpu?: { requestAdapter(): Promise<unknown> } }).gpu;
  if (gpu === undefined) return "api";
  try {
    return (await gpu.requestAdapter()) === null ? "adapter" : null;
  } catch {
    return "adapter";
  }
}
