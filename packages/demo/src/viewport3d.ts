// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws what `viewport.ts` keeps of the stage.
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
// stays in view), until "Free camera" hands the view to orbit controls.
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
import {
  AgXToneMapping,
  BufferAttribute,
  BufferGeometry,
  Camera,
  Color,
  DirectionalLight,
  DoubleSide,
  GridHelper,
  Group,
  HemisphereLight,
  InstancedMesh,
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
  RectAreaLight,
  Scene,
  ShaderChunk,
  SpotLight,
  Vector3,
  WebGLRenderer,
} from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { RectAreaLightUniformsLib } from "three/addons/lights/RectAreaLightUniformsLib.js";
import { conformCamera, lightSizeFactor } from "@forest-rs/layerstack_stagehand";
import type { CameraItem, LightItem, MaterialItem, StagehandMesh } from "@forest-rs/layerstack_stagehand";
import { TextureStore, surfaceMaterial, uvChannels } from "./surfaces.ts";
import type { AssetExists, AssetReader } from "./surfaces.ts";
import type { ViewItem, ViewportScene, ViewportUpdate } from "./viewport.ts";

/** A three.js view of a {@link ViewportScene}, updated in place from each {@link ViewportUpdate}. */
export class Viewport {
  readonly #renderer: WebGLRenderer;
  readonly #scene = new Scene();
  /** The free camera, which the orbit controls move. */
  readonly #camera = new PerspectiveCamera(45, 1, 0.05, 500);
  /** The stage's camera: its matrices are set from layerstack's, never computed by three.js. */
  readonly #stageCamera = new Camera();
  readonly #controls: OrbitControls;
  readonly #canvas: HTMLCanvasElement;
  /** "Free camera": checked, the orbit controls' camera draws; unchecked, the stage's when it has one. */
  readonly #free: HTMLInputElement | null;
  /** The stage camera last listed, and its exposure scale. */
  #usdCamera: CameraItem | null = null;
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
  /** The scene last applied, to restyle textured draws when their images arrive. */
  #applied: ViewportScene | null = null;

  /**
   * `read` and `exists` reach the stage's assets (textures and UDIM tiles)
   * as the asset loop's host does; without them textures draw as their
   * fallbacks.
   */
  constructor(
    canvas: HTMLCanvasElement,
    free: HTMLInputElement | null = null,
    read: AssetReader = async () => null,
    exists: AssetExists = async () => false,
  ) {
    this.#textures = new TextureStore(read, exists, () => this.#restyleTextured());
    this.#canvas = canvas;
    this.#free = free;
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
    free?.addEventListener("change", () => {
      this.#controls.enabled = this.#looking() === null;
      this.#draw();
    });
    const grid = new GridHelper(40, 40, 0x55554f, 0x3a3a35);
    grid.position.y = -0.001;
    this.#scene.add(grid);
    this.#controls = new OrbitControls(this.#camera, canvas);
    this.#controls.addEventListener("change", () => this.#draw());
    new ResizeObserver(() => this.#resize()).observe(canvas);
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
    for (const id of new Set([...update.matrices, ...update.visibility])) {
      const view = items.get(id);
      if (view !== undefined && !added.has(id)) this.#place(view, selected);
    }
    const restyle = selected !== this.#selected ? new Set(items.keys()) : update.recolored;
    for (const id of restyle) {
      const view = items.get(id);
      const object = this.#objects.get(id);
      if (view === undefined || object === undefined || added.has(id)) continue;
      for (const material of [object.material].flat()) material.dispose();
      object.material = materials(view, selected, scene.materials, this.#textures);
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
    if (reset || update.cameras.size > 0) this.#usdCamera = scene.camera;
    this.#controls.enabled = this.#looking() === null;
    const framed = JSON.stringify(scene.bounds);
    if (scene.bounds !== null && framed !== this.#framed) {
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
      view.kind === "lines"
        ? new LineSegments(geometry, styled as LineBasicMaterial)
        : view.kind === "points"
          ? new Points(geometry, styled as PointsMaterial)
          : view.item.source.kind === "gprim"
            ? new Mesh(geometry, styled as MeshStandardMaterial | MeshStandardMaterial[])
            : new InstancedMesh(geometry, styled as MeshStandardMaterial | MeshStandardMaterial[], Math.max(1, placements));
    object.matrixAutoUpdate = false;
    this.#objects.set(view.item.id, object);
    this.#scene.add(object);
    this.#place(view, selected);
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
      if (placements > 0) object.matrix.fromArray(matrices);
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
      if (!bound.some((path) => path !== null && (scene.materials.get(path)?.textures.length ?? 0) > 0)) continue;
      const object = this.#objects.get(view.item.id);
      if (object === undefined) continue;
      for (const material of [object.material].flat()) material.dispose();
      object.material = materials(view, this.#selected, scene.materials, this.#textures);
    }
    this.#link(scene);
    this.#draw();
  }

  /** The stage camera the view looks through, or `null` for the free camera. */
  #looking(): CameraItem | null {
    return this.#free?.checked === true || this.#usdCamera?.camera == null ? null : this.#usdCamera;
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
    const radius = Math.max(0.5, Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 2);
    // Close enough that the bounds' sphere fills most of the view's height.
    const distance = (0.8 * radius) / Math.tan((this.#camera.fov * Math.PI) / 360);
    this.#camera.position.copy(center).add(new Vector3(0.55, 0.45, 0.7).normalize().multiplyScalar(distance));
    this.#camera.near = distance / 100;
    this.#camera.far = distance * 10;
    this.#camera.updateProjectionMatrix();
    this.#controls.target.copy(center);
    this.#controls.update();
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
  if (kind === "lines") return object instanceof LineSegments;
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

/** A geometry over the item's stagehand buffer: indexed, with normals, colors and a group per range. */
function makeGeometry(view: ViewItem): BufferGeometry {
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

/** The material's link masks, patching its shader the first time. */
function linkMasks(material: MeshStandardMaterial): LinkMasks {
  const kept = material.userData.linkMasks as LinkMasks | undefined;
  if (kept !== undefined) return kept;
  const masks: LinkMasks = {
    Point: new Float32Array(LINK_SLOTS).fill(1),
    Spot: new Float32Array(LINK_SLOTS).fill(1),
    Directional: new Float32Array(LINK_SLOTS).fill(1),
    RectArea: new Float32Array(LINK_SLOTS).fill(1),
    Hemisphere: new Float32Array(LINK_SLOTS).fill(1),
  };
  material.userData.linkMasks = masks;
  if (LINKED_LIGHTS === null) return masks;
  material.onBeforeCompile = (shader) => {
    for (const type of Object.keys(masks) as LinkType[]) shader.uniforms[`link${type}`] = { value: masks[type] };
    shader.fragmentShader = shader.fragmentShader
      .replace("void main() {", `${LINK_UNIFORMS}\nvoid main() {`)
      .replace("#include <lights_fragment_begin>", LINKED_LIGHTS);
  };
  // Every patched material shares one program.
  material.customProgramCacheKey = () => "light-links";
  return masks;
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
