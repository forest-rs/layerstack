// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Object, property, attribute and prim UI hints.
//!
//! Hints describe presentation only; expression evaluation belongs to a UI.
//! Read precedence and dictionary paths match OpenUSD 26.8 `UsdUI*Hints`.
//! Authoring updates only local dictionary entries and uses explicit legacy
//! mirroring, rather than process environment variables.
//! AOUSD Core §12.2.5 (dictionary metadata), §12.3 (attribute values).
use crate::{PrimView, Scene, SchemaEdit};
use alloc::{string::String, sync::Arc, vec::Vec};
use layerstack::{PathId, PropertyKind, PropertyPath, TargetPath, Value};

/// Invalid UI hint target or data. Failed setters author nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiHintError {
    /// The prim or property does not exist.
    MissingObject(TargetPath),
    /// This hint requires a prim, property, or attribute of another kind.
    WrongObjectKind,
    /// An expanded-group dictionary contains a value other than bool.
    InvalidExpandedGroup(String),
    /// A shown-if dictionary contains a value other than string.
    InvalidShownIfGroup(String),
    /// No value is stored for the requested label.
    MissingValueLabel(String),
    /// A labeled value is incompatible with the attribute's declared type.
    IncompatibleValueLabel(String),
}
impl core::fmt::Display for UiHintError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "UI hint: {self:?}")
    }
}
impl core::error::Error for UiHintError {}

/// A dictionary in OpenUSD's ordered metadata representation.
pub type HintDictionary = Vec<(Arc<str>, Value)>;
fn dictionary(value: Option<Value>) -> HintDictionary {
    match value {
        Some(Value::Dictionary(entries)) => entries,
        _ => Vec::new(),
    }
}
pub(crate) fn entry<'a>(dictionary: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let (key, rest) = keys.split_first()?;
    let Value::Dictionary(entries) = dictionary else {
        return None;
    };
    let value = &entries.iter().find(|(name, _)| name.as_ref() == *key)?.1;
    if rest.is_empty() {
        Some(value)
    } else {
        entry(value, rest)
    }
}
pub(crate) fn set_dictionary_path(dictionary: &mut Value, keys: &[&str], value: Value) {
    let Some((key, rest)) = keys.split_first() else {
        return;
    };
    if !matches!(dictionary, Value::Dictionary(_)) {
        *dictionary = Value::Dictionary(Vec::new());
    }
    let Value::Dictionary(entries) = dictionary else {
        unreachable!()
    };
    let i = match entries.iter().position(|(name, _)| name.as_ref() == *key) {
        Some(i) => i,
        None => {
            entries.push((Arc::from(*key), Value::Dictionary(Vec::new())));
            entries.len() - 1
        }
    };
    if rest.is_empty() {
        entries[i].1 = value;
    } else {
        set_dictionary_path(&mut entries[i].1, rest, value);
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
}
fn string(value: Option<Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(String::from(s.as_ref())),
        _ => None,
    }
}

