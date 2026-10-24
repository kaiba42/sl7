//! Packing and durable publication run outside the filesystem state lock.
use super::*;
use crate::store::Packer;
use serde::Serialize;
#[cfg(test)]
use std::sync::mpsc;
use std::{
    thread::{self, JoinHandle},
    time::Instant,
};

const QUEUE_LIMIT: usize = 1024;

pub(super) fn worker_count(value: Option<&str>) -> crate::Result<usize> {
    match value {
        None => Ok(2),
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=8).contains(n))
            .ok_or_else(|| {
                crate::Error::Invalid("SL7_SEAL_WORKERS must be an integer from 1 to 8".into())
            }),
    }
}

pub(super) struct Workers(Vec<JoinHandle<()>>);

#[derive(Default, Serialize)]
pub(super) struct Stats {
    import_pipeline: bool,
    pipelined_prepares: u64,
    workers: usize,
    active_seals: usize,
    max_parallel_seals: usize,
    active_prepares: usize,
    max_parallel_prepares: usize,
    completed: u64,
    canceled: u64,
    skipped: u64,
    unchanged_seals_avoided: u64,
    errors: u64,
    candidate_seconds: f64,
    candidate_catalog_wait_seconds: f64,
    active_candidates: usize,
    max_parallel_candidates: usize,
    prepare_seconds: f64,
    publish_seconds: f64,
    max_publish_seconds: f64,
    catalog_publish_seconds: f64,
    retirement_seconds: f64,
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
    pub(super) catalog_syncs: u64,
    pub(super) catalog_sync_seconds: f64,
    pub(super) namespace_commits: u64,
    pub(super) namespace_commit_seconds: f64,
    pub(super) write_operations: u64,
    pub(super) sync_operations: u64,
    pub(super) active_native_io: usize,
    pub(super) max_parallel_native_io: usize,
    pub(super) native_io_seconds: f64,
    pub(super) native_state_checks: u64,
    pub(super) native_state_cache_hits: u64,
    pub(super) read_operations: u64,
    pub(super) active_reads: usize,
    pub(super) max_parallel_reads: usize,
    pub(super) read_seconds: f64,
    pub(super) read_state_seconds: f64,
    pub(super) read_wait_seconds: f64,
    pub(super) mutation_read_wait_seconds: f64,
    pub(super) mutation_publication_wait_seconds: f64,
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
        if node.seal_checked {
            self.seal_stats.unchanged_seals_avoided += 1;
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
            wake.notify_one();
        }
    }

    fn take_job(&mut self) -> Option<Job> {
        loop {
            let ino = self.seal_queue.pop_front().or_else(|| {
                self.nodes
                    .iter()
                    .find_map(|(&ino, node)| (node.queued && !node.packing).then_some(ino))
            })?;
            let Some(node) = self.nodes.get_mut(&ino) else {
                continue;
            };
            if !node.queued || node.packing {
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
                    self.seal_stats.active_seals += 1;
                    self.seal_stats.max_parallel_seals = self
                        .seal_stats
                        .max_parallel_seals
                        .max(self.seal_stats.active_seals);
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

    fn checked_seal(&mut self, ino: INodeNo, version: u64) {
        if let Some(node) = self.nodes.get_mut(&ino) {
            if node.epoch.load(Ordering::Relaxed) == version {
                node.seal_checked = true;
                // Read-only closes during preparation may have queued another
                // check. The completed decision covers them at this epoch.
                // Any stale hot-queue entry is ignored by take_job.
                node.queued = false;
            }
        }
    }
}

pub(super) fn start(
    state: Arc<Mutex<State>>,
    catalog_writer: Arc<Mutex<()>>,
    publication_done: Arc<Condvar>,
    connections: Vec<(Packer, crate::live::Live)>,
) -> io::Result<Workers> {
    let wake = Arc::new(Condvar::new());
    {
        let mut state = state
            .lock()
            .map_err(|_| io::Error::other("poisoned filesystem state"))?;
        state.wake = Some(Arc::clone(&wake));
        state.seal_stats.workers = connections.len();
        state.seal_stats.import_pipeline = connections
            .first()
            .is_some_and(|(packer, _)| packer.pipeline_enabled());
    }
    let mut workers = Workers(Vec::new());
    for (index, (packer, publisher)) in connections.into_iter().enumerate() {
        let worker_state = Arc::clone(&state);
        let writer = Arc::clone(&catalog_writer);
        let done = Arc::clone(&publication_done);
        let wake = Arc::clone(&wake);
        match thread::Builder::new()
            .name(format!("sl7-pack-{index}"))
            .spawn(move || run(worker_state, writer, done, packer, publisher, wake))
        {
            Ok(worker) => workers.0.push(worker),
            Err(error) => {
                stop(&state, workers);
                return Err(error);
            }
        }
    }
    Ok(workers)
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
            state.seal_stats.active_seals -= 1;
            // A close during preparation can requeue this inode. It becomes
            // eligible only now, after the previous candidate has finished.
            if let Some(wake) = &state.wake {
                wake.notify_one();
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
    wake: Arc<Condvar>,
) {
    loop {
        let job = {
            let Ok(mut state) = state.lock() else {
                return;
            };
            loop {
                let started = Instant::now();
                let job = state.take_job();
                let seconds = started.elapsed().as_secs_f64();
                state.seal_stats.selection_state_seconds += seconds;
                state.seal_stats.max_selection_state_seconds =
                    state.seal_stats.max_selection_state_seconds.max(seconds);
                match job {
                    Some(job) => break job,
                    None if state.stopping => return,
                    None => {
                        state = match wake.wait(state) {
                            Ok(state) => state,
                            Err(_) => return,
                        };
                    }
                }
            }
        };
        // Pinning the inode and capturing its epoch are the only selection
        // work under filesystem state. Identity assignment and its fsync
        // run on the worker connection; concurrent native mutations invalidate
        // the captured epoch before this candidate can be published.
        let publication = Publication {
            state: Arc::clone(&state),
            done: Arc::clone(&publication_done),
            ino: job.ino,
        };
        let candidate = {
            let Ok(mut s) = state.lock() else {
                return;
            };
            s.seal_stats.active_candidates += 1;
            s.seal_stats.max_parallel_candidates = s
                .seal_stats
                .max_parallel_candidates
                .max(s.seal_stats.active_candidates);
            drop(s);
            let started = Instant::now();
            let mut catalog_wait = 0.0;
            let candidate = publisher.begin_seal_with(&job.pin, |publisher| {
                let waited = Instant::now();
                let writer = catalog_writer
                    .lock()
                    .map_err(|_| crate::Error::Invalid("poisoned catalog writer".into()))?;
                catalog_wait += waited.elapsed().as_secs_f64();
                let id = publisher.allocate_identity();
                drop(writer);
                #[cfg(test)]
                {
                    let pause = state.lock().unwrap().identity_pause.take();
                    if let Some((ready, resume)) = pause {
                        let _ = ready.send(());
                        let _ = resume.recv();
                    }
                }
                id
            });
            let seconds = started.elapsed().as_secs_f64();
            let Ok(mut state) = state.lock() else {
                return;
            };
            state.seal_stats.active_candidates -= 1;
            state.seal_stats.candidate_catalog_wait_seconds += catalog_wait;
            state.seal_stats.candidate_seconds += seconds - catalog_wait;
            match candidate {
                Ok(Some(candidate)) => candidate,
                Ok(None) => {
                    state.seal_stats.skipped += 1;
                    state.checked_seal(job.ino, job.version);
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
        {
            let Ok(mut state) = state.lock() else {
                return;
            };
            state.seal_stats.active_prepares += 1;
            state.seal_stats.max_parallel_prepares = state
                .seal_stats
                .max_parallel_prepares
                .max(state.seal_stats.active_prepares);
        }
        let started = Instant::now();
        let pipelines_before = packer.pipeline_prepares();
        let prepared = packer.prepare(&fd_path(&job.pin), || {
            job.epoch.load(Ordering::Relaxed) != job.version
        });
        let elapsed = started.elapsed().as_secs_f64();
        {
            let Ok(mut state) = state.lock() else {
                return;
            };
            state.seal_stats.active_prepares -= 1;
            state.seal_stats.pipelined_prepares += packer.pipeline_prepares() - pipelines_before;
        }
        #[cfg(test)]
        if let Some((ready, resume)) = prepare_pause {
            let _ = ready.send(());
            let _ = resume.recv();
        }
        // All catalog writers acquire this permit before filesystem state.
        // Waiters therefore never pin the state mutex behind SQLite commits.
        let Ok(writer) = catalog_writer.lock() else {
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
                let node = state.nodes.get_mut(&job.ino).unwrap();
                node.publishing = true;
                // Attribute callbacks can still run during publication. They
                // must consult the catalog before native retirement can begin.
                node.native_unoverridden = false;
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
        let committed = publisher.publish_seal(job.pin, candidate, prepared);
        let catalog_seconds = started.elapsed().as_secs_f64();
        // No catalog access remains. Keep this inode's publication gate closed
        // while allowing unrelated catalog writers to overlap native retirement.
        drop(writer);
        #[cfg(test)]
        if committed.is_ok() {
            let pause = state.lock().unwrap().retire_pause.take();
            if let Some((ready, resume)) = pause {
                let _ = ready.send(());
                let _ = resume.recv();
            }
        }
        let mut retirement_seconds = 0.0;
        let result = committed.and_then(|committed| {
            let started = Instant::now();
            let result = committed.retire();
            retirement_seconds = started.elapsed().as_secs_f64();
            result
        });
        let seconds = started.elapsed().as_secs_f64();
        {
            let Ok(mut state) = state.lock() else {
                return;
            };
            let locked = Instant::now();
            match result {
                Ok(_) => {
                    state.seal_stats.completed += 1;
                    state.checked_seal(job.ino, job.version);
                }
                Err(error) => {
                    state.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(job.ino));
                }
            }
            state.seal_stats.publish_seconds += seconds;
            state.seal_stats.catalog_publish_seconds += catalog_seconds;
            state.seal_stats.retirement_seconds += retirement_seconds;
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

pub(super) fn stop(state: &Mutex<State>, workers: Workers) {
    if let Ok(mut state) = state.lock() {
        state.stopping = true;
        if let Some(wake) = &state.wake {
            wake.notify_all();
        }
    }
    for worker in workers.0 {
        if worker.join().is_err() {
            eprintln!("sl7: background sealing worker panicked");
            if let Ok(mut state) = state.lock() {
                state.worker_failed = true;
            }
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

    #[test]
    fn worker_limit_is_bounded() {
        assert_eq!(worker_count(None).unwrap(), 2);
        for count in 1..=8 {
            assert_eq!(worker_count(Some(&count.to_string())).unwrap(), count);
        }
        for value in ["", "0", "9", "-1", "two", "18446744073709551616"] {
            assert!(worker_count(Some(value)).is_err());
        }
    }

    #[test]
    fn native_state_cache_is_revoked_before_publication_and_relearned_after_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs =
            StagingFs::deduplicating_with_workers(crate::Store::init(&root).unwrap(), 1).unwrap();
        let mut bytes = vec![42; 128 * 1024];
        fs::write(root.join("tree/data"), &bytes).unwrap();
        let ino = fs
            .with(|s| {
                let ino = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
                s.entry_attr(ino)?;
                Ok(ino)
            })
            .unwrap();
        let fh = fs.open_handle(ino, libc::O_RDWR).unwrap();
        let checks = fs
            .with_read(|s| {
                assert!(s.nodes[&ino].native_unoverridden);
                Ok(s.seal_stats.native_state_checks)
            })
            .unwrap();
        for value in 0..4 {
            fs.native_io(ino, fh, true, |f| f.write_all_at(&[value], 0))
                .unwrap();
        }
        bytes[0] = 3;
        assert_eq!(fs.read_handle(ino, fh, 0, bytes.len()).unwrap(), bytes);
        fs.with_read(|s| {
            assert_eq!(s.seal_stats.native_state_checks, checks);
            assert!(s.seal_stats.native_state_cache_hits >= 4);
            assert_eq!(s.attr(ino)?.size, bytes.len() as u64);
            Ok(())
        })
        .unwrap();

        let (ready, publishing) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        fs.with_read(|s| {
            s.publish_pause = Some((ready, paused));
            s.release(ino, fh)
        })
        .unwrap();
        publishing.recv_timeout(Duration::from_secs(10)).unwrap();
        let revoked = fs
            .with_read(|s| Ok(!s.nodes[&ino].native_unoverridden))
            .unwrap();
        resume.send(()).unwrap();
        assert!(revoked, "native fast path survived publication gate");
        idle(&fs);
        let reader = fs.open_handle(ino, libc::O_RDONLY).unwrap();
        assert_eq!(fs.read_handle(ino, reader, 0, bytes.len()).unwrap(), bytes);
        let writer = fs.open_handle(ino, libc::O_RDWR).unwrap();
        fs.with_read(|s| {
            // Hydration keeps the logical timestamp override until mutation.
            assert!(!s.nodes[&ino].native_unoverridden);
            assert!(
                s.live
                    .as_ref()
                    .unwrap()
                    .metadata(s.inode(ino)?)
                    .unwrap()
                    .is_some()
            );
            Ok(())
        })
        .unwrap();
        fs.native_io(ino, writer, true, |f| {
            f.write_all_at(b"new", bytes.len() as u64)
        })
        .unwrap();
        bytes.extend_from_slice(b"new");
        fs.with_read(|s| {
            assert!(s.nodes[&ino].native_unoverridden);
            assert!(
                s.live
                    .as_ref()
                    .unwrap()
                    .metadata(s.inode(ino)?)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(s.attr(ino)?.size, bytes.len() as u64);
            Ok(())
        })
        .unwrap();
        assert_eq!(fs.read_handle(ino, reader, 0, bytes.len()).unwrap(), bytes);
        fs.with_read(|s| {
            s.release(ino, reader)?;
            s.release(ino, writer)
        })
        .unwrap();
        drop(fs);
        let live = crate::live::Live::open(crate::Store::open(&root).unwrap()).unwrap();
        let file = File::open(root.join("tree/data")).unwrap();
        assert_eq!(live.read(&file, 0, bytes.len()).unwrap().unwrap(), bytes);
        live.store.check().unwrap();
    }

    #[test]
    fn another_worker_publishes_while_first_candidate_is_paused() {
        two_workers_progress(false);
    }

    #[test]
    fn another_worker_publishes_before_first_identity_sync_finishes() {
        two_workers_progress(true);
    }

    fn two_workers_progress(pause_identity: bool) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs =
            StagingFs::deduplicating_with_workers(crate::Store::init(&root).unwrap(), 2).unwrap();
        // Both workers prepare identical, initially absent chunks. The second
        // publishes first, so the first must resolve its now-stale chunk lookup.
        let bytes = vec![42; 256 * 1024];
        for name in ["first", "second"] {
            fs::write(root.join("tree").join(name), &bytes).unwrap();
        }
        let (ready, prepared) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        fs.with(|s| {
            if pause_identity {
                s.identity_pause = Some((ready, paused));
            } else {
                s.seal_pause = Some((ready, paused));
            }
            let ino = s.lookup(INodeNo::ROOT, OsStr::new("first"))?;
            s.entry_attr(ino)?;
            let fh = s.open(ino, libc::O_RDONLY)?;
            s.release(ino, fh)
        })
        .unwrap();
        prepared.recv_timeout(Duration::from_secs(10)).unwrap();
        let second = fs
            .with_read(|s| {
                let ino = s.lookup(INodeNo::ROOT, OsStr::new("second"))?;
                s.entry_attr(ino)?;
                let fh = s.open(ino, libc::O_RDONLY)?;
                s.release(ino, fh)?;
                Ok(ino)
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let progressed = loop {
            let complete = fs
                .with_read(|s| {
                    Ok(s.live
                        .as_ref()
                        .unwrap()
                        .sealed(s.inode(second)?)
                        .map_err(storage)?
                        .is_some()
                        && !s.nodes[&second].packing)
                })
                .unwrap();
            if complete || Instant::now() >= deadline {
                break complete;
            }
            thread::sleep(Duration::from_millis(5));
        };
        // Always release the worker before assertions, including on failure.
        resume.send(()).unwrap();
        idle(&fs);
        assert!(progressed, "second candidate waited for the first worker");
        fs.with(|s| {
            assert_eq!(s.seal_stats.completed, 2);
            assert_eq!(s.seal_stats.errors, 0);
            assert_eq!(s.seal_stats.max_parallel_seals, 2);
            assert_eq!(s.seal_stats.active_seals, 0);
            assert_eq!(s.seal_stats.active_candidates, 0);
            if pause_identity {
                assert_eq!(s.seal_stats.max_parallel_candidates, 2);
            }
            Ok(())
        })
        .unwrap();
        drop(fs);
        let live = crate::live::Live::open(crate::Store::open(&root).unwrap()).unwrap();
        for name in ["first", "second"] {
            let file = File::open(root.join("tree").join(name)).unwrap();
            assert_eq!(live.read(&file, 0, bytes.len()).unwrap().unwrap(), bytes);
        }
        live.store.check().unwrap();
    }

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
    fn unchanged_seals_are_not_requeued_and_mutation_restores_eligibility() {
        for presealed in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("store");
            let mut live = crate::live::Live::open(crate::Store::init(&root).unwrap()).unwrap();
            let mut bytes = vec![42; 128 * 1024];
            fs::write(root.join("tree/data"), &bytes).unwrap();
            if presealed {
                live.seal(&File::open(root.join("tree/data")).unwrap())
                    .unwrap();
            }
            drop(live);
            let fs = StagingFs::deduplicating(crate::Store::open(&root).unwrap()).unwrap();
            let ino = fs
                .with(|s| {
                    let ino = s.lookup(INodeNo::ROOT, OsStr::new("data"))?;
                    s.entry_attr(ino)?;
                    let fh = s.open(ino, libc::O_RDONLY)?;
                    s.release(ino, fh)?;
                    Ok(ino)
                })
                .unwrap();
            idle(&fs);
            let before = fs
                .with_read(|s| {
                    assert!(s.nodes[&ino].seal_checked);
                    Ok((s.seal_stats.completed, s.seal_stats.skipped))
                })
                .unwrap();
            for _ in 0..8 {
                let fh = fs.open_handle(ino, libc::O_RDONLY).unwrap();
                assert_eq!(fs.read_handle(ino, fh, 0, bytes.len()).unwrap(), bytes);
                fs.with_read(|s| s.release(ino, fh)).unwrap();
            }
            idle(&fs);
            fs.with_read(|s| {
                assert_eq!((s.seal_stats.completed, s.seal_stats.skipped), before);
                assert_eq!(s.seal_stats.unchanged_seals_avoided, 8);
                Ok(())
            })
            .unwrap();
            let fh = fs.open_handle(ino, libc::O_RDWR).unwrap();
            fs.with_read(|s| {
                assert!(!s.nodes[&ino].seal_checked);
                Ok(())
            })
            .unwrap();
            fs.write_handle(ino, fh, 0, &[77]).unwrap();
            bytes[0] = 77;
            fs.with_read(|s| s.release(ino, fh)).unwrap();
            idle(&fs);
            fs.with_read(|s| {
                assert_eq!(s.seal_stats.completed, before.0 + 1);
                assert!(s.nodes[&ino].seal_checked);
                Ok(())
            })
            .unwrap();
            drop(fs);
            let live = crate::live::Live::open(crate::Store::open(&root).unwrap()).unwrap();
            assert_eq!(
                live.read(&File::open(root.join("tree/data")).unwrap(), 0, bytes.len())
                    .unwrap()
                    .unwrap(),
                bytes
            );
            live.store.check().unwrap();
        }
    }

    #[test]
    fn successful_seal_absorbs_readonly_close_queued_during_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs = StagingFs::deduplicating(crate::Store::init(&root).unwrap()).unwrap();
        fs::write(root.join("tree/data"), vec![77; 128 * 1024]).unwrap();
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
        fs.with(|s| {
            let fh = s.open(ino, libc::O_RDONLY)?;
            s.release(ino, fh)?;
            assert!(s.nodes[&ino].queued);
            Ok(())
        })
        .unwrap();
        resume.send(()).unwrap();
        idle(&fs);
        fs.with_read(|s| {
            assert_eq!(s.seal_stats.completed, 1);
            assert_eq!(s.seal_stats.skipped, 0);
            assert!(s.nodes[&ino].seal_checked);
            Ok(())
        })
        .unwrap();
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
    fn retirement_blocks_its_inode_but_allows_unrelated_catalog_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let fs = StagingFs::deduplicating(crate::Store::init(&root).unwrap()).unwrap();
        let bytes = vec![42; 128 * 1024];
        fs::write(root.join("tree/first"), &bytes).unwrap();
        fs::write(root.join("tree/second"), &bytes).unwrap();
        let (ready, retiring) = mpsc::sync_channel(1);
        let (resume, paused) = mpsc::sync_channel(1);
        let (a, b) = fs
            .with(|s| {
                let a = s.lookup(INodeNo::ROOT, OsStr::new("first"))?;
                let b = s.lookup(INodeNo::ROOT, OsStr::new("second"))?;
                s.entry_attr(a)?;
                s.entry_attr(b)?;
                let pin = s.inode(b)?.try_clone()?;
                s.live.as_mut().unwrap().seal(&pin).map_err(storage)?;
                s.retire_pause = Some((ready, paused));
                let fh = s.open(a, libc::O_RDONLY)?;
                s.release(a, fh)?;
                Ok((a, b))
            })
            .unwrap();
        retiring.recv_timeout(Duration::from_secs(10)).unwrap();
        thread::scope(|scope| {
            let fs = &fs;
            let writer = scope.spawn(move || {
                let fh = fs.open_handle(a, libc::O_RDWR)?;
                fs.write_handle(a, fh, 0, b"changed")?;
                fs.with_read(|s| s.release(a, fh))
            });
            let namespace = scope.spawn(move || {
                fs.with_namespace_mutation(
                    |_| Ok(vec![a]),
                    |s| {
                        fs::hard_link(
                            s.child(INodeNo::ROOT, OsStr::new("first"))?,
                            s.child(INodeNo::ROOT, OsStr::new("alias"))?,
                        )?;
                        Ok(((), vec![a]))
                    },
                    |_, ()| Ok(()),
                )
            });
            // Both mutation entry points must actually reach the inode gate.
            // Merely not receiving a result could mean a thread wasn't scheduled.
            let deadline = Instant::now() + Duration::from_secs(5);
            let blocked = loop {
                if fs
                    .with_read(|s| Ok(s.nodes.get(&a).is_some_and(|n| n.waiting_mutations == 2)))
                    .unwrap()
                {
                    break true;
                }
                if writer.is_finished() || namespace.is_finished() || Instant::now() >= deadline {
                    break false;
                }
                thread::sleep(Duration::from_millis(1));
            };
            let (done, completed) = mpsc::sync_channel(1);
            scope.spawn(move || {
                let result = (|| {
                    // Hydration commits on another connection, and fsync writes
                    // a FULL barrier: both need the released catalog permit.
                    let fh = fs.open_handle(b, libc::O_RDWR)?;
                    fs.write_handle(b, fh, 0, b"other")?;
                    fs.sync_handle(b, fh, false)?;
                    let value = fs.read_handle(b, fh, 0, 5)?;
                    fs.with_read(|s| s.release(b, fh))?;
                    Ok::<_, Errno>(value)
                })();
                done.send(result).unwrap();
            });
            let independent = completed.recv_timeout(Duration::from_secs(5));
            let during_retirement = fs.with_read(|s| {
                Ok((
                    s.nodes.get(&a).is_some_and(|n| n.publishing),
                    s.live
                        .as_ref()
                        .unwrap()
                        .sealed(s.inode(a)?)
                        .map_err(storage)?
                        .is_some(),
                    s.attr(a)?.size,
                ))
            });
            // Resume before assertions/joins even when the overlap check failed.
            resume.send(()).unwrap();
            writer.join().unwrap().unwrap();
            namespace.join().unwrap().unwrap();
            let (still_publishing, bound, size) = during_retirement.unwrap();
            assert!(bound);
            assert_eq!(size, bytes.len() as u64);
            assert!(
                blocked && still_publishing,
                "same-inode mutation bypassed retirement"
            );
            assert_eq!(
                independent
                    .expect("retirement held the catalog permit")
                    .unwrap(),
                b"other"
            );
        });
        let fh = fs.open_handle(a, libc::O_RDONLY).unwrap();
        let mut expected = bytes;
        expected[..7].copy_from_slice(b"changed");
        assert_eq!(fs.read_handle(a, fh, 0, expected.len()).unwrap(), expected);
        fs.with_read(|s| {
            assert_eq!(s.lookup(INodeNo::ROOT, OsStr::new("alias"))?, a);
            assert!(s.seal_stats.mutation_publication_wait_seconds > 0.0);
            s.release(a, fh)
        })
        .unwrap();
        drop(fs);
        crate::Store::open(&root).unwrap().check().unwrap();
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
            // Inject failure after manifest insertion but before the atomic
            // manifest/binding transaction can commit.
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
            let stats = s.live.as_ref().unwrap().store.stats().map_err(storage)?;
            assert_eq!(stats.files, 0, "failed binding left an immutable manifest");
            assert_eq!(stats.unique_chunks, 0, "failed binding published chunks");
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
