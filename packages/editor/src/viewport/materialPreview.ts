// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A material drawn on a sphere, for the inspector: a small three.js
// renderer of its own, lit by a room environment (for reflections) and a
// key light (for relief).
//
// A `UsdPreviewSurface` material is the viewport's own three.js material
// (`surfaceMaterial`, mapped as surfaces.ts describes). An OpenPBR surface
// (`readOpenPbr`) is approximated by `MeshPhysicalMaterial`:
//
// - `base_color` × `base_weight` → `color` (a texture → `map`, tinted by
//   the weight); `base_metalness` → `metalness` (`metalnessMap`, blue);
//   `specular_roughness` → `roughness` (`roughnessMap`, green). A float
//   image is taken to be grey, so the channel three.js reads is its value.
// - `specular_weight` → `specularIntensity` (a texture's default: three.js
//   reads its map's alpha); `specular_color` → `specularColor`
//   (`specularColorMap`); `specular_ior` → `ior` (clamped to [1, 2.333]);
//   `specular_roughness_anisotropy` → `anisotropy`.
// - `coat_weight`, `coat_roughness` → `clearcoat`, `clearcoatRoughness`
//   (maps on red and green); the coat's colour and normal are not drawn.
// - `fuzz_*` → `sheen`, `sheenColor`, `sheenRoughness`.
// - `emission_color` → `emissive`, at `emission_luminance` / 1000 (three.js
//   lights here are not in nits).
// - `transmission_weight` → `transmission`, `transmission_color` and
//   `transmission_depth` → `attenuationColor` and `attenuationDistance`.
// - `geometry_opacity` → `opacity` (`alphaMap`, green), blended.
// - `thin_film_*` → `iridescence`, `iridescenceIOR`, and its thickness
//   (micrometres) as `iridescenceThicknessRange` (nanometres).
// - `geometry_normal`'s image → `normalMap` at the `ND_normalmap` scale; a
//   flipped red or green channel negates `normalScale`'s x or y.
//
// The sphere's `st` runs four times around its equator and twice from
// pole to pole: a square texture tile stays square, and a tile of about a
// metre is drawn about life size on a sphere of a metre's radius.
import {
  AgXToneMapping,
  Color,
  DirectionalLight,
  Mesh,
  MeshPhysicalMaterial,
  MeshStandardMaterial,
  PerspectiveCamera,
  PMREMGenerator,
  Scene,
  SphereGeometry,
  SRGBColorSpace,
  WebGLRenderer,
} from "three";
import { RoomEnvironment } from "three/examples/jsm/environments/RoomEnvironment.js";
import type { MaterialItem } from "@forest-rs/layerstack_stagehand";
import type { OpenPbrSurface } from "../model/openpbr.ts";
import { type AssetExists, type AssetReader, surfaceMaterial, TextureStore, use } from "./surfaces.ts";

/** What the preview draws: a `UsdPreviewSurface` record, or an OpenPBR surface. */
export type PreviewSource = { readonly kind: "previewSurface"; readonly item: MaterialItem } | { readonly kind: "openPbr"; readonly surface: OpenPbrSurface };

const NO_CHANNELS = new Map<string, number>();

/** A sphere in a canvas, drawing one material at a time. */
export class MaterialPreview {
  readonly #renderer: WebGLRenderer;
  readonly #scene = new Scene();
  readonly #camera = new PerspectiveCamera(30, 1, 0.1, 100);
  readonly #sphere: Mesh<SphereGeometry, MeshStandardMaterial | MeshPhysicalMaterial>;
  readonly #textures: TextureStore;
  readonly #resizer: ResizeObserver;
  readonly #canvas: HTMLCanvasElement;
  #source: PreviewSource | null = null;
  #frame = 0;

