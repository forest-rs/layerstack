// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! UDIM tile discovery through the document's storage and package catalog.
//!
//! OpenUSD finds a pattern's tiles in two steps (`UsdUtils_LocalizationContext::_GetUdimTiles`):
//! `UsdShadeUdimUtils::ResolveUdimPath` resolves tile spellings relative to
//! the authoring layer, 1001 upwards, until one exists, and spells that
//! tile's resolved path with `<UDIM>` again; `ResolveUdimTilePaths` then
//! probes every tile of that resolved pattern. Discovery here takes the
//! same two steps, with the same results, and spends its storage work
//! explicitly: when the storage can list a directory
//! ([`Storage::list_siblings`]) the second step reads one listing instead
//! of probing each tile, a host-owned [`UdimDiscovery`] keeps the sets,
//! listings and packages it has found for later calls, and every storage
//! call is counted in [`UdimDiscoveryWork`].
use super::*;
use core::ops::AddAssign;
use layerstack::asset::{UdimPattern, UdimTile};

/// One existing tile of a UDIM set.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UdimTileAsset {
    /// The tile number.
    pub tile: UdimTile,
    /// The tile spelled as authored: the pattern with this tile's number
    /// (`UsdShadeUdimUtils::ReplaceUdimPattern`).
    pub asset_path: String,
    /// The tile's resolved identity, in the directory of the set's resolved
    /// pattern: a storage identifier or a `package[member]` path. Pass it to
    /// [`StageDocument::read_asset_bytes`] without an anchor to read the tile.
    /// Renderers resolve tiles there too; OpenUSD's dependency listing
    /// instead resolves each tile's spelling on its own, which differs only
    /// when search paths split one set's tiles across directories.
    pub identifier: String,
}

/// Storage and package index work spent by discovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct UdimDiscoveryWork {
    /// [`Storage::identify`] calls.
    pub identifies: usize,
    /// [`Storage::exists`] calls.
    pub existence_checks: usize,
    /// [`Storage::list_siblings`] calls that enumerated a directory.
    pub listings: usize,
    /// Directory listings an earlier call had read, used again.
    pub listings_reused: usize,
    /// Identifiers those listings returned, each matched against the pattern.
    pub listed_entries: usize,
    /// Member lookups in a package's resident index, which read nothing.
    pub member_lookups: usize,
    /// Packages read from storage for a pattern spelled `package[member]`
    /// outside the document's resident packages; each is read once.
    pub package_reads: usize,
    /// Calls whose set an earlier call had found, from any layer and
    /// spelling, so its later tiles were not looked for again.
    pub sets_reused: usize,
}

impl AddAssign for UdimDiscoveryWork {
    fn add_assign(&mut self, other: Self) {
        self.identifies += other.identifies;
        self.existence_checks += other.existence_checks;
        self.listings += other.listings;
        self.listings_reused += other.listings_reused;
        self.listed_entries += other.listed_entries;
        self.member_lookups += other.member_lookups;
        self.package_reads += other.package_reads;
        self.sets_reused += other.sets_reused;
    }
}

/// The existing tiles of one UDIM pattern and the work spent finding them.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UdimTiles {
    /// The resolved identity of the first existing tile, spelled with
    /// `<UDIM>` again (`UsdShadeUdimUtils::ResolveUdimPath`); `None` when
    /// no tile exists.
    pub resolved_pattern: Option<String>,
    /// The existing tiles in ascending tile order, the order OpenUSD
    /// returns them in; empty when none of 1001 through 1100 exists.
    pub tiles: Vec<UdimTileAsset>,
    /// The storage work this call performed.
    pub work: UdimDiscoveryWork,
}

/// What UDIM discovery has found for one document, kept for later calls.
///
/// A set's tiles are looked for once, whichever layer and spelling name it
/// again; a directory is listed once, whichever sets it holds; a package
/// read for an explicit `package[member]` pattern is read once; and a use
/// asked about again, by the same anchor and spelling, spends no work.
/// The host owns the lifetime: nothing here is refreshed, so clear it, or
/// start another, when the storage may have changed. Layer IDs in its keys
/// belong to one document.
///
/// A listing reads every entry of its directory: on a local APFS volume a
/// 2000-file directory lists in about 1 ms, where probing the 99 later
/// tiles of one set takes about 0.1 ms, and a 3-file directory lists in
/// 0.02 ms. The default lists, which pays off in small directories and
/// wherever several sets share a directory's listing; [`Self::probing`]
/// never lists, for storage where a listing costs more than the probes it
/// saves.
#[derive(Clone, Debug, Default)]
pub struct UdimDiscovery {
    probing: bool,
    uses: BTreeMap<(Option<LayerId>, String), Known>,
    sets: BTreeMap<String, BTreeSet<UdimTile>>,
    /// Sorted listings by directory ([`directory`]).
    listings: BTreeMap<String, Arc<Listing>>,
    archives: BTreeMap<String, Arc<PackageSnapshot>>,
    work: UdimDiscoveryWork,
}

