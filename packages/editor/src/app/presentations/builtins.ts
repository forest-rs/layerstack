// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The editor's own presentations and commands, registered through the
// same calls a plugin makes (`registerPresentation`, `registerCommand`).
import { registerObjectPresentations } from "./objects.tsx";
import { registerPrimPresentations } from "./prims.tsx";
import { registerValuePresentations } from "./values.tsx";

/** Registers every built-in presentation and command; called once, before the editor renders. */
export function registerBuiltInPresentations(): void {
  registerValuePresentations();
  registerObjectPresentations();
  registerPrimPresentations();
}
