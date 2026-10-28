// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit dependency preparation and immutable package export.
use super::*;
use crate::udim::Discovery;
use layerstack::asset::{UdimPattern, UdimTile};
use layerstack::asset_dependencies::{
    AssetDependency, AssetDependencyKind, DependencyReport, DependencyTarget, ResolvedDependency,
    extract_layer_dependencies, rewrite_layer_assets,
};
use layerstack_usdz::localize::{
    LocalizationPlan, LocalizedAsset, LocalizedLayer, relative_member,
};

/// Host decision for an authored dependency, including inactive list items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PackageInclusion {
    /// Include this concrete resource and its dependencies.
    Include,
    /// Keep this resource external, anchoring its identifier to its original source.
    /// A UDIM pattern is anchored to the directory of its first tile
    /// (`UsdShadeUdimUtils::ResolveUdimPath`); expressions and other
    /// patterns remain authored for host expansion.
    KeepExternal,
}
/// Explicit inventory and opaque resource limits. Format imports can themselves
/// follow layer arcs; the layer count bounds inventory work, not scene allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackagePreparationLimits {
    /// Maximum distinct layers inspected.
    pub layers: usize,
    /// Maximum distinct opaque assets included.
    pub assets: usize,
    /// Maximum cumulative opaque asset bytes included.
    pub asset_bytes: u64,
    /// Per-resource and nonresident archive transport limits.
    pub read: AssetReadLimits,
}
impl Default for PackagePreparationLimits {
    fn default() -> Self {
        Self {
            layers: 100_000,
            assets: 100_000,
            asset_bytes: 1024 * 1024 * 1024,
            read: AssetReadLimits {
                bytes: 1024 * 1024 * 1024,
                package_bytes: 1024 * 1024 * 1024,
            },
        }
    }
}
/// Package candidate with an independent source/interner snapshot. Preparation
/// performs explicit I/O but changes no live layers, bindings or dirty state.
/// Package source layers become native members, including their unsaved edits.
#[derive(Debug)]
pub struct PreparedPackage {
    store: InMemoryStore,
    plan: Option<LocalizationPlan>,
    report: DependencyReport,
    udim_work: UdimDiscoveryWork,
}
impl PreparedPackage {
    /// All authored uses, inclusion decisions and unresolved reasons.
    pub fn report(&self) -> &DependencyReport {
        &self.report
    }
    /// Deterministic rewritten layers and destinations; absent when incomplete.
    pub fn plan(&self) -> Option<&LocalizationPlan> {
        self.plan.as_ref()
    }
    /// Storage work spent finding UDIM tiles, summed over every UDIM use of
    /// the preparation, which shares one [`UdimDiscovery`]: each set is
    /// looked for and each directory listed once
    /// ([`UdimDiscoveryWork::sets_reused`], `listings_reused`).
    pub fn udim_work(&self) -> UdimDiscoveryWork {
        self.udim_work
    }
    /// Serializes using the candidate's own interners, without transport I/O.
    pub fn write_usdz(&self) -> Result<Vec<u8>, IoError> {
        self.plan
            .as_ref()
            .ok_or_else(|| {
                IoError::new(IoErrorKind::Rejected, "package has unresolved dependencies")
            })?
            .write_usdz(&self.store.tokens, &self.store.paths)
            .map_err(localization_error)
    }
}
#[derive(Clone)]
enum Resource {
    Layer(LayerId),
    Asset(AssetBytes),
    /// A UDIM set, by its resolved pattern.
    Tiles(Arc<str>),
    External(Arc<str>),
    Failed(String),
}

/// The tiles of one packaged UDIM set.
struct TileSet {
    /// The member pattern from the segment holding `<UDIM>` on, such as
    /// `color.<UDIM>.png`, as OpenUSD keeps a packaged tile's file name.
    tail: String,
    tiles: Vec<(UdimTile, AssetBytes)>,
}

