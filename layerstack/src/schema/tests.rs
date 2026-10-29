// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, string::String, vec::Vec};

use super::*;

fn attr(tokens: &TokenInterner, name: &str, fallback: impl Into<Value>) -> PropertyDefinition {
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
    tokens: &TokenInterner,
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
    let t = TokenInterner::default();
    let (foo, bar, baz) = (t.intern("Foo"), t.intern("Bar"), t.intern("Baz"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::new(foo, SchemaKind::AbstractTyped)
                .with_property(attr(&t, "fooprop", 0)),
        )
        .register(
            SchemaDefinition::typed(bar)
                .with_parent(foo)
                .with_property(attr(&t, "barprop", 1.0_f32)),
        )
        .register(SchemaDefinition::typed(baz).with_parent(foo));
    let registry = builder.build(&t);

    // Spec: AOUSD Core §13.3.1's example.
    let bar1 = registry.prim_definition(Some(bar), &[], &t);
    assert_eq!(bar1.type_name(), Some(bar));
    assert!(bar1.is_a(bar) && bar1.is_a(foo) && !bar1.is_a(baz));
    let baz1 = registry.prim_definition(Some(baz), &[], &t);
    assert!(baz1.is_a(baz) && baz1.is_a(foo) && !baz1.is_a(bar));
    assert_eq!(
        fallback(&registry, Some(bar), &[], "fooprop", &t),
        Some(Value::Int(0))
    );
    assert_eq!(fallback(&registry, Some(baz), &[], "barprop", &t), None);

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

/// A flattened definition, as a generated schema layer holds one, is the
/// whole prim definition: OpenUSD composes nothing of its ancestors in. A
/// hand-built one composes its parent chain up to a flattened ancestor.
#[test]
fn flattened_definitions_compose_no_ancestor() {
    let t = TokenInterner::default();
    let [base, slim, mid, leaf, label] =
        ["Base", "Slim", "Mid", "Leaf", "LabelAPI"].map(|n| t.intern(n));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::new(base, SchemaKind::AbstractTyped)
                .with_property(attr(&t, "base:mass", 1))
                .with_built_in(label),
        )
        .register(SchemaDefinition::api(label).with_property(attr(&t, "label:text", "")))
        .register(
            SchemaDefinition::typed(slim)
                .with_parent(base)
                .flattened()
                .with_property(attr(&t, "slim:x", 1)),
        )
        .register(
            SchemaDefinition::new(mid, SchemaKind::AbstractTyped)
                .with_parent(base)
                .flattened()
                .with_property(attr(&t, "mid:y", 2)),
        )
        .register(
            SchemaDefinition::typed(leaf)
                .with_parent(mid)
                .with_property(attr(&t, "leaf:z", 3)),
        );
    let registry = builder.build(&t);
    let names = |definition: &PrimDefinition| -> Vec<String> {
        definition
            .properties()
            .iter()
            .map(|p| String::from(t.resolve(p.name)))
            .collect()
    };
    // OpenUSD: a generated class that leaves its base's properties out
    // lacks them; it still is its base.
    let slim_definition = registry.prim_definition(Some(slim), &[], &t);
    assert_eq!(names(&slim_definition), ["slim:x"]);
    assert!(slim_definition.is_a(base) && slim_definition.applied_schemas().is_empty());
    // Composed by hand up to `Mid`, whose class holds the rest.
    let leaf_definition = registry.prim_definition(Some(leaf), &[], &t);
    assert_eq!(names(&leaf_definition), ["leaf:z", "mid:y"]);
    assert!(leaf_definition.is_a(base));
    assert!(registry.issues().is_empty(), "{:?}", registry.issues());
}

/// Additional bases give ancestry in C3 order, which auto-applies and
/// `apiSchemaCanOnlyApplyTo` follow, and no properties: OpenUSD's
/// `TfType` ancestry of a hand-written `plugInfo.json`'s `bases`.
#[test]
fn additional_bases_give_c3_ancestry_but_no_properties() {
    let t = TokenInterner::default();
    let [body, tagged, thing, twin, gadget, muddle, tag, only, lost] = [
        "Body", "Tagged", "Thing", "Twin", "Gadget", "Muddle", "TagAPI", "OnlyAPI", "Lost",
    ]
    .map(|n| t.intern(n));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::new(body, SchemaKind::AbstractTyped).with_property(attr(
                &t,
                "body:mass",
                1,
            )),
        )
        .register(
            SchemaDefinition::new(tagged, SchemaKind::AbstractTyped).with_property(attr(
                &t,
                "tag:label",
                "",
            )),
        )
        .register(
            SchemaDefinition::typed(thing)
                .with_parent(body)
                .with_additional_base(tagged)
                .with_property(attr(&t, "thing:size", 3)),
        )
        .register(
            SchemaDefinition::typed(twin)
                .with_parent(tagged)
                .with_additional_base(body),
        )
        .register(
            SchemaDefinition::typed(gadget)
                .with_parent(thing)
                .with_additional_base(lost)
                .with_additional_base(tag),
        )
        .register(
            SchemaDefinition::typed(muddle)
                .with_parent(body)
                .with_additional_base(thing),
        )
        .register(SchemaDefinition::api(tag).with_property(attr(&t, "tag:mode", "on")))
        .register(SchemaDefinition::api(only).with_can_only_apply_to(tagged))
        .auto_apply(tag, tagged);
    let registry = builder.build(&t);

    assert_eq!(registry.ancestors(thing), [thing, body, tagged]);
    assert_eq!(registry.ancestors(twin), [twin, tagged, body]);
    assert_eq!(registry.ancestors(gadget), [gadget, thing, body, tagged]);
    assert!(registry.ancestors(tag).is_empty());
    let definition = registry.prim_definition(Some(thing), &[], &t);
    assert!(definition.is_a(tagged) && definition.is_a(body));
    assert_eq!(definition.ancestors(), [thing, body, tagged]);
    // The parent's properties compose in, the additional base's do not;
    // the auto-apply to `Tagged` reaches `Thing` through it.
    let names: Vec<&str> = definition
        .properties()
        .iter()
        .map(|p| t.resolve(p.name))
        .collect();
    assert_eq!(names, ["thing:size", "body:mass", "tag:mode"]);
    assert_eq!(applied_names(&definition, &t), ["TagAPI"]);
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(thing), &[], &t),
            only,
            None,
            &t
        ),
        Ok(())
    );
    assert!(
        registry
            .can_apply(
                &registry.prim_definition(Some(gadget), &[], &t),
                only,
                None,
                &t
            )
            .is_ok()
    );

    // `Muddle` names `Body` before `Thing`, which names `Body` first: no
    // linearization, but it still is each ancestor.
    assert_eq!(registry.ancestors(muddle)[0], muddle);
    let mut ancestors = registry.ancestors(muddle).to_vec();
    ancestors.sort_unstable();
    let mut expected = vec![muddle, body, thing, tagged];
    expected.sort_unstable();
    assert_eq!(ancestors, expected);
    assert_eq!(
        registry.issues(),
        [
            SchemaIssue::InvalidBase {
                schema: gadget,
                base: lost,
                reason: InvalidBaseReason::Unregistered,
            },
            SchemaIssue::InvalidBase {
                schema: gadget,
                base: tag,
                reason: InvalidBaseReason::NotTyped,
            },
            SchemaIssue::InconsistentAncestry {
                schema: muddle,
                bases: vec![body, thing],
            },
        ]
    );
}

