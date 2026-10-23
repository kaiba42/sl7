# Where SL7 performance work should start

Historical baseline. The four changes below are now implemented; see the
[measured optimization results](optimizations.md).

Measured on 2026-10-23 using the exact Synkro revision, lockfile, Rust compiler,
and optimized SL7 binary from the [build baseline](synkro-build-performance.md).
The complete [profiling results](profiling.json) include sample counts, request
latencies, syscall counts, and the isolated hash-backend experiment.

## First finding: enable ARM64 SHA-256 acceleration

SHA-256 occupied **70.3–72.6% of daemon userspace CPU samples during rebuilds**,
and 62.6% during the clean build. The pinned `sha2` 0.10.9 selects its ARM64
hardware backend only with the `asm` feature; SL7 does not currently enable it.
The profiling VM advertises the ARM64 `sha2` CPU capability. This is a dependency
configuration issue, not evidence that content hashing itself is impractical.

A separate probe used SL7's exact chunk-hash domain prefix and length prefix,
processing 1 GiB as repeated 8 KiB inputs. Three samples per backend, in mixed
order, produced these medians:

| Backend | Seconds per GiB | Throughput |
| --- | ---: | ---: |
| Current software backend | 1.869 | 548 MiB/s |
| `sha2/asm` hardware backend | 0.330 | 3,100 MiB/s |

The hardware backend was **5.66× faster in this hash-only probe**. Both produced
identical digests and checksums, and passed the SHA-256 `abc` known-answer vector.
This is not a measured whole-build speedup. The first optimization experiment
should enable `sha2/asm`, run the correctness suites, then repeat the matched
Cargo build comparison. It preserves the hash algorithm and store format.

## Where the worker spends its time

Two profiled one-line rebuilds took 57.0 and 70.7 seconds; the unprofiled rebuild
in the same session took 62.6 seconds. These are diagnostic runs, not enough
samples to estimate tracing overhead precisely. No SL7 implementation code was
changed or instrumented for the profiles.

The sampled call stacks split into these mutually exclusive categories, assigning
hydration first, then sealing, then the mounted read callback:

| Daemon userspace CPU category | Rebuild 1 | Rebuild 2 |
| --- | ---: | ---: |
| Reads through the mount | 50.9% | 52.3% |
| Sealing files | 34.0% | 31.5% |
| Hydrating files | 13.5% | 13.8% |
| Other | 1.7% | 2.4% |

These categories each include hashing and other work beneath them. They must
not be added to the SHA-256 percentage above. CPU percentages describe sampled
**userspace execution in the daemon**, not total build wall time or kernel time.

Kernel FUSE events provide a separate view of latency. The table sums time from
`fuse_request_sent` to `fuse_request_end`, an approximation to userspace response
time that also includes scheduling and kernel processing around the callback:

| Operation | Rebuild 1 | Rebuild 2 | Share of aggregate response time |
| --- | ---: | ---: | ---: |
| Read | 18.45 s | 23.41 s | 45.8–47.4% |
| Release / close | 11.38 s | 14.26 s | 28.2–28.9% |
| Set attributes | 4.88 s | 6.05 s | 12.1–12.2% |
| All operations | 40.33 s | 49.39 s | 100% |

Most callbacks were fast; the largest stalls were not. A single release took
4.34 seconds in one rebuild and 6.66 seconds in the other. A single attribute
change took 4.88 and 6.04 seconds. Other requests waited up to 4.34 and 6.66
seconds between their send and sent events.

Aggregate request dispatch delay was 61.69 and 69.71 request-seconds. About
77.5% and 71.5% of that delay overlapped release response intervals. Concurrent
client requests wait at the same time, so these sums **must not be added to build
wall time**. The overlap establishes that long releases coincide with widespread
waiting; it is not a controlled measurement of an asynchronous-sealing speedup.

## Next priorities

1. **Enable the hardware SHA backend and rerun the build comparison.** It is the
   largest measured CPU hotspot and the smallest initial change.
