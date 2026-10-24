# Native-I/O feasibility: mapping lifetime

An isolated FUSE passthrough experiment on Linux 6.12.58 with an ext4 backing
volume passed four mapping-lifetime cases. This establishes a useful constraint
for a future native-I/O path; SL7 does not enable passthrough in production.

| Mapping | FUSE release before munmap | Lease blocked while mapped | Lease granted after munmap | Reopen and visibility |
| --- | --- | --- | --- | --- |
| Read-only | yes | yes | yes | passed |
| Private writable | yes | yes | yes | passed |
| Shared writable | yes | yes | yes | passed |
| Child-held shared mapping after parent unmaps | yes | yes | yes | passed |

The experiment received eight opens and eight releases, registered and closed
eight backing IDs, and exited after a normal unmount. In the child case, the
parent had already unmapped before the child wrote through its surviving mapping.
The native-file lease still blocked, and the child’s writes remained visible
through a new FUSE open. Private mapping writes remained private.

A separate native-only control tested three mapping modes on the same kernel.
All three blocked the lease on ext4. All three incorrectly allowed the lease on
overlayfs despite a surviving mapping. These observations rule out an
unconditional lease-based retirement guard across backing filesystems.

The kernel supports bypassing the userspace daemon for selected operations using
registered backing files; capability negotiation and privileges are required.
[Kernel passthrough documentation](https://docs.kernel.org/filesystems/fuse/fuse-passthrough.html).
The tested kernel’s mapping helper replaces the VMA’s file with the lower backing
file. That is consistent with the observed early FUSE releases.
[Linux 6.12 mapping implementation](https://github.com/torvalds/linux/blob/v6.12/fs/backing-file.c#L306).

Any SL7 integration still needs a coherent metadata and mutation protocol for
writes that bypass the daemon, an inode gate against new opens during publication,
and a way to retry sealing after an unobserved final unmap. Existing logical
metadata overrides make blindly enabling passthrough after hydration incorrect.
Crash and remount behavior also matters: a mapping can outlive the daemon, so a
later mount must not revert to relying solely on FUSE handle counts. Compatibility
with older readers must account for that requirement.

This probe did not test SL7 retirement, crash recovery, permission transitions,
all forms of cross-process descriptor transfer, or build performance. It supports
a qualified experiment on the tested ext4 setup, not a general safety claim.
No Cargo speedup is claimed.

[Reproducible experiment](../../experiments/passthrough-lifetime/README.md) ·
[Results and source hashes](passthrough-lifetime.json).
