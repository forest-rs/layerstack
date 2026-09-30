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

[API documentation](https://docs.rs/layerstack_schemas) ·
[Source](https://github.com/forest-rs/layerstack/tree/main/layerstack_schemas)

Requires Rust **1.88** or later and `no_std + alloc`. Add
`layerstack_schemas = "0.1"` to your dependencies. To select individual domains,
disable default features and enable the domain features you need. The optional
`std` feature currently adds no behavior.

Each domain is a Cargo feature (`usd-geom`, `usd-lux`, …); `all`, the
default, enables every one.

The `usd-shade` feature also resolves composed connections and material surface,
displacement and volume sources through node graphs and ordered render contexts.
Results retain the selected endpoint, other candidates, diagnostics and dependencies.

The `usd-geom` feature provides caller-owned transform and bounds caches:

- `XformCache` shares ancestor work through parent slots, validates edited
  transforms lazily, and retains static locals across animation frames.
  `relative_transform` walks local operations toward an ancestor, stopping at
  and reporting a reset.
- `bounds::BoundsCache` computes oriented world, local, untransformed and
  relative bounds from authored or computed extents and model `extentsHint`, with purpose
  and visibility filtering. Relative bounds convert coordinate frames even
  across resets. Time changes retain static bounds and reevaluate temporal
  dependencies on demand, including sampled visibility of excluded children.
- `Xformable::transform_time_samples` returns composed sample times in stage
  time. `transform_might_be_time_varying` reports numeric-time variability,
  including splines; a single sample can still differ from the default value.
- `XformableEdit::set_common_transform` and `set_common_transform_at` author
  `CommonTransform` translation, pivot, Euler rotation and scale values. They
  complete compatible partial stacks, preserve resets and existing precision,
  and reject incompatible stacks before authoring anything.

Pass every successful `LiveStage` change report to each cache's `apply_changes`,
or explicitly invalidate after manual edits. Caches do not observe edits
implicitly; clear them before using an unrelated scene. Bounds source edits
evict affected descendants and dirty reduction ancestors. Wide static parents
lazily retain reductions after their first edit; subsequent leaf edits update
only changed contributions and their reduction paths. Small or animated parents
use the ordinary child fold, and structural changes rebuild affected reductions. Cache stats expose
computed, reused and invalidated work.

When no valid extent is authored, bounds are computed from mesh points or
cube, sphere, cylinder, cone and capsule parameters. Other procedural extent
providers and point-instancer bounds remain unsupported and return explicit
errors. Common transform helpers do not decompose arbitrary
matrices or rewrite incompatible op stacks. Prepared operation recipes remain
an internal cache implementation detail.

To regenerate after an OpenUSD upgrade, install the matching usd-core wheel,
check out OpenUSD's sources at the same release (for each property's
`apiName`, which names the accessors, and for `usdMtlx`, which the wheel is
built without), and run:

```sh
cargo run -p layerstack_schemagen -- --pxr <site-packages>/pxr --source <OpenUSD>
```

`--check` fails instead when the checked-in files are stale.

### Shading values and port authoring

`Scene::value_sources` follows connections through material and node-graph
interfaces to shader outputs **or authored values**. It preserves branch traces,
cycles and invalid targets. A local authored value is used only when none of
that attribute's connections produces a source. `shader_sources` remains the
shader-only query used for material terminals.

Views expose `input(name)`, `output(name)` and `ports(kind)`. A `shading::Port`
can read its own value at a time or trace its providers; tracing does not evaluate
shaders. Edit handles expose `create_input`/`create_output`, returning `PortEdit`
for values and explicit connection replacement. `disconnect_sources` blocks
weaker connections with an empty list; `clear_sources` removes the local opinion.
Both use the existing mapped transaction and undo path.

This slice requires source ports to exist (including ports created earlier in
the edit). It does not implement `CanConnect`, source-port auto-creation,
individual list insertion/deletion helpers, Sdr, or connectability plugins.
Those policies are separate from composed connection inspection.

Run `cargo run -p layerstack_examples --bin shading_values` for a complete
interface-input authoring and value-source inspection example.

Retained computed queries are available in `retained` with `usd-geom` and
`usd-shade`. Caller-owned `RetainedQueries` observe a host-owned `LiveStage`, retaining world
transforms, world bounds and shading providers through edits and time changes.
Stage callbacks are independently usable; `QuerySession` optionally bundles scene
ownership and queries. Explicit polling
reports answer, dependency and provenance changes separately, with bounded causes
and computation counters. See [the model and limits](https://github.com/forest-rs/layerstack/blob/main/docs/retained-queries.md)
and the `retained_queries` example in `layerstack_examples`.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

The generated tables and views in `src/generated` derive from OpenUSD's schema
definitions and are under the Tomorrow Open Source Technology License 1.0
(`LICENSE-TOST-1.0`, `NOTICE`). The rest of the crate is under Apache-2.0 OR
MIT (`LICENSE-APACHE`, `LICENSE-MIT`).
