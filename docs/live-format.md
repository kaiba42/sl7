# Writable store protocol, version 1

The native tree supplies namespace and inode identity. A `user.sl7.file.v1` xattr
on each tracked regular inode points to an AUTOINCREMENT `live_files` row. Hard
links retain that identity; native inode-number reuse cannot alias an old row.
`live_config` versions this extension separately from the immutable v1 catalog.

Each allocating connection reserves 64 identity rows in one FULL-synchronous
transaction, then assigns them to files as needed. A row is durable before its
xattr is attached, and that inode is still synced before manifest publication.
Failed assignments and unused reservations remain unreachable rows for offline
GC. GC clears the current connection's reservation pool before deleting orphan
rows; AUTOINCREMENT prevents those identities from being recycled. Mounts use
one allocation pool per sealing worker, holding at most 64 not-yet-assigned rows
per allocating connection. Abandoned reservations across sessions
remain until GC. The schema and identity/xattr durability ordering are unchanged.

Each live row records a generation, optional manifest ID, and optional logical
size/atime/mtime/ctime snapshot. Internal manifests use reserved view
`:sl7-live-v1:` and names `<identity>-<generation>`. The next generation is planned
before packing and committed atomically with the manifest and live binding.
Canceled preparations or rolled-back publications leave no label and consume no
generation. Previously committed generations are never reused. Existing offline
imports under the reserved view prevent live initialization.

## Authority and transitions

**A committed manifest is authoritative whenever its live binding exists.** Native
bytes may be intact, partially retired/hydrated, or sparse. Reads use the manifest
until hydration commits removal of its binding.

Sealing, with no open handles/mappings:

1. Under filesystem state, pin the inode and capture its mutation epoch. Release
   state before capturing metadata, assigning an xattr identity, or reading the
   generation. A new identity and its xattr still become durable before publication.
   Planning the next generation requires only a read on the worker connection.
2. Chunk native bytes and prepare immutable packs outside filesystem state. Pack
   writers coalesce header and payload writes in a bounded 64 KiB buffer per
   active writer. The buffer is flushed before syncing the file; pack data and
   directory entries are synced before the manifest commits. Buffering changes
   neither record bytes nor content hashes.
3. Acquire the catalog-writer permit, then briefly reacquire filesystem state.
   Require the same epoch and zero open handles. Discard stale work; otherwise
   mark this inode as publishing and release filesystem state.
4. Insert the manifest, advance the generation conditionally, and bind the inode
   in one SQLite WAL/FULL transaction, then
   release the catalog permit. The committed seal owns a pinned native descriptor and its logical
   metadata; it requires no further catalog access.
5. Keep the inode's publication gate closed while truncating the native file,
   restoring its logical length as a sparse placeholder, restoring atime/mtime,
   and syncing. Permissions and hard links stay native. Neither filesystem state
   nor the catalog-writer permit is held; unrelated catalog mutations can overlap.
6. Briefly reacquire state to clear the inode's publication gate and wake waiters.
   Error/unwind cleanup also clears the gate; committed bindings retain authority.
   Same-inode mutations wait directly on that gate without holding either lock;
   acquiring the catalog permit alone no longer proves retirement has finished.

Manifest insertion or binding failure rolls back the entire publication. Native
bytes remain authoritative, and any previously synced packs are GC orphans. This
replaces two sequential FULL commits with one atomic FULL commit. The synchronous
seal path used by recovery fixtures exercises the same transaction as the worker.
Generation allocation also uses that transaction, removing the former separate
commit before packing. A stale candidate cannot overwrite a newer generation;
counter exhaustion is rejected without changing the stored integer. Stores from
older versions may contain orphan manifests whose labels were committed before
binding failed; their labels are no greater than the stored generation, so the
next generation remains distinct. The schema and older-version read compatibility
are unchanged.

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
transaction. Successful replies wait for a SQLite WAL/NORMAL commit, which makes
the timestamps visible immediately without a disk barrier on every namespace
operation. This restricted writer can update timestamps but cannot publish or
remove content mappings. Content transitions and chmod/utime retain WAL/FULL.

File and directory fsync (including datasync) acquire the catalog-writer permit
outside filesystem state, temporarily select FULL on the timestamp connection,
and commit a changed `live_sync` barrier row before syncing the native descriptor.
That nonempty FULL transaction flushes earlier WAL commits, including timestamps
from completed namespace operations; a passive checkpoint or empty transaction
would not establish this guarantee. A barrier failure is returned to the caller,
and the inode reservation is released on error. The additive one-row table is
compatible with existing v1 stores and older readers.

