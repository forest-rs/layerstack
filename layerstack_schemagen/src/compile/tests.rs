// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Each usdGenSchema rule, broken by the smallest schema that breaks it.
//! The schemas follow `testUsdGenSchema`'s `schemaFail*.usda` where one
//! breaks the rule.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{CompileError, CompileOptions, Rule, compile};

/// The base classes of OpenUSD's `usd/schema.usda`.
const BASE: &str = r#"#usda 1.0
def "GLOBAL" (
    customData = {
        string libraryName = "usd"
        string libraryPath = "pxr/usd/usd"
    }
)
{
}
class "Typed"
{
}
class "APISchemaBase"
{
}
"#;

/// A codeless library's header, sublayering the base classes.
const CODELESS: &str = r#"#usda 1.0
(
    subLayers = [@usd/schema.usda@]
)
over "GLOBAL" (
    customData = {
        string libraryName = "fixture"
        string libraryPath = "."
        bool skipCodeGeneration = true
    }
)
{
}
"#;

/// A library with code, whose tokens usdGenSchema also checks.
const CODEFUL: &str = r#"#usda 1.0
(
    subLayers = [@usd/schema.usda@]
)
over "GLOBAL" (
    customData = {
        string libraryName = "fixture"
        string libraryPath = "pxr/fixture"
    }
)
{
}
"#;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A directory of its own, removed when dropped.
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "layerstack-schema-compiler-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("usd")).unwrap();
        std::fs::write(path.join("usd/schema.usda"), BASE).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Compiles `text` as a schema file next to the base classes.
fn compile_text(text: &str) -> Result<super::CompiledSchema, CompileError> {
    let directory = Directory::new();
    let source = directory.0.join("schema.usda");
    std::fs::write(&source, text).unwrap();
    compile(
        &source,
        &CompileOptions::new().with_search_path(&directory.0),
    )
}

/// The rule `classes` breaks in a codeless library, and where.
fn broken(classes: &str) -> (String, Rule) {
    broken_in(CODELESS, classes)
}

fn broken_in(header: &str, classes: &str) -> (String, Rule) {
    match compile_text(&format!("{header}{classes}")) {
        Err(CompileError::Schema { path, rule }) => (path, rule),
        other => panic!("{classes}: {other:?}"),
    }
}

#[test]
fn a_well_formed_library_compiles() {
    let compiled = compile_text(&format!(
        "{CODELESS}{}",
        r#"class Thing "Thing" (inherits = </Typed>) { int count = 1 }"#
    ))
    .unwrap();
    assert_eq!(compiled.classes().len(), 1);
    assert!(
        compiled
            .generated_schema_usda()
            .contains("class Thing \"Thing\"")
    );
}

#[test]
fn library_metadata_rules() {
    let rule = |text: &str| match compile_text(text) {
        Err(CompileError::Schema { path, rule }) => {
            assert_eq!(path, "/GLOBAL");
            rule
        }
        other => panic!("{text}: {other:?}"),
    };
    assert!(matches!(
        rule("#usda 1.0\n(\n    subLayers = [@usd/schema.usda@]\n)\n"),
        Rule::MissingGlobalPrim { .. }
    ));
    assert!(matches!(
        rule("#usda 1.0\nover \"GLOBAL\"\n{\n}\n"),
        Rule::MissingLibraryData { .. }
    ));
    assert!(matches!(
        rule("#usda 1.0\nover \"GLOBAL\" (customData = {string libraryPath = \"x\"})\n{\n}\n"),
        Rule::MissingLibraryName { .. }
    ));
    assert!(matches!(
        rule("#usda 1.0\nover \"GLOBAL\" (customData = {string libraryName = \"x\"})\n{\n}\n"),
        Rule::MissingLibraryPath { .. }
    ));
}

/// Whether a broken rule is the one a case breaks.
type Expected = fn(&Rule) -> bool;

