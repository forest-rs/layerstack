// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws what `../model/draws.ts` keeps of the stage.
// Every matrix and color comes from layerstack and every triangle from
// stagehand's buffers (indexed, with normals, per-vertex colors and a
// group per subset range); three.js only rasterizes them. No USD loader is
// involved. Each update applies the draw list's diffs in place: an
// `InstancedMesh` per instanced item whose matrices are rewritten, a
// buffer geometry per stagehand buffer whose attributes are refilled, a
// three.js light per `UsdLux` light the diff adds or changes, and nothing
// rebuilt that the diffs do not name.
//
// The camera is the stage's (`UsdGeomCamera`, its view and projection
// matrices as `GfFrustum` computes them), fitted to the canvas by
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
//   loaded).
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
// AgX tone mapping fits the highlights into the display's range. A stage
// without lights is lit by a fixed sky and sun, as before.
//
// Light linking: three.js lights every lit material with every light in
// the scene; `Light.layers` only decides whether a camera sees the light
// at all (r186's `WebGLRenderer` pushes a light when its layers test the
// camera's), not which objects it lights, and its node materials'
// `lightsNode` are `WebGPURenderer`'s. So each draw's `MeshStandardMaterial`
// (or `MeshPhysicalMaterial`) is patched, once, in `onBeforeCompile`: the
// `lights_fragment_begin` chunk multiplies each point, spot, directional
// and rect area light's color, and each hemisphere light's irradiance, by
// a per-material uniform of 0 or 1 for the light in that slot. The slots
// are three.js's own: `WebGLLights` numbers the lights of each type in the
// order the renderer meets them (a depth-first walk of the visible scene),
// shadow-casting and textured ones first, which none here are. Every
// patched material shares one program; linking a light again rewrites
// uniforms only. The masks come from the draw list
// (`ViewportScene.unlit`: stagehand's evaluated `lightLink`s). Limits: one
// material per `InstancedMesh`, so a prototype's draw is lit as its first
// instance is; eight lights of a type are linked (more are lit as three.js
// lights them); lines and points are unlit anyway; and `shadowLink` is
// shown but not drawn, as the viewport casts no shadows.
//
// Skinning (`../model/skinning.ts`): a draw a skeleton deforms on the GPU
// is a `SkinnedMesh` over its stagehand buffer, whose `skinIndex` and
// `skinWeight` (eight per vertex, see `SKINNING`) are the joint
// influences gathered into vertex order, with a
// `Skeleton` of its own: one bone per joint of the binding, each bone's
// world matrix the joint's skinning transform at the time and each bone
// inverse the geometry's bind transform, bound with an identity bind
// matrix (detached). three.js then moves a rest point `p` by
// `Σ w · skinningTransform · bindTransform · p`, the points UsdSkel skins
// into the skeleton's space, and the object is placed by the skeleton's
// world transform. A time change rewrites the bones' matrices and nothing
// else. Normals are skinned through each joint matrix's inverse transpose,
// blended and normalized, as `UsdSkelSkinNormals` skins them: three.js's
// own `skinnormal_vertex` blends the matrices themselves, which bends
// normals wrong under joints that scale. So the skinned draws' materials
// are patched in `onBeforeCompile`, beside their light-link patch, and
// keyed apart (`patches`); three.js's chunks stay as they are. A mesh with
// blend shapes skins its blend-shaped rest pose, uploaded once (again only
// when the animation's weights vary). A draw deformed on the CPU (dual
// quaternions) is a plain mesh whose positions and normals are refilled
// each time.
import {
  AgXToneMapping,
  Bone,
  BufferAttribute,
  BufferGeometry,
  Camera,
  Color,
  DetachedBindMode,
  DirectionalLight,
  DoubleSide,
  GridHelper,
  Group,
  HemisphereLight,
  InstancedMesh,
  InterleavedBuffer,
  InterleavedBufferAttribute,
  LineBasicMaterial,
  LineSegments,
  Matrix4,
  Mesh,
  MeshStandardMaterial,
  Object3D,
  PerspectiveCamera,
  PointLight,
  Points,
  PointsMaterial,
  Quaternion,
  Raycaster,
  RectAreaLight,
  Scene,
  ShaderChunk,
  Skeleton,
  SkinnedMesh,
  SpotLight,
  Vector2,
  Vector3,
  WebGLRenderer,
} from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { RectAreaLightUniformsLib } from "three/addons/lights/RectAreaLightUniformsLib.js";
import { conformCamera, lightSizeFactor } from "@forest-rs/layerstack_stagehand";
import type { CameraItem, LightItem, MaterialItem, StagehandMesh } from "@forest-rs/layerstack_stagehand";
import { TextureStore, surfaceMaterial, uvChannels } from "./surfaces.ts";
import { standinGeometry, standinMaterials, standinTextured } from "./standins.ts";
import type { AssetExists, AssetReader } from "./surfaces.ts";
import type { ViewItem, ViewportScene, ViewportUpdate } from "../model/draws.ts";

