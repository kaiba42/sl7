# Parallel compression: Cargo build comparison

Clean-build medians: Native Cargo 149.42s, SL7 / compression serial 168.52s, SL7 / 2 compression workers 169.14s. One-line rebuild medians: Native Cargo 4.03s, SL7 / compression serial 6.18s, SL7 / 2 compression workers 6.21s.

Enabling two compression workers per sealer changed the clean median by -0.4% and the edit median by -0.4% (positive means faster). Both SL7 modes finish sealing and close all FUSE handles between stages. These two rounds do not establish statistical significance. Compression stays disabled by default: only two of six paired edits were faster, and the recorded sequence sum improved in one round and worsened in the other. Actual compression overlapped, but this experiment does not show a consistent end-to-end benefit.

Both SL7 cases use the same executable, two sealing workers, the 128 KiB chunker buffer, and the opt-in status diagnostic. Compression worker count is the only configuration difference; the import-stream pipeline is disabled in both cases. Cargo latency and the subsequent wait for sealing are recorded separately. Every SL7 stage finishes with three quiet observations with matching progress counters, zero queued inodes, no active I/O or seals, and no open FUSE handles. This includes queue overflow and workers asleep in I/O.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, Oaktown top, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / compression serial | SL7 / 2 compression workers |
| --- | ---: | ---: | ---: |
| Clean build | 149.42 (148.59–150.26) | 168.52 (168.16–168.88) | 169.14 (168.55–169.72) |
| One-line rebuild | 4.03 (3.80–5.30) | 6.18 (5.46–9.31) | 6.21 (5.31–10.48) |
| Unchanged build | 0.28 (0.28–0.30) | 0.44 (0.35–0.62) | 0.41 (0.36–0.57) |
| Unchanged after remount | 0.27 (0.27–0.27) | 0.56 (0.54–0.57) | 0.57 (0.55–0.58) |

SL7 / 2 compression workers was faster than the serial-compression baseline in 2 of 6 paired edits.

## Cargo plus post-build settling

These sums add the separately measured settling wait to Cargo time. They exclude executable-help checks and are not a total wall-clock or durability-equivalence measure against native Cargo.

| Operation | Native Cargo | SL7 / compression serial | SL7 / 2 compression workers |
| --- | ---: | ---: | ---: |
| Clean build | 149.42 (148.59–150.26) | 180.28 (176.80–183.77) | 179.20 (177.71–180.69) |
| One-line rebuild | 4.03 (3.80–5.30) | 10.39 (7.51–13.40) | 11.88 (7.02–12.09) |
| Unchanged build | 0.28 (0.28–0.30) | 0.64 (0.55–0.82) | 0.62 (0.56–0.77) |
| Unchanged after remount | 0.27 (0.27–0.27) | 0.76 (0.74–0.77) | 0.77 (0.75–0.78) |


## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Peak compression workers / sealer | Chunks compressed on workers | Peak candidate phases | Candidate catalog wait (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / compression serial | 2 | 2 | 49.34 | 109.39 | 46.45 | 28.08 | 12.05 | 0 | 0 | 2 | 7.55 |
| 1 / SL7 / 2 compression workers | 2 | 2 | 48.48 | 102.39 | 45.22 | 28.21 | 11.92 | 2 | 216383 | 2 | 8.68 |
| 2 / SL7 / 2 compression workers | 2 | 2 | 48.61 | 103.67 | 47.21 | 28.80 | 12.12 | 2 | 215523 | 2 | 8.45 |
| 2 / SL7 / compression serial | 2 | 2 | 47.72 | 104.04 | 45.12 | 28.36 | 12.80 | 0 | 0 | 2 | 7.17 |

Peak phase counts measure overlap, not CPU utilization. Both modes use the same counters. Candidate service time excludes waits for the catalog permit; both modes record those waits separately. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. The post-build wait includes at least 0.2s for three observations even when already quiet. This is an isolated-workload observation, not an fsync guarantee or a barrier against future clients. Diagnostic xattr support is enabled in both SL7 cases; these results use a different method from the historical CPU-idle comparisons.

## Recorded sequence costs

| Round / backend | Cargo (s) | Post-build settling (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 164.72 | 0.00 | 0.00 | 164.72 |
| 2 / Native Cargo | 162.02 | 0.00 | 0.00 | 162.02 |
| 1 / SL7 / compression serial | 189.83 | 25.05 | 0.61 | 215.49 |
| 2 / SL7 / compression serial | 190.84 | 21.82 | 0.59 | 213.25 |
| 1 / SL7 / 2 compression workers | 192.80 | 18.80 | 0.59 | 212.19 |
| 2 / SL7 / 2 compression workers | 189.56 | 25.24 | 0.61 | 215.41 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / compression serial | 1.90 (1.89–1.91) |
| SL7 / 2 compression workers | 1.90 (1.90–1.91) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `a33846b7daaf9a0558617aede2fb0d656a5c9f7d`.

[HTML tables](compression-costs.html) · [JSON measurements](compression-costs.json) · [Validation](compression-validation.json)
