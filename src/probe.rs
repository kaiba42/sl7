//! Run the same filesystem behavior probe on a native directory or a future mount.
//! This first probe does not certify proc-macro loading or power-loss durability.
use crate::{Error, Result};
use fs2::FileExt;
use serde::Serialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Debug, Serialize)]
pub struct Report {
    pub directory: PathBuf,
    pub checks: Vec<&'static str>,
}
fn verify(name: &str, expected: &[u8], actual: &[u8]) -> Result<()> {
    if expected != actual {
        return Err(Error::Corrupt(format!(
            "probe {name}: expected {expected:?}, got {actual:?}"
        )));
    }
    Ok(())
}
pub fn run(directory: &Path) -> Result<Report> {
    let temp = tempfile::Builder::new()
        .prefix("sl7-probe-")
        .tempdir_in(directory)?;
    let root = temp.path();
    let a = root.join("a");
    let linked = root.join("linked");
    let mut checks = Vec::new();
    let mut file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&a)?;
    file.set_len(4096)?;
    file.write_all(b"original")?;
    file.sync_all()?;
    fs::hard_link(&a, &linked)?;
    verify("hard link", b"original", &fs::read(&linked)?[..8])?;
    checks.push("hard_link");
    // SAFETY: this probe owns the temporary file. Mappings never outlive the
    // backing inode, no truncation occurs while mapped, and no other writer runs.
    let mapped = unsafe { memmap2::MmapOptions::new().map(&file)? };
    verify("read mapping", b"original", &mapped[..8])?;
    checks.push("read_mmap");
    file.try_lock_exclusive()?;
    let other = File::open(&linked)?;
    match other.try_lock_exclusive() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
        Err(e) => return Err(Error::Io(e)),
        Ok(()) => {
            return Err(Error::Corrupt(
                "probe lock contention: second descriptor acquired an exclusive lock".into(),
            ));
        }
    }
    FileExt::unlock(&file)?;
    checks.push("lock_contention");
    let replacement = root.join("replacement");
    fs::write(&replacement, b"replacement")?;
    fs::rename(&replacement, &a)?;
    verify("renamed path", b"replacement", &fs::read(&a)?)?;
    verify("old mapping after rename", b"original", &mapped[..8])?;
    checks.push("rename_over_open");
    fs::remove_file(&linked)?;
    drop(other);
    verify("unlinked mapping", b"original", &mapped[..8])?;
    checks.push("unlink_open_mapping");
    drop(mapped);
    // SAFETY: exclusive control of this inode; no competing mappings or writers.
    let mut writable = unsafe { memmap2::MmapOptions::new().map_mut(&file)? };
    writable[..8].copy_from_slice(b"modified");
    writable.flush()?;
    drop(writable);
    file.seek(SeekFrom::Start(0))?;
    let mut buffer = [0; 8];
    file.read_exact(&mut buffer)?;
    verify("shared writable mapping", b"modified", &buffer)?;
    checks.push("shared_write_mmap");
    file.set_len(2)?;
    file.set_len(4096)?;
    file.seek(SeekFrom::Start(2))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    verify("truncate and extend zeroes", &vec![0; 4094], &tail)?;
    checks.push("truncate_extend");
    file.sync_all()?;
    crate::sync_directory(root)?;
    checks.push("fsync_and_directory_sync");
    let executable = root.join("sl7-exec-probe");
    fs::copy(std::env::current_exe()?, &executable)?;
    let output = Command::new(&executable).arg("--version").output()?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "executable probe exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    checks.push("execute_native_binary");
    Ok(Report {
        directory: directory.to_path_buf(),
        checks,
    })
}
