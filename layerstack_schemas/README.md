# layerstack_schemas

Every schema domain OpenUSD ships (`usd`, `usdGeom`, `usdShade`, `usdLux`,
`usdSkel`, `usdPhysics`, `usdVol`, `usdRender`, `usdMtlx` and the rest) as
`layerstack` schema definitions, generated from OpenUSD's own `generatedSchema.usda` and
`plugInfo.json` files by `layerstack_schemagen`, so nothing is parsed at run
time, and as typed views over a composed stage: `usd_geom::Mesh`,
`usd_lux::SphereLight`, `usd::CollectionApi` and every other schema, with a
getter per property (its fallback applied), enums for `allowedTokens`, and
edit handles whose setters author through a `SchemaEdit` transaction.

Search rustdoc with C++ names such as `UsdGeomMesh`,
`UsdGeomMesh::GetFaceVertexCountsAttr`, `UsdGeomXformCache` or
`UsdGeomPrimvar::ComputeFlattened`. Generated views, getters and setters retain
verified native aliases; core stage APIs and computed helpers have corresponding
aliases too. Read the nearby semantic notes: typed getters return resolved values
rather than C++ property handles, authoring is transactional, and cache controls
remain explicit.

## Runtime API schemas

`SchemaEdit::apply_api(path, "PipelineTintAPI", None)` applies a registered API
by name. Pass `Some("main")` for a multiple-apply instance. It uses the same
applicability checks as generated API types and collects an ordinary transaction.
A Rust type is unnecessary. The host loads the codeless schema library and its
plugin declarations into the stage's registry, using the store's interners;
`layerstack::schema::read_generated_schema` reads runtime schema layers.
Run `cargo run -p layerstack_examples --bin codeless_api` for application,
fallback reads and undo without generated getters.

## Numeric buffer ownership

Matching numeric array getters, including `Mesh::points`, return
`Option<Arc<Vec<T>>>`. A dense native read clones the shared owner in O(1);
it does not copy the elements. `as_slice()` borrows that storage.
`as_ref().clone()` explicitly materializes an independently mutable vector;
`Arc::make_mut` copies only when another owner retains the buffer.
Half values exposed as `f32` and matrices exposed as nested rows still require
representation conversion and return vectors. Text arrays retain their previous types.

| Operation | Element allocation/copy cost |
| --- | --- |
| Matching numeric getter / `value::read_*_array_shared` | Shared-owner increment; no element allocation or copy |
| `value::borrow_*_array` | Borrow only; returns `None` for legacy storage |
| Existing slice setter / `value::write_*_array` | Allocates and copies all elements |
| `_owned` setter | Transfers the vector allocation and capacity; allocates a shared-owner header |
| `_shared` setter | Transfers an `Arc<Vec<T>>`; no element allocation or copy |
| `value::read_*_array` | Explicit vector materialization; copies native elements |

The same setters have `_at` forms for time samples. `Primvar::indices` retains
shared storage, and `PrimvarEdit::set_indices_owned` / `set_indices_shared`
transfer their inputs. `PrimvarEdit::set(Value::TypedArray(...))` already retains
the shared value buffer.

For an immediate borrowed operation, use `Stage::read_property` with
`value::borrow_float3_array` inside its callback. A callback's borrowed slice
cannot escape the source's lifetime. A typed getter's shared owner can outlive
the stage, layer and store.

Legacy boxed arrays convert to native buffers. Sparse array composition and
numeric interpolation can allocate a composed result; the getter retains that
result without copying it again. Deferred native storage may decode on first
access, using retained memory without file I/O. There is no promise of sharing
between independent sparse/interpolated evaluations. Prefer generated checked
array getters such as `mesh.try_points(Time::Default)` when loading geometry:
`Ok(None)` means missing, blocked or incompatible data; `Err` preserves a deferred
decode failure. `PrimView::try_read_value` and `value::try_read_*_array_shared`
provide the same distinction for generic typed reads and native conversions.
Ordinary `Option` getters report no value on decoding failure. Raw numeric
storage exposes `TypedArray::try_materialize`.

Generated shader-node numeric array defaults also return shared owners.

## Retained scene and material handoffs

With `usd-geom` and `usd-shade`, `scene_records::SceneObserver::update` captures
checked polygon meshes, native occurrences, point-instance inputs, inherited
visibility/purpose, indexed primvars and material assignments. Initial records
and later component deltas use observer-local incarnation handles. Unchanged
buffers retain their shared owners; deleting and recreating a path retires its
old handle. Lost notice history reports a reset and reconstructs the inventory.
Failed capture preserves the last successful records. Polling and time selection
are explicit; triangulation and subdivision evaluation belong to the geometry
consumer.
Source-domain changes are rejected before interpreting IDs. To accept a new
domain after a prepared document reload, explicitly discard the consumer's
inventory and call `observer.clear()` before its next update. Old handles remain
retired and previously captured shared data stays owned by its holders.

