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
[current build performance measurements](validation/oaktown-build-performance.md).
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
FUSE handle/mapping is released. One background worker packs files outside the
filesystem mutex and validates that they remain unchanged before publishing.
Normal unmount drains pending sealing. Reads reconstruct requested ranges through
a bounded verified-chunk cache; writable opens hydrate sealed files
before mutation. Hard links share an inode; separate files keep separate identities.
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

This is an owned-build-tree prototype. It serializes callbacks and needs temporary
native space for hydration. Decoded chunks and extent mappings are cached within
fixed budgets, and one background worker packs closed files.
GC is offline and can be expensive. Arbitrary multi-user ownership, restrictive
permission transitions, ACL/xattr passthrough, special files, direct I/O, online
GC, remote storage, and native macOS semantics are not qualified. Current mounted
suites run as container root. Process-death tests are not storage-device failure
certification.

The earlier artifact smoke test stored two identical 58.38 MiB `.rmeta` files in
35.16 MiB including the catalog, with zero new chunks or pack bytes on the second
import: [recorded result](validation/real-artifact-smoke.json).
