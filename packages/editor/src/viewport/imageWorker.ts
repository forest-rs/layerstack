// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The worker of `imageDecoders.ts`: decodes an image file into an
// `ImageBitmap` with the options it is sent, scaled while decoding when
// they say, and transfers the bitmap back.

/** A request: an image file and how to decode it. */
interface DecodeRequest {
  readonly id: number;
  readonly blob: Blob;
  readonly options: ImageBitmapOptions;
}

/** The worker's own scope, as far as it is used here. */
interface Scope {
  onmessage: ((event: MessageEvent<DecodeRequest>) => void) | null;
  postMessage(message: unknown, transfer?: Transferable[]): void;
}

const scope = self as unknown as Scope;

scope.onmessage = ({ data: { id, blob, options } }) => {
  createImageBitmap(blob, options).then(
    (bitmap) => scope.postMessage({ id, bitmap }, [bitmap]),
    (error: unknown) => scope.postMessage({ id, error: error instanceof Error ? error.message : String(error) }),
  );
};

export {};
