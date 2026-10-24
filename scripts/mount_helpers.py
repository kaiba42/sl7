"""Linux mount-table inspection that also detects disconnected FUSE mounts."""
from pathlib import Path
from contextlib import contextmanager
import re
import shutil
import subprocess
import tempfile
import json
import os
import time


def mount_status(mount):
    status = json.loads(os.getxattr(mount, 'user.sl7.status'))
    if status.get('schema_version') != 1:
        raise RuntimeError('Unsupported SL7 mount status schema')
    return status


def wait_for_quiescence(daemon, mount, timeout=180):
    """Wait for closed handles and drained work, including workers asleep in I/O.

    Three matching quiet observations avoid accepting a transient gap. This is
    an observation for isolated workloads, not a durability barrier or a promise
    that another client cannot start new work after it returns.
    """
    start = time.monotonic()
    stable = polls = 0
    previous = None
    while True:
        if daemon.poll() is not None:
            raise RuntimeError('SL7 exited while waiting for quiescence')
        status = mount_status(mount)
        polls += 1
        sealing = status['sealing']
        if status['worker_failed'] or status['stopping'] or sealing['errors']:
            raise RuntimeError(f'SL7 failed while draining: {status}')
        progress = tuple(sealing[key] for key in (
            'completed', 'canceled', 'skipped', 'write_operations',
            'read_operations', 'sync_operations', 'namespace_commits'))
        if status['quiescent']:
            stable = stable + 1 if progress == previous else 1
        else:
            stable = 0
        previous = progress
        elapsed = time.monotonic() - start
        if stable >= 3:
            return dict(wait_seconds=elapsed, polls=polls, status=status)
        if elapsed >= timeout:
            raise RuntimeError(f'SL7 did not become quiescent: {status}')
        time.sleep(0.1)


def is_mounted(path):
    # os.path.ismount() uses stat and returns False on ENOTCONN. A killed FUSE
    # daemon leaves a mount behind that must still be explicitly unmounted.
    target = str(path)
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        encoded = line.split()[4]
        mountpoint = re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), encoded)
        if mountpoint == target:
            return True
    return False


@contextmanager
def isolated_test_directory(prefix):
    """Keep runtime recursive bind exports from retaining our test FUSE mounts.

    Make the parent unbindable *before* mounting FUSE below it. A recursive bind
    of the container tree then prunes this subtree. This is mount isolation, not
    a forced/lazy unmount: the suites still require a normal unmount and exit 0.
    """
    root = Path(tempfile.mkdtemp(prefix=prefix))
    try:
        subprocess.check_call(["mount", "--bind", str(root), str(root)])
        subprocess.check_call(["mount", "--make-unbindable", str(root)])
        yield root
    finally:
        # Never recursively delete through an active or disconnected FUSE mount.
        if is_mounted(root / "mount"):
            raise RuntimeError(f"test mount remains active; preserving {root}")
        if is_mounted(root):
            subprocess.check_call(["umount", str(root)])
        shutil.rmtree(root)
