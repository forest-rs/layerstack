// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Application-owned stage I/O, above the composition kernel and format crates.
//! Storage determines identifiers and transport; this crate coordinates imported
//! layers, dirty generations and explicit reloads. No global resolver or cache.
//! AOUSD Core §9 (asset resolution), §16 (formats); OpenUSD `UsdStage::Open`,
//! `Save`, `SaveSessionLayers`, `Reload`. Packages are read-only source layers.
//!
//! With the default `std` feature, open a filesystem-backed stage explicitly:
//!
//! ```no_run
//! # #[cfg(feature = "std")]
//! # fn example() -> Result<(), layerstack_io::IoError> {
//! use layerstack::StageOptions;
//! use layerstack_io::{Filesystem, StageDocument};
//!
//! let storage = Filesystem::new(".", [])?;
//! let document = StageDocument::open(storage, "scene.usda", StageOptions::default())?;
//! println!("Loaded {} layers", document.load_report().layers.len());
//! # Ok(())
//! # }
//! ```
//!
//! For schema fallbacks and typed views, use [`StageDocument::open_in`]: build
//! the registry using that store's tokens, then pass the same store and its
//! registry in `StageOptions`. `open` creates a fresh store and cannot share a
//! prebuilt registry's token domain. [`Storage`] supports transports without `std`.
//! [`StageDocument::parts_mut`] provides explicit access for atomic authoring;
//! saving, exporting and reloading remain separate operations.
//!
//! [`StageDocument::open_with`] and [`StageDocument::open_in_with`] accept
//! [`LoadOptions`] for eager or retained USDC arrays and a per-file decoder
//! budget. These options also govern dependencies, package members and reloads.
//! With `std`, [`StageDocument::retained_values`] exposes decoder statistics.
//! Retained import still reads complete encoded files; package members each
//! retain an independent byte copy. Numeric payload failures surface through
//! checked attribute reads and [`IoErrorKind::Decode`] during save/export.
//!
//! [`StageDocument::prepare_reload`] separates import/validation from publication.
//! Dropping its candidate rejects it; [`PreparedReload::commit`] reuses the
//! candidate composition and preserves stage observers. Both document and
//! candidate provide `read_asset_bytes` for package-aware arbitrary resources;
//! pass the asset's winning authoring layer, not its composed prim's root layer.
#![no_std]
extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, Layer, LayerId, LayerStore, LiveStage,
    PathInterner, ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack_usdz::{ImportDiagnostic, MemberDiagnostic};
pub use layerstack_usdz::{UsdcArrayLoading, UsdcReadOptions};

mod reload;
pub use reload::PreparedReload;
mod asset_bytes;
pub use asset_bytes::{AssetByteSource, AssetBytes, AssetReadLimits};

#[cfg(feature = "std")]
mod filesystem;
#[cfg(feature = "std")]
pub use filesystem::Filesystem;

/// Error category retained separately from its explanatory message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoErrorKind {
    /// No source exists at the selected identifier.
    NotFound,
    /// Storage could not read or write the bytes.
    Storage,
    /// The format reader rejected the source or strict import found errors.
    Rejected,
    /// The requested format is unsupported.
    Unsupported,
    /// A retained numeric source failed to decode during save/export.
    Decode,
    /// Package source layers cannot be saved individually.
    ReadOnly,
    /// Reload would overwrite dirty authored state under preserve policy.
    DirtyReload,
    /// A selected layer is absent from the document's store.
    MissingLayer,
    /// The document has exhausted its layer identity space.
    IdentityExhausted,
}
/// A storage or format failure. Operations retain the layer/identifier context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IoError {
    /// Machine-readable category.
    pub kind: IoErrorKind,
    /// Human-readable evidence from storage or the format implementation.
    pub message: String,
    /// Original retained-array failure when `kind` is [`IoErrorKind::Decode`].
    pub array_read_error: Option<layerstack::ArrayReadError>,
}
impl IoError {
    /// Constructs an error for a host storage implementation.
    pub fn new(kind: IoErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            array_read_error: None,
        }
    }
}
impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}
impl core::error::Error for IoError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.array_read_error
            .as_ref()
            .map(|error| error as &dyn core::error::Error)
    }
}

