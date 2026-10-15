# Stage workflow probes

Follow the [measurement protocol](MEASUREMENT.md) when extending these workloads or
comparing results. It defines phase boundaries, equivalence checks, ownership
metrics and external-dataset controls.

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
does not establish a speedup; that version still copied numeric arrays between
the intermediate document and writer specs. Results are recorded in
`scalar-writer.csv`, `scalar-save-*.csv` and `scalar-save-memory.json` beside the
earlier phase results.

Layer saves now consume their temporary document, moving numeric arrays, strings
and other owned buffers into writer specs. Authored layers and retained snapshots
keep their ownership. Relative to `b04ba9cd`, the same wide probe's peak RSS fell
from 69,648,384 bytes to 53,460,992 / 53,182,464 bytes (about 66 to 51 MiB, a 23%
reduction), with byte-identical exported files. This is process peak RSS,
including allocator retention, rather than a live-allocation measurement.

The `usdc_layer_save` benchmark prepares four distinct float arrays outside the
timer, either as defaults or as two time samples. It also measures the borrowed
`write_document` API, whose input remains intact. On the same host/profile:

| Save workload | Before (µs) | After, two runs (µs) |
|---|---:|---:|
| Layer, static, 64 elements per array | 15.37 | 15.63 / 15.55 |
| Layer, animated, 64 elements per array | 16.51 | 16.59 / 16.93 |
| Borrowed document, static, 64 elements | 13.50 | 14.00 / 14.00 |
| Borrowed document, animated, 64 elements | 14.45 | 15.26 / 15.38 |
| Layer, static, one million elements per array | 18,934 | 18,420 / 18,240 |
| Layer, animated, one million elements per array | 19,644 | 18,580 / 18,678 |
| Borrowed document, static, one million elements | 18,212 | 18,119 / 18,513 |
| Borrowed document, animated, one million elements | 18,855 | 18,968 / 18,761 |

The large layer saves improve about 3–5%; large borrowed documents remain near
baseline. Tiny borrowed documents regress about 4–6% (0.5–0.9 µs): the shared
lowering clones the document structure before consuming its buffers. This is a
measured tradeoff for one lowering implementation, not an across-the-board speed
claim. Raw means and 95% confidence intervals are in `owned-save.csv`; phase CSVs
and peak counters are in `owned-save-*.csv` and `owned-save-memory.json`.

```sh
cargo bench -p wind_tunnel --bench usdc_write -- usdc_layer_save --warm-up-time 1 --measurement-time 1 --sample-size 10
```

Integer compression counts one candidate delta separately, avoiding tree lookups
for its occurrences while preserving exact frequencies and the largest-delta tie
rule. The candidate is the first inter-element delta, which covers the usual step
in ramps and periodic ranges. Every other delta still enters the exact histogram;
no sample or guessed mode determines the encoding.

Relative to `7d9ace51`, on the same host/profile, Criterion means were:

| Integer workload, 65,536 elements | Before (µs) | After, two runs (µs) |
|---|---:|---:|
| Ramp, i32 | 163.52 | 78.41 / 78.89 |
| Ramp, i64 | 168.17 | 72.43 / 76.20 |
| Periodic, i32 | 216.46 | 121.86 / 130.30 |
| Periodic, i64 | 210.80 | 113.65 / 113.01 |
| Alternating, i32 | 223.51 | 168.61 / 175.36 |
| Alternating, i64 | 249.29 | 193.28 / 203.16 |
| Random, i32 | 5,468.68 | 5,467.94 / 5,546.04 |
| Random, i64 | 6,092.43 | 6,170.84 / 6,128.93 |

Structured integer arrays improve about 19–57%; random arrays range from unchanged
to 1.4% slower. Random deltas rarely hit the separate counter. The corresponding
four-array layer saves improve about 26–35%: static means fall from 19.49 ms to
12.63 / 12.61 ms, and animated means from 19.08 ms to 13.13 / 14.16 ms. These float
fixtures select integer encoding; the results do not imply a speedup for arbitrary
floating-point payloads. Tiny inputs and borrowed-document cases are included in
`delta-writer.csv`, with means and 95% confidence intervals. Fixtures are prepared
outside the timer; each measurement uses 1 s warm-up, 1 s measurement and 10 samples.

