# layerstack_schemas

Every schema domain OpenUSD ships (`usd`, `usdGeom`, `usdShade`, `usdLux`,
`usdSkel`, `usdPhysics`, `usdVol`, `usdRender`, `usdMtlx` and the rest) as
`layerstack` schema definitions, generated from OpenUSD's own `generatedSchema.usda` and
`plugInfo.json` files by `layerstack_schemagen`, so nothing is parsed at run
time, and as typed views over a composed stage: `usd_geom::Mesh`,
`usd_lux::SphereLight`, `usd::CollectionApi` and every other schema, with a
getter per property (its fallback applied), enums for `allowedTokens`, and
edit handles whose setters author through a `SchemaEdit` transaction.

```rust
let mut tokens = layerstack::TokenInterner::default();
let schemas = layerstack_schemas::openusd(&mut tokens);
```

Each domain is a Cargo feature (`usd-geom`, `usd-lux`, …); `all`, the
default, enables every one.

The `usd-shade` feature also resolves composed connections and material surface,
displacement and volume sources through node graphs and ordered render contexts.
Results retain the selected endpoint, other candidates, diagnostics and dependencies.

The `usd-geom` feature provides caller-owned transform and bounds caches.
`bounds::BoundsCache` computes oriented local/world bounds from authored extents
and model extent hints, with purpose and visibility filtering. Invalidate after
edits to evict affected descendants and ancestors. Procedural extent providers
and point-instancer bounds return explicit unsupported errors.

To regenerate after an OpenUSD upgrade, install the matching usd-core wheel,
check out OpenUSD's sources at the same release (for each property's
`apiName`, which names the accessors, and for `usdMtlx`, which the wheel is
built without), and run:

```sh
cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr --source <OpenUSD>
```

`--check` fails instead when the checked-in files are stale.

## License

The generated tables and views in `src/generated` derive from OpenUSD's schema
definitions and are under the Tomorrow Open Source Technology License 1.0
(`LICENSE-TOST-1.0`, `NOTICE`). The rest of the crate is under Apache-2.0 OR
MIT (`LICENSE-APACHE`, `LICENSE-MIT`).
