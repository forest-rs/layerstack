// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::shading::preview_surface::{PreviewExpression, PreviewIssueKind, PreviewSurfaceNetwork};
use alloc::vec;
use layerstack::{
    EditTarget, InMemoryStore, Layer, LayerId, ListOp, LiveStage, PrimSpec, PropertySpec,
    Reference, StageOptions, TargetPath, Transaction,
};

fn attr(value: Value, ty: &str) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::new(ty, false, value.clone())).with_default(value)
}
fn port(
    store: &mut InMemoryStore,
    layer: &mut Layer,
    path: &str,
    name: &str,
    ty: &str,
    value: Option<Value>,
    source: Option<&str>,
) {
    let p = store.path(path);
    let n = store.tokens.intern(name);
    let scalar = match ty {
        "float" => Value::Float(0.),
        "float2" => Value::Vec2f([0.; 2]),
        "float3" | "color3f" => Value::Vec3f([0.; 3]),
        "float4" => Value::Vec4f([0.; 4]),
        "asset" => Value::Asset("".into()),
        "token" => Value::Token(store.tokens.intern("")),
        _ => Value::String("".into()),
    };
    let mut spec = PropertySpec::typed_attribute(PropertyType::new(ty, false, scalar));
    if let Some(value) = value {
        spec = spec.with_default(value);
    }
    if let Some(source) = source {
        spec.targets = Some(ListOp::explicit(vec![TargetPath::Property(
            store.property_path(source),
        )]));
    }
    layer.prims.get_mut(&p).unwrap().set_property(n, spec);
}
fn fixture() -> (InMemoryStore, LiveStage, PathId) {
    let mut store = InMemoryStore::default();
    let schemas = Arc::new(crate::openusd(&mut store.tokens));
    let mut weak = Layer::new(LayerId(2));
    for (path, ty, id) in [
        ("/Library", "Material", None),
        ("/Library/G", "NodeGraph", None),
        ("/Library/P", "Shader", Some("UsdPreviewSurface")),
        ("/Library/T", "Shader", Some("UsdUVTexture")),
        ("/Library/R", "Shader", Some("UsdPrimvarReader_float2")),
        ("/Library/X", "Shader", Some("UsdTransform2d")),
    ] {
        let p = store.path(path);
        let t = store.tokens.intern(ty);
        weak.insert_prim(p, PrimSpec::def().with_type_name(t));
        if let Some(id) = id {
            let id = store.tokens.intern(id);
            port(
                &mut store,
                &mut weak,
                path,
                "info:id",
                "token",
                Some(Value::Token(id)),
                None,
            );
        }
    }
    port(
        &mut store,
        &mut weak,
        "/Library",
        "outputs:surface",
        "token",
        None,
        Some("/Library/P.outputs:surface"),
    );
    port(
        &mut store,
        &mut weak,
        "/Library",
        "inputs:uvName",
        "string",
        Some(Value::String("weakUV".into())),
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/G",
        "inputs:name",
        "string",
        Some(Value::String("graphFallback".into())),
        Some("/Library.inputs:uvName"),
    );
    port(
        &mut store,
        &mut weak,
        "/Library/P",
        "outputs:surface",
        "token",
        None,
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/P",
        "inputs:diffuseColor",
        "color3f",
        None,
        Some("/Library/T.outputs:rgb"),
    );
    port(
        &mut store,
        &mut weak,
        "/Library/P",
        "inputs:roughness",
        "float",
        None,
        Some("/Library/T.outputs:r"),
    );
    port(
        &mut store,
        &mut weak,
        "/Library/T",
        "outputs:rgb",
        "float3",
        None,
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/T",
        "outputs:r",
        "float",
        None,
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/T",
        "inputs:file",
        "asset",
        Some(Value::Asset("../textures/paint.png".into())),
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/T",
        "inputs:st",
        "float2",
        None,
        Some("/Library/X.outputs:result"),
    );
    port(
        &mut store,
        &mut weak,
        "/Library/X",
        "outputs:result",
        "float2",
        None,
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/X",
        "inputs:in",
        "float2",
        None,
        Some("/Library/R.outputs:result"),
    );
    port(
        &mut store,
        &mut weak,
        "/Library/X",
        "inputs:rotation",
        "float",
        Some(Value::Float(45.)),
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/R",
        "outputs:result",
        "float2",
        None,
        None,
    );
    port(
        &mut store,
        &mut weak,
        "/Library/R",
        "inputs:varname",
        "string",
        None,
        Some("/Library/G.inputs:name"),
    );
    let mat = store.path("/Mat");
    let library = store.path("/Library");
    let mut root = Layer::new(LayerId(1));
    let mut spec = PrimSpec::def();
    spec.references = ListOp::prepended(vec![Reference::new(LayerId(2), library)]);
    root.insert_prim(mat, spec);
    port(
        &mut store,
        &mut root,
        "/Mat",
        "inputs:uvName",
        "string",
        Some(Value::String("surfaceUv".into())),
        None,
    );
    store.insert_layer(weak);
    store.insert_layer(root);
    let live = LiveStage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            with_provenance: false,
            ..StageOptions::default()
        },
    );
    (store, live, mat)
}
fn capture(store: &InMemoryStore, live: &LiveStage, mat: PathId) -> MaterialNetwork {
    MaterialNetwork::capture(
        &Scene::new(live.stage(), store),
        mat,
        MaterialTerminal::Surface,
        &[],
        Time::Default,
    )
    .unwrap()
}
#[test]
fn typed_preview_handoff_preserves_defaults_interfaces_shared_nodes_and_asset_anchors() {
    let (store, live, mat) = fixture();
    let network = Arc::new(capture(&store, &live, mat));
    assert_eq!(network.primvars, vec!["surfaceUv"]);
    let textures: Vec<_> = network
        .nodes
        .iter()
        .filter(|n| n.identifier.as_deref() == Some("UsdUVTexture"))
        .collect();
    assert_eq!(textures.len(), 1, "shared texture captured once");
    let file = textures[0]
        .input("file")
        .unwrap()
        .constant
        .as_ref()
        .unwrap()
        .value
        .asset_reference()
        .unwrap();
    assert_eq!(file.authored_path, "../textures/paint.png");
    assert_eq!(
        file.source.as_ref().unwrap().layer,
        LayerId(2),
        "reference keeps authoring layer with optional provenance disabled"
    );
    let surface = network.node(network.source.shader.unwrap()).unwrap();
    assert_eq!(
        surface
            .input("metallic")
            .unwrap()
            .constant
            .as_ref()
            .unwrap()
            .origin,
        MaterialValueOrigin::NodeDefault
    );
    assert!(
        surface.input("metallic").unwrap().own.value.is_none(),
        "definition fallback is not authored USD"
    );
    let preview = PreviewSurfaceNetwork::capture(network, 32);
    assert!(preview.is_complete(), "{:?}", preview.issues);
    let Some(PreviewExpression::Texture(texture)) =
        &preview.input("diffuseColor").unwrap().expression
    else {
        panic!("expected RGB texture expression")
    };
    assert_eq!(texture.file.source.as_ref().unwrap().layer, LayerId(2));
    let PreviewExpression::Transform2d {
        input, rotation, ..
    } = texture.coordinates.as_ref()
    else {
        panic!("expected coordinate transform")
    };
    assert_eq!(*rotation, 45.);
    assert!(matches!(input.as_ref(),PreviewExpression::Primvar {name,..} if name=="surfaceUv"));
    assert!(matches!(
        preview.input("roughness").unwrap().expression,
        Some(PreviewExpression::Texture(_))
    ));
}
#[test]
fn retained_identity_revisions_and_missed_change_history_recover() {
    let (mut store, mut live, mat) = fixture();
    let mut cache = MaterialNetworkCache::new(Time::Default, &[]);
    let first = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let same = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert!(
        Arc::ptr_eq(&first.network, &same.network),
        "cache hit keeps exact immutable handle"
    );
    let path = store.property_path("/Library/X.inputs:rotation");
    let mut tx = Transaction::new();
    tx.set_default(
        EditTarget::for_layer(LayerId(2)).property(path),
        Value::Float(90.),
    );
    live.apply(&mut store, &tx).unwrap();
    let changed = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(first.revisions.topology, changed.revisions.topology);
    assert_ne!(first.revisions.parameters, changed.revisions.parameters);
    assert_eq!(first.revisions.resources, changed.revisions.resources);
    let node = first
        .network
        .nodes
        .iter()
        .find(|n| n.identifier.as_deref() == Some("UsdTransform2d"))
        .unwrap();
    assert_eq!(
        node.input("rotation")
            .unwrap()
            .constant
            .as_ref()
            .unwrap()
            .value
            .value,
        Some(Value::Float(45.)),
        "old immutable output remains valid"
    );
    let file = store.property_path("/Library/T.inputs:file");
    let mut tx = Transaction::new();
    tx.set_default(
        EditTarget::for_layer(LayerId(2)).property(file),
        Value::Asset("../textures/new.png".into()),
    );
    live.apply(&mut store, &tx).unwrap();
    let resource = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(changed.revisions.topology, resource.revisions.topology);
    assert_ne!(changed.revisions.resources, resource.revisions.resources);
    let id = store.property_path("/Library/T.info:id");
    let unknown = store.tokens.intern("VendorTexture");
    let mut tx = Transaction::new();
    tx.set_default(
        EditTarget::for_layer(LayerId(2)).property(id),
        Value::Token(unknown),
    );
    live.apply(&mut store, &tx).unwrap();
    let graph = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_ne!(graph.revisions.topology, resource.revisions.topology);
    assert!(
        graph
            .network
            .issues
            .iter()
            .any(|i| matches!(i, MaterialIssue::UnknownNode(_)))
    );
    let preview = PreviewSurfaceNetwork::capture(graph.network, 32);
    assert!(
        !preview.is_complete(),
        "unknown texture is retained and reported"
    );
    assert!(
        preview
            .issues
            .iter()
            .any(|i| i.kind == PreviewIssueKind::UnknownNode)
    );
    let mut other = store.snapshot();
    let other_live = LiveStage::compose(&mut other, LayerId(1), StageOptions::default());
    assert!(matches!(
        cache.get(
            &Scene::new(other_live.stage(), &other),
            mat,
            MaterialTerminal::Surface
        ),
        Err(MaterialCaptureError::CacheDomain)
    ));
}
#[test]
fn invalid_connections_keep_native_authored_fallback_and_cycles_are_explicit() {
    let (mut store, mut live, mat) = fixture();
    let p = store.property_path("/Library/P.inputs:roughness");
    let missing = store.property_path("/Library/Missing.outputs:value");
    let mut tx = Transaction::new();
    tx.set_targets(
        EditTarget::for_layer(LayerId(2)).property(p),
        ListOp::explicit(vec![TargetPath::Property(missing)]),
    );
    tx.set_default(
        EditTarget::for_layer(LayerId(2)).property(p),
        Value::Float(0.4),
    );
    live.apply(&mut store, &tx).unwrap();
    let graph = capture(&store, &live, mat);
    let surface = graph.node(graph.source.shader.unwrap()).unwrap();
    assert_eq!(
        surface
            .input("roughness")
            .unwrap()
            .constant
            .as_ref()
            .unwrap()
            .value
            .value,
        Some(Value::Float(0.4))
    );
    assert!(graph.issues.iter().any(|i| matches!(
        i,
        MaterialIssue::Connection(ShadingIssue::InvalidTarget { .. })
    )));
    let x = store.property_path("/Library/X.inputs:in");
    let output = store.property_path("/Library/X.outputs:result");
    let mut tx = Transaction::new();
    tx.set_targets(
        EditTarget::for_layer(LayerId(2)).property(x),
        ListOp::explicit(vec![TargetPath::Property(output)]),
    );
    live.apply(&mut store, &tx).unwrap();
    let graph = Arc::new(capture(&store, &live, mat));
    assert!(
        graph
            .issues
            .iter()
            .any(|i| matches!(i, MaterialIssue::ShaderCycle(_)))
    );
    let preview = PreviewSurfaceNetwork::capture(graph, 32);
    assert!(
        preview
            .issues
            .iter()
            .any(|i| i.kind == PreviewIssueKind::Cycle)
    );
}
#[test]
fn context_interface_edits_and_deletion_recreation_refresh() {
    let (mut store, mut live, mat) = fixture();
    let mut cache = MaterialNetworkCache::new(Time::Default, &["missing"]);
    let first = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(first.network.source.context.as_deref(), Some(""));
    let name = store.property_path("/Mat.inputs:uvName");
    let mut tx = Transaction::new();
    tx.set_default(
        EditTarget::for_layer(LayerId(1)).property(name),
        Value::String("editedUv".into()),
    );
    let applied = live.apply(&mut store, &tx).unwrap();
    cache
        .apply_changes(&Scene::new(live.stage(), &store), &applied.changes)
        .unwrap();
    let edit = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(edit.network.primvars, vec!["editedUv"]);
    assert_eq!(first.revisions.topology, edit.revisions.topology);
    let terminal = store.property_path("/Mat.outputs:custom:surface");
    let source = store.property_path("/Mat/P.outputs:surface");
    let mut tx = Transaction::new();
    tx.create_property(
        EditTarget::for_layer(LayerId(1)).property(terminal),
        attr(Value::Token(store.tokens.intern("")), "token")
            .with_targets(ListOp::explicit(vec![TargetPath::Property(source)])),
    );
    live.apply(&mut store, &tx).unwrap();
    cache.set_render_contexts(&["custom"]);
    let context = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(context.network.source.context.as_deref(), Some("custom"));
    assert_ne!(context.revisions.topology, edit.revisions.topology);
    let target = EditTarget::for_layer(LayerId(1)).prim(mat);
    let mut tx = Transaction::new();
    tx.remove_spec(target);
    let removed = live.apply(&mut store, &tx).unwrap();
    assert!(matches!(
        cache.get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface
        ),
        Err(MaterialCaptureError::MissingMaterial(_))
    ));
    // Undo reinstates the same authored specs, but cannot revive released revisions.
    live.apply(&mut store, &removed.inverse).unwrap();
    let recreated = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_ne!(recreated.revisions.topology, context.revisions.topology);
    assert!(
        context.network.node(source.prim_path()).is_some(),
        "old capture remains readable after removal"
    );
}