/// One directory's sources, sorted as stored and with ASCII case folded,
/// so a set's entries are found by its stem in either spelling.
#[derive(Debug)]
struct Listing {
    entries: Vec<String>,
    folded: Vec<String>,
}

/// A use's answer, without the work its first call spent.
#[derive(Clone, Debug)]
struct Known {
    resolved_pattern: Option<String>,
    tiles: Vec<UdimTileAsset>,
}

impl UdimDiscovery {
    /// A discovery that never lists directories: it probes each later tile
    /// with [`Storage::exists`], as OpenUSD resolves each one.
    #[must_use]
    pub fn probing() -> Self {
        Self {
            probing: true,
            ..Self::default()
        }
    }

    /// Everything this discovery has spent since it was created or cleared.
    #[must_use]
    pub fn work(&self) -> UdimDiscoveryWork {
        self.work
    }

    /// A package this discovery read for an explicit `package[member]`
    /// pattern, by its identifier.
    pub(super) fn archive(&self, identifier: &str) -> Option<&Arc<PackageSnapshot>> {
        self.archives.get(identifier)
    }

    /// Forgets what was found, so later calls ask the storage again; the
    /// choice of [`Self::probing`] is kept.
    pub fn clear(&mut self) {
        *self = Self {
            probing: self.probing,
            ..Self::default()
        };
    }
}

impl<B: Storage> StageDocument<B> {
    /// Finds the existing tiles of a UDIM `pattern` authored in `anchor`, as
    /// OpenUSD's dependency discovery finds them, and spends no work beyond
    /// the counts it reports.
    ///
    /// Tile spellings resolve relative to `anchor` as any asset path does,
    /// inside its package first when `anchor` is a package member (AOUSD
    /// Core §9.4, §9.7), 1001 upwards until one exists
    /// (`UsdShadeUdimUtils::ResolveUdimPath`). Every other tile is then
    /// looked for beside that one (`ResolveUdimTilePaths`): with one
    /// [`Storage::list_siblings`] listing when the storage can list and
    /// the tile number is in the file name, otherwise one
    /// [`Storage::exists`] probe per tile, and in a package's index without
    /// reading anything. A listing spells names as stored: when it does not
    /// hold the first tile as identified (a case-insensitive filesystem,
    /// another Unicode normalization) the set is probed, and a later tile
    /// listed only in another ASCII case is confirmed with a probe. When no tile exists the result is empty; the
    /// storage's own failures, an anchor without a source, and a first tile
    /// whose identity no longer spells the pattern (a `..` after `<UDIM>`)
    /// are errors.
    ///
    /// `discovery` keeps what calls find for later calls (see
    /// [`UdimDiscovery`]): a host resolving many textures keeps one, so that
    /// each set is looked for, and each directory listed, once. A listing
    /// reads every entry of its directory, so a single set beside thousands
    /// of other files is cheaper probed; the listing pays off as the sets of
    /// a directory share it. [`StageDocument::prepare_package`] keeps one
    /// for its whole preparation.
    ///
    /// ```no_run
    /// # #[cfg(feature = "std")]
    /// # fn example() -> Result<(), layerstack_io::IoError> {
    /// use layerstack::{StageOptions, asset::UdimPattern};
    /// use layerstack_io::{Filesystem, StageDocument, UdimDiscovery};
    /// let mut document = StageDocument::open(
    ///     Filesystem::new(".", [])?, "scene.usda", StageOptions::default())?;
    /// let root = document.stage().stage().root_layer().expect("document root");
    /// let mut discovery = UdimDiscovery::default();
    /// let pattern = UdimPattern::parse("./textures/color.<UDIM>.png").expect("a UDIM pattern");
    /// let found = document.resolve_udim_tiles(pattern, Some(root), &mut discovery)?;
    /// for tile in &found.tiles {
    ///     let bytes = document.read_asset_bytes(&tile.identifier, None)?;
    ///     println!("tile {} has {} bytes", tile.tile, bytes.bytes.len());
    /// }
    /// println!("{:?}", discovery.work());
    /// # Ok(())
    /// # }
    /// ```
    pub fn resolve_udim_tiles(
        &self,
        pattern: UdimPattern<'_>,
        anchor: Option<LayerId>,
        discovery: &mut UdimDiscovery,
    ) -> Result<UdimTiles, IoError> {
        Discovery::new(
            &self.storage,
            &self.catalog,
            self.package_limits,
            None,
            discovery,
        )
        .tiles(pattern, anchor)
    }
}

