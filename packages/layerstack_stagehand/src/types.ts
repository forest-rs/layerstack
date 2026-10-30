// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The plain data the package takes and returns.

/** How a primvar's elements map onto a mesh (`UsdGeomPrimvar` interpolation). */
export type Interpolation = "constant" | "uniform" | "varying" | "vertex" | "faceVarying";

/** A primvar as typed arrays: element `i` is `values[indices[i]]` when indexed. */
export interface PrimvarInput {
  /** The values, flattened: three numbers per normal or color, two per `st`. */
  readonly values: Float32Array;
  /** One index into `values` per element, when indexed. */
  readonly indices?: Int32Array | null;
  readonly interpolation: Interpolation;
  /** Values per element; only 1 is drawn. Default 1. */
  readonly elementSize?: number;
}

/** A face subset (`GeomSubset` of `elementType = "face"`): its faces, drawn as one {@link DrawRange}. */
export interface SubsetInput {
  /** The subset's path, carried through for the caller; the module does not read it. */
  readonly path?: string;
  readonly faces: Int32Array;
}

/**
 * A mesh as typed arrays: what {@link Stagehand.buildMesh} plans.
 * {@link readMesh} reads one off a stage; anything else can build one.
 */
export interface MeshInput {
  /** `faceVertexCounts`. */
  readonly faceVertexCounts: Int32Array;
  /** `faceVertexIndices`. */
  readonly faceVertexIndices: Int32Array;
  /** `holeIndices`, sorted ascending as Hydra walks them. */
  readonly holeIndices?: Int32Array;
  /** `rightHanded` (default) or `leftHanded`; anything but `rightHanded` flips, as in Hydra. */
  readonly orientation?: string;
  /** `subdivisionScheme`, reported and not applied: subdivision surfaces draw as their control cage. Default `catmullClark`. */
  readonly subdivisionScheme?: string;
  /** `points`, three numbers each. */
  readonly points: Float32Array;
  /** Authored normals; without them, smooth normals are computed per point. */
  readonly normals?: PrimvarInput | null;
  /** `primvars:st`. */
  readonly st?: PrimvarInput | null;
  /** `primvars:displayColor`. */
  readonly displayColor?: PrimvarInput | null;
  /** The face subsets drawn as ranges, typically the `materialBind` family, in order. */
  readonly subsets?: readonly SubsetInput[];
}

/**
 * How faces become triangles: `concave` (the default) fans convex faces
 * exactly as Hydra does and ear-clips concave ones; `hydraFan` fans every
 * face, as Hydra does (`HdMeshUtil::ComputeTriangleIndices`).
 */
export type Triangulation = "concave" | "hydraFan";

/**
 * How smooth normals are computed for a mesh without authored ones.
 * `hydra` (the default) sums each corner's cross product as Hydra does
 * (`pxr/imaging/hd/smoothNormals.cpp`): at the reflex corner of a lone
 * concave face the normal points away from the face's front and shades
 * dark. `faceOriented` turns each corner's contribution to agree with its
 * face's normal (Newell's method): the same normals wherever every face is
 * convex, a front-facing one at a reflex corner.
 */
export type SmoothNormals = "hydra" | "faceOriented";

/** How {@link Stagehand.buildMesh} plans a mesh. */
export interface MeshOptions {
  /** Default `concave`. */
  readonly triangulation?: Triangulation;
  /** Default `hydra`. */
  readonly normals?: SmoothNormals;
}

/** A run of triangles in {@link StagehandMesh.indices}: one subset's faces, or the faces in no subset. */
export interface DrawRange {
  /** The subset's position in {@link MeshInput.subsets}; `null` for the faces in no subset (always last). */
  readonly subset: number | null;
  /** The first index (three per triangle). */
  readonly first: number;
  /** The number of indices. */
  readonly count: number;
}

/** What became of a primvar channel. */
export type ChannelState =
  | { readonly state: "absent" }
  | { readonly state: "used"; readonly interpolation: Interpolation; readonly indexed: boolean }
  | { readonly state: "dropped"; readonly reason: string };

/** A note about one authored face. */
export interface FaceNote {
  readonly face: number;
  /**
   * `hole`, `degenerate` (fewer than three vertices), `concaveFanned`
   * (a concave face fanned under `hydraFan`), `concaveClipped`, or
   * `concaveFellBack` (the ear clipper refused it: `error` says why, and it
   * was fanned).
   */
  readonly kind: "hole" | "degenerate" | "concaveFanned" | "concaveClipped" | "concaveFellBack";
  readonly error?: string;
}

/** What a mesh plan made of a mesh: enough to check the buffers without reading them. */
export interface MeshReport {
  /** Why nothing is drawn, when the topology is invalid. */
  readonly topologyProblem: { readonly code: string; readonly reason: string } | null;
  readonly triangulation: Triangulation;
  /** How smooth normals were computed, when `normals` is `computed`. */
  readonly smoothNormals: SmoothNormals;
  readonly orientation: "rightHanded" | "leftHanded";
  /** The subdivision scheme; with `refined` false, a subdivision surface is drawn as its control cage. */
  readonly subdivisionScheme: string;
  /** Always false: stage 1 draws control cages. */
  readonly refined: false;
  readonly faces: number;
  readonly drawnFaces: number;
  readonly points: number;
  readonly triangles: number;
  /** Distinct corners: a point with its normal, `st` and color. */
  readonly vertices: number;
  readonly triangleCorners: number;
  readonly holeFaces: number;
  readonly degenerateFaces: number;
  readonly concaveFaces: number;
  readonly clippedFaces: number;
  readonly fallbackFaces: number;
  /** Hole indices that skipped no face (unsorted, repeated or out of range), as in Hydra. */
  readonly unmatchedHoleIndices: number;
  readonly unassignedFaces: number;
  readonly normals: "computed" | "authored";
  readonly channels: {
    readonly normals: ChannelState;
    readonly st: ChannelState;
    readonly displayColor: ChannelState;
  };
  /** Each subset's part, in the input's order. */
  readonly subsets: readonly {
    readonly faces: number;
    readonly triangles: number;
    readonly outOfRange: number;
    readonly alreadyClaimed: number;
  }[];
  /** Each hole, degenerate and concave face, in face order. */
  readonly faceNotes: readonly FaceNote[];
}

/** An implicit shape's axis. */
export type Axis = "X" | "Y" | "Z";

/** A `UsdGeom` implicit shape with its size attributes. */
export type Shape =
  | { readonly kind: "Cube"; readonly size: number }
  | { readonly kind: "Sphere"; readonly radius: number }
  | { readonly kind: "Cylinder"; readonly radius: number; readonly height: number; readonly axis: Axis }
  | { readonly kind: "Cone"; readonly radius: number; readonly height: number; readonly axis: Axis }
  | { readonly kind: "Capsule"; readonly radius: number; readonly height: number; readonly axis: Axis }
  | { readonly kind: "Plane"; readonly width: number; readonly length: number; readonly axis: Axis };

/** How finely curved shapes are cut. */
export interface Tessellation {
  /** Segments around the axis (at least 3). Default 32. */
  readonly segments?: number;
  /** Bands from pole to pole of a sphere, and of a capsule's caps together (at least 2). Default 16. */
  readonly rings?: number;
}