#[derive(Debug)]
struct BrokenArray {
    error: ArrayReadError,
}
impl layerstack::DeferredArraySource for BrokenArray {
    fn materialize(&self) -> Result<&layerstack::TypedArray, &ArrayReadError> {
        Err(&self.error)
    }
    fn element_kind(&self) -> Value {
        Value::Float(0.)
    }
}
#[test]
fn arrays_decode_failures_and_depth_limits_remain_explicit() {
    let (mut store, mut live, mat) = fixture();
    let surface = store.path("/Library/P");
    let custom = store.tokens.intern("inputs:weights");
    let broken = store.tokens.intern("inputs:brokenWeights");
    let roughness = store.tokens.intern("inputs:roughness");
    let mut tx = Transaction::new();
    let target = EditTarget::for_layer(LayerId(2));
    tx.create_property(
        target.property(PropertyPath::new(surface, custom)),
        PropertySpec::typed_attribute(PropertyType::new("float", true, Value::Float(0.)))
            .with_default(Value::from(vec![1_f32, 2.])),
    );
    tx.set_targets(
        target.property(PropertyPath::new(surface, roughness)),
        ListOp::explicit(vec![]),
    );
    tx.create_property(
        target.property(PropertyPath::new(surface, broken)),
        PropertySpec::typed_attribute(PropertyType::new("float", true, Value::Float(0.)))
            .with_default(Value::TypedArray(layerstack::TypedArray::Deferred(
                Arc::new(BrokenArray {
                    error: ArrayReadError::InvalidData("fixture decode failed".into()),
                }),
            ))),
    );
    tx.set_default(
        target.property(PropertyPath::new(surface, roughness)),
        Value::Blocked,
    );
    live.apply(&mut store, &tx).unwrap();
    let graph = Arc::new(capture(&store, &live, mat));
    let node = graph.node(graph.source.shader.unwrap()).unwrap();
    assert_eq!(
        node.input("weights")
            .unwrap()
            .constant
            .as_ref()
            .unwrap()
            .value
            .value,
        Some(Value::from(vec![1_f32, 2.]))
    );
    assert!(
        node.input("brokenWeights")
            .unwrap()
            .own
            .decode_error
            .is_some(),
        "decode failure is retained"
    );
    assert!(
        node.input("roughness").unwrap().constant.is_none(),
        "node defaults must not conceal a block"
    );
    let preview = PreviewSurfaceNetwork::capture(graph.clone(), 32);
    assert!(
        preview
            .issues
            .iter()
            .any(|i| i.port.as_deref() == Some("roughness")
                && i.kind == PreviewIssueKind::Unavailable)
    );
    let bounded = PreviewSurfaceNetwork::capture(graph, 1);
    assert!(
        bounded
            .issues
            .iter()
            .any(|i| i.kind == PreviewIssueKind::DepthLimit)
    );
    let numeric = MaterialNetwork::capture(
        &Scene::new(live.stage(), &store),
        mat,
        MaterialTerminal::Surface,
        &[],
        Time::at(1.),
    )
    .unwrap();
    let surface = numeric.node(numeric.source.shader.unwrap()).unwrap();
    assert!(
        surface.input("roughness").unwrap().constant.is_none(),
        "numeric-time value blocks must not select definition defaults"
    );
    assert!(
        PreviewSurfaceNetwork::capture(Arc::new(numeric), 32)
            .issues
            .iter()
            .any(|i| i.port.as_deref() == Some("roughness")
                && i.kind == PreviewIssueKind::Unavailable)
    );
    // Author through the referenced source namespace.
    let source = store.path("/Library/P");
    let mut samples = Transaction::new();
    samples.set_time_sample(
        target.property(PropertyPath::new(source, roughness)),
        0.,
        Value::Blocked,
    );
    samples.set_time_sample(
        target.property(PropertyPath::new(source, roughness)),
        2.,
        Value::Float(0.75),
    );
    live.apply(&mut store, &samples).unwrap();
    for time in [Time::at(0.), Time::at(1.), Time::held(1.)] {
        let numeric = MaterialNetwork::capture(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
            &[],
            time,
        )
        .unwrap();
        let port = numeric
            .node(numeric.source.shader.unwrap())
            .unwrap()
            .input("roughness")
            .unwrap();
        assert!(
            port.constant.is_none(),
            "selected blocked sample at {time:?} remains unavailable"
        );
    }
    let numeric = MaterialNetwork::capture(
        &Scene::new(live.stage(), &store),
        mat,
        MaterialTerminal::Surface,
        &[],
        Time::at(2.),
    )
    .unwrap();
    assert_eq!(
        numeric
            .node(numeric.source.shader.unwrap())
            .unwrap()
            .input("roughness")
            .unwrap()
            .constant
            .as_ref()
            .unwrap()
            .value
            .value,
        Some(Value::Float(0.75)),
        "valid same-site sample still overrides the default block"
    );
}
#[test]
fn resource_provenance_and_numeric_time_have_separate_revisions() {
    let (mut store, mut live, mat) = fixture();
    let mut cache = MaterialNetworkCache::new(Time::Default, &[]);
    let first = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let path = store.path("/Mat/T");
    let file = store.property_path("/Mat/T.inputs:file");
    let mut tx = Transaction::new();
    let target = EditTarget::for_layer(LayerId(1));
    tx.create_prim(target.prim(path), layerstack::Specifier::Over, None);
    tx.create_property(
        target.property(file),
        PropertySpec::attribute().with_default(Value::Asset("../textures/paint.png".into())),
    );
    live.apply(&mut store, &tx).unwrap();
    let resource = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(resource.revisions.topology, first.revisions.topology);
    assert_eq!(
        resource.revisions.parameters, first.revisions.parameters,
        "same spelling/value remains the same parameter"
    );
    assert_ne!(
        resource.revisions.resources, first.revisions.resources,
        "winning asset source changed from referenced layer to root"
    );
    let rotation = store.property_path("/Library/X.inputs:rotation");
    let mut tx = Transaction::new();
    let target = EditTarget::for_layer(LayerId(2));
    tx.set_time_sample(target.property(rotation), 0., Value::Float(0.));
    tx.set_time_sample(target.property(rotation), 2., Value::Float(90.));
    live.apply(&mut store, &tx).unwrap();
    cache.set_time(Time::at(1.));
    let mid = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_eq!(mid.revisions.topology, resource.revisions.topology);
    assert_eq!(
        mid.revisions.parameters, resource.revisions.parameters,
        "numeric value equals the previously captured default"
    );
    cache.set_time(Time::at(2.));
    let end = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_ne!(end.revisions.parameters, mid.revisions.parameters);
    assert_eq!(end.revisions.resources, mid.revisions.resources);
}
#[test]
fn expired_notices_and_new_incoming_interfaces_refresh_without_false_hits() {
    let (mut store, mut live, mat) = fixture();
    let mut cursor = live.change_cursor();
    live.set_change_history_budget(layerstack::ChangeHistoryBudget {
        max_batches: 0,
        max_retained_bytes: 0,
    });
    let mut cache = MaterialNetworkCache::new(Time::Default, &[]);
    let first = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let alternate = store.property_path("/Mat.inputs:alternateName");
    let graph_input = store.property_path("/Library/G.inputs:name");
    let weak_alternate = store.property_path("/Library.inputs:alternateName");
    let mut tx = Transaction::new();
    tx.create_property(
        EditTarget::for_layer(LayerId(1)).property(alternate),
        attr(Value::String("incomingUv".into()), "string"),
    );
    tx.set_targets(
        EditTarget::for_layer(LayerId(2)).property(graph_input),
        ListOp::explicit(vec![TargetPath::Property(weak_alternate)]),
    );
    live.apply(&mut store, &tx).unwrap();
    assert!(matches!(
        live.changes_since(&mut cursor),
        Err(layerstack::ChangeHistoryError::Expired)
    ));
    let recovered = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert!(
        recovered.evaluated,
        "immutable dependencies repair expired notices"
    );
    assert_eq!(recovered.network.primvars, vec!["incomingUv"]);
    assert_ne!(recovered.revisions.topology, first.revisions.topology);
    let mut other = store.snapshot();
    let schemas = Arc::new(crate::openusd(&mut other.tokens));
    let other_live = LiveStage::compose(
        &mut other,
        LayerId(1),
        StageOptions {
            schemas: Some(schemas),
            ..StageOptions::default()
        },
    );
    cache.clear();
    assert!(
        cache
            .get(
                &Scene::new(other_live.stage(), &other),
                mat,
                MaterialTerminal::Surface
            )
            .is_ok(),
        "explicit clear rebinds cache domain"
    );
    assert!(
        PreviewSurfaceNetwork::capture(first.network, 32).is_complete(),
        "old immutable graph projects without looking up foreign store IDs"
    );
}

