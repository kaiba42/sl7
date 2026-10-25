# SL7: concurrent primitives and Turso

## Findings

DashMap, SCC, Moka, a bounded DashMap/FIFO cache, a bounded sharded LRU, and embedded Turso were executed, not assessed only from published benchmarks. Concurrent primitives substantially improved isolated throughput. Engine and cache choices have workload-specific tradeoffs; the integrated cache comparison below is the gate for claiming a Cargo improvement. Production defaults remain unchanged.

## Index: uniform

Million logical operations/second; higher is better. Same RandomState hasher family, inode-like integer keys and owned Arc values. The mixed case adds a replacement every tenth lookup. This isolates index access, not the complete inode-lease/publication protocol.

| Threads | mutex | rwlock | dashmap | scc |
| --- | --- | --- | --- | --- |
| 1 | 39.43 | 22.07 | 26.63 | 39.51 |
| 2 | 11.14 | 24.88 | 51.29 | 48.43 |
| 4 | 7.31 | 37.51 | 80.48 | 68.12 |
| 8 | 6.00 | 42.54 | 112.28 | 127.43 |

## Index: hot

Million logical operations/second; higher is better. Same RandomState hasher family, inode-like integer keys and owned Arc values. The mixed case adds a replacement every tenth lookup. This isolates index access, not the complete inode-lease/publication protocol.

| Threads | mutex | rwlock | dashmap | scc |
| --- | --- | --- | --- | --- |
| 1 | 42.95 | 52.72 | 57.94 | 44.99 |
| 2 | 12.14 | 27.12 | 58.16 | 41.28 |
| 4 | 8.14 | 34.15 | 71.32 | 83.42 |
| 8 | 6.66 | 29.34 | 113.83 | 118.33 |

## Index: mixed

Million logical operations/second; higher is better. Same RandomState hasher family, inode-like integer keys and owned Arc values. The mixed case adds a replacement every tenth lookup. This isolates index access, not the complete inode-lease/publication protocol.

| Threads | mutex | rwlock | dashmap | scc |
| --- | --- | --- | --- | --- |
| 1 | 37.77 | 35.21 | 38.77 | 23.42 |
| 2 | 9.37 | 18.25 | 43.15 | 35.79 |
| 4 | 7.02 | 14.90 | 72.69 | 68.27 |
| 8 | 5.43 | 10.24 | 91.06 | 107.27 |

## Chunk cache: uniform

Million accesses/second. Distinct 8 KiB Arc payloads, 32-byte keys and a 128 MiB accounting budget. FIFO changes eviction policy, sharded LRU divides capacity, and Moka enforces capacity eventually. All final budgets and payload checks passed; this does not establish an instantaneous Moka bound. Allocation, I/O and decoding are outside the timed region.

| Threads | lru | sharded-lru | dashmap-fifo | moka |
| --- | --- | --- | --- | --- |
| 1 | 4.04 | 3.96 | 13.67 | 4.16 |
| 2 | 1.83 | 6.31 | 23.27 | 5.95 |
| 4 | 2.34 | 10.58 | 44.76 | 8.02 |
| 8 | 2.31 | 13.34 | 84.37 | 10.21 |

## Chunk cache: hot

Million accesses/second. Distinct 8 KiB Arc payloads, 32-byte keys and a 128 MiB accounting budget. FIFO changes eviction policy, sharded LRU divides capacity, and Moka enforces capacity eventually. All final budgets and payload checks passed; this does not establish an instantaneous Moka bound. Allocation, I/O and decoding are outside the timed region.

| Threads | lru | sharded-lru | dashmap-fifo | moka |
| --- | --- | --- | --- | --- |
| 1 | 6.54 | 6.43 | 18.73 | 6.13 |
| 2 | 3.05 | 8.87 | 31.02 | 8.26 |
| 4 | 3.46 | 14.99 | 57.77 | 8.67 |
| 8 | 3.23 | 15.17 | 93.84 | 9.50 |

## Chunk cache: churn

Million accesses/second. Distinct 8 KiB Arc payloads, 32-byte keys and a 128 MiB accounting budget. FIFO changes eviction policy, sharded LRU divides capacity, and Moka enforces capacity eventually. All final budgets and payload checks passed; this does not establish an instantaneous Moka bound. Allocation, I/O and decoding are outside the timed region.

