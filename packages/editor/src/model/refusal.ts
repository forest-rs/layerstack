// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What a field says when its input did not become an edit: the text was not
// a number, or the stage refused the edit (the store reports a refusal as a
// notice, so the new notice is the reason).

/** Why a field refuses text that is not a number (see {@link parseNumber}). */
export const NOT_A_NUMBER = "Not a number";

/** `text` as a finite number, or `null` when it is empty or not one. */
export function parseNumber(text: string): number | null {
  if (text.trim() === "") return null;
  const number = Number(text);
  return Number.isFinite(number) ? number : null;
}

/** Runs `author`, and returns the message of the notice it pushed (the stage's refusal), or `undefined` when it pushed none. */
export function refusedBy(notices: readonly { readonly message: string }[], author: () => void): string | undefined {
  const before = notices.length;
  author();
  return notices.length > before ? notices[notices.length - 1]!.message : undefined;
}