#[test]
fn shared_shader_edits_refresh_both_materials_and_unrelated_edits_keep_handles() {
    let (mut store, mut live, mat) = fixture();
    let second = store.path("/Second");
    let material_type = store.tokens.intern("Material");
    let terminal = store.property_path("/Second.outputs:surface");
    let source = store.property_path("/Mat/P.outputs:surface");
    let notes = store.path("/Notes");
    let label = store.property_path("/Notes.label");
    let mut tx = Transaction::new();
    let root = EditTarget::for_layer(LayerId(1));
    tx.create_prim(
        root.prim(second),
        layerstack::Specifier::Def,
        Some(material_type),
    );
    tx.create_property(
        root.property(terminal),
        PropertySpec::typed_attribute(PropertyType::new(
            "token",
            false,
            Value::Token(store.tokens.intern("")),
        ))
        .with_targets(ListOp::explicit(vec![TargetPath::Property(source)])),
    );
    tx.create_prim(root.prim(notes), layerstack::Specifier::Def, None);
    tx.create_property(
        root.property(label),
        attr(Value::String("initial".into()), "string"),
    );
    live.apply(&mut store, &tx).unwrap();
    let mut cache = MaterialNetworkCache::new(Time::Default, &[]);
    let first = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let other = cache
        .get(
            &Scene::new(live.stage(), &store),
            second,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let mut tx = Transaction::new();
    tx.set_default(root.property(label), Value::String("edited".into()));
    live.apply(&mut store, &tx).unwrap();
    let unchanged = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert!(
        Arc::ptr_eq(&first.network, &unchanged.network),
        "unrelated prim preserves exact handle"
    );
    let rotation = store.property_path("/Library/X.inputs:rotation");
    let mut tx = Transaction::new();
    tx.set_default(
        EditTarget::for_layer(LayerId(2)).property(rotation),
        Value::Float(75.),
    );
    live.apply(&mut store, &tx).unwrap();
    let a = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let b = cache
        .get(
            &Scene::new(live.stage(), &store),
            second,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_ne!(a.revisions.parameters, first.revisions.parameters);
    assert_ne!(b.revisions.parameters, other.revisions.parameters);
    assert_eq!(a.revisions.topology, first.revisions.topology);
    assert_eq!(b.revisions.topology, other.revisions.topology);
}

#[test]
fn composed_type_changes_reselect_terminal_and_restore_shader_defaults() {
    let (mut store, mut live, mat) = fixture();
    let mut cache = MaterialNetworkCache::new(Time::Default, &[]);
    let first = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    let shader = store.path("/Mat/P");
    let graph_type = store.tokens.intern("NodeGraph");
    let mut tx = Transaction::new();
    tx.create_prim(
        EditTarget::for_layer(LayerId(1)).prim(shader),
        layerstack::Specifier::Over,
        Some(graph_type),
    );
    let edited = live.apply(&mut store, &tx).unwrap();
    let graph = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert!(
        graph.network.source.selected().is_none(),
        "a disconnected container output no longer terminates shader traversal"
    );
    assert_ne!(graph.revisions.topology, first.revisions.topology);
    assert!(
        graph
            .preview_surface(32)
            .issues
            .iter()
            .any(|i| i.kind == PreviewIssueKind::MissingSurface)
    );
    live.apply(&mut store, &edited.inverse).unwrap();
    let restored = cache
        .get(
            &Scene::new(live.stage(), &store),
            mat,
            MaterialTerminal::Surface,
        )
        .unwrap();
    assert_ne!(restored.revisions.topology, graph.revisions.topology);
    assert!(
        restored.preview_surface(32).is_complete(),
        "restoring Shader type restores definition defaults and terminal behavior"
    );
}

#[test]
fn shared_container_outputs_at_different_depths_survive_both_branch_orderings() {
    for swap in [false, true] {
        let (mut store, mut live, mat) = fixture();
        let b = store.path("/Library/B");
        let shader = store.tokens.intern("Shader");
        let graph = store.path("/Library/G");
        let surface = store.path("/Library/P");
        let mut tx = Transaction::new();
        let target = EditTarget::for_layer(LayerId(2));
        tx.create_prim(target.prim(b), layerstack::Specifier::Def, Some(shader));
        let id = store.property_path("/Library/B.info:id");
        let vendor = store.tokens.intern("VendorMixer");
        tx.create_property(target.property(id), attr(Value::Token(vendor), "token"));
        let output = store.property_path("/Library/B.outputs:r");
        tx.create_property(
            target.property(output),
            PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.))),
        );
        let a = store.property_path("/Library/G.outputs:a");
        let b_output = store.property_path("/Library/G.outputs:b");
        tx.create_property(target.property(a), attr(Value::Float(0.2), "float"));
        tx.create_property(target.property(b_output), attr(Value::Float(0.3), "float"));
        let input = store.property_path("/Library/B.inputs:value");
        tx.create_property(
            target.property(input),
            PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.)))
                .with_targets(ListOp::explicit(vec![TargetPath::Property(b_output)])),
        );
        let roughness = PropertyPath::new(surface, store.tokens.intern("inputs:roughness"));
        let metallic = PropertyPath::new(surface, store.tokens.intern("inputs:metallic"));
        tx.create_property(
            target.property(metallic),
            PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.))),
        );
        for (port, source) in if swap {
            [(roughness, output), (metallic, a)]
        } else {
            [(roughness, a), (metallic, output)]
        } {
            tx.set_targets(
                target.property(port),
                ListOp::explicit(vec![TargetPath::Property(source)]),
            );
        }
        live.apply(&mut store, &tx).unwrap();
        let network = capture(&store, &live, mat);
        let composed_graph = store.path("/Mat/G");
        let g = network.node(composed_graph).unwrap();
        for name in ["a", "b"] {
            assert!(
                g.ports
                    .iter()
                    .any(|p| p.kind == PortKind::Output && p.name == name),
                "shared container output {name} retained (swap={swap})"
            );
        }
        assert!(
            network
                .dependencies
                .iter()
                .any(|d| d.prim == composed_graph && d.property == "outputs:b")
        );
        assert_eq!(store.paths.display(graph, &store.tokens), "/Library/G");
    }
}
