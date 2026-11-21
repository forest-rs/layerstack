// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage metadata, including authored/fallback distinctions and guarded edits.
//! AOUSD Core §7.6.1, §12.2.5, §12.2.7; OpenUSD `UsdStage` metadata API.
use super::*;
use crate::{
    Applied, EditError, LiveStage, Transaction,
    diagnostic::{Describe, Names, with_store},
};

/// The stage's up axis, as defined by OpenUSD's `UsdGeomSetStageUpAxis`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpAxis {
    /// Positive Y is up (the registered fallback).
    Y,
    /// Positive Z is up.
    Z,
}

impl UpAxis {
    /// The USD token for this axis.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

/// Why a stage metadata edit was rejected. Rejection changes no authored state.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum StageMetadataError {
    /// Stage metadata may be authored only on the root or primary session layer.
    InvalidLayer(LayerId),
    /// A dictionary path contains no key or an empty component.
    InvalidKeyPath,
    /// The authored field or an intermediate dictionary key is not a dictionary.
    NotADictionary(TokenId),
    /// The field cannot be authored as layer metadata, or the value is
    /// not of its type: it is registered for no layer, or not at all
    /// ([`crate::MetadataField::check_edit`]).
    Field(crate::MetadataEditError),
    /// The underlying atomic transaction failed.
    Edit(EditError),
}
impl StageMetadataError {
    /// The error in words, its layer, names and a rejected edit's paths
    /// written as text with `store`, the store the edit was applied to
    /// ([`EditError::display`]). `Display` writes the same without a store,
    /// with ids.
    #[must_use]
    pub fn display<'a>(&'a self, store: &'a dyn LayerStore) -> impl core::fmt::Display + 'a {
        with_store(self, store)
    }
}
impl Describe for StageMetadataError {
    fn describe(&self, f: &mut core::fmt::Formatter<'_>, names: Names<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidLayer(id) => write!(
                f,
                "{} is not the stage root or primary session layer",
                names.layer(*id)
            ),
            Self::InvalidKeyPath => {
                f.write_str("dictionary key paths require nonempty colon-separated components")
            }
            Self::NotADictionary(key) => write!(
                f,
                "metadata `{}` contains a non-dictionary value along the key path",
                names.token(*key)
            ),
            Self::Field(error) => core::fmt::Display::fmt(error, f),
            Self::Edit(error) => error.describe(f, names),
        }
    }
}
impl core::fmt::Display for StageMetadataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.describe(f, Names(None))
    }
}
impl core::error::Error for StageMetadataError {}

