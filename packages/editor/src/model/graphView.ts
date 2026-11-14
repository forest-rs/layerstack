// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Where the shading graph's drawing is in its frame (`../app/ShadingView.tsx`),
// with no DOM: the view that fits the whole network or keeps names legible
// around a node, and whether a view is still one the editor chose, so a
// frame that changes size (the bottom panel growing as its tab opens) is
// fitted again, while one the person panned or zoomed stays where they
// put it.

/** Where the drawing is: its translation and zoom. */
export interface View {
  readonly x: number;
  readonly y: number;
  readonly scale: number;
}

/** A width and a height: the frame's, or the drawing's extent. */
export interface Size {
  readonly width: number;
  readonly height: number;
}

/** A node's box in the drawing. */
export interface Box extends Size {
  readonly x: number;
  readonly y: number;
}

export const MIN_SCALE = 0.05;
export const MAX_SCALE = 2.5;
/** The smallest zoom a view centred on a node uses: below it names cannot be read. */
export const READABLE = 0.8;
/** The margin kept between the drawing and the frame's edges. */
export const PADDING = 24;

/**
 * The view of a drawing of `extent` in a frame of `box`: the whole drawing
 * fitted (never zoomed past 1), or with `readable` one at least
 * {@link READABLE}, which centres `focus` (its title kept in the frame)
 * when the whole drawing does not fit at that zoom. `null` for a frame
 * with no width yet.
 */
export function fitView(box: Size, extent: Size, readable: boolean, focus: Box | null): View | null {
  if (box.width === 0) return null;
  const whole = Math.min((box.width - 2 * PADDING) / extent.width, (box.height - 2 * PADDING) / extent.height, 1);
  const scale = Math.max(MIN_SCALE, readable ? Math.max(whole, READABLE) : whole);
  if (scale === whole || focus === null) {
    return { scale, x: (box.width - extent.width * scale) / 2, y: Math.max(PADDING, (box.height - extent.height * scale) / 2) };
  }
  const y = Math.max(box.height / 2 - (focus.y + focus.height / 2) * scale, PADDING - focus.y * scale);
  return { scale, x: box.width / 2 - (focus.x + focus.width / 2) * scale, y };
}

/**
 * How the view in place was chosen: by the editor (the first view, or
 * Fit), with what it fitted (`readable`, and the node it centred on, by
 * key); `null` once the person panned, zoomed or the keyboard's focus
 * moved it, which a change of the frame's size then leaves alone.
 */
export type ViewOrigin = { readonly readable: boolean; readonly focus: string | null } | null;

/**
 * The view after the frame changed size to `box`: fitted again as it was
 * when the editor chose the view in place (`origin`), the node it centred
 * on read again (`boxOf`); `null` to keep the view, when the person moved
 * it, or the frame has no width.
 */
export function resizedView(origin: ViewOrigin, box: Size, extent: Size, boxOf: (key: string) => Box | null): View | null {
  if (origin === null) return null;
  return fitView(box, extent, origin.readable, origin.focus === null ? null : boxOf(origin.focus));
}
