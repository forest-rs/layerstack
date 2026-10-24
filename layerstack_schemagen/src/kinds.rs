// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Plugin kind declarations: the `Kinds` of each plugin's `Info` in a
//! `plugInfo.json`, read as OpenUSD's `KindRegistry` reads them.
//!
//! ```json
//! { "Plugins": [{ "Name": "props", "Info": { "Kinds": {
//!     "prop": { "baseKind": "component" },
//!     "set": { "baseKind": "assembly" }
//! }}}]}
//! ```
//!
//! [`read`] takes the declarations of a set of `plugInfo.json` files apart
//! into those OpenUSD registers and those it refuses, each refusal a
//! [`KindDeclarationError`] naming the plugin, the kind, the field and the
//! [`KindRejection`]. A schema library's Rust module
//! ([`CompiledSchema::rust_module`](crate::compile::CompiledSchema::rust_module))
//! and [`generate_kind_library`](crate::generate_kind_library) register the
//! accepted ones with `layerstack_schemas::kind::KindRegistry`, and refuse
//! to generate while any declaration is rejected.
//!
//! OpenUSD registers a kind whose base is no kind, and kinds whose bases
//! form a cycle; so does this, and `KindRegistry::issues` reports them.
//!
//! Spec: AOUSD Core §7.6.2.4.4 (`kind`), §11.4 (the model hierarchy).
//! OpenUSD: `KindRegistry::_RegisterDefaults` and `_Register`
//! (`pxr/usd/kind/registry.cpp`).

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::path::Path;

use serde_json::Value as Json;

/// OpenUSD's built-in kinds, which no plugin can declare again.
const BUILT_INS: &[&str] = &["model", "group", "assembly", "component", "subcomponent"];

/// A kind a plugin declares and OpenUSD registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindDeclaration {
    /// The declaring plugin ([`KindDeclarationError::plugin`]).
    pub plugin: String,
    /// The kind's name.
    pub kind: String,
    /// Its `baseKind`; `None` when absent or empty.
    pub base: Option<String>,
}

/// A kind declaration OpenUSD refuses, and where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindDeclarationError {
    /// The plugin: its `Name`, or `Plugins[<index>]` without one, after
    /// the file's label when [`read`] is given several.
    pub plugin: String,
    /// The declared kind, or `None` when the plugin's `Kinds` as a whole
    /// is malformed.
    pub kind: Option<String>,
    /// The offending field of the declaration (`baseKind`), if one.
    pub field: Option<&'static str>,
    /// Why it is refused.
    pub reason: KindRejection,
}

/// Why OpenUSD refuses a kind declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KindRejection {
    /// The plugin's `Kinds` is not an object; OpenUSD ignores it silently.
    KindsNotAnObject,
    /// The declaration is not an object (OpenUSD: "Expected dict for
    /// kind").
    NotAnObject,
    /// `baseKind` is not a string (OpenUSD: "Expected string for
    /// baseKind").
    BaseNotAString,
    /// The name is not an identifier (OpenUSD: "Invalid kind").
    InvalidName,
    /// The name is a built-in kind (OpenUSD: "Kind '…' has already been
    /// registered").
    BuiltIn,
    /// An earlier plugin of the set declares the kind too (OpenUSD: "Kind
    /// '…' has already been registered"). Which of the two OpenUSD keeps
    /// depends on its plugin map's order, not on file order.
    AlreadyDeclared {
        /// The plugin whose declaration stands here.
        by: String,
    },
}

impl fmt::Display for KindDeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "plugin `{}`", self.plugin)?;
        if let Some(kind) = &self.kind {
            write!(f, ", kind `{kind}`")?;
        }
        if let Some(field) = self.field {
            write!(f, ", `{field}`")?;
        }
        f.write_str(": ")?;
        match &self.reason {
            KindRejection::KindsNotAnObject => f.write_str("`Kinds` is not an object"),
            KindRejection::NotAnObject => f.write_str("the declaration is not an object"),
            KindRejection::BaseNotAString => f.write_str("not a string"),
            KindRejection::InvalidName => f.write_str("the name is not an identifier"),
            KindRejection::BuiltIn => f.write_str("redefines a built-in kind"),
            KindRejection::AlreadyDeclared { by } => {
                write!(f, "plugin `{by}` declares it already")
            }
        }
    }
}

impl std::error::Error for KindDeclarationError {}

/// The kind declarations of a set of `plugInfo.json` files, as OpenUSD
/// takes them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginKinds {
    /// What OpenUSD registers, in file and plugin order, each plugin's in
    /// name order (OpenUSD reads `Kinds` as an ordered map).
    pub accepted: Vec<KindDeclaration>,
    /// What it refuses, in the same order.
    pub rejected: Vec<KindDeclarationError>,
}

/// Why the `plugInfo.json` files could not be read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlugInfoError {
    /// The file's label.
    pub file: String,
    /// What is wrong: unreadable, not JSON, or no `Plugins` list.
    pub message: String,
}

impl fmt::Display for PlugInfoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.file, self.message)
    }
}

impl std::error::Error for PlugInfoError {}