impl Stage {
    /// Whether a meaningful authored or registered fallback value exists.
    pub fn has_metadata(&self, key: TokenId, store: &dyn LayerStore) -> bool {
        self.layer_metadata(key, store).is_some()
    }
    /// Whether the root or unmuted primary session layer authors this field.
    /// This excludes registered fallback values and session sublayers.
    pub fn has_authored_metadata(&self, key: TokenId, store: &dyn LayerStore) -> bool {
        core::iter::once(self.root_layer())
            .chain(core::iter::once(
                self.session_layer().filter(|l| !self.is_layer_muted(*l)),
            ))
            .flatten()
            .filter_map(|id| store.layer(id))
            .any(|layer| {
                if store.tokens().resolve(key) == "defaultPrim" {
                    layer.default_prim.is_some()
                } else {
                    layer.metadata(key).is_some()
                }
            })
    }
    /// Reads a colon-separated dictionary key path from composed stage metadata.
    ///
    /// The entry is almost always a plain value ([`FieldValue::as_value`]),
    /// but may hold a list op, as OpenUSD's `UsdStage::GetMetadataByDictKey`
    /// returns whatever the entry holds.
    pub fn metadata_dict_key(
        &self,
        key: TokenId,
        key_path: &str,
        store: &dyn LayerStore,
    ) -> Option<FieldValue> {
        let keys = dictionary_keys(key_path).ok()?;
        let mut value = FieldValue::Value(self.layer_metadata(key, store)?);
        for key in keys {
            let FieldValue::Value(Value::Dictionary(entries)) = value else {
                return None;
            };
            value = entries.into_iter().find(|(k, _)| k.as_ref() == key)?.1;
        }
        Some(value)
    }
    /// Whether the composed dictionary contains the key path.
    pub fn has_metadata_dict_key(
        &self,
        key: TokenId,
        key_path: &str,
        store: &dyn LayerStore,
    ) -> bool {
        self.metadata_dict_key(key, key_path, store).is_some()
    }
    /// Whether a key path is authored in the root or unmuted primary session layer.
    pub fn has_authored_metadata_dict_key(
        &self,
        key: TokenId,
        key_path: &str,
        store: &dyn LayerStore,
    ) -> bool {
        let Ok(keys) = dictionary_keys(key_path) else {
            return false;
        };
        core::iter::once(self.root_layer())
            .chain(core::iter::once(
                self.session_layer().filter(|l| !self.is_layer_muted(*l)),
            ))
            .flatten()
            .filter_map(|id| store.layer(id))
            .any(|layer| {
                let Some(FieldValue::Value(Value::Dictionary(entries))) = layer.metadata(key)
                else {
                    return false;
                };
                dictionary_get(entries, &keys).is_some()
            })
    }
    /// Start time code, defaulting to zero. This is metadata, not a playback cursor.
    pub fn start_time_code(&self, store: &dyn LayerStore) -> f64 {
        self.metadata_double("startTimeCode", 0.0, store)
    }
    /// End time code, defaulting to zero.
    pub fn end_time_code(&self, store: &dyn LayerStore) -> f64 {
        self.metadata_double("endTimeCode", 0.0, store)
    }
    /// Intended playback rate, defaulting to 24 frames per second.
    pub fn frames_per_second(&self, store: &dyn LayerStore) -> f64 {
        self.metadata_double("framesPerSecond", 24.0, store)
    }
    /// Whether both endpoints of the stage's time range are authored.
    pub fn has_authored_time_code_range(&self, store: &dyn LayerStore) -> bool {
        ["startTimeCode", "endTimeCode"].iter().all(|name| {
            store
                .tokens()
                .lookup(name)
                .is_some_and(|key| self.has_authored_metadata(key, store))
        })
    }
    /// Authored or registered color-configuration asset. No color processing is performed.
    pub fn color_configuration(&self, store: &dyn LayerStore) -> Option<Value> {
        self.layer_metadata(store.tokens().lookup("colorConfiguration")?, store)
    }
    /// Authored or registered color-management-system token.
    pub fn color_management_system(&self, store: &dyn LayerStore) -> Option<TokenId> {
        match self.layer_metadata(store.tokens().lookup("colorManagementSystem")?, store)? {
            Value::Token(v) => Some(v),
            _ => None,
        }
    }
    fn metadata_double(&self, name: &str, fallback: f64, store: &dyn LayerStore) -> f64 {
        match store
            .tokens()
            .lookup(name)
            .and_then(|key| self.layer_metadata(key, store))
        {
            Some(Value::Double(v)) => v,
            _ => fallback,
        }
    }
    /// Checks `key` as layer metadata, and `value` as a value of it.
    /// `defaultPrim` is the layer's own member.
    fn check_layer_field(
        &self,
        key: TokenId,
        value: Option<&FieldValue>,
        store: &dyn LayerStore,
    ) -> Result<(), StageMetadataError> {
        let tokens = store.tokens();
        crate::MetadataField::check_edit(
            tokens.resolve(key),
            crate::MetadataTarget::Layer,
            value,
            self.schemas(),
            tokens,
        )
        .map(|_| ())
        .map_err(StageMetadataError::Field)
    }
    fn check_metadata_layer(&self, layer: LayerId) -> Result<(), StageMetadataError> {
        if self.root_layer() == Some(layer) || self.session_layer() == Some(layer) {
            Ok(())
        } else {
            Err(StageMetadataError::InvalidLayer(layer))
        }
    }
}

