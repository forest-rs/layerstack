// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Schema-shaped edits: material bindings, shader inputs and connections,
//! base materials, geometry subsets, coordinate systems, `sdrMetadata` and
//! common transforms, lowered through `layerstack_schemas`' edit handles into
//! the batch's one [`layerstack::Transaction`].
//!
//! Lowering is in two steps, as [`crate::edit::lower`] runs them: each
//! edit is first [`prepare`]d against the store (paths interned, names and
//! values checked, failing with `path.invalid`, `argument.invalid` or a
//! `value.*` code), then [`author`]ed through the batch's one
//! [`SchemaEdit`], which sees the prims, types and applied schemas earlier
//! edits of the batch defined. A handle's refusal is a [`Refusal`], which
//! names its prims once the edit no longer borrows the store.
//!
//! Spec: AOUSD Core §7 (scene description), §12.4 (list ops of
//! connections and targets), §13.2 (applied schemas). OpenUSD:
//! `UsdShadeMaterialBindingAPI`, `UsdShadeConnectableAPI`,
//! `UsdShadeMaterial`, `UsdShadeShader`, `UsdGeomSubset`,
//! `UsdShadeCoordSysAPI`, `UsdGeomXformCommonAPI`.

use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

use layerstack::{
    InMemoryStore, PathId, PropertyPath, PropertyType, TargetPath, Value, schema::CannotApply,
};
use layerstack_schemas::{
    ApplySchemaError, BindingStrength, CannotEdit, ClassPrimError, CommonComponent,
    CommonTransform, CommonTransformError, CoordSysError, MaterialBindingError, MaterialPurpose,
    MetadataError, PrimEdit, RotationOrder, SchemaEdit,
    shading::{ConnectionModification, ConnectionSource, PortEdit, PortError},
    subset::{SubsetAuthoringError, SubsetError, SubsetFamilyType},
    usd_geom::{GeomSubsetEdit, GeomSubsetElementType, ImageableEdit, XformableEdit},
    usd_shade::{CoordSysApi, CoordSysApiEdit, MaterialBindingApi, MaterialBindingApiEdit},
    usd_shade::{MaterialEdit, ShaderEdit},
};

use crate::{error::Error, schemas::element_type, value::ValueInput};

/// One schema-shaped edit, as the wrapper sent it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SchemaInput {
    /// `UsdShadeMaterialBindingAPI::Apply(prim).Bind(material, strength,
    /// purpose)`, or its collection binding with `collection`.
    BindMaterial {
        path: String,
        material: String,
        purpose: String,
        strength: Option<String>,
        collection: Option<String>,
        binding_name: Option<String>,
    },
    /// `UnbindDirectBinding`, or `UnbindCollectionBinding` for
    /// `binding_name`.
    UnbindMaterial {
        path: String,
        purpose: String,
        binding_name: Option<String>,
    },
    /// `UsdShadeConnectableAPI::CreateInput(name, type).Set(value, time)`:
    /// the input takes the value's type.
    SetShaderInput {
        path: String,
        time: Option<f64>,
        value: u32,
    },
    /// `UsdShadeConnectableAPI::ConnectToSource`.
    ConnectToSource {
        path: String,
        source: String,
        type_name: Option<String>,
        position: String,
    },
    /// `UsdShadeConnectableAPI::DisconnectSource`, of one source or all.
    DisconnectSource {
        path: String,
        source: Option<String>,
    },
    /// `UsdShadeMaterial::SetBaseMaterial`, or `ClearBaseMaterial`.
    SetBaseMaterial { path: String, base: Option<String> },
    /// `UsdGeomSubset::CreateGeomSubset`, or
    /// `UsdShadeMaterialBindingAPI::CreateMaterialBindSubset` with
    /// `material_bind`.
    CreateGeomSubset {
        path: String,
        name: String,
        element: String,
        indices: Vec<i32>,
        family: String,
        family_type: Option<String>,
        material_bind: bool,
    },
    /// `UsdGeomSubset::SetFamilyType`.
    SetSubsetFamilyType {
        path: String,
        family: String,
        family_type: String,
    },
    /// `UsdShadeCoordSysAPI::Apply(prim, name).Bind(target)`.
    BindCoordSys {
        path: String,
        name: String,
        target: String,
    },
    /// `UsdShadeCoordSysAPI::BlockBinding`.
    BlockCoordSys { path: String, name: String },
    /// `UsdShadeCoordSysAPI::ClearBinding(removeSpec)`.
    ClearCoordSys {
        path: String,
        name: String,
        remove_spec: bool,
    },
    /// `SetSdrMetadataByKey` on a shader or a port, or
    /// `ClearSdrMetadataByKey` for `None` (every key for an empty one).
    SetSdrMetadata {
        path: String,
        key: String,
        value: Option<String>,
    },
    /// `UsdGeomXformCommonAPI::SetXformVectors` with every component, or
    /// `SetTranslate`, `SetRotate`, `SetScale` or `SetPivot` with one; at
    /// `time`, or the default with `None`.
    SetCommonTransform {
        path: String,
        time: Option<f64>,
        translate: Option<Vec<f64>>,
        rotate: Option<Vec<f64>>,
        order: Option<String>,
        scale: Option<Vec<f64>>,
        pivot: Option<Vec<f64>>,
    },
}

impl SchemaInput {
    /// The prim or property path the edit is about.
    pub(crate) fn path(&self) -> &str {
        match self {
            Self::BindMaterial { path, .. }
            | Self::UnbindMaterial { path, .. }
            | Self::SetShaderInput { path, .. }
            | Self::ConnectToSource { path, .. }
            | Self::DisconnectSource { path, .. }
            | Self::SetBaseMaterial { path, .. }
            | Self::CreateGeomSubset { path, .. }
            | Self::SetSubsetFamilyType { path, .. }
            | Self::BindCoordSys { path, .. }
            | Self::BlockCoordSys { path, .. }
            | Self::ClearCoordSys { path, .. }
            | Self::SetSdrMetadata { path, .. }
            | Self::SetCommonTransform { path, .. } => path,
        }
    }
}

