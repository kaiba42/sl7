//! Native writes and synchronization hold an inode reservation, not global state.
use super::*;

struct NativeIo<'a> {
    fs: &'a StagingFs,
    ino: INodeNo,
    started: Instant,
}

impl Drop for NativeIo<'_> {
    fn drop(&mut self) {
        let waiting = profile::span("cleanup_state_lock_wait");
        if let Ok(mut state) = self.fs.state.lock() {
            drop(waiting);
            let _phase = profile::span("cleanup_locked_work");
            let locked = Instant::now();
            let mut node = state.nodes.get_mut(&self.ino).unwrap();
            node.exclusive_io = false;
            node.readers -= 1;
            node.opens -= 1;
            drop(node);
            state.seal_stats.active_native_io -= 1;
            state.seal_stats.native_io_seconds += self.started.elapsed().as_secs_f64();
            state.schedule_seal(self.ino);
            state.reclaim(self.ino);
            let seconds = locked.elapsed().as_secs_f64();
            state.seal_stats.request_state_seconds += seconds;
            state.seal_stats.max_request_state_seconds =
                state.seal_stats.max_request_state_seconds.max(seconds);
            self.fs.publication_done.notify_all();
        }
    }
}

impl StagingFs {
    pub(super) fn native_io(
        &self,
        ino: INodeNo,
        fh: FileHandle,
        writing: bool,
        operation: impl FnOnce(&File) -> io::Result<()>,
    ) -> std::result::Result<(), Errno> {
        let (file, _reservation) = self.with_mutation(
            |_| Ok(vec![ino]),
            |state| {
                // Validate and own the descriptor before changing representation.
                let file = Arc::clone(&state.handle(ino, fh)?.file);
                if writing {
                    state.hydrate(ino)?;
                    state.mutated(ino)?;
                }
                let mut node = state.nodes.get_mut(&ino).unwrap();
                // with_mutation has excluded all reads and native operations.
                // The virtual open also protects release/forget and background
                // publication while this request runs outside filesystem state.
                node.readers += 1;
                node.opens += 1;
                node.exclusive_io = true;
                drop(node);
                if writing {
                    state.seal_stats.write_operations += 1;
                } else {
                    state.seal_stats.sync_operations += 1;
                }
                state.seal_stats.active_native_io += 1;
                state.seal_stats.max_parallel_native_io = state
                    .seal_stats
                    .max_parallel_native_io
                    .max(state.seal_stats.active_native_io);
                Ok((
                    file,
                    NativeIo {
                        fs: self,
                        ino,
                        started: Instant::now(),
                    },
                ))
            },
        )?;
        let payload = profile::span("native_payload");
        let result = operation(&file).map_err(Errno::from);
        drop(payload);
        result
    }

