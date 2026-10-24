//! Shared, bounded CPU workers for independent version-1 chunk identities.
//! Splitting, whole-file SHA, catalog queries and pack order stay on the caller.
use crate::{
    Error, Result,
    chunk::{self, Hash},
    import_stream::Chunks,
};
use std::{
    cell::Cell,
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

const BATCH: usize = 16;
const WINDOW: usize = 4;
const MIN_SIZE: u64 = 1024 * 1024;
type Hashed = (Hash, Vec<u8>);
struct Completed {
    result: Result<Vec<Hashed>>,
    bytes: u64,
}
struct Job {
    batch: Vec<Vec<u8>>,
    canceled: Arc<AtomicBool>,
    result: mpsc::Sender<Completed>,
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

/// One pool per mount, shared by every sealer. Never one pool per artifact.
pub(crate) struct Pool {
    count: usize,
    sender: Option<mpsc::SyncSender<Job>>,
    threads: Vec<JoinHandle<()>>,
    activity: Arc<Activity>,
}
impl Pool {
    pub(crate) fn new(count: usize) -> Result<Self> {
        Self::start(count, None)
    }
    fn start(count: usize, hook: Option<Arc<dyn Fn() + Send + Sync>>) -> Result<Self> {
        if count > 4 {
            return Err(Error::Invalid(
                "SL7_HASH_WORKERS must be 0 through 4".into(),
            ));
        }
        let mut pool = Self {
            count,
            sender: None,
            threads: Vec::new(),
            activity: Arc::default(),
        };
        if count == 0 {
            return Ok(pool);
        }
        let (sender, receiver) = mpsc::sync_channel::<Job>(count * 2);
        let receiver = Arc::new(Mutex::new(receiver));
        pool.sender = Some(sender);
        for index in 0..count {
            let receiver = Arc::clone(&receiver);
            let activity = Arc::clone(&pool.activity);
            let hook = hook.clone();
            pool.threads.push(
                thread::Builder::new()
                    .name(format!("sl7-hash-{index}"))
                    .spawn(move || {
                        loop {
                            // This mutex protects dequeue only, never hashing or result delivery.
                            let job = match receiver.lock() {
                                Ok(queue) => queue.recv(),
                                Err(_) => return,
                            };
                            let Ok(job) = job else {
                                return;
                            };
                            let active = activity.active.fetch_add(1, Ordering::Relaxed) + 1;
                            activity.peak.fetch_max(active, Ordering::Relaxed);
                            let guard = Active(&activity);
                            let mut bytes_hashed = 0;
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    if let Some(hook) = &hook {
                                        hook();
                                    }
                                    job.batch
                                        .into_iter()
                                        .map(|bytes| {
                                            if job.canceled.load(Ordering::Relaxed) {
                                                return Err(Error::SourceChanged(
                                                    "chunk hashing canceled".into(),
                                                ));
                                            }
                                            let hash = chunk::identity(&bytes);
                                            bytes_hashed += bytes.len() as u64;
                                            activity.chunks.fetch_add(1, Ordering::Relaxed);
                                            Ok((hash, bytes))
                                        })
                                        .collect()
                                }))
                                .unwrap_or_else(|_| {
                                    Err(Error::Invalid("chunk hashing worker panicked".into()))
                                });
                            drop(guard);
                            let _ = job.result.send(Completed {
                                result,
                                bytes: bytes_hashed,
                            });
                        }
                    })?,
            );
        }
        Ok(pool)
    }
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    pub(crate) fn from_env() -> Result<Self> {
        let value = std::env::var("SL7_HASH_WORKERS")
            .or_else(|e| match e {
                std::env::VarError::NotPresent => Ok("0".into()),
                e => Err(e),
            })
            .map_err(|_| Error::Invalid("SL7_HASH_WORKERS must be 0 through 4".into()))?;
        Self::parse(&value)
    }
    #[cfg(any(test, all(feature = "fuse", target_os = "linux")))]
    fn parse(value: &str) -> Result<Self> {
        match value {
            "0" | "1" | "2" | "3" | "4" => Self::new(value.parse().unwrap()),
            _ => Err(Error::Invalid(
                "SL7_HASH_WORKERS must be 0 through 4".into(),
            )),
        }
    }
    #[cfg(any(test, all(feature = "fuse", target_os = "linux")))]
    pub(crate) fn stats(&self) -> (usize, usize, usize) {
        (
            self.count,
            self.activity.peak.load(Ordering::Relaxed),
            self.activity.chunks.load(Ordering::Relaxed),
        )
    }
    fn submit(
        &self,
        batch: Vec<Vec<u8>>,
        canceled: Arc<AtomicBool>,
    ) -> Result<mpsc::Receiver<Completed>> {
        let (result, receipt) = mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| Error::Invalid("hash pool disabled".into()))?
            .send(Job {
                batch,
                canceled,
                result,
            })
            .map_err(|_| Error::Invalid("hash workers stopped".into()))?;
        Ok(receipt)
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.sender.take();
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
    }
}

