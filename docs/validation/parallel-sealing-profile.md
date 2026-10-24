# Re-profile of parallel sealing

The executable matches the validated implementation `04f45b58bbbd6b26af0098aefa3ce822e6d87be0`. The comparison
profile is [publication-costs](publication-costs-profile.md); these are separate
instrumented observations, so use the [matched Cargo timings](parallel-sealing.md)
for elapsed-time conclusions.

| Stage | Previous getxattr calls | Current getxattr calls | Daemon user CPU (s) | Daemon kernel CPU (s) | Background fsync calls | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 1,430,927 | 1,437,778 | 32.33 | 45.30 | 21,499 | 2 |
| edit-2-profile | 50,935 | 51,386 | 5.80 | 3.97 | 204 | 2 |
| edit-3-profile | 52,012 | 49,609 | 5.05 | 3.78 | 66 | 2 |

The daemon used 2 sealing workers, with at most
2 overlapping seal jobs and
2 overlapping preparation phases. Preparation overlap
is distinct from CPU utilization. Across the complete sequence, candidate setup
summed to 68.79s, packing 114.65s,
catalog publication 42.91s, and native retirement
31.12s. Filesystem-state holds summed to
19.65s and catalog-permit waits to
14.75s. These times can overlap.

All six Cargo commands, freshness/executable checks, incremental-output checks,
and final store integrity checking passed. Workers and I/O leases drained without
recorded sealing errors, and cache bounds held. Each traced stage contains all 19
requested syscall counters. All CPU and I/O recordings report zero lost samples;
no paired I/O syscall errors were observed. Analysis counted 0 exact duplicate
events once; the JSON exposes them and all boundary-incomplete calls.

Background work completed inside each trace can differ because of cancellation
and recording boundaries. Counts are not normalized per artifact. CPU totals
include the post-build idle observation, which does not prove that sealing has
drained; they exclude profiler shutdown and unmount. User/kernel counters are CPU
time across daemon threads, not elapsed latency or time asleep waiting for I/O.
Stack sampling remains user-space only. FUSE request tracepoints are unavailable.

[Full profile JSON](parallel-sealing-profile.json) · [Build results](parallel-sealing.md)
