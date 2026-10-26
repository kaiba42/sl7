# Directory snapshots and parking_lot

## Conclusion

Keep the directory snapshot optimization and retain the standard State mutex. Clean-build medians moved from 166.63s to 165.28s (0.8% lower), with both matched clean builds faster; edit medians barely changed. Adding parking_lot produced 165.03s clean and 6.071s edited versus 165.28s and 6.077s for directory-only: no demonstrated material end-to-end lift from this lock swap. Build-plus-settling results were mixed across rounds. These are descriptive measurements, not significance claims.

## Implementation

The directory snapshot change is committed as 128a19f. The baseline is ec02cba. The parking_lot adapter is an isolated experiment, not part of the default build.

## What changed

Directory enumeration, child descriptor pinning, and one metadata read per child run outside State. Registration happens in batches of at most 64 children. Snapshot references retain inodes across rename, unlink, and forget; the final handle takes ownership of those references. readdir needs the native file type, so full logical attributes are no longer fetched for every child.

## What parking_lot changes

The separate 0.13.0 experiment replaces only the State mutex and its condition variables. A small adapter preserves fail-closed poisoning. DashMap, the reader pool, cache and SQLite stay the same. Two dedicated tests cover panic poisoning and poisoned condition-variable reacquisition. The current crate requires Rust 1.95; SL7 declares 1.85, so this remains an isolated experiment unless adopted explicitly.

## Comparison

Baseline is ec02cba. All builds use the same fixed source revision and paths, eight Cargo jobs, two sealing workers and two hashing workers, with compression workers, pipelining and prepared reuse disabled. Each sequence starts with empty output directories and an empty store. The second round reverses backend order. Native uses the same backing filesystem. Actual incremental artifacts and all Cargo artifact graphs are verified.

## Cargo elapsed time

Medians: two clean builds, six edits, six unchanged builds, and two remount checks per backend. Raw samples are in JSON.

| Build stage | Native Cargo | SL7 before | SL7 directory snapshots | + parking_lot 0.13 |
| --- | --- | --- | --- | --- |
| clean | 149.096s | 166.627s | 165.283s | 165.034s |
| edit | 4.370s | 6.109s | 6.077s | 6.071s |
| unchanged | 0.355s | 0.417s | 0.415s | 0.416s |
| unchanged-after-remount | 0.279s | 0.579s | 0.591s | 0.593s |

## Cargo plus settling

Medians: two clean builds, six edits, six unchanged builds, and two remount checks per backend. Raw samples are in JSON.

| Build stage | Native Cargo | SL7 before | SL7 directory snapshots | + parking_lot 0.13 |
| --- | --- | --- | --- | --- |
| clean | 149.096s | 180.998s | 175.568s | 174.166s |
| edit | 4.370s | 8.804s | 7.489s | 7.548s |
| unchanged | 0.355s | 0.620s | 0.618s | 0.620s |
| unchanged-after-remount | 0.279s | 0.780s | 0.792s | 0.794s |

## Individual clean builds



| Round | Native Cargo | SL7 before | SL7 directory snapshots | + parking_lot 0.13 |
| --- | --- | --- | --- | --- |
| 1 | 149.274s | 167.333s | 165.326s | 165.445s |
| 2 | 148.918s | 165.921s | 165.240s | 164.623s |

## Retained artifact storage

After all builds and edits. Native counts allocated regular-file bytes; SL7 counts native allocation plus store files. Source inputs are outside these output directories.

| Round | Native Cargo | SL7 before | SL7 directory snapshots | + parking_lot 0.13 |
| --- | --- | --- | --- | --- |
| 1 | 6.833 GiB | 1.893 GiB | 1.925 GiB | 1.910 GiB |
| 2 | 6.833 GiB | 1.904 GiB | 1.929 GiB | 1.885 GiB |

## Complete sequence, Cargo plus settling

Eight Cargo commands per sequence. Sequence time excludes executable checks, mount startup, integrity scans and unmount drain. State totals are occupancy counters, not Cargo wall time.

| Round | Native Cargo | SL7 before | SL7 directory snapshots | + parking_lot 0.13 |
| --- | --- | --- | --- | --- |
| 1 | 164.951s | 223.314s | 203.485s | 204.082s |
| 2 | 162.604s | 202.441s | 204.295s | 201.292s |

## Complete sequence, request-side State ownership

Eight Cargo commands per sequence. Sequence time excludes executable checks, mount startup, integrity scans and unmount drain. State totals are occupancy counters, not Cargo wall time.

| Round | Native Cargo | SL7 before | SL7 directory snapshots | + parking_lot 0.13 |
| --- | --- | --- | --- | --- |
| 1 | 0.000s | 12.740s | 4.384s | 4.279s |
| 2 | 0.000s | 10.535s | 4.453s | 4.279s |

## Code-level comparison

Each profile covers one clean build. Handler/wait totals overlap across threads. State hold intervals measure lock ownership; waits also include scheduling. These numbers do not predict recoverable Cargo wall time.

