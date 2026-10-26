# Native macOS mount implementation

Status: implementation in progress. **Writable macOS mounts are not enabled.**
The first patch adds and tests backing-inode primitives on macOS and routes the
Linux adapter through the same interface. A passing `fuse-check` build is not a
successful mount, a native Cargo qualification, or a macOS performance result.

## Target architecture

```text
macOS Cargo / rustc / linker / build scripts
                  |
         macFUSE (FSKit or kernel backend)
                  |
            fuser request adapter
                  |
       SL7 inode / handle / seal state
          /                    \
  native backing operations     chunk store + SQLite catalog
          |                            |
  private APFS tree             immutable packs on APFS
```

Keep the storage format, deduplication engine, and shared-worktree model. One
daemon owns one store; each worktree gets separate target/build directory prefixes
inside its mount. macOS tools and produced executables stay on macOS.

Reuse `fuser` through macFUSE's libfuse bridge before considering a separate Apple
FSKit app-extension frontend. The pinned fuser 0.18.0 selects libfuse2 on macOS;
`fuse-check` deliberately disables mounting. Validate the installed bridge and
mount options with a real mount before treating them as supported configuration.

## First implemented slice

`src/native.rs` owns inode xattrs and directory-relative atomic rename. Linux's
live identity and writable rename paths call it now.

| Operation | Linux | macOS |
|---|---|---|
| Read/create identity xattr | Follow the held O_PATH fd's procfs magic link | `fgetxattr` / `fsetxattr` on the held fd |
| Missing xattr | `ENODATA` | `ENOATTR` |
| Atomic create-only identity | `XATTR_CREATE` | `XATTR_CREATE` |
| Rename relative to parent pins | `renameat2` | `renameatx_np` |
| No-replace / exchange flags | FUSE/Linux flags | Translate to `RENAME_EXCL` / `RENAME_SWAP` |

Identity remains `user.sl7.file.v1`, with the same value encoding and caller-owned
sync ordering. Attribute errors other than missing data propagate. Linux
O_PATH descriptors cannot simply switch to `fgetxattr`; their procfs path remains
necessary. Neither backend resolves a mutable user pathname for these xattrs.

Rename uses single-component names and held parent descriptors. It rejects dot,
dot-dot, empty and slash-containing names. Linux retains its native flags;
Darwin rejects unsupported flags rather than silently degrading their semantics.
Tests of the Darwin rename primitive do not establish that macFUSE/fuser delivers
every extended rename request; protocol coverage remains a separate gate.

Eight tests exercise identity persistence through rename, pathname replacement,
hard links and final unlink; attachment after unlink; create-only races; short
buffer errors; replacement/no-replace/exchange; invalid names/flags; renamed
parent directories; and cross-directory dangling-symlink rename. Linux runs these
with O_PATH pins; macOS runs with ordinary open descriptors.

Run the native primitives without installing macFUSE:

```sh
cargo test --locked --features fuse-check native::
```

## Remaining implementation gates

1. **Pinned inode and namespace access.** Replace the adapter's procfs-derived
   paths with operations on parent/inode descriptors: open, lookup, directory
   enumeration, readlink, link, unlink, mkdir, metadata and timestamps. Preserve
   the private-root boundary, symlink identity, rename races, and lifetime through
   final unlink. Do not substitute `F_GETPATH` or `/dev/fd` without proving the
   required semantics. Darwin's `O_SYMLINK`/`O_EVTONLY` candidates need separate
   tests; ordinary readable test descriptors are not a complete pin design.
2. **Native I/O handles and publication.** Linux reopens a pinned inode via
   procfs for independent read/write access. Establish a Darwin handle model that
   still works after unlink and chmod, without requiring write permission for
   lookup. A duplicated fd shares its file offset and cannot be used as an
   independent streaming cursor. Use positional I/O or an explicitly owned
   stream where needed. Preserve the sealer's reader/mutation/publication gates.
3. **APFS transitions and recovery.** Exercise seal, hydrate, truncate, replay,
   timestamps, xattrs, fsync and directory synchronization. Retirement currently
   truncates to zero and restores logical length; measure its sparse allocation
   and recovery behavior on APFS. Review `F_FULLFSYNC` and SQLite synchronization
   against the documented durability contract before claiming power-loss safety.
4. **Writable adapter compilation and mount integration.** Enable the macOS cfgs
   only after the dependencies above are implemented. Map stat attributes,
   timestamp fields, access flags and mount capabilities. Match case sensitivity
   and Unicode name behavior to the backing volume. Keep `fuse-check` unable to
   mount; provide an explicit backend selection with no silent backend fallback.
5. **Native mounted qualification.** Run staging, cache, live, recovery and
   self-host suites on macOS. Check executable build scripts/proc macros/binaries,
   mmap coherence, truncate and rename over mapped files, local advisory locks,
   concurrent Cargo processes, incremental=true, and two worktrees whose build
   and clean operations remain isolated. Validate remount and forced daemon-death
   recovery independently of graceful shutdown.
6. **Performance and onboarding.** Compare native Cargo/APFS against the qualified
   native SL7 backend using identical compiler inputs, fresh stores and reversed
   run order. Record build wall time separately from background sealing, disk
   allocation and daemon memory. Publish macOS-specific results and short setup,
   mount, unmount and recovery commands. Linux timing is not a macOS estimate.

## Backend qualification

Qualify FSKit first because it avoids a kernel extension. Keep the kernel backend
as a separate candidate rather than assuming FSKit preserves all existing SL7
semantics. No automatic extension installation or host-security changes belong
in the build/test scripts.

As of macFUSE 5.5.0, FSKit has no per-file fsync callback, and the macFUSE backend
documentation lists missing notification support and mount-option limitations.
The release also documents short-read/size-change workarounds. These directly
affect publication, cache coherence and durability; successful compilation or a
hello-world mount cannot resolve them. On macOS 27, close errors cannot be
reported through the FSKit close callback either. Close/flush is not assumed to
be a substitute for fsync.

Before enabling a backend, trace the operations actually delivered during
`fsync`, `msync`, close and unmount, inject failures, and verify what the caller
observes. If FSKit cannot satisfy the required contract, leave it unqualified and
evaluate the kernel backend explicitly. Do not silently weaken the store's
durability promises to make a backend pass.

Sources checked 2026-10-26:

- [macFUSE backend capabilities](https://github.com/macfuse/macfuse/wiki/FUSE-Backends)
- [macFUSE 5.5.0 release notes](https://macfuse.github.io/2026/10/17/macfuse-5.5.0.html)
- [Apple descriptor xattr API](https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/fgetxattr.2.html)
- [Apple atomic rename semantics](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/man/man2/rename.2)
