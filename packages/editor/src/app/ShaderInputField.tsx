// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A shading input's value editor, in the inspector and on the shading
// view's rows alike: its value presented in the `accept` view (a colour a
// swatch and picker, a number a field, a token its definition restricts a
// menu), read from its attribute or, for an input only its shader's
// definition has, the definition's fallback. Every edit authors through
// `EditorStore.setShaderInput`, which creates the input on its first edit
// (`UsdShadeConnectableAPI::CreateInput`).
import type { DefinedInput } from "../model/shaderDefinitions.ts";
import { inputBounds, inputEditTime, withinBounds } from "../model/shadingInputs.ts";
import { shown } from "../model/values.ts";
import type { EditorStore } from "../store.ts";
import { Present, presentationContext } from "./presentations/Present.tsx";

/**
 * The editor of the input at `path` named `name`, at the time `time`
 * reads (`at`, for an animated input); an edit authors its default, or,
 * for an input with samples, a sample at the time when it is committed
 * (`inputEditTime`). `definition` is its shader definition's, when known.
 */
export function ShaderInputField({
  store,
  path,
  name,
  definition,
  time,
  at,
}: {
  store: EditorStore;
  path: string;
  name: string;
  definition: DefinedInput | null;
  time: number | undefined;
  at: number | null;
}) {
  const context = presentationContext(store);
  if (context === null) return null;
  const attribute = context.stage.attribute(path);
  // An input with samples is keyed at the time now, though one sample alone does not make its row an animated one.
  const keyed = inputEditTime(context.stage, path, store.time) !== null;
  const read = attribute?.get(time ?? (keyed ? store.time : undefined)) ?? null;
  // An input with no value (or none authored) shows what a renderer reads: the definition's fallback.
  const value = read === null && definition !== null ? shown(definition.fallback) : shown(read);
  // A number typed here is held to the input's hard bounds (`uimin`, `uimax`), as the inspector's slider holds it.
  const port = attribute === null ? null : context.stage.port(path);
  const typeName = attribute?.typeName ?? definition?.typeName ?? value.value?.type ?? "";
  return (
    <Present
      object={{ kind: "value", attribute, typeName, shown: value, at: at ?? (keyed ? (store.time ?? null) : null), label: name, allowedTokens: definition?.allowedTokens ?? null }}
      context={context}
      accept={(next) => store.setShaderInput(path, port === null ? next : withinBounds(next, inputBounds(port.sdrMetadata)), inputEditTime(context.stage, path, store.time))}
    />
  );
}
