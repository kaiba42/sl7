//! Bounded overlap between reading/splitting/file hashing and chunk processing.
use crate::{
    Error, Result,
    chunk::{Chunker, Hash},
};
use sha2::{Digest, Sha256};
use std::{cell::Cell, io::Read, sync::mpsc, thread};

const BATCH_CHUNKS: usize = 16;
const QUEUED_BATCHES: usize = 2;
const MIN_PIPELINE_SIZE: u64 = 16 * 1024 * 1024;

/// One instance belongs to one packer. Counters need no cross-worker sharing.
pub(crate) struct Mode {
    enabled: bool,
    started: Cell<u64>,
}
impl Mode {
    pub(crate) fn disabled() -> Self {
        Self::new(false)
    }
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: Cell::new(0),
        }
    }
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    pub(crate) fn from_env() -> Result<Self> {
        match std::env::var("SL7_IMPORT_PIPELINE") {
            Err(std::env::VarError::NotPresent) => Ok(Self::disabled()),
            Ok(value) => Self::parse(&value),
            Err(_) => Err(Error::Invalid("SL7_IMPORT_PIPELINE must be 0 or 1".into())),
        }
    }
    #[cfg(any(test, all(feature = "fuse", target_os = "linux")))]
    fn parse(value: &str) -> Result<Self> {
        match value {
            "0" => Ok(Self::disabled()),
            "1" => Ok(Self::new(true)),
            _ => Err(Error::Invalid("SL7_IMPORT_PIPELINE must be 0 or 1".into())),
        }
    }
    #[cfg(all(feature = "fuse", target_os = "linux"))]
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }
    #[cfg(any(test, all(feature = "fuse", target_os = "linux")))]
    pub(crate) fn started(&self) -> u64 {
        self.started.get()
    }
    pub(crate) fn start_for(&self, bytes: u64) -> bool {
        if !self.enabled || bytes < MIN_PIPELINE_SIZE {
            return false;
        }
        self.started.set(self.started.get().saturating_add(1));
        true
    }
    #[cfg(test)]
    pub(crate) fn enabled_for_test() -> Self {
        Self::new(true)
    }
}

pub(crate) trait Chunks {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>>;
    fn finish(&mut self) -> Result<Hash>;
}
pub(crate) struct Serial<R> {
    chunks: Chunker<R>,
    digest: Sha256,
}
impl<R: Read> Serial<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            chunks: Chunker::new(reader),
            digest: Sha256::new(),
        }
    }
}
impl<R: Read> Chunks for Serial<R> {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        let bytes = self.chunks.next_chunk()?;
        if let Some(bytes) = &bytes {
            self.digest.update(bytes);
        }
        Ok(bytes)
    }
    fn finish(&mut self) -> Result<Hash> {
        Ok(std::mem::take(&mut self.digest).finalize().into())
    }
}
pub(crate) struct Pipeline<'scope> {
    receiver: mpsc::Receiver<Vec<Vec<u8>>>,
    pending: std::vec::IntoIter<Vec<u8>>,
    producer: Option<thread::ScopedJoinHandle<'scope, Result<Hash>>>,
    exhausted: bool,
}
impl Chunks for Pipeline<'_> {
    fn next_chunk(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(bytes) = self.pending.next() {
            return Ok(Some(bytes));
        }
        let batch = match self.receiver.recv() {
            Ok(batch) => batch,
            Err(_) => {
                self.exhausted = true;
                return Ok(None);
            }
        };
        self.pending = batch.into_iter();
        Ok(self.pending.next())
    }
    fn finish(&mut self) -> Result<Hash> {
        // Joining before consuming the queue can deadlock a blocked producer.
        if !self.exhausted {
            return Err(Error::Invalid(
                "chunk stream must be drained before finishing".into(),
            ));
        }
        self.producer
            .take()
            .ok_or_else(|| Error::Invalid("chunk producer already joined".into()))?
            .join()
            .map_err(|_| Error::Invalid("chunk producer panicked".into()))?
    }
}

