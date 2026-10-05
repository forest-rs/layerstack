// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{string::String, vec::Vec};

use super::*;

fn attr(tokens: &mut TokenInterner, name: &str, fallback: impl Into<Value>) -> PropertyDefinition {
    PropertyDefinition::attribute(tokens.intern(name)).with_fallback(fallback)
}

fn applied_names(definition: &PrimDefinition, tokens: &TokenInterner) -> Vec<String> {
    definition
        .applied_schemas()
        .iter()
        .map(|applied| String::from(tokens.resolve(applied.name)))
        .collect()
}

/// The fallback `name` resolves to, through both lookups, which must agree.
fn fallback(
    registry: &SchemaRegistry,
    type_name: Option<TokenId>,
    applied: &[TokenId],
    name: &str,
    tokens: &mut TokenInterner,
) -> Option<Value> {
    registry.intern_instance_names(applied, tokens);
    let property = tokens.intern(name);
    let found = registry.property_definition(type_name, applied, property, tokens);
    let definition = registry.prim_definition(type_name, applied, tokens);
    assert_eq!(found.as_ref(), definition.property(property), "{name}");
    found.and_then(|p| p.fallback)
}

#[test]
fn typed_schemas_inherit_properties_and_is_a() {
    let mut t = TokenInterner::default();
    let (foo, bar, baz) = (t.intern("Foo"), t.intern("Bar"), t.intern("Baz"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::new(foo, SchemaKind::AbstractTyped)
                .with_property(attr(&mut t, "fooprop", 0)),
        )
        .register(
            SchemaDefinition::typed(bar)
                .with_parent(foo)
                .with_property(attr(&mut t, "barprop", 1.0_f32)),
        )
        .register(SchemaDefinition::typed(baz).with_parent(foo));
    let registry = builder.build(&mut t);

    // Spec: AOUSD Core §13.3.1's example.
    let bar1 = registry.prim_definition(Some(bar), &[], &t);
    assert_eq!(bar1.type_name(), Some(bar));
    assert!(bar1.is_a(bar) && bar1.is_a(foo) && !bar1.is_a(baz));
    let baz1 = registry.prim_definition(Some(baz), &[], &t);
    assert!(baz1.is_a(baz) && baz1.is_a(foo) && !baz1.is_a(bar));
    assert_eq!(
        fallback(&registry, Some(bar), &[], "fooprop", &mut t),
        Some(Value::Int(0))
    );
    assert_eq!(fallback(&registry, Some(baz), &[], "barprop", &mut t), None);

    // An abstract or unknown type name is typeless.
    let unknown = t.intern("Unknown");
    for type_name in [foo, unknown] {
        let definition = registry.prim_definition(Some(type_name), &[], &t);
        assert_eq!(definition.type_name(), None);
        assert!(!definition.is_a(foo));
        assert!(definition.properties().is_empty());
    }
    assert!(registry.is_a(bar, foo));
    assert!(registry.issues().is_empty());
}

#[test]
fn inclusions_compose_right_after_their_schema() {
    // Spec: AOUSD Core §13.3.2.3's order: each schema's built-ins and
    // auto-applies follow it, before the next authored schema.
    let mut t = TokenInterner::default();
    let names = [
        "Tile", "LabelAPI", "StyleAPI", "ColorAPI", "OtherAPI", "AutoAPI",
    ];
    let [tile, label, style, color, other, auto] = names.map(|n| t.intern(n));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(tile))
        .register(
            SchemaDefinition::api(label)
                .with_built_in(style)
                .with_property(attr(&mut t, "text", "label")),
        )
        .register(
            SchemaDefinition::api(style)
                .with_built_in(color)
                .with_property(PropertyDefinition::attribute(t.intern("note"))),
        )
        .register(
            SchemaDefinition::api(color)
                .with_property(attr(&mut t, "note", "color"))
                .with_property(attr(&mut t, "shade", "color")),
        )
        .register(SchemaDefinition::api(other).with_property(attr(&mut t, "shade", "other")))
        .register(SchemaDefinition::api(auto).with_property(attr(&mut t, "text", "auto")))
        .auto_apply(auto, label);
    let registry = builder.build(&mut t);

    let applied = [label, other];
    let definition = registry.prim_definition(Some(tile), &applied, &t);
    assert_eq!(
        applied_names(&definition, &t),
        ["LabelAPI", "StyleAPI", "ColorAPI", "AutoAPI", "OtherAPI"]
    );
    // `ColorAPI`, nested two deep in `LabelAPI`, outranks `OtherAPI`.
    let shade = fallback(&registry, Some(tile), &applied, "shade", &mut t);
    assert_eq!(shade, Some(Value::from("color")));
    // `StyleAPI` defines `note` without a fallback; `ColorAPI` fills it in.
    let note = fallback(&registry, Some(tile), &applied, "note", &mut t);
    assert_eq!(note, Some(Value::from("color")));
    let text = fallback(&registry, Some(tile), &applied, "text", &mut t);
    assert_eq!(text, Some(Value::from("label")));
}

