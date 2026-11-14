// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// The commands on a shading port's path, wherever one is presented (a
// connection's source in the inspector, a row of the explain or changes
// panel): show it in the Shading tab's graph, and disconnect what it
// reads, one action where edits go.
import type { ShaderPort, Stage } from "@forest-rs/layerstack";
import { primOf } from "../../model/network.ts";
import { registerCommand } from "../../presentations/registry.ts";

/** The shading port at `path`, or `null` (also for a path the stage cannot read). */
function portAt(stage: Stage, path: string): ShaderPort | null {
  try {
    return stage.port(path);
  } catch {
    return null;
  }
}

/** The sources `port`'s `.connect` names: those it reads, and those that name nothing. */
function authoredSources(port: ShaderPort | null): string[] {
  return port === null ? [] : [...port.connections, ...port.invalidConnections];
}

/** Registers the commands on shading ports' paths. */
export function registerShadingCommands(): void {
  registerCommand({
    id: "builtin.shading-port.graph",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Show in the shading graph",
    applies: (object, { stage }) => portAt(stage, object.path) !== null,
    run: (object, { store }) => {
      store.select(primOf(object.path));
      store.explain(object.path, false);
      store.showBottomTab("shading");
    },
  });
  registerCommand({
    id: "builtin.shading-port.disconnect",
    kind: "propertyPath",
    type: "propertyPath",
    title: "Disconnect what it reads",
    applies: (object, { stage }) => authoredSources(portAt(stage, object.path)).length > 0,
    run: (object, { store, stage }) => {
      const connections = authoredSources(portAt(stage, object.path));
      // One source is removed from the list; several are all cleared.
      store.disconnect(object.path, connections.length === 1 ? connections[0]! : null);
    },
  });
}
