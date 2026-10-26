# SL7: where time goes in the code

## The main code-level finding

`StagingFs::opendir` visits 472,857 directory entries, calling both child lookup and attribute fetch while holding `Mutex<State>`. Those two calls consume 5.66s in total. Writes accumulate 3.32s waiting for that mutex, and 2.85s (86.0%) overlaps measured directory-open ownership. This identifies a specific source of contention, beyond merely observing a mutex wait.

## Measurement conditions

Same instrumentable binary, fixed Synkro source, empty output/store per arm, eight Cargo jobs and incremental artifacts present. The control disables recording and sampling; the profile enables both. One shared-node pair cannot precisely isolate instrumentation overhead. Daemon CPU rises even though build wall times are close. No performance optimization was applied.

| Run | Build wall (s) | Daemon user CPU (s) | Daemon kernel CPU (s) | Later settling (s) |
| --- | --- | --- | --- | --- |
| sl7-control | 166.40 | 29.59 | 30.84 | 18.97 |
| sl7-profile | 165.91 | 34.93 | 35.47 | 10.10 |

## Where time goes in our request code

These are accumulated handler seconds across parallel requests, not additive seconds of build elapsed time. Each request is partitioned into exclusive phases. Handler time begins inside SL7 and excludes pre-dispatch FUSE queueing. The earlier FUSE-lifetime totals came from a different run; subtracting these handler totals from those numbers would not measure queue time.

