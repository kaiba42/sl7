# Cargo builds after sealing finishes

Clean-build medians: Native Cargo 153.87s, SL7 / pipeline off 173.66s, SL7 / pipeline on 173.30s. One-line rebuild medians: Native Cargo 4.50s, SL7 / pipeline off 6.52s, SL7 / pipeline on 6.12s.

Enabling the pipeline changed the clean median by 0.2% and the edit median by 6.2% (positive means faster). Both SL7 modes finish sealing and close all FUSE handles between stages. These two rounds do not establish statistical significance.

Both SL7 cases use the same executable, two sealing workers, the 128 KiB chunker buffer, and the opt-in status diagnostic. The pipeline is the only configuration difference. Cargo latency and the subsequent wait for sealing are recorded separately. Every SL7 stage finishes with three quiet observations with matching progress counters, zero queued inodes, no active I/O or seals, and no open FUSE handles. This includes queue overflow and workers asleep in I/O.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, Oaktown middle, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / pipeline off | SL7 / pipeline on |
| --- | ---: | ---: | ---: |
| Clean build | 153.87 (153.65–154.09) | 173.66 (173.33–173.99) | 173.30 (170.52–176.09) |
| One-line rebuild | 4.50 (3.81–5.01) | 6.52 (5.86–7.97) | 6.12 (5.43–9.58) |
| Unchanged build | 0.29 (0.28–0.88) | 0.42 (0.37–0.60) | 0.40 (0.39–1.70) |
| Unchanged after remount | 0.28 (0.28–0.28) | 0.57 (0.55–0.60) | 0.59 (0.57–0.61) |

SL7 / pipeline on was faster than the pipeline-disabled baseline in 4 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Peak candidate phases | Candidate catalog wait (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / pipeline off | 2 | 2 | 46.46 | 117.98 | 51.62 | 31.10 | 12.53 | 2 | 9.86 |
| 1 / SL7 / pipeline on | 2 | 2 | 42.78 | 104.78 | 46.86 | 30.07 | 13.69 | 2 | 6.62 |
| 2 / SL7 / pipeline on | 2 | 2 | 46.05 | 107.71 | 46.72 | 30.75 | 13.08 | 2 | 8.00 |
| 2 / SL7 / pipeline off | 2 | 2 | 53.40 | 118.57 | 50.12 | 32.90 | 12.55 | 2 | 9.13 |

Peak phase counts measure overlap, not CPU utilization. Both modes use the same counters. Candidate service time excludes waits for the catalog permit; both modes record those waits separately. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. The post-build wait includes at least 0.2s for three observations even when already quiet. This is an isolated-workload observation, not an fsync guarantee or a barrier against future clients. Diagnostic xattr support is enabled in both SL7 cases; these results use a different method from the historical CPU-idle comparisons.

## Recorded sequence costs

| Round / backend | Cargo (s) | Post-build settling (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 168.23 | 0.00 | 0.00 | 168.23 |
| 2 / Native Cargo | 169.41 | 0.00 | 0.00 | 169.41 |
| 1 / SL7 / pipeline off | 194.93 | 23.51 | 0.66 | 219.11 |
| 2 / SL7 / pipeline off | 196.45 | 28.74 | 0.64 | 225.83 |
| 1 / SL7 / pipeline on | 195.77 | 20.30 | 0.64 | 216.72 |
| 2 / SL7 / pipeline on | 196.92 | 23.01 | 0.66 | 220.59 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / pipeline off | 1.89 (1.89–1.90) |
| SL7 / pipeline on | 1.89 (1.89–1.90) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `fcb2954a7f9902498ab77cd4ec9abbaefe8afa7c`.

[HTML tables](quiescence-costs.html) · [JSON measurements](quiescence-costs.json) · [Validation](quiescence-validation.json)