/**
 * The editor's skinning shader, for the editor's own skinned materials
 * ({@link skinnedMaterials}; the chunks of three.js stay as they are, so
 * any other `SkinnedMesh` on the page skins as three.js does): eight
 * influences per vertex (`skinIndex` and `skinWeight`, then `skinIndex1`
 * and `skinWeight1`, which every skinned geometry here has, zero-weighted
 * where unused) where three.js reads four, since production rigs weight
 * more joints than four and dropping the rest moves points visibly; and
 * normals as `UsdSkelSkinNormals` computes them (OpenUSD
 * `pxr/usd/usdSkel/utils.cpp`, `_SkinNormalsLBS`): each joint matrix's
 * linear part inverse transposed (by its cofactors), weighted and summed,
 * then normalized by the fragment shader; a singular joint contributes
 * nothing. three.js's own `skinnormal_vertex` blends the matrices
 * themselves, which bends normals wrong under scaling joints. Each entry
 * replaces one `#include` of the vertex shader.
 */
const SKINNING: readonly (readonly [string, string])[] = (() => {
  const slots: [string, string][] = [
    ["boneMatX", "skinWeight.x"],
    ["boneMatY", "skinWeight.y"],
    ["boneMatZ", "skinWeight.z"],
    ["boneMatW", "skinWeight.w"],
    ["boneMat1X", "skinWeight1.x"],
    ["boneMat1Y", "skinWeight1.y"],
    ["boneMat1Z", "skinWeight1.z"],
    ["boneMat1W", "skinWeight1.w"],
  ];
  const sum = (term: (matrix: string, weight: string) => string) => slots.map(([matrix, weight]) => `\t${term(matrix, weight)}`).join("\n");
  return [
    [
      "#include <skinning_pars_vertex>",
      `#include <skinning_pars_vertex>
#ifdef USE_SKINNING
	attribute vec4 skinIndex1;
	attribute vec4 skinWeight1;
	mat3 skinNormalMatrix( mat3 m ) {
		vec3 bc = cross( m[ 1 ], m[ 2 ] );
		float det = dot( m[ 0 ], bc );
		return det == 0.0 ? mat3( 0.0 ) : mat3( bc, cross( m[ 2 ], m[ 0 ] ), cross( m[ 0 ], m[ 1 ] ) ) / det;
	}
#endif`,
    ],
    [
      "#include <skinbase_vertex>",
      `#include <skinbase_vertex>
#ifdef USE_SKINNING
	mat4 boneMat1X = getBoneMatrix( skinIndex1.x );
	mat4 boneMat1Y = getBoneMatrix( skinIndex1.y );
	mat4 boneMat1Z = getBoneMatrix( skinIndex1.z );
	mat4 boneMat1W = getBoneMatrix( skinIndex1.w );
#endif`,
    ],
    [
      "#include <skinning_vertex>",
      `#ifdef USE_SKINNING
	vec4 skinVertex = bindMatrix * vec4( transformed, 1.0 );
	vec4 skinned = vec4( 0.0 );
${sum((matrix, weight) => `skinned += ${matrix} * skinVertex * ${weight};`)}
	transformed = ( bindMatrixInverse * skinned ).xyz;
#endif`,
    ],
    [
      "#include <skinnormal_vertex>",
      `#ifdef USE_SKINNING
	mat3 skinBind = mat3( bindMatrix );
	mat3 skinUnbind = mat3( bindMatrixInverse );
	vec3 skinnedNormal = vec3( 0.0 );
${sum((matrix, weight) => `skinnedNormal += ${weight} * ( skinNormalMatrix( skinUnbind * mat3( ${matrix} ) * skinBind ) * objectNormal );`)}
	#ifdef USE_TANGENT
		mat4 skinMatrix = mat4( 0.0 );
${sum((matrix, weight) => `\tskinMatrix += ${weight} * ${matrix};`)}
		skinMatrix = bindMatrixInverse * skinMatrix * bindMatrix;
		objectTangent = vec4( skinMatrix * vec4( objectTangent, 0.0 ) ).xyz;
	#endif
	objectNormal = skinnedNormal;
#endif`,
    ],
  ];
})();

