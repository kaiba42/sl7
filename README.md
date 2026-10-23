# SL7

SL7 is an experimental Linux filesystem that deduplicates and compresses Rust
build artifacts. Cargo writes ordinary files; SL7 shares repeated chunks and
reconstructs them on reads. Worktrees keep independent output directories.

**Prototype:** native macOS mounting is unsupported. The initial benchmark saved
72–74% of storage at a build-time cost. See the
[latest read-cache measurements](docs/validation/extent-cache.md) and the
[original native-versus-SL7 comparison](docs/validation/synkro-build-performance.md).

## Try it with Docker

Requires Docker with `/dev/fuse` support; tested with OrbStack on macOS.

```sh
git clone https://github.com/kaiba42/sl7.git
cd sl7
bash scripts/docker-test.sh
```

This builds SL7 and runs filesystem, Cargo, and recovery tests in a disposable
container. It defaults to the `orbstack` context; for another Docker setup use
`SL7_DOCKER_CONTEXT=default bash scripts/docker-test.sh`.

## Use it for builds on Linux

Requires Rust (tested with 1.97.1), a C compiler, `fuse3`, and `/dev/fuse`.
From the SL7 checkout, create a fresh store and empty mountpoint:

```sh
cargo build --locked --release --features fuse
mkdir -p /tmp/sl7-mount
target/release/sl7 --store .sl7 init
target/release/sl7 --store .sl7 mount /tmp/sl7-mount
```

Leave the mount running. In another terminal, inside your Rust project:

```sh
export CARGO_TARGET_DIR=/tmp/sl7-mount/main/target
export CARGO_BUILD_BUILD_DIR=/tmp/sl7-mount/main/build
cargo build
```

Use another prefix, such as `/tmp/sl7-mount/branch/`, for another worktree. Keep
both Cargo variables set for builds, tests, checks, and cleaning.

After stopping builds, unmount and wait for the mount process to exit. From the
SL7 checkout, inspect storage or reclaim unreferenced chunks:

```sh
fusermount3 -u /tmp/sl7-mount
target/release/sl7 --store .sl7 live-stats
target/release/sl7 --store .sl7 gc
```

Access artifacts only through the mount; sealed backing files are sparse
placeholders. GC runs offline. Start with disposable build outputs.

More: [usage](docs/usage.md) · [storage format](docs/live-format.md) ·
[validation](docs/validation/README.md). Run `bash scripts/verify.sh` for local
formatting, tests, and Clippy after fetching Cargo dependencies.
