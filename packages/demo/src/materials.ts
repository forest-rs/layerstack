// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// A prim's bound material as the inspector shows it: the binding that
// decides it, and its surface shader's inputs with their values or
// connections.
import type { Stage } from "@forest-rs/layerstack";
import { formatValue, releaseValue } from "./values.ts";

/** One input row: its name, type, and value (or `← source` when connected). */
export interface InputRow {
  readonly name: string;
  readonly type: string;
  readonly value: string;
}

/** What the inspector's material section shows for a prim. */
export interface MaterialSummary {
  /** The bound material's path, or the binding's target when no material is there. */
  readonly material: string;
  /** Where the binding comes from: `direct from /World/Robot.material:binding`. */
  readonly binding: string;
  /** The surface shader's path, or `null` when the material has none. */
  readonly shader: string | null;
  readonly inputs: readonly InputRow[];
}

/** The material bound to the prim at `path` (all purposes), read at `time`; `null` when nothing binds it. */
export function materialSummary(stage: Stage, path: string, time?: number): MaterialSummary | null {
  const binding = stage.prim(path)?.boundMaterial() ?? null;
  if (binding === null) return null;
  const strength = binding.strength === "strongerThanDescendants" ? ", stronger than descendants" : "";
  const summary = {
    material: binding.material ?? `${binding.target} (no material there)`,
    binding: `${binding.kind} from ${binding.relationship}${strength}`,
  };
  const material = binding.material === null ? null : stage.material(binding.material);
  const shader = material?.surface().shader ?? null;
  if (shader === null) return { ...summary, shader: null, inputs: [] };
  const inputs = (stage.shader(shader)?.inputs() ?? []).map((input) => {
    if (input.connections.length > 0) {
      return { name: input.name, type: input.typeName ?? "", value: `← ${input.connections.join(", ")}` };
    }
    const value = input.get(time);
    const shown = formatValue(value);
    releaseValue(value);
    return { name: input.name, type: input.typeName ?? "", value: shown };
  });
  return { ...summary, shader, inputs };
}
