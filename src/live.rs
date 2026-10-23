//! Durable native-inode ↔ immutable-manifest transitions for the Linux mount.
//! A committed manifest always wins over native bytes. Hydration clears that
//! reference only after verified native bytes and preserved timestamps are synced.
use crate::{Error, FileInfo, GcReport, Result, Stats, Store, sync_directory};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
};

const VIEW: &str = ":sl7-live-v1:";
const XATTR: &std::ffi::CStr = c"user.sl7.file.v1";
pub const MIN_SEAL_SIZE: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub size: u64,
    pub atime: i64,
    pub atime_ns: i64,
    pub mtime: i64,
    pub mtime_ns: i64,
    pub ctime: i64,
    pub ctime_ns: i64,
}
impl Metadata {
    pub fn capture(file: &File) -> Result<Self> {
        let m = file.metadata()?;
        Ok(Self {
            size: m.len(),
            atime: m.atime(),
            atime_ns: m.atime_nsec(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ctime: m.ctime(),
            ctime_ns: m.ctime_nsec(),
        })
    }
    fn restore_times(&self, file: &File) -> Result<()> {
        let times = [
            libc::timespec {
                tv_sec: self.atime,
                tv_nsec: self.atime_ns,
            },
            libc::timespec {
                tv_sec: self.mtime,
                tv_nsec: self.mtime_ns,
            },
        ];
        // SAFETY: file is live and times contains exactly two valid timespecs.
        if unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}
pub struct Sealed {
    pub manifest: FileInfo,
    pub metadata: Metadata,
}
pub struct Live {
    pub(crate) store: Store,
}
pub(crate) struct SealCandidate {
    id: i64,
    generation: i64,
    metadata: Metadata,
}
// Persistent identity, optional manifest id, optional logical metadata JSON.
type InodeRecord = (i64, Option<i64>, Option<String>);

#[derive(Serialize)]
pub struct LiveStats {
    pub files: u64,
    pub sealed_files: u64,
    pub logical_bytes: u64,
    pub native_allocated_bytes: u64,
    pub content: Stats,
}

fn fd_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}
fn path_c(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Invalid("NUL in native path".into()))
}
fn id(file: &File) -> Result<Option<i64>> {
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let path = path_c(&fd_path(file))?;
    let mut value = [0_u8; 32];
    // SAFETY: path/key are C strings and the output buffer has the given size.
    let n = unsafe {
        libc::getxattr(
            path.as_ptr(),
            XATTR.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::ENODATA) {
            return Ok(None);
        }
        return Err(e.into());
    }
    let value = std::str::from_utf8(&value[..n as usize])
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| Error::Corrupt("invalid live inode identity".into()))?;
    Ok(Some(value))
}
fn decode(value: String) -> Result<Metadata> {
    serde_json::from_str(&value).map_err(|e| Error::Corrupt(format!("live metadata: {e}")))
}
fn encode(value: &Metadata) -> Result<String> {
    serde_json::to_string(value).map_err(|e| Error::Invalid(e.to_string()))
}

