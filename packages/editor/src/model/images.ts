// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// What an image file's header says, read without decoding it: its format,
// size, channels and bits per channel, as the texture view shows them.

/** An image's header, as far as it was read. */
export interface ImageInfo {
  /** `png`, `jpeg`, `webp`, `exr`, `hdr`, `tga`, `tiff`, or `unknown`. */
  readonly format: string;
  /** Pixels; `null` when the header was not read for them. */
  readonly width: number | null;
  readonly height: number | null;
  /** The channels stored (`RGBA`, `RGB`, `gray`, `gray+alpha`, `indexed`); `null` when not read. */
  readonly channels: string | null;
  /** Bits per channel; `null` when not read. */
  readonly bits: number | null;
}

const PNG_CHANNELS: Record<number, string> = { 0: "gray", 2: "RGB", 3: "indexed", 4: "gray+alpha", 6: "RGBA" };

/** The header of the image `bytes` hold: PNG's `IHDR` and JPEG's start of frame are read; other formats are named by their signature. */
export function imageInfo(bytes: Uint8Array): ImageInfo {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const unread = (format: string): ImageInfo => ({ format, width: null, height: null, channels: null, bits: null });
  if (bytes.length >= 26 && view.getUint32(0) === 0x89504e47 && view.getUint32(12) === 0x49484452) {
    return { format: "png", width: view.getUint32(16), height: view.getUint32(20), bits: bytes[24]!, channels: PNG_CHANNELS[bytes[25]!] ?? null };
  }
  if (bytes.length >= 4 && view.getUint16(0) === 0xffd8) {
    let at = 2;
    while (at + 10 <= bytes.length) {
      if (bytes[at] !== 0xff) break;
      const marker = bytes[at + 1]!;
      if (marker === 0xff) {
        at += 1;
        continue;
      }
      if (marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc) {
        const components = bytes[at + 9]!;
        return {
          format: "jpeg",
          bits: bytes[at + 4]!,
          height: view.getUint16(at + 5),
          width: view.getUint16(at + 7),
          channels: components === 1 ? "gray" : components === 3 ? "RGB" : components === 4 ? "CMYK" : `${components}`,
        };
      }
      at += 2 + view.getUint16(at + 2);
    }
    return unread("jpeg");
  }
  if (bytes.length >= 12 && view.getUint32(0) === 0x52494646 && view.getUint32(8) === 0x57454250) return unread("webp");
  if (bytes.length >= 4 && view.getUint32(0) === 0x762f3101) return unread("exr");
  if (bytes.length >= 2 && bytes[0] === 0x23 && bytes[1] === 0x3f) return unread("hdr");
  if (bytes.length >= 4 && (view.getUint32(0) === 0x49492a00 || view.getUint32(0) === 0x4d4d002a)) return unread("tiff");
  return unread("unknown");
}
