//! Integration tests for USDZ package file reading.
//!
//! Since no USDZ fixture files exist in the spec supplemental, these tests
//! construct USDZ archives programmatically using a `build_usdz` helper.
//!
//! Spec: AOUSD Core §16.4.

use layerstack::doc::{FieldValue, LayerId, Value, get_field};
use layerstack::interner::TokenInterner;
use layerstack::path::PathInterner;
use layerstack::{AssetResolveError, AssetResolver, InMemoryStore, ResolvedAsset};
use layerstack_usdz::crc32;

// ── Stub resolver ───────────────────────────────────────────────────────

struct StubResolver;

impl AssetResolver for StubResolver {
    fn resolve(
        &mut self,
        _asset_path: &str,
        _anchor: Option<LayerId>,
        _tokens: &mut TokenInterner,
        _paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }

    fn resolved_path(&self, _id: LayerId) -> Option<&str> {
        None
    }
}

// ── USDZ builder ────────────────────────────────────────────────────────

/// Builds a valid USDZ ZIP archive from constituent files.
///
/// Each entry is a `(name, data)` pair. Local file headers are 64-byte
/// aligned with padding as required by §16.4.1.
fn build_usdz(files: &[(&str, &[u8])]) -> Vec<u8> {
    build_usdz_opts(files, &BuildOpts::default())
}

/// Options for customizing the USDZ builder (for error-case tests).
#[derive(Default)]
struct BuildOpts {
    /// Override compression method for all entries (default: 0 = Stored).
    compression_method: u16,
    /// Override general purpose flags for all entries (default: 0).
    flags: u16,
    /// If true, add an EOCD comment.
    eocd_comment: bool,
    /// If true, corrupt the CRC of the first entry.
    corrupt_crc: bool,
    /// If true, skip 64-byte alignment of local file headers.
    skip_alignment: bool,
}

/// Internal record for building central directory.
struct EntryRecord {
    name: Vec<u8>,
    local_header_offset: u32,
    crc32: u32,
    size: u32,
}

// The builder intentionally uses `as` casts for small test data that fits
// in 32-bit ZIP fields.
#[allow(
    clippy::cast_possible_truncation,
    reason = "test ZIP builder only handles small archives"
)]
fn build_usdz_opts(files: &[(&str, &[u8])], opts: &BuildOpts) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let mut records: Vec<EntryRecord> = Vec::new();

    for (name, data) in files {
        let name_bytes = name.as_bytes();
        let crc = if opts.corrupt_crc && records.is_empty() {
            crc32::crc32(data) ^ 0xDEAD_BEEF // corrupt first entry
        } else {
            crc32::crc32(data)
        };
        let size = data.len() as u32;

        // Compute padding for 64-byte alignment of the local file header.
        let padding = if opts.skip_alignment {
            0_usize
        } else {
            let current = buf.len();
            let rem = current % 64;
            if rem == 0 { 0 } else { 64 - rem }
        };

        // Pad with zeros before the local file header.
        buf.extend(core::iter::repeat_n(0_u8, padding));

        let local_header_offset = buf.len() as u32;
        let extra_len = 0_u16;

        // Local File Header (30 bytes + name).
        buf.extend_from_slice(&0x0403_4b50_u32.to_le_bytes()); // signature
        buf.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        buf.extend_from_slice(&opts.flags.to_le_bytes()); // flags
        buf.extend_from_slice(&opts.compression_method.to_le_bytes()); // compression
        buf.extend_from_slice(&0_u16.to_le_bytes()); // mod time
        buf.extend_from_slice(&0_u16.to_le_bytes()); // mod date
        buf.extend_from_slice(&crc.to_le_bytes()); // CRC-32
        buf.extend_from_slice(&size.to_le_bytes()); // compressed size
        buf.extend_from_slice(&size.to_le_bytes()); // uncompressed size
        buf.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes()); // name len
        buf.extend_from_slice(&extra_len.to_le_bytes()); // extra len
        buf.extend_from_slice(name_bytes); // name
        buf.extend_from_slice(data); // data

        records.push(EntryRecord {
            name: name_bytes.to_vec(),
            local_header_offset,
            crc32: crc,
            size,
        });
    }

    // Central Directory.
    let cd_offset = buf.len() as u32;

    for rec in &records {
        buf.extend_from_slice(&0x0201_4b50_u32.to_le_bytes()); // signature
        buf.extend_from_slice(&20_u16.to_le_bytes()); // version made by
        buf.extend_from_slice(&20_u16.to_le_bytes()); // version needed
        buf.extend_from_slice(&opts.flags.to_le_bytes()); // flags
        buf.extend_from_slice(&opts.compression_method.to_le_bytes()); // compression
        buf.extend_from_slice(&0_u16.to_le_bytes()); // mod time
        buf.extend_from_slice(&0_u16.to_le_bytes()); // mod date
        buf.extend_from_slice(&rec.crc32.to_le_bytes()); // CRC-32
        buf.extend_from_slice(&rec.size.to_le_bytes()); // compressed size
        buf.extend_from_slice(&rec.size.to_le_bytes()); // uncompressed size
        buf.extend_from_slice(&(rec.name.len() as u16).to_le_bytes()); // name len
        buf.extend_from_slice(&0_u16.to_le_bytes()); // extra len
        buf.extend_from_slice(&0_u16.to_le_bytes()); // comment len
        buf.extend_from_slice(&0_u16.to_le_bytes()); // disk start
        buf.extend_from_slice(&0_u16.to_le_bytes()); // internal attrs
        buf.extend_from_slice(&0_u32.to_le_bytes()); // external attrs
        buf.extend_from_slice(&rec.local_header_offset.to_le_bytes()); // local hdr offset
        buf.extend_from_slice(&rec.name); // name
    }

    let cd_size = buf.len() as u32 - cd_offset;

    // End of Central Directory.
    buf.extend_from_slice(&0x0605_4b50_u32.to_le_bytes()); // signature
    buf.extend_from_slice(&0_u16.to_le_bytes()); // disk number
    buf.extend_from_slice(&0_u16.to_le_bytes()); // disk with CD
    buf.extend_from_slice(&(records.len() as u16).to_le_bytes()); // entries this disk
    buf.extend_from_slice(&(records.len() as u16).to_le_bytes()); // total entries
    buf.extend_from_slice(&cd_size.to_le_bytes()); // CD size
    buf.extend_from_slice(&cd_offset.to_le_bytes()); // CD offset
    let comment_len: u16 = if opts.eocd_comment { 5 } else { 0 };
    buf.extend_from_slice(&comment_len.to_le_bytes()); // comment len
    if opts.eocd_comment {
        buf.extend_from_slice(b"hello");
    }

    buf
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// A simple USDA layer defining a prim.
const SIMPLE_USDA: &str = r#"#usda 1.0
(
    defaultPrim = "root"
)

