#!/usr/bin/env python3
"""Summarize saved perf script output; request wait sums can overlap in time."""
import argparse
import bisect
from collections import Counter, defaultdict
import heapq
import json
from pathlib import Path
import re
import statistics


def distribution(values):
    values = sorted(values)
    if not values:
        return None
    return dict(count=len(values), sum_seconds=sum(values),
                median_seconds=statistics.median(values),
                p95_seconds=values[min(len(values)-1, int(.95*len(values)))],
                p99_seconds=values[min(len(values)-1, int(.99*len(values)))],
                max_seconds=values[-1])


def fuse(path):
    pattern = re.compile(r'^\s*([\d.]+):\s+fuse:fuse_request_(send|sent|end):\s+connection (\d+) req (\d+) (.*)')
    requests = defaultdict(dict)
    events = Counter()
    duplicates = Counter()
    connections = set()
    unparsed = 0
    for line in path.read_text().splitlines():
        if not line.strip() or line.startswith('#'):
            continue
        match = pattern.match(line)
        if not match:
            unparsed += 1
            continue
        timestamp, event, connection, request, rest = match.groups()
        connections.add(connection)
        row = requests[request]
        if event in row:
            assert row[event] == float(timestamp), ('nonidentical duplicate event', request, event)
            duplicates[event] += 1
            continue
        row[event] = float(timestamp)
        events[event] += 1
        op = re.search(r'opcode (\d+) \(([^)]+)\)', rest)
        if op:
            row['opcode'], row['operation'] = int(op[1]), op[2]
        length = re.search(r'len (\d+)', rest)
        if length:
            row[event+'_len'] = int(length[1])
        error = re.search(r'error (-?\d+)', rest)
        if error:
            row['error'] = int(error[1])
    assert len(connections) == 1, connections
    assert unparsed == 0, ('unparsed trace lines', unparsed)
    by_operation = defaultdict(lambda: defaultdict(list))
    incomplete = Counter()
    errors = Counter()
    complete = []
    for row in requests.values():
        if not all(k in row for k in ('send', 'sent', 'end')):
            incomplete[','.join(k for k in ('send','sent','end') if k not in row)] += 1
            continue
        assert row['send'] <= row['sent'] <= row['end'], row
        operation = row['operation']
        by_operation[operation]['total'].append(row['end']-row['send'])
        by_operation[operation]['dispatch'].append(row['sent']-row['send'])
        by_operation[operation]['response'].append(row['end']-row['sent'])
        complete.append(row)
        if row.get('error'):
            errors[str(row['error'])] += 1
    release = sorted((r['sent'],r['end']) for r in complete if r['opcode']==18)
    merged=[]
    for start,end in release:
        if merged and start<=merged[-1][1]:
            merged[-1][1]=max(end,merged[-1][1])
        else:
            merged.append([start,end])
    starts=[start for start,_ in merged]
    prefix=[0.0]
    for start,end in merged:
        prefix.append(prefix[-1]+end-start)
    def covered(timestamp):
        i=bisect.bisect_right(starts,timestamp)-1
        return 0.0 if i<0 else prefix[i]+min(timestamp,merged[i][1])-merged[i][0]
    # These are sums across concurrent requests, not elapsed build seconds.
    overlap = 0.0
    for row in complete:
        lo, hi = row['send'], row['sent']
        overlap += covered(hi)-covered(lo)
    return dict(events=events, exact_duplicate_events=duplicates, connections=sorted(connections), complete_requests=len(complete),
                incomplete_requests=incomplete, error_counts=errors,
                operations={op:{k:distribution(v) for k,v in groups.items()} for op,groups in by_operation.items()},
                all_requests={k:distribution([r[b]-r[a] for r in complete])
                              for k,a,b in [('total','send','end'),('dispatch','send','sent'),('response','sent','end')]},
                dispatch_wait_overlapping_release_response_seconds=overlap,
                longest_requests=[dict(operation=r['operation'], total_seconds=r['end']-r['send'],
                                              dispatch_seconds=r['sent']-r['send'], response_seconds=r['end']-r['sent'])
                                  for r in heapq.nlargest(20,complete,key=lambda r:r['end']-r['send'])])


def cpu(path):
    leaf = Counter()
    inclusive = Counter()
    categories = Counter()
    sha_categories = Counter()
    sample_count = 0
    # perf script default format: a sample header, indented stack frames, blank.
    for block in path.read_text().strip().split('\n\n'):
        frames=[]
        if 'cpu-clock:u' not in block:
            continue
        for line in block.splitlines()[1:]:
            match=re.match(r'^\s+[0-9a-f]+\s+(.+?)\s+\(([^)]*)\)\s*$',line)
            if match:
                symbol=re.sub(r'\+0x[0-9a-f]+$', '', match[1])
                symbol=re.sub(r'::h[0-9a-f]{16}$', '', symbol)
                frames.append(symbol)
        if frames:
            sample_count+=1
            leaf[frames[0]]+=1
            inclusive.update(set(frames))
            if '<sl7::live::Live>::hydrate' in frames:
                category = 'hydration'
            elif '<sl7::live::Live>::seal' in frames:
                category = 'sealing'
            elif '<sl7::staging::StagingFs as fuser::Filesystem>::read' in frames:
                category = 'mounted_read'
            else:
                category = 'other'
            categories[category] += 1
            if frames[0] == 'sha2::sha256::compress256':
                sha_categories[category] += 1
    assert sample_count, 'No CPU stacks parsed'
    def rows(counter):
        return [dict(symbol=s, samples=n, sample_percent=100*n/sample_count) for s,n in counter.most_common(50)]
    return dict(samples=sample_count, top_leaf=rows(leaf), top_inclusive=rows(inclusive),
                sample_categories=categories, sha256_leaf_by_category=sha_categories,
                note='Unweighted sample counts at requested 99 Hz; inclusive percentages overlap.')


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory',type=Path)
    args=parser.parse_args()
    output={}
    for path in args.directory.glob('*-report.txt'):
        if '-cpu-' in path.name or '-fuse-' in path.name:
            assert '# Total Lost Samples: 0' in path.read_text(), ('lost samples', path)
    for path in sorted(args.directory.glob('*-fuse.txt')):
        output[path.stem]=fuse(path)
    for path in sorted(args.directory.glob('*-cpu.txt')):
        output[path.stem]=cpu(path)
    (args.directory/'analysis.json').write_text(json.dumps(output,indent=2)+'\n')
    print(json.dumps({k:v.get('samples',v.get('complete_requests')) for k,v in output.items()},indent=2))


if __name__=='__main__':
    main()
