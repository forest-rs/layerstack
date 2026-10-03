// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use layerstack::{
    AssetResolveError, AssetResolver, InMemoryStore, PrimSpec, PropertySpec, PropertyType,
    Reference, ResolvedAsset,
};

fn fixture() -> (InMemoryStore, layerstack::PathId, layerstack::TokenId) {
    let mut store = InMemoryStore::default();
    let model = store.path("/Model");
    let file = store.tokens.intern("file");
    let root_name = store.tokens.intern("Model");
    let pseudo_root = store.path("/");
    let mut root = Layer::new(LayerId(1));
    root.default_prim = Some(root_name);
    root.insert_prim(
        model,
        PrimSpec::def().with_reference(Reference::with_asset_default_prim(
            LayerId(2),
            "original/child.usda",
        )),
    );
    let mut child = Layer::new(LayerId(2));
    child.default_prim = Some(root_name);
    child.insert_prim(
        model,
        PrimSpec::def().with_property(
            file,
            PropertySpec::typed_attribute(PropertyType::new(
                "asset",
                false,
                layerstack::Value::Asset("".into()),
            ))
            .with_default(layerstack::Value::Asset("original/albedo.png".into())),
        ),
    );
    root.insert_prim(
        pseudo_root,
        PrimSpec::default().with_children(vec![root_name]),
    );
    child.insert_prim(
        pseudo_root,
        PrimSpec::default().with_children(vec![root_name]),
    );
    store.insert_layer(root);
    store.insert_layer(child);
    (store, model, file)
}
struct NoExternal(u64);
impl AssetResolver for NoExternal {
    fn resolved_path(&self, _: LayerId) -> Option<&str> {
        None
    }
    fn resolve(
        &mut self,
        _: &str,
        _: Option<LayerId>,
        _: &mut TokenInterner,
        _: &mut PathInterner,
    ) -> Result<ResolvedAsset, AssetResolveError> {
        Err(AssetResolveError::NotFound)
    }
    fn allocate_layer_id(&mut self) -> Option<LayerId> {
        self.0 += 1;
        Some(LayerId(self.0))
    }
}

#[test]
fn localized_package_round_trips_layers_and_rewrites_paths_from_each_authoring_layer() {
    let (store, model, file) = fixture();
    let original = store.layer(LayerId(2)).unwrap().clone();
    let plan = localize_asset(&store, LayerId(1), "scene.usdc", |d| {
        Ok(match &*d.identifier {
            "original/child.usda" => LocalizationTarget::layer(LayerId(2), "models/child.usda"),
            _ => LocalizationTarget::asset("textures/albedo.png", &b"image"[..]),
        })
    })
    .unwrap();
    assert_eq!(plan.resolved_uses, 2);
    assert_eq!(plan.layers[0].member.as_ref(), "scene.usdc");
    assert_eq!(
        plan.layers[1].layer.prims[&model]
            .property(file)
            .unwrap()
            .default,
        Some(layerstack::Value::Asset("../textures/albedo.png".into()))
    );
    assert_eq!(*store.layer(LayerId(2)).unwrap(), original);
    let bytes = plan.write_usdz(&store.tokens, &store.paths).unwrap();
    assert_eq!(bytes, plan.write_usdz(&store.tokens, &store.paths).unwrap());
    let mut tokens = TokenInterner::default();
    let mut paths = PathInterner::default();
    let read = crate::read_usdz(
        &bytes,
        LayerId(100),
        &mut tokens,
        &mut paths,
        &mut NoExternal(100),
    )
    .unwrap();
    assert!(!read.has_errors(), "{:?}", read.diagnostics);
    assert_eq!(read.resolved_layers.len(), 1);
    let archive = crate::zip::ZipArchive::parse(&bytes).unwrap();
    assert!(archive.entries().iter().all(|e| e.data_offset % 64 == 0));
    assert_eq!(archive.entries()[2].name.as_ref(), "textures/albedo.png");
}

#[test]
fn resolution_failure_and_member_collisions_leave_sources_unchanged() {
    let (store, _, _) = fixture();
    let source = store.layer(LayerId(1)).unwrap().clone();
    let error = localize_asset(&store, LayerId(1), "scene.usda", |d| {
        Err(LocalizationError::Resolution {
            dependency: alloc::boxed::Box::new(d.clone()),
            reason: "unavailable".into(),
        })
    })
    .unwrap_err();
    assert!(matches!(error, LocalizationError::Resolution { .. }));
    assert!(matches!(
        localize_asset(&store, LayerId(1), "scene.usda", |_| Ok(
            LocalizationTarget::layer(LayerId(2), "scene.usda")
        )),
        Err(LocalizationError::MemberCollision { .. })
    ));
    assert_eq!(*store.layer(LayerId(1)).unwrap(), source);
}

#[test]
fn unnamed_layer_links_are_named_and_host_expanded_udims_keep_templates() {
    let (mut store, model, file) = fixture();
    store
        .layer_mut(LayerId(1))
        .unwrap()
        .prims
        .get_mut(&model)
        .unwrap()
        .references
        .append[0]
        .asset = None;
    Arc::make_mut(
        &mut store
            .layer_mut(LayerId(2))
            .unwrap()
            .prims
            .get_mut(&model)
            .unwrap()
            .properties[0]
            .spec,
    )
    .default = Some(layerstack::Value::Asset("original/<UDIM>.png".into()));
    let plan = localize_asset(&store, LayerId(1), "root.usda", |d| {
        Ok(if d.identifier.is_empty() {
            LocalizationTarget::layer(d.layer_hint.unwrap(), "models/model.usda")
        } else {
            LocalizationTarget {
                member: "textures/<UDIM>.png".into(),
                resources: vec![
                    LocalizationResource::Asset {
                        member: "textures/1001.png".into(),
                        data: Arc::from(&b"a"[..]),
                    },
                    LocalizationResource::Asset {
                        member: "textures/1002.png".into(),
                        data: Arc::from(&b"b"[..]),
                    },
                ],
            }
        })
    })
    .unwrap();
    assert_eq!(plan.assets.len(), 2);
    assert_eq!(
        plan.layers[1].layer.prims[&model]
            .property(file)
            .unwrap()
            .default,
        Some(layerstack::Value::Asset("../textures/<UDIM>.png".into()))
    );
    assert!(plan.write_usdz(&store.tokens, &store.paths).is_ok());
}

#[test]
fn invalid_names_empty_expansions_and_missing_layers_are_explicit() {
    let (store, _, _) = fixture();
    assert!(matches!(
        localize_asset(&store, LayerId(1), "../root.usda", |_| unreachable!()),
        Err(LocalizationError::InvalidMember { .. })
    ));
    assert!(matches!(
        localize_asset(&store, LayerId(99), "root.usda", |_| unreachable!()),
        Err(LocalizationError::MissingLayer { .. })
    ));
    assert!(matches!(
        localize_asset(&store, LayerId(1), "root.usda", |_| Ok(
            LocalizationTarget {
                member: "models/child.usda".into(),
                resources: Vec::new()
            }
        )),
        Err(LocalizationError::MissingReplacement { .. })
    ));
}
