use sl7::{Error, Store};
use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    process::Command,
};

fn data(n: usize) -> Vec<u8> {
    let mut state = 0x123456789abcdef_u64;
    (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn deduplicates_across_views_and_survives_reopen() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    let bytes = data(500_013);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&root)?;
    let first = store.put("main", "lib.rmeta", &source)?;
    let second = store.put("branch", "different-name", &source)?;
    assert!(first.new_chunks > 1);
    assert_eq!(second.new_chunks, 0);
    assert_eq!(second.new_pack_bytes, 0);
    assert_eq!(store.stats()?.logical_bytes, bytes.len() as u64 * 2);
    drop(store);
    let store = Store::open(&root)?;
    assert_eq!(
        store.read("branch", "different-name", 0, bytes.len())?,
        bytes
    );
    assert_eq!(store.check()?.files, 2);
    Ok(())
}

#[test]
fn insertion_shares_most_content_but_preserves_changed_bytes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("input");
    let mut bytes = data(2_000_000);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&temp.path().join("store"))?;
    store.put("main", "v1", &source)?;
    bytes.insert(123_456, 77);
    fs::write(&source, &bytes)?;
    let changed = store.put("main", "v2", &source)?;
    assert!(
        changed.new_pack_bytes < 200_000,
        "{} new bytes",
        changed.new_pack_bytes
    );
    assert_eq!(store.read("main", "v2", 0, bytes.len())?, bytes);
    store.check()?;
    Ok(())
}

#[test]
fn random_ranges_cross_chunks_and_eof() -> TestResult {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("input");
    let bytes = data(300_017);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&temp.path().join("store"))?;
    store.put("a", "b", &source)?;
    for n in 0..300_usize {
        let offset = (n * 977) % (bytes.len() + 1);
        let length = [0, 1, 4096, 65536][n % 4];
        assert_eq!(
            store.read("a", "b", offset as u64, length)?,
            &bytes[offset..(offset + length).min(bytes.len())]
        );
    }
    assert!(store.read("a", "b", u64::MAX, 4096)?.is_empty());
    assert!(matches!(
        store.read("a", "b", 0, 16 * 1024 * 1024 + 1),
        Err(Error::Invalid(_))
    ));
    Ok(())
}

#[test]
fn empty_and_repeated_files_roundtrip_without_padding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("input");
    let mut store = Store::init(&temp.path().join("store"))?;
    fs::write(&source, [])?;
    let empty = store.put("a", "empty", &source)?;
    assert_eq!(empty.new_pack_bytes, 0);
    assert!(store.read("a", "empty", 0, 100)?.is_empty());
    let zeros = vec![0; 250_001];
    fs::write(&source, &zeros)?;
    store.put("a", "zeros", &source)?;
    let output = temp.path().join("output");
    store.export("a", "zeros", &output)?;
    assert_eq!(fs::read(output)?, zeros);
    store.check()?;
    Ok(())
}

#[test]
fn export_and_import_never_overwrite_existing_entries() -> TestResult {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("input");
    fs::write(&source, b"first")?;
    let mut store = Store::init(&temp.path().join("store"))?;
    store.put("a", "b", &source)?;
    fs::write(&source, b"second")?;
    assert!(matches!(
        store.put("a", "b", &source),
        Err(Error::Exists { .. })
    ));
    assert!(store.export("a", "b", &source).is_err());
    assert_eq!(fs::read(source)?, b"second");
    assert_eq!(store.read("a", "b", 0, 100)?, b"first");
    Ok(())
}

#[test]
fn owner_lock_prevents_a_second_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let store = Store::init(&root)?;
    assert!(matches!(Store::open(&root), Err(Error::Locked(_))));
    drop(store);
    Store::open(&root)?;
    Ok(())
}

#[test]
fn integrity_check_bypasses_warm_chunk_and_descriptor_caches() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    let bytes = data(100_000);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&root)?;
    store.put("a", "b", &source)?;
    assert_eq!(store.read("a", "b", 0, bytes.len())?, bytes);
    let pack = fs::read_dir(root.join("packs"))?.next().unwrap()?.path();
    fs::OpenOptions::new().write(true).open(pack)?.set_len(8)?;
    assert!(store.check().is_err());
    // A failed check also invalidates warmed entries; later reads must fail.
    assert!(store.read("a", "b", 0, bytes.len()).is_err());
    Ok(())
}

#[test]
fn corrupted_payload_never_returns_success() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    let bytes = data(100_000);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&root)?;
    store.put("a", "b", &source)?;
    let entry = fs::read_dir(root.join("packs"))?
        .next()
        .ok_or("missing pack")??;
    let mut pack = fs::OpenOptions::new().write(true).open(entry.path())?;
    pack.seek(SeekFrom::End(-1))?;
    pack.write_all(&[bytes[bytes.len() - 1] ^ 0xff])?;
    pack.sync_all()?;
    assert!(store.read("a", "b", 99_999, 1).is_err());
    assert!(store.check().is_err());
    let destination = temp.path().join("bad-export");
    assert!(store.export("a", "b", &destination).is_err());
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn truncated_pack_never_zero_fills_missing_bytes() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    fs::write(&source, b"persistent bytes")?;
    let mut store = Store::init(&root)?;
    store.put("a", "b", &source)?;
    let path = fs::read_dir(root.join("packs"))?
        .next()
        .ok_or("missing pack")??
        .path();
    fs::OpenOptions::new().write(true).open(path)?.set_len(8)?;
    assert!(store.read("a", "b", 0, 1).is_err());
    Ok(())
}

