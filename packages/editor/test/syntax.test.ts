// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The dev server serves the editor's modules untransformed, so their syntax
// must parse in every browser the editor supports, Safari included. And the
// editor draws with WebGPU only: no WebGL renderer, and no GLSL chunk
// patches (node materials and TSL instead).
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { describe, it } from "node:test";

const SOURCE = new URL("../src/", import.meta.url).pathname;

function sources(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(directory, entry.name);
    if (entry.isDirectory()) return sources(full);
    return /\.tsx?$/.test(entry.name) ? [full] : [];
  });
}

describe("editor sources", () => {
  it("declare nothing with `using`, which Safari does not parse yet", () => {
    for (const file of sources(SOURCE)) {
      const lines = readFileSync(file, "utf8").split("\n");
      lines.forEach((line, index) => {
        assert.doesNotMatch(line, /^\s*(await\s+)?using\s+[A-Za-z_$]/, `${path.relative(SOURCE, file)}:${index + 1}`);
      });
    }
  });

  it("draw with WebGPU alone: no WebGLRenderer, no shader chunk patches", () => {
    for (const file of sources(SOURCE)) {
      const code = readFileSync(file, "utf8")
        .split("\n")
        .filter((line) => !/^\s*(\/\/|\*)/.test(line))
        .join("\n");
      for (const name of ["WebGLRenderer", "WebGLRenderTarget", "onBeforeCompile", "ShaderChunk", "ShaderMaterial", "forceWebGL"]) {
        assert.doesNotMatch(code, new RegExp(`\\b${name}\\b`), `${path.relative(SOURCE, file)} uses ${name}`);
      }
    }
  });
});
