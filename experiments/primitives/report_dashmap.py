#!/usr/bin/env python3
"""Validate the integrated DashMap comparison and publish source-free aggregates."""
import argparse
import hashlib
import html
import json
from pathlib import Path

from report_builds import report


def summarize(directory):
    result = report(directory)
    raw = json.loads((directory / 'results.json').read_text())
    result['metadata'] = []
    fields = ('lookup_operations', 'getattr_operations', 'indexed_lookups',
              'indexed_getattrs', 'indexed_lookup_misses', 'request_state_seconds',
              'read_state_seconds', 'selection_state_seconds',
              'publication_state_seconds', 'catalog_wait_seconds')
    for case in raw['cases']:
        mounts = [row['values'] for row in case['daemon_counters']
                  if row['kind'] == 'sealing_stats']
        assert len(mounts) == 2
        totals = {field: sum(row.get(field, 0) for row in mounts) for field in fields}
        if case['backend'] == 'sl7':
            for row in mounts:
                assert 0 < row['indexed_lookups'] + row['indexed_lookup_misses'] <= row['lookup_operations']
                assert 0 < row['indexed_getattrs'] <= row['getattr_operations']
        totals['metadata_operations'] = totals['lookup_operations'] + totals['getattr_operations']
        totals['metadata_without_state'] = sum(totals[key] for key in
                                               ('indexed_lookups', 'indexed_getattrs', 'indexed_lookup_misses'))
        totals['metadata_without_state_percent'] = 100 * totals['metadata_without_state'] / totals['metadata_operations']
        result['metadata'].append(dict(round=case['round'], backend=case['backend'], **totals))
    result['validation']['dashmap_path_exercised_in_every_mount'] = True
    return result


