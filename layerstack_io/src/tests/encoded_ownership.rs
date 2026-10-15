// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;

#[derive(Default)]
struct Transferred {
    memory: Memory,
    pointers: BTreeMap<String, usize>,
}
impl Storage for Transferred {
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError> {
        self.memory.identify(asset, anchor)
    }
    fn read(&mut self, identifier: &str) -> Result<Vec<u8>, IoError> {
        let bytes = self.memory.read(identifier)?;
        self.pointers
            .insert(identifier.into(), bytes.as_ptr() as usize);
        Ok(bytes)
    }
    fn read_bounded(&mut self, identifier: &str, limit: u64) -> Result<Vec<u8>, IoError> {
        let bytes = self.memory.read_bounded(identifier, limit)?;
        self.pointers
            .insert(identifier.into(), bytes.as_ptr() as usize);
        Ok(bytes)
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.memory.write(identifier, bytes)
    }
}

#[test]
fn transport_vectors_reach_package_and_resource_views_without_a_payload_copy() {
    let bytes = layerstack_usdz::write_usdz(&[
        layerstack_usdz::PackageFile::new("root.usda", b"#usda 1.0\ndef \"P\" {}\n"),
        layerstack_usdz::PackageFile::new("image.png", b"package pixels"),
    ])
    .unwrap();
    let offset = layerstack_usdz::zip::ZipArchive::parse(&bytes)
        .unwrap()
        .find("image.png")
        .unwrap()
        .data_offset;
    let mut memory = Memory::with(&[("loose.png", "loose pixels")]);
    memory.files.insert("scene.usdz".into(), bytes);
    let mut document = StageDocument::open(
        Transferred {
            memory,
            ..Default::default()
        },
        "scene.usdz",
        StageOptions::default(),
    )
    .unwrap();
    let package_pointer = document.storage().pointers["scene.usdz"];
    let image = document
        .read_asset_bytes("scene.usdz[image.png]", None)
        .unwrap();
    assert_eq!(
        image.bytes.as_ptr() as usize,
        package_pointer + offset,
        "package views retain the transport's payload allocation"
    );
    let loose = document.read_asset_bytes("loose.png", None).unwrap();
    assert_eq!(
        loose.bytes.as_ptr() as usize,
        document.storage().pointers["loose.png"],
        "loose resource handoff preserves the transport allocation"
    );
    let bounded = document
        .read_asset_bytes_bounded(
            "loose.png",
            None,
            AssetReadLimits {
                bytes: 64,
                package_bytes: 4096,
            },
        )
        .unwrap();
    assert_eq!(
        bounded.bytes.as_ptr() as usize,
        document.storage().pointers["loose.png"],
        "bounded handoff preserves the transport allocation"
    );
    assert_eq!(
        document.package_cache_stats().encoded_bytes_copied,
        0,
        "the document layer copies no encoded payload bytes after transport"
    );
    drop(document);
    assert_eq!(
        image.bytes.as_ref(),
        b"package pixels",
        "package views outlive document/source owners"
    );
    assert_eq!(
        loose.bytes.as_ref(),
        b"loose pixels",
        "loose views outlive their document"
    );
}

#[derive(Clone)]
struct Shared {
    bytes: EncodedBytes,
}
impl Storage for Shared {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&mut self, _: &str) -> Result<Vec<u8>, IoError> {
        panic!("encoded cache hit must not require Vec materialization")
    }
    fn read_encoded(&mut self, _: &str) -> Result<EncodedBytes, IoError> {
        Ok(self.bytes.clone())
    }
    fn write(&mut self, _: &str, _: &[u8]) -> Result<(), IoError> {
        Err(IoError::new(
            IoErrorKind::Unsupported,
            "read-only test cache",
        ))
    }
}
#[test]
fn shared_transport_override_and_unsupported_bounded_reads_keep_their_contracts() {
    let bytes = EncodedBytes::from(b"#usda 1.0\ndef \"P\" {}\n".to_vec());
    let mut document = StageDocument::open(
        Shared {
            bytes: bytes.clone(),
        },
        "root.usda",
        StageOptions::default(),
    )
    .unwrap();
    let resource = document.read_asset_bytes("raw.bin", None).unwrap();
    assert!(
        resource.bytes.shares_backing(&bytes),
        "cached transport can return an existing immutable owner"
    );
    let error = document
        .read_asset_bytes_bounded(
            "raw.bin",
            None,
            AssetReadLimits {
                bytes: 64,
                package_bytes: 64,
            },
        )
        .unwrap_err();
    assert_eq!(
        error.kind,
        IoErrorKind::Unsupported,
        "unsupported bounds never fall back to an unbounded read"
    );
}
