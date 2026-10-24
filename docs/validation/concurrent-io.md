# Concurrent native I/O and namespace catalog commits

Clean build: 176.32s with both concurrency changes, 183.72s with metadata caching alone, and 149.22s native. One-line rebuild: 14.21s with both concurrency changes, 16.65s with metadata caching alone, and 3.82s native.

## Matched debug/incremental builds

Median seconds (minimum–maximum). Two reversed-order rounds, with two clean,
six edited, six unchanged, and two remounted samples per backend. All samples
are included. Shared-node activity and small sample counts limit precision.
Release was not rerun.

| Operation | Native Cargo | Metadata cache | Native I/O reservations | Plus namespace commits |
| --- | ---: | ---: | ---: | ---: |
| Clean build | 149.22 (149.14–149.31) | 183.72 (183.08–184.36) | 174.19 (171.50–176.87) | 176.32 (173.57–179.07) |
| One-line rebuild | 3.82 (3.78–7.36) | 16.65 (12.28–21.77) | 16.14 (12.96–18.51) | 14.21 (11.49–17.46) |
| Unchanged build | 0.31 (0.28–0.36) | 0.40 (0.39–2.86) | 0.44 (0.37–2.47) | 0.42 (0.36–2.41) |
| Unchanged after remount | 0.28 (0.28–0.29) | 0.56 (0.53–0.60) | 0.56 (0.54–0.58) | 0.61 (0.60–0.61) |

The full version was faster in 3 of 6 matched edited-build stages versus metadata caching alone. The median improvement is uneven across samples, not a dependable per-build speedup.

Adding namespace commit isolation did not improve clean-build timing over native
I/O reservations alone: its median was 1.2% higher, with overlapping ranges.

The metadata-cache backend includes the prior kernel-page retention change. The
next backend adds inode-scoped native writes and file/directory synchronization.
The last also moves link/unlink/rename catalog commits outside filesystem state.
SQLite still serializes writers and uses FULL synchronization before successful
namespace replies. The four-worker count, compression, chunking, and shared read
cache limits are unchanged.

## Where the global lock is no longer held

| Round / backend | Foreground state (s) | Native I/O outside state (s) | Peak native I/O | Namespace commits outside state (s) | Catalog wait (s) |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / Metadata cache | 58.56 | unmeasured | unmeasured | unmeasured | 29.07 |
| 2 / Metadata cache | 60.92 | unmeasured | unmeasured | unmeasured | 21.80 |
| 1 / Native I/O reservations | 47.65 | 11.37 | 4 | unmeasured | 24.21 |
| 2 / Native I/O reservations | 41.37 | 10.95 | 4 | unmeasured | 20.99 |
| 1 / Plus namespace commits | 17.20 | 9.35 | 4 | 26.85 | 14.57 |
| 2 / Plus namespace commits | 16.88 | 8.48 | 4 | 33.66 | 18.29 |

Counters cover clean, three unchanged, and three edited builds per sequence.
These are cumulative operation times, may overlap, and cannot be subtracted from
Cargo wall time. Missing counters are labeled unmeasured, not treated as zero.
Hydration, truncation, chmod/utime, and brief namespace/metadata work still use
filesystem state. Catalog writers remain serial; requests waiting for them can
still occupy the finite worker pool.

## Storage and validation

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| Metadata cache | 1.78 (1.78–1.78) |
| Native I/O reservations | 1.78 (1.78–1.78) |
| Plus namespace commits | 1.79 (1.79–1.79) |

Retained storage includes native allocation, catalogs, and referenced/orphan packs,
before GC. It excludes directory/inode/xattr overhead and host extent sharing.
The savings combine compression and deduplication.

All 64 Cargo commands and six store checks passed. All 48 native-versus-SL7
artifact-graph pairs matched in package, target, features, profile, and freshness.
Every case produced incremental outputs; unchanged/remounted builds stayed fresh.
The copied benchmark archive matches its remote SHA-256. Runtime binaries match
the independent validation records. No sealing errors or leaked active requests
were reported.

The latest implementation passed 57 Linux tests, 35 host tests, strict Clippy,
and all five mounted suites. Deterministic tests pause real native operations
and catalog commits, verify unrelated work completes before resuming them, verify
conflicting mutations wait, and exercise release/forget and unlink lifetimes.
The metadata commit test also proves unchanged content can be read from the same
inode while its timestamp commit is paused.

## Method

Oaktown namespace kaiba, node top, eight CPU quota, 12 GiB RAM, disk-backed
workspace, Rust 1.97.1, Linux 6.12.58, descriptor limit 524288. Identical archived
source revision, lockfile, compiler, jobs, paths, and workspace profiles. Outputs
and stores begin empty; dependencies are prefetched; OS caches are not dropped.
No other owned build/profile workload overlaps the benchmark.

Cargo wall time ends at Cargo exit. JSON also retains per-sample daemon CPU and
post-build idle observation, which is not a durability barrier. CPU includes
observation and executable verification; normal unmount drain is reported
separately. The older page-cache report is a separate experiment and is not a
paired comparison for isolating metadata caching.

[JSON measurements](concurrent-io.json) · [HTML tables](concurrent-io.html) ·
[Native-I/O validation](native-io-validation.json) ·
[Namespace validation](namespace-validation.json)