/// A schema edit checked against the store, its paths interned.
#[derive(Clone, Debug)]
pub(crate) enum Prepared {
    BindMaterial {
        prim: PathId,
        material: PathId,
        purpose: MaterialPurpose,
        strength: Option<BindingStrength>,
        collection: Option<PropertyPath>,
        binding_name: Option<String>,
    },
    UnbindMaterial {
        prim: PathId,
        purpose: MaterialPurpose,
        binding_name: Option<String>,
    },
    SetShaderInput {
        port: PropertyPath,
        name: String,
        ty: PropertyType,
        time: Option<f64>,
        value: Value,
    },
    ConnectToSource {
        port: PropertyPath,
        source: PropertyPath,
        ty: Option<PropertyType>,
        modification: ConnectionModification,
    },
    DisconnectSource {
        port: PropertyPath,
        source: Option<PropertyPath>,
    },
    SetBaseMaterial {
        prim: PathId,
        base: Option<PathId>,
    },
    CreateGeomSubset {
        prim: PathId,
        name: String,
        element: GeomSubsetElementType,
        indices: Vec<i32>,
        family: String,
        family_type: Option<SubsetFamilyType>,
        material_bind: bool,
    },
    SetSubsetFamilyType {
        prim: PathId,
        family: String,
        family_type: SubsetFamilyType,
    },
    BindCoordSys {
        prim: PathId,
        name: String,
        target: TargetPath,
    },
    BlockCoordSys {
        prim: PathId,
        name: String,
    },
    ClearCoordSys {
        prim: PathId,
        name: String,
        remove_spec: bool,
    },
    SetSdrMetadata {
        target: TargetPath,
        key: String,
        value: Option<String>,
    },
    SetCommonTransform {
        prim: PathId,
        time: Option<f64>,
        write: CommonWrite,
    },
}

/// What a common-transform edit writes: the whole stack, or one component.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CommonWrite {
    All(CommonTransform),
    One(CommonComponent),
}

fn rotation_order(name: Option<&str>) -> Result<RotationOrder, Error> {
    Ok(match name {
        None | Some("XYZ") => RotationOrder::Xyz,
        Some("XZY") => RotationOrder::Xzy,
        Some("YXZ") => RotationOrder::Yxz,
        Some("YZX") => RotationOrder::Yzx,
        Some("ZXY") => RotationOrder::Zxy,
        Some("ZYX") => RotationOrder::Zyx,
        Some(other) => {
            return Err(Error::new(
                "argument.invalid",
                format!("`{other}` is not a rotation order: XYZ, XZY, YXZ, YZX, ZXY or ZYX"),
            ));
        }
    })
}

/// Three finite components, as a common transform's vectors are.
fn vector3(name: &str, values: &[f64]) -> Result<[f64; 3], Error> {
    match values {
        [x, y, z] if values.iter().all(|value| value.is_finite()) => Ok([*x, *y, *z]),
        _ => Err(Error::new(
            "argument.invalid",
            format!("`{name}` takes three finite numbers"),
        )),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "rotation, scale and pivot are float3 ops"
)]
fn float3(vector: [f64; 3]) -> [f32; 3] {
    vector.map(|component| component as f32)
}

/// The write a common-transform edit asks for: every component
/// (`SetXformVectors`), or exactly one.
fn common_write(
    translate: Option<&[f64]>,
    rotate: Option<&[f64]>,
    order: Option<&str>,
    scale: Option<&[f64]>,
    pivot: Option<&[f64]>,
) -> Result<CommonWrite, Error> {
    let translate = translate.map(|v| vector3("translate", v)).transpose()?;
    let rotate = rotate.map(|v| vector3("rotate", v)).transpose()?;
    let scale = scale.map(|v| vector3("scale", v)).transpose()?;
    let pivot = pivot.map(|v| vector3("pivot", v)).transpose()?;
    let order = rotation_order(order)?;
    Ok(match (translate, rotate, scale, pivot) {
        (Some(translation), Some(rotation), Some(scale), Some(pivot)) => {
            CommonWrite::All(CommonTransform {
                translation,
                rotation: float3(rotation),
                scale: float3(scale),
                pivot: float3(pivot),
                rotation_order: order,
            })
        }
        (Some(translation), None, None, None) => {
            CommonWrite::One(CommonComponent::Translate(translation))
        }
        (None, Some(rotation), None, None) => CommonWrite::One(CommonComponent::Rotate {
            degrees: float3(rotation),
            order,
        }),
        (None, None, Some(scale), None) => CommonWrite::One(CommonComponent::Scale(float3(scale))),
        (None, None, None, Some(pivot)) => CommonWrite::One(CommonComponent::Pivot(float3(pivot))),
        _ => {
            return Err(Error::new(
                "argument.invalid",
                "a common transform edit sets one of translate, rotate, scale and pivot, or all four",
            ));
        }
    })
}

/// What the store and the batch's values give a schema edit to prepare it.
pub(crate) struct Inputs<'a> {
    pub(crate) store: &'a InMemoryStore,
    pub(crate) values: &'a [ValueInput],
}

