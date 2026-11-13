// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where the editor is with its scene, and how a module's trap is told
// from any other failure.

/** Where the editor is with its scene. */
export type EditorStatus =
  | { readonly kind: "empty" }
  | { readonly kind: "opening"; readonly label: string }
  /**
   * Loading payloads of the open scene (`label` names them), and fetching
   * what they want; or fetching what another action wants (an arc added),
   * which `title` says.
   */
  | { readonly kind: "loading"; readonly label: string; readonly title?: string }
  | { readonly kind: "ready" }
  | { readonly kind: "failed"; readonly label: string; readonly message: string }
  /**
   * A Wasm module trapped: out of memory (`module.out_of_memory`, or a raw
   * trap the stagehand module reports) or a panic. Its instance is dead
   * for the rest of the page, so the editor stops calling into it; only a
   * reload brings it back. `label` is the scene being opened, if any.
   */
  | { readonly kind: "lost"; readonly label: string | null; readonly outOfMemory: boolean; readonly message: string };

/**
 * Whether `error` (or an error it was caused by) is a Wasm module's trap,
 * after which the module cannot be used again: a `LayerstackError` with a
 * `module.trapped` or `module.out_of_memory` code, or a raw
 * `WebAssembly.RuntimeError`.
 */
export function moduleTrap(error: unknown): { readonly outOfMemory: boolean; readonly message: string; readonly raw: boolean } | null {
  for (let cause = error, depth = 0; cause instanceof Error && depth < 4; cause = cause.cause, depth += 1) {
    const code = (cause as { code?: unknown }).code;
    if (code === "module.out_of_memory" || code === "module.trapped") {
      return { outOfMemory: code === "module.out_of_memory", message: cause.message, raw: false };
    }
    if (cause instanceof WebAssembly.RuntimeError) return { outOfMemory: false, message: cause.message, raw: true };
  }
  return null;
}
