//! Packing and durable publication run outside the filesystem state lock.
use super::*;
use crate::store::Packer;
use serde::Serialize;
use std::{
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Instant,
};

const QUEUE_LIMIT: usize = 1024;

#[derive(Default, Serialize)]
pub(super) struct Stats {
    completed: u64,
    canceled: u64,
    skipped: u64,
    errors: u64,
    candidate_seconds: f64,
    prepare_seconds: f64,
    publish_seconds: f64,
    max_publish_seconds: f64,
    publication_state_seconds: f64,
    max_publication_state_seconds: f64,
    selection_state_seconds: f64,
    max_selection_state_seconds: f64,
    pub(super) request_state_seconds: f64,
    pub(super) max_request_state_seconds: f64,
    pub(super) catalog_wait_seconds: f64,
    pub(super) publication_open_wait_seconds: f64,
    pub(super) lookup_operations: u64,
    pub(super) getattr_operations: u64,
    pub(super) read_operations: u64,
    pub(super) active_reads: usize,
    pub(super) max_parallel_reads: usize,
    pub(super) read_seconds: f64,
    pub(super) read_state_seconds: f64,
    pub(super) read_wait_seconds: f64,
    pub(super) mutation_read_wait_seconds: f64,
    max_queue_len: usize,
}

struct Job {
    ino: INodeNo,
    pin: File,
    epoch: Arc<AtomicU64>,
    version: u64,
}

impl State {
    pub(super) fn schedule_seal(&mut self, ino: INodeNo) {
        if self.live.is_none() {
            return;
        }
        let Some(node) = self.nodes.get_mut(&ino) else {
            return;
        };
        if node.opens != 0 || node.queued {
            return;
        }
        let Ok(meta) = node.pin.metadata() else {
            return;
        };
        if !meta.is_file()
            || meta.nlink() == 0
            || meta.len() < crate::live::MIN_SEAL_SIZE
            || meta.mode() & 0o600 != 0o600
        {
            return;
        }
        node.queued = true;
        // Overflow stays as a bit on the already-pinned inode, not a second job
        // allocation. The worker scans those bits after draining the hot queue.
        if self.seal_queue.len() < QUEUE_LIMIT {
            self.seal_queue.push_back(ino);
        }
        self.seal_stats.max_queue_len = self.seal_stats.max_queue_len.max(self.seal_queue.len());
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }

    fn take_job(&mut self) -> Option<Job> {
        loop {
            let ino = self.seal_queue.pop_front().or_else(|| {
                self.nodes
                    .iter()
                    .find_map(|(&ino, node)| node.queued.then_some(ino))
            })?;
            let Some(node) = self.nodes.get_mut(&ino) else {
                continue;
            };
            if !node.queued {
                continue;
            }
            node.queued = false;
            if node.opens != 0 {
                continue;
            }
            let epoch = Arc::clone(&node.epoch);
            let version = epoch.load(Ordering::Relaxed);
            match node.pin.try_clone() {
                Ok(pin) => {
                    node.packing = true;
                    return Some(Job {
                        ino,
                        pin,
                        epoch,
                        version,
                    });
                }
                Err(error) => {
                    self.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(ino));
                }
            }
            self.reclaim(ino);
        }
    }
}

pub(super) fn start(
    state: Arc<Mutex<State>>,
    catalog_writer: Arc<Mutex<()>>,
    publication_done: Arc<Condvar>,
    packer: Packer,
    publisher: crate::live::Live,
) -> io::Result<JoinHandle<()>> {
    let (wake, receive) = mpsc::sync_channel(1);
    state
        .lock()
        .map_err(|_| io::Error::other("poisoned filesystem state"))?
        .wake = Some(wake);
    thread::Builder::new()
        .name("sl7-pack".into())
        .spawn(move || {
            run(
                state,
                catalog_writer,
                publication_done,
                packer,
                publisher,
                receive,
            )
        })
}

