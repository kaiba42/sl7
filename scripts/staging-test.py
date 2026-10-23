#!/usr/bin/env python3
"""Qualify the native staging adapter in the isolated Linux test container."""
import errno
import fcntl
import hashlib
import json
import mmap
import os
from pathlib import Path
import subprocess
import sys
import time
from mount_helpers import is_mounted, isolated_test_directory

BINARY = Path(os.environ.get("SL7_BINARY", "target/debug/sl7")).resolve()


def command(*args, **kwargs):
    return subprocess.check_output([str(a) for a in args], text=True, **kwargs)


def fixture(work):
    sources = work / "workspace"
    files = {
        "Cargo.toml": '[workspace]\nmembers=["app", "maker"]\nresolver="3"\n',
        "maker/Cargo.toml": '[package]\nname="sl7-maker"\nversion="0.1.0"\nedition="2024"\n[lib]\nproc-macro=true\n',
        "maker/src/lib.rs": 'extern crate proc_macro;\n#[proc_macro]\npub fn answer(_: proc_macro::TokenStream) -> proc_macro::TokenStream { "42u32".parse().unwrap() }\n',
        "app/Cargo.toml": '[package]\nname="sl7-fixture"\nversion="0.1.0"\nedition="2024"\n[dependencies]\nsl7-maker={path="../maker"}\n',
        "app/build.rs": 'fn main() { std::fs::write(std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("generated.rs"), "pub const BUILD: u32 = 7;").unwrap(); println!("cargo:rerun-if-changed=build.rs"); }\n',
        "app/src/main.rs": 'include!(concat!(env!("OUT_DIR"), "/generated.rs"));\nconst EDIT: u32 = 0;\nfn answer() -> u32 { sl7_maker::answer!() + BUILD + EDIT }\nfn main() { println!("{}", answer()); }\n#[test] fn correct() { assert_eq!(answer(), 49 + EDIT); }\n',
    }
    for name, contents in files.items():
        path = sources / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)
    return sources


def exercise(root):
    root.mkdir()
    file = root / "file"
    file.write_bytes(b"a" * 8192)
    # Mapping outlives its descriptor; a second descriptor's writes must become
    # visible through the same cached inode, including after rename/unlink.
    descriptor = os.open(file, os.O_RDWR)
    mapping = mmap.mmap(descriptor, 8192)
    os.close(descriptor)
    with file.open("r+b", buffering=0) as writer:
        writer.write(b"coherent")
        assert mapping[:8] == b"coherent"
        mapping[4096:4100] = b"maps"
        mapping.flush()
        assert os.pread(writer.fileno(), 4, 4096) == b"maps"
    replacement = root / "replacement"
    replacement.write_bytes(b"replacement")
    replacement.rename(file)
    assert file.read_bytes() == b"replacement"
    assert mapping[:8] == b"coherent"
    mapping[:4] = b"live"
    mapping.flush()
    assert mapping[:4] == b"live"
    mapping.close()
    linked = root / "linked"
    os.link(file, linked)
    assert file.stat().st_ino == linked.stat().st_ino
    file.unlink()
    assert linked.read_bytes() == b"replacement"
    symlink = root / "relative-link"
    symlink.symlink_to("linked")
    assert os.readlink(symlink) == "linked"
    assert symlink.read_bytes() == b"replacement"
    dangling = root / "dangling"
    dangling.symlink_to("missing")
    assert os.readlink(dangling) == "missing" and not dangling.exists()
    timestamp = 1_700_000_000_123_456_789
    os.utime(linked, ns=(timestamp, timestamp))
    assert linked.stat().st_mtime_ns == timestamp
    linked.chmod(0o640)
    assert linked.stat().st_mode & 0o777 == 0o640
    with linked.open("ab", buffering=0) as appended:
        appended.write(b"-append")
    assert linked.read_bytes() == b"replacement-append"
    # Sufficient entries to force several FUSE readdir replies.
    many = root / "many"
    many.mkdir()
    expected = {f"entry-{i:04d}" for i in range(300)}
    for name in expected:
        (many / name).touch()
    assert set(os.listdir(many)) == expected
    try:
        many.rmdir()
    except OSError as error:
        assert error.errno == errno.ENOTEMPTY
    else:
        raise AssertionError("removed nonempty directory")
    before = linked.stat().st_ino
    directory = root / "moved-directory"
    root.joinpath("many").rename(directory)
    assert set(os.listdir(directory)) == expected
    assert linked.stat().st_ino == before
    with linked.open("r+b") as locked:
        fcntl.flock(locked, fcntl.LOCK_EX | fcntl.LOCK_NB)
        child = subprocess.run([sys.executable, "-c", "import fcntl, sys; f=open(sys.argv[1], 'rb'); "
                                "fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)", str(linked)],
                               capture_output=True, text=True)
        assert child.returncode != 0 and "BlockingIOError" in child.stderr
    return ["mapping_outlives_descriptor", "same_inode_write_coherence",
            "rename_over_mapping", "hardlink_identity", "relative_and_dangling_symlinks",
            "nanosecond_mtime", "chmod", "append", "large_readdir", "directory_rename",
            "cross_process_lock_contention"]