impl Live {
    pub fn open(mut store: Store) -> Result<Self> {
        let exists: bool = store.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='live_config')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            let reserved: bool = store.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM files WHERE view=?1)",
                [VIEW],
                |r| r.get(0),
            )?;
            if reserved {
                return Err(Error::Invalid(
                    "reserved live view already contains imported files".into(),
                ));
            }
            let tx = store.db.transaction()?;
            tx.execute_batch("CREATE TABLE live_config(version INTEGER NOT NULL); INSERT INTO live_config VALUES(1);
                CREATE TABLE live_files(id INTEGER PRIMARY KEY AUTOINCREMENT, generation INTEGER NOT NULL DEFAULT 0,
                    manifest INTEGER REFERENCES files(id), metadata TEXT);")?;
            tx.commit()?;
        }
        let version: i64 = store
            .db
            .query_row("SELECT version FROM live_config", [], |r| r.get(0))?;
        if version != 1 {
            return Err(Error::Invalid(format!("unsupported live format {version}")));
        }
        let tree = store.root.join("tree");
        if !tree.exists() {
            let count: i64 = store
                .db
                .query_row("SELECT count(*) FROM live_files", [], |r| r.get(0))?;
            if count != 0 {
                return Err(Error::Corrupt("live namespace is missing".into()));
            }
            fs::create_dir(&tree)?;
            sync_directory(&store.root)?;
        }
        Ok(Self { store })
    }
    pub fn tree(&self) -> PathBuf {
        self.store.root.join("tree")
    }
    fn record(&self, file: &File) -> Result<Option<InodeRecord>> {
        let Some(id) = id(file)? else {
            return Ok(None);
        };
        let row = self
            .store
            .db
            .query_row(
                "SELECT manifest,metadata FROM live_files WHERE id=?1",
                [id],
                |r| Ok((id, r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.ok_or_else(|| Error::Corrupt(format!("live inode references missing identity {id}")))
            .map(Some)
    }
    pub fn metadata(&self, file: &File) -> Result<Option<Metadata>> {
        self.record(file)?
            .and_then(|(_, _, metadata)| metadata)
            .map(decode)
            .transpose()
    }
    pub fn sealed(&self, file: &File) -> Result<Option<Sealed>> {
        let Some((_, Some(manifest), metadata)) = self.record(file)? else {
            return Ok(None);
        };
        let name: String = self.store.db.query_row(
            "SELECT name FROM files WHERE id=?1 AND view=?2",
            params![manifest, VIEW],
            |r| r.get(0),
        )?;
        let manifest = self.store.info(VIEW, &name)?;
        let metadata = decode(
            metadata.ok_or_else(|| Error::Corrupt("sealed inode missing metadata".into()))?,
        )?;
        if manifest.size != metadata.size {
            return Err(Error::Corrupt(
                "sealed file size differs from manifest".into(),
            ));
        }
        Ok(Some(Sealed { manifest, metadata }))
    }
    pub fn read(&self, file: &File, offset: u64, size: usize) -> Result<Option<Vec<u8>>> {
        self.sealed(file)?
            .map(|sealed| self.store.read_file(&sealed.manifest, offset, size))
            .transpose()
    }
    fn identity(&mut self, file: &File) -> Result<i64> {
        if let Some((id, _, _)) = self.record(file)? {
            return Ok(id);
        }
        self.store
            .db
            .execute("INSERT INTO live_files DEFAULT VALUES", [])?;
        let id = self.store.db.last_insert_rowid();
        let value = id.to_string();
        let path = path_c(&fd_path(file))?;
        // SAFETY: valid C strings and value bytes live for the call. CREATE avoids
        // overwriting an identity assigned by an unexpected external writer.
        if unsafe {
            libc::setxattr(
                path.as_ptr(),
                XATTR.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                libc::XATTR_CREATE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        File::open(fd_path(file))?.sync_all()?;
        Ok(id)
    }
    /// Capture transition identity while the filesystem state is locked.
    pub(crate) fn begin_seal(&mut self, file: &File) -> Result<Option<SealCandidate>> {
        let native = file.metadata()?;
        if !native.is_file()
            || native.nlink() == 0
            || native.len() < MIN_SEAL_SIZE
            // Retirement and later hydration require owner read/write access.
            // Leave read-only outputs native instead of committing a manifest
            // which a non-root owner cannot subsequently hydrate.
            || native.mode() & 0o600 != 0o600
            || self.sealed(file)?.is_some()
        {
            return Ok(None);
        }
        let metadata = self.metadata(file)?.unwrap_or(Metadata::capture(file)?);
        let id = self.identity(file)?;
        let generation: i64 = self.store.db.query_row(
            "UPDATE live_files SET generation=generation+1 WHERE id=?1 RETURNING generation",
            [id],
            |r| r.get(0),
        )?;
        Ok(Some(SealCandidate {
            id,
            generation,
            metadata,
        }))
    }
    fn bind_seal(&mut self, file: &File, candidate: SealCandidate, manifest: i64) -> Result<bool> {
        let changed = self.store.db.execute(
            "UPDATE live_files SET manifest=?1,metadata=?2 WHERE id=?3 AND generation=?4 AND manifest IS NULL",
            params![manifest, encode(&candidate.metadata)?, candidate.id, candidate.generation],
        )?;
        if changed != 1 {
            return Err(Error::Invalid("stale seal generation".into()));
        }
        #[cfg(test)]
        crate::crash_at("live-seal-committed");
        self.retire(file, &candidate.metadata)?;
        Ok(true)
    }
    pub(crate) fn publish_seal(
        &mut self,
        file: &File,
        candidate: SealCandidate,
        prepared: crate::store::PreparedImport,
    ) -> Result<bool> {
        let report = self.store.publish_import(
            VIEW,
            &format!("{}-{}", candidate.id, candidate.generation),
            prepared,
        )?;
        self.bind_seal(file, candidate, report.file.id)
    }
    /// Synchronous transition used by offline tests and recovery fixtures.
    pub fn seal(&mut self, file: &File) -> Result<bool> {
        let Some(candidate) = self.begin_seal(file)? else {
            return Ok(false);
        };
        let report = self.store.put(
            VIEW,
            &format!("{}-{}", candidate.id, candidate.generation),
            &fd_path(file),
        )?;
        self.bind_seal(file, candidate, report.file.id)
    }
    fn retire(&self, file: &File, metadata: &Metadata) -> Result<()> {
        // The catalog is authoritative from this point, even after partial
        // retirement. The native inode remains for names, links and permissions.
        let native = OpenOptions::new().write(true).open(fd_path(file))?;
        native.set_len(0)?;
        #[cfg(test)]
        crate::crash_at("live-retired-truncated");
        native.set_len(metadata.size)?;
        metadata.restore_times(&native)?;
        native.sync_all()?;
        Ok(())
    }
    /// Finish interrupted retirement/hydration before the mount accepts requests.
    /// A committed manifest remains authoritative throughout this operation.
    pub fn recover(&mut self) -> Result<()> {
        let mut pending = vec![self.tree()];
        let mut seen = HashSet::new();
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let ty = entry.file_type()?;
                if ty.is_dir() {
                    pending.push(entry.path());
                } else if ty.is_file() {
                    let file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
                        .open(entry.path())?;
                    let m = file.metadata()?;
                    if !seen.insert((m.dev(), m.ino())) {
                        continue;
                    }
                    if let Some(sealed) = self.sealed(&file)? {
                        if m.blocks() != 0 || m.len() != sealed.metadata.size {
                            self.retire(&file, &sealed.metadata)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
    /// Hydrate before mutation. Keep the manifest committed until every byte is
    /// verified and synced, so a crash during reconstruction simply retries.
    pub fn hydrate(&mut self, file: &File) -> Result<bool> {
        let Some(sealed) = self.sealed(file)? else {
            return Ok(false);
        };
        let id = id(file)?.unwrap();
        let mut native = OpenOptions::new().write(true).open(fd_path(file))?;
        native.set_len(0)?;
        native.seek(SeekFrom::Start(0))?;
        #[cfg(test)]
        crate::crash_at("live-hydrate-truncated");
        let digest = self.store.stream(&sealed.manifest, &mut native)?;
        if digest != sealed.manifest.sha256 {
            return Err(Error::Corrupt("hydration checksum differs".into()));
        }
        sealed.metadata.restore_times(&native)?;
        native.sync_all()?;
        #[cfg(test)]
        crate::crash_at("live-hydrate-synced");
        let tx = self.store.db.transaction()?;
        tx.execute("UPDATE live_files SET manifest=NULL WHERE id=?1", [id])?;
        tx.execute("DELETE FROM extents WHERE file_id=?1", [sealed.manifest.id])?;
        tx.execute("DELETE FROM files WHERE id=?1", [sealed.manifest.id])?;
        tx.commit()?;
        #[cfg(test)]
        crate::crash_at("live-hydrate-committed");
        Ok(true)
    }
    /// Discard sealed contents without reconstructing bytes that will be removed.
    /// Until the catalog commits, recovery can still restore the old manifest.
    pub fn truncate_zero(&mut self, file: &File) -> Result<bool> {
        let Some(sealed) = self.sealed(file)? else {
            return Ok(false);
        };
        let identity = id(file)?.unwrap();
        let native = OpenOptions::new().write(true).open(fd_path(file))?;
        native.set_len(0)?;
        #[cfg(test)]
        crate::crash_at("live-zero-truncated");
        native.sync_all()?;
        #[cfg(test)]
        crate::crash_at("live-zero-synced");
        let tx = self.store.db.transaction()?;
        tx.execute(
            "UPDATE live_files SET manifest=NULL,metadata=NULL WHERE id=?1",
            [identity],
        )?;
        tx.execute("DELETE FROM extents WHERE file_id=?1", [sealed.manifest.id])?;
        tx.execute("DELETE FROM files WHERE id=?1", [sealed.manifest.id])?;
        tx.commit()?;
        #[cfg(test)]
        crate::crash_at("live-zero-committed");
        Ok(true)
    }

    /// Preserve sealed content while reflecting a successful native metadata edit.
    pub fn metadata_changed(&mut self, file: &File, atime: bool, mtime: bool) -> Result<()> {
        if let Some((identity, _, Some(value))) = self.record(file)? {
            let mut metadata = decode(value)?;
            let now = Metadata::capture(file)?;
            if atime {
                metadata.atime = now.atime;
                metadata.atime_ns = now.atime_ns;
            }
            if mtime {
                metadata.mtime = now.mtime;
                metadata.mtime_ns = now.mtime_ns;
            }
            metadata.ctime = now.ctime;
            metadata.ctime_ns = now.ctime_ns;
            self.store.db.execute(
                "UPDATE live_files SET metadata=?1 WHERE id=?2",
                params![encode(&metadata)?, identity],
            )?;
        }
        Ok(())
    }
    /// Native mutation now owns size and timestamps. Invoke before allowing a
    /// write/truncate/chmod/utime while still holding the state mutex.
    pub fn mutated(&mut self, file: &File) -> Result<()> {
        if let Some((id, manifest, metadata)) = self.record(file)? {
            if manifest.is_some() {
                return Err(Error::Corrupt("native mutation on sealed inode".into()));
            }
            if metadata.is_some() {
                self.store
                    .db
                    .execute("UPDATE live_files SET metadata=NULL WHERE id=?1", [id])?;
            }
        }
        Ok(())
    }
    pub fn namespace_changed(&mut self, file: &File) -> Result<()> {
        self.metadata_changed(file, false, false)
    }
    fn scan(&self) -> Result<(HashSet<i64>, LiveStats)> {
        let mut roots = HashSet::new();
        let mut identities = HashSet::new();
        let mut pending = vec![self.tree()];
        let mut stats = LiveStats {
            files: 0,
            sealed_files: 0,
            logical_bytes: 0,
            native_allocated_bytes: 0,
            content: self.store.stats()?,
        };
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path)? {
                let entry = entry?;
                let ty = entry.file_type()?;
                if ty.is_dir() {
                    pending.push(entry.path());
                } else if ty.is_file() {
                    let file = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
                        .open(entry.path())?;
                    let m = file.metadata()?;
                    if !identities.insert((m.dev(), m.ino())) {
                        continue;
                    }
                    let record = self.record(&file)?;
                    if let Some((id, manifest, _)) = &record {
                        roots.insert(*id);
                        stats.sealed_files += u64::from(manifest.is_some());
                    }
                    stats.files += 1;
                    stats.logical_bytes += self.metadata(&file)?.map_or(m.len(), |m| m.size);
                    stats.native_allocated_bytes += m.blocks() * 512;
                }
            }
        }
        Ok((roots, stats))
    }
    pub fn stats(&self) -> Result<LiveStats> {
        Ok(self.scan()?.1)
    }
    /// Quiescent only: owning Store excludes the mounted daemon. No descriptor or
    /// mapping roots can exist in a live session when this offline operation runs.
    pub fn gc(&mut self) -> Result<GcReport> {
        let roots = self.scan()?.0;
        let ids: Vec<i64> = self
            .store
            .db
            .prepare("SELECT id FROM live_files")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let tx = self.store.db.transaction()?;
        for id in ids {
            if !roots.contains(&id) {
                tx.execute("DELETE FROM live_files WHERE id=?1", [id])?;
            }
        }
        tx.execute("DELETE FROM extents WHERE file_id IN (SELECT id FROM files WHERE view=?1 AND id NOT IN(SELECT manifest FROM live_files WHERE manifest IS NOT NULL))", [VIEW])?;
        tx.execute("DELETE FROM files WHERE view=?1 AND id NOT IN(SELECT manifest FROM live_files WHERE manifest IS NOT NULL)", [VIEW])?;
        tx.commit()?;
        self.store.compact()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    fn bytes() -> Vec<u8> {
        (0..300_000)
            .map(|i| ((i * 17 + i / 251) % 253) as u8)
            .collect()
    }
    #[test]
    fn seal_hydrate_modify_and_collect() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut live = Live::open(Store::init(&temp.path().join("store"))?)?;
        let path = live.tree().join("artifact");
        fs::write(&path, bytes())?;
        let file = File::open(&path)?;
        let before = Metadata::capture(&file)?;
        assert!(live.seal(&file)?);
        assert_eq!(file.metadata()?.blocks(), 0);
        assert_eq!(live.read(&file, 0, 300_000)?.unwrap(), bytes());
        assert_eq!(live.metadata(&file)?.unwrap().mtime_ns, before.mtime_ns);
        live.hydrate(&file)?;
        assert_eq!(fs::read(&path)?, bytes());
        assert!(live.sealed(&file)?.is_none());
        live.mutated(&file)?;
        OpenOptions::new()
            .write(true)
            .open(&path)?
            .write_all(b"edit")?;
        assert!(live.seal(&file)?);
        assert_eq!(live.read(&file, 0, 4)?.unwrap(), b"edit");
        drop(file);
        fs::remove_file(&path)?;
        let report = live.gc()?;
        assert_eq!(report.stats.unique_chunks, 0);
        assert_eq!(report.stats.referenced_pack_bytes, 0);
        assert_eq!(report.stats.orphan_pack_bytes, 0);
        Ok(())
    }
    #[test]
    fn read_only_output_stays_native_until_owner_writable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut live = Live::open(Store::init(&temp.path().join("store"))?)?;
        let path = live.tree().join("artifact");
        fs::write(&path, bytes())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444))?;
        let file = File::open(&path)?;
        assert!(!live.seal(&file)?);
        assert!(live.sealed(&file)?.is_none());
        assert_eq!(fs::read(&path)?, bytes());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
        assert!(live.seal(&file)?);
        assert_eq!(live.read(&file, 0, 300_000)?.unwrap(), bytes());
        Ok(())
    }
    #[test]
    fn sealed_metadata_edits_preserve_manifest_and_sparse_backing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let mut live = Live::open(Store::init(&root)?)?;
        let path = live.tree().join("artifact");
        fs::write(&path, bytes())?;
        let file = File::open(&path)?;
        live.seal(&file)?;
        let manifest = live.sealed(&file)?.unwrap().manifest.id;
        let old = live.metadata(&file)?.unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444))?;
        live.metadata_changed(&file, false, false)?;
        assert_eq!(live.metadata(&file)?.unwrap().mtime_ns, old.mtime_ns);
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(123456)),
        )?;
        live.metadata_changed(&file, false, true)?;
        assert_eq!(file.metadata()?.blocks(), 0);
        assert_eq!(live.sealed(&file)?.unwrap().manifest.id, manifest);
        assert_eq!(live.metadata(&file)?.unwrap().mtime, 123456);
        drop(live);
        let mut live = Live::open(Store::open(&root)?)?;
        live.recover()?;
        assert_eq!(live.metadata(&file)?.unwrap().mtime, 123456);
        assert_eq!(live.read(&file, 0, 300_000)?.unwrap(), bytes());
        assert_eq!(file.metadata()?.blocks(), 0);
        Ok(())
    }
    #[test]
    fn truncate_zero_discards_manifest_without_reconstructing() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let mut live = Live::open(Store::init(&temp.path().join("store"))?)?;
        let path = live.tree().join("artifact");
        fs::write(&path, bytes())?;
        let file = File::open(&path)?;
        live.seal(&file)?;
        // Make reconstruction impossible: truncation must not read the old packs.
        for pack in fs::read_dir(live.store.root.join("packs"))? {
            fs::remove_file(pack?.path())?;
        }
        assert!(live.truncate_zero(&file)?);
        assert_eq!(file.metadata()?.len(), 0);
        assert!(live.sealed(&file)?.is_none());
        assert!(live.metadata(&file)?.is_none());
        Ok(())
    }
    #[test]
    fn crash_transition_child() -> Result<()> {
        let Some(root) = std::env::var_os("SL7_LIVE_TEST_ROOT") else {
            return Ok(());
        };
        let mut live = Live::open(Store::open(Path::new(&root))?)?;
        let file = File::open(live.tree().join("artifact"))?;
        let stage = std::env::var("SL7_TEST_CRASH_STAGE").unwrap();
        if stage.starts_with("live-hydrate") {
            live.hydrate(&file)?;
        } else if stage.starts_with("live-zero") {
            live.truncate_zero(&file)?;
        } else {
            live.seal(&file)?;
        }
        Ok(())
    }
    #[test]
    fn process_death_during_sealing_and_hydration_preserves_bytes() -> Result<()> {
        for stage in [
            "live-seal-committed",
            "live-retired-truncated",
            "live-hydrate-truncated",
            "live-hydrate-synced",
            "live-hydrate-committed",
            "live-zero-truncated",
            "live-zero-synced",
            "live-zero-committed",
        ] {
            let temp = tempfile::tempdir()?;
            let root = temp.path().join("store");
            let mut live = Live::open(Store::init(&root)?)?;
            let path = live.tree().join("artifact");
            fs::write(&path, bytes())?;
            if stage.starts_with("live-hydrate") || stage.starts_with("live-zero") {
                live.seal(&File::open(&path)?)?;
            }
            drop(live);
            let status = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "live::tests::crash_transition_child"])
                .env("SL7_LIVE_TEST_ROOT", &root)
                .env("SL7_TEST_CRASH_STAGE", stage)
                .stdout(std::process::Stdio::null())
                .status()?;
            assert_eq!(status.code(), Some(73), "{stage}");
            let mut live = Live::open(Store::open(&root)?)?;
            live.recover()?;
            let file = File::open(&path)?;
            let actual = match live.read(&file, 0, 300_000)? {
                Some(bytes) => bytes,
                None => fs::read(&path)?,
            };
            let expected = if stage == "live-zero-committed" {
                vec![]
            } else {
                bytes()
            };
            assert_eq!(actual, expected, "{stage}");
            live.hydrate(&file)?;
            assert_eq!(fs::read(&path)?, expected);
            live.gc()?;
        }
        Ok(())
    }
}
