// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An open stage: loading, inspection, explanation and authoring.

use alloc::{
    collections::BTreeSet,
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};

use layerstack::{
    ArcKind, ChangeCursor, Changes, CompositionLimits, Contribution, EditError, EditTarget,
    FlattenError, FlattenRequirements, IgnoreCause, InMemoryStore, Layer, LayerId, LayerMuteError,
    LayerStore, NamespaceEdit, NamespaceError, OpinionRole, PathField, PathId, PropertyField,
    ResolvedValue, SchemaKind, Specifier, StageOptions, TargetPath, Time, Transaction,
    UnrepairedPath, ValueSource, Variability,
    asset_dependencies::DependencyTarget,
    value_clips::{
        ClipAssetRequest, ClipAssetRole, ClipAssetStatus, ClipAssetUnavailable, ClipDemandReason,
        ClipEvalError, ClipIssueKind,
    },
};
use layerstack_io::{
    ApplyError, FlattenFailure, ImportPolicy, IoError, LoadOptions, LoadReport, OpenOptions,
    PackageInclusion, PackagePreparationLimits, SaveReport, StageDocument, UnavailableAsset,
};
use wasm_bindgen::prelude::*;

use crate::{
    edit::{self, EditBatch},
    error::Error,
    receipt::EditReceipt,
    storage::HostStorage,
    target::TargetInput,
    value,
    wire::Wire,
};

/// The composition limits a stage opened from TypeScript has unless its
/// `open` options set others: a browser tab's share of memory and time,
/// rather than a native host's. 2 million prim index nodes, a tenth of
/// [`CompositionLimits::DEFAULT_PRIM_INDEX_NODES`] and 15 times those of
/// the largest production scene measured; 512 nodes in one prim index,
/// half the native default and 9 times the largest index measured; the
/// native composed depth
/// ([`CompositionLimits::DEFAULT_COMPOSED_DEPTH`]). The endless namespaces
/// fuzzing found compose to them in 2.3 seconds at most natively, holding
/// 420 MB at most.
pub const BROWSER_LIMITS: CompositionLimits = CompositionLimits {
    prim_index_nodes: Some(2_000_000),
    nodes_per_prim_index: Some(512),
    composed_depth: Some(CompositionLimits::DEFAULT_COMPOSED_DEPTH),
};

/// The id the session layer of a stage opened for editing takes: below
/// the ids derived from identifiers, as host-made layers' ids are.
const SESSION_LAYER: LayerId = LayerId(1);

/// The session layer a stage opens with ([`Stage::open_with_provenance`]):
/// the strongest layer of its root layer stack, which takes its edits
/// ([`Stage::edit_layer`]).
///
/// Spec: AOUSD Core §10.3.1.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SessionLayer {
    /// No session layer.
    #[default]
    None,
    /// An empty anonymous session layer, never saved.
    Anonymous,
    /// A session layer read from `bytes`, the source of `identifier`, as
    /// OpenUSD's `UsdStage::Open(rootLayer, sessionLayer)` opens one from a
    /// file ([`layerstack_io::OpenOptions::session_layer`]): its sublayers
    /// are wanted as any dependency, the asset paths it authors anchor to
    /// it, and saving it writes it back.
    Supplied {
        /// The identifier the host supplies it under.
        identifier: String,
        /// Its source.
        bytes: Vec<u8>,
    },
}

/// An edit target resolved against the stage as it is composed now: the
/// core target, and the layer stack its layer is in.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub(crate) target: EditTarget,
    /// The layers of the target layer's stack, strongest first: the
    /// stage's for a local target, the arc node's for one through an arc.
    pub(crate) stack: Vec<LayerId>,
}

/// One open stage over a [`HostStorage`]: the native object behind the
/// TypeScript `Stage`.
///
/// Every method the module exports returns a positional record the
/// wrapper decodes; crate-private methods of the same names, without the
/// `js_` prefix, build them as `Wire` trees and document their layouts. Paths are text: a prim path
/// (`/World/Rock`) or a property path (`/World/Rock.size`).
///
/// A crate that links these bindings into a module of its own reaches the
/// document through [`Self::document`] and [`Self::document_mut`].
#[wasm_bindgen]
#[derive(Debug)]
pub struct Stage {
    document: StageDocument<HostStorage>,
    /// The observer the wrapper drains after each mutating call.
    cursor: ChangeCursor,
    /// The muted layers as the wrapper last heard them, to report the
    /// layers muted and unmuted since with the next changes.
    heard_muted: BTreeSet<LayerId>,
    /// What a receipt holds to name the stage that made it.
    identity: Arc<()>,
    /// The retained skinning evaluator and the changes it has heard.
    skel: crate::skel::SkelState,
    /// The retained transform and bounds caches and the changes they have
    /// heard.
    geom: crate::geom::GeomState,
}

/// A concrete typed schema a stage registers when it opens, besides
/// OpenUSD's: `name` is `base`, a registered typed schema (OpenUSD's, or
/// one declared before it), and has `base`'s properties and none of its
/// own, as a codeless schema whose `generatedSchema.usda` declares
/// `class "Name" (inherits = </Base>)` and nothing else. A prim of type
/// `name` is `base` to every `IsA` read (`UsdPrim::IsA`), so it draws,
/// bounds and binds as `base` does.
///
/// Spec: AOUSD Core §13.3.1 (typed schemas and their inheritance).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypedSchema {
    /// The schema's name, an identifier no registered schema has.
    pub name: String,
    /// The typed schema it derives from.
    pub base: String,
}

/// OpenUSD's schemas (`layerstack_schemas::openusd`), and `schemas`.
///
/// # Errors
///
/// `argument.invalid` for a name that is not an identifier or is
/// registered already, or a base that is not a registered typed schema.
fn registry(
    schemas: &[TypedSchema],
    tokens: &layerstack::TokenInterner,
) -> Result<layerstack::SchemaRegistry, Error> {
    let openusd = layerstack_schemas::openusd(tokens);
    if schemas.is_empty() {
        return Ok(openusd);
    }
    let mut builder = layerstack::SchemaRegistry::builder();
    layerstack_schemas::register(&mut builder, layerstack_schemas::Domain::ALL, tokens)
        .expect("generated OpenUSD metadata declarations agree");
    let mut declared: Vec<&str> = Vec::new();
    for schema in schemas {
        let invalid = |why: String| Error::new("argument.invalid", format!("schemas: {why}"));
        let identifier = schema
            .name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && schema
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !identifier {
            return Err(invalid(format!("`{}` is not an identifier", schema.name)));
        }
        let name = tokens.intern(&schema.name);
        if openusd.schema(name).is_some() || declared.contains(&schema.name.as_str()) {
            return Err(invalid(format!("`{}` is registered already", schema.name)));
        }
        let base = tokens.intern(&schema.base);
        let typed = declared.contains(&schema.base.as_str())
            || openusd
                .schema(base)
                .is_some_and(|definition| definition.kind.is_typed());
        if !typed {
            return Err(invalid(format!(
                "`{}` is not a registered typed schema",
                schema.base
            )));
        }
        builder.register(layerstack::SchemaDefinition::typed(name).with_parent(base));
        declared.push(&schema.name);
    }
    Ok(builder.build(tokens))
}

impl Stage {
    /// Opens the root layer `identifier` from `bytes`, with every OpenUSD
    /// schema registered (`layerstack_schemas::openusd`), and loads what
    /// storage has: only the root, so every dependency is unavailable
    /// until the host supplies it. `recover` imports malformed content the
    /// reader can recover from ([`ImportPolicy::AllowRecovery`]) instead
    /// of rejecting it. `session_layer` puts an empty anonymous session
    /// layer in front of the root, the stage's edit layer
    /// ([`Self::edit_layer`]): the way to edit a root that cannot be saved,
    /// such as a USDZ package's. The stage composes, and recomposes edits,
    /// within `limits` ([`StageOptions::limits`]; [`BROWSER_LIMITS`] by
    /// default in TypeScript).
    ///
    /// Spec: AOUSD Core §10.3.1 (the session layer is the strongest layer
    /// of the root layer stack).
    ///
    /// # Errors
    ///
    /// The root's read or import failure, with its [`IoErrorKind`] code
    /// (`io.rejected` for a root that does not parse).
    ///
    /// [`IoErrorKind`]: layerstack_io::IoErrorKind
    pub fn open(
        identifier: &str,
        bytes: Vec<u8>,
        recover: bool,
        session_layer: bool,
        limits: CompositionLimits,
    ) -> Result<Self, Error> {
        let session = if session_layer {
            SessionLayer::Anonymous
        } else {
            SessionLayer::None
        };
        Self::open_with_provenance(identifier, bytes, recover, session, limits, false, &[])
    }

    /// Opens a stage as in [`Self::open`], with the session layer
    /// `session_layer`, optionally including the winning source layer,
    /// spec path and field in native resolved values, and with the typed
    /// schemas `schemas` registered besides OpenUSD's.
    ///
    /// Same-module extensions use this evidence to check source correspondence.
    /// Query explanations remain available when `provenance` is false.
    ///
    /// # Errors
    ///
    /// The root's or the session layer's read or import failure, as in
    /// [`Self::open`]; `io.rejected` for a session layer that is the root;
    /// `argument.invalid` for a schema [`TypedSchema`] refuses.
    pub fn open_with_provenance(
        identifier: &str,
        bytes: Vec<u8>,
        recover: bool,
        session_layer: SessionLayer,
        limits: CompositionLimits,
        provenance: bool,
        schemas: &[TypedSchema],
    ) -> Result<Self, Error> {
        let mut storage = HostStorage::new();
        storage.supply(identifier, bytes);
        let mut store = InMemoryStore::default();
        let schemas = registry(schemas, store.tokens())?;
        let (anonymous, supplied) = match session_layer {
            SessionLayer::None => (None, None),
            SessionLayer::Anonymous => {
                store.insert_layer(Layer::new(SESSION_LAYER));
                (Some(SESSION_LAYER), None)
            }
            SessionLayer::Supplied { identifier, bytes } => {
                storage.supply(&identifier, bytes);
                (None, Some(identifier))
            }
        };
        let options = StageOptions {
            schemas: Some(Arc::new(schemas)),
            session_layer: anonymous,
            limits,
            with_provenance: provenance,
            ..StageOptions::default()
        };
        let policy = if recover {
            ImportPolicy::AllowRecovery
        } else {
            ImportPolicy::Strict
        };
        let mut document = StageDocument::open(
            storage,
            identifier,
            OpenOptions {
                stage: options,
                loading: LoadOptions {
                    import_policy: policy,
                    clock: crate::clock::load_clock(),
                    ..LoadOptions::default()
                },
                store: Some(store),
                session_layer: supplied,
            },
        )?;
        let cursor = document.parts_mut().1.change_cursor();
        let skel = crate::skel::SkelState::new(document.parts_mut().1);
        let geom = crate::geom::GeomState::new(document.parts_mut().1);
        Ok(Self {
            document,
            cursor,
            heard_muted: BTreeSet::new(),
            identity: Arc::new(()),
            skel,
            geom,
        })
    }

    /// The document: its store, live stage and storage.
    #[must_use]
    pub fn document(&self) -> &StageDocument<HostStorage> {
        &self.document
    }

    /// The skinning state.
    pub(crate) fn skel_state(&self) -> &crate::skel::SkelState {
        &self.skel
    }

    /// The document, and the skinning state, which reads it.
    pub(crate) fn document_and_skel(
        &mut self,
    ) -> (&StageDocument<HostStorage>, &mut crate::skel::SkelState) {
        (&self.document, &mut self.skel)
    }

    /// The geometry state.
    pub(crate) fn geom_state(&self) -> &crate::geom::GeomState {
        &self.geom
    }

    /// The document, and the geometry state, which reads it.
    pub(crate) fn document_and_geom(
        &mut self,
    ) -> (&StageDocument<HostStorage>, &mut crate::geom::GeomState) {
        (&self.document, &mut self.geom)
    }

    /// The document, mutably, for a crate that links these bindings and
    /// edits the stage natively.
    ///
    /// The live stage records the composed changes of every recomposition,
    /// and the wrapper hears them through its change cursor: edits applied
    /// with [`StageDocument::apply`] (or `LiveStage::apply` through
    /// [`StageDocument::parts_mut`]) and loads recompose at once. Controls
    /// queued on the live stage, such as `LiveStage::mute_layer`, and
    /// layers edited in the store directly recompose, and are heard, only
    /// once [`StageDocument::synchronize`] runs: call it before returning to
    /// JavaScript. The TypeScript `Stage.withNative` calls its listeners
    /// with those changes before it returns, layers whose muting changed
    /// included (`mutedLayers`, `unmutedLayers`).
    ///
    /// Native edits make no [`EditReceipt`]: the [`layerstack::Applied`]
    /// inverse the caller gets back is its own to keep or drop. A receipt
    /// whose guarded inverse covers a spec they changed since (a value it
    /// set, or a prim it defined that they gave a child) fails to undo with
    /// `edit.stale` and changes nothing. Content derived natively belongs
    /// in a layer the user's edits do not target, such as the session
    /// layer of a stage whose user edits target the root: it then never
    /// blocks their undo, and saving, which skips the session layer, never
    /// writes it.
    pub fn document_mut(&mut self) -> &mut StageDocument<HostStorage> {
        &mut self.document
    }

    /// The assets the stage asks for that the host has not supplied yet,
    /// each `[identifier, assetPath, anchor, usedBy, prims]`
    /// ([`StageDocument::unavailable_assets`]): the identifier to supply,
    /// the asset path as authored, the anchor layer's id,
    /// `"sublayer"`, `"reference"` or `"payload"` ([`layerstack::AssetUse::name`])
    /// and the paths of the prims composed without it.
    #[must_use]
    pub(crate) fn unavailable(&self) -> Wire {
        Wire::List(
            self.document
                .unavailable_assets()
                .into_iter()
                .map(|need| unavailable_row(need, self.document.store()))
                .collect(),
        )
    }

    /// Reads what the host supplied for the assets the stage wants
    /// ([`StageDocument::load_requested_assets`]):
    /// `[loaded, failures, reads, compositions]`, the ids of the layers it
    /// read, for each asset that failed `[asset, anchor, code, message]`
    /// (a confirmed-absent asset fails with `io.not_found`), and where its
    /// work went ([`load_work`]).
    ///
    /// # Errors
    ///
    /// A strict open's rejection of a supplied layer (`io.rejected`), with
    /// the layers read before it kept.
    pub(crate) fn load_supplied(&mut self) -> Result<Wire, Error> {
        let report = self.document.load_requested_assets()?;
        Ok(self.load_wire(&report))
    }

    /// Reads what the host supplied for the assets the stage and the layers
    /// read since it last composed want, without composing
    /// ([`StageDocument::read_requested_assets`]): `[loaded, failures,
    /// reads, compositions]`, as [`Self::load_supplied`] returns, with no
    /// compositions. The layers read list what they need themselves in
    /// [`Self::unavailable`], with no prims, until [`Self::load_supplied`]
    /// composes them once.
    ///
    /// # Errors
    ///
    /// A strict open's rejection of a supplied layer (`io.rejected`), with
    /// the layers read before it kept.
    pub(crate) fn read_supplied(&mut self) -> Result<Wire, Error> {
        let report = self.document.read_requested_assets()?;
        Ok(self.load_wire(&report))
    }

    /// `[loaded, failures, reads, compositions]` of `report`, as
    /// [`Self::load_supplied`] and [`Self::read_supplied`] return it.
    fn load_wire(&self, report: &LoadReport) -> Wire {
        let Wire::List(mut result) = load_result(report) else {
            unreachable!("a load result is a list")
        };
        result.extend(load_work(report, &self.document));
        Wire::List(result)
    }

    /// Plans what value clips the attribute at `path` needs at `time`
    /// (`None`: the default time, untimed introspection's plan) and reads
    /// those the host supplied, at most `max_loads` reads, replanning after
    /// each ([`StageDocument::prepare_clip_query`]):
    /// `[complete, budgetExhausted, loadAttempts, error, demands, unavailable, failures, issues]`.
    ///
    /// - `complete`: no demand awaits a source, nothing failed and no
    ///   budget ran out ([`layerstack::value_clips::ClipQueryPlan::is_complete`]).
    /// - `error`: the evaluator's error code, or `null`.
    /// - each demand `[assetPath, anchor, role, reason, status, owner, clipSet]`:
    ///   the asset path as authored or derived from a template, the layer
    ///   authoring the clip metadata, `value`, `manifest` or
    ///   `templateCandidate`, why the query needs it (as
    ///   [`clip_demand_reason`] names it), `loaded`, `missing`,
    ///   `unanswered` or `notResident`, and the prim and clip set that
    ///   define it.
    /// - each unavailable source `[identifier, assetPath, anchor, role, owner, clipSet]`:
    ///   one the host has not supplied, under the identifier to supply.
    /// - each failure `[asset, anchor, code, message]`, as `loadSupplied`'s.
    /// - each issue `[owner, clipSet, code]`, as [`clip_issue_code`] names it.
    ///
    /// Spec: AOUSD Core §12.3, §12.5 (value clips). OpenUSD reads clip
    /// layers synchronously as it resolves (`Usd_ClipSet`), where a
    /// browser supplies them first.
    ///
    /// # Errors
    ///
    /// `path.invalid` for a path that is not a property path.
    pub(crate) fn prepare_clip_query(
        &mut self,
        path: &str,
        time: Option<f64>,
        max_loads: usize,
        max_demands: usize,
    ) -> Result<Wire, Error> {
        check_property_path(path)?;
        let property = self
            .document
            .store()
            .try_property_path(path)
            .map_err(|_| Error::new("path.invalid", "not a property path").at_path(path))?;
        let prepared =
            self.document
                .prepare_clip_query(property, time_of(time), max_loads, max_demands);
        let store = self.document.store();
        let owner = |request: &ClipAssetRequest| {
            Wire::Text(store.paths().display(request.owner, store.tokens()))
        };
        let demands = prepared
            .plan
            .demands
            .iter()
            .map(|demand| {
                let request = &demand.request;
                Wire::List(vec![
                    Wire::text(&*request.identifier),
                    Wire::UInt64(request.anchor.0),
                    Wire::text(clip_role(request.role)),
                    Wire::text(clip_demand_reason(demand.reason)),
                    Wire::text(match request.status {
                        ClipAssetStatus::Loaded(_) => "loaded",
                        ClipAssetStatus::Unavailable(ClipAssetUnavailable::Missing) => "missing",
                        ClipAssetStatus::Unavailable(ClipAssetUnavailable::NotResident(_)) => {
                            "notResident"
                        }
                        ClipAssetStatus::Unavailable(_) => "unanswered",
                    }),
                    owner(request),
                    Wire::text(&*request.set_name),
                ])
            })
            .collect();
        let unavailable = prepared
            .unavailable
            .iter()
            .map(|asset| {
                let request = &asset.request;
                Wire::List(vec![
                    Wire::text(&*asset.identifier),
                    Wire::text(&*request.identifier),
                    Wire::UInt64(request.anchor.0),
                    Wire::text(clip_role(request.role)),
                    owner(request),
                    Wire::text(&*request.set_name),
                ])
            })
            .collect();
        let failures = prepared
            .failures
            .iter()
            .map(|failure| {
                Wire::List(vec![
                    Wire::text(&*failure.request.identifier),
                    Wire::UInt64(failure.request.anchor.0),
                    Wire::text(failure.error.code()),
                    Wire::text(failure.error.message.clone()),
                ])
            })
            .collect();
        let issues = prepared
            .plan
            .issues
            .iter()
            .map(|issue| {
                Wire::List(vec![
                    Wire::Text(store.paths().display(issue.owner, store.tokens())),
                    Wire::text(&*issue.set_name),
                    Wire::text(clip_issue_code(&issue.kind)),
                ])
            })
            .collect();
        Ok(Wire::List(vec![
            Wire::Bool(prepared.plan.is_complete()),
            Wire::Bool(prepared.budget_exhausted || prepared.plan.budget_exhausted),
            Wire::UInt64(prepared.load_attempts as u64),
            prepared
                .plan
                .error
                .map_or(Wire::Null, |error| Wire::text(clip_eval_code(error))),
            Wire::List(demands),
            Wire::List(unavailable),
            Wire::List(failures),
            Wire::List(issues),
        ]))
    }