/// Where a located tile was found.
enum Found {
    /// A loose storage source.
    Storage(String),
    /// A member of a package index, resident or read for this discovery.
    Member {
        archive: Arc<PackageSnapshot>,
        /// The package's identifier.
        outer: String,
        /// The selected member path inside it.
        member: String,
    },
}

impl Found {
    fn identifier(&self) -> String {
        match self {
            Self::Storage(identifier) => identifier.clone(),
            Self::Member { outer, member, .. } => alloc::format!("{outer}[{member}]"),
        }
    }
}

/// The text a storage's identifiers of one directory share: everything
/// before the last `/` or `\` ([`Storage::list_siblings`]).
fn directory(identifier: &str) -> Option<&str> {
    identifier.rfind(['/', '\\']).map(|end| &identifier[..end])
}

/// One call's borrowed sources, its session and its work.
pub(super) struct Discovery<'a, B> {
    storage: &'a B,
    catalog: &'a Catalog,
    package_limits: PackageLimits,
    read_limits: Option<AssetReadLimits>,
    session: &'a mut UdimDiscovery,
    work: UdimDiscoveryWork,
}

impl<'a, B: Storage> Discovery<'a, B> {
    pub(super) fn new(
        storage: &'a B,
        catalog: &'a Catalog,
        package_limits: PackageLimits,
        read_limits: Option<AssetReadLimits>,
        session: &'a mut UdimDiscovery,
    ) -> Self {
        Self {
            storage,
            catalog,
            package_limits,
            read_limits,
            session,
            work: UdimDiscoveryWork::default(),
        }
    }

    /// Both OpenUSD steps, or the session's answer; the result's work is
    /// this call's alone, and is added to the session's.
    pub(super) fn tiles(
        mut self,
        pattern: UdimPattern<'_>,
        anchor: Option<LayerId>,
    ) -> Result<UdimTiles, IoError> {
        let key = (anchor, String::from(pattern.as_str()));
        let known = match self.session.uses.get(&key) {
            Some(known) => {
                self.work.sets_reused += 1;
                known.clone()
            }
            None => {
                let known = self.discover(pattern, anchor)?;
                self.session.uses.insert(key, known.clone());
                known
            }
        };
        self.session.work += self.work;
        Ok(UdimTiles {
            resolved_pattern: known.resolved_pattern,
            tiles: known.tiles,
            work: self.work,
        })
    }

