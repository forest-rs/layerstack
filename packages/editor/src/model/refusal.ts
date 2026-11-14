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

/** A refusal said under the row of fields whose commit caused it: which row, and why. */
export interface RowRefusal<K> {
  readonly row: K;
  readonly text: string;
}

/**
 * What a section of rows says after a commit in `row` of `field` (its
 * label: `Translate X`), refused for `why` (`undefined` when the commit was
 * taken): the refusal, under that row and naming the field; nothing once
 * a commit is taken. Said as a line, not as the field's title alone,
 * which only a pointer resting on it shows.
 */
export function rowRefusal<K>(row: K, field: string, why: string | undefined): RowRefusal<K> | null {
  return why === undefined ? null : { row, text: `${field}: ${why}` };
}