/// A base that is the schema itself, directly or through its own bases, is
/// left out; the other bases still count.
#[test]
fn base_cycles_are_cut_and_reported() {
    let t = TokenInterner::default();
    let [a, b, c, me] = ["A", "B", "C", "Me"].map(|n| t.intern(n));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(a).with_parent(b))
        .register(
            SchemaDefinition::typed(b)
                .with_parent(a)
                .with_additional_base(c),
        )
        .register(SchemaDefinition::new(c, SchemaKind::AbstractTyped))
        .register(SchemaDefinition::typed(me).with_parent(me));
    let registry = builder.build(&t);
    assert_eq!(registry.ancestors(a), [a, b, c]);
    assert_eq!(registry.ancestors(me), [me]);
    assert!(registry.issues().contains(&SchemaIssue::InvalidBase {
        schema: b,
        base: a,
        reason: InvalidBaseReason::Cycle,
    }));
    assert!(registry.issues().contains(&SchemaIssue::InvalidBase {
        schema: me,
        base: me,
        reason: InvalidBaseReason::Cycle,
    }));
}

#[test]
fn inclusions_compose_right_after_their_schema() {
    // Spec: AOUSD Core §13.3.2.3's order: each schema's built-ins and
    // auto-applies follow it, before the next authored schema.
    let t = TokenInterner::default();
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
                .with_property(attr(&t, "text", "label")),
        )
        .register(
            SchemaDefinition::api(style)
                .with_built_in(color)
                .with_property(PropertyDefinition::attribute(t.intern("note"))),
        )
        .register(
            SchemaDefinition::api(color)
                .with_property(attr(&t, "note", "color"))
                .with_property(attr(&t, "shade", "color")),
        )
        .register(SchemaDefinition::api(other).with_property(attr(&t, "shade", "other")))
        .register(SchemaDefinition::api(auto).with_property(attr(&t, "text", "auto")))
        .auto_apply(auto, label);
    let registry = builder.build(&t);

    let applied = [label, other];
    let definition = registry.prim_definition(Some(tile), &applied, &t);
    assert_eq!(
        applied_names(&definition, &t),
        ["LabelAPI", "StyleAPI", "ColorAPI", "AutoAPI", "OtherAPI"]
    );
    // `ColorAPI`, nested two deep in `LabelAPI`, outranks `OtherAPI`.
    let shade = fallback(&registry, Some(tile), &applied, "shade", &t);
    assert_eq!(shade, Some(Value::from("color")));
    // `StyleAPI` defines `note` without a fallback; `ColorAPI` fills it in.
    let note = fallback(&registry, Some(tile), &applied, "note", &t);
    assert_eq!(note, Some(Value::from("color")));
    let text = fallback(&registry, Some(tile), &applied, "text", &t);
    assert_eq!(text, Some(Value::from("label")));
}

