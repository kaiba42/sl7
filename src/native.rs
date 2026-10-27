//! OS-specific backing-inode operations. These operate on held descriptors,
//! never paths recovered with F_GETPATH, so rename/unlink cannot retarget them.
use std::{
    ffi::CStr,
    fs::{File, Metadata},
    io,
    os::fd::{AsRawFd, FromRawFd},
};

#[cfg(target_os = "linux")]
pub(crate) fn fd_path(file: &File) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

/// Read an attribute from the pinned inode. Only a missing attribute is None;
/// permission, unsupported-filesystem and buffer-size errors must propagate.
pub(crate) fn read_xattr(file: &File, key: &CStr, value: &mut [u8]) -> io::Result<Option<usize>> {
    // Linux's O_PATH pins cannot use fgetxattr. Following their procfs magic
    // link addresses the pinned inode even after its last name is removed.
    #[cfg(target_os = "linux")]
    let path = std::ffi::CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
    // SAFETY: file remains live; key is terminated and value has the given size.
    let n = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::getxattr(
                path.as_ptr(),
                key.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::fgetxattr(
                file.as_raw_fd(),
                key.as_ptr(),
                value.as_mut_ptr().cast(),
                value.len(),
                0,
                0,
            )
        }
    };
    if n < 0 {
        let error = io::Error::last_os_error();
        #[cfg(target_os = "linux")]
        let missing = libc::ENODATA;
        #[cfg(target_os = "macos")]
        let missing = libc::ENOATTR;
        if error.raw_os_error() == Some(missing) {
            return Ok(None);
        }
        return Err(error);
    }
    Ok(Some(n as usize))
}

/// Atomically attach a new identity; never replace one. The caller still owns
/// synchronization/durability, including syncing the inode after this succeeds.
pub(crate) fn create_xattr(file: &File, key: &CStr, value: &[u8]) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let path = std::ffi::CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
    // SAFETY: file/key/value remain live for the call, with the stated length.
    let rc = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::setxattr(
                path.as_ptr(),
                key.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                libc::XATTR_CREATE,
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::fsetxattr(
                file.as_raw_fd(),
                key.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                libc::XATTR_CREATE,
            )
        }
    };
    result(rc)
}

fn result(rc: libc::c_int) -> io::Result<()> {
    if rc == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn child_name(name: &CStr) -> io::Result<()> {
    let name = name.to_bytes();
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    Ok(())
}

/// Pin the backing root by pathname. Only the trusted mount argument is
/// resolved this way; every descendant is reached through `pin_at`.
pub(crate) fn pin_path(path: &std::path::Path) -> io::Result<(File, Metadata)> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    open_pin(libc::AT_FDCWD, &path)
}

/// Pin a single-component child relative to a held directory descriptor. The
/// child itself is never followed if it is a symlink. Dot and dot-dot are the
/// caller's responsibility: they are namespace policy, not child names.
pub(crate) fn pin_at(dir: &File, name: &CStr) -> io::Result<(File, Metadata)> {
    child_name(name)?;
    open_pin(dir.as_raw_fd(), name)
}

/// Pin the current parent of a held directory, wherever it has been moved.
/// Callers must stop at their own root; this does not enforce a boundary.
pub(crate) fn pin_parent(dir: &File) -> io::Result<(File, Metadata)> {
    open_pin(dir.as_raw_fd(), c"..")
}

