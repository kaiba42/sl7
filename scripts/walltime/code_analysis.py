#!/usr/bin/env python3
"""Code-level phase and daemon user/kernel CPU analysis; no workload identities."""
import argparse
from bisect import bisect_right
from collections import Counter, defaultdict
import heapq
import json
from pathlib import Path
import re


def request(line):
    assert line.endswith('\n'), 'truncated trace record'
    fields = line.rstrip('\n').split('\t')
    operation, start, duration = fields[:3]
    assert operation in ('READ', 'WRITE', 'OPENDIR')
    start, duration = int(start), int(duration)
    phases, calls, intervals = {}, {}, []
    for field in fields[3:]:
        name, first, second = field.split(':')
        first, second = int(first), int(second)
        assert first >= 0 and second >= 0
        if name.startswith('@'):
            assert first + second <= duration
            intervals.append((name[1:], start + first, start + first + second))
        else:
            assert name not in phases and second > 0
            phases[name], calls[name] = first, second
    assert sum(phases.values()) == duration, 'exclusive phases do not partition request'
    assert calls.get(operation) == 1, 'missing root phase'
    return dict(operation=operation, start=start, end=start + duration, duration=duration,
                phases=phases, calls=calls, intervals=intervals)


def coverage(intervals):
    merged = []
    for start, end in sorted(intervals):
        if merged and start <= merged[-1][1]:
            merged[-1][1] = max(end, merged[-1][1])
        else:
            merged.append([start, end])
    starts = [start for start, _ in merged]
    prefix = [0]
    for start, end in merged:
        prefix.append(prefix[-1] + end - start)
    def covered(timestamp):
        index = bisect_right(starts, timestamp) - 1
        return 0 if index < 0 else prefix[index] + min(timestamp, merged[index][1]) - merged[index][0]
    return lambda start, end: covered(end) - covered(start)