/// Composed hints on a valid prim, relationship, or attribute. Construction
/// checks existence; typed hint operations check the appropriate object kind.
#[derive(Clone, Copy, Debug)]
pub struct UiHints<'a> {
    scene: Scene<'a>,
    target: TargetPath,
    kind: Option<PropertyKind>,
}
impl<'a> UiHints<'a> {
    /// Hints on an existing prim.
    #[must_use]
    pub fn prim(scene: &Scene<'a>, path: PathId) -> Option<Self> {
        scene.stage().has_prim(path).then_some(Self {
            scene: *scene,
            target: TargetPath::Prim(path),
            kind: None,
        })
    }
    /// Hints on an existing property, including a schema-defined property.
    #[must_use]
    pub fn property(scene: &Scene<'a>, path: PropertyPath) -> Option<Self> {
        let name = scene.store().tokens().resolve(path.property());
        PrimView::new(*scene, path.prim_path()).property_metadata(name)?;
        let kind = scene
            .stage()
            .resolve_property_declaration(path.prim_path(), path.property())
            .map(|d| d.kind)
            .or_else(|| {
                scene
                    .stage()
                    .property_definition_ref(path.prim_path(), path.property())
                    .map(|d| d.kind)
            })?;
        Some(Self {
            scene: *scene,
            target: TargetPath::Property(path),
            kind: Some(kind),
        })
    }
    /// The object whose hints are read.
    #[must_use]
    pub fn target(&self) -> TargetPath {
        self.target
    }
    fn metadata(&self, name: &str) -> Option<Value> {
        // OpenUSD still reads deprecated displayName/displayGroup/hidden even
        // when they are absent from the generated registered-field inventory.
        let key = self.scene.store().tokens().lookup(name)?;
        let resolved = match self.target {
            TargetPath::Prim(path) => self.scene.stage().resolve_value(path, key),
            TargetPath::Property(path) => {
                self.scene
                    .stage()
                    .resolve_property_metadata(path.prim_path(), path.property(), key)
            }
        }?;
        match resolved.value {
            layerstack::ResolvedValue::Scalar(value) => Some(value),
            layerstack::ResolvedValue::Dictionary(entries) => Some(Value::Dictionary(entries)),
            _ => None,
        }
    }
    fn hint(&self, keys: &[&str]) -> Option<Value> {
        entry(&self.metadata("uiHints")?, keys).cloned()
    }
    /// Display name from uiHints; falls back to legacy displayName, then empty.
    #[must_use]
    pub fn display_name(&self) -> String {
        string(self.hint(&["displayName"]))
            .or_else(|| string(self.metadata("displayName")))
            .unwrap_or_default()
    }
    /// Hidden state from uiHints, then legacy hidden, then false.
    #[must_use]
    pub fn hidden(&self) -> bool {
        match self.hint(&["hidden"]) {
            Some(Value::Bool(v)) => v,
            _ => matches!(self.metadata("hidden"), Some(Value::Bool(true))),
        }
    }
    /// Property display group, falling back to legacy displayGroup.
    pub fn display_group(&self) -> Result<String, UiHintError> {
        if self.kind.is_none() {
            return Err(UiHintError::WrongObjectKind);
        }
        Ok(string(self.hint(&["displayGroup"]))
            .or_else(|| string(self.metadata("displayGroup")))
            .unwrap_or_default())
    }
    /// Property visibility expression; empty means no authored condition.
    pub fn shown_if(&self) -> Result<String, UiHintError> {
        if self.kind.is_none() {
            return Err(UiHintError::WrongObjectKind);
        }
        Ok(string(self.hint(&["shownIf"])).unwrap_or_default())
    }
    /// Attribute labels mapping arbitrary names to candidate attribute values.
    pub fn value_labels(&self) -> Result<HintDictionary, UiHintError> {
        if self.kind != Some(PropertyKind::Attribute) {
            return Err(UiHintError::WrongObjectKind);
        }
        Ok(dictionary(self.hint(&["valueLabels"])))
    }
    /// Attribute label ordering, preserving duplicates and authored order.
    pub fn value_labels_order(&self) -> Result<Vec<String>, UiHintError> {
        if self.kind != Some(PropertyKind::Attribute) {
            return Err(UiHintError::WrongObjectKind);
        }
        Ok(self
            .hint(&["valueLabelsOrder"])
            .and_then(|value| {
                crate::value::read_array(
                    &value,
                    self.scene.store().tokens(),
                    crate::value::read_token,
                )
            })
            .unwrap_or_default()
            .into_iter()
            .map(String::from)
            .collect())
    }
    /// Prim expanded-group dictionary. Nested group names remain literal keys.
    pub fn display_groups_expanded(&self) -> Result<HintDictionary, UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        Ok(dictionary(self.hint(&["displayGroupsExpanded"])))
    }
    /// A literal group key's expanded state; wrong types and absence mean false.
    pub fn display_group_expanded(&self, group: &str) -> Result<bool, UiHintError> {
        Ok(self
            .display_groups_expanded()?
            .iter()
            .any(|(name, value)| name.as_ref() == group && matches!(value, Value::Bool(true))))
    }
    /// Prim group visibility expressions.
    pub fn display_groups_shown_if(&self) -> Result<HintDictionary, UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        Ok(dictionary(self.hint(&["displayGroupsShownIf"])))
    }
    /// A group's visibility expression, following colon-delimited dictionary
    /// paths as C++ does (unlike expanded groups, which use literal keys).
    pub fn display_group_shown_if(&self, group: &str) -> Result<String, UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        let mut keys = alloc::vec!["displayGroupsShownIf"];
        keys.extend(group.split(':'));
        Ok(string(self.hint(&keys)).unwrap_or_default())
    }
    /// An authoring handle retaining this object's kind.
    #[must_use]
    pub fn edit(&self) -> UiHintsEdit {
        UiHintsEdit {
            target: self.target,
            kind: self.kind,
            write_legacy: true,
        }
    }
}

