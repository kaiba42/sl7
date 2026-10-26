#!/usr/bin/env python3
"""Render the reviewed clean-build timing aggregates as Markdown and HTML."""
import argparse
from collections import Counter
import html
import json
from pathlib import Path

OCCUPANCY = {
    'at_least_one_thread_running': 'At least one build thread running',
    'runnable_but_no_thread_running': 'Runnable work, no build thread running',
    'no_runnable_thread_with_fuse_wait': 'No runnable build thread; FUSE wait present',
    'no_runnable_thread_other_wait': 'No runnable build thread; other wait',
    'unresolved_or_startup': 'Unresolved / startup',
}
PHASES = {
    'before_or_between_chain_units': 'Before / between last-unblocking chain units',
    'dependency_compile': 'Dependency compiler segments on that chain',
    'workspace_compile': 'Workspace compiler segments on that chain',
    'build_script': 'Build-script segments on that chain',
    'cargo_boundary_remainder': 'Cargo boundary remainder',
}


def render(data):
    cases = {case['label']: case for case in data['cases']}
    sl7, native = cases['sl7-profile'], cases['native-profile']
    sections = []

    def paragraph(title, text):
        sections.append((title, text, None, None))

    def table(title, text, headers, rows):
        sections.append((title, text, headers, rows))

    paragraph('What this establishes',
              f"The SL7 profile took {sl7['wall_seconds']:.2f}s. At least one build thread ran for "
              f"{sl7['scheduler']['wall_occupancy']['at_least_one_thread_running']:.2f}s, but completion-chain units also experienced filesystem stalls while unrelated compilation continued. "
              'Synchronized Cargo, scheduler, FUSE and CPU-sample measurements expose both views. '
              'The production DashMap implementation is unchanged (b5ab793). '
              'This report separates elapsed seconds from overlapping CPU and wait totals. '
              'It does not claim an exact causal allocation of the native-to-SL7 slowdown to individual locks or functions.')
    table('All four runs',
          'Capture order is shown. One control and one profile per backend, on a shared node with an eight-CPU quota. '
          'The traced native run is slower than traced SL7, while the controls show the reverse. '
          'That variability prevents a reliable native-to-SL7 performance delta or instrumentation-overhead estimate from these four runs.',
          ['Run', 'Cargo wall (s)', 'Build CPU (s)', 'SL7 CPU during build (s)', 'Later settling (s)'],
          [[c['label'], f"{c['wall_seconds']:.2f}", f"{sum(c['cargo_cpu'].values()):.2f}",
            f"{sum(c['daemon_build_cpu'].values()):.2f}" if c['daemon_build_cpu'] else '—',
            f"{c['settling_seconds']:.2f}" if c['backend'] == 'sl7' else '—'] for c in data['cases']])
    table('An additive view of elapsed build time',
          'Each column sums to its measured wall time. Categories describe all Cargo-descendant threads together. '
          'Running means at least one thread has a CPU; other threads may still be waiting. '
          'FUSE-wait occupancy is not automatically removable time, and waits can also delay dependencies while unrelated compilation runs.',
          ['Exclusive wall-time state', 'Native profile (s)', 'SL7 profile (s)'],
          [[label, f"{native['scheduler']['wall_occupancy'].get(key, 0):.3f}",
            f"{sl7['scheduler']['wall_occupancy'].get(key, 0):.3f}"] for key, label in OCCUPANCY.items()])
    phases = {}
    for case in data['cases']:
        phases[case['label']] = Counter()
        for segment in case['last_unblock_chain']:
            phases[case['label']][segment['kind']] += segment['seconds']
    table('Where the Cargo schedule goes',
          'Follow the last-finishing Cargo unit backward through the actual last-unblocking dependency edges. '
          'Metadata edges stop at metadata availability, preserving pipelined overlap. '
          'The before/between category includes other necessary parallel work, not just scheduling overhead. '
          'These columns also sum to elapsed time, but are a different view of the same time; do not add them to the table above.',
          ['Observed schedule segment', *[c['label'] for c in data['cases']]],
          [[label, *[f"{phases[c['label']][key]:.2f}" for c in data['cases']]] for key, label in PHASES.items()])
    pivotal = max((s for s in sl7['last_unblock_chain'] if s['kind'] == 'build_script'), key=lambda s: s['seconds'])
    matched_scripts = [next(s for s in c['build_script_runs'] if s['id'] == pivotal['id']) for c in (native, sl7)]
    table('A concrete completion dependency: the native crypto build',
          'The aws-lc-sys build-script run is on the observed SL7 completion chain. '
          'These measurements cover that process and its children, including native compiler tools. '
          'Other waits include coordination and waiting for work; they are not all filesystem delays. '
          'This is the same unit in both profiles, even though the final dependency chain differs.',
          ['Exclusive wall-time state', 'Native profile (s)', 'SL7 profile (s)'],
          [[label, *[f"{s['wall_occupancy'].get(key, 0):.2f}" for s in matched_scripts]] for key, label in OCCUPANCY.items()] +
          [['Total unit wall time', *[f"{s['wall_seconds']:.2f}" for s in matched_scripts]],
           ['Native-tool CPU-seconds (overlaps wall states)', *[f"{s['native_tool_cpu_seconds']:.2f}" for s in matched_scripts]]])
    chain_wait = sum(s.get('wall_occupancy', {}).get('no_runnable_thread_with_fuse_wait', 0) for s in sl7['last_unblock_chain'])
    paragraph('Why the four seconds of whole-build FUSE stalls is not a speedup ceiling',
              f"The selected completion-chain units accumulate {chain_wait:.2f}s with no runnable thread in that unit and a FUSE wait present. "
              'Other compiler processes often run during those intervals, hiding them in the whole-build occupancy chart. '
              'This demonstrates dependency-local stalls that global CPU utilization misses. '
              'The chain timing is rounded by Cargo and aligned to invocation starts; this remains an observed occupancy measure, '
              'not a proof that removing FUSE would recover that many seconds.')
    table('CPU and accumulated waits',
          'Thread-seconds overlap across threads and with one another. They do not sum to build wall time. '
          'Runqueue delay means a thread was runnable but not scheduled; this includes competition and quota effects. '
          'Other sleeps include thread joins, futex waits and I/O, so their sum is not an optimization budget. '
          'Scheduler interval totals exceed process CPU accounting by 1.1% for SL7 and 1.3% for native; '
          'this bounded reconciliation error remains visible rather than being normalized away.',
          ['Metric', 'Native profile', 'SL7 profile'],
          [[label, *[f"{c['scheduler']['build_thread_seconds'].get(key, 0):.2f}" for c in (native, sl7)]]
           for key, label in [('running', 'Build CPU-seconds from scheduler'), ('runnable', 'Runnable thread-seconds'),
                              ('blocked_fuse', 'Direct synchronous FUSE sleep thread-seconds'),
                              ('offcpu_unresolved', 'Unresolved off-CPU thread-seconds')]] +
          [['Mean build cores used', *[f"{sum(c['cargo_cpu'].values()) / c['wall_seconds']:.2f}" for c in (native, sl7)]],
           ['CPU reconciliation residual (s)', *[f"{c['scheduler']['build_cpu_residual_seconds']:.3f}" for c in (native, sl7)]]])
    table('FUSE request lifetimes during the SL7 build',
          f"All {sl7['request_pairing']['paired']:,} captured requests paired from send to completion. "
          'Rows below include only lifetimes intersecting the Cargo interval and clip them to that interval. '
          'Summed lifetimes overlap and include queuing plus service. They are not a dispatch/lock-time breakdown.',
          ['Operation', 'Count', 'Sum (s)', 'Median (µs)', 'p99 (ms)'],
          [[name, f"{r['count']:,}", f"{r['total_seconds']:.2f}", f"{r['p50_seconds'] * 1e6:.1f}", f"{r['p99_seconds'] * 1e3:.2f}"]
           for name, r in sorted(sl7['fuse_requests'].items(), key=lambda p: -p[1]['total_seconds'])[:12]])
    cpu = data['cpu_samples']
    table('Where build userspace samples land',
          'Requested 49 Hz, leaf-module attribution; percentages are samples, not elapsed build time. '
          'LLVM includes optimization/code generation and related work. rustc_driver includes compiler work outside that library. '
          'Both builds have closely comparable sample composition.',
          ['Leaf module', 'Native samples (%)', 'SL7 samples (%)'],
          [[module, *[f"{100 * cpu[label]['groups']['build']['leaf_modules'].get(module, 0) / cpu[label]['groups']['build']['samples']:.1f}%"
                      for label in ('native-profile', 'sl7-profile')]] for module in ('LLVM', 'rustc_driver', 'libc', 'cargo', 'other', 'unknown')])
    daemon = cpu['sl7-profile']['groups']['daemon']
    paragraph('SL7 work during the build',
              f"SL7 consumed {sl7['daemon_build_cpu']['user']:.2f} userspace and {sl7['daemon_build_cpu']['system']:.2f} kernel CPU-seconds before Cargo exited. "
              f"Of {daemon['samples']:,} daemon userspace samples, compression appears in {100 * daemon['inclusive_work'].get('compression', 0) / daemon['samples']:.1f}%, "
              f"SHA-256 in {100 * daemon['inclusive_work'].get('sha256', 0) / daemon['samples']:.1f}%, and SQLite in {100 * daemon['inclusive_work'].get('sqlite', 0) / daemon['samples']:.1f}%. "
              'These stack categories overlap and exclude kernel execution. Kernel CPU is a substantial part of the daemon cost; '
              'optimizing only a userspace map or hash function cannot account for all of it.')
    paragraph('Conclusions and next measurement', data['conclusions'])
    paragraph('Validation and scope',
              'Four clean Cargo builds, matching artifact graphs, executable checks, incremental-artifact checks and two SL7 integrity scans passed. '
              'Both traces passed request pairing, clock bounds and CPU-accounting checks; no trace loss was reported. '
              'Seven analyzer tests cover overlap, pipelining, scheduler races, request reuse, package IDs and CPU-window filtering. '
              'Wake-before-switch races remain explicitly unresolved in the JSON. Production code was not changed. '
              'Collection uses Linux 6.12.58, Rust 1.97.1, 8 jobs, 12 GiB memory, two SL7 sealers and two hash workers; '
              'preparation reuse, import pipelining and parallel compression are disabled. '
              'Private source, names, raw graphs, traces and sampled stack memory are excluded from public artifacts.')
    paragraph('Reproduction and provenance',
              'See scripts/walltime/README.md and clean-build-walltime.json. The JSON records binary and collector hashes, '
              'the raw archive checksum, tool versions, CPU sample aggregates, dependency phases and scheduler timelines. '
              'Cargo timing semantics: https://doc.rust-lang.org/cargo/reference/timings.html. '
              'FUSE requests were correlated by kernel request pointer because the measured kernel reports zero in the formatted send ID field.')
    title = 'What comprises a clean Rust build under SL7?'
    md = [f'# {title}', '']
    body = [f'<h1>{html.escape(title)}</h1>', '<p class="sub">Clean build • fixed DashMap binary • measured 2026-10-25/26</p>',
            '<p><a href="clean-build-walltime.json">Download measurement JSON</a> · <a href="clean-build-walltime.md">Markdown report</a></p>']
    for heading, text, headers, rows in sections:
        source = 'https://doc.rust-lang.org/cargo/reference/timings.html'
        markdown_text = text.replace(source, f'[Cargo timing reference]({source})')
        html_text = html.escape(text).replace(source, f'<a href="{source}">Cargo timing reference</a>')
        md.extend([f'## {heading}', '', markdown_text, ''])
        body.extend([f'<section><h2>{html.escape(heading)}</h2><p>{html_text}</p>'])
        if headers:
            md.extend(['| ' + ' | '.join(headers) + ' |', '| ' + ' | '.join(['---'] * len(headers)) + ' |'])
            md.extend('| ' + ' | '.join(map(str, row)) + ' |' for row in rows)
            md.append('')
            body.append('<div class="scroll"><table><thead><tr>' + ''.join(f'<th>{html.escape(h)}</th>' for h in headers) + '</tr></thead><tbody>')
            body.extend('<tr>' + ''.join(f'<td>{html.escape(str(cell))}</td>' for cell in row) + '</tr>' for row in rows)
            body.append('</tbody></table></div>')
        body.append('</section>')
    css = 'body{font:16px/1.6 system-ui,sans-serif;background:#101722;color:#e8edf5;max-width:1120px;margin:48px auto;padding:0 24px}h1{font-size:38px;line-height:1.2}h2{font-size:23px}p{max-width:100ch;color:#bfcbdc}.sub{color:#7fdbcf}section{margin:36px 0}.scroll{overflow-x:auto}table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}th,td{padding:11px 14px;text-align:right;border-bottom:1px solid #2d3c50;white-space:nowrap}th:first-child,td:first-child{text-align:left}th{background:#1b293b;color:#7fdbcf}tr:hover{background:#182438}@media(max-width:650px){body{margin:24px auto;padding:0 14px}h1{font-size:29px}th,td{padding:8px;font-size:14px}}'
    document = '<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>' + html.escape(title) + '</title><style>' + css + 'a{color:#7fdbcf}</style><body><main>' + ''.join(body) + '</main></body></html>'
    return '\n'.join(md), document


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('data', type=Path)
    args = parser.parse_args()
    md, document = render(json.loads(args.data.read_text()))
    args.data.with_suffix('.md').write_text(md)
    args.data.with_suffix('.html').write_text(document)


if __name__ == '__main__':
    main()