#[test]
fn overrides_replace_fallbacks_but_not_variability() {
    // Spec: AOUSD Core §13.3.2.2.
    let mut t = TokenInterner::default();
    let (outer, inner) = (t.intern("OuterAPI"), t.intern("InnerAPI"));
    let size = t.intern("size");
    let int = PropertyType::new("int", false, Value::Int(0));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::api(outer)
                .with_built_in(inner)
                .with_override(
                    PropertyDefinition::attribute(size)
                        .with_type(int.clone())
                        .with_fallback(20)
                        .uniform(),
                )
                .with_override(attr(&mut t, "ghost", 1)),
        )
        .register(
            SchemaDefinition::api(inner).with_property(
                PropertyDefinition::attribute(size)
                    .with_type(int)
                    .with_fallback(12),
            ),
        );
    let registry = builder.build(&mut t);

    let definition = registry.prim_definition(None, &[outer], &t);
    let size = definition.property(size).expect("size");
    assert_eq!(size.fallback, Some(Value::Int(20)));
    assert_eq!(size.variability, Variability::Varying);
    let ghost = t.intern("ghost");
    assert!(definition.property(ghost).is_none());
    assert_eq!(
        registry.issues(),
        [SchemaIssue::IgnoredOverride {
            schema: outer,
            property: ghost
        }]
    );
}

#[test]
fn multiple_apply_instances_fill_in_templates() {
    // Spec: AOUSD Core §13.3.2 (instance names, which may contain `:`) and
    // §13.3.2.1 (built-ins by type and by named instance).
    let mut t = TokenInterner::default();
    let (tile, slot, pin) = (t.intern("Tile"), t.intern("SlotAPI"), t.intern("PinAPI"));
    let (pin_extra, slot_main) = (t.intern("PinAPI:extra"), t.intern("SlotAPI:main"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(tile).with_built_in(slot_main))
        .register(
            SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi)
                .with_built_in(pin)
                .with_built_in(pin_extra)
                .with_property(attr(&mut t, "slot:__INSTANCE_NAME__:index", 0)),
        )
        .register(
            SchemaDefinition::new(pin, SchemaKind::MultipleApplyApi)
                .with_property(attr(&mut t, "pin:__INSTANCE_NAME__:offset", 0.25_f32))
                .with_property(attr(&mut t, "__INSTANCE_NAME__:pinned", "no")),
        );
    let registry = builder.build(&mut t);

    let applied = [t.intern("SlotAPI:left:upper"), slot, t.intern("TileAPI")];
    registry.intern_instance_names(&applied, &mut t);
    let definition = registry.prim_definition(Some(tile), &applied, &t);
    assert_eq!(
        applied_names(&definition, &t),
        [
            "SlotAPI:main",
            "PinAPI:main",
            "PinAPI:main:extra",
            "SlotAPI:left:upper",
            "PinAPI:left:upper",
            "PinAPI:left:upper:extra",
        ]
    );
    assert!(definition.has_api(pin));
    let left_upper = t.intern("left:upper");
    assert!(definition.has_api_instance(slot, left_upper));
    assert!(!definition.has_api_instance(pin, t.intern("left")));
    for (name, value) in [
        ("slot:left:upper:index", Value::Int(0)),
        ("pin:left:upper:extra:offset", Value::Float(0.25)),
        ("left:upper:extra:pinned", Value::from("no")),
        ("main:pinned", Value::from("no")),
    ] {
        assert_eq!(
            fallback(&registry, Some(tile), &applied, name, &mut t),
            Some(value),
            "{name}"
        );
    }
    assert_eq!(
        fallback(&registry, Some(tile), &applied, "slot:right:index", &mut t),
        None
    );

    // The template itself keeps the placeholder.
    let template = registry.schema_definition(slot).expect("SlotAPI");
    assert_eq!(
        applied_names(template, &t),
        [
            "SlotAPI:__INSTANCE_NAME__",
            "PinAPI:__INSTANCE_NAME__",
            "PinAPI:__INSTANCE_NAME__:extra"
        ]
    );
}

