#!/usr/bin/env python3
"""Export reviewed aggregates; keep workload names, graphs and raw traces private."""
import argparse
from collections import Counter, defaultdict
import hashlib
import json
import math
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit

from events import partition, read_events, requests, scheduler, totals

OPCODES = {1: 'LOOKUP', 3: 'GETATTR', 4: 'SETATTR', 9: 'MKDIR', 10: 'UNLINK', 11: 'RMDIR',
           12: 'RENAME', 13: 'LINK', 14: 'OPEN', 15: 'READ', 16: 'WRITE', 17: 'STATFS',
           18: 'RELEASE', 20: 'FSYNC', 22: 'GETXATTR', 25: 'FLUSH', 27: 'OPENDIR',
           28: 'READDIR', 29: 'RELEASEDIR', 30: 'FSYNCDIR', 35: 'CREATE', 52: 'STATX'}


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()[:12]


def package_identity(spec):
    source, fragment = spec.rsplit('#', 1)
    if '@' in fragment:
        return tuple(fragment.rsplit('@', 1))
    return unquote(urlsplit(source).path).rstrip('/').rsplit('/', 1)[1], fragment


def timing_data(path):
    html = path.read_text()
    result = {}
    for key in ('UNIT_DATA', 'CONCURRENCY_DATA'):
        match = re.search(r'const ' + key + r'\s*=\s*', html)
        assert match, key
        result[key] = json.JSONDecoder().raw_decode(html[match.end():])[0]
    return result


def stat(text):
    return {line.split()[0]: int(line.split()[1]) for line in text.splitlines()}


def cgroup_delta(case):
    before = stat(case['resources_before']['cpu.stat'])
    after = stat(case['resources_at_cargo_exit']['cpu.stat'])
    return {key: after[key] - before[key] for key in before}


