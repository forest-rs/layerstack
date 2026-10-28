// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! An open stage: loading, inspection and explanation.

use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};

use layerstack::{
    ArcKind, Contribution, IgnoreCause, InMemoryStore, LayerId, OpinionRole, ResolvedValue,
    Specifier, StageOptions, Time, ValueSource, Variability,
};
use layerstack_io::{FORMAT_ARGS_DELIMITER, ImportPolicy, LoadReport, StageDocument};
use wasm_bindgen::prelude::*;

use crate::{error::Error, storage::HostStorage, value, wire::Wire};

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
}

impl Stage {
    /// Opens the root layer `identifier` from `bytes`, with every OpenUSD
    /// schema registered (`layerstack_schemas::openusd`), and loads what
    /// storage has: only the root, so every dependency is unavailable
    /// until the host supplies it. `recover` imports malformed content the
    /// reader can recover from ([`ImportPolicy::AllowRecovery`]) instead
    /// of rejecting it.
    ///
    /// # Errors
    ///
    /// The root's read or import failure, with its [`IoErrorKind`] code
    /// (`io.rejected` for a root that does not parse).
    ///
    /// [`IoErrorKind`]: layerstack_io::IoErrorKind
    pub fn open(identifier: &str, bytes: Vec<u8>, recover: bool) -> Result<Self, Error> {
        let mut storage = HostStorage::new();
        storage.supply(identifier, bytes);
        let store = InMemoryStore::default();
        let schemas = layerstack_schemas::openusd(&store.tokens);
        let options = StageOptions {
            schemas: Some(Arc::new(schemas)),
            ..StageOptions::default()
        };
        let policy = if recover {
            ImportPolicy::AllowRecovery
        } else {
            ImportPolicy::Strict
        };
        let document = StageDocument::open_in(storage, store, identifier, options, policy)?;
        Ok(Self { document })
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

    /// The assets the stage wants, each
    /// `[identifier, asset, anchor, arc]` ([`StageDocument::unavailable_assets`]):
    /// the identifier to supply, the asset path as authored, the anchor
    /// layer's id and `"sublayers"`, `"references"` or `"payloads"`.
    #[must_use]
    pub(crate) fn wanted(&self) -> Wire {
        Wire::List(
            self.document
                .unavailable_assets()
                .into_iter()
                .map(|need| {
                    Wire::List(vec![
                        Wire::Text(need.identifier),
                        Wire::Text(need.asset),
                        Wire::UInt64(need.anchor.0),
                        Wire::text(need_arc(need.arc)),
                    ])
                })
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
    /// `[root, session, [[id, identifier, format, readOnly, dirty], ...]]`,
    /// `session` `null` without one. `identifier` is `null` for an
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
        ])
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
        if self.document.store().layer(layer).is_none() {
            return Err(Error::new(
                "layer.unknown",
                "the stage has no layer with this id",
            ));
        }
        Ok(self.document.is_dirty(layer))
    }

