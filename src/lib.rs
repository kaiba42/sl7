//! Shared artifact storage, exact range reconstruction, and optional FUSE mounts.
mod chunk;
mod gc;
pub use gc::GcReport;
#[cfg(all(feature = "fuse", target_os = "linux"))]
pub mod live;
mod pack;
pub mod probe;
mod read_cache;
#[cfg(feature = "fuse")]
pub mod readonly;
#[cfg(all(feature = "fuse", target_os = "linux"))]
pub mod staging;
mod store;
pub use store::{FileInfo, ImportReport, Stats, Store};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog failed: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("store is already in use: {0}")]
    Locked(#[source] std::io::Error),
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("integrity check failed: {0}")]
    Corrupt(String),
    #[error("artifact not found: {view}/{name}")]
    NotFound { view: String, name: String },
    #[error("artifact already exists: {view}/{name}; choose a new name")]
    Exists { view: String, name: String },
    #[error("source changed while importing: {0}")]
    SourceChanged(String),
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn sync_directory(path: &std::path::Path) -> Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Process-death injection exists only in test binaries, never in the CLI/library build.
#[cfg(test)]
pub(crate) fn crash_at(stage: &str) {
    if std::env::var("SL7_TEST_CRASH_STAGE").as_deref() == Ok(stage) {
        std::process::exit(73);
    }
}