def distribution(values):
    values = sorted(values)
    return dict(count=len(values), total_seconds=sum(values), maximum_seconds=max(values, default=0),
                p50_seconds=values[len(values) // 2] if values else 0,
                p99_seconds=values[min(len(values) - 1, math.ceil(.99 * len(values)) - 1)] if values else 0)


def union_seconds(intervals):
    total = 0
    end = 0
    for start, finish in sorted(intervals):
        total += max(0, finish - max(start, end))
        end = max(end, finish)
    return total / 1e9


def timeline(spans, start, end):
    bins = [Counter() for _ in range(math.ceil((end - start) / 1e9))]
    for span in spans:
        lo, hi = max(start, span.start), min(end, span.end)
        if hi <= lo or span.state == 'ended':
            continue
        state = span.state if span.state in ('running', 'runnable', 'blocked_fuse') else 'other_wait'
        key = ('build_' if span.role == 1 else 'daemon_') + state
        for i in range((lo - start) // 10**9, (hi - start - 1) // 10**9 + 1):
            bins[i][key] += (min(hi, start + (i + 1) * 10**9) - max(lo, start + i * 10**9)) / 1e9
    return [dict(start_seconds=i, duration_seconds=min(1, (end - start) / 1e9 - i), **row)
            for i, row in enumerate(bins)]


def last_unblock_chain(units, wall_seconds, primary_packages):
    """Cargo's observed last-unblocking chain, respecting pipelined metadata.

    This is a schedule explanation, not a counterfactual minimum build time.
    Gaps can contain other necessary work and must not be called wasted time.
    """
    by_index = {u['i']: u for u in units}
    parents = {}
    for unit in units:
        for child in unit['unblocked_units']:
            assert child not in parents
            parents[child] = (unit['i'], unit['start'] + unit['duration'], 'completion')
        for child in unit['unblocked_rmeta_units']:
            assert child not in parents
            sections = dict(unit['sections'])
            parents[child] = (unit['i'], unit['start'] + sections['frontend']['end'], 'metadata')
    current = max(units, key=lambda u: (u['start'] + u['duration'], u['start']))
    cutoff = current['start'] + current['duration']
    finish = cutoff
    reverse, visited = [], set()
    edge = 'completion'
    while True:
        assert current['i'] not in visited
        visited.add(current['i'])
        workspace = current['name'] in primary_packages
        kind = 'build_script' if 'build-script' in current['target'] else ('workspace_compile' if workspace else 'dependency_compile')
        reverse.append(dict(kind=kind, cargo_index=current['i'],
                            id=digest([current[k] for k in ('name', 'version', 'target', 'features')]),
                            start=current['start'], end=cutoff, until=edge))
        if current['i'] not in parents:
            break
        parent, boundary, edge = parents[current['i']]
        assert boundary <= current['start'] + .031, 'Cargo dependency chronology inconsistent'
        cutoff = min(boundary, current['start'])  # Cargo timestamps round to 10 ms.
        current = by_index[parent]
    segments, cursor = [], 0.
    for segment in reversed(reverse):
        if segment['start'] > cursor:
            segments.append(dict(kind='before_or_between_chain_units', start=cursor, end=segment['start']))
        segments.append(segment)
        cursor = segment['end']
    assert wall_seconds >= finish - .031
    if wall_seconds > cursor:
        segments.append(dict(kind='cargo_boundary_remainder', start=cursor, end=wall_seconds))
    for segment in segments:
        segment['seconds'] = segment['end'] - segment['start']
        assert segment['seconds'] >= -.000001
    assert abs(sum(s['seconds'] for s in segments) - wall_seconds) < .031
    return segments


def analyze_case(directory, case):
    output = directory / case['label']
    timing = timing_data(output / 'cargo-timing.html')
    cargo_units = timing['UNIT_DATA']
    wrapper_records = [p.read_text().rstrip('\n').split('\t') for p in (output / 'units').glob('*.tsv')]
    primary = {record[5] for record in wrapper_records if record[7] == '1'}
    result = {key: case[key] for key in ('label', 'backend', 'traced', 'wall_seconds', 'cargo_cpu',
                                        'daemon_build_cpu', 'daemon_settling_cpu', 'artifact_count', 'incremental_artifacts')}
    result['settling_seconds'] = (case['settled_ns'] - case['end_ns']) / 1e9
    result['cgroup_cpu_delta'] = cgroup_delta(case)
    result['cargo_units'] = [dict(id=digest([u[k] for k in ('name', 'version', 'target', 'features')]),
                                  start=u['start'], duration=u['duration'], target_kind=('build_script' if 'build' in u['target'] else 'rustc'),
                                  unblocked_units=u['unblocked_units'], unblocked_rmeta_units=u['unblocked_rmeta_units'],
                                  sections=u['sections'], index=u['i']) for u in cargo_units]
    result['cargo_concurrency'] = timing['CONCURRENCY_DATA']
    result['last_unblock_chain'] = last_unblock_chain(cargo_units, case['wall_seconds'], primary)
    if not case['traced']:
        return result, {}
    rows = read_events(output / 'events.tsv')
    reqs, audit = requests(rows)
    spans, tasks, anomalies = scheduler(rows)
    start, end = case['start_ns'], case['end_ns']
    build = totals(spans, start, end, role=1)
    daemon = totals(spans, start, end, role=2)
    actual_cpu = sum(case['cargo_cpu'].values())
    residual = actual_cpu - build.get('running', 0)
    assert abs(residual) < max(.05, actual_cpu * .03), ('CPU accounting mismatch', residual, actual_cpu)
    assert anomalies.get('switch_out_without_running', 0) == 0, anomalies
    assert anomalies.get('switch_in_while_running', 0) == 0, anomalies
    result['scheduler'] = dict(build_thread_seconds=build, daemon_thread_seconds=daemon,
                               build_cpu_residual_seconds=residual, anomalies=anomalies,
                               wall_occupancy=partition(spans, start, end),
                               note='Thread-seconds overlap. Wall occupancy is exclusive but does not measure removable delay.')
    result['timeline'] = timeline(spans, start, end)
    result['request_pairing'] = audit
    by_op = defaultdict(list)
    for request in reqs:
        duration = max(0, min(end, request['end']) - max(start, request['start'])) / 1e9
        if duration:
            by_op[request['opcode']].append(duration)
    result['fuse_requests'] = {OPCODES.get(op, str(op)): distribution(values) for op, values in by_op.items()}
    result['fuse_request_union_seconds'] = union_seconds([(max(start, r['start']), min(end, r['end']))
                                                        for r in reqs if r['start'] < end and r['end'] > start])
    result['trace_events'] = dict(Counter(row[0] for row in rows))
    groups = defaultdict(list)
    for span in spans:
        if span.unit:
            groups[span.unit].append(span)
    units, private = [], {}
    for key, members in groups.items():
        if not key.startswith('sl7u_'):
            continue
        fields = (output / 'units' / (key[5:] + '.tsv')).read_text().rstrip('\n').split('\t')
        assert len(fields) == 8, fields
        born, _, crate, kind, extra, package, version, primary = fields
        if not crate:
            continue  # rustc metadata/target probes, included in global accounting.
        born = max(start, int(born))
        died = min(end, max(span.end for span in members if span.state != 'ended'))
        identity = digest([package, version, crate, kind, extra])
        label = ('workspace' if primary == '1' else 'dependency') + ' ' + (kind or 'build-script compiler') + ' ' + identity
        occupied = partition(members, born, died, unit=key)
        work = totals(members, born, died, unit=key)
        native = [span for span in members if span.kind == 'native_tool']
        unit = dict(id=identity, label=label, start=(born - start) / 1e9, end=(died - start) / 1e9,
                    wall_seconds=(died - born) / 1e9, thread_seconds=work, wall_occupancy=occupied,
                    native_tool_cpu_seconds=totals(native, born, died).get('running', 0))
        units.append(unit)
        private[identity] = dict(package=package, crate=crate, version=version, primary=primary == '1', kind=kind)
    units.sort(key=lambda row: row['end'])
    result['compiler_units'] = units
    # Align the invocation wrapper to Cargo's rounded timing records. Store
    # the observed alignment error; never silently match a distant invocation.
    available = {u['i']: u for u in cargo_units if '(run)' not in u['target']}
    matches = {}
    for unit in sorted(units, key=lambda u: u['start']):
        identity = private[unit['id']]
        candidates = [u for u in available.values() if u['name'] == identity['package'] and u['version'] == identity['version']
                      and ('build-script' in u['target']) == identity['crate'].startswith('build_script')]
        if not candidates:
            continue
        match = min(candidates, key=lambda u: abs(u['start'] - unit['start']))
        error = unit['start'] - match['start']
        if abs(error) > .25:
            continue
        matches[match['i']] = unit
        unit['cargo_index'] = match['i']
        unit['cargo_start_alignment_seconds'] = error
        del available[match['i']]
    group_by_id = {digest([record[5], record[6], record[2], record[3], record[4]]): 'sl7u_' + record[1]
                   for record in wrapper_records if record[2]}
    # Build-script runs have no RUSTC_WRAPPER invocation. Correlate their exec
    # path with Cargo's exact emitted build-script executable instead.
    script_paths = {}
    for line in (output / 'cargo.jsonl').read_text().splitlines():
        artifact = json.loads(line)
        if artifact.get('reason') == 'compiler-artifact' and artifact['target']['kind'] == ['custom-build']:
            package, version = package_identity(artifact['package_id'])
            for filename in artifact['filenames']:
                script_paths[filename] = (package, version)
    script_matches = {}
    for row in rows:
        if row[0] != 'E' or row[6] not in script_paths:
            continue
        key = 'script_' + row[3]
        if key not in groups:
            continue
        package, version = script_paths[row[6]]
        born = int(row[1])
        candidates = [u for u in cargo_units if '(run)' in u['target'] and u['name'] == package and u['version'] == version]
        match = min(candidates, key=lambda u: abs(u['start'] - (born - start) / 1e9))
        error = (born - start) / 1e9 - match['start']
        assert abs(error) < .25, ('build-script clock mismatch', error)
        assert match['i'] not in script_matches
        script_matches[match['i']] = (key, born, error)
    result['build_script_runs'] = []
    by_index = {u['i']: u for u in cargo_units}
    for index, (key, born, error) in script_matches.items():
        cargo_unit = by_index[index]
        died = min(end, max(s.end for s in groups[key] if s.state != 'ended'))
        result['build_script_runs'].append(dict(
            id=digest([cargo_unit[k] for k in ('name', 'version', 'target', 'features')]),
            cargo_index=index, start=(born - start) / 1e9, wall_seconds=(died - born) / 1e9,
            cargo_start_alignment_seconds=error, thread_seconds=totals(groups[key], born, died),
            wall_occupancy=partition(groups[key], born, died),
            native_tool_cpu_seconds=totals([s for s in groups[key] if s.kind == 'native_tool'], born, died).get('running', 0)))
    for segment in result['last_unblock_chain']:
        unit = matches.get(segment.get('cargo_index'))
        if unit is not None:
            key = group_by_id[unit['id']]
            lo = start + round(unit['start'] * 1e9)
            segment['compiler_id'] = unit['id']
        elif segment.get('cargo_index') in script_matches:
            key, lo, error = script_matches[segment['cargo_index']]
            segment['script_start_alignment_seconds'] = error
        else:
            continue
        hi = lo + round(segment['seconds'] * 1e9)
        segment['thread_seconds'] = totals(groups[key], lo, hi, unit=key)
        segment['wall_occupancy'] = partition(groups[key], lo, hi, unit=key)
        segment['native_tool_cpu_seconds'] = totals([s for s in groups[key] if s.kind == 'native_tool'], lo, hi).get('running', 0)
    result['final_compiler_unit'] = units[-1] if units else None
    result['longest_compiler_units'] = sorted(units, key=lambda row: row['wall_seconds'], reverse=True)[:15]
    by_kind = Counter()
    for span in spans:
        if span.role == 1 and span.state == 'running':
            by_kind[span.kind] += max(0, min(end, span.end) - max(start, span.start)) / 1e9
    result['build_cpu_by_process_kind'] = dict(by_kind)
    return result, private


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    raw = json.loads((args.directory / 'results.json').read_text())
    assert raw['status'] == 'passed' and raw['artifact_graphs_equal']
    report = {key: raw[key] for key in ('binary_sha256', 'rustc', 'kernel', 'cpu_max', 'memory_max', 'jobs',
                                      'source_lock_sha256', 'compiler_wrapper_sha256', 'collector_hashes', 'bpftrace', 'perf')}
    report['cases'], private = [], {}
    for case in raw['cases']:
        result, identities = analyze_case(args.directory, case)
        report['cases'].append(result)
        private[case['label']] = identities
        print(case['label'], result['wall_seconds'], result.get('scheduler', {}).get('build_cpu_residual_seconds'), flush=True)
    report['limitations'] = [
        'One control and one instrumented clean build per backend on a shared node; no statistical-significance claim.',
        'A CPU sample or accumulated wait is not a removable wall-clock cost.',
        'Wall occupancy describes whether any observed build thread could run; it does not reconstruct all within-compiler dependencies.',
        'request_wait_answer identifies direct synchronous FUSE sleeps; other FUSE/page-cache paths may be classified by syscall or unresolved.',
        'FUSE request lifetime includes queuing and service; dispatch and individual SL7 lock waits are not separated.',
        'Runqueue delay combines scheduling competition and quota effects; cgroup throttling counters are separate and overlap.',
        'Missing wakeup races are explicitly unresolved; CPU accounting is cross-checked against process totals.',
        'Private package names, paths, invocation logs and raw traces are excluded from this report.',
    ]
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    (args.directory / 'unit-identities-private.json').write_text(json.dumps(private, indent=2) + '\n')


if __name__ == '__main__':
    main()
