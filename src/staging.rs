//! Linux writable adapter over pinned native inodes, with optional chunk backing.
//! All namespace access is anchored to pinned O_PATH descriptors. The backing
//! tree must be exclusively owned by this process while mounted. Kernel-local
//! locks and cached I/O are used; writeback-cache capability is not negotiated.
use fuser::*;
mod background;
use std::{
    collections::{HashMap, VecDeque},
    ffi::{CString, OsStr, OsString},
    fs::{self, File, Metadata, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const TTL: Duration = Duration::ZERO;
struct Inode {
    pin: File,
    identity: (u64, u64),
    references: u64,
    opens: u64,
    epoch: Arc<AtomicU64>,
    queued: bool,
    packing: bool,
}
struct DirectoryEntry {
    ino: INodeNo,
    kind: FileType,
    name: OsString,
}
struct Handle {
    ino: INodeNo,
    file: File,
    entries: Option<Vec<DirectoryEntry>>,
}
struct State {
    live: Option<crate::live::Live>,
    nodes: HashMap<INodeNo, Inode>,
    identities: HashMap<(u64, u64), INodeNo>,
    handles: HashMap<FileHandle, Handle>,
    next_inode: u64,
    next_handle: u64,
    seal_queue: VecDeque<INodeNo>,
    wake: Option<SyncSender<()>>,
    stopping: bool,
    worker_failed: bool,
    seal_stats: background::Stats,
    #[cfg(test)]
    seal_pause: Option<(SyncSender<()>, std::sync::mpsc::Receiver<()>)>,
}
pub struct StagingFs {
    state: Arc<Mutex<State>>,
    worker: Option<std::thread::JoinHandle<()>>,
    _owner: Owner,
}
struct Owner(File);
impl Drop for StagingFs {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            background::stop(&self.state, worker);
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // A fork before CLOEXEC must not extend ownership after this session.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

fn error(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}
fn storage(error: crate::Error) -> io::Error {
    match error {
        crate::Error::Io(e) => e,
        other => io::Error::other(other),
    }
}
fn cpath(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| error(libc::EINVAL))
}
fn native(rc: i32) -> io::Result<()> {
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}
fn pin(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}
fn open_native(path: &Path, flags: i32, mode: u32) -> io::Result<File> {
    let path = cpath(path)?;
    // SAFETY: path is NUL-terminated; open returns a new, uniquely owned fd.
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful open above transfers ownership of this descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn kind(meta: &Metadata) -> io::Result<FileType> {
    let ty = meta.file_type();
    if ty.is_dir() {
        Ok(FileType::Directory)
    } else if ty.is_file() {
        Ok(FileType::RegularFile)
    } else if ty.is_symlink() {
        Ok(FileType::Symlink)
    } else {
        Err(error(libc::EOPNOTSUPP))
    }
}
fn timestamp(sec: i64, nanos: i64) -> SystemTime {
    let base = if sec >= 0 {
        UNIX_EPOCH + Duration::from_secs(sec as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(sec.unsigned_abs())
    };
    base + Duration::from_nanos(nanos as u64)
}
fn timespec(time: Option<TimeOrNow>) -> io::Result<libc::timespec> {
    let Some(time) = time else {
        return Ok(libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        });
    };
    match time {
        TimeOrNow::Now => Ok(libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        }),
        TimeOrNow::SpecificTime(time) => match time.duration_since(UNIX_EPOCH) {
            Ok(d) => Ok(libc::timespec {
                tv_sec: d.as_secs().try_into().map_err(|_| error(libc::EINVAL))?,
                tv_nsec: d.subsec_nanos().into(),
            }),
            Err(e) => {
                let d = e.duration();
                let sec: i64 = d.as_secs().try_into().map_err(|_| error(libc::EINVAL))?;
                Ok(if d.subsec_nanos() == 0 {
                    libc::timespec {
                        tv_sec: -sec,
                        tv_nsec: 0,
                    }
                } else {
                    libc::timespec {
                        tv_sec: -sec - 1,
                        tv_nsec: (1_000_000_000 - d.subsec_nanos()).into(),
                    }
                })
            }
        },
    }
}

impl State {
    fn invalidate(&mut self, ino: INodeNo) -> io::Result<()> {
        self.nodes
            .get(&ino)
            .ok_or_else(|| error(libc::ENOENT))?
            .epoch
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| error(libc::EOVERFLOW))?;
        Ok(())
    }
    fn truncate_zero(&mut self, ino: INodeNo) -> io::Result<()> {
        self.invalidate(ino)?;
        if let Some(live) = &mut self.live {
            live.truncate_zero(&self.nodes.get(&ino).ok_or_else(|| error(libc::ENOENT))?.pin)
                .map_err(storage)?;
        }
        Ok(())
    }
    fn hydrate(&mut self, ino: INodeNo) -> io::Result<()> {
        self.invalidate(ino)?;
        if let Some(live) = &mut self.live {
            live.hydrate(&self.nodes.get(&ino).ok_or_else(|| error(libc::ENOENT))?.pin)
                .map_err(storage)?;
        }
        Ok(())
    }
    fn mutated(&mut self, ino: INodeNo) -> io::Result<()> {
        self.invalidate(ino)?;
        if let Some(live) = &mut self.live {
            live.mutated(&self.nodes.get(&ino).ok_or_else(|| error(libc::ENOENT))?.pin)
                .map_err(storage)?;
        }
        Ok(())
    }
    fn namespace_changed(&mut self, ino: INodeNo) -> io::Result<()> {
        self.invalidate(ino)?;
        if let Some(live) = &mut self.live {
            live.namespace_changed(&self.nodes.get(&ino).ok_or_else(|| error(libc::ENOENT))?.pin)
                .map_err(storage)?;
        }
        self.schedule_seal(ino);
        Ok(())
    }
    fn inode(&self, ino: INodeNo) -> io::Result<&File> {
        self.nodes
            .get(&ino)
            .map(|n| &n.pin)
            .ok_or_else(|| error(libc::ENOENT))
    }
    fn path(&self, ino: INodeNo) -> io::Result<PathBuf> {
        Ok(fd_path(self.inode(ino)?))
    }
    fn child(&self, parent: INodeNo, name: &OsStr) -> io::Result<PathBuf> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.as_bytes().contains(&b'/')
            || name.as_bytes().contains(&0)
        {
            return Err(error(libc::EINVAL));
        }
        if !self.inode(parent)?.metadata()?.is_dir() {
            return Err(error(libc::ENOTDIR));
        }
        Ok(self.path(parent)?.join(name))
    }
    fn register(&mut self, file: File) -> io::Result<INodeNo> {
        let meta = file.metadata()?;
        kind(&meta)?;
        let key = (meta.dev(), meta.ino());
        if let Some(&ino) = self.identities.get(&key) {
            return Ok(ino);
        }
        let ino = INodeNo(self.next_inode);
        self.next_inode = self
            .next_inode
            .checked_add(1)
            .ok_or_else(|| error(libc::EOVERFLOW))?;
        self.identities.insert(key, ino);
        self.nodes.insert(
            ino,
            Inode {
                pin: file,
                identity: key,
                references: 0,
                opens: 0,
                epoch: Arc::new(AtomicU64::new(0)),
                queued: false,
                packing: false,
            },
        );
        Ok(ino)
    }
    fn lookup(&mut self, parent: INodeNo, name: &OsStr) -> io::Result<INodeNo> {
        if !self.inode(parent)?.metadata()?.is_dir() {
            return Err(error(libc::ENOTDIR));
        }
        if name == "." || (name == ".." && parent == INodeNo::ROOT) {
            return Ok(parent);
        }
        let path = if name == ".." {
            self.path(parent)?.join("..")
        } else {
            self.child(parent, name)?
        };
        self.register(pin(&path)?)
    }
    fn attr(&self, ino: INodeNo) -> io::Result<FileAttr> {
        let m = self.inode(ino)?.metadata()?;
        let logical = self
            .live
            .as_ref()
            .map(|l| l.metadata(self.inode(ino).unwrap()).map_err(storage))
            .transpose()?
            .flatten();
        Ok(FileAttr {
            ino,
            size: logical.as_ref().map_or(m.size(), |m| m.size),
            blocks: m.blocks(),
            atime: logical
                .as_ref()
                .map_or(timestamp(m.atime(), m.atime_nsec()), |m| {
                    timestamp(m.atime, m.atime_ns)
                }),
            mtime: logical
                .as_ref()
                .map_or(timestamp(m.mtime(), m.mtime_nsec()), |m| {
                    timestamp(m.mtime, m.mtime_ns)
                }),
            ctime: logical
                .as_ref()
                .map_or(timestamp(m.ctime(), m.ctime_nsec()), |m| {
                    timestamp(m.ctime, m.ctime_ns)
                }),
            crtime: UNIX_EPOCH,
            kind: kind(&m)?,
            perm: (m.mode() & 0o7777) as u16,
            nlink: m.nlink().try_into().map_err(|_| error(libc::EOVERFLOW))?,
            uid: m.uid(),
            gid: m.gid(),
            rdev: 0,
            blksize: m.blksize() as u32,
            flags: 0,
        })
    }
    fn add_handle(
        &mut self,
        ino: INodeNo,
        file: File,
        entries: Option<Vec<DirectoryEntry>>,
    ) -> io::Result<FileHandle> {
        let fh = FileHandle(self.next_handle);
        self.next_handle = self
            .next_handle
            .checked_add(1)
            .ok_or_else(|| error(libc::EOVERFLOW))?;
        self.nodes
            .get_mut(&ino)
            .ok_or_else(|| error(libc::ENOENT))?
            .opens += 1;
        if let Some(entries) = &entries {
            for entry in entries {
                self.nodes
                    .get_mut(&entry.ino)
                    .ok_or_else(|| error(libc::ENOENT))?
                    .references += 1;
            }
        }
        self.handles.insert(fh, Handle { ino, file, entries });
        Ok(fh)
    }
    fn handle(&self, ino: INodeNo, fh: FileHandle) -> io::Result<&Handle> {
        self.handles
            .get(&fh)
            .filter(|h| h.ino == ino)
            .ok_or_else(|| error(libc::EBADF))
    }
    fn open(&mut self, ino: INodeNo, flags: i32) -> io::Result<FileHandle> {
        if !self.inode(ino)?.metadata()?.is_file() {
            return Err(error(libc::EINVAL));
        }
        if flags & libc::O_TRUNC != 0 {
            self.truncate_zero(ino)?;
        } else if flags & libc::O_ACCMODE != libc::O_RDONLY {
            self.hydrate(ino)?;
        }
        if flags & libc::O_TRUNC != 0 {
            self.mutated(ino)?;
        }
        // The kernel supplies the append offset; Linux pwrite on an O_APPEND
        // backing descriptor would incorrectly override that explicit offset.
        let file = open_native(
            &self.path(ino)?,
            (flags & !(libc::O_APPEND | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL))
                | if self.live.is_some() {
                    libc::O_NOATIME
                } else {
                    0
                },
            0,
        )?;
        self.add_handle(ino, file, None)
    }
    fn release(&mut self, ino: INodeNo, fh: FileHandle) -> io::Result<()> {
        self.handle(ino, fh)?;
        let handle = self.handles.remove(&fh).unwrap();
        if let Some(entries) = handle.entries {
            for entry in entries {
                self.forget(entry.ino, 1);
            }
        }
        self.nodes
            .get_mut(&ino)
            .ok_or_else(|| error(libc::ENOENT))?
            .opens -= 1;
        self.schedule_seal(ino);
        self.reclaim(ino);
        Ok(())
    }
    fn entry_attr(&mut self, ino: INodeNo) -> io::Result<FileAttr> {
        let attr = self.attr(ino)?;
        self.nodes.get_mut(&ino).unwrap().references += 1;
        Ok(attr)
    }
    fn forget(&mut self, ino: INodeNo, count: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            node.references = node.references.saturating_sub(count);
        }
        self.reclaim(ino);
    }
    fn reclaim(&mut self, ino: INodeNo) {
        if ino != INodeNo::ROOT
            && self
                .nodes
                .get(&ino)
                .is_some_and(|n| n.references == 0 && n.opens == 0 && !n.queued && !n.packing)
        {
            let node = self.nodes.remove(&ino).unwrap();
            self.identities.remove(&node.identity);
        }
    }
}

