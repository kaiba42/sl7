# Writable store protocol, version 1

The native tree supplies namespace and inode identity. A `user.sl7.file.v1` xattr
on each tracked regular inode points to an AUTOINCREMENT `live_files` row. Hard
links retain that identity; native inode-number reuse cannot alias an old row.
`live_config` versions this extension separately from the immutable v1 catalog.

Each live row records a generation, optional manifest ID, and optional logical
size/atime/mtime/ctime snapshot. Internal manifests use reserved view
`:sl7-live-v1:` and names `<identity>-<generation>`. A durable generation increment
prevents interrupted imports from colliding with retries. Existing offline
imports under the reserved view prevent live initialization.

## Authority and transitions

**A committed manifest is authoritative whenever its live binding exists.** Native
bytes may be intact, partially retired/hydrated, or sparse. Reads use the manifest
until hydration commits removal of its binding.

Sealing, with no open handles/mappings:

1. Capture logical metadata and durably assign an xattr identity if needed.
2. Increment the generation and capture an in-memory mutation epoch. A single
   background worker chunks native bytes and publishes immutable packs outside
   the filesystem mutex, using a separate read-only catalog connection. Pack data
   and directory entries are synced before the manifest commits.
3. Reacquire the mutex and require the same epoch and zero open handles. Discard
   stale work; otherwise commit the manifest and binding using SQLite WAL/FULL.
4. Truncate the native file, restore its logical length as a sparse placeholder,
   restore atime/mtime, and sync. Permissions and hard links stay native.

Hydration, before mutable access:

1. Keep the binding while reconstructing all bytes into the existing inode;
   verify the whole-file digest.
2. Restore timestamps and sync native data.
3. Atomically clear the binding and delete its immutable label/extent references.
   Chunks remain until GC.
4. Preserve the timestamp snapshot across the internal transition, then invalidate
   it before allowing a user mutation.

Metadata-only changes retain the manifest: permissions remain on the native
inode, while changed timestamps and ctime update the logical snapshot. They do
not reconstruct file contents. Truncation to zero first truncates and syncs the
native inode, then atomically removes its manifest and metadata binding. Before
that commit recovery still restores the old manifest; afterward the durable
empty native file is authoritative. Nonzero size changes still hydrate first.

Callbacks and publication hold one mutex; expensive packing does not. Mutations
invalidate pending work. Open readers prevent retirement until their last release.
One worker bounds active jobs; a 1,024-entry queue spills pending flags onto pinned
inodes. Normal unmount drains the worker before releasing store ownership. Canceled
work can leave orphan packs for offline GC; native bytes remain authoritative.
`SL7_CACHE_STATS=1` also prints sealing counts and preparation/publication timings.
Files below 64 KiB, unlinked files, and files lacking owner
read/write permission stay native. Sealing errors are logged and leave native
bytes or an already committed manifest authoritative. Failure of the optimization
does not turn close into a failed user write.

## Handles, mappings, and recovery

FUSE inode IDs map to pinned `O_PATH` native descriptors. Lookup references,
directory snapshots, and open handles keep pins alive. IDs are not reused within
a session. Kernel FUSE release follows the final mapping/file reference: closing
a userspace descriptor does not permit sealing while its mmap remains. Cached I/O
provides same-inode coherence; writeback-cache is not negotiated. Hydration occurs
before mutable access; sealing requires zero open handles.

Recovery scans unique native regular inodes before mounting. A committed manifest
wins over partially hydrated/retired bytes; recovery finishes sparse retirement
when native allocation or length differs. If hydration removed the binding,
synced native bytes are authoritative. Synced data from a killed daemon can be
read after remount. The store and native tree remain exclusively owned.

Unit subprocesses terminate at seal commit, retirement truncate, hydration
truncate, hydration fsync, hydration commit, and zero-truncate publication
boundaries. Mounted tests also kill the real
daemon after acknowledged file/directory fsync, remount, and verify bytes. These
exercise process failure, not arbitrary power cuts or faulty storage.

## Collection

Reads use a 128 MiB LRU of checksum-verified decoded chunks and an LRU of at most
64 open pack descriptors. Pack reads use positioned I/O. Eviction only discards
reconstructible cache state; it never removes reachable artifacts. Integrity
checks clear and bypass both caches, and collection starts with that uncached
check so deleted packs are not pinned by old descriptors. `SL7_CACHE_STATS=1`
prints cache counters to stderr when a store closes.

File-to-chunk mappings are cached as sorted 256 KiB logical windows with binary
search for the requested range. The mapping cache has a 32 MiB accounting budget
and a 4,096-window limit, separate from the decoded-byte budget. Accounting covers
extent allocations, pack strings, keys, and estimated entry overhead; it is not
an exact RSS limit. Windows are keyed by file ID, logical size, whole-file digest,
and window offset, so SQLite row-ID reuse cannot alias different file contents.
GC clears mappings before relocating packs, and integrity checks clear/bypass
them along with the other caches. Loading is capped at 1,025 rows; unusually dense
catalog windows use the original streaming query instead of an unbounded allocation.
Metadata, range, and per-chunk existence queries reuse prepared statements.
`cache_stats` includes mapping hits, misses, SQL queries, fallback counts, accounted
bytes, and retained windows.

GC requires the exclusive store lock and an unmounted tree. It scans xattr
identities, retains their bound manifests, and removes unreachable live rows and
reserved manifests. Open-file/mapping roots cannot coexist with offline GC because
the mounted daemon owns the lock.

Chunks without extent references are deleted from the catalog. For partially
live packs, GC writes and syncs replacements, commits relocated references, then
deletes old packs. It also collects orphan packs and temporary files left by
interrupted publication, vacuums/checkpoints SQLite, and checks integrity again.
A subprocess test dies after relocation commit, verifies the retained artifact
after reopen, and checks that another GC removes the orphaned old pack.

Unlink and `cargo clean` remove names; packed bytes survive until GC. Collections
between edit measurements represent the quiescent live set, not historical
revisions retained forever. Shared chunks survive while another view uses them.

## Operational boundary

Use the mount for all artifact access. Native reads of sealed `tree/` files return
sparse zeros. Copying that directory alone loses the content; backups must preserve
the entire unmounted store and xattrs. External mutation is unsupported.

There is no online control socket, multi-daemon writer protocol, or native macOS
writable backend. Native metadata
is not a complete virtual POSIX layer: arbitrary ownership and restrictive
permission transitions remain outside qualification. Background packing still serializes catalog publication and sparse retirement
with filesystem callbacks.
