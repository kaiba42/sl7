# Same-binary sealing worker scaling

Clean-build medians: Native Cargo 149.43s, SL7 / 1 worker 168.74s, SL7 / 2 workers 170.03s, SL7 / 4 workers 185.25s. One-line rebuild medians: Native Cargo 4.45s, SL7 / 1 worker 7.79s, SL7 / 2 workers 8.83s, SL7 / 4 workers 7.61s.

All SL7 cases use the same executable. SL7_SEAL_WORKERS selects one, two, or four independently connected workers. Preparation and retirement can overlap across files; durable catalog commits remain serialized. Per-inode gates prevent overlapping candidates for one file, and duplicate chunks are resolved at publication.

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds, eight cases and 64 Cargo commands; Rust 1.97.1, eight CPUs, 12 GiB, fixed source and matching output paths. Debug incremental output is present. Warm OS caches and shared-node activity limit precision. No statistical significance is claimed.

| Operation | Native Cargo | SL7 / 1 worker | SL7 / 2 workers | SL7 / 4 workers |
| --- | ---: | ---: | ---: | ---: |
| Clean build | 149.43 (148.83–150.03) | 168.74 (166.60–170.88) | 170.03 (169.68–170.37) | 185.25 (183.52–186.98) |
| One-line rebuild | 4.45 (3.76–5.24) | 7.79 (6.40–10.59) | 8.83 (6.01–11.86) | 7.61 (6.01–10.05) |
| Unchanged build | 0.43 (0.28–0.61) | 0.43 (0.39–2.81) | 0.44 (0.35–2.48) | 0.55 (0.39–1.14) |
| Unchanged after remount | 0.29 (0.29–0.29) | 0.58 (0.58–0.58) | 0.56 (0.54–0.59) | 0.58 (0.57–0.59) |

SL7 / 2 workers was faster than one worker in 3 of 6 paired edits. SL7 / 4 workers was faster than one worker in 4 of 6 paired edits.

## Conclusion

Real overlap is present, but this experiment does not establish a build-latency
win from adding workers. Two workers changed the clean-build median by
+0.8% and the edited-build median by
+13.3% relative to one worker. The recorded Cargo/idle-observation/drain
sequence median fell from 218.26s to 214.12s
(1.9%), showing that some benefit can occur outside Cargo's timed interval.
Four workers made clean builds slower in both rounds.

More workers completed more seals, canceled more candidates, and accumulated more
catalog-permit wait time in both rounds. That supports reducing repeated work and
serialized catalog setup before increasing worker count further; it does not
quantify how much of the elapsed-time difference each cause explains. The small
sample and shared node also limit conclusions about the noisy edited builds.

This executable predates the native-state cache and batched identity allocation.
Their performance is not represented here. The two-worker default remains
provisional while those separately queued comparisons run.

## Observed concurrency and phase costs

| Round / backend | Peak seal jobs | Peak preparations | Candidate (s) | Packing (s) | Catalog (s) | Retirement (s) | State holds (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / 1 worker | 1 | 1 | 56.97 | 83.58 | 30.00 | 21.51 | 18.49 |
| 1 / SL7 / 2 workers | 2 | 2 | 67.11 | 106.51 | 40.67 | 30.57 | 17.44 |
| 1 / SL7 / 4 workers | 4 | 4 | 70.21 | 114.80 | 40.76 | 31.83 | 17.00 |
| 2 / SL7 / 4 workers | 4 | 4 | 76.20 | 109.89 | 41.33 | 34.45 | 17.61 |
| 2 / SL7 / 2 workers | 2 | 2 | 70.48 | 111.91 | 39.92 | 30.67 | 17.65 |
| 2 / SL7 / 1 worker | 1 | 1 | 52.78 | 78.06 | 28.64 | 20.22 | 17.21 |

Peak preparation counts measure overlapping preparation phases, not CPU utilization. Summed phase times can overlap with each other and compiler work. The recorded sequence sum includes harness idle observations and draining unmounts but excludes setup, remount startup, executable-help checks and integrity scans. CPU-idle observation alone is not a drained queue.

## Recorded sequence costs

| Round / backend | Cargo (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 164.37 | 0.00 | 0.00 | 164.37 |
| 2 / Native Cargo | 164.15 | 0.00 | 0.00 | 164.15 |
| 1 / SL7 / 1 worker | 199.11 | 12.93 | 8.44 | 220.49 |
| 2 / SL7 / 1 worker | 193.73 | 21.65 | 0.64 | 216.02 |
| 1 / SL7 / 2 workers | 196.89 | 18.55 | 0.64 | 216.08 |
| 2 / SL7 / 2 workers | 202.95 | 8.62 | 0.59 | 212.16 |
| 1 / SL7 / 4 workers | 208.68 | 9.83 | 0.65 | 219.15 |
| 2 / SL7 / 4 workers | 214.47 | 7.52 | 0.64 | 222.63 |

## Storage before GC

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| SL7 / 1 worker | 1.78 (1.77–1.79) |
| SL7 / 2 workers | 1.94 (1.88–1.99) |
| SL7 / 4 workers | 1.92 (1.92–1.92) |

All 64 Cargo commands, 48 artifact-graph comparisons, freshness/executable checks, incremental-output checks and six store integrity checks passed. All workers and I/O leases drained without recorded sealing errors; bounded cache limits held. The executable matches the committed source and Linux validation gate. Raw private source/artifact graphs remain outside this report.

Implementation: `04f45b58bbbd6b26af0098aefa3ce822e6d87be0`.

[HTML tables](parallel-sealing.html) · [JSON measurements](parallel-sealing.json) · [Validation](parallel-validation.json)

## Work performed

| Round / backend | Completed seals | Canceled candidates | Skipped candidates | Errors | Catalog wait (s) |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / SL7 / 1 worker | 3455 | 201 | 6282 | 0 | 6.76 |
| 1 / SL7 / 2 workers | 3720 | 571 | 13902 | 0 | 14.31 |
| 1 / SL7 / 4 workers | 3885 | 977 | 17625 | 0 | 52.87 |
| 2 / SL7 / 4 workers | 3932 | 1006 | 14931 | 0 | 50.41 |
| 2 / SL7 / 2 workers | 3645 | 501 | 10230 | 0 | 17.14 |
| 2 / SL7 / 1 worker | 3448 | 180 | 10374 | 0 | 7.98 |

These are transition counts, not unique artifacts. A reopened and modified file can be sealed again. More workers can therefore change how much preparation and publication happens during the same Cargo sequence. Overlap alone does not imply less work or a shorter critical path; these counts do not isolate CPU, disk, and lock contention as causes.