/// Opaque resources admitted to a package so far, for its limits.
#[derive(Default)]
struct Admitted {
    identifiers: BTreeSet<String>,
    bytes: u64,
}
impl Admitted {
    /// Counts `bytes` once per identifier, so a file both named directly
    /// and a tile of a set is stored twice but counted once; budget
    /// overruns fail preparation.
    fn admit(
        &mut self,
        bytes: &AssetBytes,
        limits: &PackagePreparationLimits,
    ) -> Result<(), IoError> {
        if !self.identifiers.insert(bytes.identifier.clone()) {
            return Ok(());
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.bytes.len() as u64)
            .ok_or_else(|| {
                IoError::new(IoErrorKind::Rejected, "package asset byte budget overflow")
            })?;
        if self.identifiers.len() > limits.assets || self.bytes > limits.asset_bytes {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                "package asset inventory budget exceeded",
            ));
        }
        Ok(())
    }
}

/// Whether a USDZ package may hold a resource with this identifier's leaf.
fn permitted_member(identifier: &str) -> bool {
    PackagePath::parse(identifier, 32).is_ok_and(|path| {
        path.leaf()
            .rsplit_once('.')
            .is_some_and(|(_, ext)| layerstack_usdz::writer::MEMBER_EXTENSIONS.contains(&ext))
    })
}

/// The packaged spelling of a resolved UDIM pattern: its innermost member
/// from the path segment holding `<UDIM>` on, or `None` when that is not a
/// canonical member path.
fn tile_tail(resolved: &str) -> Option<String> {
    let core = resolved.trim_end_matches(']');
    let leaf = core.rsplit_once('[').map_or(core, |(_, member)| member);
    let marker = leaf.find(UdimPattern::MARKER)?;
    let start = leaf[..marker]
        .rfind(['/', '\\'])
        .map_or(0, |slash| slash + 1);
    let tail = leaf[start..].replace('\\', "/");
    PackagePath::parse(&alloc::format!("package[{tail}]"), 1)
        .is_ok_and(|path| path.members()[0] == tail)
        .then_some(tail)
}

