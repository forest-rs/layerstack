// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use alloc::{boxed::Box, vec::Vec};
use hashbrown::{HashMap, HashSet};
use layerstack::{
    AppliedSchema, PrimDefinition, PropertyDefinition, SchemaRegistry, TokenId, TokenInterner,
    schema::INSTANCE_NAME_PLACEHOLDER,
};

/// The names of the APIs already applied, with a property index built on demand.
#[derive(Debug)]
pub(super) struct AppliedIndex<'a> {
    names: HashSet<TokenId>,
    declarations: Option<Box<Declarations<'a>>>,
}

impl<'a> AppliedIndex<'a> {
    pub(super) fn new(applied: &[TokenId]) -> Self {
        Self {
            names: applied.iter().copied().collect(),
            declarations: None,
        }
    }

    pub(super) fn insert(
        &mut self,
        entry: TokenId,
        rank: usize,
        schemas: &'a SchemaRegistry,
        tokens: &'a TokenInterner,
    ) -> bool {
        if !self.names.insert(entry) {
            return false;
        }
        if let Some(declarations) = &mut self.declarations {
            declarations.insert(entry, rank, schemas, tokens);
        }
        true
    }

    pub(super) fn property(
        &mut self,
        name: TokenId,
        applied: &[TokenId],
        schemas: &'a SchemaRegistry,
        tokens: &'a TokenInterner,
    ) -> Option<&'a PropertyDefinition> {
        let declarations = self.declarations.get_or_insert_with(|| {
            let mut declarations = Box::new(Declarations::default());
            for (rank, &entry) in applied.iter().enumerate() {
                declarations.insert(entry, rank, schemas, tokens);
            }
            declarations
        });
        declarations.property(name, tokens)
    }

    #[cfg(test)]
    pub(super) fn has_declarations(&self) -> bool {
        self.declarations.is_some()
    }
}

/// Declarations of the APIs already applied to one prim, in authored strength
/// order (AOUSD Core §13.3.2.3). Index a schema's templates once even when
/// thousands of applications bind different instances. Only an authored
/// property needs an interned concrete name.
#[derive(Debug, Default)]
struct Declarations<'a> {
    families: HashSet<TokenId>,
    instances: HashMap<(TokenId, TokenId), usize>,
    named: HashMap<TokenId, Candidate<'a>>,
    templates: HashMap<&'a str, Vec<Template<'a>>>,
}

#[derive(Clone, Copy, Debug)]
struct Candidate<'a> {
    // Application order, then property order within the definition. Multiple
    // different template names can expand to the same concrete property.
    strength: (usize, usize),
    property: &'a PropertyDefinition,
}

#[derive(Debug)]
struct Template<'a> {
    schema: TokenId,
    suffix: &'a str,
    slot: usize,
    property: &'a PropertyDefinition,
}

impl<'a> Declarations<'a> {
    fn insert(
        &mut self,
        entry: TokenId,
        rank: usize,
        schemas: &'a SchemaRegistry,
        tokens: &'a TokenInterner,
    ) {
        let Some((schema, definition, instance)) = schemas.applied_definition(entry, tokens) else {
            return;
        };
        if let Some(instance) = instance {
            self.instances
                .entry((schema, tokens.intern(instance)))
                .or_insert(rank);
        }
        if self.families.insert(schema) {
            for (slot, property) in definition.properties().iter().enumerate() {
                if instance.is_some()
                    && let Some((prefix, suffix)) = template_parts(tokens.resolve(property.name))
                {
                    self.templates.entry(prefix).or_default().push(Template {
                        schema,
                        suffix,
                        slot,
                        property,
                    });
                } else {
                    self.named.entry(property.name).or_insert(Candidate {
                        strength: (rank, slot),
                        property,
                    });
                }
            }
        }
    }

    fn property(&self, name: TokenId, tokens: &TokenInterner) -> Option<&'a PropertyDefinition> {
        let mut found = self.named.get(&name).copied();
        let text = tokens.resolve(name);
        // A placeholder occupies a namespace segment, so its prefix is
        // empty or ends at a colon. The matching suffix identifies the
        // whole instance, including any colons in it.
        for at in core::iter::once(0).chain(text.match_indices(':').map(|(at, _)| at + 1)) {
            let Some(templates) = self.templates.get(&text[..at]) else {
                continue;
            };
            for template in templates {
                let Some(instance) = text[at..]
                    .strip_suffix(template.suffix)
                    .and_then(|name| tokens.lookup(name))
                else {
                    continue;
                };
                let Some(&rank) = self.instances.get(&(template.schema, instance)) else {
                    continue;
                };
                let strength = (rank, template.slot);
                if found.is_none_or(|old| strength < old.strength) {
                    found = Some(Candidate {
                        strength,
                        property: template.property,
                    });
                }
            }
        }
        found.map(|candidate| candidate.property)
    }
}

pub(super) fn template_parts(template: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for segment in template.split(':') {
        if segment == INSTANCE_NAME_PLACEHOLDER {
            return Some((&template[..offset], &template[offset + segment.len()..]));
        }
        offset += segment.len() + 1;
    }
    None
}

/// The effective applied APIs needed to check restricted authoring, separate
/// from property declarations. Built-in and auto-applied APIs take precedence.
#[derive(Debug, Default)]
pub(super) struct ApiMemberships {
    names: HashSet<TokenId>,
    schemas: HashSet<TokenId>,
    instances: HashSet<(TokenId, TokenId)>,
}

impl ApiMemberships {
    pub(super) fn new(typed: &PrimDefinition) -> Self {
        let mut memberships = Self::default();
        for &api in typed.applied_schemas() {
            memberships.record(api);
        }
        memberships
    }

    pub(super) fn insert(
        &mut self,
        entry: TokenId,
        schemas: &SchemaRegistry,
        tokens: &TokenInterner,
    ) {
        let Some((_, definition, instance)) = schemas.applied_definition(entry, tokens) else {
            return;
        };
        for &api in definition.applied_schemas() {
            let api = match instance {
                None => api,
                Some(instance) => {
                    let expand = |name| {
                        let text = tokens.resolve(name);
                        if template_parts(text).is_some() {
                            tokens.intern(crate::view::instance_name(text, instance))
                        } else {
                            name
                        }
                    };
                    AppliedSchema {
                        name: expand(api.name),
                        instance: api.instance.map(expand),
                        ..api
                    }
                }
            };
            self.record(api);
        }
    }

    pub(super) fn contains(&self, schema: TokenId, instance: Option<TokenId>) -> bool {
        match instance {
            None => self.schemas.contains(&schema),
            Some(instance) => self.instances.contains(&(schema, instance)),
        }
    }

    fn record(&mut self, api: AppliedSchema) {
        if self.names.insert(api.name) {
            self.schemas.insert(api.schema);
            if let Some(instance) = api.instance {
                self.instances.insert((api.schema, instance));
            }
        }
    }
}
