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
}

#[derive(Default)]
pub(super) struct LayerSpecs {
    headers: Vec<Header>,
    fields: Vec<Field>,
    occurrences: Vec<usize>,
    scalar_index: HashTable<usize>,
    hasher: DefaultHashBuilder,
}

// The ubiquitous specifier, typeName, custom, variability and scalar default
// fields occupy one value each. Every other value goes straight to the pool;
// the packer's typed caches handle their payloads, including float bit patterns.
#[derive(Hash, PartialEq, Eq)]
enum Scalar<'a> {
    Specifier(u8),
    Variability(u8),
    Bool(bool),
    Token(&'a str),
    String(&'a str),
}

fn scalar(value: &Value) -> Option<Scalar<'_>> {
    Some(match value {
        Value::Specifier(s) => Scalar::Specifier(*s as u8),
        Value::Variability(v) => Scalar::Variability(*v as u8),
        Value::Bool(b) => Scalar::Bool(*b),
        Value::Token(t) => Scalar::Token(t),
        Value::String(s) => Scalar::String(s),
        _ => return None,
    })
}

impl SpecSink for LayerSpecs {
    fn begin(&mut self, path: impl Into<String>, form: SpecForm) {
        self.headers.push(Header {
            path: path.into(),
            form,
            first_field: self.occurrences.len(),
        });
    }

    fn path(&self) -> &str {
        &self.headers.last().expect("an open spec").path
    }

    fn field(&mut self, name: impl AsRef<str> + Into<String>, value: Value) {
        let index = self.fields.len();
        if let Some(key) = scalar(&value) {
            let hash = self.hasher.hash_one((name.as_ref(), &key));
            if let Some(&found) = self.scalar_index.find(hash, |&i| {
                let f = &self.fields[i];
                f.name == name.as_ref() && scalar(&f.value).as_ref() == Some(&key)
            }) {
                self.occurrences.push(found);
                return;
            }
            self.scalar_index.insert_unique(hash, index, |&i| {
                let f = &self.fields[i];
                self.hasher
                    .hash_one((&f.name, scalar(&f.value).expect("indexed scalar")))
            });
        }
        self.fields.push(Field::new(name, value));
        self.occurrences.push(index);
    }
}

impl Specs for LayerSpecs {
    const VALIDATED_NAMESPACE: bool = true;
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
