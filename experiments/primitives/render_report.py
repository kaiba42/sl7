#!/usr/bin/env python3
"""Render the reviewed aggregate results as repository Markdown and HTML."""
import argparse
import html
import json
from pathlib import Path


def render(data):
    results = data['primitives']['results']
    builds = data['builds']
    sections = []
    def text(title, paragraph):
        sections.append((title, paragraph, None, None))
    def table(title, headers, rows, paragraph=''):
        sections.append((title, paragraph, headers, rows))
    def rate(group, work, threads, candidate):
        return next(r for r in results if (r['group'], r['workload'], r['threads'], r['candidate']) == (group, work, threads, candidate))
    text('Findings', 'DashMap, SCC, Moka, a bounded DashMap/FIFO cache, a bounded sharded LRU, and embedded Turso were executed, not assessed only from published benchmarks. Concurrent primitives substantially improved isolated throughput. Engine and cache choices have workload-specific tradeoffs; the integrated cache comparison below is the gate for claiming a Cargo improvement. Production defaults remain unchanged.')
    for work in ('uniform','hot','mixed'):
        names=['mutex','rwlock','dashmap','scc']
        table('Index: '+work, ['Threads']+names,
              [[t]+[f"{rate('index',work,t,n)['median_ops_per_second']/1e6:.2f}" for n in names] for t in (1,2,4,8)],
              'Million logical operations/second; higher is better. Same RandomState hasher family, inode-like integer keys and owned Arc values. The mixed case adds a replacement every tenth lookup. This isolates index access, not the complete inode-lease/publication protocol.')
    for work in ('uniform','hot','churn'):
        names=['lru','sharded-lru','dashmap-fifo','moka']
        table('Chunk cache: '+work,['Threads']+names,
              [[t]+[f"{rate('cache',work,t,n)['median_ops_per_second']/1e6:.2f}" for n in names] for t in (1,2,4,8)],
              'Million accesses/second. Distinct 8 KiB Arc payloads, 32-byte keys and a 128 MiB accounting budget. FIFO changes eviction policy, sharded LRU divides capacity, and Moka enforces capacity eventually. All final budgets and payload checks passed; this does not establish an instantaneous Moka bound. Allocation, I/O and decoding are outside the timed region.')
    for work in ('exists','repeated','range','publish-disjoint','publish-shared'):
        names=['sqlite','turso-wal','turso-mvcc']
        table('Catalog: '+work,['Connections']+names,
              [[t]+[f"{rate('catalog',work,t,n)['median_ops_per_second']:,.0f}" for n in names] for t in (1,2,4)],
              ('Transactions/second, 64 chunk references per transaction.' if work.startswith('publish') else ('Queries/second; scans 32 extents, consuming rows and the first integer column, without constructing full SL7 Location objects.' if work=='range' else 'Chunk-existence queries/second; repeated uses 64 present hashes, while exists mixes present and absent hashes.'))+' Same schema, seed, ext4 volume, FULL synchronous mode and foreign keys. Whole-transaction conflict retries are included. No pack I/O or FUSE work is included.')
    names={'baseline-sl7':'Current SL7','sl7':'16-shard cache'}
    for metric,title in [('cargo','Integrated Synkro build'),('cargo_plus_settling','Build plus settling')]:
        table(title,['Stage']+list(names.values()),
              [[r['stage']]+[f"{r[b][metric]['median']:.3f} ({r[b][metric]['minimum']:.3f}–{r[b][metric]['maximum']:.3f})" for b in names] for r in builds['timings']],
              'Seconds, median (observed range); lower is better. Two reversed-order rounds; both binaries use two hash workers and two sealers. Incremental artifacts were verified. No native Cargo arm was rerun here. Two rounds do not establish statistical significance.')
    edits=[p for p in builds['paired'] if p['stage'].startswith('edit-')]
    clean,edit=builds['timings'][:2]
    c=100*(clean['sl7']['cargo']['median']/clean['baseline-sl7']['cargo']['median']-1)
    e=100*(edit['sl7']['cargo']['median']/edit['baseline-sl7']['cargo']['median']-1)
    text('Integrated outcome',f'Relative to pre-sharding SL7, the sharded cache changed clean-build median time by {c:+.1f}% and edit median time by {e:+.1f}% (positive means slower). Native Cargo was not an arm of this experiment. {sum(p["candidate_seconds"]<p["baseline_seconds"] for p in edits)}/6 paired edits were faster. This is an experimental patch, not an adopted production default. The microbenchmark gain alone does not justify replacing the cache.')
    table('Compiler CPU time',['Stage']+list(names.values()),[[r['stage']]+[f"{r[b]['compiler_cpu']['median']:.2f}" for b in names] for r in builds['timings']],
          'CPU-seconds summed across the Cargo process tree. These and the daemon/cache counters can help expose variation, but are not critical-path profiles.')
    table('Cache behavior across each build/edit sequence', ['Round','Backend','Chunk hits','Chunk misses','Pack opens','Map misses','Load service seconds'],
          [[c['round'],names[c['backend']]]+[f"{c['mounts'][0][k]:,.3f}" if k=='load_seconds' else f"{c['mounts'][0][k]:,}" for k in ('chunk_hits','chunk_misses','pack_opens','map_misses','load_seconds')] for c in builds['cache']],
          'Before-remount counters. Load durations can overlap; they are not additive build latency. Different scheduling can change how much data reaches sealed storage and needs reconstruction.')
    text('What the integrated cache test exposed', 'The prototype divided the pack-handle budget as well as chunk/map capacity. Each shard could retain only four pack handles, and the same pack could occupy several shards. In the first sequence, pack opens increased from 10,458 to 21,033 despite fewer chunk misses. A next cache design should retain a shared pack-handle pool rather than repeat that partitioning. This is a concrete cache-locality cost, not a complete causal explanation of build-time variation: compiler CPU consumption and baseline wall times also varied between rounds.')
    text('Implementation choices', 'DashMap is a strong next candidate for the four-worker inode indexes; SCC is competitive and scales well at eight threads. Moving index lookups outside Mutex<State> requires per-inode coordination and consistent updates across the inode, identity and handle indexes. A map substitution inside the same global mutex would retain that serialization. DashMap/FIFO is the fastest cache hit path tested, but variable-size chunk budgets, eviction quality and miss loading need an integrated implementation. Moka provides a fuller cache abstraction, with eventual capacity enforcement. Turso MVCC improves concurrent publications in this probe, while range-query costs remain a concern; retaining SL7’s application-wide writer lock would prevent exploiting concurrent commits. WAL and MVCC results are distinct engine modes, not interchangeable benefits.')
    text('Validation and scope', '423 primitive samples passed equal-work/checksum/count validation. The integrated cache candidate passed 94 existing Linux tests plus a new cache-budget test, and three mounted suites covering reconstruction, mmap/linking, staging and cache coherence. All 32 Cargo commands, 16 paired artifact graphs, executable/freshness checks and four integrity scans passed. Cache byte, map-entry and descriptor budgets held. The database probe checked CHECK/FK rejection, rollback and final row counts, not power-loss recovery or a full SL7/Turso integration. DashMap/SCC/Moka/Turso remain experimental harness dependencies, outside production SL7.')
    text('Reproduction and limitations', 'AMD Ryzen 9 7940HS, Linux 6.12.58, Rust 1.97.1, Oaktown top node; pod limited to 8 CPUs and 12 GiB. Three primitive rounds rotate candidate order; JSON includes all samples and ranges. System allocator throughout: Turso’s default global mimalloc feature was disabled to avoid changing every other candidate. SQLite 3.46.0 (rusqlite 0.32.1), Turso 0.8.3, DashMap 6.2.1, SCC 3.8.8, Moka 0.12.16. Shared node and warm OS caches; descriptive measurements, no significance claim. Production source baseline df96636, with the exact candidate patch and harness hashes in JSON. See experiments/primitives/README.md for commands. Private workload source and raw artifact graphs are excluded.')
    md=['# SL7: concurrent primitives and Turso\n']
    web=['<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>SL7 primitive comparisons</title><style>body{font:16px/1.6 system-ui;background:#10151e;color:#e5ebf5;margin:0}main{max-width:1150px;margin:auto;padding:32px 24px}p{color:#c1ccde}a{color:#85c5ff}.scroll{overflow:auto}table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}th,td{padding:10px;border-bottom:1px solid #303c50;text-align:right;white-space:nowrap}th:first-child,td:first-child{text-align:left}h2{margin-top:2em}th{color:#a5d5ff}</style><main><h1>SL7: concurrent primitives and Turso</h1>']
    for title,paragraph,headers,rows in sections:
        md.extend(['\n## '+title+'\n','\n'+paragraph+'\n'])
        web.extend(['<h2>'+html.escape(title)+'</h2>','<p>'+html.escape(paragraph)+'</p>'])
        if headers:
            md.append('\n| '+' | '.join(headers)+' |\n| '+' | '.join(['---']*len(headers))+' |\n')
            md.extend('| '+' | '.join(map(str,row))+' |\n' for row in rows)
            web.append('<div class="scroll"><table><tr>'+''.join('<th>'+html.escape(str(h))+'</th>'for h in headers)+'</tr>')
            web.extend('<tr>'+''.join('<td>'+html.escape(str(cell))+'</td>'for cell in row)+'</tr>'for row in rows)
            web.append('</table></div>')
    md.append('\n[HTML tables](primitives-comparison.html) · [All measurements (JSON)](primitives-comparison.json) · [Harness and reproduction](../../experiments/primitives/README.md)\n')
    web.append('<p><a href="primitives-comparison.json">All measurements (JSON)</a></p></main></html>\n')
    return ''.join(md), ''.join(web)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input',type=Path)
    parser.add_argument('output_prefix',type=Path)
    args=parser.parse_args()
    md,web=render(json.loads(args.input.read_text()))
    args.output_prefix.with_suffix('.md').write_text(md)
    args.output_prefix.with_suffix('.html').write_text(web)
