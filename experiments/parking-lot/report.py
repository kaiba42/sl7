#!/usr/bin/env python3
"""Render the reviewed October 25 capture; new captures need new conclusions."""
import argparse
import hashlib
import html
import json
import re
import statistics
from pathlib import Path

ARMS = ('native', 'baseline-sl7', 'directories', 'parking')
LABELS = ('Native Cargo', 'SL7 before', 'SL7 directory snapshots', '+ parking_lot 0.13')


def graph(path):
    return sorted(json.dumps({key: row[key] for key in ('package_id', 'target', 'profile', 'features', 'fresh')}, sort_keys=True)
                  for line in path.read_text().splitlines()
                  if (row := json.loads(line)).get('reason') == 'compiler-artifact')


def distribution(samples):
    return dict(median=statistics.median(samples), minimum=min(samples), maximum=max(samples), samples=samples)


def summarize(directory, profile_paths):
    raw = json.loads((directory / 'results.json').read_text())
    assert raw['status'] == 'passed' and raw['rounds'] == 2 and len(raw['cases']) == 8
    assert raw['settle_mode'] == 'quiescent' and raw['jobs'] == 8
    assert all(v is None for v in raw['incremental_environment'].values())
    assert len({raw['binary_sha256s'][arm] for arm in ARMS[1:]}) == 3
    for arm in ARMS[1:]:
        assert str(raw['seal_workers'][arm]) == str(raw['hash_workers'][arm]) == '2'
        assert all(raw[k][arm] == '0' for k in ('compress_workers', 'import_pipeline', 'reuse_prepared'))
    graphs, integrity, sequences, pairs = 0, 0, [], []
    for case in raw['cases']:
        arm = case['backend']
        assert case['status'] == 'passed' and case['profile'] == 'debug'
        assert len(case['measurements']) == 8 and case['incremental_artifacts']['files'] > 0
        mounts = [r['values'] for r in case.get('daemon_counters', []) if r['kind'] == 'sealing_stats']
        if arm != 'native':
            assert case['integrity']['files'] > 0 and len(mounts) == 2
            assert all(m['errors'] == 0 for m in mounts)
            integrity += 1
        for measurement in case['measurements']:
            stage = measurement['stage']
            if stage.startswith('unchanged'):
                assert measurement['fresh_artifacts'] == measurement['artifacts']
            if arm != 'native':
                quiet = measurement['post_build_quiescence']['status']
                assert quiet['quiescent'] and quiet['drained'] and not quiet['worker_failed']
                assert quiet['open_handles'] == quiet['queue_entries'] == quiet['queued_inodes'] == 0
                assert all(quiet['sealing'][key] == 0 for key in ('errors', 'active_seals', 'active_prepares', 'active_reads'))
                prefix = f"round-{case['round']}-debug-"
                assert graph(directory / f'{prefix}{arm}-{stage}.jsonl') == graph(directory / f'{prefix}native-{stage}.jsonl')
                graphs += 1
            pairs.append(dict(round=case['round'], backend=arm, stage=stage,
                              cargo_seconds=measurement['wall_seconds'],
                              settling_seconds=measurement['post_build_idle_wait_seconds']))
        cpu_before = {k: int(v) for k, v in (line.split() for line in case['resources_before']['cpu.stat'].splitlines())}
        cpu_after = {k: int(v) for k, v in (line.split() for line in case['resources_after']['cpu.stat'].splitlines())}
        sequences.append(dict(round=case['round'], backend=arm,
                              cgroup_cpu_delta={k: cpu_after[k] - cpu_before[k] for k in cpu_before},
                              cargo_seconds=sum(m['wall_seconds'] for m in case['measurements']),
                              settling_seconds=sum(m['post_build_idle_wait_seconds'] for m in case['measurements']),
                              retained_bytes=(case['storage']['allocated_regular_bytes'] if arm == 'native' else
                                              case['storage']['native_allocated_bytes'] + case['storage']['content']['total_file_bytes']),
                              state_seconds={key: sum(m.get(key, 0) for m in mounts) for key in
                                             ('request_state_seconds', 'read_state_seconds', 'selection_state_seconds', 'publication_state_seconds', 'catalog_wait_seconds')},
                              maximum_request_state_seconds=max((m['max_request_state_seconds'] for m in mounts), default=0)))
    assert graphs == 48 and integrity == 6
    timings = []
    for stage in ('clean', 'edit', 'unchanged', 'unchanged-after-remount'):
        row = dict(stage=stage)
        for arm in ARMS:
            samples = [m for c in raw['cases'] if c['backend'] == arm for m in c['measurements']
                       if m['stage'] == stage or re.fullmatch(stage + r'-[123]', m['stage'])]
            assert len(samples) == (6 if stage in ('edit', 'unchanged') else 2)
            row[arm] = dict(cargo=distribution([m['wall_seconds'] for m in samples]),
                            cargo_plus_settling=distribution([m['wall_seconds'] + m['post_build_idle_wait_seconds'] for m in samples]),
                            compiler_cpu=distribution([m['cargo_user_seconds'] + m['cargo_system_seconds'] for m in samples]),
                            daemon_cpu=distribution([m['daemon_cpu_seconds'] for m in samples]))
        timings.append(row)
    events = dict(line.split() for line in raw['memory_events'].splitlines())
    assert int(events.get('oom_kill', 0)) == 0
    profiles = {}
    for arm, path in zip(('baseline-sl7', 'directories'), profile_paths):
        profile = json.loads(path.read_text())
        assert profile['binary_sha256'] == raw['binary_sha256s'][arm]
        assert len(profile['cases']) == 1
        phases = profile['cases'][0]['phases']
        assert phases['daemon_operation_counts_match'] and phases['opendir_state_ownership']['intervals'] > 0
        directory_phases = phases['operations']['OPENDIR']['phases_seconds']
        if arm == 'baseline-sl7':
            assert directory_phases['directory_child_attr'] > 0
        else:
            assert directory_phases['directory_child_pin'] > 0 and directory_phases['directory_register'] > 0
            assert 'directory_child_attr' not in directory_phases
        profiles[arm] = profile
    capture_roots = [directory.parent / ('profile-' + arm) for arm in ('baseline', 'directories')]
    assert graph(capture_roots[0] / 'sl7-profile/cargo.jsonl') == graph(capture_roots[1] / 'sl7-profile/cargo.jsonl')
    for root in capture_roots:
        captured = json.loads((root / 'results.json').read_text())
        assert captured['status'] == 'passed' and captured['cases'][0]['integrity']['files'] > 0
    return dict(method={k: raw[k] for k in ('revision', 'lock_sha256', 'rustc', 'jobs', 'cpu_max', 'memory_max', 'nofile_limit',
                                          'benchmark_sha256', 'mount_helpers_sha256', 'settle_mode', 'incremental_environment')},
                binaries={arm: raw['binary_sha256s'][arm] for arm in ARMS[1:]},
                timings=timings, samples=pairs, sequences=sequences, profiles=profiles,
                evidence_sha256=hashlib.sha256((directory / 'results.json').read_bytes()).hexdigest(),
                implementation_commit='128a19f', baseline_commit='ec02cba',
                validation=dict(cargo_commands=64, artifact_graph_pairs=graphs, integrity_checks=integrity,
                                profile_builds=2, profile_integrity_checks=2, cross_profile_artifact_graph_pairs=1,
                                incremental_artifacts_present=True, distinct_binaries=True, new_directory_path_traced=True, oom_kills=int(events.get('oom_kill', 0))),
                limitations=['Two reversed-order rounds on a shared node; descriptive medians, no significance claim.',
                             'All SL7 binaries include code-profile and release debug information. Recording is disabled for end-to-end runs.',
                             'Cargo time excludes executable checks and settling. Settling includes a 0.2-second observation floor.',
                             'One additional instrumented clean build per SL7 implementation; overlapping request times are not wall-time savings.',
                             'parking_lot 0.13.0 requires Rust 1.95; production declares Rust 1.85. Only State and its condition variables were replaced.'])


