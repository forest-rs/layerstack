// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js's `WebGPURenderer` draws what `../model/draws.ts`
// keeps of the stage. Every matrix and color comes from layerstack and
// every triangle from stagehand's buffers (indexed, with normals,
// per-vertex colors and a group per subset range); three.js only
// rasterizes them. No USD loader is involved. Each update applies the draw
// list's diffs in place: an `InstancedMesh` per instanced item whose
// matrices are rewritten, a buffer geometry per stagehand buffer whose
// attributes are refilled, a three.js light per `UsdLux` light the diff
// adds or changes, and nothing rebuilt that the diffs do not name. The
// renderer is the editor's one WebGPU renderer (`gpu.ts`), drawing into
// this canvas as one of its canvas targets.
//
// The camera is the stage's (`UsdGeomCamera`, its view and projection
// matrices as `GfFrustum` computes them, the projection's depth taken from
// OpenGL's [-1, 1] to WebGPU's [0, 1]), fitted to the canvas by
// `CameraUtilConformWindow`'s `fit` policy (everything the camera frames
// stays in view), when the editor looks through one; otherwise orbit
// controls move a free camera.
//
// Lights: `UsdLux` emission is luminance in nits (cd/m²); stagehand's
// `LightItem.emission.radiance` is `color * blackbody * intensity *
// 2^exposure / sizeFactor` per channel. Since r155, three.js lights are in
// physical units (`physicallyCorrectLights` is gone: always on): a
// `PointLight` or `SpotLight` in candela (cd, falling off as 1/d²), a
// `DirectionalLight` in lux (lx), a `RectAreaLight` in nits, a
// `HemisphereLight` as the irradiance (lx) it adds, and a Lambertian
// surface reflects irradiance × albedo / π. Each light's color is its
// radiance divided by its largest channel, its intensity that channel
// times:
//
// - `SphereLight` → `PointLight`: the sphere's projected area, a quarter
//   of its world surface area (luminous intensity = luminance × projected
//   area). `treatAsPoint` changes nothing: a point light either way.
// - `DiskLight` → `SpotLight` along -Z: the disk's world area (its
//   intensity on its axis); `ShapingAPI`'s cone angle is the spot's angle
//   and its softness the penumbra (`focus` and IES profiles are not
//   drawn). Without `ShapingAPI`, a 90° spot (a hemisphere).
// - `CylinderLight` → `PointLight`: its side's projected area across its
//   axis (`2 r length`, its world side area / π).
// - `RectLight` → `RectAreaLight` facing -Z, its width and height scaled
//   by the transform: the radiance itself (its `texture:file` is not
//   loaded). Every rect light shades with one shared pair of lookup
//   tables (`rectAreaLights.ts`), not two per light, per object.
// - `DistantLight` → `DirectionalLight` along -Z: the illuminance of a disk
//   of the light's angular size, radiance × π sin²(angle / 2) (the
//   `sizeFactor` `UsdLux` normalizes by, so a normalized light's intensity
//   is its illuminance in lux), or the radiance itself at angle 0.
// - `DomeLight` → `HemisphereLight` of one color: a uniform dome of
//   luminance L gives an upward surface π L of irradiance (its texture is
//   not loaded).
// - `PortalLight`: not drawn; it only guides a dome's light.
// - Mesh lights and `GeometryLight`s: not drawn as three.js lights, which
//   have no mesh shape.
//
// Materials: each draw's bound `UsdPreviewSurface` record (stagehand's
// `MaterialItem`) becomes a three.js material as `surfaces.ts` maps it, a
// range's subset material for that range of the geometry's groups; a draw
// without a material is drawn in its `displayColor` (or gray), as before.
// A material the diffs change restyles the draws binding it, and nothing
// else.
//
// A pixel then holds luminance in nits, which the `UsdLux` documentation
// makes 1 for a 1-nit light seen directly at exposure 0: the renderer's
// `toneMappingExposure` is the stage camera's
// `UsdGeomCamera::ComputeLinearExposureScale` (1 with the free camera), and
// AgX tone mapping fits the highlights into the display's range. WebGPU's
// renderer tone-maps and encodes sRGB in a pass of its own once the scene
// is drawn (and blended, and its 4× multisampling resolved); the canvas is
// cleared transparent, so the page's background is the viewport's.
//
// Default lights (`defaultLights.ts`): usdview's. A point light at the
// camera, moved to it on each draw, and a faint ambient light, added to
// the stage's own lights whether or not it has any, as usdview adds its
// default camera light; the camera light can be switched off
// (`setLightSettings`), as usdview's View > Lights menu does. Switching it
// zeroes its intensity rather than removing it, so the lit materials keep
// their pipelines (three.js builds a shader per set of lights).
//
// Light linking: three.js lights every lit material with every light in
// the scene, unless the material names its own lights: a node material's
// `lightsNode` (`lights([...])`). So a draw that `ViewportScene.unlit`
// (stagehand's evaluated `lightLink`s) leaves out of some `UsdLux` lights
// has its materials lit by the rest, the default lights (the camera light,
// the ambient) always among them; the others use the scene's. Light sets
// are shared (one `LightsNode` per set of lights, `#lightSets`), and a
// shader is built per material shape and light set, so relinking compiles
// only the shapes that meet a set for the first time; a set already met
// rewrites nothing. A set is a fixed node (`fixedNodes.ts`): three.js does
// not refresh each linked draw in full on each frame for it. Limits: one
// material per `InstancedMesh`, so a prototype's draw is lit as its first
// instance is; lines and points are unlit anyway; and `shadowLink` is
// shown but not drawn, as the viewport casts no shadows.
//
// Skinning (`../model/skinning.ts`, `skinning.ts`): a draw a skeleton
// deforms on the GPU is a `SkinnedDraw` over its stagehand buffer, whose
// `skinIndex` and `skinWeight` (eight per vertex) are the joint influences
// gathered into vertex order, with a `Skeleton` of its own: one bone per
// joint of the binding, each bone's world matrix the joint's skinning
// transform at the time and each bone inverse the geometry's bind
// transform. The shader moves a rest point `p` by
// `Σ w · skinningTransform · bindTransform · p`, the points UsdSkel skins
// into the skeleton's space, skins normals as `UsdSkelSkinNormals` does,
// and the object is placed by the skeleton's world transform. A time
// change rewrites the bones' matrices and nothing else. A mesh with blend
// shapes skins its blend-shaped rest pose, uploaded once (again only when
// the animation's weights vary). A draw deformed on the CPU (dual
// quaternions) is a plain mesh whose positions and normals are refilled
// each time.
//
// Vertices (stagehand's `DrawBuffers.vertices`): uploaded in stagehand's
// compact format as they are, `snorm16x4` positions, `snorm8x4` normals
// and `float16x2` `st` (WebGPU vertex formats, which three.js picks from
// the arrays' types and `normalized`; half floats are given as a
// `Float16Array` view of stagehand's bits, which three.js maps to
// `float16`). A position decodes to its point by one uniform scale and an
// offset per buffer, folded into the object's matrix (each instance's, for
// an `InstancedMesh`: three.js applies the instance matrix first): normals,
// not quantized relative to the bounds, are transformed as before, since a
// uniform scale only changes their length. A skinned draw's positions are
// decoded to `f32` instead (skinning runs before the model matrix). Once a
// render has uploaded a static buffer's index and positions (three.js
// keeps a GPU buffer per attribute), the arrays are dropped, three.js's and
// stagehand's (`DrawList.release`): the GPU holds them, and nothing reads
// them on the CPU after upload (bounds are computed before, framing reads
// the stage's bounds, picking is on the GPU). A lost device loses them: the
// view lets everything go, and once the shared renderer is made again, the
// draw list fills every released buffer again (`DrawList.restore`) and the
// view is made again from it.
//
// A mesh with a prebuilt cluster set (`clusters.ts`) is resident once: its
// draws wait, undrawn, while its set is fetched, then draw from the set's
// storage buffers, and its own geometry is never uploaded; its arrays are
// released then as an upload would release them. A set that fails to
// arrive lets its draws draw as any other.
//
// Points are drawn as camera-facing squares, one instanced quad per point
// (WebGPU rasterizes point primitives one pixel wide): each point's width
// is its diameter in world units, attenuated with distance as
// `PointsMaterial` attenuated a point's size.
//
// Pipelines: an update that adds draws or changes materials asks three.js
// to compile what the scene now needs off the main thread
// (`compileAsync`); the draw that follows skips what is still compiling,
// so a new material pops in a frame or so later instead of holding the
// page.
//
// Bundles (`bundles.ts`): once its compile is done, an opaque draw is
// recorded into a WebGPU render bundle (three.js's `BundleGroup`, up to
// {@link BUNDLE_CAPACITY} draws each) that each frame replays, rather than
// three.js comparing its matrix, material, geometry and lights with what
// it last drew, which was most of a frame's CPU time with a thousand draws
// or more. A replay does not look at the draws, so the view records a
// bundle again whenever it changes one of its draws (placed, posed, given
// another geometry, refilled, relinked), all of them when the scene's
// lights change, and a draw with new materials is drawn directly until
// they compile. Transparent and transmissive draws stay direct (three.js
// sorts them each frame, and transmission reads the frame drawn so far),
// as does a draw that moves as time plays. Bundled draws are not culled
// against the view, as direct ones are: a replay draws them all. Picking
// draws its own bundles with the pick materials, recorded again when the
// view's are.
//
// Picking (`picking.ts`): a click renders the scene once more into a
// 7×7-pixel RGBA8 target about the point, every draw with a pick material
// (one for surfaces and lines, one for points) that writes its object's
// pick id plus the instance index, and reads it back with
// `readRenderTargetPixelsAsync` a frame or so later. The center pixel
// decides; a line or point within three pixels of it is picked when the
// center shows nothing, as the ray's threshold let a thin line be.
import {
  AgXToneMapping,
  AmbientLight,
  Bone,
  BufferAttribute,
  BundleGroup,
  BufferGeometry,
  Camera,
  CanvasTarget,
  Color,
  DirectionalLight,
  DoubleSide,
  GridHelper,
  HemisphereLight,
  InstancedBufferAttribute,
  InstancedBufferGeometry,
  InstancedMesh,
  InterleavedBuffer,
  InterleavedBufferAttribute,
  LineBasicNodeMaterial,
  LineSegments,
  Material,
  Matrix4,
  Mesh,
  MeshStandardNodeMaterial,
  NoBlending,
  NodeMaterial,
  Object3D,
  PerspectiveCamera,
  PointLight,
  PointsNodeMaterial,
  Quaternion,
  RectAreaLight,
  RenderTarget,
  Scene,
  Skeleton,
  SpotLight,
  Vector2,
  Vector3,
  WebGPUCoordinateSystem,
} from "three/webgpu";
import type { LightsNode, WebGPURenderer } from "three/webgpu";
import { attribute, instanceIndex, lights, materialColor, max, positionView, renderGroup, screenDPR, uniform, varying, vec4 } from "three/tsl";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { conformCamera, lightSizeFactor, vertexPositions } from "@forest-rs/layerstack_stagehand";
import type { BufferCost, CameraItem, LightItem, MaterialItem, MeshVertices, VertexAttribute } from "@forest-rs/layerstack_stagehand";
import { PickIds, pickOrder, pickWindow, readIndex, unpackPickId } from "./picking.ts";
import { DEFAULT_LIGHT_SETTINGS, defaultLights, type LightSettings } from "./defaultLights.ts";
import { DrawBundles, type BundleCounts } from "./bundles.ts";
import { lightSetKey, linkedLights } from "./lightLinks.ts";
import { useSharedRectAreaTables } from "./rectAreaLights.ts";
import { fixNodes, setFixedNode } from "./fixedNodes.ts";
import { onRendererLost, sharedRenderer } from "./gpu.ts";
import { SkinnedDraw, withSkinning } from "./skinning.ts";
import { TextureStore, surfaceMaterial, uvChannels } from "./surfaces.ts";
import { standinGeometry, standinMaterials, standinTextured } from "./standins.ts";
import { ClusterScene, MAX_CLUSTER_TRIANGLES, clusterable, packVertices } from "./clusters.ts";
import type { PackedVertices } from "./clusters.ts";
import type { ClusterResidency, ClusterStats, ClusterView } from "./clusters.ts";
import { ClusterManifest, clusterKey, readClusterFile, residentCost } from "./clusterFile.ts";
import type { ManifestMesh } from "./clusterFile.ts";
import type { AssetExists, AssetReader, TextureStats } from "./surfaces.ts";
import type { ViewItem, ViewportScene, ViewportUpdate } from "../model/draws.ts";

