# Catalog statement reuse: CPU profiles

Before/after INSERT-statement reuse. Off denotes the baseline binary; on denotes the optimized binary. [Matched build timings](catalog-statements-costs.md) are measured separately.

| Stage / mode | User CPU (s) | Kernel CPU (s) | Hashed MiB | SHA leaf share |
| --- | ---: | ---: | ---: | ---: |
| clean / off | 38.22 | 43.13 | 8065.81 | 21.7% |
| edit-2-profile / off | 3.84 | 3.27 | 1717.36 | 44.4% |
| edit-3-profile / off | 2.55 | 2.74 | 1212.68 | 43.6% |
| clean / on | 37.04 | 43.43 | 8059.49 | 22.6% |
| edit-2-profile / on | 3.80 | 3.32 | 1710.41 | 44.4% |
| edit-3-profile / on | 2.62 | 2.77 | 1214.46 | 46.1% |

Version off: 12726.31 MiB hashed, 1365.72 MiB discarded hashing. Catalog publication sums to 46.54s, candidate catalog waits to 7.76s, filesystem-state holds to 13.74s and preparation service to 113.95s. These cumulative intervals overlap and are not wall-clock costs.

Version on: 12689.93 MiB hashed, 1343.68 MiB discarded hashing. Catalog publication sums to 51.24s, candidate catalog waits to 10.28s, filesystem-state holds to 13.62s and preparation service to 119.91s. These cumulative intervals overlap and are not wall-clock costs.

## SQL parsing and publication CPU

| Stage / version | CPU samples | INSERT publication inclusive share | SQL preparation inclusive share |
| --- | ---: | ---: | ---: |
| clean / off | 3791 | 9.02% | 4.19% |
| clean / on | 3706 | 5.69% | outside top 50 |
| edit-2-profile / off | 360 | 7.78% | outside top 50 |
| edit-2-profile / on | 347 | 6.63% | outside top 50 |
| edit-3-profile / off | 241 | 6.22% | outside top 50 |
| edit-3-profile / on | 230 | 5.22% | outside top 50 |

Inclusive shares overlap; do not add them. Missing from the top 50 is not proof of zero samples. Timing conclusions use the separate unprofiled build comparison.

All twelve profiled Cargo commands, executable/freshness checks and two integrity checks passed. Incremental output was present, workers drained, cache limits held, and no sealing errors were recorded. All six traced stages supplied 19 syscall counters.

Hashing volume depends on reopen/cancellation timing even though hashing code is unchanged.

One profile per mode on node middle; use the separate matched build rounds on node top for elapsed-time conclusions. Do not compare timings across nodes.

CPU scope includes Cargo, executable-help checks and post-check quiescence; excludes profiler shutdown, second quiet observation and unmount. This differs from earlier profiles that ran executable checks after sampling.

Userspace perf samples at requested 99 Hz identify CPU attribution; /proc also measures daemon kernel CPU. Shares are not wall-time savings.

Both profiles use node middle and two sealing workers; pipeline, compression workers and completed-preparation reuse are disabled. Before/after binaries are validated by hash.

No kernel FUSE tracepoints were available. Boundary-incomplete syscall intervals are excluded; all recordings reported zero lost samples.

Public I/O analysis includes aggregate operations and overlap; descriptor-level maps remain in the checksum-identified archives.

[Profile JSON](catalog-statements-costs-profile.json)
