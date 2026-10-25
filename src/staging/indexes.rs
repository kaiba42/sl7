//! Concurrent inode/identity lookups. Namespace registration, reclamation and
//! representation changes remain coordinated by State; native metadata reads
//! do not acquire it. Never carry a DashMap guard into another map operation,
//! filesystem I/O, or acquisition of the State mutex.
use super::*;

#[derive(Default)]
pub(super) struct Requests {
    lookups: AtomicU64,
    getattrs: AtomicU64,
    indexed_lookups: AtomicU64,
    indexed_getattrs: AtomicU64,
    lookup_misses: AtomicU64,
}

pub(super) fn native_attr(ino: INodeNo, m: &Metadata) -> io::Result<FileAttr> {
    Ok(FileAttr {
        ino,
        size: m.size(),
        blocks: m.blocks(),
        atime: timestamp(m.atime(), m.atime_nsec()),
        mtime: timestamp(m.mtime(), m.mtime_nsec()),
        ctime: timestamp(m.ctime(), m.ctime_nsec()),
        crtime: UNIX_EPOCH,
        kind: kind(m)?,
        perm: (m.mode() & 0o7777) as u16,
        nlink: m.nlink().try_into().map_err(|_| error(libc::EOVERFLOW))?,
        uid: m.uid(),
        gid: m.gid(),
        rdev: 0,
        blksize: m.blksize() as u32,
        flags: 0,
    })
}

impl State {
    pub(super) fn stats_snapshot(&self) -> background::Stats {
        let mut stats = self.seal_stats.clone();
        stats.lookup_operations += self.metadata_requests.lookups.load(Ordering::Relaxed);
        stats.getattr_operations += self.metadata_requests.getattrs.load(Ordering::Relaxed);
        stats.indexed_lookups = self
            .metadata_requests
            .indexed_lookups
            .load(Ordering::Relaxed);
        stats.indexed_getattrs = self
            .metadata_requests
            .indexed_getattrs
            .load(Ordering::Relaxed);
        stats.indexed_lookup_misses = self.metadata_requests.lookup_misses.load(Ordering::Relaxed);
        stats
    }
}

impl StagingFs {
    /// A descriptor pins the native inode while no index guard is held. Before
    /// replying, validate that no mutation/publication changed the snapshot.
    /// Reference acquisition and reclaim's remove_if share the shard write lock.
    fn indexed_native_attr(
        &self,
        ino: INodeNo,
        reference: bool,
        ready: impl FnOnce(),
    ) -> io::Result<Option<FileAttr>> {
        let (pin, version) = {
            let Some(node) = self.nodes.get(&ino) else {
                return Ok(None);
            };
            if !node.native_unoverridden || node.metadata_busy {
                return Ok(None);
            }
            (Arc::clone(&node.pin), node.epoch.load(Ordering::Relaxed))
        };
        ready();
        let attr = native_attr(ino, &pin.metadata()?)?;
        let valid = |node: &Inode| {
            node.native_unoverridden
                && !node.metadata_busy
                && node.epoch.load(Ordering::Relaxed) == version
        };
        if reference {
            let Some(mut node) = self.nodes.get_mut(&ino) else {
                return Ok(None);
            };
            if !valid(&node) {
                return Ok(None);
            }
            node.references = node
                .references
                .checked_add(1)
                .ok_or_else(|| error(libc::EOVERFLOW))?;
        } else if !self.nodes.get(&ino).is_some_and(|node| valid(&node)) {
            return Ok(None);
        }
        Ok(Some(attr))
    }

    pub(super) fn getattr_indexed(&self, ino: INodeNo) -> std::result::Result<FileAttr, Errno> {
        self.metadata_requests
            .getattrs
            .fetch_add(1, Ordering::Relaxed);
        if self.state.is_poisoned() {
            return Err(Errno::EIO);
        }
        if let Some(attr) = self
            .indexed_native_attr(ino, false, || {})
            .map_err(Errno::from)?
        {
            self.metadata_requests
                .indexed_getattrs
                .fetch_add(1, Ordering::Relaxed);
            return Ok(attr);
        }
        self.with_read(|state| state.attr(ino))
    }

