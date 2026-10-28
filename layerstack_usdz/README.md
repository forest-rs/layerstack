# layerstack_usdz

Read and write USDZ packages for [Layerstack](https://github.com/forest-rs/layerstack).
The writer packages already-serialized USD layers and media; the reader imports
the root layer and its resolved layer dependencies into Layerstack's data model.
Both operate on bytes, without filesystem access.

## Package a layer

Add `layerstack_usdz = "0.1"` to your dependencies:

```rust
use layerstack_usdz::{PackageFile, write_usdz};

let layer = b"#usda 1.0\ndef Xform \"Model\" {}\n";
let bytes = write_usdz(&[PackageFile::new("scene.usda", layer)]).unwrap();

assert_eq!(&bytes[..4], b"PK\x03\x04");
```

Pass the root layer first, followed by its dependent layers and media. Entries
are stored verbatim, in the given order. Use package-relative paths and author
matching asset paths in the layers. The writer rejects duplicate or unsafe paths
and unsupported member extensions, and emits deterministic, uncompressed ZIP
entries with 64-byte data alignment and CRC-32 checksums.

This is a packaging API: it does not serialize meshes or layers, rewrite asset
paths, validate the contents of members, or collect missing dependencies. Use the
repository's `layerstack_usda` or `layerstack_usdc` writer to serialize a layer;
`layerstack_mesh_export` supplies mesh authoring and an ARKit export profile.
A valid generic USDZ archive does not by itself establish ARKit compatibility.

## Read a package

[`read_usdz`](https://docs.rs/layerstack_usdz/latest/layerstack_usdz/fn.read_usdz.html)
accepts the complete package byte slice, a root `LayerId`, shared token and path
interners, and an `AssetResolver` for references outside the archive. It validates
the archive layout and member CRCs before importing the root layer. Insert both
`UsdzResult::layer` and `UsdzResult::resolved_layers` into your store before
composing a stage. Inspect `UsdzResult::diagnostics` or `has_errors()` for
recovered import problems and the stage's composition errors for unresolved arcs.
An `Ok` result can contain a partial import.

Allocate the root ID from the supplied resolver; other reachable members obtain
IDs through `AssetResolver::allocate_layer_id`. The resolver must allocate unique
IDs when the package loads additional members. Relative paths such as
`./asset.usda` are anchored to the member that authors them. Search paths such
as `asset.usda` try that member's directory and then the root layer's directory.
Missing search paths and absolute paths pass to the outer resolver.
`UsdzResult::member_paths` records loaded package members. USDA/USDC import is limited to the companion readers'
supported subsets; packaging does not expand their format coverage.

## Verified snapshots

`PackageSnapshot::new` validates all directories and member checksums, including
unused nested packages, under `PackageLimits`. `read_usdz_snapshot` reuses those
indexes and shared encoded views. The borrowed reader copies the outer archive
once to obtain owned storage. Nested identifiers select explicit members or an
inner archive's first native USD layer; relative dependencies and resources remain
inside their innermost archive. `PackageStats` reports indexed entries, directory
parses, verification bytes and backing allocation size. Small views retain the
outer allocation until dropped or explicitly detached.

## Import selected members through ranges

`range::RangePackage::open(source, RangeLimits::default())` reads the outer
central directory and end record. `layer(path)` and `member(path)` check a
selected member's local header, bounds, alignment and CRC before returning owned
`EncodedBytes`. Nested package directories open only when selected. Returned
bytes survive the reader; repeated selection reuses local-header metadata but
reads and verifies the payload again. Clones share immutable directory indexes.

`read_usdz_ranges(&mut package, selected, layer_id, tokens, paths, resolver,
members, options)` imports reachable and explicitly requested layers using the
same resolver and format pipeline as `read_usdz_snapshot`. Already imported
member identities, including nested default-layer aliases, reuse their layers
without additional payload reads. Insert the returned
root and resolved layers into a store and inspect diagnostics as above. This
mode deliberately leaves unselected members and enclosing nested-archive CRCs
unverified. Use `PackageSnapshot` for complete integrity checks.

A `RangeSource` supplies a fixed length and exact owned range reads. The host
owns source identity, version stability and transport errors. `EncodedBytes`
implements the trait for in-memory use; `layerstack_io::Storage::open_ranges`
provides an optional host transport hook, implemented by `Filesystem`.
`RangeError::Source` preserves the original transport error; `Format` identifies
layout, selection, budget or checksum failures. No source error falls back to a
complete read. `RangeLimits` bounds directory entries, directory bytes, depth,
single-member size and cumulative CRC bytes before their corresponding reads.
`RangeStats` reports requests, delivered bytes, directories and checksum work;
it never implies complete package verification.

## Features and limits

- Rust **1.89** or later; `no_std` with `alloc` and a global allocator.
- No default features. The optional `std` feature enables retained USDC numeric
  arrays through `read_usdz_with_options` and `UsdcArrayLoading::Retained`.
  `UsdzResult::retained_values` exposes a cache/statistics handle for each loaded
  USDC member. Members share checked ranges into one immutable archive allocation.
  Numeric failures
  appear during checked reads; structural validation still occurs on import.
- `UsdcReadOptions::decode_budget` limits decoder work independently per USDC
  file, including subsequent retained reads. It does not limit archive size,
  USDA parsing or total scene memory.
- Uncompressed, unencrypted 32-bit ZIP only; no Zip64.
- The writer accepts native USD layers, nested USDZ packages and supported
  image/audio extensions. The first member must be a native USD layer. See the [writer API](https://docs.rs/layerstack_usdz/latest/layerstack_usdz/writer/index.html)
  for the exact accepted member types and errors.
- Complete verification requires the full archive in memory. Explicit range imports
  read directories and selected members through a host source. The writer returns
  a complete `Vec<u8>`.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
