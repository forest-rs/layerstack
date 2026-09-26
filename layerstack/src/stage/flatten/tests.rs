// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use crate::{
    asset::AssetResolver,
    doc::{InMemoryStore, LayerOffset, Specifier},
    property::PropertyType,
    spline::{CurveType, Extrapolation, SplineData, SplineDataType},
    stage::StageOptions,
};
use alloc::{string::ToString, vec};

fn double() -> PropertyType {
    PropertyType::new("double", false, Value::Double(0.0))
}

/// Layer 1 references `/Asset` of layer 2 from `/World/Tree` through
/// `offset`; the asset authors `height` with time samples and a child.
fn referenced_scene(store: &mut InMemoryStore, offset: LayerOffset) -> TokenId {
    let height = store.tokens.intern("height");
    let (world, tree, asset, leaf) = (
        store.path("/World"),
        store.path("/World/Tree"),
        store.path("/Asset"),
        store.path("/Asset/Leaf"),
    );
    let mut root = Layer::new(LayerId(1));
    let up_axis = store.tokens.intern("upAxis");
    root.set_metadata(up_axis, Value::string("Z"));
    root.insert_prim(world, PrimSpec::def());
    let mut reference = Reference::new(LayerId(2), asset);
    reference.asset = Some("./asset.usda".into());
    reference.layer_offset = offset;
    root.insert_prim(tree, PrimSpec::def().with_reference(reference));
    store.insert_layer(root);

    let mut layer = Layer::new(LayerId(2));
    layer.insert_prim(
        asset,
        PrimSpec::def().with_property(
            height,
            PropertySpec::typed_attribute(double())
                .with_default(Value::Double(1.0))
                .with_time_samples(vec![(0.0, Value::Double(1.0)), (10.0, Value::Double(2.0))]),
        ),
    );
    layer.insert_prim(leaf, PrimSpec::def());
    store.insert_layer(layer);
    height
}

fn flatten(
    store: &mut InMemoryStore,
    requirements: &FlattenRequirements<'_>,
) -> Result<Flattened, FlattenError> {
    let stage = Stage::compose(store, LayerId(1), StageOptions::default());
    stage.flatten(store, LayerId(1), LayerId(9), requirements)
}

#[test]
fn flattened_layer_holds_the_composed_stage_without_arcs() {
    // Spec: AOUSD Core §12.3.2.1: the asset's samples land in stage
    // time through the reference's offset and scale.
    let mut store = InMemoryStore::default();
    let offset = LayerOffset {
        offset: 5.0,
        scale: 2.0,
    };
    let height = referenced_scene(&mut store, offset);
    let flat = flatten(&mut store, &FlattenRequirements::default()).expect("flattens");
    let layer = &flat.layer;

    assert_eq!(layer.id, LayerId(9));
    assert_eq!(layer.metadata, store.layers[&LayerId(1)].metadata);
    let tree = store.path("/World/Tree");
    let spec = &layer.prims[&tree];
    assert_eq!(spec.specifier, Some(Specifier::Def));
    assert_eq!(spec.references, ListOp::default(), "no arcs remain");
    assert_eq!(spec.authored_children, [store.tokens.intern("Leaf")]);
    let attribute = spec.property(height).expect("height");
    assert_eq!(attribute.default, Some(Value::Double(1.0)));
    assert_eq!(
        attribute.time_samples,
        Some(vec![(5.0, Value::Double(1.0)), (25.0, Value::Double(2.0))])
    );
    let root = store.path("/");
    assert_eq!(
        layer.prims[&root].authored_children,
        [store.tokens.intern("World")]
    );
    assert!(layer.prims.contains_key(&store.path("/World/Tree/Leaf")));
}

#[test]
fn the_report_counts_what_is_exact_and_lists_what_is_not() {
    let mut store = InMemoryStore::default();
    let offset = LayerOffset {
        offset: 5.0,
        scale: 2.0,
    };
    referenced_scene(&mut store, offset);
    let report = flatten(&mut store, &FlattenRequirements::default())
        .expect("flattens")
        .report;
    assert_eq!(
        report.preserved,
        Preserved {
            // The pseudo-root, /World, /World/Tree and its leaf.
            prims: 4,
            properties: 1,
            // `upAxis`.
            metadata_fields: 1,
            defaults: 1,
            time_samples: 0,
            splines: 0,
            targets: 0,
        }
    );
    assert_eq!(
        report.findings,
        [Finding {
            path: ObjectPath {
                prim: "/World/Tree".into(),
                property: Some("height".into()),
            },
            kind: FindingKind::Transformed(Transformation::SamplesRetimed { offset }),
            source: Some(FindingSource {
                layer: LayerId(2),
                spec: "/Asset.height".into(),
            }),
        }]
    );
    assert!(report.is_lossless());
}