    pub(super) fn sync_handle(
        &self,
        ino: INodeNo,
        fh: FileHandle,
        datasync: bool,
    ) -> std::result::Result<(), Errno> {
        self.native_io(ino, fh, false, |file| {
            if let Some(metadata) = &self.metadata_writer {
                let started = Instant::now();
                let writer = self.catalog_writer.lock().map_err(|_| error(libc::EIO))?;
                let waited = started.elapsed().as_secs_f64();
                let started = Instant::now();
                let result = metadata
                    .lock()
                    .map_err(|_| error(libc::EIO))?
                    .sync()
                    .map_err(storage);
                let seconds = started.elapsed().as_secs_f64();
                drop(writer);
                self.with_read(|s| {
                    s.seal_stats.catalog_wait_seconds += waited;
                    s.seal_stats.catalog_syncs += 1;
                    s.seal_stats.catalog_sync_seconds += seconds;
                    Ok(())
                })
                .map_err(|_| error(libc::EIO))?;
                result?;
            }
            if datasync {
                file.sync_data()
            } else {
                file.sync_all()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    fn fixture() -> (tempfile::TempDir, StagingFs, [(INodeNo, FileHandle); 2]) {
        let temp = tempfile::tempdir().unwrap();
        let fs = StagingFs::deduplicating(crate::Store::init(&temp.path().join("store")).unwrap())
            .unwrap();
        let handles = fs
            .with(|state| {
                let mut handles = Vec::new();
                for name in ["a", "b"] {
                    fs::write(
                        state.child(INodeNo::ROOT, OsStr::new(name))?,
                        vec![42; 128 * 1024],
                    )?;
                    let ino = state.lookup(INodeNo::ROOT, OsStr::new(name))?;
                    state.entry_attr(ino)?;
                    handles.push((ino, state.open(ino, libc::O_RDWR)?));
                }
                Ok(handles.try_into().unwrap())
            })
            .unwrap();
        (temp, fs, handles)
    }

    #[test]
    fn file_and_directory_sync_flush_catalog_and_propagate_failures() {
        let (_temp, fs, [(a, ah), (b, bh)]) = fixture();
        let dh = fs
            .with(|s| {
                let file = open_native(
                    &s.path(INodeNo::ROOT)?,
                    libc::O_RDONLY | libc::O_DIRECTORY,
                    0,
                )?;
                s.add_handle(INodeNo::ROOT, file, Some(Vec::new()))
            })
            .unwrap();
        let epoch = || {
            fs.with_read(|s| {
                s.live
                    .as_ref()
                    .unwrap()
                    .store
                    .db
                    .query_row("SELECT epoch FROM live_sync WHERE id=1", [], |r| {
                        r.get::<_, i64>(0)
                    })
                    .map_err(|e| storage(e.into()))
            })
            .unwrap()
        };
        for (ino, fh, datasync) in [(a, ah, false), (a, ah, true), (INodeNo::ROOT, dh, false)] {
            let before = epoch();
            fs.sync_handle(ino, fh, datasync).unwrap();
            assert_eq!(epoch(), 1 - before);
        }
        fs.with(|s| {
            s.live
                .as_ref()
                .unwrap()
                .store
                .db
                .execute_batch(
                    "CREATE TRIGGER reject_barrier BEFORE UPDATE ON live_sync
                 BEGIN SELECT RAISE(ABORT, 'injected barrier failure'); END;",
                )
                .map_err(|e| storage(e.into()))
        })
        .unwrap();
        for (ino, fh) in [(a, ah), (INodeNo::ROOT, dh)] {
            assert_eq!(fs.sync_handle(ino, fh, false), Err(Errno::EIO));
            fs.with_read(|s| {
                assert_eq!(s.seal_stats.active_native_io, 0);
                assert!(!s.nodes.get(&ino).unwrap().exclusive_io);
                assert_eq!(s.nodes.get(&ino).unwrap().readers, 0);
                Ok(())
            })
            .unwrap();
        }
        fs.write_handle(a, ah, 0, b"retry").unwrap();
        fs.write_handle(b, bh, 0, b"other").unwrap();
        assert_eq!(fs.read_handle(a, ah, 0, 5).unwrap(), b"retry");
        fs.with(|s| {
            s.live
                .as_ref()
                .unwrap()
                .store
                .db
                .execute_batch("DROP TRIGGER reject_barrier")
                .map_err(|e| storage(e.into()))?;
            assert_eq!(s.seal_stats.catalog_syncs, 5);
            Ok(())
        })
        .unwrap();
        fs.sync_handle(a, ah, false).unwrap();
        fs.with_read(|s| {
            s.release(a, ah)?;
            s.release(b, bh)?;
            s.release(INodeNo::ROOT, dh)
        })
        .unwrap();
    }

    #[test]
    fn writes_and_syncs_allow_unrelated_io_but_exclude_same_inode() {
        for writing in [false, true] {
            let (_temp, fs, [(a, ah), (b, bh)]) = fixture();
            let (ready, entered) = mpsc::channel();
            let (resume, paused) = mpsc::channel();
            thread::scope(|scope| {
                let fs = &fs;
                let first = scope.spawn(move || {
                    fs.native_io(a, ah, writing, |file| {
                        ready.send(()).unwrap();
                        paused.recv_timeout(Duration::from_secs(10)).unwrap();
                        if writing {
                            file.write_all_at(b"first", 0)
                        } else {
                            file.sync_all()
                        }
                    })
                });
                entered.recv_timeout(Duration::from_secs(5)).unwrap();
                let (sent, received) = mpsc::channel();
                let writer = scope.spawn(move || {
                    let result = fs.write_handle(a, ah, 0, b"second");
                    sent.send(()).unwrap();
                    result
                });
                let deadline = Instant::now() + Duration::from_secs(5);
                while !fs
                    .with_read(|s| Ok(s.nodes.get(&a).unwrap().waiting_mutations > 0))
                    .unwrap()
                {
                    assert!(Instant::now() < deadline, "same-inode writer did not wait");
                    thread::sleep(Duration::from_millis(1));
                }
                let (read_sent, read_done) = mpsc::channel();
                let reader = scope.spawn(move || {
                    let result = fs.read_handle(a, ah, 0, 6);
                    read_sent.send(()).unwrap();
                    result
                });
                let (sent, other_done) = mpsc::channel();
                scope.spawn(move || {
                    let result = fs
                        .write_handle(b, bh, 0, b"other")
                        .and_then(|()| fs.sync_handle(b, bh, false))
                        .and_then(|()| fs.read_handle(b, bh, 0, 5));
                    sent.send(result).unwrap();
                });
                let other = other_done.recv_timeout(Duration::from_secs(5));
                let blocked_writer = received.try_recv().is_err();
                let blocked_reader = read_done.try_recv().is_err();
                resume.send(()).unwrap();
                first.join().unwrap().unwrap();
                writer.join().unwrap().unwrap();
                assert_eq!(reader.join().unwrap().unwrap(), b"second");
                assert!(blocked_writer && blocked_reader);
                assert_eq!(
                    other
                        .expect("unrelated I/O blocked on global state")
                        .unwrap(),
                    b"other"
                );
            });
            fs.with_read(|s| {
                assert_eq!(s.seal_stats.active_native_io, 0);
                assert!(s.seal_stats.max_parallel_native_io >= 2);
                s.release(a, ah)?;
                s.release(b, bh)
            })
            .unwrap();
        }
    }

    #[test]
    fn native_io_survives_release_forget_and_reports_errors() {
        let (_temp, fs, [(a, ah), (b, bh)]) = fixture();
        for writing in [false, true] {
            let (ino, fh) = if writing { (b, bh) } else { (a, ah) };
            let result = fs.native_io(ino, fh, writing, |file| {
                fs.with_read(|s| {
                    s.release(ino, fh)?;
                    s.forget(ino, u64::MAX);
                    assert!(s.nodes.contains_key(&ino));
                    Ok(())
                })
                .unwrap();
                file.write_all_at(b"still alive", 0)?;
                file.sync_all()?;
                Err(error(libc::ENOSPC))
            });
            assert_eq!(result, Err(Errno::ENOSPC));
            fs.with_read(|s| {
                assert_eq!(s.seal_stats.active_native_io, 0);
                if let Some(node) = s.nodes.get(&ino) {
                    assert_eq!(node.readers, 0);
                    assert_eq!(node.opens, 0);
                    assert!(!node.exclusive_io);
                }
                Ok(())
            })
            .unwrap();
        }
    }

    #[test]
    fn unlink_waits_for_inflight_write_and_preserves_open_descriptor() {
        let (_temp, fs, [(a, ah), (b, bh)]) = fixture();
        let (ready, entered) = mpsc::channel();
        let (resume, paused) = mpsc::channel();
        thread::scope(|scope| {
            let fs = &fs;
            let write = scope.spawn(move || {
                fs.native_io(a, ah, true, |file| {
                    ready.send(()).unwrap();
                    paused.recv_timeout(Duration::from_secs(10)).unwrap();
                    file.write_all_at(b"written", 0)
                })
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            let unlink = scope.spawn(move || {
                fs.with_mutation(
                    |s| Ok(vec![s.lookup(INodeNo::ROOT, OsStr::new("a"))?]),
                    |s| fs::remove_file(s.child(INodeNo::ROOT, OsStr::new("a"))?),
                )
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fs
                .with_read(|s| Ok(s.nodes.get(&a).unwrap().waiting_mutations > 0))
                .unwrap()
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            fs.write_handle(b, bh, 0, b"unrelated").unwrap();
            resume.send(()).unwrap();
            write.join().unwrap().unwrap();
            unlink.join().unwrap().unwrap();
        });
        assert_eq!(fs.read_handle(a, ah, 0, 7).unwrap(), b"written");
        fs.with_read(|s| {
            s.release(a, ah)?;
            s.release(b, bh)
        })
        .unwrap();
    }
}
