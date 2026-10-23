//! Immutable namespace adapter. The store lock pins every inode for the session.
//! No writer, hydration, garbage collection or invalidation protocol is enabled.
use crate::{Error, FileInfo, Result, Store};
use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo, LockOwner,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::Path,
    sync::Mutex,
    time::{Duration, UNIX_EPOCH},
};

const TTL: Duration = Duration::from_secs(60);
type FsResult<T> = std::result::Result<T, Errno>;

enum Contents {
    Directory(BTreeMap<String, INodeNo>),
    File(FileInfo),
}

struct Node {
    parent: INodeNo,
    contents: Contents,
}

/// One immutable snapshot. The mutex serializes SQLite access; kernel page caching
/// remains enabled. Inodes are never recycled within a mounted session.
pub struct ReadOnlyFs {
    store: Mutex<Store>,
    nodes: Vec<Node>,
    uid: u32,
    gid: u32,
}

impl ReadOnlyFs {
    pub fn new(store: Store, uid: u32, gid: u32) -> Result<Self> {
        let files = store.list()?;
        let mut fs = Self {
            store: Mutex::new(store),
            nodes: vec![Node {
                parent: INodeNo::ROOT,
                contents: Contents::Directory(BTreeMap::new()),
            }],
            uid,
            gid,
        };
        for file in files {
            // Offline labels are more permissive than filesystem names. Refuse
            // ambiguous names rather than normalizing, aliasing or hiding them.
            component(&file.view)?;
            for part in file.name.split('/') {
                component(part)?;
            }
            let mut parent = fs.directory(INodeNo::ROOT, &file.view)?;
            let parts: Vec<_> = file.name.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                parent = fs.directory(parent, part)?;
            }
            let basename = parts.last().unwrap().to_string();
            fs.insert(parent, basename, Contents::File(file))?;
        }
        Ok(fs)
    }

    fn node(&self, ino: INodeNo) -> FsResult<&Node> {
        let index = u64::from(ino).checked_sub(1).ok_or(Errno::ENOENT)?;
        let index = usize::try_from(index).map_err(|_| Errno::ENOENT)?;
        self.nodes.get(index).ok_or(Errno::ENOENT)
    }

    fn children(&self, ino: INodeNo) -> FsResult<&BTreeMap<String, INodeNo>> {
        match &self.node(ino)?.contents {
            Contents::Directory(children) => Ok(children),
            Contents::File(_) => Err(Errno::ENOTDIR),
        }
    }

    fn insert(&mut self, parent: INodeNo, name: String, contents: Contents) -> Result<INodeNo> {
        let ino = INodeNo(self.nodes.len() as u64 + 1);
        let Contents::Directory(children) =
            &mut self.nodes[u64::from(parent) as usize - 1].contents
        else {
            return Err(Error::Invalid("artifact is also a parent directory".into()));
        };
        if children.contains_key(&name) {
            return Err(Error::Invalid(format!(
                "artifact path conflicts at {name:?}"
            )));
        }
        children.insert(name, ino);
        self.nodes.push(Node { parent, contents });
        Ok(ino)
    }

    fn directory(&mut self, parent: INodeNo, name: &str) -> Result<INodeNo> {
        if let Some(&ino) = self.children(parent).ok().and_then(|c| c.get(name)) {
            self.children(ino)
                .map_err(|_| Error::Invalid(format!("artifact is also a directory: {name:?}")))?;
            return Ok(ino);
        }
        self.insert(parent, name.into(), Contents::Directory(BTreeMap::new()))
    }

    fn lookup_inode(&self, parent: INodeNo, name: &OsStr) -> FsResult<INodeNo> {
        let children = self.children(parent)?;
        match name.to_str() {
            Some(".") => Ok(parent),
            Some("..") => Ok(self.node(parent)?.parent),
            Some(name) => children.get(name).copied().ok_or(Errno::ENOENT),
            None => Err(Errno::ENOENT),
        }
    }

    fn attributes(&self, ino: INodeNo) -> FsResult<FileAttr> {
        let (kind, size, perm, nlink) = match &self.node(ino)?.contents {
            Contents::File(file) => (FileType::RegularFile, file.size, 0o444, 1),
            Contents::Directory(children) => {
                let subdirs = children
                    .values()
                    .filter(|&&i| self.children(i).is_ok())
                    .count();
                (FileType::Directory, 0, 0o555, 2 + subdirs as u32)
            }
        };
        Ok(FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            flags: 0,
            blksize: 4096,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
        })
    }

    fn check_open(&self, ino: INodeNo, flags: OpenFlags, directory: bool) -> FsResult<()> {
        let is_dir = matches!(self.node(ino)?.contents, Contents::Directory(_));
        if directory && !is_dir {
            return Err(Errno::ENOTDIR);
        }
        if !directory && is_dir {
            return Err(Errno::EISDIR);
        }
        if flags.0 & (libc::O_ACCMODE | libc::O_TRUNC | libc::O_APPEND) != libc::O_RDONLY {
            return Err(Errno::EROFS);
        }
        Ok(())
    }

    fn read_bytes(&self, ino: INodeNo, offset: u64, size: u32) -> FsResult<Vec<u8>> {
        let Contents::File(file) = &self.node(ino)?.contents else {
            return Err(Errno::EISDIR);
        };
        if size > 16 * 1024 * 1024 {
            return Err(Errno::EINVAL);
        }
        let store = self.store.lock().map_err(|_| Errno::EIO)?;
        store
            .read(&file.view, &file.name, offset, size as usize)
            .map_err(|error| {
                eprintln!(
                    "sl7: read inode {} at {offset}+{size}: {error}",
                    u64::from(ino)
                );
                Errno::EIO
            })
    }

    // Stable cookies are positions after '.', '..', and sorted child names.
    // Resuming at the last accepted cookie never drops an entry from a full reply.
    fn entries(&self, ino: INodeNo) -> FsResult<impl Iterator<Item = (INodeNo, FileType, &str)>> {
        let children = self.children(ino)?;
        let dots = [
            (ino, FileType::Directory, "."),
            (self.node(ino)?.parent, FileType::Directory, ".."),
        ];
        Ok(dots
            .into_iter()
            .chain(children.iter().map(|(name, &child)| {
                let kind = if self.children(child).is_ok() {
                    FileType::Directory
                } else {
                    FileType::RegularFile
                };
                (child, kind, name.as_str())
            })))
    }
}

