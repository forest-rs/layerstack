# layerstack_io

Application-owned OpenUSD file I/O above Layerstack's composition kernel and
USDA, USDC and USDZ format implementations. `StageDocument` loads dependencies
into one shared store, exposes its retained stage, saves dirty layers separately
from session edits, and reloads selected sources with an explicit dirty policy.

[API documentation](https://docs.rs/layerstack_io) ·
[Source](https://github.com/forest-rs/layerstack/tree/main/layerstack_io)

```toml
[dependencies]
layerstack = "0.1"
layerstack_io = "0.1"
```

Requires Rust **1.89** or later.

```rust,no_run
use layerstack::StageOptions;
use layerstack_io::{Filesystem, StageDocument};

let storage = Filesystem::new(".", [])?;
let document = StageDocument::open(storage, "scene.usda", StageOptions::default())?;
println!("Loaded {} layers", document.load_report().layers.len());
# Ok::<(), layerstack_io::IoError>(())
```

For schema fallbacks and typed views, use `StageDocument::open_in`: construct
`layerstack_schemas::openusd` using the supplied store's tokens, then pass that
same store and its registry in `StageOptions`. `open` creates a fresh store;
a registry built from another store cannot share its token domain.

The default `std` feature provides `Filesystem`. Disable default features for
`no_std` + `alloc` with a host implementation of `Storage`. Identifiers, transport,
write atomicity and scheduling belong to that backend. No third-party dependencies
are added. Recoverable format errors require explicit `ImportPolicy::AllowRecovery`.

USDA and USDC source layers can be saved in place. USDZ members are read-only;
use `prepare_package` to inventory and localize a dependency closure, inspect its
report, then `export_package` to write the candidate. Layer export writes
a copy and preserves the original source binding and dirty state. Every mutating
`Layer` method records its edit, so synchronization and saving see each one.

Start with `cargo run -p layerstack_examples --example tutorial_stage_io` for a
complete create/edit/undo/export/reload workflow, or `tutorial_references` for
a filesystem dependency example. Backends implement three operations:
`identify`, `read` and `write`. Format dispatch, store-local identity allocation,
package-relative resolution, expression-asset discovery and source tracking
stay in this crate.

Opening reads only the layers the stage's variant selections reach, as OpenUSD
does: an arc inside a variant branch its own layer does not select is deferred,
and composition asks for those its selections reach (`Stage::asset_requests`),
which opening loads. After an edit or reload selects another variant,
`StageDocument::load_requested_assets` loads what it needs;
`VariantArcLoading::All` resolves every branch's arcs up front instead.

For large USDC scenes, `StageDocument::open_with` or `open_in_with` accepts
`LoadOptions` with `UsdcArrayLoading::Retained` and an optional per-file decoder
budget. The policy also applies to dependencies, package members and reloads.
`retained_values(layer_id).stats()` exposes decode attempts, failures and work
remaining. Checked attribute reads preserve deferred failures; save/export
reports them as `IoErrorKind::Decode` with the original `array_read_error`.
Encoded files are still read completely. Package members, including nested
archives, share checked ranges into one immutable encoded owner. Decoder units are not memory bytes or a scene-wide memory limit.
Run `cargo run -p layerstack_examples --bin lazy_io` for a complete example.

For a time-specific clip read, call `Stage::plan_clip_query(property, time,
interpolation, max_demands)` to inspect manifest, active-interval and interpolation
bound demands without I/O. `StageDocument::prepare_clip_query` explicitly loads
and replans under a request budget. Inspect `plan.is_complete()`, failures and
budget exhaustion before a checked value read; a complete plan can describe a
confirmed gap. A zero-load budget only inspects residency. Dormant clips outside
the required interval remain unread when an explicit manifest permits it. Automatic
manifests, templates and missing-value searches can require more files.
`clip_asset_requests` and `load_asset` remain available for custom scheduling. Other retained clients over
the document's store synchronize independently after publication.

Use `prepare_reload` or `prepare_reload_layers` when an engine must inspect a
candidate before publication. `PreparedReload` exposes its store, composed stage,
import report, source identities and retained decode statistics. Dropping it
rejects the reload; `commit()` publishes its layers, source catalog and already
composed stage together, preserving existing change observers. No source reads
or second composition occur at commit. Rejection leaves dirty generations and
dependency freshness unchanged, including failed or leaked candidates.
Preparation copies authored maps and interner indexes into an independent ID
domain; immutable numeric buffers and package bytes share ownership.
Candidate-only IDs must not be used on a rejected document. The document stays
exclusively borrowed until acceptance or rejection. Synchronize direct edits
before preparation if they change which source layers the snapshot uses.

`read_asset_bytes(path, authoring_layer)` reads arbitrary images, environment maps
and other bytes using the same package/member policy as layer imports. Capture
the winning source with `layerstack_schemas::assets::AssetReference::read` rather
than assuming the composed root authored the asset. The result contains immutable
bytes, canonical identity and package provenance; no image decoder, renderer or
new layer identity is introduced. Candidate reads use the candidate USDZ snapshot,
so keep those validated bytes for renderer/GPU publication after commit. Loose
external assets are read at call time. Explicit `package[member]` identifiers
support nested archives, for example `scene.usdz[props.usdz[textures/base.png]]`.
Relative identifiers stay inside their innermost containing package. Returned
`EncodedBytes` views expose `backing_bytes()` and `shares_backing()`; `detached()`
copies a small resource when retaining its whole archive would be undesirable.
`package_stats` reports directory/CRC work and `package_cache_stats` reports
package opens, index hits, copied encoded bytes and resident archive ownership.
Run `cargo run -p layerstack_examples --bin prepared_reload` for a complete
reject/accept workflow with checked geometry, host budgets and packaged textures.

## Bounded resources and root reloads

`read_asset_bytes_bounded(asset, authoring_layer, AssetReadLimits { bytes,
package_bytes })` rejects oversized resident members before returning shared views and
uses `Storage::read_bounded` for loose resources and nonresident archives.
The storage extension defaults to an unsupported error without invoking `read`;
custom transports opt into bounded reads explicitly. `Filesystem` bounds reads
and checks file size before allocation. Existing unbounded resource APIs retain
their behavior; use the bounded API when admitting renderer inputs.

`prepare_reload_root(policy)` reads the authored root and its freshly reachable
file dependencies once, preserving source IDs and session opinions. Old unused
sources and package members are not mandatory reload roots. Session-only file
dependencies are outside this explicit root-file scope; selected `prepare_reload`
and `prepare_reload_layers` remain available for other policies. Only sources
actually replaced by this candidate participate in dirty protection. Expression
asset bindings from replaced layers are rediscovered, so retained expression
dependencies refresh and participate in the same dirty protection. Drop rejects
all source/catalog/freshness changes; commit publishes the composed candidate.

These additions keep transport, composition and renderer ownership unchanged;
no new dependency or architectural boundary is introduced.

## Preparing a package

`prepare_package(root, "scene.usda", PackagePreparationLimits::default(),
|_| PackageInclusion::Include)` inventories authored dependencies across variants
and inactive list items in an isolated store. Resident layers include their unsaved
edits. The candidate rewrites copied layers and shares opaque resource bytes;
preparation and export preserve live source bindings and dirty generations.
Inspect `report()` for exact unresolved authoring uses and `plan()` for destinations.
Missing resources, unsupported resource types and unexpanded patterns prevent a
plan. Choose `KeepExternal` for intentional external dependencies; concrete paths
are anchored to their original sources, while expressions remain host controlled.
Use the lower-level localization callback for custom pattern expansion.

Set layer/asset inventory counts, cumulative opaque bytes and per-resource
transport limits explicitly. These bound inventory and resource reads; layer
format imports can follow arcs and the counts are not a scene allocation cap.
`LoadOptions::package` separately bounds nesting, total directory entries and
checksum work, including unused nested archives. Limits apply before admitting a
snapshot. Each resident package directory is reused for later layer/resource reads.

`Filesystem` writes a sibling candidate, synchronizes its contents, then replaces
the destination. Failed writes preserve the old file and clean up the candidate.
Symlink destinations retain the link and replace its resolved target. Replacement
changes inode identity; hard links retain the old inode. This is a single-file
replacement guarantee, not a multi-file transaction or directory durability promise.
Standard permissions are preserved on native platforms; macOS also preserves
extended metadata. WASI relies on host-granted filesystem rights, without file
permission bits.
Other platforms' extended access controls require a host-specific `Storage`.

Direct asset-valued prim `assetInfo.identifier` entries are identity uses rather
than required package resources. `prepare_package` reports them as ignored and
preserves their spelling without loading them or calling the resource inclusion
callback. Other nested assets remain dependencies, including nested dictionaries
whose keys happen to be `identifier`. Generic authored inventory and rewriting
retain identity uses as `AssetDependencyKind::Identity`; see
[the identity contract and caller update](../docs/asset-identity.md).

Storage backends can override `read_encoded` and `read_encoded_bounded` to return
an existing immutable `EncodedBytes` owner. Their defaults transfer vectors from
`read`/`read_bounded` without copying payload bytes, so existing backends benefit
without changes. Bounds remain explicit and never fall back to an unbounded read.
Package members and retained numeric inputs keep independent shared views.
`encoded_bytes_copied` counts document-layer copies after transport; copies made
inside a backend remain outside that counter. Vector owners retain spare capacity,
which `EncodedBytes::backing_bytes` accounts; `detached` explicitly copies a small
view when releasing a large owner is preferable.

## Explicit package range imports

`Storage::open_ranges(identifier)` optionally returns a fixed logical byte source
for `layerstack_usdz::range::RangePackage` and `read_usdz_ranges`. The default
reports `Unsupported` without reading. `Filesystem` implements exact bounded
range reads through a retained open file handle; there is no full-read fallback.
Filesystem sources survive atomic replacement. Hosts must exclude external
in-place writes during a source's lifetime: length/mtime checks detect ordinary
changes but do not make the file a snapshot.

This lower-level import path verifies selected member headers and CRCs, including
reachable layer dependencies. Unselected members and enclosing nested-package
CRCs remain unverified. Inspect `RangeStats` for actual read and verification
work and use `RangeLimits` to bound it. `StageDocument` continues to open completely
verified package snapshots, including unused members. Range transport is an
explicit choice for hosts assembling their own import/store workflow.