/// Host-owned identifier policy and byte transport, usable without `std`.
/// `identify` must return stable canonical identifiers within this document.
/// Writes must either complete or return an error; atomic replacement is the
/// backend's responsibility. No retries or background work are scheduled.
pub trait Storage {
    /// Resolves an authored asset identifier relative to a source identifier.
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError>;
    /// Reads a complete source.
    fn read(&mut self, identifier: &str) -> Result<Vec<u8>, IoError>;
    /// Reads at most `max_bytes` of a resource, rejecting excessive size before
    /// allocating its complete payload. Implementations may read one extra byte
    /// to detect growth. The default fails without reading; it never falls back
    /// to the unbounded `read` method. AOUSD Core §9 (host asset transport).
    fn read_bounded(&mut self, _identifier: &str, _max_bytes: u64) -> Result<Vec<u8>, IoError> {
        Err(IoError::new(
            IoErrorKind::Unsupported,
            "storage does not implement bounded resource reads",
        ))
    }
    /// Writes a complete source.
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError>;
}
/// Whether recoverable malformed or unrepresented content may be imported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportPolicy {
    /// Reject errors instead of accepting a partial representation.
    #[default]
    Strict,
    /// Preserve available content and retain all recovery diagnostics.
    AllowRecovery,
}
/// Document import policy applied consistently to root, dependencies and package members.
/// Existing opening methods use strict, eager import. Set these options explicitly
/// to defer USDC numeric decoding; checked reads then report payload errors later.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// Whether recovery diagnostics reject an import.
    pub import_policy: ImportPolicy,
    /// Per-file USDC array loading and decoder work budget.
    pub usdc: UsdcReadOptions,
}
/// Explicit protection against overwriting unsaved authored state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReloadPolicy {
    /// Reject the entire reload if any selected layer is dirty.
    PreserveDirty,
    /// Replace selected dirty layers with their stored sources.
    DiscardDirty,
}
/// A successfully loaded batch, with format evidence available to the caller.
#[derive(Clone, Debug, Default)]
pub struct LoadReport {
    /// Layers materialized during this operation, including package members.
    pub layers: Vec<LayerId>,
    /// Format diagnostics with source/member identity and original spans/paths.
    pub diagnostics: Vec<MemberDiagnostic>,
    /// Asset transport or decoding failures, including failures hidden by a
    /// recoverable authored arc. Strict imports reject these.
    pub asset_failures: Vec<AssetFailure>,
}
impl LoadReport {
    /// Whether imported content was malformed or omitted during recovery.
    pub fn has_errors(&self) -> bool {
        !self.asset_failures.is_empty() || self.diagnostics.iter().any(|d| d.diagnostic.is_error())
    }
}
/// Evidence for a failed dependency read.
#[derive(Clone, Debug)]
pub struct AssetFailure {
    /// Authored asset path.
    pub asset: String,
    /// Layer that authored the request, when present.
    pub anchor: Option<LayerId>,
    /// Original transport or format failure.
    pub error: IoError,
}
/// Failure to save one layer. Other selected layers may have saved successfully.
#[derive(Clone, Debug)]
pub struct LayerFailure {
    /// Selected layer.
    pub layer: LayerId,
    /// Why this layer was not saved.
    pub error: IoError,
}
/// Save evidence; failed layers retain their dirty generations.
#[derive(Clone, Debug, Default)]
pub struct SaveReport {
    /// Layers written successfully.
    pub saved: Vec<LayerId>,
    /// Anonymous layers with no persistent identifier, left dirty.
    pub anonymous: Vec<LayerId>,
    /// Selected layers that could not be written, left dirty.
    pub failures: Vec<LayerFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Usda,
    Usdc,
    Usdz,
}
#[derive(Clone, Debug)]
struct Source {
    identifier: String,
    format: Format,
    package: Option<LayerId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Loading,
    Loaded,
}
#[derive(Clone, Debug, Default)]
struct Catalog {
    sources: BTreeMap<LayerId, Source>,
    ids: BTreeMap<String, LayerId>,
    states: BTreeMap<LayerId, State>,
    saved: BTreeMap<LayerId, u64>,
    reserved: BTreeSet<LayerId>,
    next: u64,
    package_bytes: BTreeMap<LayerId, Arc<[u8]>>,
    package_roots: BTreeMap<LayerId, String>,
    #[cfg(feature = "std")]
    retained_values: BTreeMap<LayerId, layerstack_usdc::RetainedValues>,
}
impl Catalog {
    fn allocate(&mut self) -> Result<LayerId, IoError> {
        loop {
            let id = LayerId(self.next);
            self.next = self.next.checked_add(1).ok_or_else(|| {
                IoError::new(IoErrorKind::IdentityExhausted, "layer identities exhausted")
            })?;
            if id != LayerId::UNRESOLVED && self.reserved.insert(id) {
                return Ok(id);
            }
        }
    }
}