/// UI hint authoring handle. Setters update mapped local dictionaries, including
/// earlier setters in the same `SchemaEdit`, without copying weaker opinions.
#[derive(Clone, Copy, Debug)]
pub struct UiHintsEdit {
    target: TargetPath,
    kind: Option<PropertyKind>,
    write_legacy: bool,
}
impl UiHintsEdit {
    /// A prim handle for an existing prim or one defined earlier in this edit.
    pub fn prim(edit: &SchemaEdit<'_>, path: PathId) -> Result<Self, UiHintError> {
        if !edit.exists(path) {
            return Err(UiHintError::MissingObject(TargetPath::Prim(path)));
        }
        Ok(Self {
            target: TargetPath::Prim(path),
            kind: None,
            write_legacy: true,
        })
    }
    /// A property handle for an existing or newly created property.
    pub fn property(edit: &mut SchemaEdit<'_>, path: PropertyPath) -> Result<Self, UiHintError> {
        let name = String::from(edit.tokens().resolve(path.property()));
        let kind = edit
            .property_kind(path.prim_path(), &name)
            .ok_or(UiHintError::MissingObject(TargetPath::Property(path)))?;
        Ok(Self {
            target: TargetPath::Property(path),
            kind: Some(kind),
            write_legacy: true,
        })
    }
    /// Authors the value under a colon-delimited label path at default time.
    /// Returns errors for missing labels and incompatible declared types.
    pub fn apply_value_label(
        &self,
        edit: &mut SchemaEdit<'_>,
        label: &str,
    ) -> Result<(), UiHintError> {
        if self.kind != Some(PropertyKind::Attribute) {
            return Err(UiHintError::WrongObjectKind);
        }
        let TargetPath::Property(path) = self.target else {
            return Err(UiHintError::WrongObjectKind);
        };
        let mut keys = alloc::vec!["valueLabels"];
        keys.extend(label.split(':'));
        let value = edit
            .ui_hint(self.target, &keys)
            .ok_or_else(|| UiHintError::MissingValueLabel(label.into()))?;
        let name = String::from(edit.tokens().resolve(path.property()));
        let ty = edit
            .attribute_type(path.prim_path(), &name)
            .ok_or(UiHintError::WrongObjectKind)?;
        if !compatible(&value, &ty) {
            return Err(UiHintError::IncompatibleValueLabel(label.into()));
        }
        edit.set_value(path.prim_path(), &name, None, value);
        Ok(())
    }
    /// Whether display name, hidden, and display group also write legacy fields.
    /// Enabled by default, matching OpenUSD's default environment setting.
    #[must_use]
    pub fn with_legacy_fields(self, write_legacy: bool) -> Self {
        Self {
            write_legacy,
            ..self
        }
    }
    /// Sets the display name, optionally mirroring legacy displayName.
    pub fn set_display_name(&self, edit: &mut SchemaEdit<'_>, name: &str) {
        edit.update_ui_hint(self.target, &["displayName"], Value::string(name));
        if self.write_legacy {
            edit.set_ui_metadata(self.target, "displayName", Value::string(name));
        }
    }
    /// Sets hidden state, optionally mirroring legacy hidden.
    pub fn set_hidden(&self, edit: &mut SchemaEdit<'_>, hidden: bool) {
        edit.update_ui_hint(self.target, &["hidden"], Value::Bool(hidden));
        if self.write_legacy {
            edit.set_ui_metadata(self.target, "hidden", Value::Bool(hidden));
        }
    }
    /// Sets a property's group, optionally mirroring legacy displayGroup.
    pub fn set_display_group(
        &self,
        edit: &mut SchemaEdit<'_>,
        group: &str,
    ) -> Result<(), UiHintError> {
        if self.kind.is_none() {
            return Err(UiHintError::WrongObjectKind);
        }
        edit.update_ui_hint(self.target, &["displayGroup"], Value::string(group));
        if self.write_legacy {
            edit.set_ui_metadata(self.target, "displayGroup", Value::string(group));
        }
        Ok(())
    }
    /// Sets a property's visibility expression.
    pub fn set_shown_if(
        &self,
        edit: &mut SchemaEdit<'_>,
        expression: &str,
    ) -> Result<(), UiHintError> {
        if self.kind.is_none() {
            return Err(UiHintError::WrongObjectKind);
        }
        edit.update_ui_hint(self.target, &["shownIf"], Value::string(expression));
        Ok(())
    }
    /// Sets an attribute's value labels. Values need not match the attribute
    /// type until a label is applied, matching C++ `SetValueLabels`.
    pub fn set_value_labels(
        &self,
        edit: &mut SchemaEdit<'_>,
        labels: HintDictionary,
    ) -> Result<(), UiHintError> {
        if self.kind != Some(PropertyKind::Attribute) {
            return Err(UiHintError::WrongObjectKind);
        }
        edit.update_ui_hint(self.target, &["valueLabels"], Value::Dictionary(labels));
        Ok(())
    }
    /// Sets an attribute's label ordering without deduplication.
    pub fn set_value_labels_order(
        &self,
        edit: &mut SchemaEdit<'_>,
        order: &[&str],
    ) -> Result<(), UiHintError> {
        if self.kind != Some(PropertyKind::Attribute) {
            return Err(UiHintError::WrongObjectKind);
        }
        let value = crate::value::write_array(order, edit.tokens(), crate::value::write_token);
        edit.update_ui_hint(self.target, &["valueLabelsOrder"], value);
        Ok(())
    }
    /// Sets bool-valued literal group expansion entries. Rejects the whole
    /// dictionary before authoring if any value is not bool.
    pub fn set_display_groups_expanded(
        &self,
        edit: &mut SchemaEdit<'_>,
        expanded: HintDictionary,
    ) -> Result<(), UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        for (name, value) in &expanded {
            if !matches!(value, Value::Bool(_)) {
                return Err(UiHintError::InvalidExpandedGroup(String::from(
                    name.as_ref(),
                )));
            }
        }
        edit.update_ui_hint(
            self.target,
            &["displayGroupsExpanded"],
            Value::Dictionary(expanded),
        );
        Ok(())
    }
    /// Sets one expanded group using a literal key, including embedded colons.
    pub fn set_display_group_expanded(
        &self,
        edit: &mut SchemaEdit<'_>,
        group: &str,
        expanded: bool,
    ) -> Result<(), UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        edit.update_ui_hint(
            self.target,
            &["displayGroupsExpanded", group],
            Value::Bool(expanded),
        );
        Ok(())
    }
    /// Sets string-valued group expressions, rejecting wrong types atomically.
    pub fn set_display_groups_shown_if(
        &self,
        edit: &mut SchemaEdit<'_>,
        conditions: HintDictionary,
    ) -> Result<(), UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        for (name, value) in &conditions {
            if !matches!(value, Value::String(_)) {
                return Err(UiHintError::InvalidShownIfGroup(String::from(
                    name.as_ref(),
                )));
            }
        }
        edit.update_ui_hint(
            self.target,
            &["displayGroupsShownIf"],
            Value::Dictionary(conditions),
        );
        Ok(())
    }
    /// Sets one group expression with colon-delimited nested dictionary paths.
    pub fn set_display_group_shown_if(
        &self,
        edit: &mut SchemaEdit<'_>,
        group: &str,
        expression: &str,
    ) -> Result<(), UiHintError> {
        if self.kind.is_some() {
            return Err(UiHintError::WrongObjectKind);
        }
        let mut keys = alloc::vec!["displayGroupsShownIf"];
        keys.extend(group.split(':'));
        edit.update_ui_hint(self.target, &keys, Value::string(expression));
        Ok(())
    }
}

