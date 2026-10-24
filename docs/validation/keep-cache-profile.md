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

Foreground synchronization is a substantial remaining cost. These syscall totals
include internal storage transitions as well as FUSE callbacks: the trace does
not yet distinguish their call sites. They measure time including scheduling and
I/O waits; concurrent totals overlap and cannot be subtracted from build wall
time. Moving callback synchronization out of global state is a testable next
step, but may leave substantial synchronization inside catalog transitions.

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
