#!/usr/bin/env python3
"""Profile the matched Synkro debug build in a disposable Linux container.

Requires perf, FUSE/SYS_ADMIN, PERFMON, and the prepared /bench/source snapshot.
Uses only the SL7 mount's connection for system-wide FUSE tracepoint collection.
The source workload is fixed; the SL7 binary hash identifies the implementation.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import resource
import signal
import subprocess
import time

from mount_helpers import is_mounted, isolated_test_directory

spec = importlib.util.spec_from_file_location('benchmark_synkro', Path(__file__).with_name('benchmark-synkro.py'))
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)
daemon_cpu, settle = benchmark.daemon_cpu, benchmark.settle

ROOT = Path(os.environ.get('SL7_PROFILE_RESULTS', '/bench/results'))
SOURCE = Path('/bench/source')
BINARY = Path(os.environ.get('SL7_PROFILE_BINARY', '/sl7/target/release/sl7'))
REVISION = 'ec6d4c53b8fdf4321de220a922f7efce67788d40'
SYSCALLS = ['openat', 'close', 'read', 'pread64', 'write', 'pwrite64',
            'lseek', 'fsync', 'fdatasync', 'getxattr', 'ftruncate', 'futex']


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_BUILD_JOBS='8', SYNKRO_BUILD_SHA=REVISION[:7],
               SYNKRO_SENTRY_ENABLED='false', SYNKRO_RELEASE_NOTICES_ENABLED='false',
               SYNKRO_TEST_ISOLATION='1', CARGO_TERM_COLOR='never')
    for key in ['RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS']:
        env.pop(key, None)
    edited = SOURCE/'crates/synkro-cli/src/lib.rs'
    original = edited.read_text()
    needle = 'about = "Synkro AI rule and control enforcement CLI"'
    assert original.count(needle) == 1
    report = dict(revision=REVISION, rustc=subprocess.check_output(['rustc', '-Vv'], text=True),
                  kernel=subprocess.check_output(['uname', '-r'], text=True).strip(),
                  perf=subprocess.check_output(['perf', '--version'], text=True).strip(),
                  sl7_sha256=hashlib.sha256(BINARY.read_bytes()).hexdigest(),
                  lock_sha256=hashlib.sha256((SOURCE/'Cargo.lock').read_bytes()).hexdigest(),
                  cpu_quota=8, memory_limit_gib=12, stages=[])

    def save():
        (ROOT/'profile.json').write_text(json.dumps(report, indent=2)+'\n')

    with isolated_test_directory('sl7-profile-') as work:
        mount, store = work/'mount', work/'store'
        mount.mkdir()
        env.update(CARGO_TARGET_DIR=str(mount/'target'), CARGO_BUILD_BUILD_DIR=str(mount/'build'),
                   SYNKRO_HOME=str(work/'home'), SYNKRO_DIR=str(work/'home'))
        subprocess.check_call([str(BINARY), '--store', str(store), 'init'], stdout=subprocess.DEVNULL)
        log = (ROOT/'daemon.log').open('w')
        daemon = subprocess.Popen([str(BINARY), '--store', str(store), 'mount', str(mount)], stdout=log, stderr=log,
                                  env=dict(os.environ, SL7_CACHE_STATS="1"))
        try:
            deadline = time.monotonic()+30
            while not is_mounted(mount):
                assert daemon.poll() is None and time.monotonic()<deadline
                time.sleep(.02)
            dev = os.stat(mount).st_dev
            assert os.major(dev) == 0
            connection = os.minor(dev)
            report['fuse_connection'] = connection
            report['daemon_threads'] = len(list(Path(f'/proc/{daemon.pid}/task').iterdir()))

            def build(stage, edit=None, sample=False, trace=False, fresh=False):
                if edit:
                    description = f'Synkro AI rule and control enforcement CLI benchmark {edit}'
                    edited.write_text(original.replace(needle, f'about = "{description}"'))
                else:
                    description = None
                profilers = []
                handles = []

                def start_profiler(kind, command):
                    handle = (ROOT/f'{stage}-{kind}.log').open('w')
                    handles.append(handle)
                    process = subprocess.Popen(command, stdout=handle, stderr=handle)
                    profilers.append((kind, process))

                if sample:
                    start_profiler('cpu', ['perf', 'record', '-e', 'cpu-clock:u', '-F', '99',
                        '--call-graph', 'dwarf,8192', '-p', str(daemon.pid),
                        '-o', str(ROOT/f'{stage}-cpu.data')])
                if trace:
                    command = ['perf', 'record', '-a', '-m', '1024', '-o', str(ROOT/f'{stage}-fuse.data')]
                    for event in ['send', 'sent', 'end']:
                        command += ['-e', f'fuse:fuse_request_{event}', '--filter', f'connection == {connection}']
                    start_profiler('fuse', command)
                    start_profiler('syscalls', ['perf', 'stat', '-x', ',', '-p', str(daemon.pid),
                        '-e', ','.join(f'syscalls:sys_enter_{s}' for s in SYSCALLS),
                        '-o', str(ROOT/f'{stage}-syscalls.csv')])
                if profilers:
                    time.sleep(.3)
                    assert all(p.poll() is None for _, p in profilers), 'profiler failed to attach'
                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                cpu_before = daemon_cpu(daemon.pid)
                start = time.monotonic()
                command = ['cargo', 'build', '--workspace', '--locked', '--offline', '--message-format=json']
                try:
                    with (ROOT/f'{stage}.jsonl').open('w') as out, (ROOT/f'{stage}.stderr').open('w') as err:
                        result = subprocess.run(command, cwd=SOURCE, env=env, stdout=out, stderr=err)
                    wall = time.monotonic()-start
                    after = resource.getrusage(resource.RUSAGE_CHILDREN)
                    assert result.returncode == 0, (ROOT/f'{stage}.stderr').read_text()[-6000:]
                    idle = settle(daemon, mount)
                    cpu = daemon_cpu(daemon.pid)-cpu_before
                finally:
                    for _, process in profilers:
                        process.send_signal(signal.SIGINT)
                    for kind, process in profilers:
                        process.wait(timeout=60)
                        assert process.returncode in [0, 130, -signal.SIGINT], (kind, process.returncode)
                    for handle in handles:
                        handle.close()
                records = [a for line in (ROOT/f'{stage}.jsonl').read_text().splitlines()
                           if (a := json.loads(line)).get('reason')=='compiler-artifact']
                assert records
                assert all(a['fresh'] for a in records) if fresh else any(not a['fresh'] for a in records)
                if description or stage=='clean':
                    help_text = subprocess.check_output([str(mount/'target/debug/synkro'), '--help'], env=env, text=True)
                    assert (description or 'Synkro AI rule and control enforcement CLI') in help_text
                row = dict(stage=stage, wall_seconds=wall, daemon_cpu_seconds=cpu,
                           cargo_cpu_seconds=after.ru_utime+after.ru_stime-before.ru_utime-before.ru_stime,
                           idle_seconds=idle, artifacts=len(records), fresh_artifacts=sum(a['fresh'] for a in records),
                           cpu_sampling=sample, fuse_tracing=trace)
                report['stages'].append(row)
                save()
                print(json.dumps(row), flush=True)

            build('clean', sample=True)
            build('unchanged', fresh=True)
            build('edit-1-control', edit=1)
            build('edit-2-profile', edit=2, sample=True, trace=True)
            build('edit-3-profile', edit=3, sample=True, trace=True)
            build('unchanged-final', fresh=True)
        finally:
            edited.write_text(original)
            drain_started = time.monotonic()
            if is_mounted(mount):
                subprocess.check_call(['fusermount3', '-u', str(mount)])
            daemon.wait(timeout=180)
            report["unmount_drain_seconds"] = time.monotonic()-drain_started
            log.close()
        assert daemon.returncode == 0
        daemon_log = (ROOT/'daemon.log').read_text()
        assert 'sealing inode' not in daemon_log
        for line in daemon_log.splitlines():
            for kind in ['cache_stats', 'sealing_stats']:
                prefix = f'sl7: {kind} '
                if line.startswith(prefix):
                    report[kind] = json.loads(line[len(prefix):])
        start = time.monotonic()
        report['integrity'] = json.loads(subprocess.check_output([str(BINARY), '--store', str(store), 'check'], text=True))
        report['integrity_seconds'] = time.monotonic()-start
        report['storage'] = json.loads(subprocess.check_output([str(BINARY), '--store', str(store), 'live-stats'], text=True))
        report['memory_events'] = Path('/sys/fs/cgroup/memory.events').read_text()
        report['status'] = 'passed'
        save()
        print('Profiling and integrity checks passed', flush=True)


if __name__=='__main__':
    main()
