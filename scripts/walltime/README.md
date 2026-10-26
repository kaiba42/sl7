# Clean-build wall-time measurement

This harness collects native-storage and SL7 Cargo builds with matching clean
output directories, an unchanged SL7 binary, eight Cargo jobs, incremental
compilation enabled by the workload, and offline locked dependencies. It changes
no production SL7 code. Raw outputs contain private workload identities; keep
them under ignored `test-results/`, and publish only reviewed aggregates.

## Collection

Use a disposable privileged Linux container with `/dev/fuse`, ext4-backed scratch
space, 8 CPU quota, 12 GiB memory, Rust 1.97.1, `cc`, Python 3, fuse3, perf and
bpftrace. Prefetch the workload's dependencies before collection. Copy this
repository's `scripts/` directory into `/sl7/scripts`, the fixed workload into
`/bench/source`, and the release SL7 binary into `/bench/bin/sl7`. Set `TMPDIR` to
scratch storage. The harness reuses `benchmark-synkro.py` and `mount_helpers.py`;
its executable validation and environment defaults target the Synkro workload.

```sh
export TMPDIR=/bench/tmp
mkdir -p "$TMPDIR"
ulimit -Sn 524288
python3 /sl7/scripts/walltime/collect.py --results /bench/results/clean-builds
```

Run a tiny Rust executable named `walltime_probe` that prints `ok` first, with
`--source /path/to/fixture --fixture`, to validate tracing and mount lifecycle.
The measured sequence is native control, SL7 profile, native profile, SL7 control.
Controls retain the same invocation wrapper and Cargo timings but omit BPF/perf.
Do not run other benchmark or analysis jobs in the container during collection.

The recorded environment used kernel 6.12.58, perf 6.1.190, and official
[bpftrace v0.27.1](https://github.com/bpftrace/bpftrace/releases/tag/v0.27.1).
Its `bpftrace-x86_64` AppImage SHA-256 was
`c8d570302725bd1a3ed2f23bfa8d83dd78bde95daa98856279343052db5799c9`.
Extract the AppImage and invoke `squashfs-root/AppRun`; its extraction does not
require mounting another FUSE filesystem. Older bpftrace versions may lack the
raw tracepoint and explicit namespace/clock syntax used here.

## Analysis

After all timed builds finish, symbolize CPU samples while the toolchain and
binary are still available in the container:

```sh
for label in sl7-profile native-profile; do
  perf script --no-inline --ns -i /bench/results/clean-builds/$label/cpu.data \
    -F comm,pid,tid,time,event,ip,sym,dso \
    > /bench/results/clean-builds/$label/cpu.txt
done
python3 /sl7/scripts/walltime/cpu.py /bench/results/clean-builds /bench/cpu-summary.json
```

Download and checksum the raw results, then run analysis on a machine with enough
memory for several million scheduler records:

```sh
python3 scripts/walltime/analyze.py test-results/walltime/clean-builds \
  test-results/walltime/analysis.json
python3 -m unittest discover -s scripts/walltime -p 'test_*.py'
```

`analyze.py` writes an aggregate report and a **private** unit-identity sidecar.
Aggregate unit IDs are hashes, not a guarantee against identifying a known public
dependency. Review outputs before publication. Raw perf data can include sampled
stack memory as well as filenames and symbols; it must not be published.

## What the measurements mean

- CPU and wait totals are **thread-seconds**, which overlap. Do not add them to
  explain elapsed time. CPU is cross-checked against process resource totals.
- Exclusive wall occupancy records whether any selected thread runs, is runnable,
  or is sleeping. It does not expose all internal compiler dependencies and is
  not a measurement of removable delay.
- Cargo's observed last-unblocking chain respects metadata pipelining. Its gaps
  can contain necessary parallel work. It explains the observed schedule, not
  the theoretical shortest possible build. Cargo timing precision is 10 ms.
- A FUSE request lifetime includes queueing and response work. Direct synchronous
  sleeps are identified separately by `request_wait_answer`; not every possible
  FUSE/page-cache wait goes through that function.
- CPU sampling is userspace-only, at requested 49 Hz with a bounded 4096-byte
  DWARF stack. Analysis excludes samples after Cargo exits. SL7's build CPU and
  post-build settling CPU are also recorded separately through `/proc`.
- cgroup throttling time and runqueue wait overlap; neither is an additive build
  penalty. A shared-node, single-run profile does not establish a speedup.

## Trace correctness details

All clocks explicitly use `CLOCK_MONOTONIC`. BPF uses initial-namespace PIDs/TIDs;
the invocation wrapper's task-name marker maps these to container PIDs.
On the measured kernel the formatted FUSE send tracepoint reports request ID
zero, before assignment. Therefore raw send/end request pointers are paired;
the formatted send supplies the opcode. Per-thread callback ordering and maximum
callback separation are checked, including safe request-address reuse. The last
two fields in `D` records are reserved zeroes, **not measured errno fields**.

Collection fails on reported trace loss, missing request completions, dirty Cargo
artifacts, changed artifact graphs, failed executable checks or store integrity.
Analysis fails on impossible scheduler transitions or material CPU-accounting
disagreement. Wake-before-switch races remain explicitly unresolved. A full
request pairing check is stronger than merely observing an empty stderr, but
these checks cannot prove the absence of every possible tracing defect.

`collector_hashes` records the actual frozen collection files. Later analysis and
test edits have separate provenance; do not claim those edits ran during capture.
Delete only the owned measurement container after preserving validated results.
