mod original;
use std::{fs::File, io::{self, Read, BufReader}, hint::black_box, time::Instant};
use original::{MAX, TARGET};

struct Sliding<R> {
    reader: R,
    bytes: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    split: bool,
    gear: [u64; 256],
}
impl<R: Read> Sliding<R> {
    fn new(reader: R, capacity: usize, split: bool) -> Self {
        let mut seed=0x0123_4567_89ab_cdef_u64;
        let gear=std::array::from_fn(|_| {
            seed=seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z=seed;
            z=(z^(z>>30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z=(z^(z>>27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z^(z>>31)
        });
        Self {reader, bytes:vec![0;capacity], start:0, end:0, eof:false, split, gear}
    }
    fn next(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.end-self.start<MAX && !self.eof {
            self.bytes.copy_within(self.start..self.end, 0);
            self.end-=self.start;
            self.start=0;
            while self.end<MAX && !self.eof {
                match self.reader.read(&mut self.bytes[self.end..]) {
                    Ok(n) => { self.end+=n; self.eof=n==0; }
                    Err(e) if e.kind()==io::ErrorKind::Interrupted => (),
                    Err(e) => return Err(e),
                }
            }
        }
        let bytes=&self.bytes[self.start..self.end.min(self.start+MAX)];
        if bytes.is_empty() { return Ok(None); }
        let mut cut=bytes.len();
        let mut hash=0u64;
        if self.split {
            for (i,&byte) in bytes.iter().enumerate().take(TARGET).skip(TARGET/4) {
                hash=hash.wrapping_shl(1).wrapping_add(self.gear[byte as usize]);
                if hash & (TARGET*2-1) as u64==0 {cut=i+1;break;}
            }
            if cut==bytes.len() && bytes.len()>TARGET {
                for (i,&byte) in bytes.iter().enumerate().skip(TARGET) {
                    hash=hash.wrapping_shl(1).wrapping_add(self.gear[byte as usize]);
                    if hash & (TARGET/2-1) as u64==0 {cut=i+1;break;}
                }
            }
        } else {
            for (i,&byte) in bytes.iter().enumerate().skip(TARGET/4) {
                hash=hash.wrapping_shl(1).wrapping_add(self.gear[byte as usize]);
                let mask=if i<TARGET {TARGET*2-1} else {TARGET/2-1};
                if hash & mask as u64==0 {cut=i+1;break;}
            }
        }
        let result=bytes[..cut].to_vec();
        self.start+=cut;
        Ok(Some(result))
    }
}

fn scan(path:&str, mode:usize, verify:bool) -> io::Result<(u64,usize,Vec<(usize,[u8;32])>)> {
    let reader=BufReader::new(File::open(path)?);
    let mut old=original::Chunker::new(reader);
    let mut new=Sliding::new(BufReader::new(File::open(path)?), if mode==1 {MAX} else {MAX*4}, mode==3);
    let mut total=0u64;
    let mut count=0;
    let mut identities=Vec::new();
    loop {
        let next=if mode==0 {old.next_chunk()?} else {new.next()?};
        let Some(bytes)=next else {break;};
        total+=bytes.len() as u64;
        count+=1;
        if verify {identities.push((bytes.len(),original::identity(&bytes)));}
        black_box(&bytes);
    }
    Ok((total,count,identities))
}

fn main() -> io::Result<()> {
    let paths:Vec<_>=std::env::args().skip(1).collect();
    assert!(!paths.is_empty());
    let mut rows=Vec::new();
    for path in paths {
        let expected=scan(&path,0,true)?;
        for mode in 1..4 {assert_eq!(scan(&path,mode,true)?,expected,"mode {mode}");}
        let passes=(8*186_672_416u64/expected.0.max(1)).clamp(8,4096);
        for round in 0..6 {
            let modes:Vec<_>=if round%2==0 {(0..4).collect()} else {(0..4).rev().collect()};
            for mode in modes {
                let started=Instant::now();
                for _ in 0..passes {
                    let (bytes,chunks,_)=scan(&path,mode,false)?;
                    assert_eq!((bytes,chunks),(expected.0,expected.1));
                }
                let seconds=started.elapsed().as_secs_f64()/passes as f64;
                rows.push(serde_json::json!({"fixture":std::path::Path::new(&path).file_name().unwrap().to_str().unwrap(),"mode":mode,"round":round+1,"bytes":expected.0,"chunks":expected.1,"seconds_per_scan":seconds,"passes":passes}));
            }
        }
    }
    println!("{}",serde_json::to_string_pretty(&rows).unwrap());
    Ok(())
}
