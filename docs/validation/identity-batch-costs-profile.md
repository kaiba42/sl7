# Re-profile of batched identity allocation

The executable matches the validated implementation `d15f94009f41c4f82d35bc71b4afbe52575d0343`. The comparison
profile is [native-state-costs](native-state-costs-profile.md); these are separate
instrumented observations, so use the [matched Cargo timings](identity-batch-costs.md)
for elapsed-time conclusions.

| Stage | Previous getxattr calls | Current getxattr calls | Daemon user CPU (s) | Daemon kernel CPU (s) | Background fsync calls | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 302,971 | 313,017 | 35.40 | 41.44 | 19,096 | 2 |
| edit-2-profile | 15,082 | 15,610 | 2.96 | 2.94 | 48 | 2 |
| edit-3-profile | 14,711 | 14,487 | 2.76 | 2.84 | 48 | 2 |

The daemon used 2 sealing workers, with at most
2 overlapping seal jobs and
2 overlapping preparation phases. Preparation overlap
is distinct from CPU utilization. Across the complete sequence, candidate setup
summed to 57.65s, packing 110.69s,
catalog publication 45.45s, and native retirement
28.51s. Filesystem-state holds summed to
13.35s and catalog-permit waits to
19.34s. These times can overlap.

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

[Full profile JSON](identity-batch-costs-profile.json) · [Build results](identity-batch-costs.md)
