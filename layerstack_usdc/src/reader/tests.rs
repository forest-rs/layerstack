// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{vec, vec::Vec};

use super::*;
use crate::{
    value_type::ValueType,
    writer::{Spec, Value, write_crate},
};

fn fixture() -> Vec<u8> {
    write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/Rock", SpecForm::Prim).with_field("typeName", Value::Token("Mesh".into())),
        Spec::new("/Rock.weights", SpecForm::Attribute)
            .with_field("typeName", Value::Token("float[]".into()))
            .with_field("default", Value::FloatArray(vec![1.0, 2.0, 3.0])),
        Spec::new("/Rock{lod=}", SpecForm::VariantSet),
        Spec::new("/Rock{lod=low}", SpecForm::Variant)
            .with_field("documentation", Value::String("low detail".into())),
    ])
    .unwrap()
}

#[test]
fn lookup_and_iteration_leave_values_packed() {
    let bytes = fixture();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    let used = budget.used();
    let paths: Vec<_> = file.specs().map(CrateSpec::path).collect();
    assert!(paths.windows(2).all(|p| p[0] < p[1]));
    assert!(file.spec("/Missing").is_none());
    let spec = file.spec("/Rock.weights").unwrap();
    assert_eq!(spec.form(), SpecForm::Attribute);
    assert!(spec.field("missing").is_none());
    let names: Vec<_> = spec.fields().map(|field| field.name()).collect();
    assert!(names.contains(&"default") && names.contains(&"typeName"));
    let field = spec.field("default").unwrap();
    assert!(field.representation().is_array());
    assert_eq!(budget.used(), used);
    let CrateValue::Array(values) = field.decode(&mut budget).unwrap() else {
        panic!("array")
    };
    assert_eq!(values.len(), 3);
    assert!(matches!(values[2], CrateValue::Float(v) if v == 3.0));
    assert!(budget.used() > used);
    let text = file
        .spec("/Rock{lod=low}")
        .unwrap()
        .field("documentation")
        .unwrap()
        .decode(&mut budget)
        .unwrap();
    assert!(matches!(text, CrateValue::String(s) if s == "low detail"));
}

#[test]
fn repeated_reads_charge_the_same_budget() {
    let bytes = fixture();
    let file = CrateFile::open(&bytes, &mut DecodeBudget::for_input(bytes.len())).unwrap();
    let field = file
        .spec("/Rock.weights")
        .unwrap()
        .field("default")
        .unwrap();
    let mut probe = DecodeBudget::for_input(bytes.len());
    field.decode(&mut probe).unwrap();
    let mut budget = DecodeBudget::with_limit(probe.used());
    field.decode(&mut budget).unwrap();
    assert!(matches!(
        field.decode(&mut budget),
        Err(UsdcError::DecodeBudgetExceeded { .. })
    ));
    // A caller can start a separate operation with its own explicit budget.
    assert!(
        field
            .decode(&mut DecodeBudget::for_input(bytes.len()))
            .is_ok()
    );
}

#[test]
fn value_errors_are_deferred_but_unrelated_fields_still_read() {
    let bytes = fixture();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    let mut sections = file.sections;
    for field in &mut sections.fields {
        if sections.tokens[field.token_index as usize] == "default" {
            field.value_rep[6] = 255;
        }
    }
    let file = CrateFile::from_sections(&bytes, sections, &mut budget).unwrap();
    let field = file
        .spec("/Rock.weights")
        .unwrap()
        .field("default")
        .unwrap();
    assert!(matches!(
        field.decode(&mut budget),
        Err(UsdcError::UnknownValueType { type_byte: 255 })
    ));
    let ty = file
        .spec("/Rock")
        .unwrap()
        .field("typeName")
        .unwrap()
        .decode(&mut budget)
        .unwrap();
    assert!(matches!(ty, CrateValue::Token(s) if s == "Mesh"));
}