impl Inputs<'_> {
    fn prim(&self, path: &str) -> Result<PathId, Error> {
        if path.contains('.') {
            return Err(Error::new(
                "path.invalid",
                format!("`{path}` is not a prim path, such as /Looks/Stone"),
            ));
        }
        self.store
            .try_path(path)
            .map_err(|error| Error::new("path.invalid", format!("not a prim path: {error}")))
    }

    fn property(&self, path: &str) -> Result<PropertyPath, Error> {
        self.store.try_property_path(path).map_err(|error| {
            Error::new(
                "path.invalid",
                format!(
                    "`{path}` is not a property path, such as /Looks/Stone/Surface.inputs:roughness: {error}"
                ),
            )
        })
    }

    /// A shading port's path: an `inputs:` or `outputs:` property.
    fn port(&self, path: &str) -> Result<PropertyPath, Error> {
        let property = self.property(path)?;
        let name = self.store.tokens().resolve(property.property());
        if !(name.starts_with("inputs:") || name.starts_with("outputs:")) {
            return Err(Error::new(
                "path.invalid",
                format!(
                    "`{path}` is not a shading input or output: its name has no `inputs:` or `outputs:` namespace"
                ),
            ));
        }
        Ok(property)
    }

    fn target(&self, path: &str) -> Result<TargetPath, Error> {
        self.store.try_target_path(path).map_err(|error| {
            Error::new(
                "path.invalid",
                format!("`{path}` is not a prim or property path: {error}"),
            )
        })
    }
}

fn strength(name: Option<&str>) -> Result<Option<BindingStrength>, Error> {
    Ok(match name {
        None => None,
        Some("weakerThanDescendants") => Some(BindingStrength::WeakerThanDescendants),
        Some("strongerThanDescendants") => Some(BindingStrength::StrongerThanDescendants),
        Some(other) => {
            return Err(Error::new(
                "argument.invalid",
                format!(
                    "`{other}` is not a binding strength: weakerThanDescendants or strongerThanDescendants"
                ),
            ));
        }
    })
}

fn family_type(name: &str) -> Result<SubsetFamilyType, Error> {
    Ok(match name {
        "unrestricted" => SubsetFamilyType::Unrestricted,
        "nonOverlapping" => SubsetFamilyType::NonOverlapping,
        "partition" => SubsetFamilyType::Partition,
        other => {
            return Err(Error::new(
                "argument.invalid",
                format!(
                    "`{other}` is not a subset family type: unrestricted, nonOverlapping or partition"
                ),
            ));
        }
    })
}

/// Checks `input` and interns its paths.
///
/// # Errors
///
/// `path.invalid`, `argument.invalid` for an unknown strength, position,
/// element type or family type, and the `value.*` errors of a value.
pub(crate) fn prepare(input: &SchemaInput, inputs: &Inputs<'_>) -> Result<Prepared, Error> {
    let store = inputs.store;
    Ok(match input {
        SchemaInput::BindMaterial {
            path,
            material,
            purpose,
            strength: by,
            collection,
            binding_name,
        } => Prepared::BindMaterial {
            prim: inputs.prim(path)?,
            material: inputs.prim(material)?,
            purpose: MaterialPurpose::from_token(purpose),
            strength: strength(by.as_deref())?,
            collection: collection
                .as_deref()
                .map(|collection| inputs.property(collection))
                .transpose()?,
            binding_name: binding_name.clone(),
        },
        SchemaInput::UnbindMaterial {
            path,
            purpose,
            binding_name,
        } => Prepared::UnbindMaterial {
            prim: inputs.prim(path)?,
            purpose: MaterialPurpose::from_token(purpose),
            binding_name: binding_name.clone(),
        },
        SchemaInput::SetShaderInput { path, time, value } => {
            let port = inputs.port(path)?;
            let name = store.tokens().resolve(port.property());
            let Some(name) = name.strip_prefix("inputs:") else {
                return Err(Error::new(
                    "path.invalid",
                    format!("`{path}` is not a shading input: its name has no `inputs:` namespace"),
                ));
            };
            let slot = inputs.values.get(*value as usize).ok_or_else(|| {
                Error::new(
                    "argument.invalid",
                    "the edit names a value slot never filled",
                )
            })?;
            let ty = PropertyType::parse(&slot.type_name).map_err(|_| {
                Error::new(
                    "value.unknown_type",
                    format!("`{}` is not a registered value type name", slot.type_name),
                )
            })?;
            if let Some(time) = time
                && !time.is_finite()
            {
                return Err(Error::new(
                    "argument.invalid",
                    "a time code must be a finite number",
                ));
            }
            Prepared::SetShaderInput {
                port,
                name: name.into(),
                ty,
                time: *time,
                value: crate::value::decode_slot(inputs.values, *value, store.tokens())?,
            }
        }
        SchemaInput::ConnectToSource {
            path,
            source,
            type_name,
            position,
        } => Prepared::ConnectToSource {
            port: inputs.port(path)?,
            source: inputs.port(source)?,
            ty: type_name
                .as_deref()
                .map(|name| {
                    PropertyType::parse(name).map_err(|_| {
                        Error::new(
                            "value.unknown_type",
                            format!("`{name}` is not a registered value type name"),
                        )
                    })
                })
                .transpose()?,
            modification: match position.as_str() {
                "replace" => ConnectionModification::Replace,
                "prepend" => ConnectionModification::Prepend,
                "append" => ConnectionModification::Append,
                other => {
                    return Err(Error::new(
                        "argument.invalid",
                        format!(
                            "`{other}` is not a connection position: replace, prepend or append"
                        ),
                    ));
                }
            },
        },
        SchemaInput::DisconnectSource { path, source } => Prepared::DisconnectSource {
            port: inputs.port(path)?,
            source: source
                .as_deref()
                .map(|source| inputs.port(source))
                .transpose()?,
        },
        SchemaInput::SetBaseMaterial { path, base } => Prepared::SetBaseMaterial {
            prim: inputs.prim(path)?,
            base: base.as_deref().map(|base| inputs.prim(base)).transpose()?,
        },
        SchemaInput::CreateGeomSubset {
            path,
            name,
            element,
            indices,
            family,
            family_type: kind,
            material_bind,
        } => Prepared::CreateGeomSubset {
            prim: inputs.prim(path)?,
            name: name.clone(),
            element: element_type(element)?,
            indices: indices.clone(),
            family: family.clone(),
            family_type: kind.as_deref().map(family_type).transpose()?,
            material_bind: *material_bind,
        },
        SchemaInput::SetSubsetFamilyType {
            path,
            family,
            family_type: kind,
        } => Prepared::SetSubsetFamilyType {
            prim: inputs.prim(path)?,
            family: family.clone(),
            family_type: family_type(kind)?,
        },
        SchemaInput::BindCoordSys { path, name, target } => Prepared::BindCoordSys {
            prim: inputs.prim(path)?,
            name: name.clone(),
            target: inputs.target(target)?,
        },
        SchemaInput::BlockCoordSys { path, name } => Prepared::BlockCoordSys {
            prim: inputs.prim(path)?,
            name: name.clone(),
        },
        SchemaInput::ClearCoordSys {
            path,
            name,
            remove_spec,
        } => Prepared::ClearCoordSys {
            prim: inputs.prim(path)?,
            name: name.clone(),
            remove_spec: *remove_spec,
        },
        SchemaInput::SetCommonTransform {
            path,
            time,
            translate,
            rotate,
            order,
            scale,
            pivot,
        } => Prepared::SetCommonTransform {
            prim: inputs.prim(path)?,
            time: *time,
            write: common_write(
                translate.as_deref(),
                rotate.as_deref(),
                order.as_deref(),
                scale.as_deref(),
                pivot.as_deref(),
            )?,
        },
        SchemaInput::SetSdrMetadata { path, key, value } => Prepared::SetSdrMetadata {
            target: if path.contains('.') {
                TargetPath::Property(inputs.port(path)?)
            } else {
                TargetPath::Prim(inputs.prim(path)?)
            },
            key: key.clone(),
            value: value.clone(),
        },
    })
}

