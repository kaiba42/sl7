# Re-profile of buffered pack writes

The executable matches the validated implementation `4db4330199a3b29003a9f748ac8f9239c134b66d`. The comparison
profile is [seal-check-costs](seal-check-costs-profile.md); these are separate
instrumented observations, so use the [matched Cargo timings](pack-buffer-costs.md)
for elapsed-time conclusions.

| Stage | Previous write calls | Current write calls | Daemon user CPU (s) | Daemon kernel CPU (s) | Background fsync calls | Peak overlapping background tracked syscalls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 1,370,080 | 19,752 | 29.91 | 39.01 | 17,589 | 2 |
| edit-2-profile | 66,833 | 1,242 | 4.12 | 3.28 | 54 | 2 |
| edit-3-profile | 44,331 | 814 | 2.86 | 2.91 | 48 | 2 |

The daemon used 2 sealing workers, with at most
2 overlapping seal jobs and
2 overlapping preparation phases. Preparation overlap
is distinct from CPU utilization. Across the complete sequence, candidate setup
summed to 45.80s, packing 113.89s,
catalog publication 45.65s, and native retirement
29.78s. Filesystem-state holds summed to
12.78s and catalog-permit waits to
12.74s. These times can overlap.

Write counts cover the entire daemon, including foreground responses and background packing. They are not pack-only counts; the separate public-artifact probe isolates pack writes.

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

[Full profile JSON](pack-buffer-costs-profile.json) · [Build results](pack-buffer-costs.md)
