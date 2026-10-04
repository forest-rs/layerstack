// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Capture renderer inputs with checked reads, material subsets and bounded output.
use core::num::NonZeroUsize;
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, ListOp, LiveStage, PropertyPath, PropertySpec,
    PropertyType, StageOptions, TargetPath, Value, Variability,
};
use layerstack_schemas::{
    BindingCache, BindingOptions, MaterialPurpose, Scene, SchemaEdit, Time,
    point_instancer::InstanceTransformOptions,
    primvar::Primvar,
    usd_geom::{GeomSubset, GeomSubsetElementType, Mesh, PointInstancer},
    usd_shade::{Material, MaterialBindingApi},
    value,
};
use std::sync::Arc;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let layer = LayerId(1);
    store.insert_layer(Layer::new(layer));
    let mesh_path = store.path("/Mesh");
    let subset_path = store.path("/Mesh/Accent");
    let material_path = store.path("/BaseMaterial");
    let accent_path = store.path("/AccentMaterial");
    let instance_path = store.path("/Instances");
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let mut live = LiveStage::compose(
        &mut store,
        layer,
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let target = EditTarget::for_layer(layer);
    let mut edit = SchemaEdit::new(live.stage(), &mut store, target.clone());
    Material::define(&mut edit, material_path);
    Material::define(&mut edit, accent_path);
    let mesh = Mesh::define(&mut edit, mesh_path);
    mesh.set_points(
        &mut edit,
        &[[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
    );
    mesh.set_face_vertex_counts(&mut edit, &[3, 3]);
    mesh.set_face_vertex_indices(&mut edit, &[0, 1, 2, 0, 2, 3]);
    mesh.create_indexed_primvar(
        &mut edit,
        "st",
        PropertyType::new("texCoord2f", true, Value::Vec2f([0.; 2])),
        Value::from(vec![[0_f32, 0.], [1., 0.], [1., 1.], [0., 1.]]),
        &[0, 1, 2, 0, 2, 3],
        "faceVarying",
        1,
    )?;
    let subset = GeomSubset::define(&mut edit, subset_path);
    subset.set_element_type(&mut edit, GeomSubsetElementType::Face);
    subset.set_family_name(&mut edit, "materialBind");
    subset.set_indices(&mut edit, &[1]);
    MaterialBindingApi::apply(&mut edit, mesh_path)?;
    MaterialBindingApi::apply(&mut edit, subset_path)?;
    let instances = PointInstancer::define(&mut edit, instance_path);
    instances.set_prototypes(&mut edit, &[TargetPath::Prim(mesh_path)]);
    instances.set_proto_indices(&mut edit, &[0, 0, 0]);
    instances.set_positions(&mut edit, &[[0., 0., 0.], [2., 0., 0.], [4., 0., 0.]]);
    let mut transaction = edit.finish();
    // Binding targets and family type use USD's generic property-authoring path.
    let binding = store.tokens.intern("material:binding");
    for (path, material) in [(mesh_path, material_path), (subset_path, accent_path)] {
        transaction.create_property(
            target.property(PropertyPath::new(path, binding)),
            PropertySpec::relationship()
                .with_targets(ListOp::explicit(vec![TargetPath::Prim(material)])),
        );
    }
    let family = store.tokens.intern("subsetFamily:materialBind:familyType");
    let mut spec = PropertySpec::typed_attribute(PropertyType::new(
        "token",
        false,
        Value::Token(store.tokens.intern("")),
    ));
    spec.variability = Variability::Uniform;
    spec.default = Some(Value::Token(store.tokens.intern("nonOverlapping")));
    transaction.create_property(target.property(PropertyPath::new(mesh_path, family)), spec);
    live.apply(&mut store, &transaction)?;

    let scene = Scene::new(live.stage(), &store);
    let mesh = Mesh::new(&scene, mesh_path).unwrap();
    let points = mesh.try_points(Time::Default)?.ok_or("missing points")?;
    let mut bindings = BindingCache::new(MaterialPurpose::All, BindingOptions::default());
    let materials = bindings.material_binding_subsets(&scene, mesh_path, Time::Default)?;
    assert_eq!(
        materials.fallback.material,
        Some(material_path),
        "unassigned faces inherit the mesh material"
    );
    assert_eq!(
        materials.subsets[0].material.material,
        Some(accent_path),
        "the face subset overrides the mesh material"
    );

    let st = Primvar::new(&scene, mesh_path, "st")
        .unwrap()
        .validated_values(Time::Default)?
        .ok_or("missing texture coordinates")?;
    let uv = value::try_read_float2_array_shared(st.values(), &store.tokens)?
        .ok_or("incompatible texture coordinates")?;
    // An engine can write directly into its final vertex buffer; no USD flattened
    // array is needed between source values and the expanded vertex attributes.
    let vertex_uv: Vec<_> = (0..st.len())
        .map(|corner| uv[st.element_range(corner).unwrap().start])
        .collect();
    assert_eq!(vertex_uv.len(), 6, "two triangles require six UV corners");

    let instances = PointInstancer::new(&scene, instance_path)
        .unwrap()
        .prepare_instance_transforms(
            Time::Default,
            Time::Default,
            InstanceTransformOptions::default(),
        )?;
    let mut scratch = Vec::with_capacity(2);
    let mut placements = 0;
    instances.for_each_chunk(NonZeroUsize::new(2).unwrap(), &mut scratch, |chunk| {
        // Submit or convert this bounded slice to the engine's placement format.
        placements += chunk.len();
    });
    assert_eq!(placements, 3, "chunking preserves all instance placements");
    println!(
        "{} shared points, {} material subset, {} UV corners, {placements} streamed placements",
        points.len(),
        materials.subsets.len(),
        vertex_uv.len()
    );
    Ok(())
}