#[test]
fn class_rules() {
    let cases: &[(&str, &str, Expected)] = &[
        (
            r#"class "Thing" (inherits = </Missing>) {}"#,
            "/Missing",
            |r| matches!(r, Rule::UndefinedBase { base } if base == "Missing"),
        ),
        (
            r#"class NotThing "Thing" (inherits = </Typed>) {}"#,
            "/Thing",
            |r| matches!(r, Rule::TypeNameMismatch { type_name, .. } if type_name == "NotThing"),
        ),
        (
            r#"class "Other" (inherits = </Typed>) {}
class "Thing" (inherits = [</Typed>, </Other>]) {}"#,
            "/Thing",
            |r| matches!(r, Rule::MultipleInheritance { inherits } if inherits.len() == 2),
        ),
        (
            r#"class "OneAPI" (inherits = </APISchemaBase>) {}
class "ThingAPI" (inherits = </APISchemaBase> apiSchemas = ["OneAPI"]) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::ApiSchemasNotPrepended),
        ),
        (
            r#"class "Child" (inherits = </Typed>) {}
class "Thing" (inherits = </Typed/Child>) {}"#,
            "/Thing",
            |r| matches!(r, Rule::InvalidInherit { inherit } if inherit == "/Typed/Child"),
        ),
        (
            r#"class Thing "Thing" (inherits = </Typed>) {}
class Thing_1 "Thing_1" (inherits = </Thing>) {}"#,
            "/Thing_1",
            |r| matches!(r, Rule::InheritsOwnFamily),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token[] apiSchemaAllowedInstanceNames = ["a"]}) {}"#,
            "/ThingAPI",
            |r| {
                matches!(
                    r,
                    Rule::MultipleApplyOnlyField {
                        field: "apiSchemaAllowedInstanceNames"
                    }
                )
            },
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token apiSchemaType = "twiceApply"}) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::InvalidApiSchemaType { value } if value == "twiceApply"),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token apiSchemaType = "multipleApply" token[] apiSchemaAutoApplyTo = ["Typed"]}) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::AutoApplyNotSingleApply),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token apiSchemaType = "nonApplied" token[] apiSchemaCanOnlyApplyTo = ["Typed"]}) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::CanOnlyApplyNotApplied),
        ),
        (r#"class Thing "Thing" {}"#, "/Thing", |r| {
            matches!(r, Rule::ConcreteNotTyped)
        }),
        (
            r#"class Thing_1_2 "Thing_1_2" (inherits = </Typed>) {}"#,
            "/Thing_1_2",
            |r| matches!(r, Rule::NotAllowedIdentifier),
        ),
        (
            r#"class "Thing" (inherits = </APISchemaBase>) {}"#,
            "/Thing",
            |r| matches!(r, Rule::MissingApiSuffix),
        ),
        (
            r#"class "ThingAPI" (inherits = </Typed>) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::TypedApiSuffix),
        ),
        (
            r#"class "OneAPI" (inherits = </APISchemaBase>) {}
class "ThingAPI" (inherits = </OneAPI>) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::AppliedApiBase),
        ),
        (
            r#"class "OneAPI" (inherits = </APISchemaBase>) {}
class "ThingAPI" (inherits = </OneAPI> customData = {token apiSchemaType = "nonApplied"}) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::NonAppliedApiBase),
        ),
        (
            r#"class "Thing" (inherits = </Typed> customData = {token apiSchemaType = "singleApply"}) {}"#,
            "/Thing",
            |r| matches!(r, Rule::ApiSchemaTypeNotApi),
        ),
        (
            r#"class "Thing" (inherits = </Typed> customData = {token[] fallbackTypes = ["Old"]}) {}"#,
            "/Thing",
            |r| matches!(r, Rule::FallbackTypesNotConcrete),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token apiSchemaType = "multipleApply" token propertyNamespacePrefix = "thing"}) {}"#,
            "/ThingAPI",
            |r| matches!(r, Rule::PrefixWithoutProperties),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token apiSchemaType = "multipleApply"}) { int count }"#,
            "/ThingAPI",
            |r| matches!(r, Rule::PropertiesWithoutPrefix),
        ),
        (
            r#"class "Thing" (inherits = </Typed>) { widget count }"#,
            "/Thing.count",
            |r| matches!(r, Rule::UnknownType { type_name } if type_name == "widget"),
        ),
        (
            r#"class "Thing" (inherits = </Typed>) { token my:count
    token myCount }"#,
            "/Thing",
            |r| matches!(r, Rule::DuplicateName { name, relationship: false } if name == "Thing.myCount"),
        ),
        (
            r#"class "Thing" (inherits = </Typed>) { rel a (customData = {string apiName = "x"})
    rel b (customData = {string apiName = "x"}) }"#,
            "/Thing",
            |r| {
                matches!(
                    r,
                    Rule::DuplicateApiName {
                        relationship: true,
                        ..
                    }
                )
            },
        ),
        (
            r#"class "Base" (inherits = </Typed>) { int count }
class "Thing" (inherits = </Base>) { int count (customData = {bool apiSchemaOverride = true}) }"#,
            "/Thing.count",
            |r| matches!(r, Rule::OverrideOfDefinedProperty { .. }),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase>
    customData = {token apiSchemaType = "multipleApply" token propertyNamespacePrefix = "p"})
{ int a
    int p:__INSTANCE_NAME__:a }"#,
            "/ThingAPI",
            |r| matches!(r, Rule::PrefixedPropertyExists { name } if name == "p:__INSTANCE_NAME__:a"),
        ),
        (
            r#"class "ThingAPI" (inherits = </APISchemaBase> hidden = true) {}"#,
            "/ThingAPI",
            |r| {
                matches!(
                    r,
                    Rule::InvalidApiMetadata { fields, api_schema_type, .. }
                        if fields == &["hidden"] && api_schema_type == "singleApply"
                )
            },
        ),
    ];
    for (classes, path, rule) in cases {
        let (at, broken) = broken(classes);
        assert_eq!(&at, path, "{classes}: {broken}");
        assert!(rule(&broken), "{classes}: {broken:?}");
    }
}

