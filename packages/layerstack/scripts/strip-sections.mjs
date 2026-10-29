// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// `node strip-sections.mjs <in.wasm> <out.wasm> <name>...`: copies a
// WebAssembly module without the custom sections of those names, leaving
// every other byte as it was. `build-wasm.sh` uses it to take the
// `target_features` section out before wasm-bindgen and the `name` section
// out of the copy it reports as `raw`.
import { readFileSync, writeFileSync } from "node:fs";

const [input, output, ...names] = process.argv.slice(2);
const bytes = readFileSync(input);
const leb = (at) => {
  let value = 0;
  let shift = 0;
  for (;;) {
    const byte = bytes[at++];
    value += (byte & 0x7f) * 2 ** shift;
    shift += 7;
    if (byte < 0x80) return [value, at];
  }
};
const kept = [bytes.subarray(0, 8)];
let at = 8;
while (at < bytes.length) {
  const start = at;
  const id = bytes[at];
  const [size, body] = leb(at + 1);
  at = body + size;
  if (id === 0) {
    const [length, name] = leb(body);
    if (names.includes(bytes.toString("utf8", name, name + length))) continue;
  }
  kept.push(bytes.subarray(start, at));
}
writeFileSync(output, Buffer.concat(kept));
