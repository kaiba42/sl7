//! Namespace metadata commits retain affected inodes while releasing global state.
use super::*;

struct Reservation<'a> {
    fs: &'a StagingFs,
    inodes: Vec<INodeNo>,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.fs.state.lock() {
            let started = Instant::now();
            for &ino in &self.inodes {
                let mut node = state.nodes.get_mut(&ino).unwrap();
                node.readers -= 1;
                node.opens -= 1;
                drop(node);
                state.schedule_seal(ino);
                state.reclaim(ino);
            }
            let seconds = started.elapsed().as_secs_f64();
            state.seal_stats.request_state_seconds += seconds;
            state.seal_stats.max_request_state_seconds =
                state.seal_stats.max_request_state_seconds.max(seconds);
            self.fs.publication_done.notify_all();
        }
    }
}

impl StagingFs {
    pub(super) fn with_namespace_mutation<T, R>(
        &self,
        select: impl FnMut(&mut State) -> io::Result<Vec<INodeNo>>,
        operation: impl FnOnce(&mut State) -> io::Result<(T, Vec<INodeNo>)>,
        finish: impl FnOnce(&mut State, T) -> io::Result<R>,
    ) -> std::result::Result<R, Errno> {
        self.namespace_with(select, operation, finish, || {})
    }

    fn namespace_with<T, R>(
        &self,
        select: impl FnMut(&mut State) -> io::Result<Vec<INodeNo>>,
        operation: impl FnOnce(&mut State) -> io::Result<(T, Vec<INodeNo>)>,
        finish: impl FnOnce(&mut State, T) -> io::Result<R>,
        ready: impl FnOnce(),
    ) -> std::result::Result<R, Errno> {
        let (value, updates, _reservation) = self.with_mutation(select, |state| {
            let (value, mut inodes) = operation(state)?;
            inodes.sort_unstable();
            inodes.dedup();
            let mut updates = Vec::new();
            for &ino in &inodes {
                state.invalidate(ino)?;
                let pin = state.inode(ino)?;
                if let Some(live) = &state.live {
                    if let Some(update) = live
                        .prepare_metadata_change(&pin, false, false)
                        .map_err(storage)?
                    {
                        updates.push(update);
                    }
                }
            }
            // Complete fallible preparation before installing any reservation.
            // Content is unchanged: read leases may still overlap this metadata
            // commit, while the activity count excludes same-inode mutations.
            for &ino in &inodes {
                let mut node = state.nodes.get_mut(&ino).unwrap();
                node.opens += 1;
                node.readers += 1;
            }
            Ok((value, updates, Reservation { fs: self, inodes }))
        })?;
        if !updates.is_empty() {
            let started = Instant::now();
            let writer = self.catalog_writer.lock().map_err(|_| Errno::EIO)?;
            let waited = started.elapsed().as_secs_f64();
            // No filesystem state is held while waiting for or using SQLite.
            // The writer permit still serializes catalog writes; other inodes'
            // native I/O and read-only catalog queries can continue.
            ready();
            let started = Instant::now();
            let result = self
                .metadata_writer
                .as_ref()
                .ok_or(Errno::EIO)?
                .lock()
                .map_err(|_| Errno::EIO)?
                .commit_metadata_changes(&updates)
                .map_err(storage)
                .map_err(Errno::from);
            let elapsed = started.elapsed().as_secs_f64();
            drop(writer);
            self.with_read(|s| {
                s.seal_stats.catalog_wait_seconds += waited;
                s.seal_stats.namespace_commits += 1;
                s.seal_stats.namespace_commit_seconds += elapsed;
                Ok(())
            })?;
            result?;
        }
        // Inode references remain pinned until the reply attributes have been
        // captured, including when RELEASE/FORGET raced with the commit.
        self.with_read(|state| finish(state, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    #[test]
    fn catalog_commit_reserves_only_changed_inode() {
        let temp = tempfile::tempdir().unwrap();
        let fs = StagingFs::deduplicating(crate::Store::init(&temp.path().join("store")).unwrap())
            .unwrap();
        let (a, ah, b, bh, c, ch) = fs
            .with(|s| {
                let mut handles = Vec::new();
                for name in ["a", "b", "c"] {
                    fs::write(
                        s.child(INodeNo::ROOT, OsStr::new(name))?,
                        vec![42; 128 * 1024],
                    )?;
                    let ino = s.lookup(INodeNo::ROOT, OsStr::new(name))?;
                    s.entry_attr(ino)?;
                    if name != "b" {
                        s.live
                            .as_mut()
                            .unwrap()
                            .seal(&s.nodes.get(&ino).unwrap().pin)
                            .map_err(storage)?;
                    }
                    handles.push((
                        ino,
                        s.open(
                            ino,
                            if name == "b" {
                                libc::O_RDWR
                            } else {
                                libc::O_RDONLY
                            },
                        )?,
                    ));
                }
                Ok((
                    handles[0].0,
                    handles[0].1,
                    handles[1].0,
                    handles[1].1,
                    handles[2].0,
                    handles[2].1,
                ))
            })
            .unwrap();
        let (ready, entered) = mpsc::channel();
        let (resume, paused) = mpsc::channel();
        thread::scope(|scope| {
            let fs = &fs;
            let link = scope.spawn(move || {
                fs.namespace_with(
                    |_| Ok(vec![a]),
                    |s| {
                        fs::hard_link(
                            s.child(INodeNo::ROOT, OsStr::new("a"))?,
                            s.child(INodeNo::ROOT, OsStr::new("alias"))?,
                        )?;
                        Ok((a, vec![a]))
                    },
                    |s, ino| s.entry_attr(ino),
                    || {
                        ready.send(()).unwrap();
                        paused.recv_timeout(Duration::from_secs(10)).unwrap();
                    },
                )
            });
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                fs.read_handle(a, ah, 0, 128 * 1024).unwrap(),
                vec![42; 128 * 1024]
            );
            let (done, waiting) = mpsc::channel();
            let writer = scope.spawn(move || {
                let result = fs.with_mutation(|_| Ok(vec![a]), |_| Ok(()));
                done.send(()).unwrap();
                result
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !fs
                .with_read(|s| Ok(s.nodes.get(&a).unwrap().waiting_mutations > 0))
                .unwrap()
            {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            let (sent, other_done) = mpsc::channel();
            scope.spawn(move || {
                let result = fs
                    .write_handle(b, bh, 0, b"independent")
                    .and_then(|()| fs.read_handle(c, ch, 0, 128 * 1024));
                sent.send(result).unwrap();
            });
            let other = other_done.recv_timeout(Duration::from_secs(5));
            // Pin lifetime must cover both in-flight commit and reply creation.
            fs.with_read(|s| {
                s.release(a, ah)?;
                s.forget(a, u64::MAX);
                Ok(())
            })
            .unwrap();
            let blocked = waiting.try_recv().is_err();
            resume.send(()).unwrap();
            let attr = link.join().unwrap().unwrap();
            writer.join().unwrap().unwrap();
            assert!(blocked, "same-inode mutation bypassed catalog publication");
            assert_eq!(
                other.expect("catalog commit held global state").unwrap(),
                vec![42; 128 * 1024]
            );
            assert_eq!(attr.nlink, 2);
            fs.with_read(|s| {
                let metadata = s.nodes.get(&a).unwrap().pin.metadata()?;
                assert_eq!(
                    attr.ctime,
                    timestamp(metadata.ctime(), metadata.ctime_nsec())
                );
                assert_eq!(s.seal_stats.namespace_commits, 1);
                s.release(b, bh)?;
                s.release(c, ch)
            })
            .unwrap();
        });
    }
}
