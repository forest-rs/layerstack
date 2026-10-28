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
//! registry in `StageOptions`. Its metadata declarations also type and validate
//! USDA metadata during initial loading, explicit loads and reloads. `open`
//! creates a fresh store, so it rejects a registry
//! ([`IoErrorKind::ForeignSchemaRegistry`]): no prebuilt registry belongs to
//! that store's tokens. [`Storage`] supports transports without `std`.
//! [`StageDocument::apply`] authors the document: it applies a
//! [`layerstack::Transaction`] and loads the assets the edit asks for, such as
//! a deferred payload or a sublayer by asset path it adds. [`StageDocument::parts_mut`] gives the store and
//! live stage apart for lower-level control; saving, exporting and reloading
//! remain separate operations.
//!
//! [`StageDocument::open_with`] and [`StageDocument::open_in_with`] accept
//! [`LoadOptions`] for eager or retained USDC arrays and a per-file decoder
//! budget. These options also govern dependencies, package members and reloads.
//! With `std`, [`StageDocument::retained_values`] exposes decoder statistics.
//! Retained import reads complete encoded files; package members share checked
//! ranges into one immutable archive owner. Numeric payload failures surface through
//! checked attribute reads and [`IoErrorKind::Decode`] during save/export.
//!
//! Opening reads only the layers the stage's variant selections reach, as
//! OpenUSD does: an arc inside a variant branch its own layer does not select
//! is deferred, and composition asks for those its selections reach
//! ([`layerstack::Stage::asset_requests`]), which opening loads. After an edit
//! selects another variant, [`StageDocument::apply`] loads what it needs; after
//! a reload or an edit through [`StageDocument::parts_mut`],
//! [`StageDocument::load_requested_assets`] does; [`VariantArcLoading::All`] resolves every branch's
//! arcs up front instead.
//!
//! [`LoadOptions::formats`] registers host [`FileFormat`]s, such as procedural
//! generators, by extension. Format arguments in an asset path
//! (`terrain.grid:SDF_FORMAT_ARGS:rows=4`) are part of the layer's identity
//! and reach the format; see the [`format`] module.
//!
//! [`StageDocument::prepare_reload`] separates import/validation from publication.
//! Dropping its candidate rejects it; [`PreparedReload::commit`] reuses the
//! candidate composition and preserves stage observers. Both document and
//! candidate provide `read_asset_bytes` for package-aware arbitrary resources;
//! pass the asset's winning authoring layer, not its composed prim's root layer.
//! [`StageDocument::resolve_udim_tiles`] lists the existing tiles of a UDIM
//! texture (`color.<UDIM>.png`) through the same storage and packages, with
//! the storage work it spent.
//!
//! A host that fetches sources asynchronously, such as a browser, opens
//! with only what it has: its [`Storage`] reports a source it has not
//! received as [`IoErrorKind::Unavailable`], which leaves that arc or
//! sublayer waiting rather than missing. The loop is want, fetch, supply,
//! load: [`StageDocument::unavailable_assets`] lists the identifiers the
//! stage wants, the host supplies their bytes to its storage, and
//! [`StageDocument::load_requested_assets`] reads them, until nothing is
//! wanted. [`StageDocument::asset_status`] reports what the stage lacks now.
//! A USDZ package is one supplied source; its members resolve inside it.
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
use hashbrown::{HashMap, HashSet};
use layerstack::sort;
use layerstack::{
    AssetResolveError, AssetResolver, AssetUse, InMemoryStore, Layer, LayerId, LayerStore,
    LiveStage, PathId, PathInterner, ResolvedAsset, StageOptions, TokenInterner,
};
use layerstack::{asset::PackagePath, encoded_bytes::EncodedBytes, parallel::Threads};
use layerstack_usdz::snapshot::{PackageLimits, PackageSnapshot, PackageStats};
use layerstack_usdz::{ImportDiagnostic, MemberDiagnostic};
pub use layerstack_usdz::{UsdcArrayLoading, UsdcReadOptions};

mod apply;
pub use apply::ApplyError;
pub mod format;
mod layer_ids;
pub use format::{
    FORMAT_ARGS_DELIMITER, FileFormat, FileFormats, FormatArgs, FormatRegistrationError,
    FormatSource,
};
mod reload;
pub use reload::PreparedReload;
mod asset_bytes;
mod clips;
mod package;
mod udim;
pub use asset_bytes::{AssetByteSource, AssetBytes, AssetReadLimits};
pub use clips::{ClipLoadFailure, ClipQueryPreparation};
pub use package::{PackageInclusion, PackagePreparationLimits, PreparedPackage};
pub use udim::{UdimDiscovery, UdimDiscoveryWork, UdimTileAsset, UdimTiles};

#[cfg(feature = "std")]
mod filesystem;
#[cfg(feature = "std")]
mod parallel_open;
#[cfg(feature = "std")]
pub use filesystem::Filesystem;

/// Error category retained separately from its explanatory message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IoErrorKind {
    /// No source exists at the selected identifier.
    NotFound,
    /// The storage cannot supply the source yet, but may later: a host
    /// fetching sources asynchronously, such as a browser, has not
    /// received it. Unlike [`Self::NotFound`], a dependency read with this
    /// outcome is not missing: it stays unbound, the stage keeps asking for
    /// it ([`layerstack::Stage::asset_requests`]),
    /// [`StageDocument::unavailable_assets`] lists it with the prims
    /// composed without it ([`UnavailableAsset::prims`]), and
    /// [`StageDocument::load_requested_assets`] reads it again. A root
    /// layer, or an asset [`StageDocument::load_asset`] is asked for,
    /// that is unavailable fails that call with this kind.
    Unavailable,
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
    /// The schema registry of `StageOptions::schemas` was built on another
    /// token interner than the document's store
    /// ([`layerstack::SchemaRegistry::belongs_to`]), so its ids would name
    /// other strings there and no prim would match its schemas.
    ForeignSchemaRegistry,
}
impl IoErrorKind {
    /// The stable code naming this kind, for hosts and bindings that branch
    /// on it without matching the enum: `io.` and the variant's name in
    /// `snake_case`.
    ///
    /// | Variant | Code |
    /// |---|---|
    /// | [`Self::NotFound`] | `io.not_found` |
    /// | [`Self::Unavailable`] | `io.unavailable` |
    /// | [`Self::Storage`] | `io.storage` |
    /// | [`Self::Rejected`] | `io.rejected` |
    /// | [`Self::Unsupported`] | `io.unsupported` |
    /// | [`Self::Decode`] | `io.decode` |
    /// | [`Self::ReadOnly`] | `io.read_only` |
    /// | [`Self::DirtyReload`] | `io.dirty_reload` |
    /// | [`Self::MissingLayer`] | `io.missing_layer` |
    /// | [`Self::IdentityExhausted`] | `io.identity_exhausted` |
    /// | [`Self::ForeignSchemaRegistry`] | `io.foreign_schema_registry` |
    ///
    /// Codes are part of the public contract: a released code keeps its
    /// spelling and meaning, and a new variant brings a new code. The
    /// `io.`, `edit.` and `composition.` codes are the whole contract: the
    /// format crates' errors have no codes, and reach an [`IoError`] as
    /// its typed [`IoError::cause`].
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotFound => "io.not_found",
            Self::Unavailable => "io.unavailable",
            Self::Storage => "io.storage",
            Self::Rejected => "io.rejected",
            Self::Unsupported => "io.unsupported",
            Self::Decode => "io.decode",
            Self::ReadOnly => "io.read_only",
            Self::DirtyReload => "io.dirty_reload",
            Self::MissingLayer => "io.missing_layer",
            Self::IdentityExhausted => "io.identity_exhausted",
            Self::ForeignSchemaRegistry => "io.foreign_schema_registry",
        }
    }
}
/// A storage or format failure, naming the source it is about.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IoError {
    /// Machine-readable category.
    pub kind: IoErrorKind,
    /// Human-readable evidence from storage or the format implementation.
    pub message: String,
    /// The source the failed operation addressed: the identifier it read,
    /// wrote or rejected, or the asset path as authored when resolving it
    /// failed. `None` only for a failure no source is involved in, or one a
    /// host [`Storage`] returned directly to its own caller.
    pub identifier: Option<String>,
    /// For [`IoErrorKind::Rejected`] by a strict import, or by a USDA
    /// reader refusing a layer as OpenUSD refuses it, the error diagnostics
    /// of the source it rejected, with their spans; empty otherwise.
    pub diagnostics: Vec<MemberDiagnostic>,
    /// The format crate's typed error behind a failed save or export
    /// ([`Self::cause`]), shared so that the error stays small and cheap
    /// to clone.
    cause: Option<Arc<IoCause>>,
}
/// The typed error of a format crate behind an [`IoError`]: what a save,
/// export or package localization refused, kept whole so that its spec
/// path and variant are not reduced to a message.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum IoCause {
    /// Lowering or writing a layer as USDA, or lowering it for USDC,
    /// failed ([`IoErrorKind::Rejected`] for a layer that cannot be
    /// written as it stands, [`IoErrorKind::Decode`] for a retained array
    /// that could not decode, [`IoErrorKind::Unsupported`] otherwise).
    Save(layerstack_usda::save::SaveError),
    /// Writing a layer as USDC failed.
    UsdcWrite(layerstack_usdc::writer::UsdcWriteError),
    /// Localizing a package's layers and assets failed.
    Localization(layerstack_usdz::localize::LocalizationError),
}
impl IoCause {
    /// The cause as an error.
    fn as_error(&self) -> &(dyn core::error::Error + 'static) {
        match self {
            Self::Save(error) => error,
            Self::UsdcWrite(error) => error,
            Self::Localization(error) => error,
        }
    }
    /// The save error inside the cause, however it is wrapped.
    fn save_error(&self) -> Option<&layerstack_usda::save::SaveError> {
        match self {
            Self::Save(error)
            | Self::UsdcWrite(layerstack_usdc::writer::UsdcWriteError::Save(error))
            | Self::Localization(
                layerstack_usdz::localize::LocalizationError::Usda(error)
                | layerstack_usdz::localize::LocalizationError::Usdc(
                    layerstack_usdc::writer::UsdcWriteError::Save(error),
                ),
            ) => Some(error),
            _ => None,
        }
    }
}
impl IoError {
    /// Constructs an error for a host storage implementation.
    pub fn new(kind: IoErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            identifier: None,
            diagnostics: Vec::new(),
            cause: None,
        }
    }
    /// The format crate's typed error that caused a failed save or
    /// export, naming the spec it is about; `None` for other failures.
    /// [`core::error::Error::source`] returns it too.
    #[must_use]
    pub fn cause(&self) -> Option<&IoCause> {
        self.cause.as_deref()
    }
    /// The retained array or deferred value that could not decode, when
    /// `kind` is [`IoErrorKind::Decode`]: the cause's
    /// [`layerstack_usda::save::SaveError::ArrayRead`].
    #[must_use]
    pub fn array_read_error(&self) -> Option<&layerstack::ArrayReadError> {
        match self.cause()?.save_error()? {
            layerstack_usda::save::SaveError::ArrayRead { error, .. } => Some(error),
            _ => None,
        }
    }
    /// The stable code of the error's kind ([`IoErrorKind::code`]).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.kind.code()
    }
    /// This error, naming `identifier` as its source unless it names one
    /// already.
    #[must_use]
    pub fn at(mut self, identifier: &str) -> Self {
        if self.identifier.is_none() {
            self.identifier = Some(String::from(identifier));
        }
        self
    }
}
impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(identifier) = &self.identifier {
            write!(f, "{identifier}: ")?;
        }
        f.write_str(&self.message)
    }
}
impl core::error::Error for IoError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        self.cause().map(IoCause::as_error)
    }
}

