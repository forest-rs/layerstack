// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
use alloc::{sync::Arc, vec};
use layerstack::{
    InMemoryStore, Layer, PrimSpec, PropertySpec, PropertyType, Stage, StageOptions, Value,
};

fn setup() -> InMemoryStore {
    InMemoryStore::default()
}
fn stage(store: &mut InMemoryStore) -> Stage {
    let schemas = crate::openusd(&mut store.tokens);
    Stage::compose(
        store,
        LayerId(1),
        StageOptions {
            schemas: Some(Arc::new(schemas)),
            ..Default::default()
        },
    )
}
fn attr(name: &str, value: Value) -> PropertySpec {
    PropertySpec::typed_attribute(PropertyType::new(name, false, Value::Null)).with_default(value)
}
#[test]
fn core_type_conflicts_include_weaker_source_and_requests_are_deterministic() {
    let mut store = setup();
    let p = store.path("/Model");
    let a = store.tokens.intern("weight");
    let mut strong = Layer::new(LayerId(1));
    strong.default_prim = Some(store.tokens.intern("Model"));
    strong
        .sublayers
        .push(layerstack::SublayerEntry::new(LayerId(2)));
    let mut spec = PrimSpec::def();
    spec.set_property(a, attr("float", Value::Float(1.)));
    strong.insert_prim(p, spec);
    let mut weak = Layer::new(LayerId(2));
    let mut spec = PrimSpec::over();
    spec.set_property(a, attr("double", Value::Double(2.)));
    weak.insert_prim(p, spec);
    store.insert_layer(strong);
    store.insert_layer(weak);
    let stage = stage(&mut store);
    let scene = Scene::new(&stage, &store);
    let context = ValidationContext::new([
        BuiltinRule::AttributeTypes,
        BuiltinRule::DefaultPrim,
        BuiltinRule::AttributeTypes,
    ]);
    let report = context
        .validate(
            &scene,
            ValidationScope::Stage,
            &[Time::at(2.), Time::Default, Time::at(2.)],
        )
        .unwrap();
    assert_eq!(report.times, vec![Time::Default, Time::at(2.)]);
    assert_eq!(report.problems.len(), 1);
    let error = &report.problems[0];
    assert_eq!(error.site.prim, Some(p));
    assert_eq!(error.site.property, Some(a));
    assert_eq!(error.site.source.as_ref().unwrap().layer, LayerId(2));
    assert_eq!(
        error
            .site
            .source
            .as_ref()
            .unwrap()
            .spec
            .as_ref()
            .unwrap()
            .property(),
        Some(a)
    );
    assert!(
        matches!(&error.kind,ValidationProblemKind::AttributeTypeMismatch{expected:Some(e),authored:Some(a)} if &*e.name=="float" && &*a.name=="double")
    );
    assert_eq!(report.work.property_specs, 2);
    assert_eq!(
        report,
        context
            .validate(
                &scene,
                ValidationScope::Stage,
                &[Time::Default, Time::at(2.)]
            )
            .unwrap()
    );
    assert!(
        ValidationContext::new([BuiltinRule::DefaultPrim])
            .validate(&scene, ValidationScope::Stage, &[Time::Default])
            .unwrap()
            .is_valid()
    );
    assert_eq!(
        context.validate(&scene, ValidationScope::Stage, &[]),
        Err(ValidationError::InvalidTimes)
    );
    assert_eq!(
        context.validate(&scene, ValidationScope::Stage, &[Time::at(f64::NAN)]),
        Err(ValidationError::InvalidTimes)
    );
}
#[test]
fn stage_errors_and_default_prim_are_scoped_and_preserve_original_details() {
    let mut store = setup();
    let p = store.path("/P");
    let absent = store.path("/Absent");
    let mut layer = Layer::new(LayerId(1));
    layer
        .sublayers
        .push(layerstack::SublayerEntry::new(LayerId(1)));
    layer.insert_prim(p, PrimSpec::def());
    store.insert_layer(layer);
    let stage = stage(&mut store);
    let scene = Scene::new(&stage, &store);
    let context =
        ValidationContext::new([BuiltinRule::CompositionErrors, BuiltinRule::DefaultPrim]);
    let report = context
        .validate(&scene, ValidationScope::Stage, &[Time::Default])
        .unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| matches!(p.kind, ValidationProblemKind::InvalidDefaultPrim))
    );
    assert!(report.problems.iter().any(|p|matches!(&p.kind,ValidationProblemKind::Composition(e) if stage.composition_errors().contains(e))));
    let scoped = context
        .validate(&scene, ValidationScope::Subtree(p), &[Time::Default])
        .unwrap();
    assert!(scoped.is_valid());
    assert_eq!(
        context.validate(&scene, ValidationScope::Prim(absent), &[Time::Default]),
        Err(ValidationError::MissingScopePrim(absent))
    );
}
#[cfg(feature = "usd-geom")]
fn mesh(store: &mut InMemoryStore) -> PrimSpec {
    let mut mesh = PrimSpec::def().with_type_name(store.tokens.intern("Mesh"));
    for (name, ty, value) in [
        (
            "points",
            "point3f",
            Value::Array(vec![Value::Vec3f([0.; 3]); 3]),
        ),
        ("faceVertexCounts", "int", Value::Array(vec![Value::Int(3)])),
        (
            "faceVertexIndices",
            "int",
            Value::Array(vec![Value::Int(0), Value::Int(1), Value::Int(2)]),
        ),
    ] {
        mesh.set_property(
            store.tokens.intern(name),
            PropertySpec::typed_attribute(PropertyType::new(ty, true, Value::Null))
                .with_default(value),
        );
    }
    mesh
}
#[cfg(feature = "usd-geom")]
#[test]
fn mesh_checks_use_exact_requested_times_and_subtree_scope() {
    let mut store = setup();
    let p = store.path("/World");
    let m = store.path("/World/Mesh");
    let outside = store.path("/Other");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        p,
        PrimSpec::def().with_type_name(store.tokens.intern("Xform")),
    );
    let mut good = mesh(&mut store);
    let indices = store.tokens.intern("faceVertexIndices");
    good.set_property(
        indices,
        PropertySpec::typed_attribute(PropertyType::new("int", true, Value::Int(0)))
            .with_default(Value::Array(vec![
                Value::Int(0),
                Value::Int(1),
                Value::Int(2),
            ]))
            .with_time_samples(vec![(
                1.,
                Value::Array(vec![Value::Int(0), Value::Int(1), Value::Int(99)]),
            )]),
    );
    layer.insert_prim(m, good);
    layer.insert_prim(
        outside,
        PrimSpec::def().with_type_name(store.tokens.intern("Mesh")),
    );
    store.insert_layer(layer);
    let stage = stage(&mut store);
    let scene = Scene::new(&stage, &store);
    let context = ValidationContext::new([BuiltinRule::MeshTopology]);
    assert!(
        context
            .validate(&scene, ValidationScope::Subtree(p), &[Time::Default])
            .unwrap()
            .is_valid()
    );
    let report = context
        .validate(&scene, ValidationScope::Subtree(p), &[Time::held(1.)])
        .unwrap();
    assert_eq!(report.problems.len(), 1);
    assert_eq!(report.problems[0].site.prim, Some(m));
    assert_eq!(report.problems[0].time, Some(Time::held(1.)));
    assert!(matches!(
        report.problems[0].kind,
        ValidationProblemKind::MeshTopology(crate::subset::MeshTopologyError::InvalidVertexIndex(
            99
        ))
    ));
    assert_eq!(report.work.mesh_time_evaluations, 1);
    assert_eq!(
        context
            .validate(&scene, ValidationScope::Stage, &[Time::Default])
            .unwrap()
            .problems
            .len(),
        1
    );
}
#[cfg(feature = "usd-geom")]
#[test]
fn geometry_metadata_requires_authoring_and_subset_errors_are_structured() {
    let mut store = setup();
    let p = store.path("/Mesh");
    let sub = store.path("/Mesh/Part");
    let orphan = store.path("/Orphan");
    let mut layer = Layer::new(LayerId(1));
    let mut m = mesh(&mut store);
    m.set_property(
        store.tokens.intern("subsetFamily:pieces:familyType"),
        attr("token", Value::Token(store.tokens.intern("partition"))),
    );
    layer.insert_prim(p, m);
    let mut subset = PrimSpec::def().with_type_name(store.tokens.intern("GeomSubset"));
    subset.set_property(
        store.tokens.intern("elementType"),
        attr("token", Value::Token(store.tokens.intern("face"))),
    );
    subset.set_property(
        store.tokens.intern("familyName"),
        attr("token", Value::Token(store.tokens.intern("pieces"))),
    );
    subset.set_property(
        store.tokens.intern("indices"),
        PropertySpec::attribute().with_default(Value::Array(vec![Value::Int(5)])),
    );
    layer.insert_prim(sub, subset);
    layer.insert_prim(
        orphan,
        PrimSpec::def().with_type_name(store.tokens.intern("GeomSubset")),
    );
    store.insert_layer(layer);
    let stage = stage(&mut store);
    let scene = Scene::new(&stage, &store);
    let context = ValidationContext::new([
        BuiltinRule::GeometryStageMetadata,
        BuiltinRule::SubsetFamilies,
        BuiltinRule::SubsetParent,
    ]);
    let report = context
        .validate(&scene, ValidationScope::Stage, &[Time::at(100.)])
        .unwrap();
    assert_eq!(
        report
            .problems
            .iter()
            .filter(|p| matches!(p.kind, ValidationProblemKind::MissingGeometryMetadata(_)))
            .count(),
        2
    );
    assert!(report.problems.iter().any(|p| p.site.prim == Some(orphan)
        && matches!(p.kind, ValidationProblemKind::SubsetParentNotImageable)));
    assert!(
        report
            .problems
            .iter()
            .any(|problem| problem.site.prim == Some(p)
                && matches!(
                    problem.kind,
                    ValidationProblemKind::SubsetFamily {
                        problem: crate::subset::SubsetProblemKind::InvalidIndex(5),
                        ..
                    }
                ))
    );
    assert_eq!(
        BuiltinRule::SubsetFamilies.time_domain(),
        RuleTimeDomain::AllSubsetSamples
    );
    assert_eq!(report.work.subset_families, 1);
}
#[cfg(feature = "usd-shade")]
#[test]
fn binding_rules_distinguish_relationships_and_connection_scope() {
    let mut store = setup();
    let p = store.path("/Bound");
    let shader = store.path("/Material/Shader");
    let outside = store.path("/External");
    let material = store.path("/Material");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        material,
        PrimSpec::def().with_type_name(store.tokens.intern("Material")),
    );
    let rel = store.tokens.intern("material:binding");
    let bad = store.tokens.intern("material:binding:preview");
    let mut bound = PrimSpec::def();
    bound.set_property(rel, PropertySpec::relationship());
    bound.set_property(bad, attr("float", Value::Float(1.)));
    layer.insert_prim(p, bound);
    let input = store.tokens.intern("inputs:x");
    let output = store.tokens.intern("outputs:x");
    let mut sh = PrimSpec::def().with_type_name(store.tokens.intern("Shader"));
    sh.set_property(
        input,
        attr("float", Value::Float(0.)).with_targets(layerstack::ListOp::explicit(vec![
            layerstack::TargetPath::Property(layerstack::PropertyPath::new(outside, output)),
        ])),
    );
    layer.insert_prim(shader, sh);
    let mut external = PrimSpec::def().with_type_name(store.tokens.intern("Shader"));
    external.set_property(output, attr("float", Value::Float(0.)));
    layer.insert_prim(outside, external);
    store.insert_layer(layer);
    let stage = stage(&mut store);
    let scene = Scene::new(&stage, &store);
    let context = ValidationContext::new([
        BuiltinRule::MaterialBindingApi,
        BuiltinRule::MaterialBindingPropertyKinds,
        BuiltinRule::ShadingConnections,
    ]);
    let report = context
        .validate(&scene, ValidationScope::Stage, &[Time::Default])
        .unwrap();
    assert!(
        report
            .problems
            .iter()
            .any(|p| matches!(p.kind, ValidationProblemKind::MissingMaterialBindingApi))
    );
    assert!(report.problems.iter().any(|p| p.site.property == Some(bad)
        && matches!(
            p.kind,
            ValidationProblemKind::MaterialBindingNotRelationship
        )));
    assert!(report.problems.iter().any(|p| matches!(
        p.kind,
        ValidationProblemKind::ShadingConnection(crate::shading::ConnectionError {
            issue: crate::shading::ConnectionIssue::Encapsulation,
            ..
        })
    )));
    assert_eq!(report.work.connections, 1);
    let scoped = context
        .validate(&scene, ValidationScope::Prim(p), &[Time::Default])
        .unwrap();
    assert_eq!(scoped.problems.len(), 2);
    assert_eq!(scoped.work.connections, 0);
}

