// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The runnable publication fixture is also exercised by conformance tests and benchmarks.
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, LayerId, LayerStore, LiveStage,
    PathId, PathInterner, PropertyKind, PropertySpec, PropertyType, ResolvedAsset, StageOptions,
    TargetPath, Time, TokenInterner, TypedArray, Value,
};
use layerstack_schemas::{
    GeneratedMesh, MeshPrimvar, MeshPublication, Scene, SchemaEdit, ValidatedMesh,
    procedural::{
        EvidenceApplyError, Procedural, ProceduralError, ProceduralEvaluator, ProceduralInputError,
        ProceduralInputs,
    },
    usd_geom::PointInstancer,
};
use std::sync::Arc;

/// IDs owned by the host, the reusable-asset producer and the terrain producer.
pub(crate) const ROOT: LayerId = LayerId(1);
/// The reusable asset's authored layer.
pub(crate) const ASSET: LayerId = LayerId(2);
/// Exedra's dedicated geometry layer.
pub(crate) const TERRAIN: LayerId = LayerId(3);

/// Resolves this example's already imported layer identities.
pub(crate) struct Sources;
impl AssetResolver for Sources {
    fn resolve(
        &mut self,
        asset: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        let layer_id = match asset {
            "assets.usd" => ASSET,
            "terrain.usd" => TERRAIN,
            _ => return Err(AssetResolveError::NotFound),
        };
        Ok(ResolvedAsset {
            layer_id,
            resolved_path: asset.into(),
            layer: None,
        })
    }
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
}

/// Imports a fixture layer with the same strict diagnostics as the runnable example.
pub(crate) fn import(store: &mut InMemoryStore, text: &str, id: LayerId) {
    let read =
        layerstack_usda::read_usda(text, id, &mut store.tokens, &mut store.paths, &mut Sources);
    assert!(
        read.parse_diagnostics.is_empty(),
        "{:?}",
        read.parse_diagnostics
    );
    assert!(
        read.lower_diagnostics.is_empty(),
        "{:?}",
        read.lower_diagnostics
    );
    assert!(!read.emitted.rejected, "{:?}", read.emitted.diagnostics);
    store.insert_layer(read.emitted.layer);
}

/// A two-triangle asset with equal-valued but separately indexed normal and UV seams.
pub(crate) fn mesh() -> GeneratedMesh {
    let primvar = |name: &str, ty, zero, value| MeshPrimvar {
        name: name.into(),
        type_name: PropertyType::new(ty, true, zero),
        value,
        interpolation: "faceVarying".into(),
        element_size: 1,
        indices: Some(Arc::new(vec![0, 1, 2, 3, 4, 5])),
    };
    GeneratedMesh {
        points: Arc::new(vec![[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]]),
        face_vertex_counts: Arc::new(vec![3, 3]),
        face_vertex_indices: Arc::new(vec![0, 1, 2, 0, 2, 3]),
        primvars: vec![
            primvar(
                "primvars:st",
                "texCoord2f",
                Value::Vec2f([0.; 2]),
                Value::TypedArray(TypedArray::Vec2f(Arc::new(vec![
                    [0., 0.],
                    [1., 0.],
                    [1., 1.],
                    [0., 0.],
                    [1., 1.],
                    [0., 1.],
                ]))),
            ),
            primvar(
                "primvars:normals",
                "normal3f",
                Value::Vec3f([0.; 3]),
                Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![[0., 0., 1.]; 6]))),
            ),
        ],
    }
}

/// Geometry and a texture asset reference, evaluated without stage/file side effects.
#[derive(Clone, Debug)]
pub(crate) struct GeneratedAsset {
    pub(crate) geometry: ValidatedMesh,
    pub(crate) texture: Option<Arc<str>>,
}

