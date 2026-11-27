// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Stagehand's notes on a UsdPreviewSurface material, in words: what the
// viewport could not draw as authored, and what it drew instead.
import type { MaterialNote } from "@forest-rs/layerstack_stagehand";

/** `note` as a sentence without its full stop, naming the input (or prim) it is about. */
export function describeMaterialNote(note: MaterialNote): string {
  const at = note.input ?? note.prim ?? "the material";
  switch (note.kind) {
    case "noSurface":
      return "no surface output resolves to a shader";
    case "unsupportedSurface":
      return `the surface ${note.id ?? "shader"} is not UsdPreviewSurface, so it is not drawn`;
    case "unsupportedShader":
      return `${at} reads ${note.id ?? note.prim ?? "a shader"}, which is not evaluated; its fallback is drawn`;
    case "unresolvedConnection":
      return `${at} connects to something that is not a shading attribute`;
    case "noValue":
      return `${at} has no value at this time; its fallback is drawn`;
    case "cycle":
      return `${at} connects back to itself`;
    case "multipleSources":
      return `${at} has several sources; its fallback is drawn`;
    case "invalidValue":
      return `${at} has a value of the wrong type or an unknown token; its fallback is drawn`;
    case "missingTexture":
      return `${at} reads a texture whose file is empty or cannot be anchored`;
    case "unsupportedOutput":
      return `${at} reads the output ${note.output ?? "?"}, which its shader does not define in that shape; its fallback is drawn`;
    case "channelMismatch":
      return `${at} reads the one-channel output ${note.output ?? "?"}; its fallback is drawn`;
    case "unsupported":
      return `${at} is connected in a way the viewport cannot draw; its fallback is drawn`;
  }
}