    /// Every layer of the document:
    /// `[root, session, [[id, identifier, format, readOnly, dirty], ...], editLayer]`,
    /// `session` `null` without one and `editLayer` as [`Self::edit_layer`]. `identifier` is `null` for an
    /// anonymous layer, `format` the identifier's extension (a package
    /// member's own), and a package's layers are read-only.
    #[must_use]
    pub(crate) fn layers(&self) -> Wire {
        let stage = self.document.stage().stage();
        let rows = self
            .document
            .store()
            .layer_ids()
            .into_iter()
            .map(|id| {
                let identifier = self.document.identifier(id);
                Wire::List(vec![
                    Wire::UInt64(id.0),
                    Wire::text_or_null(identifier),
                    Wire::text(identifier.map_or("", format_of)),
                    Wire::Bool(self.read_only(id)),
                    Wire::Bool(self.document.is_dirty(id)),
                ])
            })
            .collect();
        Wire::List(vec![
            stage
                .root_layer()
                .map_or(Wire::Null, |id| Wire::UInt64(id.0)),
            stage
                .session_layer()
                .map_or(Wire::Null, |id| Wire::UInt64(id.0)),
            Wire::List(rows),
            self.edit_layer()
                .map_or(Wire::Null, |id| Wire::UInt64(id.0)),
        ])
    }

    /// The layer edits go to unless they name another: the session layer
    /// when the stage was opened with one, else the root when it can be
    /// saved, else `None` (a package root opened without a session layer).
    #[must_use]
    pub fn edit_layer(&self) -> Option<LayerId> {
        let stage = self.document.stage().stage();
        if let Some(session) = stage.session_layer() {
            return Some(session);
        }
        let root = stage.root_layer()?;
        (!self.read_only(root)).then_some(root)
    }

    /// Whether saving layer `id` would fail as read-only
    /// ([`layerstack_io::IoErrorKind::ReadOnly`]): a package's layers, and
    /// a layer built from format arguments, as the document marks them in
    /// its store ([`LayerStore::layer_read_only`]).
    fn read_only(&self, id: LayerId) -> bool {
        self.document.store().layer_read_only(id).is_some()
    }

    /// Whether layer `id` changed since it was read or last saved.
    ///
    /// # Errors
    ///
    /// `layer.unknown` when the document has no such layer.
    pub(crate) fn layer_dirty(&self, id: u64) -> Result<bool, Error> {
        let layer = LayerId(id);
        self.check_layer(layer)?;
        Ok(self.document.is_dirty(layer))
    }

    /// Mutes layer `id` (`muted` true) or unmutes it, and recomposes:
    /// whether its muting changed ([`Self::mute_and_unmute_layers`] with
    /// the one layer).
    ///
    /// # Errors
    ///
    /// As [`Self::mute_and_unmute_layers`].
    pub(crate) fn set_layer_muted(&mut self, id: u64, muted: bool) -> Result<bool, Error> {
        let named = [MuteRequest::Layer(id)];
        if muted {
            self.mute_and_unmute_layers(&named, &[])
        } else {
            self.mute_and_unmute_layers(&[], &named)
        }
    }

