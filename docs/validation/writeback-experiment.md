# Kernel writeback experiment

Kernel writeback remains disabled. An opt-in prototype was tested on Oaktown's
`middle` node with Linux 6.12.58 and Rust 1.97.1, but was not included in the
implementation or accepted for build-performance measurement.

The prototype negotiated `FUSE_WRITEBACK_CACHE` and opened backing descriptors
read/write for application write-only handles. Linux may issue a read to preserve
the untouched portion of a partially written page. It also drains dirty pages
before sending the FUSE fsync request. See the [kernel I/O documentation](https://docs.kernel.org/filesystems/fuse/fuse-io.html)
and [Linux 6.12 fsync implementation](https://github.com/torvalds/linux/blob/v6.12/fs/fuse/file.c).

Cold partial-page writes, concurrent appends, and writes through a mapping that
outlived its descriptor preserved the expected bytes. Final store integrity also
passed. However, three files' `ctime` values changed across remounts without a
logical modification. The writable adapter currently derives these values from
native metadata and ignores the kernel-supplied ctime in SETATTR. Writeback needs
a deliberate metadata design before enabling it.

The existing cache suite also stopped at an assertion that native allocation is
already nonzero while a written file is still open. That assumption is specific
to write-through behavior: dirty kernel pages need not yet have native allocation.
This assertion failure alone is not evidence of lost data. Tests must distinguish
cached visibility, backing allocation, and explicit durability guarantees.

The prototype also requires backing read permission for a write-only application
open. This restriction is documented by the [libfuse passthrough example](https://github.com/libfuse/libfuse/blob/master/example/passthrough_ll.c).
It should not silently become a new default restriction for SL7 users.

The tested prototype executable SHA-256 was
`2aa2dd0f88478edc5b2cddeff09e7a021fe943fa405dad8d371a10cda2e34228`.
An earlier source-overlay run accidentally reused the baseline executable because
of restored source timestamps. Binary identity and missing counters exposed this;
those earlier validation results were rejected. The real prototype was rebuilt
before the observations above. No Cargo speedup is claimed from this experiment.