impl<B: Storage> StageDocument<B> {
    /// Loads and inventories a dependency closure in an isolated candidate,
    /// then localizes copies. Resident unsaved layers take precedence over disk;
    /// all variants and inactive list items participate. The callback controls
    /// inclusion of resource dependencies. Direct prim `assetInfo.identifier`
    /// assets are reported as ignored identity uses, retained verbatim and never
    /// passed to the callback or loaded. Other assetInfo assets remain resources.
    /// A UDIM pattern in an asset value expands to its existing tiles
    /// ([`Self::resolve_udim_tiles`], with one [`UdimDiscovery`] for the
    /// preparation; see [`PreparedPackage::udim_work`]): its report entry has one asset
    /// target per tile, every tile is packaged, and the copied value keeps
    /// `<UDIM>` and the tile file name, `assets/000002/color.<UDIM>.png`
    /// beside members `assets/000002/color.1001.png` and so on, as OpenUSD
    /// keeps the pattern in a package. A pattern without tiles is
    /// unresolved, as OpenUSD reports it.
    /// Missing resources, expressions, clip templates and patterns in
    /// composition arcs remain inspectable and prevent export. For custom
    /// pattern expansion use the lower-level
    /// `layerstack_usdz::localize::localize_asset` callback.
    /// AOUSD Core §9.4, §9.7, §16.4; `UsdUtilsCreateNewUsdzPackage`,
    /// `UsdUtils_LocalizationContext::_GetUdimTiles`.
    ///
    /// ```no_run
    /// # #[cfg(feature = "std")]
    /// # fn example() -> Result<(), layerstack_io::IoError> {
    /// use layerstack::StageOptions;
    /// use layerstack_io::{Filesystem, StageDocument, PackageInclusion, PackagePreparationLimits};
    /// let mut document = StageDocument::open(
    ///     Filesystem::new(".", [])?, "scene.usda", StageOptions::default())?;
    /// let root = document.stage().stage().root_layer().expect("document root");
    /// let package = document.prepare_package(root, "scene.usda",
    ///     PackagePreparationLimits::default(), |_| PackageInclusion::Include)?;
    /// println!("{:?}", package.report()); // Includes exact unresolved authoring uses.
    /// if let Some(plan) = package.plan() {
    ///     println!("{} layers, {} resources", plan.layers.len(), plan.assets.len());
    ///     document.export_package("scene.usdz", &package)?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn prepare_package(
        &mut self,
        root: LayerId,
        root_member: &str,
        limits: PackagePreparationLimits,
        mut include: impl FnMut(&AssetDependency) -> PackageInclusion,
    ) -> Result<PreparedPackage, IoError> {
        layerstack_usdz::write_usdz(&[layerstack_usdz::PackageFile::new(
            root_member,
            b"#usda 1.0",
        )])
        .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
        let mut store = self.store.snapshot();
        let mut catalog = self.catalog.clone();
        let mut queue = BTreeSet::from([root]);
        let mut visited = BTreeSet::new();
        let mut resources = BTreeMap::new();
        let mut assets = BTreeMap::<String, AssetBytes>::new();
        let mut sets = BTreeMap::<Arc<str>, TileSet>::new();
        let mut discovery = UdimDiscovery::default();
        let mut admitted = Admitted::default();
        let mut report = DependencyReport::default();
        while let Some(id) = queue.pop_first() {
            if !visited.insert(id) {
                continue;
            }
            let Some(layer) = store.layer(id) else {
                report.missing_layers.push(id);
                continue;
            };
            if visited.len() > limits.layers {
                return Err(IoError::new(
                    IoErrorKind::Rejected,
                    "package layer inventory budget exceeded",
                ));
            }
            report.layers.push(id);
            for dependency in extract_layer_dependencies(layer, &store.paths, &store.tokens) {
                // UDIM patterns expand in asset values, not in composition
                // arcs, as `UsdUtils_LocalizationContext::_ProcessAssetValue`
                // expands them; an expression is evaluated first and then
                // not expanded there, so it stays host work.
                let udim = (dependency.kind == AssetDependencyKind::Value
                    && !dependency.identifier.starts_with('`'))
                .then(|| UdimPattern::parse(&dependency.identifier))
                .flatten();
                let mut find_tiles = |pattern: UdimPattern<'_>| {
                    Discovery::new(
                        &self.storage,
                        &catalog,
                        self.package_limits,
                        Some(limits.read),
                        &mut discovery,
                    )
                    .tiles(pattern, Some(id))
                    .map_err(|error| error.to_string())
                };
                let resource = if dependency.kind == AssetDependencyKind::Identity {
                    // Identity is preserved verbatim and requires no transport.
                    // AOUSD Core §7.6.2.4; OpenUSD assetInfo identifier convention.
                    Resource::External(dependency.identifier.clone())
                } else if include(&dependency) == PackageInclusion::KeepExternal {
                    if let Some(pattern) = udim {
                        // Anchored to its tiles' directory when they are
                        // here; otherwise as any concrete path, since an
                        // external set need not exist locally.
                        match find_tiles(pattern) {
                            Ok(UdimTiles {
                                resolved_pattern: Some(resolved),
                                ..
                            }) => Resource::External(Arc::from(resolved)),
                            _ => match self.external_package_dependency(
                                &catalog,
                                id,
                                &dependency.identifier,
                            ) {
                                Ok(identifier) => Resource::External(Arc::from(identifier)),
                                Err(error) => Resource::Failed(error.to_string()),
                            },
                        }
                    } else if dependency.identifier.starts_with('`')
                        || dependency.kind == AssetDependencyKind::ClipTemplate
                        || dependency.identifier.contains(['<', '#', '*'])
                    {
                        Resource::External(dependency.identifier.clone())
                    } else {
                        match self.external_package_dependency(&catalog, id, &dependency.identifier)
                        {
                            Ok(identifier) => Resource::External(Arc::from(identifier)),
                            Err(error) => Resource::Failed(error.to_string()),
                        }
                    }
                } else if let Some(id) = dependency
                    .layer_hint
                    .filter(|id| store.layer(*id).is_some())
                {
                    Resource::Layer(id)
                } else if let Some(pattern) = udim {
                    match find_tiles(pattern) {
                        Err(reason) => Resource::Failed(reason),
                        Ok(UdimTiles {
                            resolved_pattern: Some(resolved),
                            tiles,
                            ..
                        }) => {
                            let key = Arc::from(resolved);
                            if sets.contains_key(&key) {
                                Resource::Tiles(key)
                            } else if let Some(tail) = tile_tail(&key) {
                                match read_tiles(
                                    &mut self.storage,
                                    &mut catalog,
                                    &discovery,
                                    self.package_limits,
                                    &limits,
                                    &mut admitted,
                                    &tiles,
                                )? {
                                    Ok(tiles) => {
                                        sets.insert(key.clone(), TileSet { tail, tiles });
                                        Resource::Tiles(key)
                                    }
                                    Err(reason) => Resource::Failed(reason),
                                }
                            } else {
                                Resource::Failed(alloc::format!(
                                    "UDIM pattern {key} has no package member spelling"
                                ))
                            }
                        }
                        Ok(_) => Resource::Failed(no_tiles(pattern)),
                    }
                } else if dependency.identifier.starts_with('`')
                    || dependency.kind == AssetDependencyKind::ClipTemplate
                    || dependency.identifier.contains(['<', '#', '*'])
                {
                    Resource::Failed("host expansion required for expression or pattern".into())
                } else if identifier_format(&dependency.identifier, &self.formats).is_ok() {
                    let mut reader = Reader::new(
                        &mut self.storage,
                        &mut catalog,
                        self.import_policy,
                        self.usdc_options,
                        self.package_limits,
                        self.stage.options().schemas.clone(),
                    );
                    reader.variant_arcs = self.variant_arcs;
                    reader.formats = self.formats.clone();
                    match reader.load(
                        &dependency.identifier,
                        Some(id),
                        &store.tokens,
                        &store.paths,
                    ) {
                        Ok(resolved) => {
                            let target = resolved.layer_id;
                            if let Some(layer) = resolved.layer {
                                reader.pending.push(layer);
                            }
                            reader.commit(&mut store);
                            Resource::Layer(target)
                        }
                        Err(error) => Resource::Failed(error.to_string()),
                    }
                } else {
                    match asset_bytes::read(
                        &mut self.storage,
                        &mut catalog,
                        &dependency.identifier,
                        Some(id),
                        Some(limits.read),
                        self.package_limits,
                    ) {
                        Ok(bytes) if !permitted_member(&bytes.identifier) => {
                            Resource::Failed("resource type is not permitted in USDZ".into())
                        }
                        Ok(bytes) => {
                            admitted.admit(&bytes, &limits)?;
                            assets
                                .entry(bytes.identifier.clone())
                                .or_insert_with(|| bytes.clone());
                            Resource::Asset(bytes)
                        }
                        Err(error) => Resource::Failed(error.to_string()),
                    }
                };
                let targets = match &resource {
                    Resource::Layer(id) => {
                        queue.insert(*id);
                        alloc::vec![DependencyTarget::Layer(*id)]
                    }
                    Resource::Asset(bytes) => {
                        alloc::vec![DependencyTarget::Asset(Arc::from(
                            bytes.identifier.as_str()
                        ))]
                    }
                    // One target per existing tile, in tile order.
                    Resource::Tiles(key) => sets[key]
                        .tiles
                        .iter()
                        .map(|(_, bytes)| {
                            DependencyTarget::Asset(Arc::from(bytes.identifier.as_str()))
                        })
                        .collect(),
                    Resource::External(_) => alloc::vec![DependencyTarget::Ignored],
                    Resource::Failed(reason) => {
                        alloc::vec![DependencyTarget::Unresolved(Arc::from(reason.as_str()))]
                    }
                };
                report.dependencies.push(ResolvedDependency {
                    source: dependency.clone(),
                    targets,
                });
                resources.insert(dependency, resource);
            }
        }
        let mut plan = None;
        if report.is_complete() {
            let mut layers = report.layers.clone();
            layers.sort_by_key(|id| {
                catalog.sources.get(id).map_or_else(
                    || alloc::format!("anonymous/{}", id.0),
                    |s| String::from(&*s.identifier),
                )
            });
            let mut names = BTreeMap::from([(root, String::from(root_member))]);
            let mut index = 0;
            for &id in layers.iter().filter(|id| **id != root) {
                let member = loop {
                    let member = alloc::format!("layers/{index:06}.usda");
                    index += 1;
                    if member != root_member {
                        break member;
                    }
                };
                names.insert(id, member);
            }
            // Assets and UDIM sets share one index, in identifier order: an
            // asset is `assets/000001.png`; a set keeps its file name in a
            // directory, `assets/000002/color.<UDIM>.png`, so every tile
            // member is the pattern with its number.
            let mut asset_names = BTreeMap::new();
            let mut set_names = BTreeMap::new();
            let mut keys: Vec<(&str, bool)> = assets
                .keys()
                .map(|identifier| (identifier.as_str(), false))
                .chain(sets.keys().map(|pattern| (&**pattern, true)))
                .collect();
            keys.sort_unstable();
            for (index, (key, is_set)) in keys.into_iter().enumerate() {
                if is_set {
                    let name = alloc::format!("assets/{index:06}/{}", sets[key].tail);
                    set_names.insert(Arc::<str>::from(key), name);
                } else {
                    let path = PackagePath::parse(key, 32)
                        .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
                    let ext = path.leaf().rsplit_once('.').map_or("bin", |(_, ext)| ext);
                    asset_names
                        .insert(String::from(key), alloc::format!("assets/{index:06}.{ext}"));
                }
            }
            let mut localized = Vec::new();
            for id in layers {
                let member = names[&id].clone();
                let copy = rewrite_layer_assets(
                    store.layer(id).expect("inventoried layer"),
                    &store.paths,
                    &store.tokens,
                    |dependency| -> Result<Arc<str>, IoError> {
                        let destination = match &resources[dependency] {
                            Resource::Layer(id) => &names[id],
                            Resource::Asset(bytes) => &asset_names[&bytes.identifier],
                            Resource::Tiles(key) => &set_names[key],
                            Resource::External(identifier) => return Ok(identifier.clone()),
                            Resource::Failed(_) => unreachable!("complete inventory"),
                        };
                        Ok(Arc::from(relative_member(&member, destination)))
                    },
                )?;
                localized.push(LocalizedLayer {
                    source: id,
                    member: Arc::from(member),
                    layer: copy,
                });
            }
            localized
                .sort_by(|a, b| (a.source != root, &a.member).cmp(&(b.source != root, &b.member)));
            plan = Some(LocalizationPlan {
                layers: localized,
                assets: {
                    let mut localized: Vec<_> = assets
                        .into_iter()
                        .map(|(identifier, bytes)| LocalizedAsset {
                            member: Arc::from(asset_names[&identifier].as_str()),
                            data: bytes.bytes,
                        })
                        .chain(sets.iter().flat_map(|(key, set)| {
                            let name = UdimPattern::parse(&set_names[key])
                                .expect("a set name holds its marker");
                            set.tiles.iter().map(move |(tile, bytes)| LocalizedAsset {
                                member: Arc::from(name.tile_path(*tile)),
                                data: bytes.bytes.clone(),
                            })
                        }))
                        .collect();
                    localized.sort_by(|a, b| a.member.cmp(&b.member));
                    localized
                },
                resolved_uses: report.dependencies.len(),
            });
        }
        Ok(PreparedPackage {
            store,
            plan,
            report,
            udim_work: discovery.work(),
        })
    }
    /// Writes an inspected candidate using its own interners. Original source
    /// bindings and saved generations remain unchanged.
    pub fn export_package(
        &mut self,
        target: &str,
        package: &PreparedPackage,
    ) -> Result<(), IoError> {
        let identifier = self
            .storage
            .identify(target, None)
            .map_err(|e| e.at(target))?;
        self.storage
            .write(&identifier, &package.write_usdz()?)
            .map_err(|e| e.at(&identifier))
    }

    fn external_package_dependency(
        &self,
        catalog: &Catalog,
        layer: LayerId,
        asset: &str,
    ) -> Result<String, IoError> {
        let source = catalog.sources.get(&layer);
        if let Some(package) = source.and_then(|s| s.package)
            && !asset.starts_with('/')
            && !asset.contains(':')
        {
            let authoring = if package == layer {
                catalog.package_roots[&package].clone()
            } else {
                package_member_path(&source.expect("package source").identifier)
                    .expect("package member")
            };
            let archive = &catalog.package_bytes[&package];
            let member = catalog
                .package_member(archive, asset, layer, package)?
                .or_else(|| {
                    asset
                        .starts_with('.')
                        .then(|| PackagePath::anchor_member(asset, &authoring))
                        .flatten()
                });
            if let Some(member) = member {
                return Ok(alloc::format!(
                    "{}[{member}]",
                    catalog.package_identifier(package)
                ));
            }
        }
        let path = PackagePath::parse(asset, 32)
            .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
        let anchor = source.map(|s| {
            s.package.map_or(&*s.identifier, |package| {
                catalog.package_identifier(package)
            })
        });
        let outer = self
            .storage
            .identify(path.outer(), anchor)
            .map_err(|e| e.at(path.outer()))?;
        Ok(path.with_outer(&outer))
    }
}

