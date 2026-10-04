// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Primvar discovery, constant inheritance and indexed value flattening.
//!
//! OpenUSD: `UsdGeomPrimvar` and `UsdGeomPrimvarsAPI`. Value and metadata
//! composition remain the core's responsibility (AOUSD Core §12.2–12.5).
use crate::{PrimEdit, PrimView, Scene, SchemaEdit, Time};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use layerstack::{PathId, PropertyPath, PropertyType, TargetPath, Value};

/// A composed `primvars:*` attribute, excluding the `:indices` sidecar.
#[derive(Clone, Copy, Debug)]
pub struct Primvar<'a> {
    prim: PrimView<'a>,
    name: &'a str,
}

/// Invalid primvar declaration or indexed data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimvarError {
    /// The owning prim does not exist.
    MissingPrim(PathId),
    /// A name is empty, malformed or reserves the indices suffix.
    InvalidName(String),
    /// An existing property has an incompatible kind or type.
    TypeMismatch(String),
    /// Interpolation must be one of the five geometry interpolation tokens.
    InvalidInterpolation(String),
    /// Element size must be positive.
    InvalidElementSize(i32),
    /// An indexed array has no readable int-array indices at the requested time.
    MissingIndices,
    /// An index does not address a complete element in the value array.
    InvalidIndex {
        /// Position in the indices array.
        position: usize,
        /// The invalid authored index.
        index: i32,
    },
    /// Reserved legacy error; ID-target reads are now supported.
    IdTargetUnsupported,
}
impl core::fmt::Display for PrimvarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid primvar: {self:?}")
    }
}
impl core::error::Error for PrimvarError {}