`shading::MaterialNetworkCache::get` retains an immutable upstream network for a
selected material terminal and ordered render contexts. Typed ports, forwarding,
constant origins, decode failures and asset authoring anchors remain inspectable.
Separate topology, parameter and authored-resource revisions describe the USD
handoff; external texture bytes require the host's own content revisions.
`sample.preview_surface(max_depth)` projects constants, UV textures, standard
primvar readers and 2D UV transforms into typed inputs. Unknown nodes, cycles and
unsupported combinations remain explicit diagnostics. It does not compile
shaders, decode images or prescribe a GPU layout.

Run `cargo run -p layerstack_examples --bin live_scene` for the combined scene
and material workflow, or `--bin material_network` for the shading-only handoff.

## Evaluating `UsdProc` recipes

`UsdProc` describes a procedural recipe, not an execution engine. A
`GenerativeProcedural` prim names its `proceduralSystem` and supplies inputs in
the `primvars:` namespace. Opening or composing a stage does not evaluate it.
OpenUSD's additional execution machinery lives in Hydra's `HdGp`: plugin
registration, dependency updates and generated child prims in a Hydra scene
index. Those children do not automatically become authored USD layer content.

With the `usd-proc` feature, `procedural::Procedural<E>` binds one recipe to an
application-supplied `ProceduralEvaluator`. `ProceduralInputs` reads composed
attribute values and forwarded relationship targets, including schema fallbacks,
time samples and layer offsets. It records missing inputs too. Reads retain
native array owners; deferred decoding and interpolation can materialize storage.
Wrong property kinds and decoding failures are explicit errors. The system token
must match the evaluator, including when supplied by an applied schema fallback.

```rust,ignore
let mut recipe = procedural::Procedural::new(&store, recipe_path, application_evaluator);
let evaluated = recipe.snapshot(&Scene::new(live.stage(), &store), Time::Default)?;
let publication = evaluated.output().geometry.prepare(
    live.stage(), &mut store, &edit_target, mesh_path, &owned_properties,
)?;
let applied = evaluated.evidence().apply(&mut live, &mut store, &publication.transaction)?;
bounds.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
owned_properties = publication.properties;
```

Evaluation and publication are separate. Outputs are generic: the example's
evaluator returns a mesh and a texture asset reference; an application can return
model descriptions, material descriptions or image-generation results instead.
The helper owns input reads and one retained result. The host owns generator
selection, invocation order, files, output ownership and publication. No mutable
stage is supplied to the evaluator, and evaluation must have no publication side
effects. If evaluation or validation fails, the example preserves its previous
published asset. Deleting a recipe likewise requires an explicit host policy for
retaining or removing that output.

Every request checks retained schema and input query identities. Unchanged
requests and unrelated edits on other prims skip composed input resolution.
Queries are conservative at prim granularity: another opinion on a consumed prim
can require revalidation. Relationship queries retain forwarding dependencies,
including missing property targets. Changed records with equal composed values
reuse the result without invoking the evaluator; callers need not deliver change
notices. Successful evaluation replaces dynamic dependencies. Native owner
equality avoids element comparison; distinct owners can require O(elements)
comparison. `dependencies()` exposes consumed inputs; `work()` counts evaluations,
result reuse, composed reads and query reuse. Timing belongs to the host.

`snapshot()` detaches a shared output and `EvaluationEvidence` for delayed work.
Its `apply()` synchronizes source edits, verifies consumed values at the captured
time and applies the transaction while retaining mutable access to the stage and
store. Stale work authors nothing. The mesh transaction's target-layer generation
guard separately rejects intervening output-site edits. `verify()` alone checks
only the supplied scene snapshot; another edit requires another verification.
Construction captures affinity of the token and path domains. Moving the store
preserves it; replacing either interner or supplying a foreign stage/store is
rejected before path lookup. This affinity is process-local, not a file identity.

`procedural::graph::ProceduralGraph` adds an explicit output-owner registry above
single-recipe evaluation. Requesting an output builds its declared prerequisites
in dependency order and checks host resource revisions before starting. Cached
evaluation still prepares publication so removed or edited output sites can be
repaired. Detached build steps retain prerequisite USD-input/epoch evidence and
transitive resource revisions, rejecting stale delayed publication before an
upstream output has been rebuilt. A failed downstream producer reports already
published prerequisites; publication is atomic per producer, rather than across
the whole graph. Output manifests, resource snapshots and transactions remain
host-owned. Run `cargo run -p layerstack_examples --bin procedural_graph`;
an optional output directory writes the authored world that CI reopens with C++
OpenUSD.

