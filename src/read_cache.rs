//! Bounded caches for immutable, checksum-verified chunks and open pack inodes.
use crate::{Error, Result, chunk::Hash, pack};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    hash::Hash as Key,
    path::Path,
    sync::{Arc, Mutex},
    time::Instant,
};

// Fixed logical windows bound cold-read work independently of artifact size.
pub(crate) const MAP_WINDOW: u64 = 256 * 1024;
pub(crate) const MAX_MAP_ROWS: usize = 1024;

#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) struct MapKey {
    pub file: i64,
    pub size: u64,
    // SQLite file row IDs can be reused after deletion. Content identity keeps
    // an old window from being mistaken for a replacement file's mapping.
    pub digest: String,
    pub start: u64,
}

pub(crate) struct Extent {
    pub start: u64,
    pub location: pack::Location,
}

struct MapWindow {
    extents: Arc<[Extent]>,
    charge: usize,
}

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
    pub map_hits: u64,
    pub map_misses: u64,
    pub map_queries: u64,
    pub map_fallbacks: u64,
    pub map_bytes: usize,
    pub map_windows: usize,
    pub active_loads: usize,
    pub max_parallel_loads: usize,
    pub duplicate_loads: u64,
    pub load_seconds: f64,
}

struct CacheState {
    chunks: Lru<Hash, Arc<Vec<u8>>>,
    packs: Lru<String, Arc<File>>,
    byte_limit: usize,
    pack_limit: usize,
    stats: CacheStats,
    maps: Lru<MapKey, MapWindow>,
    map_limit: usize,
    map_entry_limit: usize,
}
impl Default for CacheState {
    fn default() -> Self {
        Self::new(128 * 1024 * 1024, 64)
    }
}
impl CacheState {
    fn new(byte_limit: usize, pack_limit: usize) -> Self {
        assert!(pack_limit > 0);
        Self {
            chunks: Lru::new(),
            packs: Lru::new(),
            byte_limit,
            pack_limit,
            stats: CacheStats::default(),
            maps: Lru::new(),
            map_limit: 32 * 1024 * 1024,
            map_entry_limit: 4096,
        }
    }
    pub fn stats(&self) -> CacheStats {
        let mut stats = self.stats.clone();
        stats.open_packs = self.packs.entries.len();
        stats.map_windows = self.maps.entries.len();
        stats
    }
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.packs.clear();
        self.stats.cached_bytes = 0;
        self.maps.clear();
        self.stats.map_bytes = 0;
    }
    pub fn map(&mut self, key: &MapKey) -> Option<Arc<[Extent]>> {
        if let Some(window) = self.maps.get(key) {
            self.stats.map_hits += 1;
            Some(Arc::clone(&window.extents))
        } else {
            self.stats.map_misses += 1;
            None
        }
    }
    pub fn map_query(&mut self) {
        self.stats.map_queries += 1;
    }
    pub fn map_fallback(&mut self) {
        self.stats.map_fallbacks += 1;
    }
    pub fn remember_map(&mut self, key: MapKey, extents: Arc<[Extent]>) {
        // Concurrent readers can load the same window. Charge it only once.
        if self.maps.entries.contains_key(&key) {
            return;
        }
        // Account for the extent allocation, pack strings, both LRU key copies,
        // and conservative per-entry bookkeeping. Entry count also bounds maps
        // with little payload; this is an accounting budget, not an RSS promise.
        let charge = std::mem::size_of_val(extents.as_ref())
            + extents
                .iter()
                .map(|e| e.location.pack.capacity())
                .sum::<usize>()
            + 2 * key.digest.capacity()
            + 256;
        if charge > self.map_limit || self.map_entry_limit == 0 {
            return;
        }
        while self.stats.map_bytes + charge > self.map_limit
            || self.maps.entries.len() >= self.map_entry_limit
        {
            self.stats.map_bytes -= self.maps.pop_oldest().unwrap().charge;
        }
        self.stats.map_bytes += charge;
        self.maps.insert(key, MapWindow { extents, charge });
    }
    pub fn request(&mut self, bytes: u64) {
        self.stats.requested_bytes += bytes;
    }
}

