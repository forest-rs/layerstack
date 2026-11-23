# Layerstack CLI

Install with Rust 1.89 or later. The executable is named `layerstack`. From a checkout, use `cargo install --path layerstack_cli` in place of the former `--path layerstack_io`.

## Compare authored layers

Install the command from a workspace checkout and compare two files.

```sh
cargo install --path layerstack_cli --bin layerstack
layerstack diff before.usda after.usdc
layerstack diff --json before.usdc after.usdc
```

The command compares each layer's own opinions, including variant branches,
relationship target edits, connections and authored time samples. A sublayer
or reference is compared by its authored asset path. Changing the contents of that external file alone
leaves the referring layer equal.

For example, editing two point positions in a mesh produces one line.

```text
changed /Mesh.points default: 2 of 8192 elements changed at indices [17, 804]
```

Exit status is `0` for equal authored content, `1` for differences, or `2`
when the comparison could not be completed.

JSON output contains an `equal` boolean and a `changes` array. Each change
identifies its spec path, change kind and field (or a whole spec). Array edits
include the before and after lengths, the total count of differing positions and
the first eight differing indices. Rust callers can compare two byte slices
through `layerstack_io::diff::diff_layers`. Values in the report are bounded
previews. The comparison uses the full values.

## Compare composed stages

Use `--composed` when a change in a sublayer, reference, payload or selected
variant might affect the stage. Both roots open with the filesystem resolver.
For example, two copies of a root can contain identical references to
`./model.usda` while the models beside them have different animation.

```sh
layerstack diff --composed before/scene.usda after/scene.usda
layerstack explain --time 10 after/scene.usda /Assembly.size
```

The comparison takes a lossless flattened snapshot in memory of each stage,
expands instances to their stage paths and anchors asset-valued fields to their
source layers. It compares the resulting fields, target lists, defaults and
animation in stage time. External resource contents such as texture pixels are
outside the comparison. As with `explain`, the CLI uses authored declarations
and active variant selections. A changed sample identifies both the property
and the frame to query. Explain a removal against the before stage.

The exit codes and change records match the authored diff. Composed JSON
adds `"mode":"composed"`. When a dependency cannot be read or a stage cannot
be flattened losslessly, the command exits with status `2`. The JSON error
identifies the input side, stage, code and reason.

## Explain a composed property

Take a property path from a diff and query it against the root of the stage
that uses the layer. The source layer and authored spec path identify where to
make an edit, including when a reference maps a prim to a different path.

```sh
layerstack explain scene.usda /Model.weight
layerstack explain --time 5 scene.usda /Model.animated
layerstack explain --json scene.usda /Model.parts
```

An attribute report shows its resolved value and the roles of the consulted
opinions, strongest first. Authored connections appear separately. A relationship
report shows the final target paths and each layer's authored list edits.
Arrays and dictionaries have count previews. Numeric queries use stage time.
The command composes the stage's active variants and uses authored declarations.
Schema-only properties and schema fallback values require a registry and are
outside this command's scope.

A completed query exits with status `0`, including when an authored block
leaves a property without a value. An invalid query, missing property or
incomplete stage exits with status `2`. With `--json`, query results contain
`stage`, `property`, `time` and `fields`. A stage or lookup failure produces an
`error` object with a code and message.

## Trace a material

```sh
layerstack material scene.usda /World/Mesh
layerstack material --json --purpose preview --context mtlx scene.usda /World/Mesh
```

The command uses the USD and USD Shade schemas to resolve direct, inherited and
collection bindings. It reports the winning relationship, material target,
surface shader and each connection on the terminal path. The winning
relationship is a property path you can query with `explain`. Repeat
`--context` to try render contexts in order. An unbound prim yields an
`unbound` record. A broken target or cycle has its own diagnostic record.
