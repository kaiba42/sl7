# Removing the publication lock from filesystem state

SL7 `ab25491` moves background candidate setup, manifest publication, and sparse retirement outside the filesystem-wide state mutex. It also uses four FUSE request workers so a waiting request need not stall every request. The comparison is against `de54dfc` (the previous `dc1328a` runtime), with unchanged chunking, compression, cache budgets, and durability order.

Clean builds took **24.6% less time**. Publication state-lock holds fell from **65–69 seconds to about 9.8 milliseconds per sequence**. Edited rebuilds had an **11.0% slower median**, with overlapping sample ranges; this is a clean-build improvement with an observed rebuild tradeoff.

## Matched debug builds

Median seconds (minimum–maximum). Two clean builds and six successive one-line rebuild samples per binary, across two sequences with reversed binary order. This is a focused debug/incremental comparison; native and release were not rerun.

| Operation | Before | After | Time reduction |
| --- | ---: | ---: | ---: |
| Clean build | 286.59 (282.52–290.65) | 215.96 (215.66–216.26) | 24.6% |
| One-line rebuild | 21.54 (20.61–27.42) | 23.92 (21.67–25.54) | -11.0% |
| Unchanged build | 1.17 (1.15–1.30) | 1.25 (1.22–1.38) | -6.6% |
| Unchanged after remount | 1.23 (1.23–1.23) | 1.29 (1.28–1.29) | -4.9% |

Cargo wall times stop when Cargo exits. Additional background-idle observation is recorded separately and is not a durability barrier. Cargo plus that observation:

| Operation | Before | After |
| --- | ---: | ---: |
| Clean build | 291.46 (287.34–295.58) | 223.73 (223.28–224.18) |
| One-line rebuild | 23.44 (22.61–27.72) | 25.25 (23.57–25.87) |

Daemon CPU per edited build changed from 14.12s to 16.44s. The publication lock is removed, but this change does not solve edited-build overhead; foreground serialization and the cost of the new coordination need further profiling.

## Lock measurements

Seconds per complete pre-remount sequence (clean, three unchanged builds, three edits). The old `publish_seconds` measured publication while holding filesystem state. In the new code that expensive phase runs outside state; `publication_state_seconds` measures the short validation/gate/result/cleanup sections instead. These are cumulative hold times, not automatically removable build time.

| Round / binary | Publication work | Publication state holds | Longest publication state hold | Candidate setup outside state | Selection state holds |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / Before | 65.536 | 65.535654 | 0.617583 | not measured | not measured |
| 1 / After | 65.212 | 0.009763 | 0.000028 | 59.550 | 0.178662 |
| 2 / After | 66.387 | 0.009809 | 0.000027 | 61.082 | 0.177318 |
| 2 / Before | 69.261 | 69.260765 | 0.626355 | not measured | not measured |

Remaining measured costs in the new implementation:

| Round | Foreground state holds | Longest foreground hold | Catalog-permit wait | Same-inode open wait |
| --- | ---: | ---: | ---: | ---: |
| 1 | 122.002 | 0.175 | 23.294 | 0.330 |
| 2 | 123.706 | 0.618 | 19.658 | 0.207 |

Wait totals can overlap across request threads. Foreground reconstruction, hydration, native I/O, and metadata operations still serialize under filesystem state. Catalog-writing mutations still serialize with publication through a separate permit. Four simultaneous waiting requests can occupy the request pool; this change does not promise unlimited independent progress.

## Storage and correctness

| Binary | Retained storage before GC | Normal pre-remount drain |
| --- | ---: | ---: |
| Before | 1.886 GiB (1.880–1.892) | 0.44 (0.44–0.44) s |
| After | 1.793 GiB (1.792–1.793) | 0.44 (0.44–0.45) s |

Retained storage includes native allocation, catalog, referenced packs, and orphan packs. Concurrent packing can change cancellation and orphan counts; no post-GC storage comparison was performed.

All 70 copied evidence files matched their remote SHA-256 digests. All 32 Cargo commands and four store integrity checks passed. All 16 corresponding compiler-artifact graphs matched in package, target, profile, features, and freshness. Debug incremental files were present in every case. Built executables displayed the expected description after each clean build and edit; unchanged and remounted builds were fully fresh. No sealing errors were reported.

The change passed 48 Linux Rust tests, 28 host tests, strict Clippy, and all four mounted suites (read-only, native staging, writable deduplication, and SL7 self-build). New tests pause publication and prove unrelated reconstruction and native writes can finish while it remains paused; same-inode readers and catalog writers wait, then resume. Failure injection preserves native bytes and permits retry. Existing stale-generation, open-reader, process-death, clean/GC, and recovery tests also passed.

## Method and reproduction

Oaktown namespace `kaiba`, node `top`, AMD Ryzen 9 7940HS, eight-CPU quota, 12 GiB RAM, disk-backed temporary workspace. Rust 1.97.1, Linux 6.12.58, file-descriptor limit 524288. The fixed source/lock revision, image digest, binary hashes, resource counters, individual samples, and all daemon counters are in [the JSON report](publication-lock.json). Source and output paths, compiler, jobs, and profiles were identical. Dependencies were prefetched, each case started with empty outputs and an empty store, and OS caches were not dropped. No competing SL7 tests ran during measurement. Shared-node activity and two sequence pairs limit precision.

Use binaries built from the two recorded revisions and an isolated Linux source archive:

```sh
ulimit -Sn 524288
SL7_BINARY=/bench/bin/sl7-current python3 scripts/benchmark-synkro.py \
  --source /bench/source --results /bench/results \
  --revision ec6d4c53b8fdf4321de220a922f7efce67788d40 --rounds 2 --jobs 8 \
  --baseline-binary /bench/bin/sl7-baseline \
  --profiles debug --backends baseline-sl7 sl7
```

The [previous native/original/current comparison](oaktown-build-performance.md) remains the broader baseline. Its absolute native timings are not a fresh paired control for this change. The [locking protocol](../live-format.md) documents the coordination and unchanged crash-recovery order.
