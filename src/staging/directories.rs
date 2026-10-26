//! Directory snapshots pin native identities before registering them. Only
//! registration and reference transfers need State; native I/O does not.
use super::*;

const BATCH: usize = 64;

struct Snapshot<'a> {
    fs: &'a StagingFs,
    ino: INodeNo,
    entries: Vec<DirectoryEntry>,
    armed: bool,
}

impl Snapshot<'_> {
    fn retain(&mut self, state: &mut State, entry: DirectoryEntry) -> io::Result<()> {
        let mut node = state.nodes.get_mut(&entry.ino).unwrap();
        node.references = node
            .references
            .checked_add(1)
            .ok_or_else(|| error(libc::EOVERFLOW))?;
        self.entries.push(entry);
        Ok(())
    }

    fn finish(mut self, file: File) -> Result<FileHandle, Errno> {
        self.fs.with_read(|state| {
            let fh = FileHandle(state.next_handle);
            state.next_handle = state
                .next_handle
                .checked_add(1)
                .ok_or_else(|| error(libc::EOVERFLOW))?;
            state.handles.insert(
                fh,
                Handle {
                    ino: self.ino,
                    file: Arc::new(file),
                    entries: Some(std::mem::take(&mut self.entries)),
                },
            );
            // Transfer the temporary open and all entry references to Handle.
            self.armed = false;
            Ok(fh)
        })
    }
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.fs.with_read(|state| {
                for entry in &self.entries {
                    state.forget(entry.ino, 1);
                }
                state.nodes.get_mut(&self.ino).unwrap().opens -= 1;
                state.reclaim(self.ino);
                Ok(())
            });
        }
    }
}

impl StagingFs {
    // The hook makes namespace races deterministic in tests. Production uses a
    // no-op, optimized away. The opened descriptor remains anchored if renamed.
    pub(super) fn open_directory(
        &self,
        ino: INodeNo,
        enumerated: impl FnOnce(),
    ) -> Result<FileHandle, Errno> {
        let pinned = self.with_read(|state| {
            let pinned = state.inode(ino)?;
            let mut node = state.nodes.get_mut(&ino).unwrap();
            node.opens = node
                .opens
                .checked_add(1)
                .ok_or_else(|| error(libc::EOVERFLOW))?;
            Ok(pinned)
        })?;
        let mut snapshot = Snapshot {
            fs: self,
            ino,
            entries: Vec::new(),
            armed: true,
        };
        let opened = profile::span("directory_open");
        let file = open_native(&fd_path(&pinned), libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        let path = fd_path(&file);
        // ROOT's parent must never escape the private tree.
        let parent = if ino == INodeNo::ROOT {
            pinned
        } else {
            Arc::new(pin(&path.join(".."))?)
        };
        let metadata = parent.metadata()?;
        self.with_read(|state| {
            snapshot.retain(
                state,
                DirectoryEntry {
                    ino,
                    kind: FileType::Directory,
                    name: ".".into(),
                },
            )?;
            let parent = state.register_metadata(parent.clone(), &metadata)?;
            snapshot.retain(
                state,
                DirectoryEntry {
                    ino: parent,
                    kind: FileType::Directory,
                    name: "..".into(),
                },
            )
        })?;
        drop(opened);
        let enumerate = profile::span("directory_enumerate");
        let mut names = fs::read_dir(&path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        drop(enumerate);
        let sort = profile::span("directory_sort");
        names.sort();
        drop(sort);
        enumerated();
        for names in names.chunks(BATCH) {
            let pinning = profile::span("directory_child_pin");
            let mut children = Vec::with_capacity(names.len());
            for name in names {
                let file = match pin(&path.join(name)) {
                    Ok(file) => file,
                    // Concurrent unlink/rename is allowed during enumeration.
                    Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
                    Err(e) => return Err(e.into()),
                };
                let metadata = file.metadata()?;
                let kind = kind(&metadata)?;
                children.push((name, Arc::new(file), metadata, kind));
            }
            drop(pinning);
            self.with_read(|state| {
                let _register = profile::span("directory_register");
                for (name, file, metadata, kind) in &children {
                    let child = state.register_metadata(file.clone(), metadata)?;
                    snapshot.retain(
                        state,
                        DirectoryEntry {
                            ino: child,
                            kind: *kind,
                            name: (*name).clone(),
                        },
                    )?;
                }
                Ok(())
            })?;
            // Redundant pins are closed outside State, including on error.
        }
        let _handle = profile::span("directory_handle");
        snapshot.finish(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, sync::mpsc, thread};

    fn lookup(fs: &StagingFs, name: &str) -> INodeNo {
        fs.with_read(|state| {
            let ino = state.lookup(INodeNo::ROOT, OsStr::new(name))?;
            state.entry_attr(ino)?;
            Ok(ino)
        })
        .unwrap()
    }

    #[test]
    fn snapshot_retains_hardlinks_and_symlinks_until_release() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..150 {
            fs::write(temp.path().join(format!("file{n:03}")), b"x").unwrap();
        }
        fs::hard_link(temp.path().join("file000"), temp.path().join("hardlink")).unwrap();
        symlink("missing", temp.path().join("symlink")).unwrap();
        let fs = StagingFs::new(temp.path()).unwrap();
        let fh = fs.open_directory(INodeNo::ROOT, || {}).unwrap();
        let mut state = fs.state.lock().unwrap();
        let entries = state
            .handle(INodeNo::ROOT, fh)
            .unwrap()
            .entries
            .as_ref()
            .unwrap();
        assert_eq!(entries.len(), 154);
        let entry = |name: &str| entries.iter().find(|e| e.name == name).unwrap();
        let child = entry("file000").ino;
        assert_eq!(child, entry("hardlink").ino);
        assert_eq!(entry("symlink").kind, FileType::Symlink);
        assert_eq!(entry("..").ino, INodeNo::ROOT);
        fs::remove_file(temp.path().join("file000")).unwrap();
        fs::remove_file(temp.path().join("hardlink")).unwrap();
        assert_eq!(state.nodes.get(&child).unwrap().references, 2);
        assert_eq!(state.inode(child).unwrap().metadata().unwrap().nlink(), 0);
        state.release(INodeNo::ROOT, fh).unwrap();
        assert_eq!(state.nodes.len(), 1);
        assert_eq!(state.identities.len(), 1);
        assert_eq!(state.nodes.get(&INodeNo::ROOT).unwrap().opens, 0);
    }

    #[test]
    fn namespace_and_unrelated_mutations_progress_during_enumeration() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("dir")).unwrap();
        fs::write(temp.path().join("dir/gone"), b"x").unwrap();
        fs::write(temp.path().join("dir/replaced"), b"x").unwrap();
        fs::write(temp.path().join("data"), b"old").unwrap();
        let fs = StagingFs::new(temp.path()).unwrap();
        let ino = lookup(&fs, "dir");
        let data = lookup(&fs, "data");
        let (ready, entered) = mpsc::channel();
        let (resume, paused) = mpsc::channel();
        thread::scope(|scope| {
            let fs = &fs;
            let snapshot = scope.spawn(move || {
                fs.open_directory(ino, || {
                    ready.send(()).unwrap();
                    paused.recv_timeout(Duration::from_secs(10)).unwrap();
                })
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done, completed) = mpsc::channel();
            let mutation = scope.spawn(move || {
                let result = fs.with_mutation(
                    |_| Ok(vec![data]),
                    |state| {
                        fs::write(state.path(data)?, b"new")?;
                        let dir = state.path(INodeNo::ROOT)?.join("dir");
                        fs::remove_file(dir.join("gone"))?;
                        fs::remove_file(dir.join("replaced"))?;
                        symlink("missing", dir.join("replaced"))?;
                        fs::rename(&dir, dir.with_file_name("moved"))?;
                        fs::create_dir(&dir)?;
                        fs::write(dir.join("wrong-directory"), b"wrong")?;
                        state.forget(ino, 1);
                        Ok(())
                    },
                );
                done.send(result).unwrap();
            });
            let result = completed.recv_timeout(Duration::from_secs(5));
            resume.send(()).unwrap();
            mutation.join().unwrap();
            let fh = snapshot.join().unwrap().unwrap();
            result.expect("directory I/O held State").unwrap();
            let mut state = fs.state.lock().unwrap();
            let entries = state.handle(ino, fh).unwrap().entries.as_ref().unwrap();
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[2].name, "replaced");
            assert_eq!(entries[2].kind, FileType::Symlink);
            assert_eq!(state.attr(data).unwrap().size, 3);
            state.release(ino, fh).unwrap();
            assert!(!state.nodes.contains_key(&ino));
        });
    }

