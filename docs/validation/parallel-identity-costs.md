# Parallel inode identity synchronization

Clean-build medians: Native Cargo 153.60s, SL7 / buffered packs 170.99s, SL7 / parallel inode sync 169.97s. One-line rebuild medians: Native Cargo 4.56s, SL7 / buffered packs 7.98s, SL7 / parallel inode sync 6.84s.

The edit median fell 14.2%, with four of six paired edits faster. The clean median changed only 0.6%. Unchanged builds after remount were slower (0.76s versus 0.58s). The candidate remains 10.7% above native for clean builds and 50.1% above native for edits. These two rounds show a narrower improvement than the phase-overlap counters alone might suggest.

Both SL7 cases use two sealing workers, batched identities, cached seal decisions and buffered pack writes. The candidate holds the catalog permit only while reserving a durable identity row. Read-only candidate queries, xattr assignment and inode sync can overlap across workers. The row still commits before xattr assignment, and inode sync still completes before manifest publication. Same-inode exclusion and mutation-epoch validation remain unchanged.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, Oaktown middle, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / buffered packs | SL7 / parallel inode sync |
| --- | ---: | ---: | ---: |
| Clean build | 153.60 (153.59–153.62) | 170.99 (169.88–172.11) | 169.97 (169.36–170.57) |
| One-line rebuild | 4.56 (3.90–6.02) | 7.98 (5.66–11.86) | 6.84 (5.43–8.67) |
| Unchanged build | 0.30 (0.28–0.86) | 0.56 (0.36–4.34) | 0.51 (0.35–2.35) |
| Unchanged after remount | 0.31 (0.29–0.34) | 0.58 (0.56–0.60) | 0.76 (0.74–0.79) |

SL7 / parallel inode sync was faster than the buffered-pack baseline in 4 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Peak candidate phases | Candidate catalog wait (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / buffered packs | 2 | 2 | 39.79 | 112.02 | 46.00 | 28.85 | 12.63 | not instrumented | not instrumented |
| 1 / SL7 / parallel inode sync | 2 | 2 | 38.86 | 103.14 | 45.73 | 27.77 | 12.61 | 2 | 9.13 |
| 2 / SL7 / parallel inode sync | 2 | 2 | 46.52 | 106.29 | 43.27 | 25.42 | 13.16 | 2 | 5.96 |
| 2 / SL7 / buffered packs | 2 | 2 | 41.48 | 109.78 | 44.70 | 27.25 | 12.64 | not instrumented | not instrumented |

Peak phase counts measure overlap, not CPU utilization. The baseline has no candidate-overlap counter. Candidate service time excludes waits for the catalog permit in both implementations; the candidate additionally records those waits separately. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 169.48 | 0.00 | 0.00 | 169.48 |
| 2 / Native Cargo | 169.09 | 0.00 | 0.00 | 169.09 |
| 1 / SL7 / buffered packs | 198.50 | 16.34 | 0.69 | 215.52 |
| 2 / SL7 / buffered packs | 201.98 | 7.72 | 0.65 | 210.35 |
| 1 / SL7 / parallel inode sync | 193.37 | 12.83 | 0.67 | 206.87 |
| 2 / SL7 / parallel inode sync | 194.58 | 12.73 | 3.39 | 210.70 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / buffered packs | 1.95 (1.88–2.01) |
| SL7 / parallel inode sync | 1.90 (1.90–1.91) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `5e7efe258906d2cb7aa277dce12e9db3c7cc2753`.

[HTML tables](parallel-identity-costs.html) · [JSON measurements](parallel-identity-costs.json) · [Validation](parallel-identity-validation.json)