fn compatible(value: &Value, ty: &layerstack::PropertyType) -> bool {
    let same = |value: &Value| {
        core::mem::discriminant(value) == core::mem::discriminant(&ty.default_scalar)
    };
    if ty.is_array {
        value.array_ref().is_some_and(|array| {
            array
                .typed()
                .is_none_or(|typed| same(&typed.element_kind()))
                && array.iter().all(|value| same(&value))
        })
    } else {
        same(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use layerstack::edit::EditTarget;
    use layerstack::{
        InMemoryStore, Layer, LayerId, LiveStage, PrimSpec, PropertySpec, PropertyType,
        StageOptions,
    };
    fn setup() -> (InMemoryStore, LiveStage, PathId, PropertyPath) {
        let mut store = InMemoryStore::default();
        let path = store.path("/P");
        let attr = store.tokens.intern("value");
        let ui = store.tokens.intern("uiHints");
        let mut layer = Layer::new(LayerId(1));
        layer.insert_prim(
            path,
            PrimSpec::def()
                .with_field(
                    ui,
                    Value::Dictionary(vec![("unrelated".into(), Value::Int(17))]),
                )
                .with_property(
                    attr,
                    PropertySpec::typed_attribute(PropertyType::new(
                        "float",
                        false,
                        Value::Float(0.),
                    ))
                    .with_default(Value::Float(1.)),
                ),
        );
        store.insert_layer(layer);
        let schemas = crate::openusd(&mut store.tokens);
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        (store, live, path, PropertyPath::new(path, attr))
    }
    #[test]
    fn multiple_setters_preserve_local_hints_and_group_colons() {
        let (mut store, mut live, path, _) = setup();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let hints = UiHintsEdit::prim(&edit, path).unwrap();
        hints.set_display_name(&mut edit, "Object");
        hints.set_hidden(&mut edit, true);
        hints
            .set_display_group_expanded(&mut edit, "A:B", true)
            .unwrap();
        hints
            .set_display_group_shown_if(&mut edit, "A:B", "value > 0")
            .unwrap();
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let hints = UiHints::prim(&scene, path).unwrap();
        assert_eq!(hints.display_name(), "Object");
        assert!(hints.hidden());
        assert!(hints.display_group_expanded("A:B").unwrap());
        assert!(!hints.display_group_expanded("A").unwrap());
        assert_eq!(hints.display_group_shown_if("A:B").unwrap(), "value > 0");
        assert_eq!(hints.hint(&["unrelated"]), Some(Value::Int(17)));
        assert_eq!(hints.metadata("displayName"), Some(Value::string("Object")));
    }
    #[test]
    fn labels_apply_and_invalid_setters_are_atomic() {
        let (mut store, mut live, path, attr) = setup();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let hint = UiHintsEdit::property(&mut edit, attr).unwrap();
        hint.set_value_labels(
            &mut edit,
            vec![
                ("High".into(), Value::Float(8.)),
                ("Bad".into(), Value::string("wrong")),
            ],
        )
        .unwrap();
        hint.set_value_labels_order(&mut edit, &["High", "Bad", "High"])
            .unwrap();
        hint.set_display_group(&mut edit, "Settings").unwrap();
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let hints = UiHints::property(&scene, attr).unwrap();
        assert_eq!(
            hints.value_labels_order().unwrap(),
            vec!["High", "Bad", "High"]
        );
        assert_eq!(hints.display_group().unwrap(), "Settings");
        let hints = hints.edit();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        assert!(matches!(
            hints.apply_value_label(&mut edit, "Bad"),
            Err(UiHintError::IncompatibleValueLabel(_))
        ));
        assert!(matches!(
            hints.apply_value_label(&mut edit, "Missing"),
            Err(UiHintError::MissingValueLabel(_))
        ));
        assert!(edit.transaction().is_empty());
        hints.apply_value_label(&mut edit, "High").unwrap();
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        assert_eq!(
            PrimView::new(Scene::new(live.stage(), &store), path)
                .read_value("value", crate::value::read_float),
            Some(8.)
        );
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let hints = UiHintsEdit::prim(&edit, path).unwrap();
        assert!(
            hints
                .set_display_groups_expanded(&mut edit, vec![("bad".into(), Value::Int(1))])
                .is_err()
        );
        assert!(
            hints
                .set_display_groups_shown_if(&mut edit, vec![("bad".into(), Value::Bool(true))])
                .is_err()
        );
        assert!(edit.transaction().is_empty());
    }
    #[test]
    fn stronger_ui_authoring_does_not_freeze_weaker_hints() {
        let (mut store, _, path, attr) = setup();
        let mut root = Layer::new(LayerId(2));
        root.sublayers
            .push(layerstack::SublayerEntry::new(LayerId(1)));
        store.insert_layer(root);
        let schemas = crate::openusd(&mut store.tokens);
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(2),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(2)));
        UiHintsEdit::prim(&edit, path)
            .unwrap()
            .set_hidden(&mut edit, true);
        UiHintsEdit::property(&mut edit, attr)
            .unwrap()
            .set_display_name(&mut edit, "Value");
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        assert_eq!(
            UiHints::prim(&scene, path).unwrap().hint(&["unrelated"]),
            Some(Value::Int(17))
        );
        let key = store.tokens.lookup("uiHints").unwrap();
        let local = store.layers[&LayerId(2)].prims[&path].field(key).unwrap();
        let layerstack::FieldValue::Value(local) = local else {
            panic!("dictionary")
        };
        assert!(entry(local, &["unrelated"]).is_none());
        assert_eq!(
            UiHints::property(&scene, attr).unwrap().display_name(),
            "Value"
        );
    }
    #[test]
    fn label_authored_and_applied_in_one_edit() {
        let (mut store, mut live, path, attr) = setup();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let hints = UiHintsEdit::property(&mut edit, attr).unwrap();
        hints
            .set_value_labels(
                &mut edit,
                vec![(
                    "Nested".into(),
                    Value::Dictionary(vec![("High".into(), Value::Float(9.))]),
                )],
            )
            .unwrap();
        hints.apply_value_label(&mut edit, "Nested:High").unwrap();
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        assert_eq!(
            PrimView::new(Scene::new(live.stage(), &store), path)
                .read_value("value", crate::value::read_float),
            Some(9.)
        );
    }
    #[test]
    fn mapped_reference_edit_preserves_local_and_weaker_entries() {
        let (mut store, _, source, _) = setup();
        let instance = store.path("/Instance");
        let ui = store.tokens.intern("uiHints");
        let mut weak = Layer::new(LayerId(3));
        weak.insert_prim(
            source,
            PrimSpec::over()
                .with_field(ui, Value::Dictionary(vec![("weak".into(), Value::Int(4))])),
        );
        store
            .layers
            .get_mut(&LayerId(1))
            .unwrap()
            .sublayers
            .push(layerstack::SublayerEntry::new(LayerId(3)));
        store.insert_layer(weak);
        let mut root = Layer::new(LayerId(2));
        root.insert_prim(
            instance,
            PrimSpec::def().with_reference(layerstack::Reference::new(LayerId(1), source)),
        );
        store.insert_layer(root);
        let schemas = crate::openusd(&mut store.tokens);
        let mut live = LiveStage::compose(
            &mut store,
            LayerId(2),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        let graph = live.stage().explain_prim_graph(instance).unwrap();
        let node = graph
            .nodes()
            .find(|(_, node)| node.arc_kind() == layerstack::prim_index::ArcKind::References)
            .unwrap()
            .0;
        let target = EditTarget::for_node(live.stage(), instance, node).unwrap();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, target);
        let hints = UiHintsEdit::prim(&edit, instance).unwrap();
        hints.set_display_name(&mut edit, "Reference");
        hints.set_hidden(&mut edit, true);
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let scene = Scene::new(live.stage(), &store);
        let hints = UiHints::prim(&scene, instance).unwrap();
        assert_eq!(hints.hint(&["unrelated"]), Some(Value::Int(17)));
        assert_eq!(hints.hint(&["weak"]), Some(Value::Int(4)));
        assert_eq!(hints.display_name(), "Reference");
        assert!(hints.hidden());
        let layerstack::FieldValue::Value(local) =
            store.layers[&LayerId(1)].prims[&source].field(ui).unwrap()
        else {
            panic!("dictionary")
        };
        assert!(entry(local, &["weak"]).is_none());
        assert!(entry(local, &["unrelated"]).is_some());
        assert!(
            store.layers[&LayerId(2)].prims[&instance]
                .field(ui)
                .is_none()
        );
    }
    #[test]
    fn legacy_read_fallback_and_explicit_legacy_write_policy() {
        let (mut store, mut live, path, _) = setup();
        let display = store.tokens.intern("displayName");
        let hidden = store.tokens.intern("hidden");
        let mut transaction = layerstack::Transaction::new();
        transaction.set_metadata(
            EditTarget::for_layer(LayerId(1)).prim(path),
            display,
            Value::string("Legacy").into(),
        );
        transaction.set_metadata(
            EditTarget::for_layer(LayerId(1)).prim(path),
            hidden,
            Value::Bool(true).into(),
        );
        live.apply(&mut store, &transaction).unwrap();
        let hints = UiHints::prim(&Scene::new(live.stage(), &store), path).unwrap();
        assert_eq!(hints.display_name(), "Legacy");
        assert!(hints.hidden());
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(LayerId(1)));
        let hints = UiHintsEdit::prim(&edit, path)
            .unwrap()
            .with_legacy_fields(false);
        hints.set_display_name(&mut edit, "");
        hints.set_hidden(&mut edit, false);
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        let hints = UiHints::prim(&Scene::new(live.stage(), &store), path).unwrap();
        assert_eq!(hints.display_name(), "");
        assert!(!hints.hidden());
        assert_eq!(hints.metadata("displayName"), Some(Value::string("Legacy")));
        assert_eq!(hints.metadata("hidden"), Some(Value::Bool(true)));
    }
    #[test]
    fn labeled_empty_arrays_keep_their_element_type() {
        let ty = PropertyType::new("float", true, Value::Float(0.));
        let mut tokens = layerstack::TokenInterner::default();
        assert!(compatible(
            &crate::value::write_float_array(&[], &mut tokens),
            &ty
        ));
        assert!(!compatible(
            &crate::value::write_int_array(&[], &mut tokens),
            &ty
        ));
    }
}
