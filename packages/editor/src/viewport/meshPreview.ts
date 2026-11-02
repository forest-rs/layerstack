// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A mesh alone in a canvas of the shared WebGPU renderer (`gpu.ts`), for
// the inspector's mesh view: orbited by dragging, zoomed by scrolling,
// framed on the mesh's extent. It draws the geometry the viewport already
// uploaded for the mesh (`Viewport.sharedGeometry`: the same three.js
// attributes, so the same GPU buffers, nothing uploaded again), or, for a
// mesh the viewport does not draw, the buffers stagehand built for it
// alone. Its modes: shaded (with `displayColor` when the buffers carry it),
// a checker on `st`, a face subset's triangles highlighted (its draw
// range; the rest ghosted), and, from buffers held on the CPU, wireframe
// edges and vertex normals.
import {
  BufferAttribute,
  BufferGeometry,
  CanvasTarget,
  Color,
  DirectionalLight,
  DoubleSide,
  FrontSide,
  HemisphereLight,
  LineBasicNodeMaterial,
  LineSegments,
  Matrix4,
  Mesh,
  MeshStandardNodeMaterial,
  PerspectiveCamera,
  Scene,
  Vector3,
  WireframeGeometry,
} from "three/webgpu";
import type { WebGPURenderer } from "three/webgpu";
import { checker, mix, uv, vec3 } from "three/tsl";
import { OrbitControls } from "three/addons/controls/OrbitControls.js";
import { onRendererLost, sharedRenderer } from "./gpu.ts";

/** A mesh's buffers held on the CPU: stagehand's, built for the preview. */
export interface CpuMesh {
  readonly positions: Float32Array;
  readonly normals: Float32Array;
  readonly st: Float32Array | null;
  readonly colors: Float32Array | null;
  readonly indices: Uint32Array;
}

/** What the preview draws. */
export interface MeshPreviewSource {
  /** The viewport's geometry for the mesh, and its position decode; `null` when it has none to share. */
  readonly shared: { readonly geometry: BufferGeometry; readonly decode: Matrix4 | null } | null;
  /** The mesh's buffers on the CPU, when built: drawn when nothing is shared, and the source of wireframe and normals. */
  readonly cpu: CpuMesh | null;
  /** The mesh's extent, `[minX, minY, minZ, maxX, maxY, maxZ]`, to frame. */
  readonly bounds: readonly number[];
}

/** How the preview draws. */
export interface MeshPreviewStyle {
  readonly checker: boolean;
  readonly wireframe: boolean;
  readonly normals: boolean;
  readonly doubleSided: boolean;
  /** A triangle range to highlight (`first` and `count` in indices), the rest ghosted; `null` for none. */
  readonly highlight: { readonly first: number; readonly count: number } | null;
}

/** The most vertices whose normals are drawn: past it, every n-th. */
const NORMALS_SHOWN = 20000;

export class MeshPreview {
  #renderer: WebGPURenderer | null;
  readonly #canvas: HTMLCanvasElement;
  readonly #target: CanvasTarget;
  readonly #scene = new Scene();
  readonly #camera = new PerspectiveCamera(35, 1, 0.01, 1000);
  readonly #controls: OrbitControls;
  readonly #resizer: ResizeObserver;
  readonly #unlisten: () => void;
  /** What is drawn now; replaced by each {@link show}. */
  readonly #drawn: { object: Mesh | LineSegments; owned: BufferGeometry | null }[] = [];
  #ownGeometry: BufferGeometry | null = null;
  #ownFrom: CpuMesh | null = null;
  #framed: string | null = null;
  #frame = 0;

