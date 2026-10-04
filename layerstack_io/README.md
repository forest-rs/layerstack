# layerstack_io

Application-owned OpenUSD file I/O above Layerstack's composition kernel and
USDA, USDC and USDZ format implementations. `StageDocument` loads dependencies
into one shared store, exposes its retained stage, saves dirty layers separately
from session edits, and reloads selected sources with an explicit dirty policy.

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

Value-clip requests remain explicit: inspect `Stage::clip_asset_requests`, then
use `StageDocument::load_asset` with each request's authoring layer. The host can
schedule those reads independently of rendering. Other retained clients over
the document's store synchronize independently after publication.
