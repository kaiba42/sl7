//! Exclusive-owner, quiescent collection. Publish rewritten packs before moving
//! references; retire old bytes only after the catalog commit is durable.
use crate::{Result, Stats, Store, pack, sync_directory};
use rusqlite::params;
use serde::Serialize;
use std::{collections::HashSet, fs};

#[derive(Debug, Serialize)]
pub struct GcReport {
    pub removed_chunks: usize,
    pub rewritten_packs: usize,
    pub removed_pack_bytes: u64,
    pub stats: Stats,
}

impl Store {
    /// Remove an immutable label, retaining shared chunks until explicit GC.
    pub fn remove(&mut self, view: &str, name: &str) -> Result<()> {
        let info = self.info(view, name)?;
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM extents WHERE file_id=?1", [info.id])?;
        tx.execute("DELETE FROM files WHERE id=?1", [info.id])?;
        tx.commit()?;
        Ok(())
    }

    /// Caller must first remove unreachable live manifests (if using live trees).
    /// The exclusive Store lock excludes every mounted session and offline writer.
    pub fn compact(&mut self) -> Result<GcReport> {
        self.check()?;
        let before = self.stats()?;
        let removed_chunks = self.db.execute(
            "DELETE FROM chunks WHERE NOT EXISTS(SELECT 1 FROM extents WHERE extents.hash=chunks.hash)", [],)?;
        let packs: Vec<(String, u64)> = self
            .db
            .prepare("SELECT id,size FROM packs")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let mut rewritten_packs = 0;
        for (old, old_size) in packs {
            let locations: Vec<pack::Location> = self.db.prepare(
                "SELECT hash,raw_len,offset,stored_len,codec FROM chunks WHERE pack=?1 ORDER BY offset")?
                .query_map([&old], |r| {
                    let bytes: Vec<u8> = r.get(0)?;
                    let hash = bytes.try_into().map_err(|_| rusqlite::Error::InvalidQuery)?;
                    Ok(pack::Location { hash, raw_len: r.get(1)?, pack: old.clone(), offset: r.get(2)?, stored_len: r.get(3)?, codec: r.get(4)? })
                })?.collect::<std::result::Result<_, _>>()?;
            if locations.is_empty() {
                self.db.execute("DELETE FROM packs WHERE id=?1", [&old])?;
                continue;
            }
            let live_size = 8 + locations.iter().map(|l| 49 + l.stored_len).sum::<u64>();
            if live_size == old_size {
                continue;
            }
            let mut writer = pack::Writer::new(&self.root)?;
            for loc in locations {
                writer.add(loc.hash, &pack::read(&self.root, &loc)?)?;
            }
            let (new, size, locations) = writer.finish(&self.root)?;
            let tx = self.db.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO packs VALUES(?1,?2)",
                params![new, size],
            )?;
            for loc in locations {
                tx.execute(
                    "UPDATE chunks SET pack=?1,offset=?2,stored_len=?3,codec=?4 WHERE hash=?5",
                    params![
                        loc.pack,
                        loc.offset,
                        loc.stored_len,
                        loc.codec,
                        loc.hash.as_slice()
                    ],
                )?;
            }
            tx.execute("DELETE FROM packs WHERE id=?1", [&old])?;
            tx.commit()?;
            #[cfg(test)]
            crate::crash_at("gc-catalog-committed");
            rewritten_packs += 1;
        }
        // Includes old packs from interrupted collection and orphan publication.
        let roots: HashSet<String> = self
            .db
            .prepare("SELECT id FROM packs")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for entry in fs::read_dir(self.root.join("packs"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_file()
                && name.len() == 64
                && name
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && !roots.contains(&name)
            {
                fs::remove_file(entry.path())?;
            }
        }
        sync_directory(&self.root.join("packs"))?;
        for entry in fs::read_dir(self.root.join("tmp"))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::remove_file(entry.path())?;
            }
        }
        sync_directory(&self.root.join("tmp"))?;
        self.db.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
        )?;
        let stats = self.check()?;
        Ok(GcReport {
            removed_chunks,
            rewritten_packs,
            removed_pack_bytes: (before.referenced_pack_bytes + before.orphan_pack_bytes)
                .saturating_sub(stats.referenced_pack_bytes + stats.orphan_pack_bytes),
            stats,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn crash_collection_child() -> Result<()> {
        if let Some(root) = std::env::var_os("SL7_GC_TEST_ROOT") {
            Store::open(Path::new(&root))?.compact()?;
        }
        Ok(())
    }

    #[test]
    fn process_death_after_pack_relocation_keeps_live_data() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("store");
        let source = temp.path().join("source");
        // Distinct deterministic bytes, with a shared prefix spanning chunks.
        let mut state = 7_u64;
        let bytes: Vec<u8> = (0..500_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let mut store = Store::init(&root)?;
        fs::write(&source, &bytes)?;
        store.put("test", "removed", &source)?;
        fs::write(&source, &bytes[..200_000])?;
        store.put("test", "kept", &source)?;
        store.remove("test", "removed")?;
        drop(store);
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "gc::tests::crash_collection_child"])
            .env("SL7_GC_TEST_ROOT", &root)
            .env("SL7_TEST_CRASH_STAGE", "gc-catalog-committed")
            .stdout(std::process::Stdio::null())
            .status()?;
        assert_eq!(status.code(), Some(73));
        let mut store = Store::open(&root)?;
        assert!(store.check()?.orphan_pack_bytes > 0);
        assert_eq!(store.read("test", "kept", 0, 200_000)?, bytes[..200_000]);
        assert_eq!(store.compact()?.stats.orphan_pack_bytes, 0);
        assert_eq!(store.read("test", "kept", 0, 200_000)?, bytes[..200_000]);
        Ok(())
    }
}
