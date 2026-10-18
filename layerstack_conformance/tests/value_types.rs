// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! layerstack's value type table (`layerstack::ValueType`) is OpenUSD
//! 26.08's value type registry.
//!
//! `scripts/value_types_oracle.py` records every name `Sdf.ValueTypeNames`
//! registers, with its array-ness, scalar and array forms, role, C++ type
//! and aliases (`fixtures/value_types/oracle.json`). Here:
//!
//! - every registered scalar is a `ValueType` with the same name, array
//!   form and role, and `ValueType::ALL` holds no other;
//! - every registered name, scalar or array, parses
//!   (`ValueType::parse`, `PropertyType::parse`) as that type and form, and
//!   no other name does: the aliases OpenUSD also takes (`Vec3f`,
//!   `PointFloat`, ...) are deliberately not accepted, and neither are
//!   `opaque[]` and `group[]`, which OpenUSD does not register;
//! - two types share a storage type (`ValueType::storage`) exactly when
//!   their values are the same C++ type, and a storage type has no role.
//!
//! Spec: AOUSD Core §6.2 (scalar types), §6.3 (dimensioned types), §6.5
//! (semantic roles).

#![allow(missing_docs, reason = "integration tests")]

use std::collections::BTreeMap;

use layerstack::{PropertyType, ValueType};
use serde_json::Value as Json;

const ORACLE: &str = include_str!("../fixtures/value_types/oracle.json");

/// The registered names, by name.
fn registry() -> BTreeMap<String, Json> {
    let oracle: Json = serde_json::from_str(ORACLE).expect("oracle.json");
    let version = oracle["openusd_version"].as_str().unwrap();
    assert!(version.starts_with("26."), "oracle from OpenUSD {version}");
    oracle["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ty| (ty["name"].as_str().unwrap().to_owned(), ty.clone()))
        .collect()
}

fn text(record: &Json, key: &str) -> String {
    record[key].as_str().unwrap().to_owned()
}

#[test]
fn every_registered_scalar_is_a_value_type() {
    let registry = registry();
    let scalars: Vec<&Json> = registry
        .values()
        .filter(|ty| !ty["is_array"].as_bool().unwrap())
        .collect();
    assert_eq!(scalars.len(), ValueType::ALL.len());
    for record in scalars {
        let name = text(record, "name");
        let ty = ValueType::from_name(&name).unwrap_or_else(|| panic!("{name} is a ValueType"));
        assert_eq!(ty.name(), name);
        assert_eq!(
            ty.array_name().unwrap_or(""),
            text(record, "array"),
            "{name}"
        );
        let role = ty.role().map_or("", |role| role.name());
        assert_eq!(role, text(record, "role"), "{name}");
    }
}

#[test]
fn registered_names_parse_and_no_others() {
    let registry = registry();
    for (name, record) in &registry {
        let is_array = record["is_array"].as_bool().unwrap();
        let scalar = ValueType::from_name(&text(record, "scalar")).unwrap();
        assert_eq!(ValueType::parse(name), Some((scalar, is_array)), "{name}");
        let parsed = PropertyType::parse(name).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.type_name(), name);
        assert_eq!(parsed.is_array(), is_array, "{name}");
        assert_eq!(parsed.value_type(), Some(scalar), "{name}");
        for alias in record["aliases"].as_array().unwrap() {
            let alias = alias.as_str().unwrap();
            assert!(!registry.contains_key(alias), "{alias} names another type");
            assert!(PropertyType::parse(alias).is_err(), "alias {alias}");
        }
    }
    for ty in ValueType::ALL {
        assert!(registry.contains_key(ty.name()), "{ty}");
        if ty.array_name().is_none() {
            let array = format!("{}[]", ty.name());
            assert!(!registry.contains_key(&array), "{array}");
            assert!(PropertyType::parse(&array).is_err(), "{array}");
        }
    }
}

#[test]
fn storage_types_are_the_registered_cpp_types() {
    let registry = registry();
    let cpp = |ty: ValueType| text(&registry[ty.name()], "type");
    for &a in ValueType::ALL {
        let storage = a.storage();
        assert_eq!(storage.role(), None, "{a}: {storage} has a role");
        assert_eq!(cpp(a), cpp(storage), "{a} is stored as {storage}");
        for &b in ValueType::ALL {
            assert_eq!(
                cpp(a) == cpp(b),
                storage == b.storage(),
                "{a} and {b}: same C++ type, same storage"
            );
        }
    }
}
