# Re-profile of the native-state cache

The executable matches the validated implementation `0685bac24b8e74694a1e8922e25a6054929464cb`. The comparison
profile is [parallel-sealing](parallel-sealing-profile.md); these are separate
instrumented observations, so use the [matched Cargo timings](native-state-costs.md)
for elapsed-time conclusions.

| Stage | Previous getxattr calls | Current getxattr calls | Daemon user CPU (s) | Daemon kernel CPU (s) | Background fsync calls | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 1,437,778 | 302,971 | 28.96 | 39.99 | 21,299 | 2 |
| edit-2-profile | 51,386 | 15,082 | 3.74 | 3.05 | 62 | 2 |
| edit-3-profile | 49,609 | 14,711 | 3.24 | 2.90 | 59 | 2 |

The daemon used 2 sealing workers, with at most
2 overlapping seal jobs and
2 overlapping preparation phases. Preparation overlap
is distinct from CPU utilization. Across the complete sequence, candidate setup
summed to 70.06s, packing 115.11s,
catalog publication 43.32s, and native retirement
32.67s. Filesystem-state holds summed to
13.32s and catalog-permit waits to
11.84s. These times can overlap.

The native-state cache recorded 279,024 hits and 21,805 representation checks during mutation selection. These counters describe that selection path, not every metadata or xattr operation.

The clean observation contains about 79% fewer `getxattr` calls than the earlier
profile. Kernel CPU remains 39.99s, while candidate setup and publication still
consume substantial cumulative time. These observations help locate remaining
work; the matched build comparison establishes the modest elapsed-time gain.

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

[Full profile JSON](native-state-costs-profile.json) · [Build results](native-state-costs.md)
