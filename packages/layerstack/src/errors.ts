// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/**
 * The stable code of a {@link LayerstackError}: dotted lowercase, a family
 * and a case. A released code keeps its spelling and meaning.
 *
 * Codes from the wrapper:
 * - `module.load`: the Wasm module could not be fetched or instantiated.
 * - `module.abi`: the module speaks another ABI than this package.
 * - `module.trapped`: the module trapped (a panic); the instance is dead
 *   and every later call fails with this code. Load a new instance.
 * - `stage.disposed`: the stage was disposed.
 * - `layer.affinity`: a layer of another stage was passed to this one.
 * - `argument.invalid`: an argument has the wrong type.
 * - `resolve.fetch`: the asset source's `fetch` threw or rejected.
 * - `resolve.aborted`: the `signal` passed to `resolve` aborted.
 * - `resolve.stuck`: an asset was supplied but the stage still wants it.
 * - `resolve.limit`: `resolve` reached its `maxRounds`.
 *
 * Codes from the native module:
 * - `io.*`: a read or import failure (`io.rejected` for a layer that does
 *   not parse, `io.not_found`, `io.unavailable`, ...), the codes of
 *   `layerstack_io::IoErrorKind::code`.
 * - `path.invalid`: a path argument is not an absolute prim path (or a
 *   property path, where one is asked for).
 * - `prim.not_found`: no composed prim is at the path.
 * - `layer.unknown`: the stage has no layer with the id.
 * - `value.decode`: a retained array failed to decode when read.
 *
 * Composition errors are not thrown; {@link Stage.status} lists them as
 * {@link Issue}s with `composition.*` codes
 * (`layerstack::CompositionError::code`).
 */
export type ErrorCode =
  | "module.load"
  | "module.abi"
  | "module.trapped"
  | "stage.disposed"
  | "layer.affinity"
  | "argument.invalid"
  | "resolve.fetch"
  | "resolve.aborted"
  | "resolve.stuck"
  | "resolve.limit"
  | "path.invalid"
  | "prim.not_found"
  | "layer.unknown"
  | "value.decode"
  | `io.${string}`;

/** What a {@link LayerstackError} is about, beyond its code. */
export interface ErrorDetails {
  /** The prim or property path the failure is about. */
  path?: string | undefined;
  /** The asset identifier the failure is about. */
  identifier?: string | undefined;
  /** The underlying error, such as the one an asset source's `fetch` threw. */
  cause?: unknown;
}

/**
 * Every failure this package throws: a stable {@link ErrorCode}, the
 * operation that failed (the method name, such as `open` or `resolve`),
 * and the path or asset identifier it is about.
 */
export class LayerstackError extends Error {
  /** The stable code naming the kind of failure. */
  readonly code: ErrorCode;
  /** The method that failed. */
  readonly operation: string;
  /** The prim or property path the failure is about, if any. */
  readonly path: string | undefined;
  /** The asset identifier the failure is about, if any. */
  readonly identifier: string | undefined;

  constructor(code: ErrorCode, operation: string, message: string, details: ErrorDetails = {}) {
    super(`${operation}: ${message}`, details.cause === undefined ? undefined : { cause: details.cause });
    this.name = "LayerstackError";
    this.code = code;
    this.operation = operation;
    this.path = details.path;
    this.identifier = details.identifier;
  }
}
