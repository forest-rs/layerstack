// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Validated immutable package directories and shared encoded member views.
use crate::{
    UsdzError,
    zip::{ZipArchive, ZipEntry},
};
use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
};
use layerstack::{asset::PackagePath, encoded_bytes::EncodedBytes};

/// Explicit limits for a complete package snapshot, including nested packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackageLimits {
    /// Maximum nested archive levels beneath the outer archive, at most 31.
    /// A final layer/resource member adds the 32nd package identifier level.
    pub depth: usize,
    /// Maximum total directory entries across all archive levels.
    pub entries: usize,
    /// Maximum bytes inspected for checksums, including nested archive checks.
    /// Nested bytes count at each level where a checksum inspects them.
    pub verification_bytes: u64,
}
impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            depth: 31,
            entries: 1_000_000,
            verification_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}
/// Work performed to construct one immutable, completely verified snapshot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PackageStats {
    /// Directories parsed once, including nested packages.
    pub index_parses: usize,
    /// Total indexed entries.
    pub entries: usize,
    /// Bytes inspected for CRC verification.
    pub verification_bytes: u64,
    /// Unique encoded backing allocation bytes retained by this tree.
    pub backing_bytes: usize,
}
/// One immutable archive tree. Each directory is parsed and verified once.
/// Members retain checked ranges into one encoded owner, including nested
/// archives. Clones share indexes; a replacement snapshot cannot alter old views.
/// Strict verification includes unused members and unused nested packages.
#[derive(Clone, Debug)]
pub struct PackageSnapshot {
    bytes: EncodedBytes,
    entries: BTreeMap<Arc<str>, ZipEntry>,
    root: Option<Arc<str>>,
    nested: BTreeMap<Arc<str>, Arc<Self>>,
    stats: PackageStats,
}
/// A selected member and its normalized physical path within the outer archive.
#[derive(Clone, Debug)]
pub struct PackageMember {
    /// Nested package/member path, without an outer host identifier.
    pub path: String,
    /// Immutable shared encoded view.
    pub bytes: EncodedBytes,
}
impl PackageSnapshot {
    /// Parses all directories, checks constraints and verifies every CRC under
    /// explicit cumulative limits. AOUSD Core §9.7, §16.4; USDZ “USD Constraints”.
    pub fn new(bytes: EncodedBytes, limits: PackageLimits) -> Result<Self, UsdzError> {
        if limits.depth > 31 {
            return Err(UsdzError::ConstraintViolation {
                reason: "package depth limit exceeds supported identifier depth",
            });
        }
        let mut stats = PackageStats {
            backing_bytes: bytes.backing_bytes(),
            ..PackageStats::default()
        };
        let mut snapshot = Self::build(bytes, limits, 0, &mut stats)?;
        snapshot.stats = stats;
        Ok(snapshot)
    }
    fn build(
        bytes: EncodedBytes,
        limits: PackageLimits,
        depth: usize,
        stats: &mut PackageStats,
    ) -> Result<Self, UsdzError> {
        if depth > limits.depth {
            return Err(UsdzError::ConstraintViolation {
                reason: "package nesting exceeds depth budget",
            });
        }
        let archive = ZipArchive::parse_with_entry_limit(
            &bytes,
            limits.entries.saturating_sub(stats.entries),
        )?;
        stats.index_parses += 1;
        stats.entries = stats.entries.checked_add(archive.entries().len()).ok_or(
            UsdzError::ConstraintViolation {
                reason: "package entry budget overflow",
            },
        )?;
        if stats.entries > limits.entries {
            return Err(UsdzError::ConstraintViolation {
                reason: "package entry budget exceeded",
            });
        }
        let root = archive
            .entries()
            .iter()
            // OpenUSD SdfUsdzFileFormat selects SdfZipFile::begin(), which walks
            // physical local headers. Central-directory ordering is independent.
            // AOUSD Core §16.4: first file is the root layer.
            .min_by_key(|entry| entry.data_offset)
            .filter(|e| crate::is_usd_extension(&e.name))
            .map(|e| e.name.clone());
        let mut entries = BTreeMap::new();
        let mut nested = BTreeMap::new();
        for entry in archive.entries() {
            let validated = PackagePath::parse(&alloc::format!("package[{}]", entry.name), 1)
                .map_err(|reason| UsdzError::ConstraintViolation { reason })?;
            if validated.members()[0] != *entry.name {
                return Err(UsdzError::ConstraintViolation {
                    reason: "noncanonical package entry name",
                });
            }
            stats.verification_bytes = stats
                .verification_bytes
                .checked_add(entry.size as u64)
                .ok_or(UsdzError::ConstraintViolation {
                    reason: "package verification budget overflow",
                })?;
            if stats.verification_bytes > limits.verification_bytes {
                return Err(UsdzError::ConstraintViolation {
                    reason: "package verification budget exceeded",
                });
            }
            let data = bytes
                .slice(entry.data_offset..entry.data_offset + entry.size)
                .ok_or(UsdzError::UnexpectedEof)?;
            let actual = crate::crc32::crc32(&data);
            if actual != entry.crc32 {
                return Err(UsdzError::CrcMismatch {
                    entry: entry.name.clone(),
                    expected: entry.crc32,
                    actual,
                });
            }
            if entry.name.ends_with(".usdz") {
                nested.insert(
                    entry.name.clone(),
                    Arc::new(Self::build(data, limits, depth + 1, stats)?),
                );
            }
            if entries.insert(entry.name.clone(), entry.clone()).is_some() {
                return Err(UsdzError::ConstraintViolation {
                    reason: "duplicate package member name",
                });
            }
        }
        Ok(Self {
            bytes,
            entries,
            root,
            nested,
            stats: PackageStats::default(),
        })
    }
    /// Complete construction work and encoded owner size; selection copies zero bytes.
    pub fn stats(&self) -> PackageStats {
        self.stats
    }
    /// First physical entry when it is a native USD layer. Later layers never become a default.
    pub fn root_member(&self) -> Option<&str> {
        self.root.as_deref()
    }
    /// Selects arbitrary bytes, validating every level. Never performs I/O.
    pub fn member(&self, path: &str) -> Result<PackageMember, UsdzError> {
        self.select(path, false)
    }
    /// Selects a layer; a selected package names its first-entry default layer.
    pub fn layer(&self, path: &str) -> Result<PackageMember, UsdzError> {
        self.select(path, true)
    }
    fn select(&self, path: &str, layer: bool) -> Result<PackageMember, UsdzError> {
        let parsed = PackagePath::parse(path, 32)
            .map_err(|reason| UsdzError::ConstraintViolation { reason })?;
        let mut names = alloc::vec![parsed.outer().to_string()];
        names.extend_from_slice(parsed.members());
        let mut archive = self;
        for name in &names[..names.len() - 1] {
            archive =
                archive
                    .nested
                    .get(name.as_str())
                    .ok_or_else(|| UsdzError::MissingMember {
                        member: Arc::from(path),
                    })?;
        }
        let mut name = names.last().expect("nonempty path").clone();
        if layer && let Some(inner) = archive.nested.get(name.as_str()) {
            name = inner
                .root_member()
                .ok_or(UsdzError::NoRootLayer)?
                .to_string();
            names.push(name.clone());
            archive = inner;
        }
        let entry = archive
            .entries
            .get(name.as_str())
            .ok_or_else(|| UsdzError::MissingMember {
                member: Arc::from(path),
            })?;
        Ok(PackageMember {
            path: PackagePath::join(&names[0], &names[1..]),
            bytes: archive
                .bytes
                .slice(entry.data_offset..entry.data_offset + entry.size)
                .ok_or(UsdzError::UnexpectedEof)?,
        })
    }
    /// Default layer in the same containing package as this physical member.
    pub fn containing_root(&self, member: &str) -> Option<String> {
        let path = PackagePath::parse(member, 32).ok()?;
        let mut archive = self;
        let mut names = alloc::vec![path.outer().to_string()];
        names.extend_from_slice(path.members());
        for name in &names[..names.len() - 1] {
            archive = archive.nested.get(name.as_str())?;
        }
        *names.last_mut()? = archive.root_member()?.to_string();
        Some(PackagePath::join(&names[0], &names[1..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PackageFile, write_usdz};
    #[test]
    fn nested_indexes_share_storage_and_obey_cumulative_limits() {
        let inner = write_usdz(&[
            PackageFile::new("dir/root.usda", b"#usda 1.0"),
            PackageFile::new("data.png", b"pixels"),
        ])
        .unwrap();
        let middle = write_usdz(&[
            PackageFile::new("root.usda", b"#usda 1.0"),
            PackageFile::new("inner.usdz", &inner),
            PackageFile::new("data.png", b"other pixels"),
        ])
        .unwrap();
        let outer = write_usdz(&[
            PackageFile::new("root.usda", b"#usda 1.0"),
            PackageFile::new("middle.usdz", &middle),
        ])
        .unwrap();
        let owner: EncodedBytes = Arc::<[u8]>::from(outer).into();
        let snapshot = PackageSnapshot::new(owner.clone(), PackageLimits::default()).unwrap();
        let leaf = snapshot
            .member("middle.usdz[inner.usdz[data.png]]")
            .unwrap();
        assert_eq!(leaf.bytes.as_ref(), b"pixels");
        assert!(leaf.bytes.shares_backing(&owner));
        assert_eq!(
            snapshot
                .member("middle.usdz[data.png]")
                .unwrap()
                .bytes
                .as_ref(),
            b"other pixels"
        );
        assert_eq!(
            snapshot.layer("middle.usdz[inner.usdz]").unwrap().path,
            "middle.usdz[inner.usdz[dir/root.usda]]"
        );
        assert_eq!(
            snapshot.containing_root(&leaf.path).unwrap(),
            "middle.usdz[inner.usdz[dir/root.usda]]"
        );
        assert_eq!(snapshot.stats().index_parses, 3);
        assert_eq!(snapshot.stats().entries, 7);
        assert_eq!(snapshot.stats().backing_bytes, owner.len());
        for limits in [
            PackageLimits {
                depth: 1,
                ..Default::default()
            },
            PackageLimits {
                entries: 6,
                ..Default::default()
            },
            PackageLimits {
                verification_bytes: 1,
                ..Default::default()
            },
        ] {
            assert!(PackageSnapshot::new(owner.clone(), limits).is_err());
        }
        assert!(
            snapshot
                .member("middle.usdz[missing.usdz[data.png]]")
                .is_err()
        );
        assert!(
            snapshot
                .member("middle.usdz[inner.usdz[../data.png]]")
                .is_err()
        );
        assert!(snapshot.member("middle.usdz[data.png[x]]").is_err());
    }
}
