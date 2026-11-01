// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Image sizes read from file headers, so a large image is decoded once.
import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { imageSize } from "../src/viewport/surfaces.ts";

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