impl StagingFs {
    pub fn new(backing: &Path) -> io::Result<Self> {
        let owner = File::open(backing)?;
        if !owner.metadata()?.is_dir() {
            return Err(error(libc::ENOTDIR));
        }
        fs2::FileExt::try_lock_exclusive(&owner)?;
        let mut state = State {
            live: None,
            nodes: HashMap::new(),
            identities: HashMap::new(),
            handles: HashMap::new(),
            next_inode: 1,
            next_handle: 1,
            seal_queue: VecDeque::new(),
            wake: None,
            stopping: false,
            worker_failed: false,
            seal_stats: background::Stats::default(),
            #[cfg(test)]
            seal_pause: None,
        };
        assert_eq!(state.register(pin(backing)?)?, INodeNo::ROOT);
        Ok(Self {
            state: Arc::new(Mutex::new(state)),
            worker: None,
            _owner: Owner(owner),
        })
    }
    pub fn deduplicating(store: crate::Store) -> crate::Result<Self> {
        let mut live = crate::live::Live::open(store)?;
        let mut fs = Self::new(&live.tree())?;
        live.recover()?;
        let packer = crate::store::Packer::new(&live.store.root)?;
        fs.state
            .lock()
            .map_err(|_| crate::Error::Invalid("poisoned filesystem state".into()))?
            .live = Some(live);
        fs.worker = Some(background::start(Arc::clone(&fs.state), packer)?);
        Ok(fs)
    }
    fn with<T>(
        &self,
        op: impl FnOnce(&mut State) -> io::Result<T>,
    ) -> std::result::Result<T, Errno> {
        let mut state = self.state.lock().map_err(|_| Errno::EIO)?;
        op(&mut state).map_err(Errno::from)
    }
}

