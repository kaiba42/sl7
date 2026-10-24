#!/usr/bin/env python3
"""Check live-mount kernel cache retention and coherence through real syscalls."""
import json
import mmap
import os
from pathlib import Path
import random
import runpy
import time

from mount_helpers import isolated_test_directory

Harness = runpy.run_path(str(Path(__file__).with_name('live-test.py')))['Harness']


def run(work):
    os.environ['SL7_CACHE_STATS'] = '1'
    h = Harness(work)
    payload = random.Random(2718).randbytes(1024 * 1024)
    with h.mounted():
        path = h.mount/'artifact'
        with path.open('wb', buffering=0) as writer:
            writer.write(payload)
            assert path.stat().st_blocks > 0
        h.sealed('artifact')
        # Background retirement changes allocation, not logical metadata. Its
        # cached block count must expire within the documented short timeout.
        deadline = time.monotonic() + 3
        while path.stat().st_blocks != 0:
            assert time.monotonic() < deadline, 'allocation attributes did not expire'
            time.sleep(.02)

    def counters(start):
        lines = h.log.read_text()[start:].splitlines()
        stats = {}
        for kind in ('sealing_stats', 'cache_stats'):
            prefix = f'sl7: {kind} '
            rows = [json.loads(line[len(prefix):]) for line in lines if line.startswith(prefix)]
            assert len(rows) == 1, (kind, lines)
            stats[kind] = rows[0]
        assert stats['sealing_stats']['errors'] == 0
        return stats

    def reads(count):
        start = len(h.log.read_text())
        with h.mounted():
            for _ in range(count):
                # Close each descriptor: retention must survive a fresh open.
                assert (h.mount/'artifact').read_bytes() == payload
        stats = counters(start)
        return dict(read_operations=stats['sealing_stats']['read_operations'],
                    reconstructed_bytes=stats['cache_stats']['requested_bytes'])

    single, repeated = reads(1), reads(8)
    # Every measurement starts with a new mount and cold kernel pages. A MiB
    # fixture fits comfortably in memory; request batching can vary slightly.
    assert single['read_operations'] > 0 and single['reconstructed_bytes'] >= len(payload)
    assert repeated['read_operations'] <= single['read_operations'] + 2, (
        'reopen discarded cached pages', single, repeated)
    assert repeated['reconstructed_bytes'] <= single['reconstructed_bytes'] + 8192, (
        'reopen reconstructed cached bytes', single, repeated)

    def attributes(count):
        start = len(h.log.read_text())
        with h.mounted():
            started = time.monotonic()
            for _ in range(count):
                assert (h.mount/'artifact').stat().st_size == len(payload)
            elapsed = time.monotonic() - started
        stats = counters(start)['sealing_stats']
        # Make sure the probe itself fits within one validity interval.
        assert elapsed < .5, ('metadata probe exceeded its time window', elapsed)
        return dict(lookups=stats['lookup_operations'], getattrs=stats['getattr_operations'],
                    elapsed_seconds=elapsed)

    single_stat, repeated_stat = attributes(1), attributes(128)
    for key in ('lookups', 'getattrs'):
        assert repeated_stat[key] <= single_stat[key] + 2, (
            'metadata cache did not retain attributes', key, single_stat, repeated_stat)

    with h.mounted():
        directory = h.mount/'metadata'
        directory.mkdir()
        native_directory = h.store/'tree/metadata'
        stamp = 1_600_000_000_123_456_789
        os.utime(directory, ns=(stamp, stamp))
        parent_links = directory.stat().st_nlink
        assert parent_links == native_directory.stat().st_nlink
        assert directory.stat().st_mtime_ns == stamp
        child = directory/'child'
        child.write_bytes(b'initial')
        assert directory.stat().st_mtime_ns != stamp
        ino = child.stat().st_ino
        child.chmod(0o640)
        assert child.stat().st_mode & 0o777 == 0o640
        os.utime(child, ns=(stamp, stamp))
        assert child.stat().st_mtime_ns == stamp
        with child.open('r+b', buffering=0) as writer:
            writer.write(b'changed')
            os.fsync(writer.fileno())
        assert child.stat().st_mtime_ns != stamp
        alias = directory/'link'
        os.link(child, alias)
        assert child.stat().st_nlink == alias.stat().st_nlink == 2
        alias.unlink()
        assert child.stat().st_nlink == 1
        moved = directory/'moved'
        child.rename(moved)
        assert not child.exists() and moved.stat().st_ino == ino
        child.write_bytes(b'replaced')
        replacement_ino = child.stat().st_ino
        child.replace(moved)
        assert moved.stat().st_ino == replacement_ino and moved.read_bytes() == b'replaced'
        subdir = directory/'subdir'
        assert not subdir.exists()
        os.utime(directory, ns=(stamp, stamp))
        assert directory.stat().st_mtime_ns == stamp
        subdir.mkdir()
        # Some backing filesystems report a constant directory link count (1),
        # so compare against native metadata rather than assuming 2 + subdirs.
        assert directory.stat().st_nlink == native_directory.stat().st_nlink
        assert directory.stat().st_mtime_ns == native_directory.stat().st_mtime_ns != stamp
        os.utime(directory, ns=(stamp, stamp))
        assert directory.stat().st_mtime_ns == stamp
        subdir.rmdir()
        assert not subdir.exists() and directory.stat().st_nlink == parent_links
        assert directory.stat().st_nlink == native_directory.stat().st_nlink
        assert directory.stat().st_mtime_ns == native_directory.stat().st_mtime_ns != stamp

    expected = bytearray(payload)
    replacement = bytes(b ^ 0xFF for b in payload)
    with h.mounted():
        path, alias = h.mount/'artifact', h.mount/'alias'
        assert path.read_bytes() == payload
        stamp = path.stat().st_mtime_ns
        os.link(path, alias)
        fd = os.open(path, os.O_RDONLY)
        mapping = mmap.mmap(fd, 0, access=mmap.ACCESS_READ)
        os.close(fd)
        try:
            # Same-size writes with the old timestamp restored must still be
            # visible through cached reads, mappings, aliases, and fresh opens.
            with alias.open('r+b', buffering=0) as writer:
                os.pwrite(writer.fileno(), b'updated!', 8190)
                expected[8190:8198] = b'updated!'
                os.utime(alias, ns=(stamp, stamp))
                assert path.read_bytes() == expected
                assert mapping[:] == expected
                with mmap.mmap(writer.fileno(), 0) as writable:
                    writable[16382:16390] = b'mapwrite'
                    writable.flush()
                    expected[16382:16390] = b'mapwrite'
                os.fsync(writer.fileno())
            assert alias.read_bytes() == expected
            assert mapping[:] == expected
            candidate = h.mount/'replacement'
            candidate.write_bytes(replacement)
            candidate.replace(path)
            assert path.read_bytes() == replacement
            assert alias.read_bytes() == expected
            assert mapping[:] == expected
            # Shrink/regrow must not resurrect cached bytes from the old tail.
            with alias.open('r+b', buffering=0) as writer:
                os.ftruncate(writer.fileno(), 4099)
                os.ftruncate(writer.fileno(), len(payload))
                os.fsync(writer.fileno())
            expected[4099:] = bytes(len(payload) - 4099)
            assert alias.read_bytes() == expected
            assert mapping[:] == expected
        finally:
            mapping.close()
        h.sealed('alias')
        assert alias.read_bytes() == expected
        # O_CREAT on an existing inode and O_TRUNC must both preserve coherence.
        fd = os.open(alias, os.O_CREAT | os.O_RDWR, 0o600)
        assert os.pread(fd, len(payload), 0) == expected
        os.close(fd)
        alias.write_bytes(replacement)
        os.utime(alias, ns=(stamp, stamp))
        assert alias.read_bytes() == replacement
        h.sealed('alias')
        assert alias.read_bytes() == replacement
    h.sl7('check')
    with h.mounted():
        assert (h.mount/'artifact').read_bytes() == replacement
        assert (h.mount/'alias').read_bytes() == replacement
    h.sl7('check')
    return dict(status='passed', single_open=single, eight_opens=repeated,
                single_stat=single_stat, repeated_stat=repeated_stat,
                metadata_coherence=['chmod', 'utime', 'write mtime', 'parent mtime',
                                    'link count', 'rename replacement', 'mkdir/rmdir',
                                    'background allocation expiry'],
                coherence=['same-size write with restored mtime', 'mmap writes',
                           'hard-link alias', 'rename replacement with live mapping',
                           'truncate and regrow', 'create existing', 'truncate on open',
                           'sealing and hydration', 'remount'])


if __name__ == '__main__':
    with isolated_test_directory('sl7-cache-') as work:
        print(json.dumps(run(work), indent=2))