#[test]
fn missing_root_layer_is_an_error() {
    let mut store = InMemoryStore::default();
    referenced_scene(&mut store, LayerOffset::IDENTITY);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    assert_eq!(
        stage.flatten(
            &mut store,
            LayerId(7),
            LayerId(9),
            &FlattenRequirements::default()
        ),
        Err(FlattenError::MissingRootLayer(LayerId(7)))
    );
}

/// Flattens the referenced scene through `offset` after `edit` changes the
/// asset's `/Asset` spec.
fn flatten_edited(
    offset: LayerOffset,
    requirements: &FlattenRequirements<'_>,
    edit: impl FnOnce(&mut InMemoryStore, &mut PrimSpec),
) -> Result<Flattened, FlattenError> {
    let mut store = InMemoryStore::default();
    referenced_scene(&mut store, offset);
    let asset = store.path("/Asset");
    let mut spec = store.layers[&LayerId(2)].prims[&asset].clone();
    edit(&mut store, &mut spec);
    store
        .layers
        .get_mut(&LayerId(2))
        .unwrap()
        .insert_prim(asset, spec);
    flatten(&mut store, requirements)
}

/// The unmet requirements of a refused flatten, as (requirement, path,
/// loss, spec).
fn unmet(result: Result<Flattened, FlattenError>) -> Vec<(Requirement, String, Loss, String)> {
    let Err(FlattenError::Refused(refusal)) = result else {
        panic!("expected a refusal, got {result:?}");
    };
    refusal
        .unmet
        .into_iter()
        .map(|unmet| {
            let FindingKind::Lost(loss) = unmet.finding.kind else {
                panic!("an unmet requirement is a loss");
            };
            let spec = unmet.finding.source.map(|s| s.spec).unwrap_or_default();
            (
                unmet.requirement,
                unmet.finding.path.to_string(),
                loss,
                spec,
            )
        })
        .collect()
}

fn add_spline(store: &mut InMemoryStore, spec: &mut PrimSpec) {
    let spline = SplineData {
        data_type: SplineDataType::Double,
        default_curve_type: CurveType::Bezier,
        pre_extrapolation: Extrapolation::Held,
        post_extrapolation: Extrapolation::Held,
        loop_params: None,
        knots: Vec::new(),
    };
    let name = store.tokens.intern("curve");
    spec.set_property(
        name,
        PropertySpec::typed_attribute(double()).with_spline(spline),
    );
}

fn add_clips(store: &mut InMemoryStore, spec: &mut PrimSpec) {
    let name = store.tokens.intern("clips");
    spec.set_field(name, Value::Dictionary(Vec::new()));
}

fn add_untyped(store: &mut InMemoryStore, spec: &mut PrimSpec) {
    let name = store.tokens.intern("loose");
    spec.set_property(name, PropertySpec::attribute().with_default(Value::Int(1)));
}

const SHIFTED: LayerOffset = LayerOffset {
    offset: 1.0,
    scale: 1.0,
};

#[test]
fn a_refusal_lists_every_unmet_requirement() {
    let all = |store: &mut InMemoryStore, spec: &mut PrimSpec| {
        add_spline(store, spec);
        add_clips(store, spec);
        add_untyped(store, spec);
    };
    assert_eq!(
        unmet(flatten_edited(
            SHIFTED,
            &FlattenRequirements::default(),
            all
        )),
        [
            (
                Requirement::ExactAnimation,
                "/World/Tree".into(),
                Loss::ValueClips,
                "/Asset".into()
            ),
            (
                Requirement::NoLoss,
                "/World/Tree.loose".into(),
                Loss::UntypedAttribute,
                "/Asset.loose".into()
            ),
        ]
    );
}

