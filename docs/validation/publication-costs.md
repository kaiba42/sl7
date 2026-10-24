# Shorter catalog lock scope and atomic publication

Clean-build medians: 149.30s native, 168.40s previous SL7, 167.21s after narrowing the writer lock, and 166.74s with atomic publication. Edited-build medians: 4.23s native, 8.06s previous SL7, 8.24s after narrowing the lock, and 7.63s with both changes.

Median sums of recorded Cargo, idle-observation, and draining-unmount time: Native Cargo 163.91s, Previous SL7 228.72s, Retirement outside writer lock 226.97s, Plus atomic publication 216.58s. These include harness waits and expose deferred background work separately from Cargo latency.

The first change releases the catalog writer permit after durable manifest/binding publication, before native truncation and fsync. The affected inode stays gated until retirement completes; unrelated catalog operations can proceed. The second change inserts the manifest and live binding in one FULL-synchronous transaction instead of committing each separately. Failure before commit rolls both back and leaves native bytes authoritative. The worker count, compression, chunking, bounded caches, and explicit timestamp-fsync policy are unchanged.

## Matched debug/incremental builds

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds: two clean, six edited, six unchanged, and two remounted samples per backend. Same archived workspace, Rust 1.97.1, 8 CPU / 12 GiB, disk-backed Oaktown pod, and matched source/output paths. Debug incremental output is verified present. OS caches are warm; shared-node activity and small sample counts limit precision. Release was not rerun. CPU-idle observation does not prove the background queue is drained. Timings exclude that observation; JSON also reports Cargo-plus-observation timings. No statistical significance is claimed.

| Operation | Native Cargo | Previous SL7 | Retirement outside writer lock | Plus atomic publication |
| --- | ---: | ---: | ---: | ---: |
| Clean build | 149.30 (149.14–149.45) | 168.40 (168.35–168.45) | 167.21 (167.01–167.42) | 166.74 (165.76–167.72) |
| One-line rebuild | 4.23 (3.78–5.05) | 8.06 (6.57–10.20) | 8.24 (7.36–9.46) | 7.63 (5.62–9.78) |
| Unchanged build | 0.40 (0.28–0.65) | 0.43 (0.40–6.52) | 0.46 (0.36–3.60) | 0.44 (0.38–4.74) |
| Unchanged after remount | 0.28 (0.28–0.28) | 0.66 (0.63–0.68) | 0.56 (0.55–0.57) | 0.61 (0.60–0.61) |

The latest version was faster in 2 of 6 matched edits versus Previous SL7. The latest version was faster in 5 of 6 matched edits versus Retirement outside writer lock.

## Publication and lock costs

| Round / backend | Candidate setup (s) | Packing (s) | Catalog publication (s) | Native retirement (s) | Catalog waits (s) | State holds (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 / Previous SL7 | 64.10 | 75.80 | unmeasured | unmeasured | 3.72 | 18.56 |
| 2 / Previous SL7 | 66.48 | 75.17 | unmeasured | unmeasured | 9.26 | 17.11 |
| 1 / Retirement outside writer lock | 68.54 | 77.47 | 44.20 | 19.41 | 6.31 | 17.32 |
| 2 / Retirement outside writer lock | 59.32 | 76.19 | 47.66 | 19.62 | 3.56 | 17.53 |
| 1 / Plus atomic publication | 65.26 | 73.23 | 27.81 | 20.10 | 2.42 | 18.32 |
| 2 / Plus atomic publication | 65.74 | 75.55 | 29.86 | 21.50 | 4.66 | 16.63 |

Counters cover each complete clean/unchanged/edited sequence, may overlap across threads, and cannot be subtracted from Cargo wall time. Earlier code did not split catalog publication from native retirement; missing counters are unmeasured, not zero. Candidate setup still performs durable identity/generation work under the writer permit.

## Recorded sequence costs

| Round / backend | Cargo commands (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 163.04 | 0.00 | 0.00 | 163.04 |
| 2 / Native Cargo | 164.78 | 0.00 | 0.00 | 164.78 |
| 1 / Previous SL7 | 198.62 | 21.35 | 8.00 | 227.98 |
| 2 / Previous SL7 | 199.01 | 29.87 | 0.57 | 229.45 |
| 1 / Retirement outside writer lock | 196.91 | 29.98 | 4.26 | 231.14 |
| 2 / Retirement outside writer lock | 196.19 | 26.06 | 0.56 | 222.81 |
| 1 / Plus atomic publication | 192.52 | 16.84 | 0.57 | 209.93 |
| 2 / Plus atomic publication | 196.59 | 26.06 | 0.59 | 223.24 |

The recorded sum includes harness idle observations and draining unmounts, which native Cargo does not require. It excludes initialization, remount startup, help-output checks, and integrity scans; it is not total experiment wall time.

## Storage and validation

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| Previous SL7 | 1.69 (1.62–1.75) |
| Retirement outside writer lock | 1.80 (1.76–1.84) |
| Plus atomic publication | 1.77 (1.75–1.78) |

Storage includes native allocation plus catalog/packs before GC, excluding directory/inode/xattr overhead.

Both changes passed 60 Linux tests, 35 host tests, strict Clippy, and all five mounted suites. A paused-retirement test observes same-inode writes and hardlinks waiting while another sealed inode hydrates, writes, and fsyncs. Binding-failure injection and process death after uncommitted manifest insertion leave no catalog manifest/chunks and preserve native bytes. Post-commit recovery verifies reconstructed bytes. These process-crash tests do not simulate power loss. All 64 Cargo commands, 48 native/SL7 artifact-graph comparisons, freshness/executable checks, and six store integrity checks passed. Source and binary hashes tie each tested implementation to its measured executable.

Implementation commits: Previous SL7 `b91f464dce033e5123928f8720f3f58cd3d79080`, Retirement outside writer lock `da7c1691fa159dd4b3022aa55c95c9bc63bd45c9`, Plus atomic publication `a3af884513d571fba67433b2443712a7828cbaf7`.

[HTML tables](publication-costs.html) · [JSON measurements](publication-costs.json) · [Re-profile](publication-costs-profile.md) · [Durability protocol](../live-format.md)