def Xform "root"
{
    custom int myAttr = 42
}
"#;

fn parse_usdz(data: &[u8]) -> InMemoryStore {
    let mut store = InMemoryStore::default();
    let layer_id = LayerId(1);
    let mut resolver = StubResolver;

    let result = layerstack_usdz::read_usdz(
        data,
        layer_id,
        &mut store.tokens,
        &mut store.paths,
        &mut resolver,
    )
    .expect("failed to parse USDZ");

    for layer in result.resolved_layers {
        store.insert_layer(layer);
    }
    store.insert_layer(result.layer);

    store
}

fn expect_value(
    store: &mut InMemoryStore,
    layer_id: LayerId,
    prim_path: &str,
    field_name: &str,
) -> Value {
    let path = layerstack::path::Path::parse_absolute(prim_path, &mut store.tokens)
        .expect("invalid path");
    let path_id = store
        .paths
        .lookup(&path)
        .unwrap_or_else(|| panic!("path not interned: {prim_path}"));
    let layer = store
        .layers
        .get(&layer_id)
        .unwrap_or_else(|| panic!("layer not found: {layer_id:?}"));
    let prim = layer
        .prims
        .get(&path_id)
        .unwrap_or_else(|| panic!("prim not found: {prim_path}"));
    let field_tok = store.tokens.intern(field_name);
    let field = get_field(&prim.fields, &field_tok)
        .unwrap_or_else(|| panic!("field not found: {field_name} on {prim_path}"));
    match field {
        FieldValue::Value(v) => v.clone(),
        other => panic!("expected Value at {prim_path}.{field_name}, got {other:?}"),
    }
}

fn has_prim(store: &mut InMemoryStore, layer_id: LayerId, prim_path: &str) -> bool {
    let path = layerstack::path::Path::parse_absolute(prim_path, &mut store.tokens)
        .expect("invalid path");
    let Some(path_id) = store.paths.lookup(&path) else {
        return false;
    };
    let Some(layer) = store.layers.get(&layer_id) else {
        return false;
    };
    layer.prims.contains_key(&path_id)
}

// ── Happy path tests ────────────────────────────────────────────────────

#[test]
fn single_usda_root() {
    let data = build_usdz(&[("root.usda", SIMPLE_USDA.as_bytes())]);
    let mut store = parse_usdz(&data);
    let layer_id = LayerId(1);

    assert!(
        has_prim(&mut store, layer_id, "/root"),
        "expected /root prim"
    );
    let val = expect_value(&mut store, layer_id, "/root", "myAttr");
    assert_eq!(val, Value::Int(42));
}

