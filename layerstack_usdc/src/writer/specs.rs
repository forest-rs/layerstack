// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fields retained between the authored namespace walk and crate packing.
//! Specs arrive in namespace order. The file groups properties by name, so
//! first-use indexes must wait until the specs are in crate order.

use alloc::string::String;
use alloc::vec::Vec;
use core::hash::BuildHasher;
use hashbrown::{DefaultHashBuilder, HashTable};

use super::{Field, Spec, SpecForm, Value};

/// A lowering writes the fields of the last spec it opened, in authored order.
pub(super) trait SpecSink {
    fn begin(&mut self, path: impl Into<String>, form: SpecForm);
    fn path(&self) -> &str;
    fn field(&mut self, name: impl AsRef<str> + Into<String>, value: Value);
    fn metadata(&mut self, field: Field) {
        self.field(field.name, field.value);
    }
}

impl SpecSink for Vec<Spec> {
    fn begin(&mut self, path: impl Into<String>, form: SpecForm) {
        self.push(Spec::new(path, form));
    }
    fn path(&self) -> &str {
        &self.last().expect("an open spec").path
    }
    fn field(&mut self, name: impl AsRef<str> + Into<String>, value: Value) {
        self.last_mut()
            .expect("an open spec")
            .fields
            .push(Field::new(name, value));
    }
}

pub(super) trait Specs {
    /// The authored namespace walk checked every name, duplicate and owner.
    const VALIDATED_NAMESPACE: bool;
    fn len(&self) -> usize;
    fn header(&self, index: usize) -> (&str, SpecForm);
    fn fields(&self, index: usize) -> impl ExactSizeIterator<Item = (Option<usize>, &Field)>;
    /// Caller-supplied names can shadow another field on the same spec.
    fn check_field_names(&self, _index: usize) -> bool {
        true
    }
    fn retained_field_count(&self) -> usize {
        0
    }
}

impl Specs for [Spec] {
    const VALIDATED_NAMESPACE: bool = false;
    fn len(&self) -> usize {
        self.len()
    }
    fn header(&self, index: usize) -> (&str, SpecForm) {
        let spec = &self[index];
        (&spec.path, spec.form)
    }
    fn fields(&self, index: usize) -> impl ExactSizeIterator<Item = (Option<usize>, &Field)> {
        self[index].fields.iter().map(|f| (None, f))
    }
}

struct Header {
    path: String,
    form: SpecForm,
    first_field: usize,
    has_metadata: bool,
}

#[derive(Default)]
pub(super) struct LayerSpecs {
    headers: Vec<Header>,
    fields: Vec<Field>,
    occurrences: Vec<usize>,
    field_index: HashTable<usize>,
    hasher: DefaultHashBuilder,
}

// Repeated declaration fields and short name lists occupy one value each.
// Every other value goes straight to the pool;
// the packer's typed caches handle their payloads, including float bit patterns.
#[derive(Hash, PartialEq, Eq)]
enum CommonField<'a> {
    Specifier(u8),
    Variability(u8),
    Bool(bool),
    Token(&'a str),
    String(&'a str),
    Tokens(&'a [String]),
}

fn common_field(value: &Value) -> Option<CommonField<'_>> {
    Some(match value {
        Value::Specifier(s) => CommonField::Specifier(*s as u8),
        Value::Variability(v) => CommonField::Variability(*v as u8),
        Value::Bool(b) => CommonField::Bool(*b),
        Value::Token(t) => CommonField::Token(t),
        Value::String(s) => CommonField::String(s),
        Value::TokenVector(names) if names.len() <= 8 => CommonField::Tokens(names),
        _ => return None,
    })
}

impl SpecSink for LayerSpecs {
    fn begin(&mut self, path: impl Into<String>, form: SpecForm) {
        self.headers.push(Header {
            path: path.into(),
            form,
            first_field: self.occurrences.len(),
            has_metadata: false,
        });
    }

    fn path(&self) -> &str {
        &self.headers.last().expect("an open spec").path
    }

    fn metadata(&mut self, field: Field) {
        self.headers.last_mut().expect("an open spec").has_metadata = true;
        self.field(field.name, field.value);
    }

    fn field(&mut self, name: impl AsRef<str> + Into<String>, value: Value) {
        if common_field(&value).is_some_and(|key| self.reuse(name.as_ref(), &key)) {
            return;
        }
        self.store(name, value);
    }
}

impl LayerSpecs {
    /// Register the next slot on a miss so callers only copy a token when
    /// the new field will actually own it.
    fn reuse(&mut self, name: &str, key: &CommonField<'_>) -> bool {
        let hash = self.hasher.hash_one((name, key));
        if let Some(&found) = self.field_index.find(hash, |&i| {
            let f = &self.fields[i];
            f.name == name && common_field(&f.value).as_ref() == Some(key)
        }) {
            self.occurrences.push(found);
            return true;
        }
        self.field_index
            .insert_unique(hash, self.fields.len(), |&i| {
                let f = &self.fields[i];
                self.hasher
                    .hash_one((&f.name, common_field(&f.value).expect("indexed field")))
            });
        false
    }

    fn store(&mut self, name: impl Into<String>, value: Value) {
        self.occurrences.push(self.fields.len());
        self.fields.push(Field::new(name, value));
    }

    pub(super) fn token(&mut self, name: &str, token: &str) {
        if !self.reuse(name, &CommonField::Token(token)) {
            self.store(name, Value::Token(token.into()));
        }
    }
}

impl Specs for LayerSpecs {
    const VALIDATED_NAMESPACE: bool = true;
    fn check_field_names(&self, index: usize) -> bool {
        self.headers[index].has_metadata
    }
    fn retained_field_count(&self) -> usize {
        self.fields.len()
    }
    fn len(&self) -> usize {
        self.headers.len()
    }
    fn header(&self, index: usize) -> (&str, SpecForm) {
        let header = &self.headers[index];
        (&header.path, header.form)
    }
    fn fields(&self, index: usize) -> impl ExactSizeIterator<Item = (Option<usize>, &Field)> {
        let start = self.headers[index].first_field;
        let end = self
            .headers
            .get(index + 1)
            .map_or(self.occurrences.len(), |h| h.first_field);
        self.occurrences[start..end]
            .iter()
            .map(|&i| (Some(i), &self.fields[i]))
    }
}