    pub(super) fn lookup_indexed(
        &self,
        parent: INodeNo,
        name: &OsStr,
    ) -> std::result::Result<FileAttr, Errno> {
        self.metadata_requests
            .lookups
            .fetch_add(1, Ordering::Relaxed);
        if self.state.is_poisoned() {
            return Err(Errno::EIO);
        }
        // Own the parent descriptor across pathname resolution. The parent may
        // be renamed or forgotten concurrently without reusing this fd.
        let parent_pin = self
            .nodes
            .get(&parent)
            .map(|node| Arc::clone(&node.pin))
            .ok_or(Errno::ENOENT)?;
        if !parent_pin.metadata().map_err(Errno::from)?.is_dir() {
            return Err(Errno::ENOTDIR);
        }
        if name == "." || (name == ".." && parent == INodeNo::ROOT) {
            if let Some(attr) = self
                .indexed_native_attr(parent, true, || {})
                .map_err(Errno::from)?
            {
                self.metadata_requests
                    .indexed_lookups
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(attr);
            }
            return self.with_read(|state| state.entry_attr(parent));
        }
        if name.is_empty() || name.as_bytes().contains(&b'/') || name.as_bytes().contains(&0) {
            return Err(Errno::EINVAL);
        }
        let file = match pin(&fd_path(&parent_pin).join(name)) {
            Ok(file) => file,
            Err(e) => {
                if e.raw_os_error() == Some(libc::ENOENT) {
                    self.metadata_requests
                        .lookup_misses
                        .fetch_add(1, Ordering::Relaxed);
                }
                return Err(e.into());
            }
        };
        let meta = file.metadata().map_err(Errno::from)?;
        let ino = self
            .identities
            .get(&(meta.dev(), meta.ino()))
            .map(|entry| *entry);
        if let Some(ino) = ino {
            if let Some(attr) = self
                .indexed_native_attr(ino, true, || {})
                .map_err(Errno::from)?
            {
                self.metadata_requests
                    .indexed_lookups
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(attr);
            }
        }
        // Cold identities and sealed/logically overridden inodes use the
        // existing authoritative registration/catalog path. The pin preserves
        // the inode resolved above even if its directory name changes.
        self.with_read(move |state| {
            let ino = state.register(file)?;
            state.entry_attr(ino)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    fn fixture() -> (tempfile::TempDir, StagingFs, INodeNo) {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("data"), b"indexed payload").unwrap();
        let fs = StagingFs::new(temp.path()).unwrap();
        let ino = fs
            .with_read(|state| {
                let ino = state.lookup(INodeNo::ROOT, OsStr::new("data"))?;
                state.entry_attr(ino)?;
                Ok(ino)
            })
            .unwrap();
        (temp, fs, ino)
    }

    #[test]
    fn metadata_hits_and_absent_names_progress_while_state_is_locked() {
        let (_temp, fs, ino) = fixture();
        thread::scope(|scope| {
            let state = fs.state.lock().unwrap();
            let (send, receive) = mpsc::channel();
            let fs = &fs;
            let worker = scope.spawn(move || {
                let found = fs.lookup_indexed(INodeNo::ROOT, OsStr::new("data"));
                let attr = fs.getattr_indexed(ino);
                let absent = fs.lookup_indexed(INodeNo::ROOT, OsStr::new("absent"));
                send.send((found, attr, absent)).unwrap();
            });
            let result = receive.recv_timeout(Duration::from_secs(5));
            // Release before asserting, so a regression cannot strand a worker.
            drop(state);
            worker.join().unwrap();
            let (found, attr, absent) = result.expect("metadata still waits on global State");
            assert_eq!(found.unwrap().ino, ino);
            assert_eq!(attr.unwrap().size, 15);
            assert!(matches!(absent, Err(Errno::ENOENT)));
        });
        let state = fs.state.lock().unwrap();
        assert_eq!(state.nodes.get(&ino).unwrap().references, 2);
        let stats = state.stats_snapshot();
        assert_eq!(stats.lookup_operations, 2);
        assert_eq!(stats.getattr_operations, 1);
        assert_eq!(stats.indexed_lookups, 1);
        assert_eq!(stats.indexed_getattrs, 1);
        assert_eq!(stats.indexed_lookup_misses, 1);
    }

    #[test]
    fn partial_metadata_mutation_excludes_only_its_inode_from_the_fast_path() {
        let (temp, fs, ino) = fixture();
        fs::write(temp.path().join("other"), b"other").unwrap();
        let other = fs
            .lookup_indexed(INodeNo::ROOT, OsStr::new("other"))
            .unwrap()
            .ino;
        let (ready, entered) = mpsc::channel();
        let (resume, paused) = mpsc::channel();
        thread::scope(|scope| {
            let fs = &fs;
            let mutation = scope.spawn(move || {
                fs.with_mutation(
                    |_| Ok(vec![ino]),
                    |state| {
                        state.invalidate(ino)?;
                        fs::write(state.path(ino)?, b"new")?;
                        ready.send(()).unwrap();
                        paused.recv_timeout(Duration::from_secs(10)).unwrap();
                        Ok(())
                    },
                )
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let busy = fs.indexed_native_attr(ino, false, || {});
            let independent = fs.getattr_indexed(other);
            resume.send(()).unwrap();
            mutation.join().unwrap().unwrap();
            assert!(busy.unwrap().is_none());
            assert_eq!(independent.unwrap().size, 5);
        });
        assert_eq!(fs.getattr_indexed(ino).unwrap().size, 3);
        let failed: std::result::Result<(), Errno> =
            fs.with_mutation(|_| Ok(vec![ino]), |_| Err(error(libc::EIO)));
        assert_eq!(failed, Err(Errno::EIO));
        assert!(!fs.nodes.get(&ino).unwrap().metadata_busy);
        assert_eq!(
            fs.indexed_native_attr(ino, false, || {})
                .unwrap()
                .unwrap()
                .size,
            3
        );
    }

    #[test]
    fn representation_change_rejects_unlocked_metadata_snapshot() {
        let (_temp, fs, ino) = fixture();
        let attr = fs
            .indexed_native_attr(ino, true, || {
                // Represents publication clearing the native-only shortcut before
                // its catalog commit. Acquiring this guard also proves snapshot I/O
                // does not retain a read guard on the same DashMap shard.
                let mut node = fs.nodes.get_mut(&ino).unwrap();
                node.native_unoverridden = false;
                node.epoch.fetch_add(1, Ordering::Relaxed);
            })
            .unwrap();
        assert!(attr.is_none());
        assert_eq!(fs.nodes.get(&ino).unwrap().references, 1);
    }

    #[test]
    fn reclaimed_snapshot_does_not_revive_an_inode_or_leak_a_reference() {
        let (_temp, fs, ino) = fixture();
        let attr = fs
            .indexed_native_attr(ino, true, || {
                fs.with_read(|state| {
                    state.forget(ino, 1);
                    Ok(())
                })
                .unwrap();
            })
            .unwrap();
        assert!(attr.is_none());
        assert!(!fs.nodes.contains_key(&ino));
        let next = fs
            .lookup_indexed(INodeNo::ROOT, OsStr::new("data"))
            .unwrap();
        assert_ne!(next.ino, ino);
        assert_eq!(next.size, 15);
        assert_eq!(fs.nodes.get(&next.ino).unwrap().references, 1);
    }

    #[test]
    fn concurrent_lookup_forget_and_reclaim_keep_referenced_inodes_registered() {
        let (_temp, fs, ino) = fixture();
        fs.with_read(|state| {
            state.forget(ino, 1);
            Ok(())
        })
        .unwrap();
        thread::scope(|scope| {
            for _ in 0..4 {
                let fs = &fs;
                scope.spawn(move || {
                    for _ in 0..500 {
                        let attr = fs
                            .lookup_indexed(INodeNo::ROOT, OsStr::new("data"))
                            .unwrap();
                        assert_eq!(fs.getattr_indexed(attr.ino).unwrap().size, 15);
                        fs.with_read(|state| {
                            state.forget(attr.ino, 1);
                            Ok(())
                        })
                        .unwrap();
                    }
                });
            }
        });
        assert_eq!(fs.nodes.len(), 1);
        assert_eq!(fs.identities.len(), 1);
    }
}
