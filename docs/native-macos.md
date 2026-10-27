# Native macOS mount implementation

Status: implementation in progress. **Writable macOS mounts are not enabled.**
The first two slices add backing-inode primitives, route the Linux adapter
through them, and prototype the Darwin pin; Darwin pinning is not yet general.
A passing `fuse-check` build is not a successful mount, a native Cargo
qualification, or a macOS performance result.

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

## Second slice: Linux descriptor-relative lookup; Darwin pin prototype

`native::pin_path` pins the trusted backing root; `native::pin_at` pins one
validated child relative to a held directory descriptor; `native::pin_parent`
pins a held directory's current parent. Each returns the pin and its `fstat`
metadata, and rejects inode types other than directory, regular file and
symlink with `EOPNOTSUPP`. Dot and root dot-dot stay adapter policy in
`State::lookup` and `lookup_indexed`; the primitive rejects them as child names.

**Linux (complete for this slice).** Staging's root pin, `State::lookup`, the
post-create pins of mkdir/create/symlink, the indexed lookup fast path and the
directory-snapshot child/parent pins use these primitives; none joins names
onto a procfs path any more. O_PATH pins need and grant no content access.

**Darwin (qualified prototype; acceptance invariant 4 is not met).** Darwin has
no `O_PATH`. Every unprivileged open is authorized against the caller's
*effective* access (mode bits for its class, ACL entries, file flags; root
bypasses most checks), and must request read, write or execute. A pin never
requests write, so an inode the daemon can neither read nor execute cannot be
pinned: with ordinary mode bits that includes owned mode-000 and write-only
files, and mode-000 symlinks. Measured on macOS 26.0, APFS:

| Pin | Linux | Darwin prototype |
|---|---|---|
| Flags | `O_PATH \| O_NOFOLLOW \| O_CLOEXEC` | `O_EVTONLY \| O_SYMLINK \| O_NONBLOCK \| O_NOCTTY \| O_CLOEXEC` |
| Content access | None | Read: `O_EVTONLY` is not a no-access pin |
| Read denied | Pin unaffected | Retry `O_EXEC` (file) / `O_SEARCH` (dir): no read/write |
| No effective read or execute | Pinned | `EACCES` |
| Final symlink | Pinned, not followed | `O_SYMLINK` pins the link; `O_NOFOLLOW` would be `ELOOP` |
| FIFO / socket / device | O_PATH open, rejected after `fstat` | Rejected by `fstatat(AT_SYMLINK_NOFOLLOW)` before opening |

Execute/search fallback pins support `fstat` and `openat`, but
`fgetxattr`/`fsetxattr` on them fail with `EACCES`, as do Linux `user.*` xattrs
on an inode the daemon cannot read.

The Darwin special-file preflight is a private-tree check, not a race-free
guarantee. It shows only the name's type at that instant; an outside actor could
substitute a FIFO or device before `openat`. `O_NONBLOCK` stops a FIFO open from
blocking and `O_NOCTTY` stops a tty becoming the controlling terminal, but a
device open can still run driver code with side effects. Exclusive ownership of
the backing tree, which the adapter never populates with special files, is the
assumption that excludes this. The post-open `fstat` rejects whatever was opened
if it is unsupported.

Linux and portable tests cover inode identity through rename, replacement,
hard-link removal and final unlink; lookup rooted in a renamed/replaced held
directory and its moved parent; invalid child names and non-directory parents;
relative, absolute, directory, dangling and self-loop symlinks; FIFO/socket
rejection without blocking or leaking a descriptor; and the locked and indexed
staging lookups after the backing parent is renamed.
`pins_do_not_require_or_grant_content_access` is invariant 4: it runs on
Linux and is ignored on Darwin. Running it there is the executable probe:

```sh
cargo test --locked --features fuse-check \
  native::tests::pins_do_not_require_or_grant_content_access -- --ignored
# panicked: pin "none" mode 0: Permission denied (os error 13)
```

`darwin_pin_requires_effective_read_or_execute_and_read_pins_can_read`
characterizes the prototype, deriving each expectation from `faccessat` with
`AT_EACCESS` rather than from mode bits.

### Proposed Darwin resolution

A general unprivileged Darwin pin is not available. `openbyid_np` needs
privilege and cannot reach an unlinked inode; `F_GETPATH` or `/dev/fd` reopen
would follow the name, not the inode. The proposed design gives up on an
unrestricted *backing* mode, not on inode pinning:

1. **Logical permission bits.** Keep every backing file owner-readable and
   writable, and directories owner-rwx, without ACL deny entries. Record a
   user-visible mode that differs from the backing mode as logical inode
   metadata, the way sealed sizes and timestamps already override native ones.
   Enforce it in the FUSE layer (`default_permissions` or explicit access
   checks). Pins are then always `O_EVTONLY` read pins, and identity xattrs
   always work. This changes how native mode bits are interpreted and needs a
   catalog/format decision first.
2. **I/O handles after unlink.** A pin cannot be reopened with a different
   access mode, and `dup` shares the file offset. Use positional I/O on
   per-open descriptors obtained with `openat(parent, name)` while a name
   exists. For a write open of an inode that is already unlinked, a Linux-style
   procfs reopen has no Darwin equivalent. Either keep a lazily created
   read/write descriptor per inode from before the last unlink, or move a
   still-referenced inode's last name into a private orphan directory instead
   of unlinking it (NFS-style), hiding that link from reported `nlink` and
   removing it on the last forget. The orphan approach also serves retirement
   and background sealing, which currently reopen through procfs.

Run the native primitives without installing macFUSE:

```sh
cargo test --locked --features fuse-check native::
```

## Remaining implementation gates

1. **Pinned inode and namespace access.** Linux pins and child lookup are
   descriptor-relative. Darwin pins are a prototype until the restricted-mode
   design above is decided. Replace the remaining procfs-derived paths with
   operations on parent/inode descriptors: open, directory enumeration,
   readlink, link, unlink, mkdir, metadata and timestamps. Preserve the
   private-root boundary, symlink identity, rename races, and lifetime through
   final unlink. Do not substitute `F_GETPATH` or `/dev/fd` without proving the
   required semantics.
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
