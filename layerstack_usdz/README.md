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

let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes).unwrap();
assert_eq!(archive.entries()[0].name.as_ref(), "scene.usda");
assert_eq!(archive.entries()[0].data_offset % 64, 0);
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

## Features and limits

- Rust **1.89** or later; `no_std` with `alloc` and a global allocator.
- No default features. The optional `std` feature currently adds no behavior.
- Uncompressed, unencrypted 32-bit ZIP only; no Zip64.
- The writer accepts USD layers and supported image/audio extensions, not nested
  USDZ packages. See the [writer API](https://docs.rs/layerstack_usdz/latest/layerstack_usdz/writer/index.html)
  for the exact accepted member types and errors.
- The reader requires the full archive in memory; the writer returns a complete
  `Vec<u8>`. Neither API streams files to or from disk.

## License

See [CHANGELOG.md](CHANGELOG.md) for release status.

Licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