fn full_name(name: &str) -> String {
    if name.starts_with("primvars:") {
        name.to_string()
    } else {
        format!("primvars:{name}")
    }
}
fn valid_name(name: &str) -> bool {
    name.starts_with("primvars:")
        && !name.ends_with(":indices")
        && name.split(':').all(|part| {
            !part.is_empty()
                && !part.as_bytes()[0].is_ascii_digit()
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}
fn interpolation_valid(value: &str) -> bool {
    matches!(
        value,
        "constant" | "uniform" | "vertex" | "varying" | "faceVarying"
    )
}

impl<'a> Primvar<'a> {
    /// Looks up a declared primvar by base name or full namespaced name.
    #[must_use]
    pub fn new(scene: &Scene<'a>, path: PathId, name: &str) -> Option<Self> {
        let name = full_name(name);
        let prim = PrimView::new(*scene, path);
        if !valid_name(&name) || !prim.has_attribute(&name) {
            return None;
        }
        let token = scene.store().tokens().lookup(&name)?;
        Some(Self {
            prim,
            name: scene.store().tokens().resolve(token),
        })
    }
    /// Full attribute path, including the primvar namespace.
    #[must_use]
    pub fn property(&self) -> PropertyPath {
        self.prim
            .property_path(self.name)
            .expect("existing primvar")
    }
    /// The name without the `primvars:` prefix; nested namespaces are retained.
    #[must_use]
    pub fn name(&self) -> &'a str {
        self.name.strip_prefix("primvars:").expect("validated name")
    }
    /// Composed interpolation; unauthored interpolation is `constant`.
    #[must_use]
    pub fn interpolation(&self) -> &'a str {
        self.prim
            .property_metadata(self.name)
            .and_then(|m| m.interpolation())
            .unwrap_or("constant")
    }
    /// Values per indexed element; unauthored element size is one.
    #[must_use]
    pub fn element_size(&self) -> i32 {
        self.prim
            .property_metadata(self.name)
            .and_then(|m| m.element_size())
            .unwrap_or(1)
    }
    /// The declared placeholder index for unauthored elements, or -1.
    /// Flattening preserves its indexed value; it does not remove placeholders.
    #[must_use]
    pub fn unauthored_values_index(&self) -> i32 {
        self.prim
            .property_metadata(self.name)
            .and_then(|m| m.unauthored_values_index())
            .unwrap_or(-1)
    }
    /// Whether any effective authored value exists, including animation.
    /// Schema fallbacks and blocked defaults do not count.
    #[must_use]
    pub fn has_authored_value(&self) -> bool {
        self.prim.has_authored_value(self.name)
    }
    /// Whether the indices attribute has an effective authored value.
    #[must_use]
    pub fn is_indexed(&self) -> bool {
        self.prim
            .has_authored_value(&format!("{}:indices", self.name))
    }
    /// The composed indices at `time`, retaining shared native storage.
    /// Legacy storage or sparse composition can materialize; `as_ref().clone()`
    /// makes an explicit mutable copy.
    #[must_use]
    pub fn indices(&self, time: Time) -> Option<Arc<Vec<i32>>> {
        let value = self
            .prim
            .raw_value(&format!("{}:indices", self.name), time)?;
        crate::value::read_int_array_shared(&value, self.prim.scene().store().tokens())
    }
    fn string_type(&self) -> Option<bool> {
        let scene = self.prim.scene();
        let ty = scene
            .stage()
            .resolve_property_declaration(self.prim.path(), self.property().property())
            .and_then(|p| p.type_name)
            .or_else(|| {
                scene
                    .stage()
                    .property_definition_ref(self.prim.path(), self.property().property())
                    .and_then(|p| p.type_name.clone())
            })?;
        matches!(ty.default_scalar, Value::String(_)).then_some(ty.is_array)
    }
    /// Whether a string or string-array primvar has an `:idFrom` relationship.
    /// A defined relationship overrides the attribute even when it has no targets.
    #[must_use]
    pub fn is_id_target(&self) -> bool {
        let scene = self.prim.scene();
        self.string_type().is_some()
            && scene
                .stage()
                .prim(self.prim.path(), scene.store())
                .and_then(|p| p.relationship(&format!("{}:idFrom", self.name)))
                .is_some()
    }
    /// Reads the composed value, resolving string ID targets through forwarded
    /// relationship targets. Scalar strings require exactly one terminal target.
    /// OpenUSD 26.8 string arrays require multiple targets and return the first.
    /// Empty or ambiguous targets return `None`, without retrying the local value.
    ///
    /// OpenUSD: `UsdGeomPrimvar::Get`; AOUSD Core §12.4 (relationship resolution).
    pub fn value(&self, time: Time) -> Result<Option<Value>, PrimvarError> {
        if self.is_id_target() {
            let scene = self.prim.scene();
            let targets = scene
                .stage()
                .prim(self.prim.path(), scene.store())
                .and_then(|p| p.relationship(&format!("{}:idFrom", self.name)))
                .expect("defined ID relationship")
                .forwarded_targets();
            let is_array = self.string_type().expect("string ID primvar");
            let valid = if is_array {
                targets.len() > 1
            } else {
                targets.len() == 1
            };
            return Ok(valid.then(|| {
                let value = Value::string(
                    targets[0].display(scene.store().paths(), scene.store().tokens()),
                );
                if is_array {
                    Value::array(alloc::vec![value])
                } else {
                    value
                }
            }));
        }
        Ok(self.prim.raw_value(self.name, time))
    }
    /// Sorted, unique stage-time samples of the value and, when indexed, indices.
    /// Includes effective clip samples and sparse contributions from the core.
    /// OpenUSD: `UsdGeomPrimvar::GetTimeSamples`; AOUSD Core §12.3.2.
    #[must_use]
    pub fn sample_times(&self) -> Vec<f64> {
        let scene = self.prim.scene();
        let path = self.property();
        let mut times = scene
            .stage()
            .property_sample_times(path.prim_path(), path.property());
        if self.is_indexed()
            && let Some(indices) = self.prim.property_path(&format!("{}:indices", self.name))
        {
            times.extend(
                scene
                    .stage()
                    .property_sample_times(indices.prim_path(), indices.property()),
            );
            times.sort_by(f64::total_cmp);
            times.dedup_by(|a, b| a.total_cmp(b).is_eq());
        }
        times
    }
    /// Sample times within inclusive stage-time bounds. Reversed or NaN bounds
    /// return an empty set; infinite bounds select the corresponding full range.
    #[must_use]
    pub fn sample_times_in_interval(&self, start: f64, end: f64) -> Vec<f64> {
        self.sample_times()
            .into_iter()
            .filter(|t| start <= *t && *t <= end)
            .collect()
    }
    /// Whether effective values or indexed indices might vary at numeric times.
    /// Multiple samples or a spline count even when their values agree.
    #[must_use]
    pub fn might_be_time_varying(&self) -> bool {
        let scene = self.prim.scene();
        let path = self.property();
        scene
            .stage()
            .property_might_be_time_varying(path.prim_path(), path.property())
            || (self.is_indexed()
                && self
                    .prim
                    .property_path(&format!("{}:indices", self.name))
                    .is_some_and(|p| {
                        scene
                            .stage()
                            .property_might_be_time_varying(p.prim_path(), p.property())
                    }))
    }
    /// Expands each index into `element_size` consecutive values. Scalars and
    /// nonindexed arrays pass through. Invalid indices return no partial array.
    /// Native element kinds are retained even for an empty result.
    pub fn compute_flattened(&self, time: Time) -> Result<Option<Value>, PrimvarError> {
        let Some(value) = self.value(time)? else {
            return Ok(None);
        };
        let Some(array) = value.array_ref().filter(|_| self.is_indexed()) else {
            return Ok(Some(value));
        };
        let size = self.element_size();
        let width = usize::try_from(size)
            .ok()
            .filter(|&v| v > 0)
            .ok_or(PrimvarError::InvalidElementSize(size))?;
        let indices = self.indices(time).ok_or(PrimvarError::MissingIndices)?;
        let kind = array
            .typed()
            .map(layerstack::TypedArray::element_kind)
            .or_else(|| array.get(0).map(|v| v.into_owned()));
        let mut result = Vec::new();
        for (position, index) in indices.iter().copied().enumerate() {
            let start = usize::try_from(index)
                .ok()
                .and_then(|i| i.checked_mul(width))
                .filter(|&i| i.checked_add(width).is_some_and(|end| end <= array.len()))
                .ok_or(PrimvarError::InvalidIndex { position, index })?;
            result.extend(
                (start..start + width).map(|i| array.get(i).expect("validated index").into_owned()),
            );
        }
        Ok(Some(Value::array_with_element(result, kind.as_ref())))
    }
    /// An edit handle retaining this primvar's name and declared type.
    #[must_use]
    pub fn edit(&self) -> PrimvarEdit {
        PrimvarEdit {
            prim: self.prim.path(),
            name: self.name.to_string(),
        }
    }
}