/// Why a schema edit's handle refused it: nothing of it was collected.
#[derive(Debug)]
pub(crate) enum Refusal {
    CannotEdit(CannotEdit),
    ClassPrim(ClassPrimError),
    Apply(ApplySchemaError),
    NoPrim(PathId),
    NotMaterial(PathId),
    NotShader(PathId),
    NoPort(PropertyPath),
    Binding(MaterialBindingError),
    Port(PortError),
    Subset(SubsetAuthoringError),
    CoordSys(CoordSysError),
    Metadata(MetadataError),
    NotXformable(PathId),
    CommonTransform(CommonTransformError),
}

impl From<CannotEdit> for Refusal {
    fn from(error: CannotEdit) -> Self {
        Self::CannotEdit(error)
    }
}

impl From<ClassPrimError> for Refusal {
    fn from(error: ClassPrimError) -> Self {
        match error {
            ClassPrimError::CannotEdit(error) => Self::CannotEdit(error),
            error => Self::ClassPrim(error),
        }
    }
}

impl Refusal {
    /// The error, its prims and properties named with `store`.
    pub(crate) fn into_error(self, store: &InMemoryStore) -> Error {
        let path = |id: PathId| store.paths().display(id, store.tokens());
        match self {
            Self::CannotEdit(error) => {
                Error::new("schema.cannot_edit", error.display(store).to_string())
            }
            Self::ClassPrim(error) => Error::new(
                match error {
                    ClassPrimError::NotLocal => "layer.outside_stack",
                    _ => "prim.not_class",
                },
                error.display(store).to_string(),
            ),
            Self::Apply(ApplySchemaError::CannotEdit(error)) => {
                Self::CannotEdit(error).into_error(store)
            }
            Self::Apply(ApplySchemaError::CannotApply(CannotApply::NoSuchPrim)) => Error::new(
                "prim.not_found",
                "no prim is at this path, on the stage or defined earlier in these edits",
            ),
            Self::Apply(error) => {
                Error::new("schema.cannot_apply", error.display(store).to_string())
            }
            Self::NoPrim(prim) => Error::new(
                "prim.not_found",
                format!(
                    "no prim is at {}, on the stage or defined earlier in these edits",
                    path(prim)
                ),
            ),
            Self::NotXformable(prim) => Error::new(
                "xform.not_xformable",
                format!("{} is not Xformable", path(prim)),
            ),
            Self::CommonTransform(error) => Error::new(
                match error {
                    CommonTransformError::IncompatibleOps => "xform.incompatible_ops",
                    CommonTransformError::RotationOrderMismatch { .. } => "xform.rotation_order",
                    CommonTransformError::OpAlreadyInOrder { .. } => "xform.op_listed",
                    _ => "xform.attribute_type",
                },
                error.to_string(),
            ),
            Self::NotMaterial(prim) => Error::new(
                "material.not_material",
                format!("{} is not a Material", path(prim)),
            ),
            Self::NotShader(prim) => Error::new(
                "shading.not_shader",
                format!("{} is not a Shader", path(prim)),
            ),
            Self::NoPort(port) => Error::new(
                "shading.no_port",
                format!(
                    "no shading input or output is at {}: create it first (setShaderInput, \
                     or connectToSource with a typeName)",
                    port.display(store.paths(), store.tokens())
                ),
            ),
            Self::Binding(error) => {
                let code = match &error {
                    MaterialBindingError::MissingMaterial(_) => "material.not_found",
                    _ => "material.binding_name",
                };
                Error::new(code, error.display(store).to_string())
            }
            Self::Port(error) => {
                let code = match &error {
                    PortError::MissingPrim(_) => "prim.not_found",
                    PortError::InvalidName(_) => "shading.invalid_name",
                    PortError::TypeMismatch { .. } => "shading.type_mismatch",
                    _ => "shading.invalid_attribute",
                };
                Error::new(code, error.display(store).to_string())
            }
            Self::Subset(error) => {
                let code = match &error {
                    SubsetAuthoringError::CannotEdit(_) => "schema.cannot_edit",
                    SubsetAuthoringError::InvalidName => "subset.invalid_name",
                    SubsetAuthoringError::Geometry(SubsetError::Decode { .. }) => "value.decode",
                    SubsetAuthoringError::Geometry(_) => "subset.geometry",
                    SubsetAuthoringError::Indices(_) => "subset.indices",
                    SubsetAuthoringError::IncompatibleFamilyDeclaration => {
                        "subset.family_declaration"
                    }
                    SubsetAuthoringError::NameTaken(_) => "subset.name_taken",
                    SubsetAuthoringError::UnrestrictedMaterialBindFamily => {
                        "subset.unrestricted_material_bind"
                    }
                    _ => "subset.refused",
                };
                Error::new(code, error.display(store).to_string())
            }
            Self::CoordSys(error) => {
                Error::new("coord_sys.no_binding", error.display(store).to_string())
            }
            Self::Metadata(error) => {
                let code = match &error {
                    MetadataError::NoSuchPrim => "prim.not_found",
                    MetadataError::NoSuchProperty => "shading.no_port",
                    MetadataError::CannotEdit(_) => "schema.cannot_edit",
                    MetadataError::InvalidKeyPath(_) => "metadata.key_path",
                    _ => "metadata.field",
                };
                Error::new(code, error.to_string())
            }
        }
    }
}