    /// Mutes the layers `mute` names, then unmutes those `unmute` names,
    /// as one request, and recomposes once: OpenUSD's
    /// `UsdStage::MuteAndUnmuteLayers`. Returns whether the muted set
    /// changed. A muted layer contributes no opinions and no sublayers
    /// wherever the stage reaches it, and the stage keeps it as it is: its
    /// content, edits and dirty state stay, and unmuting composes it again.
    /// Muting is the stage's state, not an authored edit, so it makes no
    /// receipt.
    ///
    /// A request names a layer by id, or by identifier anchored to the
    /// root layer as OpenUSD anchors a muted identifier
    /// ([`StageDocument::anchored_identifier`]), which may name a layer the
    /// stage has not read: it is then not read, and an arc to it composes
    /// muted (`composition.muted_asset`) rather than asking for it.
    /// Unmuting a layer not read asks for it ([`Self::unavailable`]). An
    /// empty identifier names nothing, as in OpenUSD.
    ///
    /// As `PcpCache::RequestLayerMuting` does, an unmute request spelled
    /// as a mute request is dropped, so the layer stays muted: a layer is
    /// spelled by its identifier, an anonymous one by its id. A layer
    /// muted and unmuted under two spellings (`b.usda`, `./b.usda`) ends
    /// unmuted. Any layer may be muted except the root (`mute.root_layer`),
    /// which refuses the whole request; unmuting the root, or a layer not
    /// muted, changes nothing. While a layer is muted, edits that target it
    /// fail with `layer.muted`, and saving the stage skips it, as a layer
    /// the stage does not use.
    ///
    /// Spec: AOUSD Core composes the layer stacks (§10.3.1) of the layers
    /// it is given and has no muting; this follows OpenUSD's
    /// `UsdStage::MuteAndUnmuteLayers` (`pxr/usd/usd/stage.cpp`) and
    /// `Pcp_MutedLayers` (`pxr/usd/pcp/layerStackRegistry.cpp`).
    ///
    /// # Errors
    ///
    /// `layer.unknown` for an id the document has no layer for;
    /// `mute.root_layer` when `mute` names the root; the `io.*` error of
    /// an identifier storage cannot anchor. Nothing changes on any of
    /// them. Reading the layers unmuting asks for that storage holds
    /// fails as [`Self::load_supplied`] does, after the muting changed.
    pub(crate) fn mute_and_unmute_layers(
        &mut self,
        mute: &[MuteRequest<'_>],
        unmute: &[MuteRequest<'_>],
    ) -> Result<bool, Error> {
        let spelled: Vec<String> = mute
            .iter()
            .filter_map(|request| self.spelling(request))
            .collect();
        let unmute: Vec<&MuteRequest<'_>> = unmute
            .iter()
            .filter(|request| {
                self.spelling(request)
                    .is_some_and(|spelling| !spelled.contains(&spelling))
            })
            .collect();
        let mut muting = Vec::with_capacity(mute.len());
        for request in mute {
            match *request {
                MuteRequest::Layer(id) => {
                    self.check_layer(LayerId(id))?;
                    muting.push(LayerId(id));
                }
                MuteRequest::Identifier("") => {}
                MuteRequest::Identifier(identifier) => {
                    self.document.anchored_identifier(identifier)?;
                }
            }
        }
        let mut unmuting = Vec::with_capacity(unmute.len());
        for request in unmute {
            match *request {
                MuteRequest::Layer(id) => {
                    self.check_layer(LayerId(id))?;
                    unmuting.push(LayerId(id));
                }
                MuteRequest::Identifier("") => {}
                MuteRequest::Identifier(identifier) => {
                    unmuting.extend(self.document.find_layer_named(identifier)?);
                }
            }
        }
        // Checked above: naming the identifiers now reserves no id for a
        // request that fails.
        for request in mute {
            if let MuteRequest::Identifier(identifier) = *request
                && !identifier.is_empty()
            {
                let id = self.document.layer_named(identifier)?;
                if id == self.document.root_layer() {
                    return Err(mute_error(LayerMuteError::RootLayer));
                }
                muting.push(id);
            }
        }
        // Two spellings of one layer: muted, then unmuted.
        muting.retain(|id| !unmuting.contains(id));
        let changed = self
            .document
            .mute_and_unmute_layers(&muting, &unmuting)
            .map_err(mute_error)?;
        self.document.load_requested_assets()?;
        Ok(changed)
    }

    /// How a mute request is spelled, as OpenUSD compares the requests of
    /// one call: an identifier as given, a layer by its identifier, an
    /// anonymous one by its id; `None` for the empty identifier, which
    /// names nothing.
    fn spelling(&self, request: &MuteRequest<'_>) -> Option<String> {
        match *request {
            MuteRequest::Identifier("") => None,
            MuteRequest::Identifier(identifier) => Some(identifier.to_string()),
            MuteRequest::Layer(id) => Some(
                self.document
                    .identifier(LayerId(id))
                    .map_or_else(|| format!("#{id}"), ToString::to_string),
            ),
        }
    }

    /// Whether the stage mutes the layer `identifier`, anchored to the
    /// root layer, names: OpenUSD's `UsdStage::IsLayerMuted`, which an
    /// identifier the stage never muted answers `false`.
    ///
    /// # Errors
    ///
    /// The `io.*` error of an identifier storage cannot anchor.
    pub(crate) fn is_layer_muted_named(&self, identifier: &str) -> Result<bool, Error> {
        Ok(self
            .document
            .find_layer_named(identifier)?
            .is_some_and(|id| self.document.stage().stage().is_layer_muted(id)))
    }

    /// The layers the composed stage mutes, by ascending id
    /// (`layerstack::Stage::muted_layers`), each `[id, identifier, held]`
    /// ([`Self::muting_row`]): what was muted and composed, not a control
    /// queued since and not yet synchronized.
    #[must_use]
    pub(crate) fn muted_layers(&self) -> Wire {
        Wire::List(
            self.document
                .stage()
                .stage()
                .muted_layers()
                .iter()
                .map(|&id| self.muting_row(id))
                .collect(),
        )
    }

    /// `[id, identifier, held]` for a muted or unmuted layer: its
    /// identifier (`null` for an anonymous layer), and whether the stage
    /// holds it, which it does not for a layer muted by identifier before
    /// it was read.
    fn muting_row(&self, id: LayerId) -> Wire {
        Wire::List(vec![
            Wire::UInt64(id.0),
            self.document.identifier(id).map_or(Wire::Null, Wire::text),
            Wire::Bool(self.document.store().layer(id).is_some()),
        ])
    }

    /// The stage's state now: `[prims, compositionErrors, unresolved]`,
    /// the number of prims its default traversal visits, each composition
    /// error as `[code, message, prim]` ([`layerstack::CompositionError`],
    /// `prim` `null` for an error in a layer stack) and each unresolved
    /// asset as `[anchor, assetPath, usedBy, identifier, reason]`
    /// ([`StageDocument::asset_status`]; `identifier` and `reason`, the
    /// read failure's code, `null` when no read of it failed).
    ///
    /// Spec: AOUSD Core §10.6 (composition errors).
    #[must_use]
    pub(crate) fn status(&self) -> Wire {
        let view = self.document.view();
        let store = self.document.store();
        let errors = self
            .document
            .stage()
            .stage()
            .composition_errors()
            .iter()
            .map(|error| {
                Wire::List(vec![
                    Wire::text(error.code()),
                    Wire::Text(error.display(store).to_string()),
                    error.prim().map_or(Wire::Null, |prim| {
                        Wire::Text(store.paths().display(prim, store.tokens()))
                    }),
                ])
            })
            .collect();
        let unresolved = self
            .document
            .asset_status()
            .unresolved
            .into_iter()
            .map(|asset| {
                Wire::List(vec![
                    Wire::UInt64(asset.anchor.0),
                    Wire::Text(asset.asset_path),
                    Wire::text(asset.used_by.name()),
                    asset.identifier.map_or(Wire::Null, Wire::Text),
                    asset
                        .reason
                        .map_or(Wire::Null, |kind| Wire::text(kind.code())),
                ])
            })
            .collect();
        Wire::List(vec![
            Wire::Number(view.traverse().count() as f64),
            Wire::List(errors),
            Wire::List(unresolved),
        ])
    }

    /// The composed prim at `path`:
    /// `[typeName, specifier, active, defined, abstract, instance, instanceProxy]`,
    /// or `null` when there is none.
    ///
    /// # Errors
    ///
    /// `path.invalid` when `path` is not an absolute prim path.
    pub(crate) fn prim(&self, path: &str) -> Result<Wire, Error> {
        check_prim_path(path)?;
        let Some(prim) = self.document.view().prim(path) else {
            return Ok(Wire::Null);
        };
        Ok(Wire::List(vec![
            Wire::text_or_null(prim.type_name()),
            Wire::text(match prim.specifier() {
                Specifier::Def => "def",
                Specifier::Over => "over",
                Specifier::Class => "class",
            }),
            Wire::Bool(prim.is_active()),
            Wire::Bool(prim.is_defined()),
            Wire::Bool(prim.is_abstract()),
            Wire::Bool(prim.is_instance()),
            Wire::Bool(prim.is_instance_proxy()),
        ]))
    }

    /// The native instancing prototypes (`layerstack::Stage::prototypes`):
    /// one list per prototype, in first-instance traversal order, of the
    /// paths of the instance roots sharing it, in traversal order. A
    /// prototype's position is its identity in this snapshot only.
    ///
    /// OpenUSD: `UsdStage::GetPrototypes` and `UsdPrim::GetInstances`.
    pub(crate) fn prototypes(&self) -> Wire {
        let store = self.document.store();
        Wire::List(
            self.document
                .stage()
                .stage()
                .prototypes()
                .map(|prototype| {
                    Wire::List(
                        prototype
                            .instances()
                            .iter()
                            .map(|&root| Wire::text(store.paths().display(root, store.tokens())))
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    /// The paths of every composed child of the prim at `path`, in
    /// namespace order, inactive and abstract ones included (OpenUSD
    /// `UsdPrim::GetAllChildren`).
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` when no prim is at `path`.
    pub(crate) fn children(&self, path: &str) -> Result<Vec<String>, Error> {
        let store = self.document.store();
        let prim = self.existing_prim(path)?;
        Ok(prim
            .all_children()
            .map(|child| store.paths().display(child.path(), store.tokens()))
            .collect())
    }

    /// The names of the prim's attributes, schema-defined and authored,
    /// in property order.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` when no prim is at `path`.
    pub(crate) fn attribute_names(&self, path: &str) -> Result<Vec<String>, Error> {
        let prim = self.existing_prim(path)?;
        Ok(prim
            .properties()
            .into_iter()
            .filter(|property| property.as_attribute().is_some())
            .map(|property| property.name().to_string())
            .collect())
    }

    /// The names of the prim's relationships, schema-defined and
    /// authored, in property order.
    ///
    /// Spec: AOUSD Core §7.6.5 (relationships).
    /// OpenUSD: `UsdPrim::GetRelationships`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` when no prim is at `path`.
    pub(crate) fn relationship_names(&self, path: &str) -> Result<Vec<String>, Error> {
        let prim = self.existing_prim(path)?;
        Ok(prim
            .properties()
            .into_iter()
            .filter(|property| property.as_relationship().is_some())
            .map(|property| property.name().to_string())
            .collect())
    }

    /// The variant sets of the prim at `path`, each
    /// `[name, selection, variants]`, `selection` `null` when nothing
    /// selects a variant and `variants` the set's variant names in
    /// composed order.
    ///
    /// Spec: AOUSD Core §10.5 (variant sets and selections).
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` when no prim is at `path`.
    pub(crate) fn variant_sets(&self, path: &str) -> Result<Wire, Error> {
        let prim = self.existing_prim(path)?;
        Ok(Wire::List(
            prim.variant_sets()
                .into_iter()
                .map(|(name, selection)| {
                    Wire::List(vec![
                        Wire::text(name),
                        Wire::text_or_null(selection),
                        Wire::List(
                            prim.variant_names(name)
                                .into_iter()
                                .map(Wire::text)
                                .collect(),
                        ),
                    ])
                })
                .collect(),
        ))
    }

    /// The attribute at the property path `path`:
    /// `[typeName, variability, custom, authored]`, or `null` when the
    /// prim has no such attribute.
    ///
    /// # Errors
    ///
    /// `path.invalid` when `path` is not a property path.
    pub(crate) fn attribute(&self, path: &str) -> Result<Wire, Error> {
        check_property_path(path)?;
        let Some(attribute) = self.document.view().attribute(path) else {
            return Ok(Wire::Null);
        };
        Ok(Wire::List(vec![
            Wire::text_or_null(attribute.type_name()),
            Wire::text(match attribute.variability() {
                Variability::Varying => "varying",
                Variability::Uniform => "uniform",
            }),
            Wire::Bool(attribute.is_custom()),
            Wire::Bool(attribute.is_authored()),
        ]))
    }

    /// The attribute's composed value at `time` (the default time when
    /// `None`; a time code interpolates linearly), as a value record
    /// ([`crate::value`]), or `null` when it resolves none.
    ///
    /// Spec: AOUSD Core §12.3 (value resolution).
    ///
    /// # Errors
    ///
    /// `path.invalid`; `value.decode` when a retained array fails to
    /// decode.
    pub(crate) fn get(&self, path: &str, time: Option<f64>) -> Result<Wire, Error> {
        check_property_path(path)?;
        let store = self.document.store();
        let Some(attribute) = self.document.view().attribute(path) else {
            return Ok(Wire::Null);
        };
        let resolved = attribute
            .try_get(time_of(time))
            .map_err(|error| Error::new("value.decode", error.to_string()).at_path(path))?;
        let Some(resolved) = resolved else {
            return Ok(Wire::Null);
        };
        Ok(
            value::encode(&resolved.value, attribute.type_name(), store.tokens())
                .map_err(|error| error.at_path(path))?
                .unwrap_or(Wire::Null),
        )
    }

    /// The composed time sample times of the attribute, ascending.
    ///
    /// # Errors
    ///
    /// `path.invalid`.
    pub(crate) fn time_samples(&self, path: &str) -> Result<Vec<f64>, Error> {
        check_property_path(path)?;
        Ok(self
            .document
            .view()
            .attribute(path)
            .map(|attribute| attribute.sample_times())
            .unwrap_or_default())
    }

    /// Whether each attribute's value might vary with time: several time
    /// samples, a spline, or value clips that may supply several, directly
    /// or under sparse edits; also while the clip metadata, or a set's
    /// single clip, that settles it is not loaded (unknown, answered
    /// conservatively). One per path, `1` or `0`; `0` for a path no
    /// attribute is at.
    ///
    /// Spec: AOUSD Core §12.3.2, §12.3.4 (value clips). OpenUSD:
    /// `UsdAttribute::ValueMightBeTimeVarying`.
    ///
    /// # Errors
    ///
    /// `path.invalid` for a path that is not a property path.
    pub(crate) fn values_might_vary(&self, paths: &[String]) -> Result<Vec<u8>, Error> {
        let view = self.document.view();
        paths
            .iter()
            .map(|path| {
                check_property_path(path)?;
                Ok(u8::from(
                    view.attribute(path.as_str())
                        .is_some_and(layerstack::Attribute::might_be_time_varying),
                ))
            })
            .collect()
    }

    /// Whether each attribute's value source might vary with time, the
    /// default time included: time samples (even one), a spline or value
    /// clips supply it, directly or under sparse edits; also while the
    /// clip metadata that settles whether clips supply it is not loaded
    /// (unknown, answered conservatively). One per path, `1` or `0`; `0`
    /// for a path no attribute is at.
    ///
    /// Spec: AOUSD Core §12.3.2, §12.3.4 (value clips). OpenUSD:
    /// `UsdAttributeQuery::ValueSourceMightBeTimeVarying`.
    ///
    /// # Errors
    ///
    /// `path.invalid` for a path that is not a property path.
    pub(crate) fn value_sources_might_vary(&self, paths: &[String]) -> Result<Vec<u8>, Error> {
        let view = self.document.view();
        paths
            .iter()
            .map(|path| {
                check_property_path(path)?;
                Ok(u8::from(view.attribute(path.as_str()).is_some_and(
                    layerstack::Attribute::value_source_might_be_time_varying,
                )))
            })
            .collect()
    }

    /// Why the attribute has its value at `time`:
    /// `[value, source, lower, upper, opinions]`, or `null` when the
    /// attribute does not exist. `value` is a value record or `null`;
    /// `source` is `none`, `default`, `timeSamples`, `spline`,
    /// `valueClips` or `fallback`, with the bracketing sample times
    /// `lower` and `upper` for `timeSamples` (else `null`); a blocked
    /// sample held at `time` is `timeSamples` with a `null` value and a
    /// `block` opinion, as OpenUSD's resolve info names it. Each opinion,
    /// strongest first, is
    /// `[layer, specPath, arc, role, detail, offset, scale, target]`: the
    /// layer id, the spec path in that layer, the arc that reaches it
    /// (`null` without a node), `contributed`, `block` or `ignored`, what it
    /// contributed (`value`, `arrayEdit`, `dictionary`, `listEdit`) or why
    /// it was ignored (`shadowed`, `cutOffByBlock`, `incompatible`), the
    /// layer offset mapping stage time to its layer, and the record of the
    /// edit target that writes where the opinion lives ([`crate::target`]:
    /// the opinion's node, in its layer; `null` without a node, or for a
    /// layer outside the node's stack, such as a value clip's).
    ///
    /// Spec: AOUSD Core §12.3 (value resolution), §12.3.2.1 (layer
    /// offsets). OpenUSD: `UsdAttribute::GetResolveInfo`.
    ///
    /// # Errors
    ///
    /// `path.invalid`; `value.decode` for a retained array that fails to
    /// decode.
    pub(crate) fn explain(&self, path: &str, time: Option<f64>) -> Result<Wire, Error> {
        check_property_path(path)?;
        let store = self.document.store();
        let Some(attribute) = self.document.view().attribute(path) else {
            return Ok(Wire::Null);
        };
        let Some(why) = attribute.explain(time_of(time)) else {
            return Ok(Wire::Null);
        };
        let prim = store
            .try_property_path(path)
            .map_err(|_| Error::new("path.invalid", "not a property path").at_path(path))?
            .prim_path();
        let value = match &why.value {
            Some(value) => value::encode(value, attribute.type_name(), store.tokens())
                .map_err(|error| error.at_path(path))?
                .unwrap_or(Wire::Null),
            None => Wire::Null,
        };
        let (source, lower, upper) = match why.source {
            ValueSource::None => ("none", Wire::Null, Wire::Null),
            ValueSource::Default => ("default", Wire::Null, Wire::Null),
            ValueSource::TimeSamples { lower, upper, .. } => {
                ("timeSamples", Wire::Number(lower), Wire::Number(upper))
            }
            ValueSource::Spline => ("spline", Wire::Null, Wire::Null),
            ValueSource::ValueClips => ("valueClips", Wire::Null, Wire::Null),
            ValueSource::Fallback => ("fallback", Wire::Null, Wire::Null),
        };
        let opinions = why
            .opinions
            .iter()
            .map(|opinion| {
                let (role, detail) = match &opinion.role {
                    OpinionRole::Contributed(contribution) => (
                        "contributed",
                        match contribution {
                            Contribution::Value => "value",
                            Contribution::ArrayEdit => "arrayEdit",
                            Contribution::Dictionary(_) => "dictionary",
                            Contribution::ListEdit => "listEdit",
                            // A contribution this build does not name.
                            _ => "",
                        },
                    ),
                    OpinionRole::Block => ("block", ""),
                    OpinionRole::Ignored(cause) => (
                        "ignored",
                        match cause {
                            IgnoreCause::Shadowed => "shadowed",
                            IgnoreCause::CutOffByBlock => "cutOffByBlock",
                            IgnoreCause::Incompatible => "incompatible",
                            // A cause this build does not name.
                            _ => "",
                        },
                    ),
                };
                let offset = opinion.layer_offset();
                let target = opinion
                    .node
                    .and_then(|_| {
                        self.node_target_record(prim, opinion.opinion.node(), opinion.layer())
                    })
                    .unwrap_or(Wire::Null);
                Wire::List(vec![
                    Wire::UInt64(opinion.layer().0),
                    Wire::Text(opinion.spec_path().display(store.paths(), store.tokens())),
                    opinion
                        .arc_kind()
                        .map_or(Wire::Null, |arc| Wire::text(arc_name(arc))),
                    Wire::text(role),
                    if detail.is_empty() {
                        Wire::Null
                    } else {
                        Wire::text(detail)
                    },
                    Wire::Number(offset.offset),
                    Wire::Number(offset.scale),
                    target,
                ])
            })
            .collect();
        Ok(Wire::List(vec![
            value,
            Wire::text(source),
            lower,
            upper,
            Wire::List(opinions),
        ]))
    }

    /// The bytes the host supplied so far ([`HostStorage::supplied_bytes`]).
    #[must_use]
    pub(crate) fn supplied_bytes(&self) -> u64 {
        self.document.storage().supplied_bytes()
    }

    /// The composed metadata field `key` of the prim or property at
    /// `path` as a value record ([`crate::value`]), or `null` when nothing
    /// authors it (or no object is at `path`). A token list (`apiSchemas`)
    /// is a `token[]`, a target list a `path[]` of text, and a dictionary
    /// a `"d"` record.
    ///
    /// Spec: AOUSD Core §12.2 (metadata resolution). OpenUSD:
    /// `UsdObject::GetMetadata`.
    ///
    /// # Errors
    ///
    /// `path.invalid`; `value.decode` for a retained array that fails to
    /// decode.
    pub(crate) fn metadata(&self, path: &str, key: &str) -> Result<Wire, Error> {
        let view = self.document.view();
        let resolved = if path.contains('.') {
            check_property_path(path)?;
            view.property(path)
                .and_then(|property| property.metadata(key))
        } else {
            check_prim_path(path)?;
            view.prim(path).and_then(|prim| prim.metadata(key))
        };
        let Some(resolved) = resolved else {
            return Ok(Wire::Null);
        };
        let store = self.document.store();
        let texts = |name: &str, items: Vec<String>| {
            Wire::List(vec![
                Wire::text(name),
                Wire::text("a"),
                Wire::text("string"),
                Wire::Number(1.0),
                Wire::Number(items.len() as f64),
                Wire::List(items.into_iter().map(Wire::Text).collect()),
            ])
        };
        let encoded = match resolved.value {
            ResolvedValue::Scalar(value) => value::encode(&value, None, store.tokens()),
            ResolvedValue::TokenList(items) => Ok(Some(texts(
                "token[]",
                items
                    .into_iter()
                    .map(|token| store.tokens().resolve(token).to_string())
                    .collect(),
            ))),
            ResolvedValue::PathList(items) => Ok(Some(texts(
                "path[]",
                items
                    .into_iter()
                    .map(|target| target.display(store.paths(), store.tokens()))
                    .collect(),
            ))),
            ResolvedValue::Dictionary(entries) => value::encode(
                &layerstack::Value::Dictionary(entries),
                None,
                store.tokens(),
            ),
            ResolvedValue::ValueList(items) => {
                value::encode(&layerstack::Value::Array(items), None, store.tokens())
            }
            // A resolved form this build does not encode reads as opaque.
            _ => Ok(Some(value::opaque("unknown"))),
        };
        Ok(encoded
            .map_err(|error| error.at_path(path))?
            .unwrap_or(Wire::Null))
    }

    /// The stage's metadata field `key` (`upAxis`, `metersPerUnit`,
    /// `startTimeCode`), encoded as [`Self::metadata`] encodes a value: the
    /// session layer's opinion over the root layer's, else the field's
    /// fallback (`upAxis` reads `Y`, `metersPerUnit` `0.01`); `null` for a
    /// field with neither. Sublayers' metadata does not take part. Asset
    /// paths are as authored.
    ///
    /// Spec: AOUSD Core §12.2.7 (layer metadata resolution). OpenUSD:
    /// `UsdStage::GetMetadata`.
    ///
    /// # Errors
    ///
    /// `value.decode` for a value this build cannot encode.
    pub(crate) fn stage_metadata(&self, key: &str) -> Result<Wire, Error> {
        let store = self.document.store();
        let key = store.tokens().intern(key);
        let Some(value) = self.document.stage().stage().layer_metadata(key, store) else {
            return Ok(Wire::Null);
        };
        Ok(value::encode(&value, None, store.tokens())?.unwrap_or(Wire::Null))
    }

    /// The schemas the stage was composed with, sorted by name:
    /// `[[name, kind, bases, canOnlyApplyTo, allowedInstanceNames], ...]`.
    /// `kind` is `concreteTyped`, `abstractTyped`, `singleApplyApi` or
    /// `multipleApplyApi`; `bases` a typed schema's ancestors, nearest
    /// first, in OpenUSD's order (empty for an applied schema);
    /// `canOnlyApplyTo` and `allowedInstanceNames` an applied schema's
    /// restrictions (empty when it has none).
    ///
    /// Spec: AOUSD Core §13.3 (typed and applied schemas). OpenUSD:
    /// `UsdSchemaRegistry`, `TfType::GetAllAncestorTypes`.
    #[must_use]
    pub(crate) fn schemas(&self) -> Wire {
        let store = self.document.store();
        let tokens = store.tokens();
        let Some(registry) = self.document.stage().stage().schemas() else {
            return Wire::List(Vec::new());
        };
        let names = |ids: &[layerstack::TokenId]| {
            Wire::List(
                ids.iter()
                    .map(|&id| Wire::text(tokens.resolve(id)))
                    .collect(),
            )
        };
        let mut rows: Vec<(String, Wire)> = registry
            .schemas()
            .map(|schema| {
                let name = tokens.resolve(schema.name).to_string();
                let kind = match schema.kind {
                    SchemaKind::ConcreteTyped => "concreteTyped",
                    SchemaKind::AbstractTyped => "abstractTyped",
                    SchemaKind::SingleApplyApi => "singleApplyApi",
                    SchemaKind::MultipleApplyApi => "multipleApplyApi",
                };
                // `ancestors` starts with the schema itself.
                let bases = registry.ancestors(schema.name);
                let row = Wire::List(vec![
                    Wire::text(&name),
                    Wire::text(kind),
                    names(bases.get(1..).unwrap_or(&[])),
                    names(&schema.can_only_apply_to),
                    names(&schema.allowed_instance_names),
                ]);
                (name, row)
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Wire::List(rows.into_iter().map(|(_, row)| row).collect())
    }

    /// The relationship at the property path `path`: `[custom, targets]`,
    /// its composed targets in order as text, or `null` when the prim has
    /// no such relationship.
    ///
    /// Spec: AOUSD Core §7.6.5.1.1 (`targetPaths`), §12.4 (list ops).
    /// OpenUSD: `UsdRelationship::GetTargets`.
    ///
    /// # Errors
    ///
    /// `path.invalid` when `path` is not a property path.
    pub(crate) fn relationship(&self, path: &str) -> Result<Wire, Error> {
        check_property_path(path)?;
        let store = self.document.store();
        let Some(relationship) = self.document.view().relationship(path) else {
            return Ok(Wire::Null);
        };
        let targets = relationship
            .targets()
            .into_iter()
            .map(|target| Wire::Text(target.display(store.paths(), store.tokens())))
            .collect();
        Ok(Wire::List(vec![
            Wire::Bool(relationship.is_custom()),
            Wire::List(targets),
        ]))
    }

    /// Applies the edits of `batch` to the layer `target` as one
    /// transaction ([`StageDocument::apply`]), which loads the assets the
    /// edits ask for that the host has supplied; those it has not stay
    /// unavailable (`unavailable`). `batch` is left empty.
    ///
    /// The edits name stage paths and stage times: `target` is edited
    /// through its local edit target, at the offset the stage's layer
    /// stack reads it at, so a sample set at stage time 15 in a sublayer
    /// authored with `(offset = 10)` is written at its time 5 (OpenUSD's
    /// `UsdStage::GetEditTargetForLocalLayer`).
    ///
    /// Either every edit applies or none does. The receipt holds the
    /// inverse, and its record is `[target, changes, loaded, failures,
    /// unavailable]`: the target layer's id, the composed changes of the
    /// transaction (`[created, removed, resynced, changedInfoOnly, properties, resyncedProperties]`), the
    /// ids of the layers loaded for it, each asset that failed to load as
    /// `[asset, anchor, code, message]`, and the assets it asks for that
    /// the host has not supplied, as `unavailable` rows.
    ///
    /// Spec: AOUSD Core §7 (scene description), §10.3.1 (the layer stack
    /// edits target).
    ///
    /// # Errors
    ///
    /// - `layer.unknown`, `layer.read_only` (a package member: open with a
    ///   session layer), `layer.outside_stack` (a layer the stage only
    ///   reaches through an arc, as OpenUSD's `UsdStage::SetEditTarget`
    ///   refuses one).
    /// - The lowering errors, naming the edit: `path.invalid`,
    ///   `argument.invalid` and the `value.*` codes.
    /// - `edit.*` when the transaction is rejected
    ///   ([`EditError::code`]), naming the edit for a rejected one; nothing
    ///   changed.
    pub fn apply(&mut self, target: u64, batch: &mut EditBatch) -> Result<EditReceipt, Error> {
        self.apply_target(&TargetInput::layer(target), batch)
    }

    /// Applies `batch` through the edit target `input`
    /// ([`crate::target`]), resolved against the stage now.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::resolve_target`] (`layer.muted`,
    /// `layer.outside_stack`, `target.stale`, ...), then those of
    /// [`Self::apply_recorded`].
    pub(crate) fn apply_target(
        &mut self,
        input: &TargetInput,
        batch: &mut EditBatch,
    ) -> Result<EditReceipt, Error> {
        let resolved = self.resolve_target(input)?;
        let record = Self::target_record(input, &resolved);
        self.apply_recorded(&resolved, record, batch)
    }

    /// [`Self::apply_recorded`], for tests that build a target natively:
    /// its receipt names the target by its layer's id.
    #[cfg(test)]
    pub(crate) fn apply_through(
        &mut self,
        target: &Resolved,
        batch: &mut EditBatch,
    ) -> Result<EditReceipt, Error> {
        let record = Wire::UInt64(target.target.layer().0);
        self.apply_recorded(target, record, batch)
    }

    /// Applies `batch` through `target`, as [`Self::apply`] applies it to
    /// a layer: the edits name stage paths and times, which the target
    /// maps to its layer's specs and times. The receipt names the target by
    /// `record`.
    ///
    /// # Errors
    ///
    /// As [`Self::apply`], `layer.outside_stack` aside: `layer.read_only`,
    /// `layer.muted`, the lowering errors and `edit.*`, among them
    /// `edit.rejected.unmappable` for a path the target does not map.
    fn apply_recorded(
        &mut self,
        target: &Resolved,
        record: Wire,
        batch: &mut EditBatch,
    ) -> Result<EditReceipt, Error> {
        let layer = target.target.layer();
        self.check_writable(layer)?;
        let batch = core::mem::take(batch);
        let lowered = {
            let (store, live) = self.document.parts_mut();
            edit::lower(&batch, live.stage(), store, &target.target, &target.stack)?
        };
        self.commit(record, layer, &lowered.transaction, |op| {
            let index = lowered.edit_of(op)?;
            let path = batch.edits.get(index as usize).map(edit::EditInput::path)?;
            Some((index, path))
        })
    }

    /// Undoes the edits `receipt` holds: applies its guarded inverse, and
    /// returns the receipt of the undo, whose inverse redoes them. The
    /// receipt is spent: it undoes once.
    ///
    /// # Errors
    ///
    /// - `receipt.affinity` for a receipt of another stage,
    ///   `receipt.released` for one already undone.
    /// - `edit.stale` when a slot the edits wrote changed since
    ///   ([`EditError::StaleValue`]): nothing changed, and the receipt
    ///   still holds its inverse.
    pub fn undo(&mut self, receipt: &mut EditReceipt) -> Result<EditReceipt, Error> {
        if !Arc::ptr_eq(&receipt.stage, &self.identity) {
            return Err(Error::new(
                "receipt.affinity",
                "the receipt belongs to another stage",
            ));
        }
        let Some(inverse) = receipt.inverse.take() else {
            return Err(Error::new(
                "receipt.released",
                "the receipt was already undone",
            ));
        };
        match self.commit(
            receipt.target_record.clone(),
            receipt.target,
            &inverse,
            |_| None,
        ) {
            Ok(redo) => Ok(redo),
            Err(error) => {
                receipt.inverse = Some(inverse);
                Err(error)
            }
        }
    }

    /// Moves the prim or property at `from` to `to`, both in stage
    /// namespace: a rename when only the last name changes, a reparent
    /// otherwise. [`NamespaceEdit::prepare`] plans it against the composed
    /// stage through the layer `target`, then it applies as one
    /// transaction, as [`Self::apply`] applies edits, and its receipt
    /// undoes it.
    ///
    /// Every opinion of the root layer stack moves together, the prim's
    /// variant sets and the specs inside its variants with it, and the
    /// paths that name the moved object (relationship targets,
    /// connections, internal arcs, path expressions) are repaired. So one
    /// move may edit several layers of the stack; a layer it would edit
    /// that cannot be saved fails it with `layer.read_only`.
    ///
    /// Spec: AOUSD Core §8 (namespace), §12.4 (path list ops).
    /// OpenUSD: `UsdNamespaceEditor::MovePrimAtPath` and
    /// `MovePropertyAtPath`.
    ///
    /// # Errors
    ///
    /// - `path.invalid` when a path is not a prim or property path.
    /// - The `target` errors of [`Self::apply`].
    /// - `namespace.*` when the move cannot be prepared (the codes are
    ///   listed on [`Error`]); nothing changed.
    pub fn move_object(&mut self, target: u64, from: &str, to: &str) -> Result<EditReceipt, Error> {
        self.move_through(&TargetInput::layer(target), from, to)
    }

    /// [`Self::move_object`] through the edit target `input`, which must
    /// be a local layer's: `UsdNamespaceEditor` edits the opinions of the
    /// stage's layer stack, and refuses an edit target that maps through an
    /// arc or into a variant (`namespace.unmappable`).
    ///
    /// # Errors
    ///
    /// As [`Self::move_object`]; `namespace.unmappable` for a `"variant"`
    /// or `"arc"` target.
    pub(crate) fn move_through(
        &mut self,
        input: &TargetInput,
        from: &str,
        to: &str,
    ) -> Result<EditReceipt, Error> {
        let (record, target) = self.namespace_target(input, from)?;
        let source = self.target_path(from)?;
        let destination = self.target_path(to)?;
        let prepared = {
            let (store, live) = self.document.parts_mut();
            NamespaceEdit::prepare(
                live.stage(),
                store,
                &EditTarget::for_layer(target),
                source,
                destination,
            )
        };
        let edit =
            prepared.map_err(|error| namespace_error(&error, self.document.store(), from))?;
        self.commit_namespace(record, target, &edit, from)
    }

    /// Deletes the prim or property at `path` from the composed stage
    /// through the layer `target`, as one undoable transaction:
    /// [`NamespaceEdit::prepare_delete`] removes every opinion of the root
    /// layer stack for it and the paths and arcs that name it, where a
    /// composed prim depends on them. `removePrim` edits, by contrast,
    /// remove one spec from one layer.
    ///
    /// Spec: AOUSD Core §8 (namespace), §12.4 (path list ops).
    /// OpenUSD: `UsdNamespaceEditor::DeletePrimAtPath` and
    /// `DeletePropertyAtPath`.
    ///
    /// # Errors
    ///
    /// As [`Self::move_object`].
    pub fn delete_object(&mut self, target: u64, path: &str) -> Result<EditReceipt, Error> {
        self.delete_through(&TargetInput::layer(target), path)
    }

    /// [`Self::delete_object`] through the edit target `input`, which must
    /// be a local layer's, as for [`Self::move_through`].
    ///
    /// # Errors
    ///
    /// As [`Self::move_through`].
    pub(crate) fn delete_through(
        &mut self,
        input: &TargetInput,
        path: &str,
    ) -> Result<EditReceipt, Error> {
        let (record, target) = self.namespace_target(input, path)?;
        let object = self.target_path(path)?;
        let prepared = {
            let (store, live) = self.document.parts_mut();
            NamespaceEdit::prepare_delete(
                live.stage(),
                store,
                &EditTarget::for_layer(target),
                object,
            )
        };
        let edit =
            prepared.map_err(|error| namespace_error(&error, self.document.store(), path))?;
        self.commit_namespace(record, target, &edit, path)
    }

    /// The record and layer of a namespace edit's target: a local layer's,
    /// which must be one of the stage's layer stack and writable.
    fn namespace_target(&self, input: &TargetInput, path: &str) -> Result<(Wire, LayerId), Error> {
        if input.kind != "layer" {
            return Err(Error::new(
                "namespace.unmappable",
                "a move or delete edits the opinions of the stage's layer stack, so its target \
                 must be a layer of the stack, not an arc or a variant: pass a layer, or \
                 stage.editTargetForLayer(layer)",
            )
            .at_path(path));
        }
        let resolved = self.resolve_target(input)?;
        let layer = resolved.target.layer();
        self.check_target(layer)?;
        Ok((Self::target_record(input, &resolved), layer))
    }

    /// A prim path (`/World/Rock`) or a property path (`/World/Rock.size`)
    /// as a namespace edit names it.
    fn target_path(&self, path: &str) -> Result<TargetPath, Error> {
        let store = self.document.store();
        if path.contains('.') {
            check_property_path(path)?;
            let property = store.try_property_path(path).map_err(|error| {
                Error::new("path.invalid", format!("not a property path: {error}")).at_path(path)
            })?;
            Ok(TargetPath::Property(property))
        } else {
            check_prim_path(path)?;
            let prim = store.try_path(path).map_err(|error| {
                Error::new("path.invalid", format!("not a prim path: {error}")).at_path(path)
            })?;
            Ok(TargetPath::Prim(prim))
        }
    }

    /// Applies a prepared namespace edit, which changes no read-only
    /// layer ([`NamespaceError::ReadOnlyLayer`]); its receipt lists the
    /// paths the edit leaves naming the old location
    /// ([`NamespaceEdit::unrepaired_paths`]).
    fn commit_namespace(
        &mut self,
        record: Wire,
        target: LayerId,
        edit: &NamespaceEdit,
        path: &str,
    ) -> Result<EditReceipt, Error> {
        let mut receipt = self
            .commit(record, target, edit.transaction(), |_| None)
            .map_err(|error| error.at_path(path))?;
        if let Wire::List(fields) = &mut receipt.record
            && let Some(last) = fields.last_mut()
        {
            let store = self.document.store();
            *last = Wire::List(
                edit.unrepaired_paths()
                    .iter()
                    .map(|left| unrepaired_row(left, store))
                    .collect(),
            );
        }
        Ok(receipt)
    }

    /// The composed changes since the last call:
    /// `[batches, mutedLayers, unmutedLayers]`.
    ///
    /// `batches` holds one batch per recomposition, oldest first, each a
    /// `[created, removed, resynced, changedInfoOnly, properties, resyncedProperties]` record
    /// (`changes_record`): an empty list when nothing changed, and `null`
    /// when the history the cursor needs was evicted (more than the
    /// history budget changed in one call), which the wrapper reports as a
    /// resync of `/`. `mutedLayers` and `unmutedLayers` are the ids of the
    /// layers the composed stage mutes now and did not at the last call,
    /// and the reverse ([`Self::muted_layers`]), ascending: a mute or
    /// unmute the call sees once it is composed, whoever requested it.
    /// OpenUSD: `UsdNotice::LayerMutingChanged`. With `render` false the
    /// batches are consumed unrendered and the lists are empty.
    #[must_use]
    pub(crate) fn take_changes(&mut self, render: bool) -> Wire {
        let store = self.document.store();
        let batches = match self.document.stage().changes_since(&mut self.cursor) {
            Ok(batches) => Wire::List(
                batches
                    .filter(|_| render)
                    .map(|changes| changes_record(changes, store))
                    .collect(),
            ),
            Err(_) => Wire::Null,
        };
        let muted = self.document.stage().stage().muted_layers().clone();
        let rows = |ids: &mut dyn Iterator<Item = &LayerId>| {
            Wire::List(
                ids.filter(|_| render)
                    .map(|&id| self.muting_row(id))
                    .collect(),
            )
        };
        let record = Wire::List(vec![
            batches,
            rows(&mut muted.difference(&self.heard_muted)),
            rows(&mut self.heard_muted.difference(&muted)),
        ]);
        self.heard_muted = muted;
        record
    }

    fn check_target(&self, target: LayerId) -> Result<(), Error> {
        self.check_writable(target)?;
        if !self
            .document
            .stage()
            .stage()
            .layer_stack()
            .contains(&target)
        {
            return Err(Error {
                identifier: self.document.identifier(target).map(ToString::to_string),
                ..Error::new(
                    "layer.outside_stack",
                    "the layer is not in the stage's layer stack: the stage reaches it only \
                     through an arc",
                )
            });
        }
        Ok(())
    }

    /// Checks that edits can be written to `layer`: the document has it,
    /// it can be saved and it is not muted.
    fn check_writable(&self, layer: LayerId) -> Result<(), Error> {
        let identifier = self.document.identifier(layer);
        let named = |error: Error| Error {
            identifier: identifier.map(ToString::to_string),
            ..error
        };
        if self.document.store().layer(layer).is_none() {
            return Err(Error::new(
                "layer.unknown",
                "the stage has no layer with this id",
            ));
        }
        if self.read_only(layer) {
            let message = if self.document.stage().stage().has_local_layer(layer) {
                "the layer cannot be saved, so it is not edited: open the stage with a session \
                 layer (editLayer: \"session\") and edit that"
            } else {
                "the layer cannot be saved (a package member), so it is not edited: author an \
                 override in the stage's own layers instead, through \
                 stage.editTargetForLayer(stage.editLayer)"
            };
            return Err(named(Error::new("layer.read_only", message)));
        }
        if self.document.stage().stage().is_layer_muted(layer) {
            return Err(named(Error::new(
                "layer.muted",
                "the layer is muted, so the stage does not compose it: unmute it to edit it",
            )));
        }
        Ok(())
    }

    /// Applies `txn` through the target `record` names, whose layer is
    /// `target`, as the receipt of what it did.
    fn commit<'a>(
        &mut self,
        record: Wire,
        target: LayerId,
        txn: &Transaction,
        edit_of: impl Fn(usize) -> Option<(u32, &'a str)>,
    ) -> Result<EditReceipt, Error> {
        let generations = self.generations();
        match self.document.apply(txn) {
            Ok((applied, report)) => {
                let written = self.written_since(&generations);
                Ok(self.receipt(record, target, applied, &report, None, written))
            }
            Err(ApplyError::Load { applied, error }) => {
                let written = self.written_since(&generations);
                Ok(self.receipt(
                    record,
                    target,
                    *applied,
                    &LoadReport::default(),
                    Some(&error),
                    written,
                ))
            }
            Err(ApplyError::Edit(error)) => {
                let store = self.document.store();
                // A rejected edit is named by its index among the edits
                // sent, not among the native edits they expand to.
                if let EditError::Rejected { op, reason } = &error
                    && let Some((index, path)) = edit_of(*op)
                {
                    let message = format!("edit {index} rejected: {}", reason.display(store));
                    return Err(Error::new(error.code(), message)
                        .at_edit(index)
                        .at_path(path));
                }
                Err(Error::new(error.code(), error.display(store).to_string()))
            }
            Err(error) => {
                let store = self.document.store();
                Err(Error::new(error.code(), error.display(store).to_string()))
            }
        }
    }

    /// Each layer's generation, to tell the layers a transaction wrote.
    fn generations(&self) -> Vec<(LayerId, u64)> {
        let store = self.document.store();
        store
            .layer_ids()
            .into_iter()
            .filter_map(|id| Some((id, store.layer(id)?.generation())))
            .collect()
    }

    /// The layers whose generation moved since `before`, ascending.
    fn written_since(&self, before: &[(LayerId, u64)]) -> Vec<LayerId> {
        self.generations()
            .into_iter()
            .filter(|now| !before.contains(now))
            .map(|(id, _)| id)
            .collect()
    }

    /// The receipt of an applied transaction. Its record is
    /// `[target, changes, loaded, failures, unavailable, layers,
    /// unrepaired]`: the target's record ([`crate::target`]), the
    /// composed changes of the transaction (`[created, removed, resynced,
    /// changedInfoOnly, properties, resyncedProperties]`), the ids of the layers loaded for
    /// it, each asset that failed to load as `[asset, anchor, code,
    /// message]`, the assets it asks for that the host has not supplied
    /// yet, as [`Self::unavailable`] rows, the ids of the layers it wrote
    /// (a move can write several), and, for a move or delete, the paths
    /// it leaves naming the old location as [`unrepaired_row`]s (empty
    /// here; [`Self::commit_namespace`] fills it).
    fn receipt(
        &self,
        record: Wire,
        target: LayerId,
        applied: layerstack::Applied,
        report: &LoadReport,
        failed: Option<&IoError>,
        written: Vec<LayerId>,
    ) -> EditReceipt {
        let store = self.document.store();
        let Wire::List(mut result) = load_result(report) else {
            unreachable!("a load result is a list")
        };
        let mut failures = match result.pop() {
            Some(Wire::List(failures)) => failures,
            _ => Vec::new(),
        };
        let loaded = result.pop().unwrap_or(Wire::List(Vec::new()));
        if let Some(error) = failed {
            failures.push(Wire::List(vec![
                Wire::text(error.identifier.clone().unwrap_or_default()),
                Wire::Null,
                Wire::text(error.code()),
                Wire::text(error.message.clone()),
            ]));
        }
        let unavailable = self
            .document
            .unavailable_assets()
            .into_iter()
            .filter(|need| {
                applied.asset_requests.iter().any(|request| {
                    request.anchor == need.anchor && request.asset_path == need.asset_path
                })
            })
            .map(|need| unavailable_row(need, store))
            .collect();
        let receipt = Wire::List(vec![
            record.clone(),
            changes_record(&applied.changes, store),
            loaded,
            Wire::List(failures),
            Wire::List(unavailable),
            Wire::List(written.into_iter().map(|id| Wire::UInt64(id.0)).collect()),
            Wire::List(Vec::new()),
        ]);
        EditReceipt {
            stage: Arc::clone(&self.identity),
            target,
            target_record: record,
            inverse: Some(applied.inverse),
            record: receipt,
        }
    }

    /// Saves the dirty layers the stage uses to the host's storage
    /// ([`StageDocument::save`], OpenUSD's `UsdStage::Save`: not the
    /// session layer nor its sublayers), or only layer `layer` when given
    /// ([`StageDocument::save_layers`]). Each layer is written in its own
    /// format to the identifier it was read from; the bytes become that
    /// identifier's source and are handed to the host to persist.
    ///
    /// The record is `[written, failures, anonymous, skipped]`: each layer
    /// written as `[layer, identifier, bytes]`, each that could not be as
    /// `[layer, code, message]` (`io.read_only` for a package member, which
    /// stays dirty), the ids of the dirty anonymous layers, which have
    /// nowhere to go (export them with `exportLayer`), and, for a save of
    /// the whole stage, each dirty layer with an identifier it left
    /// unwritten as `[layer, reason]`: `"muted"`, or `"unused"` for a layer
    /// the stage holds but no longer composes (an asset whose reference was
    /// removed after an edit through it). Save it with `save(layer)`.
    ///
    /// Spec: AOUSD Core §7.1 (layers are saved as authored).
    ///
    /// # Errors
    ///
    /// `layer.unknown` when `layer` names no layer of the stage.
    pub(crate) fn save(&mut self, layer: Option<u64>) -> Result<Wire, Error> {
        let (report, skipped) = match layer {
            Some(id) => {
                self.check_layer(LayerId(id))?;
                (self.document.save_layers(&[LayerId(id)]), Vec::new())
            }
            None => {
                let report = self.document.save();
                let skipped = self.skipped_by(&report);
                (report, skipped)
            }
        };
        let mut record = self.save_record(&report);
        if let Wire::List(fields) = &mut record {
            fields.push(Wire::List(skipped));
        }
        Ok(record)
    }

    /// The dirty layers with an identifier a save of the stage left
    /// unwritten, as `[layer, reason]` rows: OpenUSD's `UsdStage::Save`
    /// saves the layers the stage uses, so a muted layer and one no longer
    /// composed stay dirty.
    fn skipped_by(&self, report: &SaveReport) -> Vec<Wire> {
        let stage = self.document.stage().stage();
        // The session layer stack, which a save of the stage leaves alone
        // (`UsdStage::Save`), as `save_session_layers` saves it.
        let mut session = BTreeSet::new();
        let mut queue: Vec<LayerId> = stage.session_layer().into_iter().collect();
        while let Some(id) = queue.pop() {
            if session.insert(id)
                && let Some(layer) = self.document.store().layer(id)
            {
                queue.extend(layer.sublayers().iter().map(|sublayer| sublayer.layer));
            }
        }
        let reported = |id: LayerId| {
            report.saved.contains(&id)
                || report.anonymous.contains(&id)
                || report.failures.iter().any(|failure| failure.layer == id)
        };
        self.document
            .store()
            .layer_ids()
            .into_iter()
            .filter(|&id| {
                self.document.is_dirty(id)
                    && self.document.identifier(id).is_some()
                    && !session.contains(&id)
                    && !reported(id)
            })
            .map(|id| {
                let reason = if stage.is_layer_muted(id) {
                    "muted"
                } else {
                    "unused"
                };
                Wire::List(vec![Wire::UInt64(id.0), Wire::text(reason)])
            })
            .collect()
    }

    fn save_record(&mut self, report: &SaveReport) -> Wire {
        let mut written = self.document.storage_mut().take_written();
        let rows = report
            .saved
            .iter()
            .filter_map(|&id| {
                let identifier = self.document.identifier(id)?;
                let at = written.iter().position(|(at, _)| at == identifier)?;
                let (_, bytes) = written.swap_remove(at);
                Some(Wire::List(vec![
                    Wire::UInt64(id.0),
                    Wire::text(identifier),
                    Wire::Bytes(bytes.to_vec()),
                ]))
            })
            .collect();
        let failures = report
            .failures
            .iter()
            .map(|failure| {
                Wire::List(vec![
                    Wire::UInt64(failure.layer.0),
                    Wire::text(failure.error.code()),
                    Wire::text(failure.error.message.clone()),
                ])
            })
            .collect();
        let anonymous = report
            .anonymous
            .iter()
            .map(|id| Wire::UInt64(id.0))
            .collect();
        Wire::List(vec![
            Wire::List(rows),
            Wire::List(failures),
            Wire::List(anonymous),
        ])
    }

    /// The bytes of layer `layer` as `format` (`usda` or `usdc`), as
    /// [`StageDocument::export_layer`] writes a copy: the layer keeps its
    /// identity, binding and dirty state, and storage is not touched. Any
    /// layer exports, the session layer and package members included.
    ///
    /// # Errors
    ///
    /// `layer.unknown`; `argument.invalid` for another format; the
    /// writer's `io.unsupported` for content the format cannot hold, and
    /// `io.rejected` for a layer that cannot be written as it stands (an
    /// attribute with no type, ...).
    pub(crate) fn export_layer(&self, layer: u64, format: &str) -> Result<Vec<u8>, Error> {
        let layer = LayerId(layer);
        self.check_layer(layer)?;
        let target = export_target(format)?;
        let layer =
            self.document.store().layer(layer).ok_or_else(|| {
                Error::new("layer.unknown", "the stage has no layer with this id")
            })?;
        Ok(self.document.encode_layer(layer, target)?)
    }

    /// The composed stage flattened into one layer
    /// ([`layerstack::Stage::flatten`], OpenUSD's `UsdStage::Flatten`), as
    /// `format`: `[bytes, lossless, lost]`. Instances keep sharing their
    /// prototypes, and asset paths are anchored to the layer that authors
    /// each ([`StageDocument::flatten`]), as OpenUSD's flatten anchors
    /// them: a search path is anchored when the host supplied the asset
    /// beside its layer, and an identifier relative to the host's root is
    /// written relative to the flattened layer (`./set/albedo.png`), which
    /// resolves as the stage did when it sits at that root. A flatten that would lose something (animation a
    /// layer cannot hold, a path relative to a layer with no identifier)
    /// still exports, accepting exactly those losses, with `lossless`
    /// false and each loss in `lost` as `[path, message]`.
    ///
    /// Spec: AOUSD Core §11 (stage population), §12 (value resolution).
    ///
    /// # Errors
    ///
    /// `argument.invalid` for a format other than `usda` or `usdc`;
    /// `flatten.failed` when the document has no free layer id; the
    /// writer's `io.unsupported`.
    pub(crate) fn export_flattened(&mut self, format: &str) -> Result<Wire, Error> {
        let target = export_target(format)?;
        let requirements = FlattenRequirements::default();
        let flattened = match self.document.flatten(&requirements) {
            Err(FlattenFailure::Flatten(FlattenError::Refused(refusal))) => {
                let relaxed = requirements.accepting(&refusal.unmet);
                self.document.flatten(&relaxed)
            }
            other => other,
        }
        .map_err(|error| Error::new("flatten.failed", error.to_string()))?;
        let lost = flattened
            .report
            .lost()
            .map(|finding| {
                Wire::List(vec![
                    Wire::Text(finding.path.to_string()),
                    Wire::Text(finding.kind.to_string()),
                ])
            })
            .collect();
        let bytes = self.document.encode_layer(&flattened.layer, target)?;
        Ok(Wire::List(vec![
            Wire::Bytes(bytes),
            Wire::Bool(flattened.report.is_lossless()),
            Wire::List(lost),
        ]))
    }

    /// The root layer and everything it depends on, packaged as a USDZ
    /// ([`StageDocument::prepare_package`], OpenUSD's
    /// `UsdUtilsCreateNewUsdzPackage`): resident layers with their unsaved
    /// edits, and the other assets read from storage.
    ///
    /// The record is `[bytes, layers, assets, unresolved, omitted]`:
    /// `bytes` the package, or `null` when a dependency is unresolved (an
    /// asset the host has not supplied); the counts of layers and other
    /// assets packaged; each unresolved use as `[layer, assetPath, reason]`;
    /// and the ids of the layers the stage composes that the package
    /// leaves out: the session layer, which is not part of the root's
    /// closure (export it with `exportLayer`, or the composed stage with
    /// `exportFlattened`).
    ///
    /// Spec: AOUSD Core §16 (packages).
    ///
    /// # Errors
    ///
    /// `io.rejected` when a budget of the preparation is exceeded.
    pub(crate) fn export_package(&mut self) -> Result<Wire, Error> {
        let stage = self.document.stage().stage();
        let root = stage
            .root_layer()
            .ok_or_else(|| Error::new("package.failed", "the stage has no root layer"))?;
        let omitted: Vec<Wire> = stage
            .session_layer()
            .map(|id| Wire::UInt64(id.0))
            .into_iter()
            .collect();
        // A package read as the root keeps its root member's name; a layer
        // keeps its file name.
        let member = self
            .document
            .package_root_member(root)
            .or_else(|| self.document.identifier(root).map(package_member))
            .unwrap_or("root.usda")
            .to_string();
        let package = self.document.prepare_package(
            root,
            &member,
            PackagePreparationLimits::default(),
            |_| PackageInclusion::Include,
        )?;
        let unresolved = package
            .report()
            .dependencies
            .iter()
            .flat_map(|dependency| {
                dependency.targets.iter().filter_map(|target| match target {
                    DependencyTarget::Unresolved(reason) => Some(Wire::List(vec![
                        Wire::UInt64(dependency.source.layer.0),
                        Wire::text(&*dependency.source.identifier),
                        Wire::text(&**reason),
                    ])),
                    _ => None,
                })
            })
            .collect();
        let (bytes, layers, assets) = match package.plan() {
            Some(plan) => (
                Wire::Bytes(package.write_usdz()?),
                plan.layers.len(),
                plan.assets.len(),
            ),
            None => (Wire::Null, 0, 0),
        };
        Ok(Wire::List(vec![
            bytes,
            Wire::Number(layers as f64),
            Wire::Number(assets as f64),
            Wire::List(unresolved),
            Wire::List(omitted),
        ]))
    }

    fn check_layer(&self, layer: LayerId) -> Result<(), Error> {
        if self.document.store().layer(layer).is_none() {
            return Err(Error::new(
                "layer.unknown",
                "the stage has no layer with this id",
            ));
        }
        Ok(())
    }

    /// Whether the prim at `path` is of the typed schema `schema` or one
    /// derived from it.
    ///
    /// Spec: AOUSD Core §13.3.1. OpenUSD: `UsdPrim::IsA`.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` when no prim is at `path`.
    pub(crate) fn is_a(&self, path: &str, schema: &str) -> Result<bool, Error> {
        let prim = self.existing_prim(path)?.path();
        let scene =
            layerstack_schemas::Scene::new(self.document.stage().stage(), self.document.store());
        Ok(scene.is_a(prim, schema))
    }

    /// Whether the prim has the API schema `schema` applied (`instance`
    /// naming a multiple-apply instance), its type's built-in ones
    /// included, as `UsdPrim::HasAPI` answers.
    ///
    /// # Errors
    ///
    /// `path.invalid`, or `prim.not_found` when no prim is at `path`.
    pub(crate) fn has_api(
        &self,
        path: &str,
        schema: &str,
        instance: Option<&str>,
    ) -> Result<bool, Error> {
        let prim = self.existing_prim(path)?.path();
        let scene =
            layerstack_schemas::Scene::new(self.document.stage().stage(), self.document.store());
        Ok(scene.has_api(prim, schema, instance))
    }

    pub(crate) fn existing_prim(&self, path: &str) -> Result<layerstack::Prim<'_>, Error> {
        check_prim_path(path)?;
        self.document.view().prim(path).ok_or_else(|| {
            Error::new("prim.not_found", "no composed prim is at this path").at_path(path)
        })
    }
}

/// The JavaScript surface: each export wraps the Rust method of the same
/// name, its record and error turned into `JsValue`s.
#[wasm_bindgen]
impl Stage {
    /// Opens a stage ([`Self::open`]) within the limits
    /// `prim_index_nodes`, `nodes_per_prim_index` and `composed_depth`:
    /// each `undefined` for its
    /// [`BROWSER_LIMITS`] value, infinite for no limit, a number for that
    /// limit (truncated to an integer). `provenance` includes winning sources
    /// in native resolved values ([`Self::open_with_provenance`]). A
    /// `session_identifier` with `session_bytes` opens that session layer
    /// ([`SessionLayer::Supplied`]) in place of an anonymous one.
    /// `schemas` holds a [`TypedSchema`]'s name and base for each schema
    /// to register, one after the other.
    #[wasm_bindgen(js_name = open)]
    pub fn js_open(
        identifier: &str,
        bytes: Vec<u8>,
        recover: bool,
        session_layer: bool,
        prim_index_nodes: Option<f64>,
        nodes_per_prim_index: Option<f64>,
        composed_depth: Option<f64>,
        provenance: bool,
        session_identifier: Option<String>,
        session_bytes: Option<Vec<u8>>,
        schemas: Vec<String>,
    ) -> Result<Self, JsValue> {
        let limit = |given: Option<f64>, default: Option<usize>| match given {
            None => default,
            Some(value) if value.is_infinite() => None,
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "the wrapper passes a non-negative integer"
            )]
            Some(value) => Some(value as usize),
        };
        let limits = CompositionLimits {
            prim_index_nodes: limit(prim_index_nodes, BROWSER_LIMITS.prim_index_nodes),
            nodes_per_prim_index: limit(nodes_per_prim_index, BROWSER_LIMITS.nodes_per_prim_index),
            composed_depth: limit(composed_depth, BROWSER_LIMITS.composed_depth),
        };
        let session = match (session_identifier, session_bytes) {
            (Some(identifier), Some(bytes)) => SessionLayer::Supplied { identifier, bytes },
            _ if session_layer => SessionLayer::Anonymous,
            _ => SessionLayer::None,
        };
        let schemas: Vec<TypedSchema> = schemas
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[name, base]| TypedSchema {
                name: name.clone(),
                base: base.clone(),
            })
            .collect();
        Ok(Self::open_with_provenance(
            identifier, bytes, recover, session, limits, provenance, &schemas,
        )?)
    }

    /// Applies a batch of edits to a layer ([`Self::apply`]).
    #[wasm_bindgen(js_name = apply)]
    pub fn js_apply(&mut self, target: u64, batch: &mut EditBatch) -> Result<EditReceipt, JsValue> {
        Ok(self.apply(target, batch)?)
    }

    /// Applies a batch of edits through an edit target
    /// (`apply_target`), named by the values of its record
    /// (`target.rs`): `kind`, `layer`, `prim`, `site`, `arc`,
    /// `maps_outside`, `offset` and `scale`.
    #[wasm_bindgen(js_name = applyTarget)]
    pub fn js_apply_target(
        &mut self,
        kind: String,
        layer: u64,
        prim: Option<String>,
        site: Option<String>,
        arc: Option<String>,
        maps_outside: bool,
        offset: f64,
        scale: f64,
        batch: &mut EditBatch,
    ) -> Result<EditReceipt, JsValue> {
        let input = TargetInput {
            kind,
            layer,
            prim,
            site,
            arc,
            maps_outside,
            offset,
            scale,
        };
        Ok(self.apply_target(&input, batch)?)
    }

    /// The record of a local layer's edit target
    /// (`edit_target_for_layer`).
    #[wasm_bindgen(js_name = editTargetForLayer)]
    pub fn js_edit_target_for_layer(&self, layer: u64) -> Result<JsValue, JsValue> {
        Ok(self.edit_target_for_layer(layer)?.into())
    }

    /// Whether a layer is in the stage's layer stack
    /// (`has_local_layer`).
    #[wasm_bindgen(js_name = hasLocalLayer)]
    #[must_use]
    pub fn js_has_local_layer(&self, layer: u64) -> bool {
        self.has_local_layer(layer)
    }

    /// The record of a local variant's edit target
    /// (`variant_edit_target`).
    #[wasm_bindgen(js_name = variantEditTarget)]
    pub fn js_variant_edit_target(
        &self,
        path: &str,
        set: &str,
        variant: &str,
        layer: u64,
    ) -> Result<JsValue, JsValue> {
        Ok(self.variant_edit_target(path, set, variant, layer)?.into())
    }

    /// The record `composition_arcs` builds, as documented in `target.rs`.
    #[wasm_bindgen(js_name = compositionArcs)]
    pub fn js_composition_arcs(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.composition_arcs(path)?.into())
    }

    /// The spec path an edit target maps a stage path to
    /// (`map_edit_target`), the target named as for `apply`.
    #[wasm_bindgen(js_name = mapEditTarget)]
    pub fn js_map_edit_target(
        &self,
        kind: String,
        layer: u64,
        prim: Option<String>,
        site: Option<String>,
        arc: Option<String>,
        maps_outside: bool,
        offset: f64,
        scale: f64,
        path: &str,
    ) -> Result<Option<String>, JsValue> {
        let input = TargetInput {
            kind,
            layer,
            prim,
            site,
            arc,
            maps_outside,
            offset,
            scale,
        };
        Ok(self.map_edit_target(&input, path)?)
    }

    /// Whether the stage composes what an edit target writes
    /// (`edit_target_composed`), the target named as for `apply`.
    #[wasm_bindgen(js_name = editTargetComposed)]
    #[must_use]
    pub fn js_edit_target_composed(
        &self,
        kind: String,
        layer: u64,
        prim: Option<String>,
        site: Option<String>,
        arc: Option<String>,
        maps_outside: bool,
        offset: f64,
        scale: f64,
    ) -> bool {
        let input = TargetInput {
            kind,
            layer,
            prim,
            site,
            arc,
            maps_outside,
            offset,
            scale,
        };
        self.edit_target_composed(&input)
    }

    /// Undoes a receipt's edits ([`Self::undo`]).
    #[wasm_bindgen(js_name = undo)]
    pub fn js_undo(&mut self, receipt: &mut EditReceipt) -> Result<EditReceipt, JsValue> {
        Ok(self.undo(receipt)?)
    }

    /// Moves a prim or property ([`Self::move_object`]).
    #[wasm_bindgen(js_name = moveObject)]
    pub fn js_move_object(
        &mut self,
        target: u64,
        from: &str,
        to: &str,
    ) -> Result<EditReceipt, JsValue> {
        Ok(self.move_object(target, from, to)?)
    }

    /// Deletes a prim or property ([`Self::delete_object`]).
    #[wasm_bindgen(js_name = deleteObject)]
    pub fn js_delete_object(&mut self, target: u64, path: &str) -> Result<EditReceipt, JsValue> {
        Ok(self.delete_object(target, path)?)
    }

    /// The record `take_changes` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = takeChanges)]
    pub fn js_take_changes(&mut self, render: bool) -> JsValue {
        self.take_changes(render).into()
    }

    /// The record `metadata` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = metadata)]
    pub fn js_metadata(&self, path: &str, key: &str) -> Result<JsValue, JsValue> {
        Ok(self.metadata(path, key)?.into())
    }

    /// The record `stage_metadata` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = stageMetadata)]
    pub fn js_stage_metadata(&self, key: &str) -> Result<JsValue, JsValue> {
        Ok(self.stage_metadata(key)?.into())
    }

    /// The record `schemas` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = schemas)]
    pub fn js_schemas(&self) -> JsValue {
        self.schemas().into()
    }

    /// The record `relationship` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = relationship)]
    pub fn js_relationship(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.relationship(path)?.into())
    }

    /// The record `unavailable` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = unavailable)]
    pub fn js_unavailable(&self) -> JsValue {
        self.unavailable().into()
    }

    /// Supplies the bytes of `identifier` ([`HostStorage::supply`]).
    #[wasm_bindgen(js_name = supply)]
    pub fn js_supply(&mut self, identifier: &str, bytes: Vec<u8>) {
        self.document.storage_mut().supply(identifier, bytes);
    }

    /// Confirms `identifier` absent ([`HostStorage::confirm_absent`]).
    #[wasm_bindgen(js_name = confirmAbsent)]
    pub fn js_confirm_absent(&mut self, identifier: &str) {
        self.document.storage_mut().confirm_absent(identifier);
    }

    /// The record `load_supplied` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = loadSupplied)]
    pub fn js_load_supplied(&mut self) -> Result<JsValue, JsValue> {
        Ok(self.load_supplied()?.into())
    }

    /// The record `read_supplied` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = readSupplied)]
    pub fn js_read_supplied(&mut self) -> Result<JsValue, JsValue> {
        Ok(self.read_supplied()?.into())
    }

    /// The record `prepare_clip_query` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = prepareClipQuery)]
    pub fn js_prepare_clip_query(
        &mut self,
        path: &str,
        time: Option<f64>,
        max_loads: u32,
        max_demands: u32,
    ) -> Result<JsValue, JsValue> {
        Ok(self
            .prepare_clip_query(path, time, max_loads as usize, max_demands as usize)?
            .into())
    }

    /// The record `layers` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = layers)]
    pub fn js_layers(&self) -> JsValue {
        self.layers().into()
    }

    /// The record `layer_dirty` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = layerDirty)]
    pub fn js_layer_dirty(&self, id: u64) -> Result<bool, JsValue> {
        Ok(self.layer_dirty(id)?)
    }

    /// Mutes or unmutes a layer (`set_layer_muted`, as documented in `stage.rs`).
    #[wasm_bindgen(js_name = setLayerMuted)]
    pub fn js_set_layer_muted(&mut self, id: u64, muted: bool) -> Result<bool, JsValue> {
        Ok(self.set_layer_muted(id, muted)?)
    }

    /// Mutes the layers of `mute_ids` and `mute_identifiers`, then unmutes
    /// those of `unmute_ids` and `unmute_identifiers`, as one request
    /// (`mute_and_unmute_layers`, as documented in `stage.rs`).
    #[wasm_bindgen(js_name = muteAndUnmuteLayers)]
    pub fn js_mute_and_unmute_layers(
        &mut self,
        mute_ids: Vec<u64>,
        mute_identifiers: Vec<String>,
        unmute_ids: Vec<u64>,
        unmute_identifiers: Vec<String>,
    ) -> Result<bool, JsValue> {
        let mute = MuteRequest::list(&mute_ids, &mute_identifiers);
        let unmute = MuteRequest::list(&unmute_ids, &unmute_identifiers);
        Ok(self.mute_and_unmute_layers(&mute, &unmute)?)
    }

    /// Whether an identifier's layer is muted (`is_layer_muted_named`, as documented in `stage.rs`).
    #[wasm_bindgen(js_name = isLayerMuted)]
    pub fn js_is_layer_muted(&self, identifier: &str) -> Result<bool, JsValue> {
        Ok(self.is_layer_muted_named(identifier)?)
    }

    /// The record `muted_layers` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = mutedLayers)]
    pub fn js_muted_layers(&self) -> JsValue {
        self.muted_layers().into()
    }

    /// The record `status` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = status)]
    pub fn js_status(&self) -> JsValue {
        self.status().into()
    }

    /// The record `prim` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = prim)]
    pub fn js_prim(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.prim(path)?.into())
    }

    /// The record `children` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = children)]
    pub fn js_children(&self, path: &str) -> Result<Vec<String>, JsValue> {
        Ok(self.children(path)?)
    }

    /// The record `attribute_names` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = attributeNames)]
    pub fn js_attribute_names(&self, path: &str) -> Result<Vec<String>, JsValue> {
        Ok(self.attribute_names(path)?)
    }

    /// The record `relationship_names` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = relationshipNames)]
    pub fn js_relationship_names(&self, path: &str) -> Result<Vec<String>, JsValue> {
        Ok(self.relationship_names(path)?)
    }

    /// The record `variant_sets` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = variantSets)]
    pub fn js_variant_sets(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.variant_sets(path)?.into())
    }