impl<'a> PrimView<'a> {
    /// Declared primvars, including schema properties; excludes indices and
    /// relationships. Results are sorted by full property name.
    #[must_use]
    pub fn primvars(&self) -> Vec<Primvar<'a>> {
        let scene = self.scene();
        let mut result: Vec<_> = scene
            .stage()
            .property_names(self.path(), scene.store())
            .into_iter()
            .filter_map(|name| {
                Primvar::new(&scene, self.path(), scene.store().tokens().resolve(name))
            })
            .collect();
        result.sort_by_key(|p| p.name);
        result
    }
    /// Finds the nearest value-producing primvar. Only authored constant
    /// ancestor primvars inherit; a nearer authored nonconstant one stops the
    /// search. Blocks and fallback-only properties do not stop inheritance.
    /// OpenUSD: `UsdGeomPrimvarsAPI::FindPrimvarWithInheritance`.
    #[must_use]
    pub fn find_primvar_with_inheritance(&self, name: &str) -> Option<Primvar<'a>> {
        let scene = self.scene();
        if !scene.stage().has_prim(self.path()) {
            return None;
        }
        let local = Primvar::new(&scene, self.path(), name);
        if local.is_some_and(|p| p.has_authored_value()) {
            return local;
        }
        let mut at = scene.parent(self.path());
        while let Some(path) = at {
            at = scene.parent(path);
            if at.is_none() {
                break;
            }
            if let Some(primvar) =
                Primvar::new(&scene, path, name).filter(Primvar::has_authored_value)
            {
                return (primvar.interpolation() == "constant").then_some(primvar);
            }
        }
        local
    }
    /// Authored primvar declarations, including declarations without values.
    /// Schema-only properties are excluded; names are sorted.
    #[must_use]
    pub fn authored_primvars(&self) -> Vec<Primvar<'a>> {
        let scene = self.scene();
        let mut result: Vec<_> = scene
            .stage()
            .authored_property_names(self.path(), scene.store())
            .into_iter()
            .filter_map(|name| {
                Primvar::new(&scene, self.path(), scene.store().tokens().resolve(name))
            })
            .collect();
        result.sort_by_key(|p| p.name);
        result
    }
    /// Local primvars with an effective authored value, excluding blocked and
    /// fallback-only declarations. Animation-only values count.
    #[must_use]
    pub fn primvars_with_authored_values(&self) -> Vec<Primvar<'a>> {
        self.authored_primvars()
            .into_iter()
            .filter(Primvar::has_authored_value)
            .collect()
    }
    /// Authored primvar declarations whose attributes have a value source.
    /// Schema fallbacks on authored declarations and animation-only sources
    /// count; an ID relationship alone does not supply an attribute value.
    /// OpenUSD: `UsdGeomPrimvarsAPI::GetPrimvarsWithValues`.
    #[must_use]
    pub fn primvars_with_values(&self) -> Vec<Primvar<'a>> {
        self.authored_primvars()
            .into_iter()
            .filter(|p| p.prim.has_value(p.name))
            .collect()
    }
    /// Constant authored primvars available to children, sorted by name.
    /// Authored nonconstant values remove an ancestor of the same name; blocked
    /// or unvalued declarations leave ancestor inheritance intact.
    /// OpenUSD: `FindInheritablePrimvars`; AOUSD Core §12.2–12.5.
    #[must_use]
    pub fn inheritable_primvars(&self) -> Vec<Primvar<'a>> {
        if !self.scene().stage().has_prim(self.path()) {
            return Vec::new();
        }
        let mut paths = Vec::new();
        let mut at = Some(self.path());
        while let Some(path) = at {
            at = self.scene().parent(path);
            if at.is_some() {
                paths.push(path);
            }
        }
        let mut inherited = Vec::new();
        for path in paths.into_iter().rev() {
            if let Some(updated) =
                PrimView::new(self.scene(), path).incrementally_inheritable_primvars(&inherited)
            {
                inherited = updated;
            }
        }
        inherited
    }
    /// Updates the parent's inherited set with this prim's authored values.
    /// `None` means unchanged, allowing callers to reuse the parent's allocation;
    /// `Some([])` means local nonconstant values removed all inherited names.
    /// Pass a set from the parent's `inheritable_primvars` or this method, using
    /// the same immutable scene. Recompute after stage or store edits.
    /// OpenUSD: `FindIncrementallyInheritablePrimvars`; AOUSD Core §12.2–12.5.
    #[must_use]
    pub fn incrementally_inheritable_primvars(
        &self,
        inherited: &[Primvar<'a>],
    ) -> Option<Vec<Primvar<'a>>> {
        if !self.scene().stage().has_prim(self.path()) {
            return (!inherited.is_empty()).then(Vec::new);
        }
        self.merge_inherited_primvars(inherited, false)
    }
    fn merge_inherited_primvars(
        &self,
        inherited: &[Primvar<'a>],
        accept_all: bool,
    ) -> Option<Vec<Primvar<'a>>> {
        let mut updated: Option<Vec<Primvar<'a>>> = None;
        for pv in self.primvars_with_authored_values() {
            let current = updated.as_deref().unwrap_or(inherited);
            let found = current.iter().position(|p| p.name == pv.name);
            let keep = accept_all || pv.interpolation() == "constant";
            match (found, keep) {
                (Some(index), true) => {
                    if current[index].property() != pv.property() {
                        updated.get_or_insert_with(|| inherited.to_vec())[index] = pv;
                    }
                }
                (Some(index), false) => {
                    updated
                        .get_or_insert_with(|| inherited.to_vec())
                        .remove(index);
                }
                (None, true) => updated.get_or_insert_with(|| inherited.to_vec()).push(pv),
                (None, false) => {}
            }
        }
        if let Some(values) = &mut updated {
            values.sort_by_key(|p| p.name);
        }
        updated
    }
    /// Local and inherited authored values using a reusable parent set instead
    /// of walking ancestors. Local values of any interpolation override it.
    /// The input must come from the parent's inheritable query in this scene.
    #[must_use]
    pub fn primvars_with_inheritance_from(&self, inherited: &[Primvar<'a>]) -> Vec<Primvar<'a>> {
        if !self.scene().stage().has_prim(self.path()) {
            return Vec::new();
        }
        self.merge_inherited_primvars(inherited, true)
            .unwrap_or_else(|| inherited.to_vec())
    }
    /// Finds one local or inherited primvar without walking ancestors. A local
    /// authored value wins; otherwise a matching inherited value wins, else the
    /// local declaration is returned (it may have only a fallback or no value).
    #[must_use]
    pub fn find_primvar_with_inheritance_from(
        &self,
        name: &str,
        inherited: &[Primvar<'a>],
    ) -> Option<Primvar<'a>> {
        if !self.scene().stage().has_prim(self.path()) {
            return None;
        }
        let local = Primvar::new(&self.scene(), self.path(), name);
        if local.is_some_and(|p| p.has_authored_value()) {
            return local;
        }
        let name = full_name(name);
        inherited.iter().copied().find(|p| p.name == name).or(local)
    }
    /// All value-producing local or inherited primvars, sorted by name.
    #[must_use]
    pub fn primvars_with_inheritance(&self) -> Vec<Primvar<'a>> {
        let inherited = self
            .scene()
            .parent(self.path())
            .map(|parent| PrimView::new(self.scene(), parent).inheritable_primvars())
            .unwrap_or_default();
        self.primvars_with_inheritance_from(&inherited)
    }
}

