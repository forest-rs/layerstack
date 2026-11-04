// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport's tone mapping: usdview's sRGB encoding by default, AgX as
// a choice, each at the camera's exposure, remembered per viewer.
import assert from "node:assert/strict";
import { test } from "node:test";
import { DEFAULT_TONE_MAP, rememberToneMap, rememberedToneMap, TONE_MAP_CHOICES, toneMapOutput } from "../src/viewport/toneMap.ts";

/** Runs `body` with `storage` as `globalThis.localStorage`, then puts the original back. */
function withStorage(storage: unknown, body: () => void): void {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, get: () => storage });
  try {
    body();
  } finally {
    if (original === undefined) delete (globalThis as { localStorage?: unknown }).localStorage;
    else Object.defineProperty(globalThis, "localStorage", original);
  }
}

/** A `Storage` holding what it is given. */
function memoryStorage(): Pick<Storage, "getItem" | "setItem"> {
  const values = new Map<string, string>();
  return { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => void values.set(key, value) };
}

test("usdview's sRGB is the default, AgX the other choice", () => {
  assert.equal(DEFAULT_TONE_MAP, "srgb");
  assert.deepEqual(
    TONE_MAP_CHOICES.map(([choice]) => choice),
    ["srgb", "agx"],
  );
});

test("sRGB is a linear operator (exposure, clamp, encode); AgX its curve", () => {
  assert.equal(toneMapOutput("srgb").operator, "linear");
  assert.equal(toneMapOutput("agx").operator, "agx");
});

test("either choice draws at the camera's linear exposure scale, as Storm's terminal multiplies it", () => {
  for (const [choice] of TONE_MAP_CHOICES) {
    assert.equal(toneMapOutput(choice).exposure, 1);
    assert.equal(toneMapOutput(choice, 0.25).exposure, 0.25);
    assert.equal(toneMapOutput(choice, 4).exposure, 4);
    assert.equal(toneMapOutput(choice, 0).exposure, 0);
  }
});

test("an exposure scale that is no number of zero or more is a camera without exposure", () => {
  for (const scale of [Number.NaN, Number.POSITIVE_INFINITY, -1]) assert.equal(toneMapOutput("srgb", scale).exposure, 1);
});

test("the choice is remembered for the viewer", () => {
  withStorage(memoryStorage(), () => {
    assert.equal(rememberedToneMap(), "srgb");
    rememberToneMap("agx");
    assert.equal(rememberedToneMap(), "agx");
    rememberToneMap("srgb");
    assert.equal(rememberedToneMap(), "srgb");
  });
});

test("an unknown remembered value is the default", () => {
  const storage = memoryStorage();
  storage.setItem("layerstack-editor:tone-map", "aces");
  withStorage(storage, () => assert.equal(rememberedToneMap(), "srgb"));
});

test("blocked or missing storage leaves the default and does not throw", () => {
  const blocked = {
    getItem: () => {
      throw new Error("blocked");
    },
    setItem: () => {
      throw new Error("blocked");
    },
  };
  withStorage(blocked, () => {
    assert.equal(rememberedToneMap(), "srgb");
    rememberToneMap("agx");
  });
  withStorage(undefined, () => {
    assert.equal(rememberedToneMap(), "srgb");
    rememberToneMap("agx");
  });
});