#[test]
fn overrides_replace_fallbacks_but_not_variability() {
    // Spec: AOUSD Core §13.3.2.2.
    let t = TokenInterner::default();
    let (outer, inner) = (t.intern("OuterAPI"), t.intern("InnerAPI"));
    let size = t.intern("size");
    let int = PropertyType::parse("int").unwrap();
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
                .with_override(attr(&t, "ghost", 1)),
        )
        .register(
            SchemaDefinition::api(inner).with_property(
                PropertyDefinition::attribute(size)
                    .with_type(int)
                    .with_fallback(12),
            ),
        );
    let registry = builder.build(&t);

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
    let t = TokenInterner::default();
    let (tile, slot, pin) = (t.intern("Tile"), t.intern("SlotAPI"), t.intern("PinAPI"));
    let (pin_extra, slot_main) = (t.intern("PinAPI:extra"), t.intern("SlotAPI:main"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(tile).with_built_in(slot_main))
        .register(
            SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi)
                .with_built_in(pin)
                .with_built_in(pin_extra)
                .with_property(attr(&t, "slot:__INSTANCE_NAME__:index", 0)),
        )
        .register(
            SchemaDefinition::new(pin, SchemaKind::MultipleApplyApi)
                .with_property(attr(&t, "pin:__INSTANCE_NAME__:offset", 0.25_f32))
                .with_property(attr(&t, "__INSTANCE_NAME__:pinned", "no")),
        );
    let registry = builder.build(&t);

    let applied = [t.intern("SlotAPI:left:upper"), slot, t.intern("TileAPI")];
    registry.intern_instance_names(&applied, &t);
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
            fallback(&registry, Some(tile), &applied, name, &t),
            Some(value),
            "{name}"
        );
    }
    assert_eq!(
        fallback(&registry, Some(tile), &applied, "slot:right:index", &t),
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
    let t = TokenInterner::default();
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
    let registry = builder.build(&t);
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
    let t = TokenInterner::default();
    let (one, two) = (t.intern("OneAPI"), t.intern("TwoAPI"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(
            SchemaDefinition::api(one)
                .with_built_in(two)
                .with_property(attr(&t, "value", 1)),
        )
        .register(
            SchemaDefinition::api(two)
                .with_built_in(one)
                .with_property(attr(&t, "value", 2)),
        );
    let registry = builder.build(&t);
    let from_one = registry.prim_definition(None, &[one], &t);
    assert_eq!(applied_names(&from_one, &t), ["OneAPI", "TwoAPI"]);
    let from_two = registry.prim_definition(None, &[two], &t);
    assert_eq!(applied_names(&from_two, &t), ["TwoAPI", "OneAPI"]);
    assert_eq!(
        fallback(&registry, None, &[two], "value", &t),
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
    let t = TokenInterner::default();
    let (shape, tile) = (t.intern("Shape"), t.intern("Tile"));
    let (first, second) = (t.intern("AutoFirstAPI"), t.intern("AutoSecondAPI"));
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::new(shape, SchemaKind::AbstractTyped))
        .register(SchemaDefinition::typed(tile).with_parent(shape))
        .register(SchemaDefinition::api(first).with_property(attr(&t, "enabled", true)))
        .register(SchemaDefinition::api(second).with_property(attr(&t, "enabled", false)))
        .auto_apply(first, shape)
        .auto_apply(second, shape);
    let registry = builder.build(&t);
    let definition = registry.prim_definition(Some(tile), &[], &t);
    assert_eq!(
        applied_names(&definition, &t),
        ["AutoSecondAPI", "AutoFirstAPI"]
    );
    assert_eq!(
        fallback(&registry, Some(tile), &[], "enabled", &t),
        Some(Value::Bool(false))
    );
}

#[test]
fn a_weaker_definition_of_another_type_is_ignored() {
    let t = TokenInterner::default();
    let (strong, weak) = (t.intern("StrongAPI"), t.intern("WeakAPI"));
    let rank = t.intern("rank");
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::api(strong).with_property(
            PropertyDefinition::attribute(rank).with_type(PropertyType::parse("int").unwrap()),
        ))
        .register(
            SchemaDefinition::api(weak).with_property(
                PropertyDefinition::attribute(rank)
                    .with_type(PropertyType::parse("float").unwrap())
                    .with_fallback(2.0_f32),
            ),
        );
    let registry = builder.build(&t);
    assert_eq!(fallback(&registry, None, &[strong, weak], "rank", &t), None);
    assert_eq!(
        fallback(&registry, None, &[weak, strong], "rank", &t),
        Some(Value::Float(2.0))
    );
}

/// Building a definition never interns: a multiple-apply instance whose
/// names were not interned first is a bug the debug build reports.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "was never interned")]
fn uninterned_instance_names_are_reported() {
    let t = TokenInterner::default();
    let slot = t.intern("SlotAPI");
    let mut builder = SchemaRegistry::builder();
    builder.register(
        SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi).with_property(attr(
            &t,
            "slot:__INSTANCE_NAME__:index",
            0,
        )),
    );
    let registry = builder.build(&t);
    let applied = [t.intern("SlotAPI:fresh")];
    let _ = registry.prim_definition(None, &applied, &t);
}

