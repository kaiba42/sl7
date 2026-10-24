# Concurrent file reconstruction

SL7 `5f300bb` moves read reconstruction outside filesystem state: independent read-only catalog connections, pack reads, decompression, checksum verification, and output copying can overlap across requests. Both binaries use **four FUSE request workers**, the same shared cache budgets, chunking, compression, and durability protocol. The baseline is `2c41fc4` (runtime `ab25491`), which already moved publication outside state.

Clean build median fell by **2.1%** (217.05s → 212.39s). One-line rebuild median fell by **17.3%** (25.18s → 20.83s).

Clean builds were slower in the first two matched pairs and faster in the confirmation pair. The pooled median therefore does not establish a repeatable clean-build speedup. The edited-build comparison is the more encouraging result; overlapping sample ranges and only three sequences still limit precision.

Across all nine edited rebuilds, cumulative Cargo wall time fell from 222.75s to 199.10s (10.6% less time). This total and the per-build median describe different summaries of the same retained samples.

## Matched debug builds

Median seconds (minimum–maximum). Three clean samples and nine edit/unchanged samples per binary. The initial two rounds reversed binary order; a third before/after confirmation pair was added after the second new-binary clean build took 275.69s versus 212.39s in its first run. All samples remain included. Debug incremental compilation was enabled. Native and release were not rerun.

| Operation | Before | Concurrent reads | Time reduction |
| --- | ---: | ---: | ---: |
| Clean build | 217.05 (205.46–217.79) | 212.39 (200.57–275.69) | +2.1% |
| One-line rebuild | 25.18 (21.41–27.08) | 20.83 (18.93–25.58) | +17.3% |
| Unchanged build | 1.24 (1.22–2.25) | 1.25 (1.23–3.80) | -0.9% |
| Unchanged after remount | 1.32 (1.29–1.39) | 1.29 (1.28–1.31) | +1.7% |

| Round | Before clean | After clean | Before edit median | After edit median |
| --- | ---: | ---: | ---: | ---: |
| 1 | 205.46 | 212.39 | 22.89 | 19.96 |
| 2 | 217.79 | 275.69 | 26.25 | 23.79 |
| 3 (confirmation) | 217.05 | 200.57 | 25.18 | 20.06 |

The initial two-round comparison alone gave clean medians 211.62s → 244.04s and edit medians 25.00s → 22.31s. The third round is a follow-up measurement, not a replacement for either earlier round.

Cargo wall time stops at Cargo exit. Background-idle observation is additional time, not a durability barrier. Daemon CPU is sampled through idle observation and executable verification, not just the Cargo interval. Each column below is an independently calculated median; Cargo-plus-observation sums are computed per sample before taking the median.

| Operation | Before: Cargo + observation | After: Cargo + observation | Before: daemon CPU | After: daemon CPU |
| --- | ---: | ---: | ---: | ---: |
| Clean build | 224.47 (220.50–224.90) | 218.71 (212.79–290.62) | 127.20 (123.82–131.15) | 126.62 (120.33–142.02) |
| One-line rebuild | 26.55 (23.51–28.77) | 22.64 (20.84–27.38) | 16.58 (16.29–17.65) | 16.61 (16.43–21.14) |

## What actually runs concurrently

Each read briefly pins its inode and handle under filesystem state, then checks out one of four independent read-only SQLite connections. The decoded-payload cache, extent cache, and pack-handle cache remain shared. Their mutex covers bookkeeping; pack opens, reads, decompression, checksum verification, and copying run outside it. The cache retains at most 128 MiB of decoded payload, 32 MiB / 4096 extent windows, and 64 pack handles. Active readers can retain evicted objects until their request finishes, so these are cache accounting bounds rather than total RSS bounds.

Per-inode read leases prevent hydration, truncation, replacement, or publication from invalidating a read in flight. Mutation waiters release filesystem state and the catalog permit while waiting. They block new readers of the affected inodes and re-resolve names after waking. Reads of unrelated files can continue. Release/forget cannot retire an inode still being read.

Measurements below cover each full pre-remount sequence: clean build, three unchanged builds, and three edits. Times are cumulative seconds, not automatically recoverable wall time.

| Round / binary | Foreground state holds | Longest foreground hold | Catalog-permit wait | Publication state holds |
| --- | ---: | ---: | ---: | ---: |
| 1 / Before | 122.245 | 0.701 | 13.659 | 0.010103 |
| 1 / Concurrent reads | 80.283 | 0.295 | 21.252 | 0.010146 |
| 2 / Concurrent reads | 95.546 | 1.088 | 29.025 | 0.010765 |
| 2 / Before | 125.197 | 0.183 | 25.320 | 0.009584 |
| 3 / Before | 123.593 | 0.630 | 23.675 | 0.009965 |
| 3 / Concurrent reads | 73.655 | 0.173 | 14.568 | 0.009893 |

| Round | Peak read leases | Peak payload loads | Read bookkeeping holds | Read lease lifetime total | Payload load lifetime total | Mutation wait on readers |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 4 | 4 | 0.166 | 52.849 | 26.532 | 0.069 |
| 2 | 4 | 4 | 0.212 | 58.624 | 27.779 | 0.078 |
| 3 | 4 | 4 | 0.168 | 52.287 | 26.404 | 0.049 |

