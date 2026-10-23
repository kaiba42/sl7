#!/usr/bin/env python3
"""Matched native/SL7 Cargo builds; run inside the disposable Linux container.

Downloads/tool installation happen before this script. Each case starts with an
empty output tree and, for SL7, an empty store. Source/output paths, compiler,
features, profiles and job count stay identical. No host page caches are dropped.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import resource
import shutil
import statistics
import subprocess
import time
from mount_helpers import is_mounted, isolated_test_directory


def allocated(path):
    seen = set()
    logical = blocks = files = 0
    for parent, _, names in os.walk(path):
        for name in names:
            file = Path(parent) / name
            if file.is_symlink() or not file.is_file():
                continue
            stat = file.stat()
            key = stat.st_dev, stat.st_ino
            if key not in seen:
                seen.add(key)
                files += 1
                logical += stat.st_size
                blocks += stat.st_blocks * 512
    return dict(files=files, logical_bytes=logical, allocated_regular_bytes=blocks)


def incremental_inventory(path):
    """Count actual incremental outputs without reading or publishing their names."""
    files = logical = 0
    for parent, _, names in os.walk(path):
        if "incremental" not in Path(parent).relative_to(path).parts:
            continue
        for name in names:
            file = Path(parent) / name
            if file.is_file() and not file.is_symlink():
                files += 1
                logical += file.stat().st_size
    return dict(files=files, logical_bytes=logical)


def daemon_cpu(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")


def process_cgroup():
    """Resolve the process's v2 cgroup in namespaced and privileged containers."""
    root = Path("/sys/fs/cgroup")
    for line in Path("/proc/self/cgroup").read_text().splitlines():
        if line.startswith("0::"):
            candidate = root / line[3:].lstrip("/")
            if (candidate / "cpu.max").exists():
                return candidate
    if (root / "cpu.max").exists():
        return root
    raise RuntimeError("Cannot locate this process's cgroup v2 resource limits")


def resource_counters(cgroup):
    return {name: (cgroup / name).read_text().strip()
            for name in ["cpu.stat", "cpu.pressure", "io.pressure", "memory.events"]
            if (cgroup / name).exists()}


