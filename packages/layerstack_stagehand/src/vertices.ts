// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The vertex formats a draw list hands a renderer: what each attribute's
// numbers are, how many there are per vertex, and how positions decode.
// A mesh is planned and filled in `f32` by the module; the compact format
// is encoded here, in JavaScript, as the buffers are copied out: one pass
// over each attribute, no second module, and nothing the module itself
// must carry (its size is budgeted).

/**
 * How a draw list stores its meshes' vertices ({@link DrawOptions.vertexFormat}).
 *
 * - `compact` (the default): 16 bytes per vertex where a mesh has a
 *   position, a normal and `st`. Positions are `snorm16` ×4 (the fourth
 *   padding) relative to the mesh's bounds, decoded by
 *   {@link MeshVertices.decode} (one uniform scale and an offset, to fold
 *   into the model matrix); normals `snorm8` ×4; `st` and other texture
 *   coordinate sets `float16` ×2 when every value is below
 *   {@link HALF_ST_LIMIT} in magnitude, else `float32` ×2; colors `float32`
 *   ×3; indices `u16` when every vertex index fits (below 65535, which
 *   WebGL 2 reserves as the primitive restart index), else `u32`.
 * - `float32`: every attribute `float32` (positions and normals ×3),
 *   indices `u32`, positions as planned (an identity decode): the mesh's
 *   own arrays, shared, not copied.
 */
export type VertexFormat = "compact" | "float32";

/**
 * The numeric type of an attribute's components.
 *
 * - `float32`: 32-bit floats, in a `Float32Array`.
 * - `float16`: IEEE 754 half floats, as their bits in a `Uint16Array`
 *   (WebGL's `HALF_FLOAT`).
 * - `snorm16`: signed 16-bit integers in an `Int16Array`, read normalized:
 *   `max(q / 32767, -1)`.
 * - `snorm8`: signed 8-bit integers in an `Int8Array`, read normalized:
 *   `max(q / 127, -1)`.
 */
export type ComponentType = "float32" | "float16" | "snorm16" | "snorm8";

/** One vertex attribute: its numbers, their type, and how many a vertex has. */
export interface VertexAttribute {
  /** The numbers, {@link stride} per vertex; empty once released ({@link MeshVertices.released}). */
  readonly array: Float32Array | Uint16Array | Int16Array | Int8Array;
  readonly type: ComponentType;
  /** The components a shader reads per vertex: 3 for a position, normal or color, 2 for texture coordinates. */
  readonly size: number;
  /** The numbers per vertex in {@link array}: {@link size}, or 4 where padding keeps every vertex 4-byte aligned. */
  readonly stride: number;
  /** Whether integer components are read normalized, as {@link ComponentType} says; false for floats. */
  readonly normalized: boolean;
}

/**
 * How a position attribute's values become the mesh's points: `point =
 * value * scale + offset`, per axis, where `value` is what the attribute
 * reads (normalized for `snorm16`). One uniform scale, so the decode
 * folds into the model matrix (`model * translate(offset) * scale(scale)`)
 * and normals, which are not quantized relative to the bounds, are
 * transformed as they were. Identity (`scale` 1, `offset` 0) for
 * `float32`.
 */
export interface PositionDecode {
  readonly scale: number;
  readonly offset: readonly [number, number, number];
}

/** A texture coordinate set besides `st`, by its primvar's name ({@link MeshInput.uvSets}). */
export interface VertexUvSet {
  readonly name: string;
  readonly attribute: VertexAttribute;
}

/**
 * A mesh's vertices and indices in a {@link VertexFormat}, ready to upload
 * as they are: what a draw list's {@link DrawBuffers.vertices} holds. The
 * counts and types survive {@link DrawList.release}, which empties the
 * arrays once a renderer has uploaded them.
 */
