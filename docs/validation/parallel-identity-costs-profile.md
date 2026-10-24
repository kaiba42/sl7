# Re-profile of parallel inode identity synchronization

Both binaries ran on Oaktown middle with two sealing workers. Use the [matched Cargo comparison](parallel-identity-costs.md) for elapsed-time conclusions. These profiles identify where work and waits occur.

| Stage / binary | Daemon user CPU (s) | Kernel CPU (s) | Background fsync calls | Sum fsync time (s) | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean / baseline | 30.20 | 39.12 | 17925 | 145.89 | 2 |
| clean / candidate | 31.60 | 40.09 | 18697 | 145.82 | 2 |
| edit-2-profile / baseline | 6.63 | 3.87 | 175 | 9.19 | 2 |
| edit-2-profile / candidate | 3.81 | 3.45 | 50 | 8.23 | 2 |
| edit-3-profile / baseline | 3.10 | 2.73 | 50 | 0.60 | 2 |
| edit-3-profile / candidate | 3.00 | 2.71 | 49 | 0.66 | 2 |

The baseline completed 3,612 seals, canceled 656, and skipped 3,269. Candidate service summed to 40.62s, packing 113.82s, catalog publication 44.52s, and retirement 29.32s. Filesystem-state holds summed to 13.17s.

The candidate completed 3,621 seals, canceled 758, and skipped 4,667. Candidate service summed to 41.48s, packing 109.58s, catalog publication 45.08s, and retirement 27.97s. Filesystem-state holds summed to 14.10s.

The candidate recorded 2 overlapping candidate phases and 7.68s waiting for identity reservation's catalog permit. The baseline did not instrument these two counters. Candidate service time excludes that wait in both implementations.

All twelve profiled Cargo commands, freshness checks, executable checks, incremental-output checks and both store integrity checks passed. Worker and I/O activity drained with no sealing errors. Cache bounds held, all six traced stages supplied 19 syscall counters, and all recordings reported zero lost samples.

Build IDs remain in perf.data; updating the optional perf build-ID cache was disabled for both runs after a baseline recorder shutdown timeout.

Separate instrumented runs on the same middle node, not elapsed-time comparisons.

Phase and syscall sums overlap with each other and compiler work.

Background completions/cancellations can differ; syscall totals are not normalized per artifact.

CPU totals include post-build idle observations and exclude draining unmount.

Userspace-only CPU stacks; kernel CPU additionally counted through /proc.

No kernel FUSE request tracepoints; boundary-incomplete calls are excluded from interval timing.

Peak candidate phases include phases awaiting catalog access; the independent regression test verifies actual progress while another identity phase is paused.

[Profile JSON](parallel-identity-costs-profile.json)
