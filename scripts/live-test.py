#!/usr/bin/env python3
"""End-to-end writable deduplication and Cargo lifecycle in the Docker harness."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import errno
import hashlib
import json
import mmap
import os
from pathlib import Path
import random
import runpy
import subprocess
import time
from mount_helpers import is_mounted, isolated_test_directory

FIXTURE = runpy.run_path(str(Path(__file__).with_name("staging-test.py")))["fixture"]
BINARY = Path(os.environ.get("SL7_BINARY", "target/debug/sl7")).resolve()


class Harness:
    def __init__(self, work):
        self.work = work
        self.store = work / "store"
        self.mount = work / "mount"
        self.mount.mkdir()
        self.log = work / "daemon.log"
        self.snapshots = []
        self.commands = []
        self.sl7("init")

    def sl7(self, *args):
        return json.loads(subprocess.check_output([str(BINARY), "--store", str(self.store), *map(str, args)], text=True))

    @contextmanager
    def mounted(self, expect_kill=False):
        with self.log.open("a") as log:
            daemon = subprocess.Popen([str(BINARY), "--store", str(self.store), "mount", str(self.mount)], stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 15
                while not is_mounted(self.mount):
                    if daemon.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError(self.log.read_text())
                    time.sleep(0.02)
                yield daemon
            except BaseException:
                print(self.log.read_text(), file=__import__("sys").stderr)
                raise
            finally:
                if is_mounted(self.mount):
                    subprocess.check_call(["fusermount3", "-u", str(self.mount)])
                elif daemon.poll() is None:
                    daemon.terminate()
                try:
                    daemon.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=5)
            assert daemon.returncode == (-9 if expect_kill else 0), self.log.read_text()

    def sealed(self, relative):
        # Read-only inspection of allocation; never read placeholder data directly.
        path = self.store / "tree" / relative
        deadline = time.monotonic() + 15
        while path.stat().st_blocks != 0:
            if time.monotonic() > deadline:
                raise AssertionError(f"file did not seal: {relative}\n{self.log.read_text()}")
            time.sleep(0.02)

    def snapshot(self, name):
        self.sl7("check")
        gc = self.sl7("gc")
        stats = self.sl7("live-stats")
        self.snapshots.append({"stage": name, "live": stats, "gc": gc})
        return stats

    def cargo(self, source, view, *args, fresh=False):
        env = dict(os.environ, CARGO_TARGET_DIR=str(self.mount / view / "target"),
                   CARGO_BUILD_BUILD_DIR=str(self.mount / view / "build"))
        command = ["cargo", *args, "--offline"]
        if fresh:
            command.append("--message-format=json")
        start = time.monotonic()
        output = subprocess.check_output(command, cwd=source, env=env, text=True)
        self.commands.append({"view": view, "command": " ".join(command), "elapsed_seconds": time.monotonic() - start})
        if fresh:
            rows = [json.loads(line) for line in output.splitlines()]
            artifacts = [r for r in rows if r["reason"] == "compiler-artifact"]
            assert artifacts and all(r["fresh"] for r in artifacts), artifacts
        return output

    def executable(self, view, profile, expected):
        path = self.mount / view / "target" / profile / "sl7-fixture"
        assert subprocess.check_output([str(path)], text=True).strip() == str(expected)


def run(work):
    h = Harness(work)
    payload = random.Random(7).randbytes(256 * 1024) * 8
    with h.mounted():
        for view in ("main", "other"):
            (h.mount / view).mkdir()
        a, b = h.mount / "main/shared", h.mount / "other/shared"
        with a.open("wb") as file:
            file.write(payload)
            file.flush()
            before = os.fstat(file.fileno())
        h.sealed("main/shared")
        assert a.stat().st_mtime_ns == before.st_mtime_ns
        assert a.stat().st_ctime_ns == before.st_ctime_ns
        a.chmod(0o444)
        stamp = 1234567890000000000
        os.utime(a, ns=(stamp, stamp))
        assert a.stat().st_mtime_ns == stamp
        assert (h.store / "tree/main/shared").stat().st_blocks == 0
        a.chmod(0o644)
        b.write_bytes(payload)
        h.sealed("other/shared")
        assert a.read_bytes() == b.read_bytes() == payload
        assert a.stat().st_ino != b.stat().st_ino
    shared = h.snapshot("two-identical-files")
    assert shared["sealed_files"] == 2 and shared["native_allocated_bytes"] == 0
    assert shared["content"]["unique_raw_bytes"] < len(payload)

    with h.mounted():
        a, b = h.mount / "main/shared", h.mount / "other/shared"
        fd = os.open(a, os.O_RDONLY)
        mapping = mmap.mmap(fd, 0, access=mmap.ACCESS_READ)
        os.close(fd)
        with a.open("r+b", buffering=0) as writer:
            assert writer.read(32) == payload[:32]
            writer.seek(8190)
            writer.write(b"changed across a boundary")
            os.fsync(writer.fileno())
        assert mapping[8190:8215] == b"changed across a boundary"
        assert b.read_bytes() == payload
        assert (h.store / "tree/main/shared").stat().st_blocks > 0, "sealed while mapping was alive"
        mapping.close()
        h.sealed("main/shared")
        os.link(a, h.mount / "main/alias")
        assert a.stat().st_ino == (h.mount / "main/alias").stat().st_ino
        a.rename(h.mount / "main/renamed")
        assert (h.mount / "main/alias").read_bytes()[8190:8215] == b"changed across a boundary"
        with (h.mount / "main/renamed").open("wb") as file:
            file.write(b"truncated")
        assert (h.mount / "main/alias").read_bytes() == b"truncated"
        (h.mount / "main/renamed").unlink()
        (h.mount / "main/alias").unlink()
        b.unlink()
        probe = h.sl7("probe", h.mount)
    empty = h.snapshot("transition-fixtures-cleaned")
    assert empty["files"] == 0 and empty["content"]["unique_chunks"] == 0

    with h.mounted(expect_kill=True) as daemon:
        fd = os.open(h.mount / "main/crash-synced", os.O_CREAT | os.O_RDWR, 0o600)
        try:
            offset = 0
            while offset < len(payload):
                offset += os.write(fd, payload[offset:])
            os.fsync(fd)
            parent = os.open(h.mount / "main", os.O_RDONLY | os.O_DIRECTORY)
            os.fsync(parent)
            os.close(parent)
            daemon.kill()
            daemon.wait(timeout=5)
        finally:
            try:
                os.close(fd)
            except OSError as error:
                assert error.errno in (errno.ENOTCONN, errno.EIO, errno.ECONNABORTED), error
    with h.mounted():
        path = h.mount / "main/crash-synced"
        assert path.read_bytes() == payload
        h.sealed("main/crash-synced")
        path.unlink()
    crash_clean = h.snapshot("live-daemon-kill-restart")
    assert crash_clean["content"]["unique_chunks"] == 0

    sources = {}
    for view in ("main", "other"):
        parent = work / f"source-{view}"
        parent.mkdir()
        sources[view] = FIXTURE(parent)
    with h.mounted():
        h.cargo(sources["main"], "main", "build", "--workspace")
        h.executable("main", "debug", 49)
        h.cargo(sources["main"], "main", "build", "--workspace", fresh=True)
        h.cargo(sources["main"], "main", "test", "--workspace")
        h.cargo(sources["main"], "main", "check", "--workspace")
    debug = h.snapshot("debug-built")
    assert debug["sealed_files"] > 0
    assert debug["native_allocated_bytes"] < debug["logical_bytes"] // 2
    with h.mounted():
        h.cargo(sources["main"], "main", "build", "--workspace", fresh=True)
        h.cargo(sources["main"], "main", "build", "--release", "--workspace")
        h.cargo(sources["main"], "main", "test", "--release", "--workspace")
        h.cargo(sources["main"], "main", "build", "--release", "--workspace", fresh=True)
        h.executable("main", "release", 49)
    h.snapshot("release-built")

    with h.mounted():
        # Independent worktree builds share chunks while retaining distinct paths.
        def build_view(view):
            h.cargo(sources[view], view, "build", "--workspace")
            h.cargo(sources[view], view, "build", "--release", "--workspace")
            h.executable(view, "debug", 49)
            h.executable(view, "release", 49)
        with ThreadPoolExecutor(max_workers=2) as workers:
            list(workers.map(build_view, ("main", "other")))
    h.snapshot("two-worktrees-built")

    original = (sources["main"] / "app/src/main.rs").read_text()
    for edit in range(1, 11):
        source = sources["main"] / "app/src/main.rs"
        source.write_text(original.replace("const EDIT: u32 = 0;", f"const EDIT: u32 = {edit};"))
        with h.mounted():
            h.cargo(sources["main"], "main", "build", "--workspace")
            h.cargo(sources["main"], "main", "build", "--release", "--workspace")
            h.executable("main", "debug", 49 + edit)
            h.executable("main", "release", 49 + edit)
            h.executable("other", "debug", 49)
        h.snapshot(f"edit-{edit}")

    with h.mounted():
        h.cargo(sources["main"], "main", "test", "--workspace")
        h.cargo(sources["main"], "main", "test", "--release", "--workspace")
        h.cargo(sources["main"], "main", "build", "--workspace", fresh=True)
        h.cargo(sources["main"], "main", "build", "--release", "--workspace", fresh=True)
        h.cargo(sources["main"], "main", "clean", "--release")
        h.executable("main", "debug", 59)
        h.executable("other", "release", 49)
    h.snapshot("main-release-cleaned")
    with h.mounted():
        h.cargo(sources["main"], "main", "clean")
        h.executable("other", "debug", 49)
        h.executable("other", "release", 49)
    h.snapshot("main-cleaned-other-preserved")
    with h.mounted():
        h.cargo(sources["other"], "other", "clean")
    clean = h.snapshot("all-cleaned")
    assert clean["files"] == clean["logical_bytes"] == clean["native_allocated_bytes"] == 0
    assert clean["content"]["unique_chunks"] == clean["content"]["referenced_pack_bytes"] == clean["content"]["orphan_pack_bytes"] == 0
    with h.mounted():
        h.cargo(sources["main"], "main", "build", "--workspace")
        h.cargo(sources["main"], "main", "build", "--release", "--workspace")
        h.executable("main", "debug", 59)
        h.executable("main", "release", 59)
        h.cargo(sources["main"], "main", "clean")
    final = h.snapshot("rebuilt-after-clean-and-cleaned-again")
    assert final["content"]["unique_chunks"] == 0
    assert "sealing inode" not in h.log.read_text(), h.log.read_text()
    return {"status": "passed", "platform": subprocess.check_output(["uname", "-sr"], text=True).strip(),
            "probe": probe, "transition_checks": ["cross-view sharing", "metadata preservation", "hydration",
                "mapping pins native bytes", "hard links", "rename", "truncate", "unlink", "quiescent GC"],
            "edits": 10, "daemon_kill_restart": "passed", "cargo_commands": h.commands, "snapshots": h.snapshots,
            "final_unique_chunks": final["content"]["unique_chunks"]}


if __name__ == "__main__":
    with isolated_test_directory("sl7-live-test-") as work:
        print(json.dumps(run(work), indent=2))