  /** `read` and `exists` reach the stage's assets, as the viewport's do. */
  constructor(canvas: HTMLCanvasElement, read: AssetReader, exists: AssetExists) {
    this.#canvas = canvas;
    this.#renderer = new WebGLRenderer({ canvas, antialias: true, alpha: true });
    this.#renderer.setPixelRatio(window.devicePixelRatio);
    this.#renderer.toneMapping = AgXToneMapping;
    this.#renderer.outputColorSpace = SRGBColorSpace;
    const pmrem = new PMREMGenerator(this.#renderer);
    this.#scene.environment = pmrem.fromScene(new RoomEnvironment(), 0.04).texture;
    this.#scene.environmentIntensity = 0.8;
    pmrem.dispose();
    const key = new DirectionalLight(0xffffff, 2.2);
    key.position.set(-3, 3, 4);
    this.#scene.add(key);
    const geometry = new SphereGeometry(1, 128, 64);
    const uv = geometry.getAttribute("uv");
    for (let index = 0; index < uv.count; index++) uv.setXY(index, uv.getX(index) * 4, uv.getY(index) * 2);
    this.#sphere = new Mesh(geometry, new MeshStandardMaterial({ color: new Color(0.5, 0.5, 0.5) }));
    this.#scene.add(this.#sphere);
    this.#camera.position.set(0, 0, 4.4);
    this.#textures = new TextureStore(read, exists, () => this.#rebuild());
    this.#resizer = new ResizeObserver(() => this.#draw());
    this.#resizer.observe(canvas);
  }

  /** Draws `source`, or the plain grey sphere with `null`. */
  show(source: PreviewSource | null): void {
    this.#source = source;
    this.#rebuild();
  }

  /** Turns the sphere by a drag's pixels. */
  turn(dx: number, dy: number): void {
    this.#sphere.rotation.y += dx * 0.01;
    this.#sphere.rotation.x = Math.max(-1.4, Math.min(1.4, this.#sphere.rotation.x + dy * 0.01));
    this.#draw();
  }

  dispose(): void {
    cancelAnimationFrame(this.#frame);
    this.#resizer.disconnect();
    disposeMaterial(this.#sphere.material);
    this.#sphere.geometry.dispose();
    this.#textures.dispose();
    this.#scene.environment?.dispose();
    this.#renderer.dispose();
    // Selecting another kind of prim unmounts the preview: give its context back now.
    this.#renderer.forceContextLoss();
  }

  #rebuild(): void {
    const old = this.#sphere.material;
    const source = this.#source;
    this.#sphere.material =
      source === null
        ? new MeshStandardMaterial({ color: new Color(0.5, 0.5, 0.5), roughness: 0.6 })
        : source.kind === "previewSurface"
          ? surfaceMaterial(source.item, this.#textures, { vertexColors: false, doubleSided: false, highlighted: false, uvChannels: NO_CHANNELS })
          : openPbrMaterial(source.surface, this.#textures);
    disposeMaterial(old);
    this.#draw();
  }

  #draw(): void {
    if (this.#frame !== 0) return;
    this.#frame = requestAnimationFrame(() => {
      this.#frame = 0;
      const width = this.#canvas.clientWidth;
      const height = this.#canvas.clientHeight;
      if (width === 0 || height === 0) return;
      if (this.#canvas.width !== Math.round(width * window.devicePixelRatio)) this.#renderer.setSize(width, height, false);
      this.#camera.aspect = width / height;
      this.#camera.updateProjectionMatrix();
      this.#renderer.render(this.#scene, this.#camera);
    });
  }
}

/** Frees a material and the textures it holds (each use is its own `Texture`; the images are the store's). */
function disposeMaterial(material: MeshStandardMaterial | MeshPhysicalMaterial): void {
  for (const value of Object.values(material)) {
    if (value !== null && typeof value === "object" && "isTexture" in value) (value as { dispose(): void }).dispose();
  }
  material.dispose();
}

/** An OpenPBR surface as `MeshPhysicalMaterial`, as the header of this file maps it. */
export function openPbrMaterial(surface: OpenPbrSurface, store: TextureStore): MeshPhysicalMaterial {
  const material = new MeshPhysicalMaterial();
  const map = (texture: OpenPbrSurface["baseColor"]["texture"], color: boolean) => (texture === null ? null : use(store, NO_CHANNELS, texture, color));
  const weight = surface.baseWeight.value;
  material.map = map(surface.baseColor.texture, true);
  if (material.map !== null) material.color.setScalar(weight);
  else material.color.setRGB(...surface.baseColor.value).multiplyScalar(weight);
  material.metalnessMap = map(surface.baseMetalness.texture, false);
  material.metalness = material.metalnessMap !== null ? 1 : surface.baseMetalness.value;
  material.roughnessMap = map(surface.specularRoughness.texture, false);
  material.roughness = material.roughnessMap !== null ? 1 : surface.specularRoughness.value;
  material.specularIntensity = surface.specularWeight.value;
  material.specularColorMap = map(surface.specularColor.texture, true);
  if (material.specularColorMap === null) material.specularColor.setRGB(...surface.specularColor.value);
  material.ior = Math.min(2.333, Math.max(1, surface.specularIor));
  material.anisotropy = surface.specularAnisotropy;
  material.clearcoatMap = map(surface.coatWeight.texture, false);
  material.clearcoat = material.clearcoatMap !== null ? 1 : surface.coatWeight.value;
  material.clearcoatRoughnessMap = map(surface.coatRoughness.texture, false);
  material.clearcoatRoughness = material.clearcoatRoughnessMap !== null ? 1 : surface.coatRoughness.value;
  material.sheen = surface.fuzzWeight;
  material.sheenColorMap = map(surface.fuzzColor.texture, true);
  if (material.sheenColorMap === null) material.sheenColor.setRGB(...surface.fuzzColor.value);
  material.sheenRoughness = surface.fuzzRoughness;
  material.emissiveMap = map(surface.emissionColor.texture, true);
  material.emissive.setRGB(...(material.emissiveMap !== null ? ([1, 1, 1] as const) : surface.emissionColor.value));
  material.emissiveIntensity = surface.emissionLuminance / 1000;
  material.transmissionMap = map(surface.transmissionWeight.texture, false);
  material.transmission = material.transmissionMap !== null ? 1 : surface.transmissionWeight.value;
  if (material.transmission > 0) {
    material.thickness = 1;
    material.attenuationColor.setRGB(...surface.transmissionColor);
    material.attenuationDistance = surface.transmissionDepth > 0 ? surface.transmissionDepth : Infinity;
  }
  material.alphaMap = map(surface.opacity.texture, false);
  if (material.alphaMap !== null || surface.opacity.value < 1) {
    material.transparent = true;
    material.opacity = material.alphaMap !== null ? 1 : surface.opacity.value;
    material.depthWrite = false;
  }
  material.iridescence = surface.thinFilmWeight;
  material.iridescenceIOR = surface.thinFilmIor;
  material.iridescenceThicknessRange = [surface.thinFilmThickness * 1000, surface.thinFilmThickness * 1000];
  const normal = surface.normal;
  if (normal !== null) {
    material.normalMap = use(store, NO_CHANNELS, normal.texture, false);
    material.normalScale.set(normal.scale * (normal.flip[0] ? -1 : 1), normal.scale * (normal.flip[1] ? -1 : 1));
  }
  return material;
}
