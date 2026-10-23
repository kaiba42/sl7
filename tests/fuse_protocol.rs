//! Exercise fuser's real decoder/dispatcher/reply encoder over a local datagram
//! pair. This does not emulate kernel caching or certify an operating-system mount.
#![cfg(feature = "fuse")]

use sl7::{Store, readonly::ReadOnlyFs};
use std::{fs, os::unix::net::UnixDatagram, thread, time::Duration};

struct Session {
    socket: UnixDatagram,
    worker: Option<thread::JoinHandle<std::io::Result<()>>>,
    sequence: u64,
}

impl Session {
    fn new(fs: ReadOnlyFs) -> Self {
        let (socket, server) = UnixDatagram::pair().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let worker = thread::spawn(move || {
            fuser::Session::from_fd(
                fs,
                server.into(),
                fuser::SessionACL::default(),
                fuser::Config::default(),
            )?
            .run()
        });
        let mut session = Self {
            socket,
            worker: Some(worker),
            sequence: 0,
        };
        // fuse_init_in, protocol 7.31, no optional capabilities requested.
        let mut init = vec![0; 64];
        init[..4].copy_from_slice(&7u32.to_ne_bytes());
        init[4..8].copy_from_slice(&31u32.to_ne_bytes());
        let (errno, reply) = session.request(26, 0, &init);
        assert_eq!(errno, 0);
        assert_eq!(u32_at(&reply, 0), 7);
        session
    }

    fn request(&mut self, opcode: u32, ino: u64, body: &[u8]) -> (i32, Vec<u8>) {
        self.sequence += 1;
        // fuse_in_header: native-endian ABI, 40 bytes.
        let mut packet = Vec::new();
        packet.extend_from_slice(&(40u32 + body.len() as u32).to_ne_bytes());
        packet.extend_from_slice(&opcode.to_ne_bytes());
        packet.extend_from_slice(&self.sequence.to_ne_bytes());
        packet.extend_from_slice(&ino.to_ne_bytes());
        // libc's identity accessors have no memory-safety preconditions.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        packet.extend_from_slice(&uid.to_ne_bytes());
        packet.extend_from_slice(&gid.to_ne_bytes());
        packet.extend_from_slice(&std::process::id().to_ne_bytes());
        packet.extend_from_slice(&0u32.to_ne_bytes());
        packet.extend_from_slice(body);
        assert_eq!(self.socket.send(&packet).unwrap(), packet.len());
        let mut reply = vec![0; 128 * 1024];
        let length = self.socket.recv(&mut reply).unwrap();
        reply.truncate(length);
        assert_eq!(u32_at(&reply, 0) as usize, length);
        assert_eq!(u64_at(&reply, 8), self.sequence);
        let errno = i32::from_ne_bytes(reply[4..8].try_into().unwrap());
        (errno, reply[16..].to_vec())
    }

    fn lookup(&mut self, parent: u64, name: &str) -> u64 {
        let mut name = name.as_bytes().to_vec();
        name.push(0);
        let (errno, reply) = self.request(1, parent, &name);
        assert_eq!(errno, 0);
        u64_at(&reply, 0)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            // A malformed empty frame terminates the event loop even if a test
            // assertion unwinds. No detached worker may retain the store lock.
            let _ = self.socket.send(&[]);
            let _ = worker.join();
        }
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_ne_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
fn read_body(offset: u64, size: u32) -> Vec<u8> {
    let mut body = vec![0; 40];
    body[8..16].copy_from_slice(&offset.to_ne_bytes());
    body[16..20].copy_from_slice(&size.to_ne_bytes());
    body
}

#[test]
fn fuse_dispatch_reconstructs_reads_and_resumes_full_directory_replies() {
    let dir = tempfile::tempdir().unwrap();
    let bytes: Vec<u8> = (0..100_000)
        .map(|n| ((n * 31 + n / 997) % 251) as u8)
        .collect();
    let source = dir.path().join("source");
    fs::write(&source, &bytes).unwrap();
    let root = dir.path().join("store");
    let mut store = Store::init(&root).unwrap();
    store.put("main", "nested/artifact", &source).unwrap();
    store.put("branch", "artifact", &source).unwrap();
    let mut session = Session::new(ReadOnlyFs::new(store, 0, 0).unwrap());
    let main = session.lookup(1, "main");
    let nested = session.lookup(main, "nested");
    let file = session.lookup(nested, "artifact");
    assert_eq!(session.lookup(nested, ".."), main);
    assert_eq!(session.request(1, 1, b"absent\0").0, -libc::ENOENT);

    let mut flags = vec![0; 8];
    assert_eq!(session.request(14, file, &flags).0, 0); // OPEN
    flags[..4].copy_from_slice(&libc::O_RDWR.to_ne_bytes());
    assert_eq!(session.request(14, file, &flags).0, -libc::EROFS);
    // Keep each datagram below macOS's default local socket message limit;
    // larger reconstruction requests are covered by the adapter/store tests.
    for (offset, size) in [(0, 1024), (32_760, 1024), (99_999, 1024), (100_000, 1024)] {
        let (errno, reply) = session.request(15, file, &read_body(offset, size));
        assert_eq!(errno, 0);
        assert_eq!(
            reply,
            bytes[offset as usize..(offset as usize + size as usize).min(bytes.len())]
        );
    }
    assert_eq!(session.request(15, 1, &read_body(0, 1)).0, -libc::EISDIR);

    // 32 bytes fits exactly one dirent here. Exercise buffer-full replies,
    // resume cookies, and EOF through the actual FUSE serialization path.
    let mut names = Vec::new();
    let mut cookie = 0;
    loop {
        let (errno, reply) = session.request(28, 1, &read_body(cookie, 32));
        assert_eq!(errno, 0);
        if reply.is_empty() {
            break;
        }
        assert_eq!(reply.len(), 32);
        let next = u64_at(&reply, 8);
        assert!(next > cookie);
        cookie = next;
        let length = u32_at(&reply, 16) as usize;
        names.push(String::from_utf8(reply[24..24 + length].to_vec()).unwrap());
        assert!(names.len() <= 4);
    }
    assert_eq!(names, [".", "..", "branch", "main"]);

    for entry in fs::read_dir(root.join("packs")).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    // Warm cached content is still verified, correct data after external damage.
    assert_eq!(
        session.request(15, file, &read_body(0, 1)),
        (0, bytes[..1].to_vec())
    );
    assert_eq!(session.request(38, 0, &[]).0, 0); // DESTROY
    session.worker.take().unwrap().join().unwrap().unwrap();
    // A new session has neither decoded chunks nor pinned descriptors. Its
    // actual FUSE reply must report missing storage rather than invent bytes.
    let store = Store::open(&root).unwrap();
    let mut cold = Session::new(ReadOnlyFs::new(store, 0, 0).unwrap());
    let main = cold.lookup(1, "main");
    let nested = cold.lookup(main, "nested");
    let file = cold.lookup(nested, "artifact");
    assert_eq!(cold.request(15, file, &read_body(0, 1)).0, -libc::EIO);
    assert_eq!(cold.request(38, 0, &[]).0, 0);
    cold.worker.take().unwrap().join().unwrap().unwrap();
}
