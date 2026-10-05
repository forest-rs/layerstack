// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Request terrain and dependent scatter through an explicit procedural registry.
//! Publication, output manifests and resource snapshots remain host-owned.

use layerstack::{
    Applied, EditTarget, InMemoryStore, Layer, LayerId, LayerStore, LiveStage, PathId, PrimSpec,
    PropertyPath, PropertySpec, PropertyType, Stage, StageOptions, TargetPath, Time, Transaction,
    TypedArray, Value,
};
use layerstack_schemas::{
    GeneratedMesh, Scene, SchemaEdit, ValidatedMesh,
    procedural::{
        ProceduralEvaluator, ProceduralInputs,
        graph::{ProceduralGraph, ProducerPublisher, ProducerSpec, ResourceRevisions},
    },
    usd_geom::PointInstancer,
};
use std::{
    collections::BTreeMap,
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
};

const ROOT: LayerId = LayerId(1);
type Failure = Box<dyn Error>;

#[derive(Debug)]
enum Output {
    Terrain(ValidatedMesh),
    Scatter {
        positions: Arc<Vec<[f32; 3]>>,
        indices: Arc<Vec<i32>>,
    },
}
#[derive(Debug)]
enum Generator {
    Terrain(Arc<AtomicU32>),
    Scatter(PathId),
}
impl ProceduralEvaluator for Generator {
    type Output = Output;
    type Error = Failure;
    fn system(&self) -> &str {
        "example:world"
    }
    fn evaluate(&self, inputs: &mut ProceduralInputs<'_>) -> Result<Output, Failure> {
        match self {
            Self::Terrain(resource) => {
                let height = inputs
                    .parameter("height")?
                    .as_ref()
                    .and_then(|v| layerstack_schemas::value::read_float(v, inputs.tokens()))
                    .ok_or("terrain needs a readable height")?;
                let height = height * f32::from_bits(resource.load(Ordering::Relaxed));
                let mesh = GeneratedMesh {
                    points: Arc::new(vec![
                        [0., height, 0.],
                        [1., height, 0.],
                        [1., height, 1.],
                        [0., height, 1.],
                    ]),
                    face_vertex_counts: Arc::new(vec![4]),
                    face_vertex_indices: Arc::new(vec![0, 1, 2, 3]),
                    primvars: Vec::new(),
                }
                .into_validated()?;
                Ok(Output::Terrain(mesh))
            }
            Self::Scatter(terrain) => {
                let value = inputs
                    .attribute(*terrain, "points")?
                    .ok_or("terrain has no points")?;
                let points =
                    layerstack_schemas::value::read_float3_array_shared(&value, inputs.tokens())
                        .ok_or("terrain points have the wrong type")?;
                let positions = Arc::new(
                    points
                        .iter()
                        .map(|p| [p[0] * 3., p[1], p[2] * 3.])
                        .collect::<Vec<_>>(),
                );
                let indices = Arc::new(vec![0; positions.len()]);
                Ok(Output::Scatter { positions, indices })
            }
        }
    }
}