def phases(directory, case):
    groups = defaultdict(lambda: dict(count=0, total_ns=0, phases=Counter(), phase_calls=Counter(), durations=[], tails=[]))
    waits, dir_holds = [], []
    excluded, validated = 0, 0
    observed_counts = Counter()
    for path in sorted((directory / 'phases').glob('*.tsv')):
        with path.open() as stream:
            for line in stream:
                row = request(line)
                validated += 1
                observed_counts[row['operation']] += 1
                if row['start'] < case['start_ns'] or row['end'] > case['end_ns']:
                    excluded += 1
                    continue
                op = row['operation']
                group = groups[op]
                group['count'] += 1
                group['total_ns'] += row['duration']
                group['phases'].update(row['phases'])
                group['phase_calls'].update(row['calls'])
                group['durations'].append(row['duration'])
                tail = dict(start_seconds=(row['start'] - case['start_ns']) / 1e9,
                            seconds=row['duration'] / 1e9, phases_seconds={k: v / 1e9 for k, v in row['phases'].items()},
                            phase_calls=row['calls'])
                heapq.heappush(group['tails'], (row['duration'], row['start'], group['count'], tail))
                if len(group['tails']) > 20:
                    heapq.heappop(group['tails'])
                for name, start, end in row['intervals']:
                    if name in ('state_lock_wait', 'cleanup_state_lock_wait'):
                        waits.append((op, name, start, end))
                    if op == 'OPENDIR' and name == 'state_locked_work':
                        dir_holds.append((start, end))
    overlap = coverage(dir_holds)
    intersections = defaultdict(Counter)
    for op, name, start, end in waits:
        intersections[op][name] += overlap(start, end) / 1e9
    result = {}
    for op, group in groups.items():
        durations = sorted(group.pop('durations'))
        result[op] = dict(count=group['count'], total_seconds=group['total_ns'] / 1e9,
                          p50_seconds=durations[len(durations) // 2] / 1e9,
                          p99_seconds=durations[min(len(durations) - 1, (99 * len(durations) + 99) // 100 - 1)] / 1e9,
                          maximum_seconds=max(durations) / 1e9,
                          phases_seconds={key: value / 1e9 for key, value in group['phases'].items()},
                          phase_calls=group['phase_calls'],
                          state_wait_overlapping_opendir_hold_seconds=dict(intersections[op]),
                          longest_requests=[row[3] for row in sorted(group['tails'], reverse=True)])
        assert abs(sum(result[op]['phases_seconds'].values()) - result[op]['total_seconds']) < 1e-6
    assert all(result[op]['count'] > 0 for op in ('READ', 'WRITE', 'OPENDIR'))
    counts_match = None
    if (directory / 'daemon.log').exists():
        stats = [json.loads(line.split('sl7: sealing_stats ', 1)[1]) for line in (directory / 'daemon.log').read_text().splitlines() if line.startswith('sl7: sealing_stats ')]
        assert len(stats) == 1, 'missing or repeated daemon final counters'
        assert observed_counts['READ'] == stats[0]['read_operations'], 'missing read trace records or failed reads'
        assert observed_counts['WRITE'] == stats[0]['write_operations'], 'missing write trace records or failed writes'
        counts_match = True
    return dict(operations=result, validated_requests=validated, excluded_outside_build=excluded,
                daemon_operation_counts_match=counts_match,
                note='Per-request phases are exclusive and sum exactly to handler time. Requests overlap across workers. Pre-dispatch FUSE queueing is outside these spans. Lock-wait intervals include scheduling; overlapping OPENDIR ownership is measured directly.')


HEADER = re.compile(r'^\s*(.+?)\s+(\d+)/(\d+)\s+(\d+)\.(\d+):\s+cpu-clock:([uk]):\s*$')
FRAME = re.compile(r'^\s+[0-9a-f]+\s+(.+?)\s+\(([^)]*)\)\s*$')


def cpu_samples(path):
    current = None
    for line in path.open():
        match = HEADER.match(line)
        if match:
            if current:
                yield current
            comm, pid, tid, seconds, fraction, mode = match.groups()
            current = dict(comm=comm, pid=int(pid), tid=int(tid), mode=mode,
                           ns=int(seconds) * 10**9 + int(fraction.ljust(9, '0')), frames=[])
        elif line.strip():
            match = FRAME.match(line)
            assert match and current, line[:150]
            current['frames'].append(match.groups())
    if current:
        yield current


def cpu(path, case):
    groups = defaultdict(lambda: dict(samples=0, leaf=Counter(), sl7_caller=Counter(), threads=Counter(), sl7_inclusive=Counter(), kernel_inclusive=Counter()))
    outside = 0
    kernel_pairs = Counter()
    for sample in cpu_samples(path):
        assert sample['pid'] == case['daemon_pid'], 'unexpected sampled process'
        if not case['start_ns'] <= sample['ns'] <= case['end_ns']:
            outside += 1
            continue
        group = groups[sample['mode']]
        group['samples'] += 1
        group['threads'][sample['comm']] += 1
        frames = [(re.sub(r'::h[0-9a-f]{16}(?:\+0x[0-9a-f]+)?$', '', name), dso) for name, dso in sample['frames']]
        group['leaf'][frames[0][0] if frames else '[unknown]'] += 1
        ours = [name for name, _ in frames if name.startswith(('sl7::', '<sl7::'))]
        group['sl7_caller'][ours[0] if ours else '[no SL7 frame]'] += 1
        group['sl7_inclusive'].update(set(ours))
        group['kernel_inclusive'].update({name for name, dso in frames if 'kernel.kallsyms' in dso})
        if sample['mode'] == 'k':
            names = [name for name, _ in frames]
            stack = '\n'.join(names) + '\n'
            site = next((label for needle, label in [
                ('sl7::staging::profile::', 'profiling instrumentation'),
                ('StagingFs as fuser::Filesystem>::opendir\n', 'StagingFs::opendir'),
                ('StagingFs as fuser::Filesystem>::write\n', 'StagingFs::write'),
                ('StagingFs as fuser::Filesystem>::read\n', 'StagingFs::read'),
                ('Live>::attach_identity', 'Live::attach_identity'),
                ('Live>::retire', 'Live::retire'),
                ('Live>::publish_seal', 'Live::publish_seal'),
                ('Packer>::prepare', 'Packer::prepare'),
            ] if needle in stack), 'other / unresolved caller')
            syscall = next((name.removeprefix('__x64_sys_') for name in names if name.startswith('__x64_sys_')), None)
            if syscall is None:
                syscall = next((label for symbol, label in [('do_writev', 'writev'), ('ksys_read', 'read'),
                    ('ksys_write', 'write'), ('do_sys_ftruncate', 'ftruncate'),
                    ('do_sys_openat2', 'openat'), ('do_fsync', 'fsync'), ('vfs_statx', 'statx')]
                    if symbol in names), 'other / unresolved syscall')
            kernel_pairs[(site, syscall)] += 1
    for group in groups.values():
        for key in ('leaf', 'sl7_caller', 'sl7_inclusive', 'kernel_inclusive'):
            group[key] = [dict(symbol=name, samples=count, percent=100 * count / group['samples']) for name, count in group[key].most_common(40)]
    return dict(groups=groups, excluded_outside_build=outside,
                kernel_callsite_syscalls=[dict(site=site, syscall=syscall, samples=count,
                                              percent=100 * count / groups['k']['samples'])
                                         for (site, syscall), count in kernel_pairs.most_common()],
                note='Daemon only, requested 99 Hz separately for userspace and kernel, bounded 16 KiB DWARF stacks. Inclusive stacks overlap; leaf samples partition each mode. Missing/inlined frames limit caller attribution.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    raw = json.loads((args.directory / 'results.json').read_text())
    assert raw['status'] == 'passed' and raw['artifact_graphs_equal'] and raw['code_profile']
    result = {key: raw[key] for key in ('binary_sha256', 'rustc', 'kernel', 'perf', 'jobs', 'cpu_max', 'memory_max', 'collector_hashes')}
    result['cases'] = []
    for case in raw['cases']:
        row = {key: case[key] for key in ('label', 'wall_seconds', 'cargo_cpu', 'daemon_build_cpu', 'daemon_settling_cpu', 'artifact_count')}
        row['settling_seconds'] = (case['settled_ns'] - case['end_ns']) / 1e9
        if case['traced']:
            row['phases'] = phases(args.directory / case['label'], case)
            if (args.directory / case['label'] / 'cpu.txt').exists():
                row['cpu'] = cpu(args.directory / case['label'] / 'cpu.txt', case)
        result['cases'].append(row)
    args.output.write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
