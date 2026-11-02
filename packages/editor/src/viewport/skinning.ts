// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's GPU skinning, in TSL: eight joint influences per vertex
// (`skinIndex` and `skinWeight`, then `skinIndex1` and `skinWeight1`, which
// every skinned geometry here has, zero-weighted where unused) where
// three.js's `SkinnedMesh` reads four, since production rigs weight more
// joints than four and dropping the rest moves points visibly; and normals
// as `UsdSkelSkinNormals` computes them (OpenUSD
// `pxr/usd/usdSkel/utils.cpp`, `_SkinNormalsLBS`): each joint matrix's
// linear part inverse transposed (by its cofactors), weighted and summed,
// then normalized where lighting reads it; a singular joint contributes
// nothing. three.js's own skinning blends the matrices themselves, which
// bends normals wrong under scaling joints.
//
// A skinned draw is a {@link SkinnedDraw}: a plain `Mesh` (three.js would
// skin a `SkinnedMesh` its own way) holding a `Skeleton` of one bone per
// joint, whose `boneMatrices` (each bone's world matrix times its inverse,
// `Skeleton.update`) the viewport poses. Its bind matrix is the identity
// (the skeleton is detached), so the shader skips it. Its materials, and
// the pick material, position vertices through {@link withSkinning}, which
// skins a `SkinnedDraw` and leaves anything else to three.js; the number of
// joints is in three.js's key for the draw (`object.skeleton`), so each
// skeleton size compiles once, and skeletons too large for a uniform buffer
// read their matrices from the skeleton's bone texture instead.
import { Mesh } from "three/webgpu";
import type { BufferGeometry, Material, NodeMaterial, Skeleton } from "three/webgpu";
import { OnObjectUpdate, attribute, cross, dot, float, int, ivec2, mat3, mat4, normalLocal, positionLocal, referenceBuffer, select, texture, textureSize, vec4 } from "three/tsl";

/** A mesh skinned on the GPU by the editor's shader (see the header). */
export class SkinnedDraw extends Mesh {
  /** The draw's joints, one bone each; read by the shader as `skeleton.boneMatrices`. */
  readonly skeleton: Skeleton;
  /** Marks a draw {@link withSkinning} skins. */
  readonly isSkinnedDraw = true;

  constructor(geometry: BufferGeometry, material: Material | Material[], skeleton: Skeleton) {
    super(geometry, material);
    this.skeleton = skeleton;
    // Its bounds are one pose's: never culled.
    this.frustumCulled = false;
  }
}

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Node = any;

/** The skeleton's bone matrices as a node with `element(joint)`: a uniform buffer, or the bone texture when it does not fit one. */
function boneMatrices(builder: { getUniformBufferLimit(): number }, skeleton: Skeleton): { element(joint: Node): Node } {
  const joints = skeleton.bones.length;
  if (joints * 64 <= builder.getUniformBufferLimit()) return (referenceBuffer as Node)("skeleton.boneMatrices", "mat4", joints);
  if (skeleton.boneTexture === null) skeleton.computeBoneTexture();
  const bones: Node = texture(skeleton.boneTexture!);
  // One shader for every skeleton of this size: each draw binds its own texture.
  OnObjectUpdate(({ object }) => {
    const { skeleton } = object as SkinnedDraw;
    if (skeleton.boneTexture === null) skeleton.computeBoneTexture();
    bones.value = skeleton.boneTexture;
  });
  return {
    element(joint: Node) {
      const size = int((textureSize(bones) as Node).x).toConst();
      const j = int(joint).mul(4).toConst();
      const y = j.div(size).toConst();
      const x = j.sub(y.mul(size)).toConst();
      return mat4(bones.load(ivec2(x, y)), bones.load(ivec2(x.add(1), y)), bones.load(ivec2(x.add(2), y)), bones.load(ivec2(x.add(3), y)));
    },
  };
}

/** The inverse transpose of a 3×3 matrix by its cofactors, or zero when it is singular. */
function inverseTranspose(m: Node): Node {
  const [c0, c1, c2] = [m.element(0), m.element(1), m.element(2)];
  const bc = cross(c1, c2);
  const det = dot(c0, bc);
  const scale = select(det.equal(0), float(0), float(1).div(det));
  return mat3(bc, cross(c2, c0), cross(c0, c1)).mul(scale);
}

/** Skins `positionLocal` and `normalLocal` of a {@link SkinnedDraw} by its eight influences. */
function skin(builder: { getUniformBufferLimit(): number; hasGeometryAttribute(name: string): boolean }, object: SkinnedDraw): void {
  const bones = boneMatrices(builder, object.skeleton);
  const index = [attribute("skinIndex", "uvec4"), attribute("skinIndex1", "uvec4")];
  const weight = [attribute("skinWeight", "vec4"), attribute("skinWeight1", "vec4")];
  const point = vec4(positionLocal, 1).toConst();
  const normal = normalLocal.toConst();
  let position: Node = null;
  let skinnedNormal: Node = null;
  for (const set of [0, 1]) {
    for (const lane of ["x", "y", "z", "w"] as const) {
      const matrix = bones.element(index[set]![lane]).toConst();
      const w = weight[set]![lane];
      const moved = matrix.mul(point).mul(w);
      position = position === null ? moved : position.add(moved);
      const turned = inverseTranspose(mat3(matrix)).mul(normal).mul(w);
      skinnedNormal = skinnedNormal === null ? turned : skinnedNormal.add(turned);
    }
  }
  positionLocal.assign(position.xyz);
  if (builder.hasGeometryAttribute("normal")) normalLocal.assign(skinnedNormal);
}

/**
 * Makes `material` skin the {@link SkinnedDraw}s it draws (and nothing
 * else), before three.js's own positioning; returns it. Every material a
 * skinned draw is drawn with goes through here, its pick material included.
 */
export function withSkinning<T extends NodeMaterial>(material: T): T {
  if ((material as { skinsDraws?: boolean }).skinsDraws === true) return material;
  const own = material.setupPosition.bind(material);
  Object.assign(material, {
    skinsDraws: true,
    setupPosition(builder: Parameters<NodeMaterial["setupPosition"]>[0]) {
      const object = (builder as unknown as { object: { isSkinnedDraw?: boolean } }).object;
      if (object.isSkinnedDraw === true) skin(builder as never, object as SkinnedDraw);
      return own(builder);
    },
  });
  return material;
}