Wide, deep and animated exports are byte-identical to the baseline, recorded in
`delta-output-checks.json`. Their single-run phase CSVs (`delta-before-*.csv` and
`delta-after-*.csv`) provide workflow context, not additional statistical timing
evidence. Tests independently check the chosen mode, ties, wrapping and round trips
in both integer widths.

```sh
cargo bench -p wind_tunnel --bench usdc_write -- 'usdc_(integer_write|layer_save)' --warm-up-time 1 --measurement-time 1 --sample-size 10
```

`save_retention` measures source ownership across warming, saving, reopening and
reading again. It copies a source to a fresh destination, warms its array
properties, edits documentation metadata and saves the dirty root. Assertions
check that save marks the layer clean, preserves its authored generation and emits
no scene edit. With `snapshot`, an authored snapshot holds the old sources until
the final phase; without it, the probe checks that reload releases their decoded
arrays. Destinations must not exist.

```sh
cargo run --release -p wind_tunnel --example save_retention -- source.usdc fresh.usdc
cargo run --release -p wind_tunnel --example save_retention -- source.usdc fresh-snapshot.usdc snapshot
cargo run --release -p wind_tunnel --example workflow_fixtures -- metadata-fixtures 64 16384
```

The last command produces a wide 16,384-prim scene with four 64-element arrays;
deep and animated fixtures keep 256 prims. Two independent runs per configuration
on the same host/profile, after the integer optimization, show:

| Ownership workload | Four million floats, 256 prims | 256 floats, 16,384 prims |
|---|---:|---:|
| Save, including filesystem write | 21.9–23.4 ms | 364–369 ms |
| Open saved crate tables, per call | 33–35 µs | 2.32–2.40 ms |
| Explicit reload, cold | 2.10–2.11 ms | 126–143 ms |
| Read all arrays again | 6.1–7.6 ms | 14–16 µs |
| Decoded bytes retained after save | 16,000,000 | 1,024 |

Table opening is measured over 100 calls with saved bytes already in memory;
divide the `open_saved_tables_100` wall time by 100. It includes decoding structural
tables and building the crate lookup index, but excludes high-level layer assembly
and stage composition. The metadata workload was run without an old snapshot;
the numeric workload includes both snapshot configurations. These are phase probes,
not statistical latency benchmarks.

Save retains the old decoded arrays. Explicit reload drops them when no snapshot
holds them, starts the new source at zero decodes and decodes four arrays on the
next complete read. A held snapshot keeps all 16 MB of old arrays through that
read, alongside the new 16 MB; dropping it releases the old arrays. The inspection
handle deliberately keeps the old encoded owner (7,290 bytes) alive throughout.
RSS does not fall when native buffers are released: allocator retention and the
reload's temporary ownership affect process residency. Native element counters
measure these buffers' ownership, not total live allocation.

Explicit reload is a measurement surrogate, not a silent post-save rebind: it
reopens the source, assembles layers, recomposes, advances generation and publishes
a resync. Writer-table reuse and silent post-save rebinding are not implemented.
The writer owns encoded output and its tables; the I/O layer owns successful write
publication; authored generations and scene notices describe edits. Any storage
rebind must preserve those distinctions and existing snapshots. Keeping warmed
caches avoids the measured decode cost; replacing them trades that latency for
released native ownership. Reusing tables alone addresses the much smaller table
opening cost and does not release decoded arrays. Raw results are in
`retention-plain-*.csv`, `retention-snapshot-*.csv` and `retention-metadata-*.csv`.

The `encoded_ownership` benchmark measures transferring an already-read vector
into shared immutable encoded storage, excluding input allocation, initialization
and output destruction. Vector capacity remains retained and accounted; explicit
detachment releases a view's large backing owner. This measures ownership
transfer, not file I/O bandwidth or whole-stage opening.
