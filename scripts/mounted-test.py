#!/usr/bin/env python3
"""Disposable Linux FUSE integration test; requires /dev/fuse and mount capability.

All writable state lives in an owned temporary directory. The daemon is unmounted
and joined before that directory is removed. Intended for tests/docker/Dockerfile.
"""
from concurrent.futures import ThreadPoolExecutor
import errno
import json
import os
from pathlib import Path
import random
import runpy
import subprocess
import time
from mount_helpers import is_mounted, isolated_test_directory

SMOKE = runpy.run_path(str(Path(__file__).with_name("mount-smoke.py")))["run"]
BINARY = Path(os.environ.get("SL7_BINARY", "target/debug/sl7")).resolve()


def command(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True)


def run(work):
    store = work / "store"
    mount = work / "mount"
    mount.mkdir()

    def sl7(*args):
        return command(BINARY, "--store", store, *args)

    sl7("init")
    rng = random.Random(7)
    payload = rng.randbytes(1024 * 1024) * 4
    original, edited, empty = (work / name for name in ("original", "edited", "empty"))
    original.write_bytes(payload)
    edited.write_bytes(payload[:15000] + b"one inserted line\n" + payload[15000:])
    empty.write_bytes(b"")
    first = json.loads(sl7("put", "main", "debug/data", original))
    duplicate = json.loads(sl7("put", "duplicate", "debug/data", original))
    changed = json.loads(sl7("put", "edited", "debug/data", edited))
    assert duplicate["new_chunks"] == duplicate["new_pack_bytes"] == 0
    assert changed["new_pack_bytes"] < first["new_pack_bytes"] // 10
    sl7("put", "main", "empty", empty)

    # Build genuine compiler artifacts outside the mount, then consume their
    # chunk-backed versions through rustc's ordinary filesystem accesses.
    producer = work / "producer.rs"
    producer.write_text("pub fn answer() -> u32 { 42 }\n")
    command("rustc", "--crate-name", "sl7_fixture", "--crate-type", "rlib",
            "--emit=link,metadata", "--out-dir", work, producer)
    for extension in ("rmeta", "rlib"):
        sl7("put", "main", f"debug/libsl7_fixture.{extension}", work / f"libsl7_fixture.{extension}")
    consumer = work / "consumer.rs"
    consumer.write_text('fn main() { println!("{}", sl7_fixture::answer()); }\n')

    log_path = work / "daemon.log"
    with log_path.open("w+") as log:
        daemon = subprocess.Popen([str(BINARY), "--store", str(store), "mount-read-only", str(mount)],
                                  stdout=log, stderr=log)
        mounted = False
        try:
            deadline = time.monotonic() + 15
            while not is_mounted(mount):
                if daemon.poll() is not None or time.monotonic() > deadline:
                    log.flush()
                    raise RuntimeError(f"mount did not become ready: {log_path.read_text()}")
                time.sleep(0.05)
            mounted = True
            inputs = [(original, "main/debug/data"), (original, "duplicate/debug/data"),
                      (edited, "edited/debug/data"), (empty, "main/empty"),
                      (work / "libsl7_fixture.rmeta", "main/debug/libsl7_fixture.rmeta"),
                      (work / "libsl7_fixture.rlib", "main/debug/libsl7_fixture.rlib")]
            with ThreadPoolExecutor(max_workers=3) as readers:
                results = list(readers.map(lambda pair: SMOKE(mount, pair[0], Path(pair[1])), inputs))
            assert (mount / "main/debug/data").stat().st_ino != (mount / "duplicate/debug/data").stat().st_ino
            for operation in [lambda: (mount / "new-file").write_bytes(b"forbidden"),
                              lambda: (mount / "main/debug/data").unlink(),
                              lambda: (mount / "main/debug/data").rename(mount / "renamed")]:
                try:
                    operation()
                except OSError as error:
                    assert error.errno == errno.EROFS, error
                else:
                    raise AssertionError("read-only namespace accepted a mutation")
            locked = subprocess.run([str(BINARY), "--store", str(store), "stats"],
                                    capture_output=True, text=True)
            assert locked.returncode == 1 and "already in use" in locked.stderr
            command("rustc", "--edition=2024", "--emit=metadata", "--extern",
                    f"sl7_fixture={mount}/main/debug/libsl7_fixture.rmeta",
                    "-o", work / "consumer.rmeta", consumer)
            command("rustc", "--edition=2024", "--extern",
                    f"sl7_fixture={mount}/main/debug/libsl7_fixture.rlib",
                    "-o", work / "consumer", consumer)
            assert command(work / "consumer").strip() == "42"
        finally:
            if mounted or is_mounted(mount):
                # Never recursively remove an active mount if unmount fails.
                command("fusermount3", "-u", mount)
            else:
                daemon.terminate()
            try:
                daemon.wait(timeout=10)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=5)
            log.flush()
        assert daemon.returncode == 0, log_path.read_text()
    checked = json.loads(sl7("check"))
    return {"platform": command("uname", "-sr").strip(),
            "rustc": command("rustc", "--version").strip(), "mount": "passed",
            "artifacts": results, "duplicate_new_chunks": duplicate["new_chunks"],
            "duplicate_new_pack_bytes": duplicate["new_pack_bytes"],
            "edited_new_pack_bytes": changed["new_pack_bytes"],
            "first_new_pack_bytes": first["new_pack_bytes"],
            "namespace_mutations": "EROFS", "independent_view_inodes": True,
            "rustc_metadata_read": "passed", "rustc_rlib_link_and_execution": "passed",
            "store_lock_during_mount": "passed", "post_unmount_check": checked}


if __name__ == "__main__":
    with isolated_test_directory("sl7-mounted-test-") as work:
        print(json.dumps(run(work), indent=2))
