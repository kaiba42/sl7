# SL7 usage and implementation notes

SL7 is an experimental filesystem for sharing Rust build-artifact bytes across
worktrees. Cargo sees ordinary, independent directories. Underneath, large closed
files become content-defined chunks stored once, compressed, and reconstructed
on reads. Mutable files use native backing inodes until they can be sealed again.

The initial implementation includes a **writable Linux FUSE mount**, an offline
immutable store, a read-only mount, integrity checking, recovery, and offline
garbage collection. Native macOS mounting is not qualified. Development and real
mount tests use **OrbStack/Docker**; no macOS filesystem driver is required.

## Run the complete Docker test

```sh
bash scripts/docker-test.sh > test-results.json
```

This builds an optimized SL7 daemon and runs four suites:

- Read-only reconstruction: random reads, mmap, and `rustc` consuming `.rmeta`
  and `.rlib` files.
- Native staging: filesystem operations, Cargo build/test/check, build scripts,
  proc macros, and daemon termination/restart after file/directory sync.
- Writable deduplication: concurrent worktree builds, debug and release builds,
  ten one-line edits with executable checks, fresh builds across remounts, partial
  and full `cargo clean`, GC, and rebuilding after clean.
- Self-hosting: build and test SL7 with its entire dependency graph through SL7,
  build release, edit its CLI description, rebuild both profiles, execute the
  edited binaries, clean, rebuild, and clean again.

The script defaults to Docker context `orbstack`; override with
`SL7_DOCKER_CONTEXT`. The disposable container receives `/dev/fuse`, `SYS_ADMIN`,
and an unconfined AppArmor profile. It mounts no host directory or Docker socket.
Test roots are unbindable mounts so recursive container filesystem exports cannot
retain the test's FUSE mount. Normal unmount and daemon exit 0 are required.
The container is removed; image `sl7-mount-test:local` remains for reuse.

See [the end-to-end result](validation/end-to-end.json) and
[validation notes](validation/README.md). These are correctness and storage
measurements, not controlled performance benchmarks or power-loss certification.

For a separate native-versus-SL7 comparison using the full Synkro workspace, see
[current build performance measurements](validation/native-latest.md).
The reproducible harness covers clean builds, unchanged builds, and one-line
edits in both debug and release profiles.

## Use a writable mount on Linux

Requires Rust, a C compiler, `/dev/fuse`, `fusermount3`, and a backing filesystem
supporting sparse files, user xattrs, locks, and directory fsync. The checked
toolchain is Rust 1.97.1; `Cargo.lock` pins dependencies.

Large output trees need a generous open-file limit for the daemon, which keeps
backing inodes open. A soft limit of 1,024 failed the full-workspace benchmark;
524,288 passed. Before starting the mount, raise the shell's soft limit up to its
configured hard limit with `ulimit -Sn "$(ulimit -Hn)"`. If the hard limit is also
low, increase it in your service or container configuration.

```sh
cargo build --locked --release --features fuse
target/release/sl7 --store /data/sl7 init
mkdir -p /mnt/artifacts
target/release/sl7 --store /data/sl7 mount /mnt/artifacts
```

The mount runs in the foreground. In another terminal, from a Cargo workspace:

```sh
CARGO_TARGET_DIR=/mnt/artifacts/main/target \
CARGO_BUILD_BUILD_DIR=/mnt/artifacts/main/build cargo build

CARGO_TARGET_DIR=/mnt/artifacts/main/target \
CARGO_BUILD_BUILD_DIR=/mnt/artifacts/main/build cargo build --release
```

Use a separate prefix, such as `branch/target` and `branch/build`, for another
worktree. Keep a workspace's prefix for every command, including `test`, `check`,
and `clean`. Writers share one daemon and chunk store; worktrees retain independent
names and mutable files. Cargo still decides which compilations run. SL7 shares
output bytes; it is not a compiler-result cache.

After all users of the mount have stopped:

```sh
fusermount3 -u /mnt/artifacts
# Wait for the foreground mount process to exit, then:
target/release/sl7 --store /data/sl7 check
target/release/sl7 --store /data/sl7 live-stats
target/release/sl7 --store /data/sl7 gc
```

