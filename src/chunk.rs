use sha2::{Digest, Sha256};
use std::io::Read;
pub const TARGET: usize = 8192;
pub const MAX: usize = TARGET * 4;
pub type Hash = [u8; 32];
pub fn identity(data: &[u8]) -> Hash {
    let mut h = Sha256::new();
    h.update(b"sl7.chunk.v1\0");
    h.update((data.len() as u64).to_le_bytes());
    h.update(data);
    h.finalize().into()
}
/// Experimental normalized gear splitter from the study, with versioned parameters.
pub struct Chunker<R> {
    reader: R,
    pending: Vec<u8>,
    eof: bool,
    gear: [u64; 256],
}
impl<R: Read> Chunker<R> {
    pub fn new(reader: R) -> Self {
        let mut seed = 0x0123_4567_89ab_cdef_u64;
        let gear = std::array::from_fn(|_| {
            seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = seed;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        });
        Self {
            reader,
            pending: Vec::with_capacity(MAX),
            eof: false,
            gear,
        }
    }
    pub fn next_chunk(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        while self.pending.len() < MAX && !self.eof {
            let old = self.pending.len();
            self.pending.resize(MAX, 0);
            match self.reader.read(&mut self.pending[old..]) {
                Ok(n) => {
                    self.pending.truncate(old + n);
                    self.eof = n == 0;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => self.pending.truncate(old),
                Err(e) => return Err(e),
            }
        }
        if self.pending.is_empty() {
            return Ok(None);
        }
        let mut end = self.pending.len();
        let mut hash = 0_u64;
        for i in (TARGET / 4)..self.pending.len() {
            hash = hash
                .wrapping_shl(1)
                .wrapping_add(self.gear[self.pending[i] as usize]);
            let mask = if i < TARGET {
                TARGET * 2 - 1
            } else {
                TARGET / 2 - 1
            };
            if hash & mask as u64 == 0 {
                end = i + 1;
                break;
            }
        }
        let output = self.pending[..end].to_vec();
        self.pending.drain(..end);
        Ok(Some(output))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashSet, io::Cursor};
    fn bytes(n: usize) -> Vec<u8> {
        let mut x = 42_u64;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }
    fn chunks(r: impl Read) -> Vec<Vec<u8>> {
        let mut c = Chunker::new(r);
        std::iter::from_fn(|| c.next_chunk().unwrap()).collect()
    }
    struct Short<R>(R);
    impl<R: Read> Read for Short<R> {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let n = b.len().min(37);
            self.0.read(&mut b[..n])
        }
    }
    #[test]
    fn boundaries_ignore_io_fragmentation_and_reassemble() {
        let data = bytes(300_001);
        let normal = chunks(Cursor::new(&data));
        assert_eq!(normal, chunks(Short(Cursor::new(&data))));
        assert_eq!(normal.concat(), data);
        assert!(normal.iter().all(|c| c.len() <= MAX));
    }
    #[test]
    fn insertion_recovers_shared_regions() {
        let original = bytes(2_000_000);
        let mut edited = original.clone();
        edited.insert(123_456, 19);
        let hashes: HashSet<_> = chunks(Cursor::new(&original))
            .iter()
            .map(|c| identity(c))
            .collect();
        let shared: usize = chunks(Cursor::new(edited))
            .iter()
            .filter(|c| hashes.contains(&identity(c)))
            .map(Vec::len)
            .sum();
        assert!(shared > original.len() * 9 / 10, "shared {shared}");
    }
    #[test]
    fn empty_and_small() {
        assert!(chunks(Cursor::new([])).is_empty());
        assert_eq!(chunks(Cursor::new(b"abc")), vec![b"abc".to_vec()]);
    }
}