/// Why a kind library was not generated ([`generate_kind_library`](crate::generate_kind_library)).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KindLibraryError {
    /// No `plugInfo.json` was given.
    NoPlugins,
    /// A file could not be read.
    PlugInfo(PlugInfoError),
    /// Declarations OpenUSD refuses, every one.
    Rejected(Vec<KindDeclarationError>),
}

impl fmt::Display for KindLibraryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPlugins => f.write_str("select at least one kind plugin"),
            Self::PlugInfo(error) => error.fmt(f),
            Self::Rejected(rejected) => write_rejected(f, rejected),
        }
    }
}

impl std::error::Error for KindLibraryError {}

/// `rejected`, one per line, after a summary.
pub(crate) fn write_rejected(
    f: &mut impl fmt::Write,
    rejected: &[KindDeclarationError],
) -> fmt::Result {
    write!(f, "{} kind declarations OpenUSD refuses:", rejected.len())?;
    for error in rejected {
        write!(f, "\n  {error}")?;
    }
    Ok(())
}

/// Reads the kind declarations of `plug_infos`, `(label, text)` pairs of
/// `plugInfo.json` files (whole-line `#` comments allowed, as OpenUSD's
/// plugin reader allows them), as OpenUSD registers them: per plugin, in
/// name order, skipping each declaration it refuses. A later plugin that
/// declares a kind again is refused. A single file's plugins are labelled
/// by name alone.
///
/// # Errors
///
/// [`PlugInfoError`] for a file that is not JSON or has no `Plugins` list.
pub fn read<'a>(
    plug_infos: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<PluginKinds, PlugInfoError> {
    let files: Vec<(&str, &str)> = plug_infos.into_iter().collect();
    let mut out = PluginKinds::default();
    let mut declared: BTreeMap<String, String> = BTreeMap::new();
    for &(file, text) in &files {
        let error = |message: String| PlugInfoError {
            file: file.into(),
            message,
        };
        let json = crate::model::plug_info_text(text).map_err(error)?;
        let plugins = json["Plugins"]
            .as_array()
            .ok_or_else(|| error("no `Plugins` list".into()))?;
        for (index, plugin) in plugins.iter().enumerate() {
            let name = plugin["Name"]
                .as_str()
                .map_or_else(|| format!("Plugins[{index}]"), String::from);
            let label = if files.len() == 1 {
                name
            } else {
                format!("{file}: {name}")
            };
            read_plugin(&label, &plugin["Info"], &mut declared, &mut out);
        }
    }
    Ok(out)
}

/// One plugin's `Info.Kinds`, as `KindRegistry::_RegisterDefaults` reads
/// it.
fn read_plugin(
    plugin: &str,
    info: &Json,
    declared: &mut BTreeMap<String, String>,
    out: &mut PluginKinds,
) {
    let reject = |out: &mut PluginKinds, kind: Option<&str>, field, reason| {
        out.rejected.push(KindDeclarationError {
            plugin: plugin.into(),
            kind: kind.map(String::from),
            field,
            reason,
        });
    };
    let Some(kinds) = info.get("Kinds") else {
        return;
    };
    let Some(kinds) = kinds.as_object() else {
        reject(out, None, None, KindRejection::KindsNotAnObject);
        return;
    };
    // OpenUSD's `JsObject` is a `std::map`: declarations in name order.
    let kinds: BTreeMap<&String, &Json> = kinds.iter().collect();
    for (kind, entry) in kinds {
        let Some(entry) = entry.as_object() else {
            reject(out, Some(kind), None, KindRejection::NotAnObject);
            continue;
        };
        let base = match entry.get("baseKind") {
            None => None,
            Some(Json::String(base)) => Some(base.as_str()),
            Some(_) => {
                reject(
                    out,
                    Some(kind),
                    Some("baseKind"),
                    KindRejection::BaseNotAString,
                );
                continue;
            }
        };
        let valid = kind
            .as_bytes()
            .first()
            .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
            && kind.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
        if !valid {
            reject(out, Some(kind), None, KindRejection::InvalidName);
            continue;
        }
        if BUILT_INS.contains(&kind.as_str()) {
            reject(out, Some(kind), None, KindRejection::BuiltIn);
            continue;
        }
        if let Some(by) = declared.get(kind) {
            let by = by.clone();
            reject(out, Some(kind), None, KindRejection::AlreadyDeclared { by });
            continue;
        }
        declared.insert(kind.clone(), plugin.into());
        out.accepted.push(KindDeclaration {
            plugin: plugin.into(),
            kind: kind.clone(),
            base: base.filter(|base| !base.is_empty()).map(String::from),
        });
    }
}

/// The `KINDS` table of `accepted`, documented as `doc`.
pub(crate) fn table(accepted: &[KindDeclaration], doc: &str) -> String {
    let mut out = format!("{doc}\npub const KINDS: &[(&str, Option<&str>)] = &[\n");
    for declaration in accepted {
        let base = declaration
            .base
            .as_ref()
            .map_or_else(|| "None".into(), |base| format!("Some({base:?})"));
        let _ = writeln!(out, "    ({:?}, {base}),", declaration.kind);
    }
    out.push_str("];\n");
    out
}

