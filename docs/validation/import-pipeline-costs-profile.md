# Re-profile of the large-artifact import pipeline

The same binary ran with the import pipeline disabled and enabled on Oaktown middle with two sealing workers. Use the [matched Cargo comparison](import-pipeline-costs.md) for elapsed-time conclusions. These profiles identify where work and waits occur.

| Stage / binary | Daemon user CPU (s) | Kernel CPU (s) | Background fsync calls | Sum fsync time (s) | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean / serial | 29.20 | 38.23 | 17254 | 169.39 | 2 |
| clean / parallel | 32.51 | 40.43 | 19030 | 154.33 | 2 |
| edit-2-profile / serial | 4.26 | 3.47 | 514 | 13.66 | 2 |
| edit-2-profile / parallel | 6.19 | 3.79 | 60 | 2.78 | 2 |
| edit-3-profile / serial | 7.48 | 3.75 | 155 | 4.73 | 2 |
| edit-3-profile / parallel | 2.15 | 2.44 | 40 | 0.42 | 2 |

The serial completed 3,519 seals, canceled 661, and skipped 1,879. Candidate service summed to 46.90s, packing 129.86s, catalog publication 56.34s, and retirement 36.06s. Filesystem-state holds summed to 13.71s.

The parallel completed 3,738 seals, canceled 813, and skipped 3,758. Candidate service summed to 42.95s, packing 108.82s, catalog publication 50.32s, and retirement 28.99s. Filesystem-state holds summed to 13.22s.

The enabled run attempted 92 pipelined preparations. Canceled preparations count as attempts. The disabled run attempted zero. CPU samples below identify actual producer-thread work; phase-overlap counters alone do not measure CPU utilization.

| Stage / mode | CPU samples | Producer samples | Packing-worker samples |
| --- | ---: | ---: | ---: |
| clean / serial | 2901 | 0 | 2318 |
| clean / parallel | 3142 | 172 | 2335 |
| edit-2-profile / serial | 410 | 0 | 353 |
| edit-2-profile / parallel | 561 | 192 | 285 |
| edit-3-profile / serial | 722 | 0 | 691 |
| edit-3-profile / parallel | 179 | 74 | 79 |

Unweighted samples at requested 99 Hz; these counts show where CPU work ran, not elapsed-time savings.

All twelve profiled Cargo commands, freshness checks, executable checks, incremental-output checks and both store integrity checks passed. Worker and I/O activity drained with no sealing errors. Cache bounds held, all six traced stages supplied 19 syscall counters, and all recordings reported zero lost samples.

Raw recordings are unchanged. CPU and I/O text was re-analyzed with the recorded analyzer commit after capture to recognize accelerated x86 SHA symbols and inlined sealing workers.

Build IDs remain in perf.data. Optional perf build-ID cache updates were disabled for both runs to avoid the postprocessing timeout observed in an earlier experiment.

Separate instrumented runs on the same middle node, not elapsed-time comparisons.

Phase and syscall sums overlap with each other and compiler work.

Background completions/cancellations can differ; syscall totals are not normalized per artifact.

CPU totals include post-build idle observations and exclude draining unmount.

Userspace-only CPU stacks; kernel CPU additionally counted through /proc.

No kernel FUSE request tracepoints; boundary-incomplete calls are excluded from interval timing.

Two sealing workers in both modes; the pipeline adds at most one producer thread per worker for artifacts at least 16 MiB. Producers are attributed to sealing CPU and exposed separately by thread name.

[Profile JSON](import-pipeline-costs-profile.json)
