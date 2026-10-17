// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    PackageFile,
    snapshot::{PackageLimits, PackageSnapshot},
    write_usdz,
};
use alloc::vec;
use core::cell::RefCell;

struct Recorded {
    bytes: EncodedBytes,
    requests: RefCell<Vec<Range<u64>>>,
    fail_at: Option<u64>,
    short: bool,
}
impl Recorded {
    fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes: bytes.into(),
            requests: RefCell::new(Vec::new()),
            fail_at: None,
            short: false,
        }
    }
}
impl RangeSource for &Recorded {
    type Error = &'static str;
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn read_range(&self, range: Range<u64>) -> Result<EncodedBytes, Self::Error> {
        self.requests.borrow_mut().push(range.clone());
        if self.fail_at == Some(range.start) {
            return Err("offline");
        }
        let end = range.end - u64::from(self.short && range.start != range.end);
        Ok(self
            .bytes
            .slice(usize::try_from(range.start).unwrap()..usize::try_from(end).unwrap())
            .unwrap())
    }
}
fn package() -> Vec<u8> {
    write_usdz(&[
        PackageFile::new("root.usda", b"#usda 1.0\ndef \"P\" {}\n"),
        PackageFile::new("texture.png", &vec![7; 1024 * 1024]),
    ])
    .unwrap()
}
fn cd(bytes: &[u8]) -> usize {
    u32_at(bytes, bytes.len() - 6).unwrap() as usize
}
fn field(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn selection_reads_only_directory_headers_and_selected_payload() {
    let source = Recorded::new(package());
    let media = crate::zip::ZipArchive::parse(&source.bytes)
        .unwrap()
        .find("texture.png")
        .unwrap()
        .clone();
    let mut reader = RangePackage::open(&source, RangeLimits::default()).unwrap();
    assert_eq!(reader.stats().read_attempts, 2);
    assert_eq!(reader.stats().verification_bytes, 0);
    assert_eq!(
        reader.members().collect::<Vec<_>>(),
        vec!["root.usda", "texture.png"]
    );
    let root = reader
        .layer(reader.root_member().unwrap().to_string().as_str())
        .unwrap();
    let stats = reader.stats();
    assert!(stats.bytes_read < 512);
    assert_eq!(stats.verification_bytes, root.bytes.len() as u64);
    assert!(
        source
            .requests
            .borrow()
            .iter()
            .all(|range| range.end <= media.data_offset as u64
                || range.start >= (media.data_offset + media.size) as u64)
    );
    let clone = reader.clone();
    let again = reader.member("root.usda").unwrap();
    assert_eq!(
        reader.stats().read_attempts,
        stats.read_attempts + 1,
        "local header is reused"
    );
    assert_eq!(clone.stats(), stats, "clone work history is independent");
    drop(reader);
    drop(clone);
    drop(source);
    assert_eq!(again.bytes.as_ref(), root.bytes.as_ref());
}
#[test]
fn selected_crc_does_not_claim_unselected_integrity() {
    let mut bytes = package();
    let media = crate::zip::ZipArchive::parse(&bytes)
        .unwrap()
        .find("texture.png")
        .unwrap()
        .data_offset;
    bytes[media] ^= 1;
    assert!(matches!(
        PackageSnapshot::new(bytes.clone().into(), PackageLimits::default()),
        Err(UsdzError::CrcMismatch { .. })
    ));
    let mut reader = RangePackage::open(EncodedBytes::from(bytes), RangeLimits::default()).unwrap();
    reader.layer("root.usda").unwrap();
    assert!(matches!(
        reader.member("texture.png"),
        Err(RangeError::Format(UsdzError::CrcMismatch { .. }))
    ));
    assert!(reader.stats().verification_bytes > 1024 * 1024);
}
#[test]
fn nested_selection_translates_ranges_without_verifying_enclosing_payload() {
    let inner = package();
    let outer = write_usdz(&[
        PackageFile::new("root.usda", b"#usda 1.0\n"),
        PackageFile::new("inner.usdz", &inner),
    ])
    .unwrap();
    let source = Recorded::new(outer);
    let mut reader = RangePackage::open(&source, RangeLimits::default()).unwrap();
    let layer = reader.layer("inner.usdz").unwrap();
    assert_eq!(layer.path, "inner.usdz[root.usda]");
    assert_eq!(
        reader.containing_root(&layer.path).as_deref(),
        Some("inner.usdz[root.usda]")
    );
    assert_eq!(reader.stats().index_parses, 2);
    assert_eq!(reader.stats().entries, 4);
    assert!(reader.stats().bytes_read < 1024);
    assert_eq!(reader.stats().verification_bytes, layer.bytes.len() as u64);
    assert!(matches!(
        reader.member("inner.usdz[absent.png]"),
        Err(RangeError::Format(UsdzError::MissingMember { .. }))
    ));
    let mut reader = RangePackage::open(
        &source,
        RangeLimits {
            depth: 0,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(reader.layer("inner.usdz").is_err());
    assert_eq!(reader.stats().read_attempts, 2);
}
#[test]
fn budgets_reject_before_directory_or_payload_reads() {
    let source = Recorded::new(package());
    for limits in [
        RangeLimits {
            entries: 1,
            ..Default::default()
        },
        RangeLimits {
            directory_bytes: 1,
            ..Default::default()
        },
    ] {
        source.requests.borrow_mut().clear();
        assert!(RangePackage::open(&source, limits).is_err());
        assert_eq!(source.requests.borrow().len(), 1, "only EOCD was read");
    }
    for limits in [
        RangeLimits {
            member_bytes: 1,
            ..Default::default()
        },
        RangeLimits {
            verification_bytes: 1,
            ..Default::default()
        },
    ] {
        let mut reader = RangePackage::open(&source, limits).unwrap();
        assert!(reader.member("root.usda").is_err());
        assert_eq!(reader.stats().read_attempts, 2);
    }
    let mut reader = RangePackage::open(
        &source,
        RangeLimits {
            verification_bytes: 22,
            ..Default::default()
        },
    )
    .unwrap();
    let selected = reader.member("root.usda").unwrap();
    assert!(selected.bytes.len() <= 22);
    let before = reader.stats();
    assert!(reader.member("root.usda").is_err());
    assert_eq!(reader.stats(), before);
}
#[test]
fn source_errors_and_short_reads_are_distinct_from_missing_members() {
    let bytes = package();
    let length = bytes.len() as u64;
    let mut source = Recorded::new(bytes);
    source.fail_at = Some(length - 22);
    assert!(matches!(
        RangePackage::open(&source, RangeLimits::default()),
        Err(RangeError::Source("offline"))
    ));
    source.fail_at = None;
    source.short = true;
    assert!(matches!(
        RangePackage::open(&source, RangeLimits::default()),
        Err(RangeError::Format(UsdzError::UnexpectedEof))
    ));
    source.short = false;
    source.fail_at = Some(64);
    let mut reader = RangePackage::open(&source, RangeLimits::default()).unwrap();
    assert!(matches!(
        reader.member("root.usda"),
        Err(RangeError::Source("offline"))
    ));
}
#[test]
fn selected_local_header_must_agree_with_directory_and_stay_in_its_extent() {
    for offset in [6, 8, 14, 18, 22, 26, 28, 30] {
        let mut bytes = package();
        bytes[offset] ^= 1;
        assert!(
            crate::zip::ZipArchive::parse(&bytes).is_err(),
            "eager field {offset}"
        );
        let mut reader =
            RangePackage::open(EncodedBytes::from(bytes), RangeLimits::default()).unwrap();
        assert!(reader.layer("root.usda").is_err(), "field {offset}");
        assert_eq!(reader.stats().verification_bytes, 0);
    }
    let mut bytes = package();
    let directory = cd(&bytes);
    field(&mut bytes, directory + 46 + "root.usda".len() + 42, 1);
    let source = Recorded::new(bytes);
    let mut reader = RangePackage::open(&source, RangeLimits::default()).unwrap();
    assert!(reader.layer("root.usda").is_err());
    assert_eq!(
        reader.stats().read_attempts,
        2,
        "reject overlap before reading a local header"
    );
}
#[test]
fn rejects_malformed_directory_envelopes_and_names() {
    let bytes = package();
    let directory = cd(&bytes);
    let end = bytes.len() - 22;
    for offset in [
        end + 4,
        end + 6,
        end + 8,
        end + 10,
        end + 12,
        end + 16,
        end + 20,
        directory,
        directory + 8,
        directory + 10,
        directory + 20,
        directory + 34,
        directory + 42,
    ] {
        let mut malformed = bytes.clone();
        malformed[offset] ^= 1;
        assert!(
            RangePackage::open(EncodedBytes::from(malformed), RangeLimits::default()).is_err(),
            "field {offset}"
        );
    }
    for name in [
        b"../a.usda".as_slice(),
        b"/oot.usda",
        b"r\\ot.usda",
        b"root.usda",
    ] {
        let mut malformed = bytes.clone();
        if name == b"root.usda" {
            // Duplicate names with a differently sized second entry.
            let second = directory + 46 + "root.usda".len();
            malformed[second + 46..second + 46 + "texture.png".len()]
                .copy_from_slice(b"root.usda\0\0");
        } else {
            malformed[directory + 46..directory + 46 + 9].copy_from_slice(name);
        }
        assert!(RangePackage::open(EncodedBytes::from(malformed), RangeLimits::default()).is_err());
    }
}

/// Independent fixed-layout stored ZIP fixture; no use of the production writer.
fn independent(reverse: bool) -> Vec<u8> {
    let mut bytes = vec![0; 256];
    let names = ["root.usda", "later.usda"];
    let mut directory = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let header = i * 128;
        let data = header + 64;
        let crc = crate::crc32::crc32(b"#usda 1.0\n");
        field(&mut bytes, header, 0x0403_4b50);
        field(&mut bytes, header + 14, crc);
        field(&mut bytes, header + 18, 10);
        field(&mut bytes, header + 22, 10);
        bytes[header + 26..header + 28]
            .copy_from_slice(&(u16::try_from(name.len()).unwrap()).to_le_bytes());
        bytes[header + 28..header + 30]
            .copy_from_slice(&(u16::try_from(34 - name.len()).unwrap()).to_le_bytes());
        bytes[header + 30..header + 30 + name.len()].copy_from_slice(name.as_bytes());
        bytes[data..data + 10].copy_from_slice(b"#usda 1.0\n");
        let mut entry = vec![0; 46 + name.len()];
        field(&mut entry, 0, 0x0201_4b50);
        field(&mut entry, 16, crc);
        field(&mut entry, 20, 10);
        field(&mut entry, 24, 10);
        entry[28..30].copy_from_slice(&(u16::try_from(name.len()).unwrap()).to_le_bytes());
        field(&mut entry, 42, u32::try_from(header).unwrap());
        entry[46..].copy_from_slice(name.as_bytes());
        directory.push(entry);
    }
    if reverse {
        directory.reverse();
    }
    let size = directory.iter().map(Vec::len).sum::<usize>();
    for entry in directory {
        bytes.extend_from_slice(&entry);
    }
    let mut end = vec![0; 22];
    field(&mut end, 0, 0x0605_4b50);
    end[8..10].copy_from_slice(&2_u16.to_le_bytes());
    end[10..12].copy_from_slice(&2_u16.to_le_bytes());
    field(&mut end, 12, u32::try_from(size).unwrap());
    field(&mut end, 16, 256);
    bytes.extend_from_slice(&end);
    bytes
}
#[test]
fn physical_first_member_is_default_even_if_directory_order_differs() {
    for reverse in [false, true] {
        let bytes: EncodedBytes = independent(reverse).into();
        let complete = PackageSnapshot::new(bytes.clone(), PackageLimits::default()).unwrap();
        assert_eq!(complete.root_member(), Some("root.usda"));
        let mut reader = RangePackage::open(bytes, RangeLimits::default()).unwrap();
        assert_eq!(reader.root_member(), Some("root.usda"));
        assert_eq!(
            reader.layer("later.usda").unwrap().bytes.as_ref(),
            b"#usda 1.0\n"
        );
    }
}

#[test]
fn dependency_transport_failure_reaches_importer_without_outer_fallback() {
    use layerstack::{
        AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
    };
    struct Outside;
    impl AssetResolver for Outside {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &TokenInterner,
            _: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            panic!("found package member must not fall back")
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
        fn allocate_layer_id(&mut self) -> Option<LayerId> {
            Some(LayerId(2))
        }
    }
    let bytes = write_usdz(&[
        PackageFile::new("root.usda", b"#usda 1.0\n(subLayers = [@child.usda@])\n"),
        PackageFile::new("child.usda", b"#usda 1.0\ndef \"C\" {}\n"),
    ])
    .unwrap();
    let child = crate::zip::ZipArchive::parse(&bytes)
        .unwrap()
        .find("child.usda")
        .unwrap()
        .data_offset;
    let mut source = Recorded::new(bytes);
    source.fail_at = Some(child as u64);
    let mut reader = RangePackage::open(&source, RangeLimits::default()).unwrap();
    let result = crate::read_usdz_ranges(
        &mut reader,
        None,
        LayerId(1),
        &TokenInterner::default(),
        &PathInterner::default(),
        &mut Outside,
        &[],
        crate::UsdcReadOptions::default(),
    );
    assert!(matches!(result, Err(RangeError::Source("offline"))));
}

#[test]
fn repeated_import_selections_reuse_verified_layer_identity_without_payload_reads() {
    use layerstack::{
        AssetResolveError, AssetResolver, LayerId, PathInterner, ResolvedAsset, TokenInterner,
    };
    struct Outside;
    impl AssetResolver for Outside {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &TokenInterner,
            _: &PathInterner,
        ) -> Result<ResolvedAsset, AssetResolveError> {
            panic!("package selection needs no outer read")
        }
        fn resolved_path(&self, _: LayerId) -> Option<&str> {
            None
        }
        fn allocate_layer_id(&mut self) -> Option<LayerId> {
            Some(LayerId(2))
        }
    }
    let root = b"#usda 1.0\n";
    let child = b"#usda 1.0\ndef \"C\" {}\n";
    for nested in [false, true] {
        let inner = write_usdz(&[PackageFile::new("child.usda", child)]).unwrap();
        let bytes = if nested {
            write_usdz(&[
                PackageFile::new("root.usda", root),
                PackageFile::new("inner.usdz", &inner),
            ])
            .unwrap()
        } else {
            write_usdz(&[
                PackageFile::new("root.usda", root),
                PackageFile::new("child.usda", child),
            ])
            .unwrap()
        };
        let source = Recorded::new(bytes);
        let mut reader = RangePackage::open(
            &source,
            RangeLimits {
                verification_bytes: (root.len() + child.len()) as u64,
                ..Default::default()
            },
        )
        .unwrap();
        let members = if nested {
            ["inner.usdz", "inner.usdz[child.usda]", "inner.usdz"]
        } else {
            ["child.usda", "child.usda", "child.usda"]
        };
        let result = crate::read_usdz_ranges(
            &mut reader,
            None,
            LayerId(1),
            &TokenInterner::default(),
            &PathInterner::default(),
            &mut Outside,
            &members,
            crate::UsdcReadOptions::default(),
        )
        .unwrap();
        assert_eq!(result.resolved_layers.len(), 1);
        assert_eq!(
            result.member_paths[&LayerId(2)].as_ref(),
            if nested {
                "inner.usdz[child.usda]"
            } else {
                "child.usda"
            }
        );
        assert_eq!(
            reader.stats().verification_bytes,
            (root.len() + child.len()) as u64
        );
    }
}
