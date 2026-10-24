# Profiles with sealing drained between builds

The same binary ran with the import pipeline disabled and enabled on Oaktown middle with two sealing workers. Use the [matched Cargo comparison](quiescence-costs.md) for elapsed-time conclusions. These profiles identify where work and waits occur.

| Stage / binary | Daemon user CPU (s) | Kernel CPU (s) | Background fsync calls | Sum fsync time (s) | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean / serial | 37.11 | 42.55 | 19009 | 201.74 | 2 |
| clean / parallel | 37.74 | 43.26 | 19315 | 175.13 | 2 |
| edit-2-profile / serial | 3.81 | 3.37 | 51 | 9.29 | 2 |
| edit-2-profile / parallel | 3.99 | 3.16 | 52 | 4.64 | 2 |
| edit-3-profile / serial | 3.40 | 2.82 | 51 | 0.68 | 2 |
| edit-3-profile / parallel | 3.46 | 2.91 | 51 | 0.68 | 2 |

The serial completed 3,555 seals, canceled 646, and skipped 4,557. Candidate service summed to 50.83s, packing 129.01s, catalog publication 58.69s, and retirement 36.66s. Filesystem-state holds summed to 14.43s.

The parallel completed 3,599 seals, canceled 772, and skipped 4,253. Candidate service summed to 49.75s, packing 108.10s, catalog publication 48.57s, and retirement 29.54s. Filesystem-state holds summed to 13.56s.

The enabled run attempted 86 pipelined preparations. Canceled preparations count as attempts. The disabled run attempted zero. CPU samples below identify actual producer-thread work; phase-overlap counters alone do not measure CPU utilization.

| Stage / mode | CPU samples | Producer samples | Packing-worker samples |
| --- | ---: | ---: | ---: |
| clean / serial | 3682 | 0 | 3064 |
| clean / parallel | 3694 | 299 | 2716 |
| edit-2-profile / serial | 362 | 0 | 292 |
| edit-2-profile / parallel | 358 | 144 | 166 |
| edit-3-profile / serial | 318 | 0 | 277 |
| edit-3-profile / parallel | 297 | 134 | 128 |

Unweighted samples at requested 99 Hz; these counts show where CPU work ran, not elapsed-time savings.

All twelve profiled Cargo commands, freshness checks, executable checks, incremental-output checks and both store integrity checks passed. Worker and I/O activity drained with no sealing errors. Cache bounds held, all six traced stages supplied 19 syscall counters, and all recordings reported zero lost samples.

Raw recordings are unchanged. CPU and I/O text was re-analyzed with the recorded analyzer commit after capture to recognize accelerated x86 SHA symbols and inlined sealing workers.

Build IDs remain in perf.data. Optional perf build-ID cache updates were disabled for both runs to avoid the postprocessing timeout observed in an earlier experiment.

Separate instrumented runs on the same middle node. Every stage observes closed handles and drained work. Use the uninstrumented matched comparison for elapsed-time conclusions.

Phase and syscall sums overlap with each other and compiler work.

Background completions/cancellations can differ; syscall totals are not normalized per artifact.

CPU totals include Cargo and the first post-build quiescence wait; they exclude executable checks, profiler shutdown, the second quiet observation and draining unmount.

Userspace-only CPU stacks; kernel CPU additionally counted through /proc.

No kernel FUSE request tracepoints; boundary-incomplete calls are excluded from interval timing.

Two sealing workers in both modes; the pipeline adds at most one producer thread per worker for artifacts at least 16 MiB. Producers are attributed to sealing CPU and exposed separately by thread name.

[Profile JSON](quiescence-costs-profile.json)
