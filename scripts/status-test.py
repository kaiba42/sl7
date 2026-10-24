#!/usr/bin/env python3
"""Mounted status must distinguish idle CPU, open files and drained sealing."""
import ctypes
import errno
import json
import os
from pathlib import Path
import runpy
from mount_helpers import isolated_test_directory, mount_status, wait_for_quiescence

Harness = runpy.run_path(str(Path(__file__).with_name('live-test.py')))['Harness']


def run(work):
    h = Harness(work)
    with h.mounted() as daemon:
        try:
            os.getxattr(h.mount, 'user.sl7.unknown')
            raise AssertionError('unknown attribute was accepted')
        except OSError as error:
            assert error.errno == errno.ENODATA
        initial = wait_for_quiescence(daemon, h.mount, timeout=15)
        path = h.mount / 'data'
        payload = bytes(range(256)) * (80 * 1024)
        with path.open('wb', buffering=0) as writer:
            assert writer.write(payload) == len(payload)
            held = mount_status(h.mount)
            assert held['open_handles'] > 0 and not held['quiescent']
        sealed = wait_for_quiescence(daemon, h.mount, timeout=15)
        final = sealed['status']
        assert final['quiescent'] and final['drained']
        assert final['queued_inodes'] == final['queue_entries'] == final['open_handles'] == 0
        assert final['sealing']['completed'] >= 1
        assert (h.store / 'tree/data').stat().st_blocks == 0
        assert path.read_bytes() == payload
        wait_for_quiescence(daemon, h.mount, timeout=15)
        # Exercise FUSE's size query and too-small-buffer replies directly.
        libc = ctypes.CDLL(None, use_errno=True)
        libc.getxattr.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_void_p, ctypes.c_size_t]
        libc.getxattr.restype = ctypes.c_ssize_t
        args = (os.fsencode(h.mount), b'user.sl7.status')
        assert libc.getxattr(*args, None, 0) > 1
        buffer = ctypes.create_string_buffer(1)
        assert libc.getxattr(*args, buffer, 1) == -1 and ctypes.get_errno() == errno.ERANGE
        assert mount_status(h.mount)['schema_version'] == 1
    h.snapshot('status-observation')
    return dict(status='passed',initial=initial,held=held,sealed=sealed,
                checks=['unknown xattr does not disable status','open handle blocks quiescence',
                        'closed handle drains sealing','cold reconstruction',
                        'size query and ERANGE','store integrity and GC'])


if __name__ == '__main__':
    os.environ['SL7_MOUNT_STATUS'] = '1'
    with isolated_test_directory('sl7-status-test-') as work:
        print(json.dumps(run(work), indent=2))
