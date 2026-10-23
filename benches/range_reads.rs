//! Focused, deterministic read-path comparison; not an end-to-end Cargo benchmark.
use sl7::Store;
use std::{fs, hint::black_box, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("input");
    let root = temp.path().join("store");
    let mut random = 0xdeadbeef12345678_u64;
    let bytes: Vec<u8> = (0..32 * 1024 * 1024)
        .map(|i| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            if i % 512 < 256 { random as u8 } else { 0 }
        })
        .collect();
    fs::write(&path, &bytes)?;
    let mut store = Store::init(&root)?;
    store.put("bench", "artifact", &path)?;
    drop(store);
    for group in [
        "warm_sequential_4k",
        "warm_random_4k",
        "warm_cross_window_8k",
        "cold_sparse_4k",
        "cold_sequential_1m",
    ] {
        let mut seconds = Vec::new();
        let count = match group {
            "cold_sparse_4k" => 128,
            "cold_sequential_1m" => 32,
            _ => 65536,
        };
        let length = match group {
            "warm_cross_window_8k" => 8192,
            "cold_sequential_1m" => 1024 * 1024,
            _ => 4096,
        };
        let repeats = match group {
            "cold_sparse_4k" => 32,
            "cold_sequential_1m" => 8,
            _ => 1,
        };
        for _ in 0..7 {
            let mut elapsed = 0.0;
            for _ in 0..repeats {
                let store = Store::open(&root)?;
                if group.starts_with("warm_") {
                    for offset in (0..bytes.len()).step_by(1024 * 1024) {
                        black_box(store.read("bench", "artifact", offset as u64, 1024 * 1024)?);
                    }
                }
                let start = Instant::now();
                for n in 0..count {
                    let offset = match group {
                        "warm_sequential_4k" => (n * 4096) % bytes.len(),
                        "warm_random_4k" => (n * 104729) % (bytes.len() - length),
                        "warm_cross_window_8k" => ((n % 127 + 1) * 262144) - 4000,
                        "cold_sparse_4k" => n * 262144 + 777,
                        "cold_sequential_1m" => n * 1024 * 1024,
                        _ => unreachable!(),
                    };
                    let read = store.read("bench", "artifact", offset as u64, length)?;
                    assert_eq!(read, bytes[offset..offset + length]);
                    black_box(read);
                }
                elapsed += start.elapsed().as_secs_f64();
            }
            seconds.push(elapsed);
        }
        let mut sorted = seconds.clone();
        sorted.sort_by(f64::total_cmp);
        println!(
            "{}",
            serde_json::json!({"group":group,"reads_per_sample":count*repeats,"bytes_per_read":length,"samples_seconds":seconds,"median_seconds":sorted[3]})
        );
    }
    Ok(())
}
