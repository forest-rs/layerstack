# Schema generation

Regenerate the bundled OpenUSD schemas with the matching wheel and source:

```sh
cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr --source <OpenUSD> --check
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