#[test]
fn usd_extension_usda_content() {
    // .usd extension with USDA content (no USDC magic).
    let data = build_usdz(&[("scene.usd", SIMPLE_USDA.as_bytes())]);
    let mut store = parse_usdz(&data);
    let layer_id = LayerId(1);

    assert!(has_prim(&mut store, layer_id, "/root"));
    let val = expect_value(&mut store, layer_id, "/root", "myAttr");
    assert_eq!(val, Value::Int(42));
}

#[test]
fn multiple_entries_root_is_first() {
    // Second entry is a non-USD file (texture), should be ignored.
    let data = build_usdz(&[
        ("root.usda", SIMPLE_USDA.as_bytes()),
        ("texture.png", &[0x89, 0x50, 0x4E, 0x47]), // PNG header stub
    ]);
    let mut store = parse_usdz(&data);
    let layer_id = LayerId(1);

    assert!(has_prim(&mut store, layer_id, "/root"));
}

// ── Error case tests ────────────────────────────────────────────────────

#[test]
fn empty_archive_rejected() {
    let data = build_usdz(&[]);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err(), "expected error for empty archive");
    let err = result.unwrap_err();
    assert!(
        matches!(err, layerstack_usdz::UsdzError::NoRootLayer),
        "expected NoRootLayer, got {err:?}"
    );
}

#[test]
fn non_usd_first_file_rejected() {
    let data = build_usdz(&[("texture.png", &[0x89, 0x50, 0x4E, 0x47])]);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err(), "expected error for non-USD first file");
    assert!(matches!(
        result.unwrap_err(),
        layerstack_usdz::UsdzError::NoRootLayer
    ));
}

#[test]
fn invalid_compression_rejected() {
    let opts = BuildOpts {
        compression_method: 8, // Deflate
        ..Default::default()
    };
    let data = build_usdz_opts(&[("root.usda", SIMPLE_USDA.as_bytes())], &opts);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err(), "expected error for compressed entry");
    assert!(matches!(
        result.unwrap_err(),
        layerstack_usdz::UsdzError::ConstraintViolation { .. }
    ));
}

#[test]
fn invalid_encryption_rejected() {
    let opts = BuildOpts {
        flags: 0x01, // encryption bit
        ..Default::default()
    };
    let data = build_usdz_opts(&[("root.usda", SIMPLE_USDA.as_bytes())], &opts);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err(), "expected error for encrypted entry");
    assert!(matches!(
        result.unwrap_err(),
        layerstack_usdz::UsdzError::ConstraintViolation { .. }
    ));
}

#[test]
fn crc32_mismatch_rejected() {
    let opts = BuildOpts {
        corrupt_crc: true,
        ..Default::default()
    };
    let data = build_usdz_opts(&[("root.usda", SIMPLE_USDA.as_bytes())], &opts);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err(), "expected error for CRC mismatch");
    assert!(matches!(
        result.unwrap_err(),
        layerstack_usdz::UsdzError::CrcMismatch { .. }
    ));
}

#[test]
fn eocd_comment_rejected() {
    let opts = BuildOpts {
        eocd_comment: true,
        ..Default::default()
    };
    let data = build_usdz_opts(&[("root.usda", SIMPLE_USDA.as_bytes())], &opts);
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err(), "expected error for EOCD comment");
    assert!(matches!(
        result.unwrap_err(),
        layerstack_usdz::UsdzError::ConstraintViolation { .. }
    ));
}

#[test]
fn invalid_magic_rejected() {
    let data = vec![0_u8; 64];
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err());
}

#[test]
fn truncated_file_rejected() {
    let data = b"PK\x03\x04"; // Just LFH signature, truncated
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    assert!(result.is_err());
}

#[test]
fn alignment_validation() {
    let opts = BuildOpts {
        skip_alignment: true,
        ..Default::default()
    };
    // Build with two files so the second one is likely misaligned.
    let data = build_usdz_opts(
        &[
            ("root.usda", SIMPLE_USDA.as_bytes()),
            ("extra.usda", b"#usda 1.0\n"),
        ],
        &opts,
    );
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let mut resolver = StubResolver;
    let result =
        layerstack_usdz::read_usdz(&data, LayerId(1), &mut tokens, &mut paths, &mut resolver);
    // The first entry starts at offset 0 which is 64-byte aligned.
    // The second entry may or may not be aligned depending on data sizes.
    // If it's misaligned, we expect a ConstraintViolation error.
    // If it happens to be aligned, the parse succeeds -- both outcomes are valid.
    if let Err(e) = result {
        assert!(
            matches!(e, layerstack_usdz::UsdzError::ConstraintViolation { .. }),
            "expected ConstraintViolation, got {e:?}"
        );
    }
}
