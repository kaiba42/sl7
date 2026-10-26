# Code-level request and kernel profiling

Build the same source with the opt-in `code-profile` feature and debug line
information. Normal builds compile the scope guards to no-ops; they do not open
trace files. Enabling the feature alone also leaves recording disabled until
`SL7_CODE_PROFILE_DIR` names an existing directory.

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 cargo build --release --locked --features fuse,code-profile
python3 scripts/walltime/collect.py \
  --binary /bench/bin/sl7-code-profile --results /bench/results/code-clean \
  --cases sl7-control sl7-profile --code-profile
```

Use the disposable Linux/FUSE environment described in [README.md](README.md).
The two arms use the same instrumentable binary. The control has recording and
sampling disabled; the profile enables request scopes and records daemon-only
`cpu-clock:u` and `cpu-clock:k` samples at requested 99 Hz, with 16 KiB DWARF
stacks. Kernel symbol addresses must be readable by perf. This workflow does not
change host kernel security settings. Validate on a small mounted fixture first.

After timed workloads finish:

```sh
perf script --no-inline --ns \
  -i /bench/results/code-clean/sl7-profile/cpu.data \
  -F comm,pid,tid,time,event,ip,sym,dso \
  > /bench/results/code-clean/sl7-profile/cpu.txt
python3 scripts/walltime/code_analysis.py /bench/results/code-clean /bench/code-analysis.json
python3 -m unittest discover -s scripts/walltime -p 'test_*.py'
```

## Scope and overhead

`READ`, `WRITE`, and `OPENDIR` roots begin at the SL7 callback and end after sending
the reply. They exclude time waiting in the kernel/FUSE request queue before
dispatch. All child phases are **exclusive**: time spent in a nested phase is
subtracted from its parent. Each request's phase sum must equal its root duration
exactly. Totals across different requests overlap across worker threads and must
not be added to Cargo's elapsed build time.

State acquisition, inode gates, catalog acquisition, native payload I/O,
reconstruction, cleanup, replies, and directory enumeration/lookup/attribute work
have separate scopes. The directory snapshot implementation replaces per-child
lookup/attribute spans with `directory_child_pin` (native I/O outside State) and
`directory_register` (bounded registration batches under State). The analyzer
also reports total, p99 and maximum OPENDIR State-ownership interval durations.
`native_payload` on reads includes buffer allocation.
`directory_child_attr` includes insertion into the collected entry vector.
Lock acquisition includes contention and scheduling delay; condition-variable
waits include reacquiring the mutex. `state_locked_work` excludes nested wait
phases, but can include small bookkeeping gaps around unlock/relock paths.

Per-worker buffered files avoid a shared profiler mutex. Formatting and buffer
writes happen after each root's measured end, so they are **profiling overhead**,
not a recorded handler phase. Clock reads, thread-local access and nested scope
bookkeeping can still inflate recorded phases. CPU samples can identify profiler
work. A single enabled/disabled pair on a shared machine cannot precisely isolate
instrumentation overhead or establish a performance regression.

Analysis accepts only requests entirely inside the Cargo interval. CPU sampling
also excludes post-build settling by timestamp. Trace records made during
executable checks, inventory, and unmount remain in the private raw evidence but
are excluded from published build-phase totals.

## Trace format

`src/staging/profile.rs` writes one TSV file per daemon worker, named by PID/TID.
Each line is a completed root request:

```
OPERATION  monotonic_start_ns  elapsed_ns  phase:exclusive_ns:calls ...
```

Fields prefixed with `@` instead describe selected inclusive intervals:
`@phase:offset_from_root_ns:duration_ns`. They are **not** added to the exclusive
phase sum. These intervals let analysis intersect read/write mutex waits with
`OPENDIR` ownership of that exact mutex. For directory opens, the
`state_locked_work` interval corresponds to the `with_read` lock-held callback;
the more general mutation loop can temporarily release the mutex and is not
treated as one uninterrupted ownership interval.

Phase names and thread IDs contain no workload paths. Raw perf files still
contain stack memory and filenames, and Cargo logs contain private workload
identities. Keep raw captures ignored and publish only reviewed aggregates.
