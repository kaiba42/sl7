"""Quiescence must follow filesystem state, not an idle process CPU counter."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    'mount_helpers', Path(__file__).resolve().parents[1] / 'scripts/mount_helpers.py')
helpers = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helpers)


def status(quiet, completed=0, failed=False):
    return dict(quiescent=quiet, worker_failed=failed, stopping=False,
                sealing=dict(completed=completed, canceled=0, skipped=0,
                             write_operations=0, read_operations=0,
                             sync_operations=0, namespace_commits=0, errors=0))


class QuiescenceTests(unittest.TestCase):
    def test_busy_worker_and_new_work_reset_quiet_observations(self):
        sequence = [status(False), status(True), status(True, 1), status(True, 1), status(True, 1)]
        with patch.object(helpers, 'mount_status', side_effect=sequence), \
             patch.object(helpers.time, 'sleep'), \
             patch.object(helpers.time, 'monotonic', side_effect=[0, .1, .2, .3, .4, .5]):
            result = helpers.wait_for_quiescence(Mock(poll=Mock(return_value=None)), '/mount')
        self.assertEqual(result['polls'], 5)
        self.assertEqual(result['wait_seconds'], .5)
        self.assertEqual(result['status']['sealing']['completed'], 1)

    def test_waiting_for_io_cannot_be_accepted_as_quiet(self):
        with patch.object(helpers, 'mount_status', return_value=status(False)), \
             patch.object(helpers.time, 'monotonic', side_effect=[0, 181]):
            with self.assertRaisesRegex(RuntimeError, 'did not become quiescent'):
                helpers.wait_for_quiescence(Mock(poll=Mock(return_value=None)), '/mount')

    def test_worker_failure_and_process_exit_are_errors(self):
        with patch.object(helpers, 'mount_status', return_value=status(True, failed=True)):
            with self.assertRaisesRegex(RuntimeError, 'failed while draining'):
                helpers.wait_for_quiescence(Mock(poll=Mock(return_value=None)), '/mount')
        with self.assertRaisesRegex(RuntimeError, 'SL7 exited'):
            helpers.wait_for_quiescence(Mock(poll=Mock(return_value=0)), '/mount')

    def test_unknown_status_schema_is_rejected(self):
        with patch.object(helpers.os, 'getxattr', return_value=b'{"schema_version":2}', create=True):
            with self.assertRaisesRegex(RuntimeError, 'Unsupported'):
                helpers.mount_status('/mount')


if __name__ == '__main__':
    unittest.main()
