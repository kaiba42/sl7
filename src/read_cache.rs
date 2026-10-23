//! Bounded caches for immutable, checksum-verified chunks and open pack inodes.
use crate::{Error, Result, chunk::Hash, pack};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    hash::Hash as Key,
    path::Path,
    sync::Arc,
};

struct Lru<K, V> {
    entries: HashMap<K, (V, u64)>,
    order: BTreeMap<u64, K>,
    clock: u64,
}
impl<K: Key + Eq + Clone, V> Lru<K, V> {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
        }
    }
    fn stamp(&mut self) -> u64 {
        // A wrapping timestamp must never alias a live entry's eviction key.
        if self.clock == u64::MAX {
            let keys: Vec<K> = self.order.values().cloned().collect();
            self.order.clear();
            self.clock = 0;
            for key in keys {
                self.clock += 1;
                self.entries.get_mut(&key).unwrap().1 = self.clock;
                self.order.insert(self.clock, key);
            }
        }
        self.clock += 1;
        self.clock
    }
    fn get(&mut self, key: &K) -> Option<&mut V> {
        let stamp = self.stamp();
        let (value, old) = self.entries.get_mut(key)?;
        self.order.remove(old);
        self.order.insert(stamp, key.clone());
        *old = stamp;
        Some(value)
    }
    fn insert(&mut self, key: K, value: V) {
        let stamp = self.stamp();
        if let Some((_, old)) = self.entries.insert(key.clone(), (value, stamp)) {
            self.order.remove(&old);
        }
        self.order.insert(stamp, key);
    }
    fn pop_oldest(&mut self) -> Option<V> {
        let (_, key) = self.order.pop_first()?;
        Some(self.entries.remove(&key).unwrap().0)
    }
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.clock = 0;
    }
}

#[derive(Clone, Default, Serialize)]
pub(crate) struct CacheStats {
    pub chunk_hits: u64,
    pub chunk_misses: u64,
    pub pack_opens: u64,
    pub decoded_bytes: u64,
    pub requested_bytes: u64,
    pub cached_bytes: usize,
    pub open_packs: usize,
}

pub(crate) struct ReadCache {
    chunks: Lru<Hash, Arc<Vec<u8>>>,
    packs: Lru<String, File>,
    byte_limit: usize,
    pack_limit: usize,
    stats: CacheStats,
}
impl Default for ReadCache {
    fn default() -> Self {
        Self::new(128 * 1024 * 1024, 64)
    }
}
impl ReadCache {
    fn new(byte_limit: usize, pack_limit: usize) -> Self {
        assert!(pack_limit > 0);
        Self {
            chunks: Lru::new(),
            packs: Lru::new(),
            byte_limit,
            pack_limit,
            stats: CacheStats::default(),
        }
    }
    pub fn stats(&self) -> CacheStats {
        let mut stats = self.stats.clone();
        stats.open_packs = self.packs.entries.len();
        stats
    }
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.packs.clear();
        self.stats.cached_bytes = 0;
    }
    pub fn request(&mut self, bytes: u64) {
        self.stats.requested_bytes += bytes;
    }
    pub fn read(&mut self, root: &Path, loc: &pack::Location) -> Result<Arc<Vec<u8>>> {
        if let Some(bytes) = self.chunks.get(&loc.hash) {
            if bytes.len() as u64 != loc.raw_len {
                return Err(Error::Corrupt(
                    "cached chunk length differs from catalog".into(),
                ));
            }
            self.stats.chunk_hits += 1;
            return Ok(Arc::clone(bytes));
        }
        self.stats.chunk_misses += 1;
        if !self.packs.entries.contains_key(&loc.pack) {
            let file = pack::open(root, &loc.pack)?;
            if self.packs.entries.len() == self.pack_limit {
                self.packs.pop_oldest();
            }
            self.packs.insert(loc.pack.clone(), file);
            self.stats.pack_opens += 1;
        }
        let bytes = Arc::new(pack::read_from(self.packs.get(&loc.pack).unwrap(), loc)?);
        self.stats.decoded_bytes += bytes.len() as u64;
        // Insert only after header, decoded length, and content hash verification.
        if bytes.len() <= self.byte_limit {
            while self.stats.cached_bytes + bytes.len() > self.byte_limit {
                self.stats.cached_bytes -= self.chunks.pop_oldest().unwrap().len();
            }
            self.stats.cached_bytes += bytes.len();
            self.chunks.insert(loc.hash, Arc::clone(&bytes));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_hits_evict_by_bytes_and_reuse_pack_handles() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let _store = crate::Store::init(&root)?;
        let a = vec![7; 32768];
        let b = vec![9; 32768];
        let mut writer = pack::Writer::new(&root)?;
        writer.add(crate::chunk::identity(&a), &a)?;
        writer.add(crate::chunk::identity(&b), &b)?;
        let (_, _, locations) = writer.finish(&root)?;
        let mut cache = ReadCache::new(32768, 1);
        assert_eq!(*cache.read(&root, &locations[0])?, a);
        assert_eq!(*cache.read(&root, &locations[0])?, a);
        assert_eq!(*cache.read(&root, &locations[1])?, b);
        assert_eq!(*cache.read(&root, &locations[0])?, a);
        assert_eq!(cache.stats().chunk_hits, 1);
        assert_eq!(cache.stats().chunk_misses, 3);
        assert_eq!(cache.stats().pack_opens, 1);
        assert_eq!(cache.stats().cached_bytes, 32768);
        let c = vec![11; 16384];
        let mut writer = pack::Writer::new(&root)?;
        writer.add(crate::chunk::identity(&c), &c)?;
        let (_, _, more) = writer.finish(&root)?;
        assert_eq!(*cache.read(&root, &more[0])?, c);
        assert_eq!(cache.stats().open_packs, 1);
        assert_eq!(*cache.read(&root, &locations[0])?, a);
        assert_eq!(cache.stats().pack_opens, 3);
        cache.clear();
        assert_eq!(cache.stats().open_packs, 0);
        assert_eq!(cache.stats().cached_bytes, 0);
        Ok(())
    }
}
