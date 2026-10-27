//! Concurrent reconstruction with independent catalogs and inode-scoped read leases.
use super::*;
use crate::live::Live;
use std::ops::Deref;

pub(super) const REQUEST_WORKERS: usize = 4;

pub(super) struct Readers {
    available: Mutex<Vec<Live>>,
    returned: Condvar,
}
impl Readers {
    pub(super) fn new(store: &crate::Store) -> crate::Result<Self> {
        let mut readers = Vec::with_capacity(REQUEST_WORKERS);
        for _ in 0..REQUEST_WORKERS {
            readers.push(Live::open(store.reader_connection()?)?);
        }
        Ok(Self {
            available: Mutex::new(readers),
            returned: Condvar::new(),
        })
    }
    fn take(&self) -> io::Result<Reader<'_>> {
        let mut available = self.available.lock().map_err(|_| error(libc::EIO))?;
        loop {
            if let Some(live) = available.pop() {
                return Ok(Reader {
                    pool: self,
                    live: Some(live),
                });
            }
            available = self
                .returned
                .wait(available)
                .map_err(|_| error(libc::EIO))?;
        }
    }
}
struct Reader<'a> {
    pool: &'a Readers,
    live: Option<Live>,
}
impl Deref for Reader<'_> {
    type Target = Live;
    fn deref(&self) -> &Self::Target {
        self.live.as_ref().unwrap()
    }
}
impl Drop for Reader<'_> {
    fn drop(&mut self) {
        if let Ok(mut available) = self.pool.available.lock() {
            available.push(self.live.take().unwrap());
            self.pool.returned.notify_one();
        }
    }
}

/// Keeps both the inode and its current representation alive even if release or
/// forget arrives while the read is in flight. Same-inode mutations wait for it;
/// readers of other inodes need neither its completion nor a catalog writer.
struct ReadLease<'a> {
    fs: &'a StagingFs,
    ino: INodeNo,
    started: Instant,
}
impl Drop for ReadLease<'_> {
    fn drop(&mut self) {
        let waiting = profile::span("cleanup_state_lock_wait");
        if let Ok(mut state) = self.fs.state.lock() {
            drop(waiting);
            let _phase = profile::span("cleanup_locked_work");
            let locked = Instant::now();
            let mut node = state.nodes.get_mut(&self.ino).unwrap();
            node.readers -= 1;
            node.opens -= 1;
            drop(node);
            state.seal_stats.active_reads -= 1;
            state.seal_stats.read_seconds += self.started.elapsed().as_secs_f64();
            state.schedule_seal(self.ino);
            state.reclaim(self.ino);
            let seconds = locked.elapsed().as_secs_f64();
            state.seal_stats.read_state_seconds += seconds;
            state.seal_stats.request_state_seconds += seconds;
            state.seal_stats.max_request_state_seconds =
                state.seal_stats.max_request_state_seconds.max(seconds);
            self.fs.publication_done.notify_all();
        }
    }
}

