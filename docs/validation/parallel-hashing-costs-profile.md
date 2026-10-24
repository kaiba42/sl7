# Parallel chunk hashing: CPU profiles

One binary with chunk hashing serial (off) or on two shared workers (on). [Matched build timings](parallel-hashing-costs.md) are measured separately.

| Stage / mode | User CPU (s) | Kernel CPU (s) | Hashed MiB | SHA leaf share |
| --- | ---: | ---: | ---: | ---: |
| clean / off | 37.19 | 43.43 | 7877.83 | 21.6% |
| edit-2-profile / off | 2.90 | 2.90 | 1224.41 | 38.0% |
| edit-3-profile / off | 2.55 | 2.73 | 1216.55 | 49.8% |
| clean / on | 39.15 | 44.62 | 8042.14 | 22.4% |
| edit-2-profile / on | 3.79 | 3.07 | 1716.35 | 43.9% |
| edit-3-profile / on | 3.47 | 2.91 | 1715.51 | 47.9% |

Version off: 12127.46 MiB hashed, 896.17 MiB discarded hashing. Catalog publication sums to 49.08s, candidate catalog waits to 10.17s, filesystem-state holds to 13.62s and preparation service to 111.41s. These cumulative intervals overlap and are not wall-clock costs.

Version on: 13179.57 MiB hashed, 1854.55 MiB discarded hashing. Catalog publication sums to 52.89s, candidate catalog waits to 9.82s, filesystem-state holds to 13.16s and preparation service to 120.32s. These cumulative intervals overlap and are not wall-clock costs.

Discarded hashing attributed to candidates reopened before publication rose from 691.5 to 1690.4 MiB; mutation-discarded hashing changed from 204.7 to 164.1 MiB. Thus the extra discarded work in this profile is predominantly associated with reopen timing. One profile per mode does not establish that the worker pool reliably causes this change. The current comparison keeps prepared-result reuse disabled in both modes.

## CPU work on the shared pool

| Stage / mode | All CPU samples | Hash-worker samples | Peak hash jobs |
| --- | ---: | ---: | ---: |
| clean / off | 3725 | 0 | 0 |
| clean / on | 3900 | 411 | 2 |
| edit-2-profile / off | 263 | 0 | 0 |
| edit-2-profile / on | 358 | 74 | 2 |
| edit-3-profile / off | 237 | 0 | 0 |
| edit-3-profile / on | 315 | 74 | 2 |

Worker samples establish CPU execution on helper threads, and the peak counter measures overlapping hash jobs. Neither metric is a speedup measurement; use the separate unprofiled Cargo comparison.

All twelve profiled Cargo commands, executable/freshness checks and two integrity checks passed. Incremental output was present, workers drained, cache limits held, and no sealing errors were recorded. All six traced stages supplied 19 syscall counters.

Hashing volume depends on reopen/cancellation timing because of scheduling and bounded read-ahead.

One profile per mode on node middle; use the separate matched build rounds on node top for elapsed-time conclusions. Do not compare timings across nodes.

CPU scope includes Cargo, executable-help checks and post-check quiescence; excludes profiler shutdown, second quiet observation and unmount. This differs from earlier profiles that ran executable checks after sampling.

Userspace perf samples at requested 99 Hz identify CPU attribution; /proc also measures daemon kernel CPU. Shares are not wall-time savings.

Both profiles use node middle and two sealing workers; pipeline, compression workers and completed-preparation reuse are disabled. Both modes use the same hash-validated binary.

No kernel FUSE tracepoints were available. Boundary-incomplete syscall intervals are excluded; all recordings reported zero lost samples.

Public I/O analysis includes aggregate operations and overlap; descriptor-level maps remain in the checksum-identified archives.

[Profile JSON](parallel-hashing-costs-profile.json)
