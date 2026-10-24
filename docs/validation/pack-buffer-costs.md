# Buffering pack writes

Clean-build medians: Native Cargo 149.82s, SL7 / cached seal decisions 165.45s, SL7 / buffered packs 165.48s. One-line rebuild medians: Native Cargo 4.00s, SL7 / cached seal decisions 8.64s, SL7 / buffered packs 8.53s.

Both SL7 cases use two sealing workers, the native-state cache, batched identities and cached seal decisions. The candidate coalesces each pack writer in a 64 KiB buffer and explicitly flushes it before syncing and publishing. Record bytes, content hashes, chunking, compression and durability ordering are unchanged.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / cached seal decisions | SL7 / buffered packs |
| --- | ---: | ---: | ---: |
| Clean build | 149.82 (149.60–150.05) | 165.45 (164.43–166.48) | 165.48 (165.13–165.83) |
| One-line rebuild | 4.00 (3.78–4.71) | 8.64 (6.09–12.97) | 8.53 (5.74–11.71) |
| Unchanged build | 0.29 (0.29–0.82) | 0.41 (0.36–4.83) | 0.40 (0.35–4.05) |
| Unchanged after remount | 0.28 (0.28–0.28) | 0.55 (0.55–0.55) | 0.55 (0.54–0.56) |

SL7 / buffered packs was faster than the cached-seal-decision baseline in 4 of 6 paired edits.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Completed | Canceled | Skipped | Unchanged checks avoided |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / cached seal decisions | 2 | 2 | 44.80 | 119.53 | 49.30 | 30.06 | 13.59 | 3539 | 659 | 3142 | 10163 |
| 1 / SL7 / buffered packs | 2 | 2 | 47.21 | 103.78 | 40.68 | 26.86 | 11.78 | 3724 | 684 | 4957 | 11059 |
| 2 / SL7 / buffered packs | 2 | 2 | 36.86 | 104.69 | 41.51 | 26.38 | 11.78 | 3599 | 679 | 4669 | 10858 |
| 2 / SL7 / cached seal decisions | 2 | 2 | 42.71 | 105.93 | 40.10 | 24.82 | 11.54 | 3670 | 742 | 4433 | 10916 |

Peak preparation counts measure overlapping preparation phases, not CPU utilization. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 163.75 | 0.00 | 0.00 | 163.75 |
| 2 / Native Cargo | 164.03 | 0.00 | 0.00 | 164.03 |
| 1 / SL7 / cached seal decisions | 197.18 | 9.42 | 0.58 | 207.18 |
| 2 / SL7 / cached seal decisions | 199.03 | 12.73 | 0.62 | 212.38 |
| 1 / SL7 / buffered packs | 200.25 | 13.63 | 0.61 | 214.50 |
| 2 / SL7 / buffered packs | 191.79 | 13.73 | 0.61 | 206.13 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / cached seal decisions | 1.92 (1.89–1.96) |
| SL7 / buffered packs | 1.90 (1.89–1.90) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `4db4330199a3b29003a9f748ac8f9239c134b66d`.

[HTML tables](pack-buffer-costs.html) · [JSON measurements](pack-buffer-costs.json) · [Validation](pack-buffer-validation.json)
