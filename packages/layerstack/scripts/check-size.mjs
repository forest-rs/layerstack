// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Reports the default module's sizes and fails over the budget in
// size-budget.json. Run after `npm run build:wasm`.
import { readFileSync, writeFileSync } from "node:fs";
import { brotliCompressSync, constants, gzipSync } from "node:zlib";

const root = new URL("../", import.meta.url);
const budget = JSON.parse(readFileSync(new URL("size-budget.json", root), "utf8"));
const optimized = readFileSync(new URL("wasm/layerstack_bg.wasm", root));
const unoptimized = readFileSync(new URL("wasm/layerstack_bg.unoptimized.wasm", root));
const sizes = {
  raw: unoptimized.byteLength,
  optimized: optimized.byteLength,
  gzip: gzipSync(optimized, { level: 9 }).byteLength,
  brotli: brotliCompressSync(optimized, {
    params: { [constants.BROTLI_PARAM_QUALITY]: 11, [constants.BROTLI_PARAM_SIZE_HINT]: optimized.byteLength },
  }).byteLength,
};
writeFileSync(new URL("wasm/sizes.json", root), `${JSON.stringify(sizes, null, 2)}\n`);
const mb = (bytes) => `${(bytes / 1e6).toFixed(3)} MB`;
console.log(
  `layerstack_bg.wasm: raw ${mb(sizes.raw)}, wasm-opt ${mb(sizes.optimized)}, gzip ${mb(sizes.gzip)}, brotli ${mb(sizes.brotli)}`,
);
let over = false;
for (const key of ["optimized", "gzip"]) {
  if (sizes[key] > budget[key]) {
    console.error(`over budget: ${key} ${mb(sizes[key])} > ${mb(budget[key])}`);
    over = true;
  }
}
process.exit(over ? 1 : 0);