#[test]
fn an_unread_value_cannot_bypass_version_checks_when_decoded() {
    let bytes = fixture();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    let mut sections = file.sections;
    sections.version = CrateVersion::OLDEST_READABLE;
    for field in &mut sections.fields {
        if sections.tokens[field.token_index as usize] == "default" {
            field.value_rep = [0, 0, 0, 0, 0, 0, ValueType::Int as u8, 0x10];
        }
    }
    let file = CrateFile::from_sections(&bytes, sections, &mut budget).unwrap();
    assert!(matches!(
        file.spec("/Rock.weights")
            .unwrap()
            .field("default")
            .unwrap()
            .decode(&mut budget),
        Err(UsdcError::FeatureRequiresVersion { .. })
    ));
}

#[test]
fn malformed_table_links_are_rejected_before_lookup() {
    let bytes = fixture();
    let file = CrateFile::open(&bytes, &mut DecodeBudget::for_input(bytes.len())).unwrap();
    for mutate in [
        (|s: &mut CrateSections| s.specs[0].path_index = u32::MAX) as fn(&mut CrateSections),
        |s| s.specs[0].fieldset_index = u32::MAX,
        |s| s.fields[0].token_index = u32::MAX,
        |s| s.fieldsets[0] = i32::MAX,
        |s| *s.fieldsets.last_mut().unwrap() = 0,
        |s| s.specs.push(s.specs[0]),
    ] {
        let mut sections = file.sections.clone();
        mutate(&mut sections);
        assert!(matches!(
            CrateFile::from_sections(&bytes, sections, &mut DecodeBudget::for_input(bytes.len())),
            Err(UsdcError::Inconsistent { .. })
        ));
    }
}

#[test]
fn opening_and_index_allocation_are_budgeted() {
    let bytes = fixture();
    assert!(matches!(
        CrateFile::open(&bytes, &mut DecodeBudget::with_limit(0)),
        Err(UsdcError::DecodeBudgetExceeded { .. })
    ));
    let file = CrateFile::open(&bytes, &mut DecodeBudget::for_input(bytes.len())).unwrap();
    assert!(matches!(
        CrateFile::from_sections(&bytes, file.sections, &mut DecodeBudget::with_limit(0)),
        Err(UsdcError::DecodeBudgetExceeded { .. })
    ));
}

#[test]
fn borrowed_text_and_compact_arrays_agree_with_owned_decode() {
    for (kind, value) in [
        ("string", Value::String("café".into())),
        ("token", Value::Token("finish".into())),
        ("empty", Value::String("".into())),
        (
            "strings",
            Value::StringArray(vec!["灰色".into(), "".into(), "灰色".into()]),
        ),
        (
            "tokens",
            Value::TokenArray((0..64).map(|i| alloc::format!("v{}", i % 3)).collect()),
        ),
    ] {
        let bytes = write_crate(&[
            Spec::new("/", SpecForm::PseudoRoot),
            Spec::new("/Value", SpecForm::Prim).with_field(kind, value),
        ])
        .unwrap();
        let mut budget = DecodeBudget::for_input(bytes.len());
        let file = CrateFile::open(&bytes, &mut budget).unwrap();
        let field = file.spec("/Value").unwrap().field(kind).unwrap();
        match field.decode(&mut budget).unwrap() {
            CrateValue::String(value) | CrateValue::Token(value) => {
                assert_eq!(field.read_text(&mut budget).unwrap(), Some(value.as_str()));
                assert!(field.read_text_array(&mut budget).unwrap().is_none());
            }
            CrateValue::Array(values) => {
                let view = field.read_text_array(&mut budget).unwrap().unwrap();
                let expected: Vec<_> = values
                    .iter()
                    .map(|value| match value {
                        CrateValue::String(s) | CrateValue::Token(s) => s.as_str(),
                        _ => panic!("text"),
                    })
                    .collect();
                assert_eq!(view.iter().collect::<Vec<_>>(), expected);
                assert_eq!(
                    view.iter().rev().collect::<Vec<_>>(),
                    expected.iter().copied().rev().collect::<Vec<_>>()
                );
                assert_eq!(view.len(), expected.len());
                assert_eq!(view.is_empty(), expected.is_empty());
                assert_eq!(view.get(expected.len()), None);
                assert!(field.read_text(&mut budget).unwrap().is_none());
            }
            _ => panic!("text fixture"),
        }
        assert!(
            matches!(
                field.read_text_array(&mut DecodeBudget::with_limit(0)),
                Err(UsdcError::DecodeBudgetExceeded { .. })
            ) || !field.representation().is_array()
        );
    }
}