#[test]
fn code_generation_rules() {
    // `schemaFail2.usda`: two token values with one C++ identifier.
    let (path, rule) = broken_in(
        CODEFUL,
        r#"class "Base" (inherits = </Typed>) {
    token myVarying:Token = "fallback" (allowedTokens = ["fallback", "myVaryingToken"])
}"#,
    );
    assert_eq!(path, "/Base");
    assert!(
        matches!(&rule, Rule::TokenCollision { identifier, .. } if identifier == "myVaryingToken"),
        "{rule:?}"
    );
    // A codeless library generates no tokens, so they may collide.
    compile_text(&format!(
        "{CODELESS}{}",
        r#"class "Base" (inherits = </Typed>) {
    token myVarying:Token = "fallback" (allowedTokens = ["fallback", "myVaryingToken"])
}"#
    ))
    .expect("no tokens to collide");

    let reflecting = |schemas: &str| {
        format!(
            r#"class "MassAPI" (inherits = </APISchemaBase>) {{}}
class "Body" (inherits = </Typed> customData = {{token[] reflectedAPISchemas = [{schemas}]}}) {{}}"#
        )
    };
    let (path, rule) = broken_in(CODEFUL, &reflecting("\"MassAPI\""));
    assert_eq!(path, "/MassAPI");
    assert!(
        matches!(rule, Rule::ReflectedSchemaNotApplied { .. }),
        "{rule:?}"
    );
    let (path, rule) = broken_in(CODEFUL, &reflecting("\"NoSuchAPI\""));
    assert_eq!(path, "/NoSuchAPI");
    assert!(
        matches!(rule, Rule::UnknownReflectedSchema { .. }),
        "{rule:?}"
    );
}

#[test]
fn disallowed_fields_are_listed() {
    let text = format!(
        "{CODELESS}{}",
        r#"class "Thing" (inherits = </Typed> kind = "model") {
    rel parts = </Typed>
    double size.timeSamples = { 1: 2 }
}"#
    );
    let Err(CompileError::InvalidFields { class, fields }) = compile_text(&text) else {
        panic!("{text}");
    };
    assert_eq!(class, "/Thing");
    let found: Vec<(&str, &str)> = fields
        .iter()
        .map(|f| (f.path.as_str(), f.field.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("/Thing", "kind"),
            ("/Thing.parts", "targetPaths"),
            ("/Thing.size", "timeSamples"),
        ]
    );
    assert_eq!(
        fields[1].to_string(),
        "Relationship targets on </Thing.parts> cannot be specified in a schema."
    );
}

#[test]
fn unresolved_sublayers_and_reading_errors_are_reported() {
    let directory = Directory::new();
    // Not next to `usd/schema.usda`, which resolves next to the layer
    // naming it.
    std::fs::create_dir(directory.0.join("lib")).unwrap();
    let source = directory.0.join("lib/schema.usda");
    std::fs::write(&source, CODELESS).unwrap();
    let error = compile(&source, &CompileOptions::new()).unwrap_err();
    assert!(
        matches!(&error, CompileError::UnresolvedSublayer { asset, .. } if asset == "usd/schema.usda"),
        "{error:?}"
    );
    std::fs::write(&source, "#usda 1.0\nclass \"A\" {").unwrap();
    let error = compile(&source, &CompileOptions::new()).unwrap_err();
    assert!(matches!(error, CompileError::Read { .. }), "{error:?}");
    let error = compile(&directory.0.join("missing.usda"), &CompileOptions::new()).unwrap_err();
    assert!(matches!(error, CompileError::Read { .. }), "{error:?}");
}

