# State mutex experiment

Run on Linux with FUSE in a disposable environment.

This applies `parking_lot 0.13.0` to **only** `StagingFs::state` and the condition
variables that wait on it. Inode/identity DashMaps, the chunk cache, SQLite,
reader-pool mutex and catalog-writer mutex remain unchanged. Use an isolated
copy: `apply.py` deliberately modifies its destination and rejects reapplication.

```sh
# Start with the SL7 revision containing the directory snapshot optimization.
mkdir /tmp/sl7-parking
# Copy source without .git, build outputs, private evidence, or credentials.
tar cf - Cargo.toml Cargo.lock src tests scripts benches | tar xf - -C /tmp/sl7-parking
python3 experiments/parking-lot/apply.py /tmp/sl7-parking
cd /tmp/sl7-parking
CARGO_TARGET_DIR=/tmp/sl7-parking-target CARGO_PROFILE_RELEASE_DEBUG=1 \
  cargo build --locked --release --features fuse,code-profile
cargo test --locked --features fuse,code-profile
cargo clippy --locked --all-targets --features fuse,code-profile -- -D warnings
```

The experiment requires **Rust 1.95+** because parking_lot 0.13.0 does; it does
not change production SL7's declared Rust 1.85 minimum. The comparison used
Rust 1.97.1. Production already uses parking_lot 0.12.5 indirectly through fuser;
that version is a separate implementation and is not this experiment.

`state_sync.rs` wraps parking_lot with a poison flag so a panic during a partial
State mutation continues to fail closed. It also checks poisoning after a
condition-variable wake. Dedicated tests cover both paths. This measures the
usable, contract-preserving replacement, including that small adapter cost.
The default unfair unlock policy is retained; FairMutex and explicit fair
unlocking are separate experiments, not silently included here.

Build each source variant with its **own Cargo target directory**. Reusing a
target directory across extracted source copies can reuse a stale local-crate
artifact when timestamps precede the previous build. Record and compare binary
hashes before running any benchmark. Use the same compiler, release debug
information, features, jobs, descriptor limit and storage settings in each arm.
`code-profile` is compiled into each binary but recording is disabled during
end-to-end measurements.

See the [directory snapshot comparison](../../docs/validation/opendir-parking-lot.md)
for measurements and whether this experiment justified adoption. No compiler sources, raw sampled
memory, or private workload artifacts belong in this directory.

References: [Mutex behavior](https://docs.rs/parking_lot/0.13.0/parking_lot/type.Mutex.html),
[crate metadata](https://docs.rs/crate/parking_lot/0.13.0/source/Cargo.toml).

The counterbalanced build matrix can be reproduced with:

```sh
bash experiments/parking-lot/run-builds.sh \
  /bench/source /bench/bin/baseline /bench/bin/directories /bench/bin/parking \
  /bench/results/comparison SOURCE_REVISION
```

The private source checkout must match the Synkro fixture expected by
`scripts/benchmark-synkro.py`. The wrapper uses the same settings as this report;
replace `SOURCE_REVISION` with that checkout's revision. Each binary must first
pass Linux unit/Clippy checks and `scripts/container-test.py` with `SL7_BINARY`
pointing to it. Code-level captures use `scripts/walltime/collect.py --code-profile`
and the matching binary, as documented in `scripts/walltime/CODE_PROFILE.md`.
