#!/usr/bin/env python3
"""Validate complete primitive runs and emit descriptive, not inferential, statistics."""
import argparse
import collections
import json
import statistics
from pathlib import Path


def summarize(memory, catalog):
    rows = [json.loads(line) for path in (memory, catalog)
            for line in path.read_text().splitlines()]
    assert len(rows) == 423, (len(rows), 'expected 288 memory + 135 catalog samples')
    assert not [r for r in rows if 'error' in r], [r for r in rows if 'error' in r]
    identities = [(r['group'], r['candidate'], r['threads'], r['round'], r['workload']) for r in rows]
    assert len(set(identities)) == len(rows), 'duplicate samples'
    by_input = collections.defaultdict(list)
    groups = collections.defaultdict(list)
    for row in rows:
        assert row['seconds'] > 0 and row['operations'] > 0
        by_input[(row['group'], row['threads'], row['round'], row['workload'])].append(row)
        groups[(row['group'], row['workload'], row['threads'], row['candidate'])].append(row)
    for key, samples in by_input.items():
        assert len({r['checksum'] for r in samples}) == 1, ('checksum mismatch', key)
        assert len({r['operations'] for r in samples}) == 1, ('operation mismatch', key)
        if key[0] == 'catalog':
            assert all(r['constraints_checked'] for r in samples)
            assert len({tuple(r['counts']) for r in samples}) == 1, ('catalog count mismatch', key)
        if key[0] == 'cache':
            assert all(r['retained_bytes'] <= 128*1024*1024 for r in samples)
    results = []
    for (group, workload, threads, candidate), samples in sorted(groups.items()):
        assert {r['round'] for r in samples} == {0, 1, 2}
        rates = [r['operations']/r['seconds'] for r in samples]
        result = dict(group=group, workload=workload, threads=threads, candidate=candidate,
                      median_ops_per_second=statistics.median(rates),
                      min_ops_per_second=min(rates), max_ops_per_second=max(rates),
                      samples_seconds=[r['seconds'] for r in samples])
        if group == 'cache':
            result['hit_rates'] = [r['hits']/r['operations'] for r in samples]
        if group == 'catalog':
            result['retries'] = [r['retries'] for r in samples]
            result['p99_seconds'] = [r['p99_seconds'] for r in samples]
        results.append(result)
    return dict(schema_version=1, sample_count=len(rows), rounds=3,
                statistics='Descriptive medians and observed ranges; no significance test.',
                results=results, samples=rows)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('memory', type=Path)
    parser.add_argument('catalog', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.write_text(json.dumps(summarize(args.memory, args.catalog), indent=2)+'\n')
