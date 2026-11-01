// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A draw list's draw mode stand-ins: which models are drawn by one, what
//! they draw, and what an edit of their inputs reads.

use alloc::sync::Arc;
use alloc::vec::Vec;

use layerstack::{
    EditTarget, InMemoryStore, LayerId, LiveStage, NoAssets, PropertyPath, PropertySpec,
    PropertyType, StageOptions, Time, Transaction, Value,
};
use layerstack_schemas::Scene;

use crate::{CardFace, DrawDiff, DrawList, DrawMode, DrawOptions, ItemSource};

const SCENE: &str = r#"#usda 1.0
(
    upAxis = "Y"
)
def Xform "Set" (
    kind = "assembly"
    prepend apiSchemas = ["GeomModelAPI"]
) {
    uniform token model:drawMode = "cards"
    def Xform "Rock" (
        kind = "component"
        prepend apiSchemas = ["GeomModelAPI"]
    ) {
        float3[] extentsHint = [(-1, 0, -2), (1, 4, 2)]
        asset model:cardTextureXPos = @rock_x.png@
        def Mesh "Body" {
            int[] faceVertexCounts = [3]
            int[] faceVertexIndices = [0, 1, 2]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (0, 1, 0)]
        }
    }
    def Xform "Lamp" (
        kind = "component"
    ) {
        def Sphere "Bulb" {
            double radius = 0.5
        }
    }
    def Xform "Robot" (
        kind = "component"
        prepend apiSchemas = ["GeomModelAPI"]
    ) {
        uniform bool model:applyDrawMode = false
        def Cube "Chassis" {
            double size = 1
        }
    }
}
def Xform "Crate" (
    prepend apiSchemas = ["GeomModelAPI"]
) {
    uniform token model:drawMode = "bounds"
    uniform bool model:applyDrawMode = true
    float3[] extentsHint = [(0, 0, 0), (2, 1, 1), (-5, -5, -5), (5, 5, 5)]
    def Cube "Box" {
        double size = 2
    }
}
"#;

struct Fixture {
    store: InMemoryStore,
    live: LiveStage,
    list: DrawList,
}

impl Fixture {
    fn new() -> (Self, DrawDiff) {
        let mut store = InMemoryStore::default();
        let read = layerstack_usda::read_usda(
            SCENE,
            LayerId(1),
            store.tokens(),
            store.paths(),
            &mut NoAssets,
        );
        assert!(!read.has_errors(), "the scene reads without errors");
        store.insert_layer(read.emitted.layer);
        let schemas = Arc::new(layerstack_schemas::openusd(store.tokens()));
        let live = LiveStage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(schemas),
                ..StageOptions::default()
            },
        );
        let mut list = DrawList::new(DrawOptions {
            time: Time::Default,
            ..DrawOptions::default()
        });
        let diff = list.refresh(&Scene::new(live.stage(), &store));
        (Self { store, live, list }, diff)
    }

    /// Authors `value` as the default of the `type_name` attribute `name`
    /// of `prim`, creating it when it is not authored yet.
    fn set(&mut self, prim: &str, name: &str, type_name: &str, value: Value) -> DrawDiff {
        let path = self.store.path(prim);
        let authored = self
            .live
            .stage()
            .prim(path, &self.store)
            .and_then(|prim| prim.attribute(name))
            .and_then(|attribute| attribute.authored_value(Time::Default))
            .is_some();
        let name = self.store.tokens().intern(name);
        let property = EditTarget::for_layer(LayerId(1)).property(PropertyPath::new(path, name));
        let mut edit = Transaction::new();
        if authored {
            edit.set_default(property, value);
        } else {
            edit.create_property(
                property,
                PropertySpec::typed_attribute(PropertyType::parse(type_name).unwrap())
                    .with_default(value),
            );
        }
        let applied = self.live.apply(&mut self.store, &edit).unwrap();
        let scene = Scene::new(self.live.stage(), &self.store);
        self.list.apply_changes(&scene, &applied.changes)
    }

    fn token(&self, text: &str) -> Value {
        Value::Token(self.store.tokens().intern(text))
    }

    /// The paths drawn, sorted, each with whether it is a stand-in.
    fn drawn(&self) -> Vec<(alloc::string::String, bool)> {
        let mut drawn: Vec<_> = self
            .list
            .items()
            .map(|item| {
                (
                    alloc::string::ToString::to_string(
                        &self.store.paths().display(item.prim, self.store.tokens()),
                    ),
                    matches!(item.source, ItemSource::DrawMode { .. }),
                )
            })
            .collect();
        drawn.sort();
        drawn
    }
}

fn owned(drawn: &[(&str, bool)]) -> Vec<(alloc::string::String, bool)> {
    drawn
        .iter()
        .map(|(path, standin)| ((*path).into(), *standin))
        .collect()
}