export interface MeshVertices {
  readonly format: VertexFormat;
  /** Vertices. */
  readonly count: number;
  /** Triangles: a third of the indices. */
  readonly triangles: number;
  readonly position: VertexAttribute;
  /** How {@link position}'s values decode to points. */
  readonly decode: PositionDecode;
  /** Unit normals (each within `snorm8`'s rounding of unit length in `compact`). */
  readonly normal: VertexAttribute;
  /** `st`; `null` when it is not drawn. */
  readonly st: VertexAttribute | null;
  /** Linear RGB `displayColor`; `null` when it is not drawn. */
  readonly color: VertexAttribute | null;
  /** The texture coordinate sets besides `st`, in the mesh's order. */
  readonly uvSets: readonly VertexUvSet[];
  /** Three vertex indices per triangle, counter-clockwise seen from the front; empty once released. */
  readonly indices: Uint16Array | Uint32Array;
  /**
   * Whether the arrays were dropped after upload ({@link DrawList.release}):
   * every array is then empty and only the counts, types and decode remain.
   */
  readonly released: boolean;
}

/**
 * The largest magnitude a texture coordinate may have to be stored as
 * `float16` in the `compact` format: below 2, a half float rounds a
 * coordinate by at most 2^-11 (2^-12 below 1), under a texel of a
 * 2048-texel texture; tiling and UDIM coordinates beyond it would round
 * by 2^-10 or more, so a set holding one is kept `float32`.
 */
export const HALF_ST_LIMIT = 2;

/** What {@link encodeVertices} reads: a mesh's buffers as the module filled them. */
export interface VertexSource {
  readonly positions: Float32Array;
  readonly normals: Float32Array;
  readonly st: Float32Array | null;
  readonly colors: Float32Array | null;
  readonly uvSets: readonly { readonly name: string; readonly values: Float32Array }[];
  readonly indices: Uint32Array;
}

/** The largest snorm16 and snorm8 values. */
const SNORM16 = 32767;
const SNORM8 = 127;

/**
 * Encodes `mesh`'s buffers in `format`. `float32` shares the mesh's
 * arrays; `compact` makes new ones, as {@link VertexFormat} lays them out.
 * Each position is then within half a step, `scale / 32767 / 2`, of its
 * point on each axis, where `scale` is half the bounds' largest extent:
 * the extent / 65534 per step.
 */
export function encodeVertices(mesh: VertexSource, format: VertexFormat): MeshVertices {
  const count = mesh.positions.length / 3;
  const triangles = mesh.indices.length / 3;
  if (format === "float32") {
    const float = (array: Float32Array, size: number): VertexAttribute => ({ array, type: "float32", size, stride: size, normalized: false });
    return {
      format,
      count,
      triangles,
      position: float(mesh.positions, 3),
      decode: { scale: 1, offset: [0, 0, 0] },
      normal: float(mesh.normals, 3),
      st: mesh.st === null ? null : float(mesh.st, 2),
      color: mesh.colors === null ? null : float(mesh.colors, 3),
      uvSets: mesh.uvSets.map((set) => ({ name: set.name, attribute: float(set.values, 2) })),
      indices: mesh.indices,
      released: false,
    };
  }
  const { array: position, decode } = quantizePositions(mesh.positions);
  return {
    format,
    count,
    triangles,
    position: { array: position, type: "snorm16", size: 3, stride: 4, normalized: true },
    decode,
    normal: { array: quantizeNormals(mesh.normals), type: "snorm8", size: 3, stride: 4, normalized: true },
    st: mesh.st === null ? null : coordinates(mesh.st),
    color: mesh.colors === null ? null : { array: mesh.colors, type: "float32", size: 3, stride: 3, normalized: false },
    uvSets: mesh.uvSets.map((set) => ({ name: set.name, attribute: coordinates(set.values) })),
    indices: count < 0xffff ? new Uint16Array(mesh.indices) : mesh.indices,
    released: false,
  };
}

/**
 * Positions as `snorm16` ×4 about the center of their bounds, scaled by
 * half the bounds' largest extent (1 for a point or no points).
 */
