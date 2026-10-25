use std::{env, fs, path::PathBuf};
fn main() {
    // Compile the production LRU verbatim, avoiding a hand-copied baseline.
    let cache = fs::read_to_string("../../src/read_cache.rs").unwrap();
    let start = cache.find("struct Lru<K, V>").unwrap();
    let end = cache[start..]
        .find("#[derive(Clone, Default, Serialize)]")
        .unwrap()
        + start;
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(
        out.join("lru.rs"),
        format!(
            "use std::collections::{{HashMap,BTreeMap}}; use std::hash::Hash as Key;\n{}",
            &cache[start..end]
        ),
    )
    .unwrap();
    let store = fs::read_to_string("../../src/store.rs").unwrap();
    let schema = store
        .split("const SCHEMA: &str = \"")
        .nth(1)
        .unwrap()
        .split("\";")
        .next()
        .unwrap();
    fs::write(out.join("schema.sql"), schema).unwrap();
    println!("cargo:rerun-if-changed=../../src/read_cache.rs");
    println!("cargo:rerun-if-changed=../../src/store.rs");
}