#[cfg(all(feature = "usd-geom", feature = "usd-shade"))]
#[test]
fn material_subset_policy_matches_authored_presence_and_family_type() {
    let mut store = setup();
    let parent = store.path("/Mesh");
    let part = store.path("/Mesh/Part");
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(parent, mesh(&mut store));
    let mut subset = PrimSpec::def().with_type_name(store.tokens.intern("GeomSubset"));
    subset.set_property(
        store.tokens.intern("material:binding"),
        PropertySpec::relationship(),
    );
    layer.insert_prim(part, subset.clone());
    store.insert_layer(layer.clone());
    let context = ValidationContext::new([BuiltinRule::MaterialSubsetFamilies]);
    let initial = stage(&mut store);
    let report = context
        .validate(
            &Scene::new(&initial, &store),
            ValidationScope::Stage,
            &[Time::Default],
        )
        .unwrap();
    assert!(matches!(
        report.problems[0].kind,
        ValidationProblemKind::MissingMaterialSubsetFamilyName
    ));
    subset.set_property(
        store.tokens.intern("familyName"),
        attr("token", Value::Token(store.tokens.intern("materialBind"))),
    );
    layer.insert_prim(part, subset.clone());
    store.insert_layer(layer.clone());
    let composed = stage(&mut store);
    let report = context
        .validate(
            &Scene::new(&composed, &store),
            ValidationScope::Stage,
            &[Time::Default],
        )
        .unwrap();
    assert_eq!(report.problems.len(), 1);
    assert!(matches!(
        report.problems[0].kind,
        ValidationProblemKind::UnrestrictedMaterialSubsetFamily
    ));
    let mut geom = mesh(&mut store);
    geom.set_property(
        store.tokens.intern("subsetFamily:materialBind:familyType"),
        attr("token", Value::Token(store.tokens.intern("nonOverlapping"))),
    );
    layer.insert_prim(parent, geom);
    store.insert_layer(layer.clone());
    let composed = stage(&mut store);
    assert!(
        context
            .validate(
                &Scene::new(&composed, &store),
                ValidationScope::Stage,
                &[Time::Default]
            )
            .unwrap()
            .is_valid()
    );
    // C++ checks familyName's authored presence, not a forced materialBind value.
    subset.set_property(
        store.tokens.intern("familyName"),
        attr("token", Value::Token(store.tokens.intern("customFamily"))),
    );
    layer.insert_prim(part, subset);
    store.insert_layer(layer);
    let composed = stage(&mut store);
    assert!(
        context
            .validate(
                &Scene::new(&composed, &store),
                ValidationScope::Stage,
                &[Time::Default]
            )
            .unwrap()
            .is_valid()
    );
}
#[cfg(feature = "usd-geom")]
#[test]
fn complete_valid_mesh_scene_passes_all_selected_builtins() {
    let mut store = setup();
    let p = store.path("/Mesh");
    let mut layer = Layer::new(LayerId(1));
    layer.default_prim = Some(store.tokens.intern("Mesh"));
    layer.set_metadata(store.tokens.intern("metersPerUnit"), Value::Double(1.));
    layer.set_metadata(
        store.tokens.intern("upAxis"),
        Value::Token(store.tokens.intern("Y")),
    );
    layer.insert_prim(p, mesh(&mut store));
    store.insert_layer(layer);
    let composed = stage(&mut store);
    let report = ValidationContext::default()
        .validate(
            &Scene::new(&composed, &store),
            ValidationScope::Stage,
            &[Time::Default, Time::at(1.)],
        )
        .unwrap();
    assert!(report.is_valid(), "{:?}", report.problems);
    assert_eq!(report.work.mesh_time_evaluations, 2);
}

