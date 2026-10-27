// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

/** An owned native layer handle. Obtain it from this Stage's inspection result. */
export interface Layer {
  readonly id: string;
  readonly name: string;
  readonly muted: boolean;
}
/** An owned inverse receipt. It cannot be consumed by another Stage client. */
export interface UndoReceipt { readonly id: string }
const layerOwners = new WeakMap<object, Stage>();
const receiptOwners = new WeakMap<object, Stage>();

/** A host-provided native endpoint. It can be local Wasm or a remote service. */
export interface StageTransport {
  request(request: string): string | Promise<string>;
}

/** Explicit native scalar encodings. Double values must be finite. */
export type ScalarValue =
  | { type: "bool"; value: boolean }
  | { type: "int" | "uint" | "float" | "double"; value: number }
  | { type: "int64" | "uint64"; value: string }
  | { type: "string" | "token" | "asset"; value: string }
  | { type: "double3"; value: readonly [number, number, number] }
  | { type: "token[]"; value: readonly string[] };

/** Typed edits are lowered to one atomic native transaction by the endpoint. */
export type Edit =
  | { type: "definePrim"; path: string; typeName?: string }
  | { type: "createAttribute"; path: string; name: string; typeName: string; value: ScalarValue; variability?: "uniform" | "varying" }
  | { type: "setAttribute"; path: string; name: string; value: ScalarValue }
  | { type: "addReference"; path: string; layer: Layer; primPath: string }
  | { type: "setInstanceable"; path: string; value: boolean };

/** Native change evidence, expressed as owned absolute path spellings. */
export interface Changes {
  created: string[];
  removed: string[];
  resynced: string[];
  changedInfoOnly: string[];
}

/** Successful source authoring plus its native inverse. Generated output is separate. */
export interface EditResult {
  receipt: UndoReceipt;
  changes: Changes;
}

/** Useful stage inventory without initiating procedural geometry evaluation. */
export interface StageInfo {
  rootLayer: string;
  layers: Layer[];
  schemaTypes: string[];
  capabilities: string[];
  metersPerUnit?: number;
  upAxis?: string;
}

/** A named composed scalar query. Unsupported array encodings are explicit. */
export interface AttributeSnapshot {
  typeName: string | null;
  variability: "uniform" | "varying";
  value: ScalarValue | null;
  unsupportedEncoding?: string;
}

/** Current native composition. Matrices use USD row-vector order, in f64. */
export interface PrimSnapshot {
  path: string;
  typeName: string | null;
  isInstance: boolean;
  isInstanceProxy: boolean;
  propertyNames: string[];
  attributes: Record<string, AttributeSnapshot>;
  worldTransform: number[] | null;
}

/** Structured endpoint refusal. Failed transactions do not partially author. */
export class LayerstackError extends Error {
  readonly code: string;
  readonly context?: unknown;
  constructor(code: string, message: string, context?: unknown) {
    super(message);
    this.name = "LayerstackError";
    this.code = code;
    this.context = context;
  }
}

/** Version-one wire envelope. Affinity is checked on every request after describe. */
export interface Request {
  protocol: "layerstack";
  version: 1;
  stage?: string;
  store?: string;
  op: Record<string, unknown> & { type: string };
}

interface Response<T> {
  protocol: "layerstack";
  version: 1;
  stage: string;
  store: string;
  ok: boolean;
  result?: T;
  error?: { code: string; message: string; context?: unknown };
}

/**
 * A client attached to one native stage/store pair, not a JavaScript scene tree.
 * The host owns the endpoint and the native runtime. Await edits to preserve
 * their order. Identifiers and receipts cannot be used with another transport.
 */
export class Stage {
  readonly stageId: string;
  readonly storeId: string;
  #transport: StageTransport;
  #disposed = false;

  private constructor(transport: StageTransport, stage: string, store: string) {
    this.#transport = transport;
    this.stageId = stage;
    this.storeId = store;
  }

  /** Connects to an existing host stage; never creates a competing scene model. */
  static async connect(transport: StageTransport): Promise<Stage> {
    const response = await exchange<StageInfo>(transport, { protocol: "layerstack", version: 1, op: { type: "describe" } });
    return new Stage(transport, response.stage, response.store);
  }

