// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fields retained between the authored namespace walk and crate packing.
//! Specs arrive in namespace order. The file groups properties by name, so
//! first-use indexes must wait until the specs are in crate order.

use alloc::string::String;
use alloc::vec::Vec;
use core::hash::{BuildHasher, Hash, Hasher};
use hashbrown::{DefaultHashBuilder, HashTable};

use super::{Field, Spec, SpecForm, Value};

/// A lowering writes the fields of the last spec it opened, in authored order.
pub(super) trait SpecSink {
    fn begin(&mut self, path: impl AsRef<str> + Into<String>, form: SpecForm);
    fn path(&self) -> &str;
    fn field(&mut self, name: impl AsRef<str> + Into<String>, value: Value);
    fn metadata(&mut self, field: Field) {
        self.field(field.name, field.value);
    }
}

impl SpecSink for Vec<Spec> {
    fn begin(&mut self, path: impl AsRef<str> + Into<String>, form: SpecForm) {
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

/// A checked declaration already has an owner and a separate property name.
/// Keep that boundary until a diagnostic actually needs the full spelling.
#[derive(Clone, Copy)]
pub(super) enum SpecPath<'a> {
    Text(&'a str),
    Property {
        owner: usize,
        prim: &'a str,
        name: &'a str,
    },
}

impl From<SpecPath<'_>> for String {
    fn from(path: SpecPath<'_>) -> Self {
        match path {
            SpecPath::Text(text) => text.into(),
            SpecPath::Property { prim, name, .. } => [prim, ".", name].concat(),
        }
    }
}

pub(super) trait Specs {
    /// The authored namespace walk checked every name, duplicate and owner.
    const VALIDATED_NAMESPACE: bool;
    fn len(&self) -> usize;
    fn header(&self, index: usize) -> (SpecPath<'_>, SpecForm);
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
    fn header(&self, index: usize) -> (SpecPath<'_>, SpecForm) {
        let spec = &self[index];
        (SpecPath::Text(&spec.path), spec.form)
    }
    fn fields(&self, index: usize) -> impl ExactSizeIterator<Item = (Option<usize>, &Field)> {
        self[index].fields.iter().map(|f| (None, f))
    }
}

struct Header {
    start: usize,
    owner: Option<usize>,
    form: SpecForm,
    first_field: usize,
    has_metadata: bool,
}

#[derive(Default)]
pub(super) struct LayerSpecs {
    headers: Vec<Header>,
    text: String,
    fields: Vec<Field>,
    occurrences: Vec<usize>,
    field_index: HashTable<usize>,
    hasher: DefaultHashBuilder,
}

// Repeated declaration fields and short name lists occupy one value each.
// Every other value goes straight to the pool;
// the packer's typed caches handle their payloads, including float bit patterns.
#[derive(Clone, Copy)]
enum Names<'a> {
    Stored(&'a [String]),
    Borrowed(&'a [&'a str]),
}

impl Names<'_> {
    fn as_names(&self) -> impl ExactSizeIterator<Item = &str> {
        let len = match self {
            Self::Stored(s) => s.len(),
            Self::Borrowed(s) => s.len(),
        };
        (0..len).map(move |i| match self {
            Self::Stored(s) => s[i].as_str(),
            Self::Borrowed(s) => s[i],
        })
    }
}
impl PartialEq for Names<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.as_names().eq(other.as_names())
    }
}
impl Eq for Names<'_> {}
impl Hash for Names<'_> {
    fn hash<H: Hasher>(&self, h: &mut H) {
        let names = self.as_names();
        names.len().hash(h);
        for n in names {
            n.hash(h);
        }
    }
}

#[derive(Hash, PartialEq, Eq)]
enum CommonField<'a> {
    Specifier(u8),
    Variability(u8),
    Bool(bool),
    Token(&'a str),
    String(&'a str),
    Tokens(Names<'a>),
}

fn common_field(value: &Value) -> Option<CommonField<'_>> {
    Some(match value {
        Value::Specifier(s) => CommonField::Specifier(*s as u8),
        Value::Variability(v) => CommonField::Variability(*v as u8),
        Value::Bool(b) => CommonField::Bool(*b),
        Value::Token(t) => CommonField::Token(t),
        Value::String(s) => CommonField::String(s),
        Value::TokenVector(names) if names.len() <= 8 => CommonField::Tokens(Names::Stored(names)),
        _ => return None,
    })
}

impl SpecSink for LayerSpecs {
    fn begin(&mut self, path: impl AsRef<str> + Into<String>, form: SpecForm) {
        self.header(path.as_ref(), form, None);
    }

    fn path(&self) -> &str {
        let header = self.headers.last().expect("an open spec");
        debug_assert!(header.owner.is_none(), "metadata uses a fully spelled path");
        &self.text[header.start..]
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
    fn header(&mut self, part: &str, form: SpecForm, owner: Option<usize>) -> usize {
        let index = self.headers.len();
        self.headers.push(Header {
            start: self.text.len(),
            owner,
            form,
            first_field: self.occurrences.len(),
            has_metadata: false,
        });
        self.text.push_str(part);
        index
    }

    pub(super) fn plain_prim(
        &mut self,
        id: layerstack::PathId,
        paths: &layerstack::PathInterner,
        tokens: &layerstack::TokenInterner,
    ) -> usize {
        let index = self.header("", SpecForm::Prim, None);
        for &segment in paths.resolve(id).segments() {
            self.text.push('/');
            self.text.push_str(tokens.resolve(segment));
        }
        index
    }

    pub(super) fn property(&mut self, owner: usize, name: &str, form: SpecForm) {
        self.header(name, form, Some(owner));
    }

    fn part(&self, index: usize) -> &str {
        let end = self
            .headers
            .get(index + 1)
            .map_or(self.text.len(), |h| h.start);
        &self.text[self.headers[index].start..end]
    }

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

    /// Name lists recur across prims of the same type. Probe with borrowed
    /// names so only distinct short lists allocate strings in the field pool.
    pub(super) fn names<'a>(&mut self, field: &str, names: impl ExactSizeIterator<Item = &'a str>) {
        if names.len() <= 8 {
            let names: smallvec::SmallVec<[&str; 8]> = names.collect();
            if self.reuse(field, &CommonField::Tokens(Names::Borrowed(&names))) {
                return;
            }
            self.store(
                field,
                Value::TokenVector(names.into_iter().map(String::from).collect()),
            );
        } else {
            self.store(field, Value::TokenVector(names.map(String::from).collect()));
        }
    }

    pub(super) fn string(&mut self, name: &str, value: &str) {
        if !self.reuse(name, &CommonField::String(value)) {
            self.store(name, Value::String(value.into()));
        }
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
    fn header(&self, index: usize) -> (SpecPath<'_>, SpecForm) {
        let header = &self.headers[index];
        (
            match header.owner {
                Some(owner) => SpecPath::Property {
                    owner,
                    prim: self.part(owner),
                    name: self.part(index),
                },
                None => SpecPath::Text(self.part(index)),
            },
            header.form,
        )
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
