// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::*;
#[cfg(feature = "usd-geom")]
use alloc::collections::BTreeSet;
use alloc::vec;
use layerstack::{Path, PropertyKind, PropertyPath, PropertyType};

fn time_order(a: &Time, b: &Time) -> core::cmp::Ordering {
    match (a, b) {
        (Time::Default, Time::Default) => core::cmp::Ordering::Equal,
        (Time::Default, _) => core::cmp::Ordering::Less,
        (_, Time::Default) => core::cmp::Ordering::Greater,
        (
            Time::At {
                code: a,
                interpolation: ia,
            },
            Time::At {
                code: b,
                interpolation: ib,
            },
        ) => a
            .partial_cmp(b)
            .expect("finite times")
            .then_with(|| (*ia as u8).cmp(&(*ib as u8))),
    }
}
pub(super) fn validate(
    rules: &[BuiltinRule],
    custom: &[custom::RegisteredValidator],
    scene: &Scene<'_>,
    scope: ValidationScope,
    times: &[Time],
) -> Result<ValidationReport, ValidationError> {
    if times.is_empty()
        || times
            .iter()
            .any(|t| matches!(t,Time::At{code,..} if !code.is_finite()))
    {
        return Err(ValidationError::InvalidTimes);
    }
    let stage = scene.stage();
    let paths = scene.store().paths();
    let tokens = scene.store().tokens();
    let root = paths.lookup(&Path::root());
    let scope_root = match scope {
        ValidationScope::Stage => root,
        ValidationScope::Prim(p) | ValidationScope::Subtree(p) => {
            if !stage.has_prim(p) {
                return Err(ValidationError::MissingScopePrim(p));
            }
            Some(p)
        }
    };
    let mut prims: Vec<_> = match (scope, scope_root) {
        (ValidationScope::Prim(p), _) => vec![p],
        (_, Some(p)) => stage
            .traverse(p)
            .filter(|p| Some(*p) != root && stage.has_prim(*p))
            .collect(),
        _ => Vec::new(),
    };
    prims.sort_by(|a, b| paths.resolve(*a).cmp_with_tokens(paths.resolve(*b), tokens));
    prims.dedup();
    let mut times = times.to_vec();
    times.sort_by(time_order);
    times.dedup();
    let mut out = ValidationReport {
        rules: rules
            .iter()
            .copied()
            .map(ValidatorId::Builtin)
            .chain(
                custom
                    .iter()
                    .map(|v| ValidatorId::Custom(v.metadata.id.clone())),
            )
            .collect(),
        scope,
        times,
        problems: Vec::new(),
        failures: Vec::new(),
        work: ValidationWork {
            prims: prims.len(),
            ..Default::default()
        },
    };
    for &rule in rules {
        match rule {
            BuiltinRule::CompositionErrors => {
                out.work.rule_invocations += 1;
                for error in stage.composition_errors() {
                    let included = match (scope, error.prim()) {
                        (ValidationScope::Stage, _) => true,
                        (ValidationScope::Prim(p), Some(q)) => p == q,
                        (ValidationScope::Subtree(p), Some(q)) => {
                            paths.resolve(p).is_prefix_of(paths.resolve(q))
                        }
                        _ => false,
                    };
                    if included {
                        push(
                            &mut out,
                            rule,
                            composition_site(scene, error),
                            None,
                            ValidationProblemKind::Composition(error.clone()),
                        );
                    }
                }
            }
            BuiltinRule::DefaultPrim => {
                if scope == ValidationScope::Stage {
                    out.work.rule_invocations += 1;
                    if !valid_default_prim(scene) {
                        push(
                            &mut out,
                            rule,
                            stage_site(scene),
                            None,
                            ValidationProblemKind::InvalidDefaultPrim,
                        );
                    }
                }
            }
            #[cfg(feature = "usd-geom")]
            BuiltinRule::GeometryStageMetadata => {
                if scope == ValidationScope::Stage {
                    out.work.rule_invocations += 1;
                    for name in ["metersPerUnit", "upAxis"] {
                        let authored = stage
                            .root_layer()
                            .and_then(|id| scene.store().layer(id))
                            .is_some_and(|layer| {
                                tokens
                                    .lookup(name)
                                    .is_some_and(|key| layer.metadata.iter().any(|f| f.name == key))
                            });
                        if !authored {
                            push(
                                &mut out,
                                rule,
                                stage_site(scene),
                                None,
                                ValidationProblemKind::MissingGeometryMetadata(name),
                            );
                        }
                    }
                }
            }
            _ => {
                for &prim in &prims {
                    out.work.rule_invocations += 1;
                    prim_rule(scene, prim, rule, &mut out);
                }
            }
        }
    }
    for registered in custom {
        match registered.metadata.domain {
            ValidatorDomain::Stage if scope == ValidationScope::Stage => {
                custom::run(registered, scene, ValidatorTarget::Stage, &mut out);
            }
            ValidatorDomain::Stage => {}
            ValidatorDomain::Prim => {
                for &prim in &prims {
                    custom::run(registered, scene, ValidatorTarget::Prim(prim), &mut out);
                }
            }
        }
    }
    out.problems.sort_by(|a, b| {
        a.rule
            .cmp(&b.rule)
            .then_with(|| match (a.site.prim, b.site.prim) {
                (Some(a), Some(b)) => paths.resolve(a).cmp_with_tokens(paths.resolve(b), tokens),
                (a, b) => a.cmp(&b),
            })
            .then_with(|| {
                a.site
                    .property
                    .map(|p| tokens.resolve(p))
                    .cmp(&b.site.property.map(|p| tokens.resolve(p)))
            })
            .then_with(|| match (&a.time, &b.time) {
                (Some(a), Some(b)) => time_order(a, b),
                (a, b) => a.is_some().cmp(&b.is_some()),
            })
            .then_with(|| {
                a.site
                    .source
                    .as_ref()
                    .map(|s| (&s.layer, &s.spec))
                    .cmp(&b.site.source.as_ref().map(|s| (&s.layer, &s.spec)))
            })
            .then_with(|| alloc::format!("{:?}", a.kind).cmp(&alloc::format!("{:?}", b.kind)))
    });
    Ok(out)
}
fn valid_default_prim(scene: &Scene<'_>) -> bool {
    let Some(layer) = scene
        .stage()
        .root_layer()
        .and_then(|id| scene.store().layer(id))
    else {
        return false;
    };
    let Some(name) = layer.default_prim else {
        return false;
    };
    // Reuse canonical Sdf defaultPrim grammar without mutating the scene's interner.
    let mut scratch = layerstack::TokenInterner::default();
    let mut probe = layerstack::Layer::new(layer.id);
    probe.default_prim = Some(scratch.intern(scene.store().tokens().resolve(name)));
    let Some(path) = probe.default_prim_path(&mut scratch) else {
        return false;
    };
    let Some(segments) = path
        .segments()
        .iter()
        .map(|s| scene.store().tokens().lookup(scratch.resolve(*s)))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    scene
        .store()
        .paths()
        .lookup(&Path::root().join(&segments))
        .is_some_and(|p| scene.stage().has_prim(p))
}
fn stage_site(scene: &Scene<'_>) -> ValidationSite {
    ValidationSite {
        prim: None,
        property: None,
        source: scene
            .stage()
            .root_layer()
            .map(|layer| ValidationSource { layer, spec: None }),
    }
}
#[cfg(any(feature = "usd-geom", feature = "usd-shade"))]
fn prim_site(scene: &Scene<'_>, prim: PathId) -> ValidationSite {
    let source = scene
        .stage()
        .prim_stack(prim)
        .and_then(|s| s.into_iter().next())
        .map(|(layer, spec)| ValidationSource {
            layer,
            spec: Some(spec),
        });
    ValidationSite {
        prim: Some(prim),
        property: None,
        source,
    }
}
#[cfg(feature = "usd-shade")]
fn property_site(scene: &Scene<'_>, prim: PathId, property: TokenId) -> ValidationSite {
    let source = scene
        .stage()
        .explain_property_path(PropertyPath::new(prim, property))
        .and_then(|ops| ops.first())
        .map(|op| ValidationSource {
            layer: op.key.layer_id,
            spec: Some(op.key.spec_path.clone()),
        });
    ValidationSite {
        prim: Some(prim),
        property: Some(property),
        source,
    }
}
fn push(
    out: &mut ValidationReport,
    rule: BuiltinRule,
    site: ValidationSite,
    time: Option<Time>,
    kind: ValidationProblemKind,
) {
    out.problems.push(ValidationProblem {
        rule: ValidatorId::Builtin(rule),
        site,
        time,
        kind,
    });
}
fn type_name(t: Option<&PropertyType>) -> Option<AttributeTypeName> {
    t.map(|t| AttributeTypeName {
        name: t.type_name.clone(),
        is_array: t.is_array,
    })
}
fn properties(scene: &Scene<'_>, prim: PathId) -> Vec<TokenId> {
    let mut names = scene.stage().authored_property_names(prim, scene.store());
    names.sort_by(|a, b| {
        scene
            .store()
            .tokens()
            .resolve(*a)
            .cmp(scene.store().tokens().resolve(*b))
    });
    names
}
fn prim_rule(scene: &Scene<'_>, prim: PathId, rule: BuiltinRule, out: &mut ValidationReport) {
    match rule {
        BuiltinRule::AttributeTypes => {
            for property in properties(scene, prim) {
                let Some(decl) = scene.stage().resolve_property_declaration(prim, property) else {
                    continue;
                };
                let schema = scene.stage().property_definition_ref(prim, property);
                let kind = schema.map_or(decl.kind, |s| s.kind);
                if kind != PropertyKind::Attribute {
                    continue;
                }
                let expected = type_name(
                    schema
                        .and_then(|s| s.type_name.as_ref())
                        .or(decl.type_name.as_ref()),
                );
                if let Some(ops) = scene
                    .stage()
                    .explain_property_path(PropertyPath::new(prim, property))
                {
                    for op in ops {
                        if let Some(spec) = op.value.as_property() {
                            out.work.property_specs += 1;
                            let authored = type_name(spec.type_name.as_ref());
                            if authored != expected {
                                push(
                                    out,
                                    rule,
                                    ValidationSite {
                                        prim: Some(prim),
                                        property: Some(property),
                                        source: Some(ValidationSource {
                                            layer: op.key.layer_id,
                                            spec: Some(op.key.spec_path.clone()),
                                        }),
                                    },
                                    None,
                                    ValidationProblemKind::AttributeTypeMismatch {
                                        expected: expected.clone(),
                                        authored,
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }
        #[cfg(feature = "usd-geom")]
        BuiltinRule::MeshTopology => {
            if let Some(mesh) = crate::usd_geom::Mesh::new(scene, prim) {
                for time in out.times.clone() {
                    out.work.mesh_time_evaluations += 1;
                    if let Err(problem) = mesh.validate_topology(time) {
                        push(
                            out,
                            rule,
                            prim_site(scene, prim),
                            Some(time),
                            ValidationProblemKind::MeshTopology(problem),
                        );
                    }
                }
            }
        }
        #[cfg(feature = "usd-geom")]
        BuiltinRule::SubsetFamilies => {
            if let Some(imageable) = crate::usd_geom::Imageable::new(scene, prim) {
                let families: BTreeSet<_> = imageable
                    .geom_subsets(None, None)
                    .iter()
                    .filter_map(|s| {
                        s.family_name()
                            .filter(|f| !f.is_empty())
                            .map(alloc::string::String::from)
                    })
                    .collect();
                for family in families {
                    let members = imageable.geom_subsets(None, Some(&family));
                    let Some(element) = members.first().and_then(|s| s.element_type()) else {
                        push(
                            out,
                            rule,
                            prim_site(scene, prim),
                            None,
                            ValidationProblemKind::SubsetFamily {
                                family: family.clone(),
                                problem: crate::subset::SubsetProblemKind::InvalidGeometry,
                            },
                        );
                        continue;
                    };
                    out.work.subset_families += 1;
                    for problem in imageable.validate_subset_family(&element, &family).problems {
                        push(
                            out,
                            rule,
                            prim_site(scene, problem.path),
                            Some(problem.time),
                            ValidationProblemKind::SubsetFamily {
                                family: family.clone(),
                                problem: problem.kind,
                            },
                        );
                    }
                }
            }
        }
        #[cfg(feature = "usd-geom")]
        BuiltinRule::SubsetParent => {
            if scene.is_a(prim, "GeomSubset")
                && !scene
                    .parent(prim)
                    .is_some_and(|p| scene.is_a(p, "Imageable"))
            {
                push(
                    out,
                    rule,
                    prim_site(scene, prim),
                    None,
                    ValidationProblemKind::SubsetParentNotImageable,
                );
            }
        }
        #[cfg(feature = "usd-geom")]
        BuiltinRule::GeometryEncapsulation if scene.is_a(prim, "Boundable") => {
            let mut parent = scene.parent(prim);
            while let Some(p) = parent {
                if scene.is_a(p, "Gprim") {
                    push(
                        out,
                        rule,
                        prim_site(scene, prim),
                        None,
                        ValidationProblemKind::GprimAncestor(p),
                    );
                    break;
                }
                parent = scene.parent(p);
            }
        }
        #[cfg(feature = "usd-shade")]
        BuiltinRule::MaterialBindingApi
        | BuiltinRule::MaterialBindingPropertyKinds
        | BuiltinRule::ShadingConnections => shade(scene, prim, rule, out),
        #[cfg(all(feature = "usd-shade", feature = "usd-geom"))]
        BuiltinRule::MaterialSubsetFamilies => material_subsets(scene, prim, rule, out),
        _ => (),
    }
}
#[cfg(feature = "usd-shade")]
fn shade(scene: &Scene<'_>, prim: PathId, rule: BuiltinRule, out: &mut ValidationReport) {
    let mut binding_rel = false;
    for property in properties(scene, prim) {
        let name = scene.store().tokens().resolve(property);
        let kind = scene
            .stage()
            .resolve_property_declaration(prim, property)
            .map(|d| d.kind);
        if name.starts_with("material:binding") {
            binding_rel |= kind == Some(PropertyKind::Relationship);
            if rule == BuiltinRule::MaterialBindingPropertyKinds
                && kind != Some(PropertyKind::Relationship)
            {
                push(
                    out,
                    rule,
                    property_site(scene, prim, property),
                    None,
                    ValidationProblemKind::MaterialBindingNotRelationship,
                );
            }
        }
        if rule != BuiltinRule::ShadingConnections
            || !(name.starts_with("inputs:") || name.starts_with("outputs:"))
            || kind != Some(PropertyKind::Attribute)
        {
            continue;
        }
        // Restrict to C++'s built-in shading families, not arbitrary custom ports.
        if !(scene.is_a(prim, "Shader")
            || scene.is_a(prim, "NodeGraph")
            || scene.is_a(prim, "Material"))
        {
            continue;
        }
        let destination = PropertyPath::new(prim, property);
        if let Some(targets) = scene.stage().resolve_target_list_path(destination) {
            for source in targets.value {
                out.work.connections += 1;
                let problem = match source {
                    layerstack::TargetPath::Property(source) => scene
                        .validate_shading_connection(destination, source)
                        .err()
                        .map(ValidationProblemKind::ShadingConnection),
                    target => Some(ValidationProblemKind::ShadingSourceNotProperty(target)),
                };
                if let Some(problem) = problem {
                    push(
                        out,
                        rule,
                        property_site(scene, prim, property),
                        None,
                        problem,
                    );
                }
            }
        }
    }
    if rule == BuiltinRule::MaterialBindingApi
        && binding_rel
        && !scene.has_api(prim, "MaterialBindingAPI", None)
    {
        push(
            out,
            rule,
            prim_site(scene, prim),
            None,
            ValidationProblemKind::MissingMaterialBindingApi,
        );
    }
}
#[cfg(all(feature = "usd-shade", feature = "usd-geom"))]
fn material_subsets(
    scene: &Scene<'_>,
    prim: PathId,
    rule: BuiltinRule,
    out: &mut ValidationReport,
) {
    if let Some(subset) = crate::usd_geom::GeomSubset::new(scene, prim) {
        let bound = properties(scene, prim).into_iter().any(|p| {
            scene
                .store()
                .tokens()
                .resolve(p)
                .starts_with("material:binding")
                && scene
                    .stage()
                    .resolve_property_declaration(prim, p)
                    .is_some_and(|d| d.kind == PropertyKind::Relationship)
        });
        // C++ checks authored presence, not equality to materialBind.
        if bound && !subset.has_authored_value("familyName") {
            push(
                out,
                rule,
                prim_site(scene, prim),
                None,
                ValidationProblemKind::MissingMaterialSubsetFamilyName,
            );
        }
    }
    if let Some(imageable) = crate::usd_geom::Imageable::new(scene, prim)
        && !imageable
            .geom_subsets(None, Some("materialBind"))
            .is_empty()
        && imageable.subset_family_type("materialBind") == "unrestricted"
    {
        push(
            out,
            rule,
            prim_site(scene, prim),
            None,
            ValidationProblemKind::UnrestrictedMaterialSubsetFamily,
        );
    }
}

fn composition_site(scene: &Scene<'_>, error: &CompositionError) -> ValidationSite {
    let mut site = ValidationSite {
        prim: error.prim(),
        property: None,
        source: None,
    };
    let layer = match error {
        CompositionError::SublayerCycle(e) => Some(e.layer),
        CompositionError::UnresolvedSublayer(e) => Some(e.layer),
        CompositionError::InvalidAuthoredRelocation(e) => Some(e.layer),
        CompositionError::InvalidConflictingRelocation(e) => Some(e.layer),
        CompositionError::VariableExpressionError(e) => Some(e.layer),
        CompositionError::OpinionAtRelocationSource(e) => {
            site.source = Some(ValidationSource {
                layer: e.layer,
                spec: Some(SpecPath::from_prim_path(e.path, scene.store().paths())),
            });
            None
        }
        CompositionError::InconsistentPropertyType(e) => {
            site.property = Some(e.property);
            site.source = Some(ValidationSource {
                layer: e.conflicting_layer,
                spec: Some(e.conflicting_spec.clone()),
            });
            None
        }
        CompositionError::InvalidExternalTargetPath(e) => {
            site.property = Some(e.property);
            site.source = Some(ValidationSource {
                layer: e.layer,
                spec: Some(e.spec.clone()),
            });
            None
        }
        CompositionError::InvalidInstanceTargetPath(e) => {
            site.property = Some(e.property);
            site.source = Some(ValidationSource {
                layer: e.layer,
                spec: Some(e.spec.clone()),
            });
            None
        }
        _ => None,
    };
    if let Some(layer) = layer {
        site.source = Some(ValidationSource { layer, spec: None });
    }
    site
}
