fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let sqlite = rusqlite::Connection::open(temp.path().join("sqlite.db"))?;
    let version: String = sqlite.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
    let cache: i64 = sqlite.query_row("PRAGMA cache_size", [], |r| r.get(0))?;
    let page: i64 = sqlite.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    println!(
        "{}",
        serde_json::json!({"engine":"sqlite","version":version,"cache_size":cache,"page_size":page})
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    runtime.block_on(async {
        let db = turso::Builder::new_local(temp.path().join("turso.db").to_str().unwrap()).build().await?;
        let conn = db.connect()?;
        let mut rows = conn.query("PRAGMA cache_size", ()).await?;
        let cache: i64 = rows.next().await?.unwrap().get(0)?;
        drop(rows);
        let mut rows = conn.query("PRAGMA page_size", ()).await?;
        let page: i64 = rows.next().await?.unwrap().get(0)?;
        println!("{}", serde_json::json!({"engine":"turso","crate_version":"0.8.3","cache_size":cache,"page_size":page}));
        Ok::<_, Box<dyn std::error::Error>>(())
    })
}
