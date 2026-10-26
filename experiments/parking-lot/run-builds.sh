#!/usr/bin/env bash
# Run inside a disposable Linux/FUSE environment after validating each binary.
set -euo pipefail
if [[ $# != 6 ]]; then
  echo "usage: $0 SOURCE BASELINE_BINARY DIRECTORY_BINARY PARKING_BINARY RESULTS REVISION" >&2
  exit 2
fi
repo=$(cd "$(dirname "$0")/../.." && pwd)
ulimit -Sn 524288
SL7_BINARY="$3" python3 "$repo/scripts/benchmark-synkro.py" \
  --source "$1" --results "$5" --revision "$6" \
  --rounds 2 --jobs 8 --settle-mode quiescent \
  --baseline-binary "$2" \
  --comparison-binary "directories=$3" --comparison-binary "parking=$4" \
  --backends native baseline-sl7 directories parking --profiles debug \
  --seal-workers baseline-sl7=2 --seal-workers directories=2 --seal-workers parking=2 \
  --hash-workers baseline-sl7=2 --hash-workers directories=2 --hash-workers parking=2 \
  --compress-workers baseline-sl7=0 --compress-workers directories=0 --compress-workers parking=0 \
  --import-pipeline baseline-sl7=0 --import-pipeline directories=0 --import-pipeline parking=0 \
  --reuse-prepared baseline-sl7=0 --reuse-prepared directories=0 --reuse-prepared parking=0
