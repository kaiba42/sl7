# Re-profile after explicit catalog barriers

The measured executable matches the validated implementation and the matched
native/before/after build comparison. The previous version already allowed
concurrent filesystem I/O; this version additionally removes eager disk barriers
from namespace timestamp commits, preserving explicit file/directory fsync.

| Stage | Before foreground fsyncs | Latest foreground fsyncs | Before fsync time (s) | Latest fsync time (s) | Latest peak overlapping tracked calls |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean | 1199 | 16 | 11.147 | 0.076 | 4 |
| edit-2-profile | 1137 | 0 | 7.747 | 0.000 | 1 |
| edit-3-profile | 1137 | 3 | 5.276 | 0.321 | 1 |

These are actual recorded syscall counts/timings, not configured worker counts.
The earlier edited traces issued 1,136 WAL syncs and one database sync apiece.
Descriptor attribution and foreground/background breakdowns for the new traces
are included in the JSON. Tracking covers fsync, fdatasync, and pwrite64; it does
not measure overlap of every daemon operation.

For the full profiled sequence, namespace commits took
0.456s cumulatively; catalog-permit waits totaled
5.917s. Explicit catalog barriers were attempted
16 times, taking 0.078s. Filesystem-state
holds totaled 17.613s. These totals may overlap and cannot
be subtracted from Cargo wall time.

## Remaining costs and concurrency

The clean trace records 26,548
complete background fsyncs totaling
157.76s cumulatively.
They overlap compiler work and are not additional elapsed build seconds. Candidate
setup and publication took 70.12s and
73.53s across the full sequence; both still serialize catalog
writers. Combining safe publication transactions and narrowing that permit around
native retirement are concrete follow-ups to investigate.

The clean stage also counted
1,442,677
getxattr calls. Caching inode representation state could reduce repeated metadata
lookups, but call volume alone does not establish their elapsed-time cost. The
current CPU sampling captures user-space stacks; a kernel-inclusive profile would
help distinguish metadata syscall cost from hashing and compression.

Four tracked foreground calls overlapped in the clean trace. The edited traces
show one at a time after the long foreground syncs disappear. Peak overlap is not
a speedup metric: the work left on the critical path matters more than the number
of threads configured. Extra foreground workers alone have no demonstrated benefit
for these edited-build traces.

## Validation and limits

All six Cargo commands, freshness/executable checks, incremental-output checks,
and final store integrity checks passed. CPU and I/O recordings reported zero
lost samples and no observed syscall errors. Boundary-incomplete calls are listed
in JSON and excluded from timing intervals. The separate perf-stat collector now
emitted all 19 requested syscall counters in each traced stage; missing counters
are rejected. The compressed artifact matches its remote hash, and the executable
matches the validated binary hash.

Instrumented wall times are not the performance comparison. The old and new
profiles are separate observations on a shared node. Use the
[matched uninstrumented builds](catalog-barrier.md) for speedup estimates. The
kernel lacks FUSE request tracepoints. A CPU-idle observation after a build does
not prove that the background queue is drained.

[Full profile JSON](catalog-barrier-profile.json) · [HTML build tables](catalog-barrier.html)
