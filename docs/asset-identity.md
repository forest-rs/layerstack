# Asset identity during package preparation

A direct asset-valued `identifier` in a prim's `assetInfo` dictionary describes
that model's identity. It does not require the identified asset to exist for the
current scene to be packaged. Package preparation preserves the authored spelling,
reports an ignored identity use, and neither loads it nor requests an inclusion
decision from the resource callback.

The authored-asset APIs still inventory this value and allow a caller to rewrite
it explicitly. `AssetDependencyKind::Identity` distinguishes it from resource
uses, even when both use the same asset spelling in the same metadata dictionary.
Other `assetInfo` assets remain required resources. An `identifier` nested inside
another dictionary, an array, property/layer metadata or reference customData has
no special identity role. Variant-qualified prim identities follow the same rule.

## Updating callers

Exhaustive matches on `AssetDependencyKind` must handle the new `Identity` variant.
Code that previously treated every `Value` dependency as a required resource can
handle `Identity` separately or return `DependencyTarget::Ignored`. Generic
inventory and copy-based rewriting continue to include both roles. Packaging
performs this distinction automatically; other host dependency policies remain
explicit.