/// Host-owned identifier policy and byte transport, usable without `std`.
/// `identify` must return stable canonical identifiers within this document.
/// A read of a source the host cannot supply yet returns
/// [`IoErrorKind::Unavailable`], one it knows does not exist
/// [`IoErrorKind::NotFound`].
/// Reads take `&self`: with the `std` feature a storage is [`Sync`]
/// ([`layerstack::MaybeSync`]), and opening a stage may identify and read
/// several sources at once from different threads.
/// Writes must either complete or return an error; atomic replacement is the
/// backend's responsibility. No retries or background work are scheduled.
/// A document saves USDA through [`Self::write_streamed`], other formats
/// through [`Self::write`].
pub trait Storage: layerstack::MaybeSync {
    /// Resolves an authored asset identifier relative to a source identifier.
    fn identify(&self, asset: &str, anchor: Option<&str>) -> Result<String, IoError>;
    /// Reads a complete source.
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError>;
    /// Reads immutable encoded storage, permitting an ownership-preserving
    /// transport or cache hit. The default transfers [`Self::read`]'s vector
    /// without copying payload bytes. Source owners and their views stay valid
    /// independently of later reads or filesystem replacement.
    fn read_encoded(&self, identifier: &str) -> Result<EncodedBytes, IoError> {
        self.read(identifier).map(EncodedBytes::from)
    }
    /// Reads at most `max_bytes` of a resource, rejecting excessive size before
    /// allocating its complete payload. Implementations may read one extra byte
    /// to detect growth. The default fails without reading; it never falls back
    /// to the unbounded `read` method. AOUSD Core §9 (host asset transport).
    fn read_bounded(&self, _identifier: &str, _max_bytes: u64) -> Result<Vec<u8>, IoError> {
        Err(IoError::new(
            IoErrorKind::Unsupported,
            "storage does not implement bounded resource reads",
        ))
    }
    /// Reads bounded immutable encoded storage without copying a returned vector.
    /// The default uses [`Self::read_bounded`], including its unsupported result;
    /// it never falls back to an unbounded read. Overrides must enforce the bound
    /// before allocating or returning the payload. AOUSD Core §9 (host transport).
    fn read_encoded_bounded(
        &self,
        identifier: &str,
        max_bytes: u64,
    ) -> Result<EncodedBytes, IoError> {
        self.read_bounded(identifier, max_bytes)
            .map(EncodedBytes::from)
    }
    /// Opens a fixed logical source for explicit package range imports through
    /// [`layerstack_usdz::range::RangePackage`] and [`layerstack_usdz::read_usdz_ranges`].
    /// The source must satisfy [`layerstack_usdz::range::RangeSource`]'s stability
    /// contract. The default fails without reading; there is no full-read fallback.
    /// [`StageDocument`] continues to use complete package verification.
    ///
    /// ```no_run
    /// # #[cfg(feature = "std")]
    /// # fn example() -> Result<(), Box<dyn core::error::Error>> {
    /// use layerstack_io::{Filesystem, Storage};
    /// use layerstack_usdz::range::{RangeLimits, RangePackage};
    /// let mut storage = Filesystem::new(".", [])?;
    /// let identifier = storage.identify("scene.usdz", None)?;
    /// let mut package = RangePackage::open(storage.open_ranges(&identifier)?, RangeLimits::default())?;
    /// let root = package.root_member().ok_or(layerstack_usdz::UsdzError::NoRootLayer)?.to_owned();
    /// let selected = package.layer(&root)?;
    /// println!("{} bytes, {:?}", selected.bytes.len(), package.stats());
    /// # Ok(())
    /// # }
    /// ```
    fn open_ranges(
        &self,
        _identifier: &str,
    ) -> Result<Arc<dyn layerstack_usdz::range::RangeSource<Error = IoError> + Send + Sync>, IoError>
    {
        Err(IoError::new(
            IoErrorKind::Unsupported,
            "storage does not implement range sources",
        ))
    }
    /// Whether `identifier`, as [`Self::identify`] returns it, names an
    /// existing source, reading none of it. UDIM tile discovery
    /// ([`StageDocument::resolve_udim_tiles`]) asks this of tile spellings,
    /// as OpenUSD asks `ArResolver::Resolve`. The default fails with
    /// [`IoErrorKind::Unsupported`]; it never reads the source to find out.
    fn exists(&self, _identifier: &str) -> Result<bool, IoError> {
        Err(IoError::new(
            IoErrorKind::Unsupported,
            "storage does not implement existence queries",
        ))
    }
    /// The identifiers of every existing source in the same directory as
    /// `identifier`, which need not exist itself, spelled as
    /// [`Self::identify`] spells them; an absent directory lists as empty.
    /// `Ok(None)` when the backend cannot enumerate, the default: callers
    /// then probe with [`Self::exists`]. One listing replaces up to 99
    /// probes for the tiles of a UDIM set; a [`UdimDiscovery`] keeps it for
    /// the directory's other sets, keyed by the identifier's text before its
    /// last `/` or `\`, which the identifiers of one directory must share.
    fn list_siblings(&self, _identifier: &str) -> Result<Option<Vec<String>>, IoError> {
        Ok(None)
    }
    /// Writes a complete source.
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError>;
    /// Writes a complete source whose bytes `produce` hands, in order, to the
    /// sink it is given, so that neither side holds them all.
    ///
    /// The source is written only when `produce` returns `Ok` and every
    /// write to the sink succeeds; otherwise this returns the first error
    /// (`produce` returns a sink error it was given) and leaves an existing
    /// source as it was, as a failed [`Self::write`] does. [`Filesystem`]
    /// streams into the temporary file it renames over the destination. The
    /// default gathers the bytes and calls [`Self::write`] once, after
    /// `produce` succeeds.
    fn write_streamed(
        &mut self,
        identifier: &str,
        produce: &mut dyn FnMut(&mut dyn ByteSink) -> Result<(), IoError>,
    ) -> Result<(), IoError> {
        let mut bytes = Vec::new();
        produce(&mut bytes)?;
        self.write(identifier, &bytes)
    }
}

/// Where [`Storage::write_streamed`] sends a source's bytes, in order.
pub trait ByteSink {
    /// Appends `bytes` to the source being written.
    ///
    /// # Errors
    ///
    /// The backend's error; the write then fails as a whole and leaves an
    /// existing source as it was.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), IoError>;
}

impl ByteSink for Vec<u8> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), IoError> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// USDA text into a [`ByteSink`], keeping the sink's first error for the
/// caller ([`core::fmt::Write`] reports none).
struct TextSink<'s> {
    sink: &'s mut dyn ByteSink,
    error: Option<IoError>,
}

