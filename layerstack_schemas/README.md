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

Requires Rust **1.89** or later and `no_std + alloc`. Add
`layerstack_schemas = "0.1"` to your dependencies. To select individual domains,
disable default features and enable the domain features you need. The optional
`std` feature currently adds no behavior.

Each domain is a Cargo feature (`usd-geom`, `usd-lux`, …); `all`, the
default, enables every one.

The `usd-shade` feature also resolves composed connections and material surface,
displacement and volume sources through node graphs and ordered render contexts.
Results retain the selected endpoint, other candidates, diagnostics and dependencies.
`shading::nodes` adds typed views and transaction edit handles for the standard
preview surface, UV texture, primvar reader and 2D transform nodes. Input getters
read composed USD values; associated `_default` functions expose node definition
defaults explicitly. Typed port creation supports the ordinary connection APIs.

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
kinds and reporting invalid indices without a partial result. String primvars
with `:idFrom` indirection return an explicit unsupported error.

Edit handles expose `create_primvar` with a declared `PropertyType`.
`PrimvarEdit` authors values, samples, interpolation, element size and indices
through `SchemaEdit`; invalid names, metadata and incompatible declarations
append no edits. Ordinary transaction validation and undo remain in force.

`PointInstancer::compute_mask` matches inactive and invisible IDs against stable
IDs or array positions. `compute_instance_transforms` retains original indices
and IDs after masking, includes optional prototype-local transforms and anchors
velocity, acceleration and angular velocity samples to an explicit base time.
Invalid topology returns an error. Misaligned motion arrays are ignored;
ordinary interpolation is used when no usable linear or angular motion remains. Spline and sparse-edit motion sources return an explicit
unsupported error. Bounds caches and retained queries track external and nested
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
