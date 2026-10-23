use crate::{
    Error, Result,
    chunk::{self, Hash},
    hex, sync_directory,
};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::fs::FileExt,
    path::Path,
};
use tempfile::NamedTempFile;
const MAGIC: &[u8; 8] = b"SL7PACK1";
const HEADER: usize = 49;
pub const LIMIT: u64 = 64 * 1024 * 1024;
#[derive(Clone)]
pub struct Location {
    pub hash: Hash,
    pub raw_len: u64,
    pub pack: String,
    pub offset: u64,
    pub stored_len: u64,
    pub codec: u8,
}
pub struct Writer {
    temp: NamedTempFile,
    digest: Sha256,
    pub size: u64,
    locations: Vec<Location>,
}
impl Writer {
    pub fn new(root: &Path) -> Result<Self> {
        let temp = NamedTempFile::new_in(root.join("tmp"))?;
        let mut this = Self {
            temp,
            digest: Sha256::new(),
            size: 0,
            locations: vec![],
        };
        this.append(MAGIC)?;
        Ok(this)
    }
    fn append(&mut self, bytes: &[u8]) -> Result<()> {
        self.temp.write_all(bytes)?;
        self.digest.update(bytes);
        self.size += bytes.len() as u64;
        Ok(())
    }
    pub fn add(&mut self, hash: Hash, raw: &[u8]) -> Result<()> {
        let compressed = zstd::bulk::compress(raw, 1)?;
        let (codec, stored) = if compressed.len() < raw.len() {
            (1, compressed.as_slice())
        } else {
            (0, raw)
        };
        let loc = Location {
            hash,
            raw_len: raw.len() as u64,
            pack: String::new(),
            offset: self.size,
            stored_len: stored.len() as u64,
            codec,
        };
        self.append(&hash)?;
        self.append(&loc.raw_len.to_le_bytes())?;
        self.append(&loc.stored_len.to_le_bytes())?;
        self.append(&[codec])?;
        self.append(stored)?;
        self.locations.push(loc);
        Ok(())
    }
    pub fn finish(mut self, root: &Path) -> Result<(String, u64, Vec<Location>)> {
        self.temp.as_file().sync_all()?;
        #[cfg(test)]
        crate::crash_at("pack-synced");
        let id = hex(&self.digest.finalize());
        let destination = root.join("packs").join(&id);
        match self.temp.persist_noclobber(&destination) {
            Ok(_) => (),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_pack(root, &id)?
            }
            Err(e) => return Err(e.error.into()),
        }
        sync_directory(&root.join("packs"))?;
        #[cfg(test)]
        crate::crash_at("pack-published");
        for loc in &mut self.locations {
            loc.pack.clone_from(&id);
        }
        Ok((id, self.size, self.locations))
    }
}
fn pack_path(root: &Path, id: &str) -> Result<std::path::PathBuf> {
    if id.len() != 64 {
        return Err(Error::Corrupt(format!(
            "pack identifier length {}, expected 64",
            id.len()
        )));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::Corrupt(format!(
            "pack identifier is not lowercase hexadecimal: {id:?}"
        )));
    }
    Ok(root.join("packs").join(id))
}
pub fn verify_pack(root: &Path, id: &str) -> Result<()> {
    let mut file = File::open(pack_path(root, id)?)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    let actual = hex(&digest.finalize());
    if actual != id {
        return Err(Error::Corrupt(format!(
            "pack checksum: expected {id}, got {actual}"
        )));
    }
    Ok(())
}
fn require_equal(field: &str, expected: u64, actual: u64) -> Result<()> {
    if expected != actual {
        return Err(Error::Corrupt(format!(
            "{field}: expected {expected}, got {actual}"
        )));
    }
    Ok(())
}
pub fn read(root: &Path, loc: &Location) -> Result<Vec<u8>> {
    read_from(&open(root, &loc.pack)?, loc)
}
pub(crate) fn open(root: &Path, id: &str) -> Result<File> {
    let file = File::open(pack_path(root, id)?)?;
    let mut magic = [0_u8; 8];
    file.read_exact_at(&mut magic, 0)?;
    if &magic != MAGIC {
        return Err(Error::Corrupt(format!("pack magic: {magic:?}")));
    }
    Ok(file)
}
pub(crate) fn read_from(file: &File, loc: &Location) -> Result<Vec<u8>> {
    if !(1..=chunk::MAX as u64).contains(&loc.raw_len) {
        return Err(Error::Corrupt(format!(
            "decoded length out of bounds: {}",
            loc.raw_len
        )));
    }
    if !(1..=chunk::MAX as u64).contains(&loc.stored_len) {
        return Err(Error::Corrupt(format!(
            "encoded length out of bounds: {}",
            loc.stored_len
        )));
    }
    if loc.offset < 8 {
        return Err(Error::Corrupt(format!(
            "record offset overlaps magic: {}",
            loc.offset
        )));
    }
    let mut header = [0_u8; HEADER];
    file.read_exact_at(&mut header, loc.offset)?;
    if header[..32] != loc.hash {
        return Err(Error::Corrupt(format!(
            "record identity: expected {}, got {}",
            hex(&loc.hash),
            hex(&header[..32])
        )));
    }
    require_equal(
        "record decoded length",
        loc.raw_len,
        u64::from_le_bytes(header[32..40].try_into().unwrap()),
    )?;
    require_equal(
        "record encoded length",
        loc.stored_len,
        u64::from_le_bytes(header[40..48].try_into().unwrap()),
    )?;
    require_equal("record codec", loc.codec as u64, header[48] as u64)?;
    let mut stored = vec![0; loc.stored_len as usize];
    let data_offset = loc
        .offset
        .checked_add(HEADER as u64)
        .ok_or_else(|| Error::Corrupt("record offset overflow".into()))?;
    file.read_exact_at(&mut stored, data_offset)?;
    let decoded = match loc.codec {
        0 => stored,
        1 => zstd::bulk::decompress(&stored, loc.raw_len as usize)?,
        codec => return Err(Error::Corrupt(format!("unknown codec: {codec}"))),
    };
    require_equal("decoded length", loc.raw_len, decoded.len() as u64)?;
    let actual = chunk::identity(&decoded);
    if actual != loc.hash {
        return Err(Error::Corrupt(format!(
            "decoded checksum: expected {}, got {}",
            hex(&loc.hash),
            hex(&actual)
        )));
    }
    Ok(decoded)
}