The binding retains one shared output and its query records. Detached results
extend those lifetimes and can pin other opinions on consumed prims; applications
budget pending work. `GeneratedMesh::into_validated()` lets an evaluator return
an immutable `ValidatedMesh`, so repeated publication skips geometry validation
and extent derivation too. The runnable fixture uses this path for both producers.

This adapter remains in `layerstack_schemas`: its responsibility is composed
schema input access and retained evaluation state. A future execution runtime
with its own plugins, scheduling or resource management would have a separate
responsibility and can consume this contract.

Retain the helper within one store. Changing evaluation time conservatively
reevaluates; it does not author a time sample. Use `evaluator_mut()` to change
configuration, or `invalidate()` after generator code or external resources
change. Asset paths are tracked as values; replacing image bytes at an unchanged
path is not a USD value change. The helper does not track file contents,
attribute connections, metadata or arbitrary child traversal, discover a producer
graph, schedule work, load C++ plugins, or implement Hydra's runtime. Evaluators
must use tracked reads and explicitly invalidate for other dependencies. Hosts
must publish delayed results through current input evidence. Explicit invalidation
also retires detached evidence. Applications own external-resource concurrency;
USD evidence does not establish file contents or freshness at another frame.

The runnable example authors terrain and asset recipes under `/Recipes`, then
evaluates upstream terrain before the asset. The asset reads terrain points
through `primvars:source`, shares unchanged topology/UV/normal buffers and updates
its authored source and material's texture reference atomically. Native and point
instances consume that source under `/World`. Editing `primvars:height` changes
geometry only after explicit evaluation and publication. The material includes
`UsdPreviewSurface`, `UsdUVTexture` and a UV reader; the example publishes texture
references, not generated image files or a Substance implementation. Supply the
referenced images when rendering it.

Run `cargo run -p layerstack_examples --example producer_durability` for unchanged
requests, unrelated edits, authored-output repair, same-ID source-layer replacement,
stale delayed work and history recovery with both producers. Configure authored
evidence through each `Layer::set_change_history_budget`, and composed report replay
through `LiveStage::set_change_history_budget`. Both default to 64 batches and
1 MiB. `change_history_stats()` reports retained batches/bytes, allocated bytes,
recorded work, evictions and discarded oversized batches. Retained-byte limits
include record headers and owned vector capacities; spare ring capacity is included
in allocated bytes separately, and allocator overhead is excluded. These counters
do not measure whole-stage, query, detached-result or application memory.

Call `live.synchronize(&mut store)` before cursor reads. On
`ChangeHistoryError::Expired`, the cursor advances and notice-dependent consumers
must rebuild (for example, `bounds.clear()`) before processing later reports.
Oversized batches still reach synchronous callbacks. Missing authored evidence
causes conservative recomposition; query-backed producers verify current records
without relying on replay. History loss alone need not invalidate generated
outputs or retire otherwise-current input evidence.

## Publishing generated meshes

`GeneratedMesh::prepare` is the complete polygon-mesh publication path through
a caller-owned stage and explicit `EditTarget`. It creates a new authored `Mesh`
or updates an existing mesh, with `subdivisionScheme = "none"` and a derived
extent. Points, topology and numeric primvars retain their shared buffers.
It rejects nonfinite points, polygons with fewer than three corners, bad topology,
incompatible declarations, bad interpolation, element sizes, indices and cardinality
before returning a transaction. Live publication and file export use the same
primvar cardinality and index validators. Indices retain separately indexed UV
and normal seams even when the selected values happen to be equal.

```rust,ignore
let publication = generated_mesh.prepare(
    live.stage(), &mut store, &edit_target, mesh_path, &owned_properties,
)?;
let applied = live.apply(&mut store, &publication.transaction)?;
bounds.apply_changes(&Scene::new(live.stage(), &store), &applied.changes);
owned_properties = publication.properties; // Only after successful application.
```

For repeated requests, consume the generated data once with
`let snapshot = generated_mesh.into_validated()?;`, then call `snapshot.prepare`
with the same arguments. Clones share its immutable geometry and cached extent.
Mutating an exported owner uses copy-on-write and cannot alter the validated
snapshot. Deferred numeric primvars decode once at construction. New point or
topology data requires a new validated snapshot; untouched buffers keep their
owners. `validation_work()` describes the initial validation; the returned
publication's `work` reports zero geometry validations and extent visits on reuse.

