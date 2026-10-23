#!/usr/bin/env bash
# Snapshot a Synkro revision and compare native output storage with SL7 in Linux.
set -euo pipefail
cd "$(dirname "$0")/.."
checkout="${1:?usage: bash scripts/benchmark-synkro.sh /path/to/synkro-rs [results-directory]}"
results="${2:-test-results/synkro-benchmark}"
context="${SL7_DOCKER_CONTEXT:-orbstack}"
image="${SL7_BENCH_IMAGE:-sl7-mount-test:local}"
rounds="${SL7_BENCH_ROUNDS:-2}"
jobs="${SL7_BENCH_JOBS:-8}"
container="sl7-synkro-benchmark-$$"
mkdir -p "$results"
results="$(cd "$results" && pwd)"
revision="$(git -C "$checkout" rev-parse HEAD)"
docker --context "$context" image inspect "$image" >/dev/null
archive="$(mktemp "$results/source.XXXXXX")"
git -C "$checkout" archive --format=tar -o "$archive" "$revision"
printf 'Benchmarking committed source %s (uncommitted files excluded)\n' "$revision"

cleanup() {
    result=$?
    trap - EXIT
    docker --context "$context" cp "$container:/bench/results/." "$results/" || true
    docker --context "$context" rm -f "$container" >/dev/null || true
    rm -f "$archive"
    exit "$result"
}
trap cleanup EXIT
docker --context "$context" create --name "$container" --cpus="$jobs" \
    --memory=12g --memory-swap=12g --device /dev/fuse --cap-add SYS_ADMIN \
    --security-opt apparmor=unconfined "$image" sleep infinity >/dev/null
docker --context "$context" start "$container" >/dev/null
docker --context "$context" exec "$container" mkdir -p /bench/source /bench/results
docker --context "$context" cp "$archive" "$container:/bench/source.tar"
docker --context "$context" cp scripts/benchmark-synkro.py "$container:/sl7/scripts/benchmark-synkro.py"
docker --context "$context" cp scripts/mount_helpers.py "$container:/sl7/scripts/mount_helpers.py"
docker --context "$context" exec "$container" tar xf /bench/source.tar -C /bench/source
# Dependency installation/downloads are excluded from every measurement.
docker --context "$context" exec -w /bench/source "$container" sh -c \
    'apt-get update && apt-get install -y --no-install-recommends cmake pkg-config && cargo fetch --locked --target "$(rustc -vV | sed -n "s/^host: //p")"' \
    >"$results/setup.log" 2>&1
docker --context "$context" exec "$container" python3 /sl7/scripts/benchmark-synkro.py \
    --source /bench/source --results /bench/results --revision "$revision" \
    --rounds "$rounds" --jobs "$jobs" | tee "$results/progress.log"
