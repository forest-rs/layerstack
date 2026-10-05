// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Arbitrary asset transport using the document's layer and package provenance.

use super::*;

/// Where resolved asset bytes came from; no format decoding is implied.
#[derive(Clone, Debug, PartialEq, Eq)]
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
pub struct AssetBytes {
    /// Canonical resolved identity, suitable for host cache keys.
    pub identifier: String,
    /// Complete immutable bytes. Cloning this result shares this allocation.
    pub bytes: Arc<[u8]>,
    /// Transport/package provenance of these bytes.
    pub source: AssetByteSource,
    /// Layer relative to which the authored asset was resolved, if supplied.
    pub anchor: Option<LayerId>,
}

impl<B: Storage> StageDocument<B> {
    /// Resolves and reads arbitrary bytes relative to their authoring layer.
    ///
    /// Uses resident USDZ snapshots before host transport. Explicit `./` or `../`
    /// package paths never fall through to external storage; search paths try the
    /// authoring member, package root, then the outer resolver. Explicit
    /// `package.usdz[member]` identifiers are supported for one archive level.
    /// Unknown/anonymous anchors fail rather than silently resolve elsewhere.
    /// No layers are imported and no bindings or dirty generations are changed.
    /// AOUSD Core §9.4, §9.7; OpenUSD `ArResolver::OpenAsset` and package resolution.
    pub fn read_asset_bytes(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
    ) -> Result<AssetBytes, IoError> {
        read(&mut self.storage, &self.catalog, asset, anchor)
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
        read(&mut self.document.storage, &self.catalog, asset, anchor)
    }
}

/// Same member selection as layer import, without importing or allocating layer IDs.
impl Catalog {
    pub(super) fn package_member(
        &self,
        archive: &layerstack_usdz::zip::ZipArchive<'_>,
        asset: &str,
        anchor: LayerId,
        package: LayerId,
    ) -> Result<Option<String>, IoError> {
        let root_member = &self.package_roots[&package];
        let authoring = if anchor == package {
            root_member.as_str()
        } else {
            self.sources[&anchor]
                .identifier
                .strip_suffix(']')
                .and_then(|s| s.rsplit_once('[').map(|(_, m)| m))
                .expect("package member source")
        };
        let path = asset.replace('\\', "/");
        let candidates = core::iter::once(authoring)
            .chain((!path.starts_with('.')).then_some(root_member.as_str()))
            .filter_map(|anchor| {
                layerstack::asset::anchor_asset_path(
                    &alloc::format!("./{path}"),
                    &alloc::format!("./{anchor}"),
                )
                .and_then(|p| p.strip_prefix("./").map(ToString::to_string))
                .filter(|p| !p.starts_with("../"))
            });
        Ok(candidates
            .into_iter()
            .find(|member| archive.find(member).is_some()))
    }
}

fn read<B: Storage>(
    storage: &mut B,
    catalog: &Catalog,
    asset: &str,
    anchor: Option<LayerId>,
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
        .and_then(|id| catalog.sources.get(&id))
        .or(source)
        .map(|s| s.identifier.as_str());
    if let Some((outer, member)) = asset.split_once('[') {
        let member = member
            .strip_suffix(']')
            .filter(|m| !m.contains(['[', ']']))
            .ok_or_else(|| {
                IoError::new(
                    IoErrorKind::Unsupported,
                    "requires a single package[member] identifier",
                )
            })?;
        let identifier = storage.identify(outer, outer_anchor)?;
        let member = layerstack::asset::normalize_asset_path(&member.replace('\\', "/"));
        let member = member.strip_prefix("./").unwrap_or(&member);
        if member.is_empty()
            || member.starts_with('/')
            || member == ".."
            || member.starts_with("../")
        {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "asset member must remain inside its package",
            ));
        }
        let bytes = catalog
            .ids
            .get(&identifier)
            .and_then(|id| catalog.package_bytes.get(id))
            .cloned();
        let bytes = match bytes {
            Some(bytes) => bytes,
            None => Arc::from(storage.read(&identifier)?),
        };
        return package_bytes(&bytes, &identifier, member, anchor);
    }
    if let Some(package) = package
        && !asset.starts_with('/')
        && !asset.contains(':')
    {
        let archive = layerstack_usdz::zip::ZipArchive::parse(&catalog.package_bytes[&package])
            .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
        if let Some(member) =
            catalog.package_member(&archive, asset, anchor.expect("package anchor"), package)?
        {
            return package_entry(
                &archive,
                &catalog.sources[&package].identifier,
                &member,
                anchor,
            );
        }
        if asset.starts_with('.') {
            return Err(IoError::new(
                IoErrorKind::NotFound,
                alloc::format!("missing package-relative asset {asset}"),
            ));
        }
    }
    let identifier = storage.identify(asset, outer_anchor)?;
    Ok(AssetBytes {
        bytes: Arc::from(storage.read(&identifier)?),
        identifier,
        source: AssetByteSource::Storage,
        anchor,
    })
}

fn package_bytes(
    bytes: &[u8],
    package: &str,
    member: &str,
    anchor: Option<LayerId>,
) -> Result<AssetBytes, IoError> {
    let archive = layerstack_usdz::zip::ZipArchive::parse(bytes)
        .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
    package_entry(&archive, package, member, anchor)
}

fn package_entry(
    archive: &layerstack_usdz::zip::ZipArchive<'_>,
    package: &str,
    member: &str,
    anchor: Option<LayerId>,
) -> Result<AssetBytes, IoError> {
    let entry = archive.find(member).ok_or_else(|| {
        IoError::new(
            IoErrorKind::NotFound,
            alloc::format!("missing package member {member}"),
        )
    })?;
    Ok(AssetBytes {
        identifier: alloc::format!("{package}[{member}]"),
        bytes: Arc::from(archive.entry_data(entry)),
        source: AssetByteSource::PackageMember {
            package: package.into(),
            member: member.into(),
        },
        anchor,
    })
}