def render(data, prefix):
    sections = []
    def paragraph(title, text):
        sections.append((title, text, None, None))
    def table(title, headers, rows, text=''):
        sections.append((title, text, headers, rows))
    paragraph('Conclusion', 'Keep the directory snapshot optimization and retain the standard State mutex. '
              'Clean-build medians moved from 166.63s to 165.28s (0.8% lower), with both matched clean builds faster; edit medians barely changed. '
              'Adding parking_lot produced 165.03s clean and 6.071s edited versus 165.28s and 6.077s for directory-only: no demonstrated material end-to-end lift from this lock swap. '
              'Build-plus-settling results were mixed across rounds. These are descriptive measurements, not significance claims.')
    paragraph('Implementation', 'The directory snapshot change is committed as 128a19f. The baseline is ec02cba. ' +
              'The parking_lot adapter is an isolated experiment, not part of the default build.')
    paragraph('What changed', 'Directory enumeration, child descriptor pinning, and one metadata read per child run outside State. '
              'Registration happens in batches of at most 64 children. Snapshot references retain inodes across rename, unlink, and forget; '
              'the final handle takes ownership of those references. readdir needs the native file type, so full logical attributes are no longer fetched for every child.')
    paragraph('What parking_lot changes', 'The separate 0.13.0 experiment replaces only the State mutex and its condition variables. '
              'A small adapter preserves fail-closed poisoning. DashMap, the reader pool, cache and SQLite stay the same. '
              'Two dedicated tests cover panic poisoning and poisoned condition-variable reacquisition. The current crate requires Rust 1.95; '
              'SL7 declares 1.85, so this remains an isolated experiment unless adopted explicitly.')
    paragraph('Comparison', 'Baseline is ec02cba. All builds use the same fixed source revision and paths, eight Cargo jobs, '
              'two sealing workers and two hashing workers, with compression workers, pipelining and prepared reuse disabled. '
              'Each sequence starts with empty output directories and an empty store. The second round reverses backend order. '
              'Native uses the same backing filesystem. Actual incremental artifacts and all Cargo artifact graphs are verified.')
    for metric, title in [('cargo', 'Cargo elapsed time'), ('cargo_plus_settling', 'Cargo plus settling')]:
        table(title, ['Build stage', *LABELS], [[row['stage'], *[f"{row[arm][metric]['median']:.3f}s" for arm in ARMS]] for row in data['timings']],
              'Medians: two clean builds, six edits, six unchanged builds, and two remount checks per backend. Raw samples are in JSON.')
    table('Individual clean builds', ['Round', *LABELS], [[str(r), *[f"{next(s['cargo_seconds'] for s in data['samples'] if s['round'] == r and s['backend'] == arm and s['stage'] == 'clean'):.3f}s" for arm in ARMS]] for r in (1, 2)])
    table('Retained artifact storage', ['Round', *LABELS],
          [[str(r), *[f"{next(s['retained_bytes'] for s in data['sequences'] if s['round'] == r and s['backend'] == arm)/2**30:.3f} GiB" for arm in ARMS]] for r in (1, 2)],
          'After all builds and edits. Native counts allocated regular-file bytes; SL7 counts native allocation plus store files. Source inputs are outside these output directories.')
    for key, title in [('cargo_plus_settling', 'Complete sequence, Cargo plus settling'), ('request_state', 'Complete sequence, request-side State ownership')]:
        rows = []
        for r in (1, 2):
            values = []
            for arm in ARMS:
                seq = next(s for s in data['sequences'] if s['round'] == r and s['backend'] == arm)
                value = seq['cargo_seconds'] + seq['settling_seconds'] if key == 'cargo_plus_settling' else seq['state_seconds']['request_state_seconds']
                values.append(f'{value:.3f}s')
            rows.append([str(r), *values])
        table(title, ['Round', *LABELS], rows, 'Eight Cargo commands per sequence. Sequence time excludes executable checks, mount startup, integrity scans and unmount drain. State totals are occupancy counters, not Cargo wall time.')
    before, after = [data['profiles'][arm]['cases'][0] for arm in ('baseline-sl7', 'directories')]
    def metric(case, operation, field):
        return case['phases']['operations'][operation][field]
    metrics = [
        ('Profiled Cargo elapsed', before['wall_seconds'], after['wall_seconds']),
        ('OPENDIR accumulated handler time', metric(before, 'OPENDIR', 'total_seconds'), metric(after, 'OPENDIR', 'total_seconds')),
        ('OPENDIR p99 handler latency', metric(before, 'OPENDIR', 'p99_seconds'), metric(after, 'OPENDIR', 'p99_seconds')),
        ('OPENDIR total State ownership', before['phases']['opendir_state_ownership']['total_seconds'], after['phases']['opendir_state_ownership']['total_seconds']),
        ('OPENDIR p99 State hold', before['phases']['opendir_state_ownership']['p99_seconds'], after['phases']['opendir_state_ownership']['p99_seconds']),
        ('OPENDIR maximum State hold', before['phases']['opendir_state_ownership']['maximum_seconds'], after['phases']['opendir_state_ownership']['maximum_seconds']),
    ]
    for op in ('WRITE', 'READ'):
        metrics.append((op + ' accumulated handler time', metric(before, op, 'total_seconds'), metric(after, op, 'total_seconds')))
        metrics.append((op + ' p99 handler latency', metric(before, op, 'p99_seconds'), metric(after, op, 'p99_seconds')))
        for phase in ('native_payload', 'inode_gate_wait', 'reconstruct'):
            metrics.append((op + ' ' + phase, *[metric(case, op, 'phases_seconds').get(phase, 0) for case in (before, after)]))
        values = [sum(v for k, v in metric(case, op, 'phases_seconds').items() if k in ('state_lock_wait', 'cleanup_state_lock_wait')) for case in (before, after)]
        metrics.append((op + ' accumulated State wait', *values))
        metrics.append((op + ' wait overlapping OPENDIR ownership', *[sum(metric(case, op, 'state_wait_overlapping_opendir_hold_seconds').values()) for case in (before, after)]))
    table('Code-level comparison', ['Measurement', 'SL7 before', 'Directory snapshots', 'Change'],
          [[name, f'{a:.6f}s', f'{b:.6f}s', f'{100*(b/a-1):+.1f}%' if a else 'n/a'] for name, a, b in metrics],
          'Each profile covers one clean build. Handler/wait totals overlap across threads. State hold intervals measure lock ownership; '
          'waits also include scheduling. These numbers do not predict recoverable Cargo wall time.')
    table('Profiled daemon CPU', ['Mode', 'SL7 before', 'Directory snapshots'],
          [[mode, f"{before['daemon_build_cpu'][mode]:.3f}s", f"{after['daemon_build_cpu'][mode]:.3f}s"] for mode in ('user', 'system')],
          'CPU consumed during Cargo only; excludes post-build sealing. Both profiles enable per-request tracing and 99 Hz user/kernel sampling.')
    rows = []
    for site, syscall in [('StagingFs::opendir', 'openat'), ('StagingFs::opendir', 'statx'), ('StagingFs::write', 'pwrite64'),
                          ('StagingFs::write', 'writev'), ('Live::attach_identity', 'fsync'), ('Live::retire', 'ftruncate')]:
        values = []
        for case in (before, after):
            samples = next((r for r in case['cpu']['kernel_callsite_syscalls'] if r['site'] == site and r['syscall'] == syscall), None)
            values.append(f"{samples['percent']:.2f}% ({samples['samples']})" if samples else '0 samples')
        rows.append([site + ' → ' + syscall, *values])
    table('Selected kernel attribution', ['SL7 caller → syscall', 'SL7 before', 'Directory snapshots'], rows,
          'Percent of sampled daemon kernel CPU, with sample count. Samples are not syscall counts; these selected rows do not cover all kernel work.')
    paragraph('What the code profile establishes', 'The directory bottleneck was removed: State ownership fell from 6.004s to 0.192s (96.8%), '
              'and write-side State waits fell from 3.128s to 0.604s (80.7%). The overlap between write waits and directory ownership fell from 2.334s to 0.007s. '
              'However, total write-handler time rose from 13.50s to 14.70s in these two profiles: native payload I/O and same-inode waits were higher. '
              'Read p99 also rose, while directory p99 nearly halved. A single profile pair cannot establish causality for those other changes. '
              'This is a demonstrated directory/global-lock improvement, not a general reduction in every request latency.')
    paragraph('Next code-level targets', 'In the optimized profile, WRITE spends 9.015 accumulated seconds in native_payload and 2.885s in inode_gate_wait. '
              'The corresponding code is StagingFs::native_io in src/staging/operations.rs and the mutation reservation in src/staging.rs. '
              'Kernel samples attribute 20.2% of daemon kernel CPU to StagingFs::write → pwrite64. '
              'Packer::prepare remains on 61.6% of sampled daemon userspace stacks, inclusively. '
              'Those are stronger targets for the next investigation than another State mutex swap; neither CPU shares nor overlapping handler totals are wall-time savings estimates.')
    paragraph('Correctness and provenance', 'The directory implementation passed 104 Linux unit/integration tests and the parking_lot variant passed 106; each intentionally ignored one manual throughput test. Both passed strict Clippy and all five mounted suites (readonly, staging, cache, live and selfhost). Eleven analyzer tests passed. The comparison validates 64 Cargo commands, 48 matching artifact-graph pairs, '
              'six store integrity scans, incremental outputs, and drained background workers. Two additional profile builds pass integrity scans and share the same artifact graph. Distinct binary hashes are required. '
              'The traced candidate must execute directory_child_pin and directory_register and must not execute directory_child_attr. '
              'A stale shared-target build was detected and replaced before measurements; the corrected candidate used an isolated Cargo target directory. '
              'The pod descriptor limit was set to 524,288, matching prior runs, after the initial mounted test hit its default 1,024 limit.')
    paragraph('Limits', ' '.join(data['limitations']))
    markdown = ['# Directory snapshots and parking_lot\n']
    blocks = []
    for title, text, headers, rows in sections:
        markdown += [f'## {title}\n', text + '\n']
        block = f'<section><h2>{html.escape(title)}</h2><p>{html.escape(text)}</p>'
        if headers:
            markdown += ['| ' + ' | '.join(headers) + ' |', '| ' + ' | '.join(['---'] * len(headers)) + ' |']
            markdown += ['| ' + ' | '.join(row) + ' |' for row in rows]
            markdown += ['']
            block += '<div class="scroll"><table><thead><tr>' + ''.join(f'<th>{html.escape(v)}</th>' for v in headers) + '</tr></thead><tbody>'
            block += ''.join('<tr>' + ''.join(f'<td>{html.escape(v)}</td>' for v in row) + '</tr>' for row in rows) + '</tbody></table></div>'
        blocks.append(block + '</section>')
    markdown += ['## Code and references\n', '[Directory snapshots](../../src/staging/directories.rs), [State registration](../../src/staging.rs), [isolated parking_lot adapter](../../experiments/parking-lot/state_sync.rs).\n', '[parking_lot 0.13 Mutex behavior](https://docs.rs/parking_lot/0.13.0/parking_lot/type.Mutex.html) and [crate metadata](https://docs.rs/crate/parking_lot/0.13.0/source/Cargo.toml).\n']
    prefix.with_suffix('.md').write_text('\n'.join(markdown).rstrip() + '\n')
    prefix.with_suffix('.html').write_text('''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>SL7 directory snapshots and parking_lot</title>
<style>body{max-width:1120px;margin:48px auto;padding:0 24px;background:#101820;color:#edf4f5;font:17px/1.6 system-ui}h1{font-size:38px;line-height:1.2}h2{font-size:25px}section{margin:36px 0;padding:22px;border:1px solid #34505a;border-radius:12px;background:#15252e}table{border-collapse:collapse;width:100%;font-size:15px;font-variant-numeric:tabular-nums}th,td{text-align:right;padding:10px;border-bottom:1px solid #34505a;white-space:nowrap}th:first-child,td:first-child{text-align:left;white-space:normal}th{color:#9de3d0}.scroll{overflow:auto}p{color:#c7d6dd}a{color:#9de3d0}</style>
<h1>SL7: directory snapshots and parking_lot</h1><p>Measured code paths, build comparisons, and a separate lock implementation experiment.</p>''' + ''.join(blocks) + '<p><a href="opendir-parking-lot.json">All measurements and binary hashes (JSON)</a> · <a href="https://github.com/kaiba42/sl7/blob/128a19f/src/staging/directories.rs">Directory implementation</a> · <a href="https://docs.rs/parking_lot/0.13.0/parking_lot/type.Mutex.html">parking_lot docs</a></p></html>\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('builds', type=Path)
    parser.add_argument('baseline_profile', type=Path)
    parser.add_argument('directories_profile', type=Path)
    parser.add_argument('output_prefix', type=Path)
    parser.add_argument('--validation', type=Path, required=True)
    args = parser.parse_args()
    # Narrative conclusions were reviewed against this specific capture.
    # Reject other data rather than silently retaining stale conclusions.
    expected = ['19d64045b191988c21b9060c7cfd32429731c0526e4618228bc5ccd515f84fb2', 'af596139b01cdad67c404a37781804fadc142fadf5d4b19a2536fc1010030e2d', '359575c5c9ca49f2c681dda76bf12ab22a92b45a8dcf95abeb666fb0cb9337d0']
    paths = [args.builds / 'results.json', args.baseline_profile, args.directories_profile]
    assert [hashlib.sha256(p.read_bytes()).hexdigest() for p in paths] == expected, 'New capture: review narrative conclusions before publishing'
    result = summarize(args.builds, [args.baseline_profile, args.directories_profile])
    result['verification'] = json.loads(args.validation.read_text())
    assert result['verification']['linux_unit_integration_tests']['directories']['passed'] == 104
    assert result['verification']['linux_unit_integration_tests']['parking']['passed'] == 106
    assert result['verification']['analyzer_tests'] == 11
    assert all(v == 'passed' for suite in result['verification']['mounted_suites'].values() for v in suite.values())
    result['raw_evidence_archive_sha256'] = 'e6ab27f967cae606057067fee96241d00bf343bc86bd482661afcb91598f2d84'
    args.output_prefix.with_suffix('.json').write_text(json.dumps(result, indent=2) + '\n')
    render(result, args.output_prefix)