    /// The record `attribute` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = attribute)]
    pub fn js_attribute(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.attribute(path)?.into())
    }

    /// The record `get` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = get)]
    pub fn js_get(&self, path: &str, time: Option<f64>) -> Result<JsValue, JsValue> {
        Ok(self.get(path, time)?.into())
    }

    /// Whether the prim is of a typed schema (`is_a`), as documented in
    /// `stage.rs`.
    #[wasm_bindgen(js_name = isA)]
    pub fn js_is_a(&self, path: &str, schema: &str) -> Result<bool, JsValue> {
        Ok(self.is_a(path, schema)?)
    }

    /// Whether the prim has an API schema applied (`has_api`), as
    /// documented in `stage.rs`.
    #[wasm_bindgen(js_name = hasAPI)]
    pub fn js_has_api(
        &self,
        path: &str,
        schema: &str,
        instance: Option<String>,
    ) -> Result<bool, JsValue> {
        Ok(self.has_api(path, schema, instance.as_deref())?)
    }

    /// The record `time_samples` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = timeSamples)]
    pub fn js_time_samples(&self, path: &str) -> Result<Vec<f64>, JsValue> {
        Ok(self.time_samples(path)?)
    }

    /// The flags `values_might_vary` reads, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = valuesMightVary)]
    pub fn js_values_might_vary(&self, paths: Vec<String>) -> Result<Vec<u8>, JsValue> {
        Ok(self.values_might_vary(&paths)?)
    }

    /// The flags `value_sources_might_vary` reads, as documented in
    /// `stage.rs`.
    #[wasm_bindgen(js_name = valueSourcesMightVary)]
    pub fn js_value_sources_might_vary(&self, paths: Vec<String>) -> Result<Vec<u8>, JsValue> {
        Ok(self.value_sources_might_vary(&paths)?)
    }

    /// The record `explain` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = explain)]
    pub fn js_explain(&self, path: &str, time: Option<f64>) -> Result<JsValue, JsValue> {
        Ok(self.explain(path, time)?.into())
    }

    /// The record `save` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = save)]
    pub fn js_save(&mut self, layer: Option<u64>) -> Result<JsValue, JsValue> {
        Ok(self.save(layer)?.into())
    }

    /// The bytes `export_layer` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = exportLayer)]
    pub fn js_export_layer(&self, layer: u64, format: &str) -> Result<Vec<u8>, JsValue> {
        Ok(self.export_layer(layer, format)?)
    }

    /// The record `export_flattened` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = exportFlattened)]
    pub fn js_export_flattened(&mut self, format: &str) -> Result<JsValue, JsValue> {
        Ok(self.export_flattened(format)?.into())
    }

    /// The record `export_package` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = exportPackage)]
    pub fn js_export_package(&mut self) -> Result<JsValue, JsValue> {
        Ok(self.export_package()?.into())
    }

    /// The record `collection_membership` builds, as documented in
    /// `schemas.rs`.
    #[wasm_bindgen(js_name = collectionMembership)]
    pub fn js_collection_membership(
        &self,
        path: &str,
        name: &str,
        paths: Vec<String>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.collection_membership(path, name, &paths)?.into())
    }

    /// The record `bound_materials` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = boundMaterials)]
    pub fn js_bound_materials(
        &self,
        paths: Vec<String>,
        purpose: &str,
    ) -> Result<JsValue, JsValue> {
        Ok(self.bound_materials(&paths, purpose)?.into())
    }

    /// The record `material_subsets` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = materialSubsets)]
    pub fn js_material_subsets(
        &self,
        path: &str,
        purpose: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.material_subsets(path, purpose, time)?.into())
    }

    /// The record `material` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = material)]
    pub fn js_material(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.material(path)?.into())
    }

    /// The record `material_terminal` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = materialTerminal)]
    pub fn js_material_terminal(
        &self,
        path: &str,
        terminal: &str,
        contexts: Vec<String>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.material_terminal(path, terminal, &contexts)?.into())
    }

    /// The record `shader` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = shader)]
    pub fn js_shader(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.shader(path)?.into())
    }

    /// The record `ports` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = ports)]
    pub fn js_ports(&self, path: &str, kind: &str) -> Result<JsValue, JsValue> {
        Ok(self.ports(path, kind)?.into())
    }

    /// The record `port` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = port)]
    pub fn js_port(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.port(path)?.into())
    }

    /// The record `connection_source_info` builds, as documented in
    /// `schemas.rs`.
    #[wasm_bindgen(js_name = connectionSourceInfo)]
    pub fn js_connection_source_info(&self, path: &str) -> JsValue {
        self.connection_source_info(path).into()
    }

    /// The record `value_sources` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = valueSources)]
    pub fn js_value_sources(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.value_sources(path)?.into())
    }

    /// The record `geom_subsets` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = geomSubsets)]
    pub fn js_geom_subsets(
        &self,
        path: &str,
        element: Option<String>,
        family: Option<String>,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self
            .geom_subsets(path, element.as_deref(), family.as_deref(), time)?
            .into())
    }

    /// The record `mesh_topology` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = meshTopology)]
    pub fn js_mesh_topology(&self, path: &str, time: Option<f64>) -> Result<JsValue, JsValue> {
        Ok(self.mesh_topology(path, time)?.into())
    }

    /// The record `subset_family_type` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = subsetFamilyType)]
    pub fn js_subset_family_type(&self, path: &str, family: &str) -> Result<String, JsValue> {
        Ok(self.subset_family_type(path, family)?)
    }

    /// The record `validate_subset_family` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = validateSubsetFamily)]
    pub fn js_validate_subset_family(
        &self,
        path: &str,
        element: &str,
        family: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self
            .validate_subset_family(path, element, family, time)?
            .into())
    }

    /// The record `coord_sys_bindings` builds, as documented in `schemas.rs`.
    #[wasm_bindgen(js_name = coordSysBindings)]
    pub fn js_coord_sys_bindings(&self, path: &str, inherited: bool) -> Result<JsValue, JsValue> {
        Ok(self.coord_sys_bindings(path, inherited)?.into())
    }

    /// The record `skinning_cache` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = skinningCache)]
    pub fn js_skinning_cache(&self) -> JsValue {
        self.skinning_cache().into()
    }

    /// The record `skinning` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = skinning)]
    pub fn js_skinning(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.skinning(path)?.into())
    }

    /// The record `skinning_influences` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = skinningInfluences)]
    pub fn js_skinning_influences(
        &self,
        path: &str,
        time: Option<f64>,
        interleaved: bool,
    ) -> Result<JsValue, JsValue> {
        Ok(self.skinning_influences(path, time, interleaved)?.into())
    }

    /// The record `skinning_transforms` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = skinningTransforms)]
    pub fn js_skinning_transforms(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.skinning_transforms(path, time)?.into())
    }

    /// The record `blend_shaped_points` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = blendShapedPoints)]
    pub fn js_blend_shaped_points(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.blend_shaped_points(path, time)?.into())
    }

    /// The record `blend_shaped_normals` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = blendShapedNormals)]
    pub fn js_blend_shaped_normals(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.blend_shaped_normals(path, time)?.into())
    }

    /// The record `deformed_points` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = deformedPoints)]
    pub fn js_deformed_points(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.deformed_points(path, time)?.into())
    }

    /// The record `deformed_normals` builds, as documented in `skel.rs`.
    #[wasm_bindgen(js_name = deformedNormals)]
    pub fn js_deformed_normals(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.deformed_normals(path, time)?.into())
    }

    /// The record `transform_cache` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = transformCache)]
    pub fn js_transform_cache(&self) -> JsValue {
        self.transform_cache().into()
    }

    /// The record `bounds_cache` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = boundsCache)]
    pub fn js_bounds_cache(&self) -> JsValue {
        self.bounds_cache().into()
    }

    /// The record `camera` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = camera)]
    pub fn js_camera(
        &mut self,
        path: &str,
        time: Option<f64>,
        policy: Option<String>,
        aspect: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.camera(path, time, policy.as_deref(), aspect)?.into())
    }

    /// The record `local_transform` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = localTransform)]
    pub fn js_local_transform(
        &mut self,
        path: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.local_transform(path, time)?.into())
    }

    /// The matrices `world_transforms` computes, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = worldTransforms)]
    pub fn js_world_transforms(
        &mut self,
        paths: Vec<String>,
        time: Option<f64>,
        parent: bool,
    ) -> Result<Vec<f64>, JsValue> {
        Ok(self.world_transforms(&paths, time, parent)?)
    }

    /// The flags `world_transforms_might_vary` reads, as documented in
    /// `geom.rs`.
    #[wasm_bindgen(js_name = worldTransformsMightVary)]
    pub fn js_world_transforms_might_vary(
        &mut self,
        paths: Vec<String>,
        time: Option<f64>,
    ) -> Result<Vec<u8>, JsValue> {
        Ok(self.world_transforms_might_vary(&paths, time)?)
    }

    /// The record `xformable` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = xformable)]
    pub fn js_xformable(&self, path: &str) -> Result<JsValue, JsValue> {
        Ok(self.xformable(path)?.into())
    }

    /// The record `common_transform` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = commonTransform)]
    pub fn js_common_transform(&self, path: &str, time: Option<f64>) -> Result<JsValue, JsValue> {
        Ok(self.common_transform(path, time)?.into())
    }

    /// The record `visibilities` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = visibilities)]
    pub fn js_visibilities(
        &self,
        paths: Vec<String>,
        time: Option<f64>,
        purpose: Option<String>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.visibilities(&paths, time, purpose.as_deref())?.into())
    }

    /// The record `purposes` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = purposes)]
    pub fn js_purposes(&self, paths: Vec<String>) -> Result<JsValue, JsValue> {
        Ok(self.purposes(&paths)?.into())
    }

    /// The record `primvars` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = primvars)]
    pub fn js_primvars(&self, path: &str, inherited: bool) -> Result<JsValue, JsValue> {
        Ok(self.primvars(path, inherited)?.into())
    }

    /// The record `primvar` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = primvar)]
    pub fn js_primvar(&self, path: &str, name: &str, inherited: bool) -> Result<JsValue, JsValue> {
        Ok(self.primvar(path, name, inherited)?.into())
    }

    /// The record `primvar_value` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = primvarValue)]
    pub fn js_primvar_value(
        &self,
        path: &str,
        name: &str,
        time: Option<f64>,
        flattened: bool,
    ) -> Result<JsValue, JsValue> {
        Ok(self.primvar_value(path, name, time, flattened)?.into())
    }

    /// The record `primvar_indices` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = primvarIndices)]
    pub fn js_primvar_indices(
        &self,
        path: &str,
        name: &str,
        time: Option<f64>,
    ) -> Result<JsValue, JsValue> {
        Ok(self.primvar_indices(path, name, time)?.into())
    }

    /// The record `bounds` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = bounds)]
    pub fn js_bounds(
        &mut self,
        paths: Vec<String>,
        time: Option<f64>,
        purposes: Vec<String>,
        hints: bool,
        ignore_visibility: bool,
        space: &str,
    ) -> Result<JsValue, JsValue> {
        Ok(self
            .bounds(&paths, time, &purposes, hints, ignore_visibility, space)?
            .into())
    }

    /// The extents `extents_hint` computes, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = extentsHint)]
    pub fn js_extents_hint(
        &mut self,
        path: &str,
        time: Option<f64>,
        purposes: Vec<String>,
        hints: bool,
        ignore_visibility: bool,
    ) -> Result<Vec<f64>, JsValue> {
        Ok(self.extents_hint(path, time, &purposes, hints, ignore_visibility)?)
    }

    /// The record `instance_transforms` builds, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = instanceTransforms)]
    pub fn js_instance_transforms(
        &self,
        path: &str,
        time: Option<f64>,
        base_time: Option<f64>,
        include_prototype_transform: bool,
        apply_mask: bool,
    ) -> Result<JsValue, JsValue> {
        Ok(self
            .instance_transforms(
                path,
                time,
                base_time,
                include_prototype_transform,
                apply_mask,
            )?
            .into())
    }

    /// The record `prototypes` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = prototypes)]
    pub fn js_prototypes(&self) -> JsValue {
        self.prototypes().into()
    }

    /// The mask `instance_mask` computes, as documented in `geom.rs`.
    #[wasm_bindgen(js_name = instanceMask)]
    pub fn js_instance_mask(&self, path: &str, time: Option<f64>) -> Result<Vec<u8>, JsValue> {
        Ok(self.instance_mask(path, time)?)
    }

    /// The record `supplied_bytes` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = suppliedBytes)]
    pub fn js_supplied_bytes(&self) -> u64 {
        self.supplied_bytes()
    }
}