#[test]
fn selective_materialization_does_not_decode_or_own_the_other_product() {
    struct NoAssets;
    impl layerstack::AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<layerstack::LayerId>,
            _: &layerstack::TokenInterner,
            _: &layerstack::PathInterner,
        ) -> Result<layerstack::ResolvedAsset, layerstack::AssetResolveError> {
            Err(layerstack::AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: layerstack::LayerId) -> Option<&str> {
            None
        }
    }
    let bytes = write_crate(&[
        Spec::new("/", SpecForm::PseudoRoot),
        Spec::new("/Selected", SpecForm::Prim),
        Spec::new("/Selected.name", SpecForm::Attribute)
            .with_field("typeName", Value::Token("string".into()))
            .with_field("default", Value::String("Chair".into())),
        Spec::new("/Unselected", SpecForm::Prim),
        Spec::new("/Unselected.secret", SpecForm::Attribute)
            .with_field("typeName", Value::Token("string".into()))
            .with_field("default", Value::String("Unrelated".into())),
    ])
    .unwrap();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    let mut sections = file.sections;
    let bad=sections.fields.iter().position(|field|matches!(decode_value_within(&RawValueRep::new(field.value_rep),&bytes,&sections,&mut budget),Ok(CrateValue::String(s)) if s=="Unrelated")).unwrap();
    sections.fields[bad].value_rep[6] = 255;
    let file = CrateFile::from_sections(&bytes, sections, &mut budget).unwrap();
    let mut store = layerstack::InMemoryStore::default();
    let selected = file
        .materialize_where(
            layerstack::LayerId(1),
            &store.tokens,
            &store.paths,
            &mut NoAssets,
            &mut budget,
            |spec| {
                spec.path() == "/"
                    || spec.path() == "/Selected"
                    || spec.path().starts_with("/Selected.")
            },
        )
        .unwrap();
    assert!(selected.diagnostics.is_empty());
    assert_eq!(selected.layer.prims().len(), 1);
    assert!(
        file.materialize_where(
            layerstack::LayerId(2),
            &store.tokens,
            &store.paths,
            &mut NoAssets,
            &mut budget,
            |_| true
        )
        .is_err()
    );
    store.insert_layer(selected.layer);
    let stage = layerstack::Stage::compose(
        &mut store,
        layerstack::LayerId(1),
        layerstack::StageOptions::default(),
    );
    assert!(stage.has_prim(store.path("/Selected")));
    assert!(!stage.has_prim(store.path("/Unselected")));
}

#[test]
fn selecting_every_spec_preserves_the_bulk_import_including_variants() {
    struct NoAssets;
    impl layerstack::AssetResolver for NoAssets {
        fn resolve(
            &mut self,
            _: &str,
            _: Option<layerstack::LayerId>,
            _: &layerstack::TokenInterner,
            _: &layerstack::PathInterner,
        ) -> Result<layerstack::ResolvedAsset, layerstack::AssetResolveError> {
            Err(layerstack::AssetResolveError::NotFound)
        }
        fn resolved_path(&self, _: layerstack::LayerId) -> Option<&str> {
            None
        }
    }
    let bytes = fixture();
    let mut budget = DecodeBudget::for_input(bytes.len());
    let file = CrateFile::open(&bytes, &mut budget).unwrap();
    let tokens = layerstack::TokenInterner::default();
    let paths = layerstack::PathInterner::default();
    let selected = file
        .materialize_where(
            layerstack::LayerId(1),
            &tokens,
            &paths,
            &mut NoAssets,
            &mut budget,
            |_| true,
        )
        .unwrap();
    let full = crate::read_usdc(
        &bytes,
        layerstack::LayerId(1),
        &tokens,
        &paths,
        &mut NoAssets,
    )
    .unwrap();
    assert_eq!(selected.layer, full.layer);
    assert_eq!(selected.diagnostics, full.diagnostics);
    assert!(
        file.materialize_where(
            layerstack::LayerId(1),
            &tokens,
            &paths,
            &mut NoAssets,
            &mut DecodeBudget::with_limit(0),
            |_| true
        )
        .is_err()
    );
}
