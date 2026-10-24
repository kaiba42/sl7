# Profiling after kernel-page retention

The measured implementation is `14f72b4`, the same executable used in the
[matched build comparison](keep-cache.md). The daemon has four request workers,
independent read catalogs, and retained kernel pages. This profile ran afterward
on the same Oaktown pod with eight CPUs and 12 GiB RAM, with debug incremental
compilation enabled.

| Per edited build | Edit 2 | Edit 3 |
| --- | ---: | ---: |
| Foreground fsync calls | 1,134 | 1,137 |
| Foreground fsync cumulative seconds | 5.49 | 7.75 |
| Foreground pwrite64 cumulative seconds | 0.53 | 0.49 |
| Background fsync cumulative seconds | 3.81 | 4.17 |
| CPU samples in background sealing | 68.5% | 69.9% |

Foreground synchronization is a substantial remaining cost. All 1,134 foreground
fsync calls in edit 2 target descriptor 5; in edit 3, 1,136 target descriptor 5
and one targets descriptor 4. A disposable mount of the same executable identifies
5 as `catalog.sqlite-wal` and 4 as `catalog.sqlite`. This strongly attributes the
observed waits to catalog durability, rather than output-file fsync callbacks.
The trace does not capture their individual call stacks.

These durations include scheduling and I/O waits; concurrent totals overlap and
cannot be subtracted from build wall time. Moving callback synchronization out of
global state alone is therefore unlikely to remove most of this cost. Namespace
metadata commits must also release filesystem state while SQLite synchronizes;
the catalog writer itself remains serial. The durability setting remains FULL.

CPU samples mostly land in background packing/sealing. User CPU sampling omits
blocked time, so this does not establish packing as the build's critical path.
Metadata lookup/getattr and catalog queries remain visible in foreground samples.
The next experiments are bounded metadata caching and inode-scoped reservations
for native writes and synchronization. Neither has a measured speedup in this
report.

The clean build, unchanged builds, control edit, two profiled edits, executable
checks, and final store integrity check passed. All profilers shut down normally.
Every CPU and syscall recording reports zero lost samples. Both syscall traces
have matched enter/exit pairs, no boundary-incomplete calls, and no syscall errors.
The compressed raw artifact was copied and checked against its remote SHA-256.

The kernel lacks FUSE request tracepoints, so the harness records their absence
and continues with daemon-only CPU and syscall profiling. The analyzer supports
both labeled and pretty-printed perf syscall returns, with five parser tests.
Instrumented timings are retained in [JSON](keep-cache-profile.json); use the
separate matched benchmark for performance comparisons.
