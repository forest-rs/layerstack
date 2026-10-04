# Changelog

## Unreleased

Initial release.

- `LayerStore::identity` retains token/path domain affinity. `Stage` captures it;
  `PrimSnapshot`, `AttributeQuery::is_current` and `RelationshipQuery` expose
  current composition evidence without resolving unchanged inputs.
- Authored and composed change histories expose configurable batch/byte budgets
  and memory/work counters. Defaults are now 64 batches **and 1 MiB** per history.
  Migration: consumers may encounter expiry earlier than the former batch-only
  limit; rebuild notice-dependent caches on `ChangeHistoryError::Expired` (the
  cursor advances) or explicitly raise the budget. Source synchronization uses
  conservative recomposition when authored evidence is lost.

- `Layer::prim_at` reads an exact authored prim site, including variant-branch
  prims, without substituting selected or composed prims. This additive API
  supports validation of caller-owned publication targets.
- Prototype record sharing buckets source identities and mapped paths before
  exact, bit-preserving comparison. Material connections no longer compare every
  earlier occurrence when their remapped targets differ. Value refreshes revisit
  only changed member groups; empty refreshes skip resharing entirely.
