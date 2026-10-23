# Initial implementation validation

This page preserves the original measurements. The subsequent
[optimization report](optimizations.md) covers the current implementation,
with [per-change validation](optimizations-validation.json) and fresh profiles.

The full Synkro workspace has a separate
[native-versus-SL7 build performance report](synkro-build-performance.md),
including clean builds, one-line rebuilds, and storage before GC.
The follow-up [profiling report](profiling.md) identifies CPU hotspots and
filesystem request delays in the unchanged baseline implementation.

The initial machine-readable report is [end-to-end.json](end-to-end.json).
Run `bash scripts/docker-test.sh` to reproduce all four mounted suites. Run
`bash scripts/verify.sh` for host formatting, compilation, tests, and Clippy.

Validation uses Rust 1.97.1, an optimized SL7 daemon, and Linux ARM64 in OrbStack
on macOS 26.0. The Docker image is built from `rust:1.97-bookworm` with the checked
Cargo lockfile. The Linux build runs 35 Rust tests and strict Clippy; the macOS
`fuse-check` build runs 28 tests and strict Clippy. Linux-specific writable mount
and transition tests are not compiled on macOS. Actual mounts run as container
root; native macOS and unprivileged mounting remain unqualified.

## What the end-to-end tests establish

- Read-only random/pread/mmap reconstruction is byte-exact. `rustc` consumes
  reconstructed metadata and links a reconstructed library.
- Native staging and writable deduplication preserve the tested handle, mapping,
  hard-link, rename, unlink, truncate, timestamp, executable, and lock behavior.
- Cargo builds, tests, checks, build scripts, and proc macros run through mounted
  target and intermediate directories. Debug/release executables produce the
  expected result after changes. Unchanged builds remain fresh after remount.
- Two source workspaces use separate output namespaces concurrently. Ten changes
  each modify only the fixture's `EDIT` constant; both profiles rebuild and the
  other worktree continues to run its original executable.
- SL7 builds and tests its own full dependency graph through the deduplicating
  mount. A CLI-description edit appears in rebuilt debug/release executables.
- Partial clean preserves the other profile/worktree. Full clean plus offline GC
  removes all artifact chunks and packs. Both workloads rebuild after clean and
  return to zero artifact payload after another clean/GC.
- Synced files survive real daemon termination and remount. Subprocess tests
  exercise interruption during pack publication, sealing, hydration, and GC.

Every snapshot performs integrity checking, offline collection, and storage
accounting. Therefore edit snapshots measure current live content after GC,
not the cost of retaining every historical output. Cargo's own retained
incremental generations are still part of the live namespace.

## Storage accounting

Measured on 2026-10-23, after integrity checking and offline GC:

| Stage | Logical regular-file bytes | Approximate retained storage |
| --- | ---: | ---: |
| SL7 self-build, debug + release + tests/check | 592.26 MiB | 151.20 MiB |
| SL7 after the CLI-description edit and rebuild | 592.26 MiB | 151.21 MiB |
| Fixture, two worktrees built | 52.98 MiB | 13.23 MiB |
| Fixture after ten one-line edits | 53.15 MiB | 13.36 MiB |
| Either workload after full clean + GC | 0 | 80 KiB catalog bookkeeping |

The self-build retained approximately 74.5% less than its logical bytes. Its
description edit increased accounted storage by 4,425 bytes. The ten-edit fixture
increased accounted storage by 137,111 bytes relative to the two-worktree baseline.
These are observations for these workloads, with GC between stages, not universal
deduplication ratios. Both final stores had zero native file allocation, zero
chunks, and zero referenced/orphan pack bytes. The live fixture executed 42 Cargo
commands and the self-host suite executed 16.

The report contains every stage's `live` and `gc` fields. `logical_bytes` counts
unique native regular inodes; hard-link aliases are not counted twice.
`native_allocated_bytes` is `st_blocks * 512`, including unsealed/small files.
`content.total_file_bytes` includes referenced/orphan packs, catalog/WAL/SHM,
and temporary files. Their sum approximates retained storage, excluding native
directory/inode/xattr overhead and host filesystem extent sharing. Logical length
of a sparse placeholder is not allocated storage.

Timings in the report are observations, not performance comparisons. The small
fixture uses Cargo's default debug/incremental settings; SL7's own dev/test
profiles disable debug information and incremental compilation. Neither workload
is a full run over the original 70 GiB corpus. Historical study estimates must not
be presented as measurements of this implementation.

## Container shutdown investigation

Without additional mount isolation, the first Cargo workload in a fresh OrbStack
container intermittently left the daemon running after `fusermount3 -u` returned
success. The mount disappeared from the container's mount table, while the FUSE
worker remained in `fuse_dev_do_read`. No Rust panic or pending SL7 callback was
observed. Direct `umount`, disabling incremental compilation, tmpfs backing,
another private mount namespace, waiting, and `sync -f` did not reliably fix it.

Creating a bind mount of the test parent and marking it unbindable **before**
mounting FUSE produced clean exits in eight fresh-container reproductions, then
passed the complete mounted lifecycle. The harness uses that isolation in
`scripts/mount_helpers.py`. It still requires ordinary unmount and exit status 0;
a timed-out daemon killed by cleanup fails the test. Explicit daemon-kill recovery
cases alone expect termination by signal.

This evidence is consistent with a recursive bind copy retaining the FUSE mount.
The owner of that reference was not conclusively identified, so this is not a
confirmed OrbStack or kernel defect. OrbStack documents its
[container filesystem exports](https://docs.orbstack.dev/features/native-files).
Linux [unbindable mount semantics](https://man7.org/linux/man-pages/man7/mount_namespaces.7.html)
prune those subtrees from recursive bind copies. The harness change prevents that
class of external reference without weakening SL7's shutdown assertion.

For a manual container mount, the same isolation can be prepared as root:

```sh
mkdir -p /mnt/sl7-private
mount --bind /mnt/sl7-private /mnt/sl7-private
mount --make-unbindable /mnt/sl7-private
mkdir /mnt/sl7-private/artifacts
# Mount SL7 at /mnt/sl7-private/artifacts.
# After stopping clients, unmount SL7 normally and wait for its daemon to exit.
umount /mnt/sl7-private
```

## Limits

These checks qualify the tested Linux build-tree operations and explicit
process-death boundaries. They do not certify power-loss recovery, arbitrary
POSIX permissions/ownership, hostile external writers, direct I/O, live GC,
production-scale latency, or native macOS behavior. In that initial version, sealing and reads serialized; current background packing
is described in the optimization report. GC remains offline. The store's native `tree/` is private implementation data and must
not be used as ordinary artifacts after files have been sealed.

Earlier read-only/staging reports and the single-real-artifact smoke report are
retained alongside the current report as historical evidence.
