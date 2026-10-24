# Batching durable inode identity allocation

Clean-build medians: Native Cargo 149.31s, SL7 / native-state cache 166.26s, SL7 / batched identities 165.42s. One-line rebuild medians: Native Cargo 4.32s, SL7 / native-state cache 7.82s, SL7 / batched identities 8.52s.

Batching reduced candidate-setup time, but this sample does not establish a consistent build-speed improvement: the clean median fell 0.50%, while the edit median rose 8.98% and only three of six paired edits improved. Candidate setup still runs under the catalog permit in this implementation, including inode sync. The next experiment separates durable ID reservation from inode I/O.

Both SL7 cases use two sealing workers and the native-state cache. The candidate reserves 64 inode identities in one FULL transaction instead of committing an identity row for each new file. Every row is durable before attaching its xattr, and the inode is still synced before manifest publication. Unused reservations are reclaimed by offline GC. Chunking, compression, schema and durability ordering are unchanged.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / native-state cache | SL7 / batched identities |
| --- | ---: | ---: | ---: |
| Clean build | 149.31 (148.92–149.69) | 166.26 (166.15–166.37) | 165.42 (164.56–166.29) |
| One-line rebuild | 4.32 (3.88–5.42) | 7.82 (5.86–14.91) | 8.52 (5.82–11.68) |
| Unchanged build | 0.49 (0.28–4.45) | 0.45 (0.39–3.00) | 0.58 (0.34–2.83) |
| Unchanged after remount | 0.28 (0.27–0.28) | 0.57 (0.55–0.59) | 0.58 (0.56–0.60) |

SL7 / batched identities was faster than the native-state-cache baseline in 3 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Native-state checks | Native-state cache hits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / native-state cache | 2 | 2 | 73.22 | 113.78 | 42.21 | 29.79 | 12.10 | 22296 | 291170 |
| 1 / SL7 / batched identities | 2 | 2 | 41.97 | 111.31 | 42.93 | 26.89 | 12.14 | 23304 | 290396 |
| 2 / SL7 / batched identities | 2 | 2 | 46.62 | 115.66 | 40.74 | 26.26 | 12.28 | 24294 | 290526 |
| 2 / SL7 / native-state cache | 2 | 2 | 62.89 | 102.06 | 41.58 | 27.93 | 11.68 | 23862 | 291425 |

Peak preparation counts measure overlapping preparation phases, not CPU utilization. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 168.48 | 0.00 | 0.00 | 168.48 |
| 2 / Native Cargo | 164.43 | 0.00 | 0.00 | 164.43 |
| 1 / SL7 / native-state cache | 200.12 | 8.52 | 0.61 | 209.25 |
| 2 / SL7 / native-state cache | 193.48 | 14.33 | 0.63 | 208.44 |
| 1 / SL7 / batched identities | 191.61 | 10.93 | 0.62 | 203.15 |
| 2 / SL7 / batched identities | 198.02 | 10.43 | 0.60 | 209.04 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / native-state cache | 1.93 (1.88–1.98) |
| SL7 / batched identities | 2.02 (1.96–2.07) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `d15f94009f41c4f82d35bc71b4afbe52575d0343`.

[HTML tables](identity-batch-costs.html) · [JSON measurements](identity-batch-costs.json) · [Validation](identity-batch-validation.json)
