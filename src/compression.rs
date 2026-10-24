//! Bounded, persistent compression workers. Only catalog misses enter this pool.
use crate::{Error, Result, chunk::Hash, pack};
use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

const BATCH: usize = 16;
const WINDOW: usize = 4;
type Batch = Vec<(Hash, Vec<u8>)>;
type EncodedBatch = Vec<(Hash, pack::Encoded)>;
type Receipt = mpsc::Receiver<Result<EncodedBatch>>;
struct Job {
    bytes: Batch,
    result: mpsc::Sender<Result<EncodedBatch>>,
    canceled: Arc<AtomicBool>,
}
#[derive(Default)]
struct Activity {
    active: AtomicUsize,
    peak: AtomicUsize,
    chunks: AtomicUsize,
}
struct Active<'a>(&'a Activity);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One pool per sealing worker; threads are created lazily and reused across files.
pub(crate) struct Encoder {
    count: usize,
    sender: Option<mpsc::SyncSender<Job>>,
    threads: Vec<JoinHandle<()>>,
    activity: Arc<Activity>,
    #[cfg(test)]
    before_encode: Option<Arc<dyn Fn() + Send + Sync>>,
}
impl Encoder {
    pub(crate) fn new(count: usize) -> Self {
        Self {
            count,
            sender: None,
            threads: Vec::new(),
            activity: Arc::default(),
            #[cfg(test)]
            before_encode: None,
        }
    }
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    pub(crate) fn from_env() -> Result<Self> {
        let value = match std::env::var("SL7_COMPRESS_WORKERS") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => "0".into(),
            Err(_) => {
                return Err(Error::Invalid(
                    "SL7_COMPRESS_WORKERS must be 0 through 4".into(),
                ));
            }
        };
        Self::parse(&value)
    }
    #[cfg(any(test, all(feature = "fuse", target_os = "linux")))]
    fn parse(value: &str) -> Result<Self> {
        match value {
            "0" | "1" | "2" | "3" | "4" => Ok(Self::new(value.parse().unwrap())),
            _ => Err(Error::Invalid(
                "SL7_COMPRESS_WORKERS must be 0 through 4".into(),
            )),
        }
    }
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    pub(crate) fn stats(&self) -> (usize, usize, usize) {
        (
            self.count,
            self.activity.peak.load(Ordering::Relaxed),
            self.activity.chunks.load(Ordering::Relaxed),
        )
    }
    fn start(&mut self) -> Result<()> {
        if self.sender.is_some() {
            return Ok(());
        }
        let (sender, receiver) = mpsc::sync_channel::<Job>(WINDOW);
        let receiver = Arc::new(Mutex::new(receiver));
        for index in 0..self.count {
            let receiver = Arc::clone(&receiver);
            let activity = Arc::clone(&self.activity);
            #[cfg(test)]
            let before_encode = self.before_encode.clone();
            let spawned = thread::Builder::new()
                .name(format!("sl7-zstd-{index}"))
                .spawn(move || {
                    loop {
                        // Release the queue lock before doing any compression.
                        let job = match receiver.lock() {
                            Ok(receiver) => receiver.recv(),
                            Err(_) => return,
                        };
                        let Ok(job) = job else {
                            return;
                        };
                        let active = activity.active.fetch_add(1, Ordering::Relaxed) + 1;
                        activity.peak.fetch_max(active, Ordering::Relaxed);
                        let guard = Active(&activity);
                        #[cfg(test)]
                        if let Some(hook) = &before_encode {
                            hook();
                        }
                        let result = job
                            .bytes
                            .into_iter()
                            .map(|(hash, bytes)| {
                                if job.canceled.load(Ordering::Relaxed) {
                                    return Err(Error::SourceChanged(
                                        "compression canceled".into(),
                                    ));
                                }
                                let encoded = pack::Encoded::new(bytes)?;
                                activity.chunks.fetch_add(1, Ordering::Relaxed);
                                Ok((hash, encoded))
                            })
                            .collect();
                        drop(guard);
                        let _ = job.result.send(result);
                    }
                });
            match spawned {
                Ok(handle) => self.threads.push(handle),
                Err(error) => {
                    drop(sender);
                    for handle in self.threads.drain(..) {
                        let _ = handle.join();
                    }
                    return Err(error.into());
                }
            }
        }
        self.sender = Some(sender);
        Ok(())
    }
    fn submit(&mut self, bytes: Batch, canceled: Arc<AtomicBool>) -> Result<Receipt> {
        self.start()?;
        let (result, receipt) = mpsc::channel();
        self.sender
            .as_ref()
            .unwrap()
            .send(Job {
                bytes,
                result,
                canceled,
            })
            .map_err(|_| Error::Invalid("compression workers stopped".into()))?;
        Ok(receipt)
    }
}
impl Drop for Encoder {
    fn drop(&mut self) {
        self.sender.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// At most four submitted batches plus one accumulating batch per import. Each
/// batch has at most 16 * 32 KiB. Ordered receipts preserve exact pack bytes.
pub(crate) struct Packs<'a> {
    root: &'a Path,
    limit: u64,
    encoder: &'a mut Encoder,
    parallel: bool,
    batch: Batch,
    pending: VecDeque<Receipt>,
    canceled: Arc<AtomicBool>,
    writer: Option<pack::Writer>,
    packs: Vec<(String, u64)>,
    locations: Vec<pack::Location>,
}
impl<'a> Packs<'a> {
    pub(crate) fn new(root: &'a Path, limit: u64, encoder: &'a mut Encoder, size: u64) -> Self {
        let parallel = encoder.count != 0 && size >= 16 * 1024 * 1024;
        Self {
            root,
            limit,
            encoder,
            parallel,
            batch: Vec::new(),
            pending: VecDeque::new(),
            canceled: Arc::new(AtomicBool::new(false)),
            writer: None,
            packs: Vec::new(),
            locations: Vec::new(),
        }
    }
    pub(crate) fn add(&mut self, hash: Hash, bytes: Vec<u8>) -> Result<()> {
        if !self.parallel {
            self.rollover(bytes.len() as u64)?;
            return self.writer.as_mut().unwrap().add(hash, &bytes);
        }
        self.batch.push((hash, bytes));
        if self.batch.len() == BATCH {
            self.submit()?;
        }
        Ok(())
    }
    fn submit(&mut self) -> Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        if self.pending.len() == WINDOW {
            self.drain_one()?;
        }
        self.pending.push_back(
            self.encoder
                .submit(std::mem::take(&mut self.batch), Arc::clone(&self.canceled))?,
        );
        Ok(())
    }
    fn drain_one(&mut self) -> Result<()> {
        let batch =
            self.pending.pop_front().unwrap().recv().map_err(|_| {
                Error::Invalid("compression worker stopped before replying".into())
            })??;
        for (hash, encoded) in batch {
            self.rollover(encoded.raw_len)?;
            self.writer.as_mut().unwrap().add_encoded(hash, &encoded)?;
        }
        Ok(())
    }
    fn finish_writer(&mut self) -> Result<()> {
        if let Some(writer) = self.writer.take() {
            let (id, size, locations) = writer.finish(self.root)?;
            self.packs.push((id, size));
            self.locations.extend(locations);
        }
        Ok(())
    }
    fn rollover(&mut self, raw_len: u64) -> Result<()> {
        if self
            .writer
            .as_ref()
            .is_some_and(|w| w.size + raw_len + 49 > self.limit)
        {
            self.finish_writer()?;
        }
        if self.writer.is_none() {
            self.writer = Some(pack::Writer::new(self.root)?);
        }
        Ok(())
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
        self.submit()?;
        while !self.pending.is_empty() {
            self.drain_one()?;
        }
        self.finish_writer()
    }
    pub(crate) fn take_results(&mut self) -> (Vec<(String, u64)>, Vec<pack::Location>) {
        (
            std::mem::take(&mut self.packs),
            std::mem::take(&mut self.locations),
        )
    }
}
impl Drop for Packs<'_> {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Relaxed);
        // No background compression may outlive a preparation, including an
        // error/cancellation. Otherwise the mount could falsely report drained.
        for receipt in self.pending.drain(..) {
            let _ = receipt.recv();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Condvar, time::Duration};

    #[test]
    fn workers_execute_concurrently_and_survive_abandoned_results() -> Result<()> {
        let mut encoder = Encoder::new(2);
        let (entered, received) = mpsc::channel();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = Arc::clone(&gate);
        encoder.before_encode = Some(Arc::new(move || {
            entered.send(()).unwrap();
            let (lock, wake) = &*worker_gate;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
        }));
        let canceled = Arc::new(AtomicBool::new(false));
        let first = encoder.submit(vec![([1; 32], vec![1; 8192])], Arc::clone(&canceled))?;
        let second = encoder.submit(vec![([2; 32], vec![2; 8192])], Arc::clone(&canceled))?;
        let a = received.recv_timeout(Duration::from_secs(5));
        let b = received.recv_timeout(Duration::from_secs(5));
        // Always resume workers before assertions, including failed concurrency.
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        assert!(a.is_ok() && b.is_ok(), "compression did not overlap");
        drop(first);
        assert_eq!(second.recv().unwrap()?.len(), 1);
        let third = encoder.submit(vec![([3; 32], vec![3; 8192])], canceled)?;
        assert_eq!(third.recv().unwrap()?[0].0, [3; 32]);
        assert_eq!(encoder.activity.peak.load(Ordering::Relaxed), 2);
        Ok(())
    }

    #[test]
    fn cancellation_skips_work_and_worker_panic_disconnects_receipt() -> Result<()> {
        let mut encoder = Encoder::new(1);
        let canceled = Arc::new(AtomicBool::new(true));
        let result = encoder
            .submit(vec![([1; 32], vec![1; 8192])], canceled)?
            .recv()
            .unwrap();
        assert!(matches!(result, Err(Error::SourceChanged(_))));
        assert_eq!(encoder.activity.chunks.load(Ordering::Relaxed), 0);
        let mut broken = Encoder::new(1);
        broken.before_encode = Some(Arc::new(|| panic!("injected encoder failure")));
        let receipt = broken.submit(
            vec![([1; 32], vec![1; 8192])],
            Arc::new(AtomicBool::new(false)),
        )?;
        assert!(matches!(
            receipt.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        Ok(())
    }

    #[test]
    fn worker_configuration_is_bounded() {
        for value in ["", "-1", "5", "999", "true", "02"] {
            assert!(Encoder::parse(value).is_err());
        }
        for count in 0..=4 {
            assert_eq!(Encoder::parse(&count.to_string()).unwrap().count, count);
        }
    }
}