/// Why a UDIM use with no existing tile is unresolved; OpenUSD reports the
/// pattern itself unresolved (`UsdUtilsComputeAllDependencies`).
fn no_tiles(pattern: UdimPattern<'_>) -> String {
    alloc::format!(
        "no UDIM tile {} through {} exists for {pattern}",
        UdimTile::FIRST,
        UdimTile::LAST
    )
}

/// Reads every tile of a set for packaging, then admits them all to the
/// package limits: `Ok(Err(reason))` when one cannot be packaged, which
/// charges nothing, `Err` when the package exceeds its limits. A tile in a
/// package that discovery read is taken from that read.
fn read_tiles<B: Storage>(
    storage: &mut B,
    catalog: &mut Catalog,
    discovery: &UdimDiscovery,
    package_limits: PackageLimits,
    limits: &PackagePreparationLimits,
    admitted: &mut Admitted,
    tiles: &[UdimTileAsset],
) -> Result<Result<Vec<(UdimTile, AssetBytes)>, String>, IoError> {
    let mut read = Vec::with_capacity(tiles.len());
    for tile in tiles {
        let path = PackagePath::parse(&tile.identifier, 32).ok();
        let held = path.as_ref().and_then(|path| {
            let (first, rest) = path.members().split_first()?;
            let archive = discovery.archive(path.outer())?;
            Some(asset_bytes::package_entry(
                archive,
                path.outer(),
                &PackagePath::join(first, rest),
                None,
                Some(limits.read),
            ))
        });
        let bytes = match held {
            Some(bytes) => bytes,
            None => asset_bytes::read(
                storage,
                catalog,
                &tile.identifier,
                None,
                Some(limits.read),
                package_limits,
            ),
        };
        match bytes {
            Ok(bytes) if permitted_member(&bytes.identifier) => read.push((tile.tile, bytes)),
            Ok(bytes) => {
                return Ok(Err(alloc::format!(
                    "UDIM tile {}: resource type is not permitted in USDZ",
                    bytes.identifier
                )));
            }
            Err(error) => {
                return Ok(Err(alloc::format!(
                    "UDIM tile {}: {error}",
                    tile.identifier
                )));
            }
        }
    }
    for (_, bytes) in &read {
        admitted.admit(bytes, limits)?;
    }
    Ok(Ok(read))
}