#[test]
fn applied_draw_modes_replace_a_models_subtree() {
    let (fixture, diff) = Fixture::new();
    assert_eq!(
        fixture.drawn(),
        owned(&[
            ("/Crate", true),
            ("/Set/Lamp/Bulb", false),
            ("/Set/Robot/Chassis", false),
            ("/Set/Rock", true),
        ]),
        "the lamp has no GeomModelAPI; the robot does not apply its mode"
    );
    assert_eq!(diff.work.standins_read, 2);

    // The rock: its inherited cards, in its extents hint, textured on X.
    let rock = fixture.store.path("/Set/Rock");
    let item = fixture.list.items().find(|item| item.prim == rock).unwrap();
    assert_eq!(item.matrices.len(), 1);
    assert_eq!(item.material, None);
    let buffers = fixture.list.buffers(item.buffer).unwrap();
    let standin = buffers.standin.as_ref().unwrap();
    assert_eq!(standin.standin.mode, DrawMode::Cards);
    assert_eq!(
        standin.input.extent,
        Some([[-1.0, 0.0, -2.0], [1.0, 4.0, 2.0]])
    );
    let faces: Vec<CardFace> = standin.standin.cards.iter().map(|card| card.face).collect();
    assert_eq!(faces, [CardFace::XPos, CardFace::XNeg]);
    assert!(
        standin
            .standin
            .cards
            .iter()
            .all(|card| card.texture == Some(CardFace::XPos))
    );
    let texture = standin.textures[CardFace::XPos.index()].as_ref().unwrap();
    assert_eq!(texture.asset_path, "rock_x.png");
    assert_eq!(standin.color, [0.18; 3], "the schema's fallback");

    // The crate: bounds of the union of its default, render and proxy
    // hints.
    let crate_path = fixture.store.path("/Crate");
    let item = fixture
        .list
        .items()
        .find(|item| item.prim == crate_path)
        .unwrap();
    let standin = fixture
        .list
        .buffers(item.buffer)
        .unwrap()
        .standin
        .as_ref()
        .unwrap();
    assert_eq!(standin.standin.mode, DrawMode::Bounds);
    assert_eq!(standin.input.extent, Some([[-5.0; 3], [5.0; 3]]));
    assert_eq!(standin.standin.lines.len(), 24);
}

#[test]
fn a_draw_mode_edit_discovers_only_its_subtree() {
    let (mut fixture, _) = Fixture::new();
    let default = fixture.token("default");
    let diff = fixture.set("/Set", "model:drawMode", "token", default);
    assert_eq!(diff.work.draw_mode_roots, 1);
    // The rock's stand-in gives way to its mesh; the crate is not read.
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.freed.len(), 1);
    assert_eq!(diff.work.items_untouched, 1, "the crate");
    assert_eq!(diff.work.standins_read, 0);
    assert!(fixture.drawn().contains(&("/Set/Rock/Body".into(), false)));

    let origin = fixture.token("origin");
    let diff = fixture.set("/Set/Rock", "model:drawMode", "token", origin);
    assert_eq!(diff.work.draw_mode_roots, 1);
    assert_eq!((diff.removed.len(), diff.added.len()), (1, 1));
    assert_eq!(diff.work.items_untouched, 3);
    let rock = fixture.store.path("/Set/Rock");
    let item = fixture.list.items().find(|item| item.prim == rock).unwrap();
    let standin = &fixture.list.buffers(item.buffer).unwrap().standin;
    assert_eq!(standin.as_ref().unwrap().standin.lines.len(), 6);

    // The robot applies its inherited mode once it says so.
    let cards = fixture.token("cards");
    fixture.set("/Set", "model:drawMode", "token", cards);
    let diff = fixture.set(
        "/Set/Robot",
        "model:applyDrawMode",
        "bool",
        Value::Bool(true),
    );
    assert_eq!((diff.removed.len(), diff.added.len()), (1, 1));
    assert!(fixture.drawn().contains(&("/Set/Robot".into(), true)));
}

#[test]
fn a_card_input_edit_refills_only_its_standin() {
    let (mut fixture, _) = Fixture::new();
    let diff = fixture.set(
        "/Set/Rock",
        "model:drawModeColor",
        "color3f",
        Value::Vec3f([1.0, 0.0, 0.0]),
    );
    assert_eq!(diff.work.draw_mode_roots, 0);
    assert_eq!(diff.work.standins_read, 1);
    assert_eq!(diff.refilled.len(), 1);
    assert_eq!(diff.work.items_untouched, 3);

    // `model:cardGeometry` on the rock lays its cards on the box.
    let boxed = fixture.token("box");
    let diff = fixture.set("/Set/Rock", "model:cardGeometry", "token", boxed);
    assert_eq!(diff.work.standins_read, 1);
    let rock = fixture.store.path("/Set/Rock");
    let item = fixture.list.items().find(|item| item.prim == rock).unwrap();
    let standin = fixture
        .list
        .buffers(item.buffer)
        .unwrap()
        .standin
        .as_ref()
        .unwrap();
    assert!(
        standin.standin.cards[0]
            .corners
            .iter()
            .all(|corner| corner[0] == 1.0)
    );

    // An inherited `model:cardVisibility` reaches the stand-ins beneath.
    let simple = fixture.token("simple");
    let diff = fixture.set("/Set", "model:cardVisibility", "token", simple);
    assert_eq!(diff.work.standins_read, 1);
    assert_eq!(diff.work.items_untouched, 3);
}
