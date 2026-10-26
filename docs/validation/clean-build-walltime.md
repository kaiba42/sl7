# What comprises a clean Rust build under SL7?

## What this establishes

The SL7 profile took 189.92s. At least one build thread ran for 183.52s, but completion-chain units also experienced filesystem stalls while unrelated compilation continued. Synchronized Cargo, scheduler, FUSE and CPU-sample measurements expose both views. The production DashMap implementation is unchanged (b5ab793). This report separates elapsed seconds from overlapping CPU and wait totals. It does not claim an exact causal allocation of the native-to-SL7 slowdown to individual locks or functions.

## All four runs

Capture order is shown. One control and one profile per backend, on a shared node with an eight-CPU quota. The traced native run is slower than traced SL7, while the controls show the reverse. That variability prevents a reliable native-to-SL7 performance delta or instrumentation-overhead estimate from these four runs.

| Run | Cargo wall (s) | Build CPU (s) | SL7 CPU during build (s) | Later settling (s) |
| --- | --- | --- | --- | --- |
| native-control | 150.63 | 1018.60 | — | — |
| sl7-profile | 189.92 | 1137.80 | 72.98 | 13.83 |
| native-profile | 211.38 | 1198.39 | — | — |
| sl7-control | 218.55 | 1208.85 | 65.44 | 15.04 |

## An additive view of elapsed build time

Each column sums to its measured wall time. Categories describe all Cargo-descendant threads together. Running means at least one thread has a CPU; other threads may still be waiting. FUSE-wait occupancy is not automatically removable time, and waits can also delay dependencies while unrelated compilation runs.

| Exclusive wall-time state | Native profile (s) | SL7 profile (s) |
| --- | --- | --- |
| At least one build thread running | 206.444 | 183.518 |
| Runnable work, no build thread running | 2.053 | 1.823 |
| No runnable build thread; FUSE wait present | 0.000 | 4.003 |
| No runnable build thread; other wait | 2.833 | 0.534 |
| Unresolved / startup | 0.052 | 0.043 |

## Where the Cargo schedule goes

Follow the last-finishing Cargo unit backward through the actual last-unblocking dependency edges. Metadata edges stop at metadata availability, preserving pipelined overlap. The before/between category includes other necessary parallel work, not just scheduling overhead. These columns also sum to elapsed time, but are a different view of the same time; do not add them to the table above.

| Observed schedule segment | native-control | sl7-profile | native-profile | sl7-control |
| --- | --- | --- | --- | --- |
| Before / between last-unblocking chain units | 72.43 | 9.18 | 109.42 | 103.50 |
| Dependency compiler segments on that chain | 26.53 | 16.76 | 38.23 | 38.87 |
| Workspace compiler segments on that chain | 50.92 | 71.04 | 62.54 | 74.73 |
| Build-script segments on that chain | 0.69 | 92.67 | 1.06 | 1.33 |
| Cargo boundary remainder | 0.06 | 0.27 | 0.13 | 0.12 |

## A concrete completion dependency: the native crypto build

The aws-lc-sys build-script run is on the observed SL7 completion chain. These measurements cover that process and its children, including native compiler tools. Other waits include coordination and waiting for work; they are not all filesystem delays. This is the same unit in both profiles, even though the final dependency chain differs.

| Exclusive wall-time state | Native profile (s) | SL7 profile (s) |
| --- | --- | --- |
| At least one build thread running | 37.83 | 39.95 |
| Runnable work, no build thread running | 6.85 | 2.44 |
| No runnable build thread; FUSE wait present | 0.00 | 12.39 |
| No runnable build thread; other wait | 35.55 | 33.84 |
| Unresolved / startup | 0.01 | 0.00 |
| Total unit wall time | 80.24 | 88.62 |
| Native-tool CPU-seconds (overlaps wall states) | 37.55 | 39.68 |

## Why the four seconds of whole-build FUSE stalls is not a speedup ceiling

The selected completion-chain units accumulate 20.28s with no runnable thread in that unit and a FUSE wait present. Other compiler processes often run during those intervals, hiding them in the whole-build occupancy chart. This demonstrates dependency-local stalls that global CPU utilization misses. The chain timing is rounded by Cargo and aligned to invocation starts; this remains an observed occupancy measure, not a proof that removing FUSE would recover that many seconds.

## CPU and accumulated waits

Thread-seconds overlap across threads and with one another. They do not sum to build wall time. Runqueue delay means a thread was runnable but not scheduled; this includes competition and quota effects. Other sleeps include thread joins, futex waits and I/O, so their sum is not an optimization budget. Scheduler interval totals exceed process CPU accounting by 1.1% for SL7 and 1.3% for native; this bounded reconciliation error remains visible rather than being normalized away.

