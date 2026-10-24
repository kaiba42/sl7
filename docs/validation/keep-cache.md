# Preserving kernel pages across artifact opens

SL7 `14f72b4` returns `FOPEN_KEEP_CACHE` for files in its private live mount. Reopening an unchanged artifact now retains kernel-cached pages. The previous implementation (`099d807`, runtime `5f300bb`) discarded them on each open, causing repeated reconstruction. Native staging keeps its existing conservative behavior.

Clean build median fell by **6.9%** (211.22s → 196.69s); native was 150.24s. One-line rebuild median fell by **36.0%** (22.32s → 14.29s); native was 4.52s.

## Matched debug/incremental builds

Median seconds (minimum–maximum). Two clean samples and six edited/unchanged samples per backend, in two reversed-order rounds. All samples are retained, including slow unchanged-build samples. These are small samples on a shared node; ranges matter. Release was not rerun in this experiment.

| Operation | Native | Before | Retained kernel pages | Reduction vs before | Over native |
| --- | ---: | ---: | ---: | ---: | ---: |
| Clean build | 150.24 (149.55–150.94) | 211.22 (210.50–211.95) | 196.69 (192.12–201.26) | +6.9% | +30.9% |
| One-line rebuild | 4.52 (3.96–6.57) | 22.32 (18.50–26.01) | 14.29 (13.98–17.65) | +36.0% | +216.0% |
| Unchanged build | 0.49 (0.38–4.05) | 1.26 (1.22–3.02) | 1.22 (1.20–4.67) | +3.3% | +147.4% |
| Unchanged after remount | 0.28 (0.28–0.28) | 1.32 (1.32–1.33) | 1.31 (1.29–1.33) | +1.0% | +361.1% |

Cargo wall time ends at Cargo exit. The following observation waits for apparent background idleness and is not a durability barrier. Daemon CPU is measured through that observation and executable verification. Each column is summarized independently; Cargo-plus-observation sums are computed per sample before taking their median.

| Operation | Native Cargo | Before Cargo + observation | After Cargo + observation | Before daemon CPU | After daemon CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| Clean build | 150.24 (149.55–150.94) | 218.29 (214.81–221.77) | 208.22 (194.93–221.51) | 125.57 (124.95–126.19) | 103.53 (100.70–106.35) |
| One-line rebuild | 4.52 (3.96–6.57) | 23.37 (20.31–27.21) | 16.15 (15.79–17.95) | 16.55 (16.48–16.76) | 8.27 (7.94–8.78) |

## What work disappeared

Counters cover each complete sequence before remount: clean, three unchanged builds, and three edits. They are cumulative and cannot be subtracted from build wall time. Requested/decoded bytes include reads through SL7's shared payload cache, including foreground hydration.

| Round / backend | Read callbacks | Requested GiB | Decoded GiB | Foreground state holds (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Before | 435824 | 20.36 | 16.10 | 76.58 |
| 2 / Before | 436564 | 20.36 | 16.17 | 77.98 |
| 1 / Retained kernel pages | 153135 | 0.43 | 0.68 | 70.81 |
| 2 / Retained kernel pages | 152873 | 0.45 | 0.66 | 72.54 |

The kernel's normal reclaimable page cache is separate from SL7's userspace cache. The mount still shares the same 128 MiB decoded-payload budget, 32 MiB / 4096 extent-window budget, and 64 cached pack handles across four FUSE request workers. Compression, chunking, synchronization, and recovery ordering are unchanged. Native uses the kernel page cache as well.

## Storage and correctness

| Backend | Retained GiB before GC |
| --- | ---: |
| Native | 6.83 (6.83–6.83) |
| Before | 1.79 (1.79–1.80) |
| Retained kernel pages | 1.79 (1.79–1.79) |

Storage is after three edits, before GC. SL7 totals include native allocation, catalog, referenced and orphan packs. Directory/inode/xattr overhead and host extent sharing are excluded. These savings combine compression and deduplication.

The new mounted regression test starts from cold mounts. One 1 MiB read and eight separate opens each caused **8 read callbacks / 1 MiB reconstructed** with the change. The previous binary reconstructed **8 MiB in 64 callbacks** across eight opens and failed the retention assertion as expected. Tests also cover same-size writes with restored timestamps, mmap writes, hard links, rename replacement with a live mapping, truncation/regrowth, create-existing, truncate-on-open, sealing/hydration, and remounts.

All 53 Linux tests, 35 host tests, strict Clippy, and five mounted suites passed. All 48 benchmark Cargo commands and four store integrity checks passed. All 32 native-versus-SL7 artifact-graph pairs matched in package, target, profile, features, and freshness. Every case contained incremental outputs, and unchanged/remounted builds stayed fresh. All 104 copied benchmark/gate files matched remote SHA-256 digests. No sealing errors were reported. Binary and changed-source hashes connect the measured executable to the committed implementation.

## Method and reproduction

Oaktown namespace `kaiba`, node `top`, Ryzen 9 7940HS, eight-CPU quota, 12 GiB RAM, disk-backed workspace, Rust 1.97.1, Linux 6.12.58, descriptor limit 524288. The three backends use identical source, lockfile, compiler, paths, jobs, and workspace profiles. Dependencies were prefetched and outputs/stores began empty; OS caches were not dropped. No other SL7 builds or profiles ran on this node during measurement.

```sh
ulimit -Sn 524288
SL7_BINARY=/bench/bin/sl7-current python3 scripts/benchmark-synkro.py \
  --source /bench/source --results /bench/results/keep-cache-builds \
  --revision ec6d4c53b8fdf4321de220a922f7efce67788d40 --rounds 2 --jobs 8 \
  --profiles debug --backends native baseline-sl7 sl7 \
  --baseline-binary /bench/bin/sl7-baseline
```

The [kernel interface](https://github.com/libfuse/libfuse/blob/master/include/fuse_kernel.h) defines the cache-preserving open flag. [JSON measurements](keep-cache.json) retain samples, resources, hashes, and counters; [HTML tables](keep-cache.html) provide a compact view. [Validation details](keep-cache-validation.json) record the mounted retention gate. This change is one step toward native performance; remaining overhead requires further measurement and optimization.