| Threads | lru | sharded-lru | dashmap-fifo | moka |
| --- | --- | --- | --- | --- |
| 1 | 2.15 | 1.69 | 5.78 | 1.06 |
| 2 | 1.46 | 3.35 | 7.15 | 1.66 |
| 4 | 1.34 | 8.01 | 10.41 | 2.21 |
| 8 | 1.25 | 8.47 | 17.85 | 2.28 |

## Catalog: exists

Chunk-existence queries/second; repeated uses 64 present hashes, while exists mixes present and absent hashes. Same schema, seed, ext4 volume, FULL synchronous mode and foreign keys. Whole-transaction conflict retries are included. No pack I/O or FUSE work is included.

| Connections | sqlite | turso-wal | turso-mvcc |
| --- | --- | --- | --- |
| 1 | 292,601 | 702,930 | 369,924 |
| 2 | 558,536 | 837,814 | 420,806 |
| 4 | 1,037,993 | 2,350,038 | 658,300 |

## Catalog: repeated

Chunk-existence queries/second; repeated uses 64 present hashes, while exists mixes present and absent hashes. Same schema, seed, ext4 volume, FULL synchronous mode and foreign keys. Whole-transaction conflict retries are included. No pack I/O or FUSE work is included.

| Connections | sqlite | turso-wal | turso-mvcc |
| --- | --- | --- | --- |
| 1 | 390,440 | 732,867 | 306,724 |
| 2 | 621,935 | 1,009,891 | 529,868 |
| 4 | 953,713 | 2,388,963 | 684,848 |

## Catalog: range

Queries/second; scans 32 extents, consuming rows and the first integer column, without constructing full SL7 Location objects. Same schema, seed, ext4 volume, FULL synchronous mode and foreign keys. Whole-transaction conflict retries are included. No pack I/O or FUSE work is included.

| Connections | sqlite | turso-wal | turso-mvcc |
| --- | --- | --- | --- |
| 1 | 54,711 | 25,098 | 10,632 |
| 2 | 60,220 | 50,490 | 17,812 |
| 4 | 61,083 | 93,101 | 29,325 |

## Catalog: publish-disjoint

Transactions/second, 64 chunk references per transaction. Same schema, seed, ext4 volume, FULL synchronous mode and foreign keys. Whole-transaction conflict retries are included. No pack I/O or FUSE work is included.

| Connections | sqlite | turso-wal | turso-mvcc |
| --- | --- | --- | --- |
| 1 | 138 | 127 | 125 |
| 2 | 141 | 136 | 207 |
| 4 | 127 | 146 | 405 |

## Catalog: publish-shared

Transactions/second, 64 chunk references per transaction. Same schema, seed, ext4 volume, FULL synchronous mode and foreign keys. Whole-transaction conflict retries are included. No pack I/O or FUSE work is included.

| Connections | sqlite | turso-wal | turso-mvcc |
| --- | --- | --- | --- |
| 1 | 160 | 133 | 138 |
| 2 | 162 | 160 | 206 |
| 4 | 156 | 164 | 397 |

## Integrated Synkro build

Seconds, median (observed range); lower is better. Two reversed-order rounds; both binaries use two hash workers and two sealers. Incremental artifacts were verified. No native Cargo arm was rerun here. Two rounds do not establish statistical significance.

| Stage | Current SL7 | 16-shard cache |
| --- | --- | --- |
| clean | 193.917 (176.983–210.852) | 220.064 (217.658–222.469) |
| edit | 7.351 (6.618–8.493) | 7.840 (6.484–10.841) |
| unchanged | 0.500 (0.430–0.888) | 0.519 (0.430–0.820) |
| unchanged-after-remount | 0.674 (0.623–0.724) | 1.026 (0.733–1.319) |

## Build plus settling

Seconds, median (observed range); lower is better. Two reversed-order rounds; both binaries use two hash workers and two sealers. Incremental artifacts were verified. No native Cargo arm was rerun here. Two rounds do not establish statistical significance.

| Stage | Current SL7 | 16-shard cache |
| --- | --- | --- |
| clean | 203.469 (187.441–219.498) | 229.569 (226.990–232.148) |
| edit | 9.637 (8.226–10.588) | 10.955 (8.537–12.752) |
| unchanged | 0.702 (0.631–1.089) | 0.720 (0.631–1.022) |
| unchanged-after-remount | 0.875 (0.824–0.925) | 1.227 (0.934–1.521) |

