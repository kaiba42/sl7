# Bounded large-artifact import pipeline

Clean-build medians: Native Cargo 154.50s, SL7 / prior implementation 171.07s, SL7 / pipeline off 174.18s, SL7 / pipeline on 170.73s. One-line rebuild medians: Native Cargo 4.42s, SL7 / prior implementation 7.77s, SL7 / pipeline off 6.08s, SL7 / pipeline on 8.42s.

With the pipeline enabled, the clean median was 2.0% faster and the edit median was 38.5% slower than the same binary with it disabled. It won 3 of six paired edits. The pipeline remains opt-in. Some unmounts still needed up to 6.76s to drain work after CPU-idle observation; these measurements describe the recorded build sequence and do not isolate fully settled edit builds. Two rounds do not establish statistical significance.

All SL7 cases use two sealing workers. Pipeline off and on use the same binary. For files at least 16 MiB, the enabled pipeline reads, chunks and hashes the whole file on a producer thread while the sealing worker hashes individual chunks, checks the catalog, compresses and writes packs. Its queue is bounded to two batches of 16 chunks. The prior implementation is included to expose any serial refactoring cost. Pack bytes, hashes, schema, same-inode exclusion and publication durability are unchanged.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, eight cases and 64 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, Oaktown middle, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / prior implementation | SL7 / pipeline off | SL7 / pipeline on |
| --- | ---: | ---: | ---: | ---: |
| Clean build | 154.50 (154.34–154.67) | 171.07 (170.16–171.97) | 174.18 (172.46–175.89) | 170.73 (169.55–171.91) |
| One-line rebuild | 4.42 (3.85–6.31) | 7.77 (6.01–10.63) | 6.08 (5.47–9.08) | 8.42 (5.36–9.86) |
| Unchanged build | 0.42 (0.29–1.11) | 0.48 (0.35–3.81) | 0.42 (0.36–5.58) | 0.49 (0.35–2.04) |
| Unchanged after remount | 0.35 (0.28–0.43) | 0.61 (0.59–0.64) | 0.68 (0.59–0.77) | 0.58 (0.57–0.58) |

SL7 / pipeline off was faster than the prior-implementation baseline in 4 of 6 paired edits. SL7 / pipeline on was faster than the prior-implementation baseline in 3 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Peak candidate phases | Candidate catalog wait (s) | Pipelined preparations |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / prior implementation | 2 | 2 | 44.17 | 119.58 | 50.14 | 29.97 | 15.74 | 2 | 8.26 | not instrumented |
| 1 / SL7 / pipeline off | 2 | 2 | 49.28 | 111.72 | 46.29 | 30.82 | 12.39 | 2 | 7.93 | 0 |
| 1 / SL7 / pipeline on | 2 | 2 | 46.47 | 105.77 | 51.95 | 29.44 | 14.60 | 2 | 9.06 | 95 |
| 2 / SL7 / pipeline on | 2 | 2 | 48.08 | 109.77 | 45.01 | 29.01 | 13.67 | 2 | 8.44 | 94 |
| 2 / SL7 / pipeline off | 2 | 2 | 43.31 | 107.28 | 43.27 | 29.30 | 15.05 | 2 | 8.04 | 0 |
| 2 / SL7 / prior implementation | 2 | 2 | 50.12 | 109.89 | 43.53 | 29.62 | 14.44 | 2 | 7.13 | not instrumented |

Peak phase counts measure overlap, not CPU utilization. Pipeline preparation counts include canceled attempts. Peak phase counts alone do not prove CPU parallelism. Candidate service time excludes waits for the catalog permit; every implementation in this comparison records those waits separately. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 172.20 | 0.00 | 0.00 | 172.20 |
| 2 / Native Cargo | 168.92 | 0.00 | 0.00 | 168.92 |
| 1 / SL7 / prior implementation | 201.56 | 8.32 | 0.65 | 210.52 |
| 2 / SL7 / prior implementation | 196.77 | 12.23 | 5.90 | 214.89 |
| 1 / SL7 / pipeline off | 197.74 | 14.23 | 6.76 | 218.73 |
| 2 / SL7 / pipeline off | 202.92 | 13.43 | 0.66 | 217.01 |
| 1 / SL7 / pipeline on | 197.19 | 15.23 | 0.66 | 213.08 |
| 2 / SL7 / pipeline on | 197.80 | 6.72 | 5.27 | 209.78 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / prior implementation | 1.98 (1.90–2.06) |
| SL7 / pipeline off | 1.89 (1.89–1.89) |
| SL7 / pipeline on | 1.95 (1.89–2.00) |

All 64 Cargo commands, 48 artifact-graph comparisons, freshness/executable checks, incremental-output checks and six store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `40f34e887920e0687282e157b64ed6d239743f19`.

[HTML tables](import-pipeline-costs.html) · [JSON measurements](import-pipeline-costs.json) · [Validation](import-pipeline-validation.json)
