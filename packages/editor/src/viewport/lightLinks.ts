// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Which of the viewport's lights light a draw, apart from three.js so it can
// be tested in Node. A draw's materials are lit by the lights its light
// links leave it in (`ViewportScene.unlit` names the `UsdLux` lights that
// leave it out); a light the viewport adds itself (the camera light, the
// ambient) has no `UsdLux` light and lights every draw. Draws lit by the
// same lights share one set (`lightSetKey`), and so one shader per material
// shape.

/**
 * The lights of `all` that light a draw `unlit` leaves out of the `UsdLux`
 * lights it names: those whose key (`keyOf`) it does not name, and those
 * without a key. `all` itself (the same array) when that is every one, so
 * a caller can tell "every light" from a set by identity.
 */
export function linkedLights<T>(all: T[], keyOf: (light: T) => string | undefined, unlit: ReadonlySet<string>): T[] {
  const lit = all.filter((light) => {
    const key = keyOf(light);
    return key === undefined || !unlit.has(key);
  });
  return lit.length === all.length ? all : lit;
}

/** The key of a set of lights by their ids, whatever their order: equal sets, equal keys. */
export function lightSetKey(ids: readonly number[]): string {
  return [...ids].sort((a, b) => a - b).join(" ");
}
