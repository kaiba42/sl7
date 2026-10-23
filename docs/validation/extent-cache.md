# Cached extent mappings

Repeated range reads now reuse the immutable file-to-chunk mapping, avoiding a
fresh extent/chunk SQL join for each request. Sorted 256 KiB logical windows use
binary search to find the requested chunks. A separate 32 MiB accounting budget
and 4,096-entry limit bound retained mapping metadata; cold reads never load an
entire large file's map. Existing decoded-chunk and pack-descriptor limits remain
128 MiB and 64 descriptors.

The key includes file ID, size, whole-file digest, and window offset. This prevents
stale mappings when SQLite reuses a deleted file's ID. Integrity checks clear and
bypass mappings; GC clears them before moving pack locations. A capped loader
falls back to streaming SQL for unusually dense catalogs. Prepared statements
are also reused for file lookup, live metadata, extent queries, and repeated
chunk-existence checks. No storage-format migration is required.

## Focused measurements

Compared with `a6b41601b31610f8ed485791f57db6e63815f90b` on Linux ARM64 in
OrbStack, Rust 1.97.1, an eight-CPU quota and 12 GiB memory limit. The same
32 MiB synthetic artifact and byte-for-byte read assertions were used in both
implementations. Separate Cargo target directories, distinct executable hashes,
identical harness hashes, and matching operation counts were verified.

Runs were serial in before/after/after/before order, with seven samples per group
per run. Values below pool the fourteen samples for each implementation. This is
a dependency-free microbenchmark, not Criterion, and no statistical significance
is claimed. The complete samples and environment are in [extent-cache.json](extent-cache.json).

| Read workload | Before, median µs/read | After, median µs/read | Observed ratio |
| --- | ---: | ---: | ---: |
| Warm sequential, 4 KiB | 11.07 | 2.45 | 4.52× faster |
| Warm random, 4 KiB | 10.80 | 3.49 | 3.10× faster |
| Warm window-crossing, 8 KiB | 10.98 | 3.13 | 3.51× faster |
| Cold sparse, 4 KiB | 31.15 | 28.97 | Inconclusive |
| Cold sequential, 1 MiB | 734.31 | 703.57 | Inconclusive |

Warm-read gains repeated in both passes. Cold baseline pass medians varied widely:
24.44–54.96 µs for sparse reads and 651.62–912.48 µs for sequential reads. The
optimized cold medians fell within those ranges, so the small pooled differences
are not evidence of a reliable cold-read improvement. Prefetching mapping rows can
also add work for isolated small reads.

“Cold” means SL7 caches are empty, with a new Store each round; OS caches were not
flushed. Warm groups prime the whole artifact before timing. Each warm sample
performs 65,536 reads. Cold samples aggregate 32 rounds of 128 sparse reads or eight
rounds of 32 sequential reads. Opening the store and preparing the fixture are
outside the measured intervals.

Each warmed optimized Store executed **128 mapping queries during priming and no
additional mapping queries for its 65,536 timed reads**. The retained maps occupied
457,472 accounted bytes and 128 entries for this fixture. File metadata lookup
still executes SQL; this does not claim the entire read path is database-free.

These results measure the combined mapping cache and prepared-statement changes
through `Store::read`. They do not include FUSE dispatch or compiler execution,
and are not a new whole-Cargo-build speedup. The previously measured build results
remain in [the earlier optimization report](optimizations.md).

## Validation and reproduction

Passed 34 host Rust tests, 45 Linux Rust tests, strict Clippy, and all four actual
mounted suites (read-only, staging, live, and self-host). Regressions cover chunk
and window boundaries, EOF, SQLite row-ID reuse, warmed reads after GC relocation,
uncached integrity checking, mapping budget/entry eviction, and dense-catalog
fallback including a complete integrity check.

Run the focused benchmark with:

```sh
CARGO_TARGET_DIR=target/range-bench cargo bench --locked --bench range_reads
```

For comparisons, copy the identical benchmark harness and its `[[bench]]` manifest
entry into the baseline checkout, and give each checkout a **different** target
directory. Enable `SL7_CACHE_STATS=1` to collect mapping counters. Run the normal
host and mounted suites with `bash scripts/verify.sh` and
`bash scripts/docker-test.sh`.