/// Example host evaluator; real Exedra/Sylva engines implement the same input seam.
#[derive(Debug)]
pub(crate) struct AssetGenerator {
    system: &'static str,
    pub(crate) template: GeneratedMesh,
    external_points: bool,
}
/// Binds the example asset evaluator after reopening a stage too.
pub(crate) fn asset_generator(
    store: &dyn LayerStore,
    recipe: PathId,
) -> Procedural<AssetGenerator> {
    Procedural::new(
        store,
        recipe,
        AssetGenerator {
            system: "example:asset",
            template: mesh(),
            external_points: true,
        },
    )
}
/// Failures remain inspectable without replacing previously published geometry.
#[derive(Debug)]
pub(crate) enum GeneratorError {
    Input(ProceduralInputError),
    Parameter(&'static str),
    Validation(layerstack_schemas::MeshPublicationError),
}
impl From<ProceduralInputError> for GeneratorError {
    fn from(error: ProceduralInputError) -> Self {
        Self::Input(error)
    }
}
impl std::fmt::Display for GeneratorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(error) => error.fmt(f),
            Self::Parameter(name) => write!(f, "invalid procedural parameter {name}"),
            Self::Validation(error) => write!(f, "generated geometry: {error}"),
        }
    }
}
impl std::error::Error for GeneratorError {}
impl ProceduralEvaluator for AssetGenerator {
    type Output = GeneratedAsset;
    type Error = GeneratorError;
    fn system(&self) -> &str {
        self.system
    }
    fn evaluate(
        &self,
        inputs: &mut ProceduralInputs<'_>,
    ) -> Result<GeneratedAsset, GeneratorError> {
        let height = inputs
            .parameter("height")?
            .as_ref()
            .and_then(|v| layerstack_schemas::value::read_float(v, inputs.tokens()))
            .filter(|v| v.is_finite() && *v > 0.)
            .ok_or(GeneratorError::Parameter("height"))?;
        let mut geometry = self.template.clone();
        let mut texture = None;
        if self.external_points {
            let targets = inputs
                .parameter_targets("source")?
                .ok_or(GeneratorError::Parameter("source"))?;
            let [TargetPath::Property(source)] = targets.as_slice() else {
                return Err(GeneratorError::Parameter("source"));
            };
            let name = inputs.tokens().resolve(source.property()).to_owned();
            geometry.points = inputs
                .attribute(source.prim_path(), &name)?
                .as_ref()
                .and_then(|v| {
                    layerstack_schemas::value::read_float3_array_shared(v, inputs.tokens())
                })
                .ok_or(GeneratorError::Parameter("source points"))?;
            let topology_value = inputs
                .parameter("topology")?
                .ok_or(GeneratorError::Parameter("topology"))?;
            match layerstack_schemas::value::read_token(&topology_value, inputs.tokens()) {
                Some("triangles") => {}
                Some("quad") => {
                    geometry.face_vertex_counts = Arc::new(vec![4]);
                    geometry.face_vertex_indices = Arc::new(vec![0, 1, 2, 3]);
                    for primvar in &mut geometry.primvars {
                        primvar.indices = Some(Arc::new(vec![0, 1, 2, 5]));
                    }
                }
                _ => return Err(GeneratorError::Parameter("topology")),
            }
            let normals = inputs
                .parameter("normals")?
                .as_ref()
                .and_then(|v| layerstack_schemas::value::read_bool(v, inputs.tokens()))
                .ok_or(GeneratorError::Parameter("normals"))?;
            if !normals {
                geometry.primvars.retain(|p| p.name != "primvars:normals");
            }
            texture = inputs
                .parameter("texture")?
                .as_ref()
                .and_then(|v| layerstack_schemas::value::read_asset(v, inputs.tokens()));
            if texture.is_none() {
                return Err(GeneratorError::Parameter("texture"));
            }
        }
        if height != 1. {
            // Producer evaluation materializes only the points it changes.
            geometry.points = Arc::new(
                geometry
                    .points
                    .iter()
                    .map(|p| [p[0], p[1] * height, p[2]])
                    .collect(),
            );
        }
        let geometry = geometry
            .into_validated()
            .map_err(GeneratorError::Validation)?;
        Ok(GeneratedAsset { geometry, texture })
    }
}

/// Read/evaluation/publication failures are separate; an output is committed only on success.
#[derive(Debug)]
pub(crate) enum RecipeError {
    Evaluation(ProceduralError<GeneratorError>),
    Publication(layerstack_schemas::MeshPublicationError),
    Apply(EvidenceApplyError),
}
impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Evaluation(e) => e.fmt(f),
            Self::Publication(e) => write!(f, "publication: {e:?}"),
            Self::Apply(e) => write!(f, "application: {e:?}"),
        }
    }
}
impl std::error::Error for RecipeError {}