| Operation / code | Measured work | Accumulated seconds |
| --- | --- | --- |
| [write_handle → write_all_at](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L779) | Native backing-file write | 6.803 |
| [with_mutation + NativeIo::drop](https://github.com/kaiba42/sl7/blob/main/src/staging/operations.rs#L10) | WRITE State acquisition, including cleanup | 3.317 |
| [opendir → State::lookup](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L335) | Pin/register each directory child | 3.037 |
| [opendir → State::attr](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L349) | Full attributes to obtain just kind | 2.623 |
| [read_with → Condvar::wait](https://github.com/kaiba42/sl7/blob/main/src/staging/reads.rs#L119) | Inode mutation/exclusive-I/O gate | 1.825 |
| [read_with → read_at](https://github.com/kaiba42/sl7/blob/main/src/staging/reads.rs#L177) | Native read, including buffer allocation | 1.131 |
| [read_with → Live::read](https://github.com/kaiba42/sl7/blob/main/src/staging/reads.rs#L168) | Catalog/chunk reconstruction | 0.783 |
| [Readers::take](https://github.com/kaiba42/sl7/blob/main/src/staging/reads.rs#L25) | Reader-connection acquisition | 0.000972 |
| [write → reply.written](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L1027) | FUSE reply send | 1.350 |
| [read → reply.data](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L1007) | FUSE read reply | 0.836 |

## Complete request totals and tails

Exclusive phase details and the twenty longest requests for each operation are preserved in the JSON. Totals overlap across workers. Percentiles describe complete handler lifetimes in this profiled run.

| Operation | Requests | Sum (s) | Median (µs) | p99 (ms) | Maximum (ms) |
| --- | --- | --- | --- | --- | --- |
| WRITE | 258,634 | 12.761 | 11.1 | 0.63 | 460.89 |
| OPENDIR | 2,583 | 6.281 | 58.4 | 20.12 | 43.02 |
| READ | 108,750 | 5.041 | 7.8 | 0.34 | 460.90 |

## Why opendir is expensive

[The directory-open loop](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L1224) runs inside `with_read`, which owns the global state mutex. `State::lookup` checks parent metadata, constructs a descriptor-relative path, pins the child, and registers its identity. `State::attr` then fetches metadata again and may query logical metadata, although readdir needs only the entry kind. Child lookup plus attributes account for 90.1% of directory-open handler time; enumeration is 0.340s and sorting is 0.046s. This path calls the stateful helpers directly, rather than the separate registered-inode metadata request fast path.

## Which State waits coincide with opendir owning the mutex?

Wait and ownership intervals are recorded on the same monotonic clock, then intersected. Waiting includes scheduling delay; this overlap does not prove an equal end-to-end speedup from removing the owner work.

| Waiting operation | State wait (s) | Overlapping opendir ownership (s) | Share |
| --- | --- | --- | --- |
| WRITE | 3.317 | 2.854 | 86.0% |
| READ | 0.276 | 0.186 | 67.6% |
| OPENDIR | 0.080 | 0.068 | 85.0% |

## Kernel CPU linked to SL7 callers

3,328 daemon kernel samples at requested 99 Hz. Each sample is assigned one caller/syscall pair; percentages below are shares of sampled kernel CPU, not build wall time. Unresolved callers remain explicit in the JSON.

| SL7 caller | Kernel entry | Kernel samples (%) |
| --- | --- | --- |
| StagingFs::write | pwrite64 | 18.57% |
| StagingFs::write | writev | 5.86% |
| Live::attach_identity | fsync | 4.45% |
| StagingFs::opendir | openat | 4.21% |
| Packer::prepare | write | 3.22% |
| Live::retire | ftruncate | 3.16% |
| StagingFs::opendir | other / unresolved syscall | 3.06% |
| Packer::prepare | read | 2.79% |
| Live::publish_seal | pwrite64 | 2.67% |
| StagingFs::read | pread64 | 2.61% |
| StagingFs::read | writev | 2.46% |
| StagingFs::opendir | statx | 2.19% |
| Live::publish_seal | fsync | 2.16% |
| Live::publish_seal | pread64 | 1.92% |
| Packer::prepare | pread64 | 1.62% |
| Packer::prepare | fcntl | 1.56% |
| Packer::prepare | fsync | 1.08% |

## What those kernel stacks mean in the code

[write_handle](https://github.com/kaiba42/sl7/blob/main/src/staging.rs#L772) delegates to native I/O and `FileExt::write_all_at`; its stacks lead into `pwrite64`, `ext4_buffered_write_iter` and `generic_perform_write`. Sending the FUSE reply is a separate `writev` path. [Live::attach_identity](https://github.com/kaiba42/sl7/blob/main/src/live.rs#L321) writes the identity xattr and calls `sync_all`; [Live::retire](https://github.com/kaiba42/sl7/blob/main/src/live.rs#L447) truncates native data, restores sparse logical length/timestamps, and syncs. Directory-open stacks account for 11.3% of kernel samples. The kernel profile also observes inotify callbacks; their watcher origin was not investigated. No host settings were changed.

## Userspace CPU: the exact SL7 functions

3,518 daemon userspace samples. Inclusive function percentages overlap through callers and callees. Background packing is a different cost from foreground request latency.

| Function | What it does | Inclusive userspace samples (%) |
| --- | --- | --- |
| [Packer::prepare](https://github.com/kaiba42/sl7/blob/main/src/store.rs#L156) | Preparation subtree | 59.4% |
| [Packs::add](https://github.com/kaiba42/sl7/blob/main/src/compression.rs#L203) | Compression / pack insertion subtree | 31.1% |
| [Chunker::next_chunk](https://github.com/kaiba42/sl7/blob/main/src/chunk.rs#L44) | Read-ahead, boundary scan, chunk allocation | 13.0% |
| [chunk::identity](https://github.com/kaiba42/sl7/blob/main/src/chunk.rs#L6) | Per-chunk SHA-256 identity | 10.3% |
| [Live::publish_seal](https://github.com/kaiba42/sl7/blob/main/src/live.rs#L405) | Catalog publication subtree | 8.0% |

## The longest read/write stall

The longest write spent 460.68ms inside `write_all_at`. The longest read spent 460.75ms at the inode gate and began 27.1µs after that write began. This is consistent with a read waiting for a long same-inode write. Inode IDs were not recorded, so that pairing is an inference. On-CPU kernel samples do not explain the sleeping portion of this specific long write.

## Where I would focus implementation next

First, simplify directory entry construction so it does not compute full logical attributes just to obtain a file type, then move enumeration and metadata work out of the filesystem-wide mutex while preserving namespace-race behavior. That targets measured work that also blocks writes. Second, investigate backing-write sizes, writeback stalls and FUSE reply copying at `native_io` / `write_handle`; the longest read tail is at the inode gate, while chunk reconstruction and reader-pool waiting are smaller totals. Background CPU work is concentrated in compression, chunking/hashing and publication. Changes to identity or retirement syncs must preserve the existing crash-recovery contract.

## Validation and boundaries

Two clean builds, matching artifact graphs, executable checks and two store integrity scans passed. All 370,959 recorded requests passed exact phase-sum validation; 992 outside-build requests were excluded. Total READ/WRITE trace counts match the daemon's independent final counters. Both normal and code-profile Linux configurations passed 99 tests and strict Clippy; ten Python analyzer tests passed. The mounted fixture passed before the full run. Trace formatting is outside the measured handler interval but still costs CPU; nested clock/bookkeeping costs remain inside phases. Stack unwinding is bounded and misses some/inlined callers. Raw perf memory, source identities and Cargo logs remain private. Source/binary hashes and archive checksum are in the JSON. Reproduction: scripts/walltime/CODE_PROFILE.md. This change adds opt-in diagnostics, with no storage or concurrency algorithm change.