/** A clustered draw's last cut, read back ({@link Viewport.clusterCut}). */
export interface ClusterCut {
  /** The key of the set's file. */
  readonly key: string;
  /** The visible clusters: each an instance of the item and a cluster of its set. */
  readonly pairs: readonly { readonly instance: number; readonly cluster: number }[];
  /** The world-space view the cut was chosen for. */
  readonly camera: { readonly position: readonly number[]; readonly projectionY: number; readonly viewportHeight: number; readonly zNear: number };
  readonly view: ClusterView;
}

/** The lit materials the viewport makes: a surface's, a stand-in card's, or a plain color's. */
type LitMaterial = MeshStandardNodeMaterial;

/** What `materials` gives a draw. */
type Styled = LitMaterial | LitMaterial[] | LineBasicNodeMaterial | PointsNodeMaterial;

/** Anything TSL takes: the node types are too loose to be worth spelling here. */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
type Node = any;

/** A three.js view of a {@link ViewportScene}, updated in place from each {@link ViewportUpdate}. */
export class Viewport {
  #renderer: WebGPURenderer;
  /** This canvas, as the shared renderer's target. */
  readonly #target: CanvasTarget;
  readonly #scene = new Scene();
  /** The free camera, which the orbit controls move. */
  readonly #camera = new PerspectiveCamera(45, 1, 0.05, 500);
  /** The stage's camera: its matrices are set from layerstack's, never computed by three.js. */
  readonly #stageCamera = new Camera();
  #controls: OrbitControls;
  /** The ground grid, across the stage's up axis, its cells a power of ten sized to what is framed. */
  #grid: GridHelper;
  #cell = 1;
  /** The stage's up axis: the free camera's up, and the grid's normal. */
  #up: "Y" | "Z" = "Y";
  readonly #canvas: HTMLCanvasElement;
  /** The stage camera to look through, by path; `null` for the free camera. */
  #through: string | null = null;
  /** The stage's cameras, by path, as last listed. */
  #cameras = new Map<string, CameraItem>();
  readonly #resizer: ResizeObserver;
  /** The three.js objects of each `UsdLux` light, by key (its path, or path and instance): the light and, for a directional or spot light, its target. */
  readonly #lights = new Map<string, Object3D[]>();
  /** The `UsdLux` light each three.js light draws, by object. */
  readonly #lightKeys = new Map<Object3D, string>();
  /** usdview's default camera light: a point light without falloff, put at the camera on each draw. */
  readonly #cameraLight = new PointLight(0xffffff, defaultLights(DEFAULT_LIGHT_SETTINGS).cameraLight, 0, 0);
  /** usdview's scene ambient. */
  readonly #ambient = new AmbientLight(0xffffff, defaultLights(DEFAULT_LIGHT_SETTINGS).ambient);
  /** One geometry per stagehand buffer, by key: a sphere of one radius is uploaded once. */
  readonly #geometries = new Map<number, BufferGeometry>();
  /**
   * One object per draw item, by id: a `Mesh` for a gprim, an
   * `InstancedMesh` for a group, `LineSegments` for curves drawn as lines,
   * a `Mesh` of a quad per point for a point set.
   */
  readonly #objects = new Map<number, Drawn>();
  /** Where each object is drawn from: a bundle, or the scene directly (see the header). */
  readonly #draws: DrawBundles<Drawn>;
  /** Whether an update was applied yet. */
  #synced = false;
  /** The selection the materials were last made for. */
  #selected: string | null = null;
  /** The bounds the camera was last framed on, to frame again only when they change. */
  #framed = "";
  /** The textures the materials read. */
  readonly #textures: TextureStore;
  /** Whether a restyle for arrived images is scheduled. */
  #restyling = false;
  /** The scene last applied, to restyle textured draws when their images arrive. */
  #applied: ViewportScene | null = null;
  /**
   * Materials a restyle replaced, disposed after the next render. A
   * material takes its pipeline when it is first drawn, so disposing the
   * one it replaces before then would release the pipeline while its
   * replacement still needs it, and three.js would build it again.
   */
  readonly #retired: Material[] = [];
  /** Geometries of static buffers not yet seen uploaded, released once a render has ({@link onUploaded}). */
  readonly #pending = new Set<BufferGeometry>();
  /** GPU picking's target, materials and ids, made at the first pick. */
  #picker: Picker | null = null;
  /** The pick in flight, which the next follows. */
  #picking: Promise<unknown> = Promise.resolve();
  /** The light sets of linked draws' materials, by their lights' ids: one `LightsNode` per set. */
  #lightSets = new Map<string, LightsNode>();
  /** Whether a lost device left the view without a renderer, until the next is made. */
  #deviceLost = false;
  /** Stops listening for a lost device. */
  readonly #unlisten: () => void;
  /** The draws of meshes with a prebuilt cluster set, culled and cut on the GPU (`clusters.ts`). */
  readonly #clusters = new ClusterScene();
  /** The keys of the prebuilt sets the dev server has; `null` until listed (or without a server). */
  #clusterKeys: Set<string> | null = null;
  /** Buffers whose set is being fetched, by buffer key: the geometry it is for, so a rebuilt buffer drops the answer. */
  readonly #clusterLoads = new Map<number, BufferGeometry>();
  /** Each clustered buffer's set key, for checking a cut against the set's file. */
  readonly #clusterKeyOf = new Map<number, string>();
  /** The stages' manifests: sets found by prim before a mesh is built, for the budget ({@link #reportResident}). */
  #manifest = new ClusterManifest([]);
  /** Prims whose mesh, once built, did not have the set its manifest named: weighed as any other from then on. */
  readonly #manifestMisses = new Set<string>();
  /** Buffers looked up before the server listed its sets, fetched once it has (when it has theirs). */
  #clusterWaiting: { buffer: number; geometry: BufferGeometry; key: string; vertices: PackedVertices; decode: readonly [number, number, number, number] }[] = [];
  /** Buffers whose set arrived (or failed to), their draws not yet made again ({@link #convertArrived}). */
  readonly #clusterArrived = new Set<number>();
  /** Items not drawn yet because their buffer's set is on its way: drawn from it once it arrives, so their own geometry is never uploaded. */
  readonly #awaitingSet = new Set<number>();