#[derive(Default)]
struct Publisher {
    manifests: BTreeMap<PathId, Vec<String>>,
    pending: BTreeMap<PathId, Vec<String>>,
    terrain: Option<PathId>,
}
impl ProducerPublisher<Output> for Publisher {
    type Error = Failure;
    fn prepare(
        &mut self,
        spec: &ProducerSpec,
        output: &Output,
        stage: &Stage,
        store: &mut dyn LayerStore,
    ) -> Result<Transaction, Failure> {
        let target = EditTarget::for_layer(ROOT);
        match output {
            Output::Terrain(mesh) => {
                let previous = self
                    .manifests
                    .get(&spec.recipe)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let publication = mesh.prepare(stage, store, &target, spec.outputs[0], previous)?;
                self.pending.insert(spec.recipe, publication.properties);
                Ok(publication.transaction)
            }
            Output::Scatter { positions, indices } => {
                let site = spec.outputs[0];
                let terrain = self.terrain.ok_or("no terrain prototype registered")?;
                let desired = [
                    (
                        "positions",
                        Value::TypedArray(TypedArray::Vec3f(positions.clone())),
                    ),
                    (
                        "protoIndices",
                        Value::TypedArray(TypedArray::Int(indices.clone())),
                    ),
                ];
                let arrays_match = desired.iter().all(|(name, desired)| {
                    store
                        .tokens()
                        .lookup(name)
                        .and_then(|token| {
                            store.layer(ROOT)?.property(PropertyPath::new(site, token))
                        })
                        .and_then(|p| p.default.as_ref())
                        .is_some_and(|v| v.same_representation(desired))
                });
                let prototypes_match = store
                    .tokens()
                    .lookup("prototypes")
                    .and_then(|token| store.layer(ROOT)?.property(PropertyPath::new(site, token)))
                    .is_some_and(|p| {
                        p.targets.as_ref().is_some_and(|targets| {
                            targets.explicit.as_deref() == Some(&[TargetPath::Prim(terrain)])
                        })
                    });
                if arrays_match
                    && prototypes_match
                    && PointInstancer::new(&Scene::new(stage, store), site).is_some()
                {
                    let mut transaction = Transaction::new();
                    transaction.expect_generation(
                        ROOT,
                        store
                            .layer(ROOT)
                            .ok_or("missing output layer")?
                            .generation(),
                    );
                    return Ok(transaction);
                }
                let generation = store
                    .layer(ROOT)
                    .ok_or("missing output layer")?
                    .generation();
                let existing =
                    PointInstancer::new(&Scene::new(stage, store), site).map(|view| view.edit());
                let mut edit = SchemaEdit::new(stage, store, target);
                let instancer = existing.unwrap_or_else(|| PointInstancer::define(&mut edit, site));
                instancer.set_positions_shared(&mut edit, positions.clone());
                instancer.set_proto_indices_shared(&mut edit, indices.clone());
                instancer.set_prototypes(&mut edit, &[TargetPath::Prim(terrain)]);
                let mut transaction = edit.finish();
                transaction.expect_generation(ROOT, generation);
                Ok(transaction)
            }
        }
    }
    fn committed(&mut self, spec: &ProducerSpec, _: &Applied) {
        if let Some(manifest) = self.pending.remove(&spec.recipe) {
            self.manifests.insert(spec.recipe, manifest);
        }
    }
}

