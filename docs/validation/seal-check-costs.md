# Caching unchanged seal decisions

Clean-build medians: Native Cargo 149.60s, SL7 / batched identities 165.10s, SL7 / cached seal decisions 164.51s. One-line rebuild medians: Native Cargo 4.17s, SL7 / batched identities 6.67s, SL7 / cached seal decisions 8.20s.

Caching seal decisions did not produce a consistent build-speed improvement in this sample. The clean median fell 0.36%, while the edit median rose 22.9%; only two of six paired edits improved. Unchanged-build median fell 9.6%. Background phase reductions should not be treated as equivalent reductions in Cargo latency.

Both SL7 cases use two sealing workers, the native-state cache and batched identity allocation. The candidate remembers completed seal decisions for the current in-memory inode epoch. Read-only closes skip scheduling; every mutation clears the decision. Cancellation and errors do not mark an inode checked. Chunking, compression, schema and durability ordering are unchanged.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / batched identities | SL7 / cached seal decisions |
| --- | ---: | ---: | ---: |
| Clean build | 149.60 (149.52–149.69) | 165.10 (163.89–166.32) | 164.51 (164.40–164.63) |
| One-line rebuild | 4.17 (3.76–4.58) | 6.67 (5.61–16.31) | 8.20 (5.87–12.98) |
| Unchanged build | 0.29 (0.28–2.31) | 0.45 (0.35–5.87) | 0.40 (0.38–3.13) |
| Unchanged after remount | 0.28 (0.27–0.29) | 0.53 (0.52–0.54) | 0.56 (0.55–0.57) |

SL7 / cached seal decisions was faster than the batched-identity baseline in 2 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Completed | Canceled | Skipped | Unchanged checks avoided |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / batched identities | 2 | 2 | 49.00 | 117.94 | 48.59 | 30.74 | 13.60 | 3699 | 707 | 10564 | unmeasured |
| 1 / SL7 / cached seal decisions | 2 | 2 | 44.38 | 105.97 | 41.95 | 27.27 | 11.52 | 3642 | 619 | 4445 | 10822 |
| 2 / SL7 / cached seal decisions | 2 | 2 | 42.39 | 114.29 | 40.92 | 27.18 | 11.82 | 3660 | 731 | 3531 | 10578 |
| 2 / SL7 / batched identities | 2 | 2 | 37.78 | 108.97 | 41.67 | 26.38 | 12.20 | 3656 | 730 | 14167 | unmeasured |

Peak preparation counts measure overlapping preparation phases, not CPU utilization. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 164.22 | 0.00 | 0.00 | 164.22 |
| 2 / Native Cargo | 164.85 | 0.00 | 0.00 | 164.85 |
| 1 / SL7 / batched identities | 201.57 | 7.52 | 0.58 | 209.67 |
| 2 / SL7 / batched identities | 189.76 | 15.64 | 0.64 | 206.04 |
| 1 / SL7 / cached seal decisions | 196.53 | 18.75 | 0.63 | 215.90 |
| 2 / SL7 / cached seal decisions | 195.14 | 7.62 | 0.59 | 203.34 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / batched identities | 1.93 (1.90–1.96) |
| SL7 / cached seal decisions | 1.94 (1.88–2.00) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `bcd8da2f495cc1415a4a98200cc956c1d82024f8`.

[HTML tables](seal-check-costs.html) · [JSON measurements](seal-check-costs.json) · [Validation](seal-check-validation.json)