#[cfg(feature = "usd-geom")]
#[test]
fn schema_attribute_type_wins_over_an_incompatible_authored_declaration() {
    let mut store = setup();
    let p = store.path("/Mesh");
    let mut layer = Layer::new(LayerId(1));
    let mut m = mesh(&mut store);
    let points = store.tokens.intern("points");
    m.set_property(
        points,
        PropertySpec::typed_attribute(PropertyType::new("double", true, Value::Double(0.))),
    );
    layer.insert_prim(p, m);
    store.insert_layer(layer);
    let composed = stage(&mut store);
    let report = ValidationContext::new([BuiltinRule::AttributeTypes])
        .validate(
            &Scene::new(&composed, &store),
            ValidationScope::Prim(p),
            &[Time::Default],
        )
        .unwrap();
    assert_eq!(report.problems.len(), 1);
    assert!(
        matches!(&report.problems[0].kind,ValidationProblemKind::AttributeTypeMismatch{expected:Some(expected),authored:Some(authored)} if &*expected.name=="point3f" && expected.is_array && &*authored.name=="double")
    );
}

// These consumers import only the public validation and scene APIs.
mod third_party {
    use crate::{
        Scene,
        validation::{
            BuiltinRule, CustomValidationProblem, RuleTimeDomain, ValidationContext,
            ValidationProblemKind, ValidationScope, ValidationSite, ValidationSource, Validator,
            ValidatorDomain, ValidatorFailure, ValidatorId, ValidatorMetadata,
            ValidatorRegistrationError, ValidatorTarget,
        },
    };
    use alloc::{sync::Arc, vec, vec::Vec};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use layerstack::{
        InMemoryStore, Layer, LayerId, PathId, PrimSpec, Stage, StageOptions, Time, Value,
    };

