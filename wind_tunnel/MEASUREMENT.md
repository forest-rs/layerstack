# Measurement protocol

Use benchmarks to establish a specific claim about a reproducible workload.
The [workflow probes and recorded results](README.md) cover traversal, retained
numeric reads, encoding and save ownership. Their conclusions apply to those
fixtures and timer boundaries.

## Record the experiment

Record baseline and candidate revisions, dirty-tree changes, Rust/profile/target,
host/OS, feature flags, command, repetitions and raw output. Identify generated
fixture parameters or an external dataset's revision/checksum and entry point.
Name the hypothesis and metric before interpreting a result.

Compile outside the measured process. Prepare fixtures outside benchmark timers.
Separate transport, format opening, authored-layer assembly, composition, traversal,
first demand, repeat demand, edits, synchronization, serialization, filesystem
publication and destruction. State exactly which phases a reported total includes.
Retain failures as results; a skipped or incomplete operation is not a fast success.

Distinguish warmed process caches from filesystem caches and retained decoded
values. A new process does not guarantee cold filesystem I/O. Do not label a run
"cold" without naming what was reset. Use independent process repetitions for
process-memory claims, and alternating baseline/candidate runs to reduce drift.
Keep competing builds and heavy workloads out of timing runs. Profile separately:
sampling and allocation instrumentation can alter the workload's timing.

## Prove equivalent work

Before comparing timings, compare ordered namespace results, requested values,
source evidence where relevant, diagnostics and output fidelity. Match population
masks, payload rules, active/defined/loaded predicates, instance-proxy policy,
time/interpolation and recovery policy. Different load scopes need separate labels.

Raw archive opening, root-layer export and dependency-complete packaging are
different workloads. Byte-identical output is useful for an encoding change;
independent semantic reimport is needed when representations legitimately differ.
Compare incremental results with a fresh composition of the same authored state.

## Workload controls

| Workload | Vary independently and observe |
| --- | --- |
| Traversal | Width, depth, subtree roots, flags and instances; ordered results and visited work |
| Retained reads | Metadata-only inspection, first array, all arrays, repeat reads and malformed first demand; decode counts and retained ownership |
| Incremental edits | Changed region versus unrelated background, layer count and dependency fanout; recomposition, routing and domain computation work |
| Instances | Shared asset size versus occurrence count and distinct buffers; prototype and occurrence work separately |
| Save | Tiny/large, numeric/metadata-heavy, static/animated and compressibility; encoding, write, readback and exact input preservation |
| Snapshot retention | Warm save/reload with and without old holders; old/new encoded and decoded owners, then holder release |
| Packages and clips | Nesting, member count, dependency closure and queried interval; directory/CRC work, requested files and confirmed gaps |

For retained-query routing, increase subscription count while holding the changed
region fixed. A small list of updated paths is not evidence of bounded work if the
router still scans every subscription. Similarly, a lazy API is not evidence of
cheap opening if unrelated nonnumeric fields are materialized during assembly.

## Interpret memory and timing precisely

- RSS and peak RSS include allocator retention and other process residency; they
  do not measure live object ownership or fragmentation directly.
- Encoded owner bytes, shared views, copied bytes, native element bytes and decoder
  units describe different resources. Report their scope and holders. Catalog
  counters do not include every external snapshot automatically.
- Logical copied bytes are not allocation traffic. Use a host allocation profiler
  to attribute live allocations, transient peaks and allocator behavior.
- Releasing an owner can reduce live allocations without reducing RSS. Replacing
  a warmed source can release memory and impose a later decode cost; report both.
- Inclusive CPU samples contain child calls and cannot be summed as independent
  costs. A hotspot from an earlier revision must be reprofiled after optimization.
- Report distributions or confidence intervals for benchmark claims. Single-run
  phase probes locate costs but do not establish a statistical speedup. Include
  regressions and absolute costs alongside improvements.

## External scene workloads

Keep large datasets opt-in and caller supplied. Record the source revision, asset
inventory and unresolved dependencies without committing external scene assets.
Select both native-instanced and expanded entry points when available. Dispatch
formats by content, since a `.usd` path can contain text or binary data.

Measure metadata opening, geometry demand, instance/proxy traversal, local edits,
shared-asset fanout and relocated-package reopen separately. State whether layer
counts include session layers. Counts of meshes, points and faces help diagnose
missing work but do not establish geometric or rendered equivalence; subdivision,
transforms and material semantics still matter.

Historical experiments remain evidence for their original revisions. Promote their
workload design and reproducible fixtures; rerun before presenting old timings as
current behavior or extrapolating them to general scene performance.