#[test]
fn invalid_inclusions_are_skipped_and_reported() {
    let mut t = TokenInterner::default();
    let (single, multi) = (t.intern("SingleAPI"), t.intern("MultiAPI"));
    let (bad_single, missing) = (t.intern("SingleAPI:named"), t.intern("MissingAPI"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::api(single).with_built_in(multi))
        .register(
            SchemaDefinition::new(multi, SchemaKind::MultipleApplyApi)
                .with_built_in(bad_single)
                .with_built_in(missing),
        );
    let registry = builder.build(&mut t);
    let issues = registry.issues();
    assert!(issues.contains(&SchemaIssue::InvalidInclusion {
        schema: single,
        included: multi
    }));
    assert!(issues.contains(&SchemaIssue::InvalidInclusion {
        schema: multi,
        included: bad_single
    }));
    assert!(issues.contains(&SchemaIssue::UnknownInclusion {
        schema: multi,
        included: missing
    }));
    let definition = registry.prim_definition(None, &[single], &t);
    assert_eq!(applied_names(&definition, &t), ["SingleAPI"]);
}

#[test]
fn inclusion_cycles_build_each_definition_from_the_top() {
    let mut t = TokenInterner::default();
    let (one, two) = (t.intern("OneAPI"), t.intern("TwoAPI"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::api(one)
                .with_built_in(two)
                .with_property(attr(&mut t, "value", 1)),
        )
        .register(
            SchemaDefinition::api(two)
                .with_built_in(one)
                .with_property(attr(&mut t, "value", 2)),
        );
    let registry = builder.build(&mut t);
    let from_one = registry.prim_definition(None, &[one], &t);
    assert_eq!(applied_names(&from_one, &t), ["OneAPI", "TwoAPI"]);
    let from_two = registry.prim_definition(None, &[two], &t);
    assert_eq!(applied_names(&from_two, &t), ["TwoAPI", "OneAPI"]);
    assert_eq!(
        fallback(&registry, None, &[two], "value", &mut t),
        Some(Value::Int(2))
    );
    assert!(
        registry
            .issues()
            .iter()
            .all(|issue| matches!(issue, SchemaIssue::InclusionCycle { .. }))
    );
}

#[test]
fn auto_applies_reach_derived_types_in_reverse_dictionary_order() {
    // OpenUSD orders a type's auto-applied schemas in reverse dictionary
    // order (`Usd_SortAutoAppliedAPISchemas`).
    let mut t = TokenInterner::default();
    let (shape, tile) = (t.intern("Shape"), t.intern("Tile"));
    let (first, second) = (t.intern("AutoFirstAPI"), t.intern("AutoSecondAPI"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::new(shape, SchemaKind::AbstractTyped))
        .register(SchemaDefinition::typed(tile).with_parent(shape))
        .register(SchemaDefinition::api(first).with_property(attr(&mut t, "enabled", true)))
        .register(SchemaDefinition::api(second).with_property(attr(&mut t, "enabled", false)))
        .auto_apply(first, shape)
        .auto_apply(second, shape);
    let registry = builder.build(&mut t);
    let definition = registry.prim_definition(Some(tile), &[], &t);
    assert_eq!(
        applied_names(&definition, &t),
        ["AutoSecondAPI", "AutoFirstAPI"]
    );
    assert_eq!(
        fallback(&registry, Some(tile), &[], "enabled", &mut t),
        Some(Value::Bool(false))
    );
}

#[test]
fn a_weaker_definition_of_another_type_is_ignored() {
    let mut t = TokenInterner::default();
    let (strong, weak) = (t.intern("StrongAPI"), t.intern("WeakAPI"));
    let rank = t.intern("rank");
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::api(strong).with_property(
            PropertyDefinition::attribute(rank).with_type(PropertyType::new(
                "int",
                false,
                Value::Int(0),
            )),
        ))
        .register(
            SchemaDefinition::api(weak).with_property(
                PropertyDefinition::attribute(rank)
                    .with_type(PropertyType::new("float", false, Value::Float(0.0)))
                    .with_fallback(2.0_f32),
            ),
        );
    let registry = builder.build(&mut t);
    assert_eq!(
        fallback(&registry, None, &[strong, weak], "rank", &mut t),
        None
    );
    assert_eq!(
        fallback(&registry, None, &[weak, strong], "rank", &mut t),
        Some(Value::Float(2.0))
    );
}

/// Building a definition never interns: a multiple-apply instance whose
/// names were not interned first is a bug the debug build reports.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "was never interned")]
fn uninterned_instance_names_are_reported() {
    let mut t = TokenInterner::default();
    let slot = t.intern("SlotAPI");
    let mut builder = SchemaRegistry::builder();
    builder.register(
        SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi).with_property(attr(
            &mut t,
            "slot:__INSTANCE_NAME__:index",
            0,
        )),
    );
    let registry = builder.build(&mut t);
    let applied = [t.intern("SlotAPI:fresh")];
    let _ = registry.prim_definition(None, &applied, &t);
}

/// A single-property lookup finds exactly the names the built definition
/// has: never a template name, nor a name one segment off an instance's.
#[test]
fn property_lookup_matches_the_built_definition() {
    let mut t = TokenInterner::default();
    let [tile, slot, pin, label] = ["Tile", "SlotAPI", "PinAPI", "LabelAPI"].map(|n| t.intern(n));
    let pin_extra = t.intern("PinAPI:extra");
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(tile).with_built_in(label))
        .register(SchemaDefinition::api(label).with_property(attr(&mut t, "label", "x")))
        .register(
            SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi)
                .with_built_in(pin)
                .with_built_in(pin_extra)
                .with_property(attr(&mut t, "slot:__INSTANCE_NAME__:index", 3))
                .with_property(attr(&mut t, "slot:__INSTANCE_NAME__", 1)),
        )
        .register(
            SchemaDefinition::new(pin, SchemaKind::MultipleApplyApi).with_property(attr(
                &mut t,
                "__INSTANCE_NAME__:pinned",
                2,
            )),
        );
    let registry = builder.build(&mut t);
    let applied = [t.intern("SlotAPI:left"), t.intern("SlotAPI:a:b")];
    registry.intern_instance_names(&applied, &mut t);
    let definition = registry.prim_definition(Some(tile), &applied, &t);

    let mut names: Vec<String> = definition
        .properties()
        .iter()
        .map(|p| String::from(t.resolve(p.name)))
        .collect();
    for schema in [slot, pin] {
        let template = registry.schema_definition(schema).expect("template");
        names.extend(
            template
                .properties()
                .iter()
                .map(|p| String::from(t.resolve(p.name))),
        );
    }
    let near: Vec<String> = names
        .iter()
        .flat_map(|name| {
            [
                alloc::format!("{name}:x"),
                alloc::format!("x:{name}"),
                name.replace("left", "right"),
                name.replace("a:b", "a"),
                name.replace("a:b", "b"),
            ]
        })
        .collect();
    names.extend(near);
    for name in names {
        let property = t.intern(&name);
        assert_eq!(
            registry
                .property_definition(Some(tile), &applied, property, &t)
                .as_ref(),
            definition.property(property),
            "{name}"
        );
    }
    let template = t.intern("slot:__INSTANCE_NAME__:index");
    assert!(definition.property(template).is_none());
    assert!(
        registry
            .property_definition(Some(tile), &applied, template, &t)
            .is_none()
    );
    let left_index = t.intern("slot:left:index");
    assert_eq!(
        registry
            .property_definition(Some(tile), &applied, left_index, &t)
            .and_then(|p| p.fallback),
        Some(Value::Int(3))
    );
}

