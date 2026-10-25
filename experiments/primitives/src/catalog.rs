use rusqlite::{params, types::Value};
use serde_json::json;
use std::{
    path::Path,
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const N: i64 = 16384;
const EXISTS: &str = "SELECT EXISTS(SELECT 1 FROM chunks WHERE hash=?1)";
const RANGE: &str = "SELECT e.offset,c.hash,c.raw_len,c.pack,c.offset,c.stored_len,c.codec FROM extents e JOIN chunks c ON c.hash=e.hash WHERE e.file_id=?1 AND e.offset>=?2 AND e.offset<?3 ORDER BY e.offset";
fn hash(k: i64) -> Vec<u8> {
    let mut h = vec![0; 32];
    for i in 0..4 {
        h[i * 8..i * 8 + 8].copy_from_slice(
            &(k as u64)
                .wrapping_mul(0x9e3779b97f4a7c15u64.wrapping_add(i as u64 * 2))
                .to_le_bytes(),
        );
    }
    h
}
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
}
enum Conn {
    Sql(rusqlite::Connection),
    Tur(turso::Connection),
}
enum Stmt<'a> {
    Sql(rusqlite::Statement<'a>),
    Tur(turso::Statement),
}
fn tv(v: &[Value]) -> Vec<turso::Value> {
    v.iter()
        .map(|v| match v {
            Value::Integer(x) => turso::Value::Integer(*x),
            Value::Text(x) => turso::Value::Text(x.clone()),
            Value::Blob(x) => turso::Value::Blob(x.clone()),
            _ => panic!("unsupported benchmark value"),
        })
        .collect()
}
impl Conn {
    async fn prepare(&self, sql: &str) -> Result<Stmt<'_>> {
        Ok(match self {
            Self::Sql(c) => Stmt::Sql(c.prepare(sql)?),
            Self::Tur(c) => Stmt::Tur(c.prepare(sql).await?),
        })
    }
    async fn exec(&self, sql: &str) -> Result<()> {
        match self {
            Self::Sql(c) => c.execute_batch(sql)?,
            Self::Tur(c) => c.execute_batch(sql).await?,
        }
        Ok(())
    }
    async fn scalar(&self, sql: &str) -> Result<i64> {
        Ok(self.prepare(sql).await?.query(&[]).await?.1)
    }
}
impl Stmt<'_> {
    async fn exec(&mut self, p: &[Value]) -> Result<()> {
        match self {
            Self::Sql(s) => {
                s.execute(rusqlite::params_from_iter(p))?;
            }
            Self::Tur(s) => {
                s.execute(tv(p)).await?;
            }
        }
        Ok(())
    }
    async fn query(&mut self, p: &[Value]) -> Result<(usize, i64)> {
        let mut count = 0;
        let mut sum = 0;
        match self {
            Self::Sql(s) => {
                let mut r = s.query(rusqlite::params_from_iter(p))?;
                while let Some(r) = r.next()? {
                    count += 1;
                    sum += r.get::<_, i64>(0)?;
                }
            }
            Self::Tur(s) => {
                let mut r = s.query(tv(p)).await?;
                while let Some(r) = r.next().await? {
                    count += 1;
                    sum += r.get::<i64>(0)?;
                }
            }
        }
        Ok((count, sum))
    }
}
fn seed(path: &Path) -> Result<()> {
    let c = rusqlite::Connection::open(path)?;
    c.execute_batch(include_str!(concat!(env!("OUT_DIR"), "/schema.sql")))?;
    c.execute_batch("PRAGMA foreign_keys=ON; BEGIN; INSERT INTO packs VALUES('seed',134217736)")?;
    {
        let mut chunks = c.prepare("INSERT INTO chunks VALUES(?1,8192,'seed',?2,8192,0)")?;
        let mut extents = c.prepare("INSERT INTO extents VALUES(1,?1,?2)")?;
        c.execute(
            "INSERT INTO files VALUES(1,'seed','seed',?1,?2)",
            params![N * 8192, hash(0)],
        )?;
        for k in 0..N {
            let h = hash(k);
            chunks.execute(params![h, 8 + k * 8192])?;
            extents.execute(params![k * 8192, h])?;
        }
    }
    c.execute_batch("COMMIT")?;
    Ok(())
}
async fn open(path: &Path, db: Option<&turso::Database>) -> Result<Conn> {
    let c = if let Some(db) = db {
        Conn::Tur(db.connect()?)
    } else {
        let c = rusqlite::Connection::open(path)?;
        c.busy_timeout(Duration::from_secs(10))?;
        Conn::Sql(c)
    };
    c.exec("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON")
        .await?;
    assert_eq!(c.scalar("PRAGMA synchronous").await?, 2);
    assert_eq!(c.scalar("PRAGMA foreign_keys").await?, 1);
    Ok(c)
}
fn retryable(e: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    if let Some(e) = e.downcast_ref::<turso::Error>() {
        matches!(e, turso::Error::Busy(_) | turso::Error::BusySnapshot(_))
            || matches!(e, turso::Error::Error(message) if message == "Write-write conflict")
    } else if let Some(rusqlite::Error::SqliteFailure(e, _)) = e.downcast_ref::<rusqlite::Error>() {
        matches!(
            e.code,
            rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
        )
    } else {
        false
    }
}
async fn writer(
    c: &Conn,
    mvcc: bool,
    tid: usize,
    shared: bool,
    b: &Barrier,
) -> Result<(usize, Vec<f64>)> {
    let mut pack = c
        .prepare("INSERT OR IGNORE INTO packs VALUES(?1,524296)")
        .await?;
    let mut chunk = c
        .prepare("INSERT INTO chunks VALUES(?1,8192,?2,?3,8192,0) ON CONFLICT(hash) DO NOTHING")
        .await?;
    let mut file = c
        .prepare(
            "INSERT INTO files(view,name,size,digest) VALUES('bench',?1,524288,?2) RETURNING id",
        )
        .await?;
    let mut extent = c.prepare("INSERT INTO extents VALUES(?1,?2,?3)").await?;
    let mut retries = 0;
    let mut latencies = Vec::new();
    b.wait();
    b.wait();
    for tx in 0..30 {
        let name = format!("{tid}-{tx}");
        let start = Instant::now();
        let mut attempt = 0;
        loop {
            let result: Result<()> = async {
                c.exec(if mvcc {
                    "BEGIN CONCURRENT"
                } else {
                    "BEGIN IMMEDIATE"
                })
                .await?;
                pack.exec(&[Value::Text(name.clone())]).await?;
                let (_, id) = file
                    .query(&[Value::Text(name.clone()), Value::Blob(hash(tx))])
                    .await?;
                for k in 0..64 {
                    let key = N + if shared {
                        tx * 64 + k
                    } else {
                        (tid as i64 * 30 + tx) * 64 + k
                    };
                    let h = hash(key);
                    chunk
                        .exec(&[
                            Value::Blob(h.clone()),
                            Value::Text(name.clone()),
                            Value::Integer(8 + k * 8192),
                        ])
                        .await?;
                    extent
                        .exec(&[Value::Integer(id), Value::Integer(k * 8192), Value::Blob(h)])
                        .await?;
                }
                c.exec("COMMIT").await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => break,
                Err(e) => {
                    let _ = c.exec("ROLLBACK").await;
                    if !retryable(e.as_ref()) || attempt >= 1000 {
                        return Err(e);
                    }
                    retries += 1;
                    attempt += 1;
                    tokio::time::sleep(Duration::from_micros((attempt * 50).min(2000))).await;
                }
            }
        }
        latencies.push(start.elapsed().as_secs_f64());
    }
    Ok((retries, latencies))
}
fn one(seed_path: &Path, engine: &str, threads: usize, round: usize, workload: &str) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("catalog.db");
    std::fs::copy(seed_path, &path)?;
    let runtime = rt();
    let mvcc = engine == "turso-mvcc";
    let db = if engine.starts_with("turso") {
        Some(
            runtime.block_on(
                turso::Builder::new_local(path.to_str().unwrap())
                    .experimental_mvcc_passive_checkpoint(mvcc)
                    .build(),
            )?,
        )
    } else {
        None
    };
    let c = runtime.block_on(open(&path, db.as_ref()))?;
    runtime.block_on(async {
        let sql = if mvcc {
            "PRAGMA journal_mode=mvcc"
        } else {
            "PRAGMA journal_mode=WAL"
        };
        match &c {
            Conn::Sql(db) => {
                let mode: String = db.query_row(sql, [], |r| r.get(0))?;
                assert_eq!(mode, "wal");
            }
            Conn::Tur(db) => {
                let mut rows = db.query(sql, ()).await?;
                let row = rows.next().await?.expect("journal mode row");
                let mode: String = row.get(0)?;
                assert_eq!(mode, if mvcc { "mvcc" } else { "wal" });
                while rows.next().await?.is_some() {}
            }
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    // Verify CHECK, FK and rollback semantics before recording any timing.
    runtime.block_on(async {
        assert!(
            c.exec("INSERT INTO packs VALUES('invalid',0)")
                .await
                .is_err()
        );
        assert!(
            c.exec("INSERT INTO extents VALUES(999999,0,zeroblob(32))")
                .await
                .is_err()
        );
        c.exec("BEGIN; INSERT INTO packs VALUES('rollback',8); ROLLBACK")
            .await?;
        assert_eq!(
            c.scalar("SELECT count(*) FROM packs WHERE id='rollback'")
                .await?,
            0
        );
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    let barrier = Arc::new(Barrier::new(threads + 1));
    let (seconds, retries, mut latency, checksum) = thread::scope(|s| -> Result<_> {
        let mut workers = Vec::new();
        for tid in 0..threads {
            let b = barrier.clone();
            let db = db.as_ref();
            let path = &path;
            workers.push(s.spawn(move || -> Result<_> {
                rt().block_on(async {
                    let c = open(path, db).await?;
                    if workload.starts_with("publish") {
                        let (r, l) =
                            writer(&c, mvcc, tid, workload.ends_with("shared"), &b).await?;
                        return Ok((r, l, 0));
                    }
                    let sql = if workload == "range" { RANGE } else { EXISTS };
                    let mut stmt = c.prepare(sql).await?;
                    let mut checksum = 0;
                    // Generate all keys outside the timed section, including 50% absent keys.
                    let inputs: Vec<Vec<Value>> = (0..10_000)
                        .map(|i| {
                            let k = if workload == "repeated" {
                                i % 64
                            } else {
                                (i * 7919 + tid as i64 * 17) % (N * 2)
                            };
                            if workload == "range" {
                                let start = (k % (N - 32)) * 8192;
                                vec![
                                    Value::Integer(1),
                                    Value::Integer(start),
                                    Value::Integer(start + 32 * 8192),
                                ]
                            } else {
                                vec![Value::Blob(hash(k))]
                            }
                        })
                        .collect();
                    b.wait();
                    b.wait();
                    for p in inputs {
                        let (rows, sum) = stmt.query(&p).await?;
                        if workload == "range" {
                            assert_eq!(rows, 32);
                        }
                        checksum += sum;
                    }
                    Ok((0, Vec::new(), checksum))
                })
            }));
        }
        barrier.wait();
        let start = Instant::now();
        barrier.wait();
        let (mut retries, mut latency, mut checksum) = (0, Vec::new(), 0);
        for w in workers {
            let (r, mut l, c) = w.join().expect("catalog worker panicked")?;
            retries += r;
            latency.append(&mut l);
            checksum += c;
        }
        Ok((start.elapsed().as_secs_f64(), retries, latency, checksum))
    })?;
    let counts = runtime.block_on(async {
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
            c.scalar("SELECT count(*) FROM files").await?,
            c.scalar("SELECT count(*) FROM extents").await?,
            c.scalar("SELECT count(*) FROM chunks").await?,
        ))
    })?;
    if workload.starts_with("publish") {
        assert_eq!(counts.0, 1 + threads as i64 * 30);
        assert_eq!(counts.1, N + threads as i64 * 30 * 64);
        assert_eq!(
            counts.2,
            N + if workload.ends_with("shared") {
                30 * 64
            } else {
                threads as i64 * 30 * 64
            }
        );
    }
    latency.sort_by(f64::total_cmp);
    let p99 = latency.get(latency.len() * 99 / 100).copied();
    println!(
        "{}",
        json!({"group":"catalog","candidate":engine,"threads":threads,"round":round,"workload":workload,"seconds":seconds,"operations":threads*if workload.starts_with("publish"){30}else{10000},"retries":retries,"p99_seconds":p99,"checksum":checksum,"counts":counts,"constraints_checked":true})
    );
    Ok(())
}
pub fn run() {
    let temp = tempfile::tempdir().unwrap();
    let seed_path = temp.path().join("seed.db");
    seed(&seed_path).unwrap();
    for round in 0..3 {
        for threads in [1, 2, 4] {
            for workload in [
                "exists",
                "repeated",
                "range",
                "publish-disjoint",
                "publish-shared",
            ] {
                let mut engines = vec!["sqlite", "turso-wal", "turso-mvcc"];
                engines.rotate_left(round);
                for engine in engines {
                    if let Err(e) = one(&seed_path, engine, threads, round, workload) {
                        println!(
                            "{}",
                            json!({"group":"catalog","candidate":engine,"threads":threads,"round":round,"workload":workload,"error":e.to_string()})
                        );
                    }
                }
            }
        }
    }
}