impl StagingFs {
    pub(super) fn read_handle(
        &self,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: usize,
    ) -> std::result::Result<Vec<u8>, Errno> {
        self.read_with(ino, fh, offset, size, || {})
    }
    fn read_with(
        &self,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: usize,
        ready: impl FnOnce(),
    ) -> std::result::Result<Vec<u8>, Errno> {
        if size > 16 * 1024 * 1024 {
            return Err(Errno::EINVAL);
        }
        let (file, pin, native, _lease) = {
            let waiting = profile::span("state_lock_wait");
            let mut state = self.state.lock().map_err(|_| Errno::EIO)?;
            drop(waiting);
            let _phase = profile::span("state_locked_work");
            let waited = Instant::now();
            let mut blocked = false;
            while state
                .nodes
                .get(&ino)
                .is_some_and(|n| n.waiting_mutations != 0 || n.exclusive_io)
            {
                blocked = true;
                let _gate = profile::span("inode_gate_wait");
                state = self.publication_done.wait(state).map_err(|_| Errno::EIO)?;
            }
            if blocked {
                state.seal_stats.read_wait_seconds += waited.elapsed().as_secs_f64();
            }
            let locked = Instant::now();
            let file = Arc::clone(&state.handle(ino, fh).map_err(Errno::from)?.file);
            let mut node = state.nodes.get_mut(&ino).ok_or(Errno::ENOENT)?;
            let pin = Arc::clone(&node.pin);
            let native = node.native_unoverridden;
            node.readers += 1;
            node.opens += 1;
            drop(node);
            state.seal_stats.read_operations += 1;
            state.seal_stats.active_reads += 1;
            state.seal_stats.max_parallel_reads = state
                .seal_stats
                .max_parallel_reads
                .max(state.seal_stats.active_reads);
            let seconds = locked.elapsed().as_secs_f64();
            state.seal_stats.read_state_seconds += seconds;
            state.seal_stats.request_state_seconds += seconds;
            state.seal_stats.max_request_state_seconds =
                state.seal_stats.max_request_state_seconds.max(seconds);
            (
                file,
                pin,
                native,
                ReadLease {
                    fs: self,
                    ino,
                    started: Instant::now(),
                },
            )
        };
        // Used by deterministic tests to pause a real read after pinning. There
        // is no callback or allocation here in the production instantiation.
        ready();
        if let Some(readers) = self.readers.as_ref().filter(|_| !native) {
            let waiting = profile::span("reader_pool_wait");
            let reader = readers.take().map_err(Errno::from)?;
            drop(waiting);
            let _reconstruct = profile::span("reconstruct");
            if let Some(bytes) = reader
                .read(&pin, offset, size)
                .map_err(storage)
                .map_err(Errno::from)?
            {
                return Ok(bytes);
            }
        }
        let _payload = profile::span("native_payload");
        let mut bytes = vec![0; size];
        let mut read = 0;
        while read < bytes.len() {
            match file.read_at(
                &mut bytes[read..],
                offset.checked_add(read as u64).ok_or(Errno::EINVAL)?,
            ) {
                Ok(0) => break,
                Ok(n) => read += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        bytes.truncate(read);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, sync::mpsc, thread};

    fn fixture(sealed: bool) -> (tempfile::TempDir, StagingFs, [(INodeNo, FileHandle); 2]) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs = StagingFs::deduplicating(crate::Store::init(&root).unwrap()).unwrap();
        for (name, byte) in [("a", 42), ("b", 77)] {
            fs::write(root.join("tree").join(name), vec![byte; 128 * 1024]).unwrap();
        }
        let handles = fs
            .with(|s| {
                let mut handles = Vec::new();
                for name in ["a", "b"] {
                    let ino = s.lookup(INodeNo::ROOT, OsStr::new(name))?;
                    s.entry_attr(ino)?;
                    if sealed {
                        let pin = Arc::clone(&s.nodes.get(&ino).unwrap().pin);
                        s.live.as_mut().unwrap().seal(&pin).map_err(storage)?;
                    }
                    handles.push((ino, s.open(ino, libc::O_RDONLY)?));
                }
                Ok(handles.try_into().unwrap())
            })
            .unwrap();
        (temp, fs, handles)
    }

    #[test]
    fn independent_reads_run_while_same_inode_writer_waits() {
        for sealed in [false, true] {
            let (_temp, fs, [(a, ah), (b, bh)]) = fixture(sealed);
            let (ready, entered) = mpsc::channel();
            let (resume, paused) = mpsc::channel();
            thread::scope(|scope| {
                let fs = &fs;
                let reader = scope.spawn(move || {
                    fs.read_with(a, ah, 0, 128 * 1024, || {
                        ready.send(()).unwrap();
                        paused.recv_timeout(Duration::from_secs(10)).unwrap();
                    })
                });
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                let writer = scope.spawn(move || {
                    let fh = fs.open_handle(a, libc::O_RDWR).unwrap();
                    fs.write_handle(a, fh, 0, b"changed").unwrap();
                    fs.with_read(|s| s.release(a, fh)).unwrap();
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                let waiting = loop {
                    if fs
                        .with_read(|s| Ok(s.nodes.get(&a).unwrap().waiting_mutations > 0))
                        .unwrap()
                    {
                        break true;
                    }
                    if Instant::now() >= deadline {
                        break false;
                    }
                    thread::sleep(Duration::from_millis(1));
                };
                let (finished, independent) = mpsc::channel();
                scope.spawn(move || {
                    finished.send(fs.read_handle(b, bh, 0, 128 * 1024)).unwrap();
                });
                let result = independent.recv_timeout(Duration::from_secs(5));
                resume.send(()).unwrap();
                let original = reader.join().unwrap().unwrap();
                writer.join().unwrap();
                assert!(waiting, "writer bypassed the active inode read");
                assert_eq!(
                    result.expect("waiting writer held global state").unwrap(),
                    vec![77; 128 * 1024]
                );
                assert_eq!(original, vec![42; 128 * 1024]);
            });
            assert_eq!(fs.read_handle(a, ah, 0, 7).unwrap(), b"changed");
            fs.with_read(|s| {
                assert_eq!(s.seal_stats.active_reads, 0);
                assert!(s.seal_stats.max_parallel_reads >= 2);
                s.release(a, ah)?;
                s.release(b, bh)
            })
            .unwrap();
        }
    }

    #[test]
    fn read_survives_release_and_forget_before_reconstruction_finishes() {
        for sealed in [false, true] {
            let (_temp, fs, [(a, ah), (b, bh)]) = fixture(sealed);
            let (ready, entered) = mpsc::channel();
            let (resume, paused) = mpsc::channel();
            thread::scope(|scope| {
                let fs = &fs;
                let reader = scope.spawn(move || {
                    fs.read_with(a, ah, 0, 128 * 1024, || {
                        ready.send(()).unwrap();
                        paused.recv_timeout(Duration::from_secs(10)).unwrap();
                    })
                });
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                fs.with_read(|s| {
                    s.release(a, ah)?;
                    s.forget(a, 1);
                    assert_eq!(s.nodes.get(&a).unwrap().opens, 1);
                    assert_eq!(s.nodes.get(&a).unwrap().readers, 1);
                    assert!(!s.nodes.get(&a).unwrap().publishing);
                    Ok(())
                })
                .unwrap();
                resume.send(()).unwrap();
                assert_eq!(reader.join().unwrap().unwrap(), vec![42; 128 * 1024]);
            });
            fs.with_read(|s| s.release(b, bh)).unwrap();
            let root = fs
                .with_read(|s| Ok(s.live.as_ref().unwrap().store.root.clone()))
                .unwrap();
            drop(fs);
            crate::Store::open(&root).unwrap().check().unwrap();
        }
    }

    #[test]
    fn waiting_rename_rechecks_replaced_destination_before_mutation() {
        let (temp, fs, [(a, ah), (b, bh)]) = fixture(true);
        fs::write(temp.path().join("store/tree/c"), vec![99; 128 * 1024]).unwrap();
        let (c, ch) = fs
            .with_read(|s| {
                let c = s.lookup(INodeNo::ROOT, OsStr::new("c"))?;
                s.entry_attr(c)?;
                Ok((c, s.open(c, libc::O_RDONLY)?))
            })
            .unwrap();
        let waiting = |ino| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if fs
                    .with_read(|s| Ok(s.nodes.get(&ino).unwrap().waiting_mutations > 0))
                    .unwrap()
                {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                thread::sleep(Duration::from_millis(1));
            }
        };
        let (a_ready, a_entered) = mpsc::channel();
        let (a_resume, a_paused) = mpsc::channel();
        let (c_ready, c_entered) = mpsc::channel();
        let (c_resume, c_paused) = mpsc::channel();
        thread::scope(|scope| {
            let fs = &fs;
            let ar = scope.spawn(move || {
                fs.read_with(a, ah, 0, 128 * 1024, || {
                    a_ready.send(()).unwrap();
                    a_paused.recv_timeout(Duration::from_secs(10)).unwrap();
                })
            });
            a_entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let (done, completed) = mpsc::channel();
            let rename = scope.spawn(move || {
                let result = fs.with_mutation(
                    |s| {
                        Ok(vec![
                            s.lookup(INodeNo::ROOT, OsStr::new("a"))?,
                            s.lookup(INodeNo::ROOT, OsStr::new("b"))?,
                        ])
                    },
                    |s| {
                        let old = s.lookup(INodeNo::ROOT, OsStr::new("b"))?;
                        fs::rename(
                            s.child(INodeNo::ROOT, OsStr::new("a"))?,
                            s.child(INodeNo::ROOT, OsStr::new("b"))?,
                        )?;
                        s.namespace_changed(a)?;
                        s.namespace_changed(old)
                    },
                );
                done.send(()).unwrap();
                result
            });
            let waited_a = waiting(a);
            fs.with_mutation(
                |_| Ok(vec![b, c]),
                |s| {
                    fs::rename(
                        s.child(INodeNo::ROOT, OsStr::new("c"))?,
                        s.child(INodeNo::ROOT, OsStr::new("b"))?,
                    )?;
                    s.namespace_changed(b)?;
                    s.namespace_changed(c)
                },
            )
            .unwrap();
            let cr = scope.spawn(move || {
                fs.read_with(c, ch, 0, 128 * 1024, || {
                    c_ready.send(()).unwrap();
                    c_paused.recv_timeout(Duration::from_secs(10)).unwrap();
                })
            });
            c_entered.recv_timeout(Duration::from_secs(5)).unwrap();
            a_resume.send(()).unwrap();
            let waited_c = waiting(c);
            let not_completed = completed.try_recv().is_err();
            c_resume.send(()).unwrap();
            assert_eq!(ar.join().unwrap().unwrap(), vec![42; 128 * 1024]);
            assert_eq!(cr.join().unwrap().unwrap(), vec![99; 128 * 1024]);
            rename.join().unwrap().unwrap();
            assert!(
                waited_a && waited_c && not_completed,
                "rename bypassed a reader of its new destination"
            );
        });
        fs.with_read(|s| {
            assert_eq!(s.lookup(INodeNo::ROOT, OsStr::new("b"))?, a);
            s.release(a, ah)?;
            s.release(b, bh)?;
            s.release(c, ch)
        })
        .unwrap();
        let root = temp.path().join("store");
        drop(fs);
        crate::Store::open(&root).unwrap().check().unwrap();
    }

    #[test]
    fn corrupt_read_releases_lease_and_allows_truncation() {
        let (temp, fs, [(a, ah), (b, bh)]) = fixture(true);
        let root = temp.path().join("store");
        for entry in fs::read_dir(root.join("packs")).unwrap() {
            OpenOptions::new()
                .write(true)
                .open(entry.unwrap().path())
                .unwrap()
                .write_all_at(b"bad", 8)
                .unwrap();
        }
        assert_eq!(fs.read_handle(a, ah, 0, 128 * 1024), Err(Errno::EIO));
        fs.with_read(|s| {
            assert_eq!(s.nodes.get(&a).unwrap().readers, 0);
            assert_eq!(s.seal_stats.active_reads, 0);
            Ok(())
        })
        .unwrap();
        let fh = fs.open_handle(a, libc::O_WRONLY | libc::O_TRUNC).unwrap();
        fs.write_handle(a, fh, 0, b"recovered").unwrap();
        assert_eq!(fs.read_handle(a, ah, 0, 9).unwrap(), b"recovered");
        fs.with_read(|s| {
            s.release(a, fh)?;
            s.release(a, ah)?;
            s.release(b, bh)
        })
        .unwrap();
    }
}
