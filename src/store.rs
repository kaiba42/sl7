//! Immutable imports and range reconstruction shared by CLI and mount adapters.
use crate::{
    Error, Result,
    chunk::{self, Hash},
    hex, pack,
    read_cache::{Extent, MAP_WINDOW, MAX_MAP_ROWS, MapKey},
    sync_directory,
};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{BufReader, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
const APP_ID: i64 = 0x534c3701;
const MAX_READ: usize = 16 * 1024 * 1024;
const SCHEMA: &str = "
CREATE TABLE packs(id TEXT PRIMARY KEY, size INTEGER NOT NULL CHECK(size >= 8));
CREATE TABLE chunks(hash BLOB PRIMARY KEY CHECK(length(hash)=32), raw_len INTEGER NOT NULL CHECK(raw_len BETWEEN 1 AND 32768), pack TEXT NOT NULL REFERENCES packs(id), offset INTEGER NOT NULL CHECK(offset>=8), stored_len INTEGER NOT NULL CHECK(stored_len BETWEEN 1 AND 32768), codec INTEGER NOT NULL CHECK(codec IN (0,1)));
CREATE TABLE files(id INTEGER PRIMARY KEY, view TEXT NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL CHECK(size>=0), digest BLOB NOT NULL CHECK(length(digest)=32), UNIQUE(view,name));
CREATE TABLE extents(file_id INTEGER NOT NULL REFERENCES files(id), offset INTEGER NOT NULL CHECK(offset>=0), hash BLOB NOT NULL REFERENCES chunks(hash), PRIMARY KEY(file_id,offset));
PRAGMA application_id=1397503745;
PRAGMA user_version=1;";
#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub id: i64,
    pub view: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub file: FileInfo,
    pub new_chunks: usize,
    pub new_pack_bytes: u64,
    pub references: usize,
}
#[derive(Debug, Serialize)]
pub struct Stats {
    pub files: u64,
    pub logical_bytes: u64,
    pub unique_chunks: u64,
    pub unique_raw_bytes: u64,
    pub compressed_payload_bytes: u64,
    pub referenced_pack_bytes: u64,
    pub catalog_bytes: u64,
    pub orphan_pack_bytes: u64,
    pub temporary_bytes: u64,
    pub total_file_bytes: u64,
}
/// Exclusive owner lock excludes other CLI commands and mounted sessions.
pub struct Store {
    pub(crate) root: PathBuf,
    pub(crate) db: Connection,
    pub(crate) cache: RefCell<crate::read_cache::ReadCache>,
    // Declared after the catalog so SQLite closes before ownership is released.
    _lock: OwnerLock,
}

impl Drop for Store {
    fn drop(&mut self) {
        if std::env::var_os("SL7_CACHE_STATS").is_some() {
            if let Ok(stats) = serde_json::to_string(&self.cache.borrow().stats()) {
                eprintln!("sl7: cache_stats {stats}");
            }
        }
    }
}

/// Durable packs without catalog references. Abandoned candidates are GC orphans.
pub(crate) struct PreparedImport {
    size: u64,
    digest: Hash,
    extents: Vec<(u64, Hash)>,
    locations: Vec<pack::Location>,
    packs: Vec<(String, u64)>,
}

#[cfg(all(feature = "fuse", target_os = "linux"))]
pub(crate) struct Packer {
    root: PathBuf,
    db: Connection,
}
#[cfg(all(feature = "fuse", target_os = "linux"))]
impl Packer {
    pub fn new(root: &Path) -> Result<Self> {
        let db = Connection::open_with_flags(
            root.join("catalog.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        db.busy_timeout(std::time::Duration::from_secs(10))?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
        })
    }
    pub fn prepare(&self, source: &Path, canceled: impl Fn() -> bool) -> Result<PreparedImport> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOATIME)
            .open(source)?;
        prepare_import(&self.root, &self.db, &file, pack::LIMIT, canceled)
    }
}