`cargo clean` removes namespace entries. **Unmount and run `gc` to reclaim their
unreferenced chunks and compact partially live packs.** GC shrinks the catalog;
a small empty catalog remains after artifact bytes are gone. The exclusive store
lock rejects offline commands while a mount owns the store.

Access artifacts through the mount, never directly through the store's `tree/`:
sealed files there are sparse placeholders, not usable copies. Backups must
preserve the whole unmounted store, including native inodes and xattrs. The empty
mountpoint must be outside the store; neither path may contain the other.
Inside runtimes that recursively export container filesystems, use an unbindable
mount parent as the harness does; see [validation notes](validation/README.md).

## Storage and transitions

- `tree/`: names, links, permissions, small/active native files, and placeholders.
- `catalog.sqlite`: manifests, extents, chunk locations, and native-inode bindings.
- `packs/`: immutable compressed chunks in packs up to 64 MiB.
- `tmp/`: unpublished packs; `owner.lock`: exclusive ownership.

Chunking targets 8 KiB with a 32 KiB maximum. SHA-256 identifies chunks; Zstandard
compresses each independently with a raw fallback. Identical files add namespace
and extent metadata but no new chunk payload. Edits can reuse unchanged chunks.

Files at least 64 KiB with owner read/write permission are sealed after the last
FUSE handle/mapping is released. Two background workers prepare independent files
outside the filesystem mutex, validating their mutation epoch before publication.
Catalog commits remain serialized. Set `SL7_SEAL_WORKERS=1` for serial sealing;
values from 1 through 8 are supported. More workers do not necessarily reduce build
time; see the [worker comparison](validation/parallel-sealing.md).

For experimental overlap within large files, start the mount with
`SL7_IMPORT_PIPELINE=1` (default `0`). Files at least 16 MiB use a producer thread
for reading, chunking, and whole-file hashing while the sealing worker hashes
chunks, checks for duplicates, compresses, and writes packs. Each worker queues
at most two batches of 16 chunks (1 MiB of queued payload, plus producer and
consumer batches). Small files and offline `put` imports remain sequential.
This consumes additional CPU and does not guarantee faster Cargo builds.
The [matched comparison](validation/import-pipeline-costs.md) did not show a
consistent benefit across clean and edit builds. A later
[comparison that drains sealing between stages](validation/quiescence-costs.md)
measured 0.2% lower clean-build and 6.2% lower edit medians, with four of six
paired edits faster; two rounds do not establish a reliable speedup.
With `SL7_CACHE_STATS=1`, the unmount report includes `import_pipeline` and
`pipelined_prepares`; the latter counts attempted preparations, including canceled
ones. Cancellation closes the queue and joins the producer before returning.

For parallel compression within an artifact, use `SL7_COMPRESS_WORKERS=2`
(default `0`, supported range 0–4). Each sealing worker lazily creates its own
persistent compression pool. Only novel chunks from files at least 16 MiB enter
that pool; duplicate chunks and smaller files avoid it. The coordinator keeps
chunk hashing and catalog lookups ordered, submits batches of 16 novel chunks,
and writes compressed results in original order. Chunk identities, pack bytes,
rollover, and durability barriers remain unchanged. This works independently of
`SL7_IMPORT_PIPELINE`.

At most four submitted batches plus one accumulating batch are retained per
sealer: at most 2.5 MiB of raw-equivalent payload, plus per-worker compression
scratch/output, metadata, and existing import buffers. With two sealers and two
compression workers, up to four compression threads can run. Cancellation skips
queued work and drains every outstanding receipt before preparation returns;
normal unmount therefore cannot leave compression running. Diagnostics expose
`compression_workers_per_sealer`, `max_parallel_compression_per_sealer`, and
`compressed_chunks_on_workers` (including work from subsequently canceled seals).
The peak is a per-sealer high-water mark, not a mount-wide concurrency count.
The [matched build comparison](validation/compression-costs.md) found essentially
unchanged Cargo latency (about 0.4% slower medians) and mixed settling costs.
The pool stays disabled by default; measured concurrency alone is not a speedup.