#[test]
fn relaxed_requirements_report_losses_instead() {
    // Without exact animation every loss still refuses under `RefuseAny`,
    // as `NoLoss`.
    let inexact = FlattenRequirements {
        exact_animation: false,
        ..FlattenRequirements::default()
    };
    let requirements: Vec<Requirement> = unmet(flatten_edited(SHIFTED, &inexact, add_clips))
        .into_iter()
        .map(|(requirement, ..)| requirement)
        .collect();
    assert_eq!(requirements, [Requirement::NoLoss]);

    // Accepting losses no requirement names flattens, and reports them.
    let lenient = FlattenRequirements {
        exact_animation: false,
        losses: LossPolicy::RefuseRequired,
        ..FlattenRequirements::default()
    };
    let flat = flatten_edited(SHIFTED, &lenient, |store, spec| {
        add_clips(store, spec);
        add_untyped(store, spec);
    })
    .expect("losses are accepted");
    let lost: Vec<(String, FindingKind)> = flat
        .report
        .lost()
        .map(|finding| (finding.path.to_string(), finding.kind.clone()))
        .collect();
    assert_eq!(
        lost,
        [
            ("/World/Tree".into(), FindingKind::Lost(Loss::ValueClips)),
            (
                "/World/Tree.loose".into(),
                FindingKind::Lost(Loss::UntypedAttribute)
            ),
        ]
    );
    let tree = flat
        .layer
        .prims
        .values()
        .find(|spec| spec.properties.len() == 1 && spec.specifier == Some(Specifier::Def));
    let tree = tree.expect("the tree keeps its other property");
    assert!(tree.fields.is_empty(), "the lost clips are not written");
}

#[test]
fn accepting_the_unmet_requirements_flattens_with_the_same_losses() {
    let all = |store: &mut InMemoryStore, spec: &mut PrimSpec| {
        add_spline(store, spec);
        add_clips(store, spec);
        add_untyped(store, spec);
    };
    let requirements = FlattenRequirements::default();
    let Err(FlattenError::Refused(refusal)) = flatten_edited(SHIFTED, &requirements, all) else {
        panic!("refused");
    };
    let relaxed = requirements.accepting(&refusal.unmet);
    assert_eq!(
        relaxed,
        FlattenRequirements {
            exact_animation: false,
            losses: LossPolicy::RefuseRequired,
            ..requirements
        }
    );
    let flat = flatten_edited(SHIFTED, &relaxed, all).expect("the losses are accepted");
    let lost: Vec<&Finding> = flat.report.lost().collect();
    let refused: Vec<&Finding> = refusal.unmet.iter().map(|u| &u.finding).collect();
    assert_eq!(lost, refused);
}

#[test]
fn exact_animation_alone_refuses_under_refuse_required() {
    let requirements = FlattenRequirements {
        losses: LossPolicy::RefuseRequired,
        ..FlattenRequirements::default()
    };
    assert_eq!(
        unmet(flatten_edited(SHIFTED, &requirements, |store, spec| {
            add_clips(store, spec);
            add_untyped(store, spec);
        })),
        [(
            Requirement::ExactAnimation,
            "/World/Tree".into(),
            Loss::ValueClips,
            "/Asset".into()
        )]
    );
}

#[test]
fn a_spline_through_an_offset_is_retimed() {
    use crate::spline::{Knot, KnotInterp};

    let flat = flatten_edited(SHIFTED, &FlattenRequirements::default(), |store, spec| {
        add_spline(store, spec);
        let curve = store.tokens.intern("curve");
        let spline = spec
            .properties
            .iter_mut()
            .find(|p| p.name == curve)
            .and_then(|p| p.spec.spline.as_mut())
            .unwrap();
        spline.knots.push(Knot {
            time: 2.0,
            value: 1.0,
            pre_value: None,
            next_interp: KnotInterp::Held,
            curve_type: CurveType::Bezier,
            pre_tan_maya_form: false,
            post_tan_maya_form: false,
            pre_tan_width: 0.0,
            post_tan_width: 0.0,
            pre_tan_slope: 0.0,
            post_tan_slope: 0.0,
        });
    })
    .expect("flattens");
    let spline = flat
        .layer
        .prims
        .values()
        .flat_map(|spec| &spec.properties)
        .find_map(|p| p.spec.spline.as_ref())
        .expect("the spline is written");
    assert_eq!(spline.knots[0].time, 3.0, "in stage time");
    let retimed: Vec<String> = flat.report.transformed().map(ToString::to_string).collect();
    assert!(
        retimed.contains(
            &"/World/Tree.curve: transformed: spline retimed (offset 1, scale 1) \
              (layer 2, /Asset.curve)"
                .to_string()
        ),
        "{retimed:?}"
    );
}

