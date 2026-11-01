// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A fingerprint of a value read from a stage: its typed arrays' bytes and
// its other fields, hashed in two independent 32-bit lanes. A draw list
// keeps one in place of a static mesh's input, to tell whether a read
// returned the same input without holding the arrays.

/** Two 32-bit hash lanes, with distinct seeds and multipliers. */
class Lanes {
  a = 0x9747b28c;
  b = 0x85ebca6b;

  word(value: number): void {
    // MurmurHash3's 32-bit mix in one lane, a different odd multiplier and
    // rotation in the other.
    let k = Math.imul(value, 0xcc9e2d51);
    k = (k << 15) | (k >>> 17);
    this.a = Math.imul(this.a ^ Math.imul(k, 0x1b873593), 5) + 0xe6546b64;
    let j = Math.imul(value ^ (value >>> 16), 0x2c1b3c6d);
    j = (j << 13) | (j >>> 19);
    this.b = Math.imul(this.b ^ Math.imul(j, 0x297a2d39), 9) + 0x7feb352d;
  }

  bytes(view: ArrayBufferView): void {
    const bytes = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
    const whole = bytes.byteOffset % 4 === 0 ? bytes.byteLength >>> 2 : 0;
    if (whole > 0) {
      const words = new Uint32Array(bytes.buffer, bytes.byteOffset, whole);
      for (let i = 0; i < words.length; i++) this.word(words[i]!);
    }
    for (let i = whole * 4; i < bytes.byteLength; i++) this.word(bytes[i]!);
    this.word(bytes.byteLength);
  }

  text(value: string): void {
    for (let i = 0; i < value.length; i++) this.word(value.charCodeAt(i));
    this.word(value.length);
  }
}

/**
 * The fingerprint of `value`: equal for values with equal typed arrays
 * (byte for byte, of the same kind) and equal other fields, and, with
 * 64 bits of hash and the byte count, different for different ones but
 * for a negligible chance.
 */
export function fingerprint(value: unknown): string {
  const lanes = new Lanes();
  let total = 0;
  const visit = (value: unknown): void => {
    if (ArrayBuffer.isView(value)) {
      lanes.text(value.constructor.name);
      lanes.bytes(value);
      total += value.byteLength;
    } else if (Array.isArray(value)) {
      lanes.word(0x5b);
      for (const element of value) visit(element);
      lanes.word(value.length);
    } else if (value !== null && typeof value === "object") {
      lanes.word(0x7b);
      for (const key of Object.keys(value).sort()) {
        const field = (value as Record<string, unknown>)[key];
        if (field === undefined) continue;
        lanes.text(key);
        visit(field);
      }
    } else {
      lanes.text(`${typeof value}:${String(value)}`);
    }
  };
  visit(value);
  return `${(lanes.a >>> 0).toString(16)}${(lanes.b >>> 0).toString(16).padStart(8, "0")}:${total}`;
}
