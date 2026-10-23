"""Linux mount-table inspection that also detects disconnected FUSE mounts."""
from pathlib import Path
from contextlib import contextmanager
import re
import shutil
import subprocess
import tempfile


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
