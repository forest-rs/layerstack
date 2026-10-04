// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Stage metadata, including authored/fallback distinctions and guarded edits.
//! AOUSD Core §7.6.1, §12.2.5, §12.2.7; OpenUSD `UsdStage` metadata API.
use super::*;
use crate::{Applied, EditError, LiveStage, Transaction};

/// Why a stage metadata edit was rejected. Rejection changes no authored state.
#[derive(Clone, Debug, PartialEq)]
pub enum StageMetadataError {
    /// Stage metadata may be authored only on the root or primary session layer.
    InvalidLayer(LayerId),
    /// A dictionary path contains no key or an empty component.
    InvalidKeyPath,
    /// The authored field or an intermediate dictionary key is not a dictionary.
    NotADictionary(TokenId),
    /// The underlying atomic transaction failed.
    Edit(EditError),
}
impl core::fmt::Display for StageMetadataError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidLayer(id) => write!(
                f,
                "layer {} is not the stage root or primary session layer",
                id.0
            ),
            Self::InvalidKeyPath => {
                f.write_str("dictionary key paths require nonempty colon-separated components")
            }
            Self::NotADictionary(key) => write!(
                f,
                "metadata {key:?} contains a non-dictionary value along the key path"
            ),
            Self::Edit(error) => error.fmt(f),
        }
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
    pub fn metadata_dict_key(
        &self,
        key: TokenId,
        key_path: &str,
        store: &dyn LayerStore,
    ) -> Option<Value> {
        let keys = dictionary_keys(key_path).ok()?;
        let mut value = self.layer_metadata(key, store)?;
        for key in keys {
            let Value::Dictionary(entries) = value else {
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
                let Some(FieldValue::Value(value)) = layer.metadata(key) else {
                    return false;
                };
                dictionary_get(value, &keys).is_some()
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
    pub fn set_stage_metadata(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        key: TokenId,
        value: FieldValue,
    ) -> Result<Applied, StageMetadataError> {
        self.stage().check_metadata_layer(layer)?;
        let mut edit = Transaction::new();
        edit.set_layer_metadata(layer, key, value);
        self.apply(store, &edit).map_err(StageMetadataError::Edit)
    }
    /// Clears only the selected layer's authored stage metadata field.
    pub fn clear_stage_metadata(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        key: TokenId,
    ) -> Result<Applied, StageMetadataError> {
        self.stage().check_metadata_layer(layer)?;
        let mut edit = Transaction::new();
        edit.clear_layer_metadata(layer, key);
        self.apply(store, &edit).map_err(StageMetadataError::Edit)
    }
    /// Sets or clears one authored dictionary key. The selected layer's other
    /// keys are retained; weaker dictionaries remain available to composition.
    /// Undo guards the whole authored dictionary field, like other metadata edits.
    pub fn set_stage_metadata_dict_key(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        key: TokenId,
        key_path: &str,
        value: Option<Value>,
    ) -> Result<Applied, StageMetadataError> {
        self.stage().check_metadata_layer(layer)?;
        let keys = dictionary_keys(key_path)?;
        let field = store.layer(layer).and_then(|l| l.metadata(key));
        let mut dictionary = match field {
            Some(FieldValue::Value(Value::Dictionary(entries))) => {
                Value::Dictionary(entries.clone())
            }
            None => Value::Dictionary(Vec::new()),
            _ => return Err(StageMetadataError::NotADictionary(key)),
        };
        if value.is_none() && dictionary_get(&dictionary, &keys).is_none() {
            return self
                .apply(store, &Transaction::new())
                .map_err(StageMetadataError::Edit);
        }
        dictionary_set(&mut dictionary, &keys, value, key)?;
        self.set_stage_metadata(store, layer, key, FieldValue::Value(dictionary))
    }
    /// Authors the root layer's default prim, independent of session overrides.
    /// A name is stored as authored; callers can inspect `Stage::default_prim`.
    pub fn set_default_prim(
        &mut self,
        store: &mut dyn LayerStore,
        name: Option<TokenId>,
    ) -> Result<Applied, StageMetadataError> {
        let key = store.tokens_mut().intern("defaultPrim");
        let layer = self.stage().root_layer().expect("live stage root");
        match name {
            Some(name) => {
                self.set_stage_metadata(store, layer, key, FieldValue::Value(Value::Token(name)))
            }
            None => self.clear_stage_metadata(store, layer, key),
        }
    }
    /// Authors a start time code on the selected metadata layer.
    pub fn set_start_time_code(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_time_metadata(store, layer, "startTimeCode", value)
    }
    /// Authors an end time code on the selected metadata layer.
    pub fn set_end_time_code(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_time_metadata(store, layer, "endTimeCode", value)
    }
    /// Authors time codes per second on the selected metadata layer.
    pub fn set_time_codes_per_second(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_time_metadata(store, layer, "timeCodesPerSecond", value)
    }
    /// Authors the intended playback rate on the selected metadata layer.
    pub fn set_frames_per_second(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        self.set_time_metadata(store, layer, "framesPerSecond", value)
    }
    fn set_time_metadata(
        &mut self,
        store: &mut dyn LayerStore,
        layer: LayerId,
        name: &str,
        value: f64,
    ) -> Result<Applied, StageMetadataError> {
        let key = store.tokens_mut().intern(name);
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
fn dictionary_get<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let mut value = value;
    for key in keys {
        let Value::Dictionary(entries) = value else {
            return None;
        };
        value = &entries.iter().find(|(name, _)| name.as_ref() == *key)?.1;
    }
    Some(value)
}
fn dictionary_set(
    value: &mut Value,
    keys: &[&str],
    replacement: Option<Value>,
    field: TokenId,
) -> Result<(), StageMetadataError> {
    let Value::Dictionary(entries) = value else {
        return Err(StageMetadataError::NotADictionary(field));
    };
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
            entries.push((keys[0].into(), Value::Dictionary(Vec::new())));
            entries.len() - 1
        }
    };
    dictionary_set(&mut entries[position].1, &keys[1..], replacement, field)
}
