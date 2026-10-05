// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Retain textured native/point instances and consume independent component deltas.
//! Geometry and material graphs stay owned across edits; rendering belongs to the host.
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, LayerId, LiveStage, PathInterner,
    ResolvedAsset, StageOptions, TokenInterner, Transaction, TypedArray, Value,
};
use layerstack_schemas::{Time, scene_records::SceneObserver};
use std::sync::Arc;

struct NoAssets;
impl AssetResolver for NoAssets {
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(layerstack_schemas::openusd(&mut store.tokens));
    let parsed = layerstack_usda::parser::parse(
        r#"#usda 1.0
class Xform "Asset" {
    def Mesh "Geom" {
        point3f[] points = [(0,0,0), (1,0,0), (1,1,0), (0,1,0)]
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0,1,2,3]
        uniform token subdivisionScheme = "none"
        texCoord2f[] primvars:st = [(0,0), (1,0), (1,1), (0,1)] (interpolation = "faceVarying")
    }
}
def Xform "A" (instanceable = true; prepend references = </Asset>) {
    double3 xformOp:translate = (0,0,0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    rel material:binding = </Paint>
}
def Xform "B" (instanceable = true; prepend references = </Asset>) {
    double3 xformOp:translate = (3,0,0)
    uniform token[] xformOpOrder = ["xformOp:translate"]
    rel material:binding = </Paint>
}
def Mesh "PointPrototype" {
    point3f[] points = [(0,0,0), (1,0,0), (0,1,0)]
    int[] faceVertexCounts = [3]
    int[] faceVertexIndices = [0,1,2]
    uniform token subdivisionScheme = "none"
    texCoord2f[] primvars:st = [(0,0), (1,0), (0,1)] (interpolation = "vertex")
    rel material:binding = </Paint>
}
def PointInstancer "Scatter" {
    rel prototypes = [</PointPrototype>]
    int[] protoIndices = [0,0]
    point3f[] positions = [(0,3,0), (3,3,0)]
    int64[] ids = [42,99]
    int64[] invisibleIds = [42]
}
def Material "Paint" {
    token outputs:surface.connect = </Paint/Surface.outputs:surface>
    def Shader "Surface" {
        uniform token info:id = "UsdPreviewSurface"
        token outputs:surface
        float inputs:roughness = 0.3
        color3f inputs:diffuseColor.connect = </Paint/Texture.outputs:rgb>
    }
    def Shader "Texture" {
        uniform token info:id = "UsdUVTexture"
        asset inputs:file = @textures/paint.png@
        float2 inputs:st.connect = </Paint/UV.outputs:result>
        float3 outputs:rgb
    }
    def Shader "UV" {
        uniform token info:id = "UsdPrimvarReader_float2"
        string inputs:varname = "st"
        float2 outputs:result
    }
}
"#,
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let emitted = layerstack_usda::emit::emit(
        &parsed.layer,
        LayerId(1),
        &mut store.tokens,
        &mut store.paths,
        &mut NoAssets,
    );
    assert!(emitted.diagnostics.is_empty(), "{:?}", emitted.diagnostics);
    store.insert_layer(emitted.layer);
    let mut live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..Default::default()
        },
    );
    let a = store.path("/A/Geom");
    let b = store.path("/B/Geom");
    let scatter = store.path("/Scatter");
    let paint = store.path("/Paint");
    let mut observer = SceneObserver::default();
    let initial = observer.update(&mut live, &mut store, Time::Default)?;
    let geometry = observer.record_at(a).unwrap().mesh.clone().unwrap();
    assert!(
        Arc::ptr_eq(
            &geometry,
            observer.record_at(b).unwrap().mesh.as_ref().unwrap()
        ),
        "native descendants share one polygon record"
    );
    let point_inputs = observer
        .record_at(scatter)
        .unwrap()
        .point_instances
        .clone()
        .unwrap();
    assert_eq!(
        point_inputs
            .surviving()
            .map(|i| (i.index, i.id))
            .collect::<Vec<_>>(),
        [(1, 99)],
        "mask compaction preserves original indices and authored IDs"
    );
    let material = observer.material(paint).unwrap().clone();
    let preview = material.preview_surface(16);
    assert!(preview.is_complete(), "{:?}", preview.issues);
    assert_eq!(
        material.network.primvars,
        ["st"],
        "texture network requires the UV primvar"
    );
    println!(
        "Initial: {} records, {} polygon sources, {} material nodes, {:?}.",
        initial.added.len(),
        initial.work.meshes,
        material.network.nodes.len(),
        material.revisions
    );

    let target = EditTarget::for_layer(LayerId(1));
    let translation = store.property_path("/A.xformOp:translate");
    let mut transaction = Transaction::new();
    transaction.set_default(target.property(translation), Value::Vec3d([1., 0., 0.]));
    live.apply(&mut store, &transaction)?;
    let transform = observer.update(&mut live, &mut store, Time::Default)?;
    assert_eq!(
        transform.work.meshes, 0,
        "transform edits skip geometry validation"
    );
    assert!(
        transform
            .changed
            .iter()
            .all(|c| c.components.transform && !c.components.geometry),
        "transform edit changes only transform components"
    );
    assert!(
        Arc::ptr_eq(
            &geometry,
            observer.record_at(a).unwrap().mesh.as_ref().unwrap()
        ),
        "transform edit retains polygon owners"
    );
    assert!(
        Arc::ptr_eq(
            &point_inputs,
            observer
                .record_at(scatter)
                .unwrap()
                .point_instances
                .as_ref()
                .unwrap()
        ),
        "transform edit retains point-instance inputs"
    );
    assert!(
        Arc::ptr_eq(
            &material.network,
            &observer.material(paint).unwrap().network
        ),
        "transform edit retains the material graph"
    );
    println!(
        "Transform: {} component updates, {} geometry validations.",
        transform.changed.len(),
        transform.work.meshes
    );

    let roughness = store.property_path("/Paint/Surface.inputs:roughness");
    let mut transaction = Transaction::new();
    transaction.set_default(target.property(roughness), Value::Float(0.7));
    live.apply(&mut store, &transaction)?;
    let shading = observer.update(&mut live, &mut store, Time::Default)?;
    let changed_material = observer.material(paint).unwrap().clone();
    assert_eq!(
        shading.materials,
        [paint],
        "parameter edit reports its referenced material"
    );
    assert!(
        shading.changed.is_empty(),
        "parameter edit leaves geometry records unchanged"
    );
    assert_eq!(
        material.revisions.topology, changed_material.revisions.topology,
        "roughness leaves topology unchanged"
    );
    assert_ne!(
        material.revisions.parameters, changed_material.revisions.parameters,
        "roughness changes captured parameters"
    );
    assert_eq!(
        material.revisions.resources, changed_material.revisions.resources,
        "roughness leaves authored resources unchanged"
    );
    assert!(
        Arc::ptr_eq(
            &geometry,
            observer.record_at(a).unwrap().mesh.as_ref().unwrap()
        ),
        "material edit retains polygon owners"
    );
    assert!(
        Arc::ptr_eq(
            &point_inputs,
            observer
                .record_at(scatter)
                .unwrap()
                .point_instances
                .as_ref()
                .unwrap()
        ),
        "material edit retains point-instance inputs"
    );
    assert!(
        changed_material.preview_surface(16).is_complete(),
        "parameter edit preserves a complete typed handoff"
    );
    println!(
        "Material: {} upstream graph update, parameter revision {}.",
        shading.materials.len(),
        changed_material.revisions.parameters
    );

    let positions = store.property_path("/Scatter.positions");
    let mut transaction = Transaction::new();
    transaction.set_default(
        target.property(positions),
        Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![
            [0., 4., 0.],
            [3., 4., 0.],
        ]))),
    );
    live.apply(&mut store, &transaction)?;
    let points = observer.update(&mut live, &mut store, Time::Default)?;
    assert_eq!(
        points.work.point_instancers, 1,
        "position edit recaptures one instancer"
    );
    assert_eq!(
        points.work.meshes, 0,
        "position edit skips polygon validation"
    );
    assert!(
        points
            .changed
            .iter()
            .all(|c| c.components.point_instances && !c.components.geometry),
        "position edit reports only point-input changes"
    );
    assert!(
        Arc::ptr_eq(
            &geometry,
            observer.record_at(a).unwrap().mesh.as_ref().unwrap()
        ),
        "position edit retains unrelated polygons"
    );
    assert!(
        Arc::ptr_eq(
            &changed_material.network,
            &observer.material(paint).unwrap().network
        ),
        "position edit retains the upstream material graph"
    );
    println!(
        "Point edit: {} original instances checked; geometry and material owners retained.",
        points.work.point_instances
    );
    Ok(())
}