export function quantizePositions(positions: Float32Array): { array: Int16Array; decode: PositionDecode } {
  const count = positions.length / 3;
  const min = [Infinity, Infinity, Infinity];
  const max = [-Infinity, -Infinity, -Infinity];
  for (let at = 0; at < positions.length; at += 3) {
    for (let axis = 0; axis < 3; axis++) {
      const value = positions[at + axis]!;
      if (value < min[axis]!) min[axis] = value;
      if (value > max[axis]!) max[axis] = value;
    }
  }
  const offset: [number, number, number] = count === 0 ? [0, 0, 0] : [0, 1, 2].map((axis) => (min[axis]! + max[axis]!) / 2) as [number, number, number];
  const half = count === 0 ? 0 : Math.max(max[0]! - min[0]!, max[1]! - min[1]!, max[2]! - min[2]!) / 2;
  const scale = half > 0 && Number.isFinite(half) ? half : 1;
  const array = new Int16Array(4 * count);
  const toUnit = SNORM16 / scale;
  const [x, y, z] = offset;
  const snorm = (value: number) => {
    const q = Math.round(value * toUnit);
    return q > SNORM16 ? SNORM16 : q < -SNORM16 ? -SNORM16 : q;
  };
  for (let vertex = 0; vertex < count; vertex++) {
    array[4 * vertex] = snorm(positions[3 * vertex]! - x);
    array[4 * vertex + 1] = snorm(positions[3 * vertex + 1]! - y);
    array[4 * vertex + 2] = snorm(positions[3 * vertex + 2]! - z);
  }
  return { array, decode: { scale, offset } };
}

/** Unit normals as `snorm8` ×4, each component rounded to the nearest 127th. */
export function quantizeNormals(normals: Float32Array): Int8Array {
  const count = normals.length / 3;
  const array = new Int8Array(4 * count);
  for (let vertex = 0; vertex < count; vertex++) {
    for (let axis = 0; axis < 3; axis++) {
      const q = Math.round(normals[3 * vertex + axis]! * SNORM8);
      array[4 * vertex + axis] = q > SNORM8 ? SNORM8 : q < -SNORM8 ? -SNORM8 : q;
    }
  }
  return array;
}

/** Texture coordinates as `float16` ×2 when every one is below {@link HALF_ST_LIMIT} in magnitude, else as they are. */
function coordinates(values: Float32Array): VertexAttribute {
  for (const value of values) {
    if (!(Math.abs(value) < HALF_ST_LIMIT)) return { array: values, type: "float32", size: 2, stride: 2, normalized: false };
  }
  return { array: halves(values), type: "float16", size: 2, stride: 2, normalized: false };
}

/** The runtime's `Float16Array`, where it has one: it converts natively, as {@link toHalf} does. */
const Float16 = (globalThis as { Float16Array?: new (values: Float32Array) => ArrayBufferView }).Float16Array;

/** `values` as half-float bits ({@link toHalf}). */
function halves(values: Float32Array): Uint16Array {
  if (Float16 !== undefined) {
    const native = new Float16(values);
    return new Uint16Array(native.buffer, native.byteOffset, values.length);
  }
  const array = new Uint16Array(values.length);
  for (let at = 0; at < values.length; at++) array[at] = toHalf(values[at]!);
  return array;
}

const scratch = new Float32Array(1);
const bits = new Uint32Array(scratch.buffer);

/** `value` as the bits of the nearest half float (ties to even), as a GPU converts it. */
export function toHalf(value: number): number {
  scratch[0] = value;
  const x = bits[0]!;
  const sign = (x >>> 16) & 0x8000;
  const exponent = (x >>> 23) & 0xff;
  const mantissa = x & 0x7fffff;
  if (exponent === 0xff) return sign | 0x7c00 | (mantissa === 0 ? 0 : 0x200);
  const e = exponent - 127 + 15;
  if (e >= 0x1f) return sign | 0x7c00;
  if (e <= 0) {
    // A subnormal half (or zero): the full mantissa shifted into place.
    if (e < -10) return sign;
    const full = mantissa | 0x800000;
    const shift = 14 - e;
    let half = full >>> shift;
    const rest = full & ((1 << shift) - 1);
    const middle = 1 << (shift - 1);
    if (rest > middle || (rest === middle && (half & 1) === 1)) half++;
    return sign | half;
  }
  let half = (e << 10) | (mantissa >>> 13);
  const rest = mantissa & 0x1fff;
  // A carry out of the mantissa rounds into the exponent, as it should.
  if (rest > 0x1000 || (rest === 0x1000 && (half & 1) === 1)) half++;
  return sign | half;
}

