#!/usr/bin/env bash
# Reproduce profiling of the exact source revision used by the build baseline.
set -euo pipefail
cd "$(dirname "$0")/.."
checkout="${1:?usage: bash scripts/profile-synkro.sh /path/to/synkro-rs [results-directory]}"
results="${2:-test-results/profile}"
context="${SL7_DOCKER_CONTEXT:-orbstack}"
image="${SL7_BENCH_IMAGE:-sl7-mount-test:local}"
container="sl7-profile-$$"
revision=ec6d4c53b8fdf4321de220a922f7efce67788d40
mkdir -p "$results"
results="$(cd "$results" && pwd)"
archive="$(mktemp "$results/source.XXXXXX")"
git -C "$checkout" archive --format=tar -o "$archive" "$revision"
cleanup() {
    result=$?
    trap - EXIT
    docker --context "$context" cp "$container:/bench/results/." "$results/" || true
    docker --context "$context" rm -f "$container" >/dev/null || true
    rm -f "$archive"
    exit "$result"
}
trap cleanup EXIT
docker --context "$context" create --name "$container" --cpus=8 --memory=12g --memory-swap=12g \
    --device /dev/fuse --cap-add SYS_ADMIN --cap-add SYS_PTRACE --cap-add PERFMON \
    --security-opt apparmor=unconfined --security-opt seccomp=unconfined \
    "$image" sleep infinity >/dev/null
docker --context "$context" start "$container" >/dev/null
docker --context "$context" exec "$container" mkdir -p /bench/source /bench/results
docker --context "$context" cp "$archive" "$container:/bench/source.tar"
docker --context "$context" exec "$container" tar xf /bench/source.tar -C /bench/source
# The image supplies dependencies, not the implementation being measured.
for source in Cargo.toml Cargo.lock; do
    docker --context "$context" cp "$source" "$container:/sl7/$source"
done
docker --context "$context" cp src/. "$container:/sl7/src/"
docker --context "$context" exec "$container" cargo build --locked --release --features fuse \
    >"$results/sl7-build.log" 2>&1
for script in profile-synkro.py benchmark-synkro.py mount_helpers.py analyze-profile.py; do
    docker --context "$context" cp "scripts/$script" "$container:/sl7/scripts/$script"
done
docker --context "$context" exec -w /bench/source "$container" sh -c \
    'apt-get update && apt-get install -y --no-install-recommends cmake pkg-config linux-perf && cargo fetch --locked --target aarch64-unknown-linux-gnu' \
    >"$results/setup.log" 2>&1
docker --context "$context" exec "$container" python3 /sl7/scripts/profile-synkro.py | tee "$results/progress.log"
docker --context "$context" exec "$container" sh -c '
    set -eu
    for file in /bench/results/*-cpu.data; do
        perf script -i "$file" > "${file%.data}.txt"
        perf report --stdio --no-children -g none --percent-limit 0.5 -i "$file" > "${file%.data}-report.txt"
    done
    for file in /bench/results/*-fuse.data; do
        perf script --ns -F time,event,trace -i "$file" > "${file%.data}.txt"
        perf report --stdio --no-children -g none --percent-limit 100 -i "$file" > "${file%.data}-report.txt"
    done
    python3 /sl7/scripts/analyze-profile.py /bench/results
'
