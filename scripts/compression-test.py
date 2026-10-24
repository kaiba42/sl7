#!/usr/bin/env python3
"""Qualify parallel compression on novel, reused, and edited mounted artifacts."""
import hashlib
import json
import os
from pathlib import Path
import random
import runpy
from mount_helpers import isolated_test_directory, wait_for_quiescence

Harness = runpy.run_path(str(Path(__file__).with_name('live-test.py')))['Harness']


def run(work):
    h = Harness(work)
    # Unique, partly compressible content exceeds the large-file threshold.
    source = random.Random(42).randbytes(10 * 1024 * 1024)
    payload = b''.join(source[i:i + 512] * 2 for i in range(0, len(source), 512))
    with h.mounted() as daemon:
        original = h.mount / 'original'
        original.write_bytes(payload)
        first = wait_for_quiescence(daemon, h.mount)['status']['sealing']
        assert first['compression_workers_per_sealer'] == 2
        assert first['compressed_chunks_on_workers'] > 100
        assert 1 <= first['max_parallel_compression_per_sealer'] <= 2
        assert (h.store / 'tree/original').stat().st_blocks == 0
        duplicate = h.mount / 'duplicate'
        duplicate.write_bytes(payload)
        reused = wait_for_quiescence(daemon, h.mount)['status']['sealing']
        assert reused['compressed_chunks_on_workers'] == first['compressed_chunks_on_workers']
        assert duplicate.read_bytes() == payload
        wait_for_quiescence(daemon, h.mount)
        changed = bytearray(payload)
        changed[len(changed) // 2 + 123] ^= 1
        with original.open('r+b', buffering=0) as writer:
            writer.seek(len(changed) // 2 + 123)
            writer.write(changed[len(changed) // 2 + 123:len(changed) // 2 + 124])
        edited = wait_for_quiescence(daemon, h.mount)['status']['sealing']
        assert first['compressed_chunks_on_workers'] < edited['compressed_chunks_on_workers']
        assert edited['compressed_chunks_on_workers'] < 2 * first['compressed_chunks_on_workers']
        assert original.read_bytes() == changed
        assert duplicate.read_bytes() == payload
        wait_for_quiescence(daemon, h.mount)
    h.snapshot('parallel-compression')
    return dict(status='passed', payload_bytes=len(payload),
                payload_sha256=hashlib.sha256(payload).hexdigest(),
                first=first, reused=reused, edited=edited,
                checks=['novel chunks use compression workers',
                        'duplicate chunks bypass compression',
                        'one-byte edit reuses chunks',
                        'exact reconstruction and sparse retirement',
                        'drained work before unmount', 'store integrity and GC'])


if __name__ == '__main__':
    os.environ['SL7_MOUNT_STATUS'] = '1'
    os.environ['SL7_COMPRESS_WORKERS'] = '2'
    with isolated_test_directory('sl7-compression-test-') as work:
        print(json.dumps(run(work), indent=2))
