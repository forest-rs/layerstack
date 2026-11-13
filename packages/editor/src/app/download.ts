// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Handing bytes to the person as a file: a Blob URL and an anchor's
// `download`, the browser saving it where it saves downloads.
import type { Layer, LayerFormat } from "@forest-rs/layerstack";
import { fileName } from "../model/files.ts";
import type { EditorStore } from "../store.ts";

/** The media type of a file the editor writes, by its name's extension. */
function mediaType(name: string): string {
  if (/\.usdz$/i.test(name)) return "model/vnd.usdz+zip";
  if (/\.usda$/i.test(name)) return "text/plain";
  return "application/octet-stream";
}

/** Downloads `bytes` as a file named `name`. */
export function download(name: string, bytes: Uint8Array): void {
  const url = URL.createObjectURL(new Blob([bytes.slice()], { type: mediaType(name) }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = name;
  anchor.rel = "noopener";
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  // The browser has the file once the click is handled; the URL can go after.
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

/**
 * Downloads several files, one after another with a pause between: a
 * browser that would drop clicks in quick succession sees them apart, and
 * asks once whether the page may download several files.
 */
export async function downloadAll(files: readonly { readonly name: string; readonly bytes: Uint8Array }[]): Promise<void> {
  for (const [index, file] of files.entries()) {
    if (index > 0) await new Promise((resolve) => setTimeout(resolve, 250));
    download(file.name, file.bytes);
  }
}

/** Downloads `layer` of the open stage as `format`, named after it (`robot.usdc`); a failure is a notice. */
export function exportLayerFile(store: EditorStore, layer: Layer, format: LayerFormat): void {
  const bytes = store.exportLayer(layer, format);
  if (bytes !== null) download(fileName(layer.identifier, format), bytes);
}
