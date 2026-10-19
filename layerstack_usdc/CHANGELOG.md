# Changelog

## Unreleased

Initial release.

- Indexed reads can borrow string/token scalars and retain compact text-array
  indices without per-element owned strings.
- Immutable files can materialize an explicit authored-spec selection into an
  editable layer without first constructing the whole layer.
