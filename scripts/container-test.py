#!/usr/bin/env python3
"""Run all four actual mount suites and emit one machine-readable report."""
import json
from pathlib import Path
import subprocess
import sys

results = {}
for name, script in [("readonly", "mounted-test.py"), ("staging", "staging-test.py"), ("live", "live-test.py"), ("selfhost", "selfhost-test.py")]:
    results[name] = json.loads(subprocess.check_output(
        [sys.executable, str(Path(__file__).with_name(script))], text=True))
print(json.dumps(results, indent=2))
