// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The built-in panel presentations of prims: the inspector's sections,
// each registered for the presentation type it is about, as a plugin's
// view would be. Priorities place them: 100 the prim's state, 90 its
// transform, 50 a view of what it is, 40 its draw mode, 30 its variants,
// 0 its properties, below 0 what follows them.
import { registerPresentation } from "../../presentations/registry.ts";
import { DrawModeSection } from "../DrawModeSection.tsx";
import { CompositionSection } from "../CompositionSection.tsx";
import { MaterialSection, State } from "../PrimSections.tsx";
import { PropertyList } from "../PropertyList.tsx";
import { ShadingSection } from "../ShadingSection.tsx";
import { TransformSection } from "../TransformSection.tsx";
import { VariantsSection } from "../VariantsSection.tsx";
import { CurvesView, PointsView } from "../views/CurvesView.tsx";
import { InstancerView } from "../views/InstancerView.tsx";
import { MeshView } from "../views/MeshView.tsx";
import { CameraView, LightView, SkeletonView, XformOpsView } from "../views/TypeViews.tsx";

/** Registers the built-in prim panels. */
export function registerPrimPresentations(): void {
  registerPresentation({
    id: "builtin.state",
    kind: "prim",
    type: "Prim",
    title: "State",
    priority: 100,
    panel: ({ object, context }) => <State store={context.store} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.transform",
    kind: "prim",
    type: "Xformable",
    title: "Transform",
    priority: 90,
    panel: ({ object, context }) => <TransformSection store={context.store} stage={context.stage} prim={object.prim} />,
  });
  for (const type of ["Material", "Shader"]) {
    registerPresentation({
      id: `builtin.shading.${type.toLowerCase()}`,
      kind: "prim",
      type,
      title: type,
      priority: 50,
      panel: ({ object, context }) => <ShadingSection store={context.store} stage={context.stage} prim={object.prim} />,
    });
  }
  registerPresentation({ id: "builtin.mesh", kind: "prim", type: "Mesh", title: "Mesh", priority: 50, panel: MeshView });
  registerPresentation({ id: "builtin.curves", kind: "prim", type: "Curves", title: "Curves", priority: 50, panel: CurvesView });
  registerPresentation({ id: "builtin.points", kind: "prim", type: "Points", title: "Points", priority: 50, panel: PointsView });
  registerPresentation({ id: "builtin.point-instancer", kind: "prim", type: "PointInstancer", title: "Point instancer", priority: 50, panel: InstancerView });
  registerPresentation({ id: "builtin.light", kind: "prim", type: "LightAPI", title: "Light", priority: 50, panel: LightView });
  registerPresentation({ id: "builtin.camera", kind: "prim", type: "Camera", title: "Camera", priority: 50, panel: CameraView });
  registerPresentation({ id: "builtin.skeleton", kind: "prim", type: "Skeleton", title: "Skeleton", priority: 50, panel: SkeletonView });
  registerPresentation({ id: "builtin.skel-animation", kind: "prim", type: "SkelAnimation", title: "Skeleton animation", priority: 50, panel: SkeletonView });
  registerPresentation({ id: "builtin.xform-ops", kind: "prim", type: "Xformable", title: "Transform ops", priority: -5, panel: XformOpsView });
  registerPresentation({
    id: "builtin.draw-mode",
    kind: "prim",
    type: "GeomModelAPI",
    title: "Draw mode",
    priority: 40,
    panel: ({ object, context }) => <DrawModeSection store={context.store} stage={context.stage} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.variants",
    kind: "prim",
    type: "Prim",
    title: "Variants",
    priority: 30,
    applies: ({ prim }) => prim.variantSets().length > 0,
    panel: ({ object, context }) => <VariantsSection store={context.store} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.properties",
    kind: "prim",
    type: "Prim",
    title: "Properties",
    priority: 0,
    panel: ({ object, context }) => <PropertyList store={context.store} stage={context.stage} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.bound-material",
    kind: "prim",
    type: "Prim",
    title: "Bound material",
    priority: -10,
    panel: ({ object, context }) => <MaterialSection stage={context.stage} prim={object.prim} />,
  });
  registerPresentation({
    id: "builtin.composition",
    kind: "prim",
    type: "Prim",
    title: "Composition",
    priority: -20,
    panel: ({ object, context }) => <CompositionSection store={context.store} prim={object.prim} />,
  });
}
