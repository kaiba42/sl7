# Reusing the chunker read buffer

Clean-build medians: Native Cargo 149.71s, SL7 / previous chunker 168.93s, SL7 / buffered chunker 167.03s. One-line rebuild medians: Native Cargo 3.80s, SL7 / previous chunker 7.04s, SL7 / buffered chunker 6.60s.

The candidate changed the clean median by 1.1% and the edit median by 6.3% (positive means faster). It remains 11.6% above native for clean builds and 73.7% above native for edits. These two rounds do not establish statistical significance.

Both SL7 cases use two sealing workers with the large-artifact import pipeline disabled. The candidate keeps a 128 KiB read buffer and advances a cursor, compacting only when fewer than 32 KiB remain. The splitter still examines at most 32 KiB. Chunk identities, pack bytes, schema and durability ordering are unchanged. No additional threads are introduced.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, Oaktown top, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / previous chunker | SL7 / buffered chunker |
| --- | ---: | ---: | ---: |
| Clean build | 149.71 (148.60–150.83) | 168.93 (168.65–169.21) | 167.03 (166.32–167.75) |
| One-line rebuild | 3.80 (3.75–4.09) | 7.04 (5.53–9.29) | 6.60 (5.35–9.61) |
| Unchanged build | 0.30 (0.28–4.48) | 0.42 (0.36–2.20) | 0.41 (0.36–2.68) |
| Unchanged after remount | 0.28 (0.27–0.28) | 0.53 (0.52–0.54) | 0.54 (0.54–0.55) |

SL7 / buffered chunker was faster than the previous-chunker baseline in 4 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Peak candidate phases | Candidate catalog wait (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / previous chunker | 2 | 2 | 50.02 | 112.94 | 47.97 | 29.68 | 14.29 | 2 | 8.39 |
| 1 / SL7 / buffered chunker | 2 | 2 | 42.21 | 107.83 | 44.78 | 29.64 | 12.95 | 2 | 7.33 |
| 2 / SL7 / buffered chunker | 2 | 2 | 44.06 | 106.82 | 44.74 | 29.59 | 13.02 | 2 | 7.60 |
| 2 / SL7 / previous chunker | 2 | 2 | 43.99 | 108.21 | 46.42 | 29.78 | 13.19 | 2 | 8.57 |

Peak phase counts measure overlap, not CPU utilization. Both cases expose the same concurrency counters. Candidate service time excludes waits for the catalog permit in both implementations; the candidate additionally records those waits separately. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 167.80 | 0.00 | 0.00 | 167.80 |
| 2 / Native Cargo | 164.18 | 0.00 | 0.00 | 164.18 |
| 1 / SL7 / previous chunker | 195.80 | 13.03 | 0.61 | 209.45 |
| 2 / SL7 / previous chunker | 191.97 | 16.54 | 0.60 | 209.10 |
| 1 / SL7 / buffered chunker | 193.40 | 14.73 | 0.59 | 208.73 |
| 2 / SL7 / buffered chunker | 190.21 | 10.93 | 0.62 | 201.75 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / previous chunker | 1.90 (1.89–1.90) |
| SL7 / buffered chunker | 1.89 (1.89–1.90) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `9d202ea9138927a1816a6202237fe5a9d2a08b94`.

[HTML tables](chunker-buffer-costs.html) · [JSON measurements](chunker-buffer-costs.json) · [Validation](chunker-buffer-validation.json)
