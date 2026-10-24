# Re-profile of cached seal decisions

The executable matches the validated implementation `bcd8da2f495cc1415a4a98200cc956c1d82024f8`. The comparison
profile is [identity-batch-costs](identity-batch-costs-profile.md); these are separate
instrumented observations, so use the [matched Cargo timings](seal-check-costs.md)
for elapsed-time conclusions.

| Stage | Previous getxattr calls | Current getxattr calls | Daemon user CPU (s) | Daemon kernel CPU (s) | Background fsync calls | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 313,017 | 303,869 | 31.02 | 40.29 | 18,274 | 2 |
| edit-2-profile | 15,610 | 13,099 | 3.86 | 3.02 | 53 | 1 |
| edit-3-profile | 14,487 | 13,024 | 2.82 | 2.84 | 48 | 2 |

The daemon used 2 sealing workers, with at most
2 overlapping seal jobs and
2 overlapping preparation phases. Preparation overlap
is distinct from CPU utilization. Across the complete sequence, candidate setup
summed to 48.40s, packing 117.74s,
catalog publication 47.60s, and native retirement
29.56s. Filesystem-state holds summed to
12.59s and catalog-permit waits to
16.25s. These times can overlap.

The scheduler avoided 10,977 unchanged checks. It completed 3,669 seals, canceled 649 candidates, and skipped 4,382 selected candidates. These are transitions, not unique artifacts.

All six Cargo commands, freshness/executable checks, incremental-output checks,
and final store integrity checking passed. Workers and I/O leases drained without
recorded sealing errors, and cache bounds held. Each traced stage contains all 19
requested syscall counters. All CPU and I/O recordings report zero lost samples;
no paired I/O syscall errors were observed. Analysis counted 1 exact duplicate
events once; the JSON exposes them and all boundary-incomplete calls.

Background work completed inside each trace can differ because of cancellation
and recording boundaries. Counts are not normalized per artifact. CPU totals
include the post-build idle observation, which does not prove that sealing has
drained; they exclude profiler shutdown and unmount. User/kernel counters are CPU
time across daemon threads, not elapsed latency or time asleep waiting for I/O.
Stack sampling remains user-space only. FUSE request tracepoints are unavailable.

[Full profile JSON](seal-check-costs-profile.json) · [Build results](seal-check-costs.md)