    fn discover(
        &mut self,
        pattern: UdimPattern<'_>,
        anchor: Option<LayerId>,
    ) -> Result<Known, IoError> {
        let mut first = None;
        for tile in UdimTile::all() {
            if let Some(found) = self.locate(&pattern.tile_path(tile), anchor)? {
                first = Some((tile, found));
                break;
            }
        }
        let Some((first_tile, found)) = first else {
            return Ok(Known {
                resolved_pattern: None,
                tiles: Vec::new(),
            });
        };
        let spelled = |text: &str| {
            Spelled::split(text, first_tile, pattern.suffix()).ok_or_else(|| {
                IoError::new(
                    IoErrorKind::Rejected,
                    alloc::format!(
                        "UDIM tile {first_tile} of {pattern} resolves to {text}, \
                         which does not spell the pattern's tile"
                    ),
                )
            })
        };
        let resolved = spelled(&found.identifier())?;
        let resolved_pattern = resolved.tile_text(UdimPattern::MARKER);
        if let Some(tiles) = self.session.sets.get(&resolved_pattern) {
            self.work.sets_reused += 1;
            return Ok(resolved.found(pattern, resolved_pattern, tiles));
        }
        let mut tiles = BTreeSet::from([first_tile]);
        let later = UdimTile::all().skip_while(|tile| *tile <= first_tile);
        match &found {
            Found::Member {
                archive, member, ..
            } => {
                let member = spelled(member)?;
                for tile in later {
                    self.work.member_lookups += 1;
                    if archive.member(&member.tile(tile)).is_ok() {
                        tiles.insert(tile);
                    }
                }
            }
            Found::Storage(identifier) => {
                // A tile in a directory name (`<UDIM>/color.png`) puts each
                // tile in its own directory, which one listing cannot cover.
                let listing = if self.session.probing || resolved.suffix.contains(['/', '\\']) {
                    None
                } else {
                    self.listing(identifier)?
                };
                // A listing spells names as stored. When the first tile is
                // not listed as identified (a case-insensitive filesystem,
                // another Unicode normalization), probing decides instead.
                let listing = listing.filter(|listing| {
                    listing
                        .entries
                        .binary_search(&resolved.tile(first_tile))
                        .is_ok()
                });
                match listing {
                    Some(listing) => {
                        let entries = &listing.entries;
                        // Sorted, so a set's tiles are the run of entries
                        // that start with its stem.
                        let start =
                            entries.partition_point(|e| e.as_str() < resolved.stem.as_str());
                        tiles.extend(
                            entries[start..]
                                .iter()
                                .take_while(|entry| entry.starts_with(resolved.stem.as_str()))
                                .filter_map(|entry| resolved.tile_of(entry))
                                .filter(|tile| *tile > first_tile),
                        );
                        // A tile stored in another case is the authored
                        // tile only if the storage says so.
                        let stem = resolved.stem.to_ascii_lowercase();
                        let start = listing.folded.partition_point(|e| *e < stem);
                        let loose: BTreeSet<UdimTile> = listing.folded[start..]
                            .iter()
                            .take_while(|entry| entry.starts_with(stem.as_str()))
                            .filter_map(|entry| resolved.tile_of_ignoring_case(entry))
                            .filter(|tile| *tile > first_tile && !tiles.contains(tile))
                            .collect();
                        for tile in loose {
                            let identifier = resolved.tile(tile);
                            self.work.existence_checks += 1;
                            if self
                                .storage
                                .exists(&identifier)
                                .map_err(|e| e.at(&identifier))?
                            {
                                tiles.insert(tile);
                            }
                        }
                    }
                    None => {
                        for tile in later {
                            let identifier = resolved.tile(tile);
                            self.work.existence_checks += 1;
                            if self
                                .storage
                                .exists(&identifier)
                                .map_err(|e| e.at(&identifier))?
                            {
                                tiles.insert(tile);
                            }
                        }
                    }
                }
            }
        }
        let found = resolved.found(pattern, resolved_pattern.clone(), &tiles);
        self.session.sets.insert(resolved_pattern, tiles);
        Ok(found)
    }

    /// The sources beside `identifier`, sorted: the session's listing of
    /// its directory, or a new one; `None` when the storage cannot list.
    fn listing(&mut self, identifier: &str) -> Result<Option<Arc<Listing>>, IoError> {
        let directory = directory(identifier);
        if let Some(entries) = directory.and_then(|d| self.session.listings.get(d)) {
            self.work.listings_reused += 1;
            return Ok(Some(entries.clone()));
        }
        let Some(mut entries) = self
            .storage
            .list_siblings(identifier)
            .map_err(|e| e.at(identifier))?
        else {
            return Ok(None);
        };
        entries.sort_unstable();
        self.work.listings += 1;
        self.work.listed_entries += entries.len();
        let mut folded: Vec<String> = entries.iter().map(|e| e.to_ascii_lowercase()).collect();
        folded.sort_unstable();
        let entries = Arc::new(Listing { entries, folded });
        if let Some(directory) = directory {
            self.session
                .listings
                .insert(directory.into(), entries.clone());
        }
        Ok(Some(entries))
    }

    /// The source one tile spelling resolves to, as [`asset_bytes::read`]
    /// would select it, without reading it.
    fn locate(&mut self, asset: &str, anchor: Option<LayerId>) -> Result<Option<Found>, IoError> {
        let catalog = self.catalog;
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
            .or_else(|| source.map(|s| &*s.identifier));
        if let Some(package) = package
            && !asset.starts_with('/')
            && !asset.contains(':')
        {
            self.work.member_lookups += 1;
            let archive = &catalog.package_bytes[&package];
            let anchor = anchor.expect("package anchor");
            if let Some(member) = catalog.package_member(archive, asset, anchor, package)? {
                return Ok(Some(Found::Member {
                    archive: archive.clone(),
                    outer: catalog.package_identifier(package).into(),
                    member,
                }));
            }
            if asset.starts_with('.') {
                return Ok(None);
            }
        }
        let path = PackagePath::parse(asset, 32)
            .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
        self.work.identifies += 1;
        let identifier = self
            .storage
            .identify(path.outer(), outer_anchor)
            .map_err(|e| e.at(path.outer()))?;
        let Some((first, rest)) = path.members().split_first() else {
            self.work.existence_checks += 1;
            let exists = self
                .storage
                .exists(&identifier)
                .map_err(|e| e.at(&identifier))?;
            return Ok(exists.then_some(Found::Storage(identifier)));
        };
        let archive = self.archive(&identifier)?;
        self.work.member_lookups += 1;
        match archive.member(&PackagePath::join(first, rest)) {
            Ok(selected) => Ok(Some(Found::Member {
                archive,
                outer: identifier,
                member: selected.path,
            })),
            Err(layerstack_usdz::UsdzError::MissingMember { .. }) => Ok(None),
            Err(error) => Err(IoError::new(IoErrorKind::Rejected, error.to_string())),
        }
    }

