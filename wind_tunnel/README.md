# Stage workflow probes

These examples use the public composition and document APIs. Build once, then run
the executables directly so compiler memory and time are outside measurements:

```sh
cargo build --release -p wind_tunnel --example prim_traversal --example workflow_fixtures --example stage_workflows
target/release/examples/prim_traversal
target/release/examples/workflow_fixtures .local/workflow-fixtures 1000000
target/release/examples/stage_workflows .local/workflow-fixtures/wide.usdc .local/workflow-fixtures/wide-export.usdc
```

Repeat the last command for `deep.usdc`, `animated.usdc` and `nested.usdz`, using a
distinct output path each time. `malformed.usdc` exercises checked import failure.
On macOS, `/usr/bin/time -l` around the built executable reports process peak RSS.
Use a host allocation profiler for live allocation/fragmentation attribution.

`prim_traversal` excludes setup and composition, checks ordered namespace equality,
and reports a median of seven warmed windows of at least 75 ms. An optional output
argument records ordered prim paths. Inspecting all prims and applying ALL/DEFAULT
predicates are measured separately. Ancestor state now travels with the iterator
stack; the root still resolves its ancestor state when a range starts in a subtree.
There is no persistent status cache to invalidate after edits.

Measured on macOS 27.0.1 arm64 with Rust 1.98.1, release profile, 2026-10-11:

| DEFAULT traversal | Before (µs) | After, two runs (µs) |
|---|---:|---:|
| Wide 64 | 7.20 | 1.51 / 1.44 |
| Wide 256 | 30.18 | 6.12 / 6.30 |
| Deep 64 | 526.53 | 1.98 / 2.12 |
| Deep 128 | 3,692.05 | 4.03 / 4.17 |
| Deep 256 | 27,917.71 | 8.31 / 8.96 |

The deep 256 case improves by approximately 3,100–3,400×; the wide case by about
5×. Ordered namespace identities match the baseline. These synthetic results
establish an algorithmic traversal improvement, not an end-to-end scene speedup.
The predicate regression checks 486 flag/proxy combinations at every fixture root;
native stage traversal cases additionally exercise instances and pruning.

`stage_workflows` reports each phase before sampling current RSS, so process
sampling is outside wall time. It distinguishes encoded owner bytes/copies,
package directory work, decode attempts, cached native element bytes and remaining
decoder work units. RSS is in KiB, or `unavailable` when the host denies sampling;
it is neither live allocation size nor mapped address space. Decoder work units
are not memory bytes. Catalog ownership excludes old external snapshot holders.

The generated fixtures contain 256 prims and four distinct, compressible arrays
of one million floats (five arrays in the deep fixture). Animated arrays have two
sample times. Nested USDZ wraps the wide file and an opaque resource. In the
recorded phase runs:

- Opening, traversal and declaration inventory decode zero numeric arrays.
- First demand decodes one 4 MB native array; complete demand decodes 16 MB
  (20 MB deep). Repeat demand adds no decode attempts.
- Nested import retains one 7,895-byte encoded owner, parses two directories once,
  and copies no individual member payloads.
- Process peak RSS is about 102 MiB wide, 133 MiB deep/animated and 31 MiB nested.
  The nested export writes its authored root layer; it does not serialize the
  referenced numeric member. Its smaller peak is not a comparable full-scene save.
- Warm animated interpolation still costs about 1 ms for four arrays despite
  decoding nothing further. Result materialization remains measurable work.

The edit phase changes documentation metadata, synchronizes, then restores its
inverse outside timing before export. Export serializes only the authored root
layer to a separate file. A complete dependency package is a separate
`StageDocument::prepare_package` / `export_package` workflow.

Raw CSVs and environment/peak counters are in
[results/stage-workflows-2026-10-11](results/stage-workflows-2026-10-11).
They characterize these workloads; general lazy-field memory still needs
allocation profiling before further optimization.

Scalar USDC array writing now inspects borrowed native elements, creating neither
an `f64` copy nor a little-endian copy of the whole input before choosing its
encoding. The integer candidate buffer reserves its final capacity. Encoding
choices and output bits are preserved, including signed zero and NaN payloads.
Run the focused benchmarks with fixtures prepared outside measurement:

```sh
cargo bench -p wind_tunnel --bench usdc_write -- usdc_numeric_write --warm-up-time 1 --measurement-time 1 --sample-size 10
```

On the same host/profile above, relative to `76509d15`, Criterion means for one
million elements were:

| Scalar array encoding | Before (ms) | After, two runs (ms) |
|---|---:|---:|
| Float, integral | 5.019 | 4.428 / 4.366 |
| Float, lookup | 9.236 | 4.711 / 4.705 |
| Float, plain | 1.304 | 0.698 / 0.708 |
| Half, lookup | 6.269 | 4.243 / 4.011 |
| Double, lookup | 4.734 | 4.209 / 4.151 |

These large-array cases improve about 11–49%; the 64-element cases remain near
5–6 µs. The wide end-to-end probe's peak RSS fell from 107,298,816 bytes to
69,632,000 / 69,353,472 bytes (about 102 to 66 MiB), with byte-identical exported
files. This measures process peak, including allocator retention, rather than
live buffer ownership. End-to-end serialization wall time remains noisy and
does not establish a speedup; conversion through the intermediate document and
writer specs still copies numeric arrays. Results are recorded in
`scalar-writer.csv`, `scalar-save-*.csv` and `scalar-save-memory.json` beside the
earlier phase results.
