// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

import { createContext, use } from "react";
import { type EditorStore, useEditor } from "../store.ts";

/** The editor every panel reads and acts through. */
export const EditorContext = createContext<EditorStore | null>(null);

/** The editor, re-rendering the caller after each change it reports. */
export function useStore(): EditorStore {
  const store = use(EditorContext);
  if (store === null) throw new Error("useStore outside EditorContext");
  useEditor(store);
  return store;
}