impl LiveStage {
    /// Sets stage metadata on the explicitly selected root or primary session layer.
    /// The atomic edit returns guarded undo and recomposes pending notifications.
    ///
    /// The field must be registered for layers, built in or in the stage's
    /// schema registry, and `value` must be of its type, as OpenUSD's
    /// `UsdStage::SetMetadata` requires
    /// ([`crate::MetadataField::check_edit`]).
    pub fn set_stage_metadata(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        key: TokenId,
        value: FieldValue,
    ) -> Result<Applied, StageMetadataError> {
        self.stage().check_metadata_layer(layer)?;
        self.stage().check_layer_field(key, Some(&value), store)?;
        let mut edit = Transaction::new();
        edit.set_layer_metadata(layer, key, value);
        self.apply(store, &edit).map_err(StageMetadataError::Edit)
    }
    /// Clears only the selected layer's authored stage metadata field, a
    /// field registered for layers ([`Self::set_stage_metadata`]).
    pub fn clear_stage_metadata(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        key: TokenId,
    ) -> Result<Applied, StageMetadataError> {
        self.stage().check_metadata_layer(layer)?;
        self.stage().check_layer_field(key, None, store)?;
        let mut edit = Transaction::new();
        edit.clear_layer_metadata(layer, key);
        self.apply(store, &edit).map_err(StageMetadataError::Edit)
    }
    /// Sets or clears one authored dictionary key. The selected layer's other
    /// keys are retained; weaker dictionaries remain available to composition.
    /// Undo guards the whole authored dictionary field, like other metadata edits.
    ///
    /// The entry may hold a plain value or a list op, as OpenUSD's
    /// `UsdStage::SetMetadataByDictKey` sets any value.
    pub fn set_stage_metadata_dict_key(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        key: TokenId,
        key_path: &str,
        value: Option<FieldValue>,
    ) -> Result<Applied, StageMetadataError> {
        self.stage().check_metadata_layer(layer)?;
        let keys = dictionary_keys(key_path)?;
        let field = store.layer(layer).and_then(|l| l.metadata(key));
        let mut dictionary = match field {
            Some(FieldValue::Value(Value::Dictionary(entries))) => entries.clone(),
            None => Vec::new(),
            _ => return Err(StageMetadataError::NotADictionary(key)),
        };
        if value.is_none() && dictionary_get(&dictionary, &keys).is_none() {
            return self
                .apply(store, &Transaction::new())
                .map_err(StageMetadataError::Edit);
        }
        dictionary_set(&mut dictionary, &keys, value, key)?;
        self.set_stage_metadata(store, layer, key, Value::Dictionary(dictionary).into())
    }
    /// Authors the root layer's default prim, independent of session overrides.
    /// A name is stored as authored; callers can inspect `Stage::default_prim`.
    pub fn set_default_prim(
        &mut self,
        store: &mut dyn LayerStore,
        name: Option<TokenId>,
    ) -> Result<Applied, StageMetadataError> {
        let key = store.tokens().intern("defaultPrim");
        let layer = self.stage().root_layer().expect("live stage root");
        match name {
            Some(name) => {
                self.set_stage_metadata(store, layer, key, FieldValue::Value(Value::Token(name)))
            }
            None => self.clear_stage_metadata(store, layer, key),
        }
    }
    /// Authors `upAxis` on the root or primary session layer. The primary
    /// session layer's opinion is stronger than the root layer's.
    ///
    /// Matches OpenUSD's `UsdGeomSetStageUpAxis`, whose allowed tokens are
    /// `Y` and `Z`. The edit uses the registered layer metadata field
    /// ([`Self::set_stage_metadata`]) and returns a guarded inverse.
    pub fn set_up_axis(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        axis: UpAxis,
    ) -> Result<Applied, StageMetadataError> {
        let key = store.tokens().intern("upAxis");
        let value = Value::Token(store.tokens().intern(axis.token()));
        self.set_stage_metadata(store, layer, key, value.into())
    }
    /// Authors `metersPerUnit`, the length of one scene unit in meters,
    /// on the root or primary session layer. Use `1.0` for meters or
    /// `0.01` for centimeters.
    ///
    /// Matches OpenUSD's `UsdGeomSetStageMetersPerUnit`, which stores the
    /// supplied double as-is. The edit uses the registered layer metadata
    /// field ([`Self::set_stage_metadata`]) and returns a guarded inverse.
    pub fn set_meters_per_unit(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_double_metadata(store, layer, "metersPerUnit", value)
    }
    /// Authors a start time code on the selected metadata layer.
    pub fn set_start_time_code(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_double_metadata(store, layer, "startTimeCode", value)
    }
    /// Authors an end time code on the selected metadata layer.
    pub fn set_end_time_code(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_double_metadata(store, layer, "endTimeCode", value)
    }
    /// Authors time codes per second on the selected metadata layer.
    pub fn set_time_codes_per_second(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_double_metadata(store, layer, "timeCodesPerSecond", value)
    }
    /// Authors the intended playback rate on the selected metadata layer.
    pub fn set_frames_per_second(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_double_metadata(store, layer, "framesPerSecond", value)
    }
    fn set_double_metadata(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        name: &str,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        let key = store.tokens().intern(name);
        self.set_stage_metadata(store, layer, key, FieldValue::Value(Value::Double(value)))
    }
}