#[test]
fn generated_schema_layers_read_as_definitions() {
    use crate::{
        doc::{FieldValue, Layer, LayerId, PrimSpec},
        listop::ListOp,
        path::{Path, PathInterner},
        property::PropertySpec,
    };
    use alloc::sync::Arc;

    let mut t = TokenInterner::default();
    let mut paths = PathInterner::default();
    let (slot, pin, tile) = (t.intern("SlotAPI"), t.intern("PinAPI"), t.intern("Tile"));
    let [by_type, by_instance] =
        ["PinAPI:__INSTANCE_NAME__", "PinAPI:__INSTANCE_NAME__:extra"].map(|n| t.intern(n));
    let (offset, index) = (
        t.intern("pin:__INSTANCE_NAME__:offset"),
        t.intern("slot:__INSTANCE_NAME__:index"),
    );
    let (api_schemas, custom_data) = (t.intern("apiSchemas"), t.intern("customData"));
    let float = PropertyType::new("float", false, Value::Float(0.0));

    let mut layer = Layer::new(LayerId(1));
    let slot_path = paths.intern(Path::root().join(&[slot]));
    layer.insert_prim(
        slot_path,
        PrimSpec::class()
            .with_field(
                api_schemas,
                FieldValue::TokenListOp(ListOp::explicit(vec![by_type, by_instance])),
            )
            .with_field(
                custom_data,
                Value::Dictionary(vec![(
                    Arc::from("apiSchemaOverridePropertyNames"),
                    Value::Array(vec![Value::Token(offset)]),
                )]),
            )
            .with_property(
                offset,
                PropertySpec::typed_attribute(float.clone())
                    .uniform()
                    .with_default(Value::Float(0.5)),
            )
            .with_property(
                index,
                PropertySpec::typed_attribute(PropertyType::new("int", false, Value::Int(0)))
                    .with_default(Value::Int(0)),
            ),
    );
    let pin_path = paths.intern(Path::root().join(&[pin]));
    layer.insert_prim(
        pin_path,
        PrimSpec::class().with_property(
            offset,
            PropertySpec::typed_attribute(float).with_default(Value::Float(0.25)),
        ),
    );

    let main = t.intern("main");
    let declared = [
        SchemaDeclaration {
            allowed_instance_names: vec![main],
            ..SchemaDeclaration::new(slot, SchemaKind::MultipleApplyApi)
        },
        SchemaDeclaration::new(pin, SchemaKind::MultipleApplyApi),
    ];
    let schemas = read_generated_schema(&layer, &declared, &mut t, &paths).expect("read");
    let slot_schema = &schemas[0];
    assert_eq!(slot_schema.built_ins, [pin, t.intern("PinAPI:extra")]);
    assert_eq!(
        slot_schema
            .properties
            .iter()
            .map(|p| p.name)
            .collect::<Vec<_>>(),
        [index]
    );
    assert_eq!(slot_schema.overrides[0].name, offset);
    assert_eq!(slot_schema.allowed_instance_names, [main]);

    let mut builder = SchemaRegistry::builder();
    for schema in schemas {
        builder.register(schema);
    }
    let registry = builder.build(&mut t);
    let applied = [t.intern("SlotAPI:a")];
    assert_eq!(
        fallback(&registry, None, &applied, "pin:a:offset", &mut t),
        Some(Value::Float(0.5))
    );
    assert_eq!(
        fallback(&registry, None, &applied, "pin:a:extra:offset", &mut t),
        Some(Value::Float(0.25))
    );

    let missing = [SchemaDeclaration::new(tile, SchemaKind::ConcreteTyped)];
    assert_eq!(
        read_generated_schema(&layer, &missing, &mut t, &paths),
        Err(GeneratedSchemaError::MissingSchema { name: tile })
    );
}

