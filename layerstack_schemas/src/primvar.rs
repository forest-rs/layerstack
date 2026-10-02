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
    vec::Vec,
};
use layerstack::{PathId, PropertyPath, PropertyType, Value};

/// A composed `primvars:*` attribute, excluding the `:indices` sidecar.
#[derive(Clone, Copy, Debug)]
pub struct Primvar<'a> {
    prim: PrimView<'a>,
    name: &'a str,
}

/// Invalid primvar declaration, indexed data or unsupported ID indirection.
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
    /// A string primvar uses the unsupported `:idFrom` relationship mechanism.
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
    /// The composed indices at `time`, if readable as an int array.
    #[must_use]
    pub fn indices(&self, time: Time) -> Option<Vec<i32>> {
        let value = self
            .prim
            .raw_value(&format!("{}:indices", self.name), time)?;
        crate::value::read_int_array(&value, self.prim.scene().store().tokens())
    }
    /// Reads the composed value. ID-target string indirection returns an
    /// explicit unsupported error instead of reading a misleading local value.
    pub fn value(&self, time: Time) -> Result<Option<Value>, PrimvarError> {
        let scene = self.prim.scene();
        let property = scene
            .stage()
            .resolve_property_declaration(self.prim.path(), self.property().property());
        if property
            .and_then(|p| p.type_name)
            .is_some_and(|t| matches!(t.default_scalar, Value::String(_)))
            && self
                .prim
                .property_metadata(&format!("{}:idFrom", self.name))
                .is_some()
        {
            return Err(PrimvarError::IdTargetUnsupported);
        }
        Ok(self.prim.raw_value(self.name, time))
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
        for (position, index) in indices.into_iter().enumerate() {
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
    /// All value-producing local or inherited primvars, sorted by name.
    #[must_use]
    pub fn primvars_with_inheritance(&self) -> Vec<Primvar<'a>> {
        let mut names = Vec::new();
        let mut at = Some(self.path());
        while let Some(path) = at {
            names.extend(
                PrimView::new(self.scene(), path)
                    .primvars()
                    .into_iter()
                    .map(|p| p.name),
            );
            at = self.scene().parent(path);
        }
        names.sort_unstable();
        names.dedup();
        names
            .into_iter()
            .filter_map(|name| self.find_primvar_with_inheritance(name))
            .filter(Primvar::has_authored_value)
            .collect()
    }
}

/// Authors one primvar through an explicit mapped transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimvarEdit {
    prim: PathId,
    name: String,
}
impl PrimEdit {
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
        let value = crate::value::write_int_array(indices, edit.tokens());
        edit.set_value(self.prim, &name, time, value);
        Ok(self)
    }
}