/// Authors one primvar through an explicit mapped transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimvarEdit {
    prim: PathId,
    name: String,
}
impl PrimEdit {
    /// Creates and authors an indexed array primvar as one fallible child group.
    /// Invalid names, metadata or sidecars append no edits.
    pub fn create_indexed_primvar(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        ty: PropertyType,
        value: Value,
        indices: &[i32],
        interpolation: &str,
        element_size: i32,
    ) -> Result<PrimvarEdit, PrimvarError> {
        edit.group(|edit| {
            let pv = self.create_primvar(edit, name, ty)?;
            pv.set_interpolation(edit, interpolation)?;
            pv.set_element_size(edit, element_size)?;
            pv.set(edit, value)?;
            pv.set_indices(edit, indices, None)?;
            Ok(pv)
        })
    }
    /// Creates and authors a nonindexed primvar, blocking any existing indices
    /// in the target layer so weaker indexing cannot leak into the new value.
    /// Invalid declarations or metadata append no edits.
    pub fn create_nonindexed_primvar(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        ty: PropertyType,
        value: Value,
        interpolation: &str,
        element_size: i32,
    ) -> Result<PrimvarEdit, PrimvarError> {
        edit.group(|edit| {
            let pv = self.create_primvar(edit, name, ty)?;
            pv.set_interpolation(edit, interpolation)?;
            pv.set_element_size(edit, element_size)?;
            pv.set(edit, value)?;
            if pv.validate(edit)?.is_array {
                pv.block_indices(edit)?;
            }
            Ok(pv)
        })
    }
    /// Creates a noncustom primvar, preserving existing compatible declarations.
    /// Invalid names, missing prims and incompatible properties append no edits.
    pub fn create_primvar(
        &self,
        edit: &mut SchemaEdit<'_>,
        name: &str,
        ty: PropertyType,
    ) -> Result<PrimvarEdit, PrimvarError> {
        let name = full_name(name);
        if !valid_name(&name) {
            return Err(PrimvarError::InvalidName(name));
        }
        if !edit.exists(self.path()) {
            return Err(PrimvarError::MissingPrim(self.path()));
        }
        if let Some(kind) = edit.property_kind(self.path(), &name) {
            if kind != layerstack::PropertyKind::Attribute
                || edit
                    .attribute_type(self.path(), &name)
                    .is_none_or(|old| old.type_name != ty.type_name || old.is_array != ty.is_array)
            {
                return Err(PrimvarError::TypeMismatch(name));
            }
        } else {
            edit.create_attribute(self.path(), &name, ty);
        }
        Ok(PrimvarEdit {
            prim: self.path(),
            name,
        })
    }
}
impl PrimvarEdit {
    /// Blocks this value and, for arrays, its indices in the edit target,
    /// clearing local samples and splines. An indices block is created even
    /// when no sidecar exists, preventing later weaker indices from leaking in.
    /// Declaration and other metadata remain intact.
    /// OpenUSD: `UsdGeomPrimvarsAPI::BlockPrimvar`; AOUSD Core §12.3.6.
    pub fn block(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, PrimvarError> {
        let ty = self.validate(edit)?;
        if ty.is_array {
            self.block_indices(edit)?;
        }
        edit.block_attribute(self.prim, &self.name);
        Ok(self)
    }
    /// Blocks array indices in the target layer, creating a noncustom `int[]`
    /// sidecar if needed to mask future weaker indexing. Local animation is
    /// cleared. Scalar primvars and relationship sidecars append no edits.
    pub fn block_indices(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, PrimvarError> {
        if !self.validate(edit)?.is_array {
            return Err(PrimvarError::TypeMismatch(self.name.clone()));
        }
        let indices = format!("{}:indices", self.name);
        match edit.property_kind(self.prim, &indices) {
            Some(layerstack::PropertyKind::Attribute) => {}
            Some(_) => return Err(PrimvarError::TypeMismatch(indices)),
            None => edit.create_attribute(
                self.prim,
                &indices,
                PropertyType::new("int", true, Value::Int(0)),
            ),
        }
        edit.block_attribute(self.prim, &indices);
        Ok(self)
    }
    /// Removes this primvar and its indices specs from the edit target only.
    /// Weaker declarations can reappear. An ID relationship is an independent
    /// property and is retained, matching `RemovePrimvar`.
    pub fn remove(&self, edit: &mut SchemaEdit<'_>) -> Result<&Self, PrimvarError> {
        self.validate(edit)?;
        edit.remove_property(self.prim, &self.name);
        let indices = format!("{}:indices", self.name);
        if edit.property_kind(self.prim, &indices) == Some(layerstack::PropertyKind::Attribute) {
            edit.remove_property(self.prim, &indices);
        }
        Ok(self)
    }
    /// Authors the string primvar's ID relationship through the edit target.
    /// `None` targets this prim. Targets need not exist; prim and property paths
    /// are mapped by the core transaction. Other attribute types append no edits.
    /// OpenUSD: `UsdGeomPrimvar::SetIdTarget`; AOUSD Core §12.4.
    pub fn set_id_target(
        &self,
        edit: &mut SchemaEdit<'_>,
        target: Option<TargetPath>,
    ) -> Result<&Self, PrimvarError> {
        let ty = self.validate(edit)?;
        if !matches!(ty.default_scalar, Value::String(_)) {
            return Err(PrimvarError::TypeMismatch(self.name.clone()));
        }
        let name = format!("{}:idFrom", self.name);
        match edit.property_kind(self.prim, &name) {
            Some(layerstack::PropertyKind::Relationship) => {}
            Some(_) => return Err(PrimvarError::TypeMismatch(name)),
            None => edit.create_relationship(self.prim, &name),
        }
        edit.set_targets(
            self.prim,
            &name,
            &[target.unwrap_or(TargetPath::Prim(self.prim))],
        );
        Ok(self)
    }
    fn validate(&self, edit: &mut SchemaEdit<'_>) -> Result<PropertyType, PrimvarError> {
        if !edit.exists(self.prim) {
            return Err(PrimvarError::MissingPrim(self.prim));
        }
        edit.attribute_type(self.prim, &self.name)
            .ok_or_else(|| PrimvarError::TypeMismatch(self.name.clone()))
    }
    /// Authors a default; the core validates its value type when committing.
    pub fn set(&self, edit: &mut SchemaEdit<'_>, value: Value) -> Result<&Self, PrimvarError> {
        self.validate(edit)?;
        edit.set_value(self.prim, &self.name, None, value);
        Ok(self)
    }
    /// Authors a sample in stage time through the edit target.
    pub fn set_at(
        &self,
        edit: &mut SchemaEdit<'_>,
        time: f64,
        value: Value,
    ) -> Result<&Self, PrimvarError> {
        self.validate(edit)?;
        edit.set_value(self.prim, &self.name, Some(time), value);
        Ok(self)
    }
    /// Authors one of the five valid interpolation tokens.
    pub fn set_interpolation(
        &self,
        edit: &mut SchemaEdit<'_>,
        interpolation: &str,
    ) -> Result<&Self, PrimvarError> {
        if !interpolation_valid(interpolation) {
            return Err(PrimvarError::InvalidInterpolation(
                interpolation.to_string(),
            ));
        }
        self.validate(edit)?;
        let token = edit.tokens().intern(interpolation);
        edit.set_property_metadata(self.prim, &self.name, "interpolation", Value::Token(token));
        Ok(self)
    }
    /// Authors a positive element size.
    pub fn set_element_size(
        &self,
        edit: &mut SchemaEdit<'_>,
        size: i32,
    ) -> Result<&Self, PrimvarError> {
        if size <= 0 {
            return Err(PrimvarError::InvalidElementSize(size));
        }
        self.validate(edit)?;
        edit.set_property_metadata(self.prim, &self.name, "elementSize", Value::Int(size));
        Ok(self)
    }
    /// Authors indices for an array-valued primvar, at default or numeric time.
    pub fn set_indices(
        &self,
        edit: &mut SchemaEdit<'_>,
        indices: &[i32],
        time: Option<f64>,
    ) -> Result<&Self, PrimvarError> {
        self.set_indices_owned(edit, indices.to_vec(), time)
    }
    /// Transfers an index vector without copying its allocation or capacity.
    /// Allocates only a shared owner; `time` is mapped through the edit target.
    pub fn set_indices_owned(
        &self,
        edit: &mut SchemaEdit<'_>,
        indices: Vec<i32>,
        time: Option<f64>,
    ) -> Result<&Self, PrimvarError> {
        self.set_indices_shared(edit, Arc::new(indices), time)
    }
    /// Transfers a shared index buffer without copying or allocating elements.
    /// Indices are checked against values when flattening or publishing a mesh.
    pub fn set_indices_shared(
        &self,
        edit: &mut SchemaEdit<'_>,
        indices: Arc<Vec<i32>>,
        time: Option<f64>,
    ) -> Result<&Self, PrimvarError> {
        if !self.validate(edit)?.is_array {
            return Err(PrimvarError::TypeMismatch(self.name.clone()));
        }
        let name = format!("{}:indices", self.name);
        let ty = PropertyType::new("int", true, Value::Int(0));
        if let Some(old) = edit.attribute_type(self.prim, &name) {
            if old != ty {
                return Err(PrimvarError::TypeMismatch(name));
            }
        } else if edit.property_kind(self.prim, &name).is_some() {
            return Err(PrimvarError::TypeMismatch(name));
        } else {
            edit.create_attribute(self.prim, &name, ty);
        }
        let value = crate::value::write_int_array_shared(indices, edit.tokens());
        edit.set_value(self.prim, &name, time, value);
        Ok(self)
    }
}
