# Schema generation

Generated views retain native schema identifiers and searchable C++ class and
accessor names. Native aliases are verified against registered types and matching
source headers. Getters return values (`Get*Attr().Get()`), while setters queue
authoring until transaction application (`Create*Attr().Set()`). Codeless schemas
get their native schema spelling without invented C++ declarations. Regeneration
preserves the aliases and their semantic notes.

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
build-script counterpart. It needs `python3` with its standard XML reader only
during generation. Local whole-file XIncludes and NodeDef inheritance are
resolved; inherited defaults and port types are retained. Numeric, boolean,
string and filename ports are supported. Unsupported types, nonliteral defaults,
missing definitions, include/inheritance cycles and incompatible redeclarations
fail generation. Unselected definitions do not expand the generated library.

This is interface generation, not MaterialX graph import/export, shader
compilation, implementation selection or rendering. `MaterialXConfigAPI` alone
does not supply those features. Generated identifiers require a renderer that
recognizes the selected NodeDefs. OSL parameter introspection remains a separate
adapter; no OSL toolchain is required by this generator.
