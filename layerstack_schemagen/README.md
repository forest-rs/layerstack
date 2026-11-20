# Schema generation

Generated views retain native schema identifiers and searchable C++ class and
accessor names. Native aliases are verified against registered types and matching
source headers. Getters return values (`Get*Attr().Get()`), while setters queue
authoring until transaction application (`Create*Attr().Set()`). Codeless schemas
get their native schema spelling without invented C++ declarations. Regeneration
preserves the aliases and their semantic notes.

Install the command with `cargo install layerstack_schemagen`, at the version
of `layerstack_schemas` your library builds on. `layerstack_schemagen --help`
lists its commands and flags, and `layerstack_schemagen compile --help` the
compiler's. Inside this repository, `cargo run -p layerstack_schemagen --` runs
the same command. Regenerating the bundled schemas (`--pxr`) writes this
repository's `layerstack_schemas`, so run it from a checkout; an installed
command compiles libraries and runs the generators that take `--out`.

Regenerate the bundled OpenUSD schemas with the matching wheel and source:

```sh
cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr --source <OpenUSD> --check
```

Property tables retain native schema metadata such as `allowedTokens`,
`colorSpace`, `displayGroup`, and dictionary fields. `customData` remains available
only during generation (for documentation and `apiName`); composition arcs,
children, clips, time samples, splines, and connection/target paths have no runtime
schema metadata fallback, following `UsdSchemaRegistry::IsDisallowedField`.
Structural type, variability, and default values use their dedicated table fields.
Unsupported metadata constructors fail generation rather than dropping a field.

Compile a downstream schema library, in usdGenSchema's `schema.usda` authoring
form, into the `generatedSchema.usda` and `plugInfo.json` OpenUSD's
`usdGenSchema` writes for it, so its schemas load in OpenUSD tools and in
layerstack (`layerstack::schema::read_generated_schema`):

```sh
layerstack_schemagen compile schema/schema.usda --out schema --openusd-source <OpenUSD>
```

Sublayers such as `@usdGeom/schema.usda@` resolve next to the layer naming them,
then through `--search <dir>`, `--openusd-source <checkout>` (its `pxr/usd`) and
`--plugin-root <dir>` (each `<plugin>/resources`, such as the usd-core wheel's
`pxr/pluginfo`, which ships the `schema.usda` files). An unresolved sublayer is an
error naming it. Each class is checked against usdGenSchema's rules, and a broken
rule is a structured error (`compile::Rule`). `plugInfo.json` is built on the one
already in `--out`, so edits outside the generated types survive, as they do with
usdGenSchema: a codeless plugin loaded from its own directory replaces the
template's `@PLUG_INFO_...@` placeholders once. `--check` writes nothing and fails
on a difference. `compile::compile` is the library counterpart, which also lists
each class's kind, base and application rules. No C++ or Python is generated.

`--rust <file.rs>` also writes the library as a Rust module (`--out` and
`--install` become optional): typed views, edit handles and token enums of its
schemas, of the shape `layerstack_schemas` gives OpenUSD's, which deref to
OpenUSD's views (`Xformable`) and are named for each property's `apiName`, and a
`register` function adding the schemas to a `SchemaRegistryBuilder` from static
tables. `CompiledSchema::rust_module` is the build-script counterpart; see the
`compile` module docs for the Cargo and `build.rs` setup.

```sh
layerstack_schemagen compile schema/schema.usda --openusd-source <OpenUSD> --rust src/schemas.rs
```

Generate a downstream shader module without an OpenUSD installation:

```sh
cargo run -p layerstack_schemagen -- --shader-defs shaders/shaderDefs.usda --out src/nodes.rs
```

`--check` verifies the existing output without writing. Relative sublayers and
references compose before ports are collected, including inherited inputs.
Missing assets, composition errors, unsupported types and Rust name collisions
fail generation. Shader libraries declare `def Shader` prims with `info:id`.

Build scripts can call `generate_shader_library`, write its result into
`OUT_DIR`, and expose it with a `#[path = "..."] pub mod nodes` declaration.
Generated modules need `layerstack` and `layerstack_schemas` with `usd-shade`;
they use `no_std` plus `alloc`, and no runtime discovery or generator dependency.
Build-script library generation emits Rust directly without invoking `rustfmt`.
Format generated modules before checking them into source control.

Inputs expose composed values separately from definition defaults. Typed port
creation, setters and sample setters use the caller's `SchemaEdit`, including
its edit target and undo transaction. They do not evaluate shaders or connections.

Selected MaterialX interfaces can use the same generator:

```sh
cargo run -p layerstack_schemagen -- --materialx libraries/nodes.mtlx --node ND_my_node --out src/nodes.rs
```

Repeat `--node` to select more NodeDefs. `generate_materialx_library` is the
build-script counterpart. Generation needs `python3` with its standard XML
reader. The CLI uses `rustfmt` so `--check` can verify a checked-in module.
Regenerate existing CLI output once before using `--check` with this version.
Local whole-file XIncludes and NodeDef inheritance are resolved. Supported port
types include numeric scalars, vectors, colors, matrices, booleans, strings,
filenames and `surfaceshader`. Unsupported types, invalid defaults, missing
definitions, include or inheritance cycles, incompatible redeclarations and
generated Rust name collisions fail generation.

Each typed node has a `DEFINITION` constant with the MaterialX name, direction,
type and default of every port, alongside its USD property name and type.
The module's `NODE_DEFINITIONS` lists every selected interface. A
sole `surfaceshader` output named `out` becomes `token outputs:surface` for
compatibility with the material terminal authored by OpenUSD's `usdMtlx`
reader. Other output names are retained. Inputs with literal defaults have
typed default accessors. The descriptor preserves `defaultgeomprop` and
`defaultinput` references, along with any output fallback literal. Regenerated
modules use the `layerstack_schemas::materialx` descriptor types, available with
the existing `usd-shade` feature.

This mode generates authoring interfaces. Rendering the authored shaders
requires a renderer that recognizes the selected NodeDefs. OSL parameter
introspection uses a separate adapter.