// Pins have no content access on Linux (O_PATH). Darwin has no O_PATH: every
// unprivileged open checks a read, write or execute permission, and O_EVTONLY
// still grants read. Prefer the event-only read pin, then fall back to an
// execute/search-only descriptor; never request write access. A mode-000 inode
// therefore cannot be pinned on Darwin without privilege (EACCES).
fn open_pin(dir: libc::c_int, name: &CStr) -> io::Result<(File, Metadata)> {
    #[cfg(target_os = "linux")]
    let file = openat(dir, name, libc::O_PATH | libc::O_NOFOLLOW)?;
    #[cfg(target_os = "macos")]
    let file = {
        // Reject special files present now, before opening them.
        let fallback = match lstat_at(dir, name)? & libc::S_IFMT {
            libc::S_IFDIR => Some(libc::O_SEARCH),
            libc::S_IFREG => Some(libc::O_EXEC),
            libc::S_IFLNK => None,
            _ => return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)),
        };
        // O_SYMLINK opens a final symlink itself; O_NOFOLLOW would fail with
        // ELOOP instead. The lstat above only shows the name's type at that
        // instant: an outside actor could substitute a FIFO or device before
        // openat. NONBLOCK keeps a FIFO from blocking and NOCTTY a tty from
        // becoming controlling, but a device open can still have driver side
        // effects. The private backing tree, which the adapter never populates
        // with special files, is what rules that out; this is not race-free.
        let flags = libc::O_SYMLINK | libc::O_NONBLOCK | libc::O_NOCTTY;
        match (openat(dir, name, flags | libc::O_EVTONLY), fallback) {
            (Err(e), Some(access)) if e.raw_os_error() == Some(libc::EACCES) => {
                openat(dir, name, flags | access)?
            }
            (result, _) => result?,
        }
    };
    // The opened inode is authoritative, even if the name changed after lstat.
    let meta = file.metadata()?;
    let ty = meta.file_type();
    if !(ty.is_dir() || ty.is_file() || ty.is_symlink()) {
        return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    }
    Ok((file, meta))
}