/// A port handle for `port`, which must exist, on the stage or created
/// earlier in the edit.
fn existing_port(edit: &mut SchemaEdit<'_>, port: PropertyPath) -> Result<PortEdit, Refusal> {
    PortEdit::get(edit, port)?.ok_or(Refusal::NoPort(port))
}

/// A connection source naming the attribute at `path`.
fn source_of(edit: &mut SchemaEdit<'_>, path: PropertyPath) -> ConnectionSource {
    let name = String::from(edit.tokens().resolve(path.property()));
    ConnectionSource::new(path.prim_path(), &name)
}

/// Collects `prepared` through `edit`'s handles.
///
/// # Errors
///
/// The handle's [`Refusal`]; nothing of the edit is collected then.
pub(crate) fn author(edit: &mut SchemaEdit<'_>, prepared: Prepared) -> Result<(), Refusal> {
    match prepared {
        Prepared::BindMaterial {
            prim,
            material,
            purpose,
            strength,
            collection,
            binding_name,
        } => {
            // The handle refuses a material where no prim is
            // (`MissingMaterial`), as OpenUSD's `Bind` refuses the empty
            // path of one, and binds any existing prim whatever its type.
            let handle = MaterialBindingApi::apply(edit, prim).map_err(Refusal::Apply)?;
            match collection {
                None => handle.bind(edit, material, strength, &purpose),
                Some(collection) => handle.bind_collection(
                    edit,
                    collection,
                    material,
                    binding_name.as_deref(),
                    strength,
                    &purpose,
                ),
            }
            .map_err(Refusal::Binding)
        }
        Prepared::UnbindMaterial {
            prim,
            purpose,
            binding_name,
        } => {
            let handle = MaterialBindingApiEdit::new(edit, prim)?.ok_or(Refusal::NoPrim(prim))?;
            match binding_name {
                None => handle.unbind(edit, &purpose),
                Some(name) => handle.unbind_collection(edit, &name, &purpose),
            }
            .map_err(Refusal::Binding)
        }
        Prepared::SetShaderInput {
            port,
            name,
            ty,
            time,
            value,
        } => {
            let prim =
                PrimEdit::new(edit, port.prim_path())?.ok_or(Refusal::NoPrim(port.prim_path()))?;
            let input = prim.create_input(edit, &name, ty).map_err(Refusal::Port)?;
            match time {
                None => input.set(edit, value),
                Some(time) => input.set_at(edit, time, value),
            }
            .map(|_| ())
            .map_err(Refusal::Port)
        }
        Prepared::ConnectToSource {
            port,
            source,
            ty,
            modification,
        } => {
            let handle = match (PortEdit::get(edit, port)?, ty) {
                (Some(handle), _) => handle,
                (None, Some(ty)) => {
                    let prim = PrimEdit::new(edit, port.prim_path())?
                        .ok_or(Refusal::NoPrim(port.prim_path()))?;
                    let full = String::from(edit.tokens().resolve(port.property()));
                    match full.strip_prefix("inputs:") {
                        Some(name) => prim.create_input(edit, name, ty),
                        None => prim.create_output(
                            edit,
                            full.strip_prefix("outputs:").unwrap_or(&full),
                            ty,
                        ),
                    }
                    .map_err(Refusal::Port)?
                }
                (None, None) => return Err(Refusal::NoPort(port)),
            };
            let source = source_of(edit, source);
            handle
                .connect_to_source(edit, source, modification)
                .map(|_| ())
                .map_err(Refusal::Port)
        }
        Prepared::DisconnectSource { port, source } => {
            let handle = existing_port(edit, port)?;
            match source {
                Some(source) => {
                    let source = source_of(edit, source);
                    handle.disconnect_source(edit, source)
                }
                None => handle.disconnect_sources(edit),
            }
            .map(|_| ())
            .map_err(Refusal::Port)
        }
        Prepared::SetBaseMaterial { prim, base } => {
            let Some(handle) = MaterialEdit::new(edit, prim)? else {
                return Err(if PrimEdit::new(edit, prim)?.is_some() {
                    Refusal::NotMaterial(prim)
                } else {
                    Refusal::NoPrim(prim)
                });
            };
            match base {
                Some(base) => handle.set_base_material(edit, Some(base)),
                None => handle.clear_base_material(edit),
            };
            Ok(())
        }
        Prepared::CreateGeomSubset {
            prim,
            name,
            element,
            indices,
            family,
            family_type,
            material_bind,
        } => {
            if material_bind {
                let handle =
                    MaterialBindingApiEdit::new(edit, prim)?.ok_or(Refusal::NoPrim(prim))?;
                handle
                    .create_material_bind_subset(edit, &name, element, &indices)
                    .map(|_| ())
                    .map_err(Refusal::Subset)
            } else {
                GeomSubsetEdit::create(edit, prim, &name, element, &indices, &family, family_type)
                    .map(|_| ())
                    .map_err(Refusal::Subset)
            }
        }
        Prepared::SetSubsetFamilyType {
            prim,
            family,
            family_type,
        } => {
            let handle = PrimEdit::new(edit, prim)?.ok_or(Refusal::NoPrim(prim))?;
            ImageableEdit::from_prim(handle)
                .set_subset_family_type(edit, &family, family_type)
                .map_err(Refusal::Subset)
        }
        Prepared::BindCoordSys { prim, name, target } => {
            let handle = CoordSysApi::apply(edit, prim, &name).map_err(Refusal::Apply)?;
            handle.bind(edit, target);
            Ok(())
        }
        Prepared::BlockCoordSys { prim, name } => {
            let handle = CoordSysApiEdit::new(edit, prim, &name)?.ok_or(Refusal::NoPrim(prim))?;
            handle.block_binding(edit);
            Ok(())
        }
        Prepared::ClearCoordSys {
            prim,
            name,
            remove_spec,
        } => {
            let handle = CoordSysApiEdit::new(edit, prim, &name)?.ok_or(Refusal::NoPrim(prim))?;
            handle
                .clear_binding(edit, remove_spec)
                .map(|_| ())
                .map_err(Refusal::CoordSys)
        }
        Prepared::SetCommonTransform { prim, time, write } => {
            let Some(xform) = XformableEdit::new(edit, prim)? else {
                return Err(if PrimEdit::new(edit, prim)?.is_some() {
                    Refusal::NotXformable(prim)
                } else {
                    Refusal::NoPrim(prim)
                });
            };
            let written = match (write, time) {
                (CommonWrite::All(value), None) => xform.set_common_transform(edit, &value),
                (CommonWrite::All(value), Some(t)) => {
                    xform.set_common_transform_at(edit, t, &value)
                }
                (CommonWrite::One(component), None) => xform.set_common_component(edit, component),
                (CommonWrite::One(component), Some(t)) => {
                    xform.set_common_component_at(edit, t, component)
                }
            };
            written.map(|_| ()).map_err(Refusal::CommonTransform)
        }
        Prepared::SetSdrMetadata { target, key, value } => match target {
            TargetPath::Prim(prim) => {
                let Some(shader) = ShaderEdit::new(edit, prim)? else {
                    return Err(if PrimEdit::new(edit, prim)?.is_some() {
                        Refusal::NotShader(prim)
                    } else {
                        Refusal::NoPrim(prim)
                    });
                };
                match value {
                    Some(value) => shader.set_sdr_metadata_by_key(edit, &key, &value),
                    None => shader.clear_sdr_metadata_by_key(edit, &key),
                }
                .map(|_| ())
                .map_err(Refusal::Metadata)
            }
            TargetPath::Property(port) => {
                let handle = existing_port(edit, port)?;
                match value {
                    Some(value) => handle.set_sdr_metadata_by_key(edit, &key, &value),
                    None => handle.clear_sdr_metadata_by_key(edit, &key),
                }
                .map(|_| ())
                .map_err(Refusal::Metadata)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use crate::{
        edit::EditBatch,
        schemas::tests::{SHADED, open, show},
        stage::Stage,
    };

    fn root(stage: &Stage) -> u64 {
        stage.document().root_layer().0
    }

    fn text(stage: &Stage) -> String {
        String::from_utf8(stage.export_layer(root(stage), "usda").unwrap()).unwrap()
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| String::from(*item)).collect()
    }

    #[test]
    fn a_common_transform_edit_sets_one_component_or_all_four() {
        let one = [1.0, 2.0, 3.0];
        assert!(matches!(
            super::common_write(Some(&one), None, None, None, None),
            Ok(super::CommonWrite::One(_))
        ));
        assert!(matches!(
            super::common_write(Some(&one), Some(&one), Some("ZYX"), Some(&one), Some(&one)),
            Ok(super::CommonWrite::All(_))
        ));
        let code = |result: Result<super::CommonWrite, crate::error::Error>| {
            result.err().map(|error| error.code)
        };
        // Two components, none, a short vector, a bad number, a bad order.
        for refused in [
            super::common_write(Some(&one), Some(&one), None, None, None),
            super::common_write(None, None, None, None, None),
            super::common_write(Some(&[1.0, 2.0]), None, None, None, None),
            super::common_write(Some(&[1.0, f64::NAN, 2.0]), None, None, None, None),
            super::common_write(None, Some(&one), Some("XY"), None, None),
        ] {
            assert_eq!(code(refused), Some("argument.invalid"));
        }
    }

    /// An editor's "new material" in one undoable batch: the material and
    /// its shader defined, an input set, the surface connected, metadata,
    /// a base material, bindings (one to a subset the batch creates) and a
    /// coordinate system.
    #[test]
    fn an_editor_builds_and_binds_a_material_in_one_undoable_batch() {
        let mut stage = open(SHADED);
        let before = text(&stage);
        let mut batch = EditBatch::new();
        batch.define_prim("/Looks/Clay".into(), "def".into(), Some("Material".into()));
        batch.define_prim(
            "/Looks/Clay/Surface".into(),
            "def".into(),
            Some("Shader".into()),
        );
        let roughness = batch.value_numbers("float".into(), vec![0.25]);
        batch.set_shader_input(
            "/Looks/Clay/Surface.inputs:roughness".into(),
            None,
            roughness,
        );
        batch.connect_to_source(
            "/Looks/Clay.outputs:surface".into(),
            "/Looks/Clay/Surface.outputs:surface".into(),
            Some("token".into()),
            "replace".into(),
        );
        batch.set_sdr_metadata(
            "/Looks/Clay/Surface".into(),
            "role".into(),
            Some("surface".into()),
        );
        batch.set_sdr_metadata(
            "/Looks/Clay/Surface.inputs:roughness".into(),
            "page".into(),
            Some("basic".into()),
        );
        batch.set_base_material("/Looks/Clay".into(), Some("/Looks/Stone".into()));
        batch.bind_material(
            "/World/Lamp".into(),
            "/Looks/Clay".into(),
            String::new(),
            Some("strongerThanDescendants".into()),
            None,
            None,
        );
        batch.create_geom_subset(
            "/World/Rock".into(),
            "Bottom".into(),
            "face".into(),
            vec![0],
            "materialBind".into(),
            None,
            true,
        );
        batch.bind_material(
            "/World/Rock/Bottom".into(),
            "/Looks/Clay".into(),
            String::new(),
            None,
            None,
            None,
        );
        batch.bind_coord_sys(
            "/World/Rock".into(),
            "decal".into(),
            "/World/Projector".into(),
        );
        let mut receipt = stage.apply(root(&stage), &mut batch).unwrap();

        assert_eq!(
            show(
                &stage
                    .bound_materials(&paths(&["/World/Lamp", "/World/Rock/Bottom"]), "")
                    .unwrap()
            ),
            "[[/Looks/Clay, /Looks/Clay, /World/Lamp.material:binding, direct, null, \
             strongerThanDescendants, , false], \
             [/Looks/Clay, /Looks/Clay, /World/Rock/Bottom.material:binding, direct, null, \
             weakerThanDescendants, , false]]"
        );
        // The bottom face completes the partition.
        let subsets = show(&stage.material_subsets("/World/Rock", "", None).unwrap());
        assert!(subsets.ends_with(", []]"), "{subsets}");
        assert!(
            subsets.contains("[/World/Rock/Bottom, Int([0]), [/Looks/Clay, "),
            "{subsets}"
        );
        assert_eq!(
            show(&stage.material("/Looks/Clay").unwrap()),
            "[/Looks/Stone]"
        );
        assert_eq!(
            show(
                &stage
                    .material_terminal("/Looks/Clay", "surface", &[])
                    .unwrap()
            ),
            "[/Looks/Clay/Surface, , [[/Looks/Clay/Surface.outputs:surface, \
             [/Looks/Clay.outputs:surface, /Looks/Clay/Surface.outputs:surface]]]]"
        );
        assert_eq!(
            show(&stage.ports("/Looks/Clay/Surface", "input").unwrap()),
            "[[/Looks/Clay/Surface.inputs:diffuseColor, input, color3f, [], \
             [/Looks/Clay/Texture.outputs:rgb], []], \
             [/Looks/Clay/Surface.inputs:roughness, input, float, [[page, basic]], [], []]]",
            "the base material's network shows through the specialize arc"
        );
        assert_eq!(
            show(&stage.shader("/Looks/Clay/Surface").unwrap()),
            "[UsdPreviewSurface, id, [[role, surface]]]",
            "the id shows through too"
        );
        assert_eq!(
            show(&stage.coord_sys_bindings("/World/Rock", false).unwrap()),
            "[[decal, /World/Rock.coordSys:decal:binding, /World/Projector]]"
        );

        let mut redo = stage.undo(&mut receipt).unwrap();
        assert_eq!(text(&stage), before, "undo restores the layer");
        stage.undo(&mut redo).unwrap();
        assert_eq!(
            show(&stage.material("/Looks/Clay").unwrap()),
            "[/Looks/Stone]"
        );
    }

    /// Refusals name the edit and change nothing.
    #[test]
    fn invalid_schema_edits_fail_with_their_codes_and_change_nothing() {
        let mut stage = open(SHADED);
        let before = text(&stage);
        let refused = |stage: &mut Stage, fill: &dyn Fn(&mut EditBatch)| {
            let mut batch = EditBatch::new();
            batch.set_active("/World/Projector".into(), false);
            fill(&mut batch);
            let error = stage.apply(root(stage), &mut batch).unwrap_err();
            assert_eq!(error.edit, Some(1), "{error}");
            (error.code, error.path.unwrap_or_default(), error.message)
        };
        let (code, path, message) = refused(&mut stage, &|batch| {
            batch.bind_material(
                "/World/Rock".into(),
                "/Looks/Missing".into(),
                String::new(),
                None,
                None,
                None,
            );
        });
        assert_eq!((code, path.as_str()), ("material.not_found", "/World/Rock"));
        assert!(
            message.contains("no prim is at the material path /Looks/Missing"),
            "{message}"
        );
        let (code, _, message) = refused(&mut stage, &|batch| {
            batch.connect_to_source(
                "/Looks/Stone/Surface.inputs:metallic".into(),
                "/Looks/Stone/Texture.outputs:rgb".into(),
                None,
                "replace".into(),
            );
        });
        assert_eq!(code, "shading.no_port");
        assert!(
            message.contains("/Looks/Stone/Surface.inputs:metallic"),
            "{message}"
        );
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.disconnect_source("/Looks/Stone/Surface.inputs:metallic".into(), None);
        });
        assert_eq!(code, "shading.no_port");
        let (code, ..) = refused(&mut stage, &|batch| {
            let value = batch.value_numbers("float".into(), vec![1.0]);
            batch.set_shader_input("/Looks/Gone/Surface.inputs:roughness".into(), None, value);
        });
        assert_eq!(code, "prim.not_found");
        let (code, ..) = refused(&mut stage, &|batch| {
            let value = batch.value_numbers("float".into(), vec![1.0]);
            batch.set_shader_input("/Looks/Stone/Surface.outputs:surface".into(), None, value);
        });
        assert_eq!(code, "path.invalid");
        let (code, ..) = refused(&mut stage, &|batch| {
            let value = batch.value_numbers("double".into(), vec![1.0]);
            batch.set_shader_input("/Looks/Stone/Surface.inputs:roughness".into(), None, value);
        });
        assert_eq!(code, "shading.type_mismatch");
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.set_base_material("/World/Rock".into(), Some("/Looks/Stone".into()));
        });
        assert_eq!(code, "material.not_material");
        let (code, _, message) = refused(&mut stage, &|batch| {
            batch.create_geom_subset(
                "/World/Rock".into(),
                "Far".into(),
                "face".into(),
                vec![7],
                "materialBind".into(),
                None,
                true,
            );
        });
        assert_eq!(code, "subset.indices");
        assert!(message.contains("index 7"), "{message}");
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.create_geom_subset(
                "/World".into(),
                "Top".into(),
                "face".into(),
                vec![0],
                "materialBind".into(),
                None,
                false,
            );
        });
        assert_eq!(code, "subset.geometry");
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.clear_coord_sys("/World/Rock".into(), "decal".into(), false);
        });
        assert_eq!(code, "coord_sys.no_binding");
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.set_sdr_metadata("/Looks/Stone".into(), "role".into(), Some("x".into()));
        });
        assert_eq!(code, "shading.not_shader");
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.bind_material(
                "/World/Rock".into(),
                "/Looks/Stone".into(),
                String::new(),
                Some("strongest".into()),
                None,
                None,
            );
        });
        assert_eq!(code, "argument.invalid");
        let (code, ..) = refused(&mut stage, &|batch| {
            batch.bind_material(
                "/World/Rock".into(),
                "/Looks/Stone".into(),
                String::new(),
                None,
                Some("/World.collection:all".into()),
                Some("bad:name".into()),
            );
        });
        assert_eq!(code, "material.binding_name");
        assert_eq!(text(&stage), before, "nothing changed");
    }

    #[test]
    fn unbinding_blocking_and_clearing_author_what_openusd_authors() {
        let mut stage = open(SHADED);
        let mut batch = EditBatch::new();
        batch.unbind_material("/World/Rock/Top".into(), String::new(), None);
        batch.block_coord_sys("/World/Rock".into(), "paint".into());
        batch.disconnect_source(
            "/Looks/Stone/Surface.inputs:diffuseColor".into(),
            Some("/Looks/Stone/Texture.outputs:rgb".into()),
        );
        batch.set_subset_family_type(
            "/World/Rock".into(),
            "materialBind".into(),
            "nonOverlapping".into(),
        );
        batch.set_sdr_metadata("/Looks/Stone/Surface".into(), "role".into(), None);
        // Any existing prim binds, whatever its type, as in OpenUSD; it
        // resolves no material.
        batch.bind_material(
            "/World/Lamp".into(),
            "/World/Projector".into(),
            String::new(),
            None,
            None,
            None,
        );
        stage.apply(root(&stage), &mut batch).unwrap();
        assert_eq!(
            show(&stage.bound_materials(&paths(&["/World/Lamp"]), "").unwrap()),
            "[[null, /World/Projector, /World/Lamp.material:binding, direct, null, \
             weakerThanDescendants, , false]]"
        );
        // The top face falls back to the mesh's inherited stone.
        assert!(
            show(
                &stage
                    .bound_materials(&paths(&["/World/Rock/Top"]), "")
                    .unwrap()
            )
            .starts_with("[[/Looks/Stone, /Looks/Stone, /World.material:binding")
        );
        // A blocked name binds nothing at the rock: an explicit empty
        // target list, and the name is inherited from further up.
        assert_eq!(
            show(
                &stage
                    .relationship("/World/Rock.coordSys:paint:binding")
                    .unwrap()
            ),
            "[false, []]"
        );
        assert_eq!(
            show(&stage.coord_sys_bindings("/World/Rock", true).unwrap()),
            "[[paint, /World.coordSys:paint:binding, /World/Projector]]"
        );
        assert!(
            show(&stage.ports("/Looks/Stone/Surface", "input").unwrap()).starts_with(
                "[[/Looks/Stone/Surface.inputs:diffuseColor, input, color3f, [], [], []]"
            )
        );
        assert_eq!(
            stage
                .subset_family_type("/World/Rock", "materialBind")
                .unwrap(),
            "nonOverlapping"
        );
        assert_eq!(
            show(&stage.shader("/Looks/Stone/Surface").unwrap()),
            "[UsdPreviewSurface, id, []]"
        );
        let mut batch = EditBatch::new();
        batch.clear_coord_sys("/World/Rock".into(), "paint".into(), true);
        stage.apply(root(&stage), &mut batch).unwrap();
        assert_eq!(
            stage
                .relationship("/World/Rock.coordSys:paint:binding")
                .unwrap(),
            crate::wire::Wire::Null,
            "the spec is removed"
        );
    }
}