/// At most four outstanding batches plus a current batch: <=2.5 MiB per sealer.
/// Results are consumed in submission order even when workers finish out of order.
/// Drop cancels and drains all receipts, so no hashing outlives a preparation.
pub(crate) struct Stream<'a, C: Chunks> {
    source: &'a mut C,
    pool: &'a Pool,
    parallel: bool,
    exhausted: bool,
    pending: VecDeque<mpsc::Receiver<Completed>>,
    current: std::vec::IntoIter<Hashed>,
    canceled: Arc<AtomicBool>,
    hash_bytes: &'a Cell<u64>,
}
impl<'a, C: Chunks> Stream<'a, C> {
    pub(crate) fn new(
        source: &'a mut C,
        pool: &'a Pool,
        size: u64,
        hash_bytes: &'a Cell<u64>,
    ) -> Self {
        Self {
            source,
            pool,
            parallel: pool.count != 0 && size >= MIN_SIZE,
            exhausted: false,
            pending: VecDeque::new(),
            current: Vec::new().into_iter(),
            canceled: Arc::new(AtomicBool::new(false)),
            hash_bytes,
        }
    }
    fn account(&self, bytes: u64) {
        self.hash_bytes
            .set(self.hash_bytes.get().saturating_add(bytes));
    }
    pub(crate) fn next(&mut self, canceled: &impl Fn() -> bool) -> Result<Option<Hashed>> {
        if canceled() {
            return Err(Error::SourceChanged("sealing candidate canceled".into()));
        }
        if !self.parallel {
            return self
                .source
                .next_chunk()?
                .map(|bytes| {
                    let hash = chunk::identity(&bytes);
                    self.account(bytes.len() as u64);
                    Ok((hash, bytes))
                })
                .transpose();
        }
        if let Some(chunk) = self.current.next() {
            return Ok(Some(chunk));
        }
        while !self.exhausted && self.pending.len() < WINDOW {
            let mut batch = Vec::with_capacity(BATCH);
            for _ in 0..BATCH {
                if canceled() {
                    return Err(Error::SourceChanged("sealing candidate canceled".into()));
                }
                match self.source.next_chunk()? {
                    Some(bytes) => batch.push(bytes),
                    None => {
                        self.exhausted = true;
                        break;
                    }
                }
            }
            if !batch.is_empty() {
                self.pending
                    .push_back(self.pool.submit(batch, Arc::clone(&self.canceled))?);
            }
        }
        let Some(receipt) = self.pending.pop_front() else {
            return Ok(None);
        };
        let completed = receipt
            .recv()
            .map_err(|_| Error::Invalid("hash worker lost result".into()))?;
        self.account(completed.bytes);
        self.current = completed.result?.into_iter();
        if canceled() {
            return Err(Error::SourceChanged("sealing candidate canceled".into()));
        }
        Ok(self.current.next())
    }
    pub(crate) fn finish(&mut self) -> Result<Hash> {
        if !self.pending.is_empty() || self.current.len() != 0 || (self.parallel && !self.exhausted)
        {
            return Err(Error::Invalid(
                "hash stream must be drained before finishing".into(),
            ));
        }
        self.source.finish()
    }
}
impl<C: Chunks> Drop for Stream<'_, C> {
    fn drop(&mut self) {
        self.canceled.store(true, Ordering::Relaxed);
        while let Some(receipt) = self.pending.pop_front() {
            if let Ok(completed) = receipt.recv() {
                self.account(completed.bytes);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import_stream::Serial;
    use sha2::{Digest, Sha256};
    use std::{
        io::{self, Cursor},
        sync::Barrier,
    };

    #[test]
    fn ordered_results_match_serial_and_two_workers_overlap_on_one_file() -> Result<()> {
        let barrier = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let pool = Pool::start(
            2,
            Some(Arc::new(move || {
                // Force the first two independent batches to execute together.
                if calls.fetch_add(1, Ordering::Relaxed) < 2 {
                    barrier.wait();
                }
            })),
        )?;
        let bytes: Vec<_> = (0..MIN_SIZE as usize + 13)
            .map(|n| (n.wrapping_mul(7919) >> 8) as u8)
            .collect();
        let mut serial = Serial::new(Cursor::new(&bytes));
        let mut expected = Vec::new();
        while let Some(bytes) = serial.next_chunk()? {
            expected.push((chunk::identity(&bytes), bytes));
        }
        let mut source = Serial::new(Cursor::new(&bytes));
        let count = Cell::new(0);
        let mut stream = Stream::new(&mut source, &pool, bytes.len() as u64, &count);
        let mut actual = Vec::new();
        while let Some(chunk) = stream.next(&|| false)? {
            actual.push(chunk);
        }
        assert_eq!(actual, expected);
        assert_eq!(stream.finish()?, <Hash>::from(Sha256::digest(&bytes)));
        assert_eq!(pool.stats(), (2, 2, expected.len()));
        assert_eq!(count.get(), bytes.len() as u64);
        Ok(())
    }
    #[test]
    fn cancellation_and_read_errors_drain_workers_and_pool_can_be_reused() -> Result<()> {
        struct Broken {
            count: usize,
        }
        impl Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.count == 0 {
                    return Err(io::ErrorKind::BrokenPipe.into());
                }
                let n = buf.len().min(self.count);
                buf[..n].fill(42);
                self.count -= n;
                Ok(n)
            }
        }
        use std::io::Read;
        let pool = Pool::new(2)?;
        let count = Cell::new(0);
        let mut source = Serial::new(io::repeat(42));
        {
            let mut stream = Stream::new(&mut source, &pool, u64::MAX, &count);
            assert!(stream.next(&|| false)?.is_some());
            assert!(matches!(
                stream.next(&|| true),
                Err(Error::SourceChanged(_))
            ));
        }
        assert_eq!(pool.activity.active.load(Ordering::Relaxed), 0);
        assert!(count.get() <= (WINDOW * BATCH * chunk::MAX) as u64);
        let mut source = Serial::new(Broken {
            count: 20 * chunk::MAX,
        });
        {
            let mut stream = Stream::new(&mut source, &pool, u64::MAX, &count);
            assert!(
                matches!(stream.next(&|| false), Err(Error::Io(e)) if e.kind() == io::ErrorKind::BrokenPipe)
            );
        }
        assert_eq!(pool.activity.active.load(Ordering::Relaxed), 0);
        let mut source = Serial::new(Cursor::new(vec![42; MIN_SIZE as usize]));
        let mut stream = Stream::new(&mut source, &pool, MIN_SIZE, &count);
        while stream.next(&|| false)?.is_some() {}
        stream.finish()?;
        Ok(())
    }
    #[test]
    fn panic_becomes_error_and_pool_survives() -> Result<()> {
        let first = AtomicBool::new(true);
        let pool = Pool::start(
            2,
            Some(Arc::new(move || {
                assert!(!first.swap(false, Ordering::Relaxed), "injected hash panic");
            })),
        )?;
        let count = Cell::new(0);
        for attempt in 0..2 {
            let mut source = Serial::new(Cursor::new(vec![42; MIN_SIZE as usize]));
            let mut stream = Stream::new(&mut source, &pool, MIN_SIZE, &count);
            let result = (|| {
                while stream.next(&|| false)?.is_some() {}
                stream.finish()
            })();
            if attempt == 0 {
                assert!(matches!(result, Err(Error::Invalid(_))));
            } else {
                result?;
            }
        }
        Ok(())
    }
    #[test]
    fn small_files_and_empty_inputs_bypass_pool_and_configuration_is_bounded() -> Result<()> {
        for value in ["", "5", "-1", "true", " 2"] {
            assert!(Pool::parse(value).is_err());
        }
        let pool = Pool::new(4)?;
        for bytes in [vec![], vec![1, 2, 3], vec![42; MIN_SIZE as usize - 1]] {
            let mut source = Serial::new(Cursor::new(&bytes));
            let count = Cell::new(0);
            let mut stream = Stream::new(&mut source, &pool, bytes.len() as u64, &count);
            let mut actual = Vec::new();
            while let Some((_, bytes)) = stream.next(&|| false)? {
                actual.extend(bytes);
            }
            assert_eq!(actual, bytes);
            assert_eq!(stream.finish()?, <Hash>::from(Sha256::digest(&bytes)));
        }
        assert_eq!(pool.stats(), (4, 0, 0));
        Ok(())
    }
}
