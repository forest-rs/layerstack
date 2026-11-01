// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { loadLayerstack } from "@forest-rs/layerstack";
import { loadStagehand } from "@forest-rs/layerstack_stagehand";
import { App } from "./app/App.tsx";
import { EditorContext } from "./app/context.ts";
import { initialSource } from "./app/scenes.ts";
import { EditorStore } from "./store.ts";
import "./index.css";

const [layerstack, stagehand] = await Promise.all([loadLayerstack(), loadStagehand()]);
const store = new EditorStore(layerstack, stagehand);
void store.open(await initialSource());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <EditorContext value={store}>
      <App />
    </EditorContext>
  </StrictMode>,
);