/// One document's store, retained stage and source catalog. Owns no transport
/// policy beyond the supplied `Storage`. Multiple retained clients may compose
/// against `store`; each client synchronizes independently after source changes.
#[doc(alias = "UsdStage")]
pub struct StageDocument<B> {
    store: InMemoryStore,
    stage: LiveStage,
    storage: B,
    catalog: Catalog,
    import_policy: ImportPolicy,
    usdc_options: UsdcReadOptions,
    load_report: LoadReport,
}
impl<B: core::fmt::Debug> core::fmt::Debug for StageDocument<B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StageDocument")
            .field("storage", &self.storage)
            .field("catalog", &self.catalog)
            .field("import_policy", &self.import_policy)
            .field("usdc_options", &self.usdc_options)
            .field("load_report", &self.load_report)
            .finish_non_exhaustive()
    }
}
impl<B: Storage> StageDocument<B> {
    /// Creates a new USDA or USDC root bound to `target`; nothing is written
    /// until `save`. The new root remains dirty, including an empty document.
    #[doc(alias = "UsdStage::CreateNew")]
    #[doc(alias = "CreateNew")]
    pub fn create(storage: B, target: &str, options: StageOptions) -> Result<Self, IoError> {
        Self::create_in(storage, InMemoryStore::default(), target, options)
    }
    /// Creates a persistent root alongside host-owned schemas/session layers.
    pub fn create_in(
        storage: B,
        mut store: InMemoryStore,
        target: &str,
        options: StageOptions,
    ) -> Result<Self, IoError> {
        let identifier = storage.identify(target, None)?;
        let format = identifier_format(&identifier)?;
        if format == Format::Usdz {
            return Err(IoError::new(
                IoErrorKind::ReadOnly,
                "new packages require an explicit localization plan",
            ));
        }
        let mut catalog = Catalog {
            reserved: store.layers.keys().copied().collect(),
            ..Default::default()
        };
        let root = catalog.allocate()?;
        catalog.ids.insert(identifier.clone(), root);
        catalog.sources.insert(
            root,
            Source {
                identifier,
                format,
                package: None,
            },
        );
        catalog.states.insert(root, State::Loaded);
        store.insert_layer(Layer::new(root));
        let stage = LiveStage::compose(&mut store, root, options);
        Ok(Self {
            store,
            stage,
            storage,
            catalog,
            import_policy: ImportPolicy::Strict,
            usdc_options: UsdcReadOptions::default(),
            load_report: LoadReport::default(),
        })
    }
    /// Opens a stage with strict import into a new store.
    #[doc(alias = "UsdStage::Open")]
    #[doc(alias = "Open")]
    pub fn open(storage: B, asset: &str, options: StageOptions) -> Result<Self, IoError> {
        Self::open_with(storage, asset, options, LoadOptions::default())
    }
    /// Opens with explicit root/dependency import and per-file USDC policies.
    pub fn open_with(
        storage: B,
        asset: &str,
        options: StageOptions,
        loading: LoadOptions,
    ) -> Result<Self, IoError> {
        Self::open_in_with(storage, InMemoryStore::default(), asset, options, loading)
    }
    /// Opens into an existing store, allowing a host-owned session layer and
    /// schema registry. Existing identifiers remain reserved. Import is staged;
    /// failed reads never publish partially imported layers to a live stage.
    #[doc(alias = "UsdStage::Open")]
    #[doc(alias = "Open")]
    pub fn open_in(
        storage: B,
        store: InMemoryStore,
        asset: &str,
        options: StageOptions,
        import_policy: ImportPolicy,
    ) -> Result<Self, IoError> {
        Self::open_in_with(
            storage,
            store,
            asset,
            options,
            LoadOptions {
                import_policy,
                ..Default::default()
            },
        )
    }
    /// Opens with host-owned schemas/session layers and an explicit load policy.
    /// Registry tokens must belong to `store`, as for [`Self::open_in`]. Failed
    /// structural imports publish no layers; retained payloads are checked on demand.
    pub fn open_in_with(
        mut storage: B,
        mut store: InMemoryStore,
        asset: &str,
        options: StageOptions,
        loading: LoadOptions,
    ) -> Result<Self, IoError> {
        let import_policy = loading.import_policy;
        let mut catalog = Catalog {
            reserved: store.layers.keys().copied().collect(),
            ..Default::default()
        };
        let mut reader = Reader::new(&mut storage, &mut catalog, import_policy, loading.usdc);
        let resolved = reader.load(asset, None, &mut store.tokens, &mut store.paths)?;
        if let Some(layer) = resolved.layer {
            reader.pending.push(layer);
        }
        reader.resolve_expressions(
            &mut store,
            layerstack::LayerStackIdentifier {
                root: resolved.layer_id,
                session: options.session_layer,
            },
        )?;
        let report = reader.commit(&mut store);
        let stage = LiveStage::compose(&mut store, resolved.layer_id, options);
        Ok(Self {
            store,
            stage,
            storage,
            catalog,
            import_policy,
            usdc_options: loading.usdc,
            load_report: report,
        })
    }
    /// Loads an explicitly requested source, anchored to a resident layer.
    /// Useful for host-selected value-clip or expression-asset requests. New
    /// bindings become visible to each client at its next synchronization.
    pub fn load_asset(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
    ) -> Result<(LayerId, LoadReport), IoError> {
        let mut catalog = self.catalog.clone();
        catalog.reserved.extend(self.store.layers.keys().copied());
        let mut reader = Reader::new(
            &mut self.storage,
            &mut catalog,
            self.import_policy,
            self.usdc_options,
        );
        let resolved = reader.load(asset, anchor, &mut self.store.tokens, &mut self.store.paths)?;
        let id = resolved.layer_id;
        if let Some(anchor) = anchor {
            reader.bindings.insert((anchor, asset.into()), id);
        }
        if let Some(layer) = resolved.layer {
            reader.pending.push(layer);
        }
        reader.resolve_expressions(
            &mut self.store,
            layerstack::LayerStackIdentifier {
                root: self.stage.stage().root_layer().expect("document root"),
                session: self.stage.stage().session_layer(),
            },
        )?;
        let report = reader.commit(&mut self.store);
        self.catalog = catalog;
        self.synchronize();
        Ok((id, report))
    }
    /// The policy used for future explicit loads and reloads.
    pub fn load_options(&self) -> LoadOptions {
        LoadOptions {
            import_policy: self.import_policy,
            usdc: self.usdc_options,
        }
    }
    /// Cache inspection for a retained USDC layer without decoding arrays.
    /// `None` means an eager/non-USDC or unknown source. Cloned handles can inspect
    /// old snapshots after reload. Footprints exclude structural and composed data;
    /// demand-all queries retain encoded bytes plus decoded buffers.
    #[cfg(feature = "std")]
    pub fn retained_values(&self, layer: LayerId) -> Option<&layerstack_usdc::RetainedValues> {
        self.catalog.retained_values.get(&layer)
    }
    /// Authored layers and the document's shared interners.
    pub fn store(&self) -> &InMemoryStore {
        &self.store
    }
    /// Mutable authored store. After direct edits call `synchronize` on each client.
    pub fn store_mut(&mut self) -> &mut InMemoryStore {
        &mut self.store
    }
    /// Retained primary stage.
    pub fn stage(&self) -> &LiveStage {
        &self.stage
    }
    /// Disjoint mutable access for atomic stage edits and explicit controls.
    pub fn parts_mut(&mut self) -> (&mut InMemoryStore, &mut LiveStage) {
        (&mut self.store, &mut self.stage)
    }
    /// Host-owned transport, for inspecting writes or receiving incoming bytes.
    pub fn storage(&self) -> &B {
        &self.storage
    }
    /// Mutable transport. Changed source bytes require an explicit reload.
    pub fn storage_mut(&mut self) -> &mut B {
        &mut self.storage
    }
    /// Evidence from the initial import.
    pub fn load_report(&self) -> &LoadReport {
        &self.load_report
    }
    /// Identifier of a loaded layer. Package members use `package[member]`.
    pub fn identifier(&self, layer: LayerId) -> Option<&str> {
        self.catalog
            .sources
            .get(&layer)
            .map(|s| s.identifier.as_str())
    }
    /// Whether a layer has changed since import or its last successful save.
    /// Anonymous layers have no saved generation and remain dirty.
    pub fn is_dirty(&self, layer: LayerId) -> bool {
        self.store
            .layer(layer)
            .is_some_and(|l| self.catalog.saved.get(&layer) != Some(&l.generation()))
    }
    /// Creates an anonymous layer without colliding with imported identities.
    pub fn create_anonymous_layer(&mut self) -> Result<LayerId, IoError> {
        self.catalog
            .reserved
            .extend(self.store.layers.keys().copied());
        let id = self.catalog.allocate()?;
        self.store.insert_layer(Layer::new(id));
        Ok(id)
    }
    /// Synchronizes direct authored changes into the primary retained stage.
    pub fn synchronize(&mut self) {
        self.stage.synchronize(&mut self.store);
    }
    /// Saves dirty used layers, excluding the primary session and its sublayers.
    /// Only resident sources participate; anonymous layers are reported separately.
    #[doc(alias = "UsdStage::Save")]
    #[doc(alias = "Save")]
    pub fn save(&mut self) -> SaveReport {
        self.synchronize();
        let session = self.session_layers();
        let layers = self
            .stage
            .stage()
            .used_layers(true)
            .difference(&session)
            .copied()
            .collect::<Vec<_>>();
        self.save_layers(&layers)
    }
    /// Saves dirty resident layers in the primary session's sublayer stack only.
    #[doc(alias = "UsdStage::SaveSessionLayers")]
    #[doc(alias = "SaveSessionLayers")]
    pub fn save_session_layers(&mut self) -> SaveReport {
        self.synchronize();
        let layers: Vec<_> = self.session_layers().into_iter().collect();
        self.save_layers(&layers)
    }
    /// Saves the explicit selection. Unsupported content fails before the backend
    /// is called; each successful write advances only that layer's saved generation.
    pub fn save_layers(&mut self, layers: &[LayerId]) -> SaveReport {
        let mut report = SaveReport::default();
        for id in layers.iter().copied().collect::<BTreeSet<_>>() {
            if !self.store.layers.contains_key(&id) {
                report.failures.push(LayerFailure {
                    layer: id,
                    error: IoError::new(IoErrorKind::MissingLayer, "selected layer is absent"),
                });
                continue;
            }
            if !self.is_dirty(id) {
                continue;
            }
            let Some(source) = self.catalog.sources.get(&id).cloned() else {
                report.anonymous.push(id);
                continue;
            };
            let result = if source.package.is_some() {
                Err(IoError::new(
                    IoErrorKind::ReadOnly,
                    "package layers are read-only; export a localization plan instead",
                ))
            } else {
                self.serialize(id, source.format)
                    .and_then(|bytes| self.storage.write(&source.identifier, &bytes))
            };
            match result {
                Ok(()) => {
                    self.catalog
                        .saved
                        .insert(id, self.store.layers[&id].generation());
                    report.saved.push(id);
                }
                Err(error) => report.failures.push(LayerFailure { layer: id, error }),
            }
        }
        report
    }
    /// Exports an authored layer as USDA (`.usda`) or USDC (`.usd`/`.usdc`).
    /// Writes a copy: does not rebind arcs, rename the source or mark it saved.
    pub fn export_layer(&mut self, layer: LayerId, target: &str) -> Result<(), IoError> {
        let identifier = self.storage.identify(target, None)?;
        let format = identifier_format(&identifier)?;
        let bytes = self.serialize(layer, format)?;
        self.storage.write(&identifier, &bytes)
    }
    /// Writes an explicit, already localized package plan. Asset selection and
    /// package member naming belong to the caller and existing localization API.
    pub fn export_usdz(
        &mut self,
        target: &str,
        plan: &layerstack_usdz::localize::LocalizationPlan,
    ) -> Result<(), IoError> {
        let identifier = self.storage.identify(target, None)?;
        let bytes = plan
            .write_usdz(&self.store.tokens, &self.store.paths)
            .map_err(|error| match error {
                layerstack_usdz::localize::LocalizationError::Usda(error)
                | layerstack_usdz::localize::LocalizationError::Usdc(
                    layerstack_usdc::writer::UsdcWriteError::Save(error),
                ) => save_error(error),
                other => IoError::new(IoErrorKind::Unsupported, other.to_string()),
            })?;
        self.storage.write(&identifier, &bytes)
    }
    /// Reloads resident used sources, excluding anonymous/session layers.
    /// Equivalent to preparing and immediately committing [`Self::prepare_reload`].
    #[doc(alias = "UsdStage::Reload")]
    #[doc(alias = "Reload")]
    pub fn reload(&mut self, policy: ReloadPolicy) -> Result<LoadReport, IoError> {
        self.synchronize();
        Ok(self.prepare_reload(policy)?.commit())
    }
    /// Reloads an explicit source selection with no validation pause.
    /// Equivalent to preparing and immediately committing [`Self::prepare_reload_layers`].
    pub fn reload_layers(
        &mut self,
        layers: &[LayerId],
        policy: ReloadPolicy,
    ) -> Result<LoadReport, IoError> {
        Ok(self.prepare_reload_layers(layers, policy)?.commit())
    }
    fn serialize(&self, id: LayerId, format: Format) -> Result<Vec<u8>, IoError> {
        let layer = self
            .store
            .layer(id)
            .ok_or_else(|| IoError::new(IoErrorKind::MissingLayer, "selected layer is absent"))?;
        match format {
            Format::Usda => {
                layerstack_usda::save::save_usda(layer, &self.store.tokens, &self.store.paths)
                    .map(String::into_bytes)
                    .map_err(save_error)
            }
            Format::Usdc => {
                layerstack_usdc::writer::save_layer(layer, &self.store.tokens, &self.store.paths)
                    .map_err(|error| match error {
                        layerstack_usdc::writer::UsdcWriteError::Save(error) => save_error(error),
                        error => IoError::new(IoErrorKind::Unsupported, error.to_string()),
                    })
            }
            Format::Usdz => Err(IoError::new(
                IoErrorKind::Unsupported,
                "package export requires an explicit localization plan",
            )),
        }
    }
    fn session_layers(&self) -> BTreeSet<LayerId> {
        let mut layers = BTreeSet::new();
        let mut queue: Vec<_> = self.stage.stage().session_layer().into_iter().collect();
        while let Some(id) = queue.pop() {
            if !layers.insert(id) {
                continue;
            }
            if let Some(layer) = self.store.layer(id) {
                for sublayer in &layer.sublayers {
                    queue.push(sublayer.layer);
                }
            }
        }
        layers
    }
}

