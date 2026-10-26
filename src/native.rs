//! OS-specific backing-inode operations. These operate on held descriptors,
//! never paths recovered with F_GETPATH, so rename/unlink cannot retarget them.
use std::{ffi::CStr, fs::File, io, os::fd::AsRawFd};

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
    use std::{fs, path::Path};

    const KEY: &CStr = c"user.sl7.file.v1";

    fn pin(path: &Path) -> File {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_PATH | libc::O_NOFOLLOW)
                .open(path)
                .unwrap()
        }
        #[cfg(target_os = "macos")]
        {
            File::open(path).unwrap()
        }
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