Each preparation still inspects the actual authored site and owned properties,
so deletion or replacement is detected even with unchanged inputs. Payload
arrays are compared by owner and rebound to the snapshot when owners differ,
including equal contents in an independently allocated buffer. The cached
two-point extent is compared by value. This avoids geometry scans without a
content hash. Empty retained transactions also carry the target-generation guard.
Planning costs scale with properties, metadata and authored samples, not geometry
size; preparation allocates property specs, names and transaction storage.

Each producer owns its property manifest at one authored site. Keep the manifest
with that producer's state and pass it to subsequent preparations at that same
site. Omitted owned properties, including obsolete `:indices` sidecars, are
removed from the target layer; weaker opinions can become visible again.
Unrelated attributes, material relationships and other layers are preserved.
An existing local declaration with a different type is rejected. A target-layer
generation guard prevents a prepared update from overwriting intervening edits.
An explicit type at the mapped authored site governs validation; an unselected
Mesh branch remains editable while a Cube sibling is selected. Untyped existing
sites require a composed Mesh. Publication leaves the selected sibling unchanged.
An unchanged publication has an empty transaction. After reloading producer state,
restore its manifest or explicitly supply its known owned names before pruning.

Preparation validates the entire snapshot and scans its points/topology/indices,
even for unchanged updates. It allocates small transaction/property records and
a derived extent buffer (at most two points). Native owner equality avoids
element comparison; independently allocated buffers can require O(elements)
comparison. Producer evaluation, immutable geometry-content hashing and buffer
pooling remain caller responsibilities.

Publish reusable geometry at its authored source and reference the asset root
with `instanceable = true`, or place that root through `PointInstancer`.
Instance-root material overrides and per-instance primvars remain separate from
shared geometry. Native-instance descendants are read-only proxies and are
rejected by publication. LayerStack's runtime prototype inventory is read-only;
it does not expose editable synthetic prototype paths. An authored source site
is identified by layer and spec path; a `PrototypeId` identifies only the current
composed snapshot; a producer's geometry-content identity is a separate key it
defines and maintains. Never persist prototype IDs as asset addresses or hashes.

Feed complete `Changes` reports to bounds and retained consumers. Point-instancer
prototype dependencies include masked and missing prototypes, so edits, deletion
and recreation can invalidate them. Clear consumers after losing change history
or changing scenes. Unchanged source and placement buffers remain shared.

The publisher handles complete **default-time polygon mesh snapshots** and native
numeric primvars. It removes existing samples on retained owned properties when
replacing a snapshot. Use schema `_owned_at` / `_shared_at` setters for animation,
including matching extent samples, and validate cardinality at each sampled time;
topology-changing intervals require a consumer policy for discontinuities.
Edit targets map referenced/variant sites and time offsets. The publisher does
not evaluate Substance graphs, execute `UsdProc` plugins, infer producer ownership
from composition, deduplicate geometry by content or run renderer adapters.
The `procedural` helper above invokes application evaluators; C++ `HdGp` plugins
and their runtime remain outside this publication path.

Run the complete two-producer workflow with:

```sh
cargo run -p layerstack_examples --example generated_assets -- ./generated-output
LAYERSTACK_USD_PYTHON=/path/to/usd-python cargo test -p layerstack_conformance --test generated_assets
cargo bench -p wind_tunnel --bench geometry_publication
```

The example keeps recipes under `/Recipes`, an editable source under `/Assets`
and placements under `/World`;
consumers select `/World` for placed scene content. Tests reopen all three authored
layers as USDA and USDC and compare materials, seams, IDs/masks, transforms, bounds
and offset samples with OpenUSD 26.8. The oracle also resolves sampled recipes,
independently evaluates the example generator, and compares generated points,
topology and material texture references. This proves the authored recipe and
publication contract, not parity with Hydra's procedural execution runtime.
The benchmark measures a 4,096-point,
7,938-triangle mesh with 100 native references and 100 / 10,000 / 100,000 point
instances. On the development Apple silicon host, a short Criterion run measured:

| Point instances | Initial publication | Unchanged | Points | Topology | Prototype edit + scatter bounds |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 100 | 4.38 ms | 21.9 µs | 77.4 µs | 67.9 µs | 100 µs |
| 10,000 | 4.42 ms | 21.0 µs | 77.6 µs | 68.6 µs | 396 µs |
| 100,000 | 4.53 ms | 21.1 µs | 77.9 µs | 68.5 µs | 3.84 ms |

Native references have a different cost. The `native_geometry_publication`
group holds point instances at 100 and varies native references. At 10,000
native references, recreating the source measured 2.81 s, unchanged publication
21.1 µs, point edits 25.0 ms, topology edits 24.1 ms and prototype edits with bounds
31.3 ms. This is an explicit initial-release scaling limit: source creation or
structural replacement can recompose many native occurrences. Prefer
`PointInstancer` for large scatter populations, and budget native-asset structural
updates separately. Buffer sharing does not eliminate composition work.