Peak payload loads count overlapping in-flight load scopes, including scheduling and cache-insertion time; they are not a claim that this many cores were continuously busy. Deterministic tests separately pause two cold loads inside their reconstruction closures and require both to enter before either can finish. The benchmark wall times measure the resulting application effect. Read-lease lifetime excludes initial filesystem-state acquisition but includes connection-pool and state-reacquisition waits, so it must not be compared to the old state-lock total as equivalent work. All active read/load counters returned to zero at shutdown.

Foreground writes, hydration, fsync, and metadata callbacks still use filesystem state. Catalog writers still serialize through the publication permit. This implementation parallelizes read reconstruction; it does not eliminate every filesystem bottleneck.

## Shared cache and storage

| Round / binary | Decoded GiB | Chunk hit rate | Duplicate loads | Cached MiB | Map MiB | Map windows | Pack handles |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / Before | 16.08 | 40.5% | not measured | 128.00 | 10.68 | 4096 | 64 |
| 1 / Concurrent reads | 16.07 | 39.7% | 2796 | 127.99 | 10.67 | 4096 | 64 |
| 2 / Concurrent reads | 16.00 | 40.0% | 2383 | 127.99 | 10.68 | 4096 | 64 |
| 2 / Before | 16.06 | 40.5% | not measured | 127.99 | 10.70 | 4096 | 64 |
| 3 / Before | 16.05 | 40.9% | not measured | 127.99 | 10.69 | 4096 | 64 |
| 3 / Concurrent reads | 16.05 | 40.5% | 2871 | 128.00 | 10.69 | 4096 | 64 |

Duplicate loads are concurrent misses that found the same chunk already inserted when they completed. They can spend extra decode work but do not double-charge the cache. Cache accounting stayed within the existing limits in all three new sequences.

| Binary | Retained GiB before GC | Normal pre-remount drain (seconds) |
| --- | ---: | ---: |
| Before | 1.797 (1.793–1.798) | 0.45 (0.45–0.45) |
| Concurrent reads | 1.789 (1.787–1.793) | 0.44 (0.44–0.45) |

Storage includes native allocation, catalog, referenced packs, and orphan packs, before GC. Packing order and cancellation can change retained history. No post-GC storage comparison was performed.

## Correctness and evidence

All 48 Cargo commands and six store integrity checks passed. All 24 corresponding compiler-artifact graphs matched in package, target, profile, features, and freshness. Incremental files were present in every case. Executables displayed the expected description after each clean/edit build; unchanged and remounted builds stayed fresh. No sealing errors were reported. All 105 copied evidence files matched their remote SHA-256 digests.

The implementation passed 53 Linux Rust tests, 35 host tests, strict Clippy, and all four mounted suites: read-only, native staging, writable deduplication, and SL7 self-build. Concurrency tests cover overlapping distinct/duplicate chunk loads, shared accounting, same-inode writer exclusion, unrelated read progress, release/forget during a read, rename destination re-resolution, error cleanup, and read-only connection/store-owner lifetime. Existing mmap, process-death, recovery, and clean/GC checks also passed.

## Resource variation

| Round / binary | Cgroup CPU seconds | CPU pressure: some | I/O pressure: some | CPU throttled seconds |
| --- | ---: | ---: | ---: | ---: |
| 1 / Before | 1341.57 | 1.22 | 13.26 | 1.84 |
| 1 / Concurrent reads | 1336.66 | 1.35 | 7.80 | 2.50 |
| 2 / Concurrent reads | 1674.73 | 16.46 | 9.50 | 1.99 |
| 2 / Before | 1333.01 | 1.24 | 15.74 | 2.13 |
| 3 / Before | 1348.19 | 1.30 | 16.40 | 2.36 |
| 3 / Concurrent reads | 1333.08 | 1.27 | 7.98 | 2.05 |

Counters cover whole cases, including verification and store checking. Pressure and throttling are reported cumulative counters; they are not adjustments to build times and must not be subtracted from the samples. Variation in these counters cannot by itself distinguish external contention from effects of the implementation. No samples were discarded.

## Method and reproduction

Measured 2026-10-24. Oaktown namespace `kaiba`, node `top`, AMD Ryzen 9 7940HS, eight-CPU quota, 12 GiB RAM, disk-backed temporary workspace. Rust 1.97.1, Linux 6.12.58, descriptor limit 524288. Source revision, lockfile, compiler, jobs, and profile were identical; source/output paths matched within each paired invocation. Dependencies were prefetched; every case began with empty outputs and an empty store. OS caches were not dropped. No other SL7 tests ran during measurement. Shared-node activity and three sequence pairs limit precision.

```sh
ulimit -Sn 524288
SL7_BINARY=/bench/bin/sl7-current python3 scripts/benchmark-synkro.py \
  --source /bench/source --results /bench/results \
  --revision ec6d4c53b8fdf4321de220a922f7efce67788d40 --rounds 2 --jobs 8 \
  --baseline-binary /bench/bin/sl7-baseline \
  --profiles debug --backends baseline-sl7 sl7
```

For the third confirmation pair, rerun with `--rounds 1` and a separate results directory. Source and mount paths are identical within each invocation; the temporary mount prefix differs between the initial and confirmation runs.

[JSON measurements](parallel-reads.json) include binary hashes, resources, samples, and daemon counters. [HTML tables](parallel-reads.html) provide a compact view. The earlier [native comparison](native-latest.md) is historical context, not a fresh native control for this change. See the [locking protocol](../live-format.md) for the correctness boundaries.
