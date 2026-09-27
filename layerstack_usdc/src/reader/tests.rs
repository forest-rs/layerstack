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
