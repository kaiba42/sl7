"""Strict request pairing and scheduler accounting for trace.bt's private TSV."""
from collections import Counter, defaultdict
from dataclasses import dataclass


@dataclass(slots=True)
class Span:
    start: int
    end: int
    tid: int
    role: int
    state: str
    unit: str | None
    kind: str


def read_events(path):
    rows = [line.split('\t') for line in path.read_text().splitlines() if line.strip()]
    allowed = {'READY', 'END', 'E', 'F', 'M', 'I', 'O', 'W', 'X', 'P', 'S', 'D'}
    assert rows and all(row[0] in allowed for row in rows), 'unknown trace output'
    rows.sort(key=lambda row: int(row[1]))
    assert rows[0][0] == 'READY' and rows[0][2] == 'pointer-v3'
    assert rows[-1][0] == 'END', 'trace missing orderly termination'
    return rows


def requests(rows):
    # Raw and formatted callbacks execute once per send, in the same thread.
    # Pair their per-thread order, independently of callback registration order.
    # The formatted tracepoint's unique field is zero on the measured kernel.
    pointers, sends = defaultdict(list), defaultdict(list)
    for row in rows:
        if row[0] == 'P':
            pointers[row[2]].append(row)
        elif row[0] == 'S':
            sends[row[2]].append(row)
    assert pointers.keys() == sends.keys(), 'missing send callback'
    rewritten = []
    max_callback_gap = 0
    for tid in pointers:
        assert len(pointers[tid]) == len(sends[tid]), 'lost raw/formatted send event'
        for pointer, send in zip(pointers[tid], sends[tid]):
            gap = abs(int(pointer[1]) - int(send[1]))
            assert gap < 10_000_000 and pointer[3] == send[3], 'send callback correlation failed'
            max_callback_gap = max(max_callback_gap, gap)
            assert int(pointer[4]) != 0
            rewritten.append([send[0], send[1], tid, send[3], pointer[4], send[5], send[6]])
    rewritten.extend(row for row in rows if row[0] == 'D')
    rewritten.sort(key=lambda row: int(row[1]))
    pending, complete = {}, []
    for row in rewritten:
        if row[0] == 'S':
            pointer = row[4]
            assert pointer not in pending, 'request address reused before completion'
            pending[pointer] = row
        else:
            assert row[2] in pending, 'completion without send'
            send = pending.pop(row[2])
            assert int(row[1]) >= int(send[1])
            complete.append(dict(start=int(send[1]), end=int(row[1]), tid=int(send[2]),
                                 pid=int(send[3]), opcode=int(send[5]), connection=int(send[6])))
    assert not pending, f'{len(pending)} requests missing completion'
    return complete, dict(paired=len(complete), max_send_callback_gap_ns=max_callback_gap)


def task_kind(name, previous='other'):
    leaf = name.rsplit('/', 1)[-1]
    if leaf in ('cargo', 'rustup'):
        return 'cargo'
    if leaf == 'rustc':
        return 'compiler'
    if leaf in ('cc', 'gcc', 'collect2', 'ld', 'ld.lld', 'rust-lld', 'clang', 'clang++', 'cc1', 'cc1plus', 'as', 'ar'):
        return 'native_tool'
    if leaf.startswith('build-script-'):
        return 'build_script'
    if leaf == 'sl7':
        return 'sl7'
    return previous


