# Synkro build performance comparison

This experiment compares normal Linux artifact storage with the initial SL7
writable mount, using a committed snapshot of `synkro-rs`:
`ec6d4c53b8fdf4321de220a922f7efce67788d40` (version 0.3.10).
It measures the full Rust workspace, not a synthetic crate or Criterion runtime
benchmark. The original checkout and its build artifacts are untouched.

## Results — 2026-10-23

SL7 reduced retained artifact storage by about **74% in debug and 72% in release**,
but this implementation substantially increased debug build latency. All eight
cases and 64 Cargo commands passed. The machine-readable
[measurements](synkro-build-performance.json) include every sample, storage
snapshot, toolchain identity, source revision, and SL7 executable hash.

Times below are **median seconds (minimum–maximum)**. Each backend has two clean
samples and six edit/unchanged samples per profile. Multipliers divide the two
medians; they are observations from this run, not statistical confidence bounds.

| Profile / operation | Native seconds | SL7 seconds | SL7 / native median |
| --- | ---: | ---: | ---: |
| Debug clean | 206.0 (202.0–210.1) | 476.9 (460.3–493.6) | 2.31× |
| Debug one-line rebuild | 22.8 (15.0–27.0) | 69.9 (60.2–74.7) | 3.07× |
| Debug unchanged | 0.72 (0.20–3.03) | 0.51 (0.37–2.25) | 0.72× |
| Release clean | 231.4 (218.6–244.1) | 356.4 (310.9–401.8) | 1.54× |
| Release one-line rebuild | 45.7 (40.0–113.3) | 49.9 (45.5–58.9) | 1.09× |
| Release unchanged | 0.51 (0.18–1.16) | 0.43 (0.39–1.57) | 0.84× |

Debug slowdown is consistent across both rounds. Release measurements are noisier:
SL7 clean builds ranged from 5m11s to 6m42s, and native edit builds included
71-second and 113-second samples. The release edit median alone is therefore
weak evidence for a precise overhead estimate. Unchanged builds have overlapping
ranges; their lower SL7 medians do **not** establish a speedup.

The first unchanged build after restarting the SL7 mount took 2.60–3.37 seconds
in debug and 1.86–2.36 seconds in release. All Cargo artifacts were still fresh.
The native counterparts, without a remount, took 0.65–0.85 and 0.31–0.45 seconds.

| Storage after three edits, before GC | Native | SL7, including catalog and history | Reduction |
| --- | ---: | ---: | ---: |
| Debug | 6.80 GiB | 1.76 GiB | 74.1% |
| Release | 1.84 GiB | 0.51 GiB | 72.1% |

Both rounds round to the same storage values. These measurements combine
compression and deduplication; they do not isolate each technique's contribution
or measure additional savings from sharing multiple worktrees.

Independent comparison of all 32 native/SL7 command pairs found identical Cargo
artifact graphs, including packages, targets, profiles, features, and freshness
flags. The harness checked the executable's expected CLI description after clean
builds and every edit. All four SL7 integrity checks passed, with no sealing
errors. Integrity checks took 84–86 seconds for debug and 21–24 seconds for
release, excluded from build times. Final normal unmounts took 0.013–0.029 seconds.
The container peaked at about 8.30 GiB of memory under its 12 GiB limit and
recorded no memory-limit or OOM events.

The current prototype demonstrates useful storage savings, but its debug rebuild
cost is too high to recommend as a transparent everyday build directory without
performance work. This is a result for the implementation tested, not an
inherent cost of content deduplication.

## Method

Both backends run serially in the same OrbStack Linux ARM64 container, using
Rust 1.97.1, an 8-CPU quota, eight Cargo jobs, and a 12 GiB memory limit with no
container swap allowance. Source and artifact paths are identical between cases.
All input files reside in Linux storage; there is no macOS source bind mount.
The native baseline uses the container's normal filesystem; SL7 stores its native
tree, SQLite catalog, and packs on that same underlying filesystem.

Each case starts with empty Cargo output directories. SL7 also starts with an
empty store, so clean builds receive no benefit from earlier deduplicated builds.
Dependencies are fetched before measurement. OS page caches are not flushed:
"clean" means clean compiler artifacts, not a cold machine. Compiler wrappers
and custom Rust flags are unset. Workspace profiles and default features are
unchanged, including optimized dev dependencies and embedded PostgreSQL.