/** A three.js view of a {@link ViewportScene}, updated in place from each {@link ViewportUpdate}. */
export class Viewport {
  readonly #renderer: WebGLRenderer;
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
  /** The sky and sun lighting a stage without lights. */
  readonly #fallback = new Group();
  /** One geometry per stagehand buffer, by key: a sphere of one radius is uploaded once. */
  readonly #geometries = new Map<number, BufferGeometry>();
  /**
   * One object per draw item, by id: a `Mesh` for a gprim, an
   * `InstancedMesh` for a group, `LineSegments` for curves drawn as lines,
   * `Points` for a point set.
   */
  readonly #objects = new Map<number, Drawn>();
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
   * `read` and `exists` reach the stage's assets (textures and UDIM tiles)
   * as the asset loop's host does; without them textures draw as their
   * fallbacks.
   */
  constructor(canvas: HTMLCanvasElement, read: AssetReader = async () => null, exists: AssetExists = async () => false) {
    // Images arrive one by one: the draws are restyled once per batch of them.
    this.#textures = new TextureStore(read, exists, () => {
      if (this.#restyling) return;
      this.#restyling = true;
      setTimeout(() => {
        this.#restyling = false;
        this.#restyleTextured();
      }, 100);
    });
    this.#canvas = canvas;
    this.#renderer = new WebGLRenderer({ canvas, antialias: true });
    this.#renderer.setPixelRatio(window.devicePixelRatio);
    this.#renderer.toneMapping = AgXToneMapping;
    RectAreaLightUniformsLib.init();
    this.#scene.background = new Color(0x20201e);
    this.#fallback.add(new HemisphereLight(0xffffff, 0x404038, 1.4));
    const sun = new DirectionalLight(0xffffff, 1.8);
    sun.position.set(5, 10, 7);
    this.#fallback.add(sun);
    this.#scene.add(this.#fallback);
    this.#stageCamera.matrixAutoUpdate = false;
    this.#stageCamera.matrixWorldAutoUpdate = false;
    this.#grid = makeGrid("Y", 1);
    this.#scene.add(this.#grid);
    this.#controls = this.#makeControls();
    this.#resizer = new ResizeObserver(() => this.#resize());
    this.#resizer.observe(canvas);
  }

  /**
   * Orients the view to the stage's `upAxis`: the free camera orbits
   * about it, and the grid lies across it. Stage cameras are unaffected:
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

  /** Frames the free camera on `bounds` (`[minX, minY, minZ, maxX, maxY, maxZ]`). */
  frame(bounds: readonly number[]): void {
    this.#frame(bounds);
    this.#draw();
  }

  /**
   * The prim drawn under the canvas point (`x`, `y` in CSS pixels from its
   * top left), nearest first: a gprim, the instance proxy an instanced draw
   * places there, or a point instancer; `null` over nothing.
   */
  pick(x: number, y: number): string | null {
    const { clientWidth: width, clientHeight: height } = this.#canvas;
    if (width === 0 || height === 0) return null;
    const looking = this.#looking();
    const camera = looking === null ? this.#camera : this.#stageCamera;
    const raycaster = new Raycaster();
    raycaster.params.Line.threshold = 0.02;
    raycaster.setFromCamera(new Vector2((x / width) * 2 - 1, -(y / height) * 2 + 1), camera);
    const ids = new Map([...this.#objects].map(([id, object]) => [object as Object3D, id]));
    // A skinned draw's bounds move with its pose: three.js keeps the first it computed.
    for (const object of this.#objects.values()) if (object instanceof SkinnedMesh) object.computeBoundingSphere();
    const hits = raycaster.intersectObjects([...this.#objects.values()].filter((object) => object.visible), false);
    for (const hit of hits) {
      const id = ids.get(hit.object);
      const view = id === undefined ? undefined : this.#applied?.items.find((candidate) => candidate.item.id === id);
      if (view === undefined) continue;
      const { item } = view;
      if (item.source.kind === "pointInstancer") return item.source.instancer;
      return item.instances[hit.instanceId ?? 0] ?? item.prim;
    }
    return null;
  }

  /** Releases the renderer, geometries, materials and textures. */
  dispose(): void {
    this.#resizer.disconnect();
    this.#controls.dispose();
    for (const id of [...this.#objects.keys()]) this.#drop(id);
    for (const geometry of this.#geometries.values()) geometry.dispose();
    this.#geometries.clear();
    for (const path of [...this.#lights.keys()]) this.#dropLight(path);
    this.#renderer.dispose();
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
    if (reset) {
      for (const id of [...this.#objects.keys()]) this.#drop(id);
      for (const geometry of this.#geometries.values()) geometry.dispose();
      this.#geometries.clear();
    }
    for (const id of update.removed) this.#drop(id);
    for (const key of update.freed) {
      this.#geometries.get(key)?.dispose();
      this.#geometries.delete(key);
    }
    // New and rebuilt buffers: a new geometry, handed to the objects drawing it.
    for (const key of new Set([...update.buffersAdded, ...update.rebuilt])) {
      const view = scene.items.find((candidate) => candidate.item.buffer === key);
      if (view === undefined) continue;
      this.#geometries.get(key)?.dispose();
      this.#geometries.set(key, makeGeometry(view));
      for (const [id, object] of this.#objects) {
        if (items.get(id)?.item.buffer === key) object.geometry = this.#geometries.get(key)!;
      }
    }
    // Refilled buffers: the same indices, new vertex data in place.
    for (const key of update.refilled) {
      const geometry = this.#geometries.get(key);
      const view = scene.items.find((candidate) => candidate.item.buffer === key);
      if (geometry === undefined || view === undefined || update.rebuilt.has(key)) continue;
      if (view.buffers.standin !== null) {
        // A stand-in's cards may come and go: its geometry and object made again.
        geometry.dispose();
        this.#geometries.set(key, makeGeometry(view));
        if (!update.added.has(view.item.id)) {
          this.#drop(view.item.id);
          this.#add(view, selected);
        }
        continue;
      }
      const object = [...this.#objects.entries()].find(([id]) => items.get(id)?.item.buffer === key)?.[1];
      if (object !== undefined && !fits(object, view.kind)) {
        // A curve's widths crossed the line width: the same buffers, drawn the other way.
        this.#geometries.get(key)?.dispose();
        this.#geometries.set(key, makeGeometry(view));
        this.#drop(view.item.id);
        this.#add(view, selected);
        continue;
      }
      if (view.kind === "lines" || view.kind === "points") {
        refillSamples(geometry, view);
        continue;
      }
      const mesh = view.buffers.mesh;
      geometry.setAttribute("position", new BufferAttribute(mesh.positions, 3));
      geometry.setAttribute("normal", new BufferAttribute(mesh.normals, 3));
      if (mesh.colors !== null) geometry.setAttribute("color", new BufferAttribute(mesh.colors, 3));
      if (mesh.st !== null) geometry.setAttribute("uv", new BufferAttribute(mesh.st, 2));
      setUvSets(geometry, mesh);
      geometry.computeBoundingSphere();
    }
    const added = reset ? new Set(items.keys()) : update.added;
    for (const id of added) {
      const view = items.get(id);
      if (view !== undefined) this.#add(view, selected);
    }
    for (const id of update.rebound) {
      const view = items.get(id);
      const object = this.#objects.get(id);
      if (view !== undefined && object !== undefined) object.geometry = this.#geometries.get(view.item.buffer)!;
    }
    // Skinning bound again, begun or ended: the object made again over its buffer.
    for (const id of update.reskinned) {
      const view = items.get(id);
      if (added.has(id)) continue;
      this.#drop(id);
      if (view !== undefined) this.#add(view, selected);
    }
    for (const id of new Set([...update.matrices, ...update.visibility])) {
      const view = items.get(id);
      if (view !== undefined && !added.has(id) && !update.reskinned.has(id)) this.#place(view, selected);
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
      for (const material of [object.material].flat()) material.dispose();
      object.material = restyled(object, materials(view, selected, scene.materials, this.#textures));
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
    this.#fallback.visible = this.#lights.size === 0;
    this.#link(scene);
    if (reset || update.cameras.size > 0) this.#cameras = new Map(scene.cameras.map((camera) => [camera.path, camera]));
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
    if (geometry === undefined) {
      geometry = makeGeometry(view);
      this.#geometries.set(view.item.buffer, geometry);
    }
    const placements = view.item.matrices.length / 16;
    const styled = materials(view, selected, this.#applied?.materials ?? new Map(), this.#textures);
    const object: Drawn =
      view.skin?.mode === "gpu"
        ? skinnedMesh(geometry, styled as MeshStandardMaterial | MeshStandardMaterial[], view)
        : view.kind === "lines" || view.kind === "standinLines"
        ? new LineSegments(geometry, styled as LineBasicMaterial)
        : view.kind === "points"
          ? new Points(geometry, styled as PointsMaterial)
          : view.item.source.kind === "gprim" || view.item.source.kind === "drawMode"
            ? new Mesh(geometry, styled as MeshStandardMaterial | MeshStandardMaterial[])
            : new InstancedMesh(geometry, styled as MeshStandardMaterial | MeshStandardMaterial[], Math.max(1, placements));
    object.matrixAutoUpdate = false;
    this.#objects.set(view.item.id, object);
    this.#scene.add(object);
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
    object.matrix.fromArray(skin.world);
    if (object instanceof SkinnedMesh) {
      const { bones, boneInverses } = object.skeleton;
      bones.forEach((bone, joint) => bone.matrixWorld.fromArray(skin.palette, 16 * joint));
      for (const inverse of boneInverses) inverse.fromArray(skin.bind);
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
    const matrices = view.item.matrices;
    const placements = matrices.length / 16;
    object.visible = placements > 0;
    // Row-by-row row-vector matrices are three.js's column-major column-vector ones.
    if (!(object instanceof InstancedMesh)) {
      // A skinned draw is placed by its skeleton ({@link #pose}).
      if (placements > 0 && view.skin === null) object.matrix.fromArray(matrices);
      return;
    }
    if (placements > object.instanceMatrix.count) {
      this.#drop(view.item.id);
      this.#add(view, selected);
      return;
    }
    const matrix = new Matrix4();
    for (let i = 0; i < placements; i++) object.setMatrixAt(i, matrix.fromArray(matrices, 16 * i));
    object.count = placements;
    object.instanceMatrix.needsUpdate = true;
    object.computeBoundingSphere();
  }

  #addLight(light: LightItem): void {
    const objects = threeLights(light);
    for (const object of objects) {
      this.#scene.add(object);
      this.#lightKeys.set(object, light.key);
    }
    this.#lights.set(light.key, objects);
  }

  /**
   * Writes every draw's light-link masks: 0 in the slot of each three.js
   * light whose `UsdLux` light `scene.unlit` says does not light the draw,
   * 1 elsewhere. Slots move when lights come and go, so every draw is
   * written; only uniforms change.
   */
  #link(scene: ViewportScene): void {
    const slots = lightSlots(this.#scene);
    for (const [id, object] of this.#objects) {
      const unlit = scene.unlit.get(id);
      for (const material of [object.material].flat()) {
        if (!(material instanceof MeshStandardMaterial)) continue;
        const masks = linkMasks(material);
        for (const mask of Object.values(masks)) mask.fill(1);
        if (unlit === undefined) continue;
        for (const [light, [type, slot]] of slots) {
          const key = this.#lightKeys.get(light);
          if (key !== undefined && unlit.has(key) && slot < LINK_SLOTS) masks[type][slot] = 0;
        }
      }
    }
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
      for (const material of [object.material].flat()) material.dispose();
      object.material = restyled(object, materials(view, this.#selected, scene.materials, this.#textures));
    }
    this.#link(scene);
    this.#draw();
  }

  /** The stage camera the view looks through, or `null` for the free camera. */
  #looking(): CameraItem | null {
    const camera = this.#through === null ? undefined : this.#cameras.get(this.#through);
    return camera?.camera == null ? null : camera;
  }

  #drop(id: number): void {
    const object = this.#objects.get(id);
    if (object === undefined) return;
    this.#scene.remove(object);
    for (const material of [object.material].flat()) material.dispose();
    if (object instanceof InstancedMesh) object.dispose();
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
    this.#renderer.setSize(width, height, false);
    this.#camera.aspect = width / height;
    this.#camera.updateProjectionMatrix();
    this.#draw();
  }

  #draw(): void {
    const looking = this.#looking()?.camera ?? null;
    if (looking === null) {
      this.#renderer.toneMappingExposure = 1;
      this.#renderer.render(this.#scene, this.#camera);
      return;
    }
    // Row-by-row row-vector matrices are three.js's column-major
    // column-vector ones: `fromArray` reads them as they are.
    const { clientWidth: width, clientHeight: height } = this.#canvas;
    const fitted = conformCamera(looking, "fit", height > 0 ? width / height : 1);
    const camera = this.#stageCamera;
    camera.projectionMatrix.fromArray(fitted.projectionMatrix);
    camera.projectionMatrixInverse.copy(camera.projectionMatrix).invert();
    camera.matrixWorldInverse.fromArray(fitted.view);
    camera.matrixWorld.copy(camera.matrixWorldInverse).invert();
    this.#renderer.toneMappingExposure = looking.linearExposureScale;
    this.#renderer.render(this.#scene, camera);
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
 * `UsdPreviewSurface` record's (`surfaces.ts`), else its color.
 */
function materials(
  view: ViewItem,
  selected: string | null,
  records: ReadonlyMap<string, MaterialItem>,
  textures: TextureStore,
): MeshStandardMaterial | MeshStandardMaterial[] | LineBasicMaterial | PointsMaterial {
  const { item } = view;
  const paths = [item.prim, ...item.instances];
  const highlighted =
    selected !== null && paths.some((path) => selected === path || selected.startsWith(`${path}/`) || path.startsWith(`${selected}/`));
  // Lines and points: unlit, in their per-sample (or prim) display color.
  const tint = (vertexColors: boolean) =>
    highlighted ? new Color(0x6f9fff) : vertexColors ? new Color(1, 1, 1) : new Color().setRGB(...(view.color as [number, number, number]));
  if (view.buffers.standin !== null) return standinMaterials(view.buffers.standin, highlighted, textures);
  if (view.kind === "lines") {
    const vertexColors = view.buffers.curves?.colors != null;
    return new LineBasicMaterial({ color: tint(vertexColors), vertexColors });
  }
  if (view.kind === "points") {
    const points = view.buffers.points!;
    const vertexColors = points.colors !== null;
    // One size for the set (three.js points take one): its widest point's
    // width, in world units with size attenuation.
    const size = points.widths.reduce((widest, width) => Math.max(widest, width), 0);
    return new PointsMaterial({ color: tint(vertexColors), vertexColors, size, sizeAttenuation: true });
  }
  const colors = view.buffers.mesh.colors;
  // One map for every range's material: a subset's reads its sets' channels.
  const channels = uvChannels(view.buffers.mesh);
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
    return new MeshStandardMaterial({
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

/**
 * The texture coordinate sets besides `st` stagehand drew for the mesh's
 * materials, as three.js's `uv1` to `uv3` (`st` is `uv`) by the mesh's
 * {@link uvChannels}, which `surfaces.ts` points each texture at.
 */
function setUvSets(geometry: BufferGeometry, mesh: StagehandMesh): void {
  const channels = uvChannels(mesh);
  for (const set of mesh.uvSets) {
    const channel = channels.get(set.name);
    if (channel !== undefined) geometry.setAttribute(`uv${channel}`, new BufferAttribute(set.values, 2));
  }
}

/** The three.js objects a draw item becomes. */
type Drawn = Mesh | InstancedMesh | LineSegments | Points;

/** Whether `object` draws a view of `kind`: lines, points, or triangles. */
function fits(object: Drawn, kind: string): boolean {
  if (kind === "lines" || kind === "standinLines") return object instanceof LineSegments;
  if (kind === "points") return object instanceof Points;
  return object instanceof Mesh;
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
  const { positions, colors } = samplesOf(view);
  geometry.setAttribute("position", new BufferAttribute(positions, 3));
  if (colors !== null) geometry.setAttribute("color", new BufferAttribute(colors, 3));
  geometry.computeBoundingSphere();
}

/** New materials for `object`: a skinned draw's patched for its skinning. */
function restyled(object: Drawn, styled: ReturnType<typeof materials>): ReturnType<typeof materials> {
  return object instanceof SkinnedMesh ? skinnedMaterials(styled as MeshStandardMaterial | MeshStandardMaterial[]) : styled;
}

/**
 * A `SkinnedMesh` drawing a GPU-skinned item: its geometry gets the
 * gathered influences as `skinIndex`, `skinWeight`, `skinIndex1` and
 * `skinWeight1`, and the mesh a skeleton of one bone per joint, outside
 * the scene graph, posed by {@link Viewport} each time. Never
 * frustum-culled: three.js would cull it by the bounds of one pose.
 */
function skinnedMesh(geometry: BufferGeometry, material: MeshStandardMaterial | MeshStandardMaterial[], view: ViewItem): SkinnedMesh {
  const skin = view.skin!;
  const influences = skin.influences!;
  // Eight per vertex, as two attributes of four ({@link patchSkinning}).
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
  const mesh = new SkinnedMesh(geometry, skinnedMaterials(material));
  mesh.bindMode = DetachedBindMode;
  mesh.bind(new Skeleton(bones, bones.map(() => inverse.clone())), new Matrix4());
  mesh.frustumCulled = false;
  return mesh;
}

/** A geometry over the item's stagehand buffer: indexed, with normals, colors and a group per range. */
function makeGeometry(view: ViewItem): BufferGeometry {
  if (view.buffers.standin !== null) return standinGeometry(view.buffers.standin);
  if (view.kind === "lines" || view.kind === "points") {
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
  geometry.setIndex(new BufferAttribute(mesh.indices, 1));
  geometry.setAttribute("position", new BufferAttribute(mesh.positions, 3));
  geometry.setAttribute("normal", new BufferAttribute(mesh.normals, 3));
  if (mesh.colors !== null) geometry.setAttribute("color", new BufferAttribute(mesh.colors, 3));
  if (mesh.st !== null) geometry.setAttribute("uv", new BufferAttribute(mesh.st, 2));
  setUvSets(geometry, mesh);
  // A group per range when there are subsets, each drawn with its range's material.
  if (mesh.ranges.length > 1) mesh.ranges.forEach((range, index) => geometry.addGroup(range.first, range.count, index));
  geometry.computeBoundingSphere();
  return geometry;
}

/** The lights of one three.js type a material is linked against; more are lit as three.js lights them. */
const LINK_SLOTS = 8;

/** The three.js light types whose contributions a link mask scales, by their uniform's suffix. */
type LinkType = "Point" | "Spot" | "Directional" | "RectArea" | "Hemisphere";

/** A material's link masks: per light type, 1 where the light in that slot lights it, 0 where a light link leaves it out. */
type LinkMasks = Record<LinkType, Float32Array>;

/**
 * `lights_fragment_begin` with each light's contribution scaled by its
 * slot's mask; `null` if this three.js's chunk does not have the lines
 * patched, and draws are then lit by every light.
 */
const LINKED_LIGHTS = ((): string | null => {
  let chunk = ShaderChunk.lights_fragment_begin;
  const guarded = (statement: string) => `\n\t\t#if ( UNROLLED_LOOP_INDEX < ${LINK_SLOTS} )\n\t\t${statement}\n\t\t#endif\n`;
  const patches: [string, string][] = [
    ["pointLight = pointLights[ i ];", `pointLight = pointLights[ i ];${guarded("pointLight.color *= linkPoint[ i ];")}`],
    ["spotLight = spotLights[ i ];", `spotLight = spotLights[ i ];${guarded("spotLight.color *= linkSpot[ i ];")}`],
    ["directionalLight = directionalLights[ i ];", `directionalLight = directionalLights[ i ];${guarded("directionalLight.color *= linkDirectional[ i ];")}`],
    ["rectAreaLight = rectAreaLights[ i ];", `rectAreaLight = rectAreaLights[ i ];${guarded("rectAreaLight.color *= linkRectArea[ i ];")}`],
    [
      "irradiance += getHemisphereLightIrradiance( hemisphereLights[ i ], geometryNormal );",
      `${guarded("irradiance += ( linkHemisphere[ i ] - 1.0 ) * getHemisphereLightIrradiance( hemisphereLights[ i ], geometryNormal );")}irradiance += getHemisphereLightIrradiance( hemisphereLights[ i ], geometryNormal );`,
    ],
  ];
  for (const [line, patched] of patches) {
    if (!chunk.includes(line)) return null;
    chunk = chunk.replace(line, patched);
  }
  return chunk;
})();

/** The uniforms the patched chunk reads. */
const LINK_UNIFORMS = (["Point", "Spot", "Directional", "RectArea", "Hemisphere"] as const)
  .map((type) => `uniform float link${type}[ ${LINK_SLOTS} ];`)
  .join("\n");

/**
 * The patches a material's shader carries: its light-link masks
 * ({@link linkMasks}) and the editor's skinning ({@link skinnedMaterials}).
 * One `onBeforeCompile` applies whichever it has, after the material's own
 * (its maps' channels, `readChannels`), so all compose on one material, and its `customProgramCacheKey` names them, so materials
 * patched alike share one program and never one patched otherwise (three.js
 * would key every material with a custom `onBeforeCompile` by the
 * function's source, the same for all of them).
 */
interface Patches {
  links: LinkMasks | null;
  skinning: boolean;
}

/** The material's patches, its shader hooked the first time. */
function patches(material: MeshStandardMaterial): Patches {
  const kept = material.userData.patches as Patches | undefined;
  if (kept !== undefined) return kept;
  const patched: Patches = { links: null, skinning: false };
  material.userData.patches = patched;
  // After the material's own patch (`readChannels`, the maps' channels), keyed with it.
  const before = material.onBeforeCompile;
  const own = material.customProgramCacheKey();
  material.onBeforeCompile = (shader, renderer) => {
    before.call(material, shader, renderer);
    const { links, skinning } = patched;
    if (links !== null && LINKED_LIGHTS !== null) {
      for (const type of Object.keys(links) as LinkType[]) shader.uniforms[`link${type}`] = { value: links[type] };
      shader.fragmentShader = shader.fragmentShader
        .replace("void main() {", `${LINK_UNIFORMS}\nvoid main() {`)
        .replace("#include <lights_fragment_begin>", LINKED_LIGHTS);
    }
    if (skinning) for (const [include, chunk] of SKINNING) shader.vertexShader = shader.vertexShader.replace(include, chunk);
  };
  material.customProgramCacheKey = () =>
    [own, patched.links !== null && LINKED_LIGHTS !== null ? "light-links" : "", patched.skinning ? "usdskel-skinning" : ""].join("|");
  return patched;
}

/** The material's link masks, its shader patched the first time. */
function linkMasks(material: MeshStandardMaterial): LinkMasks {
  const patched = patches(material);
  if (patched.links !== null) return patched.links;
  patched.links = {
    Point: new Float32Array(LINK_SLOTS).fill(1),
    Spot: new Float32Array(LINK_SLOTS).fill(1),
    Directional: new Float32Array(LINK_SLOTS).fill(1),
    RectArea: new Float32Array(LINK_SLOTS).fill(1),
    Hemisphere: new Float32Array(LINK_SLOTS).fill(1),
  };
  // A program compiled without the masks is compiled again.
  material.needsUpdate = true;
  return patched.links;
}

/** The materials of a {@link skinnedMesh}, their shaders patched with the editor's skinning ({@link SKINNING}). */
function skinnedMaterials<T extends MeshStandardMaterial | MeshStandardMaterial[]>(styled: T): T {
  for (const material of [styled].flat()) {
    const patched = patches(material);
    if (patched.skinning) continue;
    patched.skinning = true;
    material.needsUpdate = true;
  }
  return styled;
}

/**
 * Each light's type and slot as `WebGLLights` numbers them: lights of a
 * type in the order the renderer meets them, a depth-first walk of the
 * visible objects, shadow-casting and textured ones first (`castShadow`,
 * `map`), stably.
 */
function lightSlots(scene: Scene): Map<Object3D, [LinkType, number]> {
  const found: Object3D[] = [];
  const walk = (object: Object3D) => {
    if (!object.visible) return;
    if ((object as { isLight?: boolean }).isLight) found.push(object);
    for (const child of object.children) walk(child);
  };
  walk(scene);
  const rank = (light: Object3D) => ((light as { castShadow?: boolean }).castShadow ? 2 : 0) + ((light as { map?: unknown }).map ? 1 : 0);
  found.sort((a, b) => rank(b) - rank(a));
  const counts: Record<LinkType, number> = { Point: 0, Spot: 0, Directional: 0, RectArea: 0, Hemisphere: 0 };
  const slots = new Map<Object3D, [LinkType, number]>();
  for (const light of found) {
    const type: LinkType | null =
      light instanceof PointLight
        ? "Point"
        : light instanceof SpotLight
          ? "Spot"
          : light instanceof DirectionalLight
            ? "Directional"
            : light instanceof RectAreaLight
              ? "RectArea"
              : light instanceof HemisphereLight
                ? "Hemisphere"
                : null;
    if (type === null) continue;
    slots.set(light, [type, counts[type]++]);
  }
  return slots;
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
