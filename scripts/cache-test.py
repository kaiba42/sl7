#!/usr/bin/env python3
"""Check live-mount kernel cache retention and coherence through real syscalls."""
import json
import mmap
import os
from pathlib import Path
import random
import runpy

from mount_helpers import isolated_test_directory

Harness = runpy.run_path(str(Path(__file__).with_name('live-test.py')))['Harness']


def run(work):
    os.environ['SL7_CACHE_STATS'] = '1'
    h = Harness(work)
    payload = random.Random(2718).randbytes(1024 * 1024)
    with h.mounted():
        (h.mount/'artifact').write_bytes(payload)
        h.sealed('artifact')

    def reads(count):
        start = len(h.log.read_text())
        with h.mounted():
            for _ in range(count):
                # Close each descriptor: retention must survive a fresh open.
                assert (h.mount/'artifact').read_bytes() == payload
        lines = h.log.read_text()[start:].splitlines()
        stats = {}
        for kind in ('sealing_stats', 'cache_stats'):
            prefix = f'sl7: {kind} '
            rows = [json.loads(line[len(prefix):]) for line in lines if line.startswith(prefix)]
            assert len(rows) == 1, (kind, lines)
            stats[kind] = rows[0]
        assert stats['sealing_stats']['errors'] == 0
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
                coherence=['same-size write with restored mtime', 'mmap writes',
                           'hard-link alias', 'rename replacement with live mapping',
                           'truncate and regrow', 'create existing', 'truncate on open',
                           'sealing and hydration', 'remount'])


if __name__ == '__main__':
    with isolated_test_directory('sl7-cache-') as work:
        print(json.dumps(run(work), indent=2))