#[test]
fn missing_manifest_extent_is_an_error() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    fs::write(&source, data(100_000))?;
    let mut store = Store::init(&root)?;
    store.put("a", "b", &source)?;
    drop(store);
    let db = rusqlite::Connection::open(root.join("catalog.sqlite"))?;
    db.execute("DELETE FROM extents WHERE offset=0", [])?;
    drop(db);
    let store = Store::open(&root)?;
    assert!(matches!(
        store.read("a", "b", 0, 100),
        Err(Error::Corrupt(_))
    ));
    assert!(store.check().is_err());
    Ok(())
}

#[test]
fn unsupported_format_is_rejected_without_reinitialization() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    drop(Store::init(&root)?);
    let db = rusqlite::Connection::open(root.join("catalog.sqlite"))?;
    db.pragma_update(None, "user_version", 99)?;
    drop(db);
    assert!(matches!(Store::open(&root), Err(Error::Invalid(_))));
    assert!(Store::init(&root).is_err());
    Ok(())
}

#[test]
fn integrity_check_rejects_extents_past_eof() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    fs::write(&source, b"abc")?;
    let mut store = Store::init(&root)?;
    store.put("a", "b", &source)?;
    drop(store);
    let db = rusqlite::Connection::open(root.join("catalog.sqlite"))?;
    db.execute("INSERT INTO extents SELECT file_id,3,hash FROM extents", [])?;
    drop(db);
    let store = Store::open(&root)?;
    assert_eq!(store.read("a", "b", 0, 3)?, b"abc");
    assert!(matches!(store.check(), Err(Error::Corrupt(_))));
    Ok(())
}

#[test]
fn cli_put_read_export_and_probe() -> TestResult {
    let temp = tempfile::tempdir()?;
    let store = temp.path().join("store");
    let source = temp.path().join("input");
    fs::write(&source, b"test byte-range output")?;
    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_sl7"))
            .arg("--store")
            .arg(&store)
            .args(args)
            .output()
    };
    assert!(run(&["init".as_ref()])?.status.success());
    assert!(
        run(&[
            "put".as_ref(),
            "main".as_ref(),
            "a".as_ref(),
            source.as_os_str()
        ])?
        .status
        .success()
    );
    let read = run(&[
        "read".as_ref(),
        "main".as_ref(),
        "a".as_ref(),
        "--offset".as_ref(),
        "5".as_ref(),
        "--length".as_ref(),
        "4".as_ref(),
    ])?;
    assert!(read.status.success());
    assert_eq!(read.stdout, b"byte");
    let probe = run(&["probe".as_ref(), temp.path().as_os_str()])?;
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
    assert!(report["checks"].as_array().ok_or("missing checks")?.len() >= 8);
    Ok(())
}
#[test]
fn collection_rewrites_partially_live_packs_and_reclaims_cleaned_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let mut seed = 7_u64;
    let bytes: Vec<u8> = (0..500_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        })
        .collect();
    std::fs::write(&a, &bytes).unwrap();
    std::fs::write(&b, &bytes[..200_000]).unwrap();
    let mut store = sl7::Store::init(&root).unwrap();
    store.put("a", "file", &a).unwrap();
    store.put("b", "file", &b).unwrap();
    store.remove("a", "file").unwrap();
    let report = store.compact().unwrap();
    assert!(report.rewritten_packs > 0);
    assert!(report.removed_pack_bytes > 200_000);
    assert_eq!(
        store.read("b", "file", 0, 200_000).unwrap(),
        bytes[..200_000]
    );
    store.remove("b", "file").unwrap();
    let report = store.compact().unwrap();
    assert_eq!(report.stats.unique_chunks, 0);
    assert_eq!(report.stats.referenced_pack_bytes, 0);
    assert_eq!(report.stats.orphan_pack_bytes, 0);
}

#[test]
fn cached_maps_cross_windows_survive_id_reuse_and_gc() -> TestResult {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("input");
    let root = temp.path().join("store");
    let bytes = data(2 * 1024 * 1024 + 17);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&root)?;
    let old = store.put("view", "file", &source)?.file;
    for offset in [0, 262143, 262144, 524280, 1048569, bytes.len() - 3] {
        for _ in 0..2 {
            assert_eq!(
                store.read("view", "file", offset as u64, 600_000)?,
                bytes[offset..(offset + 600_000).min(bytes.len())]
            );
        }
    }
    store.remove("view", "file")?;
    let mut replacement = bytes.clone();
    replacement[262143..262150].copy_from_slice(b"changed");
    fs::write(&source, &replacement)?;
    let new = store.put("view", "file", &source)?.file;
    assert_eq!(old.id, new.id, "test must exercise SQLite row ID reuse");
    assert_eq!(
        store.read("view", "file", 0, replacement.len())?,
        replacement
    );
    // The old file contributes dead chunks to shared packs. Warm maps must not
    // retain locations into packs removed or rewritten during compaction.
    store.compact()?;
    assert_eq!(
        store.read("view", "file", 0, replacement.len())?,
        replacement
    );
    Ok(())
}

#[test]
fn integrity_check_cannot_be_satisfied_by_a_cached_extent_map() -> TestResult {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("store");
    let source = temp.path().join("input");
    let bytes = data(700_000);
    fs::write(&source, &bytes)?;
    let mut store = Store::init(&root)?;
    let info = store.put("a", "b", &source)?.file;
    assert_eq!(store.read("a", "b", 0, bytes.len())?, bytes);
    let db = rusqlite::Connection::open(root.join("catalog.sqlite"))?;
    db.execute(
        "DELETE FROM extents WHERE file_id=?1 AND offset=0",
        [info.id],
    )?;
    assert!(store.check().is_err());
    assert!(store.read("a", "b", 0, 4096).is_err());
    Ok(())
}
