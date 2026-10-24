//! Disposable one-file FUSE experiment; not an SL7 storage implementation.
use fuser::*;
use std::{
    collections::HashSet,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, UNIX_EPOCH},
};
#[derive(Default)]
struct State {
    backing: Option<BackingId>,
    handles: HashSet<FileHandle>,
    next: u64,
}
struct Probe {
    path: PathBuf,
    events: PathBuf,
    state: Mutex<State>,
}
impl Probe {
    fn event(&self, event: &str) {
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events)
            .unwrap();
        writeln!(f, "{event}").unwrap();
    }
    fn attr(&self, ino: INodeNo) -> io::Result<FileAttr> {
        let m = fs::metadata(if ino == INodeNo::ROOT {
            self.path.parent().unwrap()
        } else {
            &self.path
        })?;
        Ok(FileAttr {
            ino,
            size: m.size(),
            blocks: m.blocks(),
            atime: m.accessed()?,
            mtime: m.modified()?,
            ctime: UNIX_EPOCH + Duration::new(m.ctime() as u64, m.ctime_nsec() as u32),
            crtime: UNIX_EPOCH,
            kind: if ino == INodeNo::ROOT {
                FileType::Directory
            } else {
                FileType::RegularFile
            },
            perm: (m.mode() & 0o7777) as u16,
            nlink: m.nlink() as u32,
            uid: m.uid(),
            gid: m.gid(),
            rdev: m.rdev() as u32,
            blksize: m.blksize() as u32,
            flags: 0,
        })
    }
}
impl Filesystem for Probe {
    fn init(&mut self, _: &Request, config: &mut KernelConfig) -> io::Result<()> {
        config
            .set_max_stack_depth(1)
            .map_err(|_| io::Error::other("stack depth rejected"))?;
        config
            .add_capabilities(InitFlags::FUSE_PASSTHROUGH)
            .map_err(|_| io::Error::other("passthrough unavailable"))?;
        self.event("init");
        Ok(())
    }
    fn lookup(&self, _: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent != INodeNo::ROOT || name != "data" {
            reply.error(Errno::ENOENT);
            return;
        }
        match self.attr(INodeNo(2)) {
            Ok(a) => reply.entry(&Duration::ZERO, &a, Generation(0)),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn getattr(&self, _: &Request, ino: INodeNo, _: Option<FileHandle>, reply: ReplyAttr) {
        if ino != INodeNo::ROOT && ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        match self.attr(ino) {
            Ok(a) => reply.attr(&Duration::ZERO, &a),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn open(&self, _: &Request, ino: INodeNo, _: OpenFlags, reply: ReplyOpen) {
        if ino != INodeNo(2) {
            reply.error(Errno::ENOENT);
            return;
        }
        let mut state = self.state.lock().unwrap();
        if state.backing.is_none() {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .unwrap();
            match reply.open_backing(&file) {
                Ok(id) => state.backing = Some(id),
                Err(e) => {
                    self.event(&format!("register_error {e}"));
                    reply.error(Errno::EIO);
                    return;
                }
            }
            self.event("backing_register");
        }
        state.next += 1;
        let fh = FileHandle(state.next);
        assert!(state.handles.insert(fh));
        self.event("open");
        reply.opened_passthrough(fh, FopenFlags::empty(), state.backing.as_ref().unwrap());
    }
    fn flush(&self, _: &Request, _: INodeNo, _: FileHandle, _: LockOwner, reply: ReplyEmpty) {
        reply.ok();
    }
    fn release(
        &self,
        _: &Request,
        _: INodeNo,
        fh: FileHandle,
        _: OpenFlags,
        _: Option<LockOwner>,
        _: bool,
        reply: ReplyEmpty,
    ) {
        let mut state = self.state.lock().unwrap();
        assert!(state.handles.remove(&fh));
        if state.handles.is_empty() {
            state.backing.take();
            self.event("backing_close");
        }
        self.event("release");
        reply.ok();
    }
    fn destroy(&mut self) {
        self.event("destroy");
    }
}
fn main() -> io::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    assert_eq!(args.len(), 4);
    let probe = Probe {
        path: PathBuf::from(&args[1]),
        events: PathBuf::from(&args[3]),
        state: Mutex::default(),
    };
    let mut config = Config::default();
    config.n_threads = Some(2);
    config.mount_options = vec![
        MountOption::RW,
        MountOption::DefaultPermissions,
        MountOption::NoAtime,
        MountOption::FSName("sl7-passthrough-proof".into()),
    ];
    fuser::mount(probe, &args[2], &config)
}
