#!/usr/bin/env python3
"""Validate matched build evidence; export only aggregate, non-source data."""
import argparse
import json
import re
import statistics
from pathlib import Path


def distribution(values):
    return dict(median=statistics.median(values), minimum=min(values), maximum=max(values), samples=values)


def graph(path):
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    return sorted([json.dumps({key: row[key] for key in ('package_id', 'target', 'profile', 'features', 'fresh')}, sort_keys=True)
                   for row in rows if row.get('reason') == 'compiler-artifact'])


def report(directory):
    data = json.loads((directory/'results.json').read_text())
    assert data['status'] == 'passed' and data['rounds'] == 2 and len(data['cases']) == 4
    assert data['hash_workers'] == {'sl7': '2', 'baseline-sl7': '2'}
    for field in ('reuse_prepared', 'import_pipeline', 'compress_workers'):
        assert data[field] == {'sl7': '0', 'baseline-sl7': '0'}
    assert data['settle_mode'] == 'quiescent'
    assert all(v is None for v in data['incremental_environment'].values())
    cache, sequences, graphs, paired = [], [], 0, []
    for case in data['cases']:
        assert case['status'] == 'passed' and case['profile'] == 'debug'
        assert len(case['measurements']) == 8 and case['incremental_artifacts']['files'] > 0
        assert case['integrity']['files'] > 0
        for measurement in case['measurements']:
            quiet = measurement['post_build_quiescence']['status']
            assert quiet['quiescent'] and quiet['drained'] and not quiet['worker_failed']
            assert quiet['open_handles'] == quiet['queue_entries'] == quiet['queued_inodes'] == 0
            seals = quiet['sealing']
            assert seals['errors'] == seals['active_seals'] == seals['active_prepares'] == seals['active_reads'] == 0
            assert seals['hash_workers'] == 2 and seals['workers'] == 2
            if measurement['stage'].startswith('unchanged'):
                assert measurement['fresh_artifacts'] == measurement['artifacts']
            if case['backend'] == 'sl7':
                prefix = f"round-{case['round']}-debug-"
                stage = measurement['stage']
                assert graph(directory/f'{prefix}sl7-{stage}.jsonl') == graph(directory/f'{prefix}baseline-sl7-{stage}.jsonl')
                graphs += 1
                baseline = next(c for c in data['cases'] if c['round'] == case['round'] and c['backend'] == 'baseline-sl7')
                other = next(m for m in baseline['measurements'] if m['stage'] == stage)
                paired.append(dict(round=case['round'], stage=stage, baseline_seconds=other['wall_seconds'], candidate_seconds=measurement['wall_seconds']))
        counters = [r['values'] for r in case['daemon_counters'] if r['kind'] == 'cache_stats']
        assert len(counters) == 2
        for row in counters:
            assert row['cached_bytes'] <= 128*2**20 and row['map_bytes'] <= 32*2**20
            assert row['open_packs'] <= 64 and row['map_windows'] <= 4096 and row['active_loads'] == 0
        cache.append(dict(round=case['round'], backend=case['backend'], mounts=counters))
        sequences.append(dict(round=case['round'], backend=case['backend'],
                              cargo_seconds=sum(m['wall_seconds'] for m in case['measurements']),
                              settling_seconds=sum(m['post_build_idle_wait_seconds'] for m in case['measurements']),
                              drain_seconds=case['pre_remount_drain_seconds']+case['unmount_drain_seconds'],
                              retained_bytes=case['storage']['native_allocated_bytes']+case['storage']['content']['total_file_bytes']))
    assert graphs == 16
    timings = []
    for stage in ('clean', 'edit', 'unchanged', 'unchanged-after-remount'):
        row = dict(stage=stage)
        for backend in ('baseline-sl7', 'sl7'):
            samples = [m for c in data['cases'] if c['backend'] == backend for m in c['measurements']
                       if m['stage'] == stage or re.fullmatch(stage+r'-[123]', m['stage'])]
            assert len(samples) == (6 if stage in ('edit', 'unchanged') else 2)
            row[backend] = dict(cargo=distribution([m['wall_seconds'] for m in samples]),
                                cargo_plus_settling=distribution([m['wall_seconds']+m['post_build_idle_wait_seconds'] for m in samples]),
                                compiler_cpu=distribution([m['cargo_user_seconds']+m['cargo_system_seconds'] for m in samples]),
                                daemon_cpu=distribution([m['daemon_cpu_seconds'] for m in samples]))
        timings.append(row)
    return dict(method={k: data[k] for k in ('revision','lock_sha256','rustc','jobs','cpu_max','memory_max','nofile_limit','benchmark_sha256','mount_helpers_sha256','settle_mode','reuse_prepared','import_pipeline','compress_workers','hash_workers')},
                binaries=data['binary_sha256s'], timings=timings, paired=paired, cache=cache, sequences=sequences,
                validation=dict(cargo_commands=32, artifact_graph_pairs=graphs, integrity_checks=4, incremental_artifacts_present=True),
                limitations=['Two counterbalanced rounds on a shared node; no significance claim.',
                             'No native Cargo arm in this experiment; baseline is current SL7.',
                             'Cargo wall time excludes executable checks and subsequent settling.',
                             'Settling includes a minimum 0.2-second observation delay.',
                             'Sequence sum excludes executable checks, setup, mount startup and integrity scans.',
                             'Counters can overlap and do not establish critical-path attribution.'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.write_text(json.dumps(report(args.directory), indent=2)+'\n')
