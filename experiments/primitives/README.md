# SL7 primitive comparisons

Run on an otherwise idle Linux machine with Rust 1.97 or newer:

```sh
cargo bench --manifest-path experiments/primitives/Cargo.toml --locked --bench primitives -- memory
cargo bench --manifest-path experiments/primitives/Cargo.toml --locked --bench primitives -- catalog
```

The harness emits JSON lines. It uses three rounds, rotating candidate order,
fixed operation counts, and a start barrier. Setup is outside the measured region.
This is a custom benchmark, not Criterion; medians are descriptive, with no
statistical-significance claim. Versions are pinned in the separate benchmark
lockfile; production FUSE independently depends on DashMap for its inode indexes.
The other alternative implementations remain benchmark-only. Turso's default features
are disabled: its default `mimalloc` feature installs a process-wide allocator,
which would otherwise also change every map/cache and SQLite measurement.
All candidates here use Rust's system allocator, matching production SL7.

- **Indexes:** standard `Mutex<HashMap>`, `RwLock<HashMap>`, DashMap and SCC.
  All use Rust's `RandomState`, integer inode-like keys and owned `Arc` values.
  Uniform reads, 90% hot-key reads, and reads with an additional replacement on every tenth operation run
  at 1/2/4/8 threads. This isolates index operations; it does not implement
  SL7's multi-index publication and inode-lease protocol.
- **Caches:** the production LRU is compiled directly from `src/read_cache.rs`;
  alternatives are 16 independent LRU shards, DashMap with 16 bounded FIFO
  admission queues, and Moka's weighted cache. Keys are 32-byte digests and
  values are distinct `Arc<Vec<u8>>` allocations of 8 KiB. Uniform/hot reads
  and a working set twice the 128 MiB payload budget exercise eviction.
  All data is preallocated; no disk IO, decoding or payload allocation is timed.
  Hit counts are reported. FIFO, shard-local LRU and Moka's policy are deliberately
  different from global LRU. Moka's budget is checked after maintenance; this
  is not a proof of the strict instantaneous bound that SL7 currently provides.
- **Catalogs:** bundled SQLite through SL7's rusqlite version, embedded Turso
  in WAL mode, and Turso MVCC with passive checkpointing. All begin with the
  identical SQLite-format seed catalog, SL7's actual schema, 16,384 chunks,
  one pack and one extent per chunk. Every connection enables and checks
  `foreign_keys=ON` and `synchronous=FULL`. Persistent prepared statements
  perform mixed present/absent existence lookups, repeated present lookups,
  and 32-extent range joins. Reads consume rows and the first integer column;
  they do not construct full SL7 extent objects. Writers publish 64 chunk
  references per transaction, use automatically assigned file IDs, and race
  either distinct or shared new chunk hashes. Each writer has an independent
  connection; SQLite serializes via `BEGIN IMMEDIATE`, Turso MVCC uses
  `BEGIN CONCURRENT`. Conflicts retry the whole transaction, with bounded
  backoff; retries and p99 transaction latency are reported. Both adapters
  receive the same owned parameter representation; Turso additionally converts
  those values to its public API types. Pack writes/fsyncs and the FUSE layer
  are outside this catalog experiment. CHECK/FK failures, rollback, final
  file/chunk/extent counts are checked. These are functional checks, not
  power-loss or process-crash durability tests.

A primitive speedup is not an end-to-end Cargo speedup. Changes to eviction,
transaction semantics or inode coordination must pass SL7's correctness suite
before an integrated build result can justify adoption.

The optional `sharded-read-cache.patch` applies to SL7 at `df96636`. It keeps
production chunk verification and LRU code, divides all four cache budgets
among 16 shards, and uses an atomic requested-byte counter. Cached pack handles
can be duplicated across shards, but their total limit stays 64. Per-shard
capacity cannot be borrowed, so hot or uneven workloads can evict earlier.
This is an integrated candidate for measurement, **not an adopted default**.

```sh
git apply experiments/primitives/sharded-read-cache.patch
cargo test --locked --features fuse
# Run existing scripts/mounted-test.py, staging-test.py and cache-test.py
# with SL7_BINARY pointing to the candidate, then benchmark-synkro.py.
```

Validate completeness, equal work and checksums, then summarize:

```sh
python3 experiments/primitives/summarize.py memory.jsonl catalog.jsonl summary.json
```

`report_builds.py` validates the matched full-build results and compares Cargo's
artifact graphs locally before exporting only timings, counters and aggregate
validation. Raw graphs and private source stay outside the public report.
Catalog measurements should set `TMPDIR` to the same local filesystem as the
SL7 store; do not mix container overlayfs and the backing volume. Run the
`settings` example to record effective engine cache/page defaults.

The pinned Turso 0.8.3 SDK exposes its core write/write conflict as the generic
`Error::Error("Write-write conflict")`; the harness retries that exact error as
well as Busy/BusySnapshot. Other errors fail the sample rather than being hidden
by retry loops. The source seed and setup are excluded from timing. All writer
operations, whole-transaction retries and backoff are timed.

## Integrated DashMap comparison

The focused production change in `b5ab793` replaces the inode and identity
indexes and serves eligible metadata requests without the global state mutex.
It does not apply `sharded-read-cache.patch` or change eviction policy.

Build the baseline from `85f3358` and the candidate from `b5ab793` with
`cargo build --locked --release --features fuse`, saving each binary separately.
Then, in a disposable Linux FUSE environment with dependencies already fetched:

```sh
ulimit -Sn 524288
SL7_BINARY=/bench/bin/sl7-dashmap TMPDIR=/bench/tmp \
python3 scripts/benchmark-synkro.py \
  --source /bench/source --results /bench/results/dashmap-builds \
  --revision ec6d4c53b8fdf4321de220a922f7efce67788d40 \
  --rounds 2 --jobs 8 --profiles debug --backends baseline-sl7 sl7 \
  --baseline-binary /bench/bin/sl7-baseline \
  --hash-workers baseline-sl7=2 --hash-workers sl7=2 \
  --import-pipeline baseline-sl7=0 --import-pipeline sl7=0 \
  --compress-workers baseline-sl7=0 --compress-workers sl7=0 \
  --reuse-prepared baseline-sl7=0 --reuse-prepared sl7=0 \
  --settle-mode quiescent
```

The workload source is private and not distributed with this repository.
`report_dashmap.py RESULTS OUTPUT_PREFIX --provenance PROVENANCE.json` runs the
same graph/integrity/budget gates as `report_builds.py`, additionally requires
concurrent-path hits in every candidate mount, and emits Markdown/HTML/JSON.
Provenance must include `implementation_commit` and `candidate_binary_sha256`;
the latter must match the benchmark. Other supplied provenance fields are copied
to the public report and must contain no private workload data.