fn main() -> Result<(), Failure> {
    let mut store = InMemoryStore::default();
    let terrain_recipe = store.path("/Recipes/Terrain");
    let scatter_recipe = store.path("/Recipes/Scatter");
    let terrain = store.path("/Assets/Terrain");
    let scatter = store.path("/World/Scatter");
    let ty = store.tokens.intern("GenerativeProcedural");
    let system_name = store.tokens.intern("proceduralSystem");
    let system = store.tokens.intern("example:world");
    let height = store.property_path("/Recipes/Terrain.primvars:height");
    let mut layer = Layer::new(ROOT);
    // Low-level layer authoring includes ordered child lists; schema publication
    // subsequently maintains the output sites under these parents.
    let pseudo_root = PrimSpec::default().with_children(
        ["Recipes", "Assets", "World"]
            .map(|name| store.tokens.intern(name))
            .to_vec(),
    );
    layer.insert_prim(store.path("/"), pseudo_root);
    for path in ["/Recipes", "/Assets", "/World"] {
        let mut parent = PrimSpec::def();
        if path == "/Recipes" {
            parent.authored_children = ["Terrain", "Scatter"]
                .map(|name| store.tokens.intern(name))
                .to_vec();
        }
        layer.insert_prim(store.path(path), parent);
    }
    for recipe in [terrain_recipe, scatter_recipe] {
        let mut prim = PrimSpec::def();
        prim.type_name = Some(ty);
        layer.insert_prim(recipe, prim);
        layer.set_property(
            PropertyPath::new(recipe, system_name),
            PropertySpec::typed_attribute(PropertyType::new("token", false, Value::Token(system)))
                .with_default(Value::Token(system)),
        );
    }
    layer.set_property(
        height,
        PropertySpec::typed_attribute(PropertyType::new("float", false, Value::Float(0.)))
            .with_default(Value::Float(1.)),
    );
    store.insert_layer(layer);
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
        ..Default::default()
    };
    let mut live = LiveStage::compose(&mut store, ROOT, options);
    let heightmap = Arc::new(AtomicU32::new(1_f32.to_bits()));
    let mut graph = ProceduralGraph::new(&store);
    graph.register(
        &store,
        ProducerSpec {
            recipe: terrain_recipe,
            outputs: vec![terrain],
            inputs: Vec::new(),
            resources: vec!["heightmap".into()],
        },
        Generator::Terrain(heightmap.clone()),
    )?;
    graph.register(
        &store,
        ProducerSpec {
            recipe: scatter_recipe,
            outputs: vec![scatter],
            inputs: vec![terrain],
            resources: Vec::new(),
        },
        Generator::Scatter(terrain),
    )?;
    let mut publisher = Publisher {
        terrain: Some(terrain),
        ..Default::default()
    };
    let mut revisions = ResourceRevisions::from([("heightmap".into(), 1)]);
    let initial = graph
        .request(
            &mut live,
            &mut store,
            scatter,
            Time::Default,
            &revisions,
            &mut publisher,
        )
        .map_err(|e| e.to_string())?;
    assert_eq!(
        initial.steps.len(),
        2,
        "request builds both producers in dependency order"
    );
    let unchanged = graph
        .request(
            &mut live,
            &mut store,
            scatter,
            Time::Default,
            &revisions,
            &mut publisher,
        )
        .map_err(|e| e.to_string())?;
    assert!(
        unchanged
            .steps
            .iter()
            .all(|s| !s.evaluated && s.changes == layerstack::Changes::default()),
        "unchanged request reuses generated buffers without authoring"
    );
    heightmap.store(2_f32.to_bits(), Ordering::Relaxed);
    revisions.insert("heightmap".into(), 2);
    assert!(
        initial.steps[1]
            .verify(&Scene::new(live.stage(), &store), &revisions)
            .is_err(),
        "old downstream work includes the prerequisite resource guard"
    );
    let rebuilt = graph
        .request(
            &mut live,
            &mut store,
            scatter,
            Time::Default,
            &revisions,
            &mut publisher,
        )
        .map_err(|e| e.to_string())?;
    assert!(
        rebuilt.steps.iter().all(|s| s.evaluated),
        "heightmap change rebuilds terrain and changed scatter inputs"
    );
    let mesh = layerstack_schemas::usd_geom::Mesh::new(&Scene::new(live.stage(), &store), terrain)
        .ok_or("missing terrain mesh")?;
    assert_eq!(
        mesh.subdivision_scheme().as_ref().map(|v| v.as_str()),
        Some("none"),
        "publisher authors polygon mesh semantics"
    );
    let scene = Scene::new(live.stage(), &store);
    let instancer = PointInstancer::new(&scene, scatter).ok_or("missing scatter")?;
    assert_eq!(
        instancer
            .try_positions(Time::Default)?
            .ok_or("missing placements")?
            .len(),
        4,
        "scatter publishes one placement per terrain point"
    );
    if let Some(directory) = std::env::args_os().nth(1) {
        std::fs::create_dir_all(&directory)?;
        let document = layerstack_usda::save::layer_document(
            &store.layers[&ROOT],
            &store.tokens,
            &store.paths,
        )?;
        std::fs::write(
            std::path::Path::new(&directory).join("procedural_world.usda"),
            document.to_usda()?,
        )?;
    }
    println!(
        "Terrain and scatter built, reused unchanged buffers, and rebuilt after heightmap revision: {:?}",
        graph.work()
    );
    Ok(())
}