/// A single-property lookup finds exactly the names the built definition
/// has: never a template name, nor a name one segment off an instance's.
#[test]
fn property_lookup_matches_the_built_definition() {
    let t = TokenInterner::default();
    let [tile, slot, pin, label] = ["Tile", "SlotAPI", "PinAPI", "LabelAPI"].map(|n| t.intern(n));
    let pin_extra = t.intern("PinAPI:extra");
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(tile).with_built_in(label))
        .register(SchemaDefinition::api(label).with_property(attr(&t, "label", "x")))
        .register(
            SchemaDefinition::new(slot, SchemaKind::MultipleApplyApi)
                .with_built_in(pin)
                .with_built_in(pin_extra)
                .with_property(attr(&t, "slot:__INSTANCE_NAME__:index", 3))
                .with_property(attr(&t, "slot:__INSTANCE_NAME__", 1)),
        )
        .register(
            SchemaDefinition::new(pin, SchemaKind::MultipleApplyApi).with_property(attr(
                &t,
                "__INSTANCE_NAME__:pinned",
                2,
            )),
        );
    let registry = builder.build(&t);
    let applied = [t.intern("SlotAPI:left"), t.intern("SlotAPI:a:b")];
    registry.intern_instance_names(&applied, &t);
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

    let t = TokenInterner::default();
    let paths = PathInterner::default();
    let (slot, pin, tile) = (t.intern("SlotAPI"), t.intern("PinAPI"), t.intern("Tile"));
    let [by_type, by_instance] =
        ["PinAPI:__INSTANCE_NAME__", "PinAPI:__INSTANCE_NAME__:extra"].map(|n| t.intern(n));
    let (offset, index) = (
        t.intern("pin:__INSTANCE_NAME__:offset"),
        t.intern("slot:__INSTANCE_NAME__:index"),
    );
    let (api_schemas, custom_data) = (t.intern("apiSchemas"), t.intern("customData"));
    let float = PropertyType::parse("float").unwrap();

    let mut layer = Layer::new(LayerId(1));
    let slot_path = paths.intern(Path::root().join(&[slot]));
    layer.insert_prim(
        slot_path,
        PrimSpec::class()
            .with_field(
                api_schemas,
                FieldValue::TokenListOp(Box::new(ListOp::explicit(vec![by_type, by_instance]))),
            )
            .with_field(
                custom_data,
                Value::Dictionary(vec![(
                    Arc::from("apiSchemaOverridePropertyNames"),
                    Value::Array(vec![Value::Token(offset)]).into(),
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
                PropertySpec::typed_attribute(PropertyType::parse("int").unwrap())
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
    let schemas = read_generated_schema(&layer, &declared, &t, &paths).expect("read");
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
    let registry = builder.build(&t);
    let applied = [t.intern("SlotAPI:a")];
    assert_eq!(
        fallback(&registry, None, &applied, "pin:a:offset", &t),
        Some(Value::Float(0.5))
    );
    assert_eq!(
        fallback(&registry, None, &applied, "pin:a:extra:offset", &t),
        Some(Value::Float(0.25))
    );

    let missing = [SchemaDeclaration::new(tile, SchemaKind::ConcreteTyped)];
    assert_eq!(
        read_generated_schema(&layer, &missing, &t, &paths),
        Err(GeneratedSchemaError::MissingSchema { name: tile })
    );
}

/// Where an applied schema may be applied, and with which instance names.
///
/// OpenUSD: `UsdPrim::CanApplyAPI`,
/// `UsdSchemaRegistry::IsAllowedAPISchemaInstanceName`.
#[test]
fn can_apply_checks_instance_names_and_prim_types() {
    let t = TokenInterner::default();
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
                .with_property(attr(&t, "pin:__INSTANCE_NAME__:offset", 0))
                .with_property(attr(&t, "pin:__INSTANCE_NAME__", 0)),
        );
    let registry = builder.build(&t);

    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(tile), &[], &t),
            label,
            None,
            &t
        ),
        Ok(())
    );
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(other), &[], &t),
            label,
            None,
            &t
        ),
        Err(CannotApply::NotATarget {
            allowed: vec![shape]
        })
    );
    assert!(
        registry
            .can_apply(&registry.prim_definition(None, &[], &t), label, None, &t)
            .is_err()
    );
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(tile), &[], &t),
            label,
            Some("x"),
            &t
        ),
        Err(CannotApply::UnexpectedInstanceName)
    );
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(tile), &[], &t),
            slot,
            None,
            &t
        ),
        Err(CannotApply::MissingInstanceName)
    );
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(tile), &[], &t),
            tile,
            None,
            &t
        ),
        Err(CannotApply::NotAnAppliedSchema)
    );

    // Allowed instance names, and a per-instance type list.
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(tile), &[], &t),
            slot,
            Some("main"),
            &t
        ),
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
    let limits = limits.build(&t);
    for instance in ["empty", "missing"] {
        assert_eq!(
            limits.can_only_apply_to(limited, Some(instance), &t),
            [tile]
        );
        assert!(
            limits
                .can_apply(
                    &limits.prim_definition(None, &[], &t),
                    limited,
                    Some(instance),
                    &t
                )
                .is_err()
        );
        assert_eq!(
            limits.can_apply(
                &limits.prim_definition(Some(tile), &[], &t),
                limited,
                Some(instance),
                &t
            ),
            Ok(())
        );
    }
    assert_eq!(limits.can_only_apply_to(limited, Some("open"), &t), [shape]);
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(tile), &[], &t),
            slot,
            Some("left"),
            &t
        ),
        Err(CannotApply::InstanceNameNotAllowed)
    );
    assert!(
        registry
            .can_apply(
                &registry.prim_definition(Some(tile), &[], &t),
                slot,
                Some("right"),
                &t
            )
            .is_err()
    );
    assert_eq!(
        registry.can_apply(
            &registry.prim_definition(Some(other), &[], &t),
            slot,
            Some("right"),
            &t
        ),
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

/// `apiSchemaCanOnlyApplyTo` may name applied schemas, with or without an
/// instance, as well as typed schemas; a prim matching any one name may
/// take the schema.
///
/// OpenUSD 26.11: `UsdPrim::CanApplyAPI` (`_IsPrimValidApplyToTarget`).
#[test]
fn can_apply_accepts_a_prim_with_a_listed_applied_schema() {
    let t = TokenInterner::default();
    let [tile, panel, sensor, port, gauge, valve, meter, ghost] = [
        "Tile",
        "Panel",
        "SensorAPI",
        "PortAPI",
        "GaugeAPI",
        "ValveAPI",
        "MeterAPI",
        "GhostAPI",
    ]
    .map(|n| t.intern(n));
    let [port_inlet, port_outlet] = ["PortAPI:inlet", "PortAPI:outlet"].map(|n| t.intern(n));
    let missing = t.intern("MissingAPI");
    let mut builder = SchemaRegistry::builder();
    builder
        .register(SchemaDefinition::typed(tile))
        .register(SchemaDefinition::typed(panel).with_built_in(sensor))
        .register(SchemaDefinition::api(sensor))
        .register(
            SchemaDefinition::new(port, SchemaKind::MultipleApplyApi).with_property(attr(
                &t,
                "port:__INSTANCE_NAME__:rate",
                0,
            )),
        )
        .register(
            SchemaDefinition::api(gauge)
                .with_can_only_apply_to(sensor)
                .with_can_only_apply_to(tile),
        )
        .register(SchemaDefinition::api(valve).with_can_only_apply_to(port_inlet))
        .register(SchemaDefinition::api(meter).with_can_only_apply_to(port))
        .register(SchemaDefinition::api(ghost).with_can_only_apply_to(missing));
    let registry = builder.build(&t);
    let prim = |type_name: Option<TokenId>, applied: &[TokenId]| {
        registry.intern_instance_names(applied, &t);
        registry.prim_definition(type_name, applied, &t)
    };
    let can =
        |definition: &PrimDefinition, schema| registry.can_apply(definition, schema, None, &t);

    let plain = prim(None, &[]);
    let sensed = prim(None, &[sensor]);
    let inlet = prim(None, &[port_inlet]);
    let outlet = prim(None, &[port_outlet]);
    // By type, by an applied schema, by a built-in one.
    assert_eq!(can(&prim(Some(tile), &[]), gauge), Ok(()));
    assert_eq!(can(&sensed, gauge), Ok(()));
    assert_eq!(can(&prim(Some(panel), &[]), gauge), Ok(()));
    assert_eq!(
        can(&plain, gauge),
        Err(CannotApply::NotATarget {
            allowed: vec![sensor, tile]
        })
    );
    // A named instance, or any instance.
    assert_eq!(can(&inlet, valve), Ok(()));
    assert!(can(&outlet, valve).is_err());
    assert_eq!(can(&inlet, meter), Ok(()));
    assert_eq!(can(&outlet, meter), Ok(()));
    assert!(can(&sensed, meter).is_err());
    // A name that is no schema matches nothing.
    assert!(can(&sensed, ghost).is_err());
}

#[test]
fn property_metadata_composes_strong_to_weak_only_for_matching_types() {
    use crate::{FieldValue, PropertyType};
    let tokens = TokenInterner::default();
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
    let float = PropertyType::parse("float").unwrap();
    let dictionary = |items: &[(&str, i32)]| {
        Value::Dictionary(vec![(
            "nested".into(),
            Value::Dictionary(
                items
                    .iter()
                    .map(|(key, value)| ((*key).into(), Value::Int(*value).into()))
                    .collect(),
            )
            .into(),
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
                .with_type(PropertyType::parse("int").unwrap())
                .with_metadata(only_wrong, Value::Bool(true)),
        ),
    );
    let registry = builder.build(&tokens);
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
    let tokens = TokenInterner::default();
    let strong = tokens.intern("StrongAPI");
    let weak = tokens.intern("WeakAPI");
    let name = tokens.intern("size");
    let dict = tokens.intern("settings");
    let dictionary = |key: &str| Value::Dictionary(vec![(key.into(), Value::Int(1).into())]);
    let float = PropertyType::parse("float").unwrap();
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
    let registry = builder.build(&tokens);
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
                .map(|(key, value)| ((*key).into(), Value::Int(*value).into()))
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
    let registry = Arc::new(builder.build(&store.tokens));
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
            ("a".into(), Value::Int(9).into()),
            ("b".into(), Value::Int(2).into())
        ])
    );
    assert_eq!(
        stage
            .resolve_authored_property_metadata(prim, name, settings)
            .unwrap()
            .value,
        ResolvedValue::Dictionary(vec![("a".into(), Value::Int(9).into())])
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

/// A store with a `Rock` prim, and a registry defining `Rock` built on
/// `tokens`.
fn rock_scene(
    registry_tokens: Option<&TokenInterner>,
) -> (crate::InMemoryStore, crate::StageOptions) {
    let mut store = crate::InMemoryStore::default();
    let rock = store.tokens.intern("Rock");
    let mut layer = crate::Layer::new(crate::LayerId(1));
    let mut spec = crate::PrimSpec::def();
    spec.type_name = Some(rock);
    layer.insert_prim(store.path("/Boulder"), spec);
    store.insert_layer(layer);
    let tokens = registry_tokens.unwrap_or(&store.tokens);
    let mut builder = SchemaRegistry::builder();
    builder.register(
        SchemaDefinition::typed(tokens.intern("Rock")).with_property(
            PropertyDefinition::attribute(tokens.intern("hardness")).with_fallback(5.5),
        ),
    );
    let options = crate::StageOptions {
        schemas: Some(Arc::new(builder.build(tokens))),
        ..crate::StageOptions::default()
    };
    (store, options)
}

#[test]
fn a_registry_on_another_interner_is_reported_and_not_used() {
    use crate::{CompositionError, LayerId, LiveStage, Stage};
    // The foreign interner hands out the same ids for other names: `Rock`
    // here is id 1, and `Boulder` is id 1 in the store.
    let foreign = TokenInterner::default();
    let (mut store, options) = rock_scene(Some(&foreign));
    let registry = options.schemas.clone().unwrap();
    assert!(!registry.belongs_to(&store.tokens) && registry.belongs_to(&foreign));
    let boulder = store.path("/Boulder");

    let stage = Stage::compose(&mut store, LayerId(1), options.clone());
    assert_eq!(
        stage.composition_errors(),
        [CompositionError::ForeignSchemaRegistry]
    );
    assert!(stage.schemas().is_none(), "the registry is not used");
    assert!(stage.options().schemas.is_some(), "the options are kept");
    let hardness = store.tokens.intern("hardness");
    assert!(stage.resolve_value_with_schema(boulder, hardness).is_none());

    // A live stage reports it from every full composition, and keeps it
    // across partial ones.
    let mut live = LiveStage::compose(&mut store, LayerId(1), options);
    assert_eq!(
        live.stage().composition_errors(),
        [CompositionError::ForeignSchemaRegistry]
    );
    let mut txn = crate::Transaction::new();
    let at = crate::EditTarget::for_layer(LayerId(1));
    txn.create_prim(at.prim(store.path("/Pebble")), crate::Specifier::Def, None);
    live.apply(&mut store, &txn).unwrap();
    assert!(!live.recomposition_work().full_rebuild);
    assert_eq!(
        live.stage().composition_errors(),
        [CompositionError::ForeignSchemaRegistry]
    );
}

#[test]
fn a_registry_belongs_to_its_interner_and_to_snapshots_taken_after_it() {
    use crate::{LayerId, Stage};
    let (mut store, options) = rock_scene(None);
    let registry = options.schemas.clone().unwrap();
    assert!(registry.belongs_to(&store.tokens));
    assert!(SchemaRegistry::default().belongs_to(&TokenInterner::default()));
    let stage = Stage::compose(&mut store, LayerId(1), options.clone());
    assert!(stage.composition_errors().is_empty());
    let boulder = store.path("/Boulder");
    let hardness = store.tokens.intern("hardness");
    assert_eq!(
        stage
            .resolve_value_with_schema(boulder, hardness)
            .map(|resolved| resolved.value),
        Some(crate::ResolvedValue::Scalar(Value::Double(5.5)))
    );

    // A published snapshot keeps the registry's ids, through any number of
    // snapshots, even once the interners in between are gone.
    let snapshot = store.snapshot();
    assert!(registry.belongs_to(&snapshot.tokens));
    let again = snapshot.snapshot();
    drop(snapshot);
    let mut third = again.snapshot();
    drop(again);
    assert!(registry.belongs_to(&third.tokens));
    assert!(
        Stage::compose(&mut third, LayerId(1), options.clone())
            .composition_errors()
            .is_empty()
    );

    // A registry built after a snapshot diverged may hold ids the snapshot
    // gives to other names.
    let snapshot = store.snapshot();
    let mut builder = SchemaRegistry::builder();
    builder.register(SchemaDefinition::typed(store.tokens.intern("Fern")));
    let later = builder.build(&store.tokens);
    assert!(later.belongs_to(&store.tokens));
    assert!(!later.belongs_to(&snapshot.tokens));
}