/// `[created, removed, resynced, changedInfoOnly, properties,
/// resyncedProperties]`: the prim paths of `changes` as text, its
/// property-field inventories ([`Changes::property_changes`]) as
/// `[prim, fields]`, each field `[property, field, metadata]` (`field`
/// `default`, `animation`, `timeSamples`, `targets`, `metadata` with the
/// metadata field's name, or `other`), and the property paths of its
/// property resyncs ([`Changes::resynced_properties`]) as text.
fn changes_record(changes: &Changes, store: &InMemoryStore) -> Wire {
    let tokens = store.tokens();
    let path = |id: PathId| Wire::Text(store.paths().display(id, tokens));
    let paths = |ids: &[PathId]| Wire::List(ids.iter().map(|&id| path(id)).collect());
    let properties = changes
        .property_changes
        .iter()
        .map(|prim| {
            let fields = prim
                .fields
                .iter()
                .map(|change| {
                    let (field, metadata) = match change.field {
                        PropertyField::Default => ("default", None),
                        PropertyField::Animation => ("animation", None),
                        PropertyField::TimeSamples => ("timeSamples", None),
                        PropertyField::Targets => ("targets", None),
                        PropertyField::Metadata(key) => ("metadata", Some(key)),
                        _ => ("other", None),
                    };
                    Wire::List(vec![
                        Wire::text(tokens.resolve(change.name)),
                        Wire::text(field),
                        metadata.map_or(Wire::Null, |key| Wire::text(tokens.resolve(key))),
                    ])
                })
                .collect();
            Wire::List(vec![path(prim.prim), Wire::List(fields)])
        })
        .collect();
    Wire::List(vec![
        paths(&changes.created),
        paths(&changes.removed),
        paths(&changes.resynced),
        paths(&changes.changed_info_only),
        Wire::List(properties),
        Wire::List(
            changes
                .resynced_properties
                .iter()
                .map(|property| Wire::Text(property.display(store.paths(), tokens)))
                .collect(),
        ),
    ])
}

