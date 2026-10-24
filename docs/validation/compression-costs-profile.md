# Parallel compression CPU profiles

The same binary ran with compression workers disabled and with two workers per sealer. Use the [matched Cargo comparison](compression-costs.md) for elapsed-time conclusions.

| Stage / mode | Daemon user CPU (s) | Kernel CPU (s) | Background fsync calls | Sum fsync time (s) |
| --- | ---: | ---: | ---: | ---: |
| clean / off | 37.14 | 42.87 | 19287 | 176.21 |
| clean / on | 38.62 | 43.56 | 19532 | 173.03 |
| edit-2-profile / off | 3.03 | 2.94 | 48 | 4.10 |
| edit-2-profile / on | 3.61 | 2.90 | 53 | 5.40 |
| edit-3-profile / off | 2.59 | 2.77 | 49 | 10.46 |
| edit-3-profile / on | 2.63 | 2.60 | 47 | 0.65 |

Compression off: 3,608 completed seals, 708 canceled, 0 chunks compressed on workers, and peak 0 active compression workers per sealer. Packing summed to 107.95s, catalog publication 46.02s, and retirement 26.62s. Filesystem-state holds summed to 12.50s. These overlapping sums are not wall-clock savings.

Compression on: 3,659 completed seals, 657 canceled, 212,957 chunks compressed on workers, and peak 2 active compression workers per sealer. Packing summed to 103.46s, catalog publication 45.37s, and retirement 28.11s. Filesystem-state holds summed to 12.50s. These overlapping sums are not wall-clock savings.

| Stage / mode | CPU samples | Compression-worker samples | Packing-worker samples |
| --- | ---: | ---: | ---: |
| clean / off | 3754 | 0 | 3035 |
| clean / on | 3858 | 489 | 2603 |
| edit-2-profile / off | 286 | 0 | 224 |
| edit-2-profile / on | 328 | 18 | 265 |
| edit-3-profile / off | 232 | 0 | 202 |
| edit-3-profile / on | 235 | 9 | 194 |

Unweighted samples at requested 99 Hz identify actual worker CPU execution; overlap counters alone do not measure utilization.

## Remaining userspace work

| Stage / mode | SHA-256 leaf share | Chunk-identity inclusive share |
| --- | ---: | ---: |
| clean / off | 21.7% | 10.2% |
| clean / on | 19.8% | 9.2% |
| edit-2-profile / off | 41.3% | 21.7% |
| edit-2-profile / on | 42.7% | 21.6% |
| edit-3-profile / off | 51.7% | 28.9% |
| edit-3-profile / on | 45.1% | 25.5% |

SHA leaf samples include whole-file, chunk, and pack hashing. Chunk-identity inclusive samples overlap them; these columns must not be added. Duplicate chunks bypass compression, but they still pass through chunking, whole-file hashing, chunk hashing and catalog lookup. Compression parallelism cannot remove those costs. These userspace shares also exclude the substantial kernel CPU shown above.

All twelve profiled Cargo commands, freshness, executable, incremental-output and two store integrity checks passed. Workers and I/O drained with no recorded sealing errors. Cache bounds held, all six traced stages supplied 19 syscall counters, and all recordings reported zero lost samples.

Separate instrumented runs on Oaktown top. Every stage observes closed handles and drained work. Use the uninstrumented matched comparison for elapsed-time conclusions.

Phase and syscall sums overlap with each other and compiler work. Background completions/cancellations can differ; totals are not normalized per artifact.

CPU totals include Cargo and the first post-build quiescence wait; they exclude executable checks, profiler shutdown, the second quiet observation and draining unmount.

Userspace-only CPU stacks; kernel CPU additionally counted through /proc. No kernel FUSE request tracepoints; boundary-incomplete calls are excluded from interval timing.

Two sealing workers in both modes; the enabled pool adds two persistent compression workers per sealer, created lazily for novel chunks in artifacts at least 16 MiB. The import-stream pipeline is disabled in both.

Build IDs remain in perf.data. Optional perf build-ID cache updates were disabled in both runs.

[Profile JSON](compression-costs-profile.json)