## Integrated outcome

Relative to pre-sharding SL7, the sharded cache changed clean-build median time by +13.5% and edit median time by +6.6% (positive means slower). Native Cargo was not an arm of this experiment. 3/6 paired edits were faster. This is an experimental patch, not an adopted production default. The microbenchmark gain alone does not justify replacing the cache.

## Compiler CPU time

CPU-seconds summed across the Cargo process tree. These and the daemon/cache counters can help expose variation, but are not critical-path profiles.

| Stage | Current SL7 | 16-shard cache |
| --- | --- | --- |
| clean | 1173.59 | 1217.38 |
| edit | 8.17 | 8.17 |
| unchanged | 0.38 | 0.38 |
| unchanged-after-remount | 0.40 | 0.41 |

## Cache behavior across each build/edit sequence

Before-remount counters. Load durations can overlap; they are not additive build latency. Different scheduling can change how much data reaches sealed storage and needs reconstruction.

| Round | Backend | Chunk hits | Chunk misses | Pack opens | Map misses | Load service seconds |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Current SL7 | 20,635 | 87,570 | 10,458 | 13,544 | 4.817 |
| 1 | 16-shard cache | 9,400 | 45,091 | 21,033 | 10,052 | 7.152 |
| 2 | 16-shard cache | 7,885 | 41,890 | 19,933 | 8,571 | 2.756 |
| 2 | Current SL7 | 7,217 | 40,071 | 5,524 | 8,578 | 3.812 |

## What the integrated cache test exposed

The prototype divided the pack-handle budget as well as chunk/map capacity. Each shard could retain only four pack handles, and the same pack could occupy several shards. In the first sequence, pack opens increased from 10,458 to 21,033 despite fewer chunk misses. A next cache design should retain a shared pack-handle pool rather than repeat that partitioning. This is a concrete cache-locality cost, not a complete causal explanation of build-time variation: compiler CPU consumption and baseline wall times also varied between rounds.

## Implementation choices

DashMap is a strong next candidate for the four-worker inode indexes; SCC is competitive and scales well at eight threads. Moving index lookups outside Mutex<State> requires per-inode coordination and consistent updates across the inode, identity and handle indexes. A map substitution inside the same global mutex would retain that serialization. DashMap/FIFO is the fastest cache hit path tested, but variable-size chunk budgets, eviction quality and miss loading need an integrated implementation. Moka provides a fuller cache abstraction, with eventual capacity enforcement. Turso MVCC improves concurrent publications in this probe, while range-query costs remain a concern; retaining SL7’s application-wide writer lock would prevent exploiting concurrent commits. WAL and MVCC results are distinct engine modes, not interchangeable benefits.

## Validation and scope

423 primitive samples passed equal-work/checksum/count validation. The integrated cache candidate passed 94 existing Linux tests plus a new cache-budget test, and three mounted suites covering reconstruction, mmap/linking, staging and cache coherence. All 32 Cargo commands, 16 paired artifact graphs, executable/freshness checks and four integrity scans passed. Cache byte, map-entry and descriptor budgets held. The database probe checked CHECK/FK rejection, rollback and final row counts, not power-loss recovery or a full SL7/Turso integration. DashMap/SCC/Moka/Turso remain experimental harness dependencies, outside production SL7.

## Reproduction and limitations

AMD Ryzen 9 7940HS, Linux 6.12.58, Rust 1.97.1, Oaktown top node; pod limited to 8 CPUs and 12 GiB. Three primitive rounds rotate candidate order; JSON includes all samples and ranges. System allocator throughout: Turso’s default global mimalloc feature was disabled to avoid changing every other candidate. SQLite 3.46.0 (rusqlite 0.32.1), Turso 0.8.3, DashMap 6.2.1, SCC 3.8.8, Moka 0.12.16. Shared node and warm OS caches; descriptive measurements, no significance claim. Production source baseline df96636, with the exact candidate patch and harness hashes in JSON. See experiments/primitives/README.md for commands. Private workload source and raw artifact graphs are excluded.

[HTML tables](primitives-comparison.html) · [All measurements (JSON)](primitives-comparison.json) · [Harness and reproduction](../../experiments/primitives/README.md)
