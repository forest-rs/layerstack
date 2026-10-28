# Changelog

## Unreleased

Initial release.

- Regenerates `layerstack_schemas`' tables and views from a usd-core wheel and
  the OpenUSD sources of its release, with `--check` for CI.
- `compile`: a custom `schema.usda` library into the `generatedSchema.usda` and
  `plugInfo.json` OpenUSD's usdGenSchema writes, checked against its rules, and
  into a Rust module of typed views, edit handles and token enums
  (`--rust`, or `CompiledSchema::rust_module` from a build script).
- Shader-node, MaterialX node and kind-plugin generators.