#[test]
fn sparse_array_edit_samples_are_baked() {
    use crate::array_edit::{ArrayEdit, ArrayEditOp, ArrayEditOperand, ArrayIndex};

    // The asset samples dense arrays at 0 and 10; the referencing layer
    // edits the first element at 5. Each edit composes over the weaker
    // sample held at its time, and each weaker sample under the edit held
    // at its time: 0 and 10 are edited too.
    let mut store = InMemoryStore::default();
    referenced_scene(&mut store, LayerOffset::IDENTITY);
    let ids = store.tokens.intern("ids");
    let ints = PropertyType::new("int", true, Value::Int(0));
    let array = |items: &[i32]| Value::Array(items.iter().map(|&i| Value::Int(i)).collect());
    let asset = store.path("/Asset");
    let asset_spec = store
        .layers
        .get_mut(&LayerId(2))
        .unwrap()
        .prims
        .get_mut(&asset)
        .unwrap();
    asset_spec.set_property(
        ids,
        PropertySpec::typed_attribute(ints.clone())
            .with_time_samples(vec![(0.0, array(&[1, 2, 3])), (10.0, array(&[4, 5, 6]))]),
    );
    let tree = store.path("/World/Tree");
    let edit = ArrayEdit {
        ops: vec![ArrayEditOp::Write {
            src: ArrayEditOperand::Literal(Value::Int(9)),
            index: ArrayIndex::Position(0),
        }],
    };
    let tree_spec = store
        .layers
        .get_mut(&LayerId(1))
        .unwrap()
        .prims
        .get_mut(&tree)
        .unwrap();
    tree_spec.set_property(
        ids,
        PropertySpec::typed_attribute(ints).with_time_samples(vec![(5.0, Value::ArrayEdit(edit))]),
    );
    let flat = flatten(&mut store, &FlattenRequirements::default()).expect("flattens");
    let samples = flat.layer.prims[&tree]
        .property(ids)
        .unwrap()
        .time_samples
        .clone();
    assert_eq!(
        samples,
        Some(vec![
            (0.0, array(&[9, 2, 3])),
            (5.0, array(&[9, 2, 3])),
            (10.0, array(&[9, 5, 6])),
        ])
    );
    let baked: Vec<String> = flat
        .report
        .transformed()
        .filter(|f| f.kind == FindingKind::Transformed(Transformation::ArrayEditsBaked))
        .map(|f| f.path.to_string())
        .collect();
    assert_eq!(baked, ["/World/Tree.ids"]);
}

#[test]
fn asset_paths_are_external_and_anchoring_is_required_when_declared() {
    let texture = |store: &mut InMemoryStore, spec: &mut PrimSpec| {
        let name = store.tokens.intern("texture");
        spec.set_property(
            name,
            PropertySpec::typed_attribute(PropertyType::new(
                "asset",
                false,
                Value::Asset("".into()),
            ))
            .with_default(Value::Asset("./bark.png".into())),
        );
    };
    let flat = flatten_edited(
        LayerOffset::IDENTITY,
        &FlattenRequirements::default(),
        texture,
    )
    .expect("flattens");
    let external: Vec<String> = flat.report.external().map(ToString::to_string).collect();
    assert_eq!(
        external,
        ["/World/Tree.texture: external: @./bark.png@ (layer 2, /Asset.texture)"]
    );

    /// Knows where the asset layer is when `known`.
    struct Located {
        known: bool,
    }
    impl AssetResolver for Located {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<LayerId>,
            _: &mut crate::interner::TokenInterner,
            _: &mut crate::path::PathInterner,
        ) -> Result<crate::asset::ResolvedAsset, crate::asset::AssetResolveError> {
            Err(crate::asset::AssetResolveError::NotFound)
        }
        fn resolved_path(&self, id: LayerId) -> Option<&str> {
            (self.known && id == LayerId(2)).then_some("/assets/trees/asset.usda")
        }
    }
    let anchored = FlattenRequirements {
        asset_paths: AssetPaths::Anchored(&Located { known: false }),
        ..FlattenRequirements::default()
    };
    assert_eq!(
        unmet(flatten_edited(LayerOffset::IDENTITY, &anchored, texture)),
        [(
            Requirement::AnchoredAssetPaths,
            "/World/Tree.texture".into(),
            Loss::UnanchoredAssetPath,
            "/Asset.texture".into()
        )]
    );

    let located = Located { known: true };
    let anchored = FlattenRequirements {
        asset_paths: AssetPaths::Anchored(&located),
        ..FlattenRequirements::default()
    };
    let flat = flatten_edited(LayerOffset::IDENTITY, &anchored, texture).expect("anchors");
    let findings: Vec<String> = flat
        .report
        .findings
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        findings,
        [
            "/World/Tree.texture: transformed: @./bark.png@ anchored as \
             @/assets/trees/bark.png@ (layer 2, /Asset.texture)",
            "/World/Tree.texture: external: @/assets/trees/bark.png@ (layer 2, /Asset.texture)",
        ]
    );
}