Operations on the publishing inode wait; unrelated reads and native writes can
continue through the four request workers.
Normal unmount drains pending sealing. Reads reconstruct concurrently outside the
global filesystem lock, using independent catalog connections and shared bounded
caches. Same-inode mutations wait for active reads; writable opens hydrate sealed
files before mutation. Hard links share an inode; separate files keep separate identities.
Reopening a file preserves its kernel page-cache contents, avoiding reconstruction
of unchanged pages on every open. Writes and truncation through the mount remain
coherent. Kernel cache memory is reclaimable and separate from SL7's userspace
cache budgets.

For live diagnostics, start the mount with `SL7_MOUNT_STATUS=1`. On Linux, read
its root's virtual `user.sl7.status` attribute as JSON:

```sh
python3 -c 'import os; print(os.getxattr("/tmp/sl7-mount", "user.sl7.status").decode())'
```

`drained` means no queued sealing, active sealing, reads, or native I/O remain.
`quiescent` additionally requires closed FUSE handles, no fatal worker failure,
and no shutdown in progress.
The snapshot includes inode queue overflow and workers asleep in I/O. It is an
observation, not an fsync or a barrier against subsequent client activity. Status
support is disabled by default. The benchmark's `--settle-mode quiescent` enables
it and records three stable quiet observations after each build and executable
check; the historical `cpu-idle` mode remains available for comparison.

Read-only closes do not schedule further sealing once the inode's current
mutation epoch has been checked. A write or metadata mutation restores eligibility.
Successful name lookups and attributes are cached in the kernel for one second.
Changes through the mount remain immediately visible; allocated block counts
changed by background sealing can take that interval to refresh in `stat`.
See [the live format and recovery protocol](live-format.md).

`live-stats` separates logical bytes, native allocated blocks, and content-store
bytes. Native allocation plus `content.total_file_bytes` approximates storage use,
excluding native directory/inode/xattr overhead, host APFS extents, and Docker
image space. Sparse-placeholder file lengths are not physical allocation.

## Offline store and read-only mount

The default build supports immutable imports without FUSE:

```sh
cargo build --locked
target/debug/sl7 --store /tmp/sl7-demo init
target/debug/sl7 --store /tmp/sl7-demo put main library path/to/library.rmeta
target/debug/sl7 --store /tmp/sl7-demo put branch library path/to/library.rmeta
target/debug/sl7 --store /tmp/sl7-demo read main library --offset 4096 --length 4096
target/debug/sl7 --store /tmp/sl7-demo get main library /tmp/restored.rmeta
target/debug/sl7 --store /tmp/sl7-demo check
```

Import sources must be quiescent; view/name labels are immutable. `read` emits
bytes, metadata commands emit JSON, and `get` verifies the digest and refuses
overwrite. With `--features fuse`, `mount-read-only DIRECTORY` exposes
`<view>/<name>` read-only/noexec. Offline imports preserve bytes, not original
modes/timestamps. `mount-staging BACKING DIRECTORY` is the non-deduplicating native
qualification adapter.

## Local checks and limits

```sh
cargo fetch --locked       # once, if dependencies are not cached
bash scripts/verify.sh    # fmt, check, tests, strict Clippy
```

On macOS, `fuse-check` compiles/tests the read-only adapter without a runtime and
refuses real mounts. Do not use `--all-features` to mount. Local checks use the installed Rust toolchain.

This is an owned-build-tree prototype. Read reconstruction and background
publication run outside filesystem state. Native payload writes can overlap across
inodes, while hydration and metadata/namespace bookkeeping still hold filesystem
state. Catalog writes remain serialized. Hydration needs temporary native space.
Decoded chunks and extent mappings are cached within fixed budgets, and bounded
background workers pack closed files.
GC is offline and can be expensive. Arbitrary multi-user ownership, restrictive
permission transitions, ACL/xattr passthrough, special files, direct I/O, online
GC, remote storage, and native macOS semantics are not qualified. Current mounted
suites run as container root. Process-death tests are not storage-device failure
certification.

The earlier artifact smoke test stored two identical 58.38 MiB `.rmeta` files in
35.16 MiB including the catalog, with zero new chunks or pack bytes on the second
import: [recorded result](validation/real-artifact-smoke.json).
