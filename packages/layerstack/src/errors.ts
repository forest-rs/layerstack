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
 * - `target.affinity`: an edit target of another stage was passed to this one.
 * - `target.not_in_arc`: {@link CompositionArc.editTarget} was given a layer
 *   outside the arc's layer stack.
 * - `target.relocate_source`: {@link CompositionArc.editTarget} of a
 *   `relocates` arc, whose specs never contribute.
 * - `receipt.affinity`: a receipt of another stage was passed to this one.
 * - `receipt.released`: the receipt was undone or released already.
 * - `array.released`: the array handle was released.
 * - `array.no_memory`: the module does not expose its memory, so an array
 *   handle cannot be viewed or copied.
 * - `listener.failed`: a change listener threw. Not thrown: reported
 *   (`reportError`, else `console.error`) with the listener's error as
 *   `cause`, after the other listeners ran.
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
 * - `value.unknown_type`, `value.kind`, `value.shape`, `value.range`,
 *   `value.unsupported`: a `ValueInput` does not make a value of its
 *   type (an unregistered type name, the wrong kind of scalar, the wrong
 *   number of components, a number that is not an exact integer of the
 *   type, a value kind that cannot be authored).
 * - `layer.read_only`: the edits target a layer that cannot be saved (a
 *   USDZ member); open with `editLayer: "session"`. A move or delete
 *   that would change such a layer fails with it too, naming the layer.
 * - `layer.outside_stack`: the edits target a layer the stage reaches only
 *   through an arc: edit it through an {@link Opinion.editTarget} or a
 *   {@link CompositionArc.editTarget}.
 * - `target.stale`: no arc maps the {@link EditTarget} any more (the arc
 *   was removed, or the prim renamed or removed); `path` is the prim.
 *   Take a new target from the prim's opinions or arcs.
 * - `layer.muted`: the edits target a muted layer ({@link Stage.muteLayer});
 *   unmute it to edit it.
 * - `mute.root_layer`: {@link Stage.muteLayer} or
 *   {@link Stage.muteAndUnmuteLayers} was given the root layer, by any
 *   spelling, which cannot be muted; nothing changed.
 * - `mute.refused`: the module refused a mute request for a reason this
 *   package does not know yet.
 * - `namespace.*`: a {@link Stage.move} or {@link Stage.delete} that
 *   cannot be made; nothing changed: `namespace.invalid_paths` (not two
 *   distinct non-root prims or two properties), `namespace.not_found`,
 *   `namespace.collision` (the destination is occupied),
 *   `namespace.missing_parent`, `namespace.cycle` (a prim beneath
 *   itself), `namespace.instance_proxy` (inside an instance),
 *   `namespace.unmappable`, `namespace.split_opinions`,
 *   `namespace.unsupported` (a composition case not handled yet).
 * - `flatten.failed`: the stage could not be flattened (it has no root).
 * - `package.failed`: the stage could not be packaged (it has no root).
 * - `edit.*`: the edits were rejected and nothing changed:
 *   `edit.stale` (an undo whose slots changed since; the receipt stays
 *   undoable), `edit.rejected.*` (such as `edit.rejected.type_mismatch`
 *   or `edit.rejected.spec_exists`), the codes of
 *   `layerstack::EditError::code`.
 * - `subset.*`: `subset.geometry`, a subset read or
 *   {@link CreateGeomSubsetEdit} on a prim that is not geometry of the
 *   subsets' element type ({@link Prim.materialSubsets} of a prim that is
 *   not a mesh); and refused subset edits: `subset.indices` (an index out
 *   of the geometry's range), `subset.invalid_name`, `subset.name_taken`
 *   (a prim of another type has the name), `subset.family_declaration`,
 *   `subset.unrestricted_material_bind`.
 * - `material.not_found`: {@link BindMaterialEdit} names a material
 *   where no prim is, on the stage or defined earlier in the same edits,
 *   as OpenUSD's `Bind` refuses it (any existing prim binds).
 * - `material.not_material`: {@link SetBaseMaterialEdit} of a prim that
 *   is not a `Material`.
 * - `material.binding_name`: a collection binding name with a namespace,
 *   or a purpose that is not an identifier.
 * - `shading.*`: a refused shading edit: `shading.no_port` (no input or
 *   output at the path), `shading.not_shader`, `shading.type_mismatch`
 *   (an input exists with another type), `shading.invalid_name`,
 *   `shading.invalid_attribute`.
 * - `schema.cannot_edit`: a schema edit of a prim beneath an instance or
 *   in a prototype, which OpenUSD refuses; `schema.cannot_apply`: an API
 *   schema the prim may not have.
 * - `coord_sys.no_binding`: {@link ClearCoordSysEdit} of a binding that
 *   does not exist.
 * - `metadata.key_path`, `metadata.field`: an `sdrMetadata` key with an
 *   empty part, or a field the spec cannot hold.
 * - `skel.*`: a skinning read or computation that cannot be made:
 *   `skel.unbound` (no skeleton with joint influences binds the prim),
 *   `skel.influences` (not as many weights as joint indices, or not whole
 *   blocks), `skel.deformation` (influences that do not fit the points or
 *   name a joint past the palette), `skel.input` (a required input is
 *   missing or of another type), `skel.array_length`, `skel.target`,
 *   `skel.topology`, `skel.primvar`, `skel.method` (an unknown skinning
 *   method), `skel.singular` (a transform without an inverse).
 *
 * Composition errors are not thrown; {@link Stage.status} lists them as
 * {@link Issue}s with `composition.*` codes
 * (`layerstack::CompositionError::code`).
 *
 * A {@link LayerstackError}'s `code` may also be a {@link HostErrorCode}.
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
  | "target.affinity"
  | "target.not_in_arc"
  | "target.relocate_source"
  | "target.stale"
  | "receipt.affinity"
  | "receipt.released"
  | "array.released"
  | "array.no_memory"
  | "listener.failed"
  | "path.invalid"
  | "prim.not_found"
  | "layer.unknown"
  | "value.decode"
  | "value.unknown_type"
  | "value.kind"
  | "value.shape"
  | "value.range"
  | "value.unsupported"
  | "layer.read_only"
  | "layer.outside_stack"
  | "layer.muted"
  | "mute.root_layer"
  | "mute.refused"
  | "flatten.failed"
  | "package.failed"
  | `edit.${string}`
  | `namespace.${string}`
  | `subset.${string}`
  | `material.${string}`
  | `shading.${string}`
  | `schema.${string}`
  | `coord_sys.${string}`
  | `metadata.${string}`
  | `skel.${string}`
  | `io.${string}`;

/**
 * The code of an error a native extension raises, through
 * {@link Stage.withNative}: the code of the `layerstack_wasm::Error` its
 * Rust returns, passed through as it is. It is any string, so
 * `error.code === "domain.failed"` type-checks while editors still offer
 * this package's {@link ErrorCode}s. An extension names its codes in a
 * family of its own (`domain.*`), outside this package's families, so a
 * code always says whose error it is.
 */
export type HostErrorCode = string & {};

/** What a {@link LayerstackError} is about, beyond its code. */
export interface ErrorDetails {
  /** The prim or property path the failure is about. */
  path?: string | undefined;
  /** The asset identifier the failure is about. */
  identifier?: string | undefined;
  /** The index of the edit the failure is about, in the edits passed to `Stage.apply`. */
  edit?: number | undefined;
  /** The underlying error, such as the one an asset source's `fetch` threw. */
  cause?: unknown;
}

/**
 * Every failure this package throws: a stable {@link ErrorCode} (or a
 * native extension's {@link HostErrorCode}), the operation that failed
 * (the method name, such as `open` or `resolve`, or the operation named
 * to {@link Stage.withNative}), and the path, asset identifier or edit it
 * is about.
 */
export class LayerstackError extends Error {
  /** The stable code naming the kind of failure: one of {@link ErrorCode}, or a native extension's own. */
  readonly code: ErrorCode | HostErrorCode;
  /** The method that failed. */
  readonly operation: string;
  /** The prim or property path the failure is about, if any. */
  readonly path: string | undefined;
  /** The asset identifier the failure is about, if any. */
  readonly identifier: string | undefined;
  /** The index of the edit the failure is about, in the edits passed to `Stage.apply`, if any. */
  readonly edit: number | undefined;

  constructor(code: ErrorCode | HostErrorCode, operation: string, message: string, details: ErrorDetails = {}) {
    super(`${operation}: ${message}`, details.cause === undefined ? undefined : { cause: details.cause });
    this.name = "LayerstackError";
    this.code = code;
    this.operation = operation;
    this.path = details.path;
    this.identifier = details.identifier;
    this.edit = details.edit;
  }
}