/// Clear the inode's publication gate even if the publisher unwinds. Its native
/// contents cannot be opened until the catalog binding and retirement finish.
struct Publication {
    state: Arc<Mutex<State>>,
    done: Arc<Condvar>,
    ino: INodeNo,
}
impl Drop for Publication {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            let started = Instant::now();
            if let Some(node) = state.nodes.get_mut(&self.ino) {
                node.publishing = false;
                node.packing = false;
            }
            state.reclaim(self.ino);
            let elapsed = started.elapsed().as_secs_f64();
            state.seal_stats.publication_state_seconds += elapsed;
            state.seal_stats.max_publication_state_seconds =
                state.seal_stats.max_publication_state_seconds.max(elapsed);
            self.done.notify_all();
        }
    }
}

fn run(
    state: Arc<Mutex<State>>,
    catalog_writer: Arc<Mutex<()>>,
    publication_done: Arc<Condvar>,
    packer: Packer,
    mut publisher: crate::live::Live,
    wake: Receiver<()>,
) {
    loop {
        let job = {
            let Ok(mut state) = state.lock() else {
                return;
            };
            let started = Instant::now();
            let job = state.take_job();
            let seconds = started.elapsed().as_secs_f64();
            state.seal_stats.selection_state_seconds += seconds;
            state.seal_stats.max_selection_state_seconds =
                state.seal_stats.max_selection_state_seconds.max(seconds);
            match job {
                Some(job) => job,
                None if state.stopping => return,
                None => {
                    drop(state);
                    if wake.recv().is_err() {
                        return;
                    }
                    continue;
                }
            }
        };
        // Pinning the inode and capturing its epoch are the only selection
        // work under filesystem state. Identity/generation commits and fsync
        // run on the worker connection; concurrent native mutations invalidate
        // the captured epoch before this candidate can be published.
        let publication = Publication {
            state: Arc::clone(&state),
            done: Arc::clone(&publication_done),
            ino: job.ino,
        };
        let candidate = {
            let Ok(_writer) = catalog_writer.lock() else {
                return;
            };
            let started = Instant::now();
            let candidate = publisher.begin_seal(&job.pin);
            let seconds = started.elapsed().as_secs_f64();
            let Ok(mut state) = state.lock() else {
                return;
            };
            state.seal_stats.candidate_seconds += seconds;
            match candidate {
                Ok(Some(candidate)) => candidate,
                Ok(None) => {
                    state.seal_stats.skipped += 1;
                    continue;
                }
                Err(error) => {
                    state.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(job.ino));
                    continue;
                }
            }
        };
        #[cfg(test)]
        let prepare_pause = state.lock().unwrap().seal_pause.take();
        let started = Instant::now();
        let prepared = packer.prepare(&fd_path(&job.pin), || {
            job.epoch.load(Ordering::Relaxed) != job.version
        });
        let elapsed = started.elapsed().as_secs_f64();
        #[cfg(test)]
        if let Some((ready, resume)) = prepare_pause {
            let _ = ready.send(());
            let _ = resume.recv();
        }
        // All catalog writers acquire this permit before filesystem state.
        // Waiters therefore never pin the state mutex behind SQLite commits.
        let Ok(_writer) = catalog_writer.lock() else {
            return;
        };
        #[cfg(test)]
        let pause;
        let prepared = {
            let Ok(mut state) = state.lock() else {
                return;
            };
            let locked = Instant::now();
            state.seal_stats.prepare_seconds += elapsed;
            let valid = state.nodes.get(&job.ino).is_some_and(|node| {
                node.opens == 0 && node.epoch.load(Ordering::Relaxed) == job.version
            });
            let prepared = match prepared {
                Ok(prepared) if valid => Some(prepared),
                Ok(_) | Err(crate::Error::SourceChanged(_)) => {
                    state.seal_stats.canceled += 1;
                    None
                }
                Err(_) if !valid => {
                    state.seal_stats.canceled += 1;
                    None
                }
                Err(error) => {
                    state.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(job.ino));
                    None
                }
            };
            if prepared.is_some() {
                state.nodes.get_mut(&job.ino).unwrap().publishing = true;
            }
            #[cfg(test)]
            {
                pause = prepared.as_ref().and_then(|_| state.publish_pause.take());
            }
            let seconds = locked.elapsed().as_secs_f64();
            state.seal_stats.publication_state_seconds += seconds;
            state.seal_stats.max_publication_state_seconds =
                state.seal_stats.max_publication_state_seconds.max(seconds);
            prepared
        };
        let Some(prepared) = prepared else {
            continue;
        };
        #[cfg(test)]
        if let Some((ready, resume)) = pause {
            let _ = ready.send(());
            let _ = resume.recv();
        }
        let started = Instant::now();
        let result = publisher.publish_seal(&job.pin, candidate, prepared);
        let seconds = started.elapsed().as_secs_f64();
        {
            let Ok(mut state) = state.lock() else {
                return;
            };
            let locked = Instant::now();
            match result {
                Ok(_) => state.seal_stats.completed += 1,
                Err(error) => {
                    state.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(job.ino));
                }
            }
            state.seal_stats.publish_seconds += seconds;
            state.seal_stats.max_publish_seconds =
                state.seal_stats.max_publish_seconds.max(seconds);
            let elapsed = locked.elapsed().as_secs_f64();
            state.seal_stats.publication_state_seconds += elapsed;
            state.seal_stats.max_publication_state_seconds =
                state.seal_stats.max_publication_state_seconds.max(elapsed);
        }
        drop(publication);
    }
}

