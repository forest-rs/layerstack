// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arbitrary asset transport using the document's layer and package provenance.

use super::*;

/// Where resolved asset bytes came from; no format decoding is implied.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum AssetByteSource {
    /// Bytes supplied by the host storage backend.
    Storage,
    /// Bytes selected from a USDZ archive.
    PackageMember {
        /// Canonical identifier of the containing archive.
        package: String,
        /// Normalized archive-relative member path.
        member: String,
    },
}

/// Resolved identity and complete bytes for an arbitrary asset.
///
/// Keeps image/environment assets independent of any renderer or image decoder.
/// Package members use the same `package[member]` identity as imported layers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AssetBytes {
    /// Canonical resolved identity, suitable for host cache keys.
    pub identifier: String,
    /// Complete immutable bytes. Cloning this result shares this allocation.
    pub bytes: EncodedBytes,
    /// Transport/package provenance of these bytes.
    pub source: AssetByteSource,
    /// Layer relative to which the authored asset was resolved, if supplied.
    pub anchor: Option<LayerId>,
}

/// Explicit allocation limits for resource and nonresident archive transport.
///
/// Build it as a struct literal; it has no default, so a literal names
/// every field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssetReadLimits {
    /// Maximum returned resource bytes, checked before returning a shared package view.
    pub bytes: u64,
    /// Maximum archive bytes when an explicit package is not already resident.
    /// Resident source snapshots need no transport allocation and bypass this limit.
    pub package_bytes: u64,
}

impl<B: Storage> StageDocument<B> {
    /// Resolves and reads arbitrary bytes relative to their authoring layer.
    ///
    /// Uses resident USDZ snapshots before host transport. Explicit `./` or `../`
    /// package paths never fall through to external storage; search paths try the
    /// authoring member, package root, then the outer resolver. Explicit
    /// `package.usdz[member]` identifiers support nested archives.
    /// Unknown/anonymous anchors fail rather than silently resolve elsewhere.
    /// No layers are imported and no bindings or dirty generations are changed.
    /// AOUSD Core §9.4, §9.7; OpenUSD `ArResolver::OpenAsset` and package resolution.
    pub fn read_asset_bytes(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
    ) -> Result<AssetBytes, IoError> {
        read(
            &mut self.storage,
            &mut self.catalog,
            asset,
            anchor,
            None,
            self.package_limits,
        )
    }
    /// Resolves opaque bytes with limits checked before payload allocation.
    /// Loose resources require `Storage::read_bounded`; resident package members
    /// check their existing slice length before returning a shared view.
    /// Nonresident explicit packages also obey `limits.package_bytes`.
    /// No unbounded fallback occurs. AOUSD Core §9.4, §9.7.
    pub fn read_asset_bytes_bounded(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        limits: AssetReadLimits,
    ) -> Result<AssetBytes, IoError> {
        read(
            &mut self.storage,
            &mut self.catalog,
            asset,
            anchor,
            Some(limits),
            self.package_limits,
        )
    }
}

impl<B: Storage> PreparedReload<'_, B> {
    /// Resolves arbitrary bytes using the candidate's source provenance.
    ///
    /// In particular, USDZ reads see the candidate archive, not the previously
    /// published package. Retain the returned bytes for the renderer publication;
    /// commit never rereads them. Loose external assets are read at call time.
    pub fn read_asset_bytes(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
    ) -> Result<AssetBytes, IoError> {
        read(
            &mut self.document.storage,
            &mut self.catalog,
            asset,
            anchor,
            None,
            self.document.package_limits,
        )
    }
    /// Resolves opaque bytes with limits checked before payload allocation.
    /// Loose resources require `Storage::read_bounded`; resident package members
    /// check their existing slice length before returning a shared view.
    /// Nonresident explicit packages also obey `limits.package_bytes`.
    /// No unbounded fallback occurs. AOUSD Core §9.4, §9.7.
    pub fn read_asset_bytes_bounded(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        limits: AssetReadLimits,
    ) -> Result<AssetBytes, IoError> {
        read(
            &mut self.document.storage,
            &mut self.catalog,
            asset,
            anchor,
            Some(limits),
            self.document.package_limits,
        )
    }
}