/// The error of a mute request the live stage refuses: `mute.root_layer`.
fn mute_error(error: LayerMuteError) -> Error {
    match error {
        LayerMuteError::RootLayer => {
            Error::new("mute.root_layer", "the root layer cannot be muted")
        }
        _ => Error::new("mute.refused", error.to_string()),
    }
}

/// A layer a mute or unmute request names: by id, or by identifier.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MuteRequest<'a> {
    /// The layer with this id, which the stage holds.
    Layer(u64),
    /// The layer this identifier names, anchored to the root layer.
    Identifier(&'a str),
}

impl<'a> MuteRequest<'a> {
    /// The requests naming the layers `ids`, then those `identifiers` name.
    fn list(ids: &[u64], identifiers: &'a [String]) -> Vec<Self> {
        ids.iter()
            .map(|&id| Self::Layer(id))
            .chain(
                identifiers
                    .iter()
                    .map(|identifier| Self::Identifier(identifier)),
            )
            .collect()
    }
}

/// `[identifier, assetPath, anchor, usedBy, prims]` for an unavailable
/// asset, as [`Stage::unavailable`] lists it.
/// A path a namespace edit leaves naming the old location
/// ([`UnrepairedPath`]): `[layer, spec, field, path, key]`, the id of the
/// layer authoring it, the spec holding it where the edit leaves it
/// (`/World/Room{light=off}.lamp`), the field (`references`, `payloads`,
/// `inherits`, `specializes`, `targets`, `connections`, `default`,
/// `timeSamples` or `metadata`), the path it names, and the metadata
/// field's name for `metadata` (`null` otherwise).
fn unrepaired_row(left: &UnrepairedPath, store: &InMemoryStore) -> Wire {
    let (field, key) = match left.field {
        PathField::References => ("references", None),
        PathField::Payloads => ("payloads", None),
        PathField::Inherits => ("inherits", None),
        PathField::Specializes => ("specializes", None),
        PathField::Targets => ("targets", None),
        PathField::Connections => ("connections", None),
        PathField::Default => ("default", None),
        PathField::TimeSamples => ("timeSamples", None),
        PathField::Metadata(key) => ("metadata", Some(store.tokens().resolve(key).to_string())),
        _ => ("metadata", None),
    };
    Wire::List(vec![
        Wire::UInt64(left.layer.0),
        Wire::text(left.spec.display(store.paths(), store.tokens())),
        Wire::text(field),
        Wire::text(left.path.display(store.paths(), store.tokens())),
        Wire::text_or_null(key),
    ])
}

fn unavailable_row(asset: UnavailableAsset, store: &InMemoryStore) -> Wire {
    let prims = asset
        .prims
        .iter()
        .map(|&prim| Wire::Text(store.paths().display(prim, store.tokens())))
        .collect();
    Wire::List(vec![
        Wire::Text(asset.identifier),
        Wire::Text(asset.asset_path),
        Wire::UInt64(asset.anchor.0),
        Wire::text(asset.used_by.name()),
        Wire::List(prims),
    ])
}

/// The name of a clip asset's role.
fn clip_role(role: ClipAssetRole) -> &'static str {
    match role {
        ClipAssetRole::Value => "value",
        ClipAssetRole::Manifest => "manifest",
        ClipAssetRole::TemplateCandidate => "templateCandidate",
    }
}

/// The name of why a clip query needs an asset; `""` for a reason this
/// build does not name.
fn clip_demand_reason(reason: ClipDemandReason) -> &'static str {
    match reason {
        ClipDemandReason::Manifest => "manifest",
        ClipDemandReason::AutomaticManifest => "automaticManifest",
        ClipDemandReason::ActiveInterval => "activeInterval",
        ClipDemandReason::LowerInterpolationBound => "lowerInterpolationBound",
        ClipDemandReason::UpperInterpolationBound => "upperInterpolationBound",
        ClipDemandReason::TemplateSchedule => "templateSchedule",
        ClipDemandReason::MissingValueSearch => "missingValueSearch",
        ClipDemandReason::StrongerSourceComposition => "strongerSourceComposition",
        ClipDemandReason::SingleClipSamples => "singleClipSamples",
        _ => "",
    }
}

/// The code of a clip set's preparation issue; `""` for a kind this build
/// does not name.
fn clip_issue_code(kind: &ClipIssueKind) -> &'static str {
    match kind {
        ClipIssueKind::InvalidMetadata(_) => "invalidMetadata",
        ClipIssueKind::InvalidTemplate => "invalidTemplate",
        ClipIssueKind::UnresolvedTemplate => "unresolvedTemplate",
        ClipIssueKind::UnsupportedSpline => "unsupportedSpline",
        ClipIssueKind::InvalidSchedule => "invalidSchedule",
        ClipIssueKind::AssetExpression { .. } => "assetExpression",
        _ => "",
    }
}

/// The code of a clip evaluation error; `""` for one this build does not
/// name.
fn clip_eval_code(error: ClipEvalError) -> &'static str {
    match error {
        ClipEvalError::EmptySchedule => "emptySchedule",
        ClipEvalError::InvalidActivation => "invalidActivation",
        ClipEvalError::InvalidTimeMapping => "invalidTimeMapping",
        ClipEvalError::InvalidSamples => "invalidSamples",
        ClipEvalError::InvalidQuery => "invalidQuery",
        ClipEvalError::UnsupportedSpline => "unsupportedSpline",
        ClipEvalError::InvalidSpline => "invalidSpline",
        _ => "",
    }
}

/// `[loaded, failures]` for a load's report.
fn load_result(report: &LoadReport) -> Wire {
    let loaded = report
        .layers
        .iter()
        .map(|layer| Wire::UInt64(layer.0))
        .collect();
    let failures = report
        .asset_failures
        .iter()
        .map(|failure| {
            Wire::List(vec![
                Wire::text(failure.asset_path.clone()),
                failure.anchor.map_or(Wire::Null, |id| Wire::UInt64(id.0)),
                Wire::text(failure.error.code()),
                Wire::text(failure.error.message.clone()),
            ])
        })
        .collect();
    Wire::List(vec![Wire::List(loaded), Wire::List(failures)])
}

/// `[reads, compositions]` for a load's report, times in milliseconds on
/// the stage's clock (`performance.now()` in a JavaScript host,
/// [`crate::clock`]), `null` without one:
///
/// - each read `[layer, bytes, prims, start, end, own]`
///   ([`layerstack_io::LayerRead`]): the layer's id, the encoded bytes and
///   prim specs, its span, nested reads included, and its own time;
/// - each composition `[layers, start, end]`
///   ([`layerstack_io::Composition`]).
fn load_work(report: &LoadReport, document: &StageDocument<HostStorage>) -> [Wire; 2] {
    let time = |reading| Wire::Number(crate::clock::millis(reading));
    let reads = report
        .reads
        .iter()
        .map(|read| {
            let (start, end, own) = read.time.map_or((Wire::Null, Wire::Null, Wire::Null), |t| {
                (time(t.span.start), time(t.span.end), time(t.own))
            });
            Wire::List(vec![
                Wire::UInt64(read.layer.0),
                Wire::text_or_null(document.identifier(read.layer)),
                Wire::Number(read.bytes as f64),
                Wire::Number(read.prims as f64),
                start,
                end,
                own,
            ])
        })
        .collect();
    let compositions = report
        .compositions
        .iter()
        .map(|composition| {
            let (start, end) = composition
                .time
                .map_or((Wire::Null, Wire::Null), |t| (time(t.start), time(t.end)));
            Wire::List(vec![Wire::Number(composition.layers as f64), start, end])
        })
        .collect();
    [Wire::List(reads), Wire::List(compositions)]
}

/// The time a query reads: the default time, or a time code interpolated
/// linearly (OpenUSD's default).
pub(crate) fn time_of(time: Option<f64>) -> Time {
    time.map_or(Time::Default, Time::at)
}

/// An arc kind as the wrapper spells it: the scene description field's
/// name, `local` for a local opinion.
/// The name of an arc kind, as records give it.
pub(crate) fn arc_name(arc: ArcKind) -> &'static str {
    match arc {
        ArcKind::Local => "local",
        ArcKind::Inherits => "inherits",
        ArcKind::Variants => "variants",
        ArcKind::Relocates => "relocates",
        ArcKind::References => "references",
        ArcKind::Payloads => "payloads",
        ArcKind::Specializes => "specializes",
    }
}

/// The format of a layer by its identifier's extension, a package
/// member's own (`scene.usdz[root.usdc]` is `usdc`).
fn format_of(identifier: &str) -> &str {
    let name = match identifier.strip_suffix(']') {
        Some(rest) => rest.rsplit_once('[').map_or(rest, |(_, member)| member),
        None => identifier,
    };
    let file = name.rsplit_once('/').map_or(name, |(_, file)| file);
    match file.rsplit_once('.') {
        Some((_, extension)) => extension,
        None => "",
    }
}

/// The target whose extension picks `format` for
/// [`StageDocument::encode_layer`]; it names nothing in storage.
fn export_target(format: &str) -> Result<&'static str, Error> {
    match format {
        "usda" => Ok("export.usda"),
        "usdc" => Ok("export.usdc"),
        other => Err(Error::new(
            "argument.invalid",
            format!("`{other}` is not a layer format: usda or usdc"),
        )),
    }
}

/// The member name a package gives the root layer read from
/// `identifier`: its file name, or the innermost package member's.
fn package_member(identifier: &str) -> &str {
    let name = identifier.trim_end_matches(']');
    let name = name.rsplit_once('[').map_or(name, |(_, member)| member);
    name.rsplit_once('/').map_or(name, |(_, file)| file)
}

/// The stable code of a namespace edit that cannot be prepared:
///
/// - `namespace.invalid_paths`: not two distinct non-root prims or two
///   distinct properties.
/// - `namespace.not_found`: nothing is composed or authored at the source.
/// - `namespace.collision`: the destination is occupied.
/// - `namespace.missing_parent`: the destination's parent does not exist.
/// - `namespace.cycle`: a prim cannot move beneath itself.
/// - `namespace.instance_proxy`: the object is inside an instance; edit
///   the instance's prototype source instead.
/// - `namespace.unmappable`: the edit target does not map both paths.
/// - `namespace.split_opinions`: the move needs edits in a layer outside
///   the target.
/// - `namespace.unsupported`: a composition case the editor cannot yet
///   change without changing its meaning.
fn namespace_code(error: &NamespaceError) -> &'static str {
    match error {
        NamespaceError::InvalidPaths => "namespace.invalid_paths",
        NamespaceError::NoSuchObject => "namespace.not_found",
        NamespaceError::Collision => "namespace.collision",
        NamespaceError::MissingParent => "namespace.missing_parent",
        NamespaceError::Cycle => "namespace.cycle",
        NamespaceError::InstanceProxy => "namespace.instance_proxy",
        NamespaceError::Unmappable => "namespace.unmappable",
        NamespaceError::SplitOpinions { .. } => "namespace.split_opinions",
        NamespaceError::ReadOnlyLayer { .. } => "layer.read_only",
        _ => "namespace.unsupported",
    }
}

/// `error` as the bindings throw it, its layer and spec written with
/// `store`, and the identifier of the read-only layer a namespace edit
/// would change.
fn namespace_error(error: &NamespaceError, store: &dyn LayerStore, path: &str) -> Error {
    let identifier = match error {
        NamespaceError::ReadOnlyLayer { layer, .. } => {
            store.layer_identifier(*layer).map(ToString::to_string)
        }
        _ => None,
    };
    Error {
        identifier,
        ..Error::new(namespace_code(error), error.display(store).to_string()).at_path(path)
    }
}

/// `path` is an absolute prim path: `/`, or `/` and names separated by
/// `/`, none empty and none holding a `.`.
pub(crate) fn check_prim_path(path: &str) -> Result<(), Error> {
    let valid = path == "/"
        || path.strip_prefix('/').is_some_and(|rest| {
            rest.split('/')
                .all(|name| !name.is_empty() && !name.contains(['.', '[', ']']))
        });
    if valid {
        Ok(())
    } else {
        Err(Error::new(
            "path.invalid",
            "not an absolute prim path, such as /World/Rock",
        )
        .at_path(path))
    }
}

