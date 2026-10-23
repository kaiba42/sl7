//! One packing worker; publication alone re-enters the filesystem state lock.
use super::*;
use crate::{live::SealCandidate, store::Packer};
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
    prepare_seconds: f64,
    publish_seconds: f64,
    max_publish_seconds: f64,
    max_queue_len: usize,
}

struct Job {
    ino: INodeNo,
    pin: File,
    epoch: Arc<AtomicU64>,
    version: u64,
    candidate: SealCandidate,
    #[cfg(test)]
    pause: Option<(SyncSender<()>, Receiver<()>)>,
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
            let result = node
                .pin
                .try_clone()
                .map_err(crate::Error::from)
                .and_then(|pin| {
                    self.live
                        .as_mut()
                        .unwrap()
                        .begin_seal(&pin)
                        .map(|candidate| (pin, candidate))
                });
            match result {
                Ok((pin, Some(candidate))) => {
                    self.nodes.get_mut(&ino).unwrap().packing = true;
                    return Some(Job {
                        ino,
                        pin,
                        epoch,
                        version,
                        candidate,
                        #[cfg(test)]
                        pause: self.seal_pause.take(),
                    });
                }
                Ok((_, None)) => self.seal_stats.skipped += 1,
                Err(error) => {
                    self.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(ino));
                }
            }
            self.reclaim(ino);
        }
    }
}

pub(super) fn start(state: Arc<Mutex<State>>, packer: Packer) -> io::Result<JoinHandle<()>> {
    let (wake, receive) = mpsc::sync_channel(1);
    state
        .lock()
        .map_err(|_| io::Error::other("poisoned filesystem state"))?
        .wake = Some(wake);
    thread::Builder::new()
        .name("sl7-pack".into())
        .spawn(move || run(state, packer, receive))
}

fn run(state: Arc<Mutex<State>>, packer: Packer, wake: Receiver<()>) {
    loop {
        let job = {
            let Ok(mut state) = state.lock() else {
                return;
            };
            match state.take_job() {
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
        let started = Instant::now();
        let prepared = packer.prepare(&fd_path(&job.pin), || {
            job.epoch.load(Ordering::Relaxed) != job.version
        });
        let elapsed = started.elapsed().as_secs_f64();
        #[cfg(test)]
        if let Some((ready, resume)) = job.pause {
            let _ = ready.send(());
            let _ = resume.recv();
        }
        let Ok(mut state) = state.lock() else {
            return;
        };
        state.seal_stats.prepare_seconds += elapsed;
        let valid = state.nodes.get(&job.ino).is_some_and(|node| {
            node.opens == 0 && node.epoch.load(Ordering::Relaxed) == job.version
        });
        if !valid {
            state.seal_stats.canceled += 1;
        } else {
            match prepared {
                Ok(prepared) => {
                    let started = Instant::now();
                    match state.live.as_mut().unwrap().publish_seal(
                        &job.pin,
                        job.candidate,
                        prepared,
                    ) {
                        Ok(_) => state.seal_stats.completed += 1,
                        Err(error) => {
                            state.seal_stats.errors += 1;
                            eprintln!("sl7: sealing inode {}: {error}", u64::from(job.ino));
                        }
                    }
                    let seconds = started.elapsed().as_secs_f64();
                    state.seal_stats.publish_seconds += seconds;
                    state.seal_stats.max_publish_seconds =
                        state.seal_stats.max_publish_seconds.max(seconds);
                }
                Err(crate::Error::SourceChanged(_)) => state.seal_stats.canceled += 1,
                Err(error) => {
                    state.seal_stats.errors += 1;
                    eprintln!("sl7: sealing inode {}: {error}", u64::from(job.ino));
                }
            }
        }
        if let Some(node) = state.nodes.get_mut(&job.ino) {
            node.packing = false;
        }
        state.reclaim(job.ino);
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
