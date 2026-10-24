#!/usr/bin/env python3
"""Large-file mounted qualification for the opt-in background import pipeline."""
import json
import mmap
import os
from pathlib import Path
import random
import runpy
from mount_helpers import isolated_test_directory

Harness = runpy.run_path(str(Path(__file__).with_name('live-test.py')))['Harness']


def run(work):
    h = Harness(work)
    payload = random.Random(42).randbytes(256 * 1024) * 80
    expected = bytearray(payload)
    with h.mounted():
        path = h.mount / 'large'
        path.write_bytes(payload)
        h.sealed('large')
        assert path.read_bytes() == payload
    with h.mounted():
        path = h.mount / 'large'
        fd = os.open(path, os.O_RDONLY)
        mapping = mmap.mmap(fd, 0, access=mmap.ACCESS_READ)
        os.close(fd)
        fd = os.open(path, os.O_WRONLY)
        try:
            for offset, data in [(4094, b'across-page'), (len(payload)//2+123, b'edited')]:
                assert os.pwrite(fd, data, offset) == len(data)
                expected[offset:offset+len(data)] = data
            os.fsync(fd)
        finally:
            os.close(fd)
        assert mapping[:] == expected
        assert (h.store / 'tree/large').stat().st_blocks > 0
        mapping.close()
        h.sealed('large')
        assert path.read_bytes() == expected
    h.snapshot('large-sealed-and-edited')
    # Explicitly synced native bytes must survive daemon death even if a
    # producer is preparing an uncommitted manifest at the time of the kill.
    with h.mounted(expect_kill=True) as daemon:
        with (h.mount / 'durable').open('wb', buffering=0) as file:
            assert file.write(expected) == len(expected)
            os.fsync(file.fileno())
        daemon.kill()
        daemon.wait(timeout=10)
    with h.mounted():
        assert (h.mount / 'large').read_bytes() == expected
        assert (h.mount / 'durable').read_bytes() == expected
        h.sealed('durable')
    h.snapshot('recovered')
    counters = [json.loads(line.split('sl7: sealing_stats ', 1)[1])
                for line in h.log.read_text().splitlines() if 'sl7: sealing_stats ' in line]
    enabled = os.environ.get('SL7_IMPORT_PIPELINE', '0') == '1'
    assert counters and all(c['import_pipeline'] == enabled for c in counters)
    assert all(c['errors'] == c['active_prepares'] == c['active_seals'] == 0 for c in counters)
    attempts = sum(c['pipelined_prepares'] for c in counters)
    assert attempts >= 2 if enabled else attempts == 0
    assert 'sealing inode' not in h.log.read_text(), h.log.read_text()
    return dict(status='passed',pipeline=enabled,pipelined_prepares=attempts,
                bytes_per_file=len(payload),snapshots=h.snapshots,counters=counters,
                checks=['cold reconstruction','mmap outlives descriptor','mutation then reseal',
                        'fsync then daemon death','remount recovery','store integrity and GC'])


if __name__ == '__main__':
    os.environ['SL7_CACHE_STATS'] = '1'
    with isolated_test_directory('sl7-pipeline-test-') as work:
        print(json.dumps(run(work), indent=2))