2. **Avoid hydrating entire files for metadata-only changes.** Hydration took
   13–14% of sampled userspace CPU, with stacks passing through `setattr`.
   [The current callback](../../src/staging.rs) hydrates every regular file before
   applying permissions, timestamps, or size changes. Metadata-only operations
   should preserve sealed content; truncation to zero should not need to read
   discarded bytes. The trace does not include attribute flags, so it does not
   identify which particular attribute caused the long observed calls. Preserve
   logical metadata and recovery semantics when changing these paths.
3. **Cache verified decoded chunks and reuse pack descriptors.** The read path
   currently opens a pack, checks its header, reads and decompresses a chunk,
   and hashes the entire decoded chunk for each lookup. One rebuild issued
   412,994 `openat`, 1,674,125 `read`, 594,072 `pread64`, 302,137 `lseek`, and
   761,170 `getxattr` calls in the daemon. These are daemon-wide counts, not all
   exclusively pack reads. A bounded cache can reuse already-verified immutable
   chunks; verification should not simply be removed. Measure cache hit rates
   and read amplification before choosing a cache size.
4. **Move expensive sealing off the sole request worker.** Long releases block
   unrelated operations. Fuser defaults to one worker and SL7 does not override
   it; state is also protected by one mutex. Extra workers alone would still
   serialize on that mutex. Use per-inode transition ownership and bounded
   packing workers, preserving publication ordering and file/mapping semantics.

There were zero observed `futex` syscalls during either traced rebuild. This is
consistent with the single worker, not evidence that the lock architecture can
scale. Optimizing lock acquisition itself, changing databases, or replacing Zstd
would be lower priorities than the measured hashing and transition costs.

## Method and validity

- Same ARM64 OrbStack image and SL7 binary hash as the baseline; 8-CPU quota,
  eight Cargo jobs, 12 GiB memory limit, no swap allowance.
- Exact committed source snapshot; isolated Linux source and output directories.
  Downloads happened before measurement. Default debug profile and features;
  warm OS caches; empty Cargo outputs for the initial build.
- `perf record -e cpu-clock:u -F 99 --call-graph dwarf,8192` attached only to the
  SL7 daemon. The clean build yielded 15,661 samples; rebuilds yielded 2,636 and
  3,136. All three CPU recordings reported zero lost samples.
- FUSE send/sent/end tracepoints were collected system-wide with a kernel filter
  restricting collection to this mount's connection. There were 648,210 and
  658,265 complete requests, no incomplete unique requests, and zero reported
  lost samples. One identical repeated `sent` event was deduplicated.
- Syscall counters attached only to the daemon. Kernel symbol maps were
  restricted; this experiment does not provide kernel stack attribution.
- All six Cargo commands succeeded, both unchanged builds were entirely fresh,
  edited executables displayed the expected description, normal unmount
  succeeded, and the final full store integrity check passed. No memory-limit
  or OOM events were recorded during the workload.
- OrbStack stopped after the workload and integrity checks had completed. Its
  saved recordings were recovered and copied out before further analysis. The
  later hash probe ran after restart. The disposable container was removed;
  no new Docker image was created.

## Reproduce

Using the test image produced by `scripts/docker-test.sh` and a checkout
containing baseline revision `ec6d4c53b8fdf4321de220a922f7efce67788d40`:

```sh
bash scripts/profile-synkro.sh /path/to/synkro-rs test-results/profile
```

The script uses a disposable container with FUSE and perf capabilities, copies
raw recordings and summaries out, then removes the container. Kernel perf and
FUSE tracepoint availability vary by runtime. Raw traces stay in ignored
`test-results/`; the checked-in JSON contains only derived measurements.

For the separate hash probe, inside the same Linux ARM64 environment:

```sh
cd scripts/hash-backend-probe
cargo run --release --locked
cargo run --release --locked --features hardware
```

Repeat the probe in mixed order. It isolates hash throughput on hot input data;
it does not model I/O, compression, cache capacity, or an entire Cargo build.