/** The value of the half float whose bits are `half`. */
export function fromHalf(half: number): number {
  const sign = half & 0x8000 ? -1 : 1;
  const exponent = (half >>> 10) & 0x1f;
  const mantissa = half & 0x3ff;
  if (exponent === 0) return sign * mantissa * 2 ** -24;
  if (exponent === 0x1f) return mantissa === 0 ? sign * Infinity : NaN;
  return sign * (1 + mantissa / 1024) * 2 ** (exponent - 15);
}

/** The value an attribute's component `at` (an index into its array) reads as. */
function component(attribute: VertexAttribute, at: number): number {
  const value = attribute.array[at]!;
  switch (attribute.type) {
    case "float32":
      return value;
    case "float16":
      return fromHalf(value);
    case "snorm16":
      return Math.max(value / SNORM16, -1);
    case "snorm8":
      return Math.max(value / SNORM8, -1);
  }
}

/**
 * An attribute's values as `f32`, {@link VertexAttribute.size} per vertex,
 * as a GPU reads them (positions not decoded: see {@link vertexPositions}).
 * Empty once released.
 */
export function attributeValues(attribute: VertexAttribute): Float32Array {
  const count = attribute.array.length / attribute.stride;
  const out = new Float32Array(count * attribute.size);
  for (let vertex = 0; vertex < count; vertex++) {
    for (let c = 0; c < attribute.size; c++) out[vertex * attribute.size + c] = component(attribute, vertex * attribute.stride + c);
  }
  return out;
}

/** The mesh's points, three per vertex, decoded from {@link MeshVertices.position}; empty once released. */
export function vertexPositions(vertices: MeshVertices): Float32Array {
  const out = attributeValues(vertices.position);
  const { scale, offset } = vertices.decode;
  for (let at = 0; at < out.length; at++) out[at] = out[at]! * scale + offset[at % 3]!;
  return out;
}

/** The bytes of a component of `type`. */
export function componentBytes(type: ComponentType): number {
  return type === "float32" ? 4 : type === "snorm8" ? 1 : 2;
}

/** The bytes a renderer uploads for `vertices`, from their counts and types: whether or not they were released. */
export function verticesGpuBytes(vertices: MeshVertices): number {
  const attributes = [vertices.position, vertices.normal, vertices.st, vertices.color, ...vertices.uvSets.map((set) => set.attribute)];
  let bytes = vertices.triangles * 3 * vertices.indices.BYTES_PER_ELEMENT;
  for (const attribute of attributes) if (attribute !== null) bytes += vertices.count * attribute.stride * componentBytes(attribute.type);
  return bytes;
}

/** `vertices` with every array emptied in place, counts, types and decode kept: {@link MeshVertices.released}. */
export function releaseVertices(vertices: MeshVertices): void {
  type Writable<T> = { -readonly [K in keyof T]: T[K] };
  const empty = (attribute: VertexAttribute | null) => {
    if (attribute !== null) (attribute as Writable<VertexAttribute>).array = new (attribute.array.constructor as new (length: number) => VertexAttribute["array"])(0);
  };
  for (const attribute of [vertices.position, vertices.normal, vertices.st, vertices.color, ...vertices.uvSets.map((set) => set.attribute)]) empty(attribute);
  const writable = vertices as Writable<MeshVertices>;
  writable.indices = vertices.indices instanceof Uint16Array ? new Uint16Array(0) : new Uint32Array(0);
  writable.released = true;
}