    /// The stage's state now: `[prims, compositionErrors, missing]`, the
    /// number of prims its default traversal visits, each composition
    /// error as `[code, message, prim]` ([`layerstack::CompositionError`],
    /// `prim` `null` for an error in a layer stack) and each confirmed
    /// missing asset as `[anchor, asset, arc]`
    /// ([`StageDocument::asset_status`]).
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
                        Wire::Text(store.paths.display(prim, &store.tokens))
                    }),
                ])
            })
            .collect();
        let missing = self
            .document
            .asset_status()
            .missing
            .into_iter()
            .map(|asset| {
                Wire::List(vec![
                    Wire::UInt64(asset.anchor.0),
                    Wire::Text(asset.asset),
                    Wire::text(need_arc(asset.arc)),
                ])
            })
            .collect();
        Wire::List(vec![
            Wire::Number(view.traverse().count() as f64),
            Wire::List(errors),
            Wire::List(missing),
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
            .map(|child| store.paths.display(child.path(), &store.tokens))
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
            value::encode(&resolved.value, attribute.type_name(), &store.tokens)
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
            Some(value) => value::encode(value, attribute.type_name(), &store.tokens)
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
                        },
                    ),
                    OpinionRole::Block => ("block", ""),
                    OpinionRole::Ignored(cause) => (
                        "ignored",
                        match cause {
                            IgnoreCause::Shadowed => "shadowed",
                            IgnoreCause::CutOffByBlock => "cutOffByBlock",
                            IgnoreCause::Incompatible => "incompatible",
                        },
                    ),
                };
                let offset = opinion.layer_offset();
                Wire::List(vec![
                    Wire::UInt64(opinion.layer().0),
                    Wire::Text(opinion.spec_path().display(&store.paths, &store.tokens)),
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
            ResolvedValue::Scalar(value) => value::encode(&value, None, &store.tokens),
            ResolvedValue::TokenList(items) => Ok(Some(texts(
                "token[]",
                items
                    .into_iter()
                    .map(|token| store.tokens.resolve(token).to_string())
                    .collect(),
            ))),
            ResolvedValue::PathList(items) => Ok(Some(texts(
                "path[]",
                items
                    .into_iter()
                    .map(|target| target.display(&store.paths, &store.tokens))
                    .collect(),
            ))),
            ResolvedValue::Dictionary(entries) => {
                value::encode(&layerstack::Value::Dictionary(entries), None, &store.tokens)
            }
            ResolvedValue::ValueList(items) => {
                value::encode(&layerstack::Value::Array(items), None, &store.tokens)
            }
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
            .map(|target| Wire::Text(target.display(&store.paths, &store.tokens)))
            .collect();
        Ok(Wire::List(vec![
            Wire::Bool(relationship.is_custom()),
            Wire::List(targets),
        ]))
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
    /// Opens a stage ([`Self::open`]).
    #[wasm_bindgen(js_name = open)]
    pub fn js_open(identifier: &str, bytes: Vec<u8>, recover: bool) -> Result<Self, JsValue> {
        Ok(Self::open(identifier, bytes, recover)?)
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

    /// The record `wanted` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = wanted)]
    pub fn js_wanted(&self) -> JsValue {
        self.wanted().into()
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

    /// The record `supplied_bytes` builds, as documented in `stage.rs`.
    #[wasm_bindgen(js_name = suppliedBytes)]
    pub fn js_supplied_bytes(&self) -> u64 {
        self.supplied_bytes()
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
                Wire::text(failure.asset.clone()),
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

/// What asks for an asset, as the wrapper spells it.
fn need_arc(arc: ArcKind) -> &'static str {
    match arc {
        ArcKind::Local => "sublayers",
        other => arc_name(other),
    }
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
        let mut stage = Stage::open("scene.usda", ROOT.as_bytes().to_vec(), false).unwrap();
        assert_eq!(
            identifiers(&stage.wanted()),
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
        assert!(stage.wanted().as_list().unwrap().is_empty());
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
        let stage = Stage::open("scene.usda", ROOT.as_bytes().to_vec(), false).unwrap();
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
        let mut stage = Stage::open("scene.usda", ROOT.as_bytes().to_vec(), false).unwrap();
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
        let stage = Stage::open("scene.usda", ROOT.as_bytes().to_vec(), false).unwrap();
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
        let rejected = Stage::open("bad.usda", b"#usda 1.0\ndef {".to_vec(), false).unwrap_err();
        assert_eq!(rejected.code, "io.rejected");
        assert_eq!(rejected.identifier.as_deref(), Some("bad.usda"));
    }

    #[test]
    fn a_reference_in_a_supplied_layer_is_wanted_next() {
        let mut stage = Stage::open("scene.usda", ROOT.as_bytes().to_vec(), false).unwrap();
        let ball =
            "#usda 1.0\ndef \"Ball\" (\n    prepend references = @./trim.usda@</Trim>\n) {\n}\n";
        let trim = "#usda 1.0\ndef \"Trim\" {\n    token tag = \"trim\"\n}\n";
        let storage = stage.document_mut().storage_mut();
        storage.supply("sub.usda", SUB.as_bytes().to_vec());
        storage.supply("parts/ball.usda", ball.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert_eq!(identifiers(&stage.wanted()), ["parts/trim.usda"]);
        stage
            .document_mut()
            .storage_mut()
            .supply("parts/trim.usda", trim.as_bytes().to_vec());
        stage.load_supplied().unwrap();
        assert!(stage.wanted().as_list().unwrap().is_empty());
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
        let stage = Stage::open("scene.usda", root.as_bytes().to_vec(), false).unwrap();
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
}