| Metric | Native profile | SL7 profile |
| --- | --- | --- |
| Build CPU-seconds from scheduler | 1214.02 | 1150.63 |
| Runnable thread-seconds | 209.34 | 41.03 |
| Direct synchronous FUSE sleep thread-seconds | 0.00 | 72.89 |
| Unresolved off-CPU thread-seconds | 0.11 | 0.01 |
| Mean build cores used | 5.67 | 5.99 |
| CPU reconciliation residual (s) | -15.638 | -12.826 |

## FUSE request lifetimes during the SL7 build

All 868,271 captured requests paired from send to completion. Rows below include only lifetimes intersecting the Cargo interval and clip them to that interval. Summed lifetimes overlap and include queuing plus service. They are not a dispatch/lock-time breakdown.

| Operation | Count | Sum (s) | Median (µs) | p99 (ms) |
| --- | --- | --- | --- | --- |
| WRITE | 255,759 | 32.33 | 18.4 | 0.94 |
| READ | 108,415 | 12.99 | 15.5 | 1.21 |
| OPENDIR | 2,583 | 8.59 | 75.1 | 28.79 |
| RELEASE | 32,679 | 8.18 | 15.5 | 0.65 |
| CREATE | 11,535 | 7.47 | 82.9 | 9.46 |
| LOOKUP | 171,183 | 6.13 | 20.6 | 0.11 |
| GETXATTR | 158,460 | 5.65 | 9.7 | 0.05 |
| OPEN | 24,599 | 3.51 | 23.6 | 0.14 |
| MKDIR | 2,800 | 2.03 | 69.6 | 4.59 |
| UNLINK | 5,667 | 1.58 | 66.8 | 6.09 |
| FLUSH | 40,314 | 1.40 | 12.0 | 0.10 |
| GETATTR | 33,074 | 0.89 | 13.9 | 0.09 |

## Where build userspace samples land

Requested 49 Hz, leaf-module attribution; percentages are samples, not elapsed build time. LLVM includes optimization/code generation and related work. rustc_driver includes compiler work outside that library. Both builds have closely comparable sample composition.

| Leaf module | Native samples (%) | SL7 samples (%) |
| --- | --- | --- |
| LLVM | 63.7% | 64.2% |
| rustc_driver | 24.8% | 24.2% |
| libc | 2.7% | 2.6% |
| cargo | 0.0% | 0.0% |
| other | 8.8% | 8.9% |
| unknown | 0.0% | 0.0% |

## SL7 work during the build

SL7 consumed 31.50 userspace and 41.48 kernel CPU-seconds before Cargo exited. Of 1,523 daemon userspace samples, compression appears in 31.6%, SHA-256 in 19.9%, and SQLite in 17.1%. These stack categories overlap and exclude kernel execution. Kernel CPU is a substantial part of the daemon cost; optimizing only a userspace map or hash function cannot account for all of it.

## Conclusions and next measurement

The 189.92s SL7 profile is mostly concurrent compiler activity, but global activity hides dependency-local filesystem stalls. Its selected completion-chain units show 20.28s with no runnable thread in that unit and a direct FUSE wait present. The aws-lc-sys build-script alone shows 12.39s in that state. Basic LOOKUP plus GETATTR requests total only 7.02 overlapping request-seconds, versus 32.33 for WRITE, 12.99 for READ and 8.59 for OPENDIR. The next focused investigation should correlate those request tails with daemon queueing, backing-file I/O, publication and kernel CPU on the affected dependency chain. No new optimization was made. The noisy controls prevent claiming that any measured category explains a precise number of seconds of native-to-SL7 slowdown, or that all observed FUSE stalls are recoverable.

## Validation and scope

Four clean Cargo builds, matching artifact graphs, executable checks, incremental-artifact checks and two SL7 integrity scans passed. Both traces passed request pairing, clock bounds and CPU-accounting checks; no trace loss was reported. Seven analyzer tests cover overlap, pipelining, scheduler races, request reuse, package IDs and CPU-window filtering. Wake-before-switch races remain explicitly unresolved in the JSON. Production code was not changed. Collection uses Linux 6.12.58, Rust 1.97.1, 8 jobs, 12 GiB memory, two SL7 sealers and two hash workers; preparation reuse, import pipelining and parallel compression are disabled. Private source, names, raw graphs, traces and sampled stack memory are excluded from public artifacts.

## Reproduction and provenance

See scripts/walltime/README.md and clean-build-walltime.json. The JSON records binary and collector hashes, the raw archive checksum, tool versions, CPU sample aggregates, dependency phases and scheduler timelines. Cargo timing semantics: [Cargo timing reference](https://doc.rust-lang.org/cargo/reference/timings.html). FUSE requests were correlated by kernel request pointer because the measured kernel reports zero in the formatted send ID field.
