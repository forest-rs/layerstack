// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The viewport: three.js draws what `viewport.ts` read from the stage.
// Every matrix, triangle and color comes from layerstack; three.js only
// rasterizes them. No USD loader is involved.
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
import type { DrawItem, Triangles, ViewportScene } from "./viewport.ts";

/** A three.js view of a {@link ViewportScene}, updated in place after each action. */
export class Viewport {
  readonly #renderer: WebGLRenderer;
  readonly #scene = new Scene();
  readonly #camera = new PerspectiveCamera(45, 1, 0.05, 500);
  readonly #controls: OrbitControls;
  readonly #canvas: HTMLCanvasElement;
  /** Shared buffers by triangles key: a sphere of one radius is uploaded once. */
  readonly #geometries = new Map<string, BufferGeometry>();
  #objects: (Mesh | InstancedMesh)[] = [];
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

  /** Shows `scene`, `selected` highlighted, framing the camera when the bounds changed. */
  update(scene: ViewportScene, selected: string | null): void {
    for (const object of this.#objects) {
      this.#scene.remove(object);
      (object.material as MeshStandardMaterial).dispose();
      if (object instanceof InstancedMesh) object.dispose();
    }
    this.#objects = scene.items.map((item) => this.#object(item, selected));
    const keys = new Set(scene.items.map((item) => item.triangles.key));
    for (const [key, geometry] of this.#geometries) {
      if (keys.has(key)) continue;
      geometry.dispose();
      this.#geometries.delete(key);
    }
    for (const object of this.#objects) this.#scene.add(object);
    const framed = JSON.stringify(scene.bounds);
    if (scene.bounds !== null && framed !== this.#framed) {
      this.#framed = framed;
      this.#frame(scene.bounds);
    }
    this.#draw();
  }

  #object(item: DrawItem, selected: string | null): Mesh | InstancedMesh {
    const geometry = this.#geometry(item.triangles);
    const [r, g, b] = item.color;
    const highlighted = selected !== null && (selected === item.path || selected.startsWith(`${item.path}/`) || item.path.startsWith(`${selected}/`));
    const material = new MeshStandardMaterial({
      color: new Color().setRGB(r, g, b),
      roughness: 0.7,
      ...(item.kind === "mesh" ? { side: DoubleSide } : {}),
      emissive: highlighted ? new Color(0x2f5fd0) : new Color(0),
      emissiveIntensity: highlighted ? 0.6 : 0,
    });
    const matrix = new Matrix4();
    const placements = item.matrices.length / 16;
    // Row-by-row row-vector matrices are three.js's column-major column-vector ones.
    if (item.prototype === null) {
      const mesh = new Mesh(geometry, material);
      mesh.matrixAutoUpdate = false;
      mesh.matrix.fromArray(item.matrices);
      return mesh;
    }
    const instances = new InstancedMesh(geometry, material, placements);
    for (let i = 0; i < placements; i++) instances.setMatrixAt(i, matrix.fromArray(item.matrices, 16 * i));
    instances.instanceMatrix.needsUpdate = true;
    instances.computeBoundingSphere();
    return instances;
  }

  #geometry(triangles: Triangles): BufferGeometry {
    let geometry = this.#geometries.get(triangles.key);
    if (geometry === undefined) {
      geometry = new BufferGeometry();
      geometry.setAttribute("position", new BufferAttribute(triangles.positions, 3));
      geometry.setAttribute("normal", new BufferAttribute(triangles.normals, 3));
      geometry.computeBoundingSphere();
      this.#geometries.set(triangles.key, geometry);
    }
    return geometry;
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
