#!/usr/bin/env python3
"""Build SL7 and its full dependency graph through the deduplicating mount."""
import json
from pathlib import Path
import runpy
import shutil
import subprocess
from mount_helpers import is_mounted, isolated_test_directory

Harness = runpy.run_path(str(Path(__file__).with_name("live-test.py")))["Harness"]


def run(work):
    h = Harness(work)
    source = work / "source"
    source.mkdir()
    for name in ("Cargo.toml", "Cargo.lock"):
        shutil.copy2(Path("/sl7") / name, source / name)
    for name in ("src", "tests", "benches"):
        shutil.copytree(Path("/sl7") / name, source / name)

    def cargo(*args, fresh=False):
        return h.cargo(source, "selfhost", *args, fresh=fresh)

    def binary(profile, edited=False):
        path = h.mount / "selfhost/target" / profile / "sl7"
        assert subprocess.check_output([str(path), "--version"], text=True).strip() == "sl7 0.1.0"
        if edited:
            assert "Self-hosted edited artifact storage" in subprocess.check_output([str(path), "--help"], text=True)

    with h.mounted():
        cargo("build", "--locked", "--features", "fuse")
        cargo("test", "--locked", "--features", "fuse")
        cargo("check", "--locked", "--features", "fuse")
        cargo("build", "--release", "--locked", "--features", "fuse")
        binary("debug")
        binary("release")
        cargo("build", "--locked", "--features", "fuse", fresh=True)
        cargo("build", "--release", "--locked", "--features", "fuse", fresh=True)
    built = h.snapshot("selfhost-debug-release-built")
    assert built["sealed_files"] > 20, built
    assert built["native_allocated_bytes"] < built["logical_bytes"] // 2, built
    main = source / "src/main.rs"
    text = main.read_text()
    assert "Shared artifact storage prototype" in text
    main.write_text(text.replace("Shared artifact storage prototype", "Self-hosted edited artifact storage"))
    with h.mounted():
        cargo("build", "--locked", "--features", "fuse")
        cargo("build", "--release", "--locked", "--features", "fuse")
        cargo("test", "--locked", "--features", "fuse")
        binary("debug", edited=True)
        binary("release", edited=True)
    h.snapshot("selfhost-edited-and-rebuilt")
    with h.mounted():
        cargo("build", "--locked", "--features", "fuse", fresh=True)
        cargo("build", "--release", "--locked", "--features", "fuse", fresh=True)
        cargo("clean", "--release")
        binary("debug", edited=True)
    h.snapshot("selfhost-release-cleaned")
    with h.mounted():
        cargo("clean")
    clean = h.snapshot("selfhost-all-cleaned")
    assert clean["files"] == clean["native_allocated_bytes"] == clean["content"]["unique_chunks"] == 0
    with h.mounted():
        cargo("build", "--locked", "--features", "fuse")
        cargo("build", "--release", "--locked", "--features", "fuse")
        binary("debug", edited=True)
        binary("release", edited=True)
        cargo("clean")
    final = h.snapshot("selfhost-rebuilt-after-clean-then-cleaned")
    assert final["content"]["referenced_pack_bytes"] == final["content"]["orphan_pack_bytes"] == 0
    assert "sealing inode" not in h.log.read_text(), h.log.read_text()
    return {"status": "passed", "project": "SL7 with all fuse dependencies",
            "cargo_commands": h.commands, "snapshots": h.snapshots,
            "final_unique_chunks": final["content"]["unique_chunks"]}


if __name__ == "__main__":
    with isolated_test_directory("sl7-selfhost-test-") as work:
        print(json.dumps(run(work), indent=2))