pub(crate) fn with_pipeline<R: Read + Send, T>(
    reader: R,
    consume: impl FnOnce(&mut Pipeline<'_>) -> Result<T>,
) -> Result<T> {
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(QUEUED_BATCHES);
        let producer = thread::Builder::new()
            .name("sl7-chunks".into())
            .spawn_scoped(scope, move || {
                let mut chunks = Serial::new(reader);
                let mut batch = Vec::with_capacity(BATCH_CHUNKS);
                while let Some(bytes) = chunks.next_chunk()? {
                    batch.push(bytes);
                    if batch.len() == BATCH_CHUNKS {
                        let ready = std::mem::replace(&mut batch, Vec::with_capacity(BATCH_CHUNKS));
                        if sender.send(ready).is_err() {
                            return Err(Error::SourceChanged("chunk consumer stopped".into()));
                        }
                    }
                }
                if !batch.is_empty() && sender.send(batch).is_err() {
                    return Err(Error::SourceChanged("chunk consumer stopped".into()));
                }
                chunks.finish()
            })?;
        let mut stream = Pipeline {
            receiver,
            pending: Vec::new().into_iter(),
            producer: Some(producer),
            exhausted: false,
        };
        // Close the receiver before joining even when consume returns an error.
        // This wakes a producer blocked by backpressure. Joining explicitly also
        // prevents a producer panic from escaping scope on a consumer error.
        let result = consume(&mut stream);
        let producer = stream.producer.take();
        drop(stream);
        if let Some(producer) = producer {
            let produced = producer
                .join()
                .map_err(|_| Error::Invalid("chunk producer panicked".into()))?;
            if result.is_ok() {
                produced?;
            }
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{self, Cursor},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    fn collect(chunks: &mut impl Chunks) -> Result<(Vec<Vec<u8>>, Hash)> {
        let mut bytes = Vec::new();
        while let Some(chunk) = chunks.next_chunk()? {
            bytes.push(chunk);
        }
        Ok((bytes, chunks.finish()?))
    }
    #[test]
    fn empty_partial_and_multiple_batches_match_serial_bytes_and_digest() -> Result<()> {
        let mut seed = 42_u64;
        let data: Vec<_> = (0..33 * crate::chunk::MAX + 7)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect();
        struct Short<R>(R);
        impl<R: Read> Read for Short<R> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = buf.len().min(37);
                self.0.read(&mut buf[..n])
            }
        }
        for size in [
            0,
            1,
            crate::chunk::MAX - 1,
            crate::chunk::MAX + 1,
            data.len(),
        ] {
            let bytes = &data[..size];
            let expected = collect(&mut Serial::new(Cursor::new(bytes)))?;
            let actual = with_pipeline(Short(Cursor::new(bytes)), |chunks| collect(chunks))?;
            assert_eq!(actual, expected);
            assert_eq!(actual.0.concat(), bytes);
            assert_eq!(actual.1, <Hash>::from(Sha256::digest(bytes)));
        }
        Ok(())
    }
    #[test]
    fn producer_error_is_not_successful_eof() {
        struct Broken {
            remaining: usize,
        }
        impl Read for Broken {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "injected read failure",
                    ));
                }
                let n = buf.len().min(self.remaining);
                buf[..n].fill(42);
                self.remaining -= n;
                Ok(n)
            }
        }
        let result = with_pipeline(
            Broken {
                remaining: 40 * crate::chunk::MAX,
            },
            |chunks| collect(chunks),
        );
        assert!(matches!(result, Err(Error::Io(e)) if e.kind()==io::ErrorKind::BrokenPipe));
    }
    #[test]
    fn producer_panic_is_reported_without_unwinding_the_caller() {
        struct Panics;
        impl Read for Panics {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("injected producer panic");
            }
        }
        assert!(
            matches!(with_pipeline(Panics, |chunks| collect(chunks)), Err(Error::Invalid(message)) if message=="chunk producer panicked")
        );
    }
    #[test]
    fn cancellation_closes_a_backpressured_queue_and_joins_the_producer() {
        struct Counted {
            count: Arc<AtomicUsize>,
        }
        impl Read for Counted {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                buf.fill(42);
                self.count.fetch_add(buf.len(), Ordering::Relaxed);
                Ok(buf.len())
            }
        }
        let mut chunks = Chunker::new(io::repeat(42));
        let produced_bytes: usize = (0..(QUEUED_BATCHES + 1) * BATCH_CHUNKS)
            .map(|_| chunks.next_chunk().unwrap().unwrap().len())
            .sum();
        let (done, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let count = Arc::new(AtomicUsize::new(0));
            let result: Result<()> = with_pipeline(
                Counted {
                    count: Arc::clone(&count),
                },
                |_| {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while count.load(Ordering::Relaxed) < produced_bytes
                        && Instant::now() < deadline
                    {
                        thread::sleep(Duration::from_millis(1));
                    }
                    assert!(
                        count.load(Ordering::Relaxed) >= produced_bytes,
                        "producer made no progress"
                    );
                    Err(Error::SourceChanged("injected cancellation".into()))
                },
            );
            let bound = ((QUEUED_BATCHES + 1) * BATCH_CHUNKS + 1) * crate::chunk::MAX;
            done.send((result, count.load(Ordering::Relaxed), bound))
                .unwrap();
        });
        let (result, read, bound) = result
            .recv_timeout(Duration::from_secs(5))
            .expect("canceled pipeline did not join");
        worker.join().unwrap();
        assert!(
            matches!(result, Err(Error::SourceChanged(message)) if message=="injected cancellation")
        );
        assert!(
            read <= bound,
            "unbounded producer read {read}, expected at most {bound}"
        );
    }
    #[test]
    fn finishing_before_eof_cancels_instead_of_joining_a_blocked_producer() {
        let (done, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let result = with_pipeline(io::repeat(42), |chunks| chunks.finish());
            done.send(result).unwrap();
        });
        let result = result
            .recv_timeout(Duration::from_secs(5))
            .expect("premature finish deadlocked");
        worker.join().unwrap();
        assert!(matches!(result, Err(Error::Invalid(message))
            if message == "chunk stream must be drained before finishing"));
    }
    #[test]
    fn configuration_is_explicit_and_small_files_stay_sequential() {
        for value in ["", "2", "true", "-1", " 1"] {
            assert!(Mode::parse(value).is_err());
        }
        let disabled = Mode::parse("0").unwrap();
        assert!(!disabled.start_for(u64::MAX));
        assert_eq!(disabled.started(), 0);
        let enabled = Mode::parse("1").unwrap();
        assert!(!enabled.start_for(MIN_PIPELINE_SIZE - 1));
        assert!(enabled.start_for(MIN_PIPELINE_SIZE));
        assert_eq!(enabled.started(), 1);
    }
}