Material networks have occurrence-relative shader connections. Prototype record
sharing buckets source identities and mapped paths before exact comparison, so
different targets do not compare against every earlier occurrence. Hashes are
only candidate filters, never identities; numeric buffers are not hashed.
Value refreshes revisit changed member groups only, and empty refreshes skip
resharing. This preserves the exact sharing contract without repeated work on
unrelated material records.

These include validation, planning and live application. Initial publication means
recreating an absent source in an already populated stage; fixture setup, producer
evaluation and buffer construction are excluded. Bounds evaluation is included
only in the last column. The short run used 10 samples, 0.1 s warmup and 1 s target
measurement per case; these are reference measurements, not latency guarantees.
The current fixture includes the material network described above.

The `procedural_publication` group includes input revalidation, application
evaluation and validated publication for the same mesh and placement counts:

| Point instances | Unchanged | Recipe point update | Upstream point update | Recipe update + scatter bounds |
| ---: | ---: | ---: | ---: | ---: |
| 100 | 2.00 µs | 76.9 µs | 78.3 µs | 98.4 µs |
| 10,000 | 2.02 µs | 77.5 µs | 78.5 µs | 390 µs |
| 100,000 | 2.03 µs | 77.4 µs | 77.5 µs | 3.77 ms |

Each case has 100 native references. Recipe edits and upstream terrain
evaluation/publication are excluded from these timings; the upstream column
measures the dependent asset after terrain changes. Unchanged requests check
query identities, reuse validated geometry and verify detached input evidence;
they skip input resolution and geometry scans. The prior raw-mesh workflow
measured about 23 µs unchanged at these counts. Only the last
column includes bounds consumption. Initial publication and topology workloads
are covered by the direct publication groups above.

`cargo bench -p wind_tunnel --bench producer_durability` isolates unchanged
mesh preparation/application from scene population and generator work:

| Points | Raw `GeneratedMesh` | Retained `ValidatedMesh` |
| ---: | ---: | ---: |
| 100 | 1.35 µs | 1.22 µs |
| 4,096 | 4.91 µs | 1.18 µs |
| 1,000,000 | 883 µs | 1.19 µs |

This workload uses points without faces/primvars to isolate point validation and
extent work. Initial validation/publication is excluded. The raw baseline was
measured before migrating the benchmark to retained snapshots, on the same host
with the same short Criterion settings. The polygon/instance table above covers
the complete producer path. These measurements do not establish allocation-free
publication; planning still allocates as described above.

The existing `numeric_arrays/schema_points` benchmark fell from about 179 µs to
22 ns for a million-point dense getter, with pointer-identity tests establishing
that the improvement comes from retaining storage.

```rust
let mut tokens = layerstack::TokenInterner::default();
let schemas = layerstack_schemas::openusd(&mut tokens);
```