  /** Inventories native layers, registered schemas and supported operations. */
  async inspect(): Promise<StageInfo> {
    const info = await this.#call<StageInfo>({ type: "inspect" });
    const layers = info.layers.map(value => {
      const layer = Object.freeze({ id: value.id, name: value.name, muted: value.muted });
      layerOwners.set(layer, this);
      return layer;
    });
    return { ...info, layers };
  }
  /** Queries current composed prim/property names and the requested scalar values. */
  query(path: string, attributes: readonly string[] = []): Promise<PrimSnapshot> {
    return this.#call({ type: "query", path, attributes });
  }
  /** Direct composed children, including native instance namespace. */
  children(path: string): Promise<string[]> { return this.#call({ type: "children", path }); }
  /** Applies all edits atomically to one explicit native layer. */
  async transaction(target: Layer, edits: readonly Edit[]): Promise<EditResult> {
    const layer = this.#layer(target);
    const lowered = edits.map(edit => edit.type === "addReference" ? { ...edit, layer: this.#layer(edit.layer) } : edit);
    return this.#editResult(await this.#call({ type: "transaction", target: layer, edits: lowered }));
  }
  /** Applies one retained inverse once; returns a new receipt for the inverse edit. */
  async undo(receipt: UndoReceipt): Promise<EditResult> {
    if (receiptOwners.get(receipt) !== this) throw new LayerstackError("receipt.affinity", "Undo receipt belongs to another stage or was already consumed");
    const result = this.#editResult(await this.#call({ type: "undo", receipt: receipt.id }));
    receiptOwners.delete(receipt);
    return result;
  }
  /** Releases an inverse without editing the stage, bounding native retention. */
  async releaseReceipt(receipt: UndoReceipt): Promise<void> {
    if (receiptOwners.get(receipt) !== this) throw new LayerstackError("receipt.affinity", "Undo receipt belongs to another stage or was already consumed");
    await this.#call({ type: "releaseReceipt", receipt: receipt.id });
    receiptOwners.delete(receipt);
  }
  /** Changes stage-local composition controls without modifying the layer. */
  mute(layer: Layer, muted: boolean): Promise<{ changes: Changes }> {
    return this.#call({ type: "mute", layer: this.#layer(layer), muted });
  }
  /** Serializes a real layer. Relative asset paths retain their native meaning. */
  layerText(layer: Layer): Promise<{ name: string; text: string }> {
    return this.#call({ type: "layerText", layer: this.#layer(layer) });
  }
  /** Releases the native application stage; further calls fail locally. */
  async dispose(): Promise<void> {
    if (this.#disposed) return;
    await this.#call({ type: "dispose" });
    this.#disposed = true;
  }

  async #call<T>(op: Request["op"]): Promise<T> {
    if (this.#disposed) throw new LayerstackError("stage.disposed", "This stage client has been disposed");
    const response = await exchange<T>(this.#transport, {
      protocol: "layerstack", version: 1, stage: this.stageId, store: this.storeId, op,
    });
    return response.result as T;
  }

  #layer(layer: Layer): string {
    if (layerOwners.get(layer) !== this) throw new LayerstackError("layer.affinity", "Layer handle belongs to another stage client");
    return layer.id;
  }

  #editResult(value: { receipt: string; changes: Changes }): EditResult {
    const receipt = Object.freeze({ id: value.receipt });
    receiptOwners.set(receipt, this);
    return { receipt, changes: value.changes };
  }
}

async function exchange<T>(transport: StageTransport, request: Request): Promise<Response<T>> {
  let value: unknown;
  try { value = JSON.parse(await transport.request(JSON.stringify(request))); }
  catch (error) {
    if (error instanceof LayerstackError) throw error;
    throw new LayerstackError("transport.invalid", "The stage transport failed or returned invalid JSON", String(error));
  }
  const response = value as Response<T>;
  if (!response || response.protocol !== "layerstack" || response.version !== 1 || typeof response.stage !== "string" || typeof response.store !== "string" || typeof response.ok !== "boolean") {
    throw new LayerstackError("protocol.invalid", "Expected a version-one native stage response");
  }
  if (request.stage !== undefined && (response.stage !== request.stage || response.store !== request.store)) {
    throw new LayerstackError("stage.affinity", "The response belongs to another native stage or store");
  }
  if (!response.ok) {
    const error = response.error;
    if (!error || typeof error.code !== "string" || typeof error.message !== "string") throw new LayerstackError("protocol.invalid", "Missing structured native stage error");
    throw new LayerstackError(error.code, error.message, error.context);
  }
  return response;
}
