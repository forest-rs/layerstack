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

/** A texture coordinate set besides `st`, by its primvar's name. */
export interface UvSetInput {
  /** The primvar's name, without `primvars:`. */
  readonly name: string;
  readonly primvar: PrimvarInput;
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
  /**
   * Texture coordinate sets besides `st`: `float2` primvars a material
   * reads by name (`st1`, `uv`), each drawn as a channel of its own
   * ({@link StagehandMesh.uvSets}), splitting vertices where it differs.
   */
  readonly uvSets?: readonly UvSetInput[];
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
  /** Each texture coordinate set's fate besides `st`, in the input's order. */
  readonly uvSets: readonly { readonly name: string; readonly channel: ChannelState }[];
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
  | { readonly kind: "Plane"; readonly width: number; readonly length: number; readonly axis: Axis }
  /** `Cylinder_1`: a radius per end, `radiusTop` at `+axis`. */
  | {
      readonly kind: "Cylinder_1";
      readonly radiusTop: number;
      readonly radiusBottom: number;
      readonly height: number;
      readonly axis: Axis;
    }
  /** `Capsule_1`: a capping sphere radius per end, meeting the slanted side tangentially. */
  | {
      readonly kind: "Capsule_1";
      readonly radiusTop: number;
      readonly radiusBottom: number;
      readonly height: number;
      readonly axis: Axis;
    };

/** How finely curved shapes are cut. */
export interface Tessellation {
  /** Segments around the axis (at least 3). Default 32. */
  readonly segments?: number;
  /** Bands from pole to pole of a sphere, and of a capsule's caps together (at least 2). Default 16. */
  readonly rings?: number;
}

/** A curve schema and its own attributes, as {@link CurvesInput.form}. */
export type CurveForm =
  /** `BasisCurves`: its `type`, `basis` and `wrap` tokens (fallbacks `cubic`, `bezier`, `nonperiodic`). A token the schema does not allow draws the curves as linear (nonperiodic for a wrap), named in {@link CurvesReport.fallbacks}. */
  | { readonly kind: "basis"; readonly type: string; readonly basis: string; readonly wrap: string }
  /** `NurbsCurves`: `order` per curve, `knots` one curve after another, `ranges` (pairs, flat; empty for each whole domain), `pointWeights` (empty for all 1). */
  | {
      readonly kind: "nurbs";
      readonly order: Int32Array;
      readonly knots: Float64Array;
      readonly ranges: Float64Array;
      readonly weights: Float64Array;
    }
  /** `HermiteCurves`: one tangent per point, three numbers each. */
  | { readonly kind: "hermite"; readonly tangents: Float32Array };

/** Curves as typed arrays: what {@link Stagehand.buildCurves} lays out. {@link readCurves} reads them off a stage. */
export interface CurvesInput {
  readonly form: CurveForm;
  /** `curveVertexCounts`. */
  readonly curveVertexCounts: Int32Array;
  /** `points`, three numbers each. */
  readonly points: Float32Array;
  /** `widths` (one number per element), at its interpolation; every width 1 without. */
  readonly widths?: PrimvarInput | null;
  /** `normals`: with them the curves sweep as ribbons facing them, without as tubes. */
  readonly normals?: PrimvarInput | null;
  /** `primvars:displayColor`. */
  readonly displayColor?: PrimvarInput | null;
  /** `primvars:displayOpacity` (one number per element). */
  readonly displayOpacity?: PrimvarInput | null;
}

/** How curves are sampled and drawn, as `layerstack_stagehand::CurveOptions`. */
export interface CurveOptions {
  /** `lines` (default): line segments only; `swept`: tubes of {@link sides} sides, or ribbons where normals are authored, as triangles too. */
  readonly style?: "lines" | "swept";
  /** Samples per cubic segment and per NURBS knot span. Default 8. */
  readonly samples?: number;
  /** Sides of a tube (at least 3). Default 8. */
  readonly sides?: number;
}

/**
 * Why a NURBS curve is not drawn, as `exedra_curves::NurbsError`:
 * `order` (below 2), `tooFewPoints` (fewer than the order), `knotCount`
 * (not points + order), `knots` (decreasing or not finite), `weightCount`
 * (not one per point), `weight` (not positive and finite), `emptyDomain`
 * (its first and last knots are equal), `emptyRange` (its `ranges` entry
 * meets the domain in a single point or not at all), `point` (a control
 * coordinate not finite, or not finite once weighted), `other` (a reason this
 * version of the module does not name).
 */
export type NurbsCode =
  | "order"
  | "tooFewPoints"
  | "knotCount"
  | "knots"
  | "weightCount"
  | "weight"
  | "emptyDomain"
  | "emptyRange"
  | "point"
  | "other";

/** What a curves plan made of its input, as `layerstack_stagehand::CurvesReport`. */
export interface CurvesReport {
  /** How the curves were evaluated: the type, basis (`null` for linear) and wrap used. */
  readonly evaluation:
    | { readonly kind: "basis"; readonly type: "linear" | "cubic"; readonly basis: "bezier" | "bspline" | "catmullRom" | null; readonly wrap: "nonperiodic" | "periodic" | "pinned" }
    | { readonly kind: "nurbs" }
    | { readonly kind: "hermite" };
  /** Tokens drawn as something else. */
  readonly fallbacks: readonly { readonly attribute: "type" | "basis" | "wrap"; readonly token: string; readonly used: string }[];
  readonly curves: number;
  readonly drawnCurves: number;
  /** Segments drawn per curve (knot spans for NURBS); 0 for a curve not drawn. */
  readonly segments: readonly number[];
  /** Samples per segment; linear curves are drawn through their vertices. */
  readonly samples: number;
  /** Samples in the buffers, a closed curve's first repeated at its end. */
  readonly vertices: number;
  readonly lines: number;
  readonly triangles: number;
  /** What the curves were swept as. */
  readonly geometry: { readonly kind: "lines" } | { readonly kind: "tubes"; readonly sides: number } | { readonly kind: "ribbons" };
  readonly channels: {
    readonly widths: ChannelState;
    readonly normals: ChannelState;
    readonly displayColor: ChannelState;
    readonly displayOpacity: ChannelState;
  };
  /** Curves drawn otherwise than authored: `skipped` (too few vertices), `repeated` (fewer than a segment, the last vertex repeated as Hydra does), `unused` (vertices past the last Bezier segment), `nurbs` (not evaluable, by {@link NurbsCode}). */
  readonly notes: readonly (
    | { readonly curve: number; readonly kind: "skipped" | "repeated"; readonly count: number }
    | { readonly curve: number; readonly kind: "unused"; readonly unused: number }
    | { readonly curve: number; readonly kind: "nurbs"; readonly code: NurbsCode }
  )[];
  /** Swept stations at a repeated point, a cusp, a zero width or a ribbon normal along the curve. */
  readonly degenerateStations: number;
  /** Samples whose width was negative or not finite, drawn as 0: a Catmull-Rom chain of widths can undershoot, and a pinned end's reflected width can fall below zero. */
  readonly clampedWidths: number;
  /** Why nothing is drawn: `negativeCount`, `pointCount` or `arrays`. */
  readonly problem: { readonly code: "negativeCount" | "pointCount" | "arrays"; readonly reason: string } | null;
  /** `UsdGeomCurves::ComputeExtent`: the points' bounds grown by half the largest width. */
  readonly extent: readonly [readonly [number, number, number], readonly [number, number, number]] | null;
}

/** A `Points` as typed arrays: what {@link fillPoints} fills. {@link readPointSet} reads one off a stage. */
export interface PointsInput {
  /** `points`, three numbers each. */
  readonly points: Float32Array;
  /** `widths` (diameters, one number per element): `vertex`, `varying` or `constant`; every width 1 without. */
  readonly widths?: PrimvarInput | null;
  /** `normals`: with them each point is a disc facing its normal, without a sphere. */
  readonly normals?: PrimvarInput | null;
  readonly displayColor?: PrimvarInput | null;
  readonly displayOpacity?: PrimvarInput | null;
  /** `ids`: each point's identity over time, one per point, or empty. */
  readonly ids?: BigInt64Array | null;
}

/** What {@link fillPoints} made of a point set, as `layerstack_stagehand::PointsReport`. */
export interface PointsReport {
  readonly points: number;
  /** `spheres`, or `discs` when normals are drawn. */
  readonly shape: "spheres" | "discs";
  readonly channels: {
    readonly widths: ChannelState;
    readonly normals: ChannelState;
    readonly displayColor: ChannelState;
    readonly displayOpacity: ChannelState;
    readonly ids: ChannelState;
  };
  /** `UsdGeomPoints::ComputeExtent`; `null` when widths are given but not one per point. */
  readonly extent: readonly [readonly [number, number, number], readonly [number, number, number]] | null;
}

/** A point set's buffers, one entry per point. */
export interface PointBuffers {
  readonly positions: Float32Array;
  /** Diameters (1 where `widths` is not drawn). */
  readonly widths: Float32Array;
  /** Unit normals, three per point, for discs. */
  readonly normals: Float32Array | null;
  readonly colors: Float32Array | null;
  readonly opacities: Float32Array | null;
  /** Each point's id when `ids` is drawn; empty otherwise. */
  readonly ids: BigInt64Array;
  readonly report: PointsReport;
}
