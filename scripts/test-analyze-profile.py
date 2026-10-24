#!/usr/bin/env python3
"""Exercise syscall timing attribution, boundaries, and invalid trace rejection."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('analysis', Path(__file__).with_name('analyze-profile.py'))
analysis = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analysis)


class IoTraceTests(unittest.TestCase):
    def parse(self, text):
        with tempfile.TemporaryDirectory() as work:
            path = Path(work)/'trace.txt'
            path.write_text(text)
            return analysis.io_latencies(path)

    def test_interleaved_threads_and_cut_boundary_calls(self):
        result = self.parse('''fuser-1 3 0.050000000: syscalls:sys_exit_fsync: ret: 0x0
fuser-0 1 0.100000000: syscalls:sys_enter_fsync: fd: 0x6
sl7-pack 2 0.200000000: syscalls:sys_enter_fdatasync: fd: 0x7
fuser-0 1 0.400000000: syscalls:sys_exit_fsync: ret: 0
sl7-pack 2 0.800000000: syscalls:sys_exit_fdatasync: ret: 0xfffffffffffffffb
fuser-0 1 0.900000000: syscalls:sys_enter_pwrite64: fd: 0x6
fuser-2 4 0.950000000: syscalls:sys_enter_fsync: fd: 0x8
fuser-0 1 1.100000000: syscalls:sys_exit_pwrite64: ret: 0x1000
''')
        self.assertAlmostEqual(result['operations']['foreground/fsync']['sum_seconds'], .3)
        self.assertAlmostEqual(result['operations']['background/fdatasync']['sum_seconds'], .6)
        self.assertAlmostEqual(result['operations']['foreground/pwrite64']['sum_seconds'], .2)
        self.assertEqual(result['incomplete'], {'missing_enter': 1, 'missing_exit': 1})
        self.assertEqual(result['errors'], {'background/fdatasync': 1})
        self.assertEqual(sum(x['count'] for x in result['operations'].values()), 3)

    def test_perf_pretty_printed_return(self):
        result = self.parse('fuser-3 130963 1.000000000: syscalls:sys_enter_pwrite64: fd: 3\nfuser-3 130963 1.100000000: syscalls:sys_exit_pwrite64: 0x18\n')
        self.assertAlmostEqual(result['operations']['foreground/pwrite64']['sum_seconds'], .1)
        self.assertEqual(result['errors'], {})
        self.assertEqual(result['descriptor_calls'], {'foreground/pwrite64': {'3': 1}})

    def test_rejects_mismatched_exit(self):
        with self.assertRaises(AssertionError):
            self.parse('''fuser-0 1 1.0: syscalls:sys_enter_fsync: fd: 3
fuser-0 1 2.0: syscalls:sys_exit_pwrite64: ret: 0
''')

    def test_counts_overlapping_foreground_syscalls(self):
        result = self.parse('''fuser-0 1 0.1: syscalls:sys_enter_pwrite64: fd: 3
fuser-1 2 0.2: syscalls:sys_enter_pwrite64: fd: 4
fuser-0 1 0.4: syscalls:sys_exit_pwrite64: 0x10
fuser-1 2 0.8: syscalls:sys_exit_pwrite64: 0x10
''')
        self.assertEqual(result['overlap']['foreground']['max_concurrent_syscalls'], 2)
        self.assertAlmostEqual(result['overlap']['foreground']['sum_seconds'], .9)
        self.assertAlmostEqual(result['overlap']['foreground']['union_seconds'], .7)

    def test_rejects_lost_exit_inside_trace(self):
        with self.assertRaises(AssertionError):
            self.parse('''fuser-0 1 1.0: syscalls:sys_enter_fsync: fd: 3
fuser-0 1 2.0: syscalls:sys_enter_fsync: fd: 3
''')

    def test_rejects_unknown_format(self):
        with self.assertRaises(AssertionError):
            self.parse('unrecognized trace output\n')


if __name__ == '__main__':
    unittest.main()