def settle(daemon, mount):
    """Observe the daemon idle after a request barrier; report wait separately.

    This is not a durability guarantee. Normal unmount at the end of each case
    separately measures the remaining drain and requires daemon exit 0.
    """
    start = time.monotonic()
    os.stat(mount)
    last = daemon_cpu(daemon.pid)
    stable = 0
    while stable < 3:
        if daemon.poll() is not None:
            raise RuntimeError("SL7 exited during benchmark")
        time.sleep(0.1)
        now = daemon_cpu(daemon.pid)
        stable = stable + 1 if now == last else 0
        last = now
        if time.monotonic() - start > 180:
            raise RuntimeError("SL7 did not become idle after build")
    return time.monotonic() - start


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--rounds", type=int, default=2)
    parser.add_argument("--jobs", type=int, default=8)
    parser.add_argument("--baseline-binary", type=Path,
                        help="Also measure an older SL7 binary on the same machine")
    parser.add_argument("--profiles", nargs="+", choices=["debug", "release"],
                        default=["debug", "release"])
    parser.add_argument("--backends", nargs="+", choices=["native", "baseline-sl7", "sl7"],
                        help="Select a focused comparison; default includes native and SL7")
    args = parser.parse_args()
    args.backends = args.backends or (["native", "baseline-sl7", "sl7"] if args.baseline_binary else ["native", "sl7"])
    if "baseline-sl7" in args.backends and not args.baseline_binary:
        parser.error("baseline-sl7 requires --baseline-binary")
    if args.rounds < 1 or len(set(args.backends)) != len(args.backends) or len(set(args.profiles)) != len(args.profiles):
        parser.error("rounds must be positive; profiles and backends must be unique")
    cgroup = process_cgroup()
    args.results.mkdir(parents=True, exist_ok=True)
    binary = Path(os.environ.get("SL7_BINARY", "/sl7/target/release/sl7"))
    edited_file = args.source / "crates/synkro-cli/src/lib.rs"
    original = edited_file.read_text()
    needle = 'about = "Synkro AI rule and control enforcement CLI"'
    assert original.count(needle) == 1
    env = dict(os.environ, CARGO_BUILD_JOBS=str(args.jobs),
               SYNKRO_BUILD_SHA=args.revision[:7], SYNKRO_SENTRY_ENABLED="false",
               SYNKRO_RELEASE_NOTICES_ENABLED="false", SYNKRO_TEST_ISOLATION="1",
               CARGO_TERM_COLOR="never")
    for name in ("RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"):
        env.pop(name, None)
    report = dict(revision=args.revision, rustc=subprocess.check_output(["rustc", "-Vv"], text=True),
                  platform=subprocess.check_output(["uname", "-a"], text=True).strip(),
                  sl7_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                  lock_sha256=hashlib.sha256((args.source / "Cargo.lock").read_bytes()).hexdigest(),
                  jobs=args.jobs, rounds=args.rounds, cases=[],
                  cpu_max=(cgroup / "cpu.max").read_text().strip(),
                  memory_max=(cgroup / "memory.max").read_text().strip(),
                  nofile_limit=resource.getrlimit(resource.RLIMIT_NOFILE),
                  incremental_environment={key:env.get(key) for key in ["CARGO_INCREMENTAL", "CARGO_BUILD_INCREMENTAL", "CARGO_PROFILE_DEV_INCREMENTAL", "CARGO_PROFILE_RELEASE_INCREMENTAL"]},
                  method="Empty Cargo outputs; pre-fetched dependencies; warm OS caches; serial, counterbalanced cases; unchanged workspace profiles/default features")
    if args.baseline_binary:
        report["baseline_sl7_sha256"] = hashlib.sha256(args.baseline_binary.read_bytes()).hexdigest()

    def save():
        (args.results / "results.json").write_text(json.dumps(report, indent=2) + "\n")

    with isolated_test_directory("sl7-synkro-benchmark-") as work:
        mount = work / "mount"
        store = work / "store"
        env.update(CARGO_TARGET_DIR=str(mount / "target"), CARGO_BUILD_BUILD_DIR=str(mount / "build"),
                   SYNKRO_HOME=str(work / "home"), SYNKRO_DIR=str(work / "home"))
        for round_id in range(args.rounds):
            profiles = list(args.profiles)
            backends = list(args.backends)
            if round_id % 2:
                profiles.reverse()
                backends.reverse()
            for profile in profiles:
                for backend in backends:
                    case_binary = args.baseline_binary if backend == "baseline-sl7" else binary
                    label = f"round-{round_id + 1}-{profile}-{backend}"
                    edited_file.write_text(original)
                    mount.mkdir()
                    daemon = None
                    case = dict(round=round_id + 1, profile=profile, backend=backend, measurements=[])
                    case["resources_before"] = resource_counters(cgroup)
                    report["cases"].append(case)
                    log_path = args.results / f"{label}-daemon.log"
                    daemon_log = log_path.open("w")

                    def sl7(*command):
                        return json.loads(subprocess.check_output([str(case_binary), "--store", str(store), *command], text=True))

                    def start_mount():
                        process = subprocess.Popen([str(case_binary), "--store", str(store), "mount", str(mount)], stdout=daemon_log, stderr=daemon_log,
                                                   env=dict(os.environ, SL7_CACHE_STATS="1"))
                        deadline = time.monotonic() + 30
                        while not is_mounted(mount):
                            if process.poll() is not None or time.monotonic() > deadline:
                                raise RuntimeError(log_path.read_text())
                            time.sleep(.02)
                        return process

                    def stop_mount(process):
                        start = time.monotonic()
                        subprocess.check_call(["fusermount3", "-u", str(mount)])
                        process.wait(timeout=180)
                        assert process.returncode == 0, log_path.read_text()
                        return time.monotonic() - start

                    def build(stage, expected=None, fresh=False):
                        command = ["cargo", "build", "--workspace", "--locked", "--offline", "--message-format=json"]
                        if profile == "release":
                            command.append("--release")
                        prefix = args.results / f"{label}-{stage}"
                        before = resource.getrusage(resource.RUSAGE_CHILDREN)
                        daemon_before = daemon_cpu(daemon.pid) if daemon else 0
                        start = time.monotonic()
                        with prefix.with_suffix(".jsonl").open("w") as stdout, prefix.with_suffix(".stderr").open("w") as stderr:
                            result = subprocess.run(command, cwd=args.source, env=env, stdout=stdout, stderr=stderr)
                        elapsed = time.monotonic() - start
                        after = resource.getrusage(resource.RUSAGE_CHILDREN)
                        if result.returncode:
                            raise RuntimeError(prefix.with_suffix(".stderr").read_text()[-12000:])
                        idle = settle(daemon, mount) if daemon else 0
                        artifacts = [r for line in prefix.with_suffix(".jsonl").read_text().splitlines()
                                     if (r := json.loads(line)).get("reason") == "compiler-artifact"]
                        assert artifacts
                        if fresh:
                            assert all(r["fresh"] for r in artifacts), [r["package_id"] for r in artifacts if not r["fresh"]]
                        else:
                            assert any(not r["fresh"] for r in artifacts), "edit/cold build unexpectedly fresh"
                        if expected:
                            help_text = subprocess.check_output([str(mount / "target" / profile / "synkro"), "--help"], env=env, text=True)
                            assert expected in help_text
                        measurement = dict(stage=stage, command=command, wall_seconds=elapsed,
                                           cargo_user_seconds=after.ru_utime-before.ru_utime,
                                           cargo_system_seconds=after.ru_stime-before.ru_stime,
                                           daemon_cpu_seconds=(daemon_cpu(daemon.pid)-daemon_before) if daemon else 0,
                                           post_build_idle_wait_seconds=idle,
                                           fresh_artifacts=sum(r["fresh"] for r in artifacts), artifacts=len(artifacts))
                        case["measurements"].append(measurement)
                        save()
                        print(f"{label} {stage}: {elapsed:.3f}s; daemon CPU {measurement['daemon_cpu_seconds']:.2f}s", flush=True)

                    try:
                        if backend != "native":
                            sl7("init")
                            daemon = start_mount()
                        build("clean", "Synkro AI rule and control enforcement CLI")
                        for i in range(3):
                            build(f"unchanged-{i+1}", fresh=True)
                        for i in range(3):
                            description = f"Synkro AI rule and control enforcement CLI benchmark {i+1}"
                            edited_file.write_text(original.replace(needle, f'about = "{description}"'))
                            build(f"edit-{i+1}", description)
                        if daemon:
                            case["pre_remount_drain_seconds"] = stop_mount(daemon)
                            daemon = start_mount()
                        build("unchanged-after-remount", fresh=True)
                    finally:
                        if daemon and daemon.poll() is None:
                            case["unmount_drain_seconds"] = stop_mount(daemon)
                        daemon_log.close()
                        edited_file.write_text(original)
                    assert not is_mounted(mount)
                    if backend != "native":
                        assert "sealing inode" not in log_path.read_text(), log_path.read_text()
                        check_start = time.monotonic()
                        case["integrity"] = sl7("check")
                        case["integrity_check_seconds"] = time.monotonic() - check_start
                        case["storage"] = sl7("live-stats")
                        case["incremental_artifacts"] = incremental_inventory(store / "tree")
                        case["daemon_counters"] = []
                        for line in log_path.read_text().splitlines():
                            for kind in ["cache_stats", "sealing_stats"]:
                                prefix = f"sl7: {kind} "
                                if line.startswith(prefix):
                                    case["daemon_counters"].append({"kind":kind, "values":json.loads(line[len(prefix):])})
                        shutil.rmtree(store)
                    else:
                        case["storage"] = allocated(mount)
                        case["incremental_artifacts"] = incremental_inventory(mount)
                    case["status"] = "passed"
                    case["resources_after"] = resource_counters(cgroup)
                    save()
                    shutil.rmtree(mount)
        report["memory_events"] = (cgroup / "memory.events").read_text()
        report["memory_peak"] = int((cgroup / "memory.peak").read_text())
        report["status"] = "passed"
        save()
    for profile in args.profiles:
        for stage in ("clean", "unchanged", "edit", "unchanged-after-remount"):
            times = {}
            for backend in args.backends:
                samples = [m["wall_seconds"] for c in report["cases"] if c["profile"] == profile and c["backend"] == backend
                           for m in c["measurements"] if m["stage"] == stage or (stage in ("unchanged", "edit") and m["stage"].rsplit("-",1)[-1].isdigit() and m["stage"].startswith(stage+"-"))]
                times[backend] = statistics.median(samples)
            print(profile, stage, json.dumps(times), flush=True)


if __name__ == "__main__":
    main()
