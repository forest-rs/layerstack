// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Image sizes read from file headers, so a large image is decoded once; and
// which images arrived since the draws reading them were last restyled.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { TextureStore, autoColorSpace, imageSize } from "../src/viewport/surfaces.ts";

/** A PNG's signature and `IHDR` chunk, which hold its size. */
function png(width: number, height: number): Uint8Array {
  const bytes = new Uint8Array(33);
  const view = new DataView(bytes.buffer);
  bytes.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  view.setUint32(8, 13);
  bytes.set([0x49, 0x48, 0x44, 0x52], 12);
  view.setUint32(16, width);
  view.setUint32(20, height);
  return bytes;
}

/** A JPEG's SOI, then `segments` (marker, payload), then a baseline start of frame. */
function jpeg(width: number, height: number, segments: [number, number[]][] = []): Uint8Array {
  const out = [0xff, 0xd8];
  for (const [marker, payload] of segments) out.push(0xff, marker, (payload.length + 2) >> 8, (payload.length + 2) & 0xff, ...payload);
  out.push(0xff, 0xc0, 0, 11, 8, height >> 8, height & 0xff, width >> 8, width & 0xff, 1, 1, 0x11, 0);
  return new Uint8Array(out);
}

/** A PNG's signature, `IHDR` (of `depth` bits and color `type`), and a `gAMA` chunk when `gamma` is given, then `IDAT`. */
function pngOf(depth: number, type: number, gamma?: number): Uint8Array {
  const chunks: number[][] = [];
  const chunk = (kind: string, data: number[]) => {
    const length = data.length;
    chunks.push([length >>> 24, (length >>> 16) & 255, (length >>> 8) & 255, length & 255, ...[...kind].map((c) => c.charCodeAt(0)), ...data, 0, 0, 0, 0]);
  };
  chunk("IHDR", [0, 0, 0, 16, 0, 0, 0, 16, depth, type, 0, 0, 0]);
  if (gamma !== undefined) {
    const value = Math.round(gamma * 100000);
    chunk("gAMA", [value >>> 24, (value >>> 16) & 255, (value >>> 8) & 255, value & 255]);
  }
  chunk("IDAT", []);
  return new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, ...chunks.flat()]);
}

/** A JPEG's SOI and a baseline start of frame of `components` 8-bit components. */
function jpegOf(components: number): Uint8Array {
  const length = 8 + 3 * components;
  const out = [0xff, 0xd8, 0xff, 0xc0, 0, length, 8, 0, 16, 0, 16, components];
  for (let i = 0; i < components; i++) out.push(i + 1, 0x11, 0);
  return new Uint8Array(out);
}

describe("autoColorSpace", () => {
  it("reads 8-bit color as sRGB, as Storm reads `auto`", () => {
    assert.equal(autoColorSpace(pngOf(8, 2)), "sRGB");
    assert.equal(autoColorSpace(pngOf(8, 6)), "sRGB");
    assert.equal(autoColorSpace(pngOf(4, 3)), "sRGB", "a palette decodes to 8-bit color");
    assert.equal(autoColorSpace(jpegOf(3)), "sRGB");
  });

  it("reads gray, gray with alpha and 16-bit samples as raw", () => {
    assert.equal(autoColorSpace(pngOf(8, 0)), "raw");
    assert.equal(autoColorSpace(pngOf(8, 4)), "raw");
    assert.equal(autoColorSpace(pngOf(16, 2)), "raw");
    assert.equal(autoColorSpace(jpegOf(1)), "raw");
  });

  it("follows a PNG's gamma first", () => {
    assert.equal(autoColorSpace(pngOf(8, 2, 1)), "raw");
    assert.equal(autoColorSpace(pngOf(8, 0, 1 / 2.2)), "sRGB");
  });

  it("does not say for other formats", () => {
    assert.equal(autoColorSpace(new TextEncoder().encode("GIF89a")), null);
  });
});

describe("imageSize", () => {
  it("reads a PNG's size from its IHDR chunk", () => {
    assert.deepEqual(imageSize(png(4096, 2048)), { width: 4096, height: 2048 });
  });

  it("reads a JPEG's size from its start of frame, past other segments", () => {
    const app0 = [0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0];
    assert.deepEqual(imageSize(jpeg(3000, 1000, [[0xe0, app0], [0xdb, new Array(65).fill(0)]])), { width: 3000, height: 1000 });
  });

  it("does not read a JPEG with EXIF data, whose orientation may turn it", () => {
    assert.equal(imageSize(jpeg(3000, 1000, [[0xe1, [0x45, 0x78, 0x69, 0x66, 0, 0]]])), null);
  });

  it("does not read other formats or cut headers", () => {
    assert.equal(imageSize(new TextEncoder().encode("GIF89a")), null);
    assert.equal(imageSize(png(16, 16).subarray(0, 20)), null);
    assert.equal(imageSize(jpeg(16, 16).subarray(0, 8)), null);
  });
});

describe("TextureStore.takeArrived", () => {
  it("names each image once, after it arrives or fails, for the draws reading it to be restyled", async () => {
    let arrivals = 0;
    const store = new TextureStore(async () => null, async () => false, () => (arrivals += 1));
    const file = (identifier: string) => ({ file: { assetPath: identifier, layer: null, identifier, udim: false } });
    assert.equal(store.image(file("a.png")), undefined, "loading");
    assert.equal(store.image(file("b.png")), undefined);
    assert.equal(store.takeArrived().size, 0, "nothing has arrived yet");
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(arrivals, 2);
    assert.deepEqual([...store.takeArrived()].sort(), ["a.png", "b.png"]);
    assert.equal(store.takeArrived().size, 0, "each once");
    assert.equal(store.image(file("a.png")), null, "unreadable once arrived");
    assert.equal(store.takeArrived().size, 0, "asking again does not load again");
  });
});

describe("TextureStore.dispose", () => {
  const file = (identifier: string) => ({ file: { assetPath: identifier, layer: null, identifier, udim: false } });

  it("forgets what it loaded, so another scene's file of the same name is read afresh", async () => {
    let reads = 0;
    const store = new TextureStore(async () => (reads += 1, null), async () => false, () => undefined);
    store.image(file("textures/a.png"));
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(store.image(file("textures/a.png")), null, "the first scene's answer");
    store.dispose();
    assert.equal(store.image(file("textures/a.png")), undefined, "loading again, not the first scene's answer");
    assert.equal(store.stats.pending, 1);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(reads, 2);
  });

  it("ignores an image that was on its way when it was cleared", async () => {
    let arrivals = 0;
    let finish: (bytes: Uint8Array | null) => void = () => undefined;
    const store = new TextureStore(() => new Promise((resolve) => (finish = resolve)), async () => false, () => (arrivals += 1));
    store.image(file("textures/a.png"));
    store.dispose();
    finish(null);
    await new Promise((resolve) => setTimeout(resolve, 0));
    assert.equal(arrivals, 0, "nothing is drawing it");
    assert.equal(store.takeArrived().size, 0);
    assert.equal(store.image(file("textures/a.png")), undefined, "asked for again rather than answered by the old request");
  });
});
