# SL7: integrated DashMap metadata indexes

## What changed

DashMap 6.2.1 now backs the live inode and native-identity indexes. Registered native LOOKUP and GETATTR requests, plus absent-name probes, run outside Mutex<State>. Owned descriptors pin native inodes across I/O; epoch validation rejects snapshots spanning representation changes. Reference acquisition and reclamation share a shard write lock. Multi-syscall mutations temporarily exclude only their affected inodes. Cold registration, sealed/logically overridden metadata, handles, mutations and publication retain their coordinated paths. Cache policy and SQLite are unchanged.

## Comparison

Baseline is pre-DashMap SL7 (85f3358; runtime source identical to 218a79d), candidate is b5ab793. This is a real DashMap integration, distinct from the earlier 16-shard LRU experiment. There is no native Cargo arm in this comparison. Both arms use eight Cargo jobs, two sealers and two hash workers; preparation reuse, import pipelining and parallel compression are disabled. Two rounds reverse arm order on the same Oaktown node. Each arm starts with empty output directories and an empty store. Incremental artifacts and matching Cargo artifact graphs are verified.

## Cargo build time

Seconds: median (observed range). Lower is better; negative changes are faster. Two clean/remount samples and six edited/unchanged samples per arm. Cargo time excludes executable checks and subsequent background settling. Settling includes a minimum 0.2-second observation delay.

| Stage | Pre-DashMap SL7 | DashMap SL7 | Elapsed change |
| --- | --- | --- | --- |
| clean | 213.901 (209.527–218.275) | 199.272 (197.855–200.688) | -6.8% |
| edit | 7.072 (6.277–9.928) | 7.324 (6.710–10.950) | +3.6% |
| unchanged | 0.496 (0.444–0.758) | 0.509 (0.433–0.551) | +2.7% |
| unchanged-after-remount | 0.850 (0.704–0.996) | 0.703 (0.654–0.751) | -17.3% |

## Build plus settling

Seconds: median (observed range). Lower is better; negative changes are faster. Two clean/remount samples and six edited/unchanged samples per arm. Cargo time excludes executable checks and subsequent background settling. Settling includes a minimum 0.2-second observation delay.

| Stage | Pre-DashMap SL7 | DashMap SL7 | Elapsed change |
| --- | --- | --- | --- |
| clean | 230.396 (226.921–233.871) | 211.830 (207.433–216.226) | -8.1% |
| edit | 8.745 (8.335–11.537) | 10.148 (8.689–13.651) | +16.0% |
| unchanged | 0.697 (0.649–0.959) | 0.712 (0.637–0.752) | +2.1% |
| unchanged-after-remount | 1.051 (0.905–1.197) | 0.904 (0.856–0.953) | -14.0% |

## Paired outcomes

DashMap was faster in 2/2 paired clean builds and 2/6 paired edits. The JSON preserves every sample. Two rounds on a shared node do not establish statistical significance. The earlier approximately 11× index microbenchmark result is not an expected whole-build speedup: this change removes coordination from a subset of filesystem requests, while compilation, reconstruction, hashing, publication and other coordinated operations still contribute to elapsed time.

## Requests served outside global state

Counts sum both mounts in each sequence, including executable checks, status observations and storage inventory. These confirm the concurrent path was exercised; they do not measure its isolated latency. Other lookup errors also bypass State but are not counted here.

| Round | Native LOOKUP hits | Absent-name probes | Native GETATTR hits | All metadata requests | Bypassed State |
| --- | --- | --- | --- | --- | --- |
| 1 | 20,900 | 147,329 | 37,113 | 232,863 | 88.2% |
| 2 | 21,099 | 147,329 | 36,998 | 231,865 | 88.6% |

## Accumulated time holding filesystem state

End-of-mount counters summed across each sequence. They measure time inside existing State sections, not elapsed build time or time waiting for DashMap shards. Fast metadata callbacks intentionally no longer contribute to request-state time. Counters alone do not establish critical-path attribution.

| Round | Arm | Request state seconds | Read state seconds |
| --- | --- | --- | --- |
| 1 | Baseline | 15.694 | 0.052 |
| 1 | DashMap | 13.633 | 0.061 |
| 2 | DashMap | 13.581 | 0.061 |
| 2 | Baseline | 16.401 | 0.054 |

## CPU time

Median CPU seconds. Compiler includes Cargo children. Daemon measurements include the build, executable check and settling interval; they are not exclusively CPU on the build critical path.

| Stage | Compiler baseline | Compiler DashMap | Daemon baseline | Daemon DashMap |
| --- | --- | --- | --- | --- |
| clean | 1246.070 | 1213.068 | 76.970 | 76.255 |
| edit | 8.007 | 7.997 | 6.775 | 6.850 |
| unchanged | 0.372 | 0.378 | 0.135 | 0.140 |
| unchanged-after-remount | 0.427 | 0.398 | 0.215 | 0.200 |

## Correctness

99 Linux and 51 macOS Rust tests passed (one existing ignored probe on each), strict Clippy passed, and all five mounted suites passed. New tests exercise progress with State held, mutation and publication invalidation, concurrent lookup/forget/reclaim, and failed-mutation cleanup. All 32 real Cargo commands, 16 paired artifact graphs, executable/freshness checks and four integrity scans passed. Chunk, mapping and file-descriptor budgets held. Both candidate mounts in each round exercised the concurrent metadata path.

## Environment and scope

Oaktown kaiba namespace, top node; 8 CPU and 12 GiB pod limits, Rust 1.97.1, Linux x86_64, ext4 backing storage. Dependencies were fetched beforehand; OS page caches remain warm. No other SL7 benchmark ran concurrently. Binary hashes, workload revision, compiler identity, aggregate samples and source hashes are in the JSON. Private workload source, paths and raw artifact graphs are excluded. This is a focused inode-index integration; it does not evaluate a DashMap chunk cache, SCC, evmap or Turso inside the full filesystem.

[HTML tables](dashmap-indexes.html) · [Measurements and provenance (JSON)](dashmap-indexes.json)
