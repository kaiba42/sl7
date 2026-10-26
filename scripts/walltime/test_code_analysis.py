import tempfile
from pathlib import Path
import unittest

from code_analysis import coverage, phases, request


class CodeAnalysisTests(unittest.TestCase):
    def test_exclusive_accounting_rejects_missing_time(self):
        row = request('WRITE\t100\t10\tWRITE:2:1\tnative_payload:8:1\t@native_payload:1:8\n')
        self.assertEqual(row['intervals'], [('native_payload', 101, 109)])
        with self.assertRaises(AssertionError):
            request('READ\t0\t10\tREAD:9:1\n')
        with self.assertRaises(AssertionError):
            request('READ\t0\t10\tREAD:10:1\t@reply:9:2\n')

    def test_overlap_merges_owners_and_clips_wait_intervals(self):
        overlap = coverage([(3, 7), (5, 9), (12, 14)])
        self.assertEqual(overlap(0, 20), 8)
        self.assertEqual(overlap(6, 13), 4)
        self.assertEqual(overlap(9, 12), 0)
        self.assertEqual(coverage([])(1, 9), 0)

    def test_phases_exclude_post_build_and_measure_directory_contention(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'phases').mkdir()
            (root / 'phases/worker.tsv').write_text(
                'OPENDIR\t100\t20\tOPENDIR:0:1\tstate_locked_work:20:1\t@state_locked_work:0:20\n'
                'READ\t105\t20\tREAD:0:1\tstate_lock_wait:15:1\tnative_payload:5:1\t@state_lock_wait:0:15\n'
                'WRITE\t115\t20\tWRITE:0:1\tstate_lock_wait:5:1\tnative_payload:15:1\t@state_lock_wait:0:5\n'
                'READ\t200\t10\tREAD:10:1\n')
            result = phases(root, dict(start_ns=100, end_ns=150))
        self.assertEqual(result['excluded_outside_build'], 1)
        self.assertEqual(result['operations']['READ']['state_wait_overlapping_opendir_hold_seconds']['state_lock_wait'], 15e-9)
        self.assertEqual(result['operations']['WRITE']['count'], 1)


if __name__ == '__main__':
    unittest.main()
