// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A small layered scene, and a USDZ writer for packaging it, so the tests
// carry no binary fixtures.
import { crc32 } from "node:zlib";

const text = new TextEncoder();

/**
 * The scene's sources by identifier: the root sublayers `sub.usda` and
 * references `parts/ball.usda`, which references `parts/trim.usda`, so
 * resolving takes two rounds.
 */
export const scene: Record<string, string> = {
  "scene.usda": `#usda 1.0
(
    subLayers = [@./sub.usda@]
)
class "Base" {
    double size = 2
}
def Sphere "Ball" (
    inherits = </Base>
    prepend references = @./parts/ball.usda@</Ball>
) {
}
def "Garden" {
    def "Moss" {}
    def "Fern" (active = false) {}
}
`,
  "sub.usda": `#usda 1.0
over "Ball" {
    double size = 5
    int64 serial = 9007199254740993
}
`,
  "parts/ball.usda": `#usda 1.0
def "Ball" (
    prepend references = @./trim.usda@</Trim>
) {
    point3f[] points = [(0, 0, 0), (1, 2, 3)]
    double radius = 0.5
}
`,
  "parts/trim.usda": `#usda 1.0
def "Trim" {
    token tag = "trim"
}
`,
};

/** The bytes of a scene source. */
export function bytes(identifier: string): Uint8Array {
  const source = scene[identifier];
  if (source === undefined) throw new Error(`no fixture ${identifier}`);
  return text.encode(source);
}

/** An asset source over `files` that records what it was asked for, with a round boundary per call batch. */
export function mockSource(files: Record<string, string | Uint8Array | null>): {
  fetch(identifier: string): Promise<Uint8Array | null>;
  asked: string[];
} {
  const asked: string[] = [];
  return {
    asked,
    async fetch(identifier: string) {
      asked.push(identifier);
      await new Promise((resolve) => setTimeout(resolve, 1));
      const file = files[identifier];
      if (file === undefined) throw new Error(`the mock has no ${identifier}`);
      if (file === null) return null;
      return typeof file === "string" ? text.encode(file) : file;
    },
  };
}

/** A USDZ package: an uncompressed zip whose file data starts on 64-byte boundaries, the first file its root layer. */
export function usdz(files: readonly (readonly [string, string])[]): Uint8Array {
  const chunks: Uint8Array[] = [];
  const central: Uint8Array[] = [];
  let offset = 0;
  for (const [name, source] of files) {
    const nameBytes = text.encode(name);
    const data = text.encode(source);
    let pad = (64 - ((offset + 30 + nameBytes.length) % 64)) % 64;
    if (pad > 0 && pad < 4) pad += 64;
    const crc = crc32(data);
    const local = new DataView(new ArrayBuffer(30));
    local.setUint32(0, 0x04034b50, true);
    local.setUint16(4, 20, true);
    local.setUint16(12, 0x21, true);
    local.setUint32(14, crc, true);
    local.setUint32(18, data.length, true);
    local.setUint32(22, data.length, true);
    local.setUint16(26, nameBytes.length, true);
    local.setUint16(28, pad, true);
    const extra = new DataView(new ArrayBuffer(pad));
    if (pad > 0) {
      extra.setUint16(0, 0x1986, true);
      extra.setUint16(2, pad - 4, true);
    }
    const entry = new DataView(new ArrayBuffer(46));
    entry.setUint32(0, 0x02014b50, true);
    entry.setUint16(4, 20, true);
    entry.setUint16(6, 20, true);
    entry.setUint16(14, 0x21, true);
    entry.setUint32(16, crc, true);
    entry.setUint32(20, data.length, true);
    entry.setUint32(24, data.length, true);
    entry.setUint16(28, nameBytes.length, true);
    entry.setUint32(42, offset, true);
    central.push(new Uint8Array(entry.buffer), nameBytes);
    chunks.push(new Uint8Array(local.buffer), nameBytes, new Uint8Array(extra.buffer), data);
    offset += 30 + nameBytes.length + pad + data.length;
  }
  const centralSize = central.reduce((sum, chunk) => sum + chunk.length, 0);
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true);
  end.setUint16(8, files.length, true);
  end.setUint16(10, files.length, true);
  end.setUint32(12, centralSize, true);
  end.setUint32(16, offset, true);
  const all = [...chunks, ...central, new Uint8Array(end.buffer)];
  const out = new Uint8Array(all.reduce((sum, chunk) => sum + chunk.length, 0));
  let at = 0;
  for (const chunk of all) {
    out.set(chunk, at);
    at += chunk.length;
  }
  return out;
}
