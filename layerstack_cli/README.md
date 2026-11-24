# Layerstack CLI

Install with Rust 1.89 or later. The executable is named `layerstack`. From a checkout, use `cargo install --path layerstack_cli` in place of the former `--path layerstack_io`.

## Compare authored layers

Install the command from a workspace checkout and compare two files.

```sh
cargo install --path layerstack_cli --bin layerstack
layerstack diff before.usda after.usdc
layerstack diff --json before.usdc after.usdc
layerstack diff --stat before.usda after.usda
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

With `--stat`, the command prints one line with counts of added, removed and
changed records and distinct paths. A single path can have several field or
sample changes. Use `--stat --json` for the counts as numbers. Full detail and
summary use the same comparison and exit codes in both modes. The summary counts
records as it compares and skips value previews. Use `layerstack diff --quiet
BEFORE AFTER` in scripts that only need the exit status. It validates both
inputs, stops comparing at the first difference and writes diagnostics to stderr
if the comparison fails.

By default, the CLI can decode the two authored layers concurrently when each
input is at least 256 KiB. Pass `--jobs 1` to reduce peak memory by decoding
one layer at a time. Byte-identical files still go through validation once.

JSON output contains an `equal` boolean and a `changes` array. Each change
identifies its spec path, change kind and field (or a whole spec). Array edits
include the before and after lengths, the total count of differing positions and
the first eight differing indices. Rust callers can compare two byte slices
through `layerstack_io::diff::diff_layers`. For repeated comparisons,
`layerstack_io::diff::ComparableLayer::read` prepares each input once and
supports detailed changes, counts, or an equality check. Values in the report are bounded
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

## Find asset dependencies

```sh
layerstack deps scene.usda
layerstack deps --json scene.usdz
```

Each record names the authored asset, role, source layer and spec. A field is
included where the layer assigns one. Discovery visits all variant branches in
the layers it can load. Loaded layers report their identifiers. Local resources
are checked for existence and a UDIM pattern produces one row per tile.
Missing files stay in the report so you can locate the broken opinion.
Identity metadata is labeled separately. Resolver expressions, clip templates
and opaque package resources can have `unchecked` status. Exit status 0 means
the inventory ran, including when it found missing uses.

## Browse scene paths

```sh
layerstack inspect scene.usda
layerstack inspect --json scene.usda /World
layerstack inspect --limit 100 scene.usda /World/Mesh
```

Start at the root to see its default prim and children, then pass a child path
to inspect that prim. Each listing includes the total direct children and
authored properties. Up to 25 of each are shown by default. Use a property
path with `explain` to query its value or targets.

## Check a stage

```sh
layerstack check scene.usda
layerstack check scene.usda --json
```

The check reports missing assets, composition errors and inconsistent mesh face
counts or vertex indices. Asset discovery uses the same filesystem and UDIM
checks as `deps` and visits all authored variant branches in the layers it can
load. Resolver expressions, clip templates and opaque package resources can
yield `unchecked` records. Asset existence checks leave contents such as
texture pixels unread.

Mesh validation uses the stage's active composition at USD default time. It
visits composed prims in ordinary stage traversal, which skips inactive
branches and instance proxy descendants. Numeric samples, interpolation and
clip evaluation are outside mesh coverage. A topology finding gives the
composed prim path and the strongest contributing authored site for each
implicated input when available. An authored block can also be the source.

Exit status is `0` when the checks ran and found no errors, even if some
asset uses were unchecked. Status `1` means the check found errors. Status
`2` means the invocation was invalid or the check was incomplete, for
example because a layer failed to parse.

JSON output uses the `records` array shared with `deps`. Its first record
has kind `check` and gives coverage and counts. Findings carry a `status`
of `missing`, `error`, `failed` (incomplete), or `unchecked`. Asset findings
have the same kinds and authored fields as `deps` where available. Composition
and topology findings have kinds `composition` and `meshTopology`.
An invocation or stage-open failure produces an `error` object with a code
and message.