    #[test]
    fn errors_roll_back_all_batches_and_temporary_open() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..150 {
            fs::write(temp.path().join(format!("file{n:03}")), b"x").unwrap();
        }
        let fs = StagingFs::new(temp.path()).unwrap();
        fs.state.lock().unwrap().next_handle = u64::MAX;
        assert_eq!(
            fs.open_directory(INodeNo::ROOT, || {}),
            Err(Errno::EOVERFLOW)
        );
        let state = fs.state.lock().unwrap();
        assert_eq!(state.nodes.len(), 1);
        assert_eq!(state.identities.len(), 1);
        assert!(state.handles.is_empty());
        let root = state.nodes.get(&INodeNo::ROOT).unwrap();
        assert_eq!(root.opens, 0);
        assert_eq!(root.references, 0);
    }

    #[test]
    fn unsupported_child_after_first_batch_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        for n in 0..70 {
            fs::write(temp.path().join(format!("file{n:03}")), b"x").unwrap();
        }
        let _socket = std::os::unix::net::UnixListener::bind(temp.path().join("z-socket")).unwrap();
        let fs = StagingFs::new(temp.path()).unwrap();
        assert_eq!(
            fs.open_directory(INodeNo::ROOT, || {}),
            Err(Errno::EOPNOTSUPP)
        );
        let state = fs.state.lock().unwrap();
        assert_eq!(state.nodes.len(), 1);
        assert_eq!(state.identities.len(), 1);
        assert_eq!(state.nodes.get(&INodeNo::ROOT).unwrap().opens, 0);
    }

    #[test]
    fn removed_directory_survives_forget_until_snapshot_release() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("dir")).unwrap();
        let fs = StagingFs::new(temp.path()).unwrap();
        let ino = lookup(&fs, "dir");
        let fh = fs
            .open_directory(ino, || {
                fs::remove_dir(temp.path().join("dir")).unwrap();
                fs.with_read(|s| {
                    s.forget(ino, 1);
                    Ok(())
                })
                .unwrap();
            })
            .unwrap();
        let mut state = fs.state.lock().unwrap();
        assert_eq!(
            state
                .handle(ino, fh)
                .unwrap()
                .entries
                .as_ref()
                .unwrap()
                .len(),
            2
        );
        state.release(ino, fh).unwrap();
        assert!(!state.nodes.contains_key(&ino));
    }
}
