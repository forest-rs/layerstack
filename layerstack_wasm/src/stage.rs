// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An open stage: loading, inspection, explanation and authoring.

use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};

use layerstack::{
    ArcKind, ChangeCursor, Changes, CompositionLimits, Contribution, EditError, FlattenError,
    FlattenRequirements, IgnoreCause, InMemoryStore, Layer, LayerId, OpinionRole, PathId,
    ResolvedValue, Specifier, StageOptions, Time, Transaction, ValueSource, Variability,
    asset_dependencies::DependencyTarget,
};
use layerstack_io::{
    ApplyError, FORMAT_ARGS_DELIMITER, ImportPolicy, IoError, LoadOptions, LoadReport, OpenOptions,
    PackageInclusion, PackagePreparationLimits, SaveReport, StageDocument, UnavailableAsset,
};
use wasm_bindgen::prelude::*;

use crate::{
    edit::{self, EditBatch, Scope},
    error::Error,
    receipt::EditReceipt,
    storage::HostStorage,
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
    /// What a receipt holds to name the stage that made it.
    identity: Arc<()>,
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
        let mut storage = HostStorage::new();
        storage.supply(identifier, bytes);
        let mut store = InMemoryStore::default();
        let schemas = layerstack_schemas::openusd(store.tokens());
        if session_layer {
            store.insert_layer(Layer::new(SESSION_LAYER));
        }
        let options = StageOptions {
            schemas: Some(Arc::new(schemas)),
            session_layer: session_layer.then_some(SESSION_LAYER),
            limits,
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
                    ..LoadOptions::default()
                },
                store: Some(store),
            },
        )?;
        let cursor = document.parts_mut().1.change_cursor();
        Ok(Self {
            document,
            cursor,
            identity: Arc::new(()),
        })
    }

    /// The document: its store, live stage and storage.
    #[must_use]
    pub fn document(&self) -> &StageDocument<HostStorage> {
        &self.document
    }

    /// The document, mutably. A caller that edits it reports its changes
    /// to its own users; nothing here observes them.
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
    /// ([`StageDocument::load_requested_assets`]): `[loaded, failures]`,
    /// the ids of the layers it read and, for each asset that failed,
    /// `[asset, anchor, code, message]` (a confirmed-absent asset fails
    /// with `io.not_found`).
    ///
    /// # Errors
    ///
    /// A strict open's rejection of a supplied layer (`io.rejected`), with
    /// the layers read before it kept.
    pub(crate) fn load_supplied(&mut self) -> Result<Wire, Error> {
        let report = self.document.load_requested_assets()?;
        Ok(load_result(&report))
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
                    Wire::Bool(self.read_only(id, identifier)),
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
        (!self.read_only(root, self.document.identifier(root))).then_some(root)
    }

    /// Whether saving layer `id` would fail as read-only
    /// ([`layerstack_io::IoErrorKind::ReadOnly`]): a package's layers, and
    /// a layer built from format arguments.
    fn read_only(&self, id: LayerId, identifier: Option<&str>) -> bool {
        self.document.package_stats(id).is_some()
            || identifier.is_some_and(|text| text.contains(FORMAT_ARGS_DELIMITER))
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

    /// Why the attribute has its value at `time`:
    /// `[value, source, lower, upper, opinions]`, or `null` when the
    /// attribute does not exist. `value` is a value record or `null`;
    /// `source` is `none`, `default`, `timeSamples`, `spline`,
    /// `valueClips` or `fallback`, with the bracketing sample times
    /// `lower` and `upper` for `timeSamples` (else `null`). Each opinion,
    /// strongest first, is
    /// `[layer, specPath, arc, role, detail, offset, scale]`: the layer
    /// id, the spec path in that layer, the arc that reaches it (`null`
    /// without a node), `contributed`, `block` or `ignored`, what it
    /// contributed (`value`, `arrayEdit`, `dictionary`, `listEdit`) or why
    /// it was ignored (`shadowed`, `cutOffByBlock`, `incompatible`), and
    /// the layer offset mapping stage time to its layer.
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
    /// Either every edit applies or none does. The receipt holds the
    /// inverse, and its record is `[target, changes, loaded, failures,
    /// unavailable]`: the target layer's id, the composed changes of the
    /// transaction (`[created, removed, resynced, changedInfoOnly]`), the
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
        let target = LayerId(target);
        self.check_target(target)?;
        let batch = core::mem::take(batch);
        let lowered = {
            let stage = self.document.stage().stage();
            let scope = Scope {
                store: self.document.store(),
                view: self.document.view(),
                target,
                stack: stage.layer_stack(),
            };
            edit::lower(&batch, &scope)?
        };
        self.commit(target, &lowered.transaction, |op| {
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
        match self.commit(receipt.target, &inverse, |_| None) {
            Ok(redo) => Ok(redo),
            Err(error) => {
                receipt.inverse = Some(inverse);
                Err(error)
            }
        }
    }

    /// The composed changes since the last call, one batch per
    /// recomposition, oldest first, each
    /// `[created, removed, resynced, changedInfoOnly]` prim paths as text
    /// ([`Changes`]); an empty list when nothing changed, and `null` when
    /// the history the cursor needs was evicted (more than the history
    /// budget changed in one call), which the wrapper reports as a
    /// resync of `/`. With `render` false the batches are consumed
    /// unrendered and the list is empty.
    #[must_use]
    pub(crate) fn take_changes(&mut self, render: bool) -> Wire {
        let store = self.document.store();
        match self.document.stage().changes_since(&mut self.cursor) {
            Ok(batches) => Wire::List(
                batches
                    .filter(|_| render)
                    .map(|changes| changes_record(changes, store))
                    .collect(),
            ),
            Err(_) => Wire::Null,
        }
    }

    fn check_target(&self, target: LayerId) -> Result<(), Error> {
        let identifier = self.document.identifier(target);
        let named = |error: Error| Error {
            identifier: identifier.map(ToString::to_string),
            ..error
        };
        if self.document.store().layer(target).is_none() {
            return Err(Error::new(
                "layer.unknown",
                "the stage has no layer with this id",
            ));
        }
        if self.read_only(target, identifier) {
            return Err(named(Error::new(
                "layer.read_only",
                "the layer cannot be saved, so it is not edited: open the stage with a session \
                 layer (editLayer: \"session\") and edit that",
            )));
        }
        if !self
            .document
            .stage()
            .stage()
            .layer_stack()
            .contains(&target)
        {
            return Err(named(Error::new(
                "layer.outside_stack",
                "the layer is not in the stage's layer stack: the stage reaches it only \
                 through an arc",
            )));
        }
        Ok(())
    }

    /// Applies `txn` to `target`, as the receipt of what it did.
    fn commit<'a>(
        &mut self,
        target: LayerId,
        txn: &Transaction,
        edit_of: impl Fn(usize) -> Option<(u32, &'a str)>,
    ) -> Result<EditReceipt, Error> {
        match self.document.apply(txn) {
            Ok((applied, report)) => Ok(self.receipt(target, applied, &report, None)),
            Err(ApplyError::Load { applied, error }) => {
                Ok(self.receipt(target, *applied, &LoadReport::default(), Some(&error)))
            }
            Err(ApplyError::Edit(error)) => {
                let store = self.document.store();
                let mut out = Error::new(error.code(), error.display(store).to_string());
                if let EditError::Rejected { op, .. } = &error
                    && let Some((index, path)) = edit_of(*op)
                {
                    out = out.at_edit(index).at_path(path);
                }
                Err(out)
            }
            Err(error) => {
                let store = self.document.store();
                Err(Error::new(error.code(), error.display(store).to_string()))
            }
        }
    }

    /// The receipt of an applied transaction. Its record is
    /// `[target, changes, loaded, failures, unavailable]`: the target layer's
    /// id, the composed changes of the transaction
    /// (`[created, removed, resynced, changedInfoOnly]`), the ids of the
    /// layers loaded for it, each asset that failed to load as
    /// `[asset, anchor, code, message]`, and the assets it asks for that
    /// the host has not supplied yet, as [`Self::unavailable`] rows.
    fn receipt(
        &self,
        target: LayerId,
        applied: layerstack::Applied,
        report: &LoadReport,
        failed: Option<&IoError>,
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
        let record = Wire::List(vec![
            Wire::UInt64(target.0),
            changes_record(&applied.changes, store),
            loaded,
            Wire::List(failures),
            Wire::List(unavailable),
        ]);
        EditReceipt {
            stage: Arc::clone(&self.identity),
            target,
            inverse: Some(applied.inverse),
            record,
        }
    }

    /// Saves the dirty layers the stage uses to the host's storage
    /// ([`StageDocument::save`], OpenUSD's `UsdStage::Save`: not the
    /// session layer, which is anonymous), or only layer `layer` when given
    /// ([`StageDocument::save_layers`]). Each layer is written in its own
    /// format to the identifier it was read from; the bytes become that
    /// identifier's source and are handed to the host to persist.
    ///
    /// The record is `[written, failures, anonymous]`: each layer written as
    /// `[layer, identifier, bytes]`, each that could not be as
    /// `[layer, code, message]` (`io.read_only` for a package member, which
    /// stays dirty), and the ids of the dirty anonymous layers, which have
    /// nowhere to go (export them with `exportLayer`).
    ///
    /// Spec: AOUSD Core §7.1 (layers are saved as authored).
    ///
    /// # Errors
    ///
    /// `layer.unknown` when `layer` names no layer of the stage.
    pub(crate) fn save(&mut self, layer: Option<u64>) -> Result<Wire, Error> {
        let report = match layer {
            Some(id) => {
                self.check_layer(LayerId(id))?;
                self.document.save_layers(&[LayerId(id)])
            }
            None => self.document.save(),
        };
        Ok(self.save_record(&report))
    }

    fn save_record(&mut self, report: &SaveReport) -> Wire {
        let mut written = self.document.storage_mut().take_written();
        let rows = report
            .saved
            .iter()
            .filter_map(|&id| {
                let identifier = self.document.identifier(id)?;
                let bytes = written.remove(identifier)?;
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
    /// prototypes and asset paths are written as authored. A flatten that
    /// would lose something (animation a layer cannot hold) still exports,
    /// accepting exactly those losses, with `lossless` false and each loss
    /// in `lost` as `[path, message]`.
    ///
    /// Spec: AOUSD Core §11 (stage population), §12 (value resolution).
    ///
    /// # Errors
    ///
    /// `argument.invalid` for a format other than `usda` or `usdc`;
    /// `flatten.failed` when the stage has no root; the writer's
    /// `io.unsupported`.
    pub(crate) fn export_flattened(&mut self, format: &str) -> Result<Wire, Error> {
        let target = export_target(format)?;
        let id = self.unused_layer_id();
        let (store, live) = self.document.parts_mut();
        let stage = live.stage();
        let root = stage
            .root_layer()
            .ok_or_else(|| Error::new("flatten.failed", "the stage has no root layer"))?;
        let requirements = FlattenRequirements::default();
        let flattened = match stage.flatten(store, root, id, &requirements) {
            Err(FlattenError::Refused(refusal)) => {
                let relaxed = requirements.accepting(&refusal.unmet);
                stage.flatten(store, root, id, &relaxed)
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

    /// The smallest layer id above the session layer's that no layer of
    /// the store has, for a layer the bindings build and never store.
    fn unused_layer_id(&self) -> LayerId {
        let store = self.document.store();
        (SESSION_LAYER.0 + 1..)
            .map(LayerId)
            .find(|id| !store.contains_layer(*id))
            .unwrap_or(SESSION_LAYER)
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

    fn existing_prim(&self, path: &str) -> Result<layerstack::Prim<'_>, Error> {
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
    /// limit (truncated to an integer).
    #[wasm_bindgen(js_name = open)]
    pub fn js_open(
        identifier: &str,
        bytes: Vec<u8>,
        recover: bool,
        session_layer: bool,
        prim_index_nodes: Option<f64>,
        nodes_per_prim_index: Option<f64>,
        composed_depth: Option<f64>,
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
        Ok(Self::open(
            identifier,
            bytes,
            recover,
            session_layer,
            limits,
        )?)
    }

    /// Applies a batch of edits to a layer ([`Self::apply`]).
    #[wasm_bindgen(js_name = apply)]
    pub fn js_apply(&mut self, target: u64, batch: &mut EditBatch) -> Result<EditReceipt, JsValue> {
        Ok(self.apply(target, batch)?)
    }

    /// Undoes a receipt's edits ([`Self::undo`]).
    #[wasm_bindgen(js_name = undo)]
    pub fn js_undo(&mut self, receipt: &mut EditReceipt) -> Result<EditReceipt, JsValue> {
        Ok(self.undo(receipt)?)
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

    /// The record `time_samples` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = timeSamples)]
    pub fn js_time_samples(&self, path: &str) -> Result<Vec<f64>, JsValue> {
        Ok(self.time_samples(path)?)
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

    /// The record `supplied_bytes` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = suppliedBytes)]
    pub fn js_supplied_bytes(&self) -> u64 {
        self.supplied_bytes()
    }
}

/// `[created, removed, resynced, changedInfoOnly]`: the prim paths of
/// `changes` as text.
fn changes_record(changes: &Changes, store: &InMemoryStore) -> Wire {
    let paths = |ids: &[PathId]| {
        Wire::List(
            ids.iter()
                .map(|&id| Wire::Text(store.paths().display(id, store.tokens())))
                .collect(),
        )
    };
    Wire::List(vec![
        paths(&changes.created),
        paths(&changes.removed),
        paths(&changes.resynced),
        paths(&changes.changed_info_only),
    ])
}

/// `[identifier, assetPath, anchor, usedBy, prims]` for an unavailable
/// asset, as [`Stage::unavailable`] lists it.
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

/// The time a query reads: the default time, or a time code interpolated
/// linearly (OpenUSD's default).
fn time_of(time: Option<f64>) -> Time {
    time.map_or(Time::Default, Time::at)
}

/// An arc kind as the wrapper spells it: the scene description field's
/// name, `local` for a local opinion.
fn arc_name(arc: ArcKind) -> &'static str {
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

/// `path` is an absolute prim path: `/`, or `/` and names separated by
/// `/`, none empty and none holding a `.`.
fn check_prim_path(path: &str) -> Result<(), Error> {
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
fn check_property_path(path: &str) -> Result<(), Error> {
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
        assert_eq!(loaded.as_list().unwrap()[0].as_list().unwrap().len(), 2);
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
        let [written, failures, anonymous] = record.as_list().unwrap() else {
            panic!("{record:?}");
        };
        assert!(failures.as_list().unwrap().is_empty());
        assert!(anonymous.as_list().unwrap().is_empty());
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
}
