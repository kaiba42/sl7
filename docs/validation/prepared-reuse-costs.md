# Completed preparation reuse: build comparison

Reusing completed preparations changed the clean Cargo median by +2.1% and the edit median by -0.4% (positive means faster). 2/6 paired edits were faster. Two rounds do not establish a reliable speedup; reuse remains opt-in.

Same executable, two sealing workers, pipeline and compression workers disabled. SL7_REUSE_PREPARED toggles a cache of at most four completed preparations and 16 MiB of retained metadata capacities. Read-only reopen delays publication without discarding unchanged work; any mutation invalidates it. Native data is retained until all readers close.

Median seconds (range):

| Stage | Native Cargo | SL7 / reuse off | SL7 / reuse on |
| --- | ---: | ---: | ---: |
| clean | 150.239 (149.443–151.036) | 171.908 (170.974–172.842) | 168.255 (167.783–168.726) |
| edit | 4.318 (3.743–4.878) | 6.284 (5.402–7.275) | 6.308 (5.439–10.696) |
| unchanged | 0.288 (0.278–0.396) | 0.431 (0.359–0.611) | 0.425 (0.358–0.586) |
| unchanged-after-remount | 0.406 (0.277–0.535) | 0.545 (0.539–0.550) | 0.542 (0.534–0.550) |

## Cargo plus settling

| Stage | Native Cargo | SL7 / reuse off | SL7 / reuse on |
| --- | ---: | ---: | ---: |
| clean | 150.239 (149.443–151.036) | 182.871 (182.340–183.403) | 181.279 (180.488–182.070) |
| edit | 4.318 (3.743–4.878) | 9.899 (7.214–13.623) | 9.544 (7.149–12.506) |
| unchanged | 0.288 (0.278–0.396) | 0.632 (0.561–0.812) | 0.626 (0.560–0.788) |
| unchanged-after-remount | 0.406 (0.277–0.535) | 0.746 (0.741–0.751) | 0.743 (0.735–0.752) |

## Work avoided

| Stage / mode | Hashed MiB median | Discarded MiB median | Reused MiB median | Daemon CPU seconds median |
| --- | ---: | ---: | ---: | ---: |
| clean / SL7 / reuse off | 8462.92 | 708.07 | 0.00 | 72.43 |
| clean / SL7 / reuse on | 7948.45 | 118.76 | 137.33 | 71.34 |
| edit / SL7 / reuse off | 1590.50 | 373.62 | 0.00 | 5.91 |
| edit / SL7 / reuse on | 1236.45 | 19.56 | 242.24 | 5.43 |

## Paired edit waits

| Round / edit | Cargo off → on (s) | Catalog wait off → on (s) | State held off → on (s) | Read service off → on (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / edit-1 | 6.55 → 6.61 | 0.14 → 0.16 | 0.26 → 0.25 | 1.70 → 1.26 |
| 1 / edit-2 | 6.02 → 6.01 | 0.15 → 0.21 | 0.37 → 0.31 | 1.33 → 1.88 |
| 1 / edit-3 | 5.40 → 5.44 | 0.09 → 0.13 | 0.33 → 0.34 | 0.51 → 0.55 |
| 2 / edit-1 | 6.58 → 10.70 | 0.13 → 1.43 | 0.28 → 0.54 | 1.61 → 4.13 |
| 2 / edit-2 | 7.28 → 5.73 | 0.12 → 0.14 | 0.32 → 0.29 | 1.10 → 0.84 |
| 2 / edit-3 | 5.50 → 8.35 | 0.10 → 1.78 | 0.31 → 1.31 | 0.52 → 0.53 |

The two slower enabled edits in round 2 coincided with much larger foreground catalog-wait totals. This suggests investigating publication scheduling and catalog serialization. These counters cover Cargo, executable checks and settling, and are cumulative service/wait times that can overlap; they do not establish the critical path or prove that reuse caused each slowdown.

## Retained storage

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.832 |
| SL7 / reuse off | 1.906 |
| SL7 / reuse on | 1.884 |

## Recorded sequence costs

| Round / backend | Cargo (s) | Settling (s) | Drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 163.34 | 0.00 | 0.00 | 163.34 |
| 1 / SL7 / reuse off | 192.75 | 20.92 | 0.60 | 214.28 |
| 1 / SL7 / reuse on | 187.77 | 24.35 | 0.57 | 212.69 |
| 2 / SL7 / reuse on | 195.39 | 18.50 | 0.57 | 214.47 |
| 2 / SL7 / reuse off | 192.28 | 25.35 | 0.59 | 218.21 |
| 2 / Native Cargo | 165.47 | 0.00 | 0.00 | 165.47 |

This recorded sum excludes executable-check duration, setup, remount startup and integrity scans. It is not total elapsed time or a native-versus-SL7 durability comparison.

All 48 Cargo commands, 32 artifact-graph comparisons, executable/freshness checks, and four integrity checks passed. Incremental output was present. Every SL7 stage drained, recorded no sealing errors, and respected cache limits. Linux validation passed 90 tests in each mode, plus 47 host tests, 11 Python tests, strict Clippy and all five mounted suites.

Two counterbalanced rounds on one shared 8-CPU/12-GiB node; no statistical significance claim. Warm OS caches; debug profile with incremental artifacts verified. Fixed private workload revision. Cargo wall time excludes executable checks and subsequent sealing. Cargo plus settling excludes executable-check duration and does not establish native durability equivalence. Each SL7 stage waits for three stable quiescent observations, including at least 0.2s observation delay. Native has no SL7 settling wait. Reused bytes count preparation attempts skipped, not necessarily unique bytes or completed publications. Preparation and lock service times overlap and cannot be treated as added build latency.

Implementation: `15fb0607fc5ee11ed7c9d22c6c137ca4d5c416c2`.

[HTML](prepared-reuse-costs.html) · [JSON](prepared-reuse-costs.json) · [Validation](prepared-reuse-validation.json) · [CPU profiles](prepared-reuse-costs-profile.md)
