# Catalog statement reuse: build comparison

Reusing INSERT statements changed the clean Cargo median by -0.6% and the edit median by +7.7% (positive means faster). 3/6 paired edits were faster. Two rounds do not establish a reliable speedup; CPU profiles separately check the targeted SQL parsing cost.

Before/after binaries differ only in preparing pack and chunk INSERT statements once per nonempty import instead of once per row. SQL, transactions, chunk identities, compression, publication and durability semantics are unchanged. Both use two sealing workers with import pipeline, compression workers and completed-preparation reuse disabled.

Median seconds (range):

| Stage | Native Cargo | SL7 / before | SL7 / statement reuse |
| --- | ---: | ---: | ---: |
| clean | 149.236 (148.487–149.985) | 169.409 (169.327–169.491) | 170.361 (169.808–170.915) |
| edit | 4.482 (3.782–5.421) | 6.485 (5.396–6.910) | 5.988 (5.414–9.487) |
| unchanged | 0.376 (0.280–0.589) | 0.421 (0.359–2.072) | 0.418 (0.351–0.574) |
| unchanged-after-remount | 0.307 (0.283–0.332) | 0.541 (0.538–0.543) | 0.526 (0.526–0.527) |

## Cargo plus settling

| Stage | Native Cargo | SL7 / before | SL7 / statement reuse |
| --- | ---: | ---: | ---: |
| clean | 149.236 (148.487–149.985) | 179.971 (176.065–183.877) | 180.016 (178.054–181.978) |
| edit | 4.482 (3.782–5.421) | 8.922 (7.107–11.344) | 8.501 (7.123–11.594) |
| unchanged | 0.376 (0.280–0.589) | 0.622 (0.561–2.273) | 0.620 (0.552–0.776) |
| unchanged-after-remount | 0.307 (0.283–0.332) | 0.742 (0.739–0.744) | 0.728 (0.727–0.728) |

## Work and CPU

| Stage / version | Hashed MiB median | Discarded MiB median | Daemon CPU seconds median |
| --- | ---: | ---: | ---: |
| clean / SL7 / before | 8016.25 | 334.87 | 71.31 |
| clean / SL7 / statement reuse | 8270.74 | 498.79 | 71.48 |
| edit / SL7 / before | 1350.28 | 137.60 | 5.26 |
| edit / SL7 / statement reuse | 1459.06 | 246.38 | 5.53 |

## Paired edit waits

| Round / edit | Cargo off → on (s) | Catalog wait off → on (s) | State held off → on (s) | Read service off → on (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / edit-1 | 6.61 → 6.60 | 0.13 → 0.11 | 0.26 → 0.26 | 1.45 → 1.48 |
| 1 / edit-2 | 6.46 → 5.78 | 0.13 → 0.12 | 0.81 → 0.30 | 1.15 → 0.97 |
| 1 / edit-3 | 6.08 → 6.20 | 0.09 → 0.07 | 0.99 → 0.89 | 0.52 → 0.54 |
| 2 / edit-1 | 6.91 → 9.49 | 0.12 → 0.74 | 0.28 → 0.35 | 1.58 → 4.72 |
| 2 / edit-2 | 6.51 → 5.66 | 0.00 → 0.10 | 0.42 → 0.32 | 1.77 → 0.84 |
| 2 / edit-3 | 5.40 → 5.41 | 0.10 → 0.08 | 0.32 → 0.32 | 0.54 → 0.50 |

These counters cover Cargo, executable checks and settling, and are cumulative service/wait times that can overlap. They expose changes in catalog contention and filesystem work but do not establish the critical path or prove causality for individual slow builds.

## Retained storage

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.832 |
| SL7 / before | 1.889 |
| SL7 / statement reuse | 1.905 |

## Recorded sequence costs

| Round / backend | Cargo (s) | Settling (s) | Drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 164.97 | 0.00 | 0.00 | 164.97 |
| 1 / SL7 / before | 190.83 | 24.65 | 0.58 | 216.06 |
| 1 / SL7 / statement reuse | 190.25 | 19.31 | 0.58 | 210.15 |
| 2 / SL7 / statement reuse | 193.34 | 17.40 | 0.61 | 211.35 |
| 2 / SL7 / before | 191.57 | 15.79 | 0.58 | 207.95 |
| 2 / Native Cargo | 163.86 | 0.00 | 0.00 | 163.86 |

This recorded sum excludes executable-check duration, setup, remount startup and integrity scans. It is not total elapsed time or a native-versus-SL7 durability comparison.

All 48 Cargo commands, 32 artifact-graph comparisons, executable/freshness checks, and four integrity checks passed. Incremental output was present. Every SL7 stage drained, recorded no sealing errors, and respected cache limits. Linux validation passed 90 tests, plus 47 host tests, 11 Python tests, strict Clippy and all five mounted suites.

Two counterbalanced rounds on one shared 8-CPU/12-GiB node; no statistical significance claim. Warm OS caches; debug profile with incremental artifacts verified. Fixed private workload revision. Cargo wall time excludes executable checks and subsequent sealing. Cargo plus settling excludes executable-check duration and does not establish native durability equivalence. Each SL7 stage waits for three stable quiescent observations, including at least 0.2s observation delay. Native has no SL7 settling wait. Cancellation timing changes hashing volume between runs even when hashing code is unchanged. Preparation and lock service times overlap and cannot be treated as added build latency.

Implementation: `f22566967c05f55e64b70e82e0be70037550d8fb`.

[HTML](catalog-statements-costs.html) · [JSON](catalog-statements-costs.json) · [Validation](catalog-statements-validation.json) · [CPU profiles](catalog-statements-costs-profile.md)
