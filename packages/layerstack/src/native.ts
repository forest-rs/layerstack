// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/**
 * `@forest-rs/layerstack/native`: the shapes of the native module the
 * package wraps, for passing a module built by another crate that links
 * `layerstack_wasm` to {@link loadLayerstack}. They follow the module's
 * ABI (`ABI`), not the package's API: an app uses the root export.
 *
 * @module
 */

export type { NativeArrayHandle } from "./arrays.js";
export type { NativeEditBatch } from "./edits.js";
export type { NativeModule, NativeReceipt, NativeStage } from "./stage.js";