/// Where an applied schema may be applied, and with which instance names.
///
/// OpenUSD: `UsdPrim::CanApplyAPI`,
/// `UsdSchemaRegistry::IsAllowedAPISchemaInstanceName`.
#[test]
fn can_apply_checks_instance_names_and_prim_types() {
    let mut t = TokenInterner::default();
    let [shape, tile, other, label, slot, pin] =
        ["Shape", "Tile", "Other", "LabelAPI", "SlotAPI", "PinAPI"].map(|n| t.intern(n));
    let (main, right) = (t.intern("main"), t.intern("right"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::new(shape, SchemaKind::AbstractTyped))
        .register(SchemaDefinition::typed(tile).with_parent(shape))
        .register(SchemaDefinition::typed(other))
        .register(SchemaDefinition::api(label).with_can_only_apply_to(shape))
        .register(SchemaDefinition {
            instance_can_only_apply_to: vec![(right, vec![other]), (main, Vec::new())],
            ..SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi)
                .with_allowed_instance_name(main)
                .with_allowed_instance_name(right)
        })
        .register(
            SchemaDefinition::new(pin, SchemaKind::MultipleApplyApi)
                .with_property(attr(&mut t, "pin:__INSTANCE_NAME__:offset", 0))
                .with_property(attr(&mut t, "pin:__INSTANCE_NAME__", 0)),
        );
    let registry = builder.build(&mut t);

    assert_eq!(registry.can_apply(Some(tile), label, None, &t), Ok(()));
    assert_eq!(
        registry.can_apply(Some(other), label, None, &t),
        Err(CannotApply::PrimType {
            allowed: vec![shape]
        })
    );
    assert!(registry.can_apply(None, label, None, &t).is_err());
    assert_eq!(
        registry.can_apply(Some(tile), label, Some("x"), &t),
        Err(CannotApply::UnexpectedInstanceName)
    );
    assert_eq!(
        registry.can_apply(Some(tile), slot, None, &t),
        Err(CannotApply::MissingInstanceName)
    );
    assert_eq!(
        registry.can_apply(Some(tile), tile, None, &t),
        Err(CannotApply::NotAnAppliedSchema)
    );

    // Allowed instance names, and a per-instance type list.
    assert_eq!(
        registry.can_apply(Some(tile), slot, Some("main"), &t),
        Ok(())
    );
    // An empty instance list leaves the schema's in force; so does none.
    let limited = t.intern("LimitedAPI");
    let (open, empty) = (t.intern("open"), t.intern("empty"));
    let mut limits = SchemaRegistry::builder();
    limits
        .register(SchemaDefinition::new(shape, SchemaKind::AbstractTyped))
        .register(SchemaDefinition::typed(tile).with_parent(shape))
        .register(SchemaDefinition {
            instance_can_only_apply_to: vec![(empty, Vec::new()), (open, vec![shape])],
            ..SchemaDefinition::new(limited, SchemaKind::MultipleApplyApi)
                .with_can_only_apply_to(tile)
        });
    let limits = limits.build(&mut t);
    for instance in ["empty", "missing"] {
        assert_eq!(
            limits.can_only_apply_to(limited, Some(instance), &t),
            [tile]
        );
        assert!(limits.can_apply(None, limited, Some(instance), &t).is_err());
        assert_eq!(
            limits.can_apply(Some(tile), limited, Some(instance), &t),
            Ok(())
        );
    }
    assert_eq!(limits.can_only_apply_to(limited, Some("open"), &t), [shape]);
    assert_eq!(
        registry.can_apply(Some(tile), slot, Some("left"), &t),
        Err(CannotApply::InstanceNameNotAllowed)
    );
    assert!(
        registry
            .can_apply(Some(tile), slot, Some("right"), &t)
            .is_err()
    );
    assert_eq!(
        registry.can_apply(Some(other), slot, Some("right"), &t),
        Ok(())
    );
    assert_eq!(registry.can_only_apply_to(slot, Some("right"), &t), [other]);
    assert!(
        registry
            .can_only_apply_to(slot, Some("main"), &t)
            .is_empty()
    );

    // An instance's last identifier may not be a property's base name.
    for (instance, allowed) in [
        ("free", true),
        ("a:b", true),
        ("offset", false),
        ("x:offset", false),
        ("offset:x", true),
        ("", false),
        ("1st", false),
        ("a::b", false),
        ("é", true),
        ("℘", true),
        ("a²", false),
        ("x:ⅰ", true),
        ("a\u{0301}", true),
        ("\u{0301}a", false),
    ] {
        assert_eq!(
            registry.is_allowed_instance_name(pin, instance, &t),
            allowed,
            "{instance:?}"
        );
    }
    assert!(!registry.is_allowed_instance_name(label, "free", &t));
}