| Measurement | SL7 before | Directory snapshots | Change |
| --- | --- | --- | --- |
| Profiled Cargo elapsed | 169.511821s | 167.770505s | -1.0% |
| OPENDIR accumulated handler time | 6.116412s | 3.155630s | -48.4% |
| OPENDIR p99 handler latency | 0.019134s | 0.010254s | -46.4% |
| OPENDIR total State ownership | 6.003966s | 0.191808s | -96.8% |
| OPENDIR p99 State hold | 0.018900s | 0.000045s | -99.8% |
| OPENDIR maximum State hold | 0.043799s | 0.001121s | -97.4% |
| WRITE accumulated handler time | 13.502445s | 14.696766s | +8.8% |
| WRITE p99 handler latency | 0.000654s | 0.000619s | -5.3% |
| WRITE native_payload | 7.126426s | 9.014899s | +26.5% |
| WRITE inode_gate_wait | 1.017051s | 2.885103s | +183.7% |
| WRITE reconstruct | 0.000000s | 0.000000s | n/a |
| WRITE accumulated State wait | 3.128090s | 0.603501s | -80.7% |
| WRITE wait overlapping OPENDIR ownership | 2.334011s | 0.007212s | -99.7% |
| READ accumulated handler time | 5.450385s | 5.027364s | -7.8% |
| READ p99 handler latency | 0.000388s | 0.000429s | +10.6% |
| READ native_payload | 1.615461s | 1.327557s | -17.8% |
| READ inode_gate_wait | 1.273049s | 0.756829s | -40.5% |
| READ reconstruct | 1.154327s | 1.852489s | +60.5% |
| READ accumulated State wait | 0.389719s | 0.082190s | -78.9% |
| READ wait overlapping OPENDIR ownership | 0.299569s | 0.002564s | -99.1% |

## Profiled daemon CPU

CPU consumed during Cargo only; excludes post-build sealing. Both profiles enable per-request tracing and 99 Hz user/kernel sampling.

| Mode | SL7 before | Directory snapshots |
| --- | --- | --- |
| user | 30.020s | 31.320s |
| system | 34.090s | 33.510s |

## Selected kernel attribution

Percent of sampled daemon kernel CPU, with sample count. Samples are not syscall counts; these selected rows do not cover all kernel work.

| SL7 caller → syscall | SL7 before | Directory snapshots |
| --- | --- | --- |
| StagingFs::opendir → openat | 3.25% (104) | 4.44% (139) |
| StagingFs::opendir → statx | 1.38% (44) | 0.70% (22) |
| StagingFs::write → pwrite64 | 19.99% (639) | 20.24% (634) |
| StagingFs::write → writev | 6.20% (198) | 5.52% (173) |
| Live::attach_identity → fsync | 4.19% (134) | 5.17% (162) |
| Live::retire → ftruncate | 3.69% (118) | 3.58% (112) |

## What the code profile establishes

The directory bottleneck was removed: State ownership fell from 6.004s to 0.192s (96.8%), and write-side State waits fell from 3.128s to 0.604s (80.7%). The overlap between write waits and directory ownership fell from 2.334s to 0.007s. However, total write-handler time rose from 13.50s to 14.70s in these two profiles: native payload I/O and same-inode waits were higher. Read p99 also rose, while directory p99 nearly halved. A single profile pair cannot establish causality for those other changes. This is a demonstrated directory/global-lock improvement, not a general reduction in every request latency.

## Next code-level targets

In the optimized profile, WRITE spends 9.015 accumulated seconds in native_payload and 2.885s in inode_gate_wait. The corresponding code is StagingFs::native_io in src/staging/operations.rs and the mutation reservation in src/staging.rs. Kernel samples attribute 20.2% of daemon kernel CPU to StagingFs::write → pwrite64. Packer::prepare remains on 61.6% of sampled daemon userspace stacks, inclusively. Those are stronger targets for the next investigation than another State mutex swap; neither CPU shares nor overlapping handler totals are wall-time savings estimates.

## Correctness and provenance

The directory implementation passed 104 Linux unit/integration tests and the parking_lot variant passed 106; each intentionally ignored one manual throughput test. Both passed strict Clippy and all five mounted suites (readonly, staging, cache, live and selfhost). Eleven analyzer tests passed. The comparison validates 64 Cargo commands, 48 matching artifact-graph pairs, six store integrity scans, incremental outputs, and drained background workers. Two additional profile builds pass integrity scans and share the same artifact graph. Distinct binary hashes are required. The traced candidate must execute directory_child_pin and directory_register and must not execute directory_child_attr. A stale shared-target build was detected and replaced before measurements; the corrected candidate used an isolated Cargo target directory. The pod descriptor limit was set to 524,288, matching prior runs, after the initial mounted test hit its default 1,024 limit.

## Limits

Two reversed-order rounds on a shared node; descriptive medians, no significance claim. All SL7 binaries include code-profile and release debug information. Recording is disabled for end-to-end runs. Cargo time excludes executable checks and settling. Settling includes a 0.2-second observation floor. One additional instrumented clean build per SL7 implementation; overlapping request times are not wall-time savings. parking_lot 0.13.0 requires Rust 1.95; production declares Rust 1.85. Only State and its condition variables were replaced.

## Code and references

[Directory snapshots](../../src/staging/directories.rs), [State registration](../../src/staging.rs), [isolated parking_lot adapter](../../experiments/parking-lot/state_sync.rs).

[parking_lot 0.13 Mutex behavior](https://docs.rs/parking_lot/0.13.0/parking_lot/type.Mutex.html) and [crate metadata](https://docs.rs/crate/parking_lot/0.13.0/source/Cargo.toml).