The measured commands are:

```sh
cargo build --workspace --locked --offline --message-format=json
cargo build --workspace --locked --offline --message-format=json --release
```

`CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` both point into the backend under
test. `SYNKRO_BUILD_SHA` is pinned to the snapshot, so Git metadata cannot cause
incidental rebuilds. Release here means ordinary `cargo build --release`, not the
project's separate release-symbols/deployment pipeline.

For each backend/profile case, the harness measures one clean build, three
unchanged builds, three successive one-line edits to the CLI description, and
one unchanged build after an SL7 remount. The native counterpart of that last
measurement remains an ordinary unchanged build. Cargo JSON verifies that
unchanged builds are entirely fresh and edited builds recompile. After clean and
edited builds, the produced `synkro --help` must show the expected description.
No daemon installation or application-service startup is performed.

There are two rounds. Round 1 runs native then SL7, debug then release. Round 2
reverses both orders. Thus each table row has two clean-build samples or six
unchanged/edit samples per backend. Small sample sizes and shared-machine load
do not establish statistical significance; individual samples and ranges matter.

## Timing boundaries and storage

Build wall time starts immediately before spawning Cargo and ends when Cargo
exits. Downloading, mounting, binary validation, integrity checks, storage scans,
and teardown are outside that interval. A separately recorded idle observation
allows asynchronous SL7 work to settle between builds; it includes a minimum
approximately 0.3-second observation period and is not a durability guarantee.
The final normal unmount drains remaining operations and must exit cleanly;
its duration is recorded separately. Daemon CPU includes build, idle observation,
and executable validation, while Cargo CPU covers Cargo and compiler children.

Every SL7 case runs a full integrity check after unmount. Storage is measured
after all three edits, **before GC**. Native accounting sums unique regular
inodes' allocated blocks. SL7 accounting includes native allocated blocks plus
packs and catalog/WAL/SHM/temporary bytes, including uncollected historical data.
Neither includes native directory/inode/xattr overhead or host physical extent
accounting. Each case's entire disposable output/store is removed afterward.

## Reproduce

With the existing `sl7-mount-test:local` image (built by `scripts/docker-test.sh`):

```sh
bash scripts/benchmark-synkro.sh /path/to/synkro-rs test-results/synkro-benchmark
```

The script snapshots committed `HEAD`, excluding uncommitted files, and uses a
disposable container. It copies JSON measurements and build logs out before
removing that container. `SL7_BENCH_ROUNDS` and `SL7_BENCH_JOBS` configure repeats
and the CPU/job budget; the defaults are 2 and 8. `SL7_DOCKER_CONTEXT` defaults to
`orbstack`. These measurements qualify this Linux prototype, not native macOS
build performance.

## Interpreting overhead

This measures the initial implementation, which intentionally serializes its
filesystem callbacks. In [the writable adapter](../../src/staging.rs), the shared
state mutex covers reads, writes, hydration, and last-close sealing. In
[the live store](../../src/live.rs), sealing hashes, compresses, publishes packs,
commits metadata, and retires native bytes while that mutex remains held.
Other compiler processes can wait behind that work.

Across the two clean debug runs, mean total measured CPU time was approximately
1,211 CPU-seconds native and 1,273 CPU-seconds with SL7, including its daemon:
about 5% more CPU time despite 131% more wall time. This is consistent with
reduced parallelism and waiting, but does not attribute time to a particular
lock, syscall, or storage operation. CPU accounting boundaries are described
above and are slightly broader for the SL7 daemon.

There are also avoidable paths worth investigating: writable opens with
`O_TRUNC` currently hydrate the old file before truncating it, and reconstruction
has no decoded chunk cache. These are source-level observations and optimization
candidates, not a profiler's attribution of the measured time. The experiment
does not establish an inherent performance limit for deduplicating storage.

The next useful experiment is callback/wait-time profiling, then a measured
change to serialization or unnecessary hydration. The benchmark harness should
be rerun against each change with the same source snapshot and profiles.
