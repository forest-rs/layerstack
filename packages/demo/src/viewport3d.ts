// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws what `viewport.ts` keeps of the stage.
// Every matrix and color comes from layerstack and every triangle from
// stagehand's buffers (indexed, with normals, per-vertex colors and a
// group per subset range); three.js only rasterizes them. No USD loader is
// involved. Each update applies the draw list's diffs in place: an
// `InstancedMesh` per instanced item whose matrices are rewritten, a
// buffer geometry per stagehand buffer whose attributes are refilled, and
// nothing rebuilt that the diffs do not name.
import {
  BufferAttribute,
  BufferGeometry,
  Color,
  DirectionalLight,
  DoubleSide,
  GridHelper,
  HemisphereLight,
  InstancedMesh,
  Matrix4,
  Mesh,
  MeshStandardMaterial,
  PerspectiveCamera,
  Scene,
  Vector3,
  WebGLRenderer,
} from "three";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import type { ViewItem, ViewportScene, ViewportUpdate } from "./viewport.ts";

/** A three.js view of a {@link ViewportScene}, updated in place from each {@link ViewportUpdate}. */
export class Viewport {
  readonly #renderer: WebGLRenderer;
  readonly #scene = new Scene();
  readonly #camera = new PerspectiveCamera(45, 1, 0.05, 500);
  readonly #controls: OrbitControls;
  readonly #canvas: HTMLCanvasElement;
  /** One geometry per stagehand buffer, by key: a sphere of one radius is uploaded once. */
  readonly #geometries = new Map<number, BufferGeometry>();
  /** One object per draw item, by id: a `Mesh` for a gprim, an `InstancedMesh` for a group. */
  readonly #objects = new Map<number, Mesh | InstancedMesh>();
  /** Whether an update was applied yet. */
  #synced = false;
  /** The selection the materials were last made for. */
  #selected: string | null = null;
  /** The bounds the camera was last framed on, to frame again only when they change. */
  #framed = "";

  constructor(canvas: HTMLCanvasElement) {
    this.#canvas = canvas;
    this.#renderer = new WebGLRenderer({ canvas, antialias: true });
    this.#renderer.setPixelRatio(window.devicePixelRatio);
    this.#scene.background = new Color(0x20201e);
    this.#scene.add(new HemisphereLight(0xffffff, 0x404038, 1.4));
    const sun = new DirectionalLight(0xffffff, 1.8);
    sun.position.set(5, 10, 7);
    this.#scene.add(sun);
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
      const mesh = view.buffers.mesh;
      geometry.setAttribute("position", new BufferAttribute(mesh.positions, 3));
      geometry.setAttribute("normal", new BufferAttribute(mesh.normals, 3));
      if (mesh.colors !== null) geometry.setAttribute("color", new BufferAttribute(mesh.colors, 3));
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
      object.material = materials(view, selected);
    }
    this.#selected = selected;
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
    const object =
      view.item.source.kind === "gprim"
        ? new Mesh(geometry, materials(view, selected))
        : new InstancedMesh(geometry, materials(view, selected), Math.max(1, placements));
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
    this.#renderer.render(this.#scene, this.#camera);
  }
}

/** The item's materials: one per range when it has subsets, its own color otherwise, highlighted when selected. */
function materials(view: ViewItem, selected: string | null): MeshStandardMaterial | MeshStandardMaterial[] {
  const { item } = view;
  const paths = [item.prim, ...item.instances];
  const highlighted =
    selected !== null && paths.some((path) => selected === path || selected.startsWith(`${path}/`) || path.startsWith(`${selected}/`));
  const colors = view.buffers.mesh.colors;
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
  return view.buffers.mesh.ranges.length > 1 ? view.rangeColors.map(material) : material(null);
}

/** A geometry over the item's stagehand buffer: indexed, with normals, colors and a group per range. */
function makeGeometry(view: ViewItem): BufferGeometry {
  const mesh = view.buffers.mesh;
  const geometry = new BufferGeometry();
  geometry.setIndex(new BufferAttribute(mesh.indices, 1));
  geometry.setAttribute("position", new BufferAttribute(mesh.positions, 3));
  geometry.setAttribute("normal", new BufferAttribute(mesh.normals, 3));
  if (mesh.colors !== null) geometry.setAttribute("color", new BufferAttribute(mesh.colors, 3));
  // A group per range when there are subsets, each drawn with its range's material.
  if (mesh.ranges.length > 1) mesh.ranges.forEach((range, index) => geometry.addGroup(range.first, range.count, index));
  geometry.computeBoundingSphere();
  return geometry;
}
