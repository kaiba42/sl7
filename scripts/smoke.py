#!/usr/bin/env python3
"""Read-only source smoke test. Temporary store/exports are removed on exit."""
import argparse
import hashlib
import json
from pathlib import Path
import random
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument("sources", type=Path, nargs="+")
parser.add_argument("--binary", type=Path, default=Path("target/debug/sl7"))
parser.add_argument("--output", type=Path)
args = parser.parse_args()
binary = args.binary.resolve()


def checksum(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


with tempfile.TemporaryDirectory(prefix="sl7-smoke-") as tmp:
    root = Path(tmp)

    def run(*command):
        return subprocess.run(
            [str(binary), "--store", str(root / "store"), *map(str, command)],
            check=True, capture_output=True,
        ).stdout

    run("init")
    imports = []
    rng = random.Random(42)
    reads = 0
    for index, source in enumerate(args.sources):
        before = checksum(source)
        name = f"artifact-{index}"
        imported = json.loads(run("put", "main", name, source))
        duplicate = json.loads(run("put", "duplicate-view", name, source))
        assert duplicate["new_chunks"] == 0
        assert duplicate["new_pack_bytes"] == 0
        assert imported["file"]["sha256"] == before
        exported = root / f"export-{index}"
        run("get", "main", name, exported)
        assert checksum(exported) == before
        exported.unlink()
        with source.open("rb") as stream:
            for _ in range(25):
                offset = rng.randrange(source.stat().st_size + 1)
                size = rng.choice([1, 4096, 65536])
                stream.seek(offset)
                actual = run("read", "main", name, "--offset", offset, "--length", size)
                assert actual == stream.read(size)
                reads += 1
        assert checksum(source) == before, "source changed during smoke test"
        imports.append({"source": str(source), "first": imported, "duplicate": duplicate})
    result = {
        "imports": imports, "verified_random_reads": reads,
        "stats": json.loads(run("check")),
        "native_probe": json.loads(run("probe", root)),
        "temporary_store_removed_on_exit": True,
    }
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
