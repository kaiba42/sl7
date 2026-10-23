# Four optimizations: measured results

Follow-up: [cached extent mappings and prepared statements](extent-cache.md).

All four changes were committed and pushed separately:

1. [`cc1c5ef`](https://github.com/kaiba42/sl7/commit/cc1c5ef): ARM64 SHA-256 acceleration.
2. [`332aa27`](https://github.com/kaiba42/sl7/commit/332aa27): metadata-only updates and zero truncation avoid hydration.
3. [`6c5cd22`](https://github.com/kaiba42/sl7/commit/6c5cd22): 128 MiB verified-chunk cache and 64 cached pack descriptors.
4. [`3a44bf0`](https://github.com/kaiba42/sl7/commit/3a44bf0): background packing with mutation/open-handle validation and shutdown draining.

## Matched measurements

Measured on 2026-10-23. OrbStack had updated its kernel since the original profile, so the exact original SL7 binary was rerun for this comparison. Both runs used Linux 7.2.12, Rust 1.97.1, the same Synkro source revision and lockfile, an eight-CPU quota, 12 GiB of memory, and initially empty output directories. Dependencies were fetched beforehand. The original host Synkro checkout was not modified.

| Workload | Before | After | Wall-time reduction | After-build idle observation |
| --- | ---: | ---: | ---: | ---: |
| Clean debug build | 431.18 s | 394.93 s | 8.4% | 4.44 s |
| Unchanged build | 0.86 s | 1.36 s | -57.3% | 0.31 s |
| One-line edit, untraced | 70.21 s | 57.46 s | 18.2% | 3.40 s |
| One-line edit 2, traced | 79.02 s | 43.29 s | 45.2% | 1.76 s |
| One-line edit 3, traced | 97.72 s | 34.31 s | 64.9% | 1.76 s |
| Final unchanged build | 1.26 s | 0.63 s | 49.8% | 0.31 s |

Negative reductions are regressions. Unchanged-build results are mixed. Each implementation ran one diagnostic sequence; these are observed combined effects, not statistically established gains or isolated attribution to each change. The optimized sequence ran first; OS caches were not flushed. Native filesystem performance was not remeasured.

Cargo wall time excludes the separately reported idle observation (about 0.31 seconds per stage before). Including that observation, reductions were:

- Clean debug build: 431.49 → 399.37 seconds (7.4%).
- One-line edit, untraced: 70.52 → 60.86 seconds (13.7%).
- One-line edit 2, traced: 79.32 → 45.05 seconds (43.2%).
- One-line edit 3, traced: 98.03 → 36.07 seconds (63.2%).

Normal unmount drained the optimized worker in **0.097 seconds**. All six Cargo builds in each run passed, edited CLI output was verified, unchanged builds stayed fresh, and the final uncached integrity check passed. Daemon CPU includes the observed post-build work. It fell from 256.54 to 224.24 CPU-seconds for the clean build and by 28.3%, 50.2%, and 70.3% for the three rebuilds. Cargo CPU in the optimized clean run was higher: 1,501.98 versus 1,027.31 CPU-seconds. These observations do not establish a uniform reduction in total CPU use. The JSON records both components.

## What the traces establish

The following sums combine the two traced rebuilds. They are overlapping request-seconds, not elapsed build time:

| FUSE response time | Before | After |
| --- | ---: | ---: |
| Read | 62.092 s | 27.824 s |
| Release / close | 30.898 s | 0.328 s |
| Set attributes | 15.767 s | 0.001 s |
| All responses | 119.917 s | 40.954 s |
| All dispatch waits | 164.888 s | 34.918 s |

The largest release response fell from **8.281 to 0.279 seconds**. Optimized attribute updates stayed below 0.2 milliseconds. Hardware SHA-256 is present in the sampled stacks; its named leaf accounts for 21–22% of optimized rebuild userspace samples. Hydration accounts for only 1–2%. Roughly 12–13% of rebuild leaf symbols remain unresolved, and kernel stacks were not resolved; userspace sample percentages are not whole-build percentages. All CPU/FUSE reports recorded zero lost samples.

## Costs and remaining priorities

Retained storage **before GC** rose from 1.760 to 1.904 GiB. The optimized store includes **154.6 MiB of orphan packs**; the worker canceled 947 candidates and completed 4234 with zero errors. Canceled work leaves native bytes authoritative and may leave packs for offline GC. Storage is native allocated blocks plus content-store file bytes, excluding directory/inode/xattr and host filesystem overhead.

The chunk-cache hit rate was **39.8%** across the whole sequence. It reached its 128 MiB limit, retained at most 64 descriptors, and opened packs 63,254 times. These counters include reads for CLI output verification.

Next work should focus on:

1. **Reconstruction reads.** They account for 67–71% of optimized rebuild userspace samples and 65–71% of aggregate FUSE response time. Measure cache-size/readahead choices and catalog/extent lookup separately; decompression and checksum verification remain substantial.
2. **Catalog access and publication.** Publication plus sparse retirement still held the mutex for 51.44 seconds across the sequence, at most 0.832 seconds at once. Evaluate prepared-statement reuse and shorter publication transactions. `pread64` calls increased despite fewer opens and no seeks; attribute those calls and capture client/kernel CPU before assuming descriptor caching eliminated I/O overhead.
3. **Canceled work.** Avoid or promptly collect abandoned packs while retaining the current generation checks and recovery ordering.

## Validation and reproduction

Every change passed host checks and the full Linux mounted suite before its separate commit. The final version passed 30 host Rust tests, 41 Linux Rust tests, strict Clippy, read-only/staging/live mounts, and self-host build/test/check/edit/clean/rebuild checks. New regressions cover zero-truncate recovery, cache integrity bypass, concurrent writes invalidating prepared content, open readers preventing retirement, and shutdown draining.

Run `bash scripts/verify.sh` and `bash scripts/docker-test.sh`. For profiling, use `bash scripts/profile-synkro.sh /path/to/synkro-rs test-results/profile-new`; the wrapper copies and builds the current SL7 sources instead of trusting the image’s old binary. The Python runner accepts `SL7_PROFILE_BINARY` and `SL7_PROFILE_RESULTS` to compare a saved original binary in the same container. Raw traces and source archives remain ignored.

Full evidence: [optimized profile](optimized-profiling.json), [fresh original-binary profile](current-baseline-profiling.json), [per-change validation](optimizations-validation.json). The [original profile](profiling.md) remains historical evidence.
