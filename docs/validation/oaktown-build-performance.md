# Synkro build performance on Oaktown

A matched comparison of native storage, the original SL7 (`724d71b`), and current SL7 (`dc1328a`), run serially on one Oaktown x86-64 node. This directly measures the combined implementation changes on one machine. The original OrbStack/ARM64 measurements remain historical context; absolute times across machines are not an optimization comparison.

Current SL7 reduces clean-build time by **26.7% in debug and 22.1% in release** relative to original SL7. Edited rebuilds improve **17.2% and 3.2%**, respectively. Debug rebuilds remain the largest performance gap: current SL7 takes **4.49× native Cargo time** on this workload. These measurements cover the combined changes, not the extent cache in isolation.

## Build times

Median seconds (minimum–maximum). Two clean samples and six edit/unchanged samples per backend/profile.

| Profile / operation | Native | Original SL7 | Current SL7 | Current/native | Original/current |
| --- | ---: | ---: | ---: | ---: | ---: |
| Debug clean build | 149.07 (148.95–149.20) | 407.30 (403.42–411.18) | 298.37 (297.03–299.70) | 2.00× | 1.37× |
| Debug one-line rebuild | 4.81 (3.84–5.86) | 26.07 (25.03–30.83) | 21.58 (20.17–27.25) | 4.49× | 1.21× |
| Debug unchanged build | 0.28 (0.28–0.29) | 1.17 (1.16–1.32) | 1.16 (1.14–3.23) | 4.14× | 1.01× |
| Release clean build | 170.78 (170.28–171.28) | 314.50 (313.59–315.41) | 245.14 (244.95–245.34) | 1.44× | 1.28× |
| Release one-line rebuild | 39.91 (39.82–40.08) | 47.30 (46.97–49.41) | 45.78 (45.23–45.96) | 1.15× | 1.03× |
| Release unchanged build | 0.29 (0.29–0.33) | 1.25 (1.23–1.33) | 1.23 (1.21–1.31) | 4.24× | 1.02× |

Current/native above 1 means slower than native. Original/current above 1 means an improvement over original SL7. Small sample counts and shared-node activity limit precision; subsecond unchanged-build differences should not be treated as reliable speedups.

## Retained storage before GC

| Profile | Native | Original SL7 | Current SL7 | Current saving |
| --- | ---: | ---: | ---: | ---: |
| Debug | 6.83 GiB | 1.80 GiB | 1.89 GiB | 72.3% |
| Release | 1.87 GiB | 0.51 GiB | 0.66 GiB | 64.7% |

Measured after three edits. Native uses allocated regular-file blocks; SL7 includes native allocated blocks, packs, catalog/WAL/SHM, temporary files and uncollected history/orphans. Directory/inode/xattr and host physical extent overhead are excluded. These savings combine compression and deduplication, and do not isolate sharing across worktrees.

Current SL7 retains 5.3% more debug storage and 28.8% more release storage than original SL7 before GC. Background packing left median orphan-pack totals of 110 MiB and 162 MiB, respectively. These bytes are included above; offline GC can reclaim unreferenced packs, but post-GC storage was not measured in this comparison.

## Background work and remounts

Cargo times stop at Cargo exit. Post-build idle observation is separate and is not a durability barrier. Normal unmount drains the worker and must exit successfully. Only SL7 daemons are restarted; the corresponding native stage is another unchanged build. Daemon CPU counters include the subsequent idle observation and CLI verification, while Cargo wall and CPU times stop at Cargo exit.

| Profile / operation | Current Cargo | Additional idle observation | Cargo + observation |
| --- | ---: | ---: | ---: |
| Debug clean build | 298.37s | 3.96s | 302.33s |
| Debug one-line rebuild | 21.58s | 1.90s | 23.54s |
| Release clean build | 245.14s | 0.80s | 245.95s |
| Release one-line rebuild | 45.78s | 0.55s | 46.34s |

Current SL7 normal unmounts took 0.009–0.455s. All after-remount builds remained fresh.
Current SL7 debug after-remount builds: 1.24 (1.22–1.26) seconds.
Current SL7 release after-remount builds: 1.33 (1.32–1.33) seconds.

## Validation and method

All 12 cases, 96 Cargo commands, and eight SL7 integrity checks passed. The 64 corresponding native/SL7 compiler-artifact graphs matched in package, target, profile, features and freshness. The built CLI was executed after clean and edited builds to verify the expected description. Current SL7 reported no sealing errors.

Every debug case produced actual incremental-output files, while release cases did not. Incremental environment overrides were unset and workspace profile defaults were preserved.
The incremental inventory counts paths and logical lengths, not unique allocated bytes.

Compiler: `rustc 1.97.1 (8bab26f4f 2026-07-14)`; target x86-64 Linux. CPU quota: `800000 100000`; eight Cargo jobs; memory limit 12 GiB. Peak memory over the pod lifetime: 9.50 GiB. Memory events: `low 0; high 0; max 0; oom 0; oom_kill 0; oom_group_kill 0`.

The benchmark used the kaiba namespace, a single node, and a disk-backed emptyDir for both native and SL7 output. Round 1 ran native, original SL7, current SL7, debug then release; round 2 reversed both orders. Every case started with empty output directories and a fresh SL7 store. Source/output paths, compiler, source revision, lockfile, features and job count were identical. Downloads and setup were excluded from timing; OS page caches were not flushed. Per-case cgroup CPU/throttling, I/O pressure and memory counters are in the raw report.

The initial remote attempt failed because the inherited soft open-file limit was 1,024. The completed run restarted every case from empty outputs with a soft limit of 524,288 for all backends. Failed-attempt timings were excluded. This exposes a deployment requirement for large SL7 output trees; a low descriptor limit can cause build failures.

A preceding Mac run was interrupted when the user moved testing to Oaktown. Its completed first debug pair showed 194.74s native versus 821.90s current SL7 clean, and edit medians of 22.10s versus 136.79s. These are provisional observations from another machine, not combined with this data or proof of an implementation regression.

## Reproduction

Build release binaries with `--locked --features fuse` from commits `724d71b` and `dc1328a` in separate source/target directories. Use a clean archive of Synkro revision `ec6d4c53b8fdf4321de220a922f7efce67788d40`, Rust 1.97.1, pre-fetched dependencies, FUSE access, and an 8-CPU/12-GiB container. Then run:

```sh
ulimit -Sn 524288
TMPDIR=/bench/tmp SL7_BINARY=/bench/bin/sl7-current \
  python3 scripts/benchmark-synkro.py \
  --source /bench/source --results /bench/results \
  --revision ec6d4c53b8fdf4321de220a922f7efce67788d40 \
  --rounds 2 --jobs 8 --baseline-binary /bench/bin/sl7-baseline
```

Evidence: [all measurements](oaktown-build-performance.json), [summary JSON](oaktown-summary.json), [HTML table](oaktown-build-performance.html), [original OrbStack benchmark](synkro-build-performance.md).
