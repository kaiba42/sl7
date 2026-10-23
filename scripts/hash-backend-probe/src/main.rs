use sha2::{Digest, Sha256};
use std::{hint::black_box, time::Instant};
fn main() {
    let chunk = (0..8192)
        .map(|i| (i * 131 + i / 17) as u8)
        .collect::<Vec<_>>();
    let iterations = 131072u64;
    let mut digest = [0u8; 32];
    let mut checksum = 0u64;
    let start = Instant::now();
    for _ in 0..iterations {
        let data = black_box(chunk.as_slice());
        let mut h = Sha256::new();
        h.update(b"sl7.chunk.v1\0");
        h.update((data.len() as u64).to_le_bytes());
        h.update(data);
        digest = black_box(h.finalize().into());
        checksum = checksum.wrapping_add(u64::from_le_bytes(digest[..8].try_into().unwrap()));
    }
    let elapsed = start.elapsed().as_secs_f64();
    let digest = digest
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let known = format!("{:x}", Sha256::digest(b"abc"));
    assert_eq!(
        known,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    println!(
        "{{\"hardware_feature\":{},\"bytes\":{},\"chunk_bytes\":8192,\"seconds\":{},\"mib_per_second\":{},\"digest\":\"{}\",\"checksum\":{}}}",
        cfg!(feature = "hardware"),
        iterations * 8192,
        elapsed,
        1024.0 / elapsed,
        digest,
        checksum
    );
}