def render(data):
    sections = []

    def paragraph(title, text):
        sections.append((title, text, None, None))

    def table(title, headers, rows, text):
        sections.append((title, text, headers, rows))

    paragraph('What changed',
              'DashMap 6.2.1 now backs the live inode and native-identity indexes. '
              'Registered native LOOKUP and GETATTR requests, plus absent-name probes, '
              'run outside Mutex<State>. Owned descriptors pin native inodes across I/O; '
              'epoch validation rejects snapshots spanning representation changes. '
              'Reference acquisition and reclamation share a shard write lock. '
              'Multi-syscall mutations temporarily exclude only their affected inodes. '
              'Cold registration, sealed/logically overridden metadata, handles, mutations '
              'and publication retain their coordinated paths. Cache policy and SQLite are unchanged.')
    paragraph('Comparison',
              'Baseline is pre-DashMap SL7 (85f3358; runtime source identical to 218a79d), '
              f"candidate is {data['implementation_commit'][:7]}. "
              'This is a real DashMap integration, distinct from the earlier 16-shard LRU experiment. '
              'There is no native Cargo arm in this comparison. Both arms use eight Cargo jobs, '
              'two sealers and two hash workers; preparation reuse, import pipelining and parallel '
              'compression are disabled. Two rounds reverse arm order on the same Oaktown node. '
              'Each arm starts with empty output directories and an empty store. Incremental '
              'artifacts and matching Cargo artifact graphs are verified.')
    for metric, title in [('cargo', 'Cargo build time'), ('cargo_plus_settling', 'Build plus settling')]:
        rows = []
        for row in data['timings']:
            before, after = (row[b][metric] for b in ('baseline-sl7', 'sl7'))
            rows.append([row['stage'],
                         f"{before['median']:.3f} ({before['minimum']:.3f}–{before['maximum']:.3f})",
                         f"{after['median']:.3f} ({after['minimum']:.3f}–{after['maximum']:.3f})",
                         f"{100 * (after['median'] / before['median'] - 1):+.1f}%"])
        table(title, ['Stage', 'Pre-DashMap SL7', 'DashMap SL7', 'Elapsed change'], rows,
              'Seconds: median (observed range). Lower is better; negative changes are faster. '
              'Two clean/remount samples and six edited/unchanged samples per arm. '
              'Cargo time excludes executable checks and subsequent background settling. '
              'Settling includes a minimum 0.2-second observation delay.')
    edits = [row for row in data['paired'] if row['stage'].startswith('edit-')]
    clean = [row for row in data['paired'] if row['stage'] == 'clean']
    faster = lambda rows: sum(row['candidate_seconds'] < row['baseline_seconds'] for row in rows)
    paragraph('Paired outcomes',
              f'DashMap was faster in {faster(clean)}/2 paired clean builds and '
              f'{faster(edits)}/6 paired edits. The JSON preserves every sample. '
              'Two rounds on a shared node do not establish statistical significance. '
              'The earlier approximately 11× index microbenchmark result is not an expected '
              'whole-build speedup: this change removes coordination from a subset of filesystem '
              'requests, while compilation, reconstruction, hashing, publication and other '
              'coordinated operations still contribute to elapsed time.')
    table('Requests served outside global state',
          ['Round', 'Native LOOKUP hits', 'Absent-name probes', 'Native GETATTR hits', 'All metadata requests', 'Bypassed State'],
          [[row['round'], f"{row['indexed_lookups']:,}", f"{row['indexed_lookup_misses']:,}",
            f"{row['indexed_getattrs']:,}", f"{row['metadata_operations']:,}",
            f"{row['metadata_without_state_percent']:.1f}%"]
           for row in data['metadata'] if row['backend'] == 'sl7'],
          'Counts sum both mounts in each sequence, including executable checks, status observations '
          'and storage inventory. These confirm the concurrent path was exercised; they do not '
          'measure its isolated latency. Other lookup errors also bypass State but are not counted here.')
    table('Accumulated time holding filesystem state',
          ['Round', 'Arm', 'Request state seconds', 'Read state seconds'],
          [[row['round'], 'DashMap' if row['backend'] == 'sl7' else 'Baseline',
            f"{row['request_state_seconds']:.3f}", f"{row['read_state_seconds']:.3f}"]
           for row in data['metadata']],
          'End-of-mount counters summed across each sequence. They measure time inside existing '
          'State sections, not elapsed build time or time waiting for DashMap shards. '
          'Fast metadata callbacks intentionally no longer contribute to request-state time. '
          'Counters alone do not establish critical-path attribution.')
    table('CPU time', ['Stage', 'Compiler baseline', 'Compiler DashMap', 'Daemon baseline', 'Daemon DashMap'],
          [[row['stage']] + [f"{row[backend][metric]['median']:.3f}" for metric in ('compiler_cpu', 'daemon_cpu')
                            for backend in ('baseline-sl7', 'sl7')] for row in data['timings']],
          'Median CPU seconds. Compiler includes Cargo children. Daemon measurements include '
          'the build, executable check and settling interval; they are not exclusively CPU on the build critical path.')
    paragraph('Correctness',
              '99 Linux and 51 macOS Rust tests passed (one existing ignored probe on each), strict Clippy passed, '
              'and all five mounted suites passed. New tests exercise progress with State held, '
              'mutation and publication invalidation, concurrent lookup/forget/reclaim, and '
              'failed-mutation cleanup. All 32 real Cargo commands, 16 paired artifact graphs, '
              'executable/freshness checks and four integrity scans passed. Chunk, mapping and '
              'file-descriptor budgets held. Both candidate mounts in each round exercised '
              'the concurrent metadata path.')
    paragraph('Environment and scope',
              'Oaktown kaiba namespace, top node; 8 CPU and 12 GiB pod limits, Rust 1.97.1, '
              'Linux x86_64, ext4 backing storage. Dependencies were fetched beforehand; OS '
              'page caches remain warm. No other SL7 benchmark ran concurrently. Binary hashes, '
              'workload revision, compiler identity, aggregate samples and source hashes are '
              'in the JSON. Private workload source, paths and raw artifact graphs are excluded. '
              'This is a focused inode-index integration; it does not evaluate a DashMap chunk '
              'cache, SCC, evmap or Turso inside the full filesystem.')
    md = ['# SL7: integrated DashMap metadata indexes\n']
    web = ['<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
           '<title>SL7 DashMap build comparison</title><style>body{font:16px/1.6 system-ui;background:#10151e;color:#e5ebf5;margin:0}'
           'main{max-width:1150px;margin:auto;padding:32px 24px}p{color:#c1ccde}a{color:#85c5ff}.scroll{overflow:auto}'
           'table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}th,td{padding:10px;border-bottom:1px solid #303c50;'
           'text-align:right;white-space:nowrap}th:first-child,td:first-child{text-align:left}h2{margin-top:2em}th{color:#a5d5ff}'
           '</style><main><h1>SL7: integrated DashMap metadata indexes</h1>']
    for title, text, headers, rows in sections:
        md.extend(['\n## ' + title + '\n\n', text + '\n'])
        web.extend(['<h2>' + html.escape(title) + '</h2><p>' + html.escape(text) + '</p>'])
        if headers:
            md.append('\n| ' + ' | '.join(headers) + ' |\n| ' + ' | '.join(['---'] * len(headers)) + ' |\n')
            md.extend('| ' + ' | '.join(map(str, row)) + ' |\n' for row in rows)
            web.append('<div class="scroll"><table><tr>' + ''.join('<th>' + html.escape(h) + '</th>' for h in headers) + '</tr>')
            web.extend('<tr>' + ''.join('<td>' + html.escape(str(cell)) + '</td>' for cell in row) + '</tr>' for row in rows)
            web.append('</table></div>')
    md.append('\n[HTML tables](dashmap-indexes.html) · [Measurements and provenance (JSON)](dashmap-indexes.json)\n')
    web.append('<p><a href="dashmap-indexes.json">Measurements and provenance (JSON)</a></p></main></html>\n')
    return ''.join(md), ''.join(web)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('output_prefix', type=Path)
    parser.add_argument('--provenance', type=Path, required=True)
    args = parser.parse_args()
    data = summarize(args.directory)
    provenance = json.loads(args.provenance.read_text())
    assert data['binaries']['sl7'] == provenance['candidate_binary_sha256']
    data.update(provenance)
    data['report_script_sha256'] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    args.output_prefix.with_suffix('.json').write_text(json.dumps(data, indent=2) + '\n')
    md, web = render(data)
    args.output_prefix.with_suffix('.md').write_text(md)
    args.output_prefix.with_suffix('.html').write_text(web)