fn openat(dir: libc::c_int, name: &CStr, flags: libc::c_int) -> io::Result<File> {
    // SAFETY: dir is AT_FDCWD or a live descriptor; name is NUL-terminated.
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful openat above transfers ownership of this descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(target_os = "macos")]
fn lstat_at(dir: libc::c_int, name: &CStr) -> io::Result<libc::mode_t> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: dir is AT_FDCWD or live; stat is written in full on success.
    result(unsafe {
        libc::fstatat(
            dir,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    // SAFETY: fstatat succeeded and initialized stat.
    Ok(unsafe { stat.assume_init() }.st_mode)
}

/// Rename single-component children relative to held directory descriptors.
/// The flags are FUSE wire values, not Darwin's renameatx_np flag values.
pub(crate) fn rename_at(
    old_dir: &File,
    old: &CStr,
    new_dir: &File,
    new: &CStr,
    flags: fuser::RenameFlags,
) -> io::Result<()> {
    child_name(old)?;
    child_name(new)?;
    #[cfg(target_os = "macos")]
    let flags = match flags.bits() {
        0 => 0,
        // fuser exposes the named constants only on Linux. These are Linux/FUSE
        // RENAME_NOREPLACE and RENAME_EXCHANGE, respectively; Darwin EXCL is 4.
        1 => libc::RENAME_EXCL,
        2 => libc::RENAME_SWAP,
        3 => return Err(io::Error::from_raw_os_error(libc::EINVAL)),
        _ => return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)),
    };
    // SAFETY: live directory descriptors and terminated single-component names.
    let rc = unsafe {
        #[cfg(target_os = "linux")]
        {
            libc::renameat2(
                old_dir.as_raw_fd(),
                old.as_ptr(),
                new_dir.as_raw_fd(),
                new.as_ptr(),
                flags.bits(),
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::renameatx_np(
                old_dir.as_raw_fd(),
                old.as_ptr(),
                new_dir.as_raw_fd(),
                new.as_ptr(),
                flags,
            )
        }
    };
    result(rc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuser::RenameFlags;
    use std::{
        fs,
        os::unix::{ffi::OsStrExt, fs::MetadataExt},
        path::Path,
    };

    const KEY: &CStr = c"user.sl7.file.v1";

    fn pin(path: &Path) -> File {
        pin_path(path).unwrap().0
    }

    fn identity(meta: &Metadata) -> (u64, u64) {
        (meta.dev(), meta.ino())
    }

    fn child(dir: &File, name: &CStr) -> io::Result<(File, Metadata)> {
        pin_at(dir, name)
    }

    fn errno<T>(result: io::Result<T>) -> Option<i32> {
        result.err().and_then(|e| e.raw_os_error())
    }

    // Diagnostic only: find live descriptors naming `path`. Production code
    // never maps descriptors back to pathnames.
    fn descriptors_naming(path: &Path) -> usize {
        let path = path
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join(path.file_name().unwrap());
        (0..4096)
            .filter(|fd| {
                #[cfg(target_os = "linux")]
                {
                    fs::read_link(format!("/proc/self/fd/{fd}")).is_ok_and(|p| p == path)
                }
                #[cfg(target_os = "macos")]
                {
                    let mut buf = [0u8; libc::PATH_MAX as usize];
                    // SAFETY: buf has PATH_MAX bytes, as F_GETPATH requires.
                    let rc = unsafe { libc::fcntl(*fd, libc::F_GETPATH, buf.as_mut_ptr()) };
                    rc == 0
                        && CStr::from_bytes_until_nul(&buf).is_ok_and(|p| {
                            Path::new(std::ffi::OsStr::from_bytes(p.to_bytes())) == path
                        })
                }
            })
            .count()
    }

    fn cloexec(file: &File) -> bool {
        // SAFETY: file is live for the call.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        flags & libc::FD_CLOEXEC != 0
    }

    #[test]
    fn pin_follows_inode_through_rename_replacement_hardlink_and_unlink() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = pin(tmp.path());
        fs::write(tmp.path().join("a"), b"original").unwrap();
        let (file, meta) = child(&dir, c"a").unwrap();
        assert!(meta.is_file() && cloexec(&file));
        fs::hard_link(tmp.path().join("a"), tmp.path().join("alias")).unwrap();
        let (_, alias) = child(&dir, c"alias").unwrap();
        assert_eq!(identity(&alias), identity(&meta));
        fs::rename(tmp.path().join("a"), tmp.path().join("moved")).unwrap();
        fs::write(tmp.path().join("a"), b"replacement").unwrap();
        let (_, replacement) = child(&dir, c"a").unwrap();
        assert_ne!(identity(&replacement), identity(&meta));
        assert_eq!(identity(&file.metadata().unwrap()), identity(&meta));
        fs::remove_file(tmp.path().join("alias")).unwrap();
        assert_eq!(file.metadata().unwrap().nlink(), 1);
        fs::remove_file(tmp.path().join("moved")).unwrap();
        let after = file.metadata().unwrap();
        assert_eq!(
            (identity(&after), after.nlink(), after.len()),
            (identity(&meta), 0, 8)
        );
        assert_eq!(errno(child(&dir, c"moved")), Some(libc::ENOENT));
    }

    #[test]
    fn child_lookup_stays_rooted_in_held_directory_after_parent_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("x"), b"held").unwrap();
        let original = fs::symlink_metadata(parent.join("x")).unwrap();
        let held = pin(&parent);
        fs::rename(&parent, tmp.path().join("moved")).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("x"), b"impostor").unwrap();
        fs::write(parent.join("only-new"), b"").unwrap();
        assert_eq!(
            identity(&child(&held, c"x").unwrap().1),
            identity(&original)
        );
        assert_eq!(errno(child(&held, c"only-new")), Some(libc::ENOENT));
        let up = pin_parent(&held).unwrap().1;
        assert_eq!(identity(&up), identity(&fs::metadata(tmp.path()).unwrap()));
        // Dot-dot names the held directory's current parent, not a path prefix.
        fs::rename(tmp.path().join("moved"), other.path().join("moved")).unwrap();
        let up = pin_parent(&held).unwrap().1;
        assert_eq!(
            identity(&up),
            identity(&fs::metadata(other.path()).unwrap())
        );
        assert_eq!(
            identity(&child(&held, c"x").unwrap().1),
            identity(&original)
        );
    }

    #[test]
    fn malformed_children_and_non_directory_parents_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        fs::write(tmp.path().join("file"), b"").unwrap();
        let dir = pin(tmp.path());
        for name in [
            c"", c".", c"..", c"sub/..", c"../x", c"sub/", c"/etc", c"a/b",
        ] {
            assert_eq!(errno(child(&dir, name)), Some(libc::EINVAL), "{name:?}");
        }
        assert_eq!(errno(child(&dir, c"missing")), Some(libc::ENOENT));
        let (file, _) = child(&dir, c"file").unwrap();
        assert_eq!(errno(child(&file, c"x")), Some(libc::ENOTDIR));
        assert_eq!(errno(pin_parent(&file)), Some(libc::ENOTDIR));
        assert_eq!(errno(pin_path(Path::new("bad\0path"))), Some(libc::EINVAL));
    }

    #[test]
    fn symlink_pins_do_not_follow_relative_absolute_or_dangling_targets() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("target"), b"t").unwrap();
        fs::create_dir(tmp.path().join("dir")).unwrap();
        let links = [
            (c"relative", Path::new("target")),
            (c"to-dir", Path::new("dir")),
            (c"absolute", tmp.path()),
            (c"dangling", Path::new("missing")),
            (c"loop", Path::new("loop")),
        ];
        for (name, target) in links {
            let name = std::ffi::OsStr::from_bytes(name.to_bytes());
            std::os::unix::fs::symlink(target, tmp.path().join(name)).unwrap();
        }
        let dir = pin(tmp.path());
        for (name, _) in links {
            let path = tmp
                .path()
                .join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            let (file, meta) = child(&dir, name).unwrap();
            assert!(meta.file_type().is_symlink(), "{name:?}");
            assert_eq!(
                identity(&meta),
                identity(&fs::symlink_metadata(&path).unwrap())
            );
            // The pin keeps the link after its name is replaced by its target type.
            fs::remove_file(&path).unwrap();
            fs::write(&path, b"replacement").unwrap();
            assert!(file.metadata().unwrap().file_type().is_symlink());
            assert_eq!(identity(&file.metadata().unwrap()), identity(&meta));
        }
        std::os::unix::fs::symlink("target", tmp.path().join("root-link")).unwrap();
        let (_, meta) = pin_path(&tmp.path().join("root-link")).unwrap();
        assert!(meta.file_type().is_symlink());
    }

    // Mode bits are only an input: root, ACLs and file flags decide effective
    // access, which is what the kernel checks when opening a descriptor.
    #[cfg(target_os = "macos")]
    fn accessible(dir: &File, name: &CStr, mode: libc::c_int) -> bool {
        // SAFETY: dir is live and name is NUL-terminated.
        unsafe { libc::faccessat(dir.as_raw_fd(), name.as_ptr(), mode, libc::AT_EACCESS) == 0 }
    }

    fn restricted(tmp: &Path) -> (File, [(&'static CStr, u32); 4]) {
        use std::os::unix::fs::PermissionsExt;
        let files = [
            (c"none", 0o000),
            (c"exec", 0o100),
            (c"write", 0o200),
            (c"read", 0o400),
        ];
        for (name, mode) in files {
            let path = tmp.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
            fs::write(&path, b"secret").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        let search = tmp.join("search");
        fs::create_dir(&search).unwrap();
        fs::write(search.join("x"), b"").unwrap();
        fs::set_permissions(&search, fs::Permissions::from_mode(0o100)).unwrap();
        (pin(tmp), files)
    }

    // Acceptance invariant 4. Running it on Darwin with --ignored is the
    // executable probe: the mode-000 pin fails with EACCES.
    // Under root, DAC bypass makes this vacuous; run it unprivileged too.
    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "invariant 4 open on Darwin: no O_PATH; opens need effective read/write/execute access"
    )]
    fn pins_do_not_require_or_grant_content_access() {
        use std::io::{Read, Write};
        let tmp = tempfile::tempdir().unwrap();
        let (dir, files) = restricted(tmp.path());
        for (name, mode) in files {
            let (mut file, meta) =
                child(&dir, name).unwrap_or_else(|e| panic!("pin {name:?} mode {mode:o}: {e}"));
            assert_eq!(meta.mode() & 0o777, mode, "{name:?}");
            assert_eq!(errno(file.write(b"x")), Some(libc::EBADF), "{name:?}");
            assert_eq!(errno(file.read(&mut [0; 1])), Some(libc::EBADF), "{name:?}");
        }
        let (held, meta) = child(&dir, c"search").unwrap();
        assert!(meta.is_dir());
        assert!(child(&held, c"x").unwrap().1.is_file());
        fs::set_permissions(
            tmp.path().join("search"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
    }

    // Characterizes the qualified Darwin prototype; it does not satisfy
    // invariant 4. Expectations follow effective access, not mode bits.
    #[cfg(target_os = "macos")]
    #[test]
    fn darwin_pin_requires_effective_read_or_execute_and_read_pins_can_read() {
        use std::io::{Read, Write};
        let tmp = tempfile::tempdir().unwrap();
        let (dir, files) = restricted(tmp.path());
        for (name, mode) in files {
            let readable = accessible(&dir, name, libc::R_OK);
            let executable = accessible(&dir, name, libc::X_OK);
            let pinned = child(&dir, name);
            if !readable && !executable {
                // Includes the unprivileged mode-000 and write-only cases.
                assert_eq!(errno(pinned), Some(libc::EACCES), "{name:?} {mode:o}");
                continue;
            }
            let (mut file, _) = pinned.unwrap();
            assert_eq!(errno(file.write(b"x")), Some(libc::EBADF), "{name:?}");
            let read = file.read(&mut [0; 1]);
            if readable {
                // O_EVTONLY is not a no-access pin: it carries read access.
                assert_eq!(read.unwrap(), 1, "{name:?}");
            } else {
                // Execute-only fallback: no content access, no xattr access.
                assert_eq!(errno(read), Some(libc::EBADF), "{name:?}");
                assert_eq!(
                    errno(read_xattr(&file, KEY, &mut [0; 8])),
                    Some(libc::EACCES)
                );
            }
        }
        // Search-only directories fall back to O_SEARCH and anchor lookups.
        let (held, meta) = child(&dir, c"search").unwrap();
        assert!(meta.is_dir());
        assert!(child(&held, c"x").unwrap().1.is_file());
        fs::set_permissions(
            tmp.path().join("search"),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
    }

    #[test]
    fn unsupported_inode_types_fail_without_blocking_or_leaking_descriptors() {
        let tmp = tempfile::tempdir().unwrap();
        let fifo = tmp.path().join("fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_c is NUL-terminated.
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        let socket = tmp.path().join("socket");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let dir = pin(tmp.path());
        assert!(cloexec(&dir));
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                // A blocking FIFO open would hang here, not fail the assertion.
                let errors = [errno(child(&dir, c"fifo")), errno(child(&dir, c"socket"))];
                sender.send(errors).unwrap();
            });
            let errors = receiver.recv_timeout(std::time::Duration::from_secs(5));
            if errors.is_err() {
                // Unblock the stuck opener with a writer so the scope can join.
                let _ = fs::OpenOptions::new().write(true).open(&fifo);
            }
            assert_eq!(
                errors.expect("FIFO pin blocked"),
                [Some(libc::EOPNOTSUPP); 2]
            );
        });
        assert_eq!(descriptors_naming(&fifo), 0);
        assert_eq!(descriptors_naming(&socket), 0);
    }

    fn value(file: &File) -> Option<Vec<u8>> {
        let mut bytes = [0; 32];
        read_xattr(file, KEY, &mut bytes)
            .unwrap()
            .map(|len| bytes[..len].to_vec())
    }

    #[test]
    fn identity_survives_rename_replacement_hardlink_and_unlink() {
        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old");
        let moved = tmp.path().join("moved");
        let alias = tmp.path().join("alias");
        fs::write(&old, b"original").unwrap();
        let file = pin(&old);
        assert_eq!(value(&file), None);
        create_xattr(&file, KEY, b"42").unwrap();
        fs::hard_link(&old, &alias).unwrap();
        let linked = pin(&alias);
        fs::rename(&old, &moved).unwrap();
        fs::write(&old, b"replacement").unwrap();
        let replacement = pin(&old);
        assert_eq!(value(&replacement), None);
        create_xattr(&replacement, KEY, b"99").unwrap();
        fs::remove_file(&moved).unwrap();
        fs::remove_file(&alias).unwrap();
        assert_eq!(value(&file).as_deref(), Some(b"42".as_slice()));
        assert_eq!(value(&linked), value(&file));
        assert_eq!(value(&replacement).as_deref(), Some(b"99".as_slice()));
        assert_eq!(
            create_xattr(&file, KEY, b"43").unwrap_err().raw_os_error(),
            Some(libc::EEXIST)
        );
        assert_eq!(value(&file).as_deref(), Some(b"42".as_slice()));
    }

    #[test]
    fn identity_can_be_attached_after_last_unlink() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        fs::write(&path, b"bytes").unwrap();
        let file = pin(&path);
        fs::remove_file(path).unwrap();
        create_xattr(&file, KEY, b"17").unwrap();
        assert_eq!(value(&file).as_deref(), Some(b"17".as_slice()));
    }

    #[test]
    fn attribute_errors_are_not_mistaken_for_missing_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        fs::write(&path, b"bytes").unwrap();
        let file = pin(&path);
        create_xattr(&file, KEY, b"123456").unwrap();
        let error = read_xattr(&file, KEY, &mut [0; 2]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ERANGE));
    }

    #[test]
    fn concurrent_identity_creators_do_not_overwrite_each_other() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("file");
        fs::write(&path, b"bytes").unwrap();
        let file = pin(&path);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let handles: Vec<_> = [b"41", b"42"]
                .into_iter()
                .map(|id| {
                    let file = &file;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        (id, create_xattr(file, KEY, id))
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            assert_eq!(results.iter().filter(|(_, r)| r.is_ok()).count(), 1);
            for (id, result) in results {
                match result {
                    Ok(()) => assert_eq!(value(&file).as_deref(), Some(id.as_slice())),
                    Err(e) => assert_eq!(e.raw_os_error(), Some(libc::EEXIST)),
                }
            }
        });
    }

    #[test]
    fn rename_stays_anchored_when_parent_name_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        let moved = tmp.path().join("moved");
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("old"), b"original").unwrap();
        let dir = pin(&parent);
        fs::rename(&parent, &moved).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("old"), b"replacement").unwrap();
        rename_at(&dir, c"old", &dir, c"new", RenameFlags::empty()).unwrap();
        assert_eq!(fs::read(moved.join("new")).unwrap(), b"original");
        assert_eq!(fs::read(parent.join("old")).unwrap(), b"replacement");
        assert!(!parent.join("new").exists());
    }

    #[test]
    fn rename_replace_noreplace_and_exchange_use_correct_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = pin(tmp.path());
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        fs::write(&a, b"A").unwrap();
        fs::write(&b, b"B").unwrap();
        let error =
            rename_at(&dir, c"a", &dir, c"b", RenameFlags::from_bits_retain(1)).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
        assert_eq!(fs::read(&a).unwrap(), b"A");
        assert_eq!(fs::read(&b).unwrap(), b"B");
        rename_at(&dir, c"a", &dir, c"b", RenameFlags::from_bits_retain(2)).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"B");
        assert_eq!(fs::read(&b).unwrap(), b"A");
        rename_at(&dir, c"a", &dir, c"b", RenameFlags::empty()).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read(&b).unwrap(), b"B");
        rename_at(&dir, c"b", &dir, c"a", RenameFlags::from_bits_retain(1)).unwrap();
        assert_eq!(fs::read(&a).unwrap(), b"B");
        assert!(!b.exists());
    }

    #[test]
    fn rename_invalid_flags_and_names_leave_both_entries_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = pin(tmp.path());
        fs::write(tmp.path().join("a"), b"A").unwrap();
        fs::write(tmp.path().join("b"), b"B").unwrap();
        for bits in [3, 0x8000_0000] {
            assert!(
                rename_at(&dir, c"a", &dir, c"b", RenameFlags::from_bits_retain(bits)).is_err()
            );
        }
        for name in [c"", c".", c"..", c"a/b", c"/a"] {
            for (old, new) in [(name, c"b"), (c"a", name)] {
                assert_eq!(
                    rename_at(&dir, old, &dir, new, RenameFlags::empty())
                        .unwrap_err()
                        .raw_os_error(),
                    Some(libc::EINVAL)
                );
            }
        }
        assert_eq!(fs::read(tmp.path().join("a")).unwrap(), b"A");
        assert_eq!(fs::read(tmp.path().join("b")).unwrap(), b"B");
    }

    #[test]
    fn cross_directory_rename_moves_dangling_symlink_without_following_it() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing", tmp.path().join("link")).unwrap();
        let dir = pin(tmp.path());
        let other_dir = pin(other.path());
        rename_at(&dir, c"link", &other_dir, c"moved", RenameFlags::empty()).unwrap();
        assert_eq!(
            fs::read_link(other.path().join("moved")).unwrap(),
            Path::new("missing")
        );
        assert!(fs::symlink_metadata(tmp.path().join("link")).is_err());
    }
}
