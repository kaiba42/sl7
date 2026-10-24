"""Validate syscall pairing and exact-duplicate accounting in saved perf traces."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import sys

spec = importlib.util.spec_from_file_location(
    "profile_analysis", Path(__file__).resolve().parents[1] / "scripts/analyze-profile.py")
analysis = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analysis)

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
try:
    spec = importlib.util.spec_from_file_location(
        "profile_synkro", Path(__file__).resolve().parents[1] / "scripts/profile-synkro.py")
    profiler = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(profiler)
finally:
    sys.path.pop(0)


class IoTraceTests(unittest.TestCase):
    def analyze(self, lines):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "io.txt"
            path.write_text("\n".join(lines) + "\n")
            return analysis.io_latencies(path)

    def test_exact_duplicates_preserve_overlap_and_counts(self):
        enter = "sl7-pack-0 10 1.000000001: syscalls:sys_enter_fsync: fd: 5"
        leave = "sl7-pack-0 10 1.300000001: syscalls:sys_exit_fsync: ret: 0"
        result = self.analyze([
            enter, enter,
            "sl7-pack-1 11 1.100000001: syscalls:sys_enter_fsync: fd: 6",
            leave, leave,
            "sl7-pack-1 11 1.400000001: syscalls:sys_exit_fsync: ret: 0",
            "sl7-pack 12 1.500000001: syscalls:sys_enter_fsync: fd: 7",
            "sl7-pack 12 1.600000001: syscalls:sys_exit_fsync: ret: 0",
        ])
        self.assertEqual(result["exact_duplicate_events"],
                         {"background/fsync/enter": 1, "background/fsync/exit": 1})
        self.assertEqual(result["events"], {"enter": 3, "exit": 3})
        self.assertEqual(result["operations"]["background/fsync"]["count"], 3)
        self.assertEqual(result["overlap"]["background"]["max_concurrent_syscalls"], 2)
        self.assertFalse(result["errors"])

    def test_conflicting_entry_is_not_silently_discarded(self):
        with self.assertRaisesRegex(AssertionError, "overlapping syscalls"):
            self.analyze([
                "sl7-pack 10 1.000000001: syscalls:sys_enter_fsync: fd: 5",
                "sl7-pack 10 1.000000001: syscalls:sys_enter_fsync: fd: 6",
            ])

    def test_boundary_calls_and_errors_remain_visible(self):
        result = self.analyze([
            "fuser-0 10 1.0: syscalls:sys_exit_fsync: ret: 0",
            "fuser-0 10 1.1: syscalls:sys_enter_fsync: fd: 5",
            "fuser-0 10 1.2: syscalls:sys_exit_fsync: ret: -5",
            "fuser-0 10 1.3: syscalls:sys_enter_fsync: fd: 5",
        ])
        self.assertEqual(result["incomplete"], {"missing_enter": 1, "missing_exit": 1})
        self.assertEqual(result["errors"], {"foreground/fsync": 1})


class CpuCounterTests(unittest.TestCase):
    def test_process_name_parentheses_do_not_shift_cpu_fields(self):
        # /proc/PID/stat field 2 can contain spaces and parentheses. Fields
        # 14/15 are process-wide user/system ticks; neither is child CPU time.
        fields = ['S'] + ['0'] * 10 + ['250', '75', '900', '800']
        record = '123 (sl7 (worker)) ' + ' '.join(fields)
        with patch.object(profiler.Path, 'read_text', return_value=record), \
             patch.object(profiler.os, 'sysconf', return_value=100):
            self.assertEqual(profiler.daemon_cpu_breakdown(123),
                             {'user': 2.5, 'system': 0.75})


if __name__ == "__main__":
    unittest.main()
