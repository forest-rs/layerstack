// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Every `SchemaEdit` authoring route refuses an instance proxy or a
//! prototype prim exactly where OpenUSD 26.08 refuses it, authoring nothing,
//! and authors what OpenUSD authors where it does not.
//!
//! `scripts/proxy_edits_oracle.py` writes `fixtures/proxy_edits`: a root
//! layer with two native instances of `asset.usda`, `/Instance` (a mesh with
//! a mesh child and a material child) and `/Looks` (a material with a shader
//! child). For each route (define, the three ways to apply a schema, an
//! attribute default and time sample, relationship targets set and added,
//! property metadata, a primvar, a geometry subset, a material binding, a
//! transform op, `MakeInvisible`, a UI hint, a shading input) it records,
//! on fresh copies of the layers, whether OpenUSD raised and the edit
//! target layer's exported text, on the instance itself, its child (an
//! instance proxy), that child in the prototype, a path beneath the
//! instance that no prim has, all through the root layer, and the proxy
//! through the instance's reference node. Here each case is one
//! `SchemaEdit` applied through a `LiveStage`: it must be refused exactly
//! when OpenUSD raised, with [`CannotEdit`] naming the instance or the
//! prototype, collecting nothing; and leave the same layer text, byte for
//! byte. Undoing it restores the text from before.
//!
//! OpenUSD numbers its prototypes in no fixed order, so the oracle records a
//! prototype prim by the instance it serves (`prototype_of`) and its path
//! below the prototype. layerstack names no prototype prims; any root prim
//! named `__Prototype_…` is in a prototype, so the case edits that path
//! below `/__Prototype_1`, once the instance is found to have a prototype.
//!
//! OpenUSD: `UsdStage::_ValidateEditPrim`, `_ValidateEditPrimAtPath`.
//!
//! Spec: AOUSD Core §11.3.3 (scene-graph instancing).

use std::error::Error;
use std::sync::Arc;

use layerstack::prim_index::ArcKind;
use layerstack::{
    EditTarget, LayerId, ListPosition, LiveStage, PathId, PropertyPath, PropertyType, StageOptions,
    TargetPath, Time, Value,
};
use layerstack_conformance::{
    usda_real::{LoadedStage, load_entry_usda},
    workspace_root,
};
use layerstack_schemas::ui_hints::UiHintsEdit;
use layerstack_schemas::usd::CollectionApi;
use layerstack_schemas::usd_geom::{
    GeomSubsetEdit, GeomSubsetElementType, ImageableEdit, ImageablePurpose, ImageableVisibility,
    Scope, XformableEdit,
};
use layerstack_schemas::usd_lux::ShapingApi;
use layerstack_schemas::usd_shade::MaterialBindingApi;
use layerstack_schemas::{CannotEdit, MaterialPurpose, PrimEdit, SchemaEdit, XformOpPrecision};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../../fixtures/proxy_edits/oracle.json");

/// Every route the oracle records, in its order.
const ROUTES: &[&str] = &[
    "define",
    "apply_single",
    "apply_multiple",
    "apply_by_name",
    "attribute_default",
    "attribute_sample",
    "targets",
    "add_target",
    "property_metadata",
    "primvar",
    "subset",
    "binding",
    "xform_op",
    "make_invisible",
    "ui_hints",
    "port",
];

fn oracle() -> Json {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle
}

fn load() -> LoadedStage {
    let loaded = load_entry_usda(
        &workspace_root().join("layerstack_conformance/fixtures/proxy_edits/scene.usda"),
    );
    assert!(loaded.invalid.is_empty(), "{:?}", loaded.invalid);
    loaded
}

fn live_of(loaded: &mut LoadedStage) -> LiveStage {
    let options = StageOptions {
        schemas: Some(Arc::new(layerstack_schemas::openusd(loaded.store.tokens()))),
        ..StageOptions::default()
    };
    LiveStage::compose(&mut loaded.store, loaded.root_layer, options)
}

fn layer_named(loaded: &LoadedStage, name: &str) -> LayerId {
    *loaded
        .layer_names
        .iter()
        .find(|(_, n)| n.ends_with(name))
        .unwrap_or_else(|| panic!("no layer {name}"))
        .0
}

/// The layer as OpenUSD's `SdfLayer::ExportToString` writes it.
fn text(loaded: &LoadedStage, layer: LayerId) -> String {
    let store = &loaded.store;
    layerstack_usda::save_usda(store.layer(layer).unwrap(), store.tokens(), store.paths())
        .expect("saves")
}

