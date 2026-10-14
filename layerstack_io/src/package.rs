// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Explicit dependency preparation and immutable package export.
use super::*;
use layerstack::asset_dependencies::{
    AssetDependency, AssetDependencyKind, DependencyReport, DependencyTarget, ResolvedDependency,
    extract_layer_dependencies, rewrite_layer_assets,
};
use layerstack_usdz::localize::{
    LocalizationPlan, LocalizedAsset, LocalizedLayer, relative_member,
};

/// Host decision for an authored dependency, including inactive list items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageInclusion {
    /// Include this concrete resource and its dependencies.
    Include,
    /// Keep this resource external, anchoring its identifier to its original source.
    /// Expressions and patterns remain authored for host expansion.
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
    External(Arc<str>),
    Failed(String),
}

impl<B: Storage> StageDocument<B> {
    /// Loads and inventories a dependency closure in an isolated candidate,
    /// then localizes copies. Resident unsaved layers take precedence over disk;
    /// all variants and inactive list items participate. The callback controls
    /// inclusion. Missing resources and unexpanded expressions/patterns remain
    /// inspectable and prevent export. For custom pattern expansion use the
    /// lower-level `layerstack_usdz::localize::localize_asset` callback.
    /// AOUSD Core §9.4, §9.7, §16.4; `UsdUtilsCreateNewUsdzPackage`.
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
        let mut report = DependencyReport::default();
        let mut total_bytes = 0_u64;
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
                let resource = if include(&dependency) == PackageInclusion::KeepExternal {
                    if dependency.identifier.starts_with('`')
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
                } else if dependency.identifier.starts_with('`')
                    || dependency.kind == AssetDependencyKind::ClipTemplate
                    || dependency.identifier.contains(['<', '#', '*'])
                {
                    Resource::Failed("host expansion required for expression or pattern".into())
                } else if identifier_format(&dependency.identifier).is_ok() {
                    let mut reader = Reader::new(
                        &mut self.storage,
                        &mut catalog,
                        self.import_policy,
                        self.usdc_options,
                        self.package_limits,
                        self.stage.options().schemas.clone(),
                    );
                    match reader.load(
                        &dependency.identifier,
                        Some(id),
                        &mut store.tokens,
                        &mut store.paths,
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
                        Ok(bytes)
                            if !PackagePath::parse(&bytes.identifier, 32).is_ok_and(|path| {
                                path.leaf().rsplit_once('.').is_some_and(|(_, ext)| {
                                    layerstack_usdz::writer::MEMBER_EXTENSIONS.contains(&ext)
                                })
                            }) =>
                        {
                            Resource::Failed("resource type is not permitted in USDZ".into())
                        }
                        Ok(bytes) => {
                            if !assets.contains_key(&bytes.identifier) {
                                total_bytes = total_bytes
                                    .checked_add(bytes.bytes.len() as u64)
                                    .ok_or_else(|| {
                                        IoError::new(
                                            IoErrorKind::Rejected,
                                            "package asset byte budget overflow",
                                        )
                                    })?;
                                if assets.len() >= limits.assets || total_bytes > limits.asset_bytes
                                {
                                    return Err(IoError::new(
                                        IoErrorKind::Rejected,
                                        "package asset inventory budget exceeded",
                                    ));
                                }
                                assets.insert(bytes.identifier.clone(), bytes.clone());
                            }
                            Resource::Asset(bytes)
                        }
                        Err(error) => Resource::Failed(error.to_string()),
                    }
                };
                let target = match &resource {
                    Resource::Layer(id) => {
                        queue.insert(*id);
                        DependencyTarget::Layer(*id)
                    }
                    Resource::Asset(bytes) => {
                        DependencyTarget::Asset(Arc::from(bytes.identifier.as_str()))
                    }
                    Resource::External(_) => DependencyTarget::Ignored,
                    Resource::Failed(reason) => {
                        DependencyTarget::Unresolved(Arc::from(reason.as_str()))
                    }
                };
                report.dependencies.push(ResolvedDependency {
                    source: dependency.clone(),
                    targets: alloc::vec![target],
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
                    |s| s.identifier.clone(),
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
            let asset_names: BTreeMap<_, _> = assets
                .keys()
                .enumerate()
                .map(|(index, identifier)| {
                    let path = PackagePath::parse(identifier, 32)
                        .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
                    let ext = path.leaf().rsplit_once('.').map_or("bin", |(_, ext)| ext);
                    Ok::<_, IoError>((
                        identifier.clone(),
                        alloc::format!("assets/{index:06}.{ext}"),
                    ))
                })
                .collect::<Result<_, _>>()?;
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
                assets: assets
                    .into_iter()
                    .map(|(identifier, bytes)| LocalizedAsset {
                        member: Arc::from(asset_names[&identifier].as_str()),
                        data: bytes.bytes,
                    })
                    .collect(),
                resolved_uses: report.dependencies.len(),
            });
        }
        Ok(PreparedPackage {
            store,
            plan,
            report,
        })
    }
    /// Writes an inspected candidate using its own interners. Original source
    /// bindings and saved generations remain unchanged.
    pub fn export_package(
        &mut self,
        target: &str,
        package: &PreparedPackage,
    ) -> Result<(), IoError> {
        let identifier = self.storage.identify(target, None)?;
        self.storage.write(&identifier, &package.write_usdz()?)
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
            s.package.map_or(s.identifier.as_str(), |package| {
                catalog.package_identifier(package)
            })
        });
        let outer = self.storage.identify(path.outer(), anchor)?;
        Ok(path.with_outer(&outer))
    }
}