/// Same member selection as layer import, without importing or allocating layer IDs.
impl Catalog {
    pub(super) fn package_member(
        &self,
        archive: &PackageSnapshot,
        asset: &str,
        anchor: LayerId,
        package: LayerId,
    ) -> Result<Option<String>, IoError> {
        let authoring = if anchor == package {
            self.package_roots[&package].clone()
        } else {
            package_member_path(&self.sources[&anchor].identifier).expect("package source")
        };
        let root = archive.containing_root(&authoring).ok_or_else(|| {
            IoError::new(
                IoErrorKind::Rejected,
                "package anchor has no containing default layer",
            )
        })?;
        let path = asset.replace('\\', "/");
        let mut valid = false;
        for anchor in core::iter::once(authoring.as_str())
            .chain((!path.starts_with('.')).then_some(root.as_str()))
        {
            if let Some(member) = PackagePath::anchor_member(&path, anchor) {
                valid = true;
                if archive.member(&member).is_ok() {
                    return Ok(Some(member));
                }
            }
        }
        if !valid {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "asset path escapes its containing package",
            ));
        }
        Ok(None)
    }
}

pub(super) fn read<B: Storage>(
    storage: &mut B,
    catalog: &mut Catalog,
    asset: &str,
    anchor: Option<LayerId>,
    limits: Option<AssetReadLimits>,
    package_limits: PackageLimits,
) -> Result<AssetBytes, IoError> {
    let source = anchor
        .map(|id| {
            catalog.sources.get(&id).ok_or_else(|| {
                IoError::new(
                    IoErrorKind::MissingLayer,
                    "asset anchor has no source identifier",
                )
            })
        })
        .transpose()?;
    let package = source.and_then(|s| s.package);
    let outer_anchor = package
        .map(|id| catalog.package_identifier(id))
        .or_else(|| source.map(|s| &*s.identifier))
        .map(ToString::to_string);
    if let Some(package) = package
        && !asset.starts_with('/')
        && !asset.contains(':')
    {
        catalog.cache_work.index_hits += 1;
        let archive = &catalog.package_bytes[&package];
        if let Some(member) =
            catalog.package_member(archive, asset, anchor.expect("package anchor"), package)?
        {
            return package_entry(
                archive,
                catalog.package_identifier(package),
                &member,
                anchor,
                limits,
            );
        }
        if asset.starts_with('.') {
            return Err(IoError::new(
                IoErrorKind::NotFound,
                alloc::format!("missing package-relative asset {asset}"),
            ));
        }
    }
    let path = PackagePath::parse(asset, 32)
        .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
    let identifier = storage
        .identify(path.outer(), outer_anchor.as_deref())
        .map_err(|e| e.at(path.outer()))?;
    if let Some((first, rest)) = path.members().split_first() {
        let member = PackagePath::join(first, rest);
        let resident = catalog
            .ids
            .get(identifier.as_str())
            .and_then(|id| catalog.package_bytes.get(id))
            .cloned();
        let archive = match resident {
            Some(archive) => {
                catalog.cache_work.index_hits += 1;
                archive
            }
            None => {
                let bytes = match limits {
                    Some(limits) => {
                        storage.read_encoded_bounded(&identifier, limits.package_bytes)?
                    }
                    None => storage
                        .read_encoded(&identifier)
                        .map_err(|e| e.at(&identifier))?,
                };
                let archive = Arc::new(
                    PackageSnapshot::new(bytes, package_limits)
                        .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?,
                );
                catalog.cache_work.opens += 1;
                catalog.cache_work.index_parses += archive.stats().index_parses;
                archive
            }
        };
        return package_entry(&archive, &identifier, &member, anchor, limits);
    }
    let bytes = match limits {
        Some(limits) => storage.read_encoded_bounded(&identifier, limits.bytes)?,
        None => storage
            .read_encoded(&identifier)
            .map_err(|e| e.at(&identifier))?,
    };
    Ok(AssetBytes {
        bytes,
        identifier,
        source: AssetByteSource::Storage,
        anchor,
    })
}

pub(super) fn package_entry(
    archive: &PackageSnapshot,
    package: &str,
    member: &str,
    anchor: Option<LayerId>,
    limits: Option<AssetReadLimits>,
) -> Result<AssetBytes, IoError> {
    let selected = archive.member(member).map_err(|e| {
        IoError::new(
            match e {
                layerstack_usdz::UsdzError::MissingMember { .. } => IoErrorKind::NotFound,
                _ => IoErrorKind::Rejected,
            },
            e.to_string(),
        )
    })?;
    if limits.is_some_and(|limits| selected.bytes.len() as u64 > limits.bytes) {
        return Err(IoError::new(
            IoErrorKind::Rejected,
            "resource exceeds encoded byte budget",
        ));
    }
    Ok(AssetBytes {
        identifier: alloc::format!("{package}[{}]", selected.path),
        bytes: selected.bytes,
        source: AssetByteSource::PackageMember {
            package: package.into(),
            member: selected.path,
        },
        anchor,
    })
}