impl core::fmt::Write for TextSink<'_> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.sink.write_bytes(text.as_bytes()).map_err(|error| {
            self.error = Some(error);
            core::fmt::Error
        })
    }
}
/// Whether recoverable malformed or unrepresented content may be imported.
///
/// The policy governs the content of the layers read, not whether the
/// layers a scene depends on exist. Under either policy, a reference,
/// payload or sublayer whose asset is not found
/// ([`IoErrorKind::NotFound`]) does not fail the open, load or reload: the
/// arc contributes nothing, the miss is kept in
/// [`LoadReport::asset_failures`] ([`AssetFailure::is_missing`]), and
/// composition reports it as [`layerstack::CompositionError::UnresolvedAsset`]
/// or [`layerstack::CompositionError::UnresolvedSublayer`]. A missing package
/// member is kept as an [`ImportDiagnostic::AssetResolve`] instead. A root
/// layer that is not found still fails the open, as does an asset
/// [`StageDocument::load_asset`] is asked for.
///
/// Spec: AOUSD Core §9 (asset resolution), §10.3.1 (sublayers), §10.3.2.1
/// ("If a layer stack cannot be computed for a reference's layer asset path,
/// it is a composition error and that reference is ignored"), §10.3.2.2,
/// §10.6. OpenUSD opens the stage and reports `PcpErrorInvalidAssetPath` and
/// `PcpErrorInvalidSublayerPath`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportPolicy {
    /// Reject a layer whose content is malformed or not represented: an
    /// error diagnostic from its format reader, or a dependency that was
    /// found but could not be read or was itself rejected (any
    /// [`AssetFailure`] that [`AssetFailure::is_missing`] does not report).
    /// The rejection reaches the open, load or reload that read the layer.
    ///
    /// This is stricter than OpenUSD, which opens a stage whose dependency
    /// fails to parse, ignores that dependency and reports it as
    /// `PcpErrorInvalidAssetPath`, as it does a missing one.
    /// [`Self::AllowRecovery`] opens it too, keeping what parses and the
    /// reader's diagnostics.
    #[default]
    Strict,
    /// Preserve available content and retain all recovery diagnostics.
    AllowRecovery,
}
/// Which references and payloads authored inside variant branches import
/// resolves as it reads a layer.
///
/// Spec: AOUSD Core §10.3.2.5 (only the selected variant contributes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum VariantArcLoading {
    /// Those in the branches the authoring layer selects itself. The rest
    /// are deferred ([`layerstack::Reference::deferred`]): composition asks
    /// for those its selections reach ([`layerstack::Stage::asset_requests`]),
    /// which opening loads before it returns and
    /// [`StageDocument::load_requested_assets`] loads after an edit. Opening
    /// reads only the layers the stage can use, as OpenUSD does.
    #[default]
    Selected,
    /// Every branch's, so that selecting another variant reads no source.
    All,
}
/// Document import policy applied consistently to root, dependencies and package members.
/// Existing opening methods use strict import. Set these options explicitly
/// to defer USDC numeric decoding; checked reads then report payload errors later.
///
/// Build it as a struct literal that names the fields it sets and takes
/// the rest from `..LoadOptions::default()`: a field a later release adds then
/// keeps its default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadOptions {
    /// Whether recovery diagnostics reject an import.
    pub import_policy: ImportPolicy,
    /// Per-file USDC array loading and decoder work budget.
    pub usdc: UsdcReadOptions,
    /// Cumulative directory, nesting and checksum work limits for each package.
    pub package: PackageLimits,
    /// Which arcs authored inside variant branches import resolves.
    pub variant_arcs: VariantArcLoading,
    /// File formats read beyond USDA, USDC and USDZ, by extension.
    pub formats: FileFormats,
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
#[non_exhaustive]
pub struct LoadReport {
    /// Layers materialized during this operation, including package members.
    pub layers: Vec<LayerId>,
    /// Format diagnostics with source/member identity and original spans/paths.
    pub diagnostics: Vec<MemberDiagnostic>,
    /// Asset transport or decoding failures, including failures hidden by a
    /// recoverable authored arc. Strict imports reject these, except a
    /// dependency that is not found ([`AssetFailure::is_missing`]), which
    /// both policies keep here.
    pub asset_failures: Vec<AssetFailure>,
}
impl LoadReport {
    /// Appends the evidence of a later load of the same document.
    fn merge(&mut self, other: Self) {
        self.layers.extend(other.layers);
        self.diagnostics.extend(other.diagnostics);
        self.asset_failures.extend(other.asset_failures);
    }
    /// Whether imported content was malformed, omitted during recovery, or
    /// named a dependency that was not found.
    pub fn has_errors(&self) -> bool {
        !self.asset_failures.is_empty() || self.diagnostics.iter().any(|d| d.diagnostic.is_error())
    }
}
/// Evidence for a failed dependency read.
///
/// `Display` names the asset as authored and the transport or format
/// failure; [`StageDocument::identifier`] names the `anchor` layer.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AssetFailure {
    /// Authored asset path.
    pub asset_path: String,
    /// Layer that authored the request, when present.
    pub anchor: Option<LayerId>,
    /// Original transport or format failure.
    pub error: IoError,
}
impl AssetFailure {
    /// Whether the asset was not found ([`IoErrorKind::NotFound`]): a
    /// missing dependency, which neither [`ImportPolicy`] rejects.
    ///
    /// Spec: AOUSD Core §10.3.2.1, §10.6 (a composition error, not a read
    /// failure). OpenUSD: `PcpErrorInvalidAssetPath`.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.error.kind == IoErrorKind::NotFound
    }
}
impl core::fmt::Display for AssetFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "could not open asset @{}@: {}",
            self.asset_path, self.error
        )
    }
}
/// The failure among `failures` that a strict import rejects its layer
/// with: the first that is not a missing dependency.
fn strict_failure(failures: &[AssetFailure]) -> Option<&AssetFailure> {
    failures.iter().find(|failure| !failure.is_missing())
}
/// Whether a strict import rejects its layer for `diagnostic`: an error
/// other than a package member that is not found, which is a missing
/// dependency (AOUSD Core §9.7, §10.6).
fn strict_rejects(diagnostic: &MemberDiagnostic) -> bool {
    diagnostic.diagnostic.is_error()
        && !matches!(
            diagnostic.diagnostic,
            ImportDiagnostic::AssetResolve {
                error: AssetResolveError::NotFound,
                ..
            }
        )
}
/// An asset the stage asks for that storage cannot supply yet
/// ([`IoErrorKind::Unavailable`]): it is unavailable, and may still
/// arrive. See [`StageDocument::unavailable_assets`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnavailableAsset {
    /// The source to supply, as [`Storage::identify`] named it for
    /// `asset_path` anchored to `anchor`: the key a host fetches and
    /// supplies bytes under. Several entries may share it.
    pub identifier: String,
    /// The layer authoring the arc or sublayer, which a relative
    /// `asset_path` is anchored to ([`StageDocument::identifier`] names it).
    pub anchor: LayerId,
    /// The asset path as authored.
    pub asset_path: String,
    /// What uses it: a sublayer, a reference or a payload.
    pub used_by: AssetUse,
    /// The composed prims composed without it, each once, in the order the
    /// stage lists its requests ([`layerstack::AssetRequest::prim`]): the
    /// prims whose reference or payload names it, or for a sublayer the
    /// first prim to read its layer stack, the pseudo-root for the stage's
    /// own. They are on the stage, without the asset's opinions, until it
    /// loads.
    pub prims: Vec<PathId>,
}
/// An asset the stage uses that is unresolved: composition reports it
/// ([`layerstack::CompositionError::UnresolvedAsset`] or
/// [`layerstack::CompositionError::UnresolvedSublayer`]) and its arc or
/// sublayer contributes nothing, because the asset is missing or its read
/// failed. See [`StageDocument::asset_status`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnresolvedAsset {
    /// The layer authoring the arc or sublayer, which a relative
    /// `asset_path` is anchored to.
    pub anchor: LayerId,
    /// The asset path as authored (as evaluated, for a variable
    /// expression or a dynamic payload's arguments).
    pub asset_path: String,
    /// What uses it: a sublayer, a reference or a payload.
    pub used_by: AssetUse,
    /// The source a read of it addressed ([`IoError::identifier`]), when
    /// the document read it and storage named one.
    pub identifier: Option<String>,
    /// Why a read of it failed: [`IoErrorKind::NotFound`] for an asset
    /// confirmed missing ([`Self::is_missing`]), another kind for one that
    /// was found but could not be read or was rejected, which only
    /// [`ImportPolicy::AllowRecovery`] keeps. `None` when no read of it
    /// failed in this document: the layer authoring the arc recorded it
    /// unresolved itself.
    pub reason: Option<IoErrorKind>,
}
impl UnresolvedAsset {
    /// Whether the asset is confirmed missing: its read found no source
    /// ([`IoErrorKind::NotFound`]).
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.reason == Some(IoErrorKind::NotFound)
    }
}
/// The assets a document's stage lacks now ([`StageDocument::asset_status`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AssetStatus {
    /// Assets storage cannot supply yet, which a later load can read
    /// ([`StageDocument::unavailable_assets`]).
    pub unavailable: Vec<UnavailableAsset>,
    /// Assets that contribute nothing, missing or failed to read, each
    /// once, in the order composition reports them.
    pub unresolved: Vec<UnresolvedAsset>,
}
impl AssetStatus {
    /// Whether the stage lacks no asset.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unavailable.is_empty() && self.unresolved.is_empty()
    }
}
/// Failure to save one layer. Other selected layers may have saved successfully.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct LayerFailure {
    /// Selected layer.
    pub layer: LayerId,
    /// Why this layer was not saved.
    pub error: IoError,
}
/// Save evidence; failed layers retain their dirty generations.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
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
    /// The registered format at this index of the document's [`FileFormats`].
    Custom(usize),
}
#[derive(Clone, Debug)]
struct Source {
    /// Shared with [`Catalog::ids`]' key, so recording a source copies its
    /// identifier once, as it is read.
    identifier: Arc<str>,
    format: Format,
    package: Option<LayerId>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Loading,
    Loaded,
}
/// The sources a document reads, by layer id, and the ids it has handed
/// out: a layer named by identifier gets one derived from it
/// ([`layer_ids`]), an anonymous layer the next free one below
/// [`LayerId::FIRST_DERIVED`] ([`Catalog::allocate`]).
#[derive(Clone, Debug, Default)]
struct Catalog {
    cache_work: PackageCacheStats,
    sources: BTreeMap<LayerId, Source>,
    /// The layer each identifier names. Looked up for every arc a load
    /// resolves and never listed in order, so hashed: identifiers of one
    /// scene share long directory prefixes that ordered keys compare again
    /// on every lookup.
    ids: HashMap<Arc<str>, LayerId>,
    states: BTreeMap<LayerId, State>,
    saved: BTreeMap<LayerId, u64>,
    /// Every id handed out or held by the store, which no new layer takes.
    reserved: BTreeSet<LayerId>,
    /// Where [`Catalog::allocate`] looks for a free id next.
    next: u64,
    package_bytes: BTreeMap<LayerId, Arc<PackageSnapshot>>,
    package_roots: BTreeMap<LayerId, String>,
    #[cfg(feature = "std")]
    retained_values: BTreeMap<LayerId, layerstack_usdc::RetainedValues>,
    /// The latest failed read of each dependency, by anchor layer and
    /// authored asset path, which [`StageDocument::asset_status`] gives
    /// the unresolved assets composition reports.
    failures: BTreeMap<(LayerId, String), ReadFailure>,
}
/// Why a dependency read failed, kept for [`UnresolvedAsset`].
#[derive(Clone, Debug)]
struct ReadFailure {
    /// The source the read addressed ([`IoError::identifier`]).
    identifier: Option<String>,
    /// The failure's kind.
    kind: IoErrorKind,
}
/// Package transport/index work since creation. Candidate work is published only
/// on commit. Counts exclude numeric decoding and structural layer assembly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PackageCacheStats {
    /// Fresh package trees opened and completely verified.
    pub opens: usize,
    /// Directories parsed, including nested archives.
    pub index_parses: usize,
    /// Selections served from a resident immutable package index.
    pub index_hits: usize,
    /// Source/resource bytes copied into immutable owners after host transport.
    /// Includes loose files; storage's own copies are outside this count.
    pub encoded_bytes_copied: u64,
    /// Unique encoded bytes in the current resident package catalog. Old external
    /// holders and retained loose USDC pools are outside this count.
    pub resident_backing_bytes: u64,
}
impl Catalog {
    fn cache_stats(&self) -> PackageCacheStats {
        let mut stats = self.cache_work;
        let mut seen: Vec<&Arc<PackageSnapshot>> = Vec::new();
        for archive in self.package_bytes.values() {
            if !seen.iter().any(|old| Arc::ptr_eq(old, archive)) {
                stats.resident_backing_bytes += archive.stats().backing_bytes as u64;
                seen.push(archive);
            }
        }
        stats
    }
    fn package_stats(&self, layer: LayerId) -> Option<PackageStats> {
        let package = self.sources.get(&layer)?.package.unwrap_or(layer);
        self.package_bytes
            .get(&package)
            .map(|archive| archive.stats())
    }
    fn package_identifier(&self, package: LayerId) -> &str {
        let identifier = &self.sources[&package].identifier;
        identifier
            .split_once('[')
            .map_or(identifier, |(outer, _)| outer)
    }
    /// A new id for a layer no identifier names, such as an anonymous
    /// one: the next free one below [`LayerId::FIRST_DERIVED`], the range
    /// hosts and stores hand out.
    fn allocate(&mut self) -> Result<LayerId, IoError> {
        loop {
            let id = LayerId(self.next);
            if id >= LayerId::FIRST_DERIVED {
                return Err(IoError::new(
                    IoErrorKind::IdentityExhausted,
                    "layer identities exhausted",
                ));
            }
            self.next += 1;
            if self.reserved.insert(id) {
                return Ok(id);
            }
        }
    }
    /// The id of the layer `identifier` names, new to the catalog: derived
    /// from the identifier ([`layer_ids`]), stepping past the ids taken.
    fn allocate_for(&mut self, identifier: &str) -> Result<LayerId, IoError> {
        let id = layer_ids::derive(identifier, |id| self.reserved.contains(&id))?;
        self.reserved.insert(id);
        Ok(id)
    }
}

