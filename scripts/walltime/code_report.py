#!/usr/bin/env python3
"""Render code-level findings from reviewed aggregate measurements."""
import argparse
import html
import json
from pathlib import Path
import re


def source(label, path, line):
    return f'[{label}](https://github.com/kaiba42/sl7/blob/main/{path}#L{line})'


def render(data):
    case = next(c for c in data['cases'] if c['label'] == 'sl7-profile')
    ops = case['phases']['operations']
    kernel = case['cpu']['groups']['k']
    user = case['cpu']['groups']['u']
    sections = []
    def paragraph(title, text): sections.append((title, text, None, None))
    def table(title, text, columns, rows): sections.append((title, text, columns, rows))
    def phase(op, name): return ops[op]['phases_seconds'].get(name, 0)
    def mutex(op): return phase(op, 'state_lock_wait') + phase(op, 'cleanup_state_lock_wait')
    def overlap(op): return sum(ops[op]['state_wait_overlapping_opendir_hold_seconds'].values())
    def inclusive(group, needle):
        return next(row['percent'] for row in group['sl7_inclusive'] if needle in row['symbol'])
    paragraph('The main code-level finding',
              f"`StagingFs::opendir` visits {ops['OPENDIR']['phase_calls']['directory_child_lookup']:,} directory entries, calling both child lookup and attribute fetch while holding `Mutex<State>`. "
              f"Those two calls consume {phase('OPENDIR', 'directory_child_lookup') + phase('OPENDIR', 'directory_child_attr'):.2f}s in total. "
              f"Writes accumulate {mutex('WRITE'):.2f}s waiting for that mutex, and {overlap('WRITE'):.2f}s ({100 * overlap('WRITE') / mutex('WRITE'):.1f}%) "
              'overlaps measured directory-open ownership. This identifies a specific source of contention, beyond merely observing a mutex wait.')
    table('Measurement conditions',
          'Same instrumentable binary, fixed Synkro source, empty output/store per arm, eight Cargo jobs and incremental artifacts present. '
          'The control disables recording and sampling; the profile enables both. One shared-node pair cannot precisely isolate instrumentation overhead. '
          'Daemon CPU rises even though build wall times are close. No performance optimization was applied.',
          ['Run', 'Build wall (s)', 'Daemon user CPU (s)', 'Daemon kernel CPU (s)', 'Later settling (s)'],
          [[c['label'], f"{c['wall_seconds']:.2f}", f"{c['daemon_build_cpu']['user']:.2f}",
            f"{c['daemon_build_cpu']['system']:.2f}", f"{c['settling_seconds']:.2f}"] for c in data['cases']])
    table('Where time goes in our request code',
          'These are accumulated handler seconds across parallel requests, not additive seconds of build elapsed time. '
          'Each request is partitioned into exclusive phases. Handler time begins inside SL7 and excludes pre-dispatch FUSE queueing. '
          'The earlier FUSE-lifetime totals came from a different run; subtracting these handler totals from those numbers would not measure queue time.',
          ['Operation / code', 'Measured work', 'Accumulated seconds'],
          [[source('write_handle → write_all_at', 'src/staging.rs', 779), 'Native backing-file write', f"{phase('WRITE', 'native_payload'):.3f}"],
           [source('with_mutation + NativeIo::drop', 'src/staging/operations.rs', 10), 'WRITE State acquisition, including cleanup', f"{mutex('WRITE'):.3f}"],
           [source('opendir → State::lookup', 'src/staging.rs', 335), 'Pin/register each directory child', f"{phase('OPENDIR', 'directory_child_lookup'):.3f}"],
           [source('opendir → State::attr', 'src/staging.rs', 349), 'Full attributes to obtain just kind', f"{phase('OPENDIR', 'directory_child_attr'):.3f}"],
           [source('read_with → Condvar::wait', 'src/staging/reads.rs', 119), 'Inode mutation/exclusive-I/O gate', f"{phase('READ', 'inode_gate_wait'):.3f}"],
           [source('read_with → read_at', 'src/staging/reads.rs', 177), 'Native read, including buffer allocation', f"{phase('READ', 'native_payload'):.3f}"],
           [source('read_with → Live::read', 'src/staging/reads.rs', 168), 'Catalog/chunk reconstruction', f"{phase('READ', 'reconstruct'):.3f}"],
           [source('Readers::take', 'src/staging/reads.rs', 25), 'Reader-connection acquisition', f"{phase('READ', 'reader_pool_wait'):.6f}"],
           [source('write → reply.written', 'src/staging.rs', 1027), 'FUSE reply send', f"{phase('WRITE', 'reply'):.3f}"],
           [source('read → reply.data', 'src/staging.rs', 1007), 'FUSE read reply', f"{phase('READ', 'reply'):.3f}"]])
    table('Complete request totals and tails',
          'Exclusive phase details and the twenty longest requests for each operation are preserved in the JSON. '
          'Totals overlap across workers. Percentiles describe complete handler lifetimes in this profiled run.',
          ['Operation', 'Requests', 'Sum (s)', 'Median (µs)', 'p99 (ms)', 'Maximum (ms)'],
          [[op, f"{v['count']:,}", f"{v['total_seconds']:.3f}", f"{v['p50_seconds'] * 1e6:.1f}",
            f"{v['p99_seconds'] * 1e3:.2f}", f"{v['maximum_seconds'] * 1e3:.2f}"] for op, v in ops.items()])
    paragraph('Why opendir is expensive',
              source('The directory-open loop', 'src/staging.rs', 1224) + ' runs inside `with_read`, which owns the global state mutex. '
              '`State::lookup` checks parent metadata, constructs a descriptor-relative path, pins the child, and registers its identity. '
              '`State::attr` then fetches metadata again and may query logical metadata, although readdir needs only the entry kind. '
              f"Child lookup plus attributes account for {100 * (phase('OPENDIR', 'directory_child_lookup') + phase('OPENDIR', 'directory_child_attr')) / ops['OPENDIR']['total_seconds']:.1f}% "
              f"of directory-open handler time; enumeration is {phase('OPENDIR', 'directory_enumerate'):.3f}s and sorting is {phase('OPENDIR', 'directory_sort'):.3f}s. "
              'This path calls the stateful helpers directly, rather than the separate registered-inode metadata request fast path.')
    table('Which State waits coincide with opendir owning the mutex?',
          'Wait and ownership intervals are recorded on the same monotonic clock, then intersected. '
          'Waiting includes scheduling delay; this overlap does not prove an equal end-to-end speedup from removing the owner work.',
          ['Waiting operation', 'State wait (s)', 'Overlapping opendir ownership (s)', 'Share'],
          [[op, f"{mutex(op):.3f}", f"{overlap(op):.3f}", f"{100 * overlap(op) / mutex(op):.1f}%"] for op in ('WRITE', 'READ', 'OPENDIR')])
    table('Kernel CPU linked to SL7 callers',
          f"{kernel['samples']:,} daemon kernel samples at requested 99 Hz. Each sample is assigned one caller/syscall pair; "
          'percentages below are shares of sampled kernel CPU, not build wall time. Unresolved callers remain explicit in the JSON.',
          ['SL7 caller', 'Kernel entry', 'Kernel samples (%)'],
          [[r['site'], r['syscall'], f"{r['percent']:.2f}%"] for r in case['cpu']['kernel_callsite_syscalls']
           if r['site'] != 'other / unresolved caller' and r['percent'] >= 1])
    paragraph('What those kernel stacks mean in the code',
              source('write_handle', 'src/staging.rs', 772) + ' delegates to native I/O and `FileExt::write_all_at`; its stacks lead into '
              '`pwrite64`, `ext4_buffered_write_iter` and `generic_perform_write`. Sending the FUSE reply is a separate `writev` path. '
              + source('Live::attach_identity', 'src/live.rs', 321) + ' writes the identity xattr and calls `sync_all`; '
              + source('Live::retire', 'src/live.rs', 447) + ' truncates native data, restores sparse logical length/timestamps, and syncs. '
              f"Directory-open stacks account for {inclusive(kernel, 'StagingFs as fuser::Filesystem>::opendir'):.1f}% of kernel samples. "
              'The kernel profile also observes inotify callbacks; their watcher origin was not investigated. No host settings were changed.')
    table('Userspace CPU: the exact SL7 functions',
          f"{user['samples']:,} daemon userspace samples. Inclusive function percentages overlap through callers and callees. "
          'Background packing is a different cost from foreground request latency.',
          ['Function', 'What it does', 'Inclusive userspace samples (%)'],
          [[source('Packer::prepare', 'src/store.rs', 156), 'Preparation subtree', f"{inclusive(user, 'Packer>::prepare'):.1f}%"],
           [source('Packs::add', 'src/compression.rs', 203), 'Compression / pack insertion subtree', f"{inclusive(user, 'Packs>::add'):.1f}%"],
           [source('Chunker::next_chunk', 'src/chunk.rs', 44), 'Read-ahead, boundary scan, chunk allocation', f"{inclusive(user, 'Chunker<'):.1f}%"],
           [source('chunk::identity', 'src/chunk.rs', 6), 'Per-chunk SHA-256 identity', f"{inclusive(user, 'sl7::chunk::identity'):.1f}%"],
           [source('Live::publish_seal', 'src/live.rs', 405), 'Catalog publication subtree', f"{inclusive(user, 'Live>::publish_seal'):.1f}%"]])
    longest_write, longest_read = (ops[op]['longest_requests'][0] for op in ('WRITE', 'READ'))
    paragraph('The longest read/write stall',
              f"The longest write spent {longest_write['phases_seconds']['native_payload'] * 1000:.2f}ms inside `write_all_at`. "
              f"The longest read spent {longest_read['phases_seconds']['inode_gate_wait'] * 1000:.2f}ms at the inode gate and began "
              f"{(longest_read['start_seconds'] - longest_write['start_seconds']) * 1e6:.1f}µs after that write began. "
              'This is consistent with a read waiting for a long same-inode write. Inode IDs were not recorded, so that pairing is an inference. '
              'On-CPU kernel samples do not explain the sleeping portion of this specific long write.')
    paragraph('Where I would focus implementation next',
              'First, simplify directory entry construction so it does not compute full logical attributes just to obtain a file type, '
              'then move enumeration and metadata work out of the filesystem-wide mutex while preserving namespace-race behavior. '
              'That targets measured work that also blocks writes. Second, investigate backing-write sizes, writeback stalls and FUSE reply copying '
              'at `native_io` / `write_handle`; the longest read tail is at the inode gate, while chunk reconstruction and reader-pool waiting are smaller totals. '
              'Background CPU work is concentrated in compression, chunking/hashing and publication. Changes to identity or retirement syncs must preserve the existing crash-recovery contract.')
    paragraph('Validation and boundaries',
              f"Two clean builds, matching artifact graphs, executable checks and two store integrity scans passed. "
              f"All {case['phases']['validated_requests']:,} recorded requests passed exact phase-sum validation; "
              f"{case['phases']['excluded_outside_build']:,} outside-build requests were excluded. Total READ/WRITE trace counts match the daemon's independent final counters. "
              'Both normal and code-profile Linux configurations passed 99 tests and strict Clippy; ten Python analyzer tests passed. '
              'The mounted fixture passed before the full run. Trace formatting is outside the measured handler interval but still costs CPU; '
              'nested clock/bookkeeping costs remain inside phases. Stack unwinding is bounded and misses some/inlined callers. '
              'Raw perf memory, source identities and Cargo logs remain private. Source/binary hashes and archive checksum are in the JSON. '
              'Reproduction: scripts/walltime/CODE_PROFILE.md. This change adds opt-in diagnostics, with no storage or concurrency algorithm change.')

    title = 'SL7: where time goes in the code'
    md, body = [f'# {title}', ''], [f'<h1>{title}</h1>', '<p>Code phases + userspace/kernel stacks · 2026-10-26</p>']
    def markup(text):
        text = html.escape(str(text))
        text = re.sub(r'\[([^]]+)\]\(([^)]+)\)', r'<a href="\2">\1</a>', text)
        return re.sub(r'`([^`]+)`', r'<code>\1</code>', text)
    for heading, text, columns, rows in sections:
        md.extend([f'## {heading}', '', text, ''])
        body.append(f'<section><h2>{heading}</h2><p>{markup(text)}</p>')
        if columns:
            md.extend(['| ' + ' | '.join(columns) + ' |', '| ' + ' | '.join(['---'] * len(columns)) + ' |'])
            md.extend('| ' + ' | '.join(row) + ' |' for row in rows)
            md.append('')
            body.append('<div class="scroll"><table><thead><tr>' + ''.join(f'<th>{markup(c)}</th>' for c in columns) + '</tr></thead><tbody>')
            body.extend('<tr>' + ''.join(f'<td>{markup(c)}</td>' for c in row) + '</tr>' for row in rows)
            body.append('</tbody></table></div>')
        body.append('</section>')
    css = 'body{background:#101722;color:#e8edf5;font:16px/1.6 system-ui;max-width:1120px;margin:40px auto;padding:0 22px}h1{font-size:38px}h2{font-size:23px}section{margin:34px 0}p{color:#bdcada}a{color:#7fdbcf}code{color:#f2d596}.scroll{overflow:auto}table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}th,td{text-align:left;padding:10px 13px;border-bottom:1px solid #2e3e50}th{background:#1b293b}td:last-child{white-space:nowrap}th:last-child,td:last-child{text-align:right}@media(max-width:650px){body{padding:0 12px}h1{font-size:29px}table{font-size:14px}}'
    document = f'<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>{css}</style><body><main>' + ''.join(body) + '</main></body></html>'
    return '\n'.join(md), document


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('data', type=Path)
    args = parser.parse_args()
    md, document = render(json.loads(args.data.read_text()))
    args.data.with_suffix('.md').write_text(md)
    args.data.with_suffix('.html').write_text(document)