  constructor(canvas: HTMLCanvasElement, renderer: WebGPURenderer) {
    this.#canvas = canvas;
    this.#renderer = renderer;
    this.#target = new CanvasTarget(canvas);
    this.#scene.add(new HemisphereLight(0xffffff, 0x404048, 1.6));
    const key = new DirectionalLight(0xffffff, 1.8);
    key.position.set(-1, 2, 3);
    this.#camera.add(key);
    this.#scene.add(this.#camera);
    this.#controls = new OrbitControls(this.#camera, canvas);
    this.#controls.enableDamping = false;
    this.#controls.addEventListener("change", () => this.#draw());
    this.#resizer = new ResizeObserver(() => this.#draw());
    this.#resizer.observe(canvas);
    this.#unlisten = onRendererLost(() => {
      this.#renderer = null;
      void sharedRenderer().then((next) => {
        this.#renderer = next;
        this.#draw();
      });
    });
  }

  /** Draws `source` in `style`, framing it when its extent is new. */
  show(source: MeshPreviewSource, style: MeshPreviewStyle): void {
    this.#clear();
    const geometry = source.shared?.geometry ?? this.#own(source.cpu);
    const decode = source.shared?.decode ?? null;
    if (geometry !== null) {
      const colors = geometry.getAttribute("color") !== undefined && !style.checker;
      const base = new MeshStandardNodeMaterial({
        color: colors ? new Color(1, 1, 1) : new Color(0.62, 0.62, 0.6),
        vertexColors: colors,
        roughness: 0.65,
        side: style.doubleSided ? DoubleSide : FrontSide,
      });
      if (style.checker && geometry.getAttribute("uv") !== undefined) base.colorNode = mix(vec3(0.15, 0.15, 0.17), vec3(0.85, 0.85, 0.82), checker(uv().mul(8)));
      if (style.highlight !== null) {
        base.transparent = true;
        base.opacity = 0.25;
        base.depthWrite = false;
      }
      this.#add(new Mesh(geometry, base), null, decode);
      if (style.highlight !== null) {
        // The same attributes and index, so the same GPU buffers, drawn over a range.
        const part = new BufferGeometry();
        for (const [name, attribute] of Object.entries(geometry.attributes)) part.setAttribute(name, attribute);
        part.setIndex(geometry.index);
        part.setDrawRange(style.highlight.first, style.highlight.count);
        // Its arrays may be released: the bounds are the whole geometry's, made before.
        part.boundingSphere = geometry.boundingSphere?.clone() ?? null;
        part.boundingBox = geometry.boundingBox?.clone() ?? null;
        const lit = new MeshStandardNodeMaterial({ color: new Color(1, 0.55, 0.15), emissive: new Color(0.35, 0.15, 0), roughness: 0.5, side: DoubleSide });
        // Never disposed: three.js frees a disposed geometry's attributes' GPU
        // buffers, which are the viewport's own; left to the collector, it
        // frees nothing of them.
        this.#add(new Mesh(part, lit), null, decode);
      }
    }
    const cpu = source.cpu;
    if (cpu !== null && (style.wireframe || style.normals)) {
      const own = this.#own(cpu)!;
      if (style.wireframe) {
        const edges = new WireframeGeometry(own);
        this.#add(new LineSegments(edges, new LineBasicNodeMaterial({ color: new Color(0.1, 0.1, 0.1), transparent: true, opacity: 0.55 })), edges, null);
      }
      if (style.normals) {
        const lines = normalLines(cpu, source.bounds);
        this.#add(new LineSegments(lines, new LineBasicNodeMaterial({ color: new Color(0.2, 0.55, 1) })), lines, null);
      }
    }
    const key = source.bounds.join(",");
    if (key !== this.#framed) {
      this.#framed = key;
      this.#frameOn(source.bounds);
    }
    this.#draw();
  }

  dispose(): void {
    cancelAnimationFrame(this.#frame);
    this.#clear();
    this.#ownGeometry?.dispose();
    this.#controls.dispose();
    this.#resizer.disconnect();
    this.#unlisten();
    // The renderer is shared, and so are the viewport's geometries: only this canvas's target goes.
    this.#target.dispose();
  }

  /** A geometry of the CPU buffers, made once per buffers. */
  #own(cpu: CpuMesh | null): BufferGeometry | null {
    if (cpu === null) return null;
    if (this.#ownFrom === cpu && this.#ownGeometry !== null) return this.#ownGeometry;
    this.#ownGeometry?.dispose();
    const geometry = new BufferGeometry();
    geometry.setAttribute("position", new BufferAttribute(cpu.positions, 3));
    geometry.setAttribute("normal", new BufferAttribute(cpu.normals, 3));
    if (cpu.st !== null) geometry.setAttribute("uv", new BufferAttribute(cpu.st, 2));
    if (cpu.colors !== null) geometry.setAttribute("color", new BufferAttribute(cpu.colors, 3));
    geometry.setIndex(new BufferAttribute(cpu.indices, 1));
    this.#ownGeometry = geometry;
    this.#ownFrom = cpu;
    return geometry;
  }

  #add(object: Mesh | LineSegments, owned: BufferGeometry | null, decode: Matrix4 | null): void {
    if (decode !== null) {
      object.matrixAutoUpdate = false;
      object.matrix.copy(decode);
    }
    object.frustumCulled = false;
    this.#scene.add(object);
    this.#drawn.push({ object, owned });
  }

  #clear(): void {
    for (const { object, owned } of this.#drawn) {
      this.#scene.remove(object);
      (object.material as { dispose(): void }).dispose();
      owned?.dispose();
    }
    this.#drawn.length = 0;
  }

  #frameOn(bounds: readonly number[]): void {
    const [x0 = -1, y0 = -1, z0 = -1, x1 = 1, y1 = 1, z1 = 1] = bounds;
    const center = new Vector3((x0 + x1) / 2, (y0 + y1) / 2, (z0 + z1) / 2);
    const radius = Math.max(1e-6, Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 2);
    const distance = radius / Math.sin((this.#camera.fov * Math.PI) / 360);
    this.#camera.near = distance / 100;
    this.#camera.far = distance * 100;
    this.#camera.position.copy(center).add(new Vector3(0.6, 0.45, 0.65).normalize().multiplyScalar(distance));
    this.#controls.target.copy(center);
    this.#camera.updateProjectionMatrix();
    this.#controls.update();
  }

  #draw(): void {
    if (this.#frame !== 0) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      const renderer = this.#renderer;
      const width = this.#canvas.clientWidth;
      const height = this.#canvas.clientHeight;
      if (renderer === null || width === 0 || height === 0) return;
      if (this.#canvas.width !== Math.round(width * window.devicePixelRatio)) {
        this.#target.setPixelRatio(window.devicePixelRatio);
        this.#target.setSize(width, height, false);
      }
      this.#camera.aspect = width / height;
      this.#camera.updateProjectionMatrix();
      renderer.setCanvasTarget(this.#target);
      renderer.setRenderTarget(null);
      renderer.setClearColor(0x000000, 0);
      renderer.render(this.#scene, this.#camera);
    });
  }
}

/** A line along each vertex's normal (every n-th past {@link NORMALS_SHOWN}), a 30th of the extent's diagonal long. */
function normalLines(cpu: CpuMesh, bounds: readonly number[]): BufferGeometry {
  const vertices = cpu.positions.length / 3;
  const step = Math.max(1, Math.ceil(vertices / NORMALS_SHOWN));
  const [x0 = 0, y0 = 0, z0 = 0, x1 = 1, y1 = 1, z1 = 1] = bounds;
  const length = Math.hypot(x1 - x0, y1 - y0, z1 - z0) / 30;
  const count = Math.ceil(vertices / step);
  const lines = new Float32Array(count * 6);
  for (let line = 0; line < count; line++) {
    const at = line * step * 3;
    for (let c = 0; c < 3; c++) {
      lines[line * 6 + c] = cpu.positions[at + c]!;
      lines[line * 6 + 3 + c] = cpu.positions[at + c]! + cpu.normals[at + c]! * length;
    }
  }
  const geometry = new BufferGeometry();
  geometry.setAttribute("position", new BufferAttribute(lines, 3));
  return geometry;
}
