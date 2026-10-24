# Catalog barriers at filesystem sync boundaries

One-line rebuilds fell from 13.70s to 8.36s median (38.9% lower), versus 4.42s native. Clean builds took 169.29s versus 178.83s before and 149.28s native.

The median sum of recorded Cargo, idle-observation, and draining-unmount time fell from 244.39s to 234.18s (4.2% lower). More work finished after Cargo returned, so the larger edited-build gain is a latency improvement. Unchanged-build median rose from 0.40s to 0.45s with overlapping ranges; no improvement is established there.

Reads, native writes/syncs, and namespace catalog commits already run outside the global filesystem lock. This change removes the disk barrier from each sealed-file link/unlink/rename timestamp commit. A restricted WAL/NORMAL connection commits those timestamps immediately; file or directory fsync writes a nonempty FULL transaction to flush earlier catalog changes before syncing the native descriptor. Content manifests, packs, hydration, and retirement retain their existing FULL durability ordering. Unsynced namespace timestamps may roll back after power loss. The worker count, compression, chunking, and bounded caches are unchanged.

## Matched debug/incremental builds

Median seconds (minimum–maximum), all samples included. Two reversed-order rounds: two clean, six edited, six unchanged, and two remounted samples per backend. Same archived workspace, Rust 1.97.1, 8 CPU / 12 GiB, disk-backed Oaktown pod, and matched source/output paths. Debug incremental output is verified present. OS caches are warm; shared-node activity and small sample counts limit precision. Release was not rerun. The post-build CPU-idle observation is not a proof that background sealing has drained. Timings exclude that observation; JSON also reports Cargo-plus-observation timings.

| Operation | Native Cargo | Parallel SL7, eager sync | Parallel SL7, explicit sync |
| --- | ---: | ---: | ---: |
| Clean build | 149.28 (149.16–149.41) | 178.83 (178.73–178.94) | 169.29 (168.59–169.98) |
| One-line rebuild | 4.42 (3.91–4.70) | 13.70 (11.38–22.52) | 8.36 (5.65–12.75) |
| Unchanged build | 0.32 (0.28–4.75) | 0.40 (0.38–2.61) | 0.45 (0.35–2.70) |
| Unchanged after remount | 0.28 (0.28–0.28) | 0.57 (0.53–0.61) | 0.58 (0.54–0.61) |

The new version was faster in 4 of 6 matched edited-build stages. These are two rounds with three edits each, not six independent clean-build experiments.

## Where time moved

| Round / backend | State holds (s) | Namespace commits (s) | Catalog waits (s) | Explicit barriers | Barrier time (s) |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 / Parallel SL7, eager sync | 16.90 | 25.98 | 22.23 | unmeasured | unmeasured |
| 2 / Parallel SL7, eager sync | 17.18 | 30.44 | 27.06 | unmeasured | unmeasured |
| 1 / Parallel SL7, explicit sync | 16.48 | 0.13 | 14.58 | 16 | 0.178 |
| 2 / Parallel SL7, explicit sync | 17.40 | 0.14 | 5.00 | 16 | 0.438 |

These counters cover each complete clean/unchanged/edited build sequence, may overlap across threads, and cannot be subtracted from Cargo wall time. Explicit barrier counts are attempts, including any errors; the benchmark reported no sealing errors. Earlier versions did not record these counters.

## Recorded sequence costs

| Round / backend | Cargo commands (s) | Idle observation (s) | Unmount drain (s) | Recorded sum (s) |
| --- | ---: | ---: | ---: | ---: |
| 1 / Native Cargo | 167.78 | 0.00 | 0.00 | 167.78 |
| 2 / Native Cargo | 164.14 | 0.00 | 0.00 | 164.14 |
| 1 / Parallel SL7, eager sync | 227.15 | 17.95 | 0.59 | 245.69 |
| 2 / Parallel SL7, eager sync | 232.51 | 10.03 | 0.55 | 243.09 |
| 1 / Parallel SL7, explicit sync | 202.29 | 37.89 | 0.61 | 240.79 |
| 2 / Parallel SL7, explicit sync | 195.09 | 31.88 | 0.60 | 227.57 |

The recorded sum includes the harness's idle observations and draining unmounts, which native Cargo does not require. It excludes initialization, remount startup, help-output checks, and integrity scanning; it is not the total experiment wall time. These components expose deferred sealing work separately from Cargo latency.

## Storage and validation

| Backend | Retained GiB before GC |
| --- | ---: |
| Native Cargo | 6.83 (6.83–6.83) |
| Parallel SL7, eager sync | 1.81 (1.78–1.83) |
| Parallel SL7, explicit sync | 1.78 (1.78–1.78) |

59 Linux tests, 35 host tests, strict Clippy, and all five mounted suites passed. Tests enforce FULL mode during the barrier write, inject failure, check file/directory/datasync error propagation and reservation cleanup, and recover sealed-file namespace timestamps after an acknowledged fsync and daemon kill. Process-kill tests do not simulate power loss. All 48 Cargo commands, 32 native/SL7 artifact-graph comparisons, freshness/executable checks, and four store integrity checks passed. Source and binary hashes tie the tested code to the measured executable. Storage includes native allocation plus catalog/packs before GC, excluding directory/inode/xattr overhead.

The latest measured implementation is `b91f464dce033e5123928f8720f3f58cd3d79080`; the eager-sync baseline is `bdf38437dfbbef4d090376c52581551a7a9932ab`.

[HTML tables](catalog-barrier.html) · [JSON measurements](catalog-barrier.json) · [Re-profile](catalog-barrier-profile.md) · [Durability protocol](../live-format.md)