/// `path` is a property path: an absolute prim path (not `/`), a `.` and
/// a non-empty name.
pub(crate) fn check_property_path(path: &str) -> Result<(), Error> {
    let valid = path.rsplit_once('.').is_some_and(|(prim, name)| {
        prim != "/" && !name.is_empty() && !name.contains('/') && check_prim_path(prim).is_ok()
    });
    if valid {
        Ok(())
    } else {
        Err(Error::new(
            "path.invalid",
            "not a property path, such as /World/Rock.size",
        )
        .at_path(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r#"#usda 1.0
(
    subLayers = [@./sub.usda@]
)
class "Base" {
    double size = 2
}
def Sphere "Ball" (
    inherits = </Base>
    prepend references = @./parts/ball.usda@</Ball>
) {
}
"#;
    const SUB: &str = r#"#usda 1.0
over "Ball" {
    int64 serial = 9007199254740993
}
"#;
    const BALL: &str = r#"#usda 1.0
def "Ball" {
    point3f[] points = [(0, 0, 0), (1, 2, 3)]
}
"#;

    fn identifiers(wanted: &Wire) -> Vec<&str> {
        wanted
            .as_list()
            .unwrap()
            .iter()
            .map(|need| need.as_list().unwrap()[0].as_text().unwrap())
            .collect()
    }

    /// A typed schema registered at open is its base to `IsA`, with the
    /// base's properties; a name not an identifier or registered already,
    /// and a base that is not a typed schema, are refused.
    #[test]
    fn typed_schemas_registered_at_open_are_their_base() {
        let open = |schemas: &[(&str, &str)]| {
            let schemas: Vec<TypedSchema> = schemas
                .iter()
                .map(|(name, base)| TypedSchema {
                    name: (*name).into(),
                    base: (*base).into(),
                })
                .collect();
            Stage::open_with_provenance(
                "scene.usda",
                b"#usda 1.0\ndef RockMesh \"Rock\" {\n}\ndef Boulder \"Boulder\" {\n}\n".to_vec(),
                false,
                SessionLayer::None,
                BROWSER_LIMITS,
                false,
                &schemas,
            )
        };
        let stage = open(&[("RockMesh", "Mesh"), ("Boulder", "RockMesh")]).unwrap();
        for path in ["/Rock", "/Boulder"] {
            assert!(stage.is_a(path, "Mesh").unwrap(), "{path}");
            assert!(stage.is_a(path, "Gprim").unwrap(), "{path}");
            assert!(!stage.is_a(path, "Sphere").unwrap(), "{path}");
        }
        assert!(stage.is_a("/Boulder", "RockMesh").unwrap());
        assert!(!stage.is_a("/Rock", "Boulder").unwrap());
        // The base's properties, with their fallbacks.
        let scheme = stage
            .document()
            .view()
            .attribute("/Rock.subdivisionScheme")
            .expect("Mesh defines it");
        assert!(
            scheme
                .read(Time::Default, |value| Some(value.clone()))
                .is_some()
        );
        assert_eq!(
            stage.is_a("/Gone", "Mesh").unwrap_err().code,
            "prim.not_found"
        );
        let plain = open(&[]).unwrap();
        assert!(!plain.is_a("/Rock", "Mesh").unwrap());
        for refused in [
            &[("Mesh", "Gprim")][..],
            &[("1Rock", "Mesh")],
            &[("Rock", "CollectionAPI")],
            &[("Rock", "Missing")],
            &[("Rock", "Mesh"), ("Rock", "Sphere")],
        ] {
            let error = open(refused).map(|_| ()).unwrap_err();
            assert_eq!(error.code, "argument.invalid", "{refused:?}");
        }
    }

    #[test]
    fn native_resolution_provenance_is_explicit_and_explanations_remain_available() {
        for provenance in [false, true] {
            let stage = Stage::open_with_provenance(
                "scene.usda",
                ROOT.as_bytes().to_vec(),
                false,
                SessionLayer::None,
                BROWSER_LIMITS,
                provenance,
                &[],
            )
            .unwrap();
            let document = stage.document();
            assert_eq!(document.stage().options().with_provenance, provenance);
            let attribute = document.view().attribute("/Ball.size").unwrap();
            let resolved = attribute
                .read(Time::Default, |value| Some(value.clone()))
                .unwrap();
            assert_eq!(resolved.value, layerstack::Value::Double(2.0));
            assert_eq!(resolved.provenance.is_some(), provenance);
            if let Some(source) = resolved.provenance {
                assert_eq!(source.layer, document.root_layer());
                assert_eq!(
                    source
                        .spec_path
                        .display(document.store().paths(), document.store().tokens()),
                    "/Base.size"
                );
                assert_eq!(document.store().tokens().resolve(source.field), "size");
            }
            assert!(attribute.explain(Time::Default).is_some());
        }
        let default = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        assert!(!default.document().stage().options().with_provenance);
    }

    #[test]
    fn opening_with_only_the_root_wants_its_dependencies() {
        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        assert_eq!(
            identifiers(&stage.unavailable()),
            ["parts/ball.usda", "sub.usda"]
        );
        stage
            .document_mut()
            .storage_mut()
            .supply("sub.usda", SUB.as_bytes().to_vec());
        stage
            .document_mut()
            .storage_mut()
            .supply("parts/ball.usda", BALL.as_bytes().to_vec());
        let loaded = stage.load_supplied().unwrap();
        let loaded = loaded.as_list().unwrap();
        assert_eq!(loaded[0].as_list().unwrap().len(), 2);
        // Each read: its layer, bytes and prims, and its time on the
        // stage's clock; then the composition after them.
        let reads = loaded[2].as_list().unwrap();
        let mut bytes: Vec<f64> = reads
            .iter()
            .map(|read| {
                let read = read.as_list().unwrap();
                assert!(loaded[0].as_list().unwrap().contains(&read[0]));
                if let [
                    _,
                    Wire::Text(_),
                    _,
                    _,
                    Wire::Number(start),
                    Wire::Number(end),
                    Wire::Number(own),
                ] = read
                {
                    // Within a nanosecond: the readings are converted apart.
                    assert!(start <= end && *own <= end - start + 1e-6);
                } else {
                    assert!(read[4..].iter().all(|time| *time == Wire::Null), "{read:?}");
                }
                match read[2] {
                    Wire::Number(bytes) => bytes,
                    _ => panic!("bytes are a number"),
                }
            })
            .collect();
        bytes.sort_by(f64::total_cmp);
        let mut expected = [SUB.len() as f64, BALL.len() as f64];
        expected.sort_by(f64::total_cmp);
        assert_eq!(bytes, expected);
        let compositions = loaded[3].as_list().unwrap();
        assert_eq!(compositions[0].as_list().unwrap()[0], Wire::Number(2.0));
        assert!(stage.unavailable().as_list().unwrap().is_empty());
        assert_eq!(
            stage.get("/Ball.serial", None).unwrap(),
            Wire::List(vec![
                Wire::text("int64"),
                Wire::text("s"),
                Wire::Int64(9_007_199_254_740_993)
            ])
        );
        let status = stage.status();
        let status = status.as_list().unwrap();
        assert_eq!(status[1], Wire::List(Vec::new()), "no composition errors");
    }

    #[test]
    fn explain_names_the_inherit_that_contributes() {
        let stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let why = stage.explain("/Ball.size", None).unwrap();
        let why = why.as_list().unwrap();
        assert_eq!(why[1].as_text(), Some("default"));
        let opinions = why[4].as_list().unwrap();
        let first = opinions[0].as_list().unwrap();
        assert_eq!(first[1].as_text(), Some("/Base.size"));
        assert_eq!(first[2].as_text(), Some("inherits"));
        assert_eq!(first[3].as_text(), Some("contributed"));
    }

    #[test]
    fn confirmed_absent_assets_are_missing_with_a_code() {
        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        stage
            .document_mut()
            .storage_mut()
            .confirm_absent("sub.usda");
        stage
            .document_mut()
            .storage_mut()
            .confirm_absent("parts/ball.usda");
        let loaded = stage.load_supplied().unwrap();
        let failures = loaded.as_list().unwrap()[1].as_list().unwrap();
        assert_eq!(failures.len(), 2);
        assert_eq!(
            failures[0].as_list().unwrap()[2].as_text(),
            Some("io.not_found")
        );
        let status = stage.status();
        let errors = status.as_list().unwrap()[1].as_list().unwrap();
        let codes: Vec<&str> = errors
            .iter()
            .map(|error| error.as_list().unwrap()[0].as_text().unwrap())
            .collect();
        assert!(
            codes.contains(&"composition.unresolved_sublayer"),
            "{codes:?}"
        );
        assert!(codes.contains(&"composition.unresolved_asset"), "{codes:?}");
    }

    #[test]
    fn bad_input_fails_with_codes_not_panics() {
        let stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        for path in ["Ball", "/Ball/", "//Ball", "/Ball.size"] {
            assert_eq!(stage.prim(path).unwrap_err().code, "path.invalid", "{path}");
        }
        for path in ["/Ball", "/Ball.", "/.size", "Ball.size"] {
            assert_eq!(
                stage.get(path, None).unwrap_err().code,
                "path.invalid",
                "{path}"
            );
        }
        assert_eq!(stage.prim("/Nothing").unwrap(), Wire::Null);
        assert_eq!(
            stage.children("/Nothing").unwrap_err().code,
            "prim.not_found"
        );
        assert_eq!(
            stage.layer_dirty(u64::MAX).unwrap_err().code,
            "layer.unknown"
        );
        let rejected = Stage::open(
            "bad.usda",
            b"#usda 1.0\ndef {".to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap_err();
        assert_eq!(rejected.code, "io.rejected");
        assert_eq!(rejected.identifier.as_deref(), Some("bad.usda"));
    }

    #[test]
    fn a_reference_in_a_supplied_layer_is_wanted_next() {
        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let ball =
            "#usda 1.0\ndef \"Ball\" (\n    prepend references = @./trim.usda@</Trim>\n) {\n}\n";
        let trim = "#usda 1.0\ndef \"Trim\" {\n    token tag = \"trim\"\n}\n";
        let storage = stage.document_mut().storage_mut();
        storage.supply("sub.usda", SUB.as_bytes().to_vec());
        storage.supply("parts/ball.usda", ball.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert_eq!(identifiers(&stage.unavailable()), ["parts/trim.usda"]);
        stage
            .document_mut()
            .storage_mut()
            .supply("parts/trim.usda", trim.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert!(stage.unavailable().as_list().unwrap().is_empty());
        assert_eq!(
            stage.get("/Ball.tag", None).unwrap(),
            Wire::List(vec![
                Wire::text("token"),
                Wire::text("s"),
                Wire::text("trim")
            ])
        );
    }

    #[test]
    fn formats_come_from_the_identifier() {
        assert_eq!(format_of("set/scene.usda"), "usda");
        assert_eq!(format_of("scene.usdz[root.usdc]"), "usdc");
        assert_eq!(format_of("scene.usdz"), "usdz");
        assert_eq!(format_of("v1.2/plain"), "");
    }

    #[test]
    fn schemas_list_the_registry_sorted_with_kinds_and_bases() {
        let stage = Stage::open(
            "scene.usda",
            b"#usda 1.0\n".to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let Wire::List(rows) = stage.schemas() else {
            panic!("a list");
        };
        let row = |name: &str| {
            rows.iter()
                .find(|row| matches!(row, Wire::List(fields) if fields[0] == Wire::text(name)))
                .cloned()
                .unwrap_or_else(|| panic!("{name} is registered"))
        };
        let texts =
            |items: &[&str]| Wire::List(items.iter().map(|item| Wire::text(*item)).collect());
        // As usd-core 26.11's `TfType::GetAllAncestorTypes` names them, less
        // the schema itself and the non-schema roots.
        assert_eq!(
            row("Sphere"),
            Wire::List(vec![
                Wire::text("Sphere"),
                Wire::text("concreteTyped"),
                texts(&["Gprim", "Boundable", "Xformable", "Imageable", "Typed"]),
                texts(&[]),
                texts(&[]),
            ])
        );
        let Wire::List(collection) = row("CollectionAPI") else {
            panic!("a row");
        };
        assert_eq!(collection[1], Wire::text("multipleApplyApi"));
        assert_eq!(collection[2], texts(&[]));
        let Wire::List(imageable) = row("Imageable") else {
            panic!("a row");
        };
        assert_eq!(imageable[1], Wire::text("abstractTyped"));
        let names: Vec<_> = rows
            .iter()
            .map(|row| match row {
                Wire::List(fields) => fields[0].clone(),
                _ => panic!("a row"),
            })
            .collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|name| format!("{name:?}"));
        assert_eq!(names, sorted, "sorted by name");
    }

    #[test]
    fn stage_metadata_reads_the_root_layer_and_falls_back() {
        let root = r#"#usda 1.0
(
    upAxis = "Z"
    startTimeCode = 101
    endTimeCode = 129
)
"#;
        let stage = Stage::open(
            "scene.usda",
            root.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        assert_eq!(
            stage.stage_metadata("upAxis").unwrap(),
            Wire::List(vec![Wire::text("token"), Wire::text("s"), Wire::text("Z")])
        );
        assert_eq!(
            stage.stage_metadata("startTimeCode").unwrap(),
            Wire::List(vec![
                Wire::text("double"),
                Wire::text("s"),
                Wire::Number(101.0)
            ])
        );
        // Unauthored: the field's fallback, as `UsdStage::GetMetadata` reads it.
        assert_eq!(
            stage.stage_metadata("metersPerUnit").unwrap(),
            Wire::List(vec![
                Wire::text("double"),
                Wire::text("s"),
                Wire::Number(0.01)
            ])
        );
        assert_eq!(stage.stage_metadata("noSuchField").unwrap(), Wire::Null);
    }

    #[test]
    fn metadata_relationships_and_variant_names_read() {
        let root = r#"#usda 1.0
def "World" (
    kind = "assembly"
    variants = {
        string shade = "light"
    }
    prepend variantSets = "shade"
) {
    custom rel lookAt = [</World/Rock>, </World.size>]
    double size = 2 (
        hidden = true
    )

    def "Rock" {
    }

    variantSet "shade" = {
        "light" {
        }
        "dark" {
        }
    }
}
"#;
        let stage = Stage::open(
            "scene.usda",
            root.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        assert_eq!(
            stage.metadata("/World", "kind").unwrap(),
            Wire::List(vec![
                Wire::text("token"),
                Wire::text("s"),
                Wire::text("assembly")
            ])
        );
        assert_eq!(
            stage.metadata("/World.size", "hidden").unwrap(),
            Wire::List(vec![Wire::text("bool"), Wire::text("s"), Wire::Bool(true)])
        );
        assert_eq!(
            stage.metadata("/World", "documentation").unwrap(),
            Wire::Null
        );
        assert_eq!(
            stage.relationship("/World.lookAt").unwrap(),
            Wire::List(vec![
                Wire::Bool(true),
                Wire::List(vec![Wire::text("/World/Rock"), Wire::text("/World.size")]),
            ])
        );
        assert_eq!(stage.relationship("/World.missing").unwrap(), Wire::Null);
        assert_eq!(
            stage.variant_sets("/World").unwrap(),
            Wire::List(vec![Wire::List(vec![
                Wire::text("shade"),
                Wire::text("light"),
                Wire::List(vec![Wire::text("dark"), Wire::text("light")]),
            ])])
        );
        assert_eq!(
            stage.metadata("World", "kind").unwrap_err().code,
            "path.invalid"
        );
    }

    /// The scene of the tests above with every dependency supplied.
    fn resolved() -> Stage {
        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let storage = stage.document_mut().storage_mut();
        storage.supply("sub.usda", SUB.as_bytes().to_vec());
        storage.supply("parts/ball.usda", BALL.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert!(stage.unavailable().as_list().unwrap().is_empty());
        stage
    }

    fn set_size(stage: &mut Stage, target: LayerId, size: f64) {
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![size]);
        batch.set_value("/Ball.size".into(), None, slot);
        stage.apply(target.0, &mut batch).unwrap();
    }

    fn bytes(wire: &Wire) -> &[u8] {
        match wire {
            Wire::Bytes(bytes) => bytes,
            other => panic!("{other:?}"),
        }
    }

    fn size(stage: &Stage) -> Wire {
        stage.get("/Ball.size", None).unwrap()
    }

    #[test]
    fn save_writes_dirty_layers_for_the_host_and_they_reopen() {
        let mut stage = resolved();
        let root = stage.document().stage().stage().root_layer().unwrap();
        set_size(&mut stage, root, 7.0);
        let record = stage.save(None).unwrap();
        let [written, failures, anonymous, skipped] = record.as_list().unwrap() else {
            panic!("{record:?}");
        };
        assert!(failures.as_list().unwrap().is_empty());
        assert!(anonymous.as_list().unwrap().is_empty());
        assert!(skipped.as_list().unwrap().is_empty());
        let written = written.as_list().unwrap();
        assert_eq!(written.len(), 1, "only the edited root was dirty");
        let row = written[0].as_list().unwrap();
        assert_eq!(row[0], Wire::UInt64(root.0));
        assert_eq!(row[1].as_text(), Some("scene.usda"));
        assert!(!stage.layer_dirty(root.0).unwrap());
        let saved = bytes(&row[2]).to_vec();
        let empty = stage.save(None).unwrap();
        assert!(
            empty.as_list().unwrap()[0].as_list().unwrap().is_empty(),
            "nothing dirty"
        );

        let mut reopened = Stage::open("scene.usda", saved, false, false, BROWSER_LIMITS).unwrap();
        let storage = reopened.document_mut().storage_mut();
        storage.supply("sub.usda", SUB.as_bytes().to_vec());
        storage.supply("parts/ball.usda", BALL.as_bytes().to_vec());
        reopened.load_supplied().unwrap();
        assert_eq!(size(&reopened), size(&stage));
        assert_eq!(
            reopened.export_flattened("usda").unwrap(),
            stage.export_flattened("usda").unwrap(),
            "the same composed stage"
        );
    }

    /// A dirty layer the stage no longer composes, or mutes, is left
    /// unwritten by a save of the stage, and listed; `save(layer)` writes it.
    #[test]
    fn save_lists_the_dirty_layers_it_skips() {
        let mut stage = resolved();
        let ball = stage
            .document()
            .store()
            .layer_ids()
            .into_iter()
            .find(|id| stage.document().identifier(*id) == Some("parts/ball.usda"))
            .unwrap();
        let sub = stage
            .document()
            .store()
            .layer_ids()
            .into_iter()
            .find(|id| stage.document().identifier(*id) == Some("sub.usda"))
            .unwrap();
        let root = stage.document().stage().stage().root_layer().unwrap();
        let through = {
            let input = TargetInput {
                kind: "arc".into(),
                layer: ball.0,
                prim: Some("/Ball".into()),
                site: Some("/Ball".into()),
                arc: Some("references".into()),
                maps_outside: false,
                offset: 0.0,
                scale: 1.0,
            };
            stage.resolve_target(&input).map(|_| input).unwrap()
        };
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![9.0]);
        batch.create_attribute(
            "/Ball.mass".into(),
            "double".into(),
            false,
            true,
            Some(slot),
        );
        stage.apply_target(&through, &mut batch).unwrap();
        set_size(&mut stage, sub, 3.0);
        stage.set_layer_muted(sub.0, true).unwrap();
        let mut batch = EditBatch::new();
        batch.remove_arc(
            false,
            "/Ball".into(),
            Some("./parts/ball.usda".into()),
            Some("/Ball".into()),
        );
        stage.apply(root.0, &mut batch).unwrap();
        let record = stage.save(None).unwrap();
        let fields = record.as_list().unwrap();
        let written: Vec<&Wire> = fields[0]
            .as_list()
            .unwrap()
            .iter()
            .map(|row| &row.as_list().unwrap()[0])
            .collect();
        assert_eq!(written, [&Wire::UInt64(root.0)]);
        let mut skipped = fields[3].as_list().unwrap().to_vec();
        skipped.sort_by_key(|row| format!("{row:?}"));
        let mut expected = vec![
            Wire::List(vec![Wire::UInt64(ball.0), Wire::text("unused")]),
            Wire::List(vec![Wire::UInt64(sub.0), Wire::text("muted")]),
        ];
        expected.sort_by_key(|row| format!("{row:?}"));
        assert_eq!(skipped, expected);
        assert!(stage.layer_dirty(ball.0).unwrap(), "left dirty");
        let record = stage.save(Some(ball.0)).unwrap();
        assert_eq!(
            record.as_list().unwrap()[0].as_list().unwrap().len(),
            1,
            "written when named"
        );
    }

    #[test]
    fn a_session_layer_is_anonymous_to_save_and_exports() {
        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            true,
            BROWSER_LIMITS,
        )
        .unwrap();
        let session = stage.edit_layer().unwrap();
        set_size(&mut stage, session, 9.0);
        let record = stage.save(Some(session.0)).unwrap();
        assert_eq!(
            record.as_list().unwrap()[2],
            Wire::List(vec![Wire::UInt64(session.0)])
        );
        let text = String::from_utf8(stage.export_layer(session.0, "usda").unwrap()).unwrap();
        assert!(text.contains("double size = 9"), "{text}");
        assert!(
            stage.layer_dirty(session.0).unwrap(),
            "an export changes nothing"
        );
        assert_eq!(
            stage.save(Some(u64::MAX)).unwrap_err().code,
            "layer.unknown"
        );
        assert_eq!(
            stage.export_layer(session.0, "usdz").unwrap_err().code,
            "argument.invalid"
        );
    }

    #[test]
    fn exported_layers_and_flattened_stages_reopen_in_each_format() {
        let mut stage = resolved();
        let root = stage.document().stage().stage().root_layer().unwrap();
        for format in ["usda", "usdc"] {
            let exported = stage.export_layer(root.0, format).unwrap();
            let identifier = format!("copy.{format}");
            let copy = Stage::open(&identifier, exported, false, false, BROWSER_LIMITS).unwrap();
            assert_eq!(
                identifiers(&copy.unavailable()),
                ["parts/ball.usda", "sub.usda"]
            );

            let record = stage.export_flattened(format).unwrap();
            let [flat, lossless, lost] = record.as_list().unwrap() else {
                panic!("{record:?}");
            };
            assert_eq!(*lossless, Wire::Bool(true));
            assert!(lost.as_list().unwrap().is_empty());
            let identifier = format!("flat.{format}");
            let flat = Stage::open(
                &identifier,
                bytes(flat).to_vec(),
                false,
                false,
                BROWSER_LIMITS,
            )
            .unwrap();
            assert!(
                flat.unavailable().as_list().unwrap().is_empty(),
                "no arcs left"
            );
            assert_eq!(size(&flat), size(&stage));
            assert_eq!(
                flat.get("/Ball.points", None).unwrap().as_list().unwrap()[4],
                Wire::Number(2.0)
            );
        }
        assert_eq!(
            stage.export_flattened("text").unwrap_err().code,
            "argument.invalid"
        );
    }

    /// A layer the host names relative to its root (`set/rock.usda`)
    /// anchors a path relative to it to one relative to the same root,
    /// written relative to the flattened layer (`./set/albedo.png`): the
    /// search path `set/albedo.png` would name another asset. From a
    /// flattened layer at the host's root, each names what it named.
    #[test]
    fn a_flattened_layer_marks_host_relative_identifiers_relative() {
        let text = "#usda 1.0\ndef \"Rock\"\n{\n    asset albedo = @./albedo.png@\n    \
                    asset lichen = @textures/lichen.png@\n    asset moss = @textures/moss.png@\n    \
                    asset up = @../up.png@\n    asset shared = @/shared/stone.png@\n    \
                    asset remote = @https://example.com/rock.png@\n}\n";
        let mut stage = Stage::open(
            "set/rock.usda",
            text.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        stage
            .document_mut()
            .storage_mut()
            .supply("set/textures/lichen.png", Vec::new());
        let record = stage.export_flattened("usda").unwrap();
        let [flat, lossless, _] = record.as_list().unwrap() else {
            panic!("{record:?}");
        };
        assert_eq!(*lossless, Wire::Bool(true));
        let flat = String::from_utf8(bytes(flat).to_vec()).unwrap();
        let storage = stage.document().storage();
        for (line, identifier) in [
            ("asset albedo = @./set/albedo.png@", "set/albedo.png"),
            (
                "asset lichen = @./set/textures/lichen.png@",
                "set/textures/lichen.png",
            ),
            ("asset up = @./up.png@", "up.png"),
            ("asset shared = @/shared/stone.png@", "/shared/stone.png"),
            (
                "asset remote = @https://example.com/rock.png@",
                "https://example.com/rock.png",
            ),
        ] {
            assert!(flat.contains(line), "{line} in {flat}");
            let written = &line[line.find('@').unwrap() + 1..line.len() - 1];
            assert_eq!(
                layerstack_io::Storage::identify(storage, written, Some("flat.usda")).unwrap(),
                identifier,
                "{line}"
            );
        }
        assert!(
            flat.contains("asset moss = @textures/moss.png@"),
            "a search path nothing supplied beside its layer is kept: {flat}"
        );
    }

    #[test]
    fn a_package_holds_the_layered_stage_and_reopens_complete() {
        let mut stage = resolved();
        let root = stage.document().stage().stage().root_layer().unwrap();
        set_size(&mut stage, root, 4.0);
        let record = stage.export_package().unwrap();
        let [package, layers, assets, unresolved, omitted] = record.as_list().unwrap() else {
            panic!("{record:?}");
        };
        assert_eq!(*layers, Wire::Number(3.0));
        assert_eq!(*assets, Wire::Number(0.0));
        assert!(unresolved.as_list().unwrap().is_empty());
        assert!(omitted.as_list().unwrap().is_empty());
        assert!(
            stage.layer_dirty(root.0).unwrap(),
            "packaging saves nothing"
        );
        let reopened = Stage::open(
            "scene.usdz",
            bytes(package).to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        assert!(
            reopened.unavailable().as_list().unwrap().is_empty(),
            "complete"
        );
        assert_eq!(size(&reopened), size(&stage), "with the unsaved edit");

        let mut partial = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        let record = partial.export_package().unwrap();
        let record = record.as_list().unwrap();
        assert_eq!(
            record[0],
            Wire::Null,
            "no package while a layer is unavailable"
        );
        assert_eq!(record[3].as_list().unwrap().len(), 2);
    }

    #[test]
    fn package_members_are_named_after_the_root() {
        assert_eq!(package_member("set/scene.usda"), "scene.usda");
        assert_eq!(package_member("scene.usdz[root.usdc]"), "root.usdc");
        assert_eq!(package_member("a.usdz[b.usdz[sub/c.usda]]"), "c.usda");
    }

    #[test]
    fn a_move_renames_in_every_root_stack_layer_and_its_receipt_undoes_it() {
        let mut stage = resolved();
        let root = stage.document().stage().stage().root_layer().unwrap();
        let mut receipt = stage.move_object(root.0, "/Ball", "/Orb").unwrap();
        assert_eq!(stage.prim("/Ball").unwrap(), Wire::Null);
        assert_ne!(stage.prim("/Orb").unwrap(), Wire::Null);
        // The sublayer's over moved with the root's spec.
        assert!(stage.get("/Orb.serial", None).unwrap() != Wire::Null);
        stage.undo(&mut receipt).unwrap();
        assert_ne!(stage.prim("/Ball").unwrap(), Wire::Null);
        assert_eq!(stage.prim("/Orb").unwrap(), Wire::Null);

        let error = stage.move_object(root.0, "/Ball", "/Base").unwrap_err();
        assert_eq!(error.code, "namespace.collision");
        assert_eq!(error.path.as_deref(), Some("/Ball"));
        let error = stage.delete_object(root.0, "/Gone").unwrap_err();
        assert_eq!(error.code, "namespace.not_found");
        let error = stage.move_object(root.0, "/Ball", "Orb").unwrap_err();
        assert_eq!(error.code, "path.invalid");
    }

    #[test]
    fn rejected_edits_name_their_paths() {
        let mut stage = resolved();
        let root = stage.document().stage().stage().root_layer().unwrap();
        // The definition authors more than one native edit; the message
        // names the edit as sent, as `edit` does.
        let mut batch = EditBatch::new();
        batch.define_prim("/Pebble".into(), "def".into(), Some("Sphere".into()));
        let slot = batch.value_numbers("double".into(), vec![1.0]);
        batch.set_value("/Pebble.bogus".into(), None, slot);
        let error = stage.apply(root.0, &mut batch).unwrap_err();
        assert_eq!(error.code, "edit.rejected.undeclared_type");
        assert_eq!(
            error.message,
            "edit 1 rejected: /Pebble.bogus declares no type; create it with a type first"
        );
        assert_eq!(error.edit, Some(1));
        assert_eq!(error.path.as_deref(), Some("/Pebble.bogus"));
    }

    /// A mesh whose attributes come from three layers: the root, its
    /// sublayer and the asset it references.
    const MESH_ROOT: &str = r#"#usda 1.0
(
    subLayers = [@./edit.usda@]
)
def Mesh "Rock" (
    prepend references = @./rock.usda@</Rock>
) {
    int[] faceVertexCounts = [3]
}
"#;
    const MESH_EDIT: &str = r#"#usda 1.0
over "Rock" {
    point3f[] points = [(0, 0, 0), (2, 0, 0), (0, 2, 0)]
}
"#;
    const MESH_ASSET: &str = r#"#usda 1.0
def Mesh "Rock" {
    point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
    int[] faceVertexCounts = [4]
    int[] faceVertexIndices = [0, 1, 2]
    normal3f[] primvars:normals = [(0, 0, 1), (0, 0, 1), (0, 0, 1)] (
        interpolation = "vertex"
    )
    custom int[] primvars:ns:nativeFace = [0]
}
"#;

    fn mesh_stage(provenance: bool) -> Stage {
        let mut stage = Stage::open_with_provenance(
            "root.usda",
            MESH_ROOT.as_bytes().to_vec(),
            false,
            SessionLayer::None,
            BROWSER_LIMITS,
            provenance,
            &[],
        )
        .unwrap();
        let storage = stage.document_mut().storage_mut();
        storage.supply("edit.usda", MESH_EDIT.as_bytes().to_vec());
        storage.supply("rock.usda", MESH_ASSET.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert!(stage.unavailable().as_list().unwrap().is_empty());
        stage
    }

    /// The `[identifier, spec path, field]` the default value of each of
    /// `properties` of `/Rock` names as its source.
    fn sources(stage: &Stage, properties: &[&str]) -> Vec<Option<[String; 3]>> {
        let document = stage.document();
        let store = document.store();
        properties
            .iter()
            .map(|name| {
                let attribute = document.view().attribute(&format!("/Rock.{name}")).unwrap();
                let resolved = attribute.get(Time::Default).expect("a value");
                resolved.provenance.map(|source| {
                    [
                        document.identifier(source.layer).unwrap().to_string(),
                        source.spec_path.display(store.paths(), store.tokens()),
                        store.tokens().resolve(source.field).to_string(),
                    ]
                })
            })
            .collect()
    }

    #[test]
    fn mesh_values_name_the_layer_and_spec_that_gave_them() {
        const PROPERTIES: [&str; 5] = [
            "points",
            "faceVertexCounts",
            "faceVertexIndices",
            "primvars:normals",
            "primvars:ns:nativeFace",
        ];
        let source = |layer: &str, name: &str| {
            Some([
                String::from(layer),
                format!("/Rock.{name}"),
                String::from(name),
            ])
        };
        let stage = mesh_stage(true);
        assert_eq!(
            sources(&stage, &PROPERTIES),
            [
                source("edit.usda", "points"),
                source("root.usda", "faceVertexCounts"),
                source("rock.usda", "faceVertexIndices"),
                source("rock.usda", "primvars:normals"),
                source("rock.usda", "primvars:ns:nativeFace"),
            ]
        );
        assert_eq!(
            sources(&mesh_stage(false), &PROPERTIES),
            [None, None, None, None, None],
            "without provenance the values say nothing of their sources"
        );
    }

    #[test]
    fn native_transactions_and_muting_reach_the_change_record() {
        let mut stage = mesh_stage(true);
        let _ = stage.take_changes(false);
        let root = stage.document().root_layer();
        let mesh = stage.document_mut().store_mut().path("/Rock/Derived");
        let mut txn = Transaction::new();
        txn.create_prim(EditTarget::for_layer(root).prim(mesh), Specifier::Def, None);
        stage.document_mut().apply(&txn).unwrap();
        let created = |changes: &Wire| -> Vec<String> {
            batches(changes)
                .iter()
                .flat_map(|batch| batch.as_list().unwrap()[0].as_list().unwrap().iter())
                .map(|path| path.as_text().unwrap().to_string())
                .collect()
        };
        assert_eq!(created(&stage.take_changes(true)), ["/Rock/Derived"]);

        // Muting is a queued control: heard once the document synchronizes.
        let edit = stage
            .document()
            .store()
            .layer_ids()
            .into_iter()
            .find(|&id| stage.document().identifier(id) == Some("edit.usda"))
            .unwrap();
        let (_, live) = stage.document_mut().parts_mut();
        assert!(live.mute_layer(edit).unwrap());
        let unheard = stage.take_changes(true);
        assert!(batches(&unheard).is_empty(), "nothing recomposed yet");
        assert!(muting(&unheard).0.is_empty(), "nor muted");
        stage.document_mut().synchronize();
        let heard = stage.take_changes(true);
        assert!(!batches(&heard).is_empty());
        assert_eq!(muting(&heard), (vec![edit.0], vec![]));
        assert_eq!(
            sources(&stage, &["points"]),
            [Some([
                String::from("rock.usda"),
                String::from("/Rock.points"),
                String::from("points"),
            ])],
            "the muted sublayer no longer gives the points"
        );
    }

    /// The batches of a `take_changes` record.
    fn batches(record: &Wire) -> &[Wire] {
        record.as_list().unwrap()[0].as_list().unwrap()
    }

    /// The layers a `take_changes` record says were muted and unmuted.
    fn muting(record: &Wire) -> (Vec<u64>, Vec<u64>) {
        let ids =
            |wire: &Wire| -> Vec<u64> { wire.as_list().unwrap().iter().map(row_id).collect() };
        let record = record.as_list().unwrap();
        (ids(&record[1]), ids(&record[2]))
    }

    /// The id of a `[id, identifier, held]` muting row.
    fn row_id(row: &Wire) -> u64 {
        match &row.as_list().unwrap()[0] {
            Wire::UInt64(id) => *id,
            other => panic!("{other:?}"),
        }
    }

    /// The ids `muted_layers` lists.
    fn muted_ids(stage: &Stage) -> Vec<u64> {
        stage
            .muted_layers()
            .as_list()
            .unwrap()
            .iter()
            .map(row_id)
            .collect()
    }

    /// The ids `layers` lists.
    fn layer_rows(stage: &Stage) -> Vec<u64> {
        stage.layers().as_list().unwrap()[2]
            .as_list()
            .unwrap()
            .iter()
            .map(row_id)
            .collect()
    }

    fn layer_named(stage: &Stage, identifier: &str) -> LayerId {
        stage
            .document()
            .store()
            .layer_ids()
            .into_iter()
            .find(|&id| stage.document().identifier(id) == Some(identifier))
            .unwrap()
    }

    fn has_attribute(stage: &Stage, path: &str) -> bool {
        stage.attribute(path).unwrap() != Wire::Null
    }

    #[test]
    fn muting_a_sublayer_hides_it_until_unmuted_and_is_heard() {
        let mut stage = resolved();
        let _ = stage.take_changes(false);
        let sub = layer_named(&stage, "sub.usda");
        assert!(has_attribute(&stage, "/Ball.serial"));

        assert!(stage.set_layer_muted(sub.0, true).unwrap());
        assert!(
            !has_attribute(&stage, "/Ball.serial"),
            "the muted sublayer gives no opinions"
        );
        assert_eq!(muted_ids(&stage), [sub.0]);
        let heard = stage.take_changes(true);
        assert!(!batches(&heard).is_empty(), "the stage recomposed");
        assert_eq!(muting(&heard), (vec![sub.0], vec![]));
        assert!(stage.unavailable().as_list().unwrap().is_empty());

        assert!(
            !stage.set_layer_muted(sub.0, true).unwrap(),
            "already muted"
        );
        let quiet = stage.take_changes(true);
        assert!(batches(&quiet).is_empty());
        assert_eq!(muting(&quiet), (vec![], vec![]));

        assert!(stage.set_layer_muted(sub.0, false).unwrap());
        assert!(
            has_attribute(&stage, "/Ball.serial"),
            "unmuted, it composes again"
        );
        assert!(muted_ids(&stage).is_empty());
        assert_eq!(muting(&stage.take_changes(true)), (vec![], vec![sub.0]));
        assert!(!stage.set_layer_muted(sub.0, false).unwrap(), "not muted");
    }

    #[test]
    fn the_root_and_unknown_layers_cannot_be_muted() {
        let mut stage = resolved();
        let root = stage.document().root_layer();
        let error = stage.set_layer_muted(root.0, true).unwrap_err();
        assert_eq!(error.code, "mute.root_layer");
        assert!(
            !stage.set_layer_muted(root.0, false).unwrap(),
            "unmuting the root, never muted, changes nothing"
        );
        assert!(muted_ids(&stage).is_empty());
        let error = stage.set_layer_muted(u64::MAX, true).unwrap_err();
        assert_eq!(error.code, "layer.unknown");
    }

    #[test]
    fn a_muted_layer_keeps_its_content_but_is_not_edited_through_the_stage() {
        let mut stage = resolved();
        let sub = layer_named(&stage, "sub.usda");
        let root = stage.document().root_layer();
        set_size(&mut stage, sub, 5.0);
        let undo = {
            let mut batch = EditBatch::new();
            let slot = batch.value_numbers("double".into(), vec![6.0]);
            batch.set_value("/Ball.size".into(), None, slot);
            stage.apply(root.0, &mut batch).unwrap()
        };
        assert!(stage.set_layer_muted(sub.0, true).unwrap());
        assert!(stage.layer_dirty(sub.0).unwrap(), "muting keeps the edit");
        let written = |record: &Wire| -> Vec<String> {
            record.as_list().unwrap()[0]
                .as_list()
                .unwrap()
                .iter()
                .map(|row| row.as_list().unwrap()[1].as_text().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            written(&stage.save(None).unwrap()),
            ["scene.usda"],
            "saving the stage skips the layer it does not use"
        );
        assert!(stage.layer_dirty(sub.0).unwrap());
        assert_eq!(
            written(&stage.save(Some(sub.0)).unwrap()),
            ["sub.usda"],
            "saving it by name writes it"
        );
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![1.0]);
        batch.set_value("/Ball.size".into(), None, slot);
        let error = stage.apply(sub.0, &mut batch).unwrap_err();
        assert_eq!(error.code, "layer.muted");
        assert_eq!(error.identifier.as_deref(), Some("sub.usda"));

        // Muting is not an edit: a receipt made before it still undoes.
        let mut undo = undo;
        stage.undo(&mut undo).unwrap();
        assert_eq!(
            size(&stage),
            stage.get("/Base.size", None).unwrap(),
            "the root's edit undone, the muted sublayer's 5 is not composed"
        );
        assert!(stage.set_layer_muted(sub.0, false).unwrap());
        assert_eq!(
            size(&stage),
            Wire::List(vec![
                Wire::text("double"),
                Wire::text("s"),
                Wire::Number(5.0)
            ]),
            "unmuted, its edit composes again"
        );
    }

    #[test]
    fn a_referenced_layer_and_the_session_layer_can_be_muted() {
        let mut stage = resolved();
        let ball = layer_named(&stage, "parts/ball.usda");
        assert!(has_attribute(&stage, "/Ball.points"));
        assert!(stage.set_layer_muted(ball.0, true).unwrap());
        assert!(
            !has_attribute(&stage, "/Ball.points"),
            "the reference to the muted layer contributes nothing"
        );
        assert!(
            stage.unavailable().as_list().unwrap().is_empty(),
            "nor is it wanted"
        );

        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            true,
            BROWSER_LIMITS,
        )
        .unwrap();
        let session = stage.edit_layer().unwrap();
        set_size(&mut stage, session, 9.0);
        assert!(stage.set_layer_muted(session.0, true).unwrap());
        assert_eq!(
            size(&stage),
            Wire::List(vec![
                Wire::text("double"),
                Wire::text("s"),
                Wire::Number(2.0)
            ]),
            "the muted session layer's opinion is gone"
        );
        let mut batch = EditBatch::new();
        let slot = batch.value_numbers("double".into(), vec![1.0]);
        batch.set_value("/Ball.size".into(), None, slot);
        assert_eq!(
            stage.apply(session.0, &mut batch).unwrap_err().code,
            "layer.muted"
        );
    }

    #[test]
    fn a_layer_muted_by_identifier_before_it_is_read_composes_muted() {
        let mut stage = Stage::open(
            "scene.usda",
            ROOT.as_bytes().to_vec(),
            false,
            false,
            BROWSER_LIMITS,
        )
        .unwrap();
        stage
            .document_mut()
            .storage_mut()
            .supply("sub.usda", SUB.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert_eq!(identifiers(&stage.unavailable()), ["parts/ball.usda"]);
        let _ = stage.take_changes(false);

        // Unavailable, then muted by another spelling: no longer asked for.
        let ball = [MuteRequest::Identifier("./parts/../parts/ball.usda")];
        assert!(stage.mute_and_unmute_layers(&ball, &[]).unwrap());
        assert!(stage.unavailable().as_list().unwrap().is_empty());
        assert!(stage.is_layer_muted_named("parts/ball.usda").unwrap());
        let rows = stage.muted_layers();
        let row = rows.as_list().unwrap()[0].as_list().unwrap();
        assert_eq!(row[1], Wire::text("parts/ball.usda"));
        assert_eq!(row[2], Wire::Bool(false), "not read");
        // As OpenUSD opens no layer for it (`SdfLayer::Find` gives none),
        // its id names no layer until it is unmuted and read.
        let reserved = row_id(&rows.as_list().unwrap()[0]);
        assert!(!layer_rows(&stage).contains(&reserved));
        assert_eq!(
            stage.layer_dirty(reserved).unwrap_err().code,
            "layer.unknown"
        );
        let errors = stage.status().as_list().unwrap()[1].clone();
        let errors: Vec<&[Wire]> = errors
            .as_list()
            .unwrap()
            .iter()
            .map(|error| error.as_list().unwrap())
            .collect();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0][0], Wire::text("composition.muted_asset"));
        assert_eq!(
            errors[0][1],
            Wire::text(
                "/Ball: the reference authored at /Ball in @scene.usda@ targets \
                 @parts/ball.usda@, which the stage mutes; it contributes nothing"
            ),
            "named by its identifier, not read"
        );
        let heard = stage.take_changes(true);
        assert_eq!(muting(&heard).0.len(), 1, "heard muted while not read");

        // Supplied while muted: not read.
        stage
            .document_mut()
            .storage_mut()
            .supply("parts/ball.usda", BALL.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert!(!has_attribute(&stage, "/Ball.points"));

        // Unmuted: read, as OpenUSD opens it on recomposing, under the id
        // it was muted by.
        assert!(stage.mute_and_unmute_layers(&[], &ball).unwrap());
        assert!(has_attribute(&stage, "/Ball.points"));
        assert!(layer_rows(&stage).contains(&reserved));
        assert_eq!(
            stage.document().identifier(LayerId(reserved)),
            Some("parts/ball.usda")
        );
        assert_eq!(stage.layer_dirty(reserved), Ok(false));
        assert!(stage.unavailable().as_list().unwrap().is_empty());
        let codes = stage.status().as_list().unwrap()[1]
            .as_list()
            .unwrap()
            .len();
        assert_eq!(codes, 0);
    }

    #[test]
    fn an_unmute_spelled_as_a_mute_is_dropped() {
        let mut stage = resolved();
        let sub = layer_named(&stage, "sub.usda");
        let both = [MuteRequest::Identifier("sub.usda")];
        assert!(stage.mute_and_unmute_layers(&both, &both).unwrap());
        assert_eq!(muted_ids(&stage), [sub.0], "the same spelling stays muted");
        let by_layer = [MuteRequest::Layer(sub.0)];
        assert!(
            !stage.mute_and_unmute_layers(&by_layer, &both).unwrap(),
            "a layer is spelled by its identifier"
        );
        let other = [MuteRequest::Identifier("./sub.usda")];
        assert!(stage.mute_and_unmute_layers(&both, &other).unwrap());
        assert!(
            muted_ids(&stage).is_empty(),
            "two spellings: muted, then unmuted"
        );

        let root = [
            MuteRequest::Identifier("./scene.usda"),
            MuteRequest::Layer(sub.0),
        ];
        let error = stage.mute_and_unmute_layers(&root, &[]).unwrap_err();
        assert_eq!(error.code, "mute.root_layer");
        assert!(muted_ids(&stage).is_empty(), "refused whole");
        assert!(
            !stage
                .mute_and_unmute_layers(&[MuteRequest::Identifier("")], &[])
                .unwrap()
        );
        assert!(!stage.is_layer_muted_named("never.usda").unwrap());
        assert!(
            !stage
                .mute_and_unmute_layers(&[], &[MuteRequest::Identifier("never.usda")])
                .unwrap()
        );
    }

    #[test]
    fn prototypes_list_their_instance_roots_in_traversal_order() {
        let stage = crate::schemas::tests::open(
            r#"#usda 1.0
class "Beacon" {
    def Sphere "Bulb" {
    }
}
def "North" (
    instanceable = true
    prepend references = </Beacon>
) {
}
def "South" (
    instanceable = true
    prepend references = </Beacon>
) {
}
def "Lone" (
    prepend references = </Beacon>
) {
}
"#,
        );
        assert_eq!(
            crate::schemas::tests::show(&stage.prototypes()),
            "[[/North, /South]]"
        );
    }
}