/// One mount-wide budget. Only cache bookkeeping holds this mutex; pack I/O,
/// decompression, verification, and copying run with owned Arc references.
pub(crate) struct ReadCache {
    state: Mutex<CacheState>,
}
impl Default for ReadCache {
    fn default() -> Self {
        Self::new(128 * 1024 * 1024, 64)
    }
}
impl ReadCache {
    fn new(byte_limit: usize, pack_limit: usize) -> Self {
        Self {
            state: Mutex::new(CacheState::new(byte_limit, pack_limit)),
        }
    }
    pub fn stats(&self) -> CacheStats {
        self.state.lock().unwrap().stats()
    }
    pub fn clear(&self) {
        self.state.lock().unwrap().clear();
    }
    pub fn map(&self, key: &MapKey) -> Option<Arc<[Extent]>> {
        self.state.lock().unwrap().map(key)
    }
    pub fn map_query(&self) {
        self.state.lock().unwrap().map_query();
    }
    pub fn map_fallback(&self) {
        self.state.lock().unwrap().map_fallback();
    }
    pub fn remember_map(&self, key: MapKey, extents: Arc<[Extent]>) {
        self.state.lock().unwrap().remember_map(key, extents);
    }
    pub fn request(&self, bytes: u64) {
        self.state.lock().unwrap().request(bytes);
    }
    pub fn read(&self, root: &Path, loc: &pack::Location) -> Result<Arc<Vec<u8>>> {
        self.read_with(root, loc, pack::read_from)
    }
    fn read_with(
        &self,
        root: &Path,
        loc: &pack::Location,
        load: impl FnOnce(&File, &pack::Location) -> Result<Vec<u8>>,
    ) -> Result<Arc<Vec<u8>>> {
        let cached_pack = {
            let mut state = self.state.lock().unwrap();
            if let Some(bytes) = state.chunks.get(&loc.hash).cloned() {
                if bytes.len() as u64 != loc.raw_len {
                    return Err(Error::Corrupt(
                        "cached chunk length differs from catalog".into(),
                    ));
                }
                state.stats.chunk_hits += 1;
                return Ok(bytes);
            }
            state.stats.chunk_misses += 1;
            state.packs.get(&loc.pack).cloned()
        };
        let file = match cached_pack {
            Some(file) => file,
            None => {
                let file = Arc::new(pack::open(root, &loc.pack)?);
                let mut state = self.state.lock().unwrap();
                state.stats.pack_opens += 1;
                if let Some(existing) = state.packs.get(&loc.pack) {
                    Arc::clone(existing)
                } else {
                    if state.packs.entries.len() == state.pack_limit {
                        state.packs.pop_oldest();
                    }
                    state.packs.insert(loc.pack.clone(), Arc::clone(&file));
                    file
                }
            }
        };
        let _loading = Loading::start(self);
        let bytes = Arc::new(load(&file, loc)?);
        // Only verified payloads reach insertion. Racing misses may decode
        // twice, but must neither double-charge nor overwrite an existing entry.
        let mut state = self.state.lock().unwrap();
        state.stats.decoded_bytes += bytes.len() as u64;
        if state.chunks.entries.contains_key(&loc.hash) {
            state.stats.duplicate_loads += 1;
        } else if bytes.len() <= state.byte_limit {
            while state.stats.cached_bytes + bytes.len() > state.byte_limit {
                state.stats.cached_bytes -= state.chunks.pop_oldest().unwrap().len();
            }
            state.stats.cached_bytes += bytes.len();
            state.chunks.insert(loc.hash, Arc::clone(&bytes));
        }
        Ok(bytes)
    }
}