struct OwnerLock(File);
impl Drop for OwnerLock {
    fn drop(&mut self) {
        // A concurrent fork can briefly inherit this open-file description before
        // exec closes CLOEXEC descriptors. Explicit unlock prevents the child from
        // extending our ownership after this Store is dropped. Drop cannot report
        // errors; closing the descriptor remains the final OS cleanup fallback.
        let _ = FileExt::unlock(&self.0);
    }
}
impl Store {
    pub fn init(root: &Path) -> Result<Self> {
        if root.exists() {
            return Err(Error::Invalid(format!(
                "store destination already exists: {}",
                root.display()
            )));
        }
        fs::create_dir(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        for dir in ["packs", "tmp"] {
            fs::create_dir(root.join(dir))?;
        }
        let lock = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(root.join("owner.lock"))?;
        let lock = acquire(lock)?;
        let mut db = Connection::open(root.join("catalog.sqlite"))?;
        configure(&db)?;
        let tx = db.transaction()?;
        tx.execute_batch(SCHEMA)?;
        tx.commit()?;
        sync_directory(root)?;
        sync_directory(parent_dir(root))?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
            cache: RefCell::default(),
            _lock: lock,
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("owner.lock"))?;
        let lock = acquire(lock)?;
        let db = Connection::open_with_flags(
            root.join("catalog.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        let app: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if app != APP_ID {
            return Err(Error::Corrupt(format!(
                "catalog application id {app}, expected {APP_ID}"
            )));
        }
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 1 {
            return Err(Error::Invalid(format!(
                "unsupported store version {version}"
            )));
        }
        configure(&db)?;
        Ok(Self {
            root: root.to_path_buf(),
            db,
            cache: RefCell::default(),
            _lock: lock,
        })
    }
    pub fn put(&mut self, view: &str, name: &str, source: &Path) -> Result<ImportReport> {
        self.put_with_pack_limit(view, name, source, pack::LIMIT)
    }
    fn put_with_pack_limit(
        &mut self,
        view: &str,
        name: &str,
        source: &Path,
        pack_limit: u64,
    ) -> Result<ImportReport> {
        validate_label("view", view)?;
        validate_label("artifact name", name)?;
        if self.find(view, name)?.is_some() {
            return Err(Error::Exists {
                view: view.into(),
                name: name.into(),
            });
        }
        let prepared = prepare_import(
            &self.root,
            &self.db,
            &File::open(source)?,
            pack_limit,
            || false,
        )?;
        self.publish_import(view, name, prepared)
    }
    pub(crate) fn publish_import(
        &mut self,
        view: &str,
        name: &str,
        prepared: PreparedImport,
    ) -> Result<ImportReport> {
        validate_label("view", view)?;
        validate_label("artifact name", name)?;
        let PreparedImport {
            size: offset,
            digest,
            extents,
            locations,
            packs,
        } = prepared;
        let tx = self.db.transaction()?;
        for (id, size) in &packs {
            tx.execute(
                "INSERT OR IGNORE INTO packs VALUES(?1,?2)",
                params![id, size],
            )?;
        }
        for loc in &locations {
            tx.execute(
                "INSERT INTO chunks VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    loc.hash.as_slice(),
                    loc.raw_len,
                    loc.pack,
                    loc.offset,
                    loc.stored_len,
                    loc.codec
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO files(view,name,size,digest) VALUES(?1,?2,?3,?4)",
            params![view, name, offset, digest.as_slice()],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut statement = tx.prepare("INSERT INTO extents VALUES(?1,?2,?3)")?;
            for (offset, hash) in &extents {
                statement.execute(params![id, offset, hash.as_slice()])?;
            }
        }
        tx.commit()?;
        #[cfg(test)]
        crate::crash_at("catalog-committed");
        Ok(ImportReport {
            file: FileInfo {
                id,
                view: view.into(),
                name: name.into(),
                size: offset,
                sha256: hex(&digest),
            },
            new_chunks: locations.len(),
            new_pack_bytes: packs.iter().map(|p| p.1).sum(),
            references: extents.len(),
        })
    }
    fn find(&self, view: &str, name: &str) -> Result<Option<FileInfo>> {
        Ok(self
            .db
            .prepare_cached("SELECT id,view,name,size,digest FROM files WHERE view=?1 AND name=?2")?
            .query_row(params![view, name], file_row)
            .optional()?)
    }
    pub fn info(&self, view: &str, name: &str) -> Result<FileInfo> {
        self.find(view, name)?.ok_or_else(|| Error::NotFound {
            view: view.into(),
            name: name.into(),
        })
    }
    pub fn list(&self) -> Result<Vec<FileInfo>> {
        Ok(self
            .db
            .prepare("SELECT id,view,name,size,digest FROM files ORDER BY view,name")?
            .query_map([], file_row)?
            .collect::<std::result::Result<_, _>>()?)
    }
    pub fn read(&self, view: &str, name: &str, offset: u64, length: usize) -> Result<Vec<u8>> {
        let info = self.info(view, name)?;
        self.read_file(&info, offset, length)
    }
    pub(crate) fn read_file(&self, info: &FileInfo, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.read_file_mode(info, offset, length, true)
    }
    fn read_file_mode(
        &self,
        info: &FileInfo,
        offset: u64,
        length: usize,
        cached: bool,
    ) -> Result<Vec<u8>> {
        if length > MAX_READ {
            return Err(Error::Invalid(format!(
                "read length {length} exceeds {MAX_READ}"
            )));
        }
        if offset >= info.size {
            return Ok(vec![]);
        }
        if length == 0 {
            return Ok(vec![]);
        }
        let end = offset.saturating_add(length as u64).min(info.size);
        if cached {
            self.cache.borrow_mut().request(end - offset);
        }
        let mut output = Vec::with_capacity((end - offset) as usize);
        let mut cursor = offset;
        while cursor < end {
            let window_start = cursor / MAP_WINDOW * MAP_WINDOW;
            let window_end = window_start.saturating_add(MAP_WINDOW).min(info.size);
            let stop = end.min(window_end);
            let segment_start = cursor;
            let key = MapKey {
                file: info.id,
                size: info.size,
                digest: info.sha256.clone(),
                start: window_start,
            };
            let hit = if cached {
                self.cache.borrow_mut().map(&key)
            } else {
                None
            };
            let extents = if let Some(extents) = hit {
                Some(extents)
            } else if cached {
                self.cache.borrow_mut().map_query();
                let mut query = self.db.prepare_cached(RANGE_SQL_LIMITED)?;
                let rows = query
                    .query_map(
                        params![
                            info.id,
                            window_start.saturating_sub(chunk::MAX as u64),
                            window_end,
                            (MAX_MAP_ROWS + 1) as u64
                        ],
                        extent_row,
                    )?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if rows.len() > MAX_MAP_ROWS {
                    // Dense imported catalogs remain readable without an
                    // unbounded metadata allocation or a new format restriction.
                    self.cache.borrow_mut().map_fallback();
                    None
                } else {
                    validate_window(&rows, window_start, window_end)?;
                    let rows: Arc<[Extent]> = rows.into();
                    self.cache.borrow_mut().remember_map(key, Arc::clone(&rows));
                    Some(rows)
                }
            } else {
                None
            };
            if let Some(extents) = extents {
                let first = extents.partition_point(|e| e.start + e.location.raw_len <= cursor);
                for extent in extents[first..].iter().take_while(|e| e.start < stop) {
                    self.append_extent(
                        extent,
                        &mut cursor,
                        segment_start,
                        stop,
                        cached,
                        &mut output,
                    )?;
                }
            } else {
                // Integrity checking bypasses mapping and payload caches alike.
                if cached {
                    self.cache.borrow_mut().map_query();
                }
                let mut query = self.db.prepare_cached(RANGE_SQL)?;
                for row in query.query_map(
                    params![info.id, cursor.saturating_sub(chunk::MAX as u64), stop],
                    extent_row,
                )? {
                    self.append_extent(
                        &row?,
                        &mut cursor,
                        segment_start,
                        stop,
                        cached,
                        &mut output,
                    )?;
                }
            }
            if cursor != stop {
                return Err(Error::Corrupt(format!(
                    "manifest ends at {cursor}, requested through {stop}"
                )));
            }
        }
        Ok(output)
    }
    fn append_extent(
        &self,
        extent: &Extent,
        cursor: &mut u64,
        range_start: u64,
        end: u64,
        cached: bool,
        output: &mut Vec<u8>,
    ) -> Result<()> {
        let start = extent.start;
        let loc = &extent.location;
        let chunk_end = extent_end(extent)?;
        if chunk_end <= range_start {
            return Ok(());
        }
        let left = start.max(range_start);
        let right = chunk_end.min(end);
        if left != *cursor {
            return Err(Error::Corrupt(format!(
                "extent discontinuity at {cursor}, got {left}"
            )));
        }
        let bytes = if cached {
            self.cache.borrow_mut().read(&self.root, loc)?
        } else {
            Arc::new(pack::read(&self.root, loc)?)
        };
        output.extend_from_slice(&bytes[(left - start) as usize..(right - start) as usize]);
        *cursor = right;
        Ok(())
    }
    pub fn export(&self, view: &str, name: &str, destination: &Path) -> Result<()> {
        let info = self.info(view, name)?;
        let parent = parent_dir(destination);
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let actual = self.stream(&info, &mut temp)?;
        if actual != info.sha256 {
            return Err(Error::Corrupt(format!(
                "file checksum: expected {}, got {actual}",
                info.sha256
            )));
        }
        temp.as_file().sync_all()?;
        temp.persist_noclobber(destination)
            .map_err(|e| Error::Io(e.error))?;
        sync_directory(parent)?;
        Ok(())
    }
    pub(crate) fn stream(&self, info: &FileInfo, writer: &mut impl Write) -> Result<String> {
        self.stream_mode(info, writer, true)
    }
    fn stream_mode(
        &self,
        info: &FileInfo,
        writer: &mut impl Write,
        cached: bool,
    ) -> Result<String> {
        let mut digest = Sha256::new();
        let mut offset = 0;
        while offset < info.size {
            let bytes = self.read_file_mode(info, offset, 1024 * 1024, cached)?;
            writer.write_all(&bytes)?;
            digest.update(&bytes);
            offset += bytes.len() as u64;
        }
        Ok(hex(&digest.finalize()))
    }
    pub fn check(&self) -> Result<Stats> {
        self.cache.borrow_mut().clear();
        let health: String = self
            .db
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if health != "ok" {
            return Err(Error::Corrupt(format!("catalog integrity: {health}")));
        }
        let foreign_keys: i64 =
            self.db
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                    r.get(0)
                })?;
        if foreign_keys != 0 {
            return Err(Error::Corrupt(format!(
                "{foreign_keys} broken foreign keys"
            )));
        }
        let ids = self
            .db
            .prepare("SELECT id,size FROM packs")?
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (id, size) in ids {
            pack::verify_pack(&self.root, &id)?;
            let actual = fs::metadata(self.root.join("packs").join(&id))?.len();
            if actual != size {
                return Err(Error::Corrupt(format!(
                    "pack {id} length: expected {size}, got {actual}"
                )));
            }
        }
        for row in self
            .db
            .prepare("SELECT hash,raw_len,pack,offset,stored_len,codec FROM chunks")?
            .query_map([], |r| location_row(r, 0))?
        {
            pack::read(&self.root, &row?)?;
        }
        for info in self.list()? {
            // Validate the complete partition, including extents past logical EOF
            // that an ordinary bounded read must not visit.
            let mut cursor = 0_u64;
            let mut query = self.db.prepare("SELECT e.offset,c.raw_len FROM extents e JOIN chunks c ON e.hash=c.hash WHERE e.file_id=?1 ORDER BY e.offset")?;
            for row in query.query_map([info.id], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?))
            })? {
                let (offset, length) = row?;
                if offset != cursor {
                    return Err(Error::Corrupt(format!(
                        "file {} extent offset: expected {cursor}, got {offset}",
                        info.id
                    )));
                }
                cursor = cursor.checked_add(length).ok_or_else(|| {
                    Error::Corrupt(format!("file {} extent size overflow", info.id))
                })?;
            }
            if cursor != info.size {
                return Err(Error::Corrupt(format!(
                    "file {} extent total: expected {}, got {cursor}",
                    info.id, info.size
                )));
            }
            let actual = self.stream_mode(&info, &mut std::io::sink(), false)?;
            if actual != info.sha256 {
                return Err(Error::Corrupt(format!(
                    "file {}/{} checksum: expected {}, got {actual}",
                    info.view, info.name, info.sha256
                )));
            }
        }
        self.stats()
    }
    pub fn stats(&self) -> Result<Stats> {
        let (files, logical_bytes) = self.db.query_row(
            "SELECT count(*),coalesce(sum(size),0) FROM files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (unique_chunks, unique_raw_bytes, compressed_payload_bytes) = self.db.query_row(
            "SELECT count(*),coalesce(sum(raw_len),0),coalesce(sum(stored_len),0) FROM chunks",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let referenced_pack_bytes: u64 =
            self.db
                .query_row("SELECT coalesce(sum(size),0) FROM packs", [], |r| r.get(0))?;
        let known: HashSet<String> = self
            .db
            .prepare("SELECT id FROM packs")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let mut orphan_pack_bytes = 0;
        for entry in fs::read_dir(self.root.join("packs"))? {
            let e = entry?;
            if !known.contains(&e.file_name().to_string_lossy().into_owned()) {
                orphan_pack_bytes += e.metadata()?.len();
            }
        }
        let temporary_bytes = directory_bytes(&self.root.join("tmp"))?;
        let mut catalog_bytes = 0;
        for name in ["catalog.sqlite", "catalog.sqlite-wal", "catalog.sqlite-shm"] {
            match fs::metadata(self.root.join(name)) {
                Ok(m) => catalog_bytes += m.len(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(Stats {
            files,
            logical_bytes,
            unique_chunks,
            unique_raw_bytes,
            compressed_payload_bytes,
            referenced_pack_bytes,
            catalog_bytes,
            orphan_pack_bytes,
            temporary_bytes,
            total_file_bytes: referenced_pack_bytes
                + catalog_bytes
                + orphan_pack_bytes
                + temporary_bytes,
        })
    }
}
fn prepare_import(
    root: &Path,
    db: &Connection,
    file: &File,
    pack_limit: u64,
    canceled: impl Fn() -> bool,
) -> Result<PreparedImport> {
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(Error::Invalid("source is not a regular file".into()));
    }
    // Quiescent sources only: these checks detect common races, not arbitrary external writers.
    let mut chunker = chunk::Chunker::new(BufReader::new(file));
    let mut digest = Sha256::new();
    let mut extents: Vec<(u64, Hash)> = Vec::new();
    let mut seen = HashSet::new();
    let mut locations = Vec::new();
    let mut packs = Vec::new();
    // No pack is needed until the first novel chunk.
    let mut writer: Option<pack::Writer> = None;
    let mut offset = 0_u64;
    let mut exists_query =
        db.prepare_cached("SELECT EXISTS(SELECT 1 FROM chunks WHERE hash=?1)")?;
    while let Some(bytes) = chunker.next_chunk()? {
        if canceled() {
            return Err(Error::SourceChanged("sealing candidate canceled".into()));
        }
        digest.update(&bytes);
        let hash = chunk::identity(&bytes);
        let exists: bool = exists_query.query_row([hash.as_slice()], |r| r.get(0))?;
        if !exists && seen.insert(hash) {
            if let Some(w) = writer.take() {
                if w.size + bytes.len() as u64 + 49 > pack_limit {
                    let (id, size, locs) = w.finish(root)?;
                    packs.push((id, size));
                    locations.extend(locs);
                } else {
                    writer = Some(w);
                }
            }
            let mut w = match writer.take() {
                Some(w) => w,
                None => pack::Writer::new(root)?,
            };
            w.add(hash, &bytes)?;
            writer = Some(w);
        }
        extents.push((offset, hash));
        offset = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| Error::Invalid("source length overflow".into()))?;
        if offset > i64::MAX as u64 {
            return Err(Error::Invalid("source exceeds catalog length limit".into()));
        }
    }
    let after = file.metadata()?;
    if before.len() != after.len() {
        return Err(Error::SourceChanged(format!(
            "length {} -> {}",
            before.len(),
            after.len()
        )));
    }
    if before.modified()? != after.modified()? {
        return Err(Error::SourceChanged("modification time differs".into()));
    }
    if offset != before.len() {
        return Err(Error::SourceChanged(format!(
            "read {offset} bytes, expected {}",
            before.len()
        )));
    }
    if let Some(w) = writer {
        let (id, size, locs) = w.finish(root)?;
        packs.push((id, size));
        locations.extend(locs);
    }
    // Packs and directory entries are durable before any reference is committed.
    let digest: Hash = digest.finalize().into();
    Ok(PreparedImport {
        size: offset,
        digest,
        extents,
        locations,
        packs,
    })
}

const RANGE_SQL: &str = "SELECT e.offset,c.hash,c.raw_len,c.pack,c.offset,c.stored_len,c.codec FROM extents e JOIN chunks c ON c.hash=e.hash WHERE e.file_id=?1 AND e.offset>=?2 AND e.offset<?3 ORDER BY e.offset";
const RANGE_SQL_LIMITED: &str = "SELECT e.offset,c.hash,c.raw_len,c.pack,c.offset,c.stored_len,c.codec FROM extents e JOIN chunks c ON c.hash=e.hash WHERE e.file_id=?1 AND e.offset>=?2 AND e.offset<?3 ORDER BY e.offset LIMIT ?4";
fn extent_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Extent> {
    Ok(Extent {
        start: row.get(0)?,
        location: location_row(row, 1)?,
    })
}
fn extent_end(extent: &Extent) -> Result<u64> {
    if extent.location.raw_len == 0 || extent.location.raw_len > chunk::MAX as u64 {
        return Err(Error::Corrupt("invalid extent length".into()));
    }
    extent
        .start
        .checked_add(extent.location.raw_len)
        .ok_or_else(|| Error::Corrupt("extent end overflow".into()))
}
fn validate_window(extents: &[Extent], start: u64, end: u64) -> Result<()> {
    let mut cursor = start;
    for extent in extents {
        let stop = extent_end(extent)?;
        if stop <= start {
            continue;
        }
        if extent.start.max(start) != cursor {
            return Err(Error::Corrupt(format!("extent discontinuity at {cursor}")));
        }
        cursor = stop.min(end);
    }
    if cursor != end {
        return Err(Error::Corrupt(format!(
            "manifest ends at {cursor}, expected {end}"
        )));
    }
    Ok(())
}

fn acquire(file: File) -> Result<OwnerLock> {
    match file.try_lock_exclusive() {
        Ok(()) => Ok(OwnerLock(file)),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Err(Error::Locked(e)),
        Err(e) => Err(e.into()),
    }
}
fn configure(db: &Connection) -> Result<()> {
    db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")?;
    Ok(())
}
fn validate_label(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Invalid(format!("{field} is empty")));
    }
    if value.len() > 4096 {
        return Err(Error::Invalid(format!("{field} exceeds 4096 bytes")));
    }
    Ok(())
}
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}
fn file_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileInfo> {
    Ok(FileInfo {
        id: r.get(0)?,
        view: r.get(1)?,
        name: r.get(2)?,
        size: r.get(3)?,
        sha256: hex(&r.get::<_, Vec<u8>>(4)?),
    })
}
fn location_row(r: &rusqlite::Row<'_>, i: usize) -> rusqlite::Result<pack::Location> {
    let hash: Vec<u8> = r.get(i)?;
    let hash: Hash = hash.try_into().map_err(|v: Vec<u8>| {
        rusqlite::Error::FromSqlConversionFailure(
            i,
            rusqlite::types::Type::Blob,
            Box::new(Error::Corrupt(format!("chunk hash length {}", v.len()))),
        )
    })?;
    Ok(pack::Location {
        hash,
        raw_len: r.get(i + 1)?,
        pack: r.get(i + 2)?,
        offset: r.get(i + 3)?,
        stored_len: r.get(i + 4)?,
        codec: r.get(i + 5)?,
    })
}
fn directory_bytes(path: &Path) -> Result<u64> {
    let mut bytes = 0;
    for e in fs::read_dir(path)? {
        bytes += e?.metadata()?.len();
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn warm_ranges_skip_extent_queries_and_dense_windows_fall_back() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("input");
        fs::write(&source, vec![19; 700_000])?;
        let mut store = Store::init(&temp.path().join("store"))?;
        store.put("a", "b", &source)?;
        store.read("a", "b", 0, 4096)?;
        let queries = store.cache.borrow().stats().map_queries;
        store.read("a", "b", 4096, 4096)?;
        assert_eq!(store.cache.borrow().stats().map_queries, queries);
        assert_eq!(store.cache.borrow().stats().map_hits, 1);

        // A valid, densely partitioned catalog may exceed the cache loader's
        // row cap. Preserve format compatibility via the bounded streaming path.
        fs::write(&source, [37])?;
        let info = store.put("a", "dense", &source)?.file;
        let hash: Vec<u8> = store.db.query_row(
            "SELECT hash FROM extents WHERE file_id=?1",
            [info.id],
            |r| r.get(0),
        )?;
        let tx = store.db.transaction()?;
        tx.execute(
            "UPDATE files SET size=2000,digest=?2 WHERE id=?1",
            params![info.id, Sha256::digest(vec![37; 2000]).as_slice()],
        )?;
        for offset in 1..2000 {
            tx.execute(
                "INSERT INTO extents VALUES(?1,?2,?3)",
                params![info.id, offset, &hash],
            )?;
        }
        tx.commit()?;
        assert_eq!(store.read("a", "dense", 999, 1000)?, vec![37; 1000]);
        assert_eq!(store.cache.borrow().stats().map_fallbacks, 1);
        store.check()?;
        Ok(())
    }

    #[test]
    fn dropping_owner_releases_lock_even_with_inherited_descriptor() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let store = Store::init(&root)?;
        let inherited = store._lock.0.try_clone()?;
        drop(store);
        let reopened = Store::open(&root)?;
        drop(inherited);
        drop(reopened);
        Ok(())
    }
    #[test]
    fn multiple_packs_roundtrip_and_respect_limit() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let source = temp.path().join("input");
        let mut state = 42_u64;
        let bytes: Vec<u8> = (0..500_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        fs::write(&source, &bytes)?;
        let mut store = Store::init(&root)?;
        store.put_with_pack_limit("main", "file", &source, 65536)?;
        let count: u64 = store
            .db
            .query_row("SELECT count(*) FROM packs", [], |r| r.get(0))?;
        let maximum: u64 = store
            .db
            .query_row("SELECT max(size) FROM packs", [], |r| r.get(0))?;
        assert!(count > 1);
        assert!(maximum <= 65536);
        assert_eq!(store.read("main", "file", 0, bytes.len())?, bytes);
        store.check()?;
        Ok(())
    }
    #[test]
    fn crash_import_child() -> Result<()> {
        let Some(root) = std::env::var_os("SL7_TEST_CHILD_ROOT") else {
            return Ok(());
        };
        let root = PathBuf::from(root);
        Store::open(&root)?.put("main", "new", &root.join("../new-input"))?;
        Ok(())
    }
    #[test]
    fn process_death_preserves_old_files_and_atomic_visibility() -> Result<()> {
        for stage in ["pack-synced", "pack-published", "catalog-committed"] {
            let temp = tempfile::tempdir()?;
            let root = temp.path().join("store");
            let old = temp.path().join("old-input");
            fs::write(&old, b"old committed contents")?;
            fs::write(temp.path().join("new-input"), b"new committed contents")?;
            let mut store = Store::init(&root)?;
            store.put("main", "old", &old)?;
            drop(store);
            let status = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "store::tests::crash_import_child"])
                .env("SL7_TEST_CHILD_ROOT", &root)
                .env("SL7_TEST_CRASH_STAGE", stage)
                .stdout(std::process::Stdio::null())
                .status()?;
            assert_eq!(status.code(), Some(73), "did not reach {stage}");
            let store = Store::open(&root)?;
            assert_eq!(
                store.read("main", "old", 0, 100)?,
                b"old committed contents"
            );
            if stage == "catalog-committed" {
                assert_eq!(
                    store.read("main", "new", 0, 100)?,
                    b"new committed contents"
                );
            } else {
                assert!(matches!(
                    store.info("main", "new"),
                    Err(Error::NotFound { .. })
                ));
            }
            store.check()?;
        }
        Ok(())
    }
    #[test]
    fn failed_publication_leaves_no_manifest_and_can_recover() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let source = temp.path().join("input");
        fs::write(&source, b"survives catalog rollback")?;
        let mut store = Store::init(&root)?;
        store.db.execute_batch("CREATE TRIGGER fail_publish BEFORE INSERT ON files BEGIN SELECT RAISE(ABORT,'injected publication failure'); END;")?;
        assert!(matches!(
            store.put("main", "a", &source),
            Err(Error::Sql(_))
        ));
        assert!(store.list()?.is_empty());
        assert!(store.stats()?.orphan_pack_bytes > 0);
        store.db.execute_batch("DROP TRIGGER fail_publish")?;
        drop(store);
        let mut store = Store::open(&root)?;
        store.put("main", "a", &source)?;
        assert_eq!(
            store.read("main", "a", 0, 100)?,
            b"survives catalog rollback"
        );
        assert_eq!(store.check()?.orphan_pack_bytes, 0);
        Ok(())
    }
}
