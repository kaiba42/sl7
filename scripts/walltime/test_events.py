import unittest
from pathlib import Path
import tempfile
from events import Span, partition, requests, scheduler, totals
from analyze import last_unblock_chain, package_identity
from cpu import summarize


def event(kind, seconds, *fields):
    return [kind, str(int(seconds * 1e9)), *map(str, fields)]


class AccountingTests(unittest.TestCase):
    def test_cargo_package_identity_accepts_version_only_path_fragment(self):
        self.assertEqual(package_identity('registry+https://example.invalid/index#foo@1.2.3'), ('foo', '1.2.3'))
        self.assertEqual(package_identity('path+file:///source/my%20app#1.2.3'), ('my app', '1.2.3'))

    def test_cpu_samples_exclude_settling_and_redact_symbols(self):
        def sample(pid, when, symbol, dso):
            return f'worker {pid}/{pid} {when}: cpu-clock:u: \n\t123 {symbol} ({dso})\n\n'
        text = sample(1, '1.000000001', 'private_function', '/private/program') * 101
        text += sample(2, '1.000000002', 'sha2::digest', '/bench/bin/sl7')
        text += sample(2, '2.000000001', 'sha2::digest', '/bench/bin/sl7')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'cpu.txt'
            path.write_text(text)
            result = summarize(path, 10**9, 2 * 10**9, 'sl7')
        self.assertEqual(result['outside_build_window'], 1)
        self.assertEqual(result['groups']['build']['samples'], 101)
        self.assertEqual(result['groups']['daemon']['inclusive_work']['sha256'], 1)
        self.assertNotIn('private_function', str(result))

    def test_scheduler_separates_cpu_sleep_and_runqueue_delay(self):
        rows = [event('READY', 0, 'pointer-v3'), event('E', 1, 1, 1, 1, 'cargo', '/cargo'),
                event('F', 2, 1, 2, 1), event('W', 3, 1, 2, 1), event('I', 4, 1, 2),
                event('M', 5, 1, 2, 2, 'sl7u_2'), event('O', 6, 1, 1, 1, 7, 0),
                event('O', 8, 1, 2, 1, 0, 1), event('W', 12, 1, 2, 9),
                event('I', 15, 1, 2), event('X', 20, 1, 2), event('W', 21, 1, 1, 9),
                event('I', 22, 1, 1), event('X', 23, 1, 1), event('END', 24)]
        spans, _, anomalies = scheduler(rows)
        self.assertEqual(anomalies, {})
        self.assertEqual(totals(spans, 5 * 10**9, 20 * 10**9, unit='sl7u_2'),
                         {'running': 8., 'blocked_fuse': 4., 'runnable': 3.})
        wall = partition(spans, 5 * 10**9, 20 * 10**9, unit='sl7u_2')
        self.assertEqual(wall, {'at_least_one_thread_running': 8., 'no_runnable_thread_with_fuse_wait': 4.,
                                'runnable_but_no_thread_running': 3.})

    def test_overlap_is_not_added_to_elapsed_time(self):
        spans = [Span(0, 10**9, 1, 1, 'running', 'u', 'compiler'),
                 Span(0, 10**9, 2, 1, 'blocked_fuse', 'u', 'compiler')]
        self.assertEqual(sum(totals(spans, 0, 10**9).values()), 2.)
        self.assertEqual(partition(spans, 0, 10**9), {'at_least_one_thread_running': 1.})

    def test_pointer_reuse_and_callback_order(self):
        rows = [event('P', 1, 8, 8, 123), event('S', 1.000001, 8, 8, 0, 15, 99),
                event('D', 2, 123, 0, 0), event('S', 3, 8, 8, 0, 16, 99),
                event('P', 3.000001, 8, 8, 123), event('D', 4, 123, 0, 0)]
        paired, audit = requests(rows)
        self.assertEqual([row['opcode'] for row in paired], [15, 16])
        self.assertEqual(audit['paired'], 2)
        self.assertLessEqual(audit['max_send_callback_gap_ns'], 1001)
        with self.assertRaises(AssertionError):
            requests(rows[:-1])

    def test_missing_wakeup_is_explicitly_unresolved(self):
        rows = [event('READY', 0, 'pointer-v3'), event('E', 1, 1, 1, 1, 'cargo', '/cargo'),
                event('O', 2, 1, 1, 1, 0, 1), event('I', 5, 1, 1), event('X', 6, 1, 1), event('END', 7)]
        spans, _, anomalies = scheduler(rows)
        self.assertEqual(anomalies['switch_in_without_wakeup'], 1)
        self.assertEqual(totals(spans, 0, 7 * 10**9)['offcpu_unresolved'], 3.)
        self.assertNotIn('blocked_fuse', totals(spans, 0, 7 * 10**9))

    def test_last_unblock_chain_respects_pipelined_metadata(self):
        units = [dict(i=0, name='dep', version='1', target='', features=[], start=0., duration=10.,
                      unblocked_units=[], unblocked_rmeta_units=[1], sections=[['frontend', {'end': 4.}]]),
                 dict(i=1, name='app', version='1', target='', features=[], start=5., duration=3.,
                      unblocked_units=[2], unblocked_rmeta_units=[], sections=None),
                 dict(i=2, name='app', version='1', target='bin', features=[], start=8., duration=2.,
                      unblocked_units=[], unblocked_rmeta_units=[], sections=None)]
        chain = last_unblock_chain(units, 10.1, {'app'})
        self.assertEqual([(s['kind'], round(s['seconds'], 3)) for s in chain],
                         [('dependency_compile', 4.), ('before_or_between_chain_units', 1.),
                          ('workspace_compile', 3.), ('workspace_compile', 2.), ('cargo_boundary_remainder', .1)])


if __name__ == '__main__':
    unittest.main()