def scheduler(rows):
    tasks, spans, anomalies = {}, [], Counter()
    end = int(rows[-1][1])

    def transition(tid, ns, state, override=None):
        task = tasks[tid]
        assert ns >= task['since']
        if ns > task['since']:
            spans.append(Span(task['since'], ns, tid, task['role'], override or task['state'], task['unit'], task['kind']))
        task['state'], task['since'] = state, ns

    for row in rows:
        event, ns = row[0], int(row[1])
        if event not in ('E', 'F', 'M', 'I', 'O', 'W', 'X'):
            continue
        role, tid = int(row[2]), int(row[3])
        if event == 'F':
            parent = tasks[int(row[4])]
            assert tid not in tasks or tasks[tid]['state'] == 'ended', 'live TID reuse'
            tasks[tid] = dict(role=role, parent=int(row[4]), since=ns, start=ns, state='created',
                              unit=parent['unit'], kind=parent['kind'], name=parent['name'])
        elif event == 'E':
            if tid not in tasks:
                tasks[tid] = dict(role=role, parent=None, since=ns, start=ns, state='running', unit=None, kind='other', name='')
            transition(tid, ns, 'running')
            task = tasks[tid]
            task.update(name=row[5], kind=task_kind(row[6], task['kind']), pid=int(row[4]))
            if task['kind'] == 'build_script' and task['unit'] is None:
                task['unit'] = f'script_{tid}'
                task['unit_start'] = ns
        elif event == 'M':
            transition(tid, ns, tasks[tid]['state'])
            tasks[tid]['name'] = row[5]
            if row[5].startswith('sl7u_'):
                tasks[tid]['unit'] = row[5]
                tasks[tid]['unit_start'] = ns
        elif event == 'O':
            if tasks[tid]['state'] != 'running':
                anomalies['switch_out_without_running'] += 1
            prev_state, syscall, fuse = map(int, row[4:7])
            state = 'runnable' if prev_state in (0, 256) else ('blocked_fuse' if fuse else f'blocked_syscall_{syscall}')
            transition(tid, ns, state)
        elif event == 'I':
            previous = tasks[tid]['state']
            override = None
            if previous.startswith('blocked_'):
                # Wake-before-switch races cannot be split reliably into sleep
                # and runqueue delay. Preserve them as explicitly unresolved.
                anomalies['switch_in_without_wakeup'] += 1
                override = 'offcpu_unresolved'
            elif previous == 'running':
                anomalies['switch_in_while_running'] += 1
            transition(tid, ns, 'running', override)
        elif event == 'W':
            if tasks[tid]['state'].startswith('blocked_') or tasks[tid]['state'] == 'created':
                transition(tid, ns, 'runnable')
            else:
                anomalies['wakeup_when_not_sleeping'] += 1
        elif event == 'X':
            transition(tid, ns, 'ended')
            tasks[tid]['end'] = ns
    for tid, task in tasks.items():
        if task['state'] != 'ended':
            transition(tid, end, 'trace_end')
    return spans, tasks, dict(anomalies)


def totals(spans, start, end, role=None, unit=None):
    result = Counter()
    for span in spans:
        if role is not None and span.role != role or unit is not None and span.unit != unit:
            continue
        duration = max(0, min(span.end, end) - max(span.start, start)) / 1e9
        if duration and span.state != 'ended':
            result[span.state] += duration
    return dict(result)


def partition(spans, start, end, role=1, unit=None):
    """Exclusive wall-time buckets by the observed activity of a set of threads.

    These are occupancy categories, NOT removable time or a causal critical path.
    A sleeping thread can coexist with useful work in another thread.
    """
    points = [(start, '', 0), (end, '', 0)]
    for span in spans:
        if span.role != role or unit is not None and span.unit != unit or span.state == 'ended':
            continue
        lo, hi = max(start, span.start), min(end, span.end)
        if hi > lo:
            points.extend([(lo, span.state, 1), (hi, span.state, -1)])
    points.sort()
    counts, result = Counter(), Counter()
    previous = start
    for ns, state, delta in points:
        if ns > previous:
            if counts['running']:
                bucket = 'at_least_one_thread_running'
            elif counts['runnable']:
                bucket = 'runnable_but_no_thread_running'
            elif counts['blocked_fuse']:
                bucket = 'no_runnable_thread_with_fuse_wait'
            elif any(v for k, v in counts.items() if k.startswith('blocked_')):
                bucket = 'no_runnable_thread_other_wait'
            else:
                bucket = 'unresolved_or_startup'
            result[bucket] += (ns - previous) / 1e9
        counts[state] += delta
        previous = ns
    assert abs(sum(result.values()) - (end - start) / 1e9) < .00001
    return dict(result)
