// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// `node order-functions.mjs <in.wasm> <out.wasm>`: names every function of
// a WebAssembly module so that `wasm-opt --reorder-functions-by-name` lays
// functions with similar bodies next to each other, where gzip's 32 KiB
// window and brotli find their repeats. `build-wasm.sh` runs it between two
// `wasm-opt` runs. Only the name section changes; it is a sort key, and
// `wasm-opt` leaves it out of the module it writes.
//
// Each body is summarized by a MinHash of its 6-byte shingles. The order
// starts from the first function, then repeatedly takes the unplaced
// function whose summary agrees most with the last one placed (among those
// sharing a band of the summary with it), starting again from the next
// unplaced function, in the input's order, when none agrees enough. The
// result depends only on the input bytes.
import { readFileSync, writeFileSync } from "node:fs";

const SHINGLE = 6;
const HASHES = 24;
const BAND = 3;
const BUCKET_LIMIT = 400;
const AGREEMENT = 2;

const [input, output] = process.argv.slice(2);
const bytes = readFileSync(input);
let at = 0;
const leb = () => {
  let value = 0;
  let shift = 0;
  for (;;) {
    const byte = bytes[at++];
    value += (byte & 0x7f) * 2 ** shift;
    shift += 7;
    if (byte < 0x80) return value;
  }
};
const skipName = () => {
  const length = leb();
  at += length;
};
const encode = (value) => {
  const out = [];
  do {
    let byte = value % 128;
    value = Math.floor(value / 128);
    if (value > 0) byte |= 0x80;
    out.push(byte);
  } while (value > 0);
  return Buffer.from(out);
};

// The sections, the number of imported functions and the function bodies.
const sections = [];
let imported = 0;
const bodies = [];
at = 8;
while (at < bytes.length) {
  const start = at;
  const id = bytes[at++];
  const size = leb();
  const body = at;
  const end = body + size;
  let name = null;
  if (id === 0) {
    const length = leb();
    name = bytes.toString("utf8", at, at + length);
  } else if (id === 2) {
    for (let count = leb(); count > 0; count--) {
      skipName();
      skipName();
      const kind = bytes[at++];
      if (kind === 0) {
        leb();
        imported++;
      } else if (kind === 1) {
        at++;
        const flags = leb();
        leb();
        if (flags & 1) leb();
      } else if (kind === 2) {
        const flags = leb();
        leb();
        if (flags & 1) leb();
      } else if (kind === 3) {
        at += 2;
      } else if (kind === 4) {
        at++;
        leb();
      } else {
        throw new Error(`unknown import kind ${kind}`);
      }
    }
  } else if (id === 10) {
    for (let count = leb(); count > 0; count--) {
      const length = leb();
      bodies.push(bytes.subarray(at, at + length));
      at += length;
    }
  }
  sections.push({ id, name, bytes: bytes.subarray(start, end) });
  at = end;
}

// A MinHash summary of each body.
const seeds = Array.from({ length: HASHES }, (_, k) => [
  (Math.imul(k + 1, 0x9e3779b1) | 1) >>> 0,
  Math.imul(k + 7, 0x85ebca6b) >>> 0,
]);
const summaries = bodies.map((body) => {
  const summary = new Uint32Array(HASHES).fill(0xffffffff);
  const last = Math.max(1, body.length - SHINGLE + 1);
  for (let i = 0; i < last; i++) {
    let hash = 0x811c9dc5;
    for (let j = i; j < Math.min(i + SHINGLE, body.length); j++) {
      hash = Math.imul(hash ^ body[j], 0x01000193);
    }
    for (let k = 0; k < HASHES; k++) {
      const value = (Math.imul(hash, seeds[k][0]) + seeds[k][1]) >>> 0;
      if (value < summary[k]) summary[k] = value;
    }
  }
  return summary;
});

// Functions that share a band of their summaries.
const buckets = new Map();
summaries.forEach((summary, f) => {
  for (let band = 0; band < HASHES / BAND; band++) {
    const key = `${band}:${Array.from(summary.subarray(band * BAND, (band + 1) * BAND)).join(",")}`;
    const bucket = buckets.get(key);
    if (bucket) bucket.push(f);
    else buckets.set(key, [f]);
  }
});
const neighbours = bodies.map(() => new Set());
for (const bucket of buckets.values()) {
  if (bucket.length < 2 || bucket.length > BUCKET_LIMIT) continue;
  for (const f of bucket) for (const g of bucket) if (f !== g) neighbours[f].add(g);
}
const agreement = (f, g) => {
  let same = 0;
  for (let k = 0; k < HASHES; k++) if (summaries[f][k] === summaries[g][k]) same++;
  return same;
};

const placed = new Uint8Array(bodies.length);
const order = [];
let next = 0;
let current = -1;
while (order.length < bodies.length) {
  if (current < 0) {
    while (placed[next]) next++;
    current = next;
  }
  placed[current] = 1;
  order.push(current);
  let best = -1;
  let bestAgreement = -1;
  for (const g of neighbours[current]) {
    if (placed[g]) continue;
    const same = agreement(current, g);
    if (same > bestAgreement || (same === bestAgreement && g < best)) {
      best = g;
      bestAgreement = same;
    }
  }
  current = bestAgreement >= AGREEMENT ? best : -1;
}

// A name section whose function names sort in that order; its entries go
// in increasing function index.
const width = String(bodies.length).length;
const byIndex = [encode(bodies.length)];
const keys = new Array(bodies.length);
order.forEach((f, rank) => {
  keys[f] = Buffer.from(`f${String(rank).padStart(width, "0")}`);
});
keys.forEach((key, f) => byIndex.push(encode(imported + f), encode(key.length), key));
const functionNames = Buffer.concat(byIndex);
const payload = Buffer.concat([
  encode(4),
  Buffer.from("name"),
  Buffer.from([1]),
  encode(functionNames.length),
  functionNames,
]);
const nameSection = Buffer.concat([Buffer.from([0]), encode(payload.length), payload]);
const kept = sections.filter((section) => section.name !== "name").map((section) => section.bytes);
writeFileSync(output, Buffer.concat([bytes.subarray(0, 8), ...kept, nameSection]));