#[test]
fn the_flattened_layer_verifies_against_the_stage() {
    let mut store = InMemoryStore::default();
    let offset = LayerOffset {
        offset: 5.0,
        scale: 2.0,
    };
    referenced_scene(&mut store, offset);
    let stage = Stage::compose(&mut store, LayerId(1), StageOptions::default());
    let flat = stage
        .flatten(
            &mut store,
            LayerId(1),
            LayerId(9),
            &FlattenRequirements::default(),
        )
        .expect("flattens");
    store.insert_layer(flat.layer.clone());
    let flattened = Stage::compose(&mut store, LayerId(9), StageOptions::default());
    let verification = stage.verify_flattened(&flattened, &store, &flat.report, &[7.5]);
    assert!(verification.is_equivalent(), "{verification:?}");
    let scope = &verification.scope;
    assert_eq!((scope.prims, scope.properties), (3, 1));
    // `height` at the default time, at its samples (5, 25) and at 7.5.
    assert_eq!((scope.values, scope.sample_times), (4, 2));

    // A flattened layer that lost a value does not verify.
    let height = store.property_path("/World/Tree.height");
    let mut edited = flat.layer.clone();
    let spec = edited.prims.get_mut(&height.prim_path()).unwrap();
    spec.properties[0].spec.time_samples = None;
    store.insert_layer(edited);
    let flattened = Stage::compose(&mut store, LayerId(9), StageOptions::default());
    let verification = stage.verify_flattened(&flattened, &store, &flat.report, &[7.5]);
    let kinds: Vec<&MismatchKind> = verification.mismatches.iter().map(|m| &m.kind).collect();
    assert_eq!(
        kinds,
        [
            &MismatchKind::ValueAt { time: 7.5 },
            &MismatchKind::ValueAt { time: 25.0 },
        ]
    );

    // Nor does one whose property order or active state differs.
    let mut edited = flat.layer.clone();
    let spec = edited.prims.get_mut(&height.prim_path()).unwrap();
    spec.property_order = Some(vec![height.property()]);
    spec.active = Some(true);
    store.insert_layer(edited);
    let flattened = Stage::compose(&mut store, LayerId(9), StageOptions::default());
    let verification = stage.verify_flattened(&flattened, &store, &flat.report, &[7.5]);
    let kinds: Vec<&MismatchKind> = verification.mismatches.iter().map(|m| &m.kind).collect();
    assert_eq!(
        kinds,
        [
            &MismatchKind::Metadata {
                field: "active".into()
            },
            &MismatchKind::Metadata {
                field: "propertyOrder".into()
            },
        ]
    );
}

#[test]
fn instances_share_a_prototype_or_expand() {
    let mut store = InMemoryStore::default();
    let (a, b, asset, leaf) = (
        store.path("/A"),
        store.path("/B"),
        store.path("/Asset"),
        store.path("/Asset/Leaf"),
    );
    let mut root = Layer::new(LayerId(1));
    for instance in [a, b] {
        let mut reference = Reference::new(LayerId(2), asset);
        reference.asset = Some("./asset.usda".into());
        let mut spec = PrimSpec::def().with_reference(reference);
        spec.instanceable = Some(true);
        root.insert_prim(instance, spec);
    }
    store.insert_layer(root);
    let mut layer = Layer::new(LayerId(2));
    let leaf_name = store.tokens.intern("Leaf");
    layer.insert_prim(asset, PrimSpec::def().with_children(vec![leaf_name]));
    layer.insert_prim(leaf, PrimSpec::def());
    store.insert_layer(layer);

    let shared = flatten(&mut store, &FlattenRequirements::default()).expect("flattens");
    let transformed: Vec<String> = shared
        .report
        .transformed()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        transformed,
        [
            "/A: transformed: instance of /Flattened_Prototype_1",
            "/B: transformed: instance of /Flattened_Prototype_1",
        ]
    );
    assert_eq!(
        shared.report.prototypes().collect::<Vec<_>>(),
        ["/Flattened_Prototype_1"]
    );

    let expand = FlattenRequirements {
        instancing: Instancing::Expand,
        ..FlattenRequirements::default()
    };
    let expanded = flatten(&mut store, &expand).expect("flattens");
    assert!(expanded.report.prototypes().next().is_none());
    let mut written: Vec<String> = expanded
        .layer
        .prims
        .keys()
        .map(|&p| store.paths.display(p, &store.tokens))
        .collect();
    written.sort();
    assert_eq!(written, ["/", "/A", "/A/Leaf", "/B", "/B/Leaf"]);
    assert_eq!(
        expanded
            .report
            .transformed()
            .map(|f| f.kind.clone())
            .collect::<Vec<_>>(),
        [
            FindingKind::Transformed(Transformation::InstanceExpanded),
            FindingKind::Transformed(Transformation::InstanceExpanded),
        ]
    );
}