/// The caller owns the stage, three layers and each producer's publication manifest.
pub(crate) struct GeneratedScene {
    /// Shared interners and authored layers.
    pub(crate) store: InMemoryStore,
    /// Retained composed stage.
    pub(crate) live: LiveStage,
    /// Editable geometry source, separate from instances.
    pub(crate) source: PathId,
    /// Exedra's independently published mesh.
    pub(crate) terrain: PathId,
    /// Sylva's point placements.
    pub(crate) scatter: PathId,
    /// Shared source buffers.
    pub(crate) geometry: GeneratedMesh,
    /// Last successfully applied source publication.
    pub(crate) publication: MeshPublication,
    /// Retained recipe evaluators supplied by the application.
    pub(crate) asset_generator: Procedural<AssetGenerator>,
    pub(crate) terrain_generator: Procedural<AssetGenerator>,
    /// Independently owned terrain manifest.
    pub(crate) terrain_publication: MeshPublication,
}
impl GeneratedScene {
    /// Exercises durability boundaries using the same two producers and shared asset.
    #[allow(
        dead_code,
        reason = "shared fixture used by the focused durability example"
    )]
    pub(crate) fn prove_durability(&mut self) {
        use layerstack::{ChangeHistoryBudget, ChangeHistoryError, Transaction};
        use layerstack_schemas::{
            bounds::{BoundsCache, BoundsOptions},
            procedural::{EvidenceApplyError, EvidenceError},
        };
        let original = self.geometry.points.clone();
        let topology = self.geometry.face_vertex_indices.clone();
        let before = self.asset_generator.work();
        assert_eq!(
            self.evaluate_terrain(Time::Default).unwrap(),
            layerstack::Changes::default(),
            "unchanged terrain authors no opinions"
        );
        assert_eq!(
            self.evaluate_asset(Time::Default).unwrap(),
            layerstack::Changes::default(),
            "unchanged asset authors no opinions"
        );
        assert_eq!(
            self.asset_generator.work().input_reads,
            before.input_reads,
            "unchanged requests skip resolution"
        );
        assert_eq!(
            self.publication.work.geometry_validations, 0,
            "retained snapshots skip validation"
        );
        assert_eq!(
            self.publication.work.extent_points, 0,
            "retained extent skips point visits"
        );

        let unrelated = self.store.property_path("/World.user:label");
        let mut edit = Transaction::new();
        edit.create_property(
            EditTarget::for_layer(ROOT).property(unrelated),
            PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                .with_default(Value::Int(1)),
        );
        self.live.apply(&mut self.store, &edit).unwrap();
        assert_eq!(
            self.evaluate_asset(Time::Default).unwrap(),
            layerstack::Changes::default(),
            "unrelated edits do not republish"
        );
        assert_eq!(
            self.asset_generator.work().input_reads,
            before.input_reads,
            "other prim edits do not resolve inputs"
        );

        // Detect output loss independently of the recipe's unchanged consumed inputs.
        let points = self.store.property_path("/Assets/Tree/Geometry.points");
        let mut delete = Transaction::new();
        delete.remove_spec(EditTarget::for_layer(ASSET).property(points));
        self.live.apply(&mut self.store, &delete).unwrap();
        self.evaluate_asset(Time::Default).unwrap();
        assert_eq!(
            self.asset_generator.work().evaluations,
            before.evaluations,
            "output repair reuses evaluation"
        );
        assert!(
            Arc::ptr_eq(&original, &self.geometry.points),
            "output repair preserves owners"
        );

        let delayed = self
            .asset_generator
            .snapshot(&Scene::new(self.live.stage(), &self.store), Time::Default)
            .unwrap();
        let publication = delayed
            .output()
            .geometry
            .prepare(
                self.live.stage(),
                &mut self.store,
                &EditTarget::for_layer(ASSET),
                self.source,
                &self.publication.properties,
            )
            .unwrap();
        let output_generation = self.store.layers[&ASSET].generation();
        let terrain_points = self.store.property_path("/World/Terrain.points");
        // Replace an entire source layer behind the live stage, keeping its LayerId.
        let mut replacement = self.store.layers[&TERRAIN].clone();
        replacement.set_property(
            terrain_points,
            PropertySpec::typed_attribute(PropertyType::new(
                "point3f",
                true,
                Value::Vec3f([0.; 3]),
            ))
            .with_default(Value::TypedArray(TypedArray::Vec3f(Arc::new(
                original.iter().map(|p| [p[0], p[1] * 2., p[2]]).collect(),
            )))),
        );
        self.store.insert_layer(replacement);
        assert_eq!(
            self.store.layers[&ASSET].generation(),
            output_generation,
            "source replacement does not touch output layer"
        );
        assert!(
            matches!(
                delayed
                    .evidence()
                    .apply(&mut self.live, &mut self.store, &publication.transaction),
                Err(EvidenceApplyError::Evidence(EvidenceError::Changed { .. }))
            ),
            "input evidence rejects stale work independently of the target guard"
        );
        self.evaluate_asset(Time::Default).unwrap();
        assert_eq!(
            self.geometry.points[2][1], 2.,
            "source replacement refreshes dependent geometry"
        );
        assert_eq!(
            self.publication.work.geometry_validations, 0,
            "new output is already validated"
        );

        let mut bounds = BoundsCache::new(Time::Default, BoundsOptions::default());
        bounds
            .world_bound(&Scene::new(self.live.stage(), &self.store), self.scatter)
            .unwrap();
        let mut cursor = self.live.change_cursor();
        self.live.set_change_history_budget(ChangeHistoryBudget {
            max_batches: 1,
            max_retained_bytes: 4096,
        });
        self.store
            .layers
            .get_mut(&TERRAIN)
            .unwrap()
            .set_change_history_budget(ChangeHistoryBudget {
                max_batches: 0,
                max_retained_bytes: 0,
            });
        for value in [3., 4.] {
            let height = self.store.property_path("/Recipes/Terrain.primvars:height");
            let mut edit = Transaction::new();
            edit.set_default(
                EditTarget::for_layer(TERRAIN).property(height),
                Value::Float(value),
            );
            self.live.apply(&mut self.store, &edit).unwrap();
        }
        assert!(
            matches!(
                self.live.changes_since(&mut cursor),
                Err(ChangeHistoryError::Expired)
            ),
            "lagging consumers receive explicit history expiry"
        );
        self.live
            .set_change_history_budget(ChangeHistoryBudget::default());
        // Cursors advance on expiry. Notice-dependent consumers rebuild explicitly.
        bounds.clear();
        // Query-backed producers inspect current records and need no notice replay.
        let terrain = self.evaluate_terrain(Time::Default).unwrap();
        let asset = self.evaluate_asset(Time::Default).unwrap();
        let scene = Scene::new(self.live.stage(), &self.store);
        bounds.apply_changes(&scene, &terrain);
        bounds.apply_changes(&scene, &asset);
        assert_eq!(
            bounds
                .world_bound(&scene, self.scatter)
                .unwrap()
                .aligned_range()
                .max[1],
            4.,
            "recovered bounds observe current upstream geometry"
        );
        assert!(
            self.live.change_history_stats().evicted_batches > 0,
            "composed evictions are measurable"
        );
        assert!(
            self.store.layers[&TERRAIN]
                .change_history_stats()
                .discarded_batches
                > 0,
            "authored discards are measurable"
        );
        self.live
            .changes_since(&mut cursor)
            .unwrap()
            .for_each(|change| bounds.apply_changes(&scene, change));
        assert_eq!(
            self.evaluate_asset(Time::Default).unwrap(),
            layerstack::Changes::default(),
            "recovered producer settles to unchanged work"
        );
        assert!(
            Arc::ptr_eq(&topology, &self.geometry.face_vertex_indices),
            "untouched topology remains shared through recovery"
        );
    }
    /// Builds reusable geometry, native references and a point instancer.
    /// Root sublayer offsets exercise stage-time authoring and serialization.
    pub(crate) fn new(count: usize) -> Self {
        Self::with_counts(count, count)
    }
    /// Separately sizes native references and point placements for measurements.
    pub(crate) fn with_counts(native_count: usize, count: usize) -> Self {
        let mut store = InMemoryStore::default();
        import(
            &mut store,
            r#"#usda 1.0
def Scope "Assets" {
 def Xform "Tree" {
  def Material "Material" {
   token outputs:surface.connect = </Assets/Tree/Material/Surface.outputs:surface>
   def Shader "Surface" {
    uniform token info:id = "UsdPreviewSurface"
    color3f inputs:diffuseColor.connect = </Assets/Tree/Material/Texture.outputs:rgb>
    token outputs:surface
   }
   def Shader "Texture" {
    uniform token info:id = "UsdUVTexture"
    asset inputs:file = @textures/bark.png@
    float2 inputs:st.connect = </Assets/Tree/Material/UV.outputs:result>
    float3 outputs:rgb
   }
   def Shader "UV" {
    uniform token info:id = "UsdPrimvarReader_float2"
    string inputs:varname = "st"
    float2 outputs:result
   }
  }
  def Mesh "Geometry" (prepend apiSchemas = ["MaterialBindingAPI"]) {
   custom int user:tag = 17
   rel material:binding = </Assets/Tree/Material>
  }
 }
}
def Scope "Recipes" {
 def GenerativeProcedural "Tree" {
  token proceduralSystem = "example:asset"
  float primvars:height = 1
  token primvars:topology = "triangles"
  bool primvars:normals = true
  asset primvars:texture = @textures/bark.png@
  rel primvars:source = </World/Terrain.points>
 }
}
"#,
            ASSET,
        );
        import(
            &mut store,
            "#usda 1.0\nover \"World\" {}\ndef Scope \"Recipes\" { def GenerativeProcedural \"Terrain\" { token proceduralSystem = \"example:terrain\"; float primvars:height = 1 } }\n",
            TERRAIN,
        );
        let mut root = String::from(
            "#usda 1.0\n(\n defaultPrim = \"World\"\n subLayers = [@assets.usd@ (offset = 10; scale = 2), @terrain.usd@]\n)\ndef Xform \"World\" {\n def Scope \"Materials\" { def Material \"Override\" {} }\n",
        );
        for i in 0..native_count {
            use std::fmt::Write as _;
            writeln!(root, " def Xform \"Native_{i}\" (instanceable = true; prepend references = </Assets/Tree>; prepend apiSchemas = [\"MaterialBindingAPI\"]) {{\n rel material:binding = </World/Materials/Override> (bindMaterialAs = \"strongerThanDescendants\")\n }}").unwrap();
        }
        root.push_str(" def PointInstancer \"Scatter\" { rel prototypes = </Assets/Tree> }\n}\n");
        import(&mut store, &root, ROOT);
        let options = StageOptions {
            schemas: Some(Arc::new(layerstack_schemas::openusd(&mut store.tokens))),
            with_provenance: true,
            ..Default::default()
        };
        let mut live = LiveStage::compose(&mut store, ROOT, options);
        let source = store.path("/Assets/Tree/Geometry");
        let terrain = store.path("/World/Terrain");
        let scatter = store.path("/World/Scatter");
        let terrain_recipe = store.path("/Recipes/Terrain");
        let asset_recipe = store.path("/Recipes/Tree");
        let mut terrain_generator = Procedural::new(
            &store,
            terrain_recipe,
            AssetGenerator {
                system: "example:terrain",
                template: GeneratedMesh {
                    primvars: Vec::new(),
                    ..mesh()
                },
                external_points: false,
            },
        );
        let ground = terrain_generator
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap()
            .geometry
            .clone();
        let terrain_publication = ground
            .prepare(
                live.stage(),
                &mut store,
                &EditTarget::for_layer(TERRAIN),
                terrain,
                &[],
            )
            .unwrap();
        live.apply(&mut store, &terrain_publication.transaction)
            .unwrap();
        let mut asset_generator = asset_generator(&store, asset_recipe);
        let geometry = asset_generator
            .evaluate(&Scene::new(live.stage(), &store), Time::Default)
            .unwrap()
            .geometry
            .clone();
        let publication = geometry
            .prepare(
                live.stage(),
                &mut store,
                &EditTarget::for_layer(ASSET),
                source,
                &[],
            )
            .unwrap();
        live.apply(&mut store, &publication.transaction).unwrap();
        let instancer = PointInstancer::new(&Scene::new(live.stage(), &store), scatter)
            .unwrap()
            .edit();
        let mut edit = SchemaEdit::new(live.stage(), &mut store, EditTarget::for_layer(ROOT));
        instancer.set_proto_indices_owned(&mut edit, vec![0; count]);
        instancer.set_positions_owned(
            &mut edit,
            (0..count)
                .map(|i| [f32::from(u16::try_from(i % 1000).unwrap()) * 2., 0., 0.])
                .collect(),
        );
        instancer.set_ids_owned(
            &mut edit,
            (0..count)
                .map(|i| i64::try_from(i).unwrap() + 100)
                .collect(),
        );
        if count > 1 {
            instancer.set_invisible_ids_owned(&mut edit, vec![100]);
        }
        let colors = instancer
            .create_primvar(
                &mut edit,
                "displayColor",
                PropertyType::new("color3f", true, Value::Vec3f([0.; 3])),
            )
            .unwrap();
        colors.set_interpolation(&mut edit, "vertex").unwrap();
        colors
            .set(
                &mut edit,
                Value::TypedArray(TypedArray::Vec3f(Arc::new(vec![[0.2, 0.6, 0.1]; count]))),
            )
            .unwrap();
        let transaction = edit.finish();
        live.apply(&mut store, &transaction).unwrap();
        Self {
            store,
            live,
            source,
            terrain,
            scatter,
            geometry: geometry.mesh().clone(),
            publication,
            asset_generator,
            terrain_generator,
            terrain_publication,
        }
    }

    /// Applies one validated geometry update; preserves the manifest only on success.
    pub(crate) fn publish(&mut self) -> layerstack::Changes {
        let update = self
            .geometry
            .prepare(
                self.live.stage(),
                &mut self.store,
                &EditTarget::for_layer(ASSET),
                self.source,
                &self.publication.properties,
            )
            .unwrap();
        let changes = self
            .live
            .apply(&mut self.store, &update.transaction)
            .unwrap()
            .changes;
        self.publication = update;
        changes
    }

    /// Evaluates the recipe, validates its output and publishes one default snapshot.
    /// The material file input and mesh are updated in the same guarded transaction.
    pub(crate) fn evaluate_asset(
        &mut self,
        time: Time,
    ) -> Result<layerstack::Changes, RecipeError> {
        let output = self
            .asset_generator
            .snapshot(&Scene::new(self.live.stage(), &self.store), time)
            .map_err(RecipeError::Evaluation)?;
        let target = EditTarget::for_layer(ASSET);
        let mut update = output
            .output()
            .geometry
            .prepare(
                self.live.stage(),
                &mut self.store,
                &target,
                self.source,
                &self.publication.properties,
            )
            .map_err(RecipeError::Publication)?;
        let file = self
            .store
            .property_path("/Assets/Tree/Material/Texture.inputs:file");
        let desired = Value::Asset(
            output
                .output()
                .texture
                .clone()
                .expect("asset generator supplies a texture reference"),
        );
        // Compare this producer's authored site, even when a stronger opinion masks it.
        if let Some(local) = self.store.layers[&ASSET].property(file) {
            if local.kind != PropertyKind::Attribute
                || local
                    .type_name
                    .as_ref()
                    .is_none_or(|t| t.type_name.as_ref() != "asset" || t.is_array)
            {
                return Err(RecipeError::Publication(
                    layerstack_schemas::MeshPublicationError::PropertyConflict(
                        "inputs:file".into(),
                    ),
                ));
            }
            if local.default.as_ref() != Some(&desired) {
                update
                    .transaction
                    .set_default(target.property(file), desired);
            }
            if let Some(samples) = &local.time_samples {
                for (time, _) in samples.as_slice() {
                    // for_layer has an identity time map; these are authored times.
                    update
                        .transaction
                        .remove_time_sample(target.property(file), *time);
                }
            }
        } else {
            update.transaction.create_property(
                target.property(file),
                PropertySpec::typed_attribute(PropertyType::new(
                    "asset",
                    false,
                    Value::Asset("".into()),
                ))
                .with_default(desired),
            );
        }
        if !update.transaction.is_empty() {
            update
                .transaction
                .expect_generation(ASSET, self.store.layers[&ASSET].generation());
        }
        let applied = output
            .evidence()
            .apply(&mut self.live, &mut self.store, &update.transaction)
            .map_err(RecipeError::Apply)?;
        self.geometry = output.output().geometry.mesh().clone();
        self.publication = update;
        Ok(applied.changes)
    }

    /// The host explicitly evaluates upstream terrain before its dependent asset.
    pub(crate) fn evaluate_terrain(
        &mut self,
        time: Time,
    ) -> Result<layerstack::Changes, RecipeError> {
        let output = self
            .terrain_generator
            .snapshot(&Scene::new(self.live.stage(), &self.store), time)
            .map_err(RecipeError::Evaluation)?;
        let update = output
            .output()
            .geometry
            .prepare(
                self.live.stage(),
                &mut self.store,
                &EditTarget::for_layer(TERRAIN),
                self.terrain,
                &self.terrain_publication.properties,
            )
            .map_err(RecipeError::Publication)?;
        let applied = output
            .evidence()
            .apply(&mut self.live, &mut self.store, &update.transaction)
            .map_err(RecipeError::Apply)?;
        self.terrain_publication = update;
        Ok(applied.changes)
    }
}
