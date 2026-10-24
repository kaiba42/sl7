# Re-profile after shortening publication

The measured executable matches the validated atomic-publication implementation
and the matched four-version Cargo comparison. The baseline profile is the prior
explicit-catalog-barrier version, before both new publication changes.

| Stage | Before background fsyncs | Latest background fsyncs | Before background sync time (s) | Latest background sync time (s) | Latest foreground fsyncs | Peak overlapping foreground tracked calls |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 26548 | 20008 | 157.76 | 125.42 | 26 | 4 |
| edit-2-profile | 76 | 907 | 1.86 | 7.37 | 1 | 1 |
| edit-3-profile | 90 | 164 | 1.56 | 1.33 | 3 | 1 |

These are complete recorded calls. Background sealing can complete different
amounts of work during each trace because of cancellation and recording boundaries;
the raw count differences are not normalized per sealed artifact. Sync-time sums
may overlap with compiler work and are not additional elapsed Cargo seconds.
Tracking covers fsync, fdatasync, and pwrite64; it does not measure all daemon work.

Across the full profiled sequence, catalog publication took
30.87s and native retirement took
21.65s. Retirement now runs outside the catalog permit,
with its inode gate retained. Candidate setup still took
73.86s under the writer permit. Packing took
84.82s outside it. Catalog-permit waits totaled
2.41s, and filesystem-state holds totaled
18.81s. Same-inode mutation waits for publication were
0.030s. These counters can overlap.

The clean stage counted
1,430,927
getxattr calls. Repeated representation lookups and candidate setup remain
concrete follow-up targets; syscall volume alone does not establish time spent.

## Validation and limits

All six Cargo commands, freshness/executable checks, incremental-output checks,
and final store integrity checking passed. CPU and I/O recordings report zero
lost samples and no observed I/O syscall errors. Boundary-incomplete calls are
listed in JSON and excluded from timing intervals. Each traced stage contains all
19 requested perf-stat counters. The compressed artifact matches its remote hash,
and the executable matches the validated source/binary record.

One byte-equivalent pwrite64 entry at the same recorded timestamp was emitted twice; analysis counts it once and records the exact duplicate. Conflicting event pairs still fail validation.

Instrumented wall times are not the performance comparison. Use the
[matched uninstrumented builds](publication-costs.md) to assess each change.
The kernel lacks FUSE request tracepoints; CPU sampling covers user-space stacks.
Post-build CPU-idle observation does not prove the background queue is drained.

[Full profile JSON](publication-costs-profile.json) · [HTML tables](publication-costs.html)
