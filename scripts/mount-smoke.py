#!/usr/bin/env python3
"""Verify reads through an already-running SL7 read-only mount.

Usage: python3 scripts/mount-smoke.py MOUNTPOINT SOURCE VIEW/ARTIFACT
Does not mount, import, delete, or modify files.
"""
import argparse
import errno
import hashlib
import json
import mmap
import os
from pathlib import Path
import random
import stat


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(mountpoint, source, relative):
    mountpoint = mountpoint.resolve(strict=True)
    if not os.path.ismount(mountpoint):
        raise ValueError(f"not a mounted filesystem: {mountpoint}")
    if not os.statvfs(mountpoint).f_flag & os.ST_RDONLY:
        raise ValueError("expected a read-only mount")
    if relative.is_absolute() or ".." in relative.parts or not relative.parts:
        raise ValueError("artifact must be a relative path inside the mount")
    artifact = (mountpoint / relative).resolve(strict=True)
    artifact.relative_to(mountpoint)
    source = source.resolve(strict=True)
    if source.is_relative_to(mountpoint):
        raise ValueError("reference source must be outside the mount")
    expected = digest(source)
    info = artifact.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_size != source.stat().st_size:
        raise AssertionError("artifact type or length differs")
    if digest(artifact) != expected:
        raise AssertionError("whole-file checksum differs")
    if artifact.name not in os.listdir(artifact.parent):
        raise AssertionError("directory enumeration omitted artifact")
    rng = random.Random(7)
    with source.open("rb") as reference, artifact.open("rb") as mounted:
        for _ in range(100):
            offset = rng.randrange(info.st_size + 4097)
            length = rng.choice([0, 1, 4096, 32768, 131072])
            if os.pread(reference.fileno(), length, offset) != os.pread(mounted.fileno(), length, offset):
                raise AssertionError(f"pread differs at {offset}+{length}")
        if info.st_size:
            with mmap.mmap(mounted.fileno(), 0, access=mmap.ACCESS_READ) as mapping:
                if hashlib.sha256(mapping).hexdigest() != expected:
                    raise AssertionError("mmap checksum differs")
    try:
        descriptor = os.open(artifact, os.O_RDWR)
    except OSError as error:
        if error.errno not in (errno.EROFS, errno.EACCES):
            raise
    else:
        os.close(descriptor)
        raise AssertionError("read-only mount accepted writable open")
    if digest(source) != expected:
        raise AssertionError("reference source changed during verification")
    return {"mountpoint": str(mountpoint), "artifact": str(relative),
            "size": info.st_size, "sha256": expected, "random_reads": 100,
            "read_mmap": "passed" if info.st_size else "skipped: empty file",
            "writable_open": "rejected", "source_unchanged": True}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mountpoint", type=Path)
    parser.add_argument("source", type=Path)
    parser.add_argument("artifact", type=Path)
    args = parser.parse_args()
    print(json.dumps(run(args.mountpoint, args.source, args.artifact), indent=2))
