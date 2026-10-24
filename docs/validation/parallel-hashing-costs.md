# Parallel chunk hashing: build comparison

Two shared hash workers changed the clean Cargo median by +2.7% and the edit median by +0.4% (positive means faster). 3/6 paired edits were faster. The clean-build change must be read alongside the paired rebuilds and settling costs below; two rounds do not establish statistical significance. CPU profiles separately verify execution on hash workers. The pool remains opt-in.

The same binary runs with SL7_HASH_WORKERS=0 and SL7_HASH_WORKERS=2. Independent chunk identities from files at least 1 MiB run on one mount-wide pool; splitting, whole-file SHA, catalog access and pack consumption remain ordered. Both cases use two sealing workers with import pipeline, compression workers and completed-preparation reuse disabled. Exact chunk identities, pack bytes and publication durability are preserved.

Median seconds (range):

| Stage | Native Cargo | SL7 / serial hashing | SL7 / two hash workers |
| --- | ---: | ---: | ---: |
| clean | 149.092 (148.463–149.721) | 171.049 (170.424–171.675) | 166.363 (166.297–166.429) |
| edit | 4.215 (3.754–4.837) | 6.522 (5.390–10.180) | 6.494 (5.525–10.048) |
| unchanged | 0.489 (0.385–0.611) | 0.429 (0.348–0.606) | 0.410 (0.352–0.583) |
| unchanged-after-remount | 0.277 (0.273–0.280) | 0.542 (0.538–0.545) | 0.536 (0.533–0.539) |

## Focused repeat-import throughput

| Hash workers | Repeat-import median (ms) | Range (ms) | Peak overlapping jobs |
| --- | ---: | ---: | ---: |
| 0 | 101.04 | 98.59–103.27 | 0 |
| 1 | 75.82 | 75.37–76.44 | 1 |
| 2 | 76.47 | 75.95–77.71 | 2 |
| 4 | 77.03 | 76.88–79.18 | 2 |

Six alternating-order samples per mode on node middle, one 64 MiB artifact already present in the catalog. Includes reading, splitting, whole-file SHA, chunk identities, catalog queries and extent construction; excludes publication and compression of new content. File digests were checked after every sample. This focused result is not a Cargo speedup.

## Cargo plus settling

| Stage | Native Cargo | SL7 / serial hashing | SL7 / two hash workers |
| --- | ---: | ---: | ---: |
| clean | 149.092 (148.463–149.721) | 182.513 (180.176–184.850) | 178.280 (176.256–180.304) |
| edit | 4.215 (3.754–4.837) | 11.014 (7.430–13.256) | 11.019 (7.003–12.968) |
| unchanged | 0.489 (0.385–0.611) | 0.631 (0.550–0.807) | 0.612 (0.554–0.784) |
| unchanged-after-remount | 0.277 (0.273–0.280) | 0.743 (0.739–0.746) | 0.737 (0.735–0.740) |

## Work and CPU

| Stage / version | Hashed MiB median | Discarded MiB median | Daemon CPU seconds median |
| --- | ---: | ---: | ---: |
| clean / SL7 / serial hashing | 8141.92 | 424.61 | 71.28 |
| clean / SL7 / two hash workers | 8073.24 | 362.22 | 72.56 |
| edit / SL7 / serial hashing | 1231.07 | 15.32 | 5.41 |
| edit / SL7 / two hash workers | 1465.87 | 248.99 | 5.85 |

## Paired edit waits

| Round / edit | Cargo off → on (s) | Catalog wait off → on (s) | State held off → on (s) | Read service off → on (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / edit-1 | 7.12 → 10.05 | 0.12 → 0.03 | 0.26 → 0.27 | 1.42 → 5.01 |
| 1 / edit-2 | 5.82 → 5.62 | 0.12 → 0.10 | 0.31 → 0.30 | 1.01 → 1.06 |
| 1 / edit-3 | 7.28 → 7.36 | 1.43 → 0.84 | 0.49 → 1.24 | 0.52 → 0.54 |
| 2 / edit-1 | 10.18 → 9.75 | 0.42 → 0.03 | 0.43 → 0.87 | 3.64 → 3.15 |
| 2 / edit-2 | 5.93 → 5.60 | 0.11 → 0.09 | 0.31 → 0.30 | 0.96 → 0.89 |
| 2 / edit-3 | 5.39 → 5.52 | 0.07 → 0.05 | 0.30 → 0.33 | 0.53 → 0.51 |

These counters cover Cargo, executable checks and settling, and are cumulative service/wait times that can overlap. They expose changes in catalog contention and filesystem work but do not establish the critical path or prove causality for individual slow builds.

## Retained storage

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.832 |
| SL7 / serial hashing | 1.901 |
| SL7 / two hash workers | 1.882 |

## Recorded sequence costs

| Round / backend | Cargo (s) | Settling (s) | Drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 163.75 | 0.00 | 0.00 | 163.75 |
| 1 / SL7 / serial hashing | 193.82 | 25.15 | 0.59 | 219.56 |
| 1 / SL7 / two hash workers | 191.21 | 16.80 | 0.60 | 208.61 |
| 2 / SL7 / two hash workers | 189.18 | 25.44 | 0.58 | 215.20 |
| 2 / SL7 / serial hashing | 193.85 | 20.12 | 0.58 | 214.55 |
| 2 / Native Cargo | 163.00 | 0.00 | 0.00 | 163.00 |

This recorded sum excludes executable-check duration, setup, remount startup and integrity scans. It is not total elapsed time or a native-versus-SL7 durability comparison.

All 48 Cargo commands, 32 artifact-graph comparisons, executable/freshness checks, and four integrity checks passed. Incremental output was present. Every SL7 stage drained, recorded no sealing errors, and respected cache limits. Linux validation passed 94 tests, plus 51 host tests, 18 Python tests, strict Clippy and all five mounted suites.

Two counterbalanced rounds on one shared 8-CPU/12-GiB node; no statistical significance claim. Warm OS caches; debug profile with incremental artifacts verified. Fixed private workload revision. Cargo wall time excludes executable checks and subsequent sealing. Cargo plus settling excludes executable-check duration and does not establish native durability equivalence. Each SL7 stage waits for three stable quiescent observations, including at least 0.2s observation delay. Native has no SL7 settling wait. Cancellation timing changes hashing volume between runs because of scheduling and bounded read-ahead. Preparation and lock service times overlap and cannot be treated as added build latency.

Implementation: `218a79d5f409bd1dbd22bb422f45f2951cb8f5bb`.

[HTML](parallel-hashing-costs.html) · [JSON](parallel-hashing-costs.json) · [Validation](parallel-hashing-validation.json) · [CPU profiles](parallel-hashing-costs-profile.md)