#[test]
fn property_metadata_composes_strong_to_weak_only_for_matching_types() {
    use crate::{FieldValue, PropertyType};
    let mut tokens = TokenInterner::default();
    let strong = tokens.intern("StrongAPI");
    let weak = tokens.intern("WeakAPI");
    let wrong = tokens.intern("WrongAPI");
    let name = tokens.intern("tint");
    let color = tokens.intern("colorSpace");
    let empty = tokens.intern("");
    let group = tokens.intern("displayGroup");
    let docs = tokens.intern("documentation");
    let dict = tokens.intern("settings");
    let only_wrong = tokens.intern("wrongTypeOnly");
    let float = PropertyType::new("float", false, Value::Float(0.0));
    let dictionary = |items: &[(&str, i32)]| {
        Value::Dictionary(vec![(
            "nested".into(),
            Value::Dictionary(
                items
                    .iter()
                    .map(|(key, value)| ((*key).into(), Value::Int(*value)))
                    .collect(),
            ),
        )])
    };
    let mut builder = SchemaRegistry::builder();
    builder.register(
        SchemaDefinition::new(strong, SchemaKind::SingleApplyApi).with_property(
            PropertyDefinition::attribute(name)
                .with_type(float.clone())
                .with_metadata(color, Value::Token(empty))
                .with_metadata(dict, dictionary(&[("strong", 1), ("shared", 1)])),
        ),
    );
    builder.register(
        SchemaDefinition::new(weak, SchemaKind::SingleApplyApi).with_property(
            PropertyDefinition::attribute(name)
                .with_type(float)
                .with_metadata(color, Value::string("weak"))
                .with_metadata(group, Value::string("Weak group"))
                .with_metadata(docs, Value::string("Not inherited from weaker API"))
                .with_metadata(dict, dictionary(&[("weak", 2), ("shared", 2)])),
        ),
    );
    builder.register(
        SchemaDefinition::new(wrong, SchemaKind::SingleApplyApi).with_property(
            PropertyDefinition::attribute(name)
                .with_type(PropertyType::new("int", false, Value::Int(0)))
                .with_metadata(only_wrong, Value::Bool(true)),
        ),
    );
    let registry = builder.build(&mut tokens);
    let applied = [strong, weak, wrong];
    let definition = registry.prim_definition(None, &applied, &tokens);
    let property = definition.property(name).unwrap();
    assert_eq!(
        Some(property),
        registry
            .property_definition(None, &applied, name, &tokens)
            .as_ref()
    );
    assert_eq!(
        property.metadata(color),
        Some(&FieldValue::Value(Value::Token(empty)))
    );
    assert_eq!(
        property.metadata(group),
        Some(&FieldValue::Value(Value::string("Weak group")))
    );
    assert!(property.metadata(docs).is_none());
    assert!(property.metadata(only_wrong).is_none());
    assert_eq!(
        property.metadata(dict),
        Some(&FieldValue::Value(dictionary(&[
            ("shared", 1),
            ("strong", 1),
            ("weak", 2)
        ])))
    );
}