struct Reader<'a, B> {
    storage: &'a mut B,
    catalog: &'a mut Catalog,
    policy: ImportPolicy,
    usdc_options: UsdcReadOptions,
    pending: Vec<Layer>,
    diagnostics: Vec<MemberDiagnostic>,
    bindings: BTreeMap<(LayerId, String), LayerId>,
    failures: Vec<AssetFailure>,
    reachable_only: bool,
}
impl<'a, B: Storage> Reader<'a, B> {
    fn new(
        storage: &'a mut B,
        catalog: &'a mut Catalog,
        policy: ImportPolicy,
        usdc_options: UsdcReadOptions,
    ) -> Self {
        Self {
            storage,
            catalog,
            policy,
            usdc_options,
            pending: Vec::new(),
            diagnostics: Vec::new(),
            bindings: BTreeMap::new(),
            failures: Vec::new(),
            reachable_only: false,
        }
    }
    fn resolve_expressions(
        &mut self,
        store: &mut InMemoryStore,
        stack: layerstack::LayerStackIdentifier,
    ) -> Result<(), IoError> {
        let mut attempted = BTreeSet::new();
        loop {
            let overlay = Overlay {
                store,
                layers: &self.pending,
                bindings: &self.bindings,
            };
            let requests = layerstack::expression_asset_paths_for_stack(&overlay, stack);
            let mut progress = false;
            for request in requests {
                let key = (request.anchor, request.asset_path.clone());
                if !attempted.insert(key.clone()) {
                    continue;
                }
                progress = true;
                match self.load(
                    &request.asset_path,
                    Some(request.anchor),
                    &mut store.tokens,
                    &mut store.paths,
                ) {
                    Ok(resolved) => {
                        self.bindings.insert(key, resolved.layer_id);
                        if let Some(layer) = resolved.layer {
                            self.pending.push(layer);
                        }
                    }
                    Err(error) => {
                        self.bindings.insert(key, LayerId::UNRESOLVED);
                        if self.policy == ImportPolicy::Strict {
                            return Err(error);
                        }
                        self.diagnostics.push(MemberDiagnostic {
                            member: Arc::from(request.asset_path),
                            layer_id: request.anchor,
                            diagnostic: ImportDiagnostic::UsdaEmit(
                                layerstack_usda::diagnostic::Diagnostic::error(
                                    layerstack_usda::Span::new(0, 0),
                                    error.message,
                                ),
                            ),
                        });
                    }
                }
            }
            if !progress {
                return Ok(());
            }
        }
    }
    fn commit(mut self, store: &mut InMemoryStore) -> LoadReport {
        let clean_anchors: BTreeSet<_> = self
            .bindings
            .keys()
            .map(|(anchor, _)| *anchor)
            .filter(|id| {
                store
                    .layer(*id)
                    .is_some_and(|l| self.catalog.saved.get(id) == Some(&l.generation()))
            })
            .collect();
        let mut layers = BTreeSet::new();
        for layer in self.pending.drain(..) {
            let id = layer.id;
            if layers.insert(id) {
                store.insert_layer(layer);
            }
        }
        for ((anchor, asset), id) in self.bindings {
            if id == LayerId::UNRESOLVED {
                store.mark_asset_missing(anchor, &asset);
            } else {
                store.insert_asset_layer(anchor, &asset, id);
            }
        }
        for id in clean_anchors {
            self.catalog
                .saved
                .insert(id, store.layers[&id].generation());
        }
        for &id in &layers {
            self.catalog
                .saved
                .insert(id, store.layers[&id].generation());
        }
        LoadReport {
            layers: layers.into_iter().collect(),
            diagnostics: self.diagnostics,
            asset_failures: self.failures,
        }
    }
    fn read_layer(
        &mut self,
        data: &[u8],
        identifier: &str,
        id: LayerId,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<Layer, IoError> {
        let format = if data.starts_with(b"PXR-USDC") {
            Format::Usdc
        } else if data.starts_with(b"PK\x03\x04") {
            Format::Usdz
        } else {
            Format::Usda
        };
        self.catalog
            .sources
            .get_mut(&id)
            .expect("reserved source")
            .format = format;
        let first_diagnostic = self.diagnostics.len();
        let first_failure = self.failures.len();
        let layer = match format {
            Format::Usda => {
                let source = core::str::from_utf8(data)
                    .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
                let result = layerstack_usda::read_usda(source, id, tokens, paths, self);
                self.pending.extend(result.emitted.resolved_layers);
                for diagnostic in result
                    .parse_diagnostics
                    .into_iter()
                    .map(ImportDiagnostic::UsdaParse)
                    .chain(
                        result
                            .lower_diagnostics
                            .into_iter()
                            .map(ImportDiagnostic::UsdaLower),
                    )
                    .chain(
                        result
                            .emitted
                            .diagnostics
                            .into_iter()
                            .map(ImportDiagnostic::UsdaEmit),
                    )
                {
                    self.diagnostics.push(MemberDiagnostic {
                        member: Arc::from(identifier),
                        layer_id: id,
                        diagnostic,
                    });
                }
                if result.emitted.rejected {
                    return Err(IoError::new(
                        IoErrorKind::Rejected,
                        alloc::format!("{identifier}: USDA reader rejected the layer"),
                    ));
                }
                result.emitted.layer
            }
            Format::Usdc => {
                let mut budget = self.usdc_options.budget(data.len());
                #[cfg(feature = "std")]
                let result = match self.usdc_options.arrays {
                    UsdcArrayLoading::Retained => {
                        let result = layerstack_usdc::read_usdc_lazy_within(
                            Arc::from(data),
                            id,
                            tokens,
                            paths,
                            self,
                            budget,
                        )
                        .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
                        self.catalog.retained_values.insert(id, result.values);
                        result.assembled
                    }
                    UsdcArrayLoading::Eager => layerstack_usdc::read_usdc_within(
                        data,
                        id,
                        tokens,
                        paths,
                        self,
                        &mut budget,
                    )
                    .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?,
                };
                #[cfg(not(feature = "std"))]
                let result =
                    layerstack_usdc::read_usdc_within(data, id, tokens, paths, self, &mut budget)
                        .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
                self.pending.extend(result.resolved_layers);
                self.diagnostics
                    .extend(result.diagnostics.into_iter().map(|d| MemberDiagnostic {
                        member: Arc::from(identifier),
                        layer_id: id,
                        diagnostic: ImportDiagnostic::UsdcAssemble(d),
                    }));
                result.layer
            }
            Format::Usdz => {
                if self.reachable_only {
                    // A dependent archive may choose a different first member.
                    // Its old root alias must not claim the outer package ID
                    // when that former root becomes an ordinary member.
                    self.catalog
                        .ids
                        .retain(|alias, member_id| *member_id != id || alias == identifier);
                }
                let members: Vec<String> = self
                    .catalog
                    .sources
                    .iter()
                    .filter(|(member_id, s)| {
                        !self.reachable_only && **member_id != id && s.package == Some(id)
                    })
                    .filter_map(|(_, s)| {
                        s.identifier
                            .strip_suffix(']')
                            .and_then(|s| s.rsplit_once('[').map(|(_, m)| m.to_string()))
                    })
                    .collect();
                let selected: Vec<&str> = members.iter().map(String::as_str).collect();
                let usdc_options = self.usdc_options;
                let result = layerstack_usdz::read_usdz_with_options(
                    data,
                    id,
                    tokens,
                    paths,
                    self,
                    &selected,
                    usdc_options,
                )
                .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
                #[cfg(feature = "std")]
                self.catalog.retained_values.extend(result.retained_values);
                self.catalog.package_bytes.insert(id, Arc::from(data));
                self.catalog
                    .package_roots
                    .insert(id, result.member_paths[&id].to_string());
                for (&member_id, member) in &result.member_paths {
                    let member_identifier = alloc::format!("{identifier}[{member}]");
                    self.catalog.sources.insert(
                        member_id,
                        Source {
                            identifier: member_identifier.clone(),
                            format: identifier_format(member)?,
                            package: Some(id),
                        },
                    );
                    if member_id != id || !self.catalog.ids.contains_key(&member_identifier) {
                        self.catalog.ids.insert(member_identifier, member_id);
                    }
                    self.catalog.states.insert(member_id, State::Loaded);
                }
                // The package root is resolved by its outer identifier too.
                self.catalog
                    .sources
                    .get_mut(&id)
                    .expect("package root")
                    .identifier = identifier.into();
                let root_member = self.catalog.package_roots[&id].clone();
                let alias = alloc::format!("{identifier}[{root_member}]");
                if self
                    .catalog
                    .ids
                    .get(&alias)
                    .is_some_and(|alias_id| *alias_id != id)
                {
                    // The outer package and an explicitly resident member are
                    // distinct Sdf layers even when that member becomes first.
                    let resolved = self.load_package_asset(
                        &alloc::format!(
                            "./{}",
                            root_member.rsplit('/').next().expect("package root member")
                        ),
                        id,
                        id,
                        tokens,
                        paths,
                    )?;
                    if let Some(layer) = resolved.layer {
                        self.pending.push(layer);
                    }
                }
                self.pending.extend(result.resolved_layers);
                self.diagnostics.extend(result.diagnostics);
                result.layer
            }
        };
        if self.policy == ImportPolicy::Strict
            && let Some(failure) = self.failures.get(first_failure)
        {
            return Err(failure.error.clone());
        }
        if self.policy == ImportPolicy::Strict
            && self.diagnostics[first_diagnostic..]
                .iter()
                .any(|d| d.diagnostic.is_error())
        {
            return Err(IoError::new(
                IoErrorKind::Rejected,
                alloc::format!(
                    "{identifier}: strict import rejected malformed or unrepresented content"
                ),
            ));
        }
        Ok(layer)
    }
}
impl<B: Storage> Reader<'_, B> {
    fn load(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, IoError> {
        if let Some(anchor) = anchor
            && let Some(package) = self.catalog.sources.get(&anchor).and_then(|s| s.package)
            && self.catalog.states.get(&package) == Some(&State::Loaded)
        {
            return self.load_package_asset(asset, anchor, package, tokens, paths);
        }
        self.load_external(asset, anchor, tokens, paths)
    }
    fn load_package_asset(
        &mut self,
        asset: &str,
        anchor: LayerId,
        package: LayerId,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, IoError> {
        if asset.starts_with('/') || asset.contains(':') {
            return self.load_external(asset, Some(package), tokens, paths);
        }
        let bytes = self.catalog.package_bytes[&package].clone();
        let archive = layerstack_usdz::zip::ZipArchive::parse(&bytes)
            .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
        let Some(member) = self
            .catalog
            .package_member(&archive, asset, anchor, package)?
        else {
            return if asset.starts_with('.') {
                Err(IoError::new(
                    IoErrorKind::NotFound,
                    alloc::format!("missing package-relative asset {asset}"),
                ))
            } else {
                self.load_external(asset, Some(package), tokens, paths)
            };
        };
        let entry = archive.find(&member).expect("selected package member");
        let data = archive.entry_data(entry);
        let id = self
            .allocate_package_layer_id(package, &member)
            .ok_or_else(|| {
                IoError::new(
                    IoErrorKind::IdentityExhausted,
                    "package member identity unavailable",
                )
            })?;
        let identifier = alloc::format!("{}[{member}]", self.catalog.sources[&package].identifier);
        if self.catalog.states.contains_key(&id) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: Arc::from(identifier),
                layer: None,
            });
        }
        self.catalog.sources.insert(
            id,
            Source {
                identifier: identifier.clone(),
                format: identifier_format(&member)?,
                package: Some(package),
            },
        );
        self.catalog.states.insert(id, State::Loading);
        match self.read_layer(data, &identifier, id, tokens, paths) {
            Ok(layer) => {
                self.catalog.states.insert(id, State::Loaded);
                Ok(ResolvedAsset {
                    layer_id: id,
                    resolved_path: Arc::from(identifier),
                    layer: Some(layer),
                })
            }
            Err(error) => {
                self.catalog.states.remove(&id);
                Err(error)
            }
        }
    }
    fn load_external(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, IoError> {
        let anchor_identifier = anchor
            .and_then(|id| self.catalog.sources.get(&id))
            .map(|s| s.identifier.as_str());
        let identifier = self.storage.identify(asset, anchor_identifier)?;
        let id = if let Some(&id) = self.catalog.ids.get(&identifier) {
            id
        } else {
            let id = self.catalog.allocate()?;
            self.catalog.ids.insert(identifier.clone(), id);
            self.catalog.sources.insert(
                id,
                Source {
                    identifier: identifier.clone(),
                    format: Format::Usda,
                    package: None,
                },
            );
            id
        };
        if self.catalog.states.contains_key(&id) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: Arc::from(identifier),
                layer: None,
            });
        }
        let bytes = self.storage.read(&identifier)?;
        self.catalog.states.insert(id, State::Loading);
        match self.read_layer(&bytes, &identifier, id, tokens, paths) {
            Ok(layer) => {
                self.catalog.states.insert(id, State::Loaded);
                Ok(ResolvedAsset {
                    layer_id: id,
                    resolved_path: Arc::from(identifier),
                    layer: Some(layer),
                })
            }
            Err(error) => {
                self.catalog.states.remove(&id);
                Err(error)
            }
        }
    }
}
impl<B: Storage> AssetResolver for Reader<'_, B> {
    fn resolve(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        tokens: &mut TokenInterner,
        paths: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let result = self.load(asset, anchor, tokens, paths);
        if let Err(error) = &result {
            self.failures.push(AssetFailure {
                asset: asset.into(),
                anchor,
                error: error.clone(),
            });
        }
        if let Some(anchor) = anchor {
            self.bindings.insert(
                (anchor, asset.into()),
                result
                    .as_ref()
                    .map(|r| r.layer_id)
                    .unwrap_or(LayerId::UNRESOLVED),
            );
        }
        result.map_err(asset_error)
    }
    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.catalog.sources.get(&id).map(|s| s.identifier.as_str())
    }
    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.catalog.allocate().ok()
    }
    fn existing_package_layer_id(&self, package: LayerId, member: &str) -> Option<LayerId> {
        let identifier = alloc::format!(
            "{}[{member}]",
            self.catalog.sources.get(&package)?.identifier
        );
        self.catalog.ids.get(&identifier).copied()
    }
    fn allocate_package_layer_id(&mut self, package: LayerId, member: &str) -> Option<LayerId> {
        let identifier = alloc::format!(
            "{}[{member}]",
            self.catalog.sources.get(&package)?.identifier
        );
        if let Some(&id) = self.catalog.ids.get(&identifier) {
            return Some(id);
        }
        let id = self.catalog.allocate().ok()?;
        self.catalog.ids.insert(identifier, id);
        Some(id)
    }
}
fn save_error(error: layerstack_usda::save::SaveError) -> IoError {
    let message = error.to_string();
    if let layerstack_usda::save::SaveError::ArrayRead { error, .. } = error {
        IoError {
            kind: IoErrorKind::Decode,
            message,
            array_read_error: Some(error),
        }
    } else {
        IoError::new(IoErrorKind::Unsupported, message)
    }
}
fn asset_error(error: IoError) -> AssetResolveError {
    if error.kind == IoErrorKind::NotFound {
        AssetResolveError::NotFound
    } else {
        AssetResolveError::LoadError(Arc::from(error.message))
    }
}
fn identifier_format(identifier: &str) -> Result<Format, IoError> {
    let identifier = identifier
        .strip_suffix(']')
        .and_then(|s| s.rsplit_once('[').map(|(_, member)| member))
        .unwrap_or(identifier);
    match identifier.rsplit('.').next() {
        Some("usda") => Ok(Format::Usda),
        Some("usd" | "usdc") => Ok(Format::Usdc),
        Some("usdz") => Ok(Format::Usdz),
        _ => Err(IoError::new(
            IoErrorKind::Unsupported,
            "target requires .usda, .usd, .usdc or .usdz extension",
        )),
    }
}

#[cfg(test)]
mod tests;

struct Overlay<'a> {
    store: &'a InMemoryStore,
    layers: &'a [Layer],
    bindings: &'a BTreeMap<(LayerId, String), LayerId>,
}
impl LayerStore for Overlay<'_> {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        self.layers
            .iter()
            .rev()
            .find(|l| l.id == id)
            .or_else(|| self.store.layer(id))
    }
    fn layer_mut(&mut self, _: LayerId) -> Option<&mut Layer> {
        None
    }
    fn tokens(&self) -> &TokenInterner {
        &self.store.tokens
    }
    fn paths(&self) -> &PathInterner {
        &self.store.paths
    }
    fn tokens_mut(&mut self) -> &mut TokenInterner {
        panic!("read-only import overlay")
    }
    fn paths_mut(&mut self) -> &mut PathInterner {
        panic!("read-only import overlay")
    }
    fn asset_layer(&self, anchor: LayerId, asset: &str) -> Option<LayerId> {
        match self.bindings.get(&(anchor, asset.into())) {
            Some(&id) => (id != LayerId::UNRESOLVED).then_some(id),
            None => self.store.asset_layer(anchor, asset),
        }
    }
}