Unsynced namespace timestamps can roll back after machine failure or power loss.
They survive an application crash under SQLite's WAL guarantees. This replaces
the previous eager per-operation timestamp durability with explicit fsync
boundaries; it does not relax manifest/pack publication or retirement ordering.
See [SQLite's synchronization guarantees](https://www.sqlite.org/pragma.html#pragma_synchronous).
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

Each pinned inode can cache a proven native representation with no logical
metadata override. Repeated writes then skip identity/catalog checks and redundant
hydration checks; attributes use native metadata, and reads use the native file.
Publication clears this fact under state before committing a manifest, including
when the commit later fails. Hydration alone does not establish it because logical
timestamps remain overridden until mutation. The cache adds one bit per pinned
inode and is discarded with that inode; it does not cache native size or timestamps.
This relies on the existing requirement that backing files are accessed only
through the mounted owner. Counters distinguish state checks from cache hits.

The worker, foreground, and read-pool catalog connections share one exclusive
owner lock; dropping any one connection cannot admit another mount or offline GC.
Attribute reads sample native metadata before the logical catalog override, so
concurrent retirement cannot expose a transient zero logical length.
Two workers prepare and retire independent files concurrently. Each has its own
read-only packing connection and publication connection. `SL7_SEAL_WORKERS=1`
selects serial sealing for comparison; values from 1 through 8 are supported.
Catalog transactions remain serialized, while hashing, compression, pack writes,
and native retirement can overlap. Only one job may own an inode, including when
a concurrent write and close requeue it. If workers prepare the same absent chunk,
publication keeps the first committed chunk location; both manifests reference
that content. Redundant pack bytes remain eligible for offline GC.
Completed seal decisions are cached on the pinned inode for its current mutation
epoch. Read-only closes avoid scheduling another candidate once that epoch is
checked; every mutation clears the flag. A successful decision also absorbs a
read-only close queued during preparation. Canceled and failed jobs do not mark
an inode checked. The flag is discarded when the inode is forgotten and reclaimed;
it adds no persistent metadata or change to durability ordering.
A 1,024-entry queue spills pending flags onto pinned inodes. Normal unmount drains
and joins all workers before releasing store ownership. Canceled work can leave
orphan packs for offline GC; native bytes remain authoritative.
`SL7_CACHE_STATS=1` also prints sealing counts and candidate/preparation/publication
wall times, selection/publication state-lock hold totals and maxima, foreground
state-lock hold times, catalog-permit waits, and same-inode publication-open waits.
Publication counters separately measure catalog commits and native retirement;
mutation waits distinguish publication from active readers.
Worker counters report configured workers, active/maximum concurrent seal jobs,
and active/maximum concurrent preparation phases. Summed phase times can exceed
elapsed wall time when workers overlap.
`unchanged_seals_avoided` counts read-only closes and other scheduling attempts
suppressed by the cached decision; it is not a count of unique files.
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

The inode and native `(device, inode)` indexes use DashMap. Registered native
`LOOKUP` and `GETATTR` requests and absent-name probes run without the global
filesystem-state mutex. Each request clones a pinned descriptor, drops its map
guard before native I/O, and validates the inode epoch and representation before
replying. Lookup reference acquisition and conditional reclamation use the same
shard write lock. Inode IDs remain monotonic, so a reclaimed snapshot cannot
attach to a replacement inode. Multi-syscall mutations mark only their selected
inodes busy; error cleanup restores their eligibility. Publication revokes the
native shortcut before committing or retiring bytes.

Cold registration and sealed or logically overridden attributes retain the
coordinated state/catalog path. Handle bookkeeping, mutations and publication
also retain their existing coordination. A DashMap guard must never span native
I/O, another map operation, or acquisition of the state mutex. Counters
`indexed_lookups`, `indexed_getattrs` and `indexed_lookup_misses` count requests
served by the concurrent path; total lookup/getattr counters include both paths.

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

Unit subprocesses terminate after uncommitted manifest insertion, at seal commit,
retirement truncate, hydration
truncate, hydration fsync, hydration commit, and zero-truncate publication
boundaries. Mounted tests also kill the real
daemon after acknowledged file/directory fsync, remount, and verify bytes and
sealed-file namespace timestamps. Tests also inject catalog-barrier errors,
require FULL during its write, and verify file/directory error propagation. These
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
state; eligible native metadata, reads, native writes, synchronization, and link/unlink/rename catalog commits
run outside it. SQLite catalog writes remain serialized. The four-request-worker limit
can still constrain progress when several mutations are waiting.