#[test]
fn property_metadata_override_replaces_dictionary_and_preserves_variability() {
    let mut tokens = TokenInterner::default();
    let strong = tokens.intern("StrongAPI");
    let weak = tokens.intern("WeakAPI");
    let name = tokens.intern("size");
    let dict = tokens.intern("settings");
    let dictionary = |key: &str| Value::Dictionary(vec![(key.into(), Value::Int(1))]);
    let float = PropertyType::new("float", false, Value::Float(0.0));
    let mut builder = SchemaRegistry::builder();
    builder.register(
        SchemaDefinition::new(weak, SchemaKind::SingleApplyApi).with_property(
            PropertyDefinition::attribute(name)
                .with_type(float.clone())
                .uniform()
                .with_metadata(dict, dictionary("weak")),
        ),
    );
    builder.register(
        SchemaDefinition::new(strong, SchemaKind::SingleApplyApi)
            .with_built_in(weak)
            .with_override(
                PropertyDefinition::attribute(name)
                    .with_type(float)
                    .with_metadata(dict, dictionary("override")),
            ),
    );
    let registry = builder.build(&mut tokens);
    let property = registry
        .schema_definition(strong)
        .unwrap()
        .property(name)
        .unwrap();
    assert_eq!(property.variability, Variability::Uniform);
    assert_eq!(
        property.metadata(dict),
        Some(&FieldValue::Value(dictionary("override")))
    );
}

