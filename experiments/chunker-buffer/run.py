import hashlib
import json
from pathlib import Path
import platform
import random
import subprocess

libdir = Path(subprocess.check_output(['rustc', '--print', 'target-libdir'], text=True).strip())
libs = list(libdir.glob('libstd-*.rlib'))
assert len(libs) == 1
Path('stdlib').write_bytes(libs[0].read_bytes() * 16)
Path('zeros').write_bytes(bytes(32 * 2**20))
Path('random').write_bytes(random.Random(42).randbytes(32 * 2**20))
for name, size in [('small-4k', 4096), ('small-64k', 65536), ('small-1m', 2**20)]:
    Path(name).write_bytes(random.Random(42).randbytes(size))
names = ['stdlib', 'zeros', 'random', 'small-4k', 'small-64k', 'small-1m']
binary = Path('target/release/sl7-chunker-probe')
rows = json.loads(subprocess.check_output([str(binary), *names], text=True))
assert len(rows) == 144
print(json.dumps(dict(
    status='passed', platform=platform.platform(),
    baseline_commit=Path(__file__).with_name('base-commit').read_text().strip(),
    rustc=subprocess.check_output(['rustc', '-Vv'], text=True),
    source_sha256={str(p): hashlib.sha256(p.read_bytes()).hexdigest()
                   for p in [Path('src/original.rs'), Path('src/main.rs'), Path('Cargo.lock')]},
    binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
    fixtures={name: dict(bytes=Path(name).stat().st_size,
                         sha256=hashlib.sha256(Path(name).read_bytes()).hexdigest()) for name in names},
    exact_chunk_identity_parity=True, rows=rows,
    scope='Chunking microbenchmark with warm filesystem caches; excludes hashing from timed scans, compression, catalog work and Cargo.'
), indent=2))
