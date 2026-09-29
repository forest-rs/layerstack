# Retained queries: boundary and cost contract

LiveStage owns composed change notification; RetainedQueries owns recipes,
answers and computation caches. Authoring stays with the host. QuerySession is an
optional convenience wrapper, not the only way to observe a scene.

The [stage-notification API](stage-notices.md) provides callbacks and independent
bounded-history cursors. RetainedQueries uses that cursor protocol; it does not
create a second notification system.

`queries.view(&mut live, &mut store)` synchronizes external source edits, consumes
this observer's pending reports, and returns a borrowed QueryView for any number
of polls. Drop the view before authoring, then acquire another view afterwards.
Each observer reads independently. History loss clears domain caches and reports
`QueryCause::HistoryLost` on the next evaluation; another stage is rejected.

Synchronization scans participating layer generations once per view, at
O(participating layers), not once per poll. Layer methods and direct transactions
are detected without a query-specific authoring wrapper. Raw importer-field writes
still require Layer::touch. See the notification contract for custom stores,
layer replacement, asset-resolution changes and callback behavior.

Queries are addressed by monotonically allocated, observer-local handles. Removing
one retires its handle permanently. A query follows a namespace path, not a durable
object identity; deletion produces a missing result, recreation can produce a new
result. Query handles are not serializable object identities.

Polling validates only the requested query. Transform recipes retain ancestor
paths up to reset boundaries; bounds reuse the existing cache's hierarchy/reduction
state and conservatively watch the queried namespace plus ancestor transforms;
shading retains visited properties including failed/missing targets. These recipes
have deliberately different representations, with a common report/poll contract.

A poll separates evaluated, answer_changed, dependencies_changed and
provenance_changed. An authored edit need not change the answer. A provider switch
may preserve its value but change its identity and source evidence. Time invalidates
value evaluation, not shading topology. Causes retain at most eight distinct items;
additional causes are counted, never silently presented as a complete history.

The initial router visits registered queries for each change batch. This is an
explicit O(subscriptions × reported changes) prototype, not a claimed indexed
scheduler. Polling a clean query allocates nothing. Domain caches retain their
current algorithms. Work counters and retained entry counts expose overhead.

## Using the implementation

`layerstack_schemas::retained::RetainedQueries` and `QuerySession` are available with `usd-geom` and
`usd-shade`. Run `cargo run -p layerstack_examples --bin retained_queries` for
three independent queries and a callback observing a host-owned stage.

`observe` creates a lazy subscription; `poll` returns a borrowed answer and change
flags relative to its previous poll. Polling can report evaluation with no answer
change. Each update includes bounded reasons and actual transform, bounds and
shading traversal work. `dependencies` exposes the last retained recipe. The
existing Stage value-explanation APIs provide current source opinions, layer
mappings, interpolation and fallback details on demand; no full opinion stacks
are duplicated in every subscription.

Transform and bound queries share one transform cache. Time-only evaluation
retains both ancestor recipes and shading provider/dependency storage.

Time advances with an observer epoch in O(1). Static transform and shading queries
retain answers. Bounds delegate temporal validation to BoundsCache. Removing a
query releases its answer and evidence; domain caches remain reusable until the
query collection is dropped. Handles are observer-local and never reused.

QuerySession bundles an exclusive store, LiveStage and RetainedQueries. Its
`edit`, `apply` and `edit_sources` methods remain useful for small applications.
It consumes the same journal as standalone observers. Exclusive ownership allows
clean polling without rescanning source generations. Its arbitrary-source-edit
closure requests a conservative recompose, including after a caught panic.
Independent interior-mutable host handles must still honor source generation and
synchronization contracts; no observer can discover unreported raw memory changes.

Host-owned stages should enable `StageOptions::with_provenance` for winning-layer
explanations. QuerySession enables it automatically. Query handles are scoped to
the RetainedQueries instance that issued them, not durable scene identities.

Precise property inventories currently cover transactions made solely of existing
property defaults, samples, target lists or metadata. Other edits remain
conservative. Bounds routing watches a namespace region, not a minimal list of
actual source reads. This can reconsider an unaffected query, but its domain cache
can still reuse computation. A result is compared by representation, including
float bits, with no tolerance. Shader outputs are identified, never executed.

There is intentionally no background scheduling, general
DependencyGraph, evaluation IR or durable object identity. Those require separate
consumer evidence. An index for routing can replace the initial scan without
changing the explicit polling and evidence contract.

## Vocabulary

- **Stamp:** evidence of an edit or time epoch, independent of whether a value
  actually changed. The session owns these clocks; caches own their validation.
- **Dependency:** an input whose change may affect an answer, including a missing
  property whose later creation can make a previously failed branch succeed.
- **Recipe:** retained knowledge of how to compute a query. It can be ancestor
  paths, a shading traversal, or the bounds cache's existing reduction state.
- **Validation frontier:** the portion of a recipe that needs examination after
  a change; caches can stop at already-current dependencies.
- **Retirement:** removing a query or invalidating structural state so an old
  slot/recipe cannot silently stand for a newly created object.
- **Reduction:** an aggregate built from contributions; bounds retain wide
  reductions and replace dirty contributions rather than folding all children.
- **Provenance:** provider/connection identity and authored source evidence. It is
  separate from both the evaluated value and the reason a query was reconsidered.
- **Evaluation context:** time, interpolation and the session's fixed bounds
  policies. Context changes are not authored source edits.