#[test]
fn stage_property_metadata_fallback_merges_dictionaries_and_respects_blocks() {
    use crate::{
        InMemoryStore, Layer, LayerId, PrimSpec, PropertySpec, ResolvedValue, Stage, StageOptions,
    };
    let mut store = InMemoryStore::default();
    let prim = store.path("/Prim");
    let schema = store.tokens.intern("MetadataPrim");
    let name = store.tokens.intern("value");
    let blocked = store.tokens.intern("blocked");
    let custom = store.tokens.intern("customProperty");
    let settings = store.tokens.intern("settings");
    let dictionary = |items: &[(&str, i32)]| {
        Value::Dictionary(
            items
                .iter()
                .map(|(key, value)| ((*key).into(), Value::Int(*value)))
                .collect(),
        )
    };
    let mut builder = SchemaRegistry::builder();
    builder.register(
        SchemaDefinition::new(schema, SchemaKind::ConcreteTyped)
            .with_property(
                PropertyDefinition::attribute(name)
                    .with_metadata(settings, dictionary(&[("a", 1), ("b", 2)])),
            )
            .with_property(
                PropertyDefinition::attribute(blocked)
                    .with_metadata(settings, Value::string("fallback")),
            ),
    );
    let registry = Arc::new(builder.build(&mut store.tokens));
    let mut layer = Layer::new(LayerId(1));
    layer.insert_prim(
        prim,
        PrimSpec::def()
            .with_type_name(schema)
            .with_property(
                name,
                PropertySpec::attribute().with_metadata(settings, dictionary(&[("a", 9)])),
            )
            .with_property(
                blocked,
                PropertySpec::attribute().with_metadata(settings, Value::Blocked),
            )
            .with_property(
                custom,
                PropertySpec::attribute().with_metadata(settings, Value::string("authored only")),
            ),
    );
    store.insert_layer(layer);
    let stage = Stage::compose(
        &mut store,
        LayerId(1),
        StageOptions {
            schemas: Some(registry),
            ..StageOptions::default()
        },
    );
    assert_eq!(
        stage
            .resolve_property_metadata(prim, name, settings)
            .unwrap()
            .value,
        ResolvedValue::Dictionary(vec![
            ("a".into(), Value::Int(9)),
            ("b".into(), Value::Int(2))
        ])
    );
    assert_eq!(
        stage
            .resolve_authored_property_metadata(prim, name, settings)
            .unwrap()
            .value,
        ResolvedValue::Dictionary(vec![("a".into(), Value::Int(9))])
    );
    assert!(
        stage
            .resolve_property_metadata(prim, blocked, settings)
            .is_none()
    );
    assert_eq!(
        stage
            .resolve_property_metadata(prim, custom, settings)
            .unwrap()
            .value,
        ResolvedValue::Scalar(Value::string("authored only"))
    );
}