/// The prims a case names.
struct Site {
    /// The prim the route authors.
    path: PathId,
    /// The family's instance, which relationship targets name.
    instance: PathId,
    /// The material the geometry binds, beneath the instance.
    material: PathId,
}

/// The handle `PrimEdit::new` gives for an existing prim.
fn prim(edit: &SchemaEdit<'_>, path: PathId) -> Result<PrimEdit, Box<dyn Error>> {
    Ok(PrimEdit::new(edit, path)?.ok_or("no prim")?)
}

fn imageable(edit: &SchemaEdit<'_>, path: PathId) -> Result<ImageableEdit, Box<dyn Error>> {
    Ok(ImageableEdit::new(edit, path)?.ok_or("not imageable")?)
}

/// Makes the calls of `route` that the oracle's function of the same name
/// makes in OpenUSD.
fn author(route: &str, edit: &mut SchemaEdit<'_>, site: &Site) -> Result<(), Box<dyn Error>> {
    let path = site.path;
    let float = || PropertyType::parse("float").expect("a value type");
    match route {
        // `stage.DefinePrim(path, "Scope")`
        "define" => {
            Scope::define(edit, path)?;
        }
        // `UsdLux.ShapingAPI.Apply(prim)`
        "apply_single" => {
            ShapingApi::apply(edit, path)?;
        }
        // `Usd.CollectionAPI.Apply(prim, "set")`
        "apply_multiple" => {
            CollectionApi::apply(edit, path, "set")?;
        }
        // `prim.ApplyAPI("MotionAPI")`
        "apply_by_name" => {
            edit.apply_api(path, "MotionAPI", None)?;
        }
        // `CreatePurposeAttr().Set("proxy")`
        "attribute_default" => {
            imageable(edit, path)?.set_purpose(edit, ImageablePurpose::Proxy);
        }
        // `CreateVisibilityAttr().Set("invisible", 5)`
        "attribute_sample" => {
            imageable(edit, path)?.set_visibility_at(edit, 5.0, ImageableVisibility::Invisible);
        }
        // `CreateProxyPrimRel().SetTargets([instance])`
        "targets" => {
            let targets = [TargetPath::Prim(site.instance)];
            prim(edit, path)?.write_targets(edit, "proxyPrim", &targets);
        }
        // `CreateRelationship("links", True).AddTarget(instance)`
        "add_target" => {
            let target = TargetPath::Prim(site.instance);
            prim(edit, path)?.add_target(edit, "links", target, ListPosition::BackOfPrepend);
        }
        // `GetPurposeAttr().SetMetadata("documentation", "Tagged")`
        "property_metadata" => {
            let tagged = Value::string("Tagged");
            prim(edit, path)?.write_property_metadata(edit, "purpose", "documentation", tagged)?;
        }
        // `PrimvarsAPI(prim).CreatePrimvar("tint", Float).Set(0.5)`
        "primvar" => {
            prim(edit, path)?
                .create_primvar(edit, "tint", float())?
                .set(edit, Value::Float(0.5))?;
        }
        // `Subset.CreateGeomSubset(prim, "part", "face", [0], "family")`
        "subset" => {
            let face = GeomSubsetElementType::Face;
            GeomSubsetEdit::create(edit, path, "part", face, &[0], "family")?;
        }
        // `MaterialBindingAPI.Apply(prim).Bind(material)`
        "binding" => {
            MaterialBindingApi::apply(edit, path)?.bind(
                edit,
                site.material,
                None,
                &MaterialPurpose::All,
            )?;
        }
        // `Xformable(prim).AddTranslateOp(PrecisionDouble).Set((1, 2, 3))`
        "xform_op" => {
            XformableEdit::new(edit, path)?
                .ok_or("not xformable")?
                .add_translate_op(edit, XformOpPrecision::Double)
                .map_err(|e| format!("{e:?}"))?
                .set(edit, [1.0, 2.0, 3.0])
                .map_err(|e| format!("{e:?}"))?;
        }
        // `Imageable(prim).MakeInvisible()`
        "make_invisible" => {
            imageable(edit, path)?.make_invisible(edit, Time::Default);
        }
        // `GetPurposeAttr().SetMetadataByDictKey("uiHints", "displayName",
        // "Shown")`: the dictionary alone, without the legacy field.
        "ui_hints" => {
            let purpose = PropertyPath::new(path, edit.tokens().intern("purpose"));
            UiHintsEdit::property(edit, purpose)?
                .with_legacy_fields(false)
                .set_display_name(edit, "Shown");
        }
        // `ConnectableAPI(prim).CreateInput("roughness", Float).Set(0.25)`
        "port" => {
            prim(edit, path)?
                .create_input(edit, "roughness", float())?
                .set(edit, Value::Float(0.25))?;
        }
        other => panic!("no route {other}"),
    }
    Ok(())
}