#[test]
fn a_file_without_classes_has_no_plug_info() {
    let compiled = compile_text(CODELESS).unwrap();
    assert!(compiled.classes().is_empty());
    assert_eq!(compiled.plug_info_json(), None, "usdGenSchema writes none");
    assert_eq!(
        compiled.generated_schema_usda(),
        "#usda 1.0\n(\n    \"WARNING: THIS FILE IS GENERATED BY usdGenSchema.  DO NOT EDIT.\"\n)\n\n"
    );
}

#[test]
fn messages_are_usd_gen_schema_messages() {
    let (path, rule) = broken(r#"class "Thing" (inherits = </APISchemaBase>) {}"#);
    assert_eq!(
        CompileError::Schema { path, rule }.to_string(),
        "Invalid schema definition at </Thing>\nAPI schemas must be named with an API suffix.\n\
         See https://openusd.org/release/api/_usd__page__generating_schemas.html for more \
         information."
    );
}

#[test]
fn the_command_writes_checks_and_keeps_plug_info_edits() {
    let directory = Directory::new();
    let source = directory.0.join("schema.usda");
    std::fs::write(
        &source,
        format!("{CODELESS}class Thing \"Thing\" (inherits = </Typed>) {{}}\n"),
    )
    .unwrap();
    let out = directory.0.join("out");
    let run = |check: bool| {
        let mut args = vec![
            source.display().to_string(),
            "--out".into(),
            out.display().to_string(),
            "--search".into(),
            directory.0.display().to_string(),
        ];
        if check {
            args.push("--check".into());
        }
        super::run(args.into_iter())
    };
    assert!(run(true).is_err(), "nothing written yet");
    run(false).unwrap();
    run(true).expect("up to date");
    let plug_info = out.join("plugInfo.json");
    let edited = std::fs::read_to_string(&plug_info)
        .unwrap()
        .replace("@PLUG_INFO_ROOT@", ".");
    std::fs::write(&plug_info, &edited).unwrap();
    run(true).expect("an edit outside the generated types is kept");
    std::fs::write(out.join("generatedSchema.usda"), "#usda 1.0\n").unwrap();
    let stale = run(true).unwrap_err();
    assert!(stale.contains("generatedSchema.usda"), "{stale}");
    run(false).unwrap();
    assert_eq!(std::fs::read_to_string(&plug_info).unwrap(), edited);
}

#[test]
fn installing_fills_template_placeholders_and_keeps_edits() {
    let template = "# comment\n{\"Plugins\": [{\"Info\": {\"Types\": {}}, \"LibraryPath\": \"@PLUG_INFO_LIBRARY_PATH@\", \"Name\": \"x\", \"ResourcePath\": \"@PLUG_INFO_RESOURCE_PATH@\", \"Root\": \"@PLUG_INFO_ROOT@\", \"Type\": \"resource\"}]}\n";
    let installed = super::plug_info::install(template, true).unwrap();
    assert!(!installed.contains('#'), "{installed}");
    let json: serde_json::Value = serde_json::from_str(&installed).unwrap();
    let plugin = &json["Plugins"][0];
    assert_eq!(plugin["Root"], "..");
    assert_eq!(plugin["ResourcePath"], "resources");
    assert_eq!(plugin["LibraryPath"], "");
    let edited = template.replace("@PLUG_INFO_ROOT@", "../..");
    let json: serde_json::Value =
        serde_json::from_str(&super::plug_info::install(&edited, true).unwrap()).unwrap();
    assert_eq!(
        json["Plugins"][0]["Root"], "../..",
        "an edited value is kept"
    );
    let error = super::plug_info::install(template, false).unwrap_err();
    assert!(error.to_string().contains("LibraryPath"), "{error}");
}

/// The message of the [`CompileError::Rust`] the Rust module of `classes`,
/// a codeless library, fails with.
fn rust_error(classes: &str) -> String {
    let compiled = compile_text(&format!("{CODELESS}{classes}")).expect("the schema compiles");
    match compiled.rust_module() {
        Err(CompileError::Rust { message }) => message,
        other => panic!("{classes}: {other:?}"),
    }
}

#[test]
fn rust_modules_name_their_bases_and_dependencies() {
    let compiled = compile_text(&format!(
        "{CODELESS}class Thing \"Thing\" (inherits = </Typed>) {{\n    \
         float thing:size = 2 (customData = {{ string apiName = \"extent\" }})\n}}\n"
    ))
    .unwrap();
    let module = compiled.rust_module().unwrap();
    assert!(module.contains("base: ::layerstack_schemas::usd::Typed<'a>"));
    assert!(module.contains("pub fn extent(&self) -> Option<f32>"));
    assert!(module.contains("pub fn set_extent_at("));
    assert!(module.contains(
        "pub const DEPENDENCIES: &[::layerstack_schemas::Domain] = \
         &[::layerstack_schemas::Domain::Usd];"
    ));
    assert!(module.contains("pub const LIBRARY: &str = \"fixture\";"));
}

#[test]
fn rust_names_that_cannot_be_methods_or_types_fail() {
    let typed = |properties: &str| {
        format!("class Thing \"Thing\" (inherits = </Typed>) {{\n{properties}\n}}\n")
    };
    let message = rust_error(&typed("float type"));
    assert!(message.contains("Rust keyword"), "{message}");
    let message = rust_error(&typed("float fooBar\nfloat foo_bar"));
    assert!(
        message.contains("two properties are named `foo_bar`"),
        "{message}"
    );
    let message = rust_error(&typed("float new"));
    assert!(message.contains("`new`"), "{message}");
    let message = rust_error(
        "class \"SlotAPI\" (inherits = </APISchemaBase>\n    customData = {\n        \
         token apiSchemaType = \"multipleApply\"\n        token propertyNamespacePrefix = \"slot\"\n    })\n\
         {\n    bool __INSTANCE_NAME__\n}\n",
    );
    assert!(message.contains("only an `apiName`"), "{message}");
    let message = rust_error("class Scene \"Scene\" (inherits = </Typed>) {}\n");
    assert!(
        message.contains("`Scene` is a name the module imports"),
        "{message}"
    );
    let message = rust_error(&format!(
        "{}class ThingMode \"ThingMode\" (inherits = </Typed>) {{}}\n",
        typed("token mode (allowedTokens = [\"on\", \"off\"])")
    ));
    assert!(message.contains("`ThingMode`"), "{message}");
}

#[test]
fn rust_modules_build_only_on_openusd_domains() {
    let directory = Directory::new();
    std::fs::create_dir_all(directory.0.join("other")).unwrap();
    std::fs::write(
        directory.0.join("other/schema.usda"),
        CODELESS.replace("\"fixture\"", "\"other\"") + "class \"Part\" (inherits = </Typed>) {}\n",
    )
    .unwrap();
    let source = directory.0.join("schema.usda");
    std::fs::write(
        &source,
        CODELESS.replace("@usd/schema.usda@", "@other/schema.usda@")
            + "class Widget \"Widget\" (inherits = </Part>) {}\n",
    )
    .unwrap();
    let compiled = compile(
        &source,
        &CompileOptions::new().with_search_path(&directory.0),
    )
    .expect("usdGenSchema compiles a library on another");
    let Err(CompileError::Rust { message }) = compiled.rust_module() else {
        panic!("a Rust module of a library built on another");
    };
    assert!(
        message.contains("`Part` of the `other` library"),
        "{message}"
    );
}

#[test]
fn the_command_writes_and_checks_a_rust_module() {
    let directory = Directory::new();
    let source = directory.0.join("schema.usda");
    std::fs::write(
        &source,
        format!("{CODELESS}class Thing \"Thing\" (inherits = </Typed>) {{ float size }}\n"),
    )
    .unwrap();
    let module = directory.0.join("src/schemas.rs");
    std::fs::create_dir_all(directory.0.join("src")).unwrap();
    let run = |check: bool| {
        let mut args = vec![
            source.display().to_string(),
            "--rust".into(),
            module.display().to_string(),
            "--search".into(),
            directory.0.display().to_string(),
        ];
        if check {
            args.push("--check".into());
        }
        super::run(args.into_iter())
    };
    assert!(run(true).is_err(), "nothing written yet");
    run(false).unwrap();
    run(true).expect("up to date");
    let text = std::fs::read_to_string(&module).unwrap();
    assert!(
        text.contains("pub fn size(&self) -> Option<f32> {\n"),
        "formatted"
    );
    assert!(!directory.0.join("generatedSchema.usda").exists());
    std::fs::write(&module, text.replace("size", "extent")).unwrap();
    assert!(run(true).unwrap_err().contains("schemas.rs"));
}
