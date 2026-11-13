// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a field says when its input did not become an edit: the text was not
// a number. (A refusal by the stage is what the store's edit returns.)

/** Why a field refuses text that is not a number (see {@link parseNumber}). */
export const NOT_A_NUMBER = "Not a number";

/** `text` as a finite number, or `null` when it is empty or not one. */
export function parseNumber(text: string): number | null {
  if (text.trim() === "") return null;
  const number = Number(text);
  return Number.isFinite(number) ? number : null;
}
