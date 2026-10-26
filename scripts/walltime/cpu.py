#!/usr/bin/env python3
"""Summarize userspace perf samples inside the Cargo wall-time window."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re

HEADER = re.compile(r'^\s*(.+?)\s+(\d+)/(\d+)\s+(\d+)\.(\d+):\s+cpu-clock:u:\s*$')
FRAME = re.compile(r'^\s+[0-9a-f]+\s+(.+?)\s+\(([^)]*)\)\s*$')


def samples(path):
    current = None
    with path.open() as stream:
        for line in stream:
            header = HEADER.match(line)
            if header:
                if current is not None:
                    yield current
                comm, pid, tid, seconds, fraction = header.groups()
                current = dict(comm=comm, pid=int(pid), tid=int(tid),
                               ns=int(seconds) * 10**9 + int(fraction.ljust(9, '0')), frames=[])
            elif line.strip():
                frame = FRAME.match(line)
                assert frame and current is not None, ('unparsed perf output', line[:160])
                current['frames'].append(frame.groups())
    if current is not None:
        yield current


def summarize(path, start, end, backend):
    daemon_pids = {sample['pid'] for sample in samples(path)
                   if any(dso == '/bench/bin/sl7' for _, dso in sample['frames'])}
    assert len(daemon_pids) == (1 if backend == 'sl7' else 0), daemon_pids
    groups = {key: dict(samples=0, leaf_modules=Counter(), inclusive_work=Counter()) for key in ('build', 'daemon')}
    outside = 0
    for sample in samples(path):
        if not start <= sample['ns'] <= end:
            outside += 1
            continue
        group = groups['daemon' if sample['pid'] in daemon_pids else 'build']
        group['samples'] += 1
        frames = sample['frames']
        symbol, dso = frames[0] if frames else ('[unknown]', '[unknown]')
        module = ('LLVM' if 'libLLVM' in dso else 'rustc_driver' if 'librustc_driver' in dso
                  else 'cargo' if dso.endswith('/cargo') else 'sl7' if dso == '/bench/bin/sl7'
                  else 'libc' if '/libc.so' in dso else 'unknown' if dso == '[unknown]' else 'other')
        group['leaf_modules'][module] += 1
        stack = '\n'.join(symbol for symbol, _ in frames)
        for category, needles in {
            'sha256': ('sha2::', 'sha256'),
            'compression': ('ZSTD_', 'HUF_', 'FSE_'),
            'sqlite': ('sqlite3', 'rusqlite::'),
            'allocation': ('malloc', 'realloc', 'alloc::alloc', 'free'),
            'hashmap': ('hashbrown::', 'dashmap::'),
        }.items():
            if any(needle in stack for needle in needles):
                group['inclusive_work'][category] += 1
    assert groups['build']['samples'] > 100
    return dict(groups=groups, outside_build_window=outside,
                note='Unweighted userspace samples at requested 49 Hz. Leaf modules partition samples; inclusive work overlaps. Neither is wall time. Short tasks and bounded stacks limit attribution.')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    raw = json.loads((args.directory / 'results.json').read_text())
    result = {case['label']: summarize(args.directory / case['label'] / 'cpu.txt',
                                      case['start_ns'], case['end_ns'], case['backend'])
              for case in raw['cases'] if case['traced']}
    args.output.write_text(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
