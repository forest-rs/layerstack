// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, string::String};
use core::fmt;
use layerstack::{PathId, PropertyType, Value};
use layerstack_schemas::{BatchError, BatchPrim, LayerBatch, usd_geom};

use crate::{
    Mesh, MeshProblem,
    build::{MeshAttribute, mesh_geometry},
    mesh::MeshValue,
};

/// Why a polygon mesh could not be authored into a fresh layer.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum MeshAuthorError {
    /// The geometry failed the standalone exporter's mesh validation.
    Geometry(MeshProblem),
    /// The batch rejected the destination, schema or attribute.
    Batch(BatchError),
    /// Material bindings and subsets reference materials by standalone scene name.
    SceneMaterials,
    /// The mesh carries standalone writer custom attributes.
    CustomAttributes,
    /// A user-named primvar occupies the reserved `:indices` companion name.
    ReservedPrimvarName(String),
}

impl fmt::Display for MeshAuthorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Geometry(problem) => problem.fmt(f),
            Self::Batch(error) => error.fmt(f),
            Self::SceneMaterials => f.write_str(
                "batch mesh authoring requires geometry without scene material bindings",
            ),
            Self::CustomAttributes => f.write_str(
                "batch mesh authoring requires geometry without custom writer attributes",
            ),
            Self::ReservedPrimvarName(name) => {
                write!(f, "primvar name {name} uses the reserved :indices suffix")
            }
        }
    }
}

impl core::error::Error for MeshAuthorError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Batch(error) => Some(error),
            _ => None,
        }
    }
}

impl From<BatchError> for MeshAuthorError {
    fn from(error: BatchError) -> Self {
        Self::Batch(error)
    }
}

impl From<MeshProblem> for MeshAuthorError {
    fn from(problem: MeshProblem) -> Self {
        Self::Geometry(problem)
    }
}

impl Mesh<'_> {
    /// Authors this polygon mesh within a caller-owned fresh layer.
    ///
    /// `path` comes from the batch's store and supplies the entire USD destination.
    /// Standalone scene export uses [`Self::name`]. The batch must use a registry with
    /// the OpenUSD geometry schemas. Points, topology, UVs, normals and
    /// additional primvars are validated as in standalone export and written
    /// directly as native layer records. Face-varying indices keep their own
    /// topology, including seams between equal-valued elements.
    ///
    /// A failed call leaves the batch's authored records unchanged. Scene-relative
    /// bindings, material subsets, and custom writer attributes are rejected.
    /// To bind a material by prim path, finish and compose the layer, then use
    /// [`MaterialBindingApiEdit::bind`](layerstack_schemas::usd_shade::MaterialBindingApiEdit::bind)
    /// in a [`SchemaEdit`](layerstack_schemas::SchemaEdit).
    ///
    /// ```
    /// use layerstack::{InMemoryStore, LayerId};
    /// use layerstack_mesh_export::{Faces, Mesh, Primvar};
    /// use layerstack_schemas::{LayerBatch, usd_geom::Xform};
    ///
    /// let mut store = InMemoryStore::default();
    /// let schemas = layerstack_schemas::openusd(store.tokens());
    /// let mut batch = LayerBatch::new(LayerId(1), &store, &schemas)?;
    /// batch.define(store.try_path("/World")?, Xform::SCHEMA, |_| Ok(()))?;
    /// let mesh = Mesh::new("Panel", vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
    ///     Faces::triangles(vec![0, 1, 2]))
    ///     .with_uvs(Primvar::face_varying(vec![[0., 0.], [1., 0.], [0., 1.]]));
    /// mesh.author(&mut batch, store.try_path("/World/Panel")?)?;
    /// store.insert_layer(batch.finish());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn author(&self, batch: &mut LayerBatch<'_>, path: PathId) -> Result<(), MeshAuthorError> {
        if self.material.is_some() || !self.material_subsets.is_empty() {
            return Err(MeshAuthorError::SceneMaterials);
        }
        if !self.attributes.is_empty() {
            return Err(MeshAuthorError::CustomAttributes);
        }
        if let Some(primvar) = self.primvars.iter().find(|p| p.name.ends_with(":indices")) {
            return Err(MeshAuthorError::ReservedPrimvarName(
                primvar.name.as_ref().into(),
            ));
        }
        batch.try_define(path, usd_geom::Mesh::SCHEMA, |prim| {
            mesh_geometry(self, |attribute| {
                write_attribute(prim, attribute).map_err(MeshAuthorError::from)
            })?;
            Ok(())
        })
    }
}

fn write_attribute(
    prim: &mut BatchPrim<'_>,
    attribute: MeshAttribute<'_>,
) -> Result<(), BatchError> {
    let value = match attribute.value {
        MeshValue::Bool(value) => Value::Bool(value),
        MeshValue::Token(value) => Value::Token(prim.token(value)),
        MeshValue::TokenArray(values) => Value::from(
            values
                .iter()
                .map(|token| prim.token(token))
                .collect::<alloc::vec::Vec<_>>(),
        ),
        MeshValue::IntArray(values) => Value::from(values),
        MeshValue::UIntArray(values) => Value::from(values),
        MeshValue::FloatArray(values) => Value::from(values),
        MeshValue::Float2Array(values) => Value::from(values),
        MeshValue::Float3Array(values) => Value::from(values),
        MeshValue::Float4Array(values) => Value::from(values),
        MeshValue::Matrix4d(rows) => Value::Matrix4d(Box::new(
            rows.as_flattened().try_into().expect("four rows of four"),
        )),
    };
    if attribute.declared {
        prim.set(attribute.name, value)?;
    } else {
        let ty = PropertyType::parse(attribute.type_name).expect("geometry uses registered types");
        prim.define_attribute(attribute.name, ty, value)?;
    }
    if let Some(interpolation) = attribute.interpolation {
        prim.set_interpolation(attribute.name, interpolation)?;
    }
    Ok(())
}
