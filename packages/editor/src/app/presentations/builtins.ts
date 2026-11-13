// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's own presentations and commands, registered through the
// same calls a plugin makes (`registerPresentation`, `registerCommand`).
import { registerCompositionCommands } from "./composition.ts";
import { registerInstancePresentations } from "./instances.tsx";
import { registerLayerCommands } from "./layers.ts";
import { registerObjectPresentations } from "./objects.tsx";
import { registerPrimAuthoringPresentations } from "./primAuthoring.tsx";
import { registerPrimPresentations } from "./prims.tsx";
import { registerPropertyCommands } from "./properties.ts";
import { registerShadingCommands } from "./shading.ts";
import { registerValuePresentations } from "./values.tsx";

/** Registers every built-in presentation and command; called once, before the editor renders. */
export function registerBuiltInPresentations(): void {
  registerValuePresentations();
  registerObjectPresentations();
  registerLayerCommands();
  registerPropertyCommands();
  registerPrimPresentations();
  registerPrimAuthoringPresentations();
  registerInstancePresentations();
  registerShadingCommands();
  registerCompositionCommands();
}
