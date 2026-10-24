//! Completed preparations may survive read-only reopens, bounded by retained
//! metadata bytes and entry count. Native retirement still requires no opens.
use crate::{Error, Result, live::SealCandidate, store::PreparedImport};
use fuser::INodeNo;
use std::collections::VecDeque;

const LIMIT: usize = 16 * 1024 * 1024;
const ENTRIES: usize = 4;

pub(super) struct Entry {
    pub version: u64,
    pub candidate: SealCandidate,
    pub import: PreparedImport,
    pub footprint: usize,
}
impl Entry {
    pub fn footprint(import: &PreparedImport) -> usize {
        std::mem::size_of::<Self>().saturating_add(import.heap_bytes())
    }
}
pub(super) struct Cache {
    pub enabled: bool,
    pub limit: usize,
    pub bytes: usize,
    entries: VecDeque<(INodeNo, Entry)>,
}
impl Cache {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            limit: LIMIT,
            bytes: 0,
            entries: VecDeque::new(),
        }
    }
    pub fn from_env() -> Result<Self> {
        match std::env::var("SL7_REUSE_PREPARED") {
            Err(std::env::VarError::NotPresent) => Ok(Self::new(false)),
            Ok(value) if value == "0" => Ok(Self::new(false)),
            Ok(value) if value == "1" => Ok(Self::new(true)),
            _ => Err(Error::Invalid("SL7_REUSE_PREPARED must be 0 or 1".into())),
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn contains(&self, ino: INodeNo) -> bool {
        self.entries.iter().any(|(id, _)| *id == ino)
    }
    pub fn admits(&self, footprint: usize) -> bool {
        self.enabled && footprint <= self.limit
    }
    pub fn take(&mut self, ino: INodeNo) -> Option<Entry> {
        let index = self.entries.iter().position(|(id, _)| *id == ino)?;
        let (_, entry) = self.entries.remove(index).unwrap();
        self.bytes -= entry.footprint;
        Some(entry)
    }
    /// Return evicted entries so their large vectors are freed after unlocking
    /// filesystem state. Even stale entries retain their inode until eviction.
    pub fn insert(&mut self, ino: INodeNo, entry: Entry) -> Vec<(INodeNo, Entry)> {
        assert!(self.admits(entry.footprint));
        let mut evicted = Vec::new();
        if let Some(old) = self.take(ino) {
            evicted.push((ino, old));
        }
        while self.entries.len() >= ENTRIES || self.bytes + entry.footprint > self.limit {
            let old = self.entries.pop_front().unwrap();
            self.bytes -= old.1.footprint;
            evicted.push(old);
        }
        self.bytes += entry.footprint;
        self.entries.push_back((ino, entry));
        evicted
    }
    pub fn drain(&mut self) -> VecDeque<(INodeNo, Entry)> {
        self.bytes = 0;
        std::mem::take(&mut self.entries)
    }
}
