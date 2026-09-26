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
