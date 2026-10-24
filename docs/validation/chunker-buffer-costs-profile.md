# Re-profile of the chunker read buffer

Both binaries ran on Oaktown top with two sealing workers. Use the [matched Cargo comparison](chunker-buffer-costs.md) for elapsed-time conclusions. These profiles identify where work and waits occur.

| Stage / binary | Daemon user CPU (s) | Kernel CPU (s) | Background fsync calls | Sum fsync time (s) | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean / baseline | 31.89 | 39.84 | 18536 | 169.62 | 2 |
| clean / candidate | 34.99 | 40.91 | 19837 | 157.78 | 2 |
| edit-2-profile / baseline | 3.46 | 2.87 | 47 | 1.24 | 2 |
| edit-2-profile / candidate | 4.85 | 3.16 | 57 | 0.74 | 2 |
| edit-3-profile / baseline | 3.16 | 2.94 | 50 | 0.81 | 2 |
| edit-3-profile / candidate | 1.68 | 2.33 | 38 | 1.99 | 2 |

The baseline completed 3,625 seals, canceled 738, and skipped 4,538. Candidate service summed to 48.18s, packing 122.90s, catalog publication 48.82s, and retirement 31.68s. Filesystem-state holds summed to 14.38s.

The candidate completed 3,688 seals, canceled 813, and skipped 4,450. Candidate service summed to 49.00s, packing 106.95s, catalog publication 46.40s, and retirement 29.09s. Filesystem-state holds summed to 12.91s.

The candidate recorded 2 overlapping candidate phases and 7.95s waiting for identity reservation's catalog permit. Both implementations instrument these counters. Candidate service time excludes that wait in both implementations.

All twelve profiled Cargo commands, freshness checks, executable checks, incremental-output checks and both store integrity checks passed. Worker and I/O activity drained with no sealing errors. Cache bounds held, all six traced stages supplied 19 syscall counters, and all recordings reported zero lost samples.

Raw recordings are unchanged. CPU and I/O text was re-analyzed with the recorded analyzer commit after capture to recognize accelerated x86 SHA symbols and inlined sealing workers.

Build IDs remain in perf.data. Optional perf build-ID cache updates were disabled for both runs to avoid the postprocessing timeout observed in an earlier experiment.

Separate instrumented runs on the same top node, not elapsed-time comparisons.

Phase and syscall sums overlap with each other and compiler work.

Background completions/cancellations can differ; syscall totals are not normalized per artifact.

CPU totals include post-build idle observations and exclude draining unmount.

Userspace-only CPU stacks; kernel CPU additionally counted through /proc.

No kernel FUSE request tracepoints; boundary-incomplete calls are excluded from interval timing.

Both cases use two workers with the large-artifact import pipeline disabled. The candidate changes only chunker buffering, not publication concurrency.

[Profile JSON](chunker-buffer-costs-profile.json)