    /// A package's index: resident in the document, read earlier in this
    /// discovery, or read now under the discovery's limits.
    fn archive(&mut self, identifier: &str) -> Result<Arc<PackageSnapshot>, IoError> {
        if let Some(archive) = self
            .catalog
            .ids
            .get(identifier)
            .and_then(|id| self.catalog.package_bytes.get(id))
            .or_else(|| self.session.archives.get(identifier))
        {
            return Ok(archive.clone());
        }
        let bytes = match self.read_limits {
            Some(limits) => self
                .storage
                .read_encoded_bounded(identifier, limits.package_bytes),
            None => self.storage.read_encoded(identifier),
        }
        .map_err(|e| e.at(identifier))?;
        self.work.package_reads += 1;
        let archive = Arc::new(
            PackageSnapshot::new(bytes, self.package_limits)
                .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?,
        );
        self.session
            .archives
            .insert(identifier.into(), archive.clone());
        Ok(archive)
    }
}

/// A resolved tile identity split around its tile number, so the other
/// tiles of its set are spelled beside it. A package path keeps its
/// closing brackets after the authored suffix.
struct Spelled {
    stem: String,
    suffix: String,
    close: String,
}

impl Spelled {
    /// Splits `resolved`, the identity of `tile`, around the tile number,
    /// or `None` when resolution changed the text after `<UDIM>`, which
    /// OpenUSD reports as an ambiguous first tile.
    fn split(resolved: &str, tile: UdimTile, authored_suffix: &str) -> Option<Self> {
        let core = resolved.trim_end_matches(']');
        let close = &resolved[core.len()..];
        let suffix = authored_suffix.trim_end_matches(']');
        let stem = core
            .strip_suffix(suffix)?
            .strip_suffix(alloc::format!("{tile}").as_str())?;
        Some(Self {
            stem: stem.into(),
            suffix: suffix.into(),
            close: close.into(),
        })
    }

    /// The answer for `tiles` of this resolved set.
    fn found(
        &self,
        pattern: UdimPattern<'_>,
        resolved_pattern: String,
        tiles: &BTreeSet<UdimTile>,
    ) -> Known {
        Known {
            resolved_pattern: Some(resolved_pattern),
            tiles: tiles
                .iter()
                .map(|&tile| UdimTileAsset {
                    tile,
                    asset_path: pattern.tile_path(tile),
                    identifier: self.tile(tile),
                })
                .collect(),
        }
    }

    fn tile(&self, tile: UdimTile) -> String {
        self.tile_text(&alloc::format!("{tile}"))
    }

    fn tile_text(&self, text: &str) -> String {
        alloc::format!("{}{text}{}{}", self.stem, self.suffix, self.close)
    }

    /// [`Self::tile_of`] with ASCII case ignored outside the tile number.
    fn tile_of_ignoring_case(&self, identifier: &str) -> Option<UdimTile> {
        let length = self.stem.len() + 4 + self.suffix.len() + self.close.len();
        if identifier.len() != length
            || !identifier.is_char_boundary(self.stem.len())
            || !identifier.is_char_boundary(self.stem.len() + 4)
        {
            return None;
        }
        let (stem, rest) = identifier.split_at(self.stem.len());
        let (digits, tail) = rest.split_at(4);
        let expected = alloc::format!("{}{}", self.suffix, self.close);
        (stem.eq_ignore_ascii_case(&self.stem)
            && tail.eq_ignore_ascii_case(&expected)
            && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| UdimTile::new(digits.parse().ok()?))
        .flatten()
    }

    fn tile_of(&self, identifier: &str) -> Option<UdimTile> {
        let digits = identifier
            .strip_prefix(self.stem.as_str())?
            .strip_suffix(self.close.as_str())?
            .strip_suffix(self.suffix.as_str())?;
        (digits.len() == 4 && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| UdimTile::new(digits.parse().ok()?))
            .flatten()
    }
}