/// The `CannotEdit` an error is, or is caused by.
fn cannot_edit(error: &(dyn Error + 'static)) -> Option<CannotEdit> {
    let mut at = Some(error);
    while let Some(error) = at {
        if let Some(cannot) = error.downcast_ref::<CannotEdit>() {
            return Some(*cannot);
        }
        at = error.source();
    }
    None
}

/// The instance's reference node, as the oracle's `nonlocal` target.
fn through_reference(live: &LiveStage, loaded: &LoadedStage, instance: PathId) -> EditTarget {
    let graph = live
        .stage()
        .prim(instance, &loaded.store)
        .expect("the instance")
        .composition_graph();
    let node = graph
        .depth_first()
        .into_iter()
        .find(|id| {
            graph
                .node(*id)
                .is_some_and(|node| node.arc_kind() == ArcKind::References)
        })
        .expect("the reference node");
    EditTarget::for_node(live.stage(), instance, node).expect("an edit target")
}

#[test]
fn the_layers_are_the_fixtures() {
    let oracle = oracle();
    let loaded = load();
    for name in ["scene.usda", "asset.usda"] {
        assert_eq!(
            text(&loaded, layer_named(&loaded, name)),
            oracle["before"][name].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn authoring_routes_agree_with_openusd_on_instance_proxies() {
    let oracle = oracle();
    let cases = oracle["cases"].as_array().unwrap();
    let recorded: Vec<&str> = cases
        .iter()
        .map(|case| case["route"].as_str().unwrap())
        .collect();
    for route in ROUTES {
        assert_eq!(
            recorded.iter().filter(|r| *r == route).count(),
            5,
            "{route} is recorded in every situation"
        );
    }
    assert_eq!(cases.len(), ROUTES.len() * 5);
    for case in cases {
        let route = case["route"].as_str().unwrap();
        let situation = case["situation"].as_str().unwrap();
        let name = format!("{route} {situation}");
        let mut loaded = load();
        let mut live = live_of(&mut loaded);
        let path_text = match case["prototype_of"].as_str() {
            Some(instance) => {
                let instance = loaded.store.path(instance);
                assert!(
                    live.stage()
                        .prim(instance, &loaded.store)
                        .and_then(|prim| prim.prototype())
                        .is_some(),
                    "{name}: the instance has a prototype"
                );
                format!("/__Prototype_1/{}", case["path"].as_str().unwrap())
            }
            None => case["path"].as_str().unwrap().to_owned(),
        };
        let instance_text = match case["family"].as_str().unwrap() {
            "geometry" => "/Instance",
            _ => "/Looks",
        };
        let site = Site {
            path: loaded.store.path(&path_text),
            instance: loaded.store.path(instance_text),
            material: loaded.store.path(&format!("{instance_text}/Mat")),
        };
        let layer = layer_named(&loaded, case["target"].as_str().unwrap());
        let target = if situation == "nonlocal" {
            through_reference(&live, &loaded, site.instance)
        } else {
            EditTarget::for_layer(layer)
        };
        let before = text(&loaded, layer);

        let mut edit = SchemaEdit::new(live.stage(), &mut loaded.store, target);
        let result = author(route, &mut edit, &site);
        let raised = case["raised"].as_bool().unwrap();
        assert_eq!(result.is_err(), raised, "{name}: {result:?}");
        if let Err(error) = &result {
            assert!(
                edit.transaction().is_empty(),
                "{name}: a refusal collects nothing"
            );
            let expected = match situation {
                "prototype" => CannotEdit::InPrototype,
                _ => CannotEdit::BeneathInstance {
                    instance: site.instance,
                },
            };
            assert_eq!(
                cannot_edit(error.as_ref()),
                Some(expected),
                "{name}: {error}"
            );
        }
        let transaction = edit.finish();
        let applied = live
            .apply(&mut loaded.store, &transaction)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            text(&loaded, layer),
            case["text"].as_str().unwrap(),
            "{name}: layer text"
        );
        live.apply(&mut loaded.store, &applied.inverse)
            .unwrap_or_else(|e| panic!("{name}: undo: {e}"));
        assert_eq!(text(&loaded, layer), before, "{name}: undo");
    }
}