pub(super) fn stop(state: &Mutex<State>, worker: JoinHandle<()>) {
    if let Ok(mut state) = state.lock() {
        state.stopping = true;
        if let Some(wake) = &state.wake {
            let _ = wake.try_send(());
        }
    }
    if worker.join().is_err() {
        eprintln!("sl7: background sealing worker panicked");
        if let Ok(mut state) = state.lock() {
            state.worker_failed = true;
        }
    }
    if std::env::var_os("SL7_CACHE_STATS").is_some() {
        if let Ok(state) = state.lock() {
            if let Ok(stats) = serde_json::to_string(&state.seal_stats) {
                eprintln!("sl7: sealing_stats {stats}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn idle(fs: &StagingFs) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if fs
                .with(|s| Ok(s.nodes.values().all(|n| !n.queued && !n.packing)))
                .unwrap()
            {
                return;
            }
            assert!(Instant::now() < deadline, "worker did not drain");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn publication_blocks_its_inode_but_not_unrelated_reads_or_lookups() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs = StagingFs::deduplicating(crate::Store::init(&root).unwrap()).unwrap();
        let first = vec![42; 128 * 1024];
        let second = vec![77; 128 * 1024];
        fs::write(root.join("tree/first"), &first).unwrap();
        fs::write(root.join("tree/second"), &second).unwrap();
        fs::write(root.join("tree/native"), b"native").unwrap();
        let (ready, published) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        let (a, b, c) = fs
            .with(|s| {
                let a = s.lookup(INodeNo::ROOT, OsStr::new("first"))?;
                let b = s.lookup(INodeNo::ROOT, OsStr::new("second"))?;
                let c = s.lookup(INodeNo::ROOT, OsStr::new("native"))?;
                s.entry_attr(a)?;
                s.entry_attr(b)?;
                s.entry_attr(c)?;
                let pin = s.inode(b)?.try_clone()?;
                s.live.as_mut().unwrap().seal(&pin).map_err(storage)?;
                s.publish_pause = Some((ready, paused));
                let fh = s.open(a, libc::O_RDONLY)?;
                s.release(a, fh)?;
                Ok((a, b, c))
            })
            .unwrap();
        published.recv_timeout(Duration::from_secs(10)).unwrap();
        thread::scope(|scope| {
            let (opening, opened) = mpsc::sync_channel(1);
            let (writing, wrote) = mpsc::sync_channel(1);
            let (reading, read) = mpsc::sync_channel(1);
            let first = &first;
            let second = &second;
            let fs = &fs;
            scope.spawn(move || {
                let fh = fs.open_handle(a, libc::O_RDONLY).unwrap();
                opening.send(()).unwrap();
                fs.with_read(|s| {
                    assert_eq!(
                        s.live
                            .as_ref()
                            .unwrap()
                            .read(s.inode(a)?, 0, first.len())
                            .unwrap()
                            .unwrap(),
                        *first
                    );
                    s.release(a, fh)
                })
                .unwrap();
            });
            // This writer waits for publication, but must not take filesystem
            // state with it and thereby stall the unrelated reader below.
            scope.spawn(move || {
                let fh = fs.open_handle(b, libc::O_RDWR).unwrap();
                writing.send(()).unwrap();
                fs.write_handle(b, fh, 0, b"changed").unwrap();
                fs.with_read(|s| s.release(b, fh)).unwrap();
            });
            scope.spawn(move || {
                let fh = fs.open_handle(b, libc::O_RDONLY).unwrap();
                fs.with_read(|s| {
                    assert_eq!(s.lookup(INodeNo::ROOT, OsStr::new("second"))?, b);
                    assert_eq!(s.attr(a)?.size, first.len() as u64);
                    assert_eq!(
                        s.live
                            .as_ref()
                            .unwrap()
                            .read(s.inode(b)?, 0, second.len())
                            .unwrap()
                            .unwrap(),
                        *second
                    );
                    s.release(b, fh)
                })
                .unwrap();
                let native = fs.open_handle(c, libc::O_RDWR).unwrap();
                fs.write_handle(c, native, 0, b"updated").unwrap();
                fs.with_read(|s| {
                    let mut bytes = [0; 7];
                    s.handle(c, native)?.file.read_exact_at(&mut bytes, 0)?;
                    assert_eq!(&bytes, b"updated");
                    s.release(c, native)
                })
                .unwrap();
                reading.send(()).unwrap();
            });
            let independent = read.recv_timeout(Duration::from_secs(5));
            let same_inode_blocked = opened.try_recv().is_err();
            let writer_blocked = wrote.try_recv().is_err();
            // Always release the worker before assertions or scoped joins.
            resume.send(()).unwrap();
            let eventually_opened = opened.recv_timeout(Duration::from_secs(5));
            let eventually_wrote = wrote.recv_timeout(Duration::from_secs(5));
            independent.expect("publication or a waiting writer held filesystem state");
            assert!(same_inode_blocked, "opened the inode during publication");
            assert!(writer_blocked, "catalog writer bypassed publication permit");
            eventually_opened.expect("same-inode reader did not resume");
            eventually_wrote.expect("catalog writer did not resume");
        });
        drop(fs);
        let live = crate::live::Live::open(crate::Store::open(&root).unwrap()).unwrap();
        let mut expected = second;
        expected[..7].copy_from_slice(b"changed");
        assert_eq!(
            live.read(
                &File::open(root.join("tree/second")).unwrap(),
                0,
                expected.len()
            )
            .unwrap()
            .unwrap(),
            expected
        );
        live.store.check().unwrap();
    }

    #[test]
    fn failed_publication_releases_inode_gate_and_preserves_native_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs = StagingFs::deduplicating(crate::Store::init(&root).unwrap()).unwrap();
        let bytes = vec![63; 128 * 1024];
        fs::write(root.join("tree/data"), &bytes).unwrap();
        let (ready, published) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        let ino = fs.with(|s| {
            let ino = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
            s.entry_attr(ino)?;
            // Inject a binding failure after the immutable manifest commits.
            s.live.as_ref().unwrap().store.db.execute_batch(
                "CREATE TRIGGER fail_bind BEFORE UPDATE OF manifest ON live_files WHEN NEW.manifest IS NOT NULL BEGIN SELECT RAISE(ABORT,'injected binding failure'); END;"
            ).unwrap();
            s.publish_pause = Some((ready, paused));
            let fh = s.open(ino, libc::O_RDONLY)?;
            s.release(ino, fh)?;
            Ok(ino)
        }).unwrap();
        published.recv_timeout(Duration::from_secs(10)).unwrap();
        resume.send(()).unwrap();
        idle(&fs);
        fs.with(|s| {
            assert!(!s.nodes[&ino].publishing);
            assert_eq!(s.seal_stats.errors, 1);
            assert!(
                s.live
                    .as_ref()
                    .unwrap()
                    .sealed(s.inode(ino)?)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(fs::read(s.path(ino)?)?, bytes);
            s.live
                .as_ref()
                .unwrap()
                .store
                .db
                .execute_batch("DROP TRIGGER fail_bind")
                .unwrap();
            Ok(())
        })
        .unwrap();
        let fh = fs.open_handle(ino, libc::O_RDONLY).unwrap();
        fs.with_read(|s| s.release(ino, fh)).unwrap();
        idle(&fs);
        fs.with_read(|s| {
            assert_eq!(s.seal_stats.completed, 1);
            assert_eq!(
                s.live
                    .as_ref()
                    .unwrap()
                    .read(s.inode(ino)?, 0, bytes.len())
                    .unwrap()
                    .unwrap(),
                bytes
            );
            Ok(())
        })
        .unwrap();
        drop(fs);
        crate::Store::open(&root).unwrap().check().unwrap();
    }

    #[test]
    fn stale_packing_cannot_overwrite_a_concurrent_write_and_drop_drains() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = crate::Store::init(&root).unwrap();
        let fs = StagingFs::deduplicating(store).unwrap();
        let original = vec![42; 256 * 1024];
        fs::write(root.join("tree/data"), &original).unwrap();
        let (ready, prepared) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        let ino = fs
            .with(|s| {
                s.seal_pause = Some((ready, paused));
                let ino = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
                s.entry_attr(ino)?;
                let fh = s.open(ino, libc::O_RDONLY)?;
                s.release(ino, fh)?;
                Ok(ino)
            })
            .unwrap();
        prepared.recv_timeout(Duration::from_secs(10)).unwrap();
        // The worker is paused after packing. A competing writer must be able
        // to acquire the filesystem lock and invalidate the pending result.
        let (finished, result) = mpsc::sync_channel(1);
        let state = Arc::clone(&fs.state);
        let client = thread::spawn(move || {
            let mut s = state.lock().unwrap();
            let fh = s.open(ino, libc::O_RDWR).unwrap();
            s.mutated(ino).unwrap();
            s.handle(ino, fh)
                .unwrap()
                .file
                .write_all_at(b"new bytes", 0)
                .unwrap();
            s.release(ino, fh).unwrap();
            finished.send(()).unwrap();
        });
        let concurrent = result.recv_timeout(Duration::from_secs(5));
        resume.send(()).unwrap();
        client.join().unwrap();
        concurrent.expect("packing held the filesystem lock");
        drop(fs); // joins the worker and seals the replacement generation
        let live = crate::live::Live::open(crate::Store::open(&root).unwrap()).unwrap();
        let file = File::open(root.join("tree/data")).unwrap();
        let mut expected = original;
        expected[..9].copy_from_slice(b"new bytes");
        assert_eq!(
            live.read(&file, 0, expected.len()).unwrap().unwrap(),
            expected
        );
        live.store.check().unwrap();
    }

    #[test]
    fn reader_opened_during_packing_prevents_retirement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs = StagingFs::deduplicating(crate::Store::init(&root).unwrap()).unwrap();
        let bytes = vec![77; 128 * 1024];
        fs::write(root.join("tree/data"), &bytes).unwrap();
        let (ready, prepared) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        let ino = fs
            .with(|s| {
                s.seal_pause = Some((ready, paused));
                let ino = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
                s.entry_attr(ino)?;
                let fh = s.open(ino, libc::O_RDONLY)?;
                s.release(ino, fh)?;
                Ok(ino)
            })
            .unwrap();
        prepared.recv_timeout(Duration::from_secs(10)).unwrap();
        let fh = fs.with(|s| s.open(ino, libc::O_RDONLY)).unwrap();
        resume.send(()).unwrap();
        idle(&fs);
        fs.with(|s| {
            assert!(
                s.live
                    .as_ref()
                    .unwrap()
                    .sealed(s.inode(ino)?)
                    .unwrap()
                    .is_none()
            );
            let mut actual = vec![0; bytes.len()];
            s.handle(ino, fh)?.file.read_exact_at(&mut actual, 0)?;
            assert_eq!(actual, bytes);
            assert_eq!(s.seal_stats.canceled, 1);
            s.release(ino, fh)
        })
        .unwrap();
        idle(&fs);
        fs.with(|s| {
            assert!(
                s.live
                    .as_ref()
                    .unwrap()
                    .sealed(s.inode(ino)?)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(s.seal_stats.completed, 1);
            Ok(())
        })
        .unwrap();
    }
}
