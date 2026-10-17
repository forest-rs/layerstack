// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The name rules usdGenSchema and `UsdSchemaRegistry` apply to schema and
//! property names, ported so the compiler accepts and rejects the names
//! they do.

/// `__INSTANCE_NAME__`: the instance name placeholder of a multiple-apply
/// schema's property and built-in names
/// (`UsdSchemaRegistry::MakeMultipleApplyNameTemplate("", "")`).
pub(crate) const INSTANCE_NAME_PLACEHOLDER: &str = "__INSTANCE_NAME__";

/// usdGenSchema's `_ProperCase`: each run of word characters with its
/// first character uppercased, the rest removed; a string of one character
/// or none is uppercased.
pub(crate) fn proper_case(text: &str) -> String {
    if text.chars().count() <= 1 {
        return text.to_uppercase();
    }
    text.split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let first = chars.next().expect("a non-empty word");
            first.to_uppercase().chain(chars).collect::<String>()
        })
        .collect()
}

/// usdGenSchema's `_CamelCase`: [`proper_case`] with its first character
/// lowercased; a string of one character or none is lowercased.
pub(crate) fn camel_case(text: &str) -> String {
    if text.chars().count() <= 1 {
        return text.to_lowercase();
    }
    let proper = proper_case(text);
    let mut chars = proper.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// `TfIsValidIdentifier`: an ASCII letter or `_`, then ASCII letters,
/// digits and `_`.
pub(crate) fn is_valid_identifier(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `TfMakeValidIdentifier`: every byte that cannot be in an identifier, a
/// leading digit included, becomes `_`; the empty string becomes `_`.
pub(crate) fn make_valid_identifier(text: &str) -> String {
    if text.is_empty() {
        return "_".into();
    }
    text.bytes()
        .enumerate()
        .map(|(i, b)| {
            if b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()) {
                char::from(b)
            } else {
                '_'
            }
        })
        .collect()
}

/// usdGenSchema's `_MakeValidToken`: the C++ identifier a token's name
/// becomes.
pub(crate) fn make_valid_token(token: &str, literal: bool) -> String {
    let mut id = if token.starts_with(|c: char| c.is_ascii_digit()) {
        format!("_{token}")
    } else {
        token.to_owned()
    };
    if !literal || id.contains(':') {
        id = camel_case(&id);
    }
    if !is_valid_identifier(&id) {
        id = make_valid_identifier(&id);
    }
    id
}

/// `SdfPath::JoinIdentifier` over `names`, leaving out empty ones.
fn join_identifiers(names: &[&str]) -> String {
    names
        .iter()
        .filter(|name| !name.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(":")
}

/// `UsdSchemaRegistry::MakeMultipleApplyNameTemplate`:
/// `prefix:__INSTANCE_NAME__:base`, without the empty parts.
pub(crate) fn multiple_apply_name_template(prefix: &str, base: &str) -> String {
    join_identifiers(&[prefix, INSTANCE_NAME_PLACEHOLDER, base])
}

/// `UsdSchemaRegistry::MakeMultipleApplyNameInstance`: `template` with its
/// first `__INSTANCE_NAME__` segment replaced by `instance`.
pub(crate) fn multiple_apply_name_instance(template: &str, instance: &str) -> String {
    let mut done = false;
    template
        .split(':')
        .map(|segment| {
            if !done && segment == INSTANCE_NAME_PLACEHOLDER {
                done = true;
                instance
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

/// `UsdSchemaRegistry::IsMultipleApplyNameTemplate`: whether `name` has an
/// `__INSTANCE_NAME__` segment.
pub(crate) fn is_multiple_apply_name_template(name: &str) -> bool {
    name.split(':')
        .any(|segment| segment == INSTANCE_NAME_PLACEHOLDER)
}

/// usdGenSchema's `_MakeMultipleApplySchemaNameTemplate`: a built-in of a
/// multiple-apply schema, `Other` or `Other:sub`, as the template
/// `Other:__INSTANCE_NAME__` or `Other:__INSTANCE_NAME__:sub`.
pub(crate) fn multiple_apply_schema_name_template(api_schema: &str) -> String {
    let (type_name, instance) = api_schema.split_once(':').unwrap_or((api_schema, ""));
    multiple_apply_name_template(type_name, instance)
}

/// `UsdSchemaRegistry::ParseSchemaFamilyAndVersionFromIdentifier`: the
/// family is everything before a final `_<digits>` suffix, whose digits are
/// the version; without one, the identifier is the family, at version 0.
pub(crate) fn family_and_version(identifier: &str) -> (&str, u32) {
    if let Some((family, version)) = identifier.rsplit_once('_')
        && !version.is_empty()
        && version.bytes().all(|b| b.is_ascii_digit())
        && let Ok(version) = version.parse()
    {
        return (family, version);
    }
    (identifier, 0)
}

/// `UsdSchemaRegistry::IsAllowedSchemaIdentifier`: an identifier whose
/// family is an identifier with no version suffix of its own, spelled as
/// `UsdSchemaRegistry::MakeSchemaIdentifierForFamilyAndVersion` spells it
/// (no `_0`, no leading zeros).
pub(crate) fn is_allowed_schema_identifier(identifier: &str) -> bool {
    let (family, version) = family_and_version(identifier);
    let family_allowed = is_valid_identifier(family) && family_and_version(family) == (family, 0);
    let canonical = if version == 0 {
        family.to_owned()
    } else {
        format!("{family}_{version}")
    };
    family_allowed && canonical == identifier
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cases_follow_usd_gen_schema() {
        assert_eq!(
            proper_case("collection:includeRoot"),
            "CollectionIncludeRoot"
        );
        assert_eq!(proper_case("a"), "A");
        assert_eq!(camel_case("PointBased"), "pointBased");
        assert_eq!(camel_case("inputs:diffuse_color"), "inputsDiffuse_color");
        assert_eq!(
            camel_case(&multiple_apply_name_instance(
                &multiple_apply_name_template("price", "base"),
                "_MultipleApplyTemplate_"
            )),
            "price_MultipleApplyTemplate_Base"
        );
        assert_eq!(make_valid_token("myVarying:Token", false), "myVaryingToken");
        assert_eq!(make_valid_token("2d", true), "_2d");
        assert_eq!(make_valid_identifier("\u{e9}t\u{e9}"), "__t__");
    }

    #[test]
    fn templates_follow_the_schema_registry() {
        assert_eq!(multiple_apply_name_template("", ""), "__INSTANCE_NAME__");
        assert_eq!(
            multiple_apply_name_template("collection", ""),
            "collection:__INSTANCE_NAME__"
        );
        assert_eq!(
            multiple_apply_schema_name_template("CollectionAPI:lightLink"),
            "CollectionAPI:__INSTANCE_NAME__:lightLink"
        );
        assert!(is_multiple_apply_name_template("a:__INSTANCE_NAME__:b"));
        assert!(!is_multiple_apply_name_template("a:__INSTANCE_NAME__b"));
    }

    /// The answers usd-core 26.8 gives for these identifiers.
    #[test]
    fn identifiers_follow_the_schema_registry() {
        for (id, family, version, allowed) in [
            ("Foo", "Foo", 0, true),
            ("Foo_1", "Foo", 1, true),
            ("Foo_0", "Foo", 0, false),
            ("Foo_01", "Foo", 1, false),
            ("Foo_", "Foo_", 0, true),
            ("_1", "", 1, false),
            ("Foo_1_2", "Foo_1", 2, false),
            ("Foo__1", "Foo_", 1, true),
            ("Foo_-1", "Foo_-1", 0, false),
            ("Foo_1a", "Foo_1a", 0, true),
            ("1Foo", "1Foo", 0, false),
        ] {
            assert_eq!(family_and_version(id), (family, version), "{id}");
            assert_eq!(is_allowed_schema_identifier(id), allowed, "{id}");
        }
    }
}