impl Filesystem for StagingFs {
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the owned directory fd is live and stats points to enough
        // writable storage. The result is read only after successful initialization.
        if unsafe { libc::fstatvfs(self._owner.0.as_raw_fd(), stats.as_mut_ptr()) } < 0 {
            reply.error(io::Error::last_os_error().into());
            return;
        }
        // SAFETY: successful fstatvfs initialized every field.
        let stats = unsafe { stats.assume_init() };
        reply.statfs(
            stats.f_blocks,
            stats.f_bfree,
            stats.f_bavail,
            stats.f_files,
            stats.f_ffree,
            stats.f_bsize as u32,
            stats.f_namemax as u32,
            stats.f_frsize as u32,
        );
    }
    fn forget(&self, _req: &Request, ino: INodeNo, nlookup: u64) {
        let _ = self.with(|s| {
            s.forget(ino, nlookup);
            Ok(())
        });
    }
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.with(|s| {
            let ino = s.lookup(parent, name)?;
            s.entry_attr(ino)
        }) {
            Ok(a) => reply.entry(&TTL, &a, Generation(0)),
            Err(e) => reply.error(e),
        }
    }
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.with(|s| s.attr(ino)) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(e),
        }
    }
    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        match self.with(|s| {
            let m = s.inode(ino)?.metadata()?;
            if uid.is_some_and(|u| u != m.uid())
                || gid.is_some_and(|g| g != m.gid())
                || flags.is_some()
            {
                return Err(error(libc::EOPNOTSUPP));
            }
            if m.file_type().is_symlink() {
                return Err(error(libc::EOPNOTSUPP));
            }
            s.invalidate(ino)?;
            if m.is_file() && size.is_some() {
                if size == Some(0) {
                    s.truncate_zero(ino)?;
                } else {
                    s.hydrate(ino)?;
                }
                s.mutated(ino)?;
            }
            if let Some(size) = size {
                if let Some(fh) = fh {
                    s.handle(ino, fh)?.file.set_len(size)?;
                } else {
                    open_native(&s.path(ino)?, libc::O_WRONLY, 0)?.set_len(size)?;
                }
            }
            if let Some(mode) = mode {
                fs::set_permissions(s.path(ino)?, fs::Permissions::from_mode(mode))?;
            }
            if atime.is_some() || mtime.is_some() {
                let path = cpath(&s.path(ino)?)?;
                let times = [timespec(atime)?, timespec(mtime)?];
                // SAFETY: path and both timespecs live through the call.
                native(unsafe {
                    libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0)
                })?;
            }
            if let Some(live) = &mut s.live {
                live.metadata_changed(&s.nodes[&ino].pin, atime.is_some(), mtime.is_some())
                    .map_err(storage)?;
            }
            s.schedule_seal(ino);
            s.attr(ino)
        }) {
            Ok(a) => reply.attr(&TTL, &a),
            Err(e) => reply.error(e),
        }
    }
    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        match self.with(|s| {
            let path = s.child(parent, name)?;
            let c = cpath(&path)?;
            // SAFETY: path is a valid C string.
            native(unsafe { libc::mkdir(c.as_ptr(), mode & !umask) })?;
            let ino = s.register(pin(&path)?)?;
            s.entry_attr(ino)
        }) {
            Ok(a) => reply.entry(&TTL, &a, Generation(0)),
            Err(e) => reply.error(e),
        }
    }
    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        match self.with(|s| {
            let path = s.child(parent, name)?;
            if flags & libc::O_EXCL == 0 {
                match s.lookup(parent, name) {
                    Ok(ino) => {
                        if flags & libc::O_TRUNC != 0 {
                            s.truncate_zero(ino)?;
                            s.mutated(ino)?;
                        } else {
                            s.hydrate(ino)?;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e),
                }
            }
            let file = open_native(
                &path,
                (flags & !libc::O_APPEND) | libc::O_CREAT | libc::O_NOFOLLOW,
                mode & !umask,
            )?;
            let ino = s.register(pin(&path)?)?;
            let fh = s.add_handle(ino, file, None)?;
            Ok((s.entry_attr(ino)?, fh))
        }) {
            Ok((a, fh)) => reply.created(&TTL, &a, Generation(0), fh, FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }
    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.with(|s| s.open(ino, flags.0)) {
            Ok(fh) => reply.opened(fh, FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }
    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.with(|s| {
            if size > 16 * 1024 * 1024 {
                return Err(error(libc::EINVAL));
            }
            if let Some(live) = &s.live {
                s.handle(ino, fh)?;
                if let Some(bytes) = live
                    .read(s.inode(ino)?, offset, size as usize)
                    .map_err(storage)?
                {
                    return Ok(bytes);
                }
            }
            let file = &s.handle(ino, fh)?.file;
            let mut bytes = vec![0; size as usize];
            let mut read = 0;
            while read < bytes.len() {
                match file.read_at(
                    &mut bytes[read..],
                    offset
                        .checked_add(read as u64)
                        .ok_or_else(|| error(libc::EINVAL))?,
                ) {
                    Ok(0) => break,
                    Ok(n) => read += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            bytes.truncate(read);
            Ok(bytes)
        }) {
            Ok(b) => reply.data(&b),
            Err(e) => reply.error(e),
        }
    }
    fn write(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.with(|s| {
            s.hydrate(ino)?;
            s.mutated(ino)?;
            s.handle(ino, fh)?.file.write_all_at(data, offset)
        }) {
            Ok(()) => reply.written(data.len() as u32),
            Err(e) => reply.error(e),
        }
    }
    fn flush(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        // No writeback queue or remote locks; each write already reached the
        // backing filesystem. This deliberately does not turn close into fsync.
        match self.with(|s| s.handle(ino, fh).map(|_| ())) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.with(|s| s.release(ino, fh)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn fsync(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.with(|s| {
            let file = &s.handle(ino, fh)?.file;
            if datasync {
                file.sync_data()
            } else {
                file.sync_all()
            }
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match self.with(|s| {
            let ino = s.lookup(parent, name)?;
            fs::remove_file(s.child(parent, name)?)?;
            s.namespace_changed(ino)?;
            s.reclaim(ino);
            Ok(())
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match self.with(|s| fs::remove_dir(s.child(parent, name)?)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        match self.with(|s| {
            let source = s.lookup(parent, name)?;
            let replaced = match s.lookup(newparent, newname) {
                Ok(ino) => Some(ino),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            let old = cpath(&s.child(parent, name)?)?;
            let new = cpath(&s.child(newparent, newname)?)?;
            // SAFETY: both paths are valid C strings anchored at directory fds.
            native(unsafe {
                libc::renameat2(
                    libc::AT_FDCWD,
                    old.as_ptr(),
                    libc::AT_FDCWD,
                    new.as_ptr(),
                    flags.bits(),
                )
            })?;
            s.namespace_changed(source)?;
            if let Some(ino) = replaced {
                s.namespace_changed(ino)?;
                s.reclaim(ino);
            }
            s.reclaim(source);
            Ok(())
        }) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn link(
        &self,
        _req: &Request,
        ino: INodeNo,
        newparent: INodeNo,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        match self.with(|s| {
            if !s.inode(ino)?.metadata()?.is_file() {
                return Err(error(libc::EOPNOTSUPP));
            }
            let old = cpath(&s.path(ino)?)?;
            let new = cpath(&s.child(newparent, newname)?)?;
            // SAFETY: valid paths; follow only the controlled /proc fd alias.
            native(unsafe {
                libc::linkat(
                    libc::AT_FDCWD,
                    old.as_ptr(),
                    libc::AT_FDCWD,
                    new.as_ptr(),
                    libc::AT_SYMLINK_FOLLOW,
                )
            })?;
            s.namespace_changed(ino)?;
            s.entry_attr(ino)
        }) {
            Ok(a) => reply.entry(&TTL, &a, Generation(0)),
            Err(e) => reply.error(e),
        }
    }
    fn symlink(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        match self.with(|s| {
            let path = s.child(parent, name)?;
            std::os::unix::fs::symlink(target, &path)?;
            let ino = s.register(pin(&path)?)?;
            s.entry_attr(ino)
        }) {
            Ok(a) => reply.entry(&TTL, &a, Generation(0)),
            Err(e) => reply.error(e),
        }
    }
    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        match self.with(|s| {
            let mut data = vec![0; 4096];
            // SAFETY: fd is pinned, empty path selects that symlink itself, and
            // the writable buffer has the capacity passed to readlinkat.
            let len = unsafe {
                libc::readlinkat(
                    s.inode(ino)?.as_raw_fd(),
                    c"".as_ptr(),
                    data.as_mut_ptr().cast(),
                    data.len(),
                )
            };
            if len < 0 {
                return Err(io::Error::last_os_error());
            }
            if len as usize == data.len() {
                return Err(error(libc::ENAMETOOLONG));
            }
            data.truncate(len as usize);
            Ok(data)
        }) {
            Ok(data) => reply.data(&data),
            Err(e) => reply.error(e),
        }
    }
    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        match self.with(|s| {
            let file = open_native(&s.path(ino)?, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
            let parent = s.lookup(ino, OsStr::new(".."))?;
            let mut entries = vec![
                DirectoryEntry {
                    ino,
                    kind: FileType::Directory,
                    name: ".".into(),
                },
                DirectoryEntry {
                    ino: parent,
                    kind: FileType::Directory,
                    name: "..".into(),
                },
            ];
            let mut names: Vec<_> = fs::read_dir(fd_path(&file))?
                .map(|e| e.map(|e| e.file_name()))
                .collect::<io::Result<_>>()?;
            names.sort();
            for name in names {
                let child = s.lookup(ino, &name)?;
                entries.push(DirectoryEntry {
                    ino: child,
                    kind: s.attr(child)?.kind,
                    name,
                });
            }
            s.add_handle(ino, file, Some(entries))
        }) {
            Ok(fh) => reply.opened(fh, FopenFlags::empty()),
            Err(e) => reply.error(e),
        }
    }
    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = self.with(|s| {
            let entries = s
                .handle(ino, fh)?
                .entries
                .as_ref()
                .ok_or_else(|| error(libc::ENOTDIR))?;
            for (i, e) in entries.iter().enumerate() {
                if (i as u64) < offset {
                    continue;
                }
                if reply.add(e.ino, i as u64 + 1, e.kind, &e.name) {
                    break;
                }
            }
            Ok(())
        });
        match result {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn releasedir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        match self.with(|s| s.release(ino, fh)) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
    fn fsyncdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.with(|s| s.handle(ino, fh)?.file.sync_all()) {
            Ok(()) => reply.ok(),
            Err(e) => reply.error(e),
        }
    }
}

/// This is an adapter qualification tool, not yet a deduplicating writable mount.
pub fn mount(backing: &Path, mountpoint: &Path) -> crate::Result<()> {
    let backing = backing.canonicalize()?;
    let mountpoint = mountpoint.canonicalize()?;
    if backing.starts_with(&mountpoint) || mountpoint.starts_with(&backing) {
        return Err(crate::Error::Invalid(
            "backing tree and mountpoint must not contain each other".into(),
        ));
    }
    if fs::read_dir(&mountpoint)?.next().is_some() {
        return Err(crate::Error::Invalid(
            "mountpoint must be an empty directory".into(),
        ));
    }
    let fs = StagingFs::new(&backing)?;
    run_mount(fs, &mountpoint)
}

pub fn mount_store(store: &Path, mountpoint: &Path) -> crate::Result<()> {
    let store = store.canonicalize()?;
    let mountpoint = mountpoint.canonicalize()?;
    if store.starts_with(&mountpoint) || mountpoint.starts_with(&store) {
        return Err(crate::Error::Invalid(
            "store and mountpoint must not contain each other".into(),
        ));
    }
    if fs::read_dir(&mountpoint)?.next().is_some() {
        return Err(crate::Error::Invalid("mountpoint must be empty".into()));
    }
    let fs = StagingFs::deduplicating(crate::Store::open(&store)?)?;
    run_mount(fs, &mountpoint)
}

fn run_mount(fs: StagingFs, mountpoint: &Path) -> crate::Result<()> {
    // This dedicated CLI process uses caller-provided modes/umasks. Applying
    // the daemon's umask again would silently remove requested permission bits.
    // SAFETY: called before the FUSE session starts any worker threads.
    unsafe {
        libc::umask(0);
    }
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::RW,
        MountOption::DefaultPermissions,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::Exec,
        MountOption::NoAtime,
        MountOption::FSName("sl7".into()),
    ];
    let state = Arc::clone(&fs.state);
    fuser::mount(fs, mountpoint, &config)?;
    if state
        .lock()
        .map_err(|_| io::Error::other("poisoned filesystem state"))?
        .worker_failed
    {
        return Err(io::Error::other("background sealing worker failed").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlinked_inode_lives_until_last_handle_after_forget() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("data");
        fs::write(&path, b"retained bytes").unwrap();
        let fs = StagingFs::new(root.path()).unwrap();
        fs.with(|s| {
            let ino = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
            s.entry_attr(ino)?;
            let fh = s.open(ino, libc::O_RDWR)?;
            fs::remove_file(&path)?;
            s.forget(ino, 1);
            assert!(s.nodes.contains_key(&ino));
            let mut bytes = [0; 14];
            s.handle(ino, fh)?.file.read_exact_at(&mut bytes, 0)?;
            assert_eq!(&bytes, b"retained bytes");
            s.release(ino, fh)?;
            assert_eq!(s.nodes.len(), 1);
            assert_eq!(s.identities.len(), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn hardlinks_share_identity_and_symlinks_are_not_followed_by_daemon() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("data"), b"bytes").unwrap();
        fs::hard_link(root.path().join("data"), root.path().join("alias")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.path().join("outside")).unwrap();
        let fs = StagingFs::new(root.path()).unwrap();
        fs.with(|s| {
            let a = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
            let b = s.lookup(INodeNo::ROOT, OsStr::new("alias"))?;
            assert_eq!(a, b);
            let link = s.lookup(INodeNo::ROOT, OsStr::new("outside"))?;
            assert_eq!(s.attr(link)?.kind, FileType::Symlink);
            assert!(s.open(link, libc::O_RDONLY).is_err());
            assert!(s.child(INodeNo::ROOT, OsStr::new("../escape")).is_err());
            assert!(s.child(INodeNo::ROOT, OsStr::new("..")).is_err());
            assert_eq!(s.lookup(INodeNo::ROOT, OsStr::new(".."))?, INodeNo::ROOT);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn backing_tree_has_one_owner_and_releases_ownership() {
        let root = tempfile::tempdir().unwrap();
        let fs = StagingFs::new(root.path()).unwrap();
        let inherited = fs._owner.0.try_clone().unwrap();
        assert!(StagingFs::new(root.path()).is_err());
        drop(fs);
        StagingFs::new(root.path()).unwrap();
        drop(inherited);
    }
}