fn component(name: &str) -> Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name.contains(['/', '\0'])
    {
        return Err(Error::Invalid(format!(
            "not a mountable path component: {name:?}"
        )));
    }
    Ok(())
}

impl Filesystem for ReadOnlyFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self
            .lookup_inode(parent, name)
            .and_then(|ino| self.attributes(ino))
        {
            Ok(attr) => reply.entry(&TTL, &attr, Generation(0)),
            Err(errno) => reply.error(errno),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attributes(ino) {
            Ok(attr) => reply.attr(&TTL, &attr),
            Err(errno) => reply.error(errno),
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.check_open(ino, flags, false) {
            // Safe only because the namespace and all bytes are immutable for
            // the entire session. Do not carry KEEP_CACHE into a writable mount.
            Ok(()) => reply.opened(FileHandle(0), FopenFlags::FOPEN_KEEP_CACHE),
            Err(errno) => reply.error(errno),
        }
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        match self.check_open(ino, flags, true) {
            Ok(()) => reply.opened(FileHandle(0), FopenFlags::empty()),
            Err(errno) => reply.error(errno),
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.read_bytes(ino, offset, size) {
            Ok(data) => reply.data(&data),
            Err(errno) => reply.error(errno),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        match self.entries(ino) {
            Ok(entries) => {
                for (index, (child, kind, name)) in entries.enumerate() {
                    if (index as u64) < offset {
                        continue;
                    }
                    if reply.add(child, index as u64 + 1, kind, name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(errno) => reply.error(errno),
        }
    }
}

/// Mount in the foreground. Import first; the exclusive store lock prevents
/// offline commands from changing the catalog until this session ends.
pub fn mount(store_path: &Path, mountpoint: &Path) -> Result<()> {
    if cfg!(all(target_os = "macos", feature = "fuse-check")) {
        return Err(Error::Invalid("fuse-check disables mounting on macOS; install macFUSE and build with --features fuse (without fuse-check)".into()));
    }
    let root = store_path.canonicalize()?;
    let mountpoint = mountpoint.canonicalize()?;
    if root.starts_with(&mountpoint) || mountpoint.starts_with(&root) {
        return Err(Error::Invalid(
            "store and mountpoint must not contain each other".into(),
        ));
    }
    if std::fs::read_dir(&mountpoint)?.next().is_some() {
        return Err(Error::Invalid(
            "mountpoint must be an empty directory".into(),
        ));
    }
    // These libc calls have no memory-safety preconditions.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let fs = ReadOnlyFs::new(Store::open(&root)?, uid, gid)?;
    let mut config = fuser::Config::default();
    config.mount_options = vec![
        fuser::MountOption::RO,
        fuser::MountOption::DefaultPermissions,
        fuser::MountOption::NoDev,
        fuser::MountOption::NoSuid,
        fuser::MountOption::NoExec,
        fuser::MountOption::NoAtime,
        fuser::MountOption::FSName("sl7".into()),
    ];
    fuser::mount(fs, &mountpoint, &config)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture() -> (tempfile::TempDir, ReadOnlyFs, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..100_000)
            .map(|i| ((i * 17 + i / 257) % 251) as u8)
            .collect();
        let source = dir.path().join("source");
        fs::write(&source, &bytes).unwrap();
        let mut store = Store::init(&dir.path().join("store")).unwrap();
        store
            .put("main", "debug/deps/library.rmeta", &source)
            .unwrap();
        store
            .put("branch", "debug/deps/library.rmeta", &source)
            .unwrap();
        (dir, ReadOnlyFs::new(store, 123, 456).unwrap(), bytes)
    }

    fn resolve(fs: &ReadOnlyFs, path: &str) -> INodeNo {
        path.split('/').fold(INodeNo::ROOT, |parent, part| {
            fs.lookup_inode(parent, OsStr::new(part)).unwrap()
        })
    }

    #[test]
    fn namespace_is_independent_but_bytes_are_shared() {
        let (dir, fs, bytes) = fixture();
        let a = resolve(&fs, "main/debug/deps/library.rmeta");
        let b = resolve(&fs, "branch/debug/deps/library.rmeta");
        assert_ne!(a, b);
        let attr = fs.attributes(a).unwrap();
        assert_eq!(
            (attr.size, attr.perm, attr.uid, attr.gid),
            (100_000, 0o444, 123, 456)
        );
        for ino in [a, b] {
            for (offset, size) in [
                (0, 100_000),
                (32760, 12345),
                (99990, 100),
                (100000, 10),
                (u64::MAX, 10),
            ] {
                let start = (offset as usize).min(bytes.len());
                let end = start.saturating_add(size as usize).min(bytes.len());
                assert_eq!(fs.read_bytes(ino, offset, size).unwrap(), bytes[start..end]);
            }
        }
        assert!(matches!(
            Store::open(&dir.path().join("store")),
            Err(Error::Locked(_))
        ));
        drop(fs);
        Store::open(&dir.path().join("store")).unwrap();
    }

    #[test]
    fn directory_cookies_and_errno_contracts() {
        let (_dir, fs, _) = fixture();
        let entries: Vec<_> = fs.entries(INodeNo::ROOT).unwrap().collect();
        assert_eq!(
            entries.iter().map(|e| e.2).collect::<Vec<_>>(),
            [".", "..", "branch", "main"]
        );
        for cookie in 0..=entries.len() {
            let resumed: Vec<_> = fs.entries(INodeNo::ROOT).unwrap().skip(cookie).collect();
            assert_eq!(resumed, entries[cookie..]);
        }
        let file = resolve(&fs, "main/debug/deps/library.rmeta");
        assert_eq!(
            fs.check_open(file, OpenFlags(libc::O_RDONLY), false),
            Ok(())
        );
        for flags in [libc::O_WRONLY, libc::O_RDWR, libc::O_TRUNC, libc::O_APPEND] {
            assert_eq!(
                fs.check_open(file, OpenFlags(flags), false),
                Err(Errno::EROFS)
            );
        }
        assert_eq!(fs.read_bytes(INodeNo::ROOT, 0, 1), Err(Errno::EISDIR));
        assert_eq!(fs.read_bytes(INodeNo(0), 0, 1), Err(Errno::ENOENT));
        assert_eq!(
            fs.read_bytes(file, 0, 16 * 1024 * 1024 + 1),
            Err(Errno::EINVAL)
        );
        assert_eq!(
            fs.lookup_inode(file, OsStr::new("child")),
            Err(Errno::ENOTDIR)
        );
        assert_eq!(fs.check_open(file, OpenFlags(0), true), Err(Errno::ENOTDIR));
        assert_eq!(
            fs.check_open(INodeNo::ROOT, OpenFlags(0), false),
            Err(Errno::EISDIR)
        );
        assert_eq!(
            fs.lookup_inode(INodeNo::ROOT, OsStr::new("missing")),
            Err(Errno::ENOENT)
        );
        let deps = resolve(&fs, "main/debug/deps");
        assert_eq!(
            fs.lookup_inode(deps, OsStr::new("..")),
            Ok(resolve(&fs, "main/debug"))
        );
        assert_eq!(fs.attributes(INodeNo::ROOT).unwrap().nlink, 4);
    }

    #[test]
    fn ambiguous_paths_fail_before_mounting() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"bytes").unwrap();
        let mut cases = vec![
            ("bad/view", "valid".to_string()),
            ("..", "valid".into()),
            ("main", "/absolute".into()),
            ("main", "a//b".into()),
            ("main", "a/../b".into()),
            ("main", "./a".into()),
            ("main", "trailing/".into()),
            ("main", "nul\0byte".into()),
        ];
        cases.push(("main", "a".repeat(256)));
        for (i, (view, name)) in cases.into_iter().enumerate() {
            let mut store = Store::init(&dir.path().join(format!("store{i}"))).unwrap();
            store.put(view, &name, &source).unwrap();
            assert!(matches!(
                ReadOnlyFs::new(store, 0, 0),
                Err(Error::Invalid(_))
            ));
        }
        let mut store = Store::init(&dir.path().join("conflict")).unwrap();
        store.put("main", "a", &source).unwrap();
        store.put("main", "a/b", &source).unwrap();
        assert!(matches!(
            ReadOnlyFs::new(store, 0, 0),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn missing_pack_becomes_io_error_not_zero_filled_data() {
        let (dir, fs, _) = fixture();
        let file = resolve(&fs, "main/debug/deps/library.rmeta");
        for entry in fs::read_dir(dir.path().join("store/packs")).unwrap() {
            fs::remove_file(entry.unwrap().path()).unwrap();
        }
        assert_eq!(fs.read_bytes(file, 0, 4096), Err(Errno::EIO));
    }
}
