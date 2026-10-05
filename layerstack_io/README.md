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
export a caller-selected localization plan to write a package. Layer export writes
a copy and preserves the original source binding and dirty state. Direct writes to
public importer fields must call `Layer::touch` before synchronization or saving.

Start with `cargo run -p layerstack_examples --example tutorial_stage_io` for a
complete create/edit/undo/export/reload workflow, or `tutorial_references` for
a filesystem dependency example. Backends implement three operations:
`identify`, `read` and `write`. Format dispatch, store-local identity allocation,
package-relative resolution, expression-asset discovery and source tracking
stay in this crate.

For large USDC scenes, `StageDocument::open_with` or `open_in_with` accepts
`LoadOptions` with `UsdcArrayLoading::Retained` and an optional per-file decoder
budget. The policy also applies to dependencies, package members and reloads.
`retained_values(layer_id).stats()` exposes decode attempts, failures and work
remaining. Checked attribute reads preserve deferred failures; save/export
reports them as `IoErrorKind::Decode` with the original `array_read_error`.
Encoded files are still read completely, and each package member retains its
own byte copy. Decoder units are not memory bytes or a scene-wide memory limit.
Run `cargo run -p layerstack_examples --bin lazy_io` for a complete example.

Value-clip requests remain explicit: inspect `Stage::clip_asset_requests`, then
use `StageDocument::load_asset` with each request's authoring layer. The host can
schedule those reads independently of rendering. Other retained clients over
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
support one archive level; nested archives are unsupported.
Run `cargo run -p layerstack_examples --bin prepared_reload` for a complete
reject/accept workflow with checked geometry, host budgets and packaged textures.

## Bounded resources

`read_asset_bytes_bounded(asset, authoring_layer, AssetReadLimits { bytes,
package_bytes })` rejects oversized resident members before copying them and
uses `Storage::read_bounded` for loose resources and nonresident archives.
The storage extension defaults to an unsupported error without invoking `read`;
custom transports opt into bounded reads explicitly. `Filesystem` bounds reads
and checks file size before allocation. Existing unbounded resource APIs retain
their behavior; use the bounded API when admitting renderer inputs.

