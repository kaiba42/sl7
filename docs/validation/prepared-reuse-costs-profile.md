# Completed preparation reuse: CPU profiles

Same binary, reuse off/on. [Matched build timings](prepared-reuse-costs.md) are measured separately.

| Stage / mode | User CPU (s) | Kernel CPU (s) | Hashed MiB | Reused MiB | SHA leaf share |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean / off | 38.44 | 43.79 | 8168.08 | 0.00 | 21.4% |
| edit-2-profile / off | 3.97 | 3.09 | 1853.55 | 0.00 | 42.1% |
| edit-3-profile / off | 3.38 | 2.87 | 1710.54 | 0.00 | 54.3% |
| clean / on | 38.05 | 43.90 | 7978.29 | 173.05 | 21.0% |
| edit-2-profile / on | 2.99 | 2.87 | 1216.40 | 242.24 | 36.2% |
| edit-3-profile / on | 2.51 | 2.73 | 1212.68 | 0.00 | 46.2% |

Reuse off: 0 preparation hits, 0.00 MiB reused, 13548.61 MiB hashed, 2214.80 MiB discarded hashing. Peak retained metadata: 0.00 MiB. Filesystem-state holds sum to 12.94s; preparation service sums to 115.15s. These cumulative intervals overlap and are not wall-clock costs.

Reuse on: 36 preparation hits, 415.29 MiB reused, 11643.40 MiB hashed, 110.33 MiB discarded hashing. Peak retained metadata: 0.63 MiB. Filesystem-state holds sum to 13.53s; preparation service sums to 113.60s. These cumulative intervals overlap and are not wall-clock costs.

All twelve profiled Cargo commands, executable/freshness checks and two integrity checks passed. Incremental output was present, workers drained, cache limits held, and no sealing errors were recorded. All six traced stages supplied 19 syscall counters.

Hashing volume also depends on reopen/cancellation timing. Direct cache-hit bytes are smaller than the total between-run hashing difference; do not attribute the entire difference to reused entries.

One profile per mode on node middle; use the separate matched build rounds on node top for elapsed-time conclusions. Do not compare timings across nodes.

CPU scope includes Cargo, executable-help checks and post-check quiescence; excludes profiler shutdown, second quiet observation and unmount. This differs from earlier profiles that ran executable checks after sampling.

Userspace perf samples at requested 99 Hz identify CPU attribution; /proc also measures daemon kernel CPU. Shares are not wall-time savings.

Both profiles use the same binary and node, with two sealing workers and both pipeline and compression workers disabled.

No kernel FUSE tracepoints were available. Boundary-incomplete syscall intervals are excluded; all recordings reported zero lost samples.

Public I/O analysis includes aggregate operations and overlap; descriptor-level maps remain in the checksum-identified archives.

[Profile JSON](prepared-reuse-costs-profile.json)
