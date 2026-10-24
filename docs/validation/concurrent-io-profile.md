# Re-profile: concurrent I/O works; catalog durability remains costly

The measured executable is `bdf3843`, with the same SHA-256 as the validated
namespace-concurrency implementation and the matched build comparison.

Actual foreground syscall overlap rose from **one** in both saved pre-change
edited-build traces to **three** during the new clean build and **two** during
both new edited-build traces. This counts overlapping native syscall intervals,
not merely configured workers or admitted requests. The uninstrumented benchmark
independently observed four simultaneous native-I/O reservations.

| Profile stage | Peak overlapping foreground calls | Foreground fsync calls | Foreground fsync seconds | All tracked foreground seconds (sum) | Same intervals (union) |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean | 3 | 1199 | 11.15 | 17.80 | 17.44 |
| edit-2-profile | 2 | 1137 | 7.75 | 8.26 | 8.26 |
| edit-3-profile | 2 | 1137 | 5.28 | 5.80 | 5.80 |

Tracked calls are pwrite64, fsync, and fdatasync; reads and other calls are excluded.
The sum and union being nearly equal in edited builds shows that little of the
tracked I/O time actually overlaps, despite the implementation allowing it.
Counters from the matched benchmark place global filesystem-state holds at
16.88–17.20s per full sequence, down from 58.56–60.92s with metadata caching alone.

## What still serializes

Each profiled edit issued **1,136 fsync calls to the catalog WAL and one to the
catalog database**. Unlike the earlier descriptor probe, the new profile captured
the actual daemon's descriptor mapping after every build: descriptor 18 is
catalog.sqlite-wal and 17 is catalog.sqlite. Every foreground fsync in those
edited stages targets the catalog. The two edits spent **7.75s and 5.28s** in
these calls. Moving them outside filesystem state lets unrelated work proceed;
it does not shorten the commits themselves or parallelize SQLite's writer.

Link/unlink/rename of a sealed artifact currently updates its logical timestamps
in a FULL-synchronous transaction before replying. SQLite's WAL/FULL setting
[synchronizes the WAL on each commit](https://www.sqlite.org/pragma.html#pragma_synchronous).
This is the next concrete cost to address. A future change would need to preserve
content-publication ordering and file/directory fsync guarantees; this measured
implementation retains its existing eager durability.

The clean trace also records 23,128 background fsync calls totaling 139.80s.
Those calls overlap compiler work and other daemon threads; that total is not
extra Cargo wall time. Background candidate setup and publication have multiple
durability steps per artifact, so batching or combining safe commit boundaries
is another potential improvement. Neither idea is implemented in this report.

## Validation and limits

All six Cargo commands, freshness/executable checks, incremental-output checks,
and final store integrity checking passed. CPU and I/O event recordings report
zero lost samples and no observed syscall errors. One syscall crosses the clean
recording's end and one crosses edit 2's start; interval analysis excludes these
boundary-incomplete calls. The compressed raw artifact matches its remote hash.

The separate perf-stat collector produced empty CSVs because SIGTERM did not
emit final counts. Those adjunct counts are **unavailable**, not zero, and are
excluded from all conclusions here. The profiler now uses SIGINT for perf stat,
validates every expected counter, and keeps SIGTERM for perf record. Two isolated
probes each captured exactly 100 hard links and 100 unlinks with SIGINT. The
recorded fsync/write events and their timings above are independent of that
collector. The event analyzer also passes six focused parser/overlap tests.

This kernel lacks FUSE request tracepoints. Profile wall times include recording
overhead and use different sampling coverage from the older profile; use the
[matched four-version benchmark](concurrent-io.md) for performance comparisons.
The apparent-idle observation does not guarantee the background queue is drained.

[Profile and event analysis JSON](concurrent-io-profile.json) ·
[Matched HTML tables](concurrent-io.html)
