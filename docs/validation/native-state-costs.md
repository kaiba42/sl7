# Caching proven native inode state

Clean-build medians: Native Cargo 149.52s, SL7 / worker pool 168.71s, SL7 / native-state cache 166.31s. One-line rebuild medians: Native Cargo 4.24s, SL7 / worker pool 8.42s, SL7 / native-state cache 8.12s.

Both SL7 cases use two sealing workers. The candidate caches a proven native representation with no logical metadata override. Repeated native operations avoid identity/catalog lookups and redundant hydration checks. Publication clears that fact before committing a sealed representation; hydration alone does not establish it. Chunking, compression and durability ordering are unchanged.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, six cases and 48 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / worker pool | SL7 / native-state cache |
| --- | ---: | ---: | ---: |
| Clean build | 149.52 (149.22–149.82) | 168.71 (167.69–169.73) | 166.31 (166.22–166.40) |
| One-line rebuild | 4.24 (3.76–4.78) | 8.42 (6.09–11.77) | 8.12 (5.85–10.54) |
| Unchanged build | 0.35 (0.29–4.08) | 0.46 (0.36–5.41) | 0.42 (0.37–2.86) |
| Unchanged after remount | 0.28 (0.27–0.28) | 0.60 (0.54–0.66) | 0.57 (0.54–0.60) |

SL7 / native-state cache was faster than the worker-pool baseline in 5 of 6 paired edits.

## Interpretation

The native-state cache reduced the observed clean-build median by 1.4% and the edited-build median by 3.5% relative to the worker-pool baseline. Clean builds were faster in both rounds and five of six paired edits were faster. With only two reversed-order rounds, these are measured differences rather than a statistical significance claim.

The remaining overhead versus native Cargo is 11.2% for the clean-build median and 91.8% for the edited-build median. The recorded Cargo/idle-observation/drain sequence median fell from 214.48s to 207.47s (3.3%). This sequence includes harness idle observations and is not uninterrupted developer build latency.

Filesystem-state hold totals fell from 17.1–17.6s to 11.7–12.0s across the full sequences. Those counters overlap compiler work and do not translate directly into elapsed build savings. The separate profile will measure syscall and user/kernel CPU changes.

These measurements predate batched identity allocation and cached seal decisions. Those changes have passed local correctness checks, and their isolated performance comparisons remain queued.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) | Native-state checks | Native-state cache hits |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / worker pool | 2 | 2 | 71.72 | 114.67 | 40.82 | 30.92 | 17.13 | unmeasured | unmeasured |
| 1 / SL7 / native-state cache | 2 | 2 | 67.05 | 105.08 | 40.33 | 31.68 | 11.65 | 21750 | 291477 |
| 2 / SL7 / native-state cache | 2 | 2 | 65.28 | 103.85 | 41.10 | 29.44 | 12.04 | 23436 | 290387 |
| 2 / SL7 / worker pool | 2 | 2 | 65.74 | 111.16 | 39.72 | 29.61 | 17.55 | unmeasured | unmeasured |

Peak preparation counts measure overlapping preparation phases, not CPU utilization. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 167.53 | 0.00 | 0.00 | 167.53 |
| 2 / Native Cargo | 165.31 | 0.00 | 0.00 | 165.31 |
| 1 / SL7 / worker pool | 202.51 | 14.14 | 0.64 | 217.28 |
| 2 / SL7 / worker pool | 199.31 | 11.73 | 0.63 | 211.68 |
| 1 / SL7 / native-state cache | 194.74 | 17.04 | 0.63 | 212.41 |
| 2 / SL7 / native-state cache | 193.12 | 8.82 | 0.60 | 202.54 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / worker pool | 1.94 (1.93–1.94) |
| SL7 / native-state cache | 1.88 (1.87–1.88) |

All 48 Cargo commands, 32 artifact-graph comparisons, freshness/executable checks, incremental-output checks and four store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `0685bac24b8e74694a1e8922e25a6054929464cb`.

[HTML tables](native-state-costs.html) · [JSON measurements](native-state-costs.json) · [Validation](native-state-validation.json)