/// Rejects a schema registry built on another token interner than
/// `store`'s, before anything is read or composed with it.
fn check_schemas(store: &InMemoryStore, options: &StageOptions) -> Result<(), IoError> {
    match &options.schemas {
        Some(schemas) if !schemas.belongs_to(store.tokens()) => Err(IoError::new(
            IoErrorKind::ForeignSchemaRegistry,
            "the schema registry was built on another token interner than the store's; \
             build it on the store's `tokens` and open into that store with `open_in`",
        )),
        _ => Ok(()),
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
    package_limits: PackageLimits,
    variant_arcs: VariantArcLoading,
    formats: FileFormats,
    load_report: LoadReport,
    unavailable: Unavailable,
}
/// The assets the stage asked for that storage could not supply yet
/// ([`IoErrorKind::Unavailable`]), by anchor layer and authored asset path,
/// with the identifier storage gave each.
type Unavailable = BTreeMap<(LayerId, String), String>;
impl<B: core::fmt::Debug> core::fmt::Debug for StageDocument<B> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StageDocument")
            .field("storage", &self.storage)
            .field("catalog", &self.catalog)
            .field("import_policy", &self.import_policy)
            .field("usdc_options", &self.usdc_options)
            .field("variant_arcs", &self.variant_arcs)
            .field("formats", &self.formats)
            .field("load_report", &self.load_report)
            .field("unavailable", &self.unavailable)
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
    /// The registry must be built on `store`'s tokens, as for
    /// [`Self::open_in`]. The assets the new stage asks for, such as the
    /// session layer's sublayers by asset path, are loaded before it
    /// returns ([`Self::load_requested_assets`]), and the
    /// [`Self::load_report`] holds that evidence.
    pub fn create_in(
        storage: B,
        mut store: InMemoryStore,
        target: &str,
        options: StageOptions,
    ) -> Result<Self, IoError> {
        check_schemas(&store, &options)?;
        let identifier = storage.identify(target, None).map_err(|e| e.at(target))?;
        let format = identifier_format(&identifier, &FileFormats::default())?;
        if format == Format::Usdz {
            return Err(IoError::new(
                IoErrorKind::ReadOnly,
                "new packages require an explicit localization plan",
            ));
        }
        let mut catalog = Catalog {
            reserved: store.layer_ids().into_iter().collect(),
            ..Default::default()
        };
        let root = catalog.allocate_for(&identifier)?;
        let identifier: Arc<str> = Arc::from(identifier);
        catalog.ids.insert(Arc::clone(&identifier), root);
        catalog.sources.insert(
            root,
            Source {
                identifier,
                format,
                package: None,
            },
        );
        catalog.states.insert(root, State::Loaded);
        store.set_layer_identifier(root, Arc::clone(&catalog.sources[&root].identifier));
        store.insert_layer(Layer::new(root));
        let stage = LiveStage::compose(&mut store, root, options);
        let mut document = Self {
            store,
            stage,
            storage,
            catalog,
            import_policy: ImportPolicy::Strict,
            usdc_options: UsdcReadOptions::default(),
            package_limits: PackageLimits::default(),
            variant_arcs: VariantArcLoading::default(),
            formats: FileFormats::default(),
            load_report: LoadReport::default(),
            unavailable: Unavailable::new(),
        };
        // A host-owned session layer's sublayers by asset path load with
        // the new stage, as `UsdStage::CreateNew` opens them.
        document.load_report = document.load_requested_assets()?;
        Ok(document)
    }
    /// Opens a stage with strict import into a new store.
    ///
    /// A schema registry in `options` cannot belong to the new store, so
    /// opening with one fails ([`IoErrorKind::ForeignSchemaRegistry`]); use
    /// [`Self::open_in`] with the store the registry was built on.
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
    ///
    /// # Errors
    ///
    /// [`IoErrorKind::ForeignSchemaRegistry`] when `options.schemas` was not
    /// built on `store`'s tokens; otherwise the errors of reading the root
    /// and its dependencies.
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
    /// The registry must be built on `store`'s tokens
    /// ([`layerstack::SchemaRegistry::belongs_to`]), or opening fails with
    /// [`IoErrorKind::ForeignSchemaRegistry`] before anything is read. Failed
    /// structural imports publish no layers; retained payloads are checked on demand.
    ///
    /// The dynamic sides of [`LoadOptions::formats`] ([`FileFormat::dynamic`])
    /// join `options.dynamic_formats`; one claiming an extension a format
    /// there claims is rejected ([`IoErrorKind::Rejected`]).
    ///
    /// With `options.threads` above one, the layers are read on a pool of
    /// that many threads, on `options.executor` when there is one
    /// ([`StageOptions::thread_budget`]), and a USDC
    /// layer on the threads the pool leaves idle while it is read: reading
    /// runs at most `options.threads` threads at once, as composing after it
    /// does.
    pub fn open_in_with(
        mut storage: B,
        mut store: InMemoryStore,
        asset: &str,
        mut options: StageOptions,
        loading: LoadOptions,
    ) -> Result<Self, IoError> {
        check_schemas(&store, &options)?;
        for format in loading.formats.dynamic_formats() {
            options
                .dynamic_formats
                .add(Arc::clone(format))
                .map_err(|error| IoError::new(IoErrorKind::Rejected, error.to_string()))?;
        }
        let import_policy = loading.import_policy;
        let mut catalog = Catalog {
            reserved: store.layer_ids().into_iter().collect(),
            ..Default::default()
        };
        let mut reader = Reader::new(
            &mut storage,
            &mut catalog,
            import_policy,
            loading.usdc,
            loading.package,
            options.schemas.clone(),
        );
        reader.variant_arcs = loading.variant_arcs;
        reader.formats = loading.formats.clone();
        reader.threads = options.thread_budget();
        #[cfg(feature = "std")]
        let threaded = reader.load_on_threads(asset, store.tokens(), store.paths());
        #[cfg(not(feature = "std"))]
        let threaded: Option<Result<ResolvedAsset, IoError>> = None;
        let resolved = match threaded {
            Some(resolved) => resolved?,
            None => reader.load(asset, None, store.tokens(), store.paths())?,
        };
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
        let mut document = Self {
            store,
            stage,
            storage,
            catalog,
            import_policy,
            usdc_options: loading.usdc,
            package_limits: loading.package,
            variant_arcs: loading.variant_arcs,
            formats: loading.formats,
            load_report: report,
            unavailable: Unavailable::new(),
        };
        let requested = document.load_requested_assets()?;
        document.load_report.merge(requested);
        Ok(document)
    }
    /// Loads the assets the stage asks for
    /// ([`layerstack::Stage::asset_requests`]), each anchored to the layer
    /// authoring the arc or sublayer, and composes again, until none is
    /// left: a newly loaded layer may author deferred arcs of its own.
    ///
    /// Opening a stage does this before it returns, so a host-owned session
    /// layer's sublayers by asset path
    /// ([`layerstack::SublayerEntry::unresolved`]) load with the stage. After
    /// an edit or a reload selects a variant whose arcs import deferred
    /// ([`VariantArcLoading::Selected`]), or adds a sublayer by asset path
    /// to any layer the stage reads, those arcs and sublayers contribute
    /// nothing until this loads their assets; nothing is read implicitly,
    /// and [`Self::apply`] calls this. A relative sublayer path of a layer
    /// with no identifier, such as an anonymous session layer, is
    /// identified with no anchor ([`Storage::identify`]). Each round
    /// composes only the prims whose arcs name the loaded assets. An asset
    /// that fails to load is recorded as missing, so composition reports it
    /// ([`layerstack::CompositionError::UnresolvedAsset`] or
    /// [`layerstack::CompositionError::UnresolvedSublayer`]), and kept in the
    /// report's [`LoadReport::asset_failures`]. Strict import returns the
    /// error instead, keeping the layers earlier rounds loaded, unless the
    /// asset was not found ([`AssetFailure::is_missing`]).
    ///
    /// An asset storage cannot supply yet ([`IoErrorKind::Unavailable`]) is
    /// neither loaded nor recorded as missing: the stage keeps asking for
    /// it, [`Self::unavailable_assets`] lists it, and a later call reads it
    /// again, so a host that supplies sources as they arrive calls this
    /// after each batch. Only the assets the stage asks for are read, never
    /// a resident layer, so this works whatever layers are dirty, where
    /// [`Self::reload`] would refuse ([`IoErrorKind::DirtyReload`]).
    ///
    /// Returns the layers loaded, with their diagnostics and failures.
    ///
    /// Spec: AOUSD Core §10.3.1 (sublayers), §10.3.2.1 (references),
    /// §10.3.2.2 (payloads), §10.3.2.5 (only the selected variant
    /// contributes). OpenUSD opens a sublayer authored on any layer of the
    /// stage's layer stacks, the session layer's included, when the stage
    /// recomposes (`PcpLayerStack::_BuildLayerStack`).
    pub fn load_requested_assets(&mut self) -> Result<LoadReport, IoError> {
        let mut report = LoadReport::default();
        let mut attempted = BTreeSet::new();
        loop {
            let requests: Vec<(LayerId, String)> = self
                .stage
                .stage()
                .asset_requests()
                .iter()
                .map(|request| (request.anchor, request.asset_path.clone()))
                .filter(|key| attempted.insert(key.clone()))
                .collect();
            if requests.is_empty() {
                return Ok(report);
            }
            let mut catalog = self.catalog.clone();
            catalog.reserved.extend(self.store.layer_ids());
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
            for (anchor, asset) in requests {
                let result = reader.load(
                    &asset,
                    Some(anchor),
                    self.store.tokens(),
                    self.store.paths(),
                );
                match result {
                    Ok(resolved) => {
                        self.unavailable.remove(&(anchor, asset.clone()));
                        reader.bind(anchor, asset.as_str(), resolved.layer_id);
                        if let Some(layer) = resolved.layer {
                            reader.pending.push(layer);
                        }
                    }
                    // Not supplied yet: left unbound, so the stage keeps
                    // asking for it and a later call reads it again.
                    Err(error) if error.kind == IoErrorKind::Unavailable => {
                        let identifier = error.identifier.unwrap_or_else(|| asset.clone());
                        self.unavailable.insert((anchor, asset), identifier);
                    }
                    Err(error) => {
                        self.unavailable.remove(&(anchor, asset.clone()));
                        let failure = AssetFailure {
                            asset_path: asset.clone(),
                            anchor: Some(anchor),
                            error,
                        };
                        // A missing dependency is a composition error, not
                        // a rejected read (AOUSD Core §10.3.2.1, §10.6).
                        if self.import_policy == ImportPolicy::Strict && !failure.is_missing() {
                            return Err(failure.error);
                        }
                        reader.bind(anchor, failure.asset_path.as_str(), LayerId::UNRESOLVED);
                        reader.failures.push(failure);
                    }
                }
            }
            reader.resolve_expressions(
                &mut self.store,
                layerstack::LayerStackIdentifier {
                    root: self.stage.stage().root_layer().expect("document root"),
                    session: self.stage.stage().session_layer(),
                },
            )?;
            report.merge(reader.commit(&mut self.store));
            self.catalog = catalog;
            self.synchronize();
        }
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
        catalog.reserved.extend(self.store.layer_ids());
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
        let resolved = reader.load(asset, anchor, self.store.tokens(), self.store.paths())?;
        let id = resolved.layer_id;
        if let Some(anchor) = anchor {
            reader.bind(anchor, asset, id);
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
            package: self.package_limits,
            variant_arcs: self.variant_arcs,
            formats: self.formats.clone(),
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
    /// Directory/verification work and retained backing bytes for the package
    /// containing `layer`. All its members share this snapshot and its indexes.
    /// This excludes decoded arrays, structural tables and allocator overhead.
    pub fn package_stats(&self, layer: LayerId) -> Option<PackageStats> {
        self.catalog.package_stats(layer)
    }
    /// Cumulative transport/index work and current package owner footprint.
    pub fn package_cache_stats(&self) -> PackageCacheStats {
        self.catalog.cache_stats()
    }
    /// The member path of a package's root layer (`root.usdc`) when
    /// `layer` is a package read as a layer, whose identifier is the
    /// package's (`scene.usdz`): what a new package of it names its root.
    /// `None` for any other layer, a package member included.
    ///
    /// Spec: AOUSD Core §16 (the first layer of a package is its root).
    #[must_use]
    pub fn package_root_member(&self, layer: LayerId) -> Option<&str> {
        self.catalog.package_roots.get(&layer).map(String::as_str)
    }
    /// Authored layers and the document's shared interners.
    pub fn store(&self) -> &InMemoryStore {
        &self.store
    }
    /// Mutable authored store. After direct edits call `synchronize` on each
    /// client, and [`Self::load_requested_assets`] for the assets they ask
    /// for, such as a sublayer added by asset path.
    pub fn store_mut(&mut self) -> &mut InMemoryStore {
        &mut self.store
    }
    /// Retained primary stage.
    pub fn stage(&self) -> &LiveStage {
        &self.stage
    }
    /// The composed stage bound to the document's store: the canonical way
    /// to read it ([`layerstack::StageView`]).
    ///
    /// OpenUSD: the `UsdStage` this document opened.
    pub fn view(&self) -> layerstack::StageView<'_> {
        self.stage.view(&self.store)
    }
    /// Disjoint mutable access for atomic stage edits and explicit controls.
    ///
    /// To author a transaction, prefer [`Self::apply`]: an edit applied
    /// here with [`LiveStage::apply`] may ask for assets
    /// ([`layerstack::Applied::asset_requests`]) that contribute nothing
    /// until [`Self::load_requested_assets`] loads them.
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
    /// Evidence from the initial import: what opening or creating the
    /// document read, with the failures it met then. It is not updated
    /// by later loads; [`Self::asset_status`] reports the assets the stage
    /// lacks now.
    pub fn load_report(&self) -> &LoadReport {
        &self.load_report
    }
    /// The assets the stage asks for that storage cannot supply yet
    /// ([`IoErrorKind::Unavailable`]): the list a host fetches, each once
    /// per anchor layer and authored path, in the order the stage lists
    /// its requests ([`layerstack::Stage::asset_requests`]: by anchor
    /// layer, then asset path).
    ///
    /// A host supplies their bytes to its [`Storage`]
    /// ([`Self::storage_mut`]) under [`UnavailableAsset::identifier`], or
    /// makes it report [`IoErrorKind::NotFound`] for one it confirmed
    /// absent, then calls [`Self::load_requested_assets`], which reads them
    /// without reading resident layers again, dirty ones included. A
    /// supplied layer may ask for more, so the host repeats until this is
    /// empty. It lists what the latest [`Self::load_requested_assets`]
    /// (which opening and [`Self::apply`] call) found unavailable and the
    /// stage still asks for, so an edit that drops the arc drops it here.
    ///
    /// Meanwhile the stage composes without them: a prim whose reference
    /// or payload names one is on the stage without that arc's opinions,
    /// and a layer stack without that sublayer. Each entry lists those
    /// prims ([`UnavailableAsset::prims`]), so a host can tell which are
    /// incomplete; composition reports no error for them, and does not
    /// withhold them as it withholds a prim reading a pending layer
    /// ([`layerstack::Stage::pending_prims`]).
    ///
    /// OpenUSD has no equivalent: its resolver reads synchronously.
    #[must_use]
    pub fn unavailable_assets(&self) -> Vec<UnavailableAsset> {
        let mut assets: Vec<UnavailableAsset> = Vec::new();
        for request in self.stage.stage().asset_requests() {
            let key = (request.anchor, request.asset_path.clone());
            let Some(identifier) = self.unavailable.get(&key) else {
                continue;
            };
            if let Some(last) = assets.last_mut().filter(|last| {
                last.anchor == request.anchor && last.asset_path == request.asset_path
            }) {
                if !last.prims.contains(&request.prim) {
                    last.prims.push(request.prim);
                }
                continue;
            }
            assets.push(UnavailableAsset {
                identifier: identifier.clone(),
                anchor: request.anchor,
                asset_path: request.asset_path.clone(),
                used_by: request.used_by,
                prims: alloc::vec![request.prim],
            });
        }
        assets
    }
    /// The assets the composed stage lacks now: those storage cannot
    /// supply yet ([`Self::unavailable_assets`]) and those unresolved,
    /// missing or failed to read, which composition reports
    /// ([`layerstack::CompositionError::UnresolvedAsset`] and
    /// [`layerstack::CompositionError::UnresolvedSublayer`]). Unlike
    /// [`Self::load_report`], it follows every load and edit: an asset
    /// that loads, or whose arc an edit removes, leaves it.
    ///
    /// Spec: AOUSD Core §10.6 (composition errors). OpenUSD:
    /// `UsdStage::GetCompositionErrors`, `PcpErrorInvalidAssetPath`,
    /// `PcpErrorInvalidSublayerPath`.
    #[must_use]
    pub fn asset_status(&self) -> AssetStatus {
        let mut unresolved: Vec<UnresolvedAsset> = Vec::new();
        for error in self.stage.stage().composition_errors() {
            let (anchor, asset_path, used_by) = match error {
                layerstack::CompositionError::UnresolvedAsset(error) => (
                    error.introducing_layer,
                    &error.asset_path,
                    AssetUse::of_arc(error.arc).unwrap_or(AssetUse::Reference),
                ),
                layerstack::CompositionError::UnresolvedSublayer(error) => {
                    (error.layer, &error.asset_path, AssetUse::Sublayer)
                }
                _ => continue,
            };
            if unresolved.iter().any(|asset| {
                asset.anchor == anchor
                    && asset.asset_path == *asset_path
                    && asset.used_by == used_by
            }) {
                continue;
            }
            let failure = self.catalog.failures.get(&(anchor, asset_path.clone()));
            unresolved.push(UnresolvedAsset {
                anchor,
                asset_path: asset_path.clone(),
                used_by,
                identifier: failure.and_then(|failure| failure.identifier.clone()),
                reason: failure.map(|failure| failure.kind),
            });
        }
        AssetStatus {
            unavailable: self.unavailable_assets(),
            unresolved,
        }
    }
    /// Identifier of a loaded layer. Package members use `package[member]`.
    pub fn identifier(&self, layer: LayerId) -> Option<&str> {
        self.catalog.sources.get(&layer).map(|s| &*s.identifier)
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
        self.catalog.reserved.extend(self.store.layer_ids());
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
    /// Serialization may decode retained numeric values. Saving keeps their source
    /// ownership and caches; it does not rebind them to the written bytes, change
    /// authored generations or emit a scene edit. Existing snapshots remain valid.
    pub fn save_layers(&mut self, layers: &[LayerId]) -> SaveReport {
        let mut report = SaveReport::default();
        for id in layers.iter().copied().collect::<BTreeSet<_>>() {
            if !self.store.contains_layer(id) {
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
            } else if source.identifier.contains(FORMAT_ARGS_DELIMITER) {
                Err(IoError::new(
                    IoErrorKind::ReadOnly,
                    "a layer read with format arguments is built from them; export a copy instead",
                ))
            } else {
                self.write_layer(id, source.format, &source.identifier)
            };
            match result {
                Ok(()) => {
                    self.catalog
                        .saved
                        .insert(id, self.store.layer(id).unwrap().generation());
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
        let identifier = self
            .storage
            .identify(target, None)
            .map_err(|e| e.at(target))?;
        let format = identifier_format(&identifier, &self.formats)?;
        self.write_layer(layer, format, &identifier)
    }
    /// The bytes [`Self::export_layer`] would write for `layer` to
    /// `target`, returned instead of written: USDA (`.usda`) or USDC
    /// (`.usd`/`.usdc`), by `target`'s extension, which is not resolved
    /// and names nothing in storage. `layer` need not be in the store: a
    /// flattened layer ([`layerstack::Stage::flatten`]) encodes with the
    /// store's interners, as any layer whose paths and tokens they hold.
    /// Content a format cannot hold is refused, as a save refuses it.
    ///
    /// # Errors
    ///
    /// [`IoErrorKind::Unsupported`] for content the format cannot hold, a
    /// `.usdz` target (a package needs [`Self::prepare_package`]) or an
    /// unknown extension; [`IoErrorKind::Decode`] for a retained array
    /// that fails to decode.
    pub fn encode_layer(&self, layer: &Layer, target: &str) -> Result<Vec<u8>, IoError> {
        let format = identifier_format(target, &self.formats).map_err(|e| e.at(target))?;
        let mut bytes = Vec::new();
        self.encode(layer, format, &mut bytes)?;
        Ok(bytes)
    }
    /// Writes an explicit, already localized package plan. Asset selection and
    /// package member naming belong to the caller and existing localization API.
    pub fn export_usdz(
        &mut self,
        target: &str,
        plan: &layerstack_usdz::localize::LocalizationPlan,
    ) -> Result<(), IoError> {
        let identifier = self
            .storage
            .identify(target, None)
            .map_err(|e| e.at(target))?;
        let bytes = plan
            .write_usdz(self.store.tokens(), self.store.paths())
            .map_err(localization_error)?;
        self.storage
            .write(&identifier, &bytes)
            .map_err(|e| e.at(&identifier))
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
    /// Writes layer `id` to `identifier` in `format`, refusing a layer the
    /// format cannot hold before the storage is asked to write. USDA text
    /// streams to the storage ([`Storage::write_streamed`]); the other
    /// formats are encoded whole and written at once.
    fn write_layer(
        &mut self,
        id: LayerId,
        format: Format,
        identifier: &str,
    ) -> Result<(), IoError> {
        let layer = self
            .store
            .layer(id)
            .ok_or_else(|| IoError::new(IoErrorKind::MissingLayer, "selected layer is absent"))?;
        let (tokens, paths) = (&self.store.tokens(), &self.store.paths());
        if format == Format::Usda {
            let document =
                layerstack_usda::save::usda_document(layer, tokens, paths).map_err(save_error)?;
            return self
                .storage
                .write_streamed(identifier, &mut |sink| write_usda(&document, sink))
                .map_err(|e| e.at(identifier));
        }
        let mut bytes = Vec::new();
        self.encode(layer, format, &mut bytes)?;
        self.storage
            .write(identifier, &bytes)
            .map_err(|e| e.at(identifier))
    }
    /// Encodes `layer` in `format` into `sink`, with the store's interners.
    fn encode(
        &self,
        layer: &Layer,
        format: Format,
        sink: &mut dyn ByteSink,
    ) -> Result<(), IoError> {
        let (tokens, paths) = (&self.store.tokens(), &self.store.paths());
        let bytes = match format {
            Format::Usda => {
                let document = layerstack_usda::save::usda_document(layer, tokens, paths)
                    .map_err(save_error)?;
                return write_usda(&document, sink);
            }
            Format::Usdc => layerstack_usdc::writer::save_layer(layer, tokens, paths)
                .map_err(|error| caused(IoCause::UsdcWrite(error)))?,
            Format::Usdz => {
                return Err(IoError::new(
                    IoErrorKind::Unsupported,
                    "package export requires an explicit localization plan",
                ));
            }
            Format::Custom(index) => self.formats.get(index).write(layer, tokens, paths)?,
        };
        sink.write_bytes(&bytes)
    }
    fn session_layers(&self) -> BTreeSet<LayerId> {
        let mut layers = BTreeSet::new();
        let mut queue: Vec<_> = self.stage.stage().session_layer().into_iter().collect();
        while let Some(id) = queue.pop() {
            if !layers.insert(id) {
                continue;
            }
            if let Some(layer) = self.store.layer(id) {
                for sublayer in layer.sublayers() {
                    queue.push(sublayer.layer);
                }
            }
        }
        layers
    }
}

/// Storage identities, keyed by anchor identifier and authored asset.
type Identities = BTreeMap<(Option<String>, String), Result<String, IoError>>;

/// The layers the asset paths an operation resolved bind to, by the layer
/// anchoring each path, then by path: the shape the store keeps them in
/// ([`InMemoryStore::insert_asset_layers`]), so committing them moves the
/// paths instead of copying them.
type Bindings = HashMap<LayerId, HashMap<Arc<str>, LayerId>>;

struct Reader<'a, B> {
    schemas: Option<Arc<layerstack::SchemaRegistry>>,
    storage: &'a mut B,
    catalog: &'a mut Catalog,
    policy: ImportPolicy,
    usdc_options: UsdcReadOptions,
    package_limits: PackageLimits,
    pending: Vec<Layer>,
    diagnostics: Vec<MemberDiagnostic>,
    bindings: Bindings,
    /// Storage identities already computed by this operation, keyed by anchor
    /// identifier and authored asset. `Storage::identify` must be stable within
    /// a document, so a scene that references one asset from many sites asks
    /// its transport once. Dropped with the operation, so later loads observe
    /// newly created or removed files.
    identities: Identities,
    failures: Vec<AssetFailure>,
    reachable_only: bool,
    /// Which arcs inside variant branches the layers read resolve
    /// ([`AssetResolver::defers_variant_arcs`]).
    variant_arcs: VariantArcLoading,
    /// The registered formats ([`LoadOptions::formats`]).
    formats: FileFormats,
    /// The threads the operation reads on, and walks the layers it read
    /// on afterwards ([`StageOptions::threads`]); one unless an open sets
    /// them.
    threads: Threads,
}
impl<'a, B: Storage> Reader<'a, B> {
    fn new(
        storage: &'a mut B,
        catalog: &'a mut Catalog,
        policy: ImportPolicy,
        usdc_options: UsdcReadOptions,
        package_limits: PackageLimits,
        schemas: Option<Arc<layerstack::SchemaRegistry>>,
    ) -> Self {
        Self {
            schemas,
            storage,
            catalog,
            policy,
            usdc_options,
            package_limits,
            pending: Vec::new(),
            diagnostics: Vec::new(),
            bindings: Bindings::new(),
            identities: BTreeMap::new(),
            failures: Vec::new(),
            reachable_only: false,
            variant_arcs: VariantArcLoading::All,
            formats: FileFormats::default(),
            threads: Threads::new(1),
        }
    }
    fn resolve_expressions(
        &mut self,
        store: &mut InMemoryStore,
        stack: layerstack::LayerStackIdentifier,
    ) -> Result<(), IoError> {
        // Only an asset path authored as an expression needs evaluating;
        // without one the walk over every layer stack finds nothing.
        // The layers just read are walked on the operation's threads.
        if !any_layer(&self.threads, &self.pending, &authors_expressions)
            && !store.layers().any(authors_expressions)
        {
            return Ok(());
        }
        let mut attempted = BTreeSet::new();
        loop {
            let overlay = Overlay {
                store,
                layers: &self.pending,
                index: self
                    .pending
                    .iter()
                    .enumerate()
                    .map(|(at, layer)| (layer.id(), at))
                    .collect(),
                bindings: &self.bindings,
            };
            let requests = layerstack::expression_asset_paths_for_stack(&overlay, stack);
            let mut progress = false;
            for request in requests {
                let key = (request.anchor, request.asset_path.clone());
                if !attempted.insert(key) {
                    continue;
                }
                progress = true;
                match self.load(
                    &request.asset_path,
                    Some(request.anchor),
                    store.tokens(),
                    store.paths(),
                ) {
                    Ok(resolved) => {
                        self.bind(
                            request.anchor,
                            request.asset_path.as_str(),
                            resolved.layer_id,
                        );
                        if let Some(layer) = resolved.layer {
                            self.pending.push(layer);
                        }
                    }
                    // Not supplied yet: left unbound, for composition to ask
                    // for again.
                    Err(error) if error.kind == IoErrorKind::Unavailable => {}
                    Err(error) => {
                        self.bind(
                            request.anchor,
                            request.asset_path.as_str(),
                            LayerId::UNRESOLVED,
                        );
                        // An evaluated path naming no asset is a missing
                        // dependency under either policy (AOUSD Core
                        // §10.3.2.1, §10.6); composition reports it.
                        if error.kind == IoErrorKind::NotFound {
                            self.failures.push(AssetFailure {
                                asset_path: request.asset_path,
                                anchor: Some(request.anchor),
                                error,
                            });
                            continue;
                        }
                        if self.policy == ImportPolicy::Strict {
                            return Err(error);
                        }
                        self.diagnostics.push(MemberDiagnostic::new(
                            Arc::from(request.asset_path),
                            request.anchor,
                            ImportDiagnostic::UsdaEmit(
                                layerstack_usda::diagnostic::Diagnostic::error(
                                    layerstack_usda::Span::new(0, 0),
                                    error.message,
                                ),
                            ),
                        ));
                    }
                }
            }
            if !progress {
                return Ok(());
            }
        }
    }
    /// Records that `asset`, anchored to the layer `anchor`, resolved to
    /// `layer` ([`LayerId::UNRESOLVED`] when it did not).
    fn bind(&mut self, anchor: LayerId, asset: impl Into<Arc<str>>, layer: LayerId) {
        self.bindings
            .entry(anchor)
            .or_default()
            .insert(asset.into(), layer);
    }
    fn commit(mut self, store: &mut InMemoryStore) -> LoadReport {
        let clean_anchors: BTreeSet<_> = self
            .bindings
            .keys()
            .copied()
            .filter(|id| {
                store
                    .layer(*id)
                    .is_some_and(|l| self.catalog.saved.get(id) == Some(&l.generation()))
            })
            .collect();
        // The first layer read for each id is committed; the report lists
        // them in id order.
        let mut added = HashSet::with_capacity(self.pending.len());
        let mut layers = Vec::with_capacity(self.pending.len());
        store.reserve_layers(self.pending.len());
        for mut layer in self.pending.drain(..) {
            let id = layer.id();
            if added.insert(id) {
                layers.push(id);
                // A layer new to the store arrives with the bindings of its
                // arcs: bound before the store holds it, they record no
                // change, which would look through its specs for the
                // deferred arcs naming each asset, as no stage has seen it.
                if !store.contains_layer(id)
                    && let Some(assets) = self.bindings.remove(&id)
                {
                    store.insert_asset_layers(id, assets);
                }
                // A reader records a change for each spec it inserts; no
                // stage has seen the layer before them.
                layer.forget_change_history();
                if let Some(source) = self.catalog.sources.get(&id) {
                    store.set_layer_identifier(id, Arc::clone(&source.identifier));
                }
                store.insert_layer(layer);
            }
        }
        // One change of each anchor for all its bindings: recording one
        // looks through the anchor's specs for the deferred arcs naming
        // the asset, so binding each alone revisits them per asset.
        for (anchor, assets) in core::mem::take(&mut self.bindings) {
            store.insert_asset_layers(anchor, assets);
        }
        sort::sort_unstable(&mut layers);
        for id in clean_anchors {
            self.catalog
                .saved
                .insert(id, store.layer(id).unwrap().generation());
        }
        for &id in &layers {
            self.catalog
                .saved
                .insert(id, store.layer(id).unwrap().generation());
        }
        for failure in &self.failures {
            if let Some(anchor) = failure.anchor {
                self.catalog.failures.insert(
                    (anchor, failure.asset_path.clone()),
                    ReadFailure {
                        identifier: failure.error.identifier.clone(),
                        kind: failure.error.kind,
                    },
                );
            }
        }
        LoadReport {
            layers,
            diagnostics: self.diagnostics,
            asset_failures: self.failures,
        }
    }
    fn read_layer(
        &mut self,
        data: EncodedBytes,
        identifier: &str,
        id: LayerId,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<Layer, IoError> {
        let format = source_format(&self.formats, identifier, &data);
        self.catalog
            .sources
            .get_mut(&id)
            .expect("reserved source")
            .format = format;
        let first_diagnostic = self.diagnostics.len();
        let first_failure = self.failures.len();
        let layer = match format {
            Format::Usda | Format::Usdc | Format::Custom(_) => {
                let schemas = self.schemas.clone();
                let usdc_options = self.usdc_options;
                let formats = self.formats.clone();
                let read = read_loose(
                    format,
                    &data,
                    identifier,
                    id,
                    tokens,
                    paths,
                    self,
                    schemas.as_deref(),
                    usdc_options,
                    &formats,
                    &Threads::new(1),
                );
                self.pending.extend(read.resolved_layers);
                self.diagnostics.extend(read.diagnostics);
                #[cfg(feature = "std")]
                if let Some(values) = read.retained {
                    self.catalog.retained_values.insert(id, values);
                }
                read.layer?
            }
            Format::Usdz => {
                if self.reachable_only {
                    // A dependent archive may choose a different first member.
                    // Its old root alias must not claim the outer package ID
                    // when that former root becomes an ordinary member.
                    self.catalog
                        .ids
                        .retain(|alias, member_id| *member_id != id || &**alias == identifier);
                }
                let members: Vec<String> = self
                    .catalog
                    .sources
                    .iter()
                    .filter(|(member_id, s)| {
                        !self.reachable_only && **member_id != id && s.package == Some(id)
                    })
                    .filter_map(|(_, s)| package_member_path(&s.identifier))
                    .collect();
                let selected: Vec<&str> = members.iter().map(String::as_str).collect();
                let usdc_options = self.usdc_options;
                let archive = Arc::new(
                    PackageSnapshot::new(data, self.package_limits)
                        .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?,
                );
                let selection = package_member_path(identifier);
                self.catalog.cache_work.opens += 1;
                self.catalog.cache_work.index_parses += archive.stats().index_parses;
                let result = layerstack_usdz::read_usdz_snapshot(
                    &archive,
                    selection.as_deref(),
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
                self.catalog.package_bytes.insert(id, archive);
                self.catalog
                    .package_roots
                    .insert(id, result.member_paths[&id].to_string());
                for (&member_id, member) in &result.member_paths {
                    let outer = identifier
                        .split_once('[')
                        .map_or(identifier, |(outer, _)| outer);
                    let member_identifier: Arc<str> =
                        Arc::from(alloc::format!("{outer}[{member}]"));
                    self.catalog.sources.insert(
                        member_id,
                        Source {
                            identifier: Arc::clone(&member_identifier),
                            format: identifier_format(member, &FileFormats::default())?,
                            package: Some(id),
                        },
                    );
                    if member_id != id || !self.catalog.ids.contains_key(&*member_identifier) {
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
                let outer = identifier
                    .split_once('[')
                    .map_or(identifier, |(outer, _)| outer);
                let alias = alloc::format!("{outer}[{root_member}]");
                if self
                    .catalog
                    .ids
                    .get(alias.as_str())
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
        // A dependency that is not found leaves its arc to composition to
        // report (AOUSD Core §10.3.1, §10.3.2.1, §10.6); strict import
        // rejects only content that is malformed or unrepresented.
        if self.policy == ImportPolicy::Strict
            && let Some(failure) = strict_failure(&self.failures[first_failure..])
        {
            return Err(failure.error.clone());
        }
        if self.policy == ImportPolicy::Strict
            && self.diagnostics[first_diagnostic..]
                .iter()
                .any(strict_rejects)
        {
            return Err(strict_rejection(
                identifier,
                &self.diagnostics[first_diagnostic..],
            ));
        }
        Ok(layer)
    }
}
/// What reading one USDA or USDC layer found: the layer, or the error
/// rejecting it, with its diagnostics and the layers its arcs resolved to
/// (a rejected text layer keeps both, a rejected binary layer neither).
struct LooseRead {
    layer: Result<Layer, IoError>,
    resolved_layers: Vec<Layer>,
    diagnostics: Vec<MemberDiagnostic>,
    #[cfg(feature = "std")]
    retained: Option<layerstack_usdc::RetainedValues>,
}

/// Reads the USDA, USDC or registered-format layer `data`, `identifier`
/// read as `id`, resolving its arcs through `resolver`, a USDC layer on the
/// calling thread and the threads `threads` spares.
#[expect(
    clippy::too_many_arguments,
    reason = "one reader's inputs, passed through"
)]
fn read_loose(
    format: Format,
    data: &EncodedBytes,
    identifier: &str,
    id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    schemas: Option<&layerstack::SchemaRegistry>,
    usdc_options: UsdcReadOptions,
    formats: &FileFormats,
    threads: &Threads,
) -> LooseRead {
    let mut read = LooseRead {
        layer: Err(IoError::new(IoErrorKind::Rejected, "unread layer")),
        resolved_layers: Vec::new(),
        diagnostics: Vec::new(),
        #[cfg(feature = "std")]
        retained: None,
    };
    let member = |diagnostic| MemberDiagnostic::new(Arc::from(identifier), id, diagnostic);
    if let Format::Custom(index) = format {
        read.layer = read_custom(
            formats.get(index),
            data,
            identifier,
            id,
            tokens,
            paths,
            resolver,
            &mut read.resolved_layers,
        );
        return read;
    }
    if format == Format::Usda {
        // OpenUSD 26.11 refuses a `.usda` asset that begins with a byte
        // order mark before parsing it, naming the mark
        // (`SdfUsdaFileFormat::_ReadFromAsset`, `pxr/usd/sdf/usdaFileFormat.cpp`).
        // Spec: AOUSD Core §16.2 (USDA is UTF-8).
        if let Some(mark) = layerstack_usda::ByteOrderMark::find(data) {
            read.layer = Err(IoError::new(
                IoErrorKind::Rejected,
                alloc::format!("<{identifier}> is not a valid usda layer: {mark}"),
            ));
            return read;
        }
        let source = match core::str::from_utf8(data) {
            Ok(source) => source,
            Err(e) => {
                read.layer = Err(IoError::new(IoErrorKind::Rejected, e.to_string()));
                return read;
            }
        };
        let result = if let Some(registry) = schemas {
            layerstack_usda::read_usda_with_registry(source, id, tokens, paths, resolver, registry)
        } else {
            layerstack_usda::read_usda(source, id, tokens, paths, resolver)
        };
        read.resolved_layers = result.emitted.resolved_layers;
        read.diagnostics = result
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
            .map(member)
            .collect();
        read.layer = if result.emitted.rejected {
            // Named and explained as a strict import's rejection is: OpenUSD
            // refuses the layer whatever the policy, and why is in its
            // error diagnostics.
            let mut error = IoError::new(IoErrorKind::Rejected, "USDA reader rejected the layer")
                .at(identifier);
            error.diagnostics = read
                .diagnostics
                .iter()
                .filter(|d| d.diagnostic.is_error())
                .cloned()
                .collect();
            Err(error)
        } else {
            Ok(result.emitted.layer)
        };
        return read;
    }
    let mut budget = usdc_options.budget(data.len());
    let rejected =
        |e: layerstack_usdc::UsdcError| IoError::new(IoErrorKind::Rejected, e.to_string());
    #[cfg(feature = "std")]
    let result = match usdc_options.arrays {
        UsdcArrayLoading::Retained => layerstack_usdc::read_usdc_lazy_range_within(
            data.clone(),
            id,
            tokens,
            paths,
            resolver,
            budget,
            threads,
        )
        .map(|result| {
            read.retained = Some(result.values);
            result.assembled
        }),
        UsdcArrayLoading::Eager => layerstack_usdc::read_usdc_within(
            data,
            id,
            tokens,
            paths,
            resolver,
            &mut budget,
            threads,
        ),
    };
    #[cfg(not(feature = "std"))]
    let result =
        layerstack_usdc::read_usdc_within(data, id, tokens, paths, resolver, &mut budget, threads);
    match result {
        Ok(result) => {
            read.resolved_layers = result.resolved_layers;
            read.diagnostics = result
                .diagnostics
                .into_iter()
                .map(|d| member(ImportDiagnostic::UsdcAssemble(d)))
                .collect();
            read.layer = Ok(result.layer);
        }
        Err(e) => read.layer = Err(rejected(e)),
    }
    read
}

impl<B: Storage> Reader<'_, B> {
    fn identify(&mut self, asset: &str, anchor: Option<String>) -> Result<String, IoError> {
        let storage = &*self.storage;
        self.identities
            .entry((anchor, String::from(asset)))
            .or_insert_with_key(|(anchor, asset)| {
                storage
                    .identify(asset, anchor.as_deref())
                    .map_err(|e| e.at(asset))
            })
            .clone()
    }
    fn load(
        &mut self,
        asset: &str,
        anchor: Option<LayerId>,
        tokens: &TokenInterner,
        paths: &PathInterner,
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
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<ResolvedAsset, IoError> {
        if asset.starts_with('/') || asset.contains(':') {
            return self.load_external(asset, Some(package), tokens, paths);
        }
        let archive = self.catalog.package_bytes[&package].clone();
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
        self.load_package_member(&member, package, tokens, paths)
    }
    fn load_package_member(
        &mut self,
        member: &str,
        package: LayerId,
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<ResolvedAsset, IoError> {
        self.catalog.cache_work.index_hits += 1;
        let archive = self.catalog.package_bytes[&package].clone();
        let selected = archive
            .layer(member)
            .map_err(|e| IoError::new(IoErrorKind::Rejected, e.to_string()))?;
        let member = selected.path;
        let data = selected.bytes;
        let id = self
            .allocate_package_layer_id(package, &member)
            .ok_or_else(|| {
                IoError::new(
                    IoErrorKind::IdentityExhausted,
                    "package member identity unavailable",
                )
            })?;
        let identifier: Arc<str> = Arc::from(alloc::format!(
            "{}[{member}]",
            self.catalog.package_identifier(package)
        ));
        if self.catalog.states.contains_key(&id) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: identifier,
                layer: None,
            });
        }
        self.catalog.sources.insert(
            id,
            Source {
                identifier: Arc::clone(&identifier),
                format: identifier_format(&member, &FileFormats::default())?,
                package: Some(package),
            },
        );
        self.catalog.states.insert(id, State::Loading);
        match self.read_layer(data, &identifier, id, tokens, paths) {
            Ok(layer) => {
                self.catalog.states.insert(id, State::Loaded);
                Ok(ResolvedAsset {
                    layer_id: id,
                    resolved_path: identifier,
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
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<ResolvedAsset, IoError> {
        let anchor_identifier = anchor
            .and_then(|id| self.catalog.sources.get(&id))
            .map(|s| {
                let identifier = FormatArgs::strip(&s.identifier);
                String::from(
                    identifier
                        .split_once('[')
                        .map_or(identifier, |(outer, _)| outer),
                )
            });
        let (asset, args) = format::split_args(asset)?;
        let path = PackagePath::parse(asset, 32)
            .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
        let outer = self.identify(path.outer(), anchor_identifier)?;
        let identifier: Arc<str> = Arc::from(source_identifier(&self.formats, &path, &outer, args));
        if let Some((first, rest)) = path.members().split_first()
            && let Some(&package) = self.catalog.ids.get(outer.as_str())
            && self.catalog.states.get(&package) == Some(&State::Loaded)
            && self.catalog.package_bytes.contains_key(&package)
        {
            return self.load_package_member(
                &PackagePath::join(first, rest),
                package,
                tokens,
                paths,
            );
        }
        let id = if let Some(&id) = self.catalog.ids.get(&*identifier) {
            id
        } else {
            let id = self.catalog.allocate_for(&identifier)?;
            self.catalog.ids.insert(Arc::clone(&identifier), id);
            self.catalog.sources.insert(
                id,
                Source {
                    identifier: Arc::clone(&identifier),
                    format: Format::Usda,
                    package: None,
                },
            );
            id
        };
        if self.catalog.states.contains_key(&id) {
            return Ok(ResolvedAsset {
                layer_id: id,
                resolved_path: identifier,
                layer: None,
            });
        }
        let bytes = if self
            .formats
            .for_identifier(&identifier)
            .is_none_or(FileFormat::reads_source)
        {
            self.storage
                .read_encoded(&outer)
                .map_err(|e| e.at(&outer))?
        } else {
            EncodedBytes::from(Vec::new())
        };
        self.catalog.states.insert(id, State::Loading);
        match self.read_layer(bytes, &identifier, id, tokens, paths) {
            Ok(layer) => {
                self.catalog.states.insert(id, State::Loaded);
                Ok(ResolvedAsset {
                    layer_id: id,
                    resolved_path: identifier,
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
        tokens: &TokenInterner,
        paths: &PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let result = self.load(asset, anchor, tokens, paths);
        // An asset storage cannot supply yet is neither loaded nor missing:
        // it stays unbound, and its arc deferred, so composition asks for
        // it again.
        if let Err(error) = &result
            && error.kind == IoErrorKind::Unavailable
        {
            return Err(AssetResolveError::Unavailable);
        }
        if let Err(error) = &result {
            self.failures.push(AssetFailure {
                asset_path: asset.into(),
                anchor,
                error: error.clone(),
            });
        }
        if let Some(anchor) = anchor {
            let bound = result
                .as_ref()
                .map(|r| r.layer_id)
                .unwrap_or(LayerId::UNRESOLVED);
            self.bind(anchor, asset, bound);
        }
        result.map_err(asset_error)
    }
    fn resolved_path(&self, id: LayerId) -> Option<&str> {
        self.catalog.sources.get(&id).map(|s| &*s.identifier)
    }
    fn defers_variant_arcs(&self) -> bool {
        self.variant_arcs == VariantArcLoading::Selected
    }
    fn defers_payload(&self, asset_path: &str) -> bool {
        self.formats.is_dynamic(asset_path)
    }
    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.catalog.allocate().ok()
    }
    fn existing_package_layer_id(&self, package: LayerId, member: &str) -> Option<LayerId> {
        let identifier = alloc::format!("{}[{member}]", self.catalog.package_identifier(package));
        self.catalog.ids.get(identifier.as_str()).copied()
    }
    fn allocate_package_layer_id(&mut self, package: LayerId, member: &str) -> Option<LayerId> {
        let identifier = alloc::format!("{}[{member}]", self.catalog.package_identifier(package));
        if let Some(&id) = self.catalog.ids.get(identifier.as_str()) {
            return Some(id);
        }
        let id = self.catalog.allocate_for(&identifier).ok()?;
        self.catalog.ids.insert(Arc::from(identifier), id);
        Some(id)
    }
}
/// The error a strict import returns for `identifier`, whose reading
/// reported `diagnostics`: it keeps those that rejected it
/// ([`strict_rejects`]).
fn strict_rejection(identifier: &str, diagnostics: &[MemberDiagnostic]) -> IoError {
    let mut error = IoError::new(
        IoErrorKind::Rejected,
        "strict import rejected malformed or unrepresented content",
    )
    .at(identifier);
    error.diagnostics = diagnostics
        .iter()
        .filter(|d| strict_rejects(d))
        .cloned()
        .collect();
    error
}
fn localization_error(error: layerstack_usdz::localize::LocalizationError) -> IoError {
    caused(IoCause::Localization(error))
}
/// Streams `document`'s USDA text into `sink`; a failed write fails with
/// the sink's error.
fn write_usda(
    document: &layerstack_usda::writer::Document,
    sink: &mut dyn ByteSink,
) -> Result<(), IoError> {
    let mut text = TextSink { sink, error: None };
    document.write_usda_to(&mut text).map_err(|error| {
        text.error
            .take()
            .unwrap_or_else(|| save_error(layerstack_usda::save::SaveError::Document(error)))
    })
}
fn save_error(error: layerstack_usda::save::SaveError) -> IoError {
    caused(IoCause::Save(error))
}
/// A failed save or export, keeping `cause` and taking its kind from the
/// save error inside it: [`IoErrorKind::Decode`] for a retained array
/// that could not decode, [`IoErrorKind::Rejected`] for a layer that
/// cannot be written as it stands, [`IoErrorKind::Unsupported`] for the
/// rest.
fn caused(cause: IoCause) -> IoError {
    use layerstack_usda::save::SaveError;
    let kind = match cause.save_error() {
        Some(SaveError::ArrayRead { .. }) => IoErrorKind::Decode,
        Some(SaveError::Invalid { .. }) => IoErrorKind::Rejected,
        _ => IoErrorKind::Unsupported,
    };
    let message = cause.as_error().to_string();
    IoError {
        cause: Some(Arc::new(cause)),
        ..IoError::new(kind, message)
    }
}
fn asset_error(error: IoError) -> AssetResolveError {
    match error.kind {
        IoErrorKind::NotFound => AssetResolveError::NotFound,
        IoErrorKind::Unavailable => AssetResolveError::Unavailable,
        _ => AssetResolveError::LoadError(Arc::from(error.message)),
    }
}
fn identifier_format(identifier: &str, formats: &FileFormats) -> Result<Format, IoError> {
    if let Some(index) = formats.index_for(identifier) {
        return Ok(Format::Custom(index));
    }
    let path = PackagePath::parse(identifier, 32)
        .map_err(|reason| IoError::new(IoErrorKind::Rejected, reason))?;
    match path.leaf().rsplit('.').next() {
        Some("usda") => Ok(Format::Usda),
        Some("usd" | "usdc") => Ok(Format::Usdc),
        Some("usdz") => Ok(Format::Usdz),
        _ => Err(IoError::new(
            IoErrorKind::Unsupported,
            "target requires .usda, .usd, .usdc or .usdz extension, or a registered format's",
        )),
    }
}

/// The format of the source `identifier` with the bytes `data`: a
/// registered format by extension, else a built-in one by its bytes.
fn source_format(formats: &FileFormats, identifier: &str, data: &[u8]) -> Format {
    if let Some(index) = formats.index_for(identifier) {
        Format::Custom(index)
    } else if data.starts_with(b"PXR-USDC") {
        Format::Usdc
    } else if data.starts_with(b"PK\x03\x04") {
        Format::Usdz
    } else {
        Format::Usda
    }
}

/// The canonical identifier of the source named by `asset` once its path
/// without arguments identifies as `outer`: that identity, followed by the
/// arguments naming a distinct layer ([`format::canonical_args`]).
fn source_identifier(
    formats: &FileFormats,
    path: &PackagePath,
    outer: &str,
    args: FormatArgs,
) -> String {
    let base = path.with_outer(outer);
    let format = formats.for_identifier(&base);
    format::canonical_args(args, format).identifier(&base)
}

/// Builds the layer `identifier` names with the registered `format`,
/// resolving the sublayers it authors by asset path through `resolver`.
#[expect(
    clippy::too_many_arguments,
    reason = "one reader's inputs, passed through"
)]
fn read_custom(
    format: &dyn FileFormat,
    data: &EncodedBytes,
    identifier: &str,
    id: LayerId,
    tokens: &TokenInterner,
    paths: &PathInterner,
    resolver: &mut dyn AssetResolver,
    resolved_layers: &mut Vec<Layer>,
) -> Result<Layer, IoError> {
    let (_, args) = format::split_args(identifier)?;
    let source = FormatSource {
        identifier,
        layer: id,
        bytes: format.reads_source().then_some(&**data),
        args: &args,
        tokens,
        paths,
    };
    let mut layer = format.read(&source)?;
    if layer.id() != id {
        return Err(source.rejected(alloc::format!(
            "the {} format built layer {:?}, not {id:?}",
            format.name(),
            layer.id()
        )));
    }
    for entry in layer.sublayers_mut() {
        if entry.layer != LayerId::UNRESOLVED || entry.is_expression() {
            continue;
        }
        let Some(asset) = entry.asset.clone() else {
            continue;
        };
        if let Ok(resolved) = resolver.resolve(&asset, Some(id), tokens, paths) {
            entry.layer = resolved.layer_id;
            resolved_layers.extend(resolved.layer);
        }
    }
    Ok(layer)
}

fn package_member_path(identifier: &str) -> Option<String> {
    let path = PackagePath::parse(identifier, 32).ok()?;
    let (first, rest) = path.members().split_first()?;
    Some(PackagePath::join(first, rest))
}

#[cfg(test)]
mod tests;

/// Whether `layer` authors a sublayer, reference or payload whose asset path
/// is a variable expression ([`layerstack::variable_expression`]).
/// The layers each task of a walk over many layers takes ([`any_layer`]).
const LAYERS_PER_TASK: usize = 64;

/// Whether `test` holds for any of `layers`, testing them on the threads
/// `threads` spares in tasks of [`LAYERS_PER_TASK`] layers.
fn any_layer(threads: &Threads, layers: &[Layer], test: &(dyn Fn(&Layer) -> bool + Sync)) -> bool {
    if layers.len() <= LAYERS_PER_TASK || threads.spare() == 0 {
        return layers.iter().any(test);
    }
    let work = |chunk: &[Layer], _: &mut Vec<&[Layer]>| chunk.iter().any(test);
    let tasks = layers.chunks(LAYERS_PER_TASK).collect();
    layerstack::parallel::with_pool_in(threads, &work, |pool| pool.run(tasks))
        .into_iter()
        .any(|found| found)
}

fn authors_expressions(layer: &Layer) -> bool {
    let arcs = |spec_arcs: [&layerstack::ListOp<layerstack::Reference>; 2]| {
        spec_arcs
            .into_iter()
            .any(|op| op.items().any(layerstack::Reference::is_expression))
    };
    layer
        .sublayers()
        .iter()
        .any(layerstack::SublayerEntry::is_expression)
        || !layer.known_without_reference_arcs()
            && layer
                .prims()
                .values()
                .chain(layer.variant_prims().values().flatten())
                .any(|spec| {
                    arcs([&spec.references, &spec.payloads])
                        || spec
                            .variant_branches()
                            .any(|branch| arcs([&branch.spec.references, &branch.spec.payloads]))
                })
}
struct Overlay<'a> {
    store: &'a InMemoryStore,
    layers: &'a [Layer],
    /// Where each of `layers` is, by ID: the last one read for an ID wins.
    index: HashMap<LayerId, usize>,
    bindings: &'a Bindings,
}
impl LayerStore for Overlay<'_> {
    fn layer(&self, id: LayerId) -> Option<&Layer> {
        match self.index.get(&id) {
            Some(&at) => Some(&self.layers[at]),
            None => self.store.layer(id),
        }
    }
    fn layer_mut(&mut self, _: LayerId) -> Option<&mut Layer> {
        None
    }
    fn tokens(&self) -> &TokenInterner {
        self.store.tokens()
    }
    fn paths(&self) -> &PathInterner {
        self.store.paths()
    }
    fn layer_identifier(&self, id: LayerId) -> Option<&str> {
        self.store.layer_identifier(id)
    }
    fn asset_layer(&self, anchor: LayerId, asset: &str) -> Option<LayerId> {
        match self
            .bindings
            .get(&anchor)
            .and_then(|assets| assets.get(asset))
        {
            Some(&id) => (id != LayerId::UNRESOLVED).then_some(id),
            // Fresh authored layers must rediscover evaluated expression assets;
            // published bindings describe the old source snapshot. AOUSD Core §9.
            None if self.index.contains_key(&anchor) => None,
            None => self.store.asset_layer(anchor, asset),
        }
    }
}