[API documentation](https://docs.rs/layerstack_schemas) ·
[Source](https://github.com/forest-rs/layerstack/tree/main/layerstack_schemas)

Requires Rust **1.89** or later and `no_std + alloc`. Add
`layerstack_schemas = "0.1"` to your dependencies. To select individual domains,
disable default features and enable the domain features you need. The optional
`std` feature forwards standard-library support to the optional SIMD backend;
the schema APIs remain usable with `no_std` + `alloc`.

Each domain is a Cargo feature (`usd-geom`, `usd-lux`, …); `all`, the
default, enables every one.

With `usd-geom` and `usd-shade`, `BindingCache::material_binding_subsets` joins
requested-time face-family validation with material resolution for each subset
and the parent fallback. It preserves binding strength, collection and purpose
rules and retains shared face indices. Nonempty `materialBind` families must be
`nonOverlapping` or `partition`; an unauthored family type reads as
`unrestricted`, matching C++ USD. Engines own triangulation and draw grouping.
After relevant edits, invalidate changed binding inputs and collection queries
explicitly. Clear the cache when precise dependency tracking is unavailable.

The `usd-shade` feature also resolves composed connections and material surface,
displacement and volume sources through node graphs and ordered render contexts.
Results retain the selected endpoint, other candidates, diagnostics and dependencies.
`shading::nodes` adds typed views and transaction edit handles for the standard
preview surface, UV texture, primvar reader and 2D transform nodes. Input getters
read composed USD values; associated `_default` functions expose node definition
defaults explicitly. Typed port creation supports the ordinary connection APIs.

The `usd-lux` feature captures owned lighting inputs through `light::LightInputs`
and retains them through `light::LightCache`. Checked `shaping`, `shadow` and
`environment` groups preserve USD units, negative sentinels, texture layouts and
asset readiness. `assets::AssetReference` captures winning authoring-layer evidence
without enabling global provenance, and anchors identifiers through an explicit
host resolver. Loading and decoding assets remain host operations.

With `usd-geom`, `affine::AffineFactors` exposes the full row-vector factorization,
including principal stretch orientation and reflection. `to_trs` is an optional
conversion with a caller-specified reconstruction tolerance; shear is preserved
in full factors and rejected when it exceeds that tolerance.

With `usd`, `MembershipCache` retains compiled collection queries and ordered
candidate decisions, including duplicates. With `usd-lux`, `capture_light_links`
and `capture_filter_links` use the same cache. Feed every complete edit report
to `apply_changes`; clear after losing history or changing scenes/registries.
Dependency scopes, query and decision revisions, work counters and query problems
remain inspectable. Engines own their masks, buffer layouts and GPU resource
lifetimes. See the `layerstack_examples` `lighting_inputs` binary for a full flow.

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
- `Scene::compute_motion_blur_scale`, `compute_nonlinear_sample_count` and
  `compute_velocity_scale` find the nearest readable authored `MotionAPI`
  setting through any prim type; the API view exposes the same computations.
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

When no valid extent is authored, bounds are computed from mesh points, point widths, curve control hulls and
maximum widths, or
cube, sphere, cylinder, cone and capsule parameters, or point-instancer
prototypes and instance transforms. Computed instancer extents evaluate prototypes with ordinary visibility and
without model hints, independently of the caller’s visibility and hint policies. Prototype cycles and
nesting beyond 64 instancers are rejected. Curve bounds match OpenUSD’s control-hull
approximation, including its possible Catmull-Rom and Hermite overshoot. Other procedural extent providers
remain unsupported and return explicit errors. Common transform helpers do not decompose arbitrary
matrices or rewrite incompatible op stacks. Prepared operation recipes remain
an internal cache implementation detail.

With `usd-geom`, every prim view exposes `primvars` and
`find_primvar_with_inheritance`; `primvars_with_inheritance` returns effective
value-producing primvars. Only authored constant values inherit, and a nearer
authored nonconstant value stops inheritance. `primvar::Primvar` reads
interpolation, element size, indices and placeholder metadata. `compute_flattened`
expands indexed elements at default or numeric time, preserving native array
kinds and reporting invalid indices or decoding failures without a partial result.
`validated_values` instead retains source values and optional shared indices,
with checked `element_range` mappings for direct vertex expansion or GPU upload.
It validates complete ranges for `elementSize` without allocating flattened data.
String `:idFrom` primvars resolve forwarded relationship targets with native USD
cardinality rules; invalid target cardinality returns no value.

Edit handles expose `create_primvar` with a declared `PropertyType`.
`PrimvarEdit` authors values, samples, interpolation, element size and indices
through `SchemaEdit`; invalid names, metadata and incompatible declarations
append no edits. Ordinary transaction validation and undo remain in force.

`PointInstancer::compute_mask` matches inactive and invisible IDs against stable
IDs or array positions. `compute_instance_transforms` retains original indices
and IDs after masking, includes optional prototype-local transforms and anchors
velocity, acceleration and angular velocity samples to an explicit base time.
`prepare_instance_transforms` captures validated immutable inputs for direct
iteration, reusable vectors, or bounded chunks. Captures survive later stage
edits; `compute_instance_transforms_into` validates before changing caller output.
See the `importer_inputs` binary for checked points, material subsets, indexed
UV expansion and bounded instance placement together.
Invalid topology returns an error. Misaligned motion arrays are ignored;
ordinary interpolation is used when no usable linear or angular motion remains. Sparse motion edits compose through stage resolution; anchoring uses the
effective contributing sample grids. Scalar spline motion sources return an
explicit unsupported error because instance motion attributes are arrays. Bounds caches and retained queries track external and nested
prototype dependencies; `bound_prototype_dependencies` exposes the roots
consulted, including missing targets that can recover after later edits.

With `usd-semantics`, `Scene::direct_taxonomies` and `inherited_taxonomies`
discover applied label taxonomies. `LabelsQuery` computes sorted direct and
inherited labels at one time or over an open/closed interval. It retains only
labeled prims in a caller-owned cache tied to one scene snapshot; construct a
new query after edits. Empty interval configurations are rejected explicitly.

With `usd-physics`, `physics::compute_collision_group_table` resolves group
filtering, inversion and authored merge names into a symmetric snapshot.
`PhysicsCollisionGroup::colliders_collection` uses the ordinary collection API.
Unknown group queries collide by default; malformed filter targets return a
structured error instead of aliasing an unrelated group. Merged groups share
stored pairs, whose count is inspectable. Recompute the snapshot after edits.

With `usd-render`, `usd_render::RenderSettings::compute_spec` resolves shared
settings and authored product overrides into camera-conformed output products
and ordered indices into deduplicated render channels. It retains purposes,
extra namespaced settings and output provider paths, and reports invalid cameras
and channels. The feature also enables geometry and shading schemas needed by
those computations. It resolves configuration; image and shader evaluation stay
with the renderer.

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

`usd_geom::Camera::compute_camera` resolves a sampled camera through a caller-owned
`XformCache`. The owned `camera::ComputedCamera` exposes authored lens parameters,
view/projection matrices, world frustum corners and inward planes, conservative
oriented-bound culling, and frame-relative shutter intervals. Camera rotation
conforms scale, shear and reflection as OpenUSD's `GfFrustum`; the authored world
matrix is retained separately. Matrices use USD row vectors and OpenGL `[-1,1]`
depth. Consumers choose aspect conforming, backend clip-space conversion and shutter
sampling, and execute any additional authored camera-space clipping planes.
Recompute snapshots after edits and pass ancestor changes to `XformCache`. Unknown
projection tokens and invalid geometric parameters return `CameraError`. See the
`camera_frustum` example for moving point instances sampled across a shutter.

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
(`LICENSE-TOST-1.0`, `NOTICE`). The private matrix decomposition code adapts
OpenUSD Gf under the same license. The rest of the crate is under Apache-2.0 OR
MIT (`LICENSE-APACHE`, `LICENSE-MIT`).

With `usd-skel`, `Skeleton::query` prepares a validated parent-first joint
forest and sparse animation mapping. Queries compute local, skeleton-space and
inverse-bind skinning transforms at explicit times, retaining rest defaults for
unanimated joints. `SkelBindingApi` resolves inherited skeleton and animation
bindings, including explicit empty relationships. Definition snapshots must be
rebuilt after scene edits; animation reads use normal stage value resolution.
`SkelRoot::skinning_queries` discovers geometry bindings;
`skinning_queries_with_instance_proxies` also visits native instanced rigs. `SkinningQuery` handles
independently inherited, indexed constant/vertex influence primvars, custom
joint order and geometry bind transforms. CPU points are returned in skeleton
space. Bindings honor both `classicLinear` and `dualQuaternion`; unknown tokens
return an explicit unsupported-method error. Queries expose their source prim roots;
rebuild them when bindings, definition arrays or influence metadata change.

`BlendShapeQuery` captures local dense/sparse point and normal offsets, including
weighted inbetweens, interpolation and endpoint extrapolation. Animation weights
map by name. `SkinningQuery::compute_deformed_points` applies blend shapes before
joint skinning; standalone shape queries also work without joint influences.
Normal offsets are returned without renormalization or skeletal normal skinning.
The pure kernels also offer reusable buffers; validation failures leave them
unchanged. Definition snapshots expose shape targets and refresh explicitly.

Normal skinning derives inverse-transpose matrices and normalizes results,
including explicit face-corner to point mapping for face-varying normals.
`SkinningQuery::compute_skinned_normals` reads geometry normals without applying
blend-shape offsets; singular normal matrices and unsupported interpolation error.
Constant bindings also expose their rigid skeleton-space transform, using the
reference implementation's float-frame rounding. `SkelAnimation` supplies
standalone TRS evaluation, effective sample-time unions and variability helpers;
stronger defaults/blocks mask weaker animation and layer offsets map sample times.

`SkelCache` retains definitions, inverse binds, influence arrays and geometry
buffers for one stage/store pair. Mesh parts share pose and normal palettes.
Pass every successful edit report to `apply_changes` before querying again;
this includes undo. Time changes are constant-time and evaluation stays lazy.
Static outputs survive time changes, while affected pose, weight and geometry
inputs refresh independently. Clear the cache before switching stage/store pairs.
Missing bindings and failed definitions retry instead of becoming stale negatives.
Work counters and array payload occupancy are available through `stats`/`memory`;
reported bytes exclude definitions, strings and container overhead.

```rust,ignore
let mut cache = SkelCache::new(Time::at(1.0));
let points = cache.deformed_points(&scene, geometry)?;
let work = cache.stats();
cache.set_time(Time::at(2.0));
// After applying a transaction (or its inverse) to the live stage:
cache.apply_changes(&updated_scene, &applied.changes);
let updated = cache.deformed_points(&updated_scene, geometry)?;
```

Dual-quaternion skinning blends hemisphere-aligned rotation/translation and
linear residual scale/shear, matching OpenUSD's affine joint-matrix convention.
Authored weights remain unnormalized for linear accumulation; quaternion blends
are normalized. Singular point factorization uses the reference zero-DQ fallback,
while singular normal inverse transposes return an error. The explicit
`*_with_method` kernels accept `SkinningMethod`; existing short kernel names use
classic linear blending. `SkinningQuery::skinning_method` exposes the inherited
method, and `SkelCache` shares DQS point/normal decompositions per skeleton pose.
Reference fixtures cover twist, maximum-weight pivots and ties, scale, shear,
reflections, zero/nonunit/negative weights and singular factorization.

The optional `simd` feature accelerates CPU linear-blend skinning with
`fearless_simd` 1.0. It preserves separate arithmetic and USD float rounding;
dense blend shapes retain the compiler-vectorized kernel. With `std`, CPU feature
detection selects a backend; without it, the compiled target baseline is used.
Scalar fallback remains available, and ordinary schema builds do not pull in the
SIMD dependency. `wind_tunnel` compares scalar, SIMD and `glam` kernels.

`SkinningQuery::binding_inputs(time)` resolves flattened influences and the
geometry-bind transform without reading vertices or computing a pose.
`joint_mapping()` relates binding-order indices to the shared Skeleton palette;
unmapped joints use identity transforms. `BlendShapeQuery::samples` and `sample`
expose sparse/dense point and normal offsets in contribution order, including
inbetweens. `validate_point_count` checks all samples before a complete buffer
upload, while CPU evaluation continues to validate only active samples.


`SkelCache::deformation_inputs` borrows retained binding values, shared rig-order
matrices, mapped binding-order matrices, shape samples and evaluated inbetween
contributions without reading or deforming vertex buffers. DQS bindings also
expose prepared point quaternion components and residual scale/shear; their
scalar-first component order and row-vector convention are documented on
`DualQuaternionJoint`. Normal DQS uses a separate inverse-transpose decomposition.
Validate the view against the adapter's vertex count before uploading.

`DeformationRevisions` separates skeleton and binding definitions, flattened
inputs, shared poses and local shape weights/contributions. Revisions are
conservative cache-local stamps, survive `clear`, and must only be compared within
one cache instance. A resync can rebuild several components; precise pose/weight
edits and time changes preserve independent inputs. Share a palette upload by
skeleton path and pose revision. Consumers choose GPU packing, precision and
execution, and retain their own previous-frame or shutter-sample history. The
`deformation_inputs` example demonstrates selective uploads and motion history
with renderer-owned buffers.


`SkinningQuery::compute_deformed_normals` and `SkelCache::deformed_normals` apply
animated blend-shape normal offsets before skinning and normalization. Mesh
face-varying normals expand point-indexed offsets to every corresponding corner.
Constant normals reject active per-point shape normal offsets. Skin-only normal
outputs remain independent of blend-weight changes. `SkelCache::normal_inputs`
borrows binding-order inverse transposes and normal DQS components without
reading vertex buffers; pose/input revisions support selective adapter uploads.


`skel::BlendShapeCache` retains morph-only definitions, mapped animation weights,
inbetween contributions and point/normal outputs. It inherits the geometry's
`skel:animationSource` and needs no skeleton or joint influences. Its `inputs`
view works without vertex buffers and has independent definition/weight revisions.
Results remain in geometry space; normal offsets are not normalized. Explicit
time changes, edit reports, undo, counters and memory occupancy follow the same
caller-owned cache pattern as `SkelCache`.


Both deformation caches expose `deformed_mesh_bounds` and
`deformed_world_mesh_bounds`. These explicitly bound all deformed mesh points,
ignoring authored extents and hints. Skeletal hulls are in skeleton space;
morph-only hulls are mesh-local. World bounds pair the retained hull with an
`XformCache` at the same time; apply edits to both caches. Reductions reuse point
outputs and survive independent transform edits. Counters expose bound reductions
and visited vertices. Empty meshes have empty bounds; nonfinite points error.
Width-bearing geometry and renderer displacement need separate bound policies.


`SkelCache::for_each_deformation_sample` and `BlendShapeCache::for_each_sample`
visit meshes across caller-supplied shutter times in sample-major order. Visitors
copy/pack each borrowed input view before the cache advances. Duplicate times and
interpolation policies are preserved; callbacks can return adapter errors and
stop the batch. The cache ends at the last attempted time; empty requests leave
it unchanged. No history or shader scheduling is implied.


`PointInstancer::compute_instance_transforms_at_times` evaluates ordered shutter
samples against a fixed topology/mask base, retaining original indices and IDs.
Sparse default/sample edits participate in motion anchoring without bypassing
stage composition; dense sources mask weaker sample grids. Any bad sample
returns an error without a partial batch.
