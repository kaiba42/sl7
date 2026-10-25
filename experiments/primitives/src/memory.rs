use serde_json::json;
use std::{
    collections::{VecDeque, hash_map::RandomState},
    hash::BuildHasher,
    hint::black_box,
    sync::{Arc, Barrier, Mutex, RwLock},
    thread,
    time::Instant,
};
include!(concat!(env!("OUT_DIR"), "/lru.rs"));
const KEYS: u64 = 16384;
const BYTES: usize = 8192;
type Payload = Arc<Vec<u8>>;
type Digest = [u8; 32];
fn digest(k: u64) -> Digest {
    let mut d = [0; 32];
    for i in 0..4 {
        d[i * 8..i * 8 + 8].copy_from_slice(
            &k.wrapping_mul(0x9e3779b97f4a7c15u64.wrapping_add(i as u64 * 2))
                .to_le_bytes(),
        );
    }
    d
}
trait Index: Send + Sync {
    fn get(&self, k: u64) -> Payload;
    fn put(&self, k: u64, v: Payload);
}
impl Index for Mutex<HashMap<u64, Payload>> {
    fn get(&self, k: u64) -> Payload {
        self.lock().unwrap()[&k].clone()
    }
    fn put(&self, k: u64, v: Payload) {
        self.lock().unwrap().insert(k, v);
    }
}
impl Index for RwLock<HashMap<u64, Payload>> {
    fn get(&self, k: u64) -> Payload {
        self.read().unwrap()[&k].clone()
    }
    fn put(&self, k: u64, v: Payload) {
        self.write().unwrap().insert(k, v);
    }
}
impl Index for dashmap::DashMap<u64, Payload> {
    fn get(&self, k: u64) -> Payload {
        self.get(&k).unwrap().value().clone()
    }
    fn put(&self, k: u64, v: Payload) {
        self.insert(k, v);
    }
}
impl Index for scc::HashMap<u64, Payload, RandomState> {
    fn get(&self, k: u64) -> Payload {
        self.read_sync(&k, |_, v| v.clone()).unwrap()
    }
    fn put(&self, k: u64, v: Payload) {
        self.upsert_sync(k, v);
    }
}
trait Cache: Send + Sync {
    fn get(&self, k: &Digest) -> Option<Payload>;
    fn put(&self, k: Digest, v: Payload);
    fn settle(&self) -> usize;
}
struct Locked {
    lru: Mutex<Lru<Digest, Payload>>,
    cap: usize,
}
impl Cache for Locked {
    fn get(&self, k: &Digest) -> Option<Payload> {
        self.lru.lock().unwrap().get(k).cloned()
    }
    fn put(&self, k: Digest, v: Payload) {
        let mut l = self.lru.lock().unwrap();
        if l.entries.contains_key(&k) {
            return;
        }
        if l.entries.len() == self.cap {
            l.pop_oldest();
        }
        l.insert(k, v);
    }
    fn settle(&self) -> usize {
        self.lru.lock().unwrap().entries.len() * BYTES
    }
}
struct Sharded {
    shards: Vec<Locked>,
    hasher: RandomState,
}
impl Cache for Sharded {
    fn get(&self, k: &Digest) -> Option<Payload> {
        self.shards[self.hasher.hash_one(k) as usize % self.shards.len()].get(k)
    }
    fn put(&self, k: Digest, v: Payload) {
        self.shards[self.hasher.hash_one(k) as usize % self.shards.len()].put(k, v)
    }
    fn settle(&self) -> usize {
        self.shards.iter().map(Cache::settle).sum()
    }
}
// Hits do not maintain global recency. This deliberately tests FIFO, not LRU.
struct DashFifo {
    map: dashmap::DashMap<Digest, Payload>,
    queues: Vec<Mutex<VecDeque<Digest>>>,
    hasher: RandomState,
}
impl Cache for DashFifo {
    fn get(&self, k: &Digest) -> Option<Payload> {
        self.map.get(k).map(|r| r.value().clone())
    }
    fn put(&self, k: Digest, v: Payload) {
        let mut q = self.queues[self.hasher.hash_one(k) as usize % 16]
            .lock()
            .unwrap();
        if self.map.contains_key(&k) {
            return;
        }
        if q.len() == KEYS as usize / 16 {
            self.map.remove(&q.pop_front().unwrap());
        }
        self.map.insert(k, v);
        q.push_back(k);
    }
    fn settle(&self) -> usize {
        self.map.len() * BYTES
    }
}
impl Cache for moka::sync::Cache<Digest, Payload> {
    fn get(&self, k: &Digest) -> Option<Payload> {
        self.get(k)
    }
    fn put(&self, k: Digest, v: Payload) {
        self.insert(k, v);
    }
    fn settle(&self) -> usize {
        self.run_pending_tasks();
        self.weighted_size() as usize
    }
}
fn parallel<F: Fn(usize) -> (u64, u64) + Sync>(threads: usize, f: F) -> (f64, u64, u64) {
    let barrier = Barrier::new(threads + 1);
    thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|t| {
                let f = &f;
                let b = &barrier;
                s.spawn(move || {
                    b.wait();
                    b.wait();
                    f(t)
                })
            })
            .collect();
        barrier.wait();
        let start = Instant::now();
        barrier.wait();
        let mut hits = 0;
        let mut check = 0;
        for w in workers {
            let (h, c) = w.join().unwrap();
            hits += h;
            check += c;
        }
        (start.elapsed().as_secs_f64(), hits, check)
    })
}
fn schedule(t: usize, n: usize, range: u64, hot: bool) -> Vec<u64> {
    let mut x = (t as u64 + 1) * 1234567;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            if hot && i % 10 != 0 {
                x % 64
            } else {
                x % range
            }
        })
        .collect()
}
fn index<I: Index>(map: I, name: &str, threads: usize, round: usize, mix: &str) {
    for k in 0..KEYS {
        map.put(k, Arc::new(vec![(k % 251) as u8]));
    }
    let n = 1_000_000;
    let schedules: Vec<_> = (0..threads)
        .map(|t| schedule(t, n, KEYS, mix == "hot"))
        .collect();
    let (seconds, hits, checksum) = parallel(threads, |t| {
        let mut sum = 0;
        for (i, &k) in schedules[t].iter().enumerate() {
            let v = map.get(k);
            sum += v[0] as u64;
            if mix == "mixed" && i % 10 == 0 {
                map.put(k, v);
            }
        }
        (n as u64, sum)
    });
    println!(
        "{}",
        json!({"group":"index","candidate":name,"threads":threads,"round":round,"workload":mix,"seconds":seconds,"operations":hits,"checksum":checksum})
    );
}
fn cache<C: Cache>(cache: C, name: &str, threads: usize, round: usize, mix: &str) {
    let payloads: Vec<_> = (0..KEYS * 2)
        .map(|k| Arc::new(vec![(k % 251) as u8; BYTES]))
        .collect();
    for k in 0..KEYS {
        cache.put(digest(k), payloads[k as usize].clone());
    }
    cache.settle();
    let n = if mix == "churn" { 100_000 } else { 500_000 };
    let schedules: Vec<Vec<(u64, Digest)>> = (0..threads)
        .map(|t| {
            schedule(
                t,
                n,
                if mix == "churn" { KEYS * 2 } else { KEYS / 2 },
                mix == "hot",
            )
            .into_iter()
            .map(|k| (k, digest(k)))
            .collect()
        })
        .collect();
    let (seconds, hits, checksum) = parallel(threads, |t| {
        let mut hits = 0;
        let mut sum = 0;
        for (index, k) in &schedules[t] {
            let v = match cache.get(k) {
                Some(v) => {
                    hits += 1;
                    v
                }
                None => {
                    cache.put(*k, payloads[*index as usize].clone());
                    payloads[*index as usize].clone()
                }
            };
            assert_eq!(v[0], (*index % 251) as u8);
            sum += black_box(v[0]) as u64;
        }
        (hits, sum)
    });
    let bytes = cache.settle();
    assert!(bytes <= KEYS as usize * BYTES);
    assert_eq!(
        checksum,
        schedules
            .iter()
            .flatten()
            .map(|(k, _)| k % 251)
            .sum::<u64>()
    );
    println!(
        "{}",
        json!({"group":"cache","candidate":name,"threads":threads,"round":round,"workload":mix,"seconds":seconds,"operations":threads*n,"hits":hits,"checksum":checksum,"retained_bytes":bytes})
    );
}
pub fn run() {
    let mut baseline = Lru::new();
    baseline.insert(1_u64, 7_u64);
    baseline.clear();
    assert!(baseline.get(&1).is_none());
    for round in 0..3 {
        for threads in [1, 2, 4, 8] {
            for mix in ["uniform", "hot", "mixed"] {
                let mut names = vec!["mutex", "rwlock", "dashmap", "scc"];
                names.rotate_left(round % 4);
                for name in names {
                    match name {
                        "mutex" => index(Mutex::new(HashMap::new()), name, threads, round, mix),
                        "rwlock" => index(RwLock::new(HashMap::new()), name, threads, round, mix),
                        "dashmap" => index(
                            dashmap::DashMap::with_capacity_and_hasher(
                                KEYS as usize,
                                RandomState::new(),
                            ),
                            name,
                            threads,
                            round,
                            mix,
                        ),
                        _ => index(
                            scc::HashMap::with_capacity_and_hasher(
                                KEYS as usize,
                                RandomState::new(),
                            ),
                            name,
                            threads,
                            round,
                            mix,
                        ),
                    }
                }
            }
            for mix in ["uniform", "hot", "churn"] {
                let mut names = vec!["lru", "sharded-lru", "dashmap-fifo", "moka"];
                names.rotate_left(round % 4);
                for name in names {
                    match name {
                        "lru" => cache(
                            Locked {
                                lru: Mutex::new(Lru::new()),
                                cap: KEYS as usize,
                            },
                            name,
                            threads,
                            round,
                            mix,
                        ),
                        "sharded-lru" => cache(
                            Sharded {
                                shards: (0..16)
                                    .map(|_| Locked {
                                        lru: Mutex::new(Lru::new()),
                                        cap: KEYS as usize / 16,
                                    })
                                    .collect(),
                                hasher: RandomState::new(),
                            },
                            name,
                            threads,
                            round,
                            mix,
                        ),
                        "dashmap-fifo" => cache(
                            DashFifo {
                                map: dashmap::DashMap::new(),
                                queues: (0..16).map(|_| Mutex::new(VecDeque::new())).collect(),
                                hasher: RandomState::new(),
                            },
                            name,
                            threads,
                            round,
                            mix,
                        ),
                        _ => cache(
                            moka::sync::Cache::builder()
                                .max_capacity(KEYS * BYTES as u64)
                                .weigher(|_: &Digest, v: &Payload| v.len() as u32)
                                .build(),
                            name,
                            threads,
                            round,
                            mix,
                        ),
                    }
                }
            }
        }
    }
}