def crash_restart(backing, mount, log_path):
    """Acknowledge fsync+directory sync, kill the daemon, remount, verify bytes."""
    expected = b"synced before abrupt daemon termination"
    with log_path.open("a") as log:
        for attempt in range(2):
            daemon = subprocess.Popen([str(BINARY), "mount-staging", str(backing), str(mount)], stdout=log, stderr=log)
            killed = False
            try:
                deadline = time.monotonic() + 15
                while not is_mounted(mount):
                    if daemon.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(f"remount failed: {log_path.read_text()}")
                    time.sleep(0.05)
                path = mount / "crash-synced"
                if attempt == 0:
                    with path.open("wb") as file:
                        file.write(expected)
                        os.fsync(file.fileno())
                    descriptor = os.open(mount, os.O_RDONLY | os.O_DIRECTORY)
                    os.fsync(descriptor)
                    os.close(descriptor)
                    daemon.kill()
                    killed = True
                    daemon.wait(timeout=5)
                else:
                    assert path.read_bytes() == expected
                    assert command(mount / "target/debug/sl7-fixture").strip() == "50"
            finally:
                if is_mounted(mount):
                    command("fusermount3", "-u", mount)
                elif daemon.poll() is None:
                    daemon.terminate()
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=5)
            assert daemon.returncode == (-9 if killed else 0), log_path.read_text()


def run(work):
    backing, mount = work / "backing", work / "mount"
    backing.mkdir()
    mount.mkdir()
    sources = fixture(work)
    env = dict(os.environ, CARGO_TARGET_DIR=str(mount / "target"),
               CARGO_BUILD_BUILD_DIR=str(mount / "build"))
    log_path = work / "daemon.log"
    with log_path.open("w+") as log:
        daemon = subprocess.Popen([str(BINARY), "mount-staging", str(backing), str(mount)], stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 15
            while not is_mounted(mount):
                if daemon.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(f"mount startup failed: {log_path.read_text()}")
                time.sleep(0.05)
            native_probe = json.loads(command(BINARY, "probe", work))
            mounted_probe = json.loads(command(BINARY, "probe", mount))
            assert native_probe["checks"] == mounted_probe["checks"]
            capacity = os.statvfs(mount)
            assert capacity.f_blocks > 0 and capacity.f_bavail > 0
            native_ops = exercise(work / "native-ops")
            mounted_ops = exercise(mount / "mounted-ops")
            assert native_ops == mounted_ops
            def cargo(*args):
                return command("cargo", *args, "--offline", cwd=sources, env=env)
            cargo("build", "--workspace")
            assert command(mount / "target/debug/sl7-fixture").strip() == "49"
            assert (mount / "build").is_dir(), "Cargo ignored CARGO_BUILD_BUILD_DIR"
            warm = [json.loads(line) for line in cargo("build", "--workspace", "--message-format=json").splitlines()]
            artifacts = [row for row in warm if row["reason"] == "compiler-artifact"]
            assert artifacts and all(row["fresh"] for row in artifacts), artifacts
            cargo("test", "--workspace")
            cargo("check", "--workspace")
            source = sources / "app/src/main.rs"
            source.write_text(source.read_text().replace("const EDIT: u32 = 0;", "const EDIT: u32 = 1;"))
            cargo("build", "--workspace")
            cargo("test", "--workspace")
            assert command(mount / "target/debug/sl7-fixture").strip() == "50"
            persisted = mount / "durable"
            with persisted.open("wb") as file:
                file.write(b"synced staging bytes")
                os.fsync(file.fileno())
            descriptor = os.open(mount, os.O_RDONLY | os.O_DIRECTORY)
            os.fsync(descriptor)
            os.close(descriptor)
            digest = hashlib.sha256(persisted.read_bytes()).hexdigest()
        finally:
            if is_mounted(mount):
                command("fusermount3", "-u", mount)
            else:
                daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=5)
        assert daemon.returncode == 0, log_path.read_text()
    assert hashlib.sha256((backing / "durable").read_bytes()).hexdigest() == digest
    assert command(backing / "target/debug/sl7-fixture").strip() == "50"
    crash_restart(backing, mount, log_path)
    return {"platform": command("uname", "-sr").strip(), "mount": "passed",
            "native_and_mounted_probe": mounted_probe["checks"], "comparison_checks": mounted_ops,
            "cargo_build_test_check": "passed", "cargo_warm_build_all_fresh": True,
            "cargo_build_script_and_proc_macro": "passed", "cargo_one_line_edit": "passed",
            "separate_target_and_build_directories": True, "post_unmount_native_bytes": "passed",
            "daemon_kill_restart_synced_bytes": "passed", "statvfs_capacity": "passed",
            "writable_deduplication": "not implemented: native staging adapter only"}


if __name__ == "__main__":
    with isolated_test_directory("sl7-staging-test-") as work:
        print(json.dumps(run(work), indent=2))