pub(crate) fn generate(paths: &[impl AsRef<Path>]) -> Result<String, KindLibraryError> {
    if paths.is_empty() {
        return Err(KindLibraryError::NoPlugins);
    }
    let mut texts = Vec::new();
    for path in paths {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| {
            KindLibraryError::PlugInfo(PlugInfoError {
                file: path.display().to_string(),
                message: e.to_string(),
            })
        })?;
        texts.push((path.display().to_string(), text));
    }
    let kinds = read(
        texts
            .iter()
            .map(|(file, text)| (file.as_str(), text.as_str())),
    )
    .map_err(KindLibraryError::PlugInfo)?;
    if !kinds.rejected.is_empty() {
        return Err(KindLibraryError::Rejected(kinds.rejected));
    }
    let mut out = String::from(
        "// Generated kind declarations. Do not edit. A module of its own:\n\
         // `pub mod kinds { include!(...); }`.\n\nextern crate alloc;\n\nuse alloc::vec::Vec;\n\n",
    );
    out.push_str(&table(
        &kinds.accepted,
        "/// The kinds the plugins declare, with their base kinds: each plugin's\n\
         /// in name order, as OpenUSD reads them.",
    ));
    out.push_str(
        "\n/// Registers [`KINDS`] with `registry`, as OpenUSD registers plugin\n\
         /// kinds: each that is not registered already. Returns those that are,\n\
         /// which keep their first registration; `KindRegistry::issues` reports\n\
         /// bases that are no kind and cycles.\n\
         #[must_use = \"the kinds that were not registered\"]\n\
         pub fn register(\n    registry: &mut ::layerstack_schemas::kind::KindRegistry,\n\
         ) -> Vec<::layerstack_schemas::kind::KindError> {\n    \
         registry.register_all(KINDS.iter().copied())\n}\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read_one(json: &Json) -> PluginKinds {
        read([("plugInfo.json", json.to_string().as_str())]).unwrap()
    }

    fn rejection(
        kind: Option<&str>,
        field: Option<&'static str>,
        reason: KindRejection,
    ) -> KindDeclarationError {
        KindDeclarationError {
            plugin: "fixture".into(),
            kind: kind.map(String::from),
            field,
            reason,
        }
    }

    /// OpenUSD 26.8's `KindRegistry` registers forward and unknown bases
    /// and refuses invalid names, non-object entries, non-string bases and
    /// built-in kinds, going on with the rest.
    #[test]
    fn declarations_are_taken_as_openusd_takes_them() {
        let kinds = read_one(&json!({"Plugins": [{"Name": "fixture", "Info": {"Kinds": {
            "leaf": {"baseKind": "stem"},
            "stem": {"baseKind": "model"},
            "orphan": {"baseKind": "nowhere"},
            "blank": {"baseKind": ""},
            "free": {},
            "model": {"baseKind": "group"},
            "bad name": {},
            "listed": [],
            "numeric": {"baseKind": 5}
        }}}]}));
        let accepted: Vec<(&str, Option<&str>)> = kinds
            .accepted
            .iter()
            .map(|d| (d.kind.as_str(), d.base.as_deref()))
            .collect();
        assert_eq!(
            accepted,
            [
                ("blank", None),
                ("free", None),
                ("leaf", Some("stem")),
                ("orphan", Some("nowhere")),
                ("stem", Some("model")),
            ]
        );
        assert_eq!(
            kinds.rejected,
            [
                rejection(Some("bad name"), None, KindRejection::InvalidName),
                rejection(Some("listed"), None, KindRejection::NotAnObject),
                rejection(Some("model"), None, KindRejection::BuiltIn),
                rejection(
                    Some("numeric"),
                    Some("baseKind"),
                    KindRejection::BaseNotAString
                ),
            ]
        );
        assert!(
            read_one(&json!({"Plugins": [{"Name": "fixture", "Info": {"Kinds": []}}]})).rejected
                == [rejection(None, None, KindRejection::KindsNotAnObject)]
        );
        assert!(read([("x", "{}")]).is_err());
    }

    #[test]
    fn a_later_plugin_cannot_declare_a_kind_again() {
        let first = json!({"Plugins": [{"Name": "first", "Info": {"Kinds": {"shared": {}}}}]});
        let second = json!({"Plugins": [{"Info": {"Kinds": {"shared": {"baseKind": "group"}}}}]});
        let (first, second) = (first.to_string(), second.to_string());
        let kinds = read([("a.json", first.as_str()), ("b.json", second.as_str())]).unwrap();
        assert_eq!(kinds.accepted.len(), 1);
        assert_eq!(
            kinds.rejected,
            [KindDeclarationError {
                plugin: "b.json: Plugins[0]".into(),
                kind: Some("shared".into()),
                field: None,
                reason: KindRejection::AlreadyDeclared {
                    by: "a.json: first".into()
                },
            }]
        );
        let message = kinds.rejected[0].to_string();
        assert_eq!(
            message,
            "plugin `b.json: Plugins[0]`, kind `shared`: plugin `a.json: first` declares it already"
        );
    }
}