fn dictionary_keys(path: &str) -> Result<Vec<&str>, StageMetadataError> {
    let keys: Vec<_> = path.split(':').collect();
    if keys.iter().any(|key| key.is_empty()) {
        Err(StageMetadataError::InvalidKeyPath)
    } else {
        Ok(keys)
    }
}
/// The entry at key path `keys` in `entries`, a dictionary's.
fn dictionary_get<'a>(
    entries: &'a [(Arc<str>, FieldValue)],
    keys: &[&str],
) -> Option<&'a FieldValue> {
    let (first, rest) = keys.split_first()?;
    let value = &entries.iter().find(|(name, _)| name.as_ref() == *first)?.1;
    if rest.is_empty() {
        return Some(value);
    }
    let FieldValue::Value(Value::Dictionary(entries)) = value else {
        return None;
    };
    dictionary_get(entries, rest)
}
fn dictionary_set(
    entries: &mut Vec<(Arc<str>, FieldValue)>,
    keys: &[&str],
    replacement: Option<FieldValue>,
    field: TokenId,
) -> Result<(), StageMetadataError> {
    let position = entries.iter().position(|(key, _)| key.as_ref() == keys[0]);
    if keys.len() == 1 {
        match (position, replacement) {
            (Some(i), Some(v)) => entries[i].1 = v,
            (Some(i), None) => {
                entries.remove(i);
            }
            (None, Some(v)) => entries.push((keys[0].into(), v)),
            (None, None) => {}
        }
        return Ok(());
    }
    let position = match position {
        Some(i) => i,
        None if replacement.is_none() => return Ok(()),
        None => {
            entries.push((keys[0].into(), Value::Dictionary(Vec::new()).into()));
            entries.len() - 1
        }
    };
    let FieldValue::Value(Value::Dictionary(entries)) = &mut entries[position].1 else {
        return Err(StageMetadataError::NotADictionary(field));
    };
    dictionary_set(entries, &keys[1..], replacement, field)
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use crate::{InMemoryStore, Layer, SpecPath, SublayerEntry, edit::Rejection};

    #[test]
    fn geometry_metrics_use_checked_stage_layers_and_registered_types() {
        let mut store = InMemoryStore::default();
        let (root, session, sublayer) = (LayerId(1), LayerId(2), LayerId(3));
        let axis_key = store.tokens().intern("upAxis");
        let units_key = store.tokens().intern("metersPerUnit");
        let y = store.tokens().intern("Y");
        let z = store.tokens().intern("Z");
        let mut weak = Layer::new(sublayer);
        weak.set_metadata(axis_key, Value::Token(z));
        weak.set_metadata(units_key, Value::Double(1000.0));
        store.insert_layer(weak);
        let mut root_layer = Layer::new(root);
        root_layer
            .sublayers_mut()
            .push(SublayerEntry::new(sublayer));
        store.insert_layer(root_layer);
        store.insert_layer(Layer::new(session));
        let mut live = LiveStage::compose(
            &mut store,
            root,
            StageOptions {
                session_layer: Some(session),
                ..StageOptions::default()
            },
        );

        // The usdGeom declarations are built in. Only the root and
        // primary session layer override the registered stage fallbacks.
        let metrics = |live: &LiveStage, store: &InMemoryStore| {
            (
                live.stage().layer_metadata(axis_key, store),
                live.stage().layer_metadata(units_key, store),
            )
        };
        assert_eq!(
            metrics(&live, &store),
            (Some(Value::Token(y)), Some(Value::Double(0.01))),
            "stage metrics use the registered fallbacks"
        );
        assert!(
            matches!(
                live.set_up_axis(&mut store, sublayer, UpAxis::Y),
                Err(StageMetadataError::InvalidLayer(layer)) if layer == sublayer
            ),
            "a sublayer cannot author the stage axis"
        );
        assert!(
            matches!(
                live.set_meters_per_unit(&mut store, sublayer, 1.0),
                Err(StageMetadataError::InvalidLayer(layer)) if layer == sublayer
            ),
            "a sublayer cannot author stage units"
        );
        let weak = store.layer(sublayer).unwrap();
        assert_eq!(
            weak.metadata(axis_key),
            Some(&Value::Token(z).into()),
            "a rejected edit leaves the authored axis alone"
        );
        assert_eq!(
            weak.metadata(units_key),
            Some(&Value::Double(1000.0).into()),
            "a rejected edit leaves the authored units alone"
        );

        live.set_up_axis(&mut store, root, UpAxis::Y).unwrap();
        live.set_meters_per_unit(&mut store, root, 1.0).unwrap();
        let axis = live.set_up_axis(&mut store, session, UpAxis::Z).unwrap();
        let units = live
            .set_meters_per_unit(&mut store, session, 0.001)
            .unwrap();
        assert_eq!(
            metrics(&live, &store),
            (Some(Value::Token(z)), Some(Value::Double(0.001))),
            "session opinions are strongest and use token and double values"
        );
        live.apply(&mut store, &units.inverse).unwrap();
        live.apply(&mut store, &axis.inverse).unwrap();
        assert_eq!(
            metrics(&live, &store),
            (Some(Value::Token(y)), Some(Value::Double(1.0))),
            "undoing the session edits reveals both root opinions"
        );
    }

    #[test]
    fn geometry_units_author_the_supplied_double() {
        let mut store = InMemoryStore::default();
        let root = LayerId(1);
        store.insert_layer(Layer::new(root));
        let mut live = LiveStage::compose(&mut store, root, StageOptions::default());
        let key = store.tokens().intern("metersPerUnit");
        for value in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            live.set_meters_per_unit(&mut store, root, value).unwrap();
            let Some(Value::Double(authored)) = live.stage().layer_metadata(key, &store) else {
                panic!("the authored units must be a double");
            };
            assert_eq!(
                authored.to_bits(),
                value.to_bits(),
                "preserve the given value"
            );
        }
    }

    #[test]
    fn errors_name_layers_tokens_and_paths_with_the_store() {
        let mut store = InMemoryStore::default();
        store.set_layer_identifier(LayerId(2), "./session.usda");
        let key = store.tokens().intern("customLayerData");
        let size = SpecPath::from_property_path(store.property_path("/World/Rock.size"));
        let cases = [
            (
                StageMetadataError::InvalidLayer(LayerId(2)),
                "@./session.usda@ is not the stage root or primary session layer",
            ),
            (
                StageMetadataError::NotADictionary(key),
                "metadata `customLayerData` contains a non-dictionary value along the key path",
            ),
            (
                StageMetadataError::Edit(EditError::Rejected {
                    op: 0,
                    reason: Rejection::UndeclaredType(size),
                }),
                "edit 0 rejected: /World/Rock.size declares no type; create it with a type first",
            ),
        ];
        for (error, text) in cases {
            assert_eq!(error.display(&store).to_string(), text);
        }
    }
}