  /**
   * A view drawing into `canvas` with `renderer`, the shared one
   * (`sharedRenderer`, initialized). `read` and `exists` reach the stage's
   * assets (textures and UDIM tiles) as the asset loop's host does; without
   * them textures draw as their fallbacks.
   */
  constructor(canvas: HTMLCanvasElement, renderer: WebGPURenderer, read: AssetReader = async () => null, exists: AssetExists = async () => false) {
    // Images arrive one by one: the draws are restyled once per batch of them.
    this.#textures = new TextureStore(read, exists, () => {
      this.onImages?.();
      if (this.#restyling) return;
      this.#restyling = true;
      setTimeout(() => {
        this.#restyling = false;
        this.#restyleTextured();
      }, 100);
    });
    this.#canvas = canvas;
    this.#renderer = renderer;
    this.#target = new CanvasTarget(canvas);
    this.#target.setPixelRatio(window.devicePixelRatio);
    useSharedRectAreaTables();
    this.#scene.add(this.#cameraLight, this.#ambient);
    this.#draws = new DrawBundles<Drawn>(
      this.#scene,
      () => {
        const bundle = new BundleGroup();
        this.#scene.add(bundle);
        return bundle;
      },
      BUNDLE_CAPACITY,
    );
    this.#stageCamera.matrixAutoUpdate = false;
    this.#stageCamera.matrixWorldAutoUpdate = false;
    // The stage camera's projection is converted to WebGPU's depth range here ({@link #prepareCamera}).
    this.#stageCamera.coordinateSystem = WebGPUCoordinateSystem;
    this.#camera.coordinateSystem = WebGPUCoordinateSystem;
    this.#camera.updateProjectionMatrix();
    this.#grid = makeGrid("Y", 1);
    this.#scene.add(this.#grid);
    this.#controls = this.#makeControls();
    this.#resizer = new ResizeObserver(() => this.#resize());
    this.#resizer.observe(canvas);
    this.#unlisten = onRendererLost(() => this.#lost());
    this.#clusters.onRepacked = () => {
      // New arguments and contexts: every bundle recorded again, the new
      // pipelines compiled once this render is done (a packing happens
      // within one, and three.js compiles one scene at a time).
      this.#draws.touchAll();
      setTimeout(() => this.#compile(), 0);
    };
    // The prebuilt cluster sets the dev server holds (`vite.config.ts`); none elsewhere.
    fetch(new URL("/@clusters/index.json", window.location.href)).then(
      async (response) => {
        this.#listed(response.ok && (response.headers.get("content-type") ?? "").includes("json") ? new Set((await response.json()) as string[]) : new Set());
      },
      () => this.#listed(new Set()),
    );
    fetch(new URL("/@clusters/manifests.json", window.location.href)).then(
      async (response) => {
        if (!response.ok || !(response.headers.get("content-type") ?? "").includes("json")) return;
        this.#manifest = new ClusterManifest((await response.json()) as { meshes: ManifestMesh[] }[]);
        this.#reportResident();
      },
      () => undefined,
    );
  }

  /** The server's sets listed: the buffers looked up meanwhile fetched when it has theirs. */
  #listed(keys: Set<string>): void {
    this.#clusterKeys = keys;
    const waiting = this.#clusterWaiting;
    this.#clusterWaiting = [];
    for (const { buffer, geometry, key, vertices, decode } of waiting) {
      if (this.#geometries.get(buffer) !== geometry) continue;
      if (keys.has(key)) {
        this.#fetchClusters(buffer, geometry, key, vertices, decode);
        continue;
      }
      // No set after all: its waiting draws drawn as any other.
      delete geometry.userData.awaitingSet;
      this.#clusterArrived.add(buffer);
    }
    this.#convertArrived();
  }

  /** How the clustered draws choose their cuts: the pixel error, the smallest instance drawn, culling. */
  get clusterView(): ClusterView {
    return this.#clusters.view;
  }

  /** Sets how the clustered draws choose their cuts, and draws again. */
  setClusterView(view: Partial<ClusterView>): void {
    Object.assign(this.#clusters.view, view);
    this.#draw();
  }

  /** What the clustered draws drew in a recent frame (read back every few frames); `null` before one. */
  get clusterStats(): ClusterStats | null {
    return this.#clusters.stats;
  }

  /** What the clustered draws hold: meshes, draws, instances, clusters, every level's triangles and the bytes uploaded. */
  get clusterResidency(): ClusterResidency {
    return this.#clusters.residency;
  }

  /**
   * The clusters the GPU's last cut drew for the item `id`, as (instance,
   * cluster) pairs within the item and its mesh's set, read back: for
   * checking the cut against the library's, with the set's key and the
   * world-space view the cut was chosen for. `null` for an item not drawn
   * through its cluster set.
   */
  async clusterCut(id: number): Promise<ClusterCut | null> {
    const object = this.#objects.get(id);
    if (object === undefined || !this.#clusters.isDraw(object)) return null;
    const pairs = await this.#clusters.visible(this.#renderer, object);
    const key = this.#clusterKeyOf.get(object.userData.clusterKey as number) ?? "";
    const camera = this.#clusters.cutCamera;
    return { key, pairs, camera: { ...camera, position: [...camera.position] }, view: { ...this.#clusters.view } };
  }

  /**
   * A lost device: every object and geometry let go (their GPU buffers went
   * with the device), and once the shared renderer is made again, the
   * draw list's owner refills what was released and updates the view from
   * nothing ({@link onContextRestored}).
   */
  #lost(): void {
    this.#deviceLost = true;
    for (const id of [...this.#objects.keys()]) this.#drop(id);
    // The clustered draws' buffers went with the device: their sets are fetched again as the restored buffers are made.
    this.#clusters.dispose();
    this.#clusterLoads.clear();
    this.#clusterArrived.clear();
    this.#clusterWaiting = [];
    this.#clusterKeyOf.clear();
    this.#awaitingSet.clear();
    for (const geometry of this.#geometries.values()) geometry.dispose();
    this.#geometries.clear();
    for (const material of this.#retired) material.dispose();
    this.#retired.length = 0;
    this.#pending.clear();
    this.#picker?.dispose();
    this.#picker = null;
    this.#synced = false;
    sharedRenderer().then(
      (renderer) => {
        this.#renderer = renderer;
        this.#deviceLost = false;
        this.onContextRestored?.();
        this.#draw();
      },
      (error: unknown) => console.error("WebGPU did not start again after a lost device", error),
    );
  }

  /**
   * Orients the view to the stage's `upAxis`: the free camera orbits
   * about it and the grid lies across it. Stage cameras are unaffected:
   * their matrices are the stage's.
   */
  setUpAxis(up: "Y" | "Z"): void {
    this.#up = up;
    this.#regrid();
    this.#camera.up.set(0, up === "Y" ? 1 : 0, up === "Z" ? 1 : 0);
    // The orbit controls take the camera's up when they are made.
    const target = this.#controls.target.clone();
    this.#controls.dispose();
    this.#controls = this.#makeControls();
    this.#controls.target.copy(target);
    this.#controls.enabled = this.#looking() === null;
    this.#framed = "";
    this.#draw();
  }

  /**
   * Looks through the stage camera at `path`, or with `null` through the
   * free camera the orbit controls move. A path the stage has no camera
   * at draws the free camera until one appears there.
   */
  lookThrough(path: string | null): void {
    this.#through = path;
    this.#controls.enabled = this.#looking() === null;
    this.#draw();
  }

  /**
   * Switches the default lights the stage's own are drawn with, as
   * usdview's View > Lights menu does (`defaultLights.ts`).
   */
  setLightSettings(settings: LightSettings): void {
    const lights = defaultLights(settings);
    this.#cameraLight.intensity = lights.cameraLight;
    this.#ambient.intensity = lights.ambient;
    this.#draw();
  }

  /** Frames the free camera on `bounds` (`[minX, minY, minZ, maxX, maxY, maxZ]`). */
  frame(bounds: readonly number[]): void {
    this.#frame(bounds);
    this.#draw();
  }

  /**
   * The prim drawn under the canvas point (`x`, `y` in CSS pixels from its
   * top left), nearest first: a gprim, the instance proxy an instanced draw
   * places there, or a point instancer; `null` over nothing. Read from the
   * GPU (see the header), so it answers a frame or so later; picks made
   * together answer in order.
   */
  pick(x: number, y: number): Promise<string | null> {
    const picked = this.#picking.then(() => this.#pick(x, y));
    this.#picking = picked.catch(() => null);
    return picked;
  }

  async #pick(x: number, y: number): Promise<string | null> {
    const canvas = this.#canvas;
    const renderer = this.#renderer;
    const { clientWidth: width, clientHeight: height } = canvas;
    if (width === 0 || height === 0 || this.#deviceLost) return null;
    const started = performance.now();
    const pick = (this.#picker ??= new Picker());
    // A run of ids per object, one per instance it draws, and its pick material in place of its own.
    pick.ids.clear();
    for (const [id, object] of this.#objects) {
      const clustered = this.#clusters.isDraw(object);
      object.userData.pickBase = pick.ids.add(id, object instanceof InstancedMesh ? object.count : clustered ? this.#clusters.instances(object) : 1) ?? 0;
      pick.swapped.set(object, object.material);
      object.material = isPoints(object) ? pick.points : clustered ? pick.clustered(this.#clusters.pickContext) : pick.surfaces;
      // A clustered draw of several subsets draws through its parts.
      if (clustered) {
        for (const part of this.#clusters.parts(object)) {
          if (part === object) continue;
          pick.swapped.set(part, part.material);
          part.material = object.material;
        }
      }
    }
    const camera = this.#prepareCamera();
    pick.projection.copy(camera.projectionMatrix);
    const ratio = canvas.width / width;
    camera.projectionMatrix.premultiply(pick.window.set(...(pickWindow(x * ratio, y * ratio, PICK_SIZE, PICK_SIZE, canvas.width, canvas.height) as Matrix16)));
    camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
    this.#grid.visible = false;
    try {
      this.#setState();
      renderer.setRenderTarget(pick.target);
      renderer.render(this.#scene, camera);
    } finally {
      renderer.setRenderTarget(null);
      for (const [object, material] of pick.swapped) object.material = material;
      pick.swapped.clear();
      this.#grid.visible = true;
      camera.projectionMatrix.copy(pick.projection);
      camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
    }
    const pixels = (await renderer.readRenderTargetPixelsAsync(pick.target, 0, 0, PICK_SIZE, PICK_SIZE)) as Uint8Array;
    this.lastPickMs = performance.now() - started;
    // The center pixel decides; off it, only a line or point is picked.
    for (const pixel of pick.order) {
      const hit = pick.ids.find(unpackPickId(pixels, 4 * readIndex(pixel, PICK_SIZE)));
      const object = hit === null ? undefined : this.#objects.get(hit.owner);
      if (hit === null || object === undefined) continue;
      if (pixel !== pick.order[0] && !(object instanceof LineSegments || isPoints(object))) continue;
      const view = this.#applied?.items.find((candidate) => candidate.item.id === hit.owner);
      if (view === undefined) continue;
      const { item } = view;
      if (item.source.kind === "pointInstancer") return item.source.instancer;
      return item.instances[hit.instance] ?? item.prim;
    }
    return null;
  }

  /** How long the last pick took, from its render to its pixels read back, in milliseconds. */
  lastPickMs = 0;

  /**
   * Called after a render uploaded a static buffer's geometry: releases
   * the buffer's arrays in the draw list (`DrawList.release`) and answers
   * whether it did, and the view then drops three.js's.
   */
  onUploaded: ((key: number, revision: number) => boolean) | null = null;

  /** Called after a render in which buffers were released, so a page can show what is held now. */
  onReleased: (() => void) | null = null;

  /**
   * Called when the shared renderer is made again after a lost device: the
   * uploaded buffers are gone, and the released ones must be filled again
   * (`DrawList.restore`) and the view updated with a reset.
   */
  onContextRestored: (() => void) | null = null;

  /** Called as each image arrives (or fails), so a page can show how many are still on their way. */
  onImages: (() => void) | null = null;

  /** What the view's textures hold: images decoded, still loading, and how many were scaled down to fit. */
  get textureStats(): TextureStats {
    return this.#textures.stats;
  }

  /** Releases the canvas target, geometries, materials and textures; the shared renderer stays. */
  dispose(): void {
    this.#unlisten();
    this.#picker?.dispose();
    this.#resizer.disconnect();
    this.#controls.dispose();
    for (const id of [...this.#objects.keys()]) this.#drop(id);
    for (const material of this.#retired) material.dispose();
    this.#retired.length = 0;
    for (const geometry of this.#geometries.values()) geometry.dispose();
    this.#geometries.clear();
    for (const path of [...this.#lights.keys()]) this.#dropLight(path);
    this.#clusters.dispose();
    this.#target.dispose();
  }

  /**
   * Applies `update` to what is drawn, `selected` highlighted, and frames
   * the camera when the bounds changed. Objects and geometries the update
   * does not name are left as they are.
   */
  update(scene: ViewportScene, update: ViewportUpdate, selected: string | null): void {
    this.#applied = scene;
    const items = new Map(scene.items.map((view) => [view.item.id, view]));
    // The first update this view sees (three.js loads after the first
    // render) starts from nothing, as a new stage's does.
    const reset = update.reset || !this.#synced;
    this.#synced = true;
    // A draw an update moves, poses or refills as time plays is drawn
    // directly from then on: recording its bundle each frame would cost more.
    const playing = update.timeChanged && !reset;
    if (reset) {
      this.#manifestMisses.clear();
      for (const id of [...this.#objects.keys()]) this.#drop(id);
      for (const geometry of this.#geometries.values()) geometry.dispose();
      for (const key of this.#geometries.keys()) this.#clusters.dropMesh(key);
      this.#geometries.clear();
    }
    for (const id of update.removed) this.#drop(id);
    for (const key of update.freed) {
      this.#geometries.get(key)?.dispose();
      this.#geometries.delete(key);
      this.#clusters.dropMesh(key);
    }
    // Each buffer's first view, and the objects drawing each buffer, once:
    // the loops below look them up by key.
    const views = new Map<number, ViewItem>();
    const drawing = new Map<number, number[]>();
    if (update.buffersAdded.size + update.rebuilt.size + update.refilled.size > 0) {
      for (const view of scene.items) if (!views.has(view.item.buffer)) views.set(view.item.buffer, view);
      for (const id of this.#objects.keys()) {
        const key = items.get(id)?.item.buffer;
        if (key === undefined) continue;
        const ids = drawing.get(key);
        if (ids === undefined) drawing.set(key, [id]);
        else ids.push(id);
      }
    }
    // New and rebuilt buffers: a new geometry, handed to the objects drawing
    // it, which are placed again (its positions' decode is in their matrices).
    const replace = new Set<number>();
    for (const key of new Set([...update.buffersAdded, ...update.rebuilt])) {
      const view = views.get(key);
      if (view === undefined) continue;
      this.#geometries.get(key)?.dispose();
      // A rebuilt buffer's set is another's: its draws drawn as before until its own arrives.
      const wasClustered = this.#clusters.hasMesh(key);
      this.#clusters.dropMesh(key);
      const geometry = this.#makeGeometry(view);
      this.#geometries.set(key, geometry);
      for (const id of drawing.get(key) ?? []) {
        const object = this.#objects.get(id)!;
        if (wasClustered && this.#clusters.isDraw(object)) {
          this.#drop(id);
          if (!update.added.has(id)) this.#add(items.get(id) ?? view, selected);
          continue;
        }
        object.geometry = geometry;
        replace.add(id);
      }
    }
    // Refilled buffers: the same indices, new vertex data in place.
    for (const key of update.refilled) {
      const geometry = this.#geometries.get(key);
      const view = views.get(key);
      if (geometry === undefined || view === undefined || update.rebuilt.has(key)) continue;
      if (view.buffers.standin !== null) {
        // A stand-in's cards may come and go: its geometry and object made again.
        geometry.dispose();
        this.#geometries.set(key, this.#makeGeometry(view));
        if (!update.added.has(view.item.id)) {
          this.#drop(view.item.id);
          this.#add(view, selected);
        }
        continue;
      }
      const first = drawing.get(key)?.[0];
      const object = first === undefined ? undefined : this.#objects.get(first);
      if (object !== undefined && !fits(object, view.kind)) {
        // A curve's widths crossed the line width: the same buffers, drawn the other way.
        this.#geometries.get(key)?.dispose();
        this.#geometries.set(key, this.#makeGeometry(view));
        this.#drop(view.item.id);
        this.#add(view, selected);
        continue;
      }
      if (view.kind === "lines" || view.kind === "points") {
        refillSamples(geometry, view);
        for (const id of drawing.get(key) ?? []) this.#changed(this.#objects.get(id), playing);
        continue;
      }
      // New vertices (and their decode) over the same indices, and the objects placed again.
      setVertices(geometry, view);
      geometry.computeBoundingSphere();
      for (const id of drawing.get(key) ?? []) replace.add(id);
    }
    const added = reset ? new Set(items.keys()) : update.added;
    for (const id of added) {
      const view = items.get(id);
      if (view !== undefined) this.#add(view, selected);
    }
    for (const id of update.rebound) {
      const view = items.get(id);
      const object = this.#objects.get(id);
      if (view === undefined || object === undefined) continue;
      object.geometry = this.#geometries.get(view.item.buffer)!;
      this.#changed(object, playing);
    }
    // Skinning bound again, begun or ended: the object made again over its buffer.
    for (const id of update.reskinned) {
      const view = items.get(id);
      if (added.has(id)) continue;
      this.#drop(id);
      if (view !== undefined) this.#add(view, selected);
    }
    for (const id of new Set([...update.matrices, ...update.visibility, ...replace])) {
      const view = items.get(id);
      if (view === undefined || added.has(id) || update.reskinned.has(id)) continue;
      if (playing) this.#changed(this.#objects.get(id), true);
      this.#place(view, selected);
    }
    for (const id of update.skinned) {
      const view = items.get(id);
      if (view !== undefined && !added.has(id) && !update.reskinned.has(id)) this.#pose(view);
    }
    const restyle = selected !== this.#selected ? new Set(items.keys()) : update.recolored;
    for (const id of restyle) {
      const view = items.get(id);
      const object = this.#objects.get(id);
      if (view === undefined || object === undefined || added.has(id)) continue;
      this.#retired.push(...[object.material].flat());
      object.material = restyled(object, materials(view, selected, scene.materials, this.#textures));
      if (this.#clusters.isDraw(object)) this.#clusters.style(object);
      this.#draws.drawDirectly(object);
    }
    this.#selected = selected;
    // Lights: those the update names made again, the rest left as they are.
    const lights = new Map(scene.lights.map((light) => [light.key, light]));
    if (reset) for (const path of [...this.#lights.keys()]) this.#dropLight(path);
    for (const path of update.lightsRemoved) this.#dropLight(path);
    const relit = reset ? [...lights.keys()] : [...update.lightsAdded, ...update.lightsChanged];
    for (const path of relit) {
      this.#dropLight(path);
      const light = lights.get(path);
      if (light !== undefined) this.#addLight(light);
    }
    // Every lit shader is built for the scene's lights: every bundle recorded again.
    if (relit.length > 0 || update.lightsRemoved.size > 0) this.#draws.touchAll();
    // A clustered buffer freed or rebuilt no longer costs its set; new items may be in a manifest.
    if (update.freed.size + update.rebuilt.size + update.added.size > 0 || reset) queueMicrotask(() => this.#reportResident());
    // Linked again when what it reads may have changed: not as time plays.
    const linking = added.size > 0 || restyle.size > 0 || update.reskinned.size > 0 || relit.length > 0 || update.lightsRemoved.size > 0 || update.links > 0;
    const relinked = linking && this.#link(scene);
    if (reset || update.cameras.size > 0) this.#cameras = new Map(scene.cameras.map((camera) => [camera.path, camera]));
    // New draws, materials or light sets: their pipelines compiled off the main thread.
    if (added.size > 0 || restyle.size > 0 || update.reskinned.size > 0 || relinked) this.#compile();
    this.#controls.enabled = this.#looking() === null;
    // Framed when the bounds change, but not as time plays: an animated
    // extent would move the camera every frame.
    const framed = JSON.stringify(scene.bounds);
    if (scene.bounds !== null && framed !== this.#framed && (!update.timeChanged || this.#framed === "")) {
      this.#framed = framed;
      this.#frame(scene.bounds);
    }
    this.#draw();
  }

  #add(view: ViewItem, selected: string | null): void {
    let geometry = this.#geometries.get(view.item.buffer);
    // A geometry made for skinning (positions decoded) or not, as the draw is now.
    if (geometry !== undefined && geometry.userData.skinned !== undefined && geometry.userData.skinned !== (view.skin !== null)) {
      geometry.dispose();
      geometry = undefined;
    }
    if (geometry === undefined) {
      geometry = this.#makeGeometry(view);
      this.#geometries.set(view.item.buffer, geometry);
    }
    // Its set on its way: drawn once it arrives, the geometry never uploaded meanwhile.
    if (geometry.userData.awaitingSet === true && clusterDrawable(view)) {
      this.#awaitingSet.add(view.item.id);
      return;
    }
    const placements = view.item.matrices.length / 16;
    const styled = materials(view, selected, this.#applied?.materials ?? new Map(), this.#textures);
    // Clustered: one part per subset (non-empty range), as the set was built.
    const ranges = view.buffers.mesh.ranges.flatMap((range, index) => (range.count > 0 ? [index] : []));
    const clustered =
      this.#clusters.hasMesh(view.item.buffer) && clusterDrawable(view) && (Array.isArray(styled) ? ranges.length : 1) === this.#clusters.subsets(view.item.buffer);
    const object: Drawn = clustered
      ? this.#clusters.makeDraw(view.item.buffer, styled as LitMaterial | LitMaterial[], Array.isArray(styled) ? ranges : [0])
      : view.skin?.mode === "gpu"
        ? skinnedMesh(geometry, styled as LitMaterial | LitMaterial[], view)
        : view.kind === "lines" || view.kind === "standinLines"
        ? new LineSegments(geometry, styled as LineBasicNodeMaterial)
        : view.kind === "points"
          ? pointsMesh(geometry, styled as PointsNodeMaterial)
          : view.item.source.kind === "gprim" || view.item.source.kind === "drawMode"
            ? new Mesh(geometry, styled as LitMaterial | LitMaterial[])
            : new InstancedMesh(geometry, styled as LitMaterial | LitMaterial[], Math.max(1, placements));
    object.matrixAutoUpdate = false;
    this.#objects.set(view.item.id, object);
    // Skinned draws are posed as time plays: never bundled.
    if (view.skin !== null) this.#draws.keepDirect(object);
    else this.#draws.drawDirectly(object);
    this.#place(view, selected);
    if (view.skin !== null) this.#pose(view);
  }

  /**
   * Poses a skinned draw at the time: on the GPU its bones' matrices (the
   * joint palette) and bone inverses (the bind transform); on the CPU its
   * deformed positions and normals. Either way it is placed by the
   * skeleton's world transform.
   */
  #pose(view: ViewItem): void {
    const object = this.#objects.get(view.item.id);
    const skin = view.skin;
    if (object === undefined || skin === null) return;
    this.#draws.touch(object);
    object.matrix.fromArray(skin.world);
    if (object instanceof SkinnedDraw) {
      const { bones, boneInverses } = object.skeleton;
      bones.forEach((bone, joint) => bone.matrixWorld.fromArray(skin.palette, 16 * joint));
      for (const inverse of boneInverses) inverse.fromArray(skin.bind);
      // The joint palette the shader reads (and the bone texture, when it has one).
      object.skeleton.update();
      // A blend-shaped rest pose, uploaded when it was refilled.
      if (skin.shaped && skin.positions !== null && skin.normals !== null && object.userData.rest !== skin.positions) {
        object.userData.rest = skin.positions;
        object.geometry.setAttribute("position", new BufferAttribute(skin.positions, 3));
        object.geometry.setAttribute("normal", new BufferAttribute(skin.normals, 3));
      }
      return;
    }
    if (skin.positions === null || skin.normals === null) return;
    object.geometry.setAttribute("position", new BufferAttribute(skin.positions, 3));
    object.geometry.setAttribute("normal", new BufferAttribute(skin.normals, 3));
    object.geometry.computeBoundingSphere();
  }

  /** Writes the item's matrices into its object, in place; a group outgrowing its instance buffer is made again. */
  #place(view: ViewItem, selected: string | null): void {
    const object = this.#objects.get(view.item.id);
    if (object === undefined) return;
    this.#draws.touch(object);
    const matrices = view.item.matrices;
    const placements = matrices.length / 16;
    object.visible = placements > 0;
    // Row-by-row row-vector matrices are three.js's column-major column-vector
    // ones; each is followed by the geometry's position decode, if it has one.
    if (this.#clusters.isDraw(object)) {
      // The GPU culls and places each instance: its matrix without the decode, which the scene applies per mesh.
      this.#clusters.place(object, matrices);
      return;
    }
    const decode = (object.geometry.userData.decode as Matrix4 | undefined) ?? null;
    if (!(object instanceof InstancedMesh)) {
      // A skinned draw is placed by its skeleton ({@link #pose}).
      if (placements > 0 && view.skin === null) {
        object.matrix.fromArray(matrices);
        if (decode !== null) object.matrix.multiply(decode);
      }
      return;
    }
    if (placements > object.instanceMatrix.count) {
      this.#drop(view.item.id);
      this.#add(view, selected);
      return;
    }
    const matrix = PLACED;
    for (let i = 0; i < placements; i++) {
      matrix.fromArray(matrices, 16 * i);
      if (decode !== null) matrix.multiply(decode);
      object.setMatrixAt(i, matrix);
    }
    object.count = placements;
    object.instanceMatrix.needsUpdate = true;
    object.computeBoundingSphere();
  }

  /**
   * The geometry of the item's buffer ({@link makeGeometry}), watched for
   * its upload when it is a mesh's own vertices: after each render,
   * {@link #releaseUploaded} asks whether three.js has made its index's and
   * positions' GPU buffers (one draw makes them all).
   */
  #makeGeometry(view: ViewItem): BufferGeometry {
    const geometry = makeGeometry(view);
    this.#findClusters(view, geometry);
    const position = geometry.getAttribute("position");
    if (geometry.userData.decode === undefined || geometry.userData.skinned === true || geometry.index === null || position === undefined) return geometry;
    geometry.userData.key = view.item.buffer;
    geometry.userData.revision = view.buffers.revision;
    this.#pending.add(geometry);
    geometry.addEventListener("dispose", () => this.#pending.delete(geometry));
    return geometry;
  }

  /**
   * Looks for a prebuilt cluster set of the item's buffer (by the key of
   * its content, `clusterFile.ts`) while its arrays are still held, and
   * fetches it; once it arrives, the draws of the buffer are made again
   * as clustered draws. Until then, and when there is none, they draw as
   * any other.
   */
  #findClusters(view: ViewItem, geometry: BufferGeometry): void {
    const keys = this.#clusterKeys;
    // A stand-in's box is not the mesh: nothing to look up, nothing to judge the manifest by.
    if (view.buffers.standin !== null) return;
    const listed = this.#manifest.find(view.item.prim);
    const drawable = clusterDrawable(view) && clusterable(view.buffers.vertices);
    const key = drawable && keys?.size !== 0 ? clusterKey(view.buffers.vertices) : null;
    // The manifest's hint for the prim is right only if the built mesh has its set.
    if (listed !== null && listed.key !== key && !this.#manifestMisses.has(view.item.prim)) {
      this.#manifestMisses.add(view.item.prim);
      queueMicrotask(() => this.#reportResident());
    }
    if (key === null || (keys !== null && !keys.has(key))) return;
    const buffer = view.item.buffer;
    const vertices = packVertices(view.buffers.vertices);
    const { scale, offset } = view.buffers.vertices.decode;
    const decode = [scale, offset[0], offset[1], offset[2]] as const;
    // Its draws wait for the set (`#add`), unless it fails to arrive.
    geometry.userData.awaitingSet = true;
    // Before the server has listed its sets: kept until it has.
    if (keys === null) {
      this.#clusterWaiting.push({ buffer, geometry, key, vertices, decode });
      return;
    }
    this.#fetchClusters(buffer, geometry, key, vertices, decode);
  }

  /** Fetches the set `key` of buffer `buffer` (drawn by `geometry`, its vertices packed) and, once every fetch is done, draws it clustered. */
  #fetchClusters(buffer: number, geometry: BufferGeometry, key: string, vertices: PackedVertices, decode: readonly [number, number, number, number]): void {
    this.#clusterLoads.set(buffer, geometry);
    fetch(new URL(`/@clusters/${key}.lsclust`, window.location.href))
      .then(async (response) => {
        if (!response.ok) throw new Error(`cluster set ${key}: ${response.status}`);
        const set = readClusterFile(await response.arrayBuffer());
        // A draw instance runs the vertices of the largest cluster the scene draws: a larger one would be cut short.
        if (set.clusters.triangleCount.some((triangles) => triangles > MAX_CLUSTER_TRIANGLES)) throw new Error(`cluster set ${key}: clusters over ${MAX_CLUSTER_TRIANGLES} triangles`);
        // The buffer was rebuilt or freed meanwhile: the set is not its.
        if (this.#clusterLoads.get(buffer) !== geometry || this.#geometries.get(buffer) !== geometry) return;
        this.#clusterLoads.delete(buffer);
        delete geometry.userData.awaitingSet;
        this.#clusters.addMesh(buffer, set, vertices, decode);
        this.#clusterKeyOf.set(buffer, key);
        this.#clusterArrived.add(buffer);
        this.#convertArrived();
      })
      .catch((error: unknown) => {
        if (this.#clusterLoads.get(buffer) === geometry) {
          // Drawn as any other mesh instead.
          delete geometry.userData.awaitingSet;
          this.#clusterArrived.add(buffer);
        }
        this.#clusterLoads.delete(buffer);
        console.warn(error);
        this.#convertArrived();
      });
  }

  /**
   * Once every set asked for has arrived (or failed), makes the draws of
   * their buffers again as clustered draws, together: the scene is packed
   * once and the new pipelines compiled once, not per set.
   */
  #convertArrived(): void {
    if (this.#clusterLoads.size > 0 || this.#clusterArrived.size === 0 || this.#applied === null) return;
    const arrived = this.#clusterArrived;
    for (const view of this.#applied.items) {
      if (!arrived.has(view.item.buffer) || !(this.#objects.has(view.item.id) || this.#awaitingSet.has(view.item.id))) continue;
      this.#drop(view.item.id);
      this.#add(view, this.#selected);
    }
    this.#releaseClustered(arrived);
    arrived.clear();
    this.#link(this.#applied);
    this.#compile();
    this.#draw();
    this.#reportResident();
  }

  /**
   * Lets go of the own geometry of each of `buffers` that draws only
   * through its cluster set: never uploaded, its arrays are released as an
   * upload releases them ({@link onUploaded}); uploaded (drawn before its
   * set arrived), its GPU buffers are freed as well.
   */
  #releaseClustered(buffers: ReadonlySet<number>): void {
    const drawn = new Set<BufferGeometry>();
    for (const object of this.#objects.values()) if (!this.#clusters.isDraw(object)) drawn.add(object.geometry);
    let released = false;
    for (const key of buffers) {
      const geometry = this.#geometries.get(key);
      if (geometry === undefined || !this.#clusters.hasMesh(key) || drawn.has(geometry) || geometry.userData.clusterReleased === true) continue;
      geometry.userData.clusterReleased = true;
      const pending = this.#pending.delete(geometry);
      const position = geometry.getAttribute("position") as BufferAttribute | InterleavedBufferAttribute | undefined;
      if (geometry.index !== null && position !== undefined && (uploaded(this.#renderer, geometry.index) || uploaded(this.#renderer, position))) geometry.dispose();
      if (!pending) continue;
      const { revision } = geometry.userData as { revision: number };
      if (this.onUploaded?.(key, revision) !== true) continue;
      dropArrays(geometry);
      released = true;
    }
    if (released) this.onReleased?.();
  }

  /**
   * Tells the page what the clustered meshes cost the budget, by the prim
   * of each item drawing one ({@link onResident}): a new map whenever the
   * clustered buffers change.
   */
  #reportResident(): void {
    const costs = new Map<string, BufferCost>();
    for (const view of this.#applied?.items ?? []) {
      const prim = view.item.prim;
      let cost = this.#clusters.meshCost(view.item.buffer);
      // Not built yet (a box, say): its manifest's set, until the built mesh says otherwise.
      if (cost === null && !this.#manifestMisses.has(prim)) {
        const listed = this.#manifest.find(prim);
        if (listed !== null) cost = residentCost(listed, 0);
      }
      if (cost !== null) costs.set(prim, cost);
    }
    const signature = [...costs.keys()].sort().join("\n");
    if (signature === this.#residentSignature) return;
    this.#residentSignature = signature;
    this.onResident?.(costs);
  }

  /** The prims last reported clustered ({@link #reportResident}). */
  #residentSignature = "";

  /**
   * Called with the costs of the meshes drawn from prebuilt cluster sets,
   * by item prim, whenever they change: their whole sets, once each, for
   * the draw list's budget (`DrawList.setResidentCosts`).
   */
  onResident: ((costs: ReadonlyMap<string, BufferCost>) => void) | null = null;

  #addLight(light: LightItem): void {
    const objects = threeLights(light);
    for (const object of objects) {
      this.#scene.add(object);
      this.#lightKeys.set(object, light.key);
    }
    this.#lights.set(light.key, objects);
  }

  /**
   * Gives each draw's lit materials the lights that light it: the scene's
   * (`lightsNode` `null`) when `scene.unlit` leaves it out of none, else a
   * shared set of the rest ({@link #lightSet}). A material whose set is
   * unchanged is left as it is; one whose set changed is rebuilt against a
   * shader of that set, compiled only if no material of its shape met the
   * set before. Answers whether any material's set changed.
   */
  #link(scene: ViewportScene): boolean {
    const all: Object3D[] = [this.#cameraLight, this.#ambient];
    for (const objects of this.#lights.values()) for (const object of objects) if ((object as { isLight?: boolean }).isLight === true) all.push(object);
    const sets = new Map<string, LightsNode>();
    // Each distinct list of unlinked lights resolved once.
    const byUnlit = new Map<string, LightsNode | null>();
    let changed = false;
    for (const [id, object] of this.#objects) {
      const unlit = scene.unlit.get(id);
      let node: LightsNode | null = null;
      if (unlit !== undefined) {
        const signature = [...unlit].join("\n");
        let found = byUnlit.get(signature);
        if (found === undefined) {
          const lit = linkedLights(all, (light) => this.#lightKeys.get(light), unlit);
          found = lit === all ? null : this.#lightSet(lit, sets);
          byUnlit.set(signature, found);
        }
        node = found;
      }
      for (const material of [object.material].flat()) {
        if (!(material instanceof MeshStandardNodeMaterial) || material.lightsNode === node) continue;
        setFixedNode(material, "lightsNode", node);
        material.needsUpdate = true;
        changed = true;
        // A shader of another light set: drawn directly until it compiles.
        this.#draws.drawDirectly(object);
      }
    }
    // Sets no draw uses any more are let go.
    this.#lightSets = sets;
    return changed;
  }

  /** The one `LightsNode` of `lit`, kept in `sets` (the sets in use) and reused from the last link. */
  #lightSet(lit: readonly Object3D[], sets: Map<string, LightsNode>): LightsNode {
    const key = lightSetKey(lit.map((light) => light.id));
    let node = sets.get(key) ?? this.#lightSets.get(key);
    node ??= lights(lit as never[]);
    sets.set(key, node);
    return node;
  }

  #dropLight(path: string): void {
    for (const object of this.#lights.get(path) ?? []) {
      this.#lightKeys.delete(object);
      this.#scene.remove(object);
      if ("dispose" in object && typeof object.dispose === "function") object.dispose();
    }
    this.#lights.delete(path);
  }

  /** Makes again the materials of the draws binding a material with textures: an image arrived. */
  #restyleTextured(): void {
    const scene = this.#applied;
    if (scene === null) return;
    for (const view of scene.items) {
      const bound = [view.item.material, ...view.item.subsetMaterials];
      if (!bound.some((path) => path !== null && (scene.materials.get(path)?.textures.length ?? 0) > 0) && !standinTextured(view.buffers.standin)) continue;
      const object = this.#objects.get(view.item.id);
      if (object === undefined) continue;
      this.#retired.push(...[object.material].flat());
      object.material = restyled(object, materials(view, this.#selected, scene.materials, this.#textures));
      if (this.#clusters.isDraw(object)) this.#clusters.style(object);
      this.#draws.drawDirectly(object);
    }
    this.#link(scene);
    this.#draw();
    this.#compile();
  }

  /** The stage camera the view looks through, or `null` for the free camera. */
  #looking(): CameraItem | null {
    const camera = this.#through === null ? undefined : this.#cameras.get(this.#through);
    return camera?.camera == null ? null : camera;
  }

  #drop(id: number): void {
    this.#awaitingSet.delete(id);
    const object = this.#objects.get(id);
    if (object === undefined) return;
    this.#draws.remove(object);
    this.#clusters.removeDraw(object as Mesh);
    for (const material of [object.material].flat()) material.dispose();
    if (object instanceof InstancedMesh) object.dispose();
    // A point set's geometry is its own (a quad per point over its samples).
    if (isPoints(object)) object.geometry.dispose();
    this.#objects.delete(id);
  }

  /** Points the camera at the bounds' center from far enough to see them whole. */
  #frame(bounds: readonly number[]): void {
    const [x0, y0, z0, x1, y1, z1] = bounds as [number, number, number, number, number, number];
    const center = new Vector3((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2);
    const radius = Math.max(1e-3, Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 2);
    // Cells a power of ten: 10 to 100 of them across what is framed.
    const cell = 10 ** Math.floor(Math.log10(Math.max(radius / 5, 1e-6)));
    if (cell !== this.#cell) {
      this.#cell = cell;
      this.#regrid();
    }
    // Far enough that the bounds' sphere fits the view's height, with a margin.
    const distance = (1.1 * radius) / Math.tan((this.#camera.fov * Math.PI) / 360);
    const toward = this.#up === "Y" ? new Vector3(0.55, 0.45, 0.7) : new Vector3(0.55, -0.7, 0.45);
    this.#camera.position.copy(center).add(toward.normalize().multiplyScalar(distance));
    this.#camera.near = distance / 100;
    this.#camera.far = distance * 10;
    this.#camera.updateProjectionMatrix();
    this.#controls.target.copy(center);
    this.#controls.update();
  }

  /** Makes the grid again, across the up axis with cells of `#cell`. */
  #regrid(): void {
    this.#scene.remove(this.#grid);
    this.#grid.dispose();
    this.#grid = makeGrid(this.#up, this.#cell);
    this.#scene.add(this.#grid);
  }

  #makeControls(): OrbitControls {
    const controls = new OrbitControls(this.#camera, this.#canvas);
    controls.addEventListener("change", () => this.#draw());
    return controls;
  }

  #resize(): void {
    const { clientWidth: width, clientHeight: height } = this.#canvas;
    if (width === 0 || height === 0) return;
    this.#target.setPixelRatio(window.devicePixelRatio);
    this.#target.setSize(width, height, false);
    this.#camera.aspect = width / height;
    this.#camera.updateProjectionMatrix();
    this.#draw();
  }

  #draw(): void {
    this.#render();
    for (const material of this.#retired) material.dispose();
    this.#retired.length = 0;
  }

  #render(): void {
    if (this.#deviceLost) return;
    const camera = this.#prepareCamera();
    this.#cameraLight.position.setFromMatrixPosition(camera.matrixWorld);
    this.#setState();
    // The clustered draws' cut for this camera, on the GPU, before the draws read it.
    this.#clusters.prepare(this.#renderer, camera, this.#canvas.height);
    this.#renderer.render(this.#scene, camera);
    this.#releaseUploaded();
  }

  /**
   * Points the shared renderer at this canvas with the view's output: AgX
   * at the camera's exposure, cleared transparent (the page behind is the
   * viewport's background). Before each render and compile here, since the
   * material previews draw with the same renderer.
   */
  #setState(): void {
    const renderer = this.#renderer;
    renderer.setCanvasTarget(this.#target);
    renderer.toneMapping = AgXToneMapping;
    renderer.toneMappingExposure = this.#looking()?.camera?.linearExposureScale ?? 1;
    renderer.setClearColor(0x000000, 0);
  }

  /**
   * Compiles the pipelines the scene needs off the main thread, then draws:
   * the draws made meanwhile skip what is still compiling instead of
   * waiting for it (see the header).
   */
  #compile(): void {
    if (this.#deviceLost) return;
    const camera = this.#prepareCamera();
    this.#setState();
    const started = performance.now();
    // The direct draws this compile meets join bundles once it is done.
    const met = this.#draws.compiling();
    this.#renderer.compileAsync(this.#scene, camera).then(
      () => {
        this.lastCompileMs = performance.now() - started;
        this.#draws.settle(met, bundleable);
        this.#draw();
      },
      (error: unknown) => console.warn("pipeline compile failed", error),
    );
  }

  /** How long the last asynchronous compile took, in milliseconds (0 before the first). */
  lastCompileMs = 0;

  /** How many draws are in render bundles and how many are drawn directly (see the header). */
  get bundleCounts(): BundleCounts {
    return this.#draws.counts;
  }

  /**
   * `object` changed: its bundle recorded again, or, when time playing
   * changed it, the object drawn directly from now on.
   */
  #changed(object: Drawn | undefined, playing: boolean): void {
    if (object === undefined) return;
    if (!playing) {
      this.#draws.touch(object);
      return;
    }
    if (this.#draws.isBundled(object)) {
      // three.js's render objects remember the bundle they were recorded
      // in, and once its version is theirs they skip the per-frame check
      // as if still bundled: a draw leaving a bundle for good is given
      // copies of its materials, and with them render objects of its own.
      this.#retired.push(...[object.material].flat());
      object.material = Array.isArray(object.material) ? object.material.map((material) => material.clone()) : object.material.clone();
      for (const material of [object.material].flat()) fixNodes(material);
      if (this.#clusters.isDraw(object)) this.#clusters.style(object);
    }
    this.#draws.keepDirect(object);
  }

  /** The camera drawn through: the free one, or the stage camera with its matrices set for the canvas. */
  #prepareCamera(): Camera {
    const looking = this.#looking()?.camera ?? null;
    if (looking === null) {
      this.#camera.updateMatrixWorld();
      return this.#camera;
    }
    // Row-by-row row-vector matrices are three.js's column-major
    // column-vector ones: `fromArray` reads them as they are.
    const { clientWidth: width, clientHeight: height } = this.#canvas;
    const fitted = conformCamera(looking, "fit", height > 0 ? width / height : 1);
    const camera = this.#stageCamera;
    // OpenGL's clip depth, [-w, w], taken to WebGPU's [0, w].
    camera.projectionMatrix.fromArray(fitted.projectionMatrix).premultiply(GL_TO_WEBGPU_DEPTH);
    camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
    camera.matrixWorldInverse.fromArray(fitted.view);
    camera.matrixWorld.copy(camera.matrixWorldInverse).invert();
    return camera;
  }

  /**
   * Releases the buffers whose geometries the last render uploaded
   * ({@link onUploaded}), and drops three.js's arrays of those it
   * released: the GPU holds them now.
   */
  #releaseUploaded(): void {
    if (this.#pending.size === 0) return;
    let released = false;
    for (const geometry of this.#pending) {
      const position = geometry.getAttribute("position") as BufferAttribute | InterleavedBufferAttribute;
      if (!uploaded(this.#renderer, geometry.index!) || !uploaded(this.#renderer, position)) continue;
      this.#pending.delete(geometry);
      const { key, revision } = geometry.userData as { key: number; revision: number };
      if (this.#geometries.get(key) !== geometry || this.onUploaded?.(key, revision) !== true) continue;
      dropArrays(geometry);
      released = true;
    }
    if (released) this.onReleased?.();
  }
}

/** A ground grid of 40 cells of `cell` stage units, through the origin, across the `up` axis. */
function makeGrid(up: "Y" | "Z", cell: number): GridHelper {
  const grid = new GridHelper(40 * cell, 40, 0x55554f, 0x3a3a35);
  if (up === "Z") grid.rotation.x = Math.PI / 2;
  grid.position.set(0, up === "Y" ? -0.001 * cell : 0, up === "Z" ? -0.001 * cell : 0);
  return grid;
}

/**
 * The item's materials, highlighted when selected: one per range when it
 * has subsets (a range's subset material, else the item's), each its bound
 * `UsdPreviewSurface` record's (`surfaces.ts`), else its color. With one
 * of its face subsets selected, that subset's range is highlighted and
 * the others ghosted.
 */
function materials(
  view: ViewItem,
  selected: string | null,
  records: ReadonlyMap<string, MaterialItem>,
  textures: TextureStore,
): Styled {
  const { item } = view;
  const paths = [item.prim, ...item.instances];
  const subset = selected === null ? -1 : view.buffers.subsets.indexOf(selected);
  if (subset >= 0) return subsetMaterials(view, subset, records, textures);
  const highlighted =
    selected !== null && paths.some((path) => selected === path || selected.startsWith(`${path}/`) || path.startsWith(`${selected}/`));
  // Lines and points: unlit, in their per-sample (or prim) display color.
  const tint = (vertexColors: boolean) =>
    highlighted ? new Color(0x6f9fff) : vertexColors ? new Color(1, 1, 1) : new Color().setRGB(...(view.color as [number, number, number]));
  if (view.buffers.standin !== null) return standinMaterials(view.buffers.standin, highlighted, textures);
  if (view.kind === "lines") {
    const vertexColors = view.buffers.curves?.colors != null;
    return new LineBasicNodeMaterial({ color: tint(vertexColors), vertexColors });
  }
  if (view.kind === "points") return pointsMaterial(tint(view.buffers.points!.colors !== null), view.buffers.points!.colors !== null);
  const colors = view.buffers.vertices.color;
  // One map for every range's material: a subset's reads its sets' channels.
  const channels = uvChannels(view.buffers.vertices);
  const doubleSided = view.kind === "mesh" || view.kind === "Plane";
  const record = (path: string | null) => {
    const found = path === null ? undefined : records.get(path);
    return found?.surface == null ? null : found;
  };
  const surface = (found: MaterialItem) => {
    const material = surfaceMaterial(found, textures, { vertexColors: colors !== null, doubleSided, highlighted, uvChannels: channels });
    // A constant `displayColor` the buffer does not carry (a shape's) is the prim's color.
    if (found.diffuseColor.kind === "primvar" && !material.vertexColors && found.diffuseColor.name === "displayColor" && view.colorSource === "displayColor") {
      material.color.setRGB(...(view.color as [number, number, number]));
    }
    return material;
  };
  // Per-vertex displayColor multiplies a white base; a subset's material
  // color replaces it for its range.
  const material = (color: readonly [number, number, number] | null) => {
    const vertexColors = color === null && colors !== null;
    const [r, g, b] = vertexColors ? [1, 1, 1] : (color ?? view.color);
    return new MeshStandardNodeMaterial({
      color: new Color().setRGB(r, g, b),
      vertexColors,
      roughness: 0.7,
      ...(view.kind === "mesh" || view.kind === "Plane" ? { side: DoubleSide } : {}),
      emissive: highlighted ? new Color(0x2f5fd0) : new Color(0),
      emissiveIntensity: highlighted ? 0.6 : 0,
    });
  };
  const ranges = view.buffers.mesh.ranges;
  if (ranges.length > 1) {
    return ranges.map((range, index) => {
      const found = record(range.subset === null ? item.material : (item.subsetMaterials[range.subset] ?? item.material));
      return found === null ? material(view.rangeColors[index] ?? null) : surface(found);
    });
  }
  const found = record(item.material);
  return found === null ? material(null) : surface(found);
}

/** A mesh's materials, one per range, with the range of subset `subset` highlighted and the others ghosted: drawn faintly, see-through. */
function subsetMaterials(
  view: ViewItem,
  subset: number,
  records: ReadonlyMap<string, MaterialItem>,
  textures: TextureStore,
): LitMaterial | LitMaterial[] {
  const base = materials(view, null, records, textures);
  // One range (the subset is every face): no groups, one material.
  if (view.buffers.mesh.ranges.length <= 1) {
    const material = (Array.isArray(base) ? base[0] : base) as LitMaterial;
    material.emissive = new Color(0x2f5fd0);
    material.emissiveIntensity = 0.6;
    return material;
  }
  const each = view.buffers.mesh.ranges.map((_, index) => {
    const material = (Array.isArray(base) ? base[index] : index === 0 ? base : (base as LitMaterial).clone()) as LitMaterial;
    return material;
  });
  view.buffers.mesh.ranges.forEach((range, index) => {
    const material = each[index]!;
    if (range.subset === subset) {
      material.emissive = new Color(0x2f5fd0);
      material.emissiveIntensity = 0.6;
    } else {
      material.transparent = true;
      material.opacity = 0.15;
      material.depthWrite = false;
    }
  });
  return each;
}

/**
 * Sets a mesh view's vertex attributes on `geometry` from its buffer's
 * {@link MeshVertices}, as stagehand encoded them: `position`, `normal`,
 * `color`, `uv` (`st`), and the texture coordinate sets besides `st` as
 * three.js's `uv1` to `uv3` by the mesh's {@link uvChannels}, which
 * `surfaces.ts` points each texture at. A skinned draw's positions are
 * decoded to `f32` (`userData.skinned`); any other's decode is kept as
 * `userData.decode`, for {@link Viewport} to fold into the object's
 * matrices (identity for `f32` vertices).
 */
function setVertices(geometry: BufferGeometry, view: ViewItem): void {
  const vertices = view.buffers.vertices;
  const skinned = view.skin !== null;
  geometry.setAttribute("position", skinned ? new BufferAttribute(vertexPositions(vertices), 3) : threeAttribute(vertices.position));
  geometry.setAttribute("normal", threeAttribute(vertices.normal));
  if (vertices.color !== null) geometry.setAttribute("color", threeAttribute(vertices.color));
  if (vertices.st !== null) geometry.setAttribute("uv", threeAttribute(vertices.st));
  const channels = uvChannels(vertices);
  for (const set of vertices.uvSets) {
    const channel = channels.get(set.name);
    if (channel !== undefined) geometry.setAttribute(`uv${channel}`, threeAttribute(set.attribute));
  }
  if (geometry.index?.array !== vertices.indices) geometry.setIndex(new BufferAttribute(vertices.indices, 1));
  geometry.userData.skinned = skinned;
  geometry.userData.decode = skinned ? null : decodeMatrix(vertices);
}

/** The matrix taking a buffer's position values to its points, `translate(offset) * scale(scale)`; `null` for an identity decode. */
function decodeMatrix(vertices: MeshVertices): Matrix4 | null {
  const { scale, offset } = vertices.decode;
  if (scale === 1 && offset[0] === 0 && offset[1] === 0 && offset[2] === 0) return null;
  return new Matrix4().makeScale(scale, scale, scale).setPosition(offset[0], offset[1], offset[2]);
}

/**
 * A three.js attribute over a stagehand {@link VertexAttribute}'s array,
 * not copied: interleaved where a vertex is padded (`snorm16` ×4 for three
 * components), integers normalized (`snorm16x4`, `snorm8x4`), half floats
 * a `Float16Array` view of the same bits (`float16x2`: three.js widens a
 * `Uint16Array` of half floats to 32-bit integers, as it would indices).
 */
function threeAttribute(attribute: VertexAttribute): BufferAttribute | InterleavedBufferAttribute {
  const { array: stored } = attribute;
  const array = attribute.type === "float16" ? new Float16(stored.buffer, stored.byteOffset, stored.length) : stored;
  if (attribute.stride !== attribute.size) {
    return new InterleavedBufferAttribute(new InterleavedBuffer(array as Float32Array, attribute.stride), attribute.size, 0, attribute.normalized);
  }
  return new BufferAttribute(array as Float32Array, attribute.size, attribute.normalized);
}

/** `Float16Array`, which every browser with WebGPU has (TypeScript's library does not name it yet). */
const Float16 = (globalThis as unknown as { Float16Array: new (buffer: ArrayBufferLike, byteOffset: number, length: number) => ArrayLike<number> }).Float16Array;

/** Whether three.js has made `attribute`'s GPU buffer (an interleaved one's buffer's). */
function uploaded(renderer: WebGPURenderer, attribute: BufferAttribute | InterleavedBufferAttribute): boolean {
  const held = attribute instanceof InterleavedBufferAttribute ? attribute.data : attribute;
  return (renderer.backend as unknown as { get(object: object): { buffer?: unknown } }).get(held).buffer !== undefined;
}

/**
 * Drops the arrays of a geometry three.js has uploaded: each attribute's
 * (an interleaved one's buffer's) and the index's become empty arrays of
 * their type. Counts stay, so it draws as before; only a lost device,
 * which needs the arrays again, makes it again ({@link Viewport.onContextRestored}).
 */
function dropArrays(geometry: BufferGeometry): void {
  const empty = (holder: { array: ArrayLike<number> }) => {
    holder.array = new (holder.array.constructor as new (length: number) => ArrayLike<number>)(0);
  };
  for (const attribute of Object.values(geometry.attributes)) empty(attribute instanceof InterleavedBufferAttribute ? attribute.data : (attribute as BufferAttribute));
  if (geometry.index !== null) empty(geometry.index);
}

/** The three.js objects a draw item becomes: a point set is a `Mesh` of a quad per point ({@link pointsMesh}). */
type Drawn = Mesh | InstancedMesh | LineSegments;

/**
 * The most draws one render bundle holds: a change to one draw records
 * this many again, which costs about what drawing them directly each frame
 * did (a millisecond or so), and a frame replays a bundle per this many.
 */
const BUNDLE_CAPACITY = 256;

/**
 * Whether `object` may be drawn from a render bundle: every material
 * opaque. A transparent one is sorted by distance each frame, and a
 * transmissive one reads the frame drawn so far, neither of which a
 * recording can do.
 */
function bundleable(object: Drawn): boolean {
  for (const material of [object.material].flat()) {
    if (material.transparent || ((material as { transmission?: number }).transmission ?? 0) > 0) return false;
  }
  return true;
}

/** Whether `object` draws a point set. */
function isPoints(object: Object3D): boolean {
  return object.userData.points === true;
}

/** Whether `object` draws a view of `kind`: lines, points, or triangles. */
function fits(object: Drawn, kind: string): boolean {
  if (kind === "lines" || kind === "standinLines") return object instanceof LineSegments;
  if (kind === "points") return isPoints(object);
  return object instanceof Mesh && !isPoints(object);
}

/** A curves' samples (with their line segments) or a point set's points, as positions and colors. */
function samplesOf(view: ViewItem): { positions: Float32Array; colors: Float32Array | null; lines: Uint32Array | null } {
  const curves = view.buffers.curves;
  if (curves !== null) return { positions: curves.positions, colors: curves.colors, lines: curves.lines };
  const points = view.buffers.points!;
  return { positions: points.positions, colors: points.colors, lines: null };
}

/** New sample positions and colors in place: a curves' or point set's refill. */
function refillSamples(geometry: BufferGeometry, view: ViewItem): void {
  if (geometry.userData.points === true) {
    setPoints(geometry as InstancedBufferGeometry, view);
    return;
  }
  const { positions, colors } = samplesOf(view);
  geometry.setAttribute("position", new BufferAttribute(positions, 3));
  if (colors !== null) geometry.setAttribute("color", new BufferAttribute(colors, 3));
  geometry.computeBoundingSphere();
}

/** New materials for `object`: a skinned draw's skinning it ({@link withSkinning}). */
function restyled(object: Drawn, styled: Styled): Styled {
  return object instanceof SkinnedDraw ? skinnedMaterials(styled as LitMaterial | LitMaterial[]) : styled;
}

/** The materials of a {@link SkinnedDraw}, each skinning it. */
function skinnedMaterials<T extends LitMaterial | LitMaterial[]>(styled: T): T {
  for (const material of [styled].flat()) withSkinning(material);
  return styled;
}

/** A quad's corners, about its center, one unit across: each point's square, scaled by its width. */
const QUAD = [-0.5, -0.5, 0, 0.5, -0.5, 0, 0.5, 0.5, 0, -0.5, 0.5, 0];

/**
 * A point set's geometry: one quad, drawn once per point (instanced), with
 * each point's position, width (its diameter) and color as per-instance
 * attributes ({@link pointsMaterial} reads them).
 */
function pointsGeometry(view: ViewItem): InstancedBufferGeometry {
  const geometry = new InstancedBufferGeometry();
  geometry.setIndex([0, 1, 2, 0, 2, 3]);
  geometry.setAttribute("position", new BufferAttribute(new Float32Array(QUAD), 3));
  geometry.userData.points = true;
  setPoints(geometry, view);
  return geometry;
}

/** A point set's samples as its geometry's per-instance attributes, new or refilled. */
function setPoints(geometry: InstancedBufferGeometry, view: ViewItem): void {
  const points = view.buffers.points!;
  const count = points.positions.length / 3;
  const widths = points.widths.length === count ? points.widths : new Float32Array(count).fill(points.widths[0] ?? 1);
  geometry.setAttribute("pointPosition", new InstancedBufferAttribute(points.positions, 3));
  geometry.setAttribute("pointWidth", new InstancedBufferAttribute(widths, 1));
  if (points.colors !== null) geometry.setAttribute("pointColor", new InstancedBufferAttribute(points.colors, 3));
  geometry.instanceCount = count;
}

/** The canvas's size in device pixels, as {@link HALF_HEIGHT} reads it. */
const DRAWING_SIZE = new Vector2();

/** Half the canvas's height in device pixels: a point's width at depth 1 is this many pixels per unit. */
const HALF_HEIGHT = (uniform(1) as Node).setGroup(renderGroup).onRenderUpdate(({ renderer }: { renderer: WebGPURenderer }) => renderer.getDrawingBufferSize(DRAWING_SIZE).y / 2);

/**
 * The nodes every point set's material places its quads by: each point's
 * position and color, and its size on screen, in CSS pixels as
 * `PointsNodeMaterial.sizeNode` takes it: its width attenuated with
 * distance as `PointsMaterial` attenuated a point's size in WebGL
 * (`width · height / 2 / depth` device pixels), and at least a device
 * pixel, as WebGL's points were, so a far point still shows and picks.
 */
const POINT = {
  position: attribute("pointPosition", "vec3"),
  size: max(attribute("pointWidth", "float").mul(HALF_HEIGHT).div(positionView.z.negate()), 1).div(screenDPR),
  color: attribute("pointColor", "vec3"),
};

/**
 * A point set's material: camera-facing squares of each point's width in
 * world units ({@link POINT}), in `color` (times each point's color when
 * `vertexColors`). Unlit.
 */
function pointsMaterial(color: Color, vertexColors: boolean): PointsNodeMaterial {
  const material = new PointsNodeMaterial({ color, sizeAttenuation: false, transparent: false });
  material.positionNode = POINT.position;
  material.sizeNode = POINT.size;
  if (vertexColors) material.colorNode = vec4(materialColor.mul(POINT.color), 1);
  return material;
}

/** A point set's object: a `Mesh` of its quads (the point material draws it as sprites), never culled by its quad's bounds. */
function pointsMesh(geometry: BufferGeometry, material: PointsNodeMaterial): Mesh {
  const mesh = new Mesh(geometry, material);
  mesh.userData.points = true;
  mesh.frustumCulled = false;
  return mesh;
}

/**
 * A {@link SkinnedDraw} drawing a GPU-skinned item: its geometry gets the
 * gathered influences as `skinIndex`, `skinWeight`, `skinIndex1` and
 * `skinWeight1`, and the mesh a skeleton of one bone per joint, outside
 * the scene graph, posed by {@link Viewport} each time. Never
 * frustum-culled: three.js would cull it by the bounds of one pose.
 */
function skinnedMesh(geometry: BufferGeometry, material: LitMaterial | LitMaterial[], view: ViewItem): SkinnedDraw {
  const skin = view.skin!;
  const influences = skin.influences!;
  // Eight per vertex, as two attributes of four (`skinning.ts`).
  const joints = new InterleavedBuffer(influences.joints, influences.slots);
  const weights = new InterleavedBuffer(influences.weights, influences.slots);
  geometry.setAttribute("skinIndex", new InterleavedBufferAttribute(joints, 4, 0));
  geometry.setAttribute("skinWeight", new InterleavedBufferAttribute(weights, 4, 0));
  geometry.setAttribute("skinIndex1", new InterleavedBufferAttribute(joints, 4, 4));
  geometry.setAttribute("skinWeight1", new InterleavedBufferAttribute(weights, 4, 4));
  const bones = Array.from({ length: skin.joints }, () => {
    const bone = new Bone();
    bone.matrixAutoUpdate = false;
    bone.matrixWorldAutoUpdate = false;
    return bone;
  });
  const inverse = new Matrix4().fromArray(skin.bind);
  return new SkinnedDraw(geometry, skinnedMaterials(material), new Skeleton(bones, bones.map(() => inverse.clone())));
}

/** A geometry over the item's stagehand buffer: indexed, with normals, colors and a group per range ({@link setVertices}). */
function makeGeometry(view: ViewItem): BufferGeometry {
  if (view.buffers.standin !== null) return standinGeometry(view.buffers.standin);
  if (view.kind === "points") return pointsGeometry(view);
  if (view.kind === "lines") {
    // Curves drawn as lines: two sample indices per segment; points: one vertex each.
    const { positions, colors, lines } = samplesOf(view);
    const geometry = new BufferGeometry();
    if (lines !== null) geometry.setIndex(new BufferAttribute(lines, 1));
    geometry.setAttribute("position", new BufferAttribute(positions, 3));
    if (colors !== null) geometry.setAttribute("color", new BufferAttribute(colors, 3));
    geometry.computeBoundingSphere();
    return geometry;
  }
  const mesh = view.buffers.mesh;
  const geometry = new BufferGeometry();
  setVertices(geometry, view);
  // A group per range when there are subsets, each drawn with its range's material.
  if (mesh.ranges.length > 1) mesh.ranges.forEach((range, index) => geometry.addGroup(range.first, range.count, index));
  geometry.computeBoundingSphere();
  return geometry;
}

/** Whether a view may draw through a cluster set: a static, unskinned mesh, not a stand-in. */
function clusterDrawable(view: ViewItem): boolean {
  return view.kind === "mesh" && view.skin === null && view.buffers.standin === null;
}

/** The pick target's side, in device pixels: odd, about the clicked point ({@link pickOrder}). */
const PICK_SIZE = 7;

/** Sixteen numbers, as `Matrix4.set` takes them. */
type Matrix16 = [number, number, number, number, number, number, number, number, number, number, number, number, number, number, number, number];

/** The matrix `#place` writes each instance through, reused: placing allocates nothing per instance or update. */
const PLACED = new Matrix4();

/** OpenGL's clip depth taken to WebGPU's: `z' = (z + w) / 2`. */
const GL_TO_WEBGPU_DEPTH = new Matrix4().set(1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0.5, 0.5, 0, 0, 0, 1);

/** A pick id as the four bytes of an RGBA8 texel, low byte in red ({@link unpackPickId}). */
function pickBytes(id: Node): Node {
  return vec4(id.bitAnd(255), id.shiftRight(8).bitAnd(255), id.shiftRight(16).bitAnd(255), id.shiftRight(24)).div(255);
}

/**
 * What a pick reuses, made at the first: the target, the materials (one
 * pipeline per object shape, their id uniform written per object), the
 * ids and the matrices it swaps. A pick allocates nothing per object but
 * the swap of its material.
 */
class Picker {
  readonly target = new RenderTarget(PICK_SIZE, PICK_SIZE);
  readonly order = pickOrder(PICK_SIZE);
  readonly ids = new PickIds();
  readonly projection = new Matrix4();
  readonly window = new Matrix4();
  /** Each object's own material while the pick material draws it. */
  readonly swapped = new Map<Drawn, Drawn["material"]>();
  /** The object's first pick id (`userData.pickBase`), written as each object is drawn. */
  readonly #base: Node = (uniform(0, "uint") as Node).onObjectUpdate(({ object }: { object: Object3D }) => (object.userData.pickBase as number | undefined) ?? 0);
  /**
   * Surfaces and lines: the object's first id plus the instance's index,
   * placed as three.js places them (instancing, and the editor's skinning),
   * passed to the fragment unblended (`flat`).
   */
  readonly surfaces = withSkinning(new NodeMaterial());
  /** Points: the object's id alone (its quads are instances), placed as the point material places them. */
  readonly points = new PointsNodeMaterial({ sizeAttenuation: false, transparent: false });
  /** The fragment of surfaces: the object's first id plus the instance's index. */
  readonly #id: Node;
  /** Clustered draws: the surfaces' fragment, in the clustered draws' context (made again when it changes). */
  #clustered: NodeMaterial | null = null;

  constructor() {
    for (const material of [this.surfaces, this.points]) {
      material.side = DoubleSide;
      material.blending = NoBlending;
      material.lights = false;
    }
    const id = varying(pickBytes(this.#base.add(instanceIndex))) as Node;
    id.setInterpolation("flat");
    this.#id = id;
    this.surfaces.fragmentNode = id;
    this.points.positionNode = POINT.position;
    this.points.sizeNode = POINT.size;
    this.points.fragmentNode = pickBytes(this.#base);
  }

  /** The pick material of clustered draws in `context` (the cluster scene's), made when the context changes. */
  clustered(context: Node): NodeMaterial {
    if (this.#clustered !== null && (this.#clustered as unknown as { contextNode: Node }).contextNode === context) return this.#clustered;
    this.#clustered?.dispose();
    const material = new NodeMaterial();
    material.side = DoubleSide;
    material.blending = NoBlending;
    material.lights = false;
    material.fragmentNode = this.#id;
    (material as unknown as { contextNode: Node }).contextNode = context;
    this.#clustered = material;
    return material;
  }

  dispose(): void {
    this.target.dispose();
    this.surfaces.dispose();
    this.points.dispose();
    this.#clustered?.dispose();
  }
}

/**
 * The three.js lights of a `UsdLux` light, in the units the header of this
 * file states: the light, and a directional or spot light's target.
 */
function threeLights(light: LightItem): Object3D[] {
  if (light.hidden || light.kind === "portal") return [];
  const radiance = light.emission.radiance;
  const peak = Math.max(...radiance);
  if (!(peak > 0)) return [];
  const color = new Color().setRGB(radiance[0] / peak, radiance[1] / peak, radiance[2] / peak);
  const position = new Vector3(...light.position);
  const direction = new Vector3(...light.direction);
  // The light's area in world space: `sizeFactor` as `normalize` would make it.
  const area = lightSizeFactor(light.geometry, light.matrix);
  const aimed = (object: DirectionalLight | SpotLight): Object3D[] => {
    object.target.position.copy(position).add(direction);
    return [object, object.target];
  };
  switch (light.geometry.kind) {
    case "sphere":
    case "cylinder": {
      const projected = light.geometry.kind === "sphere" ? area / 4 : area / Math.PI;
      const point = new PointLight(color, peak * projected, 0, 2);
      point.position.copy(position);
      return [point];
    }
    case "disk": {
      const shaping = light.shaping;
      const angle = Math.min(Math.PI / 2, ((shaping?.coneAngle ?? 90) * Math.PI) / 180);
      const spot = new SpotLight(color, peak * area, 0, angle, Math.min(1, Math.max(0, shaping?.coneSoftness ?? 0)), 2);
      spot.position.copy(position);
      return aimed(spot);
    }
    case "rect": {
      const translation = new Vector3();
      const rotation = new Quaternion();
      const scale = new Vector3();
      new Matrix4().fromArray(light.matrix).decompose(translation, rotation, scale);
      const rect = new RectAreaLight(color, peak, light.geometry.width * Math.abs(scale.x), light.geometry.height * Math.abs(scale.y));
      rect.position.copy(translation);
      rect.quaternion.copy(rotation);
      return [rect];
    }
    case "distant": {
      const sun = new DirectionalLight(color, peak * area);
      sun.position.copy(position).sub(direction);
      return aimed(sun);
    }
    case "dome":
      return [new HemisphereLight(color, color, Math.PI * peak)];
    // A portal only guides a dome's light; three.js has no light of a
    // mesh's shape, so a mesh or geometry light is not drawn as one.
    case "portal":
    case "mesh":
    case "geometry":
      return [];
  }
}