    struct Naming {
        id: Arc<str>,
        metadata_reads: Arc<AtomicUsize>,
    }
    impl Naming {
        fn new(id: &str) -> Self {
            Self {
                id: Arc::from(id),
                metadata_reads: Arc::new(AtomicUsize::new(0)),
            }
        }
    }
    impl Validator for Naming {
        fn metadata(&self) -> ValidatorMetadata {
            self.metadata_reads.fetch_add(1, Ordering::Relaxed);
            ValidatorMetadata {
                id: self.id.clone(),
                description: Arc::from("Require good prim names"),
                domain: ValidatorDomain::Prim,
                time_domain: RuleTimeDomain::RequestedTimes,
            }
        }
        fn validate(
            &self,
            scene: &Scene<'_>,
            target: ValidatorTarget,
            times: &[Time],
            output: &mut Vec<CustomValidationProblem>,
        ) -> Result<(), ValidatorFailure> {
            let ValidatorTarget::Prim(path) = target else {
                panic!("unexpected stage target")
            };
            let tokens = scene.store().tokens();
            let name = scene
                .store()
                .paths()
                .resolve(path)
                .segments()
                .last()
                .map(|t| tokens.resolve(*t))
                .unwrap();
            if name.starts_with("Bad") {
                let source = scene
                    .stage()
                    .prim_stack(path)
                    .unwrap()
                    .into_iter()
                    .next()
                    .map(|(layer, spec)| ValidationSource {
                        layer,
                        spec: Some(spec),
                    });
                for &time in times {
                    output.push(CustomValidationProblem {
                        site: ValidationSite {
                            prim: Some(path),
                            property: None,
                            source: source.clone(),
                        },
                        time: Some(time),
                        code: Arc::from("invalid-name"),
                        message: Arc::from("Prim name starts with Bad"),
                        details: Some(Value::String(Arc::from(name))),
                    });
                }
            }
            Ok(())
        }
    }
    struct StageFailure {
        fail: bool,
        invalid_time: bool,
    }
    impl Validator for StageFailure {
        fn metadata(&self) -> ValidatorMetadata {
            ValidatorMetadata {
                id: Arc::from("studio:stage-check"),
                description: Arc::from("Stage check"),
                domain: ValidatorDomain::Stage,
                time_domain: RuleTimeDomain::Structural,
            }
        }
        fn validate(
            &self,
            scene: &Scene<'_>,
            target: ValidatorTarget,
            times: &[Time],
            output: &mut Vec<CustomValidationProblem>,
        ) -> Result<(), ValidatorFailure> {
            assert_eq!(target, ValidatorTarget::Stage);
            assert_eq!(times, [Time::Default, Time::at(2.)]);
            output.push(CustomValidationProblem {
                site: ValidationSite {
                    prim: None,
                    property: None,
                    source: scene
                        .stage()
                        .root_layer()
                        .map(|layer| ValidationSource { layer, spec: None }),
                },
                time: self.invalid_time.then(|| Time::at(f64::NAN)),
                code: Arc::from("stage-invariant"),
                message: Arc::from("Stage invariant failed"),
                details: None,
            });
            if self.fail {
                Err(ValidatorFailure {
                    code: Arc::from("unavailable-engine"),
                    message: Arc::from("Engine unavailable"),
                })
            } else {
                Ok(())
            }
        }
    }
    fn fixture() -> (InMemoryStore, Stage, [PathId; 3]) {
        let mut store = InMemoryStore::default();
        let root = store.path("/Root");
        let bad = store.path("/Root/BadChild");
        let outside = store.path("/BadOutside");
        let mut layer = Layer::new(LayerId(1));
        layer.default_prim = Some(store.tokens.intern("Root"));
        for path in [root, bad, outside] {
            layer.insert_prim(path, PrimSpec::def());
        }
        store.insert_layer(layer);
        let schemas = crate::openusd(&mut store.tokens);
        let stage = Stage::compose(
            &mut store,
            LayerId(1),
            StageOptions {
                schemas: Some(Arc::new(schemas)),
                ..StageOptions::default()
            },
        );
        (store, stage, [root, bad, outside])
    }
    #[test]
    fn public_custom_rule_has_shared_scope_times_source_and_clone() {
        let (store, stage, [root, bad, _]) = fixture();
        let scene = Scene::new(&stage, &store);
        let mut context = ValidationContext::new([BuiltinRule::DefaultPrim]);
        let naming = Arc::new(Naming::new("studio:names"));
        context.add_validator(naming.clone()).unwrap();
        let clone = context.clone();
        let report = context
            .validate(
                &scene,
                ValidationScope::Subtree(root),
                &[Time::at(2.), Time::Default, Time::at(2.)],
            )
            .unwrap();
        assert_eq!(report.times, [Time::Default, Time::at(2.)]);
        assert!(report.is_complete());
        assert!(!report.is_valid());
        assert_eq!(report.work.prims, 2);
        assert_eq!(report.work.custom_invocations, 2);
        assert_eq!(report.work.rule_invocations, 2); // stage check excluded by subtree scope
        assert_eq!(report.problems.len(), 2);
        for finding in &report.problems {
            assert_eq!(finding.rule, ValidatorId::Custom(Arc::from("studio:names")));
            assert_eq!(finding.site.prim, Some(bad));
            assert_eq!(finding.site.source.as_ref().unwrap().layer, LayerId(1));
            assert!(finding.site.source.as_ref().unwrap().spec.is_some());
            assert!(
                matches!(&finding.kind, ValidationProblemKind::Custom { code, details: Some(Value::String(name)), .. }
                if &**code == "invalid-name" && &**name == "BadChild")
            );
        }
        assert_eq!(
            clone
                .validate(&scene, ValidationScope::Subtree(root), &report.times)
                .unwrap(),
            report
        );
        assert_eq!(naming.metadata_reads.load(Ordering::Relaxed), 1);
        assert!(
            context
                .validate(&scene, ValidationScope::Prim(root), &[Time::Default])
                .unwrap()
                .is_valid()
        );
        assert_eq!(
            context.registered_validators().next().unwrap().time_domain,
            RuleTimeDomain::RequestedTimes
        );
    }
    #[test]
    fn registration_is_atomic_and_reports_ignore_registration_order() {
        let (store, stage, [root, _, _]) = fixture();
        let scene = Scene::new(&stage, &store);
        let mut a = ValidationContext::new([]);
        let mut b = ValidationContext::new([]);
        for id in ["studio:z-names", "studio:a-names"] {
            a.add_validator(Arc::new(Naming::new(id))).unwrap();
        }
        for id in ["studio:a-names", "studio:z-names"] {
            b.add_validator(Arc::new(Naming::new(id))).unwrap();
        }
        let before = a.registered_validators().cloned().collect::<Vec<_>>();
        assert_eq!(
            a.add_validator(Arc::new(Naming::new("studio:a-names"))),
            Err(ValidatorRegistrationError::DuplicateId(Arc::from(
                "studio:a-names"
            )))
        );
        for id in ["", "unnamespaced", ":name", "namespace:", "studio:bad name"] {
            assert_eq!(
                a.add_validator(Arc::new(Naming::new(id))),
                Err(ValidatorRegistrationError::InvalidId(Arc::from(id)))
            );
        }
        assert_eq!(
            a.registered_validators().cloned().collect::<Vec<_>>(),
            before
        );
        assert_eq!(
            a.validate(
                &scene,
                ValidationScope::Subtree(root),
                &[Time::at(2.), Time::Default]
            )
            .unwrap(),
            b.validate(
                &scene,
                ValidationScope::Subtree(root),
                &[Time::Default, Time::at(2.)]
            )
            .unwrap()
        );
        let mut clone = a.clone();
        clone
            .add_validator(Arc::new(Naming::new("other:names")))
            .unwrap();
        assert_eq!(a.registered_validators().len(), 2);
        assert_eq!(clone.registered_validators().len(), 3);
    }
    #[test]
    fn failed_callbacks_discard_local_findings_and_continue_other_rules() {
        let (store, stage, [root, _, _]) = fixture();
        let scene = Scene::new(&stage, &store);
        let mut context = ValidationContext::new([BuiltinRule::DefaultPrim]);
        context
            .add_validator(Arc::new(Naming::new("studio:names")))
            .unwrap();
        context
            .add_validator(Arc::new(StageFailure {
                fail: true,
                invalid_time: false,
            }))
            .unwrap();
        let report = context
            .validate(
                &scene,
                ValidationScope::Stage,
                &[Time::Default, Time::at(2.)],
            )
            .unwrap();
        assert!(!report.is_complete());
        assert!(!report.is_valid());
        assert_eq!(report.work.custom_invocations, 4); // three prims and one stage
        assert_eq!(report.work.rule_invocations, 5);
        assert_eq!(report.problems.len(), 4); // only naming, two names at two times
        assert_eq!(report.failures.len(), 1);
        assert_eq!(
            report.failures[0].rule,
            ValidatorId::Custom(Arc::from("studio:stage-check"))
        );
        assert_eq!(report.failures[0].target, ValidatorTarget::Stage);
        assert_eq!(&*report.failures[0].failure.code, "unavailable-engine");
        let scoped = context
            .validate(&scene, ValidationScope::Prim(root), &[Time::Default])
            .unwrap();
        assert!(scoped.is_complete());
        assert!(scoped.is_valid());
        assert_eq!(scoped.work.custom_invocations, 1);
    }
    #[test]
    fn malformed_diagnostic_times_are_execution_failures() {
        let (store, stage, _) = fixture();
        let scene = Scene::new(&stage, &store);
        let mut context = ValidationContext::new([]);
        context
            .add_validator(Arc::new(StageFailure {
                fail: false,
                invalid_time: true,
            }))
            .unwrap();
        let report = context
            .validate(
                &scene,
                ValidationScope::Stage,
                &[Time::Default, Time::at(2.)],
            )
            .unwrap();
        assert!(report.problems.is_empty());
        assert!(!report.is_complete());
        assert!(!report.is_valid());
        assert_eq!(
            &*report.failures[0].failure.code,
            "validation:invalid-diagnostic-time"
        );
        assert_eq!(
            report.rules,
            vec![ValidatorId::Custom(Arc::from("studio:stage-check"))]
        );
    }
}
