// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The checks usdGenSchema makes only while generating C++ code, for a
//! library that is not codeless: `GatherTokens`'s one-to-one token
//! identifiers and `GenerateCode`'s reflected API schemas. The compiler
//! generates no code, but a schema that fails these fails usdGenSchema, so
//! it fails here too.

use std::collections::BTreeMap;

use layerstack::Value;

use super::classes::{ClassInfo, Parsed, dict_get, dict_str, dict_strings};
use super::names::{camel_case, is_valid_identifier, make_valid_token};
use super::{CompileError, Rule, SchemaKind};

/// The names `_AddToken` appends `_` to: C++ and Python keywords,
/// `interface` (a Windows macro) and `None`.
const RESERVED: &[&str] = &[
    // C++
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "atomic_cancel",
    "atomic_commit",
    "atomic_noexcept",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "reflexpr",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "synchronized",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
    // Python (`keyword.kwlist`)
    "False",
    "None",
    "True",
    "as",
    "assert",
    "async",
    "await",
    "def",
    "del",
    "elif",
    "except",
    "finally",
    "from",
    "global",
    "import",
    "in",
    "is",
    "lambda",
    "nonlocal",
    "pass",
    "raise",
    "with",
    "yield",
    // Others
    "interface",
];

/// Runs both checks.
pub(crate) fn check(
    parsed: &Parsed,
    tokens: &layerstack::TokenInterner,
) -> Result<(), CompileError> {
    check_reflected(parsed, tokens)?;
    check_tokens(parsed, tokens)
}

/// `GenerateCode`: each schema a class lists in `reflectedAPISchemas` must
/// be a class of the stage, and, when it is a single-apply schema, one the
/// class applies.
fn check_reflected(
    parsed: &Parsed,
    tokens: &layerstack::TokenInterner,
) -> Result<(), CompileError> {
    let known = |name: &str| {
        parsed
            .classes
            .iter()
            .chain(&parsed.reflected)
            .find(|class| class.name == name)
    };
    for class in &parsed.classes {
        for schema in dict_strings(&class.custom_data, "reflectedAPISchemas", tokens) {
            let error = |rule| CompileError::Schema {
                path: format!("/{schema}"),
                rule,
            };
            let reflected = known(&schema).ok_or_else(|| {
                error(Rule::UnknownReflectedSchema {
                    schema: schema.clone(),
                })
            })?;
            if reflected.kind != SchemaKind::SingleApplyApi {
                continue;
            }
            if !class.all_applied_api_schemas.contains(&schema) {
                return Err(error(Rule::ReflectedSchemaNotApplied {
                    schema: schema.clone(),
                }));
            }
        }
    }
    Ok(())
}

/// The token identifiers `GatherTokens` collects, each with its value.
#[derive(Default)]
struct Tokens(BTreeMap<String, String>);

impl Tokens {
    /// `_AddToken`: the identifier `id` makes, which must not already name
    /// another value.
    fn add(
        &mut self,
        owner: &str,
        id: &str,
        value: &str,
        literal: bool,
    ) -> Result<(), CompileError> {
        let mut id = if !is_valid_identifier(id) {
            make_valid_token(id, literal)
        } else if !literal {
            camel_case(id)
        } else {
            id.to_owned()
        };
        if RESERVED.contains(&id.as_str()) {
            id.push('_');
        }
        match self.0.get(&id) {
            Some(existing) if existing != value => Err(CompileError::Schema {
                path: owner.to_owned(),
                rule: Rule::TokenCollision {
                    identifier: id,
                    first: existing.clone(),
                    second: value.to_owned(),
                },
            }),
            Some(_) => Ok(()),
            None => {
                self.0.insert(id, value.to_owned());
                Ok(())
            }
        }
    }
}

/// `GatherTokens` with schema identifier tokens: every token a class's
/// properties, fallbacks, allowed tokens, schema tokens and namespace
/// prefix name, and the library's tokens, must map one identifier to one
/// value.
fn check_tokens(parsed: &Parsed, tokens: &layerstack::TokenInterner) -> Result<(), CompileError> {
    let mut all = Tokens::default();
    let mut identifiers = Tokens::default();
    for class in parsed.reflected.iter().chain(&parsed.classes) {
        let owner = format!("/{}", class.name);
        identifiers.add(&owner, &class.name, &class.name, true)?;
        if class.version != 0 {
            identifiers.add(&owner, &class.family, &class.family, true)?;
        }
        class_tokens(
            class,
            &mut all,
            tokens,
            parsed.library.use_literal_identifier,
        )?;
    }
    for (token, value) in &parsed.library_tokens {
        all.add("/GLOBAL", token, value, true)?;
    }
    // A property token that is also a schema identifier moves to the
    // identifiers, and must agree with it.
    let shared: Vec<String> = identifiers
        .0
        .keys()
        .filter(|id| all.0.contains_key(*id))
        .cloned()
        .collect();
    for id in shared {
        let value = all.0.remove(&id).expect("a shared identifier");
        identifiers.add("/GLOBAL", &id, &value, true)?;
    }
    Ok(())
}

fn class_tokens(
    class: &ClassInfo,
    all: &mut Tokens,
    tokens: &layerstack::TokenInterner,
    library_literal: bool,
) -> Result<(), CompileError> {
    let owner = format!("/{}", class.name);
    let owner = owner.as_str();
    let mut attributes: Vec<_> = class.attributes.iter().collect();
    attributes.sort_by_key(|attribute| attribute.name.to_lowercase());
    for attribute in attributes {
        all.add(owner, &attribute.name, &attribute.generated_name, false)?;
        if let Some(fallback) = &attribute.token_fallback {
            all.add(owner, fallback, fallback, class.use_literal_identifier)?;
        }
        for value in &attribute.allowed_tokens {
            if attribute.token_fallback.as_ref() == Some(value) || value.is_empty() {
                continue;
            }
            all.add(owner, value, value, class.use_literal_identifier)?;
        }
    }
    for relationship in &class.relationships {
        all.add(
            owner,
            &relationship.name,
            &relationship.generated_name,
            true,
        )?;
    }
    if let Some(Value::Dictionary(schema_tokens)) = dict_get(&class.custom_data, "schemaTokens") {
        for (token, info) in schema_tokens {
            let value = match info {
                Value::Dictionary(info) => dict_str(info, "value", tokens),
                _ => None,
            };
            all.add(owner, token, value.as_deref().unwrap_or(token), true)?;
        }
    }
    if let Some(prefix) = &class.property_namespace {
        all.add(
            owner,
            &make_valid_token(prefix, library_literal),
            prefix,
            true,
        )?;
    }
    Ok(())
}
