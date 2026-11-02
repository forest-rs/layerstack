// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Image files decoded into `ImageBitmap`s off the page's thread, in a few
// workers (`imageWorker.ts`).
//
// `createImageBitmap` decodes on a background thread, but it scales on the
// thread that calls it: a 4096² texture scaled to 2048² while decoding
// held the page's thread about 60 ms, and a scene of 76 of them held it in
// a long task per image while they arrived. Called in a worker, the page's
// thread only hands over the file as a `Blob` and takes the bitmap back
// (transferred, not copied); no copy of the bytes lands in a worker's heap
// to wait there for a collection an idle worker may not run. The workers are made at the first decode and
// shared by every texture store; without workers (a test in Node) images
// are decoded on the calling thread.

/** A decode on its way, and what it resolves. */
interface Pending {
  readonly resolve: (bitmap: ImageBitmap) => void;
  readonly reject: (error: Error) => void;
  readonly worker: number;
}

/** What the decoders are doing, for the status line and measurements. */
export interface DecoderStats {
  /** Workers decoding; 0 before the first decode, and without workers. */
  readonly workers: number;
  /** Decodes sent and not yet back. */
  readonly inFlight: number;
}

/** A few workers decoding images, each sent the next decode when it has the fewest on its way. */
class ImageDecoders {
  readonly #workers: Worker[] = [];
  readonly #load: number[] = [];
  readonly #pending = new Map<number, Pending>();
  #next = 0;

  /** `bytes` (copied into a `Blob`, so the caller keeps its own) decoded with `options`. */
  decode(bytes: Uint8Array, options: ImageBitmapOptions): Promise<ImageBitmap> {
    // A `Blob` copies the bytes it is made from: no copy of them first. The
    // hosts' bytes are never views of shared memory, which it would refuse.
    const blob = new Blob([bytes as Uint8Array<ArrayBuffer>]);
    if (typeof Worker === "undefined") return createImageBitmap(blob, options);
    if (this.#workers.length === 0) this.#start();
    let worker = 0;
    for (let at = 1; at < this.#workers.length; at++) if (this.#load[at]! < this.#load[worker]!) worker = at;
    const id = this.#next++;
    return new Promise<ImageBitmap>((resolve, reject) => {
      this.#pending.set(id, { resolve, reject, worker });
      this.#load[worker]! += 1;
      this.#workers[worker]!.postMessage({ id, blob, options });
    });
  }

  get stats(): DecoderStats {
    return { workers: this.#workers.length, inFlight: this.#pending.size };
  }

  /** Two to four workers: decoding is the browser's, the workers only keep scaling off the page's thread. */
  #start(): void {
    const count = Math.max(2, Math.min(4, (navigator.hardwareConcurrency ?? 4) >> 2));
    for (let at = 0; at < count; at++) {
      const worker = new Worker(new URL("./imageWorker.ts", import.meta.url), { type: "module" });
      worker.onmessage = ({ data }: MessageEvent<{ id: number; bitmap?: ImageBitmap; error?: string }>) => {
        const pending = this.#pending.get(data.id);
        if (pending === undefined) return;
        this.#pending.delete(data.id);
        this.#load[pending.worker]! -= 1;
        if (data.bitmap !== undefined) pending.resolve(data.bitmap);
        else pending.reject(new Error(data.error ?? "the image could not be decoded"));
      };
      this.#workers.push(worker);
      this.#load.push(0);
    }
  }
}

/** The page's image decoders, shared by every texture store. */
export const imageDecoders = new ImageDecoders();
