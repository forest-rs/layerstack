// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import {
  CircleIcon,
  CubeIcon,
  DashedLineCircleIcon,
  Folder01Icon,
  PaintBoardIcon,
  Sun03Icon,
  Video01Icon,
} from "@hugeicons/core-free-icons";

type Icon = typeof CubeIcon;

const GEOMETRY = new Set([
  "Mesh",
  "Cube",
  "Sphere",
  "Cylinder",
  "Cylinder_1",
  "Cone",
  "Capsule",
  "Capsule_1",
  "Plane",
  "BasisCurves",
  "NurbsCurves",
  "HermiteCurves",
  "NurbsPatch",
  "Points",
  "PointInstancer",
  "TetMesh",
  "GeomSubset",
]);

/** The icon of a prim of type `typeName`, by what it is: geometry, a light, a camera, shading, or a group. */
export function primIcon(typeName: string | null): Icon {
  if (typeName === null) return DashedLineCircleIcon;
  if (GEOMETRY.has(typeName)) return CubeIcon;
  if (typeName.endsWith("Light") || typeName === "LightFilter") return Sun03Icon;
  if (typeName === "Camera") return Video01Icon;
  if (typeName === "Material" || typeName === "Shader" || typeName === "NodeGraph") return PaintBoardIcon;
  if (typeName === "Xform" || typeName === "Scope" || typeName === "SkelRoot") return Folder01Icon;
  return CircleIcon;
}
