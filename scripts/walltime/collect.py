#!/usr/bin/env python3
"""Matched clean native/SL7 runs with Cargo, scheduler and FUSE timelines.

Raw outputs contain private workload identities. Publish only reviewed aggregates.
Run in a disposable, privileged Linux FUSE pod; never changes host tracing knobs.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import select
import shutil
import signal
import subprocess
import sys
import threading
import time

SCRIPTS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SCRIPTS))
from mount_helpers import isolated_test_directory, is_mounted, wait_for_quiescence
from events import read_events, requests

spec = importlib.util.spec_from_file_location('benchmark', SCRIPTS / 'benchmark-synkro.py')
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


def cpu(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    ticks = os.sysconf('SC_CLK_TCK')
    return {'user': int(fields[11]) / ticks, 'system': int(fields[12]) / ticks}


def difference(after, before):
    return {key: after[key] - before[key] for key in before}


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def stop(process, signum=signal.SIGINT):
    if process.poll() is None:
        process.send_signal(signum)
    try:
        code = process.wait(timeout=45)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
        raise RuntimeError('profiler shutdown timed out')
    assert code in (0, -signum, 128 + signum), code


def graph(path):
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    artifacts = [r for r in rows if r.get('reason') == 'compiler-artifact']
    assert artifacts and all(not row['fresh'] for row in artifacts)
    return sorted(json.dumps({k: row[k] for k in ('package_id', 'target', 'profile', 'features', 'fresh')}, sort_keys=True)
                  for row in artifacts)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=Path('/bench/source'))
    parser.add_argument('--binary', type=Path, default=Path('/bench/bin/sl7'))
    parser.add_argument('--results', type=Path, required=True)
    parser.add_argument('--cases', nargs='+', default=['native-control', 'sl7-profile', 'native-profile', 'sl7-control'])
    parser.add_argument('--fixture', action='store_true')
    parser.add_argument('--code-profile', action='store_true',
                        help='Collect daemon user/kernel stacks and opt-in code phases instead of scheduler/FUSE BPF')
    args = parser.parse_args()
    assert all(case in ('native-control', 'sl7-control', 'native-profile', 'sl7-profile') for case in args.cases)
    assert not args.code_profile or all(case.startswith('sl7-') for case in args.cases)
    args.results.mkdir(parents=True, exist_ok=False)
    cgroup = benchmark.process_cgroup()
    wrapper = args.results / 'rustc-wrapper'
    subprocess.run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', str(Path(__file__).with_name('rustc-wrapper.c')), '-o', str(wrapper)], check=True)
    trace = args.results / 'trace.bt'
    trace.write_text(Path(__file__).with_name('trace.bt').read_text().replace('@CGROUP@', str(cgroup.stat().st_ino)))
    report = dict(binary_sha256=sha(args.binary), rustc=subprocess.check_output(['rustc', '-Vv'], text=True),
                  bpftrace=None if args.code_profile else subprocess.check_output(['bpftrace', '--version'], text=True).strip(),
                  perf=subprocess.check_output(['perf', '--version'], text=True).strip(),
                  kernel=subprocess.check_output(['uname', '-r'], text=True).strip(),
                  cgroup_id=cgroup.stat().st_ino, cpu_max=(cgroup / 'cpu.max').read_text().strip(),
                  memory_max=(cgroup / 'memory.max').read_text().strip(), jobs=8,
                  compiler_wrapper_sha256=sha(wrapper), source_lock_sha256=sha(args.source / 'Cargo.lock'),
                  collector_hashes={p.name: sha(p) for p in Path(__file__).parent.iterdir() if p.suffix in ('.py', '.bt', '.c', '.h')},
                  code_profile=args.code_profile, cases=[])
    assert report['cpu_max'] == '800000 100000'
    assert int(report['memory_max']) == 12 * 2**30
    env = dict(os.environ, CARGO_BUILD_JOBS='8', CARGO_TERM_COLOR='never',
               SYNKRO_BUILD_SHA='ec6d4c5', SYNKRO_SENTRY_ENABLED='false',
               SYNKRO_RELEASE_NOTICES_ENABLED='false', SYNKRO_TEST_ISOLATION='1')
    for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS'):
        env.pop(key, None)
    report['incremental_environment'] = {key: env.get(key) for key in
                                       ('CARGO_INCREMENTAL', 'CARGO_BUILD_INCREMENTAL', 'CARGO_PROFILE_DEV_INCREMENTAL')}
    env['RUSTC_WRAPPER'] = str(wrapper)
    def save():
        (args.results / 'results.json').write_text(json.dumps(report, indent=2) + '\n')

    with isolated_test_directory('sl7-walltime-') as work:
        mount, store = work / 'mount', work / 'store'
        env.update(CARGO_TARGET_DIR=str(mount / 'target'), CARGO_BUILD_BUILD_DIR=str(mount / 'build'),
                   SYNKRO_HOME=str(work / 'home'), SYNKRO_DIR=str(work / 'home'))
        for label in args.cases:
            backend, mode = label.split('-')
            output = args.results / label
            output.mkdir()
            (output / 'units').mkdir()
            if args.code_profile and mode == 'profile':
                (output / 'phases').mkdir()
            env['SL7_WALLTIME_UNITS'] = str(output / 'units')
            mount.mkdir()
            daemon = tracer = sampler = build = None
            write_fd = None
            control_fds = []
            handles = []
            row = dict(label=label, backend=backend, traced=mode == 'profile')
            report['cases'].append(row)
            monitor_stop = threading.Event()
            samples = []
            def monitor():
                while not monitor_stop.is_set():
                    samples.append(dict(ns=time.monotonic_ns(), **benchmark.resource_counters(cgroup)))
                    monitor_stop.wait(.1)
            monitor_thread = threading.Thread(target=monitor)
            try:
                if mode == 'profile' and not args.code_profile:
                    trace_out = (output / 'events.tsv').open('w')
                    trace_err = (output / 'trace.stderr').open('w')
                    handles.extend([trace_out, trace_err])
                    tracer = subprocess.Popen(['bpftrace', '-q', '-I', str(Path(__file__).parent), '-B', 'line', str(trace)], stdout=trace_out, stderr=trace_err,
                                              env=dict(os.environ, BPFTRACE_PERF_RB_PAGES='4096'))
                    deadline = time.monotonic() + 60
                    while 'READY\t' not in (output / 'events.tsv').read_text():
                        assert tracer.poll() is None, (output / 'trace.stderr').read_text()
                        assert time.monotonic() < deadline, 'tracer startup timeout'
                        time.sleep(.1)
                if backend == 'sl7':
                    subprocess.run([str(args.binary), '--store', str(store), 'init'], check=True, stdout=subprocess.DEVNULL)
                    daemon_log = (output / 'daemon.log').open('w')
                    handles.append(daemon_log)
                    daemon_env = dict(os.environ, SL7_CACHE_STATS='1', SL7_MOUNT_STATUS='1',
                                      SL7_HASH_WORKERS='2', SL7_SEAL_WORKERS='2', SL7_IMPORT_PIPELINE='0',
                                      SL7_COMPRESS_WORKERS='0', SL7_REUSE_PREPARED='0')
                    daemon_env.pop('SL7_CODE_PROFILE_DIR', None)
                    if args.code_profile and mode == 'profile':
                        daemon_env['SL7_CODE_PROFILE_DIR'] = str(output / 'phases')
                    daemon = subprocess.Popen([str(args.binary), '--store', str(store), 'mount', str(mount)],
                                              stdout=daemon_log, stderr=daemon_log,
                                              env=daemon_env)
                    row['daemon_pid'] = daemon.pid
                    deadline = time.monotonic() + 30
                    while not is_mounted(mount):
                        assert daemon.poll() is None and time.monotonic() < deadline
                        time.sleep(.02)
                command = ['cargo', 'build', '--workspace', '--locked', '--offline', '--message-format=json', '--timings']
                read_fd, write_fd = os.pipe()
                with (output / 'cargo.jsonl').open('w') as out, (output / 'cargo.stderr').open('w') as err:
                    gate = 'import os,sys; fd=int(sys.argv[1]); os.read(fd,1); os.close(fd); os.execvpe(sys.argv[2],sys.argv[2:],os.environ)'
                    build = subprocess.Popen(['python3', '-c', gate, str(read_fd), *command], cwd=args.source, env=env,
                                             pass_fds=(read_fd,), stdout=out, stderr=err, start_new_session=True)
                    os.close(read_fd)
                    if mode == 'profile':
                        perf_log = (output / 'perf.stderr').open('w')
                        handles.append(perf_log)
                        pids = str(build.pid) + (f',{daemon.pid}' if daemon else '')
                        events = ['-e', 'cpu-clock:u', '-F', '49', '--call-graph', 'dwarf,4096']
                        if args.code_profile:
                            pids = str(daemon.pid)
                            events = ['-e', 'cpu-clock:u', '-e', 'cpu-clock:k', '-F', '99', '--call-graph', 'dwarf,16384']
                        control_read, control_write = os.pipe()
                        ack_read, ack_write = os.pipe()
                        control_fds.extend([control_write, ack_read])
                        sampler = subprocess.Popen(['perf', 'record', '--no-buildid-cache', '--clockid', 'mono',
                                                    *events,
                                                    '--delay', '-1', '--control', f'fd:{control_read},{ack_write}',
                                                    '-p', pids, '-o', str(output / 'cpu.data')], stdout=perf_log, stderr=perf_log,
                                                   pass_fds=(control_read, ack_write))
                        os.close(control_read)
                        os.close(ack_write)
                        os.write(control_write, b'enable\n')
                        assert select.select([ack_read], [], [], 30)[0], 'perf readiness timeout'
                        acknowledgment = os.read(ack_read, 128)
                        assert acknowledgment.rstrip(b'\0\r\n ') == b'ack', repr(acknowledgment)
                        assert sampler.poll() is None, (output / 'perf.stderr').read_text()
                    row['resources_before'] = benchmark.resource_counters(cgroup)
                    before = resource.getrusage(resource.RUSAGE_CHILDREN)
                    daemon_before = cpu(daemon.pid) if daemon else None
                    monitor_thread.start()
                    row['start_ns'] = time.monotonic_ns()
                    os.write(write_fd, b'1')
                    os.close(write_fd)
                    write_fd = None
                    code = build.wait(timeout=900)
                    row['end_ns'] = time.monotonic_ns()
                    after = resource.getrusage(resource.RUSAGE_CHILDREN)
                    daemon_after = cpu(daemon.pid) if daemon else None
                    assert code == 0, (output / 'cargo.stderr').read_text()[-4000:]
                row['wall_seconds'] = (row['end_ns'] - row['start_ns']) / 1e9
                row['cargo_cpu'] = dict(user=after.ru_utime - before.ru_utime, system=after.ru_stime - before.ru_stime)
                row['daemon_build_cpu'] = difference(daemon_after, daemon_before) if daemon else None
                row['resources_at_cargo_exit'] = benchmark.resource_counters(cgroup)
                # Observe quiescence before executable checks or artifact inventory.
                row['quiescence'] = wait_for_quiescence(daemon, mount) if daemon else None
                row['settled_ns'] = time.monotonic_ns()
                row['daemon_settling_cpu'] = difference(cpu(daemon.pid), daemon_after) if daemon else None
                row['resources_after_settling'] = benchmark.resource_counters(cgroup)
                if sampler:
                    stop(sampler, signal.SIGTERM)
                    sampler = None
                if tracer:
                    stop(tracer)
                    tracer = None
                monitor_stop.set()
                monitor_thread.join()
                (output / 'resources.json').write_text(json.dumps(samples) + '\n')
                for handle in handles:
                    handle.flush()
                if mode == 'profile':
                    perf_error = (output / 'perf.stderr').read_text()
                    assert 'lost' not in perf_error.lower(), perf_error
                if mode == 'profile' and not args.code_profile:
                    error = (output / 'trace.stderr').read_text()
                    assert not error.strip(), error
                    events = read_events(output / 'events.tsv')
                    paired, row['request_pairing'] = requests(events)
                    if backend == 'sl7':
                        assert len(paired) > 100, 'FUSE request trace is unexpectedly empty'
                    assert int(events[0][1]) < row['start_ns'] < row['end_ns'] < int(events[-1][1])
                    del events, paired
                    row['events_sha256'] = sha(output / 'events.tsv')
                row['artifact_count'] = len(graph(output / 'cargo.jsonl'))
                timing = mount / 'target/cargo-timings/cargo-timing.html'
                shutil.copyfile(timing, output / 'cargo-timing.html')
                row['incremental_artifacts'] = benchmark.incremental_inventory(mount)
                if not args.fixture:
                    assert row['incremental_artifacts']['files'] > 0
                executable = mount / ('target/debug/walltime_probe' if args.fixture else 'target/debug/synkro')
                expected = 'ok' if args.fixture else 'Synkro AI rule and control enforcement CLI'
                assert expected in subprocess.check_output([str(executable), '--help'], env=env, text=True)
                if daemon:
                    wait_for_quiescence(daemon, mount)
                row['status'] = 'passed'
                print(json.dumps({key: row[key] for key in ('label', 'wall_seconds', 'cargo_cpu', 'daemon_build_cpu', 'daemon_settling_cpu')}), flush=True)
            finally:
                already_failing = sys.exc_info()[0] is not None
                cleanup_errors = []
                if write_fd is not None:
                    os.close(write_fd)
                if build is not None and build.poll() is None:
                    os.killpg(build.pid, signal.SIGKILL)
                    build.wait()
                monitor_stop.set()
                if monitor_thread.is_alive():
                    monitor_thread.join()
                for profiler, signum in ((sampler, signal.SIGTERM), (tracer, signal.SIGINT)):
                    if profiler:
                        try:
                            stop(profiler, signum)
                        except Exception as error:
                            cleanup_errors.append(str(error))
                for fd in control_fds:
                    os.close(fd)
                if daemon:
                    if is_mounted(mount):
                        subprocess.run(['fusermount3', '-u', str(mount)], check=True)
                    daemon.wait(timeout=180)
                    assert daemon.returncode == 0
                for handle in handles:
                    handle.close()
                if cleanup_errors and not already_failing:
                    raise RuntimeError(cleanup_errors)
            if daemon:
                row['integrity'] = json.loads(subprocess.check_output([str(args.binary), '--store', str(store), 'check'], text=True))
                assert 'sealing inode' not in (output / 'daemon.log').read_text()
                shutil.rmtree(store)
            shutil.rmtree(mount)
            save()
    first = graph(args.results / args.cases[0] / 'cargo.jsonl')
    for case in args.cases[1:]:
        assert graph(args.results / case / 'cargo.jsonl') == first
    report['artifact_graphs_equal'] = True
    report['status'] = 'passed'
    save()


if __name__ == '__main__':
    main()
