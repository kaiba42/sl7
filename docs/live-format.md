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

1. Under filesystem state, pin the inode and capture its mutation epoch. Release
   state before capturing metadata, assigning an xattr identity, or incrementing
   the durable generation. Those operations use a separate worker catalog connection.
2. Chunk native bytes and prepare immutable packs outside filesystem state. Pack
   data and directory entries are synced before the manifest commits.
3. Acquire the catalog-writer permit, then briefly reacquire filesystem state.
   Require the same epoch and zero open handles. Discard stale work; otherwise
   mark this inode as publishing and release filesystem state.
4. Commit the manifest and binding using SQLite WAL/FULL, then truncate the native
   file, restore its logical length as a sparse placeholder, restore atime/mtime,
   and sync. Permissions and hard links stay native. This work holds the catalog
   permit, but not filesystem state.
5. Briefly reacquire state to clear the inode's publication gate and wake waiters.
   Error/unwind cleanup also clears the gate; committed bindings retain authority.

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

Four FUSE request workers share brief filesystem bookkeeping sections. A READ
callback pins its inode and open file under state, then releases state before any
catalog lookup, extent query, native read, pack read, decompression, checksum, or
output copying. Four independent read-only catalog connections are lent from a
bounded pool; checkout/return never holds filesystem state. All connections share
one bounded payload/mapping/descriptor cache. Cache locks cover bookkeeping only;
pack opens and verified chunk decoding run outside them. Concurrent misses may
decode the same chunk twice, but insertion charges it only once.

An in-flight read lease adds an open reference and a reader count to its inode.
Release/forget cannot reclaim that inode or allow sparse retirement until the
lease ends, including error/unwind paths. Mutations wait for readers of affected
inodes without holding state or the catalog-writer permit. Pending mutations gate
new reads of those inodes so continuous readers cannot starve a mutation. Namespace
targets are resolved again after waiting: replacing a rename destination cannot
bypass the new target's read lease. Reads of unrelated inodes continue.

Open readers prevent retirement until their last release. New opens of an inode
being published wait on its condition variable without holding filesystem state.
Background candidate setup, packing, publication, and sparse retirement also run
outside state. Native positioned writes and file/directory synchronization also
run outside state. Their inode reservations retain the descriptor and block
same-inode file mutations until completion, including error paths. Unrelated
native I/O and reads can overlap. Hydration and catalog preparation still occur
under state before the global lock is released for native I/O.
Hard-link, unlink, and rename operations capture changed logical timestamps and
reserve affected inodes under state, then commit on a separate catalog connection
outside state. Reads can overlap these metadata-only commits, including reads of
the affected inode. Same-inode mutations wait; release/forget cannot remove pins
until commit and reply-attribute capture complete. Rename updates share one
transaction. Successful replies still wait for the FULL-synchronous commit.
Foreground hydration, truncation, chmod/utime, and other short namespace/metadata
callbacks still use state.

Catalog-writing mutations acquire a separate writer permit **before** filesystem
state. Mutations of already-native inodes without a logical metadata override
need no catalog permit; their eligibility check and reservation installation share
the state lock, so publication cannot start between them. Namespace operations re-resolve
names after waiting for the permit. Mutations invalidate pending work through
an atomic epoch. A writer waiting for publication therefore does not hold state
and prevent unrelated reads or native writes. Multiple requests waiting for
catalog access can still occupy the finite request-worker pool.

The worker, foreground, and read-pool catalog connections share one exclusive
owner lock; dropping any one connection cannot admit another mount or offline GC.
Attribute reads sample native metadata before the logical catalog override, so
concurrent retirement cannot expose a transient zero logical length.
One worker bounds active jobs; a 1,024-entry queue spills pending flags onto pinned
inodes. Normal unmount drains the worker before releasing store ownership. Canceled
work can leave orphan packs for offline GC; native bytes remain authoritative.
`SL7_CACHE_STATS=1` also prints sealing counts and candidate/preparation/publication
wall times, selection/publication state-lock hold totals and maxima, foreground
state-lock hold times, catalog-permit waits, and same-inode publication-open waits.
Read counters include in-flight/max concurrent requests, time outside state, read
bookkeeping time, and waits between same-inode readers and mutations. Cache counters
include active/max overlapping payload loads and duplicate loads. A request count
alone does not prove parallel decoding; cold-load overlap has a separate counter
and deterministic test. Wait/work totals overlap across threads and are not
removable wall time.
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
before mutable access; sealing requires zero open handles and in-flight read leases.

Live-mount file opens return `FOPEN_KEEP_CACHE`: reopening an artifact preserves
its kernel page-cache contents. All logical writes, truncations, and namespace
changes go through this mount; the kernel maintains their cache coherence.
Background sealing and hydration preserve logical bytes and timestamps, so they
do not invalidate those pages. External writes to the private backing tree are
unsupported. Native staging mounts retain their conservative open behavior.
Kernel pages are reclaimable system cache, separate from SL7's bounded userspace
payload and extent caches. Writeback caching remains disabled.

The live mount also gives successful name lookups and attributes a one-second
kernel validity interval. Mutations through the mount invalidate the affected
kernel metadata; they do not wait for that interval to expire. Background
retirement can change allocated blocks without a client mutation, so `stat`'s
block count can lag by up to that interval. Logical size and preserved timestamps
do not change during retirement. Native staging retains zero metadata validity.
`lookup_operations` and `getattr_operations` count requests reaching the daemon,
allowing mounted tests and builds to verify cache effectiveness.

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
permission transitions remain outside qualification. Catalog-writing foreground transitions still serialize with publication through
the writer permit. Hydration, truncation, and chmod/utime still hold filesystem
state; reads, native writes, synchronization, and link/unlink/rename catalog commits
run outside it. SQLite catalog writes remain serialized. The four-request-worker limit
can still constrain progress when several mutations are waiting.
