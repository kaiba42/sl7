#!/usr/bin/env python3
"""Apply the State-only experiment to an isolated SL7 source copy."""
from pathlib import Path
import shutil
import sys

root = Path(sys.argv[1]).resolve()
source = Path(__file__).resolve().parent
if (root / 'src/staging/state_sync.rs').exists():
    raise SystemExit('experiment already applied')
shutil.copyfile(source / 'state_sync.rs', root / 'src/staging/state_sync.rs')
for name in ['src/staging.rs', 'src/staging/background.rs']:
    path = root / name
    text = path.read_text()
    expected = 1 if name == 'src/staging.rs' else 4
    assert text.count('Mutex<State>') == expected, 'State lock layout changed; review the adapter'
    assert text.count('Arc<Condvar>') == (2 if name == 'src/staging.rs' else 4)
    text = text.replace('Mutex<State>', 'state_sync::Mutex<State>')
    text = text.replace('Arc<Condvar>', 'Arc<state_sync::Condvar>')
    if name == 'src/staging.rs':
        text = text.replace('mod background;', 'mod background;\nmod state_sync;')
        text = text.replace('state: Arc::new(Mutex::new(state))', 'state: Arc::new(state_sync::Mutex::new(state))')
        text = text.replace('publication_done: Arc::new(Condvar::new())', 'publication_done: Arc::new(state_sync::Condvar::new())')
    else:
        text = text.replace('Arc::new(Condvar::new())', 'Arc::new(state_sync::Condvar::new())')
    path.write_text(text)
path = root / 'Cargo.toml'
path.write_text(path.read_text().replace('[dependencies]', '[dependencies]\nparking_lot = "=0.13.0"'))

shutil.copyfile(source / "Cargo.lock", root / "Cargo.lock")
