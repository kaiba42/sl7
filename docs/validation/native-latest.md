# Latest SL7 versus native Cargo storage

This comparison predates the [concurrent-read implementation and measurements](parallel-reads.md).

A fresh matched comparison of native storage and SL7 after publication was moved outside the filesystem state mutex. Both run the same Cargo/rustc commands against the full Synkro workspace. SL7 runtime: `ab25491`; checkout: `8f48ea6`.

Clean builds took 43% more time in debug and 21% more in release. Debug one-line rebuilds took 5.40× native time; release rebuilds took 17% more time. Retained storage was 74% smaller in debug and 73% smaller in release.

## Build times

Median seconds (minimum–maximum). Two clean samples and six one-line-edit/unchanged samples per backend and profile, across two reversed-order rounds. Ratios above 1 mean SL7 is slower.

| Profile / operation | Native | Latest SL7 | SL7/native | Additional time |
| --- | ---: | ---: | ---: | ---: |
| Debug · Clean build | 149.54 (148.80–150.28) | 213.11 (209.94–216.28) | 1.43× | +63.57s |
| Debug · One-line rebuild | 4.58 (4.17–4.69) | 24.73 (21.63–26.93) | 5.40× | +20.15s |
| Debug · Unchanged build | 0.28 (0.28–0.29) | 1.23 (1.22–2.12) | 4.36× | +0.95s |
| Release · Clean build | 170.92 (170.58–171.26) | 206.39 (205.90–206.88) | 1.21× | +35.47s |
| Release · One-line rebuild | 39.88 (39.66–40.06) | 46.58 (46.53–48.96) | 1.17× | +6.70s |
| Release · Unchanged build | 0.29 (0.29–0.35) | 1.32 (1.31–1.38) | 4.58× | +1.04s |

The samples are small and consecutive edits share a build tree. Ranges and per-case resource counters are retained; these are observed timings, not a statistical guarantee for other machines or projects.

## Retained storage

After three edits, before GC. GiB uses 2³⁰ bytes.

| Profile | Native | Latest SL7 | Saving |
| --- | ---: | ---: | ---: |
| Debug | 6.83 GiB | 1.79 GiB | 73.8% |
| Release | 1.87 GiB | 0.51 GiB | 72.6% |

Native storage counts allocated regular-file blocks with hard links counted once. SL7 adds native allocated blocks and all content-store files, including catalog/WAL/SHM, packs, temporary files, and uncollected history/orphans. Directory/inode/xattr overhead and host extent sharing are excluded. The saving combines compression and deduplication; it does not isolate sharing across worktrees. No post-GC comparison was performed.

## Background settling and remounts

Cargo time stops at Cargo exit. Additional background-idle observation is recorded separately; it is not a durability barrier. Normal unmount drains the worker and must exit successfully. Only SL7 remounts; the corresponding native stage is another unchanged build. Each column reports its median independently; Cargo + observation is summed per sample before taking the median.

| Profile / operation | Native Cargo | SL7 Cargo | SL7 extra idle observation | SL7 Cargo + observation |
| --- | ---: | ---: | ---: | ---: |
| Debug · Clean build | 149.54s | 213.11s | 8.12s | 221.23s |
| Debug · One-line rebuild | 4.58s | 24.73s | 1.80s | 25.79s |
| Release · Clean build | 170.92s | 206.39s | 0.80s | 207.19s |
| Release · One-line rebuild | 39.88s | 46.58s | 0.50s | 47.16s |

Normal unmount drain ranged from 0.010s to 0.441s. All unchanged and remounted builds stayed fresh.

| Profile / after-remount operation | Native unchanged | SL7 remounted |
| --- | ---: | ---: |
| Debug | 0.27 (0.27–0.27) | 1.30 (1.30–1.31) |
| Release | 0.29 (0.29–0.30) | 1.41 (1.39–1.42) |

## Validation and method

All eight cases, 64 Cargo commands, and four SL7 integrity checks passed. All 32 corresponding compiler-artifact graphs matched in package, target, profile, features, and freshness. The built Synkro CLI displayed the expected description after each clean build and edit. No sealing errors were reported. All 137 copied evidence files matched their remote SHA-256 digests.

Every debug case produced incremental files; release cases did not. Incremental environment overrides were unset, preserving workspace profile defaults. The source revision and Cargo.lock are identical to the earlier benchmarks, but all timings here are newly measured together.

Oaktown namespace `kaiba`, node `top`, AMD Ryzen 9 7940HS, eight-CPU quota, 12 GiB RAM, disk-backed emptyDir, eight Cargo jobs, Rust 1.97.1, Linux 6.12.58, file-descriptor limit 524288. Peak pod-lifetime memory was 10.35 GiB. Memory events: `low 0; high 0; max 0; oom 0; oom_kill 0; oom_group_kill 0`.

Round 1 ran native then SL7, debug then release. Round 2 reversed both orders. Every case began with empty outputs and a fresh SL7 store; source and output paths, compiler, lockfile, features, profiles, and jobs were identical. Dependencies were fetched before timing, OS page caches were not flushed, and no other SL7 validation jobs ran concurrently. Shared-node activity can still affect results. Individual samples, exact source/binary/image identifiers, resource counters, storage accounting, and daemon counters are in [the JSON report](native-latest.json).

The previous [publication-lock comparison](publication-lock.md) isolates the latest code change; the earlier [native/original/optimized comparison](oaktown-build-performance.md) remains historical context. Absolute timings across these separate runs should not be treated as another paired optimization comparison.

## Reproduction

Build SL7 from the recorded checkout and prepare an isolated Linux archive of the recorded Synkro revision with prefetched dependencies, FUSE access, and the same resource limits. Then run:

```sh
ulimit -Sn 524288
TMPDIR=/bench/tmp SL7_BINARY=/bench/bin/sl7-current \
  python3 scripts/benchmark-synkro.py \
  --source /bench/source --results /bench/results \
  --revision ec6d4c53b8fdf4321de220a922f7efce67788d40 \
  --rounds 2 --jobs 8 --backends native sl7
```

Raw Cargo logs and the private source archive remain local; the published report contains aggregate measurements. [HTML table](native-latest.html).
