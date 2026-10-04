// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The runnable publication fixture is also exercised by conformance tests and benchmarks.
use layerstack::{
    AssetResolveError, AssetResolver, EditTarget, InMemoryStore, LayerId, LiveStage, PathId,
    PathInterner, PropertyType, ResolvedAsset, StageOptions, TokenInterner, TypedArray, Value,
};
use layerstack_schemas::{
    GeneratedMesh, MeshPrimvar, MeshPublication, Scene, SchemaEdit, usd_geom::PointInstancer,
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
}
impl GeneratedScene {
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
  def Material "Material" {}
  def Mesh "Geometry" (prepend apiSchemas = ["MaterialBindingAPI"]) {
   custom int user:tag = 17
   rel material:binding = </Assets/Tree/Material>
  }
 }
}
"#,
            ASSET,
        );
        import(&mut store, "#usda 1.0\nover \"World\" {}\n", TERRAIN);
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
        let geometry = mesh();
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
        let ground = GeneratedMesh {
            primvars: Vec::new(),
            ..geometry.clone()
        };
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
            geometry,
            publication,
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
}