struct Loading<'a> {
    cache: &'a ReadCache,
    started: Instant,
}
impl<'a> Loading<'a> {
    fn start(cache: &'a ReadCache) -> Self {
        let mut state = cache.state.lock().unwrap();
        state.stats.active_loads += 1;
        state.stats.max_parallel_loads =
            state.stats.max_parallel_loads.max(state.stats.active_loads);
        Self {
            cache,
            started: Instant::now(),
        }
    }
}
impl Drop for Loading<'_> {
    fn drop(&mut self) {
        let mut state = self.cache.state.lock().unwrap();
        state.stats.active_loads -= 1;
        state.stats.load_seconds += self.started.elapsed().as_secs_f64();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cold_loads_overlap_and_racing_duplicates_stay_within_budget() -> Result<()> {
        use std::{sync::mpsc, thread, time::Duration};
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let _store = crate::Store::init(&root)?;
        let a = vec![7; 32768];
        let b = vec![9; 32768];
        let mut writer = pack::Writer::new(&root)?;
        writer.add(crate::chunk::identity(&a), &a)?;
        writer.add(crate::chunk::identity(&b), &b)?;
        let (_, _, locations) = writer.finish(&root)?;
        for duplicate in [false, true] {
            let cache = ReadCache::new(32768, 1);
            thread::scope(|scope| {
                let (ready, entered) = mpsc::channel();
                let mut resumes = Vec::new();
                let mut workers = Vec::new();
                for index in [0, if duplicate { 0 } else { 1 }] {
                    let (resume, paused) = mpsc::channel();
                    resumes.push(resume);
                    let ready = ready.clone();
                    let (cache, root, loc) = (&cache, &root, &locations[index]);
                    workers.push(scope.spawn(move || {
                        cache.read_with(root, loc, |file, loc| {
                            ready.send(()).unwrap();
                            paused.recv_timeout(Duration::from_secs(10)).unwrap();
                            pack::read_from(file, loc)
                        })
                    }));
                }
                let first = entered.recv_timeout(Duration::from_secs(5));
                let second = entered.recv_timeout(Duration::from_secs(5));
                // Unblock before any assertion so a broken lock never hangs the test.
                for resume in resumes {
                    let _ = resume.send(());
                }
                first.unwrap();
                second.expect("cold reads serialized behind the cache mutex");
                assert_eq!(*workers.remove(0).join().unwrap().unwrap(), a);
                assert_eq!(
                    *workers.remove(0).join().unwrap().unwrap(),
                    if duplicate { a.clone() } else { b.clone() }
                );
            });
            let stats = cache.stats();
            assert_eq!(stats.max_parallel_loads, 2);
            assert_eq!(stats.active_loads, 0);
            assert_eq!(stats.cached_bytes, 32768);
            assert_eq!(stats.duplicate_loads, u64::from(duplicate));
            assert_eq!(stats.open_packs, 1);
            cache.clear();
            assert!(
                cache
                    .read_with(&root, &locations[0], |_, _| Err(Error::Corrupt(
                        "injected".into()
                    )))
                    .is_err()
            );
            assert_eq!(cache.stats().active_loads, 0);
            assert_eq!(cache.stats().cached_bytes, 0);
            assert_eq!(*cache.read(&root, &locations[0])?, a);
        }
        Ok(())
    }

    #[test]
    fn map_cache_evicts_by_bytes_and_count_and_clears() {
        let key = |file| MapKey {
            file,
            size: 1,
            digest: "0".repeat(64),
            start: 0,
        };
        let extent = || -> Arc<[Extent]> {
            vec![Extent {
                start: 0,
                location: pack::Location {
                    hash: [0; 32],
                    raw_len: 1,
                    pack: "a".repeat(64),
                    offset: 8,
                    stored_len: 1,
                    codec: 0,
                },
            }]
            .into()
        };
        let mut cache = CacheState::new(1024, 1);
        cache.remember_map(key(1), extent());
        let one = cache.stats().map_bytes;
        cache.remember_map(key(1), extent());
        assert_eq!(cache.stats().map_bytes, one);
        cache.map_limit = one;
        cache.remember_map(key(2), extent());
        assert!(cache.map(&key(1)).is_none());
        assert!(cache.map(&key(2)).is_some());
        assert_eq!(cache.stats().map_bytes, one);
        cache.map_limit = one * 10;
        cache.map_entry_limit = 1;
        cache.remember_map(key(3), extent());
        assert!(cache.map(&key(2)).is_none());
        assert_eq!(cache.stats().map_windows, 1);
        cache.clear();
        assert_eq!(cache.stats().map_bytes, 0);
        assert_eq!(cache.stats().map_windows, 0);
    }

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
        let cache = ReadCache::new(32768, 1);
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
