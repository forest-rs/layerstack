# layerstack_schemas

OpenUSD's schemas (`usd`, `usdGeom`, `usdShade`, `usdLux`) as `layerstack`
schema definitions, generated from OpenUSD's own `generatedSchema.usda` and
`plugInfo.json` files by `layerstack_schemagen`, so nothing is parsed at run
time.

```rust
let mut tokens = layerstack::TokenInterner::default();
let schemas = layerstack_schemas::openusd(&mut tokens);
```

To regenerate after an OpenUSD upgrade, install the matching usd-core wheel
and run:

```sh
cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr
```

`--check` fails instead when the checked-in tables are stale.

## License

The generated tables in `src/generated` derive from OpenUSD's schema
definitions and are under the Tomorrow Open Source Technology License 1.0
(`LICENSE-TOST-1.0`, `NOTICE`). The rest of the crate is under Apache-2.0 OR
MIT.
